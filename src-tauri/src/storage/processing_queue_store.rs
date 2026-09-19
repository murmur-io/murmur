//! Deferred-processing QUEUE storage (T5 meeting surface, 2026-09-19).
//!
//! When the user stops a recording and chooses "process later", the recording's audio is already
//! on disk and verified; what has to survive an app restart is the *scheduling* decision. This
//! table is the canonical home of that decision — the UI (`/queue`) and the worker are thin
//! readers of it, never a second copy of truth.
//!
//! ## What this table deliberately does NOT hold
//! * no transcript, note or title text — a row is `(meeting_id, state, position, stage, attempts)`;
//! * no audio/master/archive PATHS — those stay owned by the pipeline's own recording rows;
//! * no raw provider error strings — only a stable, non-sensitive `error_code`.
//!
//! That is what keeps the queue outside the lock model's content surface: a row about a sealed
//! meeting leaks nothing on its own, and the command layer still masks its title and refuses to
//! run it while locked (see `.claude/rules/lock-model.md`).
//!
//! ## Ordering
//! `position` is an explicit dense non-negative integer, not insertion rowid, because "process
//! this one now" and bulk reorder have to be expressible without rewriting history.
//!
//! ## Crash safety
//! Every mutation runs in one `BEGIN IMMEDIATE` transaction and re-normalizes positions inside it,
//! so a kill between statements can never leave a hole or a duplicate position. A process that
//! dies mid-job leaves a `processing` row behind; [`Db::reconcile_processing_queue`] turns those
//! back into `queued` AT THEIR EXISTING POSITION at launch — it never auto-runs them, because
//! "later" means later until the user says otherwise.
//!
//! The methods below are an inherent-impl split of [`crate::storage::db::Db`] across files, the
//! same pattern the other `*_store.rs` modules use.

use rusqlite::{OptionalExtension, Row, TransactionBehavior};

use crate::error::{AppError, Result};
use crate::storage::db::{map_err, Db};
use crate::storage::models::{ProcessingQueueJob, ProcessingQueueState};

/// Hard ceiling on queued jobs. A queue is a short backlog of real recordings, not an unbounded
/// sink; refusing past this keeps both the list read and the reorder transaction bounded.
pub const MAX_QUEUE_JOBS: i64 = 500;

fn row_to_job(row: &Row<'_>) -> rusqlite::Result<ProcessingQueueJob> {
    Ok(ProcessingQueueJob {
        meeting_id: row.get("meeting_id")?,
        state: ProcessingQueueState::from_db(&row.get::<_, String>("state")?)
            .unwrap_or(ProcessingQueueState::Queued),
        position: row.get("position")?,
        stage: row.get("stage")?,
        attempts: row.get("attempts")?,
        error_code: row.get("error_code")?,
        enqueued_at: row.get("enqueued_at")?,
        updated_at: row.get("updated_at")?,
    })
}

const SELECT_JOB: &str = "SELECT meeting_id, state, position, stage, attempts, error_code, \
                          enqueued_at, updated_at FROM processing_queue";

impl Db {
    /// Append one meeting to the tail of the queue, or re-arm an existing row in place.
    ///
    /// Idempotent by `meeting_id`: enqueueing the same meeting twice does NOT create a second job
    /// and does NOT move it — a double-click must not reorder the user's backlog. A `failed` row
    /// re-armed this way returns to `queued` keeping its position and its attempt count (the
    /// attempts are history, not a reason to hide the job).
    ///
    /// # Errors
    /// Returns [`AppError::Storage`] if the queue is at [`MAX_QUEUE_JOBS`] or on any SQL failure.
    pub fn enqueue_processing_job(&self, meeting_id: &str, now: &str) -> Result<i64> {
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_err)?;
        let existing: Option<i64> = tx
            .query_row(
                "SELECT position FROM processing_queue WHERE meeting_id=?1",
                [meeting_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_err)?;
        let position = match existing {
            Some(position) => {
                tx.execute(
                    "UPDATE processing_queue
                        SET state='queued', error_code=NULL, stage=NULL, updated_at=?2
                      WHERE meeting_id=?1 AND state<>'processing'",
                    rusqlite::params![meeting_id, now],
                )
                .map_err(map_err)?;
                position
            }
            None => {
                let count: i64 = tx
                    .query_row("SELECT COUNT(*) FROM processing_queue", [], |row| {
                        row.get(0)
                    })
                    .map_err(map_err)?;
                if count >= MAX_QUEUE_JOBS {
                    return Err(AppError::Storage(format!(
                        "processing queue is full ({MAX_QUEUE_JOBS} jobs)"
                    )));
                }
                let position: i64 = tx
                    .query_row(
                        "SELECT COALESCE(MAX(position)+1, 0) FROM processing_queue",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(map_err)?;
                tx.execute(
                    "INSERT INTO processing_queue
                       (meeting_id, state, position, stage, attempts, error_code, enqueued_at, updated_at)
                     VALUES (?1, 'queued', ?2, NULL, 0, NULL, ?3, ?3)",
                    rusqlite::params![meeting_id, position, now],
                )
                .map_err(map_err)?;
                position
            }
        };
        tx.execute("UPDATE meetings SET status='QUEUED' WHERE id=?1 AND EXISTS (SELECT 1 FROM processing_queue WHERE meeting_id=?1 AND state='queued')", [meeting_id]).map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(position)
    }

    /// Every job in user-visible order (position, then enqueue time, then id for determinism).
    pub fn list_processing_queue(&self) -> Result<Vec<ProcessingQueueJob>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!(
                "{SELECT_JOB} ORDER BY position ASC, enqueued_at ASC, meeting_id ASC"
            ))
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], row_to_job)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// One job by id, or `None` when it is not queued at all.
    pub fn processing_job(&self, meeting_id: &str) -> Result<Option<ProcessingQueueJob>> {
        let conn = self.lock();
        conn.query_row(
            &format!("{SELECT_JOB} WHERE meeting_id=?1"),
            [meeting_id],
            row_to_job,
        )
        .optional()
        .map_err(map_err)
    }

    /// Atomically take the head of the queue for this process.
    ///
    /// The whole read-and-mark happens inside one `BEGIN IMMEDIATE`, so two callers can never
    /// claim the same job: the loser sees the row already in `processing` and skips it. Bumps
    /// `attempts` because a claim that later dies IS an attempt — otherwise a crash loop would
    /// look free forever.
    pub fn claim_next_processing_job(&self, now: &str) -> Result<Option<ProcessingQueueJob>> {
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_err)?;
        let already_running: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM processing_queue WHERE state='processing'",
                [],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        if already_running > 0 {
            return Ok(None);
        }
        let next: Option<String> = tx
            .query_row(
                "SELECT meeting_id FROM processing_queue WHERE state='queued'
                  ORDER BY position ASC, enqueued_at ASC, meeting_id ASC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_err)?;
        let Some(meeting_id) = next else {
            return Ok(None);
        };
        tx.execute(
            "UPDATE processing_queue
                SET state='processing', attempts=attempts+1, error_code=NULL, stage=NULL, updated_at=?2
              WHERE meeting_id=?1",
            rusqlite::params![&meeting_id, now],
        )
        .map_err(map_err)?;
        let job = tx
            .query_row(
                &format!("{SELECT_JOB} WHERE meeting_id=?1"),
                [&meeting_id],
                row_to_job,
            )
            .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(Some(job))
    }

    /// Claim exactly one user-selected job; never drain unrelated deferred work.
    pub fn claim_processing_job(&self, meeting_id: &str, now: &str) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_err)?;
        let changed = tx.execute("UPDATE processing_queue SET state='processing', attempts=attempts+1, error_code=NULL, stage=NULL, updated_at=?2 WHERE meeting_id=?1 AND state='queued' AND NOT EXISTS (SELECT 1 FROM processing_queue WHERE state='processing')", rusqlite::params![meeting_id, now]).map_err(map_err)?;
        if changed > 0 {
            tx.execute(
                "UPDATE meetings SET status='RECORDING' WHERE id=?1",
                [meeting_id],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(changed > 0)
    }

    /// Yield a claimed job to foreground recording without changing its ordering or attempt count.
    pub fn park_processing_job(&self, meeting_id: &str, now: &str) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_err)?;
        let changed = tx.execute("UPDATE processing_queue SET state='queued', stage=NULL, error_code=NULL, updated_at=?2 WHERE meeting_id=?1 AND state='processing'", rusqlite::params![meeting_id, now]).map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "cannot park an unclaimed queue job".into(),
            ));
        }
        tx.execute(
            "UPDATE meetings SET status='QUEUED' WHERE id=?1",
            [meeting_id],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)
    }

    /// Retry the full user selection atomically; a crash never rearms only half a bulk request.
    pub fn retry_processing_jobs(&self, ids: &[String], now: &str) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_err)?;
        Self::validate_ids(&tx, ids)?;
        for id in ids {
            tx.execute("UPDATE processing_queue SET state='queued', error_code=NULL, stage=NULL, updated_at=?2 WHERE meeting_id=?1 AND state='failed'", rusqlite::params![id, now]).map_err(map_err)?;
            tx.execute("UPDATE meetings SET status='QUEUED' WHERE id=?1 AND EXISTS (SELECT 1 FROM processing_queue WHERE meeting_id=?1 AND state='queued')", [id]).map_err(map_err)?;
        }
        tx.commit().map_err(map_err)
    }

    /// Remove scheduling metadata and leave all recordings/audio retryable in one transaction.
    pub fn remove_processing_jobs(&self, ids: &[String]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_err)?;
        Self::validate_ids(&tx, ids)?;
        for id in ids {
            let state: String = tx
                .query_row(
                    "SELECT state FROM processing_queue WHERE meeting_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .map_err(map_err)?;
            if state == "processing" {
                return Err(AppError::InvalidArg(
                    "cannot remove an active queue job".into(),
                ));
            }
            tx.execute("UPDATE meetings SET status='ERROR' WHERE id=?1", [id])
                .map_err(map_err)?;
            tx.execute("DELETE FROM processing_queue WHERE meeting_id=?1", [id])
                .map_err(map_err)?;
        }
        Self::normalize_queue_positions(&tx)?;
        tx.commit().map_err(map_err)
    }

    /// Record honest, content-free progress for a running job (e.g. `"transcribing"`).
    pub fn set_processing_job_stage(&self, meeting_id: &str, stage: &str, now: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE processing_queue SET stage=?2, updated_at=?3
              WHERE meeting_id=?1 AND state='processing'",
            rusqlite::params![meeting_id, stage, now],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Park a job as `failed` with a STABLE error code (never a raw provider message).
    pub fn fail_processing_job(&self, meeting_id: &str, error_code: &str, now: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE processing_queue SET state='failed', error_code=?2, updated_at=?3
              WHERE meeting_id=?1",
            rusqlite::params![meeting_id, error_code, now],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Move a `failed` job back to `queued` at its existing position, clearing the error code.
    /// Returns `false` when there was nothing to retry.
    pub fn retry_processing_job(&self, meeting_id: &str, now: &str) -> Result<bool> {
        let conn = self.lock();
        let changed = conn
            .execute(
                "UPDATE processing_queue SET state='queued', error_code=NULL, stage=NULL, updated_at=?2
                  WHERE meeting_id=?1 AND state='failed'",
                rusqlite::params![meeting_id, now],
            )
            .map_err(map_err)?;
        Ok(changed > 0)
    }

    /// Drop a job and close the gap it leaves. Used both for a completed job and for the user's
    /// explicit "remove" — the meeting's AUDIO is untouched either way; this deletes scheduling
    /// metadata only.
    pub fn remove_processing_job(&self, meeting_id: &str) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_err)?;
        let removed = tx
            .execute(
                "DELETE FROM processing_queue WHERE meeting_id=?1",
                [meeting_id],
            )
            .map_err(map_err)?;
        if removed > 0 {
            Self::normalize_queue_positions(&tx)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(removed > 0)
    }

    /// Put the given jobs at the FRONT of the queue, preserving their relative order among
    /// themselves and the relative order of everything else behind them.
    ///
    /// # Errors
    /// [`AppError::Storage`] if an id is unknown or repeated — a "process now" that silently
    /// ignored half its selection would be worse than refusing.
    pub fn promote_processing_jobs(&self, meeting_ids: &[String], now: &str) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_err)?;
        Self::validate_ids(&tx, meeting_ids)?;
        // Build the whole new order in memory (promoted block first, everything else behind it in
        // its current order) and write it as a dense 0..n. Positions are CHECK-constrained to be
        // non-negative, so there is no sentinel trick to lean on — and no unique index on
        // `position`, so one forward pass cannot collide.
        let promoted: std::collections::HashSet<&str> =
            meeting_ids.iter().map(String::as_str).collect();
        let mut order: Vec<String> = meeting_ids.to_vec();
        order.extend(
            Self::ordered_queue_ids(&tx)?
                .into_iter()
                .filter(|id| !promoted.contains(id.as_str())),
        );
        for (position, meeting_id) in order.iter().enumerate() {
            tx.execute(
                "UPDATE processing_queue SET position=?2, updated_at=?3 WHERE meeting_id=?1",
                rusqlite::params![meeting_id, position as i64, now],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Rewrite the whole order from an explicit id list.
    ///
    /// # Errors
    /// [`AppError::Storage`] when the list is not exactly the current set of jobs (unknown id,
    /// duplicate, or a missing one) — a partial reorder has no defensible meaning.
    pub fn reorder_processing_queue(&self, meeting_ids: &[String], now: &str) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_err)?;
        Self::validate_ids(&tx, meeting_ids)?;
        let total: i64 = tx
            .query_row("SELECT COUNT(*) FROM processing_queue", [], |row| {
                row.get(0)
            })
            .map_err(map_err)?;
        if total != meeting_ids.len() as i64 {
            return Err(AppError::Storage(
                "reorder must list every queued job exactly once".into(),
            ));
        }
        for (position, meeting_id) in meeting_ids.iter().enumerate() {
            tx.execute(
                "UPDATE processing_queue SET position=?2, updated_at=?3 WHERE meeting_id=?1",
                rusqlite::params![meeting_id, position as i64, now],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Launch-time recovery. A `processing` row can only exist if a previous process died holding
    /// it (the single-instance guard means nobody else owns it now), so return it to `queued` AT
    /// ITS EXISTING POSITION and clear its stale stage.
    ///
    /// It deliberately does NOT start anything: "process later" stays manual across restarts, and
    /// silently resuming provider work after a crash would be an unrequested egress.
    ///
    /// Returns how many interrupted jobs were reclaimed.
    pub fn reconcile_processing_queue(&self, now: &str) -> Result<usize> {
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_err)?;
        // A retry can retain an earlier attempt's note, and the pipeline writes SUMMARIZED
        // before replacing that note. Only a stage emitted AFTER this attempt's note write
        // proves completion; every claim/retry clears stage so an old witness cannot survive.
        tx.execute("DELETE FROM processing_queue WHERE state='processing' AND stage IN ('saved', 'done') AND meeting_id IN (SELECT m.id FROM meetings m WHERE m.status IN ('SUMMARIZED', 'EXPORTED') AND EXISTS (SELECT 1 FROM notes n WHERE n.meeting_id=m.id))", []).map_err(map_err)?;
        tx.execute("UPDATE meetings SET status='QUEUED' WHERE id IN (SELECT meeting_id FROM processing_queue WHERE state='processing')", []).map_err(map_err)?;
        let reclaimed = tx
            .execute(
                "UPDATE processing_queue SET state='queued', stage=NULL, updated_at=?1
                  WHERE state='processing'",
                [now],
            )
            .map_err(map_err)?;
        Self::normalize_queue_positions(&tx)?;
        tx.commit().map_err(map_err)?;
        Ok(reclaimed)
    }

    /// Collapse positions to a dense `0..n` while preserving the current order. Called inside the
    /// caller's transaction so a crash can never expose a half-renumbered queue.
    fn normalize_queue_positions(tx: &rusqlite::Transaction<'_>) -> Result<()> {
        let ordered = Self::ordered_queue_ids(tx)?;
        for (position, meeting_id) in ordered.iter().enumerate() {
            tx.execute(
                "UPDATE processing_queue SET position=?2 WHERE meeting_id=?1 AND position<>?2",
                rusqlite::params![meeting_id, position as i64],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    /// Queue ids in current user-visible order, inside the caller's transaction.
    fn ordered_queue_ids(tx: &rusqlite::Transaction<'_>) -> Result<Vec<String>> {
        let mut stmt = tx
            .prepare(
                "SELECT meeting_id FROM processing_queue
                  ORDER BY position ASC, enqueued_at ASC, meeting_id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |row| row.get(0))
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// Every id must exist exactly once in the queue and appear at most once in the request.
    fn validate_ids(tx: &rusqlite::Transaction<'_>, meeting_ids: &[String]) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for meeting_id in meeting_ids {
            if !seen.insert(meeting_id.as_str()) {
                return Err(AppError::Storage(
                    "duplicate meeting id in queue request".into(),
                ));
            }
            let exists: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM processing_queue WHERE meeting_id=?1",
                    [meeting_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(map_err)?;
            if exists.is_none() {
                return Err(AppError::Storage("unknown queue job".into()));
            }
        }
        Ok(())
    }
}
