//! Bitemporal FACTS + cross-meeting USER-MEMORY storage surface — the `facts` / `user_facts` /
//! `supersessions` persistence and their GATED reads, extracted verbatim from `storage::db`
//! (God-file split, a PURE MOVE — no behavior change). The methods below are an inherent-impl split
//! of [`crate::storage::db::Db`] across files (Rust allows one type's inherent `impl` to live in
//! multiple files of the same crate); every method retains its EXACT prior body, signature, AND
//! gating. The USER-FACING reads (`list_facts_visible`, `list_user_facts_visible`,
//! `search_user_facts_visible`, `list_open_facts_visible`, `fact_rows_for_meeting_visible`) push the
//! session `unlocked` set through `visibility_clause` verbatim, so a sealed-and-not-unlocked
//! meeting's facts stay invisible — this move relocated ONLY the code, not one character of the gate.
//! The INTERNAL un-gated reconcile inputs (`facts_for_entities`, `user_facts_all`) are pipeline-only
//! and were already un-gated by design (documented); a sealed meeting's facts are PURGED on seal.
//!
//! Shared db.rs module-level helpers `map_err` + `visibility_clause` and the Db accessor `lock` are
//! `pub(crate)`; the facts row mappers (`row_to_fact` / `row_to_user_fact` / `row_to_supersession`,
//! whose only callers are these methods) moved along as module-private free fns.
//! `reopen_import_superseded_facts_tx` was PROMOTED to `pub(crate)` because `Db::delete_meeting`
//! (which STAYS in db.rs) still calls `Self::reopen_import_superseded_facts_tx` — an inherent method,
//! resolved cross-file. The seal-purge helpers (`purge_facts_tx` / `purge_user_facts_tx` /
//! `purge_supersessions_tx`) and the schema `migrate_user_facts_fts` + the string-only
//! `facts_for_meeting_visible` reader STAY in db.rs beside the seal/migrate/detail machinery. Tests
//! stay in db.rs's `mod tests` (shared harness); the count is conserved.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, Row};

use crate::error::Result;
use crate::storage::db::{fts_match_query_any, map_err, meeting_visibility_clause, Db};

impl Db {
    // ── bitemporal FACTS layer (brain2 R2) ────────────────────────────────────
    //
    // Facts are DERIVED content tied to a meeting (the `meeting_id` anchor). The reconcile engine
    // (`crate::facts`) is pure + deterministic; these methods are the persistence + the GATED read.
    // LOCK MODEL: `facts_for_entities` is an INTERNAL un-gated read used ONLY by the pipeline to
    // reconcile (never exposed to the FE — like `raw_segments`); every USER-FACING read goes through
    // `list_facts_visible`, and a sealed meeting's facts are PURGED on seal (`purge_facts_tx`).

    /// ALL facts (open + closed) for `entity_ids` — the reconcile input. INTERNAL: this is the
    /// un-gated lifecycle read (the pipeline reconciles before any seal can hide rows), NOT a
    /// user-facing surface. Empty input → empty vec.
    pub fn facts_for_entities(&self, entity_ids: &[String]) -> Result<Vec<crate::facts::Fact>> {
        if entity_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let placeholders = (1..=entity_ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT id, entity_id, subject, predicate, object, valid_from, valid_to, recorded_at, \
                    meeting_id, confidence \
               FROM facts WHERE entity_id IN ({placeholders}) ORDER BY recorded_at ASC, id ASC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let params = rusqlite::params_from_iter(entity_ids.iter());
        let rows = stmt.query_map(params, row_to_fact).map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Apply a batch of reconcile [`FactOp`]s in ONE atomic transaction: INSERT each `Add`, set
    /// `valid_to` on each `Invalidate` (only if still open — idempotent), skip `NoOp`. A fresh UUID
    /// is minted per Add. The whole batch commits or rolls back together, so a crash mid-apply never
    /// leaves a half-reconciled (e.g. old closed but new not added) store.
    pub fn apply_fact_ops(&self, ops: &[crate::facts::FactOp]) -> Result<()> {
        use crate::facts::FactOp;
        if ops.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        for op in ops {
            match op {
                FactOp::Add(nf) => {
                    let id = uuid::Uuid::new_v4().to_string();
                    tx.execute(
                        "INSERT INTO facts \
                           (id, entity_id, subject, predicate, object, valid_from, valid_to, \
                            recorded_at, meeting_id, confidence) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9)",
                        rusqlite::params![
                            id,
                            nf.entity_id,
                            nf.subject,
                            nf.predicate,
                            nf.object,
                            nf.valid_from,
                            nf.recorded_at,
                            nf.meeting_id,
                            nf.confidence,
                        ],
                    )
                    .map_err(map_err)?;
                }
                FactOp::Invalidate { id, valid_to } => {
                    tx.execute(
                        "UPDATE facts SET valid_to = ?2 WHERE id = ?1 AND valid_to IS NULL",
                        rusqlite::params![id, valid_to],
                    )
                    .map_err(map_err)?;
                }
                FactOp::NoOp => {}
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// GATED read: the VISIBLE facts for `entity_id` (open + recently-closed), newest valid_from
    /// first. A fact is visible iff its source meeting is visible under the SAME predicate as every
    /// other graph/MCP read (`EXISTS(visible note) OR NOT EXISTS(any note)`), so a
    /// sealed-and-not-session-unlocked meeting's facts surface NOTHING. A fact with a NULL
    /// `meeting_id` (legacy/unattributed) is NOT visible — the INNER JOIN to `meetings` drops it
    /// (fail-closed). This is the single user-facing fact read (UI dossier + egress-free MCP).
    pub fn list_facts_visible(
        &self,
        entity_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<crate::facts::Fact>> {
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let sql = format!(
            "SELECT ft.id, ft.entity_id, ft.subject, ft.predicate, ft.object, ft.valid_from, \
                    ft.valid_to, ft.recorded_at, ft.meeting_id, ft.confidence \
               FROM facts ft \
               JOIN meetings m ON m.id = ft.meeting_id \
              WHERE ft.entity_id = ?1 \
                AND {meeting_visible} \
              ORDER BY (ft.valid_to IS NULL) DESC, ft.valid_from DESC, ft.id DESC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![entity_id], row_to_fact)
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    // ── Re-Truth (supersessions) ────────────────────────────────────────────────
    //
    // These are RAW lifecycle reads/writes of the `supersessions` table; the CONTENT gate
    // (folder-lock + `meeting_is_unlocked`) lives in the commands, exactly like `facts_for_entities`
    // (raw) vs `list_facts_visible` (gated). Rows are only ever RECORDED for open-folder sources, and
    // the read command re-gates, so a raw list never leaks a sealed source note's bytes.

    /// Record supersession rows, skipping any whose natural key already exists (idempotent across
    /// re-summarize — a plain re-summarize reconciles to `NoOp` and emits no new `Invalidate`, but
    /// this dedupe is the belt-and-suspenders guard). Returns how many NEW rows were inserted. Each
    /// row is inserted UNAPPLIED (`applied_at` + both pre-images NULL); the pre-images are filled only
    /// at apply time. One atomic transaction for the batch.
    pub fn record_supersessions(
        &self,
        rows: &[crate::storage::models::SupersessionRow],
    ) -> Result<usize> {
        if rows.is_empty() {
            return Ok(0);
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let mut inserted = 0usize;
        for r in rows {
            // Natural key = the full assertion (both meetings + entity/predicate/old/new). A duplicate
            // is a no-op regardless of applied state, so a re-record never resurrects a stamped row.
            let exists: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM supersessions
                       WHERE superseding_meeting_id = ?1 AND source_meeting_id = ?2
                         AND entity = ?3 AND predicate = ?4 AND old_value = ?5 AND new_value = ?6",
                    rusqlite::params![
                        r.superseding_meeting_id,
                        r.source_meeting_id,
                        r.entity,
                        r.predicate,
                        r.old_value,
                        r.new_value,
                    ],
                    |row| row.get(0),
                )
                .map_err(map_err)?;
            if exists > 0 {
                continue;
            }
            tx.execute(
                "INSERT INTO supersessions
                   (id, superseding_meeting_id, source_meeting_id, entity, predicate,
                    old_value, new_value, created_at, applied_at, source_pre_image,
                    superseding_pre_image)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, NULL)",
                rusqlite::params![
                    r.id,
                    r.superseding_meeting_id,
                    r.source_meeting_id,
                    r.entity,
                    r.predicate,
                    r.old_value,
                    r.new_value,
                    r.created_at,
                ],
            )
            .map_err(map_err)?;
            inserted += 1;
        }
        tx.commit().map_err(map_err)?;
        Ok(inserted)
    }

    /// RAW read: UNAPPLIED supersession rows for one superseding meeting, oldest first. The command
    /// re-gates each row on its SOURCE meeting (folder-lock + unlock) before surfacing it.
    pub fn unapplied_supersessions_for(
        &self,
        superseding_meeting_id: &str,
    ) -> Result<Vec<crate::storage::models::SupersessionRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, superseding_meeting_id, source_meeting_id, entity, predicate,
                        old_value, new_value, created_at, applied_at, source_pre_image,
                        superseding_pre_image
                   FROM supersessions
                  WHERE superseding_meeting_id = ?1 AND applied_at IS NULL
                  ORDER BY created_at ASC, id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(
                rusqlite::params![superseding_meeting_id],
                row_to_supersession,
            )
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// RAW read: EVERY supersession row that touches `meeting_id` on either side (source OR
    /// superseding), oldest first. Used by partial-undo to enumerate the SURVIVOR stamps on a note
    /// file (rows NOT being undone that remain applied) so their on-disk stamps can be replayed after
    /// the file is reverted to pristine. `path ↔ meeting` is 1:1 (each note has a unique `.md`), so a
    /// meeting id keys the file's survivors.
    pub fn supersessions_touching_meeting(
        &self,
        meeting_id: &str,
    ) -> Result<Vec<crate::storage::models::SupersessionRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, superseding_meeting_id, source_meeting_id, entity, predicate,
                        old_value, new_value, created_at, applied_at, source_pre_image,
                        superseding_pre_image
                   FROM supersessions
                  WHERE source_meeting_id = ?1 OR superseding_meeting_id = ?1
                  ORDER BY created_at ASC, id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![meeting_id], row_to_supersession)
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// RAW read: one supersession row by id (for apply/undo).
    pub fn get_supersession(
        &self,
        id: &str,
    ) -> Result<Option<crate::storage::models::SupersessionRow>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, superseding_meeting_id, source_meeting_id, entity, predicate,
                    old_value, new_value, created_at, applied_at, source_pre_image,
                    superseding_pre_image
               FROM supersessions WHERE id = ?1",
            rusqlite::params![id],
            row_to_supersession,
        )
        .optional()
        .map_err(map_err)
    }

    /// DURABLE-BEFORE-WRITE undo capture: persist the exact PRISTINE (pre-batch) pre-image bytes of
    /// the note(s) a supersession is about to stamp, BEFORE the `.md` is written, so a crash between
    /// the write and `mark_supersession_applied` still leaves a recoverable un-stamped pre-image. The
    /// apply path resolves the bytes from a per-note-file pristine cache and calls this at most once
    /// per field — and never re-snapshots a possibly-stamped file. `COALESCE` makes a `None` argument
    /// a NO-OP for that column (it never clobbers an already-stored pristine backup — e.g. a retry
    /// where the superseding note has since sealed). `applied_at` is left untouched (still NULL until
    /// the write completes).
    pub fn store_supersession_pre_images(
        &self,
        id: &str,
        source_pre_image: Option<&[u8]>,
        superseding_pre_image: Option<&[u8]>,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE supersessions
                SET source_pre_image = COALESCE(?2, source_pre_image),
                    superseding_pre_image = COALESCE(?3, superseding_pre_image)
              WHERE id = ?1",
            rusqlite::params![id, source_pre_image, superseding_pre_image],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Mark a supersession APPLIED: stamp `applied_at` ONLY. The pre-images are captured separately
    /// (and durably) by `store_supersession_pre_images` BEFORE the file write — so `applied_at` is the
    /// LAST write, flipped only once the note(s) are safely stamped.
    pub fn mark_supersession_applied(&self, id: &str, applied_at: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE supersessions SET applied_at = ?2 WHERE id = ?1",
            rusqlite::params![id, applied_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Clear the APPLIED state (undo): `applied_at` back to NULL and both pre-images dropped, once the
    /// caller has restored the note bytes on disk. The pre-images are transient undo scratch — they do
    /// not linger once the stamp is reverted.
    pub fn clear_supersession_applied(&self, id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE supersessions
                SET applied_at = NULL, source_pre_image = NULL, superseding_pre_image = NULL
              WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    // ── CROSS-MEETING USER MEMORY (Phase 3) ────────────────────────────────────
    //
    // User facts reuse the bitemporal `crate::facts::{Fact, FactOp}` shape and the PURE deterministic
    // `reconcile_facts` core, but persist to the SEPARATE `user_facts` table (no entity FK). In the
    // in-memory `Fact`/`NewFact` the `entity_id` field carries the USER-SCOPE SENTINEL
    // (`crate::user_memory::USER_SCOPE`) so `reconcile_facts` keys on `(sentinel, subject, predicate)`
    // — it is the reconcile key only, NOT a stored column. LOCK MODEL: `user_facts_all` is the
    // INTERNAL un-gated reconcile input (pipeline-only, before any seal can hide rows, like
    // `facts_for_entities`); every USER-FACING read goes through `list_user_facts_visible`, and a
    // sealed meeting's user facts are PURGED on seal (`purge_user_facts_tx`).

    /// ALL user facts (open + closed) — the reconcile input. INTERNAL: the un-gated lifecycle read
    /// (the pipeline reconciles before any seal can hide rows), NOT a user-facing surface. Rows are
    /// hydrated into `crate::facts::Fact` with `entity_id` set to the user-scope sentinel so the pure
    /// `reconcile_facts` keys them correctly. Newest-recorded last (stable reconcile order).
    pub fn user_facts_all(&self) -> Result<Vec<crate::facts::Fact>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, subject, predicate, object, valid_from, valid_to, recorded_at, \
                        meeting_id, confidence \
                   FROM user_facts ORDER BY recorded_at ASC, id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt.query_map([], row_to_user_fact).map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Apply a batch of reconcile [`crate::facts::FactOp`]s to `user_facts` in ONE atomic transaction:
    /// INSERT each `Add`, set `valid_to` on each `Invalidate` (only if still open — idempotent), skip
    /// `NoOp`. A fresh UUID is minted per Add. The `entity_id` on an Add op is the user-scope sentinel
    /// and is NOT persisted (there is no entity column). The whole batch commits or rolls back
    /// together, so a crash mid-apply never leaves a half-reconciled store.
    pub fn apply_user_fact_ops(&self, ops: &[crate::facts::FactOp]) -> Result<()> {
        self.apply_user_fact_ops_inner(ops, None)
    }

    /// MEM-1: apply the reconcile ops AND, in the SAME atomic tx, record every `Invalidate`d
    /// pre-existing fact under `import_meeting_id` (into `user_fact_import_supersedes`) so deleting the
    /// synthetic Memory-Import meeting can REOPEN them. Used ONLY by `import_memories` — a normal
    /// meeting's fact extraction anchors its Adds to the meeting itself and needs no reversible link
    /// (deleting that meeting purges its Adds; it does not supersede OTHER meetings' facts). Recording
    /// the closure link atomically with the closure means a crash can never leave a closed-but-unlinked
    /// (permanently-lost-on-undo) fact.
    pub fn apply_user_fact_ops_recording_import_supersedes(
        &self,
        ops: &[crate::facts::FactOp],
        import_meeting_id: &str,
    ) -> Result<()> {
        self.apply_user_fact_ops_inner(ops, Some(import_meeting_id))
    }

    fn apply_user_fact_ops_inner(
        &self,
        ops: &[crate::facts::FactOp],
        import_meeting_id: Option<&str>,
    ) -> Result<()> {
        use crate::facts::FactOp;
        if ops.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        for op in ops {
            match op {
                FactOp::Add(nf) => {
                    let id = uuid::Uuid::new_v4().to_string();
                    tx.execute(
                        "INSERT INTO user_facts \
                           (id, subject, predicate, object, valid_from, valid_to, \
                            recorded_at, meeting_id, confidence) \
                         VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8)",
                        rusqlite::params![
                            id,
                            nf.subject,
                            nf.predicate,
                            nf.object,
                            nf.valid_from,
                            nf.recorded_at,
                            nf.meeting_id,
                            nf.confidence,
                        ],
                    )
                    .map_err(map_err)?;
                }
                FactOp::Invalidate { id, valid_to } => {
                    let closed = tx
                        .execute(
                            "UPDATE user_facts SET valid_to = ?2 WHERE id = ?1 AND valid_to IS NULL",
                            rusqlite::params![id, valid_to],
                        )
                        .map_err(map_err)?;
                    // MEM-1: only record the reversible link when WE actually closed the row (the
                    // `AND valid_to IS NULL` guard) AND this is an import run — so the undo reopens
                    // exactly the facts THIS import superseded, nothing else.
                    if closed > 0 {
                        if let Some(imid) = import_meeting_id {
                            tx.execute(
                                "INSERT OR IGNORE INTO user_fact_import_supersedes
                                   (import_meeting_id, superseded_fact_id, superseded_valid_to)
                                 VALUES (?1, ?2, ?3)",
                                rusqlite::params![imid, id, valid_to],
                            )
                            .map_err(map_err)?;
                        }
                    }
                }
                FactOp::NoOp => {}
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// MEM-1 reversible-supersede UNDO (in an EXISTING tx, called from `delete_meeting`): when the
    /// deleted meeting is a synthetic Memory-Import, REOPEN every pre-existing fact that import closed
    /// (set `valid_to` back to NULL) — but ONLY where the row is still closed at the EXACT `valid_to`
    /// we stamped (so a later legitimate re-supersession by a DIFFERENT meeting is never clobbered) —
    /// then drop the link rows. Then the caller's `purge_user_facts_tx` deletes the import's OWN Adds.
    /// A non-import meeting has no link rows ⇒ a no-op. This makes "delete the import ⇒ full undo"
    /// restore the pre-existing memories instead of leaving them permanently closed.
    pub(crate) fn reopen_import_superseded_facts_tx(
        tx: &rusqlite::Transaction<'_>,
        import_meeting_id: &str,
    ) -> Result<()> {
        // Reopen each still-matching superseded fact (guarded on the recorded valid_to).
        tx.execute(
            "UPDATE user_facts
                SET valid_to = NULL
              WHERE id IN (SELECT superseded_fact_id FROM user_fact_import_supersedes
                            WHERE import_meeting_id = ?1)
                AND valid_to = (SELECT superseded_valid_to FROM user_fact_import_supersedes s
                                 WHERE s.import_meeting_id = ?1 AND s.superseded_fact_id = user_facts.id)",
            rusqlite::params![import_meeting_id],
        )
        .map_err(map_err)?;
        // Drop the link rows (the import is being deleted).
        tx.execute(
            "DELETE FROM user_fact_import_supersedes WHERE import_meeting_id = ?1",
            rusqlite::params![import_meeting_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// GATED read: the CURRENTLY-VALID (open) user facts whose SOURCE meeting is VISIBLE, newest
    /// valid_from first. Visibility uses the SAME predicate as every other graph/MCP read
    /// (`visibility_clause`): a user fact is visible iff its source meeting has a visible note (or no
    /// note yet). A row with a NULL `meeting_id` is NOT visible — the INNER JOIN to `meetings` drops
    /// it (fail-closed). This is the single user-facing user-fact read: it feeds BOTH the audit view
    /// (`get_user_memory`) AND the injected memory brief, so a sealed-and-not-session-unlocked
    /// meeting's user facts surface NOTHING and are injected into NO prompt. Only OPEN facts
    /// (`valid_to IS NULL`) are returned — a forgotten/superseded fact is closed and excluded.
    pub fn list_user_facts_visible(
        &self,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<crate::facts::Fact>> {
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let sql = format!(
            "SELECT uf.id, uf.subject, uf.predicate, uf.object, uf.valid_from, \
                    uf.valid_to, uf.recorded_at, uf.meeting_id, uf.confidence \
               FROM user_facts uf \
               JOIN meetings m ON m.id = uf.meeting_id \
              WHERE uf.valid_to IS NULL \
                AND {meeting_visible} \
              ORDER BY uf.valid_from DESC, uf.id DESC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt.query_map([], row_to_user_fact).map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Brain v2 L2.2 — GATED, RELEVANCE-FILTERED read: the top-`k` CURRENTLY-VALID (open) user
    /// facts matching `query` (BM25 over `fts_user_facts`, best first), restricted by EXACTLY the
    /// same visibility predicate as [`Self::list_user_facts_visible`] (source-meeting
    /// `visibility_clause`; NULL `meeting_id` fail-closed via the INNER JOIN). The query is defused
    /// through [`fts_match_query_any`] (an OR of quoted literal CONTENT terms — stopwords and
    /// <3-char tokens are dropped, so a natural-language question must match a fact on a real
    /// content word, never on "the"/"is"), so raw user text can never raise an FTS syntax error; an
    /// empty / punctuation-only / all-stopword query returns NO hits (the caller falls back to the
    /// full list). Only OPEN facts (`valid_to IS NULL`) are returned.
    pub fn search_user_facts_visible(
        &self,
        query: &str,
        k: usize,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<crate::facts::Fact>> {
        let Some(match_expr) = fts_match_query_any(query) else {
            return Ok(Vec::new());
        };
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let sql = format!(
            "SELECT uf.id, uf.subject, uf.predicate, uf.object, uf.valid_from, \
                    uf.valid_to, uf.recorded_at, uf.meeting_id, uf.confidence \
               FROM fts_user_facts \
               JOIN user_facts uf ON uf.rowid = fts_user_facts.rowid \
               JOIN meetings m ON m.id = uf.meeting_id \
              WHERE fts_user_facts MATCH ?1 \
                AND uf.valid_to IS NULL \
                AND {meeting_visible} \
              ORDER BY bm25(fts_user_facts) ASC, uf.valid_from DESC, uf.id DESC \
              LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![match_expr, k as i64], row_to_user_fact)
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// The persisted `facts.importance` assessments as `(fact_id, importance)` — the reflection
    /// job's "already assessed" set for ENTITY facts (only never-assessed facts hit the reasoner,
    /// so steady-state passes are LLM-free). Content-free read (ids + floats).
    // ── The sealed fact ledger ───────────────────────────────────────────────────────────────────
    //
    // A seal DELETES a meeting's facts, user facts and supersessions: their subject/predicate/object
    // are plaintext derived from the meeting. That part is right and stays. What was missing is the
    // other half of the contract every other piece of user content already has — the ciphertext that
    // lets an unlock put it back. Re-extraction is not a substitute: it needs a provider call, and it
    // cannot reconstruct `valid_from`/`valid_to` or the supersession chain, because nothing in the
    // current note text records when a fact STOPPED being true.

    /// Every ledger row anchored on `meeting_id`, in a form that round-trips through a seal.
    /// Reads the raw tables directly — the caller is the seal, which runs while the folder is still
    /// readable and is the one place that must see rows a gated reader would hide.
    pub fn raw_fact_ledger_for_meeting(&self, meeting_id: &str) -> Result<crate::storage::SealedFactLedger> {
        let conn = self.lock();
        let mut facts = Vec::new();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT id, entity_id, subject, predicate, object, valid_from, valid_to,
                            recorded_at, meeting_id, confidence
                       FROM facts WHERE meeting_id = ?1 ORDER BY id",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![meeting_id], |r| {
                    Ok(crate::facts::Fact {
                        id: r.get(0)?,
                        entity_id: r.get(1)?,
                        subject: r.get(2)?,
                        predicate: r.get(3)?,
                        object: r.get(4)?,
                        valid_from: r.get(5)?,
                        valid_to: r.get(6)?,
                        recorded_at: r.get(7)?,
                        meeting_id: r.get(8)?,
                        confidence: r.get(9)?,
                    })
                })
                .map_err(map_err)?;
            for row in rows {
                facts.push(row.map_err(map_err)?);
            }
        }
        let mut user_facts = Vec::new();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT id, subject, predicate, object, valid_from, valid_to, recorded_at,
                            meeting_id, confidence
                       FROM user_facts WHERE meeting_id = ?1 ORDER BY id",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![meeting_id], |r| {
                    Ok(crate::facts::Fact {
                        id: r.get(0)?,
                        // A user fact has no entity; the empty id keeps ONE serialized shape for
                        // both tables and is never written back into `facts`.
                        entity_id: String::new(),
                        subject: r.get(1)?,
                        predicate: r.get(2)?,
                        object: r.get(3)?,
                        valid_from: r.get(4)?,
                        valid_to: r.get(5)?,
                        recorded_at: r.get(6)?,
                        meeting_id: r.get(7)?,
                        confidence: r.get(8)?,
                    })
                })
                .map_err(map_err)?;
            for row in rows {
                user_facts.push(row.map_err(map_err)?);
            }
        }
        let mut supersessions = Vec::new();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT id, superseding_meeting_id, source_meeting_id, entity, predicate,
                            old_value, new_value, created_at, applied_at, source_pre_image,
                            superseding_pre_image
                       FROM supersessions
                      WHERE superseding_meeting_id = ?1 OR source_meeting_id = ?1
                      ORDER BY id",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![meeting_id], |r| {
                    Ok(crate::storage::SealedSupersession {
                        id: r.get(0)?,
                        superseding_meeting_id: r.get(1)?,
                        source_meeting_id: r.get(2)?,
                        entity: r.get(3)?,
                        predicate: r.get(4)?,
                        old_value: r.get(5)?,
                        new_value: r.get(6)?,
                        created_at: r.get(7)?,
                        applied_at: r.get(8)?,
                        source_pre_image: r.get(9)?,
                        superseding_pre_image: r.get(10)?,
                    })
                })
                .map_err(map_err)?;
            for row in rows {
                supersessions.push(row.map_err(map_err)?);
            }
        }
        let mut fact_importance = Vec::new();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT id, importance FROM facts
                      WHERE meeting_id = ?1 AND importance IS NOT NULL ORDER BY id",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![meeting_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
                })
                .map_err(map_err)?;
            for row in rows {
                fact_importance.push(row.map_err(map_err)?);
            }
        }
        Ok(crate::storage::SealedFactLedger {
            facts,
            fact_importance,
            user_facts,
            supersessions,
        })
    }

    /// Store the ciphertext of a meeting's ledger. The caller has already proven it decrypts back
    /// byte-identical; this only records it, and the rows are deleted by the seal's own purge.
    pub fn seal_fact_ledger(&self, meeting_id: &str, data_blob: &[u8]) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO sealed_fact_ledgers (meeting_id, data_blob, sealed_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(meeting_id) DO UPDATE SET
               data_blob = excluded.data_blob, sealed_at = excluded.sealed_at",
            rusqlite::params![
                meeting_id,
                data_blob,
                chrono::Utc::now().to_rfc3339()
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The stored ciphertext for a meeting's ledger, if it has ever been sealed.
    pub fn fact_ledger_blob(&self, meeting_id: &str) -> Result<Option<Vec<u8>>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT data_blob FROM sealed_fact_ledgers WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
            |r| r.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(map_err)
    }

    /// Put a decrypted ledger back. One transaction, and `INSERT OR IGNORE` throughout: a session
    /// unlock may run against rows that a concurrent re-extraction already re-created, and a restore
    /// must never overwrite a fresher row with the snapshot taken before the seal.
    pub fn restore_fact_ledger(
        &self,
        meeting_id: &str,
        ledger: &crate::storage::SealedFactLedger,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        for f in &ledger.facts {
            // The entity FK is `ON DELETE CASCADE`, and SQLite's ON CONFLICT clause does not apply
            // to foreign keys — a fact whose entity is gone would fail the WHOLE transaction and
            // make the unlock impossible, every time. Skipping it keeps the unlock working; the
            // fact is unreachable anyway without its entity.
            tx.execute(
                "INSERT OR IGNORE INTO facts
                   (id, entity_id, subject, predicate, object, valid_from, valid_to, recorded_at,
                    meeting_id, confidence)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10
                  WHERE EXISTS (SELECT 1 FROM entities WHERE id = ?2)",
                rusqlite::params![
                    f.id,
                    f.entity_id,
                    f.subject,
                    f.predicate,
                    f.object,
                    f.valid_from,
                    f.valid_to,
                    f.recorded_at,
                    f.meeting_id,
                    f.confidence
                ],
            )
            .map_err(map_err)?;
        }
        for f in &ledger.user_facts {
            tx.execute(
                "INSERT OR IGNORE INTO user_facts
                   (id, subject, predicate, object, valid_from, valid_to, recorded_at, meeting_id,
                    confidence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    f.id,
                    f.subject,
                    f.predicate,
                    f.object,
                    f.valid_from,
                    f.valid_to,
                    f.recorded_at,
                    f.meeting_id,
                    f.confidence
                ],
            )
            .map_err(map_err)?;
        }
        for (fact_id, importance) in &ledger.fact_importance {
            tx.execute(
                "UPDATE facts SET importance = ?2 WHERE id = ?1",
                rusqlite::params![fact_id, importance],
            )
            .map_err(map_err)?;
        }
        for s in &ledger.supersessions {
            // A supersession spans TWO meetings, and its old/new values plus its note pre-images are
            // plaintext from both. Restoring it because THIS meeting was unlocked would put the
            // other one's content back into a live table while its folder is still sealed — the
            // exact thing purge-on-seal exists to prevent. Fail closed: the row waits until the
            // other anchor is readable too, and the ciphertext still holds it.
            let other = if s.superseding_meeting_id == meeting_id {
                &s.source_meeting_id
            } else {
                &s.superseding_meeting_id
            };
            if other != meeting_id {
                // Gone, not merely sealed: `supersessions` carries no foreign key, so a row whose
                // other anchor was deleted while this folder sat locked would come back pointing at
                // a meeting that no longer exists.
                let other_exists: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM meetings WHERE id = ?1)",
                        rusqlite::params![other],
                        |r| Ok(r.get::<_, i64>(0)? != 0),
                    )
                    .map_err(map_err)?;
                if !other_exists
                    || crate::storage::links::meeting_sealed_at_rest_tx(&tx, other)?
                {
                    continue;
                }
            }
            tx.execute(
                "INSERT OR IGNORE INTO supersessions
                   (id, superseding_meeting_id, source_meeting_id, entity, predicate, old_value,
                    new_value, created_at, applied_at, source_pre_image, superseding_pre_image)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    s.id,
                    s.superseding_meeting_id,
                    s.source_meeting_id,
                    s.entity,
                    s.predicate,
                    s.old_value,
                    s.new_value,
                    s.created_at,
                    s.applied_at,
                    s.source_pre_image,
                    s.superseding_pre_image
                ],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Drop the stored ciphertext (permanent remove-lock: the plaintext rows are back for good).
    pub fn clear_fact_ledger_blob(&self, meeting_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM sealed_fact_ledgers WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub fn fact_importance_map(&self) -> Result<std::collections::HashMap<String, f64>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id, importance FROM facts WHERE importance IS NOT NULL")
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))
            .map_err(map_err)?;
        let mut out = std::collections::HashMap::new();
        for r in rows {
            let (id, imp) = r.map_err(map_err)?;
            out.insert(id, imp);
        }
        Ok(out)
    }

    /// Persist the batch-assessed importance (1–10) of ONE entity fact (`facts.importance`).
    pub fn set_fact_importance(&self, fact_id: &str, importance: f64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE facts SET importance = ?2 WHERE id = ?1",
            rusqlite::params![fact_id, importance],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// FORGET one user fact by id (bitemporal INVALIDATE, never a silent delete): close the row at
    /// `at` if it is still open. Idempotent (a already-closed row is untouched). History is preserved
    /// — the fact simply stops being current, so it drops out of `list_user_facts_visible` and the
    /// regenerated brief. Returns `true` iff a row was closed by this call.
    pub fn forget_user_fact(&self, id: &str, at: &str) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let n = tx
            .execute(
                "UPDATE user_facts SET valid_to = ?2 WHERE id = ?1 AND valid_to IS NULL",
                rusqlite::params![id, at],
            )
            .map_err(map_err)?;
        if n > 0 {
            Self::purge_all_ask_conversations_tx(&tx)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(n > 0)
    }

    /// FORGET one ENTITY fact by id — the `facts` twin of [`Db::forget_user_fact`] above.
    ///
    /// Until this existed, entity facts were effectively UNCORRECTABLE: the store exposed
    /// forget/clear for *user* facts only, so the sole way to close a wrong entity fact was for a
    /// LATER meeting to happen to assert a different object for the same
    /// `(entity_id, subject, predicate)` key. A junk row nobody would ever restate — the real
    /// `owner: claude_code` that reached a dossier as a current fact — therefore stayed current
    /// forever, and kept being reported as truth by every agent reading that dossier.
    ///
    /// Same bitemporal contract as the user-fact twin: INVALIDATE by closing the row at `at`, never
    /// delete, so the history stays on the record and only the CURRENT view changes. Idempotent —
    /// an already-closed row is untouched. Returns `true` iff this call closed a row.
    pub fn forget_entity_fact(&self, id: &str, at: &str) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let n = tx
            .execute(
                "UPDATE facts SET valid_to = ?2 WHERE id = ?1 AND valid_to IS NULL",
                rusqlite::params![id, at],
            )
            .map_err(map_err)?;
        if n > 0 {
            Self::purge_all_ask_conversations_tx(&tx)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(n > 0)
    }

    /// CLEAR all user memory: bitemporal-close EVERY currently-open user fact at `at` (invalidate,
    /// never delete — closed history stays for the record). After this the brief regenerates empty and
    /// the audit view is empty. Returns the number of facts closed.
    pub fn clear_user_facts(&self, at: &str) -> Result<usize> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let n = tx
            .execute(
                "UPDATE user_facts SET valid_to = ?1 WHERE valid_to IS NULL",
                rusqlite::params![at],
            )
            .map_err(map_err)?;
        if n > 0 {
            Self::purge_all_ask_conversations_tx(&tx)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(n)
    }

    // The `audit_findings` / `audit_runs` CRUD (`insert_audit_finding_if_new` /
    // `list_audit_finding_rows` / `get_audit_finding` / `resolve_audit_finding_row` /
    // `delete_pending_audit_findings_for_run` / `count_pending_audit_findings` / `insert_audit_run` /
    // `insert_scheduled_audit_run_claim` / `last_scheduled_audit_run_finished_at` /
    // `delete_pending_audit_finding` / `purge_pending_audit_findings_tx` /
    // `purge_all_pending_audit_findings_tx` / `purge_all_pending_audit_findings`) + `row_to_audit_finding`
    // moved to `storage::audit_store` (God-file split) — still callable as inherent `db.method()`
    // (and `Self::purge_*_pending_audit_findings_tx(&tx, ..)` from the seal paths) cross-file.

    /// GATED: every OPEN fact whose source meeting is VISIBLE — the SAME meeting-visibility
    /// predicate as [`Db::list_facts_visible`] (a NULL `meeting_id` is fail-closed via the INNER
    /// JOIN). The audit's stale/contradiction substrate.
    pub fn list_open_facts_visible(
        &self,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<crate::facts::Fact>> {
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let sql = format!(
            "SELECT ft.id, ft.entity_id, ft.subject, ft.predicate, ft.object, ft.valid_from, \
                    ft.valid_to, ft.recorded_at, ft.meeting_id, ft.confidence \
               FROM facts ft \
               JOIN meetings m ON m.id = ft.meeting_id \
              WHERE ft.valid_to IS NULL \
                AND {meeting_visible} \
              ORDER BY ft.valid_from ASC, ft.id ASC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt.query_map([], row_to_fact).map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// GATED: ONE meeting's FULL fact rows (open + closed), visible-predicate-gated exactly like
    /// [`Db::list_facts_visible`] — a sealed-and-not-session-unlocked meeting returns EMPTY
    /// (its facts are also purged on seal; this gate is defense-in-depth). Distinct from the
    /// rendered-strings reader [`Db::facts_for_meeting_visible`]: the audit's staleness math
    /// needs the `valid_to` axis, not a display string.
    pub fn fact_rows_for_meeting_visible(
        &self,
        meeting_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<crate::facts::Fact>> {
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let sql = format!(
            "SELECT ft.id, ft.entity_id, ft.subject, ft.predicate, ft.object, ft.valid_from, \
                    ft.valid_to, ft.recorded_at, ft.meeting_id, ft.confidence \
               FROM facts ft \
               JOIN meetings m ON m.id = ft.meeting_id \
              WHERE ft.meeting_id = ?1 \
                AND {meeting_visible} \
              ORDER BY ft.valid_from ASC, ft.id ASC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![meeting_id], row_to_fact)
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Is the SAME conflict pair already staged for Re-Truth review? True when a PENDING
    /// (`applied_at IS NULL`) supersession row carries the same normalized predicate and the same
    /// two meetings (either orientation) — the audit's contradiction pass then skips the pair
    /// (one review surface per conflict).
    pub fn pending_supersession_for_pair(
        &self,
        norm_predicate: &str,
        meeting_a: &str,
        meeting_b: &str,
    ) -> Result<bool> {
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM supersessions
               WHERE applied_at IS NULL
                 AND lower(trim(predicate)) = ?1
                 AND ((source_meeting_id = ?2 AND superseding_meeting_id = ?3)
                   OR (source_meeting_id = ?3 AND superseding_meeting_id = ?2)))",
            rusqlite::params![norm_predicate, meeting_a, meeting_b],
            |r| r.get(0),
        )
        .map_err(map_err)
    }
}

/// Map a `facts` row (column order matches every facts SELECT) to a [`crate::facts::Fact`].
fn row_to_fact(row: &Row<'_>) -> rusqlite::Result<crate::facts::Fact> {
    Ok(crate::facts::Fact {
        id: row.get(0)?,
        entity_id: row.get(1)?,
        subject: row.get(2)?,
        predicate: row.get(3)?,
        object: row.get(4)?,
        valid_from: row.get(5)?,
        valid_to: row.get(6)?,
        recorded_at: row.get(7)?,
        meeting_id: row.get(8)?,
        confidence: row.get(9)?,
    })
}

/// Map a `user_facts` row (column order: id, subject, predicate, object, valid_from, valid_to,
/// recorded_at, meeting_id, confidence — NO entity column) to a [`crate::facts::Fact`], stamping the
/// user-scope sentinel into `entity_id` so the pure `reconcile_facts` keys the row correctly. The
/// sentinel is a reconcile key only, never persisted.
fn row_to_user_fact(row: &Row<'_>) -> rusqlite::Result<crate::facts::Fact> {
    Ok(crate::facts::Fact {
        id: row.get(0)?,
        entity_id: crate::user_memory::USER_SCOPE.to_string(),
        subject: row.get(1)?,
        predicate: row.get(2)?,
        object: row.get(3)?,
        valid_from: row.get(4)?,
        valid_to: row.get(5)?,
        recorded_at: row.get(6)?,
        meeting_id: row.get(7)?,
        confidence: row.get(8)?,
    })
}

/// Map a `supersessions` row (id, superseding_meeting_id, source_meeting_id, entity, predicate,
/// old_value, new_value, created_at, applied_at, source_pre_image, superseding_pre_image) to a row
/// struct.
fn row_to_supersession(row: &Row<'_>) -> rusqlite::Result<crate::storage::models::SupersessionRow> {
    Ok(crate::storage::models::SupersessionRow {
        id: row.get(0)?,
        superseding_meeting_id: row.get(1)?,
        source_meeting_id: row.get(2)?,
        entity: row.get(3)?,
        predicate: row.get(4)?,
        old_value: row.get(5)?,
        new_value: row.get(6)?,
        created_at: row.get(7)?,
        applied_at: row.get(8)?,
        source_pre_image: row.get(9)?,
        superseding_pre_image: row.get(10)?,
    })
}
