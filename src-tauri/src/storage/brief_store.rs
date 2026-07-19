//! Scheduled-brief storage surface — the `brief_schedules` + `brief_runs` table CRUD + their
//! idempotent schema, extracted verbatim from `storage::db` (God-file split, a PURE MOVE — no
//! behavior change). The methods below are an inherent-impl split of [`crate::storage::db::Db`]
//! across files (Rust allows one type's inherent `impl` to live in multiple files of the same
//! crate); every method retains its EXACT prior body and signature. These are config/staging rows
//! (schedules + proposed brief markdown), not gated meeting content read through the visibility
//! layer — the brief RUNNER assembles the corpus without a per-meeting gate (documented posture),
//! and the seal-coupled purge (`purge_pending_brief_runs_tx`) stays in db.rs beside the other seal
//! machinery. Shared db.rs module-level helper `map_err` is `pub(crate)` for the sibling access;
//! the schema fn `migrate_briefs` moved here and is `pub(crate)` so `migrate()` in db.rs still calls
//! `Self::migrate_briefs(&conn)` unchanged. The row mappers `row_to_brief_schedule` /
//! `row_to_brief_run` (only ever used by these readers) moved along with them.

use rusqlite::{Connection, OptionalExtension, Row};

use crate::error::{AppError, Result};
use crate::storage::db::{map_err, Db};

impl Db {
    /// Brain v2 L5 — idempotent SCHEDULED-BRIEF schema. `brief_schedules` = the user's structured
    /// local-time schedules (config data only); `brief_runs` = the propose-accept staging rows.
    /// Lock posture (documented, audited by the lock-security review): `brief_runs.note_md` is
    /// synthesized by `crate::brief_runner` from VISIBLE-ONLY content — the runner reads with the
    /// EMPTY unlock set (the consolidation-job discipline), so sealed content can never enter a
    /// brief AT synthesis time. But `note_md` IS derived meeting content (a cross-meeting
    /// synthesis — the memory-rollup class, one layer removed): a folder locked AFTER a brief was
    /// proposed would leave that paraphrase readable, so every seal path
    /// (`purge_chunks_for_meetings` / `blank_sealed_notes_in_folders` /
    /// `reblank_locked_folders_at_rest`) AND `delete_meeting` purges PENDING rows whose
    /// `meeting_ids` (a JSON id array) intersect the sealed/deleted meetings
    /// (`purge_pending_brief_runs_tx`). ACCEPTED rows are consumed on accept (`note_md` blanked —
    /// ids + timestamps only) and survive.
    pub(crate) fn migrate_briefs(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS brief_schedules (
               id TEXT PRIMARY KEY,
               label TEXT NOT NULL,
               day_of_week INTEGER,
               hour_local INTEGER NOT NULL,
               minute_local INTEGER NOT NULL,
               scope_days INTEGER NOT NULL DEFAULT 7,
               prompt_hint TEXT,
               enabled INTEGER NOT NULL DEFAULT 1,
               last_run_at TEXT,
               created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS brief_runs (
               id TEXT PRIMARY KEY,
               schedule_id TEXT NOT NULL,
               status TEXT NOT NULL DEFAULT 'pending',
               note_md TEXT NOT NULL DEFAULT '',
               meeting_ids TEXT NOT NULL DEFAULT '[]',
               proposed_at TEXT NOT NULL,
               accepted_at TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_brief_runs_schedule ON brief_runs(schedule_id);",
        )
        .map_err(map_err)
    }

    // ── Scheduled-brief config rows + staged proposal runs ───────────────────────────────────────

    /// All brief schedules (config rows — no meeting content).
    pub fn list_brief_schedules(&self) -> Result<Vec<crate::storage::models::BriefSchedule>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, label, day_of_week, hour_local, minute_local, scope_days, \
                        prompt_hint, enabled, last_run_at, created_at \
                   FROM brief_schedules ORDER BY created_at ASC, id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt.query_map([], row_to_brief_schedule).map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Insert one brief schedule (caller validates ranges — see `create_brief_schedule`).
    pub fn insert_brief_schedule(&self, s: &crate::storage::models::BriefSchedule) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO brief_schedules \
               (id, label, day_of_week, hour_local, minute_local, scope_days, prompt_hint, \
                enabled, last_run_at, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                s.id,
                s.label,
                s.day_of_week,
                s.hour_local,
                s.minute_local,
                s.scope_days,
                s.prompt_hint,
                s.enabled as i64,
                s.last_run_at,
                s.created_at,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Update one brief schedule's editable fields (label / timing / window / hint / enabled).
    /// `last_run_at` / `created_at` are runner/system-owned and never updated here.
    pub fn update_brief_schedule(&self, s: &crate::storage::models::BriefSchedule) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE brief_schedules SET label = ?2, day_of_week = ?3, hour_local = ?4, \
                    minute_local = ?5, scope_days = ?6, prompt_hint = ?7, enabled = ?8 \
              WHERE id = ?1",
            rusqlite::params![
                s.id,
                s.label,
                s.day_of_week,
                s.hour_local,
                s.minute_local,
                s.scope_days,
                s.prompt_hint,
                s.enabled as i64,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Delete one brief schedule AND its staged runs (a dangling pending run would render a card
    /// for a schedule that no longer exists).
    pub fn delete_brief_schedule(&self, id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM brief_runs WHERE schedule_id = ?1", [id])
            .map_err(map_err)?;
        conn.execute("DELETE FROM brief_schedules WHERE id = ?1", [id])
            .map_err(map_err)?;
        Ok(())
    }

    /// Stamp the once-per-local-day guard: `last_run_at` = the LOCAL date (`YYYY-MM-DD`).
    pub fn set_brief_schedule_last_run(&self, id: &str, local_date: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE brief_schedules SET last_run_at = ?2 WHERE id = ?1",
            rusqlite::params![id, local_date],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Insert one proposed brief run (status "pending"). `meeting_ids` serializes to a JSON array
    /// of opaque ids.
    pub fn insert_brief_run(&self, r: &crate::storage::models::BriefRun) -> Result<()> {
        let ids = serde_json::to_string(&r.meeting_ids)
            .map_err(|e| AppError::Storage(format!("meeting_ids serialize: {e}")))?;
        let conn = self.lock();
        conn.execute(
            "INSERT INTO brief_runs (id, schedule_id, status, note_md, meeting_ids, proposed_at, accepted_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                r.id,
                r.schedule_id,
                r.status,
                r.note_md,
                ids,
                r.proposed_at,
                r.accepted_at,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The PENDING (not yet accepted/dismissed) brief runs, newest first — the FE's proposal cards.
    pub fn list_pending_brief_runs(&self) -> Result<Vec<crate::storage::models::BriefRun>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, schedule_id, status, note_md, meeting_ids, proposed_at, accepted_at \
                   FROM brief_runs WHERE status = 'pending' ORDER BY proposed_at DESC, id DESC",
            )
            .map_err(map_err)?;
        let rows = stmt.query_map([], row_to_brief_run).map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// One brief run by id.
    pub fn get_brief_run(&self, id: &str) -> Result<Option<crate::storage::models::BriefRun>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, schedule_id, status, note_md, meeting_ids, proposed_at, accepted_at \
               FROM brief_runs WHERE id = ?1",
            [id],
            row_to_brief_run,
        )
        .optional()
        .map_err(map_err)
    }

    /// Mark a run ACCEPTED and CONSUME its markdown (the exported vault `.md` becomes the copy —
    /// the staging row keeps only ids + timestamps afterwards).
    pub fn accept_brief_run(&self, id: &str, accepted_at: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE brief_runs SET status = 'accepted', accepted_at = ?2, note_md = '' \
              WHERE id = ?1",
            rusqlite::params![id, accepted_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Dismiss = DELETE the staged run row (nothing is kept).
    pub fn delete_brief_run(&self, id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM brief_runs WHERE id = ?1", [id])
            .map_err(map_err)?;
        Ok(())
    }
}

/// Map a `brief_schedules` row (id, label, day_of_week, hour_local, minute_local, scope_days,
/// prompt_hint, enabled, last_run_at, created_at) to a [`crate::storage::models::BriefSchedule`].
fn row_to_brief_schedule(
    row: &Row<'_>,
) -> rusqlite::Result<crate::storage::models::BriefSchedule> {
    Ok(crate::storage::models::BriefSchedule {
        id: row.get(0)?,
        label: row.get(1)?,
        day_of_week: row.get(2)?,
        hour_local: row.get(3)?,
        minute_local: row.get(4)?,
        scope_days: row.get(5)?,
        prompt_hint: row.get(6)?,
        enabled: row.get::<_, i64>(7)? != 0,
        last_run_at: row.get(8)?,
        created_at: row.get(9)?,
    })
}

/// Map a `brief_runs` row (id, schedule_id, status, note_md, meeting_ids JSON, proposed_at,
/// accepted_at) to a [`crate::storage::models::BriefRun`]. A malformed `meeting_ids` JSON degrades
/// to an empty id list (ids are advisory provenance, never content).
fn row_to_brief_run(row: &Row<'_>) -> rusqlite::Result<crate::storage::models::BriefRun> {
    let ids_json: String = row.get(4)?;
    Ok(crate::storage::models::BriefRun {
        id: row.get(0)?,
        schedule_id: row.get(1)?,
        status: row.get(2)?,
        note_md: row.get(3)?,
        meeting_ids: serde_json::from_str(&ids_json).unwrap_or_default(),
        proposed_at: row.get(5)?,
        accepted_at: row.get(6)?,
    })
}
