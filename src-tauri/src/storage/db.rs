use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, Row};

use crate::error::{AppError, Result};
use crate::storage::models::{
    Analytics, DayCount, Meeting, MeetingStatus, NoteRecord, SearchHit, StatusCount,
};
use crate::transcribe::types::Segment;

impl MeetingStatus {
    /// Stable SCREAMING_SNAKE_CASE string used as the on-disk `status` column value.
    /// Kept in sync with the serde `rename_all = "SCREAMING_SNAKE_CASE"` on the enum.
    fn as_str(&self) -> &'static str {
        match self {
            MeetingStatus::Draft => "DRAFT",
            MeetingStatus::Recording => "RECORDING",
            MeetingStatus::Transcribed => "TRANSCRIBED",
            MeetingStatus::Summarized => "SUMMARIZED",
            MeetingStatus::Exported => "EXPORTED",
            MeetingStatus::Error => "ERROR",
        }
    }
}

impl FromStr for MeetingStatus {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "DRAFT" => Ok(MeetingStatus::Draft),
            "RECORDING" => Ok(MeetingStatus::Recording),
            "TRANSCRIBED" => Ok(MeetingStatus::Transcribed),
            "SUMMARIZED" => Ok(MeetingStatus::Summarized),
            "EXPORTED" => Ok(MeetingStatus::Exported),
            "ERROR" => Ok(MeetingStatus::Error),
            other => Err(AppError::Storage(format!("unknown meeting status: {other}"))),
        }
    }
}

/// Map any rusqlite failure to a storage-domain AppError without leaking PII.
fn map_err(e: rusqlite::Error) -> AppError {
    AppError::Storage(e.to_string())
}

/// Thread-safe SQLite wrapper (internal Mutex<rusqlite::Connection>).
pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    /// Opens + runs migrations.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).map_err(map_err)?;
        // Enforce FK cascades (segments/notes → meetings) and use WAL for
        // concurrent reads while a write is in progress.
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;",
        )
        .map_err(map_err)?;
        let db = Db {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        Ok(db)
    }

    /// Idempotent CREATE TABLE IF NOT EXISTS migrations.
    pub fn migrate(&self) -> Result<()> {
        let conn = self.lock();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meetings (
               id TEXT PRIMARY KEY,
               started_at TEXT NOT NULL,
               ended_at TEXT,
               title TEXT,
               duration_s INTEGER NOT NULL DEFAULT 0,
               audio_path TEXT,
               status TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS segments (
               meeting_id TEXT NOT NULL,
               idx INTEGER NOT NULL,
               start_s REAL NOT NULL,
               end_s REAL NOT NULL,
               text TEXT NOT NULL,
               PRIMARY KEY (meeting_id, idx),
               FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS notes (
               meeting_id TEXT NOT NULL,
               provider_id TEXT NOT NULL,
               markdown TEXT NOT NULL,
               created_at TEXT NOT NULL,
               exported_path TEXT,
               PRIMARY KEY (meeting_id, provider_id),
               FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS settings (
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS timelines (
               meeting_id TEXT PRIMARY KEY,
               data TEXT NOT NULL,
               FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS meeting_tags (
               meeting_id TEXT NOT NULL,
               tag TEXT NOT NULL,
               PRIMARY KEY (meeting_id, tag),
               FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
             );",
        )
        .map_err(map_err)?;
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        // A poisoned lock means a prior writer panicked mid-statement; recover the
        // guard so the DB stays usable rather than cascading the panic.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ── meetings ────────────────────────────────────────────────────────────

    pub fn insert_meeting(&self, m: &Meeting) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO meetings
               (id, started_at, ended_at, title, duration_s, audio_path, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                m.id,
                m.started_at,
                m.ended_at,
                m.title,
                m.duration_s,
                m.audio_path,
                m.status.as_str(),
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
        let conn = self.lock();
        conn.execute(
            "UPDATE meetings SET title = ?2 WHERE id = ?1",
            rusqlite::params![id, title],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Delete a meeting and (via ON DELETE CASCADE) its segments, notes, and timeline.
    /// Audio + vault files are removed by the caller before this.
    pub fn delete_meeting(&self, id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM meetings WHERE id = ?1", rusqlite::params![id])
            .map_err(map_err)?;
        Ok(())
    }

    pub fn get_meeting(&self, id: &str) -> Result<Option<Meeting>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, started_at, ended_at, title, duration_s, audio_path, status
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
            "SELECT id, started_at, ended_at, title, duration_s, audio_path, status
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
                "SELECT id, started_at, ended_at, title, duration_s, audio_path, status
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

    /// Search meeting titles, transcript segments, and note markdown for `query`. Returns
    /// newest-first hits, each with a short snippet around the match. Case-insensitive.
    pub fn search(&self, query: &str, limit: i64) -> Result<Vec<SearchHit>> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let like = format!("%{}%", escape_like(q));
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT m.id, m.started_at, m.ended_at, m.title, m.duration_s, \
                        m.audio_path, m.status
                   FROM meetings m
                   LEFT JOIN segments s ON s.meeting_id = m.id
                   LEFT JOIN notes n ON n.meeting_id = m.id
                  WHERE m.title LIKE ?1 ESCAPE '\\'
                     OR s.text LIKE ?1 ESCAPE '\\'
                     OR n.markdown LIKE ?1 ESCAPE '\\'
                  ORDER BY m.started_at DESC, m.id DESC
                  LIMIT ?2",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![like, limit], row_to_meeting)
            .map_err(map_err)?;
        let mut meetings = Vec::new();
        for r in rows {
            meetings.push(r.map_err(map_err)??);
        }

        let mut hits = Vec::with_capacity(meetings.len());
        for m in meetings {
            let (snippet, matched_in) = search_snippet(&conn, &m, q, &like)?;
            hits.push(SearchHit {
                meeting: m,
                snippet,
                matched_in,
            });
        }
        Ok(hits)
    }

    // ── segments ────────────────────────────────────────────────────────────

    pub fn insert_segments(&self, meeting_id: &str, segments: &[Segment]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT OR REPLACE INTO segments
                       (meeting_id, idx, start_s, end_s, text)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(map_err)?;
            for seg in segments {
                stmt.execute(rusqlite::params![
                    meeting_id,
                    seg.idx,
                    seg.start_s,
                    seg.end_s,
                    seg.text,
                ])
                .map_err(map_err)?;
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// All segments for a meeting, ordered by `idx`.
    pub fn get_segments(&self, meeting_id: &str) -> Result<Vec<Segment>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT idx, start_s, end_s, text
                   FROM segments WHERE meeting_id = ?1 ORDER BY idx",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![meeting_id], |row| {
                Ok(Segment {
                    idx: row.get(0)?,
                    start_s: row.get(1)?,
                    end_s: row.get(2)?,
                    text: row.get(3)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    // ── notes ───────────────────────────────────────────────────────────────

    pub fn upsert_note(&self, note: &NoteRecord) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO notes
               (meeting_id, provider_id, markdown, created_at, exported_path)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(meeting_id, provider_id) DO UPDATE SET
               markdown = excluded.markdown,
               created_at = excluded.created_at,
               exported_path = excluded.exported_path",
            rusqlite::params![
                note.meeting_id,
                note.provider_id,
                note.markdown,
                note.created_at,
                note.exported_path,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub fn get_note(&self, meeting_id: &str, provider_id: &str) -> Result<Option<NoteRecord>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT meeting_id, provider_id, markdown, created_at, exported_path
               FROM notes WHERE meeting_id = ?1 AND provider_id = ?2",
            rusqlite::params![meeting_id, provider_id],
            row_to_note,
        )
        .optional()
        .map_err(map_err)
    }

    pub fn latest_note(&self) -> Result<Option<NoteRecord>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT meeting_id, provider_id, markdown, created_at, exported_path
               FROM notes ORDER BY created_at DESC LIMIT 1",
            [],
            row_to_note,
        )
        .optional()
        .map_err(map_err)
    }

    /// The most recent note for a meeting across providers (Detail view).
    pub fn get_latest_note_for_meeting(&self, meeting_id: &str) -> Result<Option<NoteRecord>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT meeting_id, provider_id, markdown, created_at, exported_path
               FROM notes WHERE meeting_id = ?1 ORDER BY created_at DESC LIMIT 1",
            rusqlite::params![meeting_id],
            row_to_note,
        )
        .optional()
        .map_err(map_err)
    }

    pub fn set_note_exported_path(
        &self,
        meeting_id: &str,
        provider_id: &str,
        path: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE notes SET exported_path = ?3
             WHERE meeting_id = ?1 AND provider_id = ?2",
            rusqlite::params![meeting_id, provider_id, path],
        )
        .map_err(map_err)?;
        Ok(())
    }

    // ── timelines (AI-derived speaker + topic spans; JSON blob per meeting) ──────

    pub fn get_timeline_data(&self, meeting_id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT data FROM timelines WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(map_err)
    }

    pub fn set_timeline_data(&self, meeting_id: &str, data: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO timelines (meeting_id, data) VALUES (?1, ?2)
             ON CONFLICT(meeting_id) DO UPDATE SET data = excluded.data",
            rusqlite::params![meeting_id, data],
        )
        .map_err(map_err)?;
        Ok(())
    }

    // ── meeting tags ─────────────────────────────────────────────────────────

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
                        m.status
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

    // ── settings k/v table ───────────────────────────────────────────────────

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(map_err)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub fn all_settings(&self) -> Result<Vec<(String, String)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT key, value FROM settings ORDER BY key")
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    // ── analytics ──────────────────────────────────────────────────────────────

    /// Aggregate stats for the dashboard + Analytics tab.
    pub fn analytics(&self) -> Result<Analytics> {
        let conn = self.lock();
        let total_meetings: i64 = conn
            .query_row("SELECT COUNT(*) FROM meetings", [], |r| r.get(0))
            .map_err(map_err)?;
        let total_duration_s: i64 = conn
            .query_row("SELECT COALESCE(SUM(duration_s), 0) FROM meetings", [], |r| {
                r.get(0)
            })
            .map_err(map_err)?;
        let longest_duration_s: i64 = conn
            .query_row("SELECT COALESCE(MAX(duration_s), 0) FROM meetings", [], |r| {
                r.get(0)
            })
            .map_err(map_err)?;
        let notes_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))
            .map_err(map_err)?;
        let first_meeting_at: Option<String> = conn
            .query_row("SELECT MIN(started_at) FROM meetings", [], |r| r.get(0))
            .map_err(map_err)?;
        let avg_duration_s = if total_meetings > 0 {
            total_duration_s / total_meetings
        } else {
            0
        };

        // RFC3339 UTC timestamps sort lexicographically = chronologically, so comparing
        // against a computed cutoff string is safe (avoids SQLite ISO-parsing quirks).
        let cutoff_7d = (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339();
        let meetings_7d: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM meetings WHERE started_at >= ?1",
                rusqlite::params![cutoff_7d],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        let duration_7d_s: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(duration_s), 0) FROM meetings WHERE started_at >= ?1",
                rusqlite::params![cutoff_7d],
                |r| r.get(0),
            )
            .map_err(map_err)?;

        let by_status = {
            let mut stmt = conn
                .prepare("SELECT status, COUNT(*) FROM meetings GROUP BY status")
                .map_err(map_err)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(StatusCount {
                        status: row.get(0)?,
                        count: row.get(1)?,
                    })
                })
                .map_err(map_err)?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r.map_err(map_err)?);
            }
            v
        };

        let cutoff_30d = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let per_day = {
            let mut stmt = conn
                .prepare(
                    "SELECT substr(started_at, 1, 10) AS d, COUNT(*), COALESCE(SUM(duration_s), 0)
                       FROM meetings WHERE started_at >= ?1 GROUP BY d ORDER BY d",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![cutoff_30d], |row| {
                    Ok(DayCount {
                        date: row.get(0)?,
                        count: row.get(1)?,
                        duration_s: row.get(2)?,
                    })
                })
                .map_err(map_err)?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r.map_err(map_err)?);
            }
            v
        };

        Ok(Analytics {
            total_meetings,
            total_duration_s,
            avg_duration_s,
            longest_duration_s,
            meetings_7d,
            duration_7d_s,
            notes_count,
            first_meeting_at,
            by_status,
            per_day,
        })
    }
}

// ── row mappers ──────────────────────────────────────────────────────────────
// Return `Result<Meeting>` (not raw rusqlite::Result) so the status-parse error
// is surfaced as an AppError; `query_row(...).optional()?.transpose()` flattens it.

fn row_to_meeting(row: &Row<'_>) -> rusqlite::Result<Result<Meeting>> {
    // Read every column as a rusqlite result first (so `?` here yields rusqlite::Error),
    // then fold the status-string parse (which yields AppError) into the inner Result.
    let id: String = row.get(0)?;
    let started_at: String = row.get(1)?;
    let ended_at: Option<String> = row.get(2)?;
    let title: Option<String> = row.get(3)?;
    let duration_s: i64 = row.get(4)?;
    let audio_path: Option<String> = row.get(5)?;
    let status_str: String = row.get(6)?;

    Ok(MeetingStatus::from_str(&status_str).map(|status| Meeting {
        id,
        started_at,
        ended_at,
        title,
        duration_s,
        audio_path,
        status,
    }))
}

/// Escape LIKE wildcards so user input is matched literally (paired with `ESCAPE '\'`).
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// Build a `(snippet, matched_in)` pair for a search hit, reusing the open connection.
fn search_snippet(
    conn: &Connection,
    m: &Meeting,
    q: &str,
    like: &str,
) -> Result<(String, String)> {
    let ql = q.to_lowercase();
    if let Some(t) = &m.title {
        if t.to_lowercase().contains(&ql) {
            return Ok((t.clone(), "title".to_string()));
        }
    }
    let seg: Option<String> = conn
        .query_row(
            "SELECT text FROM segments WHERE meeting_id = ?1 AND text LIKE ?2 ESCAPE '\\' \
             ORDER BY idx LIMIT 1",
            rusqlite::params![m.id, like],
            |r| r.get(0),
        )
        .optional()
        .map_err(map_err)?;
    if let Some(text) = seg {
        return Ok((excerpt(&text, q), "transcript".to_string()));
    }
    let note: Option<String> = conn
        .query_row(
            "SELECT markdown FROM notes WHERE meeting_id = ?1 AND markdown LIKE ?2 ESCAPE '\\' \
             LIMIT 1",
            rusqlite::params![m.id, like],
            |r| r.get(0),
        )
        .optional()
        .map_err(map_err)?;
    if let Some(md) = note {
        return Ok((excerpt(&md, q), "note".to_string()));
    }
    Ok((m.title.clone().unwrap_or_default(), "title".to_string()))
}

/// A ~130-char snippet around the first case-insensitive match of `q` in `text`,
/// whitespace-collapsed, with ellipses. Char-boundary safe for Unicode.
fn excerpt(text: &str, q: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let pos = flat
        .to_lowercase()
        .find(&q.to_lowercase())
        .unwrap_or(0)
        .min(flat.len());
    let mut start = pos.saturating_sub(40);
    while start > 0 && !flat.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (pos + q.len() + 90).min(flat.len());
    while end < flat.len() && !flat.is_char_boundary(end) {
        end += 1;
    }
    let body = flat.get(start..end).unwrap_or(&flat).trim();
    let mut s = String::new();
    if start > 0 {
        s.push('…');
    }
    s.push_str(body);
    if end < flat.len() {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod search_helper_tests {
    use super::{escape_like, excerpt};

    #[test]
    fn escape_like_escapes_wildcards() {
        // '%' → '\%', '_' → '\_', '\' → '\\'
        assert_eq!(escape_like("a%b_c\\d"), "a\\%b\\_c\\\\d");
    }

    #[test]
    fn excerpt_centers_on_match_with_ellipses() {
        // Match sits deep in a long string → window is clipped on both sides.
        let text = format!("{}needle{}", "alpha ".repeat(20), " omega".repeat(20));
        let e = excerpt(&text, "needle");
        assert!(e.contains("needle"));
        assert!(e.starts_with('…'), "leading ellipsis, got: {e}");
        assert!(e.ends_with('…'), "trailing ellipsis, got: {e}");
    }

    #[test]
    fn excerpt_handles_unicode_safely() {
        let text = "zażółć gęślą jaźń ąśćńółżź omówiliśmy budżet i planowanie na przyszły kwartał szczegółowo";
        let e = excerpt(text, "budżet");
        assert!(e.contains("budżet"));
    }
}

fn row_to_note(row: &Row<'_>) -> rusqlite::Result<NoteRecord> {
    Ok(NoteRecord {
        meeting_id: row.get(0)?,
        provider_id: row.get(1)?,
        markdown: row.get(2)?,
        created_at: row.get(3)?,
        exported_path: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{Meeting, MeetingStatus, NoteRecord};

    fn mem_db() -> Db {
        // In-memory DB shares the same open/migrate path as on-disk.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        let db = Db {
            conn: Mutex::new(conn),
        };
        db.migrate().unwrap();
        db
    }

    fn sample_meeting(id: &str, started_at: &str) -> Meeting {
        Meeting {
            id: id.to_string(),
            started_at: started_at.to_string(),
            ended_at: None,
            title: None,
            duration_s: 0,
            audio_path: None,
            status: MeetingStatus::Draft,
        }
    }

    #[test]
    fn migrate_is_idempotent() {
        let db = mem_db();
        db.migrate().unwrap();
        db.migrate().unwrap();
    }

    #[test]
    fn meeting_round_trip_and_lifecycle() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();

        let got = db.get_meeting("m1").unwrap().unwrap();
        assert_eq!(got.id, "m1");
        assert_eq!(got.status, MeetingStatus::Draft);
        assert_eq!(got.duration_s, 0);

        db.update_meeting_status("m1", MeetingStatus::Recording)
            .unwrap();
        assert_eq!(
            db.get_meeting("m1").unwrap().unwrap().status,
            MeetingStatus::Recording
        );

        db.set_meeting_title("m1", "Standup").unwrap();
        db.finalize_meeting("m1", "2026-06-24T10:30:00Z", 1800, "/tmp/m1.wav")
            .unwrap();

        let fin = db.get_meeting("m1").unwrap().unwrap();
        assert_eq!(fin.title.as_deref(), Some("Standup"));
        assert_eq!(fin.ended_at.as_deref(), Some("2026-06-24T10:30:00Z"));
        assert_eq!(fin.duration_s, 1800);
        assert_eq!(fin.audio_path.as_deref(), Some("/tmp/m1.wav"));

        assert!(db.get_meeting("nope").unwrap().is_none());
    }

    #[test]
    fn latest_meeting_orders_by_started_at() {
        let db = mem_db();
        assert!(db.latest_meeting().unwrap().is_none());
        db.insert_meeting(&sample_meeting("old", "2026-06-23T09:00:00Z"))
            .unwrap();
        db.insert_meeting(&sample_meeting("new", "2026-06-24T09:00:00Z"))
            .unwrap();
        assert_eq!(db.latest_meeting().unwrap().unwrap().id, "new");
    }

    #[test]
    fn segments_replace_and_cascade() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        let segs = vec![
            Segment {
                idx: 0,
                start_s: 0.0,
                end_s: 1.5,
                text: "hello".into(),
            },
            Segment {
                idx: 1,
                start_s: 1.5,
                end_s: 3.0,
                text: "world".into(),
            },
        ];
        db.insert_segments("m1", &segs).unwrap();
        // re-insert (same PKs) must not error thanks to INSERT OR REPLACE
        db.insert_segments("m1", &segs).unwrap();

        let conn = db.lock();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM segments WHERE meeting_id = 'm1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
        drop(conn);

        // deleting the meeting cascades to its segments
        db.lock()
            .execute("DELETE FROM meetings WHERE id = 'm1'", [])
            .unwrap();
        let conn = db.lock();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM segments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn note_upsert_get_and_export_path() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();

        let mut note = NoteRecord {
            meeting_id: "m1".into(),
            provider_id: "claude_code".into(),
            markdown: "# v1".into(),
            created_at: "2026-06-24T10:31:00Z".into(),
            exported_path: None,
        };
        db.upsert_note(&note).unwrap();

        // upsert overwrites the same (meeting_id, provider_id)
        note.markdown = "# v2".into();
        db.upsert_note(&note).unwrap();
        let got = db.get_note("m1", "claude_code").unwrap().unwrap();
        assert_eq!(got.markdown, "# v2");
        assert!(got.exported_path.is_none());

        db.set_note_exported_path("m1", "claude_code", "/vault/m1.md")
            .unwrap();
        let got = db.get_note("m1", "claude_code").unwrap().unwrap();
        assert_eq!(got.exported_path.as_deref(), Some("/vault/m1.md"));

        assert!(db.get_note("m1", "ollama").unwrap().is_none());
        assert_eq!(db.latest_note().unwrap().unwrap().markdown, "# v2");
    }

    #[test]
    fn settings_kv_round_trip() {
        let db = mem_db();
        assert!(db.get_setting("vault_path").unwrap().is_none());
        db.set_setting("vault_path", "/vault").unwrap();
        db.set_setting("provider_id", "claude_code").unwrap();
        // overwrite
        db.set_setting("vault_path", "/vault2").unwrap();
        assert_eq!(db.get_setting("vault_path").unwrap().as_deref(), Some("/vault2"));

        let all = db.all_settings().unwrap();
        assert_eq!(
            all,
            vec![
                ("provider_id".to_string(), "claude_code".to_string()),
                ("vault_path".to_string(), "/vault2".to_string()),
            ]
        );
    }
}
