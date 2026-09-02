//! At-rest SEAL machinery (the per-folder lock's DB layer) — the note / transcript-segment /
//! timeline / document seal + blank + restore + clear-blob writers, the folder-level relock
//! (`blank_sealed_notes_in_folders`), the startup at-rest reconciliation
//! (`reblank_locked_folders_at_rest`), and the unrecoverable-KEK discard (`discard_folder_seal`).
//! Extracted VERBATIM from `storage::db` (God-file split, a PURE MOVE — zero behavior change): an
//! inherent-impl split of [`crate::storage::db::Db`] across files (Rust allows one type's inherent
//! `impl` to live in multiple files of the same crate); every method keeps its EXACT prior body,
//! signature, AND write ordering.
//!
//! LOCK-CRITICAL — the verify-before-destroy contract is UNCHANGED by this move. The seal writers
//! here (`seal_note` / `seal_segment` / `seal_timeline` / `seal_document` / `set_timeline_data_sealed`)
//! only WRITE the AES-GCM blob (+ blank the already-empty plaintext column); the CALLER
//! (`lock_folder` / `seal_folder_extras` / `remove_lock` in `commands`) still verifies each blob
//! decrypts back byte-identical BEFORE the plaintext is blanked (this move relocated the low-level
//! writers, not the seal-verify callers). `blank_sealed_notes_in_folders` / `discard_folder_seal` /
//! `reblank_locked_folders_at_rest` re-blank plaintext ONLY where the recoverable `*_blob` is present
//! (`text_blob IS NOT NULL` guards preserved verbatim) — never destroying a never-sealed only copy.
//! The shared derived-table purge choke-points (`purge_chunks_tx` / `purge_facts_tx` /
//! `purge_doc_chunks_tx` / … , promoted to `pub(crate)` in `db.rs` since they are shared across
//! delete/seal/relock domains) and the link/audit/marker helpers (`purge_links_tx`,
//! `LINK_DECISION_KEEP`, `strip_sealed_neighbour_markers_tx`, `purge_all_pending_audit_findings_tx`,
//! already `pub(crate)` in `links.rs`/`audit_store.rs`) are reached via `Self::`. The seal DTOs
//! (`SealableNote` / `RawSegment` / `RawTimeline`) + the reconcile return type
//! (`LockedAtRestCleanup` / `LockedMeetingAudio`) stay defined in `db.rs`. Tests stay in db.rs's
//! `mod tests` (shared harness); the count is conserved.

use std::collections::HashSet;

use rusqlite::OptionalExtension;

use crate::error::Result;
use crate::storage::db::{
    map_err, Db, LockedAtRestCleanup, LockedMeetingAudio, RawSegment, RawTimeline, SealableNote,
};

impl Db {
    /// Final commit for permanent folder unlock. Every plaintext note/transcript/timeline/manual
    /// note/document and durable audio file has already been restored while the original sealed
    /// blobs remain intact. Clearing those recovery blobs and flipping `locked=0` must be ONE
    /// transaction: a crash before commit stays a normal locked/session-materialized state that
    /// startup can reblank; a crash after commit is a fully open folder. There is no intermediate
    /// locked-with-plaintext-and-no-blob state.
    pub fn commit_folder_permanent_unlock(&self, folder_id: &str) -> Result<()> {
        const FOLDER_MEETINGS: &str = "SELECT id FROM meetings WHERE folder_id=?1
             UNION SELECT n.meeting_id FROM notes n JOIN meetings m ON m.id=n.meeting_id
                    WHERE n.folder_id=?1 AND m.folder_id IS NULL";
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        tx.execute(
            "UPDATE notes SET content_blob = NULL WHERE folder_id = ?1",
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        tx.execute(
            &format!(
                "UPDATE segments SET text_blob = NULL WHERE meeting_id IN ({FOLDER_MEETINGS})"
            ),
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        tx.execute(
            &format!(
                "UPDATE timelines SET data_blob = NULL WHERE meeting_id IN ({FOLDER_MEETINGS})"
            ),
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        tx.execute(
            &format!(
                "UPDATE meetings SET manual_notes_blob = NULL WHERE id IN ({FOLDER_MEETINGS})"
            ),
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE documents SET text_blob = NULL WHERE folder_id = ?1",
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        tx.execute(
            &format!(
                "UPDATE note_attachments SET data_blob = NULL WHERE
                   document_id IN (SELECT id FROM documents WHERE folder_id = ?1)
                   OR meeting_id IN ({FOLDER_MEETINGS})"
            ),
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE folders SET locked = 0, wrapped_key = NULL WHERE id = ?1",
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)
    }

    /// Seal ONE document's text: store the AES-GCM `text_blob`, blank the plaintext `text`. The CALLER
    /// must verify the blob decrypts back byte-identical BEFORE calling this (verify-before-destroy) —
    /// exactly like [`Db::seal_manual_notes`] / [`Db::seal_note`].
    pub fn seal_document(&self, id: &str, blob: &[u8]) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE documents SET text_blob = ?2, text = '' WHERE id = ?1",
            rusqlite::params![id, blob],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Restore (or re-blank) a document's plaintext `text` for the session, leaving `text_blob`
    /// intact. Pass the decrypted plaintext on unlock; pass "" on reblank (relock). Mirrors
    /// [`Db::set_manual_notes`].
    pub fn set_document_text(&self, id: &str, text: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE documents SET text = ?2 WHERE id = ?1",
            rusqlite::params![id, text],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Clear a document's sealed `text_blob` (permanent remove-lock, after the plaintext is restored).
    /// Mirrors [`Db::clear_manual_notes_blob`].
    pub fn clear_document_blob(&self, id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE documents SET text_blob = NULL WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// FAIL-CLOSED cleanup for a salvage/retry pipeline that raced a mid-run relock: delete this
    /// meeting's segments that carry PLAINTEXT with NO sealed blob (`text_blob IS NULL`) — the
    /// unsealed fresh rows the relock's re-blank (guarded on `text_blob IS NOT NULL`) can never
    /// cover. Rows WITH a blob (the durable sealed copies) are untouched. The deleted rows are
    /// DERIVED data whose source audio survives sealed at rest (`.enc`), so nothing unrecoverable
    /// is destroyed — privacy wins over keeping a re-derivable plaintext transcript behind a lock.
    /// Returns the number of rows removed (ids/counts only in logs, never text).
    pub fn delete_unsealed_segments(&self, meeting_id: &str) -> Result<usize> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM segments WHERE meeting_id = ?1 AND text_blob IS NULL",
            rusqlite::params![meeting_id],
        )
        .map_err(map_err)
    }

    /// SEAL-ON-WRITE twin of [`Db::set_timeline_data`] for a meeting in a session-unlocked LOCKED
    /// folder (2026-07-11 audit SEAM-F1/F2): upsert the fresh plaintext (session-visible) AND its
    /// freshly-encrypted `data_blob` in ONE atomic statement, so relock/at-rest reblank restores THIS
    /// timeline — never a stale lock-time copy, and a timeline GENERATED while unlocked is sealed from
    /// birth (never a blob-less plaintext behind a lock). The CALLER must have verified the blob
    /// decrypts back byte-identical BEFORE calling this (verify-before-destroy) — exactly like
    /// [`Db::seal_timeline`]. INSERT-or-update (a freshly generated timeline has no row yet).
    pub fn set_timeline_data_sealed(
        &self,
        meeting_id: &str,
        data: &str,
        data_blob: &[u8],
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO timelines (meeting_id, data, data_blob) VALUES (?1, ?2, ?3)
             ON CONFLICT(meeting_id) DO UPDATE SET
               data = excluded.data,
               data_blob = excluded.data_blob",
            rusqlite::params![meeting_id, data, data_blob],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// DESTRUCTIVE escape hatch for a folder whose master KEK is GENUINELY UNRECOVERABLE (the caller
    /// — `discard_unrecoverable_folder_lock` — has already PROVEN, via the full read-first + candidate
    /// recovery ladder, that no key unwraps this folder's content key). Discards ONLY this folder's
    /// sealed payload and returns the folder to an open, usable state. The encrypted `*_blob`
    /// ciphertext is unrecoverable anyway (its key is gone), so this destroys nothing that was
    /// otherwise readable — it just stops the folder being permanently bricked.
    ///
    /// In one transaction, scoped to THIS folder's meetings + documents: NULL every sealed blob and
    /// blank the (already-empty) plaintext columns, purge every derived table that could hold
    /// sealed-content-derived data (chunks/vectors/facts/user-facts/correction-log/assistant-log/
    /// voiceprints/live-bullets), then clear the folder's `wrapped_key` and set `locked = 0`. Returns the at-rest
    /// audio columns of the folder's meetings so the caller can delete the orphaned `.enc` files on
    /// disk (the DB layer stays pure-SQL). NEVER call this without the caller's non-recoverability
    /// proof — it is the one path that discards sealed content by design (and now provably only the
    /// sealed, unrecoverable part — never a readable never-sealed buffer).
    pub fn discard_folder_seal(&self, folder_id: &str) -> Result<Vec<String>> {
        // Canonically filed meetings plus conservative legacy NULL-canonical note ownership.
        // Documents anchor on the folder row directly.
        const FOLDER_MEETINGS: &str = "SELECT id FROM meetings WHERE folder_id=?1
             UNION SELECT n.meeting_id FROM notes n JOIN meetings m ON m.id=n.meeting_id
                    WHERE n.folder_id=?1 AND m.folder_id IS NULL";
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;

        // Collect ONLY the SEALED (`.enc`) audio paths to unlink — a never-sealed plaintext WAV is
        // readable content and is left untouched (file kept, column kept).
        let mut enc_paths: Vec<String> = Vec::new();
        {
            let mut stmt = tx
                .prepare(&format!(
                    "SELECT audio_path, mic_master_path, sys_master_path FROM meetings \
                       WHERE id IN ({FOLDER_MEETINGS})"
                ))
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![folder_id], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                })
                .map_err(map_err)?;
            for r in rows {
                let (a, m, s) = r.map_err(map_err)?;
                for p in [a, m, s].into_iter().flatten() {
                    if p.ends_with(".enc") {
                        enc_paths.push(p);
                    }
                }
            }
        }

        // Notes: blank plaintext + export path (+ its collision-guard hash baseline, which is
        // path-coupled) ONLY of rows that WERE sealed (blob present); a never-sealed row (blob
        // NULL) keeps its readable plaintext. Then drop the unrecoverable blob.
        tx.execute(
            "UPDATE notes SET markdown = '', exported_path = NULL, exported_hash = NULL WHERE folder_id = ?1 AND content_blob IS NOT NULL",
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE notes SET content_blob = NULL WHERE folder_id = ?1",
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        // Transcript segments + timeline.
        tx.execute(
            &format!("UPDATE segments SET text = '' WHERE text_blob IS NOT NULL AND meeting_id IN ({FOLDER_MEETINGS})"),
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        tx.execute(
            &format!(
                "UPDATE segments SET text_blob = NULL WHERE meeting_id IN ({FOLDER_MEETINGS})"
            ),
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        tx.execute(
            &format!("UPDATE timelines SET data = '' WHERE data_blob IS NOT NULL AND meeting_id IN ({FOLDER_MEETINGS})"),
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        tx.execute(
            &format!(
                "UPDATE timelines SET data_blob = NULL WHERE meeting_id IN ({FOLDER_MEETINGS})"
            ),
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        // Typed realtime notes.
        tx.execute(
            &format!("UPDATE meetings SET manual_notes = '' WHERE manual_notes_blob IS NOT NULL AND id IN ({FOLDER_MEETINGS})"),
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        tx.execute(
            &format!(
                "UPDATE meetings SET manual_notes_blob = NULL WHERE id IN ({FOLDER_MEETINGS})"
            ),
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        // At-rest audio: null ONLY the SEALED (`.enc`) columns; a plaintext WAV column is preserved.
        for col in ["audio_path", "mic_master_path", "sys_master_path"] {
            tx.execute(
                &format!("UPDATE meetings SET {col} = NULL WHERE {col} LIKE '%.enc' AND id IN ({FOLDER_MEETINGS})"),
                rusqlite::params![folder_id],
            )
            .map_err(map_err)?;
        }

        // Derived / invertible tables — purge anything keyed on the folder's meetings (vectors first
        // by chunk_id, then the source rows), mirroring the seal-time purge in
        // `reblank_locked_folders_at_rest` but scoped to this one folder. Surviving never-sealed
        // plaintext re-derives its chunks/vectors on next access.
        tx.execute(
            &format!("DELETE FROM vec_chunks WHERE chunk_id IN (SELECT id FROM note_chunks WHERE meeting_id IN ({FOLDER_MEETINGS}))"),
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        // Brain v2 L1.1 — TOPIC chunks are the same purge class (vec0 rows first, then base rows).
        tx.execute(
            &format!("DELETE FROM topic_vec_chunks WHERE chunk_id IN (SELECT id FROM topic_chunks WHERE meeting_id IN ({FOLDER_MEETINGS}))"),
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        for table in [
            "note_chunks",
            "topic_chunks",
            "correction_log",
            "assistant_interactions",
            "facts",
            "user_facts",
            "speaker_voiceprints",
            // Brain v2 L4 (lock-security W3): the live-bullets crash-recovery row is the same
            // derived-plaintext class — purge it with the rest (contract consistency; a reachable
            // row at discard time only ever digests never-sealed plaintext, but the "purge every
            // derived table" contract must not silently exclude one table).
            "live_bullets",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE meeting_id IN ({FOLDER_MEETINGS})"),
                rusqlite::params![folder_id],
            )
            .map_err(map_err)?;
        }
        // (Deliberate, mirrors `memory_rollups`: PENDING `brief_runs` are NOT purged on discard —
        // discard returns every source meeting to OPEN plaintext, so a pending brief over now-open
        // content is not a leak. Every SEAL path purges them: `purge_pending_brief_runs_tx`.)

        // Vault Audit (lock review + adversarial HIGH): pending findings ARE purged on discard,
        // unlike brief runs — ALL of them (the rollup posture): a TOCTOU-orphaned row (staged in
        // the pass↔seal race the epoch withdrawal narrows but cannot fully close) may CITE this
        // folder's titles in its evidence with no matching id, so a scoped purge cannot cover it.
        // Cheap re-derivable rows; the next pass re-stages anything still true.
        Self::purge_all_pending_audit_findings_tx(&tx)?;
        Self::purge_ask_conversations_for_folders_tx(&tx, &HashSet::from([folder_id.to_string()]))?;
        // Smart-reminder audit candidates/cache are disposable source-derived plaintext too.
        // Accepted reminders live in separate tables and deliberately survive this purge.
        Self::purge_all_reminder_derived_tx(&tx)?;

        // Documents anchored on this folder: drop sealed ciphertext + plaintext + their chunks/vectors.
        tx.execute(
            "DELETE FROM doc_vec_chunks WHERE chunk_id IN \
               (SELECT id FROM doc_chunks WHERE document_id IN \
                  (SELECT id FROM documents WHERE folder_id = ?1))",
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "DELETE FROM doc_chunks WHERE document_id IN (SELECT id FROM documents WHERE folder_id = ?1)",
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE documents SET text = '' WHERE text_blob IS NOT NULL AND folder_id = ?1",
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE documents SET text_blob = NULL WHERE folder_id = ?1",
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;

        // Finally flip the folder OPEN and drop its (now-useless) wrapped content key.
        tx.execute(
            "UPDATE folders SET locked = 0, wrapped_key = NULL WHERE id = ?1",
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;

        tx.commit().map_err(map_err)?;
        Ok(enc_paths)
    }

    /// Notes assigned to a folder (the rows needed to seal/unseal): the meeting, provider,
    /// current markdown, exported path, and any existing sealed blob.
    pub fn notes_in_folder(&self, folder_id: &str) -> Result<Vec<SealableNote>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT meeting_id, provider_id, markdown, exported_path, content_blob
                   FROM notes WHERE folder_id = ?1",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |r| {
                Ok(SealableNote {
                    meeting_id: r.get(0)?,
                    provider_id: r.get(1)?,
                    markdown: r.get(2)?,
                    exported_path: r.get(3)?,
                    content_blob: r.get(4)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Every provider row of ONE meeting's note (markdown, exported path, existing blob), regardless
    /// of folder — the rows needed to seal a note moved INTO a locked folder (BLK-2). Mirrors
    /// [`Db::notes_in_folder`] but scoped to a single meeting so a move seals ONLY that note.
    pub fn sealable_notes_for_meeting(&self, meeting_id: &str) -> Result<Vec<SealableNote>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT meeting_id, provider_id, markdown, exported_path, content_blob
                   FROM notes WHERE meeting_id = ?1",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![meeting_id], |r| {
                Ok(SealableNote {
                    meeting_id: r.get(0)?,
                    provider_id: r.get(1)?,
                    markdown: r.get(2)?,
                    exported_path: r.get(3)?,
                    content_blob: r.get(4)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Seal ONE provider row of a note: store its AES-GCM `content_blob`, blank that row's
    /// plaintext `markdown`, and clear its `exported_path` (the `.md` leaves the vault).
    /// Targets `(meeting_id, provider_id)` so distinct per-provider markdown each gets its own
    /// blob — a meeting re-summarized with multiple providers never collapses to one blob (which
    /// would destroy every provider's content but the first). The whole meeting is sealed by
    /// calling this once per provider row.
    pub fn seal_note(
        &self,
        meeting_id: &str,
        provider_id: &str,
        content_blob: &[u8],
    ) -> Result<()> {
        let conn = self.lock();
        // Export-collision guard: the baseline `exported_hash` is blanked in the SAME seal
        // UPDATE that clears `exported_path` — a sealed note has no on-disk export, and a stale
        // baseline surviving the seal would mis-classify the fresh unlock re-export
        // (`write_note_to_vault` / `remove_lock` re-stamp both columns fresh).
        conn.execute(
            "UPDATE notes SET content_blob = ?3, markdown = '', exported_path = NULL,
                    exported_hash = NULL
             WHERE meeting_id = ?1 AND provider_id = ?2",
            rusqlite::params![meeting_id, provider_id, content_blob],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Restore ONE provider row's plaintext `markdown` (session-unlock or permanent remove-lock).
    /// Does NOT touch `content_blob` (the caller decides whether to clear it). Per-provider so a
    /// sibling provider's distinct markdown is never overwritten.
    pub fn restore_note_markdown(
        &self,
        meeting_id: &str,
        provider_id: &str,
        markdown: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE notes SET markdown = ?3 WHERE meeting_id = ?1 AND provider_id = ?2",
            rusqlite::params![meeting_id, provider_id, markdown],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Clear a note's sealed `content_blob` for every provider row of the meeting (permanent
    /// remove-lock, after each row's plaintext is back). Safe to target the whole meeting here:
    /// the plaintext has already been restored per-row, and we want NO blob left anywhere.
    pub fn clear_note_content_blob(&self, meeting_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE notes SET content_blob = NULL WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Re-blank the plaintext `markdown` of every note in `folder_ids` that still has a sealed
    /// `content_blob` (relock / relock-all). Idempotent; leaves the blob intact.
    /// The caller MUST first authenticate every retained blob-backed non-empty plaintext and
    /// encrypt+decrypt-verify every blob-less non-empty plaintext in every target folder under the
    /// cached folder CK, while holding the lock lifecycle guard. Any prepared missing seal must be
    /// persisted before this low-level transaction, which deliberately has no key access of its own.
    ///
    /// Returns the `exported_path`s of the memory rollups purged in the same transaction
    /// (`purge_memory_rollups_tx` — a relock re-asserts the sealed shape, so sealed-derived rollup
    /// synthesis must not linger either); the CALLER deletes those vault `.md` files.
    pub fn blank_sealed_notes_in_folders(
        &self,
        folder_ids: &HashSet<String>,
    ) -> Result<Vec<String>> {
        if folder_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let rollup_exports;
        {
            let mut stmt = tx
                .prepare(
                    "UPDATE notes SET markdown = ''
                      WHERE folder_id = ?1 AND content_blob IS NOT NULL",
                )
                .map_err(map_err)?;
            for id in folder_ids {
                stmt.execute(rusqlite::params![id]).map_err(map_err)?;
            }
            // Phase 2a LOCK-SAFETY: purge plaintext-derived chunks + their (invertible) vectors for
            // every meeting in these folders, in the SAME transaction as the plaintext blanking —
            // so a re-blanked (sealed) folder never leaves a semantic vector at rest. Resolve the
            // folders' meetings from their note rows (mirrors `meeting_ids_in_folder`).
            let mut mids = tx
                .prepare(
                    "SELECT id FROM meetings WHERE folder_id=?1
                     UNION SELECT n.meeting_id FROM notes n JOIN meetings m ON m.id=n.meeting_id
                            WHERE n.folder_id=?1 AND m.folder_id IS NULL",
                )
                .map_err(map_err)?;
            let mut meeting_ids: Vec<String> = Vec::new();
            for id in folder_ids {
                let rows = mids
                    .query_map(rusqlite::params![id], |r| r.get::<_, String>(0))
                    .map_err(map_err)?;
                for r in rows {
                    meeting_ids.push(r.map_err(map_err)?);
                }
            }
            drop(mids);
            Self::purge_chunks_tx(&tx, &meeting_ids)?;
            // Document ingestion LOCK-SAFETY: purge the (invertible) doc chunks + vectors of every
            // document in these (re-blanked / sealed) folders in the SAME transaction — so a relocked
            // folder never leaves a document's semantic vector at rest. (The document TEXT re-blank is
            // SEALED-AND-RESTORED content handled by `reblank_folder_extras`, exactly like
            // `manual_notes` — it must NOT be blanked here, where there is no CK to re-seal.)
            let mut dids = tx
                .prepare("SELECT id FROM documents WHERE folder_id = ?1")
                .map_err(map_err)?;
            let mut note_ids_stmt = tx
                .prepare("SELECT id FROM documents WHERE folder_id = ?1 AND kind = 'note'")
                .map_err(map_err)?;
            let mut document_ids: Vec<String> = Vec::new();
            let mut authored_note_ids: Vec<String> = Vec::new();
            for id in folder_ids {
                let rows = dids
                    .query_map(rusqlite::params![id], |r| r.get::<_, String>(0))
                    .map_err(map_err)?;
                for r in rows {
                    document_ids.push(r.map_err(map_err)?);
                }
                let note_rows = note_ids_stmt
                    .query_map(rusqlite::params![id], |r| r.get::<_, String>(0))
                    .map_err(map_err)?;
                for row in note_rows {
                    authored_note_ids.push(row.map_err(map_err)?);
                }
            }
            drop(dids);
            drop(note_ids_stmt);
            // A relock changes visibility authority even for a title-only source whose canonical
            // text was already empty, so content-column UPDATE triggers may legitimately stay
            // silent. Queue every governed reminder source explicitly in this same reblank
            // transaction; the event contains only kind + opaque id.
            {
                let mut queue = tx
                    .prepare(
                        "INSERT INTO reminder_source_invalidation_queue
                           (source_kind,source_id,revision)
                         VALUES (?1,?2,1)
                         ON CONFLICT(source_kind,source_id)
                         DO UPDATE SET revision=revision+1",
                    )
                    .map_err(map_err)?;
                for meeting_id in &meeting_ids {
                    queue
                        .execute(rusqlite::params!["meeting", meeting_id])
                        .map_err(map_err)?;
                }
                for note_id in &authored_note_ids {
                    queue
                        .execute(rusqlite::params!["note", note_id])
                        .map_err(map_err)?;
                }
            }
            Self::purge_doc_chunks_tx(&tx, &document_ids)?;
            // Phase F0 LOCK-SAFETY: purge the correction-log rows of every meeting in these (now
            // re-blanked / sealed) folders in the SAME transaction — a sealed meeting contributes
            // nothing to the flywheel.
            Self::purge_corrections_tx(&tx, &meeting_ids)?;
            // 2026-07-10 audit F5: the four derived-content families the LOCK tx
            // (`purge_chunks_for_meetings`) and the STARTUP reconcile
            // (`reblank_locked_folders_at_rest`) already purge were MISSING from this RELOCK tx —
            // rows re-derived DURING a session unlock (facts extraction, user memory, voice Q&A,
            // re-diarized voiceprints) survived the relock at rest. Same purge-on-seal contract,
            // same meeting scope, same atomic unit as the plaintext re-blank above.
            Self::purge_facts_tx(&tx, &meeting_ids)?;
            Self::purge_user_facts_tx(&tx, &meeting_ids)?;
            Self::purge_assistant_interactions_tx(&tx, &meeting_ids)?;
            Self::purge_speaker_voiceprints_tx(&tx, &meeting_ids)?;
            // Re-Truth LOCK-SAFETY: drop supersession rows referencing any meeting in these
            // (re-blanked / sealed) folders — an applied row's plaintext note pre-image must not
            // linger at rest for a sealed folder (same purge-on-seal contract as corrections above).
            Self::purge_supersessions_tx(&tx, &meeting_ids)?;
            // Brain v2 L4 LOCK-SAFETY: drop the live-bullets crash-recovery rows of every meeting
            // in these (re-blanked / sealed) folders in this SAME relock tx — running notes are
            // plaintext derived from the transcript and must not survive a relock at rest.
            Self::purge_live_bullets_tx(&tx, &meeting_ids)?;
            // NOTE: the typed-notes (`manual_notes`) re-blank does NOT live here — it is SEALED-AND-
            // RESTORED content (not a derived/purgeable artifact like chunks/corrections), so its
            // plaintext is re-blanked only WHERE the `manual_notes_blob` exists, by
            // `reblank_folder_extras` (relock) / `reblank_locked_folders_at_rest` (startup). Blanking
            // it here (unconditionally, with no CK to re-seal) would destroy the only copy of a
            // typed buffer that had not yet been sealed — the verify-before-destroy violation.

            // Brain v2 L5 LOCK-SAFETY: purge any PENDING scheduled-brief row referencing a meeting
            // in these (re-blanked / sealed) folders in this SAME tx — a pending brief's `note_md`
            // paraphrases the sealed notes (accepted rows were consumed on accept). Same
            // purge-on-seal contract as the rollups below.
            Self::purge_pending_brief_runs_tx(&tx, &meeting_ids)?;
            // Vault Audit LOCK-SAFETY: purge ALL pending findings in this SAME relock tx — the
            // memory-rollups posture (adversarial HIGH: evidence may cite third-party titles no
            // meeting/document id can match; a relock invalidates the pass's visibility
            // snapshot). Resolved rows were blanked on resolve and survive.
            Self::purge_all_pending_audit_findings_tx(&tx)?;
            Self::purge_ask_conversations_for_folders_tx(&tx, folder_ids)?;
            // A relock withdraws the visibility snapshot that authorized every pending Smart
            // candidate. Purge the whole derived audit domain in this same re-blank transaction.
            Self::purge_all_reminder_derived_tx(&tx)?;

            // Brain v3 PR-3 LINK-ENGINE LOCK-SAFETY: purge every DERIVED `links` row whose SRC OR DST is
            // a meeting OR document/note in these (re-blanked / sealed) folders in this SAME relock tx —
            // a link names a neighbour (its title/existence reveals a possibly-sealed item). Same
            // purge-on-seal contract as the chunks above; re-derived on unlock. A relock/seal preserves
            // the user's decision rows (`preserve_decisions=true`, Fix 1).
            Self::purge_links_tx(&tx, &meeting_ids, &document_ids, true)?;
            // Brain v2 L2.1 LOCK-SAFETY: purge ALL memory rollups in this SAME relock tx — a rollup
            // may paraphrase the just-re-sealed facts. Cheap re-derivable synthesis; regenerates
            // from VISIBLE facts on the next hourly pass. The caller deletes the exported `.md`s.
            rollup_exports = Self::purge_memory_rollups_tx(&tx)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(rollup_exports)
    }

    /// SHOULD-FIX startup reconciliation: re-assert the at-rest sealed shape of EVERY `locked=1`
    /// folder. In one transaction, re-blank the plaintext `markdown` / segment `text` / timeline
    /// `data` of any row in a locked folder that still carries its AES-GCM blob (so a crash WHILE a
    /// folder was session-unlocked — which leaves plaintext in those columns — cannot survive a
    /// restart). Only rows WITH a blob are blanked (the blob is the recoverable source of truth); a
    /// blob-less plaintext row is left untouched so we never destroy unsealed content.
    ///
    /// Returns the at-rest audio columns of every meeting in a locked folder so the caller can
    /// re-seal stray plaintext audio (remove a plaintext file whose `.enc` already exists, or
    /// re-point a dangling column at a surviving `.enc`) on disk — that filesystem step lives in
    /// `state::reconcile_locked_at_rest` (the DB layer stays pure-SQL). ALL THREE per-stream paths
    /// are surfaced — the playback WAV (`audio_path`) AND the two hi-res masters
    /// (`mic_master_path` / `sys_master_path`). A crash-while-unlocked decrypts EVERY stream that
    /// was sealed, so re-pointing only `audio_path` would leave `{id}.mic.wav` / `{id}.sys.wav`
    /// plaintext on disk forever (B1) — the masters must be reconciled with the same logic.
    ///
    /// The second tuple element is the `exported_path`s of the memory rollups purged in-tx when any
    /// folder is locked (`purge_memory_rollups_tx`) — the caller deletes those vault `.md` files.
    ///
    /// The third tuple element (2026-07-10 audit F2) is `(id, exported_path)` of every authored
    /// NOTE in a locked folder: a session unlock re-exports each note's vault `.md`
    /// (`reexport_notes_in_folder`), so a crash-while-unlocked leaves that plaintext `.md` on disk.
    /// The caller ([`crate::state`] `reconcile_locked_at_rest`) deletes each file and clears that
    /// note's `exported_path` INDIVIDUALLY, only on a successful delete / already-absent file
    /// (delete-then-clear, per row — 2026-07-10 residual W5): a FAILED delete keeps the path
    /// recorded so the next startup retries. Mirrors the clean-relock cleanup in
    /// `reblank_folder_extras`.
    pub fn reblank_locked_folders_at_rest(&self) -> Result<LockedAtRestCleanup> {
        const LOCKED_MEETINGS: &str = "SELECT m.id AS meeting_id FROM meetings m
             WHERE m.folder_id IN (SELECT id FROM folders WHERE locked=1)
             UNION
             SELECT n.meeting_id FROM notes n JOIN meetings m ON m.id=n.meeting_id
              WHERE m.folder_id IS NULL
                AND n.folder_id IN (SELECT id FROM folders WHERE locked=1)";
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        #[cfg(debug_assertions)]
        let runtime_derived_before = if crate::commands::reminder_runtime_probe_requested()
            && std::env::var_os("MURMUR_HARNESS_PHASE_TWO_NONCE").is_some()
        {
            let count: i64 = tx
                .query_row(
                    "SELECT
                       (SELECT COUNT(*) FROM reminder_audit_cache) +
                       (SELECT COUNT(*) FROM reminder_pending_suggestions)",
                    [],
                    |row| row.get(0),
                )
                .map_err(map_err)?;
            Some(usize::try_from(count).map_err(|_| {
                crate::error::AppError::Storage("runtime reminder count is negative".into())
            })?)
        } else {
            None
        };
        tx.execute(
            "UPDATE notes SET markdown = '' \
               WHERE content_blob IS NOT NULL \
                 AND folder_id IN (SELECT id FROM folders WHERE locked = 1)",
            [],
        )
        .map_err(map_err)?;
        tx.execute(
            &format!("UPDATE segments SET text = '' WHERE text_blob IS NOT NULL AND meeting_id IN ({LOCKED_MEETINGS})"),
            [],
        )
        .map_err(map_err)?;
        tx.execute(
            &format!("UPDATE timelines SET data = '' WHERE data_blob IS NOT NULL AND meeting_id IN ({LOCKED_MEETINGS})"),
            [],
        )
        .map_err(map_err)?;
        // Phase 2a LOCK-SAFETY: purge plaintext-derived chunks + vectors for every meeting in a
        // locked folder, in this same reconciliation transaction — so a crash-while-unlocked (which
        // may have re-indexed) cannot leave a semantic vector of sealed content at rest after a
        // restart. Delete vec0 rows first (by chunk_id), then the source note_chunks rows.
        tx.execute(
            &format!(
                "DELETE FROM vec_chunks WHERE chunk_id IN \
                   (SELECT id FROM note_chunks WHERE meeting_id IN ({LOCKED_MEETINGS}))"
            ),
            [],
        )
        .map_err(map_err)?;
        tx.execute(
            &format!("DELETE FROM note_chunks WHERE meeting_id IN ({LOCKED_MEETINGS})"),
            [],
        )
        .map_err(map_err)?;
        // Brain v2 L1.1 LOCK-SAFETY: TOPIC chunks are the same class of plaintext-derived data —
        // purge them (vec0 rows first, then the base rows whose `_ad` FTS trigger drops the
        // aug_text tokens) in this same reconciliation transaction.
        tx.execute(
            &format!(
                "DELETE FROM topic_vec_chunks WHERE chunk_id IN \
                   (SELECT id FROM topic_chunks WHERE meeting_id IN ({LOCKED_MEETINGS}))"
            ),
            [],
        )
        .map_err(map_err)?;
        tx.execute(
            &format!("DELETE FROM topic_chunks WHERE meeting_id IN ({LOCKED_MEETINGS})"),
            [],
        )
        .map_err(map_err)?;
        // Phase F0 LOCK-SAFETY: purge correction-log rows for every meeting in a locked folder, in
        // this same reconciliation transaction — so a crash-while-unlocked (which may have logged a
        // correction) cannot leave sealed-content-derived training data at rest after a restart.
        tx.execute(
            &format!("DELETE FROM correction_log WHERE meeting_id IN ({LOCKED_MEETINGS})"),
            [],
        )
        .map_err(map_err)?;
        // LOCK-SAFETY: purge the voice-assistant Q&A log for every meeting in a locked folder, in
        // this same reconciliation transaction — so a crash-while-unlocked (which may have persisted
        // an interaction against a since-sealed meeting) cannot leave the plaintext Q&A at rest after
        // a restart. Same purge-on-seal contract as the correction-log above.
        tx.execute(
            &format!("DELETE FROM assistant_interactions WHERE meeting_id IN ({LOCKED_MEETINGS})"),
            [],
        )
        .map_err(map_err)?;
        // Brain v2 L4 LOCK-SAFETY: purge the live-bullets crash-recovery rows for every meeting in
        // a locked folder, in this same reconciliation transaction — so a crash mid-recording into
        // a since-sealed folder cannot leave the plaintext running notes at rest after a restart.
        // Same purge-on-seal contract as the assistant-interactions above.
        tx.execute(
            &format!("DELETE FROM live_bullets WHERE meeting_id IN ({LOCKED_MEETINGS})"),
            [],
        )
        .map_err(map_err)?;
        // brain2 R2 LOCK-SAFETY: purge the bitemporal facts for every meeting in a locked folder, in
        // this same reconciliation transaction — so a crash-while-unlocked (which may have re-derived
        // facts against a since-sealed meeting) cannot leave plaintext facts at rest after a restart.
        // Same purge-on-seal contract as the correction-log / assistant-interactions above.
        tx.execute(
            &format!("DELETE FROM facts WHERE meeting_id IN ({LOCKED_MEETINGS})"),
            [],
        )
        .map_err(map_err)?;
        // Phase 3 CROSS-MEETING USER MEMORY LOCK-SAFETY: purge user-scoped facts for every meeting in
        // a locked folder, in this same reconciliation transaction — so a crash-while-unlocked (which
        // may have re-derived user memory against a since-sealed meeting) cannot leave plaintext user
        // facts at rest after a restart. Same purge-on-seal contract as `facts` above.
        tx.execute(
            &format!("DELETE FROM user_facts WHERE meeting_id IN ({LOCKED_MEETINGS})"),
            [],
        )
        .map_err(map_err)?;
        // VOICEPRINT LOCK-SAFETY: purge the (opt-in) voice biometrics captured for every meeting in a
        // locked folder, in this same reconciliation transaction — so a crash-while-unlocked (which
        // may have re-diarized against a since-sealed meeting) cannot leave a remote speaker's
        // voiceprint at rest after a restart. Same purge-on-seal contract as `user_facts` above.
        tx.execute(
            &format!("DELETE FROM speaker_voiceprints WHERE meeting_id IN ({LOCKED_MEETINGS})"),
            [],
        )
        .map_err(map_err)?;
        // Re-Truth LOCK-SAFETY: purge every supersession referencing a locked meeting on EITHER side,
        // in this same reconciliation transaction — so a crash-while-unlocked (which may have applied a
        // stamp + stored plaintext note pre-images, or recorded old/new fact-value strings against a
        // since-sealed meeting) cannot leave those plaintext bytes/strings at rest after a restart.
        // Same purge-on-seal contract as `facts` / `user_facts` above; the row references two meetings
        // so it matches on both `source_meeting_id` and `superseding_meeting_id`.
        tx.execute(
            &format!(
                "DELETE FROM supersessions \
                   WHERE source_meeting_id IN ({LOCKED_MEETINGS}) \
                      OR superseding_meeting_id IN ({LOCKED_MEETINGS})"
            ),
            [],
        )
        .map_err(map_err)?;
        // Brain v2 L5 LOCK-SAFETY: purge every PENDING scheduled-brief row referencing a meeting in
        // a locked folder, in this same reconciliation transaction — a pending brief's `note_md` is
        // cross-meeting synthesis of the referenced notes and must not survive a restart while a
        // source folder is sealed (accepted rows were consumed on accept — ids + timestamps only).
        // `meeting_ids` is a JSON TEXT array of quote-delimited UUIDs, so the per-id LIKE
        // intersection is exact (see `purge_pending_brief_runs_tx`).
        tx.execute(
            &format!(
                "DELETE FROM brief_runs WHERE status = 'pending' AND EXISTS (\
                   SELECT 1 FROM ({LOCKED_MEETINGS}) lm \
                    WHERE brief_runs.meeting_ids LIKE '%\"' || lm.meeting_id || '\"%')"
            ),
            [],
        )
        .map_err(map_err)?;
        // Vault Audit LOCK-SAFETY: purge ALL pending audit findings in this same reconciliation
        // transaction — the memory-rollups posture (adversarial HIGH: a finding staged while a
        // since-sealed folder was visible may CITE its titles in evidence with no matching id,
        // e.g. a stale finding's `see [[superseding note]]`; scoping the purge to the locked
        // folders' ids cannot cover that). A crash-while-unlocked therefore leaves no finding
        // plaintext at rest after a restart. GUARDED on any locked folder existing: this
        // reconcile runs at EVERY launch, and with zero locked folders there is no seal whose
        // snapshot a finding could violate — a lock-free vault keeps its inbox across restarts.
        // Resolved rows were blanked on resolve and survive either way.
        let any_locked: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM folders WHERE locked = 1)",
                [],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        if any_locked {
            Self::purge_all_pending_audit_findings_tx(&tx)?;
            Self::purge_ask_conversations_for_locked_folders_tx(&tx)?;
            // Startup reconciliation must not leave candidates derived during a crashed unlocked
            // session at rest. Canonical promoted reminders remain untouched.
            Self::purge_all_reminder_derived_tx(&tx)?;
        }
        // brain2 realtime notes LOCK-SAFETY: re-blank the typed-notes plaintext of every meeting in a
        // locked folder ONLY WHERE its `manual_notes_blob` exists (the sealed copy is present) — so a
        // crash-while-unlocked (which restored the plaintext) cannot leave typed plaintext at rest
        // after a restart, but a buffer that was NEVER sealed (no blob) is left intact (never destroy
        // the only copy). Mirrors the `text_blob IS NOT NULL` / `data_blob IS NOT NULL` guards above.
        tx.execute(
            &format!("UPDATE meetings SET manual_notes = '' WHERE manual_notes_blob IS NOT NULL AND manual_notes != '' AND id IN ({LOCKED_MEETINGS})"),
            [],
        )
        .map_err(map_err)?;
        // Document ingestion LOCK-SAFETY: re-blank the plaintext `text` of every document in a locked
        // folder ONLY WHERE its `text_blob` exists (the sealed copy is present) — so a
        // crash-while-unlocked (which restored the plaintext) cannot leave document plaintext at rest
        // after a restart, but a document that was NEVER sealed (no blob) is left intact (never
        // destroy the only copy). Mirrors the `manual_notes_blob IS NOT NULL` guard above.
        tx.execute(
            "UPDATE documents SET text = '' WHERE text_blob IS NOT NULL AND text != '' \
               AND folder_id IN (SELECT id FROM folders WHERE locked = 1)",
            [],
        )
        .map_err(map_err)?;
        // And purge the (invertible) doc chunks + vectors of every document in a locked folder, in
        // this same reconciliation transaction — so a crash-while-unlocked (which may have
        // re-embedded) cannot leave a document's semantic vector of sealed content at rest after a
        // restart. Delete doc_vec_chunks rows first (by chunk_id), then the source doc_chunks rows.
        tx.execute(
            "DELETE FROM doc_vec_chunks WHERE chunk_id IN \
               (SELECT id FROM doc_chunks WHERE document_id IN \
                  (SELECT id FROM documents WHERE folder_id IN \
                     (SELECT id FROM folders WHERE locked = 1)))",
            [],
        )
        .map_err(map_err)?;
        tx.execute(
            "DELETE FROM doc_chunks WHERE document_id IN \
               (SELECT id FROM documents WHERE folder_id IN \
                  (SELECT id FROM folders WHERE locked = 1))",
            [],
        )
        .map_err(map_err)?;
        // Brain-v3 audit Fix 4 (startup net): a crash BETWEEN a seal and its marker-strip could leave a
        // sealed neighbour's `[[Title]]` in a VISIBLE source note's plaintext (DB + `.md`). Repair it
        // here, BEFORE the links purge deletes the rows that name the affected sources. Resolve the
        // locked folders' meeting + document id lists, then strip each sealed title from every visible
        // source's managed block in THIS reconciliation tx. Exact export path+title cleanup authority
        // is journaled by the strip helper and drained after this transaction by `crate::state`.
        let locked_meeting_ids: Vec<String> = {
            let mut stmt = tx.prepare(LOCKED_MEETINGS).map_err(map_err)?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(map_err)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(map_err)?);
            }
            out
        };
        let locked_document_ids: Vec<String> = {
            let mut stmt = tx
                .prepare(
                    "SELECT id FROM documents WHERE folder_id IN (SELECT id FROM folders WHERE locked = 1)",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(map_err)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(map_err)?);
            }
            out
        };
        Self::strip_sealed_neighbour_markers_tx(&tx, &locked_meeting_ids, &locked_document_ids)?;
        // Brain v3 PR-3 LINK-ENGINE LOCK-SAFETY: purge every DERIVED `links` row whose meeting endpoint
        // is in a locked folder, OR whose document/note endpoint is in a locked folder, in this same
        // reconciliation transaction — so a crash-while-unlocked (which may have re-derived
        // wikilink/semantic edges against since-sealed items) cannot leave a link naming a sealed
        // neighbour at rest after a restart. Re-derived on the next unlock. A note id IS a document
        // id, so the document leg's `IN ('note','document')` covers both kinds.
        //
        // Brain-v3 audit Fix 1: PRESERVE a user's decision rows (`LINK_DECISION_KEEP` — dismissed
        // tombstones, accepted edges and manual links) across this reconcile, mirroring `purge_links_tx`, so a restart
        // (like a lock→unlock) never resurrects a dismissed suggestion or forgets an accepted edge.
        // These rows carry ids/kind/edge_type/score only — no titles/plaintext — and stay invisible
        // via the both-endpoint read gate while an endpoint is sealed.
        let keep = Self::LINK_DECISION_KEEP;
        tx.execute(
            &format!(
                "DELETE FROM links WHERE ( \
                   ((src_kind = 'meeting' AND src_id IN ({LOCKED_MEETINGS})) \
                    OR (dst_kind = 'meeting' AND dst_id IN ({LOCKED_MEETINGS}))) \
                   OR (src_kind IN ('note','document') AND src_id IN \
                        (SELECT id FROM documents WHERE folder_id IN \
                           (SELECT id FROM folders WHERE locked = 1))) \
                   OR (dst_kind IN ('note','document') AND dst_id IN \
                        (SELECT id FROM documents WHERE folder_id IN \
                           (SELECT id FROM folders WHERE locked = 1)))) \
                   AND NOT ({keep})"
            ),
            [],
        )
        .map_err(map_err)?;
        // Collect the at-rest audio columns of locked meetings for the caller's filesystem re-seal
        // pass — the playback WAV AND both hi-res masters (B1). A meeting is surfaced if ANY of the
        // three columns is set; each path is reconciled independently by the caller.
        let mut audio = Vec::new();
        {
            let mut stmt = tx
                .prepare(&format!(
                    "SELECT id, audio_path, mic_master_path, sys_master_path FROM meetings \
                       WHERE (audio_path IS NOT NULL \
                              OR mic_master_path IS NOT NULL \
                              OR sys_master_path IS NOT NULL) \
                         AND id IN ({LOCKED_MEETINGS})"
                ))
                .map_err(map_err)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(LockedMeetingAudio {
                        meeting_id: r.get::<_, String>(0)?,
                        audio_path: r.get::<_, Option<String>>(1)?,
                        mic_master_path: r.get::<_, Option<String>>(2)?,
                        sys_master_path: r.get::<_, Option<String>>(3)?,
                    })
                })
                .map_err(map_err)?;
            for r in rows {
                audio.push(r.map_err(map_err)?);
            }
        }
        // Brain v2 L2.1 LOCK-SAFETY: when ANY folder is locked, purge ALL memory rollups in this
        // same reconciliation transaction — a rollup synthesized before the seal (or during a
        // crashed unlocked session) may paraphrase sealed facts. Skipped when nothing is locked
        // (no sealed content ⇒ no leak ⇒ rollups survive restarts). The caller deletes the
        // returned exported vault `.md`s.
        let any_locked: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM folders WHERE locked = 1)",
                [],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        let rollup_exports = if any_locked {
            Self::purge_memory_rollups_tx(&tx)?
        } else {
            Vec::new()
        };
        // 2026-07-10 audit F2: surface the authored notes' re-exported vault `.md` (id, path) pairs
        // for every LOCKED folder (a crash while session-unlocked leaves them plaintext on disk with
        // the column still set). Ids + paths only — the caller deletes each file then clears THAT
        // note's column (per-row delete-then-clear, residual W5), never text (no PII here).
        let note_md_exports = {
            let mut stmt = tx
                .prepare(
                    "SELECT id, exported_path FROM documents \
                       WHERE kind = 'note' AND exported_path IS NOT NULL \
                         AND folder_id IN (SELECT id FROM folders WHERE locked = 1)",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(map_err)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(map_err)?);
            }
            out
        };
        #[cfg(debug_assertions)]
        let runtime_derived_after = if runtime_derived_before.is_some() {
            let count: i64 = tx
                .query_row(
                    "SELECT
                       (SELECT COUNT(*) FROM reminder_audit_cache) +
                       (SELECT COUNT(*) FROM reminder_pending_suggestions)",
                    [],
                    |row| row.get(0),
                )
                .map_err(map_err)?;
            Some(usize::try_from(count).map_err(|_| {
                crate::error::AppError::Storage("runtime reminder count is negative".into())
            })?)
        } else {
            None
        };
        tx.commit().map_err(map_err)?;
        #[cfg(debug_assertions)]
        if let (Some(before), Some(after)) = (runtime_derived_before, runtime_derived_after) {
            crate::commands::record_reminder_runtime_startup_reconcile(before, after)?;
        }
        Ok((audio, rollup_exports, note_md_exports))
    }

    /// The RAW segment rows of a meeting (idx, plaintext text, sealed `text_blob`), regardless of
    /// seal state — for the seal/unseal lifecycle (NOT a user-facing read; that is `get_segments`).
    pub fn raw_segments(&self, meeting_id: &str) -> Result<Vec<RawSegment>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT idx, text, text_blob FROM segments
                   WHERE meeting_id = ?1 ORDER BY idx",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![meeting_id], |r| {
                Ok(RawSegment {
                    idx: r.get(0)?,
                    text: r.get(1)?,
                    text_blob: r.get(2)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Seal ONE segment row: store its AES-GCM `text_blob` and blank the plaintext `text`.
    /// Verified-before-blank by the caller (mirrors `seal_note`).
    pub fn seal_segment(&self, meeting_id: &str, idx: i64, text_blob: &[u8]) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE segments SET text_blob = ?3, text = '' WHERE meeting_id = ?1 AND idx = ?2",
            rusqlite::params![meeting_id, idx, text_blob],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Restore ONE segment row's plaintext `text` (session-unlock / remove-lock). Leaves
    /// `text_blob` intact (the caller clears it only on permanent remove-lock).
    pub fn restore_segment_text(&self, meeting_id: &str, idx: i64, text: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE segments SET text = ?3 WHERE meeting_id = ?1 AND idx = ?2",
            rusqlite::params![meeting_id, idx, text],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Clear the sealed `text_blob` for every segment of a meeting (permanent remove-lock, after
    /// the plaintext is restored).
    pub fn clear_segment_blobs(&self, meeting_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE segments SET text_blob = NULL WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The RAW timeline row of a meeting (plaintext `data`, sealed `data_blob`), regardless of
    /// seal state — for the seal/unseal lifecycle. `None` if the meeting has no timeline cached.
    pub fn raw_timeline(&self, meeting_id: &str) -> Result<Option<RawTimeline>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT data, data_blob FROM timelines WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
            |r| {
                Ok(RawTimeline {
                    data: r.get(0)?,
                    data_blob: r.get(1)?,
                })
            },
        )
        .optional()
        .map_err(map_err)
    }

    /// Seal a meeting's timeline: store its AES-GCM `data_blob`, blank the plaintext `data`.
    pub fn seal_timeline(&self, meeting_id: &str, data_blob: &[u8]) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE timelines SET data_blob = ?2, data = '' WHERE meeting_id = ?1",
            rusqlite::params![meeting_id, data_blob],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Restore a meeting's timeline plaintext `data` (session-unlock / remove-lock). Leaves
    /// `data_blob` intact (cleared only on permanent remove-lock).
    pub fn restore_timeline_data(&self, meeting_id: &str, data: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE timelines SET data = ?2 WHERE meeting_id = ?1",
            rusqlite::params![meeting_id, data],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Clear the sealed `data_blob` for a meeting's timeline (permanent remove-lock).
    pub fn clear_timeline_blob(&self, meeting_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE timelines SET data_blob = NULL WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
        )
        .map_err(map_err)?;
        Ok(())
    }
}
