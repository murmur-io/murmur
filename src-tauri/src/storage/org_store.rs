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

use std::collections::{HashMap, HashSet};

use rusqlite::OptionalExtension;

use crate::embed::Embedder;
use crate::error::Result;
use crate::share::org_dto::parse_stable_uuid;
use crate::storage::db::{
    fts_match_content_terms_any, fts_match_query, fts_unicode61_content_terms,
    insert_share_egress_dispatch_tx, map_err, Db,
};
use crate::storage::models::OrgChunkHit;

/// Keep the fallback comfortably below SQLite's compound-SELECT and bind-parameter ceilings.
/// Natural-language org queries should never approach this; taking the first bounded unique terms
/// also prevents an untrusted query from making SQL preparation scale without bound.
const MAX_ORG_FTS_CONTENT_TERMS: usize = 32;

/// A received org item crosses from the SQLCipher-backed replica into an ordinary local note or
/// meeting. Bound that copy independently of the wire parser: old/corrupt local replicas must not
/// turn one click into an unbounded allocation. These ceilings match the local attachment owner
/// limits and stay within the authenticated org-note bundle ceiling.
const MAX_RECEIVED_ORG_IMPORT_MARKDOWN_BYTES: usize = 8 * 1024 * 1024;
const MAX_RECEIVED_ORG_IMPORT_ATTACHMENTS: usize = crate::storage::MAX_ATTACHMENTS_PER_OWNER;
const MAX_RECEIVED_ORG_IMPORT_ATTACHMENT_BYTES: usize = crate::storage::MAX_ATTACHMENT_BYTES;
const MAX_RECEIVED_ORG_IMPORT_ATTACHMENT_TOTAL_BYTES: usize =
    crate::storage::MAX_ATTACHMENT_BYTES_PER_OWNER;
const MAX_RECEIVED_ORG_IMPORT_TOTAL_BYTES: usize = murmur_protocol::caps::MAX_NOTE_BUNDLE_BYTES;

fn require_stable_uuid(value: &str, error: &'static str) -> Result<()> {
    parse_stable_uuid(value)
        .map(|_| ())
        .ok_or_else(|| crate::error::AppError::InvalidArg(error.into()))
}

/// Chunk/vector material for one org item, prepared before entering the short feed-commit
/// transaction. Keeping model inference on this side of the transaction is load-bearing: recording
/// admission may invalidate the work, while a committed feed action and its cursor must remain one
/// indivisible SQLite mutation.
pub(crate) struct PreparedOrgItemIndex {
    chunks: Vec<String>,
    vector_blobs: Option<Vec<Vec<u8>>>,
}

/// Result of a stable-document metadata commit. `changed` preserves each caller's historical
/// boolean (feed/reconcile applied, local predecessor evicted); `visibility_reduced` independently
/// reports that a previously readable authoritative head was demoted in the same transaction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OrgMetadataCommitOutcome {
    pub(crate) changed: bool,
    pub(crate) visibility_reduced: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OrgAccessAttemptRow {
    pub(crate) dispatch_id: String,
    pub(crate) org_id: String,
    pub(crate) doc_id: String,
    pub(crate) old_access: String,
    pub(crate) new_access: String,
    pub(crate) actor_user_id: String,
    pub(crate) owner_user_id: String,
}

/// Optional plaintext projection carried by an already-authenticated republish completion. All
/// expensive validation/index preparation happens before the transaction; the storage seam only
/// installs these exact bytes after the durable dispatch witness still matches.
pub(crate) struct OrgRepublishProjection<'a> {
    pub(crate) item_id: &'a str,
    pub(crate) seq: u64,
    pub(crate) author_hint: &'a str,
    pub(crate) title: &'a str,
    pub(crate) markdown: &'a str,
    pub(crate) created_at: &'a str,
    pub(crate) source_kind: Option<&'a str>,
    pub(crate) author_user_id: Option<&'a str>,
    pub(crate) prepared: &'a PreparedOrgItemIndex,
    pub(crate) attachments: &'a [crate::storage::IncomingAttachment],
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
    pub(crate) projection_sha256: Option<Vec<u8>>,
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
    pub(crate) fn begin_org_source_closure(
        &self,
        source_kind: &str,
        source_id: &str,
    ) -> Result<()> {
        if !matches!(source_kind, "meeting" | "document") || source_id.trim().is_empty() {
            return Err(crate::error::AppError::InvalidArg(
                "invalid organization share closure source".into(),
            ));
        }
        let conn = self.lock();
        conn.execute(
            "INSERT INTO org_share_closures(scope_kind,scope_id,phase,created_at)
             VALUES(?1,?2,'closing',?3)
             ON CONFLICT(scope_kind,scope_id) DO NOTHING",
            rusqlite::params![source_kind, source_id, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub(crate) fn begin_org_folder_closure(&self, folder_id: &str) -> Result<bool> {
        let conn = self.lock();
        let changed = conn
            .execute(
                "INSERT INTO org_share_closures(scope_kind,scope_id,phase,created_at)
             VALUES('folder',?1,'closing',?2)
             ON CONFLICT(scope_kind,scope_id) DO NOTHING",
                rusqlite::params![folder_id, chrono::Utc::now().to_rfc3339()],
            )
            .map_err(map_err)?;
        Ok(changed == 1)
    }

    pub(crate) fn clear_org_folder_closure(&self, folder_id: &str) -> Result<()> {
        self.lock()
            .execute(
                "DELETE FROM org_share_closures WHERE scope_kind='folder' AND scope_id=?1",
                [folder_id],
            )
            .map_err(map_err)?;
        Ok(())
    }

    /// Retire a source's closure record, making that id WRITABLE again.
    ///
    /// The closure exists so a teardown cannot race a concurrent write and so the org-sync tick can
    /// never re-pull an item the user deleted. It is keyed on the source ID, and the delete path
    /// leaves it in place — correct while the id is gone for good.
    ///
    /// TRASH RESTORE is the one case where a "deleted" id legitimately comes BACK, and the closure
    /// then blocks the restore outright (`RAISE(ABORT,'meeting source is closing')` from the
    /// `closing_*_guard` triggers). Clearing it is safe precisely because the delete already
    /// REVOKED every live org share before destroying the rows (revoke-before-delete), so there is
    /// no live server item left for the sync tick to re-pull: the restored content is local-only and
    /// unshared until the user publishes it again.
    pub(crate) fn clear_org_source_closure(
        &self,
        source_kind: &str,
        source_id: &str,
    ) -> Result<()> {
        if !matches!(source_kind, "meeting" | "document") || source_id.trim().is_empty() {
            return Err(crate::error::AppError::InvalidArg(
                "invalid organization share closure source".into(),
            ));
        }
        self.lock()
            .execute(
                "DELETE FROM org_share_closures WHERE scope_kind=?1 AND scope_id=?2",
                rusqlite::params![source_kind, source_id],
            )
            .map_err(map_err)?;
        Ok(())
    }

    pub(crate) fn complete_org_closure(&self, scope_kind: &str, scope_id: &str) -> Result<()> {
        self.lock()
            .execute(
                "UPDATE org_share_closures SET phase='closed'
              WHERE scope_kind=?1 AND scope_id=?2",
                rusqlite::params![scope_kind, scope_id],
            )
            .map_err(map_err)?;
        Ok(())
    }

    pub(crate) fn org_folder_closure_exists(&self, folder_id: &str) -> Result<bool> {
        self.lock()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM org_share_closures
                  WHERE scope_kind='folder' AND scope_id=?1)",
                [folder_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(map_err)
            .map(|exists| exists != 0)
    }

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
    pub fn set_org_context_enabled(&self, org_id: &str, enabled: bool) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let changed = tx
            .execute(
                "UPDATE org_state SET context_enabled = ?2
               WHERE org_id = ?1 AND context_enabled != ?2",
                rusqlite::params![org_id, enabled as i64],
            )
            .map_err(map_err)?;
        if !enabled && changed > 0 {
            Self::purge_all_ask_conversations_tx(&tx)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(changed > 0)
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

    // ── Member identity keys + the rotation journal ─────────────────────────────────────────────
    //
    // Rotation needs every REMAINING member's public key in one pass. The only directory the relay
    // offers is `POST /v1/keys/lookup`, keyed by email and capped at `KEY_LOOKUPS_PER_DAY = 20`,
    // against orgs of up to `MAX_ORG_MEMBERS = 50`. So keys are learned once — at the invite that
    // already looks the member up — and read locally afterwards. Nothing secret is stored: these
    // are the bytes the relay publishes to any authenticated caller.

    /// Remember a member's published identity key for this org. Replaces the row on a key change,
    /// so the caller (never this method) is the place that decides whether a CHANGED key is
    /// acceptable — storage must not be the thing that silently accepts a substituted key.
    pub fn upsert_org_member_key(
        &self,
        org_id: &str,
        user_id: &str,
        email: Option<&str>,
        pk_enc: &[u8],
        pk_sig: &[u8],
        fingerprint: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO org_member_keys
               (org_id, user_id, email, pk_enc, pk_sig, fingerprint, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(org_id, user_id) DO UPDATE SET
               email       = excluded.email,
               pk_enc      = excluded.pk_enc,
               pk_sig      = excluded.pk_sig,
               fingerprint = excluded.fingerprint,
               updated_at  = excluded.updated_at",
            rusqlite::params![
                org_id,
                user_id,
                email,
                pk_enc,
                pk_sig,
                fingerprint,
                chrono::Utc::now().to_rfc3339()
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// This device's remembered key for one member, if it has ever learned one.
    pub fn get_org_member_key(
        &self,
        org_id: &str,
        user_id: &str,
    ) -> Result<Option<crate::storage::OrgMemberKey>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT org_id, user_id, email, pk_enc, pk_sig, fingerprint, updated_at
               FROM org_member_keys WHERE org_id = ?1 AND user_id = ?2",
            rusqlite::params![org_id, user_id],
            |r| {
                Ok(crate::storage::OrgMemberKey {
                    org_id: r.get(0)?,
                    user_id: r.get(1)?,
                    email: r.get(2)?,
                    pk_enc: r.get(3)?,
                    pk_sig: r.get(4)?,
                    fingerprint: r.get(5)?,
                    updated_at: r.get(6)?,
                })
            },
        )
        .optional()
        .map_err(map_err)
    }

    /// Drop a member's remembered key — used when they leave the org, so a later re-invite has to
    /// learn the key afresh instead of trusting one this device kept while they were gone.
    pub fn forget_org_member_key(&self, org_id: &str, user_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM org_member_keys WHERE org_id = ?1 AND user_id = ?2",
            rusqlite::params![org_id, user_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Record that this org OWES a key rotation. Written BEFORE the server-side removal, so an
    /// interruption at any later point leaves a re-drivable debt rather than an org sitting on a
    /// generation the removed member still holds a key for. Idempotent: a second removal before the
    /// first rotation lands keeps one row and the ORIGINAL timestamp (the debt is the org's, not the
    /// individual removal's), while resetting `attempts` so a fresh removal is not throttled by an
    /// older failure streak.
    pub fn mark_org_rotation_pending(&self, org_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO org_rotation_pending
               (org_id, requested_at, attempts, last_error, next_attempt_at)
             VALUES (?1, ?2, 0, NULL, NULL)
             ON CONFLICT(org_id) DO UPDATE SET
               attempts = 0, last_error = NULL, next_attempt_at = NULL",
            rusqlite::params![org_id, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The rotation debt is settled — the relay committed the new generation.
    pub fn clear_org_rotation_pending(&self, org_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM org_rotation_pending WHERE org_id = ?1",
            rusqlite::params![org_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Note a failed rotation attempt and decide when the retry may run again. `error` is a short
    /// non-PII label (an `errcode` tag or an HTTP stage/status), never a message body that could
    /// carry content.
    ///
    /// `learned_something` is the difference between "this attempt is going nowhere" and "this
    /// attempt is converging slowly". A first rotation of an org invited before member keys were
    /// remembered has to resolve them a few at a time against a 20-a-day lookup quota, and
    /// throttling THAT would turn a two-hour convergence into a two-week one. A doomed attempt --
    /// a member whose account is gone but whose membership is not -- learns nothing, and gets an
    /// exponential back-off capped at six hours so it cannot spend the quota every minute forever.
    pub fn record_org_rotation_failure(
        &self,
        org_id: &str,
        error: &str,
        learned_something: bool,
    ) -> Result<()> {
        let conn = self.lock();
        // `attempts` counts attempts, always. Only the BACK-OFF is conditional: an attempt that
        // learned a key it did not have keeps its place at the front of the queue.
        let attempts: i64 = conn
            .query_row(
                "SELECT attempts FROM org_rotation_pending WHERE org_id = ?1",
                rusqlite::params![org_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_err)?
            .unwrap_or(0)
            + 1;
        let next_attempt_at = if learned_something {
            None
        } else {
            let minutes = 2i64
                .checked_pow(u32::try_from(attempts.clamp(1, 16)).unwrap_or(16))
                .unwrap_or(360)
                .min(360);
            Some((chrono::Utc::now() + chrono::Duration::minutes(minutes)).to_rfc3339())
        };
        conn.execute(
            "UPDATE org_rotation_pending
                SET attempts = ?2, last_error = ?3, next_attempt_at = ?4
              WHERE org_id = ?1",
            rusqlite::params![org_id, attempts, error, next_attempt_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Every org that still owes a rotation, oldest debt first — regardless of back-off. The
    /// diagnostic view; the retry drives [`Self::list_org_rotations_due`] instead.
    pub fn list_org_rotations_pending(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT org_id FROM org_rotation_pending ORDER BY requested_at ASC")
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_err)?);
        }
        Ok(out)
    }

    /// The owed rotations whose back-off has elapsed, oldest debt first. A row with no
    /// `next_attempt_at` has never failed (or last made progress) and is always due.
    pub fn list_org_rotations_due(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT org_id FROM org_rotation_pending
                  WHERE next_attempt_at IS NULL OR next_attempt_at <= ?1
                  ORDER BY requested_at ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![chrono::Utc::now().to_rfc3339()], |r| {
                r.get::<_, String>(0)
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Whether one org owes a rotation (with its attempt count), for tests and diagnostics.
    pub fn org_rotation_pending_attempts(&self, org_id: &str) -> Result<Option<i64>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT attempts FROM org_rotation_pending WHERE org_id = ?1",
            rusqlite::params![org_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .map_err(map_err)
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
    pub fn delete_org_state(&self, org_id: &str) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let items = Self::purge_org_replica_tx(&tx, org_id)?;
        let removed = tx
            .execute(
                "DELETE FROM org_state WHERE org_id = ?1",
                rusqlite::params![org_id],
            )
            .map_err(map_err)?;
        // The org's remembered member keys and any owed rotation go with it, in the SAME
        // transaction. A debt outliving its org is retried by every sweep forever against an org
        // `resolve_org` can no longer find, and remembered addresses have no reason to survive a
        // membership this device no longer holds.
        tx.execute(
            "DELETE FROM org_member_keys WHERE org_id = ?1",
            rusqlite::params![org_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "DELETE FROM org_rotation_pending WHERE org_id = ?1",
            rusqlite::params![org_id],
        )
        .map_err(map_err)?;
        if removed > 0 {
            // Membership withdrawal invalidates global-derived Ask even when the decrypted org
            // replica was already empty and the durable conversation is the only derived copy.
            Self::purge_all_ask_conversations_tx(&tx)?;
        }
        tx.commit().map_err(map_err)?;
        tracing::info!(target: "org", items, "atomically removed org membership and decrypted replica");
        Ok(removed > 0)
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
        self.insert_org_share_with_scrub(
            id,
            org_id,
            meeting_id,
            document_id,
            kind,
            title,
            rev,
            generation,
            content_sha256,
            true,
            created_at,
        )
    }

    /// Insert a queued share while durably recording the caller's scrub choice. Retry code must use
    /// this value so an ambiguous successful POST is replayed with the same canonical content hash.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_org_share_with_scrub(
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
        scrub: bool,
        created_at: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO org_shares
               (id, org_id, meeting_id, document_id, kind, title, rev, generation,
                content_sha256, item_id, scrub, state, last_error, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, 'queued', NULL, ?11, ?11)",
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
                scrub as i64,
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

    pub fn set_org_share_document_metadata(
        &self,
        id: &str,
        doc_id: &str,
        access: &str,
    ) -> Result<()> {
        require_stable_uuid(doc_id, "invalid org document id")?;
        if !matches!(access, "view" | "edit") {
            return Err(crate::error::AppError::InvalidArg(
                "invalid org item access".into(),
            ));
        }
        let conn = self.lock();
        let org_id = conn
            .query_row(
                "SELECT org_id FROM org_shares WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(map_err)?;
        if let Some(org_id) = org_id.as_deref() {
            require_stable_uuid(org_id, "invalid organization id")?;
        }
        conn.execute(
            "UPDATE org_shares SET doc_id = ?2, access = ?3 WHERE id = ?1",
            rusqlite::params![id, doc_id, access],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Clear server-issued stable-document metadata before retrying a publish against a relay that
    /// rejected the prior resource. This is one storage-owned mutation so command code never opens
    /// the SQLCipher connection or risks clearing only half of the identity/permission tuple.
    pub fn clear_org_share_document_metadata(&self, id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE org_shares SET doc_id = NULL, access = 'view' WHERE id = ?1",
            rusqlite::params![id],
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

    pub(crate) fn map_org_share(r: &rusqlite::Row<'_>) -> rusqlite::Result<crate::storage::OrgShareRow> {
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
            doc_id: r.get(10)?,
            access: r.get(11)?,
            scrub: r.get::<_, i64>(12)? != 0,
            state: r.get(13)?,
            last_error: r.get(14)?,
            expected_actor_user_id: r.get(15)?,
            expected_owner_user_id: r.get(16)?,
            source_version: r.get::<_, i64>(17)?.max(0) as u64,
            republish_dirty: r.get::<_, i64>(18)?.max(0) as u64,
            parent_container_id: r.get(19)?,
            position: r.get(20)?,
            explicit: r.get::<_, i64>(21)? != 0,
            created_at: r.get(22)?,
            updated_at: r.get(23)?,
        })
    }

    pub(crate) const ORG_SHARE_COLS: &'static str =
        "id, org_id, meeting_id, document_id, kind, title, rev, generation,
         content_sha256, item_id, doc_id, access, scrub, state, last_error,
         expected_actor_user_id, expected_owner_user_id, source_version, republish_dirty,
         parent_container_id, position, explicit,
         created_at, updated_at";

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

    /// Content-free dispatch witness for exact attempt completion CAS. Kept separate from the
    /// general UI row so the transport id is not propagated through unrelated read surfaces.
    pub fn org_share_dispatch_id(&self, id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT dispatch_id FROM org_shares WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_err)
        .map(Option::flatten)
    }

    /// Persist one content-free access dispatch receipt and its complete local CAS witness in the
    /// same SQLite transaction. A content mutation journal, stale local access/owner tuple, or an
    /// existing pending access attempt refuses admission without leaving a phantom egress row.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn persist_org_access_attempt_if_current(
        &self,
        ts: i64,
        host: &str,
        dispatch_id: &str,
        org_id: &str,
        doc_id: &str,
        old_access: &str,
        new_access: &str,
        actor_user_id: &str,
        owner_user_id: &str,
        created_at: &str,
    ) -> Result<bool> {
        for (value, error) in [
            (dispatch_id, "invalid org access dispatch id"),
            (org_id, "invalid org id"),
            (doc_id, "invalid org document id"),
            (actor_user_id, "invalid org access actor"),
            (owner_user_id, "invalid org document owner"),
        ] {
            require_stable_uuid(value, error)?;
        }
        if !matches!(old_access, "view" | "edit") || !matches!(new_access, "view" | "edit") {
            return Err(crate::error::AppError::InvalidArg(
                "invalid org document access".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let admissible: bool = tx
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM org_items
                    WHERE org_id=?1 AND doc_id=?2 AND tombstoned=0 AND is_current=1
                      AND access=?3 AND document_owner_user_id=?4
                 )
                 AND NOT EXISTS(
                   SELECT 1 FROM org_access_attempts
                    WHERE org_id=?1 AND doc_id=?2 AND state='pending'
                 )
                 AND NOT EXISTS(
                   SELECT 1 FROM org_access_attempts WHERE dispatch_id=?5
                 )
                 AND NOT EXISTS(
                   SELECT 1 FROM org_shares
                    WHERE org_id=?1 AND doc_id=?2 AND state='failed'
                      AND last_error IN (
                        'direct_put_pending','republish_put_pending','republish_post_pending',
                        'initial_post_pending','initial_post_replayable','projection_pending',
                        'recovery_witness_missing','org_edit_conflict'
                      )
                 )",
                rusqlite::params![org_id, doc_id, old_access, owner_user_id, dispatch_id],
                |row| Ok(row.get::<_, i64>(0)? != 0),
            )
            .map_err(map_err)?;
        if !admissible {
            return Ok(false);
        }
        insert_share_egress_dispatch_tx(&tx, ts, host, "org_share_access", 0, dispatch_id)?;
        tx.execute(
            "INSERT INTO org_access_attempts
              (dispatch_id,org_id,doc_id,old_access,new_access,actor_user_id,owner_user_id,state,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,'pending',?8)",
            rusqlite::params![
                dispatch_id,
                org_id,
                doc_id,
                old_access,
                new_access,
                actor_user_id,
                owner_user_id,
                created_at
            ],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(true)
    }

    pub(crate) fn pending_org_access_attempts(&self) -> Result<Vec<OrgAccessAttemptRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT dispatch_id,org_id,doc_id,old_access,new_access,actor_user_id,owner_user_id
                   FROM org_access_attempts WHERE state='pending' ORDER BY seq ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(OrgAccessAttemptRow {
                    dispatch_id: row.get(0)?,
                    org_id: row.get(1)?,
                    doc_id: row.get(2)?,
                    old_access: row.get(3)?,
                    new_access: row.get(4)?,
                    actor_user_id: row.get(5)?,
                    owner_user_id: row.get(6)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_err)?);
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply_org_access_attempt_if_current(
        &self,
        dispatch_id: &str,
        org_id: &str,
        doc_id: &str,
        old_access: &str,
        new_access: &str,
        actor_user_id: &str,
        owner_user_id: &str,
    ) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let changed = tx
            .execute(
                "UPDATE org_access_attempts SET state='applied'
                  WHERE dispatch_id=?1 AND org_id=?2 AND doc_id=?3 AND old_access=?4
                    AND new_access=?5 AND actor_user_id=?6 AND owner_user_id=?7 AND state='pending'
                    AND EXISTS(SELECT 1 FROM org_items
                      WHERE org_id=?2 AND doc_id=?3 AND tombstoned=0 AND is_current=1
                        AND access=?4 AND document_owner_user_id=?7)
                    AND NOT EXISTS(SELECT 1 FROM org_access_attempts newer
                      WHERE newer.org_id=?2 AND newer.doc_id=?3
                        AND newer.seq > org_access_attempts.seq AND newer.state != 'failed')
                    AND NOT EXISTS(SELECT 1 FROM org_shares
                      WHERE org_id=?2 AND doc_id=?3 AND state='failed'
                        AND last_error IN (
                          'direct_put_pending','republish_put_pending','republish_post_pending',
                          'initial_post_pending','initial_post_replayable','projection_pending',
                          'recovery_witness_missing','org_edit_conflict'
                        ))",
                rusqlite::params![
                    dispatch_id,
                    org_id,
                    doc_id,
                    old_access,
                    new_access,
                    actor_user_id,
                    owner_user_id
                ],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Ok(false);
        }
        tx.execute(
            "UPDATE org_items SET access=?3,document_owner_user_id=?4
              WHERE org_id=?1 AND doc_id=?2 AND tombstoned=0",
            rusqlite::params![org_id, doc_id, new_access, owner_user_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE org_shares SET access=?3 WHERE org_id=?1 AND doc_id=?2",
            rusqlite::params![org_id, doc_id, new_access],
        )
        .map_err(map_err)?;
        tx.execute(
            "DELETE FROM org_access_attempts WHERE org_id=?1 AND doc_id=?2
              AND seq < (SELECT seq FROM org_access_attempts WHERE dispatch_id=?3)",
            rusqlite::params![org_id, doc_id, dispatch_id],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(true)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn fail_org_access_attempt_if_current(
        &self,
        dispatch_id: &str,
        org_id: &str,
        doc_id: &str,
        old_access: &str,
        new_access: &str,
        actor_user_id: &str,
        owner_user_id: &str,
    ) -> Result<bool> {
        let conn = self.lock();
        conn.execute(
            "UPDATE org_access_attempts SET state='failed'
              WHERE dispatch_id=?1 AND org_id=?2 AND doc_id=?3 AND old_access=?4
                AND new_access=?5 AND actor_user_id=?6 AND owner_user_id=?7 AND state='pending'",
            rusqlite::params![
                dispatch_id,
                org_id,
                doc_id,
                old_access,
                new_access,
                actor_user_id,
                owner_user_id
            ],
        )
        .map(|changed| changed == 1)
        .map_err(map_err)
    }

    /// Apply an authenticated authoritative head to one still-current access attempt. Transport
    /// ambiguity may leave the relay at either access value, so recovery projects the authenticated
    /// value only while the complete durable attempt witness and content barrier still hold.
    pub(crate) fn apply_authoritative_org_access_if_current(
        &self,
        attempt: &OrgAccessAttemptRow,
        authoritative_access: &str,
    ) -> Result<bool> {
        if !matches!(authoritative_access, "view" | "edit") {
            return Err(crate::error::AppError::InvalidArg(
                "invalid authoritative org document access".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let pending: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM org_access_attempts
                  WHERE dispatch_id=?1 AND org_id=?2 AND doc_id=?3 AND old_access=?4
                    AND new_access=?5 AND actor_user_id=?6 AND owner_user_id=?7
                    AND state='pending'
                    AND NOT EXISTS(SELECT 1 FROM org_access_attempts newer
                      WHERE newer.org_id=?2 AND newer.doc_id=?3
                        AND newer.seq > org_access_attempts.seq AND newer.state != 'failed')
                    AND NOT EXISTS(SELECT 1 FROM org_shares
                      WHERE org_id=?2 AND doc_id=?3 AND state='failed'
                        AND last_error IN (
                          'direct_put_pending','republish_put_pending','republish_post_pending',
                          'initial_post_pending','initial_post_replayable','projection_pending',
                          'recovery_witness_missing','org_edit_conflict'
                        )))",
                rusqlite::params![
                    attempt.dispatch_id,
                    attempt.org_id,
                    attempt.doc_id,
                    attempt.old_access,
                    attempt.new_access,
                    attempt.actor_user_id,
                    attempt.owner_user_id
                ],
                |row| Ok(row.get::<_, i64>(0)? != 0),
            )
            .map_err(map_err)?;
        if !pending {
            return Ok(false);
        }
        tx.execute(
            "UPDATE org_items SET access=?3,document_owner_user_id=?4
              WHERE org_id=?1 AND doc_id=?2 AND tombstoned=0",
            rusqlite::params![
                attempt.org_id,
                attempt.doc_id,
                authoritative_access,
                attempt.owner_user_id
            ],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE org_shares SET access=?3 WHERE org_id=?1 AND doc_id=?2",
            rusqlite::params![attempt.org_id, attempt.doc_id, authoritative_access],
        )
        .map_err(map_err)?;
        tx.execute(
            "DELETE FROM org_access_attempts WHERE org_id=?1 AND doc_id=?2",
            rusqlite::params![attempt.org_id, attempt.doc_id],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(true)
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

    /// Resolve a revoke request arriving with the CURRENT feed item back to the origin device's
    /// durable source-share row without changing its CAS baseline (`item_id`/`rev`/hash). Exact
    /// Server item-id lookup remains the first leg so legacy rows keep working; the stable leg is
    /// scoped by `(org_id, doc_id)` and is intentionally not used by `org_resolve_source`. A server
    /// item id must never be interpreted as the local `org_shares.id` namespace.
    pub fn org_share_for_revoke_target(
        &self,
        item_id: &str,
    ) -> Result<Option<crate::storage::OrgShareRow>> {
        if let Some(row) = self.org_share_by_item(item_id)? {
            return Ok(Some(row));
        }
        let conn = self.lock();
        conn.query_row(
            &format!(
                "SELECT {} FROM org_shares
                  WHERE (org_id, doc_id) =
                        (SELECT org_id, doc_id FROM org_items
                          WHERE item_id = ?1 AND tombstoned = 0 AND doc_id IS NOT NULL)
                    AND (state = 'uploaded' OR (state = 'failed' AND item_id IS NOT NULL))
                  ORDER BY created_at ASC LIMIT 1",
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
                   WHERE (state = 'uploaded'
                      OR (state = 'failed' AND (item_id IS NOT NULL OR last_error IN
                         ('initial_post_pending','initial_post_replayable','direct_put_pending',
                          'republish_put_pending','republish_post_pending','org_edit_conflict',
                          'recovery_witness_missing','projection_pending','too_large'))))
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

    /// Destructive source operations must see every non-terminal journal, including legacy
    /// queued/generic-failed NULL-identity rows whose old client may have dispatched a POST without
    /// recording a witness. The command-layer proof classifier decides DELETE vs local cancellation
    /// vs fail-closed; this query deliberately makes no remote-absence inference from state/item_id.
    pub(crate) fn org_shares_for_source_revoke(
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
              WHERE state != 'revoked'
                AND ((?1 IS NOT NULL AND meeting_id=?1)
                  OR (?2 IS NOT NULL AND document_id=?2))
              ORDER BY created_at ASC,id ASC",
                Self::ORG_SHARE_COLS,
            ))
            .map_err(map_err)?;
        let rows = stmt
            .query_map(
                rusqlite::params![meeting_id, document_id],
                Self::map_org_share,
            )
            .map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Every LIVE org share of ONE exact source (`meeting_id` XOR `document_id`) in ONE
    /// org, OLDEST-FIRST (stable tie-break on `id`). The `(org, source)`-scoped twin of
    /// `org_shares_for_source` (which spans all orgs): powers the share IDEMPOTENCY guard + the
    /// duplicate collapse — `[0]` is the canonical KEEPER (earliest published, the identity other
    /// members first saw), `[1..]` are accidental duplicates to tombstone. A durable direct-PUT
    /// journal remains live because its `item_id` is the still-published predecessor; treating it as
    /// absent would let a second Share click mint a new document while reconciliation is pending.
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
                   WHERE org_id = ?1
                     AND (state IN ('queued','uploaded')
                       OR (state = 'failed' AND (item_id IS NOT NULL OR last_error IN
                         ('initial_post_pending','initial_post_replayable','direct_put_pending',
                          'republish_put_pending','republish_post_pending','org_edit_conflict',
                          'recovery_witness_missing','projection_pending'))))
                     AND meeting_id IS ?2 AND document_id IS ?3
                   ORDER BY CASE WHEN state='uploaded' AND item_id IS NOT NULL THEN 0 ELSE 1 END,
                            created_at ASC, id ASC",
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
                     AND (o.meeting_id IS NOT NULL OR o.document_id IS NOT NULL)
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
                "UPDATE org_shares SET state = 'revoked', last_error=NULL, dispatch_id=NULL,
                     updated_at = ?4
                   WHERE org_id = ?1 AND state='failed' AND item_id IS NULL
                     AND ((last_error='initial_post_replayable') OR
                       (dispatch_id IS NULL AND last_error IN ('too_large','seal_failed')))
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
                     AND (last_error IS NULL OR last_error NOT IN
                       ('direct_put_pending', 'republish_put_pending', 'republish_post_pending',
                        'org_edit_conflict', 'recovery_witness_missing', 'projection_pending',
                        'initial_post_pending'))
                   ORDER BY created_at DESC LIMIT 1",
                Self::ORG_SHARE_COLS
            ),
            rusqlite::params![org_id, meeting_id, document_id],
            Self::map_org_share,
        )
        .optional()
        .map_err(map_err)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn acquire_new_org_share_for_source(
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
        scrub: bool,
        now: &str,
    ) -> Result<bool> {
        let conn = self.lock();
        let changed = conn
            .execute(
                "INSERT INTO org_shares
              (id,org_id,meeting_id,document_id,kind,title,rev,generation,content_sha256,
               scrub,state,created_at,updated_at)
             SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'queued',?11,?11
              WHERE NOT EXISTS(SELECT 1 FROM org_shares
                WHERE org_id=?2 AND meeting_id IS ?3 AND document_id IS ?4
                  AND state!='revoked')",
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
                    scrub as i64,
                    now
                ],
            )
            .map_err(map_err)?;
        Ok(changed == 1)
    }

    /// SB-3 retry re-arm: reset an EXISTING org-share row back to `queued` for a fresh publish attempt,
    /// refreshing the per-attempt fields (title/content hash/generation/timestamps) and CLEARING any
    /// item_id + last_error. Used by `share_to_org_inner` when it reuses a `find_reusable_org_share`
    /// row instead of inserting a new one — so N failed attempts stay ONE row, and a later success
    /// flips that same row to uploaded (no duplicate). Idempotent on the row id.
    // One cohesive retry-row mutation; keeping every persisted attempt field visible prevents a
    // caller from accidentally retaining stale scrub/hash/generation state.
    #[allow(clippy::too_many_arguments)]
    pub fn reset_org_share_for_retry(
        &self,
        id: &str,
        title: Option<&str>,
        rev: u32,
        generation: u32,
        content_sha256: &[u8],
        scrub: bool,
        updated_at: &str,
    ) -> Result<()> {
        let conn = self.lock();
        let changed = conn
            .execute(
                "UPDATE org_shares SET state = 'queued', item_id = NULL, last_error = NULL,
               title = ?2, rev = ?3, generation = ?4, content_sha256 = ?5, scrub = ?6,
               updated_at = ?7
             WHERE id = ?1
               AND (last_error IS NULL OR last_error NOT IN
                 ('direct_put_pending', 'republish_put_pending', 'republish_post_pending',
                  'org_edit_conflict', 'recovery_witness_missing', 'projection_pending',
                  'initial_post_pending', 'initial_post_replayable'))",
                rusqlite::params![
                    id,
                    title,
                    rev as i64,
                    generation as i64,
                    content_sha256,
                    scrub as i64,
                    updated_at
                ],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(crate::error::AppError::Unavailable(
                "ambiguous org publish source changed before replay".into(),
            ));
        }
        Ok(())
    }

    /// A stable document with an unresolved mutation witness cannot accept a permission PATCH.
    /// The check is content-free and runs before the PATCH dispatch receipt/socket boundary.
    pub(crate) fn org_document_has_blocked_republish(
        &self,
        org_id: &str,
        doc_id: &str,
    ) -> Result<bool> {
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM org_shares
              WHERE org_id = ?1 AND doc_id = ?2 AND state = 'failed'
                AND last_error IN ('direct_put_pending', 'republish_put_pending',
                                   'republish_post_pending', 'initial_post_pending',
                                   'initial_post_replayable', 'org_edit_conflict',
                                   'recovery_witness_missing', 'projection_pending'))",
            rusqlite::params![org_id, doc_id],
            |row| Ok(row.get::<_, i64>(0)? != 0),
        )
        .map_err(map_err)
    }

    pub(crate) fn org_share_source_counters(&self, id: &str) -> Result<(u64, u64)> {
        let conn = self.lock();
        conn.query_row(
            "SELECT source_version, republish_dirty FROM org_shares WHERE id = ?1",
            rusqlite::params![id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?.max(0) as u64,
                    row.get::<_, i64>(1)?.max(0) as u64,
                ))
            },
        )
        .map_err(map_err)
    }

    pub(crate) fn complete_source_less_projection_if_present(
        &self,
        share_id: &str,
    ) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let changed = tx.execute(
            "DELETE FROM org_shares WHERE id=?1 AND state='failed'
              AND last_error='projection_pending' AND meeting_id IS NULL AND document_id IS NULL
              AND EXISTS(SELECT 1 FROM org_items i WHERE i.item_id=org_shares.item_id
                AND i.org_id=org_shares.org_id AND i.doc_id=org_shares.doc_id
                AND i.access=org_shares.access AND i.rev=org_shares.rev
                AND i.generation=org_shares.generation AND i.content_sha256=org_shares.content_sha256
                AND i.projection_sha256=org_shares.content_sha256
                AND i.tombstoned=0)",
            rusqlite::params![share_id],
        ).map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(changed == 1)
    }

    pub(crate) fn terminalize_and_evict_org_document(
        &self,
        org_id: &str,
        doc_id: &str,
        updated_at: &str,
    ) -> Result<bool> {
        require_stable_uuid(org_id, "invalid organization id")?;
        require_stable_uuid(doc_id, "invalid org document id")?;
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let evicted = Self::evict_org_document_tx(&tx, org_id, doc_id)?;
        tx.execute(
            "UPDATE org_shares SET state='revoked', last_error=NULL, item_id=NULL,
                    dispatch_id=NULL, updated_at=?3
              WHERE org_id=?1 AND doc_id=?2 AND state!='revoked'",
            rusqlite::params![org_id, doc_id, updated_at],
        )
        .map_err(map_err)?;
        tx.execute(
            "DELETE FROM org_access_attempts WHERE org_id=?1 AND doc_id=?2",
            rusqlite::params![org_id, doc_id],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(evicted)
    }

    /// Repair the legacy crash shape where a stable share was already marked `revoked` after the
    /// server DELETE, but its landed `item_id` survived because local document terminalization did
    /// not. The exact non-null item witness distinguishes this from a proven-never-landed local
    /// cancellation. Consume that witness in the same transaction as all-revision eviction, the
    /// terminal marker and incident-link purge, so a second sweep is a strict no-op.
    pub(crate) fn repair_revoked_org_share_terminal_state(
        &self,
        share_id: &str,
        org_id: &str,
        doc_id: &str,
    ) -> Result<bool> {
        require_stable_uuid(org_id, "invalid organization id")?;
        require_stable_uuid(doc_id, "invalid org document id")?;
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let repairable: i64 = tx
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM org_shares
                    WHERE id = ?1 AND org_id = ?2 AND doc_id = ?3
                      AND state = 'revoked' AND item_id IS NOT NULL
                 )",
                rusqlite::params![share_id, org_id, doc_id],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        if repairable == 0 {
            tx.commit().map_err(map_err)?;
            return Ok(false);
        }
        Self::evict_org_document_tx(&tx, org_id, doc_id)?;
        let consumed = tx
            .execute(
                "UPDATE org_shares
                    SET item_id = NULL, dispatch_id = NULL, last_error = NULL
                  WHERE id = ?1 AND org_id = ?2 AND doc_id = ?3
                    AND state = 'revoked' AND item_id IS NOT NULL",
                rusqlite::params![share_id, org_id, doc_id],
            )
            .map_err(map_err)?;
        if consumed != 1 {
            return Err(crate::error::AppError::Storage(
                "revoked Shared document repair changed concurrently".into(),
            ));
        }
        tx.execute(
            "DELETE FROM org_access_attempts WHERE org_id = ?1 AND doc_id = ?2",
            rusqlite::params![org_id, doc_id],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(true)
    }

    pub(crate) fn org_source_version(
        &self,
        meeting_id: Option<&str>,
        document_id: Option<&str>,
    ) -> Result<u64> {
        let (kind, id) = match (meeting_id, document_id) {
            (Some(id), None) => ("meeting", id),
            (None, Some(id)) => ("document", id),
            _ => {
                return Err(crate::error::AppError::InvalidArg(
                    "exactly one org share source is required".into(),
                ))
            }
        };
        let conn = self.lock();
        conn.query_row(
            "SELECT version FROM org_source_versions WHERE source_kind=?1 AND source_id=?2",
            rusqlite::params![kind, id],
            |row| Ok(row.get::<_, i64>(0)?.max(0) as u64),
        )
        .optional()
        .map_err(map_err)
        .map(|value| value.unwrap_or(0))
    }

    pub(crate) fn clear_org_share_dirty_if_epoch(
        &self,
        id: &str,
        source_version: u64,
        dirty_counter: u64,
        item_id: &str,
        rev: u32,
        content_sha256: &[u8],
    ) -> Result<bool> {
        let conn = self.lock();
        let changed = conn
            .execute(
                "UPDATE org_shares SET republish_dirty = 0
                  WHERE id = ?1 AND state = 'uploaded' AND last_error IS NULL
                    AND source_version = ?2 AND republish_dirty = ?3 AND republish_dirty > 0
                    AND item_id = ?4 AND rev = ?5 AND content_sha256 = ?6",
                rusqlite::params![
                    id,
                    source_version as i64,
                    dirty_counter as i64,
                    item_id,
                    rev as i64,
                    content_sha256,
                ],
            )
            .map_err(map_err)?;
        Ok(changed == 1)
    }

    /// Re-arm an authenticated-absence initial POST without changing its durable actor/owner
    /// witnesses. The equality predicates are the DB-level account-switch CAS: a stale caller can
    /// neither overwrite the witnesses nor make the row dispatchable under another account.
    #[allow(clippy::too_many_arguments)]
    pub fn reset_initial_org_share_for_replay(
        &self,
        id: &str,
        rev: u32,
        generation: u32,
        content_sha256: &[u8],
        scrub: bool,
        doc_id: &str,
        access: &str,
        expected_actor_user_id: &str,
        expected_owner_user_id: &str,
        current_actor_user_id: &str,
        updated_at: &str,
    ) -> Result<()> {
        let conn = self.lock();
        let changed = conn
            .execute(
                "UPDATE org_shares
                    SET state = 'queued', item_id = NULL, last_error = NULL, updated_at = ?11
                  WHERE id = ?1 AND state = 'failed' AND last_error = 'initial_post_replayable'
                    AND rev = ?2 AND generation = ?3 AND content_sha256 = ?4 AND scrub = ?5
                    AND doc_id = ?6 AND access = ?7
                    AND expected_actor_user_id = ?8 AND expected_owner_user_id = ?9
                    AND ?10 = ?8 AND ?10 = ?9",
                rusqlite::params![
                    id,
                    rev as i64,
                    generation as i64,
                    content_sha256,
                    scrub as i64,
                    doc_id,
                    access,
                    expected_actor_user_id,
                    expected_owner_user_id,
                    current_actor_user_id,
                    updated_at,
                ],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(crate::error::AppError::Unavailable(
                "ambiguous org publish actor or source changed before replay".into(),
            ));
        }
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

    pub(crate) fn list_dirty_uploaded_org_shares(
        &self,
    ) -> Result<Vec<crate::storage::OrgShareRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM org_shares
                  WHERE state = 'uploaded' AND last_error IS NULL
                    AND republish_dirty > 0
                    AND (meeting_id IS NOT NULL OR document_id IS NOT NULL)
                  ORDER BY updated_at ASC, id ASC",
                Self::ORG_SHARE_COLS
            ))
            .map_err(map_err)?;
        let rows = stmt.query_map([], Self::map_org_share).map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_err)?);
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
                   WHERE (state = 'uploaded' OR (state = 'failed' AND item_id IS NOT NULL))
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
                "SELECT s.item_id, s.title FROM org_shares s
                  WHERE s.state != 'revoked'
                    AND ((s.document_id IS NOT NULL AND EXISTS(
                          SELECT 1 FROM documents d WHERE d.id=s.document_id AND d.folder_id=?1))
                      OR (s.meeting_id IS NOT NULL AND EXISTS(
                          SELECT 1 FROM meetings m WHERE m.id=s.meeting_id AND
                            (m.folder_id=?1 OR (m.folder_id IS NULL AND EXISTS(
                              SELECT 1 FROM notes n WHERE n.meeting_id=m.id
                               AND n.folder_id=?1))))))",
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
                "SELECT s.id, s.item_id, s.title FROM org_shares s
                  WHERE s.state != 'revoked'
                    AND ((s.document_id IS NOT NULL AND EXISTS(
                          SELECT 1 FROM documents d WHERE d.id=s.document_id AND d.folder_id=?1))
                      OR (s.meeting_id IS NOT NULL AND EXISTS(
                          SELECT 1 FROM meetings m WHERE m.id=s.meeting_id AND
                            (m.folder_id=?1 OR (m.folder_id IS NULL AND EXISTS(
                              SELECT 1 FROM notes n WHERE n.meeting_id=m.id
                               AND n.folder_id=?1))))))",
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
        let prepared = if source_kind == Some("task") {
            PreparedOrgItemIndex {
                chunks: Vec::new(),
                vector_blobs: None,
            }
        } else {
            Self::prepare_org_item_index(title, created_at, markdown, embedder)?
        };

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

    /// Tasks deliberately never enter Brain/Ask retrieval. They retain an `org_items` lineage row
    /// for permissions and stable-document recovery, but their structured JSON receives no chunks,
    /// FTS rows, or embeddings.
    pub(crate) fn prepare_org_item_index_for_kind(
        kind: crate::share::org_envelope::OrgItemKind,
        title: &str,
        created_at: &str,
        markdown: &str,
        embedder: Option<&dyn Embedder>,
    ) -> Result<PreparedOrgItemIndex> {
        if kind == crate::share::org_envelope::OrgItemKind::Task {
            return Ok(PreparedOrgItemIndex {
                chunks: Vec::new(),
                vector_blobs: None,
            });
        }
        Self::prepare_org_item_index(title, created_at, markdown, embedder)
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
    /// observe the new local card without the old local card being evicted (or vice versa). The
    /// returned boolean is the transaction-authoritative visibility-reduction result: callers use it
    /// to bump the lifecycle epoch and invalidate open Ask renderers exactly when a live predecessor
    /// was actually evicted.
    #[cfg(test)]
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
    ) -> Result<bool> {
        self.commit_local_org_replica_with_metadata(
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
            superseded_item_id,
            None,
            "view",
            None,
            false,
        )
        .map(|outcome| outcome.changed)
    }

    /// Local publish/CAS refresh with stable document metadata stamped before predecessor eviction
    /// in the same transaction. That ordering lets the tombstone see the new live revision and
    /// preserve the revision-stable private link edge across supersession.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_local_org_replica_with_metadata(
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
        doc_id: Option<&str>,
        access: &str,
        document_owner_user_id: Option<&str>,
        is_current: bool,
    ) -> Result<OrgMetadataCommitOutcome> {
        if let Some(doc_id) = doc_id {
            require_stable_uuid(org_id, "invalid organization id")?;
            require_stable_uuid(doc_id, "invalid org document id")?;
        }
        if !matches!(access, "view" | "edit") {
            return Err(crate::error::AppError::InvalidArg(
                "invalid org item access".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        if !Self::org_membership_witness_tx(&tx, org_id, generation)? {
            // The server publish may have raced a local leave/removal. The remote item remains the
            // server's concern, but withdrawn membership must never be followed by a plaintext local
            // replica resurrection.
            return Ok(OrgMetadataCommitOutcome::default());
        }
        let existing = tx
            .query_row(
                "SELECT seq, content_sha256, tombstoned, projection_sha256
                   FROM org_items WHERE item_id = ?1",
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

        if doc_id.is_some() || document_owner_user_id.is_some() || access != "view" {
            Self::set_org_item_document_metadata_tx(
                &tx,
                item_id,
                doc_id,
                access,
                document_owner_user_id,
            )?;
        }
        let current_reduced =
            Self::set_org_item_current_tx(&tx, item_id, org_id, doc_id, is_current)?;
        if source_kind == Some("task") {
            Self::upsert_org_task_projection_tx(
                &tx,
                item_id,
                org_id,
                doc_id,
                markdown,
                access,
                author_user_id,
                document_owner_user_id,
                rev,
                generation,
                seq,
            )?;
        }

        let superseded_evicted = match superseded_item_id.filter(|old| *old != item_id) {
            Some(old_item_id) => Self::tombstone_org_item_tx(&tx, old_item_id)?,
            None => false,
        };
        // Tombstoning a live predecessor already purges in `tombstone_org_item_tx`; do not advance
        // the durable generation twice when it is also the distinct head demoted above.
        if current_reduced && !superseded_evicted {
            Self::purge_all_ask_conversations_tx(&tx)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(OrgMetadataCommitOutcome {
            changed: superseded_evicted,
            visibility_reduced: current_reduced || superseded_evicted,
        })
    }

    /// Install the plaintext/index/attachment projection of one completed stable PUT only while
    /// the exact completed dispatch is still the live share head. A concurrent revoke or access
    /// PATCH changes that witness and makes this a no-op, so a late HTTP response cannot resurrect
    /// withdrawn content or stale permissions. Every older locally-held revision of the document is
    /// purged in the same transaction, including revisions that were never the recorded predecessor.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_org_republish_projection_if_current(
        &self,
        share_id: &str,
        dispatch_id: &str,
        org_id: &str,
        doc_id: &str,
        access: &str,
        rev: u32,
        generation: u32,
        content_sha256: &[u8],
        expected_actor_user_id: &str,
        document_owner_user_id: &str,
        expected_predecessor_item_id: Option<&str>,
        expected_pending_reason: Option<&str>,
        discard_source_less_anchor: bool,
        projection: &OrgRepublishProjection<'_>,
    ) -> Result<OrgMetadataCommitOutcome> {
        require_stable_uuid(org_id, "invalid organization id")?;
        require_stable_uuid(doc_id, "invalid org document id")?;
        if !matches!(access, "view" | "edit") {
            return Err(crate::error::AppError::InvalidArg(
                "invalid org item access".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let witness: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM org_shares
                  WHERE id = ?1
                    AND ((state = 'uploaded' AND last_error IS NULL AND item_id = ?2)
                      OR (state = 'failed' AND last_error IS ?12 AND item_id IS ?13))
                    AND org_id = ?9 AND expected_actor_user_id = ?10
                    AND expected_owner_user_id = ?11
                    AND doc_id = ?3 AND access = ?4 AND rev = ?5 AND generation = ?6
                    AND content_sha256 = ?7 AND dispatch_id = ?8)",
                rusqlite::params![
                    share_id,
                    projection.item_id,
                    doc_id,
                    access,
                    rev as i64,
                    generation as i64,
                    content_sha256,
                    dispatch_id,
                    org_id,
                    expected_actor_user_id,
                    document_owner_user_id,
                    expected_pending_reason,
                    expected_predecessor_item_id,
                ],
                |row| Ok(row.get::<_, i64>(0)? != 0),
            )
            .map_err(map_err)?;
        if !witness || !Self::org_membership_witness_tx(&tx, org_id, generation)? {
            return Ok(OrgMetadataCommitOutcome::default());
        }

        let target_tombstoned = tx
            .query_row(
                "SELECT tombstoned FROM org_items WHERE item_id = ?1",
                rusqlite::params![projection.item_id],
                |row| Ok(row.get::<_, i64>(0)? != 0),
            )
            .optional()
            .map_err(map_err)?
            .unwrap_or(false);
        if target_tombstoned {
            return Ok(OrgMetadataCommitOutcome::default());
        }
        let incompatible_head: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM org_items
                  WHERE org_id = ?1 AND doc_id = ?2 AND tombstoned = 0 AND is_current = 1
                    AND item_id != ?3
                    AND (generation > ?4 OR (generation = ?4 AND rev >= ?5)
                      OR seq > ?6))",
                rusqlite::params![
                    org_id,
                    doc_id,
                    projection.item_id,
                    generation as i64,
                    rev as i64,
                    projection.seq as i64,
                ],
                |row| Ok(row.get::<_, i64>(0)? != 0),
            )
            .map_err(map_err)?;
        if incompatible_head {
            return Ok(OrgMetadataCommitOutcome::default());
        }

        let existing_target = tx
            .query_row(
                "SELECT seq, content_sha256, tombstoned, projection_sha256
                   FROM org_items WHERE item_id = ?1",
                rusqlite::params![projection.item_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?.max(0) as u64,
                        row.get::<_, Option<Vec<u8>>>(1)?,
                        row.get::<_, i64>(2)? != 0,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(map_err)?;
        let replace_attachments = existing_target.as_ref().map_or(true, |(_, _, _, witness)| {
            witness.as_deref() != Some(content_sha256)
        });
        let replace_target = match existing_target {
            None => true,
            Some((_seq, _hash, true, _projection)) => {
                return Ok(OrgMetadataCommitOutcome::default())
            }
            Some((stored_seq, _, false, _projection)) if stored_seq > projection.seq => {
                return Ok(OrgMetadataCommitOutcome::default())
            }
            Some((stored_seq, stored_hash, false, _projection)) if stored_seq == projection.seq => {
                if stored_hash.as_deref() != Some(content_sha256) {
                    return Err(crate::error::AppError::Storage(
                        "conflicting org replica payload at the same feed sequence".into(),
                    ));
                }
                false
            }
            Some(_) => true,
        };
        let replace_attachments = replace_target || replace_attachments;

        if replace_target {
            Self::upsert_org_item_prepared_tx(
                &tx,
                projection.item_id,
                org_id,
                projection.seq,
                projection.author_hint,
                projection.title,
                projection.markdown,
                projection.created_at,
                rev,
                generation,
                content_sha256,
                projection.source_kind,
                projection.author_user_id,
                projection.prepared,
            )?;
        }
        if replace_attachments {
            Self::replace_org_item_attachment_bundle_tx(
                &tx,
                projection.item_id,
                projection.attachments,
                content_sha256,
            )?;
        }
        Self::set_org_item_document_metadata_tx(
            &tx,
            projection.item_id,
            Some(doc_id),
            access,
            Some(document_owner_user_id),
        )?;
        let current_reduced =
            Self::set_org_item_current_tx(&tx, projection.item_id, org_id, Some(doc_id), true)?;
        if projection.source_kind == Some("task") {
            Self::upsert_org_task_projection_tx(
                &tx,
                projection.item_id,
                org_id,
                Some(doc_id),
                projection.markdown,
                access,
                projection.author_user_id,
                Some(document_owner_user_id),
                rev,
                generation,
                projection.seq,
            )?;
        }

        let older_ids: Vec<String> = {
            let mut stmt = tx
                .prepare(
                    "SELECT item_id FROM org_items
                      WHERE org_id = ?1 AND doc_id = ?2 AND item_id != ?3 AND tombstoned = 0
                        AND (generation < ?4 OR (generation = ?4 AND rev < ?5))",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(
                    rusqlite::params![
                        org_id,
                        doc_id,
                        projection.item_id,
                        generation as i64,
                        rev as i64,
                    ],
                    |row| row.get(0),
                )
                .map_err(map_err)?;
            let mut ids = Vec::new();
            for row in rows {
                ids.push(row.map_err(map_err)?);
            }
            ids
        };
        let had_older = !older_ids.is_empty();
        let mut evicted = false;
        for item_id in older_ids {
            evicted |= Self::tombstone_org_item_tx(&tx, &item_id)?;
        }

        let completion_changed = tx.execute(
            "UPDATE org_shares SET state='uploaded', item_id=?2, last_error=NULL, updated_at=?11
              WHERE id=?1 AND state='failed' AND last_error IS ?12 AND item_id IS ?13
                AND doc_id=?3 AND access=?4 AND rev=?5 AND generation=?6
                AND content_sha256=?7 AND dispatch_id=?8
                AND expected_actor_user_id=?9 AND expected_owner_user_id=?10",
            rusqlite::params![share_id, projection.item_id, doc_id, access, rev as i64,
                generation as i64, content_sha256, dispatch_id, expected_actor_user_id,
                document_owner_user_id, chrono::Utc::now().to_rfc3339(),
                expected_pending_reason, expected_predecessor_item_id],
        ).map_err(map_err)?;
        let still_uploaded: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM org_shares WHERE id=?1 AND state='uploaded'
                  AND last_error IS NULL AND item_id=?2 AND doc_id=?3 AND access=?4 AND rev=?5
                  AND generation=?6 AND content_sha256=?7 AND dispatch_id=?8
                  AND expected_actor_user_id=?9 AND expected_owner_user_id=?10)",
                rusqlite::params![
                    share_id,
                    projection.item_id,
                    doc_id,
                    access,
                    rev as i64,
                    generation as i64,
                    content_sha256,
                    dispatch_id,
                    expected_actor_user_id,
                    document_owner_user_id,
                ],
                |row| Ok(row.get::<_, i64>(0)? != 0),
            )
            .map_err(map_err)?;
        if !still_uploaded || (expected_pending_reason.is_some() && completion_changed != 1) {
            return Ok(OrgMetadataCommitOutcome::default());
        }
        if discard_source_less_anchor {
            tx.execute(
                "DELETE FROM org_shares WHERE id=?1 AND state='uploaded' AND item_id=?2
                  AND dispatch_id=?3 AND meeting_id IS NULL AND document_id IS NULL",
                rusqlite::params![share_id, projection.item_id, dispatch_id],
            )
            .map_err(map_err)?;
        } else {
            tx.execute(
                "UPDATE org_shares SET expected_actor_user_id = NULL
                  WHERE id = ?1 AND dispatch_id = ?2 AND expected_actor_user_id = ?3",
                rusqlite::params![share_id, dispatch_id, expected_actor_user_id],
            )
            .map_err(map_err)?;
        }
        if (current_reduced || evicted) && had_older {
            Self::purge_all_ask_conversations_tx(&tx)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(OrgMetadataCommitOutcome {
            changed: true,
            visibility_reduced: current_reduced || evicted,
        })
    }

    /// Commit one already-prepared live feed item and advance the org cursor to this action's exact
    /// sequence in ONE transaction. There is deliberately no fetched-page cursor parameter: a crash
    /// after this method can replay later page entries, but can never skip them.
    #[cfg(test)]
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
        self.commit_org_feed_item_with_metadata(
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
            None,
            "view",
            None,
            false,
        )
        .map(|outcome| outcome.changed)
    }

    /// Feed commit plus stable permission/link metadata in the same transaction. Metadata is also
    /// repaired when the content action is already behind the cursor (`false`), provided the live
    /// item exists; this closes the local-publish-before-feed gap without rewriting content/indexes.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_org_feed_item_with_metadata(
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
        doc_id: Option<&str>,
        access: &str,
        document_owner_user_id: Option<&str>,
        is_current: bool,
    ) -> Result<OrgMetadataCommitOutcome> {
        self.commit_org_feed_item_with_metadata_and_attachments(
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
            doc_id,
            access,
            document_owner_user_id,
            is_current,
            &[],
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_org_feed_item_with_metadata_and_attachments(
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
        doc_id: Option<&str>,
        access: &str,
        document_owner_user_id: Option<&str>,
        is_current: bool,
        attachments: &[crate::storage::IncomingAttachment],
    ) -> Result<OrgMetadataCommitOutcome> {
        if let Some(doc_id) = doc_id {
            require_stable_uuid(org_id, "invalid organization id")?;
            require_stable_uuid(doc_id, "invalid org document id")?;
        }
        if !matches!(access, "view" | "edit") {
            return Err(crate::error::AppError::InvalidArg(
                "invalid org item access".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        if !Self::org_membership_witness_tx(&tx, org_id, generation)? {
            return Ok(OrgMetadataCommitOutcome::default());
        }
        let claimed_seq = Self::claim_org_feed_seq_tx(&tx, org_id, seq)?;
        if !claimed_seq {
            let projection_complete = tx
                .query_row(
                    "SELECT projection_sha256 = ?2 FROM org_items
                      WHERE item_id = ?1 AND tombstoned = 0",
                    rusqlite::params![item_id, content_sha256],
                    |row| Ok(row.get::<_, Option<bool>>(0)?.unwrap_or(false)),
                )
                .optional()
                .map_err(map_err)?
                .unwrap_or(false);
            if !projection_complete {
                // The cursor may have committed before the authenticated attachment bundle in a
                // legacy build. Repair the whole projection below without moving the cursor.
            } else {
                if doc_id.is_some() || document_owner_user_id.is_some() || access != "view" {
                    Self::set_org_item_document_metadata_tx(
                        &tx,
                        item_id,
                        doc_id,
                        access,
                        document_owner_user_id,
                    )?;
                }
                let visibility_reduced =
                    Self::set_org_item_current_tx(&tx, item_id, org_id, doc_id, is_current)?;
                if source_kind == Some("task") {
                    Self::upsert_org_task_projection_tx(
                        &tx,
                        item_id,
                        org_id,
                        doc_id,
                        markdown,
                        access,
                        author_user_id,
                        document_owner_user_id,
                        rev,
                        generation,
                        seq,
                    )?;
                }
                Self::close_projection_pending_for_item_tx(
                    &tx,
                    item_id,
                    org_id,
                    doc_id,
                    access,
                    rev,
                    generation,
                    content_sha256,
                    author_user_id,
                    document_owner_user_id,
                )?;
                if visibility_reduced {
                    Self::purge_all_ask_conversations_tx(&tx)?;
                }
                tx.commit().map_err(map_err)?;
                return Ok(OrgMetadataCommitOutcome {
                    changed: false,
                    visibility_reduced,
                });
            }
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
            return Ok(OrgMetadataCommitOutcome::default());
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
        Self::replace_org_item_attachment_bundle_tx(&tx, item_id, attachments, content_sha256)?;
        if doc_id.is_some() || document_owner_user_id.is_some() || access != "view" {
            Self::set_org_item_document_metadata_tx(
                &tx,
                item_id,
                doc_id,
                access,
                document_owner_user_id,
            )?;
        }
        let visibility_reduced =
            Self::set_org_item_current_tx(&tx, item_id, org_id, doc_id, is_current)?;
        if source_kind == Some("task") {
            Self::upsert_org_task_projection_tx(
                &tx,
                item_id,
                org_id,
                doc_id,
                markdown,
                access,
                author_user_id,
                document_owner_user_id,
                rev,
                generation,
                seq,
            )?;
        }
        Self::close_projection_pending_for_item_tx(
            &tx,
            item_id,
            org_id,
            doc_id,
            access,
            rev,
            generation,
            content_sha256,
            author_user_id,
            document_owner_user_id,
        )?;
        if visibility_reduced {
            Self::purge_all_ask_conversations_tx(&tx)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(OrgMetadataCommitOutcome {
            changed: true,
            visibility_reduced,
        })
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
               projection_sha256=NULL, tombstoned=0",
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

    fn replace_org_item_attachment_bundle_tx(
        tx: &rusqlite::Transaction<'_>,
        item_id: &str,
        attachments: &[crate::storage::IncomingAttachment],
        content_sha256: &[u8],
    ) -> Result<()> {
        tx.execute(
            "DELETE FROM note_attachments WHERE org_item_id=?1",
            [item_id],
        )
        .map_err(map_err)?;
        let created_at = chrono::Utc::now().timestamp_millis();
        for attachment in attachments {
            tx.execute(
                "INSERT INTO note_attachments
                  (id,document_id,meeting_id,provider_id,org_item_id,mime_type,extension,
                   byte_len,width,height,sha256,data,data_blob,exported_path,created_at)
                 VALUES(?1,NULL,NULL,NULL,?2,?3,?4,?5,?6,?7,?8,?9,NULL,NULL,?10)",
                rusqlite::params![
                    attachment.id,
                    item_id,
                    attachment.mime_type,
                    attachment.extension,
                    i64::try_from(attachment.data.len()).map_err(|_| {
                        crate::error::AppError::InvalidArg("image is too large".into())
                    })?,
                    i64::from(attachment.width),
                    i64::from(attachment.height),
                    attachment.sha256.as_slice(),
                    attachment.data,
                    created_at
                ],
            )
            .map_err(map_err)?;
        }
        tx.execute(
            "UPDATE org_items SET projection_sha256=?2
              WHERE item_id=?1 AND tombstoned=0",
            rusqlite::params![item_id, content_sha256],
        )
        .map_err(map_err)?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn close_projection_pending_for_item_tx(
        tx: &rusqlite::Transaction<'_>,
        item_id: &str,
        org_id: &str,
        doc_id: Option<&str>,
        access: &str,
        rev: u32,
        generation: u32,
        content_sha256: &[u8],
        author_user_id: Option<&str>,
        document_owner_user_id: Option<&str>,
    ) -> Result<()> {
        let (Some(doc_id), Some(actor), Some(owner)) =
            (doc_id, author_user_id, document_owner_user_id)
        else {
            return Ok(());
        };
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE org_shares SET state='uploaded', last_error=NULL,
                    expected_actor_user_id=NULL, updated_at=?10
              WHERE state='failed' AND last_error='projection_pending'
                AND item_id=?1 AND org_id=?2 AND doc_id=?3 AND access=?4
                AND rev=?5 AND generation=?6 AND content_sha256=?7
                AND expected_actor_user_id=?8 AND expected_owner_user_id=?9
                AND (meeting_id IS NOT NULL OR document_id IS NOT NULL)",
            rusqlite::params![
                item_id,
                org_id,
                doc_id,
                access,
                rev as i64,
                generation as i64,
                content_sha256,
                actor,
                owner,
                now,
            ],
        )
        .map_err(map_err)?;
        tx.execute(
            "DELETE FROM org_shares
              WHERE state='failed' AND last_error='projection_pending'
                AND meeting_id IS NULL AND document_id IS NULL
                AND item_id=?1 AND org_id=?2 AND doc_id=?3 AND access=?4
                AND rev=?5 AND generation=?6 AND content_sha256=?7
                AND expected_actor_user_id=?8 AND expected_owner_user_id=?9",
            rusqlite::params![
                item_id,
                org_id,
                doc_id,
                access,
                rev as i64,
                generation as i64,
                content_sha256,
                actor,
                owner,
            ],
        )
        .map_err(map_err)?;
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
    #[cfg(test)]
    pub(crate) fn commit_org_feed_tombstone(
        &self,
        org_id: &str,
        item_id: &str,
        seq: u64,
    ) -> Result<bool> {
        self.commit_org_feed_tombstone_outcome(org_id, item_id, seq)
            .map(|(applied, _)| applied)
    }

    #[cfg(test)]
    pub(crate) fn commit_org_feed_tombstone_outcome(
        &self,
        org_id: &str,
        item_id: &str,
        seq: u64,
    ) -> Result<(bool, bool)> {
        self.commit_org_feed_tombstone_with_metadata_outcome(
            org_id, item_id, seq, None, false,
        )
    }

    /// Metadata-aware feed tombstone commit. A durable-document tombstone marked `is_current=true`
    /// is the cross-device signal for an authoritative stable-document DELETE; a predecessor row
    /// marked false is only a revision transition and must keep opaque private link decisions.
    /// Cursor claim, content eviction, terminal witness and link purge are one transaction.
    pub(crate) fn commit_org_feed_tombstone_with_metadata_outcome(
        &self,
        org_id: &str,
        item_id: &str,
        seq: u64,
        doc_id: Option<&str>,
        is_current: bool,
    ) -> Result<(bool, bool)> {
        if let Some(doc_id) = doc_id {
            require_stable_uuid(org_id, "invalid organization id")?;
            require_stable_uuid(doc_id, "invalid org document id")?;
        } else if is_current {
            return Err(crate::error::AppError::InvalidArg(
                "a current org tombstone requires a stable document id".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        if !Self::claim_org_feed_seq_tx(&tx, org_id, seq)? {
            return Ok((false, false));
        }
        let evicted = if let (Some(doc_id), true) = (doc_id, is_current) {
            // An authoritative stable-document DELETE withdraws the whole durable resource, not
            // merely the item id named by this feed row. Evict every locally held revision in the
            // same transaction as the terminal witness so a missed predecessor can never remain
            // searchable after the current head is gone.
            Self::evict_org_document_tx(&tx, org_id, doc_id)?
        } else {
            // Do NOT route ordinary item/predecessor tombstones through the document primitive; a
            // successor may still arrive and must retain the user's opaque relation decision.
            let evicted = Self::tombstone_org_item_tx(&tx, item_id)?;
            if doc_id.is_some() {
                tx.execute(
                    "UPDATE org_items SET is_current = 0 WHERE item_id = ?1",
                    rusqlite::params![item_id],
                )
                .map_err(map_err)?;
            }
            evicted
        };
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE org_shares SET last_error='org_edit_conflict',updated_at=?2
              WHERE state='failed' AND last_error='projection_pending' AND item_id=?1 AND org_id=?3
                AND (meeting_id IS NOT NULL OR document_id IS NOT NULL)",
            rusqlite::params![item_id, now, org_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "DELETE FROM org_shares WHERE state='failed' AND last_error='projection_pending'
              AND item_id=?1 AND org_id=?2 AND meeting_id IS NULL AND document_id IS NULL",
            rusqlite::params![item_id, org_id],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok((true, evicted))
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

    /// Evict every locally-held live revision of one stable org document in one transaction. Used
    /// after the relay confirms a stable-document DELETE, so a stale origin `item_id` cannot leave a
    /// remotely-edited current head searchable on this device.
    pub fn evict_org_document(&self, org_id: &str, doc_id: &str) -> Result<bool> {
        require_stable_uuid(org_id, "invalid organization id")?;
        require_stable_uuid(doc_id, "invalid org document id")?;
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let evicted = Self::evict_org_document_tx(&tx, org_id, doc_id)?;
        tx.commit().map_err(map_err)?;
        Ok(evicted)
    }

    /// Reconcile-side tombstone application with the same durable-document semantics as the live
    /// feed. Reconcile progress has its own cursor and is recorded by the caller afterwards, but
    /// content eviction, terminal witness creation and incident-link purge remain one transaction.
    pub(crate) fn evict_org_reconcile_tombstone_with_metadata(
        &self,
        org_id: &str,
        item_id: &str,
        doc_id: Option<&str>,
        is_current: bool,
    ) -> Result<bool> {
        if let Some(doc_id) = doc_id {
            require_stable_uuid(org_id, "invalid organization id")?;
            require_stable_uuid(doc_id, "invalid org document id")?;
        } else if is_current {
            return Err(crate::error::AppError::InvalidArg(
                "a current org tombstone requires a stable document id".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let evicted = if let (Some(doc_id), true) = (doc_id, is_current) {
            Self::evict_org_document_tx(&tx, org_id, doc_id)?
        } else {
            let evicted = Self::tombstone_org_item_tx(&tx, item_id)?;
            if doc_id.is_some() {
                tx.execute(
                    "UPDATE org_items SET is_current = 0 WHERE item_id = ?1",
                    rusqlite::params![item_id],
                )
                .map_err(map_err)?;
            }
            evicted
        };
        tx.commit().map_err(map_err)?;
        Ok(evicted)
    }

    fn evict_org_document_tx(
        tx: &rusqlite::Transaction<'_>,
        org_id: &str,
        doc_id: &str,
    ) -> Result<bool> {
        let item_ids: Vec<String> = {
            let mut stmt = tx
                .prepare(
                    "SELECT item_id FROM org_items
                      WHERE org_id = ?1 AND doc_id = ?2 AND tombstoned = 0
                      ORDER BY rev ASC, seq ASC, item_id ASC",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![org_id, doc_id], |r| r.get(0))
                .map_err(map_err)?;
            let mut ids = Vec::new();
            for row in rows {
                ids.push(row.map_err(map_err)?);
            }
            ids
        };
        let mut evicted = false;
        for item_id in item_ids {
            evicted |= Self::tombstone_org_item_tx(tx, &item_id)?;
        }
        Self::purge_org_document_links_tx(tx, org_id, doc_id)?;
        Ok(evicted)
    }

    fn purge_org_document_links_tx(
        tx: &rusqlite::Transaction<'_>,
        org_id: &str,
        doc_id: &str,
    ) -> Result<usize> {
        let endpoint = format!("{org_id}:{doc_id}");
        // Unlike a feed-item tombstone (which may merely precede a successor revision), both
        // callers reached this primitive only after an authoritative stable-document DELETE. Keep
        // a content-free negative witness so a relation living solely in a trash snapshot cannot
        // be resurrected later. A second terminal DELETE always invalidates every exact relink
        // authorization from the previous document incarnation, even though the marker upsert is
        // idempotent. All three mutations share this transaction with the replica eviction.
        tx.execute(
            "INSERT OR IGNORE INTO org_document_terminal_deletions (org_id, doc_id)
             VALUES (?1, ?2)",
            rusqlite::params![org_id, doc_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "DELETE FROM org_link_reauthorizations
              WHERE (src_kind = 'org' AND src_id = ?1)
                 OR (dst_kind = 'org' AND dst_id = ?1)",
            rusqlite::params![endpoint],
        )
        .map_err(map_err)?;
        tx.execute(
            "DELETE FROM links
              WHERE (src_kind='org' AND src_id=?1)
                 OR (dst_kind='org' AND dst_id=?1)",
            rusqlite::params![endpoint],
        )
        .map_err(map_err)
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
        Self::delete_org_task_projection_tx(tx, item_id)?;
        tx.execute(
            "UPDATE org_items SET tombstoned = 1, markdown = '', title = '',
                 projection_sha256 = NULL WHERE item_id = ?1",
            rusqlite::params![item_id],
        )
        .map_err(map_err)?;
        // A relay tombstone removes the readable replica, not the user's private graph choice.
        // `links_for_visible` and every org endpoint resolver join the live membership/context/item
        // witness, so this opaque SQLCipher row is withheld while no authoritative head exists and
        // becomes usable again if a successor revision is later ingested.
        if was_live {
            Self::purge_all_ask_conversations_tx(tx)?;
        }
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

    fn org_membership_witness_tx(
        tx: &rusqlite::Transaction<'_>,
        org_id: &str,
        generation: u32,
    ) -> Result<bool> {
        tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM org_state
              WHERE org_id = ?1 AND generation >= ?2)",
            rusqlite::params![org_id, generation as i64],
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
        // Membership withdrawal purges every decrypted org artifact below, but preserves opaque
        // user-authored `links` rows. All readers require the org_state + enabled + live-item join,
        // so the private relation is completely withheld after leave and can reappear only after a
        // genuine rejoin plus successor ingest. Explicit user unlink remains the sole graph delete.
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
        // Task plaintext and its device-private references live in their own projection rather than
        // `org_chunks`. Purge them in this same membership-withdrawal transaction; the refs follow
        // through `ON DELETE CASCADE`, so list/detail/Dashboard Work cannot retain a stale copy.
        tx.execute(
            "DELETE FROM org_tasks WHERE org_id = ?1",
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
        if items > 0 {
            Self::purge_all_ask_conversations_tx(tx)?;
        }
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
                    AND COALESCE(oi.source_kind, '') NOT IN ('task','container')
                    AND (oi.doc_id IS NULL OR oi.is_current = 1)
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
                    AND COALESCE(oi.source_kind, '') NOT IN ('task','container')
                    AND (oi.doc_id IS NULL OR oi.is_current = 1)
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
                  WHERE oi.item_id = ?1 AND oi.tombstoned = 0
                    AND COALESCE(oi.source_kind, '') NOT IN ('task','container')
                    AND (oi.doc_id IS NULL OR oi.is_current = 1)",
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
                   FROM org_items WHERE item_id = ?1 AND tombstoned = 0
                    AND COALESCE(source_kind, '') NOT IN ('task','container')
                    AND (doc_id IS NULL OR is_current = 1)",
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
            "SELECT tombstoned, content_sha256, projection_sha256 FROM org_items WHERE item_id = ?1",
            rusqlite::params![item_id],
            |r| {
                Ok(OrgReplicaState {
                    tombstoned: r.get::<_, i64>(0)? != 0,
                    content_sha256: r.get::<_, Option<Vec<u8>>>(1)?,
                    projection_sha256: r.get::<_, Option<Vec<u8>>>(2)?,
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
    #[cfg(test)]
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
        self.commit_org_reconcile_item_with_metadata(
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
            None,
            "view",
            None,
            false,
        )
        .map(|outcome| outcome.changed)
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_org_reconcile_item_with_metadata(
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
        doc_id: Option<&str>,
        access: &str,
        document_owner_user_id: Option<&str>,
        is_current: bool,
    ) -> Result<OrgMetadataCommitOutcome> {
        self.commit_org_reconcile_item_with_metadata_and_attachments(
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
            doc_id,
            access,
            document_owner_user_id,
            is_current,
            &[],
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_org_reconcile_item_with_metadata_and_attachments(
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
        doc_id: Option<&str>,
        access: &str,
        document_owner_user_id: Option<&str>,
        is_current: bool,
        attachments: &[crate::storage::IncomingAttachment],
    ) -> Result<OrgMetadataCommitOutcome> {
        if let Some(doc_id) = doc_id {
            require_stable_uuid(org_id, "invalid organization id")?;
            require_stable_uuid(doc_id, "invalid org document id")?;
        }
        if !matches!(access, "view" | "edit") {
            return Err(crate::error::AppError::InvalidArg(
                "invalid org item access".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        if !Self::org_membership_witness_tx(&tx, org_id, generation)? {
            return Ok(OrgMetadataCommitOutcome::default());
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
            return Ok(OrgMetadataCommitOutcome::default());
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
        Self::replace_org_item_attachment_bundle_tx(&tx, item_id, attachments, content_sha256)?;
        Self::set_org_item_document_metadata_tx(
            &tx,
            item_id,
            doc_id,
            access,
            document_owner_user_id,
        )?;
        let visibility_reduced =
            Self::set_org_item_current_tx(&tx, item_id, org_id, doc_id, is_current)?;
        if source_kind == Some("task") {
            Self::upsert_org_task_projection_tx(
                &tx,
                item_id,
                org_id,
                doc_id,
                markdown,
                access,
                author_user_id,
                document_owner_user_id,
                rev,
                generation,
                seq,
            )?;
        }
        Self::close_projection_pending_for_item_tx(
            &tx,
            item_id,
            org_id,
            doc_id,
            access,
            rev,
            generation,
            content_sha256,
            author_user_id,
            document_owner_user_id,
        )?;
        if visibility_reduced {
            Self::purge_all_ask_conversations_tx(&tx)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(OrgMetadataCommitOutcome {
            changed: true,
            visibility_reduced,
        })
    }

    /// Metadata-only anti-entropy repair for a hash-converged live row. No content/chunks are read or
    /// rewritten, and an existing tombstone remains permanent.
    // Metadata repair mirrors the authoritative feed identity tuple atomically and intentionally
    // spells out every witness field at the call site.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn repair_org_reconcile_metadata(
        &self,
        item_id: &str,
        org_id: &str,
        generation: u32,
        doc_id: Option<&str>,
        access: &str,
        document_owner_user_id: Option<&str>,
        is_current: bool,
    ) -> Result<OrgMetadataCommitOutcome> {
        if let Some(doc_id) = doc_id {
            require_stable_uuid(org_id, "invalid organization id")?;
            require_stable_uuid(doc_id, "invalid org document id")?;
        }
        if !matches!(access, "view" | "edit") {
            return Err(crate::error::AppError::InvalidArg(
                "invalid org item access".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        if !Self::org_membership_witness_tx(&tx, org_id, generation)? {
            return Ok(OrgMetadataCommitOutcome::default());
        }
        let current = tx
            .query_row(
                "SELECT doc_id, access, document_owner_user_id, is_current FROM org_items
                  WHERE item_id = ?1 AND org_id = ?2 AND tombstoned = 0",
                rusqlite::params![item_id, org_id],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, i64>(3)? != 0,
                    ))
                },
            )
            .optional()
            .map_err(map_err)?;
        let Some((stored_doc_id, stored_access, stored_owner, stored_current)) = current else {
            return Ok(OrgMetadataCommitOutcome::default());
        };
        if stored_doc_id.as_deref() == doc_id
            && stored_access == access
            && stored_owner.as_deref() == document_owner_user_id
            && stored_current == is_current
        {
            return Ok(OrgMetadataCommitOutcome::default());
        }
        Self::set_org_item_document_metadata_tx(
            &tx,
            item_id,
            doc_id,
            access,
            document_owner_user_id,
        )?;
        let visibility_reduced =
            Self::set_org_item_current_tx(&tx, item_id, org_id, doc_id, is_current)?;
        if visibility_reduced {
            Self::purge_all_ask_conversations_tx(&tx)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(OrgMetadataCommitOutcome {
            changed: true,
            visibility_reduced,
        })
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
                AND COALESCE(oi.source_kind, '') NOT IN ('task','container')
                AND (oi.doc_id IS NULL OR oi.is_current = 1)
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
        let conn = self.lock();
        let Some(and_expr) = fts_match_query(q) else {
            return Ok(Vec::new()); // punctuation-only / empty query → no hits, never an FTS error.
        };
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
                AND COALESCE(oi.source_kind, '') NOT IN ('task','container')
                AND (oi.doc_id IS NULL OR oi.is_current = 1)
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
        // Preserve the full pre-fallback strict query exactly. In particular, do not cap,
        // deduplicate, or stopword-filter it: the bounded canonical list belongs only to the
        // fallback and a row matching merely the first 32 terms must not satisfy a longer strict
        // query. Only an empty strict result may activate the fallback.
        let mut rows_vec = run(&mut stmt, &and_expr)?;
        if rows_vec.is_empty() {
            let terms = fts_unicode61_content_terms(&conn, q, MAX_ORG_FTS_CONTENT_TERMS)?;
            let Some(any_expr) = fts_match_content_terms_any(&terms) else {
                return Ok(Vec::new());
            };
            if any_expr != and_expr {
                // Relevance floor: an OR candidate must match at least ceil(unique_terms / 2)
                // EXACT FTS tokens in one chunk. Each UNION branch is one unique query term and
                // returns a rowid only when unicode61 MATCH sees that whole token (`Kong` therefore
                // does not match `Kongo`). GROUP/HAVING qualifies coverage before BM25 ORDER/LIMIT;
                // Rust never inspects snippets or performs substring matching.
                let branches = terms
                    .iter()
                    .map(|_| {
                        "SELECT rowid AS chunk_rowid
                           FROM fts_org_chunks
                          WHERE fts_org_chunks MATCH ?"
                    })
                    .collect::<Vec<_>>()
                    .join(" UNION ALL ");
                let threshold = i64::try_from(terms.len().div_ceil(2)).map_err(|_| {
                    crate::error::AppError::InvalidArg(
                        "org search query has too many unique content terms".to_string(),
                    )
                })?;
                let fallback_sql = format!(
                    "WITH matched_terms(chunk_rowid) AS ({branches}),
                          qualified_chunks(chunk_rowid) AS (
                              SELECT chunk_rowid
                                FROM matched_terms
                               GROUP BY chunk_rowid
                              HAVING COUNT(*) >= ?
                          )
                     SELECT oi.item_id, oi.author_hint, oi.title, oc.text, oi.content_sha256,
                            bm25(fts_org_chunks) AS rank
                       FROM fts_org_chunks
                       JOIN qualified_chunks qc ON qc.chunk_rowid = fts_org_chunks.rowid
                       JOIN org_chunks oc ON oc.id = fts_org_chunks.rowid
                       JOIN org_items oi ON oi.item_id = oc.item_id
                       JOIN org_state os ON os.org_id = oi.org_id
                      WHERE fts_org_chunks MATCH ?
                        AND oi.tombstoned = 0
                        AND os.context_enabled = 1
                        AND COALESCE(oi.source_kind, '') NOT IN ('task','container')
                        AND (oi.doc_id IS NULL OR oi.is_current = 1)
                      ORDER BY rank ASC, oi.item_id ASC
                      LIMIT ?"
                );
                let mut params: Vec<rusqlite::types::Value> = terms
                    .iter()
                    // `terms` contains only Unicode alphanumerics, but quote each one to pin exact
                    // FTS-token semantics and keep it inert as MATCH syntax.
                    .map(|term| rusqlite::types::Value::Text(format!("\"{term}\"")))
                    .collect();
                params.push(rusqlite::types::Value::Integer(threshold));
                params.push(rusqlite::types::Value::Text(any_expr));
                params.push(rusqlite::types::Value::Integer(sql_cap));

                let mut fallback_stmt = conn.prepare(&fallback_sql).map_err(map_err)?;
                let fallback_rows = fallback_stmt
                    .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                        Ok(OrgChunkHit {
                            item_id: row.get(0)?,
                            author_hint: row.get(1)?,
                            title: row.get(2)?,
                            snippet: row.get(3)?,
                            content_sha256: row.get::<_, Option<Vec<u8>>>(4)?.unwrap_or_default(),
                        })
                    })
                    .map_err(map_err)?;
                rows_vec.clear();
                for row in fallback_rows {
                    rows_vec.push(row.map_err(map_err)?);
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
            "SELECT oi.item_id, oi.doc_id, oi.author_hint, oi.title, oi.created_at, oi.rev,
                    oi.markdown, oi.access
               FROM org_items oi
               JOIN org_state os ON os.org_id = oi.org_id
              WHERE oi.item_id = ?1 AND oi.tombstoned = 0 AND os.context_enabled = 1
                AND COALESCE(oi.source_kind, '') NOT IN ('task','container')
                AND (oi.doc_id IS NULL OR oi.is_current = 1)",
            rusqlite::params![item_id],
            |r| {
                Ok(crate::storage::models::OrgItemDetail {
                    item_id: r.get(0)?,
                    doc_id: r.get(1)?,
                    link_id: None,
                    author_hint: r.get(2)?,
                    title: r.get(3)?,
                    created_at: r.get(4)?,
                    rev: r.get::<_, i64>(5)? as u32,
                    markdown: r.get(6)?,
                    access: r.get(7)?,
                    // The DB layer has no session context — the `org_get_item` command computes the real
                    // value by comparing the stored `author_user_id` with the caller's `server_user_id`.
                    editable: false,
                    can_edit: false,
                    can_manage: false,
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

    pub fn set_org_item_document_metadata(
        &self,
        item_id: &str,
        doc_id: Option<&str>,
        access: &str,
        document_owner_user_id: Option<&str>,
    ) -> Result<()> {
        if let Some(doc_id) = doc_id {
            require_stable_uuid(doc_id, "invalid org document id")?;
        }
        if !matches!(access, "view" | "edit") {
            return Err(crate::error::AppError::InvalidArg(
                "invalid org item access".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        if doc_id.is_some() {
            let org_id = tx
                .query_row(
                    "SELECT org_id FROM org_items WHERE item_id = ?1 AND tombstoned = 0",
                    rusqlite::params![item_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(map_err)?;
            if let Some(org_id) = org_id.as_deref() {
                require_stable_uuid(org_id, "invalid organization id")?;
            }
        }
        Self::set_org_item_document_metadata_tx(
            &tx,
            item_id,
            doc_id,
            access,
            document_owner_user_id,
        )?;
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    fn set_org_item_document_metadata_tx(
        tx: &rusqlite::Transaction<'_>,
        item_id: &str,
        doc_id: Option<&str>,
        access: &str,
        document_owner_user_id: Option<&str>,
    ) -> Result<()> {
        if let Some(doc_id) = doc_id {
            require_stable_uuid(doc_id, "invalid org document id")?;
        }
        tx.execute(
            "UPDATE org_items SET doc_id = COALESCE(?2, doc_id), access = ?3,
                    document_owner_user_id = COALESCE(?4, document_owner_user_id)
              WHERE item_id = ?1 AND tombstoned = 0",
            rusqlite::params![item_id, doc_id, access, document_owner_user_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    fn set_org_item_current_tx(
        tx: &rusqlite::Transaction<'_>,
        item_id: &str,
        org_id: &str,
        doc_id: Option<&str>,
        is_current: bool,
    ) -> Result<bool> {
        let target_was_current = tx
            .query_row(
                "SELECT is_current FROM org_items
                  WHERE item_id = ?1 AND org_id = ?2 AND tombstoned = 0",
                rusqlite::params![item_id, org_id],
                |row| Ok(row.get::<_, i64>(0)? != 0),
            )
            .optional()
            .map_err(map_err)?;
        let Some(target_was_current) = target_was_current else {
            return Ok(false);
        };
        let visibility_reduced = if is_current {
            let doc_id = doc_id.ok_or_else(|| {
                crate::error::AppError::InvalidArg("current org item missing doc id".into())
            })?;
            let distinct_current_demoted = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM org_items
                       WHERE org_id = ?1 AND doc_id = ?2 AND item_id != ?3
                         AND tombstoned = 0 AND is_current = 1)",
                    rusqlite::params![org_id, doc_id, item_id],
                    |row| row.get(0),
                )
                .map_err(map_err)?;
            tx.execute(
                "UPDATE org_items SET is_current = 0
                  WHERE org_id = ?1 AND doc_id = ?2 AND item_id != ?3",
                rusqlite::params![org_id, doc_id, item_id],
            )
            .map_err(map_err)?;
            distinct_current_demoted
        } else {
            target_was_current
        };
        tx.execute(
            "UPDATE org_items SET is_current = ?2 WHERE item_id = ?1 AND tombstoned = 0",
            rusqlite::params![item_id, is_current as i64],
        )
        .map_err(map_err)?;
        Ok(visibility_reduced)
    }

    /// Current locally-ingested head metadata for a stable document. This is a content-free
    /// management resolver: it does not mutate the origin share row's CAS baseline.
    pub fn current_org_document_status(
        &self,
        org_id: &str,
        doc_id: &str,
    ) -> Result<Option<(String, u32, String)>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT oi.item_id, oi.rev, oi.access
               FROM org_items oi
               JOIN org_state os ON os.org_id = oi.org_id
              WHERE oi.org_id = ?1 AND oi.doc_id = ?2 AND oi.tombstoned = 0
                AND oi.is_current = 1 AND os.context_enabled = 1
              LIMIT 1",
            rusqlite::params![org_id, doc_id],
            |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u32, r.get(2)?)),
        )
        .optional()
        .map_err(map_err)
    }

    pub fn set_org_share_access_for_document(
        &self,
        org_id: &str,
        doc_id: &str,
        access: &str,
    ) -> Result<()> {
        require_stable_uuid(org_id, "invalid organization id")?;
        require_stable_uuid(doc_id, "invalid org document id")?;
        if !matches!(access, "view" | "edit") {
            return Err(crate::error::AppError::InvalidArg(
                "invalid org item access".into(),
            ));
        }
        let conn = self.lock();
        conn.execute(
            "UPDATE org_shares SET access = ?3 WHERE org_id = ?1 AND doc_id = ?2",
            rusqlite::params![org_id, doc_id, access],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Apply the relay-confirmed permission state to the complete local stable-document resource.
    /// Every LIVE replica revision and the origin share's display metadata move together in ONE
    /// SQLCipher transaction. The origin share's `item_id`, `rev`, and content hash are deliberately
    /// untouched: they remain the last-published CAS witness, so a collaborator edit still produces
    /// a real 409 on the next automatic source republish.
    pub fn set_org_document_access_metadata(
        &self,
        org_id: &str,
        doc_id: &str,
        access: &str,
        document_owner_user_id: &str,
    ) -> Result<bool> {
        require_stable_uuid(org_id, "invalid organization id")?;
        require_stable_uuid(doc_id, "invalid org document id")?;
        if !matches!(access, "view" | "edit") {
            return Err(crate::error::AppError::InvalidArg(
                "invalid org item access".into(),
            ));
        }
        if document_owner_user_id.trim().is_empty() {
            return Err(crate::error::AppError::InvalidArg(
                "missing org document owner".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let blocked: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM org_shares
                  WHERE org_id = ?1 AND doc_id = ?2 AND state = 'failed'
                    AND last_error IN ('direct_put_pending','republish_put_pending',
                       'republish_post_pending','initial_post_pending','initial_post_replayable',
                       'projection_pending','recovery_witness_missing','org_edit_conflict'))",
                rusqlite::params![org_id, doc_id],
                |row| Ok(row.get::<_, i64>(0)? != 0),
            )
            .map_err(map_err)?;
        if blocked {
            return Ok(false);
        }
        tx.execute(
            "UPDATE org_items
                SET access = ?3, document_owner_user_id = ?4
              WHERE org_id = ?1 AND doc_id = ?2 AND tombstoned = 0",
            rusqlite::params![org_id, doc_id, access, document_owner_user_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE org_shares SET access = ?3 WHERE org_id = ?1 AND doc_id = ?2",
            rusqlite::params![org_id, doc_id, access],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(true)
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
            "SELECT org_id, doc_id, rev, created_at, author_hint, source_kind, author_user_id,
                    document_owner_user_id, access
               FROM org_items WHERE item_id = ?1 AND tombstoned = 0
                AND (doc_id IS NULL OR is_current = 1)",
            rusqlite::params![item_id],
            |r| {
                Ok(crate::storage::models::OrgItemEditCtx {
                    org_id: r.get(0)?,
                    doc_id: r.get(1)?,
                    rev: r.get::<_, i64>(2)? as u32,
                    created_at: r.get(3)?,
                    author_hint: r.get(4)?,
                    source_kind: r.get::<_, Option<String>>(5)?,
                    author_user_id: r.get::<_, Option<String>>(6)?,
                    document_owner_user_id: r.get::<_, Option<String>>(7)?,
                    access: r.get(8)?,
                })
            },
        )
        .optional()
        .map_err(map_err)
    }

    /// Resolve the current live revision of a stable document for permission management. The
    /// origin `org_shares` row is deliberately left untouched so its expected revision remains an
    /// honest CAS witness and a remote edit produces 409 on automatic republish.
    pub fn org_item_edit_ctx_by_document(
        &self,
        org_id: &str,
        doc_id: &str,
    ) -> Result<Option<crate::storage::models::OrgItemEditCtx>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT org_id, doc_id, rev, created_at, author_hint, source_kind, author_user_id,
                    document_owner_user_id, access
               FROM org_items
              WHERE org_id = ?1 AND doc_id = ?2 AND tombstoned = 0 AND is_current = 1
              LIMIT 1",
            rusqlite::params![org_id, doc_id],
            |r| {
                Ok(crate::storage::models::OrgItemEditCtx {
                    org_id: r.get(0)?,
                    doc_id: r.get(1)?,
                    rev: r.get::<_, i64>(2)? as u32,
                    created_at: r.get(3)?,
                    author_hint: r.get(4)?,
                    source_kind: r.get(5)?,
                    author_user_id: r.get(6)?,
                    document_owner_user_id: r.get(7)?,
                    access: r.get(8)?,
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
    /// the caller is a local member of `org_id` before calling this. The result is bounded to the
    /// newest 500 headers so a malformed or unexpectedly large local replica cannot allocate an
    /// unbounded IPC payload.
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
                "SELECT item_id, doc_id, title, author_hint, created_at, seq, source_kind
                   FROM org_items
                  WHERE org_id = ?1 AND tombstoned = 0
                    AND COALESCE(source_kind, '') NOT IN ('task','container')
                    AND (doc_id IS NULL OR is_current = 1)
                  ORDER BY seq DESC
                  LIMIT 500",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![org_id], |r| {
                Ok(crate::storage::models::OrgItemHeader {
                    item_id: r.get(0)?,
                    doc_id: r.get(1)?,
                    title: r.get(2)?,
                    author_hint: r.get(3)?,
                    created_at: r.get(4)?,
                    seq: r.get::<_, i64>(5)? as u64,
                    // Direct from storage now (see doc comment above); `list_org_items_inner` may still
                    // enrich/override for the caller's own items via the local `org_shares` resolver.
                    kind: r.get::<_, Option<String>>(6)?,
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
              WHERE oi.org_id = ?1 AND oi.tombstoned = 0 AND os.context_enabled = 1
                AND COALESCE(oi.source_kind, '') NOT IN ('task','container')
                AND (oi.doc_id IS NULL OR oi.is_current = 1)",
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
                    AND COALESCE(oi.source_kind, '') NOT IN ('task','container')
                    AND (oi.doc_id IS NULL OR oi.is_current = 1)
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

    /// Atomically copy the current, context-enabled head of a RECEIVED Shared Brain item into an
    /// open local container. The org replica is retained. A document becomes an authored note; a
    /// meeting becomes a local meeting shell plus its provider snapshot note. Audio is deliberately
    /// absent: the relay never supplied an authenticated local recording and this import never
    /// invents one.
    pub fn import_received_org_item_atomic(
        &self,
        item_id: &str,
        folder_id: &str,
    ) -> Result<crate::storage::models::OrgItemImportResult> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        // Read only metadata and SQLite-computed byte length first. Pulling `markdown` into Rust
        // before checking its size would already make the resource bound ineffective.
        let source = tx
            .query_row(
                "SELECT oi.org_id, oi.source_kind, oi.title, oi.created_at,
                        length(CAST(oi.markdown AS BLOB))
                   FROM org_items oi
                   JOIN org_state os ON os.org_id=oi.org_id
                  WHERE oi.item_id=?1 AND oi.tombstoned=0 AND os.context_enabled=1
                    AND (oi.doc_id IS NULL OR oi.is_current=1)
                    AND COALESCE(oi.source_kind,'') IN ('document','meeting')
                    AND NOT EXISTS (
                      SELECT 1 FROM org_shares s WHERE s.item_id=oi.item_id
                    )",
                rusqlite::params![item_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(map_err)?
            .ok_or_else(|| {
                crate::error::AppError::InvalidArg(
                    "this received Shared Brain item is unavailable or no longer current".into(),
                )
            })?;
        let (org_id, source_kind, title, source_created_at, markdown_bytes) = source;
        let markdown_bytes = usize::try_from(markdown_bytes).map_err(|_| {
            crate::error::AppError::InvalidArg(
                "shared content has an invalid markdown length".into(),
            )
        })?;
        if markdown_bytes > MAX_RECEIVED_ORG_IMPORT_MARKDOWN_BYTES {
            return Err(crate::error::AppError::InvalidArg(
                "shared note is too large to add to a Space".into(),
            ));
        }

        // Re-check the destination inside the same transaction that births the copy. Locked targets
        // are refused before any local row or attachment is written; seal-on-birth is intentionally
        // not approximated here.
        let target_open = tx
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM folders
                    WHERE id=?1 AND locked=0
                      AND COALESCE(kind,'meeting') IN ('meeting','note')
                      AND LOWER(path) <> '.murmur'
                      AND LOWER(path) NOT LIKE '.murmur/%'
                 )",
                rusqlite::params![folder_id],
                |row| row.get::<_, bool>(0),
            )
            .map_err(map_err)?;
        if !target_open {
            return Err(crate::error::AppError::Locked(
                "unlock the destination before adding shared content".into(),
            ));
        }

        // Validate count and byte totals without selecting attachment BLOBs. Check both the
        // authenticated `byte_len` witness and SQLite's actual BLOB length: a corrupt legacy row
        // must not evade the cap by lying in either direction. No local mutation has happened yet.
        let attachment_bounds = tx
            .query_row(
                "SELECT COUNT(*),
                        COALESCE(MAX(byte_len),0),
                        COALESCE(MAX(length(data)),0),
                        COALESCE(SUM(byte_len),0),
                        COALESCE(SUM(length(data)),0),
                        COALESCE(SUM(CASE WHEN byte_len != length(data) THEN 1 ELSE 0 END),0)
                   FROM note_attachments WHERE org_item_id=?1",
                rusqlite::params![item_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .map_err(map_err)?;
        let (attachment_count, max_declared, max_actual, declared_total, actual_total, mismatches) =
            attachment_bounds;
        let attachment_count = usize::try_from(attachment_count).map_err(|_| {
            crate::error::AppError::InvalidArg("invalid shared attachment count".into())
        })?;
        let max_declared = usize::try_from(max_declared).map_err(|_| {
            crate::error::AppError::InvalidArg("invalid shared attachment length".into())
        })?;
        let max_actual = usize::try_from(max_actual).map_err(|_| {
            crate::error::AppError::InvalidArg("invalid shared attachment length".into())
        })?;
        let declared_total = usize::try_from(declared_total).map_err(|_| {
            crate::error::AppError::InvalidArg("invalid shared attachment total".into())
        })?;
        let actual_total = usize::try_from(actual_total).map_err(|_| {
            crate::error::AppError::InvalidArg("invalid shared attachment total".into())
        })?;
        let imported_total = markdown_bytes.checked_add(actual_total).ok_or_else(|| {
            crate::error::AppError::InvalidArg("shared import size overflow".into())
        })?;
        if mismatches != 0 {
            return Err(crate::error::AppError::InvalidArg(
                "shared attachment metadata is inconsistent".into(),
            ));
        }
        if attachment_count > MAX_RECEIVED_ORG_IMPORT_ATTACHMENTS {
            return Err(crate::error::AppError::InvalidArg(
                "shared item has too many attachments to add to a Space".into(),
            ));
        }
        if max_declared > MAX_RECEIVED_ORG_IMPORT_ATTACHMENT_BYTES
            || max_actual > MAX_RECEIVED_ORG_IMPORT_ATTACHMENT_BYTES
        {
            return Err(crate::error::AppError::InvalidArg(
                "a shared attachment is too large to add to a Space".into(),
            ));
        }
        if declared_total > MAX_RECEIVED_ORG_IMPORT_ATTACHMENT_TOTAL_BYTES
            || actual_total > MAX_RECEIVED_ORG_IMPORT_ATTACHMENT_TOTAL_BYTES
            || imported_total > MAX_RECEIVED_ORG_IMPORT_TOTAL_BYTES
        {
            return Err(crate::error::AppError::InvalidArg(
                "shared item is too large to add to a Space".into(),
            ));
        }

        // The preflight above makes this first plaintext allocation explicitly bounded.
        let markdown = tx
            .query_row(
                "SELECT markdown FROM org_items WHERE item_id=?1",
                rusqlite::params![item_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(map_err)?;

        let mut attachment_stmt = tx
            .prepare(
                "SELECT id,mime_type,extension,byte_len,width,height,sha256,data,created_at
                   FROM note_attachments WHERE org_item_id=?1 ORDER BY created_at,id",
            )
            .map_err(map_err)?;
        let source_attachments = attachment_stmt
            .query_map(rusqlite::params![item_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Vec<u8>>(6)?,
                    row.get::<_, Vec<u8>>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            })
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        drop(attachment_stmt);

        let mut remap = HashMap::new();
        for (old_id, ..) in &source_attachments {
            remap.insert(old_id.clone(), uuid::Uuid::new_v4().to_string());
        }
        let markdown = crate::share::envelope::remap_share_images(&markdown, &remap);
        let local_id = uuid::Uuid::new_v4().to_string();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let now_iso = chrono::Utc::now().to_rfc3339();
        let clean_title = if title.trim().is_empty() {
            crate::storage::db::UNTITLED_TITLE
        } else {
            title.trim()
        };

        let local_kind = if source_kind == "document" {
            tx.execute(
                "INSERT INTO documents
                   (id,folder_id,name,title,text,kind,text_blob,created_at,updated_at,exported_path)
                 VALUES (?1,?2,?3,?4,?5,'note',NULL,?6,?6,NULL)",
                rusqlite::params![
                    local_id,
                    folder_id,
                    crate::export::sanitize_title(clean_title),
                    clean_title,
                    markdown,
                    now_ms,
                ],
            )
            .map_err(map_err)?;
            "note"
        } else {
            tx.execute(
                "INSERT INTO meetings
                   (id,started_at,ended_at,title,duration_s,audio_path,status,folder_id)
                 VALUES (?1,?2,?2,?3,0,NULL,'SUMMARIZED',?4)",
                rusqlite::params![local_id, source_created_at, clean_title, folder_id],
            )
            .map_err(map_err)?;
            tx.execute(
                "INSERT INTO notes
                   (meeting_id,provider_id,markdown,created_at,exported_path,folder_id)
                 VALUES (?1,'shared-brain',?2,?3,NULL,?4)",
                rusqlite::params![local_id, markdown, source_created_at, folder_id],
            )
            .map_err(map_err)?;
            "meeting"
        };

        for (old_id, mime, extension, byte_len, width, height, sha256, data, created_at) in
            source_attachments
        {
            let new_id = remap.get(&old_id).ok_or_else(|| {
                crate::error::AppError::Storage("attachment remap disappeared".into())
            })?;
            let (document_id, meeting_id, provider_id) = if local_kind == "note" {
                (Some(local_id.as_str()), None, None)
            } else {
                (None, Some(local_id.as_str()), Some("shared-brain"))
            };
            tx.execute(
                "INSERT INTO note_attachments
                   (id,document_id,meeting_id,provider_id,org_item_id,mime_type,extension,
                    byte_len,width,height,sha256,data,data_blob,exported_path,created_at)
                 VALUES (?1,?2,?3,?4,NULL,?5,?6,?7,?8,?9,?10,?11,NULL,NULL,?12)",
                rusqlite::params![
                    new_id,
                    document_id,
                    meeting_id,
                    provider_id,
                    mime,
                    extension,
                    byte_len,
                    width,
                    height,
                    sha256,
                    data,
                    created_at,
                ],
            )
            .map_err(map_err)?;
        }
        tx.execute(
            "INSERT INTO local_org_imports(local_kind,local_id,org_id,item_id,created_at)
             VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![local_kind, local_id, org_id, item_id, now_iso],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(crate::storage::models::OrgItemImportResult {
            kind: local_kind.to_string(),
            id: local_id,
        })
    }
}

#[cfg(test)]
mod received_import_bounds_tests {
    use super::*;
    use crate::error::AppError;
    use crate::storage::models::Folder;

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn test_db(tag: &str) -> Db {
        let path = crate::storage::db::unique_temp_path(
            &format!("murmur-org-import-bounds-{tag}"),
            "sqlite",
        );
        let _ = std::fs::remove_file(&path);
        let db = Db::open_with_key(&path, TEST_DEK).expect("open test SQLCipher db");
        db.upsert_org_state(&crate::storage::OrgState {
            org_id: "org-bounds".into(),
            name: "Bounds".into(),
            role: "member".into(),
            joined_at: "2026-08-26T00:00:00Z".into(),
            consented: false,
            last_seq: 0,
            generation: 1,
            context_enabled: true,
        })
        .expect("seed org");
        db.insert_folder(&Folder {
            id: "destination".into(),
            name: "Destination".into(),
            path: "Destination".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-08-26T00:00:00Z".into(),
        })
        .expect("seed destination");
        db
    }

    fn seed_received_item(db: &Db, item_id: &str, markdown: &str) {
        db.lock()
            .execute(
                "INSERT INTO org_items
                   (item_id,org_id,seq,author_hint,title,markdown,created_at,rev,generation,
                    content_sha256,is_current,tombstoned,source_kind)
                 VALUES (?1,'org-bounds',1,'peer','Shared item',?2,
                         '2026-08-26T00:00:00Z',1,1,X'01',1,0,'document')",
                rusqlite::params![item_id, markdown],
            )
            .expect("seed received item");
    }

    fn seed_attachment(db: &Db, item_id: &str, index: usize, data: Vec<u8>) {
        let id = format!("{item_id}-attachment-{index}");
        let byte_len = i64::try_from(data.len()).expect("test attachment length fits i64");
        db.lock()
            .execute(
                "INSERT INTO note_attachments
                   (id,document_id,meeting_id,provider_id,org_item_id,mime_type,extension,
                    byte_len,width,height,sha256,data,data_blob,exported_path,created_at)
                 VALUES (?1,NULL,NULL,NULL,?2,'image/png','png',?3,1,1,?4,?5,NULL,NULL,1)",
                rusqlite::params![id, item_id, byte_len, vec![7u8; 32], data],
            )
            .expect("seed received attachment");
    }

    fn mutation_snapshot(db: &Db, item_id: &str) -> (i64, i64, i64, i64, i64, i64) {
        db.lock()
            .query_row(
                "SELECT
                   (SELECT COUNT(*) FROM documents),
                   (SELECT COUNT(*) FROM meetings),
                   (SELECT COUNT(*) FROM local_org_imports),
                   (SELECT length(CAST(markdown AS BLOB)) FROM org_items WHERE item_id=?1),
                   (SELECT COUNT(*) FROM note_attachments WHERE org_item_id=?1),
                   (SELECT COALESCE(SUM(length(data)),0) FROM note_attachments WHERE org_item_id=?1)",
                rusqlite::params![item_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .expect("snapshot import state")
    }

    fn assert_rejected_without_mutation(db: &Db, item_id: &str) {
        let before = mutation_snapshot(db, item_id);
        let error = db
            .import_received_org_item_atomic(item_id, "destination")
            .expect_err("oversized received item must be refused");
        assert!(matches!(error, AppError::InvalidArg(_)));
        assert_eq!(
            mutation_snapshot(db, item_id),
            before,
            "rejection must preserve both the received source and local destination"
        );
    }

    #[test]
    fn received_org_import_accepts_exact_markdown_and_attachment_boundaries() {
        let db = test_db("exact");
        let markdown = "m".repeat(MAX_RECEIVED_ORG_IMPORT_MARKDOWN_BYTES);
        seed_received_item(&db, "item-exact", &markdown);
        seed_attachment(
            &db,
            "item-exact",
            0,
            vec![1u8; MAX_RECEIVED_ORG_IMPORT_ATTACHMENT_BYTES],
        );

        let imported = db
            .import_received_org_item_atomic("item-exact", "destination")
            .expect("exact limits remain importable");
        assert_eq!(imported.kind, "note");
        let local = db
            .get_note_row(&imported.id)
            .expect("read imported note")
            .expect("imported note exists");
        assert_eq!(local.text.len(), MAX_RECEIVED_ORG_IMPORT_MARKDOWN_BYTES);
        let copied = db
            .list_attachments(&crate::storage::AttachmentOwner::Document {
                document_id: imported.id,
            })
            .expect("read copied attachments");
        assert_eq!(copied.len(), 1);
        assert_eq!(
            copied[0].data.len(),
            MAX_RECEIVED_ORG_IMPORT_ATTACHMENT_BYTES
        );
        assert_eq!(
            mutation_snapshot(&db, "item-exact").3,
            MAX_RECEIVED_ORG_IMPORT_MARKDOWN_BYTES as i64,
            "the received replica remains byte-identical"
        );
    }

    #[test]
    fn received_org_import_rejects_each_resource_ceiling_before_any_mutation() {
        let db = test_db("over");

        let markdown = "m".repeat(MAX_RECEIVED_ORG_IMPORT_MARKDOWN_BYTES + 1);
        seed_received_item(&db, "item-markdown-over", &markdown);
        assert_rejected_without_mutation(&db, "item-markdown-over");

        seed_received_item(&db, "item-count-over", "body");
        for index in 0..=MAX_RECEIVED_ORG_IMPORT_ATTACHMENTS {
            seed_attachment(&db, "item-count-over", index, vec![index as u8]);
        }
        assert_rejected_without_mutation(&db, "item-count-over");

        seed_received_item(&db, "item-single-over", "body");
        seed_attachment(
            &db,
            "item-single-over",
            0,
            vec![2u8; MAX_RECEIVED_ORG_IMPORT_ATTACHMENT_BYTES + 1],
        );
        assert_rejected_without_mutation(&db, "item-single-over");

        seed_received_item(&db, "item-length-mismatch", "body");
        seed_attachment(&db, "item-length-mismatch", 0, vec![2u8]);
        db.lock()
            .execute(
                "UPDATE note_attachments SET byte_len=0 WHERE org_item_id='item-length-mismatch'",
                [],
            )
            .expect("corrupt attachment length witness");
        assert_rejected_without_mutation(&db, "item-length-mismatch");

        seed_received_item(&db, "item-attachment-total-over", "body");
        for index in 0..4 {
            seed_attachment(
                &db,
                "item-attachment-total-over",
                index,
                vec![3u8; MAX_RECEIVED_ORG_IMPORT_ATTACHMENT_BYTES],
            );
        }
        assert_rejected_without_mutation(&db, "item-attachment-total-over");

        let markdown = "m".repeat(MAX_RECEIVED_ORG_IMPORT_MARKDOWN_BYTES);
        seed_received_item(&db, "item-total-over", &markdown);
        for index in 0..3 {
            seed_attachment(
                &db,
                "item-total-over",
                index,
                vec![4u8; MAX_RECEIVED_ORG_IMPORT_ATTACHMENT_BYTES],
            );
        }
        assert_rejected_without_mutation(&db, "item-total-over");
    }
}
