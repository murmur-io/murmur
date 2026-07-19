//! Agentic-Ask / assistant-thread storage surface — the `assistant_interactions` table reads +
//! the one insert, extracted verbatim from `storage::db` (God-file split, a PURE MOVE — no
//! behavior change). The methods below are an inherent-impl split of [`crate::storage::db::Db`]
//! across files (Rust allows one type's inherent `impl` to live in multiple files of the same
//! crate); every method retains its EXACT prior body, signature, AND gating. The two readers are
//! VISIBILITY-GATED: they early-return an EMPTY list for a sealed-and-not-session-unlocked meeting
//! via `self.meeting_is_visible(meeting_id, unlocked)?` (the `meeting_is_visible` gate helper, its
//! backing `visibility_clause`, and the seal-purge `purge_assistant_interactions_tx` all STAY in
//! db.rs beside the visibility/seal machinery — this move relocated ONLY these three methods, not
//! the gate). `insert_assistant_interaction` is purely additive (never touches sealed content).
//! Shared db.rs helpers `map_err` + `lock` are `pub(crate)`; `meeting_is_visible` is a `pub` Db
//! method still resolved cross-file via `self.`. Tests stay in db.rs's `mod tests` (shared
//! harness); the count is conserved.

use std::collections::HashSet;

use crate::error::{AppError, Result};
use crate::storage::db::{map_err, Db};
use crate::storage::models::{AssistantInteraction, AssistantThreadRow};

impl Db {
    /// Persist one in-meeting voice-assistant interaction (the Q&A) against `meeting_id`. PURELY
    /// ADDITIVE: it never touches note/transcript/timeline plaintext or blobs, so it can never blank
    /// or clobber sealed content. `citations` is stored as a JSON array string. The CALLER is the
    /// off-thread voice dispatch, which is best-effort + panic-free — a persist failure there is
    /// logged (non-PII) and never disrupts recording/dispatch. Derived convenience data: it is PURGED
    /// (not sealed) when the meeting's folder is sealed (see `purge_assistant_interactions_tx`).
    #[allow(clippy::too_many_arguments)]
    pub fn insert_assistant_interaction(
        &self,
        meeting_id: &str,
        command: &str,
        answer: &str,
        citations: &[String],
        status: &str,
        source_label: Option<&str>,
        thread_id: Option<&str>,
        anchor_text: Option<&str>,
        created_at: &str,
    ) -> Result<i64> {
        let citations_json = serde_json::to_string(citations)
            .map_err(|e| AppError::Storage(format!("serialize interaction citations: {e}")))?;
        let conn = self.lock();
        conn.execute(
            "INSERT INTO assistant_interactions
                (meeting_id, command, answer, citations, status, source_label,
                 thread_id, anchor_text, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                meeting_id,
                command,
                answer,
                citations_json,
                status,
                source_label,
                thread_id,
                anchor_text,
                created_at
            ],
        )
        .map_err(map_err)?;
        Ok(conn.last_insert_rowid())
    }

    /// Read every persisted assistant interaction for `meeting_id`, oldest first, ONLY when the
    /// meeting is VISIBLE to the session `unlocked` set (a sealed-and-not-session-unlocked meeting
    /// returns an EMPTY list — never its rows). This is the gated read the detail DTO uses; it mirrors
    /// the `meeting_is_visible` predicate so the interaction log is gated exactly like the note /
    /// segments / timeline. Note: on seal the rows are PURGED (see `purge_assistant_interactions_tx`),
    /// so a sealed meeting has no rows to read anyway — the gate is defense-in-depth.
    pub fn list_assistant_interactions_visible(
        &self,
        meeting_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<AssistantInteraction>> {
        if !self.meeting_is_visible(meeting_id, unlocked)? {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT command, answer, citations, status, source_label, created_at
                   FROM assistant_interactions
                  WHERE meeting_id = ?1 ORDER BY id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![meeting_id], |r| {
                let citations_json: String = r.get(2)?;
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    citations_json,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            let (command, answer, citations_json, status, source_label, created_at) =
                r.map_err(map_err)?;
            // A malformed citations blob must never break the read — fall back to an empty list.
            let citations: Vec<String> = serde_json::from_str(&citations_json).unwrap_or_default();
            out.push(AssistantInteraction {
                command,
                answer,
                citations,
                status,
                source_label,
                created_at,
            });
        }
        Ok(out)
    }

    /// Read the persisted @brain THREAD exchanges for `meeting_id` — ONLY rows that carry a
    /// `thread_id` (legacy voice rows with NULL are EXCLUDED), oldest first — and ONLY when the
    /// meeting is VISIBLE to the session `unlocked` set. A sealed-and-not-session-unlocked meeting
    /// returns an EMPTY list, never an error (existence must not leak) — the same gate as
    /// `list_assistant_interactions_visible`. On seal the rows are PURGED anyway
    /// (`purge_assistant_interactions_tx`), so the gate is defense-in-depth.
    pub fn list_assistant_threads_visible(
        &self,
        meeting_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<AssistantThreadRow>> {
        if !self.meeting_is_visible(meeting_id, unlocked)? {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT thread_id, anchor_text, command, answer, citations, status, created_at
                   FROM assistant_interactions
                  WHERE meeting_id = ?1 AND thread_id IS NOT NULL
                  ORDER BY id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![meeting_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            let (thread_id, anchor_text, command, answer, citations_json, status, created_at) =
                r.map_err(map_err)?;
            // A malformed citations blob must never break the read — fall back to an empty list.
            let citations: Vec<String> = serde_json::from_str(&citations_json).unwrap_or_default();
            out.push(AssistantThreadRow {
                thread_id,
                anchor_text,
                command,
                answer,
                citations,
                status,
                created_at,
            });
        }
        Ok(out)
    }
}
