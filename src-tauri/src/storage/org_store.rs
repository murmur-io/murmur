//! Shared-Brain / ORG storage surface — the local org membership state, the outbound org-share
//! state machine (the `org_shares` table + its dedup/retry/revoke bookkeeping), and the decrypted
//! ORG-ITEM replica (`org_items` + its `org_chunks` / `org_vec_chunks` / `fts_org_chunks` index,
//! the KNN + FTS retrieval legs, and the read-only item/list getters). Extracted VERBATIM from
//! `storage::db` (God-file split, a PURE MOVE — zero behavior change): an inherent-impl split of
//! [`crate::storage::db::Db`] across files; every method keeps its EXACT prior body, signature, AND
//! gating.
//!
//! GATING — org items are deliberately ORG-DISCLOSED content that lives OUTSIDE the per-folder lock
//! domain (SQLCipher protects them at rest; no folder seal/`visibility_clause` gate applies — spec
//! §"Trust model"). The read gate that DOES apply is the PER-INSTANCE org toggle
//! `os.context_enabled = 1`, joined + filtered at the SQL level in `search_org_chunks_knn` /
//! `search_org_chunks_fts` / `get_org_item` / `count_org_items` EXACTLY as on trunk — a disabled
//! org's chunks/items are excluded in SQL, never read into Rust. `get_org_item` / `list_org_items`
//! stay gated on `tombstoned = 0` (+ `context_enabled` where trunk had it). The org-private helpers
//! (`map_org_state`, `map_org_share`, the `ORG_SHARE_COLS` const, `dedup_org_hits_by_item`, and
//! `purge_org_item_chunks_tx`) are used ONLY by these methods, so they moved along and stay PRIVATE
//! to this module. `fts_match_query` (shared with several db.rs FTS readers) was promoted to
//! `pub(crate)` in `db.rs` and is reached cross-file. The 1:1-share (`outbound_shares`) machinery
//! stays in `db.rs`. Tests stay in db.rs's `mod tests` (shared harness); the count is conserved.

use std::collections::HashSet;

use rusqlite::OptionalExtension;

use crate::embed::Embedder;
use crate::error::Result;
use crate::storage::db::{fts_match_query, fts_match_query_any, map_err, Db};
use crate::storage::models::OrgChunkHit;

/// Chunk/vector material for one org item, prepared before entering the short feed-commit
/// transaction. Keeping model inference on this side of the transaction is load-bearing: recording
/// admission may invalidate the work, while a committed feed action and its cursor must remain one
/// indivisible SQLite mutation.
pub(crate) struct PreparedOrgItemIndex {
    chunks: Vec<String>,
    vector_blobs: Option<Vec<Vec<u8>>>,
}

/// What this device currently holds for ONE org item id — the minimum the anti-entropy reconcile
/// sweep needs to decide "already converged, skip" vs "fetch + ingest" WITHOUT downloading a blob,
/// and the same read [`crate::commands::org_sweep_pending`] uses to spot an orphaned replica of an
/// already-revoked share. Content-free: a tombstone flag plus the opaque plaintext hash the publisher
/// sealed under — never the markdown, the title, or any key material.
#[derive(Clone, Debug)]
pub(crate) struct OrgReplicaState {
    /// `true` once the item has been evicted here. Append-only and permanent: a later live feed
    /// record must never resurrect withdrawn plaintext.
    pub(crate) tombstoned: bool,
    /// The publisher's plaintext hash for the version stored locally, or `None` for a row written
    /// before the feed carried one. Equality with the feed's hash is what lets the sweep skip an
    /// already-converged item with no blob fetch.
    pub(crate) content_sha256: Option<Vec<u8>>,
}

/// One live org item's existing chunk rows, loaded for a model-switch reindex. The keyset iterator
/// returns exactly one item at a time so a large local replica never becomes one plaintext RAM
/// buffer. Ordered chunk ids plus the canonical item version/hash form the optimistic concurrency
/// token for the vector-only commit (SQLite rowids may be reused after a clean replace, so ids alone
/// are insufficient).
pub(crate) struct OrgItemVectorBatch {
    pub(crate) item_id: String,
    pub(crate) chunk_ids: Vec<i64>,
    pub(crate) texts: Vec<String>,
    seq: u64,
    rev: u32,
    generation: u32,
    content_sha256: Option<Vec<u8>>,
}

impl Db {
    /// Upsert the locally-cached membership of an org (create/status). Preserves an existing row's
    /// local `consented` flag, `last_seq` cursor, AND `context_enabled` toggle (an incoming status
    /// refresh MUST NOT reset the consent flag, rewind the sync cursor, or silently re-enable an org
    /// the user disabled on this instance). NO content — membership metadata only.
    pub fn upsert_org_state(&self, o: &crate::storage::OrgState) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO org_state (org_id, name, role, joined_at, consented, last_seq, generation, context_enabled)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(org_id) DO UPDATE SET
               name = excluded.name,
               role = excluded.role,
               generation = excluded.generation",
            rusqlite::params![
                o.org_id,
                o.name,
                o.role,
                o.joined_at,
                o.consented as i64,
                o.last_seq,
                o.generation as i64,
                o.context_enabled as i64
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    fn map_org_state(r: &rusqlite::Row<'_>) -> rusqlite::Result<crate::storage::OrgState> {
        Ok(crate::storage::OrgState {
            org_id: r.get(0)?,
            name: r.get(1)?,
            role: r.get(2)?,
            joined_at: r.get(3)?,
            consented: r.get::<_, i64>(4)? != 0,
            last_seq: r.get(5)?,
            generation: r.get::<_, i64>(6)? as u32,
            context_enabled: r.get::<_, i64>(7)? != 0,
        })
    }

    /// The locally-cached state of one org (or `None` if not joined locally).
    pub fn get_org_state(&self, org_id: &str) -> Result<Option<crate::storage::OrgState>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT org_id, name, role, joined_at, consented, last_seq, generation, context_enabled
               FROM org_state WHERE org_id = ?1",
            rusqlite::params![org_id],
            Self::map_org_state,
        )
        .optional()
        .map_err(map_err)
    }

    /// Every locally-joined org (for the launch sweep + a future multi-org list). Ordered by join time.
    pub fn list_org_states(&self) -> Result<Vec<crate::storage::OrgState>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT org_id, name, role, joined_at, consented, last_seq, generation, context_enabled
                   FROM org_state ORDER BY joined_at ASC",
            )
            .map_err(map_err)?;
        let rows = stmt.query_map([], Self::map_org_state).map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Set the local org-egress consent flag for an org (mirrors the config consent grants: the ONLY
    /// mutator, so a status refresh can't clear it). Fail-safe ordering is the caller's concern.
    pub fn set_org_consented(&self, org_id: &str, consented: bool) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE org_state SET consented = ?2 WHERE org_id = ?1",
            rusqlite::params![org_id, consented as i64],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Set the PER-INSTANCE org context toggle (Settings → Organization): whether a JOINED org
    /// contributes content on THIS Murmur install — browsing (`list_org_items`) AND brain/assistant
    /// context (`search_org_chunks_knn`/`_fts`). The ONLY mutator (mirrors `set_org_consented`) — a
    /// status/feed refresh (`upsert_org_state`) never touches this column. Disabling never deletes the
    /// local replica; re-enabling is instant with no re-sync.
    pub fn set_org_context_enabled(&self, org_id: &str, enabled: bool) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE org_state SET context_enabled = ?2 WHERE org_id = ?1",
            rusqlite::params![org_id, enabled as i64],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Advance the synced feed cursor for an org (monotonic; a caller never rewinds it below the
    /// stored value). Used by the feed-sync slice; kept here so the schema owner defines the writer.
    pub fn set_org_last_seq(&self, org_id: &str, last_seq: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE org_state SET last_seq = ?2 WHERE org_id = ?1 AND ?2 > last_seq",
            rusqlite::params![org_id, last_seq],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Update the cached live generation for an org (after a rotation the owner drove, or a status pull).
    pub fn set_org_generation(&self, org_id: &str, generation: u32) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE org_state SET generation = ?2 WHERE org_id = ?1",
            rusqlite::params![org_id, generation as i64],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Atomically withdraw local membership AND purge its decrypted replica. Idempotent; leaves
    /// `org_shares` alone (a leave doesn't retroactively un-share — the items stay published unless
    /// explicitly revoked).
    ///
    /// The single transaction is load-bearing: deleting `org_state` first and crashing (or racing an
    /// in-flight feed commit) before a later replica purge leaves plaintext rows orphaned. Feed commits
    /// claim their sequence against this same membership row inside their own transaction, so SQLite's
    /// serialized writer order yields only two safe outcomes: commit-before-leave is purged here;
    /// leave-before-commit makes the feed claim fail before any plaintext insert.
    pub fn delete_org_state(&self, org_id: &str) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let items = Self::purge_org_replica_tx(&tx, org_id)?;
        tx.execute(
            "DELETE FROM org_state WHERE org_id = ?1",
            rusqlite::params![org_id],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        tracing::info!(target: "org", items, "atomically removed org membership and decrypted replica");
        Ok(())
    }

    /// Insert a fresh outbound org share in the `queued` state (the "Share to Brain" action). The
    /// caller sets `meeting_id` XOR `document_id`. `content_sha256` is the plaintext-envelope hash.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_org_share(
        &self,
        id: &str,
        org_id: &str,
        meeting_id: Option<&str>,
        document_id: Option<&str>,
        kind: &str,
        title: Option<&str>,
        rev: u32,
        generation: u32,
        content_sha256: &[u8],
        created_at: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO org_shares
               (id, org_id, meeting_id, document_id, kind, title, rev, generation,
                content_sha256, item_id, state, last_error, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, 'queued', NULL, ?10, ?10)",
            rusqlite::params![
                id,
                org_id,
                meeting_id,
                document_id,
                kind,
                title,
                rev as i64,
                generation as i64,
                content_sha256,
                created_at
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Advance a queued org share to `uploaded`, recording the server-assigned `item_id`. Clears any
    /// prior error. Idempotent on the share id.
    pub fn set_org_share_uploaded(&self, id: &str, item_id: &str, updated_at: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE org_shares SET state = 'uploaded', item_id = ?2, last_error = NULL,
               updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, item_id, updated_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Mark an org share `failed` with a non-PII error string (for the launch sweep to retry / the FE
    /// to surface). The error is a fixed message + status, never note content.
    pub fn set_org_share_failed(&self, id: &str, error: &str, updated_at: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE org_shares SET state = 'failed', last_error = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, error, updated_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Set an org share's state directly (e.g. `queued` → retry a `failed`, `uploaded` →
    /// `revoke_pending`, `revoke_pending` → `revoked`). Idempotent; unknown id is a no-op.
    pub fn set_org_share_state(&self, id: &str, state: &str, updated_at: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE org_shares SET state = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, state, updated_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    fn map_org_share(r: &rusqlite::Row<'_>) -> rusqlite::Result<crate::storage::OrgShareRow> {
        Ok(crate::storage::OrgShareRow {
            id: r.get(0)?,
            org_id: r.get(1)?,
            meeting_id: r.get(2)?,
            document_id: r.get(3)?,
            kind: r.get(4)?,
            title: r.get(5)?,
            rev: r.get::<_, i64>(6)? as u32,
            generation: r.get::<_, i64>(7)? as u32,
            content_sha256: r.get(8)?,
            item_id: r.get(9)?,
            state: r.get(10)?,
            last_error: r.get(11)?,
            created_at: r.get(12)?,
            updated_at: r.get(13)?,
        })
    }

    const ORG_SHARE_COLS: &'static str =
        "id, org_id, meeting_id, document_id, kind, title, rev, generation,
         content_sha256, item_id, state, last_error, created_at, updated_at";

    /// One org share by its local id.
    pub fn get_org_share(&self, id: &str) -> Result<Option<crate::storage::OrgShareRow>> {
        let conn = self.lock();
        conn.query_row(
            &format!(
                "SELECT {} FROM org_shares WHERE id = ?1",
                Self::ORG_SHARE_COLS
            ),
            rusqlite::params![id],
            Self::map_org_share,
        )
        .optional()
        .map_err(map_err)
    }

    /// The org share bearing a given server `item_id` (for revoke-by-item + self-share dedup).
    pub fn org_share_by_item(&self, item_id: &str) -> Result<Option<crate::storage::OrgShareRow>> {
        let conn = self.lock();
        conn.query_row(
            &format!(
                "SELECT {} FROM org_shares WHERE item_id = ?1",
                Self::ORG_SHARE_COLS
            ),
            rusqlite::params![item_id],
            Self::map_org_share,
        )
        .optional()
        .map_err(map_err)
    }

    /// Every LIVE-OR-STUCK-LIVE org share anchored to a given source (`meeting_id` XOR
    /// `document_id`). Powers the re-publish-on-edit fix: one logical note may be shared into SEVERAL
    /// orgs, so this returns rows ACROSS ALL of them (never restricted to the first). Returns `uploaded`
    /// rows AND `failed` rows that still carry a non-null `item_id` — the latter is a row whose MOST
    /// RECENT republish attempt failed transiently (network blip during OCK acquire / seal / blob
    /// upload / item publish) but whose PRIOR publish is still genuinely live on the server:
    /// `set_org_share_failed` deliberately does not clear `item_id` on a republish failure (only the
    /// SUCCESS path's `reset_org_share_for_retry` does), so such a row represents a live item whose
    /// latest edit hasn't synced yet — not a dead row. Excluding it here (the pre-fix behavior) made it
    /// permanently invisible to every caller keyed off this function (the edit-save republish path, the
    /// re-share-block check, the Library share badge), so it could never self-heal and a manual re-share
    /// would mint a genuine duplicate item. A `queued`/never-published `failed` row (no `item_id`, no
    /// live server item to supersede yet — the launch sweep publishes the current plaintext for it) and
    /// a `revoked`/`revoke_pending` share (intentionally torn down; an edit must not resurrect it) are
    /// still excluded. Exactly one of `meeting_id`/`document_id` must be `Some`; both-`None` returns an
    /// empty vec (no source ⇒ nothing to republish).
    pub fn org_shares_for_source(
        &self,
        meeting_id: Option<&str>,
        document_id: Option<&str>,
    ) -> Result<Vec<crate::storage::OrgShareRow>> {
        if meeting_id.is_none() && document_id.is_none() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM org_shares
                   WHERE (state = 'uploaded' OR (state = 'failed' AND item_id IS NOT NULL))
                     AND ((?1 IS NOT NULL AND meeting_id = ?1)
                       OR (?2 IS NOT NULL AND document_id = ?2))
                   ORDER BY created_at ASC",
                Self::ORG_SHARE_COLS
            ))
            .map_err(map_err)?;
        let rows = stmt
            .query_map(
                rusqlite::params![meeting_id, document_id],
                Self::map_org_share,
            )
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Every LIVE (`uploaded`) org share of ONE exact source (`meeting_id` XOR `document_id`) in ONE
    /// org, OLDEST-FIRST (stable tie-break on `id`). The `(org, source)`-scoped twin of
    /// `org_shares_for_source` (which spans all orgs): powers the share IDEMPOTENCY guard + the
    /// duplicate collapse — `[0]` is the canonical KEEPER (earliest published, the identity other
    /// members first saw), `[1..]` are accidental duplicates to tombstone. `state = 'uploaded'` only
    /// (a queued/failed row has no live server item; a revoked one was intentionally torn down).
    /// `meeting_id`/`document_id` matched NULL-safe via `IS`; both-None ⇒ empty (no source).
    pub fn uploaded_org_shares_for_source_in_org(
        &self,
        org_id: &str,
        meeting_id: Option<&str>,
        document_id: Option<&str>,
    ) -> Result<Vec<crate::storage::OrgShareRow>> {
        if meeting_id.is_none() && document_id.is_none() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM org_shares
                   WHERE org_id = ?1 AND state = 'uploaded'
                     AND meeting_id IS ?2 AND document_id IS ?3
                   ORDER BY created_at ASC, id ASC",
                Self::ORG_SHARE_COLS
            ))
            .map_err(map_err)?;
        let rows = stmt
            .query_map(
                rusqlite::params![org_id, meeting_id, document_id],
                Self::map_org_share,
            )
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Every DUPLICATE live org share across the whole DB: an `uploaded` row that has an EARLIER
    /// `uploaded` sibling for the same `(org_id, meeting_id, document_id)` — i.e. the extras to
    /// tombstone, keeping only the earliest per group. Powers the on-launch dedup sweep that cleans
    /// duplicates created before the idempotency guard existed (e.g. a double-click on Share). Tie-break
    /// on `id` so two rows sharing a `created_at` still pick ONE deterministic keeper. NEVER returns a
    /// keeper (the earliest of its group). `meeting_id`/`document_id` grouped NULL-safe via `IS`.
    pub fn duplicate_uploaded_org_shares(&self) -> Result<Vec<crate::storage::OrgShareRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM org_shares o
                   WHERE o.state = 'uploaded'
                     AND EXISTS (
                       SELECT 1 FROM org_shares e
                        WHERE e.state = 'uploaded'
                          AND e.org_id = o.org_id
                          AND e.meeting_id IS o.meeting_id
                          AND e.document_id IS o.document_id
                          AND (e.created_at < o.created_at
                            OR (e.created_at = o.created_at AND e.id < o.id)))
                   ORDER BY o.created_at ASC, o.id ASC",
                Self::ORG_SHARE_COLS
            ))
            .map_err(map_err)?;
        let rows = stmt.query_map([], Self::map_org_share).map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Cancel (mark `revoked`) any NOT-yet-uploaded (`queued`/`failed`) org share for a given
    /// (org, source). Used after a collapse when a live `uploaded` keeper already exists for that
    /// source: a pending sibling is redundant (the source is already live) and would otherwise linger
    /// as a stuck "pending" row that the launch sweep re-attempts every start. These rows have NO server
    /// `item_id`, so cancelling is LOCAL-ONLY (no tombstone). Returns the number of rows cancelled.
    /// `meeting_id`/`document_id` matched NULL-safe via `IS`; both-None ⇒ 0 (no source).
    pub fn cancel_pending_org_shares_for_source_in_org(
        &self,
        org_id: &str,
        meeting_id: Option<&str>,
        document_id: Option<&str>,
        updated_at: &str,
    ) -> Result<usize> {
        if meeting_id.is_none() && document_id.is_none() {
            return Ok(0);
        }
        let conn = self.lock();
        let n = conn
            .execute(
                "UPDATE org_shares SET state = 'revoked', updated_at = ?4
                   WHERE org_id = ?1 AND state IN ('queued', 'failed')
                     AND meeting_id IS ?2 AND document_id IS ?3",
                rusqlite::params![org_id, meeting_id, document_id, updated_at],
            )
            .map_err(map_err)?;
        Ok(n)
    }

    /// SB-3 dedup: the EXISTING retriable (`queued`/`failed`) org-share row for a logical share key
    /// (org + meeting-or-document), if any. `share_to_org_inner` REUSES it on a re-publish instead of
    /// minting a fresh row every sweep tick — without this, each failed retry inserted a NEW row while
    /// the old one survived, so a persistently-failing share amplified rows unboundedly and a later
    /// recovery double-published. Newest-created first (a stable pick if somehow >1 exists). Uploaded/
    /// revoked/revoke_pending rows are NOT reused (an uploaded share is a distinct published item; a
    /// revoked one is intentionally torn down). `meeting_id`/`document_id` are matched exactly (both
    /// NULL-safe via `IS`).
    pub fn find_reusable_org_share(
        &self,
        org_id: &str,
        meeting_id: Option<&str>,
        document_id: Option<&str>,
    ) -> Result<Option<crate::storage::OrgShareRow>> {
        let conn = self.lock();
        conn.query_row(
            &format!(
                "SELECT {} FROM org_shares
                   WHERE org_id = ?1
                     AND meeting_id IS ?2 AND document_id IS ?3
                     AND state IN ('queued', 'failed')
                   ORDER BY created_at DESC LIMIT 1",
                Self::ORG_SHARE_COLS
            ),
            rusqlite::params![org_id, meeting_id, document_id],
            Self::map_org_share,
        )
        .optional()
        .map_err(map_err)
    }

    /// SB-3 retry re-arm: reset an EXISTING org-share row back to `queued` for a fresh publish attempt,
    /// refreshing the per-attempt fields (title/content hash/generation/timestamps) and CLEARING any
    /// item_id + last_error. Used by `share_to_org_inner` when it reuses a `find_reusable_org_share`
    /// row instead of inserting a new one — so N failed attempts stay ONE row, and a later success
    /// flips that same row to uploaded (no duplicate). Idempotent on the row id.
    pub fn reset_org_share_for_retry(
        &self,
        id: &str,
        title: Option<&str>,
        rev: u32,
        generation: u32,
        content_sha256: &[u8],
        updated_at: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE org_shares SET state = 'queued', item_id = NULL, last_error = NULL,
               title = ?2, rev = ?3, generation = ?4, content_sha256 = ?5, updated_at = ?6
             WHERE id = ?1",
            rusqlite::params![
                id,
                title,
                rev as i64,
                generation as i64,
                content_sha256,
                updated_at
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// All org shares in a given `state` (the launch sweep pulls `queued` + `revoke_pending`).
    pub fn list_org_shares_in_state(
        &self,
        state: &str,
    ) -> Result<Vec<crate::storage::OrgShareRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM org_shares WHERE state = ?1 ORDER BY created_at ASC",
                Self::ORG_SHARE_COLS
            ))
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![state], Self::map_org_share)
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Every LIVE org share across ALL sources/orgs — the un-scoped twin of `org_shares_for_source`,
    /// for callers that need the whole "is this live" set rather than one source (the Library bulk
    /// share-badge listing). Same STUCK-REPUBLISH definition of "live" as `org_shares_for_source`:
    /// `uploaded` rows AND `failed` rows that still carry a non-null `item_id` — a row whose MOST
    /// RECENT republish attempt failed transiently but whose PRIOR publish is still genuinely live on
    /// the server (`set_org_share_failed` deliberately never clears `item_id`; only the success path's
    /// `reset_org_share_for_retry` does). A `queued`/never-published `failed` row (no `item_id`) or a
    /// `revoked`/`revoke_pending` share is excluded. See `org_shares_for_source` for the full rationale.
    pub fn list_live_org_shares(&self) -> Result<Vec<crate::storage::OrgShareRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM org_shares
                   WHERE state = 'uploaded' OR (state = 'failed' AND item_id IS NOT NULL)
                   ORDER BY created_at ASC",
                Self::ORG_SHARE_COLS
            ))
            .map_err(map_err)?;
        let rows = stmt.query_map([], Self::map_org_share).map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Every org share for an org (the FE list). Newest first.
    pub fn list_org_shares_for_org(
        &self,
        org_id: &str,
    ) -> Result<Vec<crate::storage::OrgShareRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM org_shares WHERE org_id = ?1 ORDER BY created_at DESC",
                Self::ORG_SHARE_COLS
            ))
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![org_id], Self::map_org_share)
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// The ACTIVE-OR-STUCK-LIVE (queued/uploaded/revoke_pending, PLUS a `failed` row that still
    /// carries a non-null `item_id`) org shares anchored to a folder's meetings + notes, for the
    /// lock×shares warn/revoke dialog. Content-free enough for the dialog (an `(item_id?, title?)`
    /// pair per share; titles render only to the local owner).
    ///
    /// The `failed AND item_id IS NOT NULL` clause mirrors the definition of "live" established by
    /// `org_shares_for_source`/`list_live_org_shares` (see their doc comments): `set_org_share_failed`
    /// deliberately never clears `item_id` on a republish failure, so such a row's PRIOR publish is
    /// still genuinely live on the server even though the row's state says `failed`. Before this fix
    /// that shape was invisible to the lock×shares dialog and to bulk-revoke
    /// (`active_org_share_ids_for_folder`), so locking a folder with a stuck failed-republish share
    /// never warned the user and never tombstoned the still-live server item.
    pub fn active_org_shares_for_folder(
        &self,
        folder_id: &str,
    ) -> Result<Vec<(Option<String>, Option<String>)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT s.item_id, s.title
                   FROM org_shares s
                   LEFT JOIN notes n  ON n.meeting_id = s.meeting_id
                   LEFT JOIN documents d ON d.id = s.document_id
                  WHERE (s.state IN ('queued','uploaded','revoke_pending')
                     OR (s.state = 'failed' AND s.item_id IS NOT NULL))
                    AND (n.folder_id = ?1 OR d.folder_id = ?1)",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// The folder's ACTIVE-OR-STUCK-LIVE org shares as `(row_id, item_id?, title)` for bulk-revoke:
    /// an uploaded row (item_id present) is tombstoned server-side; a still-`queued` row (no item_id)
    /// is cancelled locally so the launch sweep never egresses it; a `failed` row with a non-null
    /// `item_id` (a republish attempt failed but the PRIOR publish is still live — see
    /// [`Self::active_org_shares_for_folder`]) is tombstoned server-side same as an uploaded row.
    /// Same folder join + state set as [`Self::active_org_shares_for_folder`], but carries the local
    /// row id + item id for revocation.
    pub fn active_org_share_ids_for_folder(
        &self,
        folder_id: &str,
    ) -> Result<Vec<(String, Option<String>, String)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT s.id, s.item_id, s.title
                   FROM org_shares s
                   LEFT JOIN notes n     ON n.meeting_id = s.meeting_id
                   LEFT JOIN documents d ON d.id = s.document_id
                  WHERE (s.state IN ('queued','uploaded','revoke_pending')
                     OR (s.state = 'failed' AND s.item_id IS NOT NULL))
                    AND (n.folder_id = ?1 OR d.folder_id = ?1)",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                ))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    // The org partition is a decrypted REPLICA of the org feed, living in the dedicated `org_*`
    // tables OUTSIDE the folder-lock domain (spec §"Trust model": org items are deliberately
    // org-disclosed content — no folder seal/gate applies; SQLCipher protects them at rest). All
    // writes here happen after the caller OPENED the OCK-sealed envelope (`share::org_envelope`),
    // so the plaintext title/markdown are already the member's to see. NO PII in logs (ids/counts).

    /// UPSERT one decrypted org feed item + (re)index its chunks. Idempotent on `item_id`: a re-pull
    /// of the same seq REPLACES the row and re-chunks (clean replace via `index_org_item_chunks`). A
    /// bumped `rev` (an update-share) overwrites the markdown + re-indexes. `content_sha256` is the
    /// PLAINTEXT hash (the self-share dedup key). `embedder` is the member's OWN active embedder —
    /// `None`/StubEmbedder ⇒ FTS-only (no int8 vectors written; the sync report flags `ftsOnly`).
    ///
    /// `author_user_id` (2026-07-15 root-cause fix, replaces the separate `set_org_item_author`
    /// follow-up call as the ONLY writer of this column): the server-authoritative author id, when
    /// the caller already knows it at upsert time — feed-ingest passes the feed entry's own
    /// `author_user_id`; a share-time/republish-time local-replica upsert passes the CURRENT
    /// session's own server user id (the caller IS the author in both those paths). `None` when the
    /// caller genuinely doesn't know it (a light re-upsert, or a legacy call site). The
    /// `ON CONFLICT` clause uses `COALESCE(excluded.author_user_id, org_items.author_user_id)` so a
    /// `None` re-upsert can NEVER clobber an already-stamped author back to NULL — only a `Some`
    /// value ever overwrites a previous value (and only with a fresher one, since every caller here
    /// passes the authoritative id it has). This makes new/republished rows correct from the moment
    /// they're born, with zero dependency on the `backfill_null_org_item_authors` self-heal (which
    /// stays in place as a safety net for rows that predate this fix).
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_org_item(
        &self,
        item_id: &str,
        org_id: &str,
        seq: u64,
        author_hint: &str,
        title: &str,
        markdown: &str,
        created_at: &str,
        rev: u32,
        generation: u32,
        content_sha256: &[u8],
        source_kind: Option<&str>,
        author_user_id: Option<&str>,
        embedder: Option<&dyn Embedder>,
    ) -> Result<()> {
        let prepared = Self::prepare_org_item_index(title, created_at, markdown, embedder)?;

        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        Self::upsert_org_item_prepared_tx(
            &tx,
            item_id,
            org_id,
            seq,
            author_hint,
            title,
            markdown,
            created_at,
            rev,
            generation,
            content_sha256,
            source_kind,
            author_user_id,
            &prepared,
        )?;
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Prepare one org item's chunks and optional vectors without holding a SQLite write lock or a
    /// background-commit lease. A scheduled feed sync resolves one pinned persistence embedder for
    /// its whole bounded page and calls this for each live item before any action is committed.
    pub(crate) fn prepare_org_item_index(
        title: &str,
        created_at: &str,
        markdown: &str,
        embedder: Option<&dyn Embedder>,
    ) -> Result<PreparedOrgItemIndex> {
        let chunks = crate::embed::chunk_note(title, created_at, markdown);
        let vector_blobs = match embedder {
            Some(e) if !chunks.is_empty() => Some(Self::prepare_org_vector_blobs(&chunks, e)?),
            _ => None, // model absent → FTS-only (int8 vectors come later on a re-embed).
        };
        Ok(PreparedOrgItemIndex {
            chunks,
            vector_blobs,
        })
    }

    /// Embed + quantize outside the SQLite transaction / background commit lease. The vec0 write
    /// seam receives ready-to-insert int8 blobs, keeping its critical section to compare/delete/insert.
    pub(crate) fn prepare_org_vector_blobs(
        texts: &[String],
        embedder: &dyn Embedder,
    ) -> Result<Vec<Vec<u8>>> {
        let vectors = embedder.embed_passage(texts)?;
        if vectors.len() != texts.len() {
            return Err(crate::error::AppError::Storage(format!(
                "org embedder returned {} vectors for {} chunks",
                vectors.len(),
                texts.len()
            )));
        }
        Ok(vectors
            .iter()
            .map(|vector| crate::embed::vec_to_int8_blob(vector))
            .collect())
    }

    /// Apply the author's immediate local replica refresh without destroying a feed-synced real
    /// vector index for the same immutable server item. The prepared FTS-only index is installed
    /// only when the item is absent/older. An identical or newer live row keeps its chunks/vectors
    /// byte-for-byte; only a known author id may be filled in. A tombstone is never resurrected.
    ///
    /// `superseded_item_id` is tombstoned in this SAME transaction for republish, so readers cannot
    /// observe the new local card without the old local card being evicted (or vice versa).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_local_org_replica(
        &self,
        item_id: &str,
        org_id: &str,
        seq: u64,
        author_hint: &str,
        title: &str,
        markdown: &str,
        created_at: &str,
        rev: u32,
        generation: u32,
        content_sha256: &[u8],
        source_kind: Option<&str>,
        author_user_id: Option<&str>,
        prepared: &PreparedOrgItemIndex,
        superseded_item_id: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        if !Self::org_membership_exists_tx(&tx, org_id)? {
            // The server publish may have raced a local leave/removal. The remote item remains the
            // server's concern, but withdrawn membership must never be followed by a plaintext local
            // replica resurrection.
            return Ok(());
        }
        let existing = tx
            .query_row(
                "SELECT seq, content_sha256, tombstoned FROM org_items WHERE item_id = ?1",
                rusqlite::params![item_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?.max(0) as u64,
                        r.get::<_, Option<Vec<u8>>>(1)?,
                        r.get::<_, i64>(2)? != 0,
                    ))
                },
            )
            .optional()
            .map_err(map_err)?;

        let replace = match existing {
            None => true,
            Some((_stored_seq, _stored_sha, true)) => false,
            Some((stored_seq, _, false)) if stored_seq > seq => false,
            Some((stored_seq, stored_sha, false)) if stored_seq == seq => {
                if stored_sha.as_deref() != Some(content_sha256) {
                    return Err(crate::error::AppError::Storage(
                        "conflicting org replica payload at the same feed sequence".into(),
                    ));
                }
                false
            }
            Some(_) => true,
        };

        if replace {
            Self::upsert_org_item_prepared_tx(
                &tx,
                item_id,
                org_id,
                seq,
                author_hint,
                title,
                markdown,
                created_at,
                rev,
                generation,
                content_sha256,
                source_kind,
                author_user_id,
                prepared,
            )?;
        } else if let Some(author_user_id) = author_user_id {
            tx.execute(
                "UPDATE org_items
                    SET author_user_id = COALESCE(author_user_id, ?2)
                  WHERE item_id = ?1 AND tombstoned = 0",
                rusqlite::params![item_id, author_user_id],
            )
            .map_err(map_err)?;
        }

        if let Some(old_item_id) = superseded_item_id.filter(|old| *old != item_id) {
            let _evicted = Self::tombstone_org_item_tx(&tx, old_item_id)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Commit one already-prepared live feed item and advance the org cursor to this action's exact
    /// sequence in ONE transaction. There is deliberately no fetched-page cursor parameter: a crash
    /// after this method can replay later page entries, but can never skip them.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_org_feed_item(
        &self,
        item_id: &str,
        org_id: &str,
        seq: u64,
        author_hint: &str,
        title: &str,
        markdown: &str,
        created_at: &str,
        rev: u32,
        generation: u32,
        content_sha256: &[u8],
        source_kind: Option<&str>,
        author_user_id: Option<&str>,
        prepared: &PreparedOrgItemIndex,
    ) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        if !Self::claim_org_feed_seq_tx(&tx, org_id, seq)? {
            return Ok(false);
        }
        // Tombstones are permanent for an append-only item id. Even a malformed/malicious later live
        // event may advance the org cursor, but it must never restore plaintext for a withdrawn item.
        let already_tombstoned = tx
            .query_row(
                "SELECT tombstoned FROM org_items WHERE item_id = ?1",
                rusqlite::params![item_id],
                |r| Ok(r.get::<_, i64>(0)? != 0),
            )
            .optional()
            .map_err(map_err)?
            .unwrap_or(false);
        if already_tombstoned {
            tx.commit().map_err(map_err)?;
            return Ok(false);
        }
        Self::upsert_org_item_prepared_tx(
            &tx,
            item_id,
            org_id,
            seq,
            author_hint,
            title,
            markdown,
            created_at,
            rev,
            generation,
            content_sha256,
            source_kind,
            author_user_id,
            prepared,
        )?;
        tx.commit().map_err(map_err)?;
        Ok(true)
    }

    #[allow(clippy::too_many_arguments)]
    fn upsert_org_item_prepared_tx(
        tx: &rusqlite::Transaction<'_>,
        item_id: &str,
        org_id: &str,
        seq: u64,
        author_hint: &str,
        title: &str,
        markdown: &str,
        created_at: &str,
        rev: u32,
        generation: u32,
        content_sha256: &[u8],
        source_kind: Option<&str>,
        author_user_id: Option<&str>,
        prepared: &PreparedOrgItemIndex,
    ) -> Result<()> {
        // Replace the item row (idempotent upsert). CASCADE + the explicit vec purge below clear the
        // old chunks first so a re-pull never leaves stale chunks/vectors.
        Self::purge_org_item_chunks_tx(tx, item_id)?;
        tx.execute(
            "INSERT INTO org_items
               (item_id, org_id, seq, author_hint, title, markdown, created_at, rev, generation,
                content_sha256, source_kind, author_user_id, tombstoned)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,0)
             ON CONFLICT(item_id) DO UPDATE SET
               org_id=excluded.org_id, seq=excluded.seq, author_hint=excluded.author_hint,
               title=excluded.title, markdown=excluded.markdown, created_at=excluded.created_at,
               rev=excluded.rev, generation=excluded.generation,
               content_sha256=excluded.content_sha256, source_kind=excluded.source_kind,
               author_user_id=COALESCE(excluded.author_user_id, org_items.author_user_id),
               tombstoned=0",
            rusqlite::params![
                item_id,
                org_id,
                seq as i64,
                author_hint,
                title,
                markdown,
                created_at,
                rev as i64,
                generation as i64,
                content_sha256,
                source_kind,
                author_user_id,
            ],
        )
        .map_err(map_err)?;
        {
            let mut ins_chunk = tx
                .prepare("INSERT INTO org_chunks (item_id, chunk_idx, text) VALUES (?1, ?2, ?3)")
                .map_err(map_err)?;
            // int8 vec0 → the value MUST be wrapped `vec_int8(?)` (the scale-spike partition format).
            let mut ins_vec = tx
                .prepare(
                    "INSERT INTO org_vec_chunks(chunk_id, embedding) VALUES (?1, vec_int8(?2))",
                )
                .map_err(map_err)?;
            for (idx, text) in prepared.chunks.iter().enumerate() {
                ins_chunk
                    .execute(rusqlite::params![item_id, idx as i64, text])
                    .map_err(map_err)?;
                if let Some(blobs) = &prepared.vector_blobs {
                    if let Some(blob) = blobs.get(idx) {
                        let chunk_id = tx.last_insert_rowid();
                        ins_vec
                            .execute(rusqlite::params![chunk_id, blob])
                            .map_err(map_err)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// TOMBSTONE an org item — the discard-the-result alias of the ONE eviction primitive
    /// [`Db::evict_org_item`]. Kept as the historical name used by `delete_org_item_as_author`.
    /// Idempotent — tombstoning an unknown/already-tombstoned id is fine.
    pub fn tombstone_org_item(&self, item_id: &str) -> Result<()> {
        self.evict_org_item(item_id).map(|_| ())
    }

    /// Commit one tombstone and its exact feed sequence atomically. Later page entries remain
    /// replayable if the process stops immediately after this transaction.
    pub(crate) fn commit_org_feed_tombstone(
        &self,
        org_id: &str,
        item_id: &str,
        seq: u64,
    ) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        if !Self::claim_org_feed_seq_tx(&tx, org_id, seq)? {
            return Ok(false);
        }
        let _evicted = Self::tombstone_org_item_tx(&tx, item_id)?;
        tx.commit().map_err(map_err)?;
        Ok(true)
    }

    /// Advance past one terminal, permanently un-ingestable feed entry. This is intentionally a
    /// single-action transaction and accepts only that entry's sequence, never `feed.next_seq`.
    pub(crate) fn commit_org_feed_terminal_skip(&self, org_id: &str, seq: u64) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        if !Self::claim_org_feed_seq_tx(&tx, org_id, seq)? {
            return Ok(false);
        }
        tx.commit().map_err(map_err)?;
        Ok(true)
    }

    /// EVICT one org item from this device's decrypted replica — the SINGLE eviction primitive every
    /// withdrawal path goes through (feed tombstone, the anti-entropy reconcile sweep, a local
    /// `revoke_org_share`, and a republish superseding its predecessor). Returns `true` when a LIVE
    /// local row was actually evicted by THIS call (so a caller can count real convergence work);
    /// `false` for an unknown or already-tombstoned id. Idempotent.
    ///
    /// "Evicted" means everything derived from the withdrawn content is gone: chunks + int8 vectors +
    /// the FTS tokens (via [`Self::purge_org_item_chunks_tx`]'s `_ad` trigger), the plaintext
    /// `markdown`/`title` columns, AND the item's `note_attachments` image BLOBs. Only the
    /// `tombstoned = 1` header row survives, so a later re-pull of the same append-only id stays a
    /// no-op instead of resurrecting the content.
    pub fn evict_org_item(&self, item_id: &str) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let evicted = Self::tombstone_org_item_tx(&tx, item_id)?;
        tx.commit().map_err(map_err)?;
        Ok(evicted)
    }

    /// The in-transaction body of [`Db::evict_org_item`]. Returns whether a LIVE row was evicted.
    fn tombstone_org_item_tx(tx: &rusqlite::Transaction<'_>, item_id: &str) -> Result<bool> {
        let was_live: bool = tx
            .query_row(
                "SELECT tombstoned FROM org_items WHERE item_id = ?1",
                rusqlite::params![item_id],
                |r| Ok(r.get::<_, i64>(0)? == 0),
            )
            .optional()
            .map_err(map_err)?
            .unwrap_or(false);
        Self::purge_org_item_chunks_tx(tx, item_id)?;
        // Withdrawn colleague IMAGES must go with the text. `note_attachments.org_item_id` only
        // CASCADEs on a row DELETE, and a tombstone is an UPDATE — so without this the plaintext
        // image BLOBs of a revoked item survived forever. Purged inside the same transaction as the
        // text so no reader can ever observe one half of the eviction.
        Self::purge_org_item_attachments_tx(tx, item_id)?;
        tx.execute(
            "UPDATE org_items SET tombstoned = 1, markdown = '', title = '' WHERE item_id = ?1",
            rusqlite::params![item_id],
        )
        .map_err(map_err)?;
        Ok(was_live)
    }

    /// Atomically require live membership and claim a strictly newer action sequence. A zero-row
    /// update means either membership was withdrawn or a concurrent sync already committed this/newer
    /// work; callers must perform no item mutation in either case.
    fn claim_org_feed_seq_tx(
        tx: &rusqlite::Transaction<'_>,
        org_id: &str,
        seq: u64,
    ) -> Result<bool> {
        let changed = tx
            .execute(
                "UPDATE org_state SET last_seq = ?2 WHERE org_id = ?1 AND ?2 > last_seq",
                rusqlite::params![org_id, seq as i64],
            )
            .map_err(map_err)?;
        Ok(changed == 1)
    }

    fn org_membership_exists_tx(tx: &rusqlite::Transaction<'_>, org_id: &str) -> Result<bool> {
        tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM org_state WHERE org_id = ?1)",
            rusqlite::params![org_id],
            |r| r.get::<_, bool>(0),
        )
        .map_err(map_err)
    }

    /// LEAVE-A-ORG CONSENT PURGE: drop the ENTIRE decrypted replica of one org — every `org_items`
    /// row plus its derived `org_chunks` / `org_vec_chunks` / `fts_org_chunks` tokens — in ONE atomic
    /// tx. Called by `org_leave` so a departed member keeps NO searchable copy of colleagues' shared
    /// content (leak/consent invariant). [`Db::delete_org_state`] invokes the transaction helper so
    /// membership removal and this purge commit atomically; this public standalone form remains useful
    /// for idempotent repair. Order: vec0 first (its FK-less rowid mirrors `org_chunks.id`), then
    /// `org_chunks` (whose DELETE fires the `fts_org_chunks_ad` trigger, purging the keyword tokens),
    /// then the `org_items` header rows. Idempotent; an unknown org id is a no-op. Content-free log
    /// (org id + counts, never titles/bodies).
    pub fn purge_org_replica(&self, org_id: &str) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let items = Self::purge_org_replica_tx(&tx, org_id)?;
        tx.commit().map_err(map_err)?;
        tracing::info!(target: "org", items, "purged org replica on leave");
        Ok(())
    }

    fn purge_org_replica_tx(tx: &rusqlite::Transaction<'_>, org_id: &str) -> Result<usize> {
        // vec0 KNN rows for every chunk of every item in this org (the FK-less mirror table).
        tx.execute(
            "DELETE FROM org_vec_chunks WHERE chunk_id IN
               (SELECT oc.id FROM org_chunks oc
                  JOIN org_items oi ON oi.item_id = oc.item_id
                 WHERE oi.org_id = ?1)",
            rusqlite::params![org_id],
        )
        .map_err(map_err)?;
        // Source chunks (their DELETE fires the FTS `_ad` trigger → keyword tokens purged).
        tx.execute(
            "DELETE FROM org_chunks WHERE item_id IN
               (SELECT item_id FROM org_items WHERE org_id = ?1)",
            rusqlite::params![org_id],
        )
        .map_err(map_err)?;
        // Finally the item headers (the decrypted markdown/title replica).
        let items = tx
            .execute(
                "DELETE FROM org_items WHERE org_id = ?1",
                rusqlite::params![org_id],
            )
            .map_err(map_err)?;
        Ok(items)
    }

    /// Delete an org item's `org_chunks` + `org_vec_chunks` rows within an EXISTING tx. vec0 first
    /// (its FK-less rowid mirrors `org_chunks.id`), then the source rows (whose DELETE fires the FTS
    /// `_ad` trigger, purging the tokens). Mirrors [`Db::purge_doc_chunks_tx`].
    fn purge_org_item_chunks_tx(tx: &rusqlite::Transaction<'_>, item_id: &str) -> Result<()> {
        tx.execute(
            "DELETE FROM org_vec_chunks WHERE chunk_id IN
               (SELECT id FROM org_chunks WHERE item_id = ?1)",
            rusqlite::params![item_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "DELETE FROM org_chunks WHERE item_id = ?1",
            rusqlite::params![item_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Remove every persisted org embedding while retaining the decrypted item rows, plaintext
    /// chunks and their FTS trigger-backed index. A model-space switch calls this before rebuilding,
    /// so the partition is always either empty or contains vectors from the newly pinned model —
    /// never a query-invalid mixture of old and new spaces.
    pub(crate) fn purge_all_org_vectors(&self) -> Result<usize> {
        let conn = self.lock();
        conn.execute("DELETE FROM org_vec_chunks", [])
            .map_err(map_err)
    }

    /// Keyset-read exactly one LIVE org item's existing chunks for vector-only reindex. The item row,
    /// feed cursor, metadata, chunk ids/text and FTS rows are not mutated. Callers advance with the
    /// returned `item_id`, so the full replica is never materialized as one plaintext vector in RAM.
    pub(crate) fn next_org_item_vector_batch(
        &self,
        after_item_id: Option<&str>,
    ) -> Result<Option<OrgItemVectorBatch>> {
        let conn = self.lock();
        let item = conn
            .query_row(
                "SELECT oi.item_id, oi.seq, oi.rev, oi.generation, oi.content_sha256
                   FROM org_items oi
                   JOIN org_state os ON os.org_id = oi.org_id
                  WHERE oi.tombstoned = 0
                    AND (?1 IS NULL OR oi.item_id > ?1)
                    AND EXISTS (SELECT 1 FROM org_chunks oc WHERE oc.item_id = oi.item_id)
                  ORDER BY oi.item_id ASC
                  LIMIT 1",
                rusqlite::params![after_item_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?.max(0) as u64,
                        r.get::<_, i64>(2)?.max(0) as u32,
                        r.get::<_, i64>(3)?.max(0) as u32,
                        r.get::<_, Option<Vec<u8>>>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(map_err)?;
        let Some((item_id, seq, rev, generation, content_sha256)) = item else {
            return Ok(None);
        };

        let mut stmt = conn
            .prepare(
                "SELECT id, text FROM org_chunks
                  WHERE item_id = ?1
                  ORDER BY chunk_idx ASC, id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![item_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(map_err)?;
        let mut chunk_ids = Vec::new();
        let mut texts = Vec::new();
        for row in rows {
            let (chunk_id, text) = row.map_err(map_err)?;
            chunk_ids.push(chunk_id);
            texts.push(text);
        }
        Ok(Some(OrgItemVectorBatch {
            item_id,
            chunk_ids,
            texts,
            seq,
            rev,
            generation,
            content_sha256,
        }))
    }

    /// Keyset-read one live item with at least one chunk missing its vector. This is the bounded,
    /// global repair cursor used after a partial model-switch reindex or a period of FTS-only ingest;
    /// it never purges already-valid vectors and never batches plaintext across items.
    pub(crate) fn next_missing_org_item_vector_batch(
        &self,
        after_item_id: Option<&str>,
    ) -> Result<Option<OrgItemVectorBatch>> {
        let conn = self.lock();
        let item = conn
            .query_row(
                "SELECT oi.item_id, oi.seq, oi.rev, oi.generation, oi.content_sha256
                   FROM org_items oi
                   JOIN org_state os ON os.org_id = oi.org_id
                  WHERE oi.tombstoned = 0
                    AND (?1 IS NULL OR oi.item_id > ?1)
                    AND EXISTS (
                        SELECT 1 FROM org_chunks oc
                         WHERE oc.item_id = oi.item_id
                           AND NOT EXISTS (
                               SELECT 1 FROM org_vec_chunks ov WHERE ov.chunk_id = oc.id
                           )
                    )
                  ORDER BY oi.item_id ASC
                  LIMIT 1",
                rusqlite::params![after_item_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?.max(0) as u64,
                        r.get::<_, i64>(2)?.max(0) as u32,
                        r.get::<_, i64>(3)?.max(0) as u32,
                        r.get::<_, Option<Vec<u8>>>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(map_err)?;
        let Some((item_id, seq, rev, generation, content_sha256)) = item else {
            return Ok(None);
        };

        let mut stmt = conn
            .prepare(
                "SELECT id, text FROM org_chunks
                  WHERE item_id = ?1
                  ORDER BY chunk_idx ASC, id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![item_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(map_err)?;
        let mut chunk_ids = Vec::new();
        let mut texts = Vec::new();
        for row in rows {
            let (chunk_id, text) = row.map_err(map_err)?;
            chunk_ids.push(chunk_id);
            texts.push(text);
        }
        Ok(Some(OrgItemVectorBatch {
            item_id,
            chunk_ids,
            texts,
            seq,
            rev,
            generation,
            content_sha256,
        }))
    }

    /// Read one named live item for the bounded missing-vector repair performed by org sync. This is
    /// the point lookup twin of the full-reindex keyset reader and likewise materializes plaintext for
    /// only one item at a time.
    pub(crate) fn org_item_vector_batch(
        &self,
        item_id: &str,
    ) -> Result<Option<OrgItemVectorBatch>> {
        let conn = self.lock();
        let item = conn
            .query_row(
                "SELECT oi.seq, oi.rev, oi.generation, oi.content_sha256
                   FROM org_items oi
                   JOIN org_state os ON os.org_id = oi.org_id
                  WHERE oi.item_id = ?1 AND oi.tombstoned = 0",
                rusqlite::params![item_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?.max(0) as u64,
                        r.get::<_, i64>(1)?.max(0) as u32,
                        r.get::<_, i64>(2)?.max(0) as u32,
                        r.get::<_, Option<Vec<u8>>>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(map_err)?;
        let Some((seq, rev, generation, content_sha256)) = item else {
            return Ok(None);
        };

        let mut stmt = conn
            .prepare(
                "SELECT id, text FROM org_chunks
                  WHERE item_id = ?1
                  ORDER BY chunk_idx ASC, id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![item_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(map_err)?;
        let mut chunk_ids = Vec::new();
        let mut texts = Vec::new();
        for row in rows {
            let (chunk_id, text) = row.map_err(map_err)?;
            chunk_ids.push(chunk_id);
            texts.push(text);
        }
        if chunk_ids.is_empty() {
            return Ok(None);
        }
        Ok(Some(OrgItemVectorBatch {
            item_id: item_id.to_string(),
            chunk_ids,
            texts,
            seq,
            rev,
            generation,
            content_sha256,
        }))
    }

    /// Replace only one item's org-vector rows if its canonical version/hash and ordered chunk ids
    /// are unchanged since the keyset read. Embedding happens before this method; the transaction is
    /// therefore a short compare + vec0 replace and never holds SQLite or an epoch commit lease
    /// across model inference. The hash/version check is required because SQLite may reuse rowids;
    /// legacy NULL-hash rows fall back to comparing ordered ids + full texts.
    pub(crate) fn commit_org_item_vectors_if_unchanged(
        &self,
        batch: &OrgItemVectorBatch,
        vector_blobs: &[Vec<u8>],
    ) -> Result<bool> {
        if vector_blobs.len() != batch.chunk_ids.len() {
            return Err(crate::error::AppError::Storage(format!(
                "org reindex produced {} vectors for {} chunks",
                vector_blobs.len(),
                batch.chunk_ids.len()
            )));
        }

        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let current_item = tx
            .query_row(
                "SELECT seq, rev, generation, content_sha256
                   FROM org_items WHERE item_id = ?1 AND tombstoned = 0",
                rusqlite::params![batch.item_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?.max(0) as u64,
                        r.get::<_, i64>(1)?.max(0) as u32,
                        r.get::<_, i64>(2)?.max(0) as u32,
                        r.get::<_, Option<Vec<u8>>>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(map_err)?;
        let item_unchanged = match current_item {
            Some((seq, rev, generation, content_sha256)) => {
                seq == batch.seq
                    && rev == batch.rev
                    && generation == batch.generation
                    && content_sha256.as_deref() == batch.content_sha256.as_deref()
            }
            None => false,
        };
        if !item_unchanged {
            return Ok(false);
        }

        let chunks_unchanged = if batch.content_sha256.is_some() {
            // Normal protocol rows carry a canonical content hash. Version/hash + ordered ids is a
            // compact token and avoids re-reading a potentially 16 MiB item under the epoch lease.
            let mut stmt = tx
                .prepare(
                    "SELECT id FROM org_chunks
                      WHERE item_id = ?1
                      ORDER BY chunk_idx ASC, id ASC",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![batch.item_id], |r| r.get::<_, i64>(0))
                .map_err(map_err)?;
            let mut ids = Vec::new();
            for row in rows {
                ids.push(row.map_err(map_err)?);
            }
            ids.as_slice() == batch.chunk_ids.as_slice()
        } else {
            // Legacy rows may have NULL hashes. `None == None` is not a content token, and SQLite may
            // reuse deleted rowids, so only this rare path compares ordered ids + texts in full.
            let mut stmt = tx
                .prepare(
                    "SELECT id, text FROM org_chunks
                      WHERE item_id = ?1
                      ORDER BY chunk_idx ASC, id ASC",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![batch.item_id], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(map_err)?;
            let mut chunks = Vec::new();
            for row in rows {
                chunks.push(row.map_err(map_err)?);
            }
            chunks.len() == batch.chunk_ids.len()
                && chunks
                    .iter()
                    .zip(batch.chunk_ids.iter().zip(&batch.texts))
                    .all(
                        |((current_id, current_text), (expected_id, expected_text))| {
                            current_id == expected_id && current_text == expected_text
                        },
                    )
        };
        if !chunks_unchanged {
            return Ok(false);
        }

        tx.execute(
            "DELETE FROM org_vec_chunks WHERE chunk_id IN
               (SELECT id FROM org_chunks WHERE item_id = ?1)",
            rusqlite::params![batch.item_id],
        )
        .map_err(map_err)?;
        {
            let mut insert = tx
                .prepare(
                    "INSERT INTO org_vec_chunks(chunk_id, embedding) VALUES (?1, vec_int8(?2))",
                )
                .map_err(map_err)?;
            for (chunk_id, blob) in batch.chunk_ids.iter().zip(vector_blobs) {
                insert
                    .execute(rusqlite::params![chunk_id, blob])
                    .map_err(map_err)?;
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(true)
    }

    /// The synced feed cursor (`org_state.last_seq`) for an org — the max `seq` ingested so far.
    /// Returns 0 when the org is unknown/never synced. (Companion to Core's monotonic
    /// [`Db::set_org_last_seq`].)
    pub fn org_last_seq_for(&self, org_id: &str) -> Result<u64> {
        Ok(self
            .get_org_state(org_id)?
            .map(|s| s.last_seq.max(0) as u64)
            .unwrap_or(0))
    }

    // ── ANTI-ENTROPY RECONCILE (2026-07-26) ───────────────────────────────────────────────────────
    // A SECOND, slow cursor per org, wholly independent of the live `last_seq` pull cursor. The
    // server tombstones an item WITHOUT minting a fresh `seq`, and the feed is `seq > cursor` — so a
    // member already past that seq NEVER sees the tombstone on the live cursor and keeps a searchable
    // decrypted replica of withdrawn content forever. This cursor restarts at 0 and walks the whole
    // feed in small bounded steps so every record is eventually re-observed. NOTHING here writes
    // `last_seq`: the live pull's position is never rewound or clobbered.

    /// How far the slow reconcile walk has got in the CURRENT pass (0 = at the start of a pass).
    /// Returns 0 for an unknown org.
    pub fn org_reconcile_seq_for(&self, org_id: &str) -> Result<u64> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT reconcile_seq FROM org_state WHERE org_id = ?1",
                rusqlite::params![org_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map_err(map_err)?
            .map(|s| s.max(0) as u64)
            .unwrap_or(0))
    }

    /// Advance the reconcile cursor. A no-op for an unknown org (a leave mid-sweep must not
    /// resurrect state). Deliberately NOT monotonic-guarded: a pass legitimately restarts at 0.
    pub fn set_org_reconcile_seq(&self, org_id: &str, seq: u64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE org_state SET reconcile_seq = ?2 WHERE org_id = ?1",
            rusqlite::params![org_id, seq as i64],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Mark a FULL pass complete: stamp `reconcile_pass_at` and rewind the reconcile cursor to 0 so
    /// the next pass starts again from the head of the feed. `last_seq` is untouched.
    pub fn complete_org_reconcile_pass(&self, org_id: &str, at: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE org_state SET reconcile_seq = 0, reconcile_pass_at = ?2 WHERE org_id = ?1",
            rusqlite::params![org_id, at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// RFC3339 stamp of the last COMPLETED reconcile pass, or `None` when none has finished yet.
    pub fn org_reconcile_pass_at(&self, org_id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT reconcile_pass_at FROM org_state WHERE org_id = ?1",
                rusqlite::params![org_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(map_err)?
            .flatten())
    }

    /// What this device currently holds for one org item, so the reconcile sweep can decide whether a
    /// live feed record needs a blob fetch at all. `None` ⇒ the item is not held locally.
    pub(crate) fn org_replica_state(&self, item_id: &str) -> Result<Option<OrgReplicaState>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT tombstoned, content_sha256 FROM org_items WHERE item_id = ?1",
            rusqlite::params![item_id],
            |r| {
                Ok(OrgReplicaState {
                    tombstoned: r.get::<_, i64>(0)? != 0,
                    content_sha256: r.get::<_, Option<Vec<u8>>>(1)?,
                })
            },
        )
        .optional()
        .map_err(map_err)
    }

    /// Commit one already-prepared live item found by the RECONCILE sweep. Identical to
    /// [`Db::commit_org_feed_item`] except it NEVER touches `org_state.last_seq` — the sweep walks
    /// sequences that are (usually) already behind the live cursor, so claiming them would rewind or
    /// no-op the live pull. Membership is still required in the SAME transaction (a leave mid-sweep
    /// must not resurrect plaintext), and an existing tombstone is still permanent.
    ///
    /// Returns `true` when the row was actually (re)written.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_org_reconcile_item(
        &self,
        item_id: &str,
        org_id: &str,
        seq: u64,
        author_hint: &str,
        title: &str,
        markdown: &str,
        created_at: &str,
        rev: u32,
        generation: u32,
        content_sha256: &[u8],
        source_kind: Option<&str>,
        author_user_id: Option<&str>,
        prepared: &PreparedOrgItemIndex,
    ) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        if !Self::org_membership_exists_tx(&tx, org_id)? {
            return Ok(false);
        }
        let already_tombstoned = tx
            .query_row(
                "SELECT tombstoned FROM org_items WHERE item_id = ?1",
                rusqlite::params![item_id],
                |r| Ok(r.get::<_, i64>(0)? != 0),
            )
            .optional()
            .map_err(map_err)?
            .unwrap_or(false);
        if already_tombstoned {
            return Ok(false);
        }
        Self::upsert_org_item_prepared_tx(
            &tx,
            item_id,
            org_id,
            seq,
            author_hint,
            title,
            markdown,
            created_at,
            rev,
            generation,
            content_sha256,
            source_kind,
            author_user_id,
            prepared,
        )?;
        tx.commit().map_err(map_err)?;
        Ok(true)
    }

    /// GATED-FREE (no folder lock applies to org items) semantic KNN over the int8 org partition:
    /// the top-`k` nearest `org_vec_chunks` for the int8-quantized `query_vec`, joined to their
    /// (non-tombstoned) items, deduped to one hit per item (nearest). `query_vec` is the member's OWN
    /// f32 query embedding — it is int8-quantized here so it is comparable to the stored int8 vectors.
    ///
    /// PER-INSTANCE ORG TOGGLE: joined to `org_state` and filtered on `context_enabled = 1` — a
    /// disabled org's chunks are EXCLUDED at the SQL level, never read into Rust at all. This is the
    /// hard data-level gate (not a UI hide): a caller cannot accidentally surface a disabled org's
    /// content by forgetting to filter it downstream.
    ///
    /// `min_cosine` (S1) is the OPT-IN relevance floor for the int8 leg. The stored vectors are
    /// `round(unit·127)`, so the vec0 L2 distance is over 127-scaled vectors — divide by `127.0`
    /// BEFORE the cosine map (`cos ≈ cosine_from_l2_distance(d/127)`; a DIFFERENT distribution from
    /// the f32 legs, hence its own const). Sentinel `0.0` = NO floor. Applied AFTER the
    /// tombstone/context-enabled SQL gate — it can only ever REMOVE below-floor rows.
    pub fn search_org_chunks_knn(
        &self,
        query_vec: &[f32],
        k: i64,
        min_cosine: f32,
    ) -> Result<Vec<OrgChunkHit>> {
        if query_vec.is_empty() || k <= 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        // KNN isolated to the vec0 table in a CTE (a vec0 query allows a single MATCH+k); the item
        // columns + the tombstone/context-enabled filters are joined OUTSIDE it.
        let sql = "WITH knn(chunk_id, distance) AS (
                 SELECT chunk_id, distance FROM org_vec_chunks
                  WHERE embedding MATCH vec_int8(?1) AND k = ?2
                  ORDER BY distance
             )
             SELECT oi.item_id, oi.author_hint, oi.title, oc.text, oi.content_sha256, knn.distance
               FROM knn
               JOIN org_chunks oc ON oc.id = knn.chunk_id
               JOIN org_items oi ON oi.item_id = oc.item_id
               JOIN org_state os ON os.org_id = oi.org_id
              WHERE oi.tombstoned = 0 AND os.context_enabled = 1
              ORDER BY knn.distance ASC, oi.item_id ASC";
        let blob = crate::embed::vec_to_int8_blob(query_vec);
        let mut stmt = conn.prepare(sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![blob, k], |row| {
                let hit = OrgChunkHit {
                    item_id: row.get(0)?,
                    author_hint: row.get(1)?,
                    title: row.get(2)?,
                    snippet: row.get(3)?,
                    content_sha256: row.get::<_, Option<Vec<u8>>>(4)?.unwrap_or_default(),
                };
                let distance: f64 = row.get(5)?;
                Ok((hit, distance))
            })
            .map_err(map_err)?;
        // Drop below-floor candidates BEFORE the per-item dedup so the winner is the nearest survivor.
        // int8 rescale: the distance is over 127-scaled vectors ⇒ divide by 127 before the cosine map.
        let filtered = rows.filter_map(|r| match r {
            Ok((hit, distance)) => {
                if min_cosine > 0.0
                    && crate::links::cosine_from_l2_distance((distance as f32) / 127.0) < min_cosine
                {
                    None
                } else {
                    Some(Ok(hit))
                }
            }
            Err(e) => Some(Err(e)),
        });
        Self::dedup_org_hits_by_item(filtered)
    }

    /// KEYWORD (FTS5/BM25) leg over the org partition — the model-free twin of
    /// [`Db::search_org_chunks_knn`], so org text is reachable on a DEFAULT install (no e5 model).
    /// Same `context_enabled = 1` per-instance org filter as the KNN leg — see its doc.
    ///
    /// CRITICAL (scale-spike finding #2): the `LIMIT` is PUSHED DOWN into the SQL (bm25-ordered),
    /// NOT applied in Rust after reading every match — the unbounded production reader hit an 8.8 s
    /// p95 tail at 1M chunks. We over-fetch a small multiple of `limit` (so per-item dedup still has
    /// candidates) then cap in Rust; the SQL ceiling is the real bound.
    pub fn search_org_chunks_fts(&self, query: &str, limit: i64) -> Result<Vec<OrgChunkHit>> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let q = query.trim();
        let Some(and_expr) = fts_match_query(q) else {
            return Ok(Vec::new()); // punctuation-only / empty query → no hits, never an FTS error.
        };
        let conn = self.lock();
        // Over-fetch a bounded multiple of `limit` so per-item dedup has candidates, but keep the SQL
        // LIMIT as the hard bound (spike #2). 8× is generous for a per-item dedup at small `limit`.
        let sql_cap = limit.saturating_mul(8).clamp(limit, 512);
        let sql = "SELECT oi.item_id, oi.author_hint, oi.title, oc.text, oi.content_sha256,
                          bm25(fts_org_chunks) AS rank
               FROM fts_org_chunks
               JOIN org_chunks oc ON oc.id = fts_org_chunks.rowid
               JOIN org_items oi ON oi.item_id = oc.item_id
               JOIN org_state os ON os.org_id = oi.org_id
              WHERE fts_org_chunks MATCH ?1 AND oi.tombstoned = 0 AND os.context_enabled = 1
              ORDER BY rank ASC, oi.item_id ASC
              LIMIT ?2";
        let mut stmt = conn.prepare(sql).map_err(map_err)?;
        // Collect the (already-gated) candidate rows for a given match expression.
        let run = |stmt: &mut rusqlite::Statement, expr: &str| -> Result<Vec<OrgChunkHit>> {
            let rows = stmt
                .query_map(rusqlite::params![expr, sql_cap], |row| {
                    Ok(OrgChunkHit {
                        item_id: row.get(0)?,
                        author_hint: row.get(1)?,
                        title: row.get(2)?,
                        snippet: row.get(3)?,
                        content_sha256: row.get::<_, Option<Vec<u8>>>(4)?.unwrap_or_default(),
                    })
                })
                .map_err(map_err)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(map_err)?);
            }
            Ok(out)
        };
        // S2 AND→OR fallback: implicit-AND matched nothing ⇒ retry with the content-word OR twin.
        // Fires only on an empty AND result — never widens a successful query.
        let mut rows_vec = run(&mut stmt, &and_expr)?;
        if rows_vec.is_empty() {
            if let Some(any_expr) = fts_match_query_any(q) {
                if any_expr != and_expr {
                    // #19 — the OR leg needs a RELEVANCE FLOOR. Without one, a six-word question
                    // matched anything containing ONE of its words: a real query for
                    // "hybrid mode source of truth Kong Operator" returned two notes titled "Kongo"
                    // and "Kong test" whose bodies were scraped Murmur UI text. The tool description
                    // actively nudges an agent here as a fallback, so following the docs produced
                    // pure noise.
                    //
                    // The floor is COVERAGE, not a bm25 constant, deliberately: a bm25 threshold
                    // would need calibrating against a real vault before anyone could say what it
                    // means, while "matched at least half the content words" is deterministic,
                    // explainable, and cannot silently drift.
                    rows_vec = run(&mut stmt, &any_expr)?
                        .into_iter()
                        .filter(|h| passes_or_leg_floor(q, h))
                        .collect();
                }
            }
        }
        let mut hits = Self::dedup_org_hits_by_item(rows_vec.into_iter().map(Ok))?;
        hits.truncate(limit as usize);
        Ok(hits)
    }

    /// Dedup a stream of org chunk hits to ONE per item (first-seen = best-ranked, since callers
    /// order by distance/bm25 ascending). Shared by both retrieval legs.
    fn dedup_org_hits_by_item(
        rows: impl Iterator<Item = rusqlite::Result<OrgChunkHit>>,
    ) -> Result<Vec<OrgChunkHit>> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut hits = Vec::new();
        for r in rows {
            let hit = r.map_err(map_err)?;
            if !seen.insert(hit.item_id.clone()) {
                continue;
            }
            hits.push(hit);
        }
        Ok(hits)
    }

    /// The full decrypted org item (for the read-only FE viewer). `None` for an unknown, TOMBSTONED,
    /// OR per-instance-DISABLED item's org (a stale citation/bookmark to `/org-item/:id` must not read
    /// through the toggle — same `context_enabled = 1` gate as `search_org_chunks_knn`/`_fts`). No lock
    /// gate otherwise — org items are deliberately org-disclosed content.
    pub fn get_org_item(
        &self,
        item_id: &str,
    ) -> Result<Option<crate::storage::models::OrgItemDetail>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT oi.item_id, oi.author_hint, oi.title, oi.created_at, oi.rev, oi.markdown
               FROM org_items oi
               JOIN org_state os ON os.org_id = oi.org_id
              WHERE oi.item_id = ?1 AND oi.tombstoned = 0 AND os.context_enabled = 1",
            rusqlite::params![item_id],
            |r| {
                Ok(crate::storage::models::OrgItemDetail {
                    item_id: r.get(0)?,
                    author_hint: r.get(1)?,
                    title: r.get(2)?,
                    created_at: r.get(3)?,
                    rev: r.get::<_, i64>(4)? as u32,
                    markdown: r.get(5)?,
                    // The DB layer has no session context — the `org_get_item` command computes the real
                    // value by comparing the stored `author_user_id` with the caller's `server_user_id`.
                    editable: false,
                })
            },
        )
        .optional()
        .map_err(map_err)
    }

    /// Stamp an org item's `author_user_id` (the server account id of its author, from the feed). Called
    /// right after `upsert_org_item` at feed-ingest so a second machine can recognise its OWN items and
    /// offer edit-in-place. Idempotent; a no-op for an unknown id. (2026-07-14.)
    pub fn set_org_item_author(&self, item_id: &str, author_user_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE org_items SET author_user_id = ?2 WHERE item_id = ?1",
            rusqlite::params![item_id, author_user_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The item ids of this org's LIVE (non-tombstoned) local replica rows that are still missing
    /// `author_user_id` — the stale-ingest gap (rows ingested before the column/stamping existed, or
    /// via the local-replica upsert at share/republish time, whose cursor has already advanced past
    /// them so a normal cursor-based feed pull never re-visits them). Retained for callers that need
    /// the complete id set; scheduled sync uses the bounded seq-aware helper below and never re-pulls
    /// the full feed. (2026-07-15.)
    pub fn org_item_ids_with_null_author(&self, org_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT item_id FROM org_items
                   WHERE org_id = ?1 AND tombstoned = 0 AND author_user_id IS NULL",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![org_id], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Oldest live items whose author still needs the one-page feed repair, paired with their stored
    /// feed sequence. Starting the side-query immediately before the smallest sequence makes bounded
    /// progress without a second persistent cursor or a full-history replay on every tick.
    pub(crate) fn org_items_with_null_author_seq(
        &self,
        org_id: &str,
        limit: i64,
    ) -> Result<Vec<(String, u64)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT item_id, seq FROM org_items
                   WHERE org_id = ?1 AND tombstoned = 0 AND author_user_id IS NULL
                  ORDER BY seq ASC, item_id ASC
                  LIMIT ?2",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![org_id, limit.max(0)], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_err)?);
        }
        Ok(out)
    }

    /// The context the `org_update_own_item` egress command needs to re-publish an edited org item the
    /// caller authored (org id, current rev, original created_at + source_kind, stored author id). `None`
    /// for an unknown / tombstoned item. (2026-07-14.)
    pub fn org_item_edit_ctx(
        &self,
        item_id: &str,
    ) -> Result<Option<crate::storage::models::OrgItemEditCtx>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT org_id, rev, created_at, source_kind, author_user_id
               FROM org_items WHERE item_id = ?1 AND tombstoned = 0",
            rusqlite::params![item_id],
            |r| {
                Ok(crate::storage::models::OrgItemEditCtx {
                    org_id: r.get(0)?,
                    rev: r.get::<_, i64>(1)? as u32,
                    created_at: r.get(2)?,
                    source_kind: r.get::<_, Option<String>>(3)?,
                    author_user_id: r.get::<_, Option<String>>(4)?,
                })
            },
        )
        .optional()
        .map_err(map_err)
    }

    /// The browsable LIST of one org's live (non-tombstoned) items — headers only (no `markdown`
    /// body; that's [`Db::get_org_item`]). Newest-first by feed `seq`. This is what lets a member SEE
    /// what colleagues shared into the org instead of only search-hitting it. Org items are
    /// deliberately org-disclosed content (no folder lock gate applies); the COMMAND layer re-checks
    /// the caller is a local member of `org_id` before calling this.
    ///
    /// `kind` is now populated DIRECTLY from the stored `source_kind` column (opened off the item's
    /// `OrgEnvelope` at ingest — see `upsert_org_item`) for EVERY item, not just ones this device
    /// published: a v2-envelope item from a colleague now classifies correctly. Stays `None` for a row
    /// ingested before this column existed, or from a peer still on an old v1-only client (honest
    /// "unclassified", never guessed). `list_org_items_inner` may still override this with the
    /// owned-item resolver for the caller's OWN items (correct even when the column is somehow null).
    pub fn list_org_items(
        &self,
        org_id: &str,
    ) -> Result<Vec<crate::storage::models::OrgItemHeader>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT item_id, title, author_hint, created_at, seq, source_kind
                   FROM org_items
                  WHERE org_id = ?1 AND tombstoned = 0
                  ORDER BY seq DESC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![org_id], |r| {
                Ok(crate::storage::models::OrgItemHeader {
                    item_id: r.get(0)?,
                    title: r.get(1)?,
                    author_hint: r.get(2)?,
                    created_at: r.get(3)?,
                    seq: r.get::<_, i64>(4)? as u64,
                    // Direct from storage now (see doc comment above); `list_org_items_inner` may still
                    // enrich/override for the caller's own items via the local `org_shares` resolver.
                    kind: r.get::<_, Option<String>>(5)?,
                    owned_source: None,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// COUNT of one org's live (non-tombstoned) RECEIVED items — the size of the local org replica
    /// (what colleagues shared IN). Distinct from the outbound `org_shares` count (what THIS member
    /// published OUT); the two were conflated so the Settings item count showed the caller's own
    /// uploads and read "0 items" to a receiver. Content-free.
    ///
    /// PER-INSTANCE ORG TOGGLE: joined to `org_state` and filtered on `context_enabled = 1` — same
    /// gate as `search_org_chunks_knn`/`_fts`/`get_org_item`/`list_org_items_inner`. Without this a
    /// disabled org's `received_count` stayed stale/inflated (the raw local-replica row count) even
    /// though every other read of the same table (search, browse) correctly reports it as empty —
    /// the count and the actual gated content must agree for the same org.
    pub fn count_org_items(&self, org_id: &str) -> Result<u32> {
        let conn = self.lock();
        conn.query_row(
            "SELECT COUNT(*)
               FROM org_items oi
               JOIN org_state os ON os.org_id = oi.org_id
              WHERE oi.org_id = ?1 AND oi.tombstoned = 0 AND os.context_enabled = 1",
            rusqlite::params![org_id],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n as u32)
        .map_err(map_err)
    }

    /// Every non-null `content_sha256` from the local `org_shares` rows (across all orgs the user has
    /// shared into) — the SELF-SHARE dedup key set. A retrieval hit whose hash is in this set is the
    /// caller's OWN published item and is relabelled/dropped so a member never sees their own share
    /// echoed back as an "org" result. Content-free (opaque hashes only).
    pub fn all_org_shared_content_hashes(&self) -> Result<Vec<Vec<u8>>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT content_sha256 FROM org_shares WHERE content_sha256 IS NOT NULL")
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, Vec<u8>>(0))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// LIVE org item ids that still LACK any int8 vector (chunks present but no `org_vec_chunks`) —
    /// the re-embed backlog once a real embedder appears on a member that ingested FTS-only. Bounded
    /// by `limit`. Empty when every live item is already embedded (or FTS-only with no chunks).
    pub fn org_items_needing_embed(&self, org_id: &str, limit: i64) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT oc.item_id
                   FROM org_chunks oc
                   JOIN org_items oi ON oi.item_id = oc.item_id
                   JOIN org_state os ON os.org_id = oi.org_id
                  WHERE oi.org_id = ?1 AND oi.tombstoned = 0
                    AND NOT EXISTS (SELECT 1 FROM org_vec_chunks v WHERE v.chunk_id = oc.id)
                  LIMIT ?2",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![org_id, limit], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }
}

/// The RELEVANCE FLOOR for the org-search OR-fallback leg (#19).
///
/// The AND leg is already precise — every content word had to appear. The OR twin exists so a
/// slightly-off phrasing still finds something, but on its own it admits a hit that matched exactly
/// ONE word out of six. With no floor that produced the reported failure: the query
/// "hybrid mode source of truth Kong Operator" returned notes titled "Kongo" and "Kong test". The
/// tool description actively nudges an agent here as a fallback, so following the docs yielded pure
/// noise.
///
/// The floor is COVERAGE, not a bm25 constant, deliberately: require the chunk to actually contain
/// at least HALF the query's content words (minimum two; a one-word query is exempt because there
/// is nothing to be partial about). Deterministic and explainable — a bm25 threshold would need
/// calibrating against a real vault before anyone could say what a given value means, and would
/// then drift silently as the corpus grew.
pub(crate) fn passes_or_leg_floor(query: &str, hit: &crate::storage::models::OrgChunkHit) -> bool {
    if is_mostly_interface_text(&hit.snippet) {
        return false;
    }
    let haystack = format!("{} {}", hit.title, hit.snippet).to_lowercase();
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .filter(|t| t.chars().count() >= 3 && !crate::summarize::related_context::is_stopword(t))
        .collect();
    if terms.len() <= 1 {
        return true;
    }
    let matched = terms
        .iter()
        .filter(|t| haystack.contains(t.as_str()))
        .count();
    // HALF, ROUNDED DOWN, never below one. The rounding direction is load-bearing and was found by
    // an existing test rather than reasoned into: `s2_and_to_or_fallback_recovers_multiword_miss_org`
    // pins that the two-word query "etykieta parcel" must still recover an item sharing only
    // "parcel" — a Polish query against an English note, where the domain term is the ONLY word
    // that can match. Rounding UP would require 2 of 2 and silently destroy that cross-language
    // recall.
    //
    // Proportion is what separates signal from the reported noise: 1 of 2 is 50% and legitimate;
    // 1 of 6 is 17% and is how "hybrid mode source of truth Kong Operator" reached a note titled
    // "Kongo". Short queries are already specific; long ones must actually be covered.
    matched >= (terms.len() / 2).max(1)
}

/// Whether a chunk is mostly UI chrome rather than authored prose.
///
/// The reported noise notes were scraped Murmur INTERFACE text — "Ask Brain to edit…", "Refine",
/// "Shorten", "Translate", "✕", "↵". That is never prose a colleague wrote, and never an answer.
fn is_mostly_interface_text(text: &str) -> bool {
    const UI_TOKENS: &[&str] = &[
        "ask brain to edit",
        "refine",
        "shorten",
        "translate",
        "regenerate",
        "copy to clipboard",
        "✕",
        "↵",
    ];
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    let lc = t.to_lowercase();
    let ui_chars: usize = UI_TOKENS
        .iter()
        .filter(|tok| lc.contains(*tok))
        .map(|tok| tok.len())
        .sum();
    // More than half the body being known interface strings ⇒ it is a screen, not a note.
    ui_chars * 2 > t.len()
}

#[cfg(test)]
mod or_leg_floor_tests {
    use super::*;
    use crate::storage::models::OrgChunkHit;

    fn hit(title: &str, snippet: &str) -> OrgChunkHit {
        OrgChunkHit {
            item_id: "i1".into(),
            author_hint: "colleague".into(),
            title: title.into(),
            snippet: snippet.into(),
            content_sha256: Vec::new(),
        }
    }

    /// R16/#19 (regression). The EXACT reported failure: a six-word question matched notes sharing
    /// ONE word, because the OR-fallback leg had no floor at all.
    #[test]
    fn one_shared_word_out_of_six_is_not_a_match() {
        let q = "hybrid mode source of truth Kong Operator";
        assert!(
            !passes_or_leg_floor(q, &hit("Kongo", "a trip report about Kongo")),
            "matching only the token Kong must not qualify"
        );
        assert!(
            !passes_or_leg_floor(q, &hit("Kong test", "some unrelated scratch note")),
            "a title-only token overlap must not qualify"
        );
        // A genuinely relevant colleague note still passes — the floor must not kill recall.
        assert!(
            passes_or_leg_floor(
                q,
                &hit(
                    "Kong Operator design",
                    "we agreed the Kong Operator is the source of truth in hybrid mode"
                )
            ),
            "a note covering most of the query must still be found"
        );
    }

    /// A single-word query is exempt — there is nothing to be partial about.
    #[test]
    fn a_one_word_query_is_not_floored() {
        assert!(passes_or_leg_floor("Konnect", &hit("Konnect", "anything")));
    }

    /// The floor must NOT destroy the cross-language recall the AND→OR fallback exists for.
    ///
    /// `s2_and_to_or_fallback_recovers_multiword_miss_org` pins this at the reader level: a Polish
    /// query against an English note, where the domain term is the only word that CAN match. One of
    /// two words is 50% and legitimate; the rejected noise case is one of six, which is 17%. Pinned
    /// here too so the rounding direction cannot be "simplified" to `div_ceil` later.
    #[test]
    fn a_two_word_query_still_matches_on_its_one_shared_domain_term() {
        assert!(passes_or_leg_floor(
            "etykieta parcel",
            &hit("Parcels", "parcel size delivery schedule")
        ));
    }

    /// Scraped INTERFACE text is never an answer, however many words it happens to share.
    #[test]
    fn scraped_interface_text_is_rejected() {
        assert!(!passes_or_leg_floor(
            "hybrid mode source of truth Kong Operator",
            &hit("Kong test", "Ask Brain to edit… Refine Shorten Translate ✕ ↵")
        ));
    }
}
