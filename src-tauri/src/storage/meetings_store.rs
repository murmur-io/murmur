//! Meeting storage surface — the `meetings`-table row CRUD + crash-recovery status lifecycle + the
//! per-meeting typed-notes (`manual_notes`) seal lifecycle + tags + per-stream master audio paths +
//! the VISIBILITY-GATED meeting readers, extracted verbatim from `storage::db` (God-file split, a
//! PURE MOVE — no behavior change). The methods below are an inherent-impl split of
//! [`crate::storage::db::Db`] across files (Rust allows one type's inherent `impl` to live in
//! multiple files of the same crate); every method retains its EXACT prior body, signature, AND
//! gating. The gated readers (`list_meetings_visible`, `meeting_by_title_visible`,
//! `meeting_by_title_folded_visible`, `meeting_is_visible`) route through `visibility_clause`
//! EXACTLY as on trunk — a meeting whose every note is sealed-and-not-session-unlocked stays
//! invisible. The manual-notes SEAL twins (`set_manual_notes_sealed`, `seal_manual_notes`) are
//! unchanged low-level `meetings`-column writers: the CALLER still verifies the blob decrypts back
//! byte-identical BEFORE calling (verify-before-destroy) — the seal-verify caller stays in
//! `commands.rs`. The audio master-path writers only re-point non-content path columns (the
//! audio-at-rest seal lifecycle drives them; the plaintext WAV removal + `encrypt_file`
//! verify-before-destroy stay in `commands.rs`). The heavier `delete_meeting` (with its
//! cross-domain `purge_*_tx` fan-out) and the search/index machinery STAY in db.rs. Shared db.rs
//! helpers `map_err` / `visibility_clause` are `pub(crate)`, and `row_to_meeting` is `pub(crate)`
//! (reached cross-file); the `RawManualNotes` DTO stays in db.rs. Tests stay in db.rs's `mod tests`
//! (shared harness); the count is conserved.

use std::collections::HashSet;

use rusqlite::OptionalExtension;

use crate::error::{AppError, Result};
use crate::storage::db::{
    map_err, meeting_visibility_clause, row_to_meeting, visibility_clause, Db, RawManualNotes,
};
use crate::storage::models::{Meeting, MeetingStatus};

pub(crate) type MeetingTriageRow = (Meeting, i64, bool);

/// Visibility of one `meetings m` row. Canonical recording placement wins; rows intentionally left
/// NULL by the conservative migration retain the legacy note-owned gate (and therefore never pick
/// an arbitrary provider folder).
fn meeting_visible_sql(unlocked: &HashSet<String>) -> String {
    meeting_visibility_clause("m", unlocked)
}

impl Db {
    pub fn insert_meeting(&self, m: &Meeting) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO meetings
               (id, started_at, ended_at, title, duration_s, audio_path, status, folder_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                m.id,
                m.started_at,
                m.ended_at,
                m.title,
                m.duration_s,
                m.audio_path,
                m.status.as_str(),
                m.folder_id,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub fn update_meeting_status(&self, id: &str, status: MeetingStatus) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE meetings SET status = ?2 WHERE id = ?1",
            rusqlite::params![id, status.as_str()],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Compare-and-swap a meeting's status: set it to `to` ONLY if it is currently `from`.
    /// Returns `true` when the transition landed (exactly one row changed), `false` when the row
    /// was in a different status (or absent) — the loser of a concurrent claim. Used by
    /// `retry_transcription` as its single-flight claim (`Error → Recording`): two simultaneous
    /// retries of the same meeting can never both run the pipeline.
    pub fn transition_meeting_status(
        &self,
        id: &str,
        from: MeetingStatus,
        to: MeetingStatus,
    ) -> Result<bool> {
        let conn = self.lock();
        let n = conn
            .execute(
                "UPDATE meetings SET status = ?3 WHERE id = ?1 AND status = ?2",
                rusqlite::params![id, from.as_str(), to.as_str()],
            )
            .map_err(map_err)?;
        Ok(n == 1)
    }

    /// Ids of every meeting still stuck in the non-terminal `RECORDING` status — the crash "ghosts"
    /// startup recovery must resolve (spill salvage / disk salvage / reconcile-to-`ERROR`).
    /// UUIDs only, no content.
    pub fn stuck_recording_ids(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id FROM meetings WHERE status = ?1")
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![MeetingStatus::Recording.as_str()], |r| {
                r.get::<_, String>(0)
            })
            .map_err(map_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(map_err)
    }

    /// Crash-recovery reconcile: flip every meeting still stuck in `RECORDING` to the terminal
    /// `ERROR` state. Returns the number of rows reconciled.
    ///
    /// `start_recording` (`commands.rs`) inserts a meeting row in `RECORDING` up-front so a crash /
    /// SIGKILL mid-capture leaves a recoverable row instead of losing the meeting outright. A process
    /// that dies before `stop_recording`, though, never transitions that row out of `RECORDING`, so
    /// it lingers in the library as a "ghost" that still looks live forever. Run this once at launch
    /// (from `lib.rs` setup, after the DB is open + migrated) to make each ghost HONEST: it becomes a
    /// plain terminal `ERROR` row (which the library already renders as "Error" — no audio, no note,
    /// no spinner).
    ///
    /// ADDITIVE + non-destructive: no row is deleted and no other column is touched. Full audio
    /// salvage of an abandoned recording (mic spill) is a SEPARATE, later task. Idempotent — with no
    /// live recording a second call reconciles 0. Non-`RECORDING` rows (Complete/Error/…) are left
    /// untouched by the `WHERE status = 'RECORDING'` guard. Logs only meeting UUIDs + a count (no PII).
    pub fn reconcile_stuck_recordings(&self) -> Result<usize> {
        self.reconcile_stuck_recordings_except(&[])
    }

    /// [`Self::reconcile_stuck_recordings`], but SKIPPING every meeting id in `claimed` — the rows the
    /// STAGE-2 crash-salvage (`audio::spill`) is handling THIS launch. Salvage runs BEFORE reconcile
    /// in setup and CLAIMS the recoverable ghosts (reconstructing their audio + running the pipeline,
    /// which sets their final status itself); reconcile must NOT clobber a claimed row to `ERROR` in
    /// the window before the async salvage worker transitions it. Every OTHER stuck `RECORDING` ghost
    /// (no spill / not recoverable) is still flipped to the terminal `ERROR` state exactly as before.
    /// Idempotent + additive (the per-row `AND status = RECORDING` guard). Logs ids + counts, no PII.
    pub fn reconcile_stuck_recordings_except(&self, claimed: &[String]) -> Result<usize> {
        // Collect the ghost ids first (UUIDs — not PII) so the reconcile is auditable in the log.
        // Done BEFORE taking the connection lock (`stuck_recording_ids` locks internally; the Mutex
        // is not re-entrant).
        let ids: Vec<String> = self.stuck_recording_ids()?;
        let conn = self.lock();
        // Exclude the rows salvage claimed this launch — it owns their final status.
        let to_reconcile: Vec<&String> = ids.iter().filter(|id| !claimed.contains(id)).collect();
        if to_reconcile.is_empty() {
            return Ok(0);
        }
        let mut n = 0usize;
        for id in &to_reconcile {
            n += conn
                .execute(
                    "UPDATE meetings SET status = ?2 WHERE id = ?1 AND status = ?3",
                    rusqlite::params![
                        id,
                        MeetingStatus::Error.as_str(),
                        MeetingStatus::Recording.as_str()
                    ],
                )
                .map_err(map_err)?;
        }
        tracing::info!(
            target: "startup",
            reconciled = n,
            skipped_for_salvage = claimed.len(),
            ids = ?to_reconcile,
            "reconciled stuck RECORDING meetings to ERROR (crash recovery)"
        );
        Ok(n)
    }

    pub fn finalize_meeting(
        &self,
        id: &str,
        ended_at: &str,
        duration_s: i64,
        audio_path: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE meetings
               SET ended_at = ?2, duration_s = ?3, audio_path = ?4
             WHERE id = ?1",
            rusqlite::params![id, ended_at, duration_s, audio_path],
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub fn set_meeting_title(&self, id: &str, title: &str) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let changed = tx.execute(
            "UPDATE meetings SET title = ?2 WHERE id = ?1",
            rusqlite::params![id, title],
        )
        .map_err(map_err)?;
        if changed != 0 {
            tx.execute(
                "UPDATE org_shares SET republish_dirty = republish_dirty + 1
                  WHERE meeting_id = ?1 AND state IN ('queued','uploaded','failed')",
                rusqlite::params![id],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)
    }

    /// Upsert the meeting's typed-notes plaintext. Used by the FE autosave (write the whole buffer)
    /// AND by the unseal/remove-lock RESTORE (write the decrypted plaintext back). No-op on an
    /// unknown meeting.
    pub fn set_manual_notes(&self, meeting_id: &str, text: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE meetings SET manual_notes = ?2 WHERE id = ?1",
            rusqlite::params![meeting_id, text],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// SEAL-ON-WRITE twin of [`Db::set_manual_notes`] for a meeting in a session-unlocked LOCKED
    /// folder: persist the fresh plaintext (session-visible) AND its freshly-encrypted
    /// `manual_notes_blob` in ONE atomic statement, so relock/at-rest reblank restores THIS buffer —
    /// never a stale lock-time copy. The CALLER must have verified the blob decrypts back
    /// byte-identical BEFORE calling this (verify-before-destroy) — exactly like
    /// [`Db::seal_manual_notes`]. 2026-07-10 audit F1.
    pub fn set_manual_notes_sealed(&self, meeting_id: &str, text: &str, blob: &[u8]) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE meetings SET manual_notes = ?2, manual_notes_blob = ?3 WHERE id = ?1",
            rusqlite::params![meeting_id, text, blob],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The meeting's typed-notes plaintext, or "" when never set / NULL (legacy rows) / unknown id /
    /// sealed-and-blanked. UNGATED at the DB layer — callers that return this to a surface MUST gate
    /// first (`meeting_is_unlocked` in commands / `meeting_is_visible` for the live brain). The
    /// (re)summarize fold reads it raw (it is the producer of the note plaintext, not a leak surface).
    pub fn get_manual_notes(&self, meeting_id: &str) -> Result<String> {
        let conn = self.lock();
        let text: Option<String> = conn
            .query_row(
                "SELECT manual_notes FROM meetings WHERE id = ?1",
                rusqlite::params![meeting_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(map_err)?
            .flatten();
        Ok(text.unwrap_or_default())
    }

    /// The meeting's typed notes in EITHER seal state (plaintext + the AES-GCM blob under the folder
    /// CK) — the read used by the unseal/reblank lifecycle. `None` only when the meeting row is
    /// absent. Mirrors [`Db::raw_timeline`].
    pub fn raw_manual_notes(&self, meeting_id: &str) -> Result<Option<RawManualNotes>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT COALESCE(manual_notes, ''), manual_notes_blob FROM meetings WHERE id = ?1",
            rusqlite::params![meeting_id],
            |r| {
                Ok(RawManualNotes {
                    text: r.get(0)?,
                    blob: r.get(1)?,
                })
            },
        )
        .optional()
        .map_err(map_err)
    }

    /// Seal a meeting's typed notes: store the AES-GCM `manual_notes_blob`, blank the plaintext
    /// `manual_notes`. The CALLER must verify the blob decrypts back byte-identical BEFORE calling
    /// this (verify-before-destroy) — exactly like [`Db::seal_timeline`].
    pub fn seal_manual_notes(&self, meeting_id: &str, blob: &[u8]) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE meetings SET manual_notes_blob = ?2, manual_notes = '' WHERE id = ?1",
            rusqlite::params![meeting_id, blob],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Clear a meeting's sealed `manual_notes_blob` (permanent remove-lock, after the plaintext is
    /// restored). Mirrors [`Db::clear_timeline_blob`].
    pub fn clear_manual_notes_blob(&self, meeting_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE meetings SET manual_notes_blob = NULL WHERE id = ?1",
            rusqlite::params![meeting_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub fn get_meeting(&self, id: &str) -> Result<Option<Meeting>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, started_at, ended_at, title, duration_s, audio_path, status, folder_id
               FROM meetings WHERE id = ?1",
            rusqlite::params![id],
            row_to_meeting,
        )
        .optional()
        .map_err(map_err)?
        .transpose()
    }

    /// Content-free meeting projection for command authorization and masked DTOs. Deliberately
    /// omits the real title and audio path, while retaining non-content lifecycle metadata needed
    /// to distinguish an unknown id and render the locked shell.
    pub fn get_meeting_gate_anchor(&self, id: &str) -> Result<Option<Meeting>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, started_at, ended_at, NULL, duration_s, NULL, status, folder_id
               FROM meetings WHERE id = ?1",
            rusqlite::params![id],
            row_to_meeting,
        )
        .optional()
        .map_err(map_err)?
        .transpose()
    }

    pub fn latest_meeting(&self) -> Result<Option<Meeting>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, started_at, ended_at, title, duration_s, audio_path, status, folder_id
               FROM meetings ORDER BY started_at DESC, id DESC LIMIT 1",
            [],
            row_to_meeting,
        )
        .optional()
        .map_err(map_err)?
        .transpose()
    }

    /// Recent meetings, newest first (Library list).
    pub fn list_meetings(&self, limit: i64) -> Result<Vec<Meeting>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, started_at, ended_at, title, duration_s, audio_path, status, folder_id
                   FROM meetings ORDER BY started_at DESC, id DESC LIMIT ?1",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![limit], row_to_meeting)
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            // row_to_meeting yields rusqlite::Result<Result<Meeting>>: unwrap both layers.
            out.push(r.map_err(map_err)??);
        }
        Ok(out)
    }

    /// Replace all tags for a meeting with `tags` (trimmed, blanks dropped).
    pub fn set_meeting_tags(&self, meeting_id: &str, tags: &[String]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        tx.execute(
            "DELETE FROM meeting_tags WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
        )
        .map_err(map_err)?;
        {
            let mut stmt = tx
                .prepare("INSERT OR IGNORE INTO meeting_tags (meeting_id, tag) VALUES (?1, ?2)")
                .map_err(map_err)?;
            for tag in tags {
                let t = tag.trim();
                if !t.is_empty() {
                    stmt.execute(rusqlite::params![meeting_id, t])
                        .map_err(map_err)?;
                }
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// All tags for a meeting, sorted.
    pub fn get_meeting_tags(&self, meeting_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT tag FROM meeting_tags WHERE meeting_id = ?1 ORDER BY tag")
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![meeting_id], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// All distinct tags across meetings, sorted (for the filter UI).
    pub fn list_all_tags(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT DISTINCT tag FROM meeting_tags ORDER BY tag")
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Meetings carrying `tag`, newest first.
    pub fn list_meetings_by_tag(&self, tag: &str) -> Result<Vec<Meeting>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT m.id, m.started_at, m.ended_at, m.title, m.duration_s, m.audio_path, \
                        m.status, m.folder_id
                   FROM meetings m
                   JOIN meeting_tags t ON t.meeting_id = m.id
                  WHERE t.tag = ?1
                  ORDER BY m.started_at DESC, m.id DESC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![tag], row_to_meeting)
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)??);
        }
        Ok(out)
    }

    /// Set (or clear) a meeting's `audio_path` — used by the audio-at-rest encryption lifecycle
    /// to re-point at the decrypted-for-session copy and back to the plaintext WAV on remove-lock.
    pub fn set_meeting_audio_path(&self, meeting_id: &str, audio_path: Option<&str>) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE meetings SET audio_path = ?2 WHERE id = ?1",
            rusqlite::params![meeting_id, audio_path],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Read a meeting's per-stream master paths `(mic_master_path, sys_master_path)`. A TARGETED
    /// query so the masters never ride on the `Meeting` struct / its DTO — keeping them off
    /// `Meeting` is what makes a masked-detail leak structurally impossible. NULL when not kept.
    pub fn get_meeting_master_paths(
        &self,
        meeting_id: &str,
    ) -> Result<(Option<String>, Option<String>)> {
        let conn = self.lock();
        conn.query_row(
            "SELECT mic_master_path, sys_master_path FROM meetings WHERE id = ?1",
            rusqlite::params![meeting_id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .optional()
        .map_err(map_err)?
        .ok_or_else(|| AppError::Storage(format!("no meeting with id {meeting_id}")))
    }

    /// Set (or clear) a meeting's mic master path (the audio-at-rest seal lifecycle re-points it).
    pub fn set_meeting_mic_master_path(&self, meeting_id: &str, path: Option<&str>) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE meetings SET mic_master_path = ?2 WHERE id = ?1",
            rusqlite::params![meeting_id, path],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Set (or clear) a meeting's system master path (the audio-at-rest seal lifecycle re-points it).
    pub fn set_meeting_sys_master_path(&self, meeting_id: &str, path: Option<&str>) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE meetings SET sys_master_path = ?2 WHERE id = ?1",
            rusqlite::params![meeting_id, path],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Race-safe clear of `audio_path`: NULL it ONLY if it still equals `expected` (the plaintext
    /// path the prune snapshotted). If a concurrent seal re-pointed it to `.enc` in between, the
    /// `AND audio_path = ?2` fails to match → no-op → the freshly-sealed pointer SURVIVES. Used by
    /// the storage prune, which snapshots candidates OUTSIDE the seal lifecycle lock (TOCTOU-safe).
    pub fn clear_meeting_audio_path_if(&self, meeting_id: &str, expected: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE meetings SET audio_path = NULL WHERE id = ?1 AND audio_path = ?2",
            rusqlite::params![meeting_id, expected],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Race-safe clear of `mic_master_path`: NULL it ONLY if it still equals `expected`. Mirrors
    /// [`Db::clear_meeting_audio_path_if`] — a concurrent seal that re-pointed it survives.
    pub fn clear_meeting_mic_master_path_if(&self, meeting_id: &str, expected: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE meetings SET mic_master_path = NULL WHERE id = ?1 AND mic_master_path = ?2",
            rusqlite::params![meeting_id, expected],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Race-safe clear of `sys_master_path`: NULL it ONLY if it still equals `expected`. Mirrors
    /// [`Db::clear_meeting_audio_path_if`] — a concurrent seal that re-pointed it survives.
    pub fn clear_meeting_sys_master_path_if(&self, meeting_id: &str, expected: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE meetings SET sys_master_path = NULL WHERE id = ?1 AND sys_master_path = ?2",
            rusqlite::params![meeting_id, expected],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Recent visible meetings only (MCP `list_recent_meetings`). A meeting is visible if it has
    /// no note, or any of its notes is visible (open/unlocked folder).
    pub fn list_meetings_visible(
        &self,
        limit: i64,
        unlocked: &HashSet<String>,
        scope: Option<&[String]>,
    ) -> Result<Vec<Meeting>> {
        let conn = self.lock();
        let meeting_visible = meeting_visible_sql(unlocked);
        // Scope narrows the fallback too. Without this, a scoped question that matches nothing
        // would answer from the WHOLE vault's recent meetings — silently outside the scope the
        // user chose, which is worse than answering "nothing here".
        let scoped = crate::storage::db::folder_scope_clause("m", scope);
        // A meeting is hidden only when EVERY note it has is sealed-and-not-unlocked. Expressed
        // as: no note row exists that is currently sealed-and-hidden for this meeting, unless a
        // sibling visible note exists. Simpler + correct: keep the meeting if it has zero notes
        // OR at least one visible note.
        let sql = format!(
            "SELECT m.id, m.started_at, m.ended_at, m.title, m.duration_s, m.audio_path, m.status,
                    m.folder_id
               FROM meetings m
              WHERE {meeting_visible}{scoped}
              ORDER BY m.started_at DESC, m.id DESC
              LIMIT ?1"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![limit], row_to_meeting)
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)??);
        }
        Ok(out)
    }

    /// A bounded, visibility-gated aggregate for local-MCP meeting triage.
    ///
    /// Each row carries the meeting, transcript character count, and whether at least one visible
    /// note exists. One SQL statement supplies all three, so `list_recent_meetings` does not issue
    /// an extra transcript/note read per meeting. A sealed-and-not-session-unlocked meeting produces
    /// no row at all, exactly like [`Self::list_meetings_visible`].
    pub fn list_meeting_triage_visible(
        &self,
        limit: i64,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<MeetingTriageRow>> {
        let conn = self.lock();
        let meeting_visible = meeting_visible_sql(unlocked);
        let visible = visibility_clause("n", unlocked);
        let limit = limit.clamp(1, 100);
        let sql = format!(
            "SELECT m.id, m.started_at, m.ended_at, m.title, m.duration_s, m.audio_path, m.status,
                    m.folder_id,
                    COALESCE(
                      (SELECT SUM(LENGTH(s.text)) FROM segments s WHERE s.meeting_id = m.id),
                      0
                    ) AS transcript_chars,
                    EXISTS (
                      SELECT 1 FROM notes n
                       LEFT JOIN folders f ON f.id = n.folder_id
                       WHERE n.meeting_id = m.id AND {visible}
                    ) AS has_visible_note
               FROM meetings m
              WHERE {meeting_visible}
              ORDER BY m.started_at DESC, m.id DESC
              LIMIT ?1"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![limit], |row| {
                let meeting = row_to_meeting(row)?;
                let transcript_chars: i64 = row.get(8)?;
                let has_visible_note = row.get::<_, i64>(9)? != 0;
                Ok((meeting, transcript_chars, has_visible_note))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (meeting, transcript_chars, has_visible_note) = row.map_err(map_err)?;
            out.push((meeting?, transcript_chars, has_visible_note));
        }
        Ok(out)
    }

    /// The most recent VISIBLE meeting titled exactly `title` — the Ask surface's citation→source
    /// resolution (a `[[Title]]` wikilink back to a meeting id/date chip). Applies the SAME
    /// visibility predicate as [`Self::list_meetings_visible`], so a sealed-and-not-session-unlocked
    /// meeting can never resolve — a citation string can't become an existence/date leak. Exact
    /// (case-sensitive) title match; newest first when titles collide.
    pub fn meeting_by_title_visible(
        &self,
        title: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<Meeting>> {
        let conn = self.lock();
        let meeting_visible = meeting_visible_sql(unlocked);
        let sql = format!(
            "SELECT m.id, m.started_at, m.ended_at, m.title, m.duration_s, m.audio_path, m.status,
                    m.folder_id
               FROM meetings m
              WHERE m.title = ?1
                AND {meeting_visible}
              ORDER BY m.started_at DESC, m.id DESC
              LIMIT 1"
        );
        conn.query_row(&sql, rusqlite::params![title], row_to_meeting)
            .optional()
            .map_err(map_err)?
            .transpose()
    }

    /// Full meeting row through one SQL visibility predicate. Unlike a check-then-`get_meeting`
    /// pair, a concurrent relock cannot land between authorization and reading title/audio_path.
    ///
    /// Canonical ownership invariant: a meeting's folder edge is `meetings.folder_id`, including
    /// before its first note exists. A NULL canonical edge is unfiled unless conservative legacy
    /// note ownership governs it. Do not infer placement from optional artifacts: segment/timeline/
    /// manual-note blobs and encrypted audio can all legitimately be absent. The predicate below
    /// gates and hydrates the row in the same SQL statement.
    pub fn get_meeting_if_visible(
        &self,
        id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<Meeting>> {
        let conn = self.lock();
        let meeting_visible = meeting_visible_sql(unlocked);
        let sql = format!(
            "SELECT m.id, m.started_at, m.ended_at, m.title, m.duration_s, m.audio_path, m.status,
                    m.folder_id
               FROM meetings m
              WHERE m.id = ?1 AND {meeting_visible}"
        );
        conn.query_row(&sql, rusqlite::params![id], row_to_meeting)
            .optional()
            .map_err(map_err)?
            .transpose()
    }

    /// CASE-FOLDED twin of [`Self::meeting_by_title_visible`] (brain-v3 audit Fix 6): the same
    /// gated resolver but comparing `LOWER(m.title) = LOWER(?1)` so `[[project x]]` resolves a
    /// meeting titled "Project X". Used ONLY as [`Self::resolve_wikilink`]'s meeting-leg fallback
    /// after the exact match misses. Same visibility predicate — a sealed meeting never resolves.
    /// (SQLite `LOWER()` folds ASCII only; full Unicode fold is deferred with the note leg's.)
    pub(crate) fn meeting_by_title_folded_visible(
        &self,
        title: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<Meeting>> {
        let conn = self.lock();
        let meeting_visible = meeting_visible_sql(unlocked);
        let sql = format!(
            "SELECT m.id, m.started_at, m.ended_at, m.title, m.duration_s, m.audio_path, m.status,
                    m.folder_id
               FROM meetings m
              WHERE LOWER(m.title) = LOWER(?1)
                AND {meeting_visible}
              ORDER BY m.started_at DESC, m.id DESC
              LIMIT 1"
        );
        conn.query_row(&sql, rusqlite::params![title], row_to_meeting)
            .optional()
            .map_err(map_err)?
            .transpose()
    }

    /// Whether a meeting is visible at all (any note visible, or no notes) — gates the transcript
    /// in MCP `get_meeting` so a sealed meeting's transcript is not leaked either.
    pub fn meeting_is_visible(&self, meeting_id: &str, unlocked: &HashSet<String>) -> Result<bool> {
        let conn = self.lock();
        let meeting_visible = meeting_visible_sql(unlocked);
        let sql = format!("SELECT EXISTS(SELECT 1 FROM meetings m WHERE m.id=?1 AND {meeting_visible})");
        conn.query_row(&sql, rusqlite::params![meeting_id], |r| {
            Ok(r.get::<_, i64>(0)? != 0)
        })
        .map_err(map_err)
    }
}
