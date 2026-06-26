use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, Row};

use crate::error::{AppError, Result};
use std::collections::HashSet;

use crate::storage::models::{
    Analytics, DayCount, Folder, Meeting, MeetingStatus, NoteRecord, RecipeRecord, SearchHit,
    StatusCount,
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
    /// Opens the encrypted DB (fetching the SQLCipher key from the Keychain) + runs migrations.
    /// This is the path used by the MCP server thread too — it transparently keys the handle.
    pub fn open(path: &Path) -> Result<Self> {
        let dek = crate::secrets::get_or_create_db_dek()?;
        Self::open_with_key(path, &dek)
    }

    /// Opens an (SQLCipher-encrypted) DB with an explicit raw-hex key. The `PRAGMA key` MUST be
    /// the first statement on the connection, before any other PRAGMA or query.
    pub fn open_with_key(path: &Path, dek_hex: &str) -> Result<Self> {
        let conn = Connection::open(path).map_err(map_err)?;
        // SQLCipher: key the connection FIRST (raw 32-byte key as a hex blob ⇒ no KDF).
        conn.pragma_update(None, "key", format!("x'{dek_hex}'"))
            .map_err(map_err)?;
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
             );
             CREATE TABLE IF NOT EXISTS saved_recipes (
               id TEXT PRIMARY KEY,
               title TEXT NOT NULL,
               prompt TEXT NOT NULL,
               created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS folders (
               id TEXT PRIMARY KEY,
               name TEXT NOT NULL,
               path TEXT NOT NULL UNIQUE,
               parent_id TEXT,
               locked INTEGER NOT NULL DEFAULT 0,
               wrapped_key BLOB,
               created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_folders_parent ON folders(parent_id);",
        )
        .map_err(map_err)?;
        // Guarded ALTERs — notes gain a folder association + a sealed-content blob (AES-GCM
        // markdown when the folder is locked; NULL when open). migrate() re-runs each launch and
        // `ALTER ADD COLUMN` errors if the column already exists, so check pragma_table_info first.
        Self::add_column_if_missing(&conn, "notes", "folder_id", "TEXT")?;
        Self::add_column_if_missing(&conn, "notes", "content_blob", "BLOB")?;
        Ok(())
    }

    /// Add `column` to `table` if it is not already present (idempotent migration guard).
    fn add_column_if_missing(
        conn: &Connection,
        table: &str,
        column: &str,
        decl: &str,
    ) -> Result<()> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(map_err)?;
        let exists = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .map_err(map_err)?
            .filter_map(|r| r.ok())
            .any(|c| c == column);
        drop(stmt);
        if !exists {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl};"))
                .map_err(map_err)?;
        }
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

    // ── saved recipes ────────────────────────────────────────────────────────

    pub fn list_saved_recipes(&self) -> Result<Vec<RecipeRecord>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, title, prompt, created_at FROM saved_recipes ORDER BY created_at DESC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(RecipeRecord {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    prompt: r.get(2)?,
                    created_at: r.get(3)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    pub fn insert_recipe(&self, r: &RecipeRecord) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO saved_recipes (id, title, prompt, created_at) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![r.id, r.title, r.prompt, r.created_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub fn delete_recipe(&self, id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM saved_recipes WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(map_err)?;
        Ok(())
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

    // ── folders ──────────────────────────────────────────────────────────────

    /// Insert a folder row. `path` is the vault-relative folder path (UNIQUE).
    pub fn insert_folder(&self, f: &Folder) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO folders (id, name, path, parent_id, locked, wrapped_key, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
            rusqlite::params![
                f.id,
                f.name,
                f.path,
                f.parent_id,
                f.locked as i64,
                f.created_at,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// All folders (creation order). The tree is assembled by the caller.
    pub fn list_folders(&self) -> Result<Vec<Folder>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, name, path, parent_id, locked, created_at
                   FROM folders ORDER BY created_at, name",
            )
            .map_err(map_err)?;
        let rows = stmt.query_map([], row_to_folder).map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    pub fn folder_by_id(&self, id: &str) -> Result<Option<Folder>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, name, path, parent_id, locked, created_at FROM folders WHERE id = ?1",
            rusqlite::params![id],
            row_to_folder,
        )
        .optional()
        .map_err(map_err)
    }

    /// Set a folder's `locked` flag + its KEK-wrapped content key (`Some` when sealing,
    /// `None` to clear on permanent remove-lock).
    pub fn set_folder_locked(
        &self,
        id: &str,
        locked: bool,
        wrapped_key: Option<&[u8]>,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE folders SET locked = ?2, wrapped_key = ?3 WHERE id = ?1",
            rusqlite::params![id, locked as i64, wrapped_key],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The KEK-wrapped content key for a sealed folder (`None` if the column is NULL).
    pub fn folder_wrapped_key(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT wrapped_key FROM folders WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get::<_, Option<Vec<u8>>>(0),
        )
        .optional()
        .map_err(map_err)
        .map(Option::flatten)
    }

    /// Count of notes assigned to each folder id (only folders with ≥1 note appear).
    pub fn count_notes_per_folder(&self) -> Result<std::collections::HashMap<String, usize>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT folder_id, COUNT(*) FROM notes
                  WHERE folder_id IS NOT NULL GROUP BY folder_id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))
            })
            .map_err(map_err)?;
        let mut out = std::collections::HashMap::new();
        for r in rows {
            let (id, n) = r.map_err(map_err)?;
            out.insert(id, n);
        }
        Ok(out)
    }

    /// Assign (or clear) a note's folder. Targets every provider row for the meeting so the
    /// note moves as a unit.
    pub fn set_note_folder(&self, meeting_id: &str, folder_id: Option<&str>) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE notes SET folder_id = ?2 WHERE meeting_id = ?1",
            rusqlite::params![meeting_id, folder_id],
        )
        .map_err(map_err)?;
        Ok(())
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

    /// Seal ONE provider row of a note: store its AES-GCM `content_blob`, blank that row's
    /// plaintext `markdown`, and clear its `exported_path` (the `.md` leaves the vault).
    /// Targets `(meeting_id, provider_id)` so distinct per-provider markdown each gets its own
    /// blob — a meeting re-summarized with multiple providers never collapses to one blob (which
    /// would destroy every provider's content but the first). The whole meeting is sealed by
    /// calling this once per provider row.
    pub fn seal_note(&self, meeting_id: &str, provider_id: &str, content_blob: &[u8]) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE notes SET content_blob = ?3, markdown = '', exported_path = NULL
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
    pub fn blank_sealed_notes_in_folders(&self, folder_ids: &HashSet<String>) -> Result<()> {
        if folder_ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
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
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Folder ids that are sealed (`locked=1`) — used to re-blank every sealed note on relock-all.
    pub fn locked_folder_ids(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id FROM folders WHERE locked = 1")
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

    // ── exposure-aware reads (MCP visibility filter, Stage D) ──────────────────
    //
    // A note is visible iff its folder is NULL or open (`locked=0`) OR its folder id is in the
    // session `unlocked` set. Since session-unlock decrypts `content_blob` back into the
    // `markdown` column, these reads see plaintext exactly like the in-app reads — they only add
    // the folder predicate. Sealed-and-not-session-unlocked notes are invisible.

    /// Search visible meetings only (MCP `search_meetings`).
    pub fn search_visible(
        &self,
        query: &str,
        limit: i64,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<SearchHit>> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let like = format!("%{}%", escape_like(q));
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT DISTINCT m.id, m.started_at, m.ended_at, m.title, m.duration_s, \
                    m.audio_path, m.status
               FROM meetings m
               LEFT JOIN segments s ON s.meeting_id = m.id
               LEFT JOIN notes n ON n.meeting_id = m.id
               LEFT JOIN folders f ON f.id = n.folder_id
              WHERE (m.title LIKE ?1 ESCAPE '\\'
                  OR s.text LIKE ?1 ESCAPE '\\'
                  OR n.markdown LIKE ?1 ESCAPE '\\')
                AND {visible}
              ORDER BY m.started_at DESC, m.id DESC
              LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
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

    /// Recent visible meetings only (MCP `list_recent_meetings`). A meeting is visible if it has
    /// no note, or any of its notes is visible (open/unlocked folder).
    pub fn list_meetings_visible(
        &self,
        limit: i64,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<Meeting>> {
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        // A meeting is hidden only when EVERY note it has is sealed-and-not-unlocked. Expressed
        // as: no note row exists that is currently sealed-and-hidden for this meeting, unless a
        // sibling visible note exists. Simpler + correct: keep the meeting if it has zero notes
        // OR at least one visible note.
        let sql = format!(
            "SELECT m.id, m.started_at, m.ended_at, m.title, m.duration_s, m.audio_path, m.status
               FROM meetings m
              WHERE NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id)
                 OR EXISTS (
                      SELECT 1 FROM notes n
                       LEFT JOIN folders f ON f.id = n.folder_id
                       WHERE n.meeting_id = m.id AND {visible}
                    )
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

    /// The latest visible note for a meeting (MCP `get_meeting`); `None` if the meeting's note
    /// is sealed-and-not-session-unlocked.
    pub fn get_note_if_visible(
        &self,
        meeting_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<NoteRecord>> {
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT n.meeting_id, n.provider_id, n.markdown, n.created_at, n.exported_path
               FROM notes n
               LEFT JOIN folders f ON f.id = n.folder_id
              WHERE n.meeting_id = ?1 AND {visible}
              ORDER BY n.created_at DESC LIMIT 1"
        );
        conn.query_row(&sql, rusqlite::params![meeting_id], row_to_note)
            .optional()
            .map_err(map_err)
    }

    /// Whether a meeting is visible at all (any note visible, or no notes) — gates the transcript
    /// in MCP `get_meeting` so a sealed meeting's transcript is not leaked either.
    pub fn meeting_is_visible(
        &self,
        meeting_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<bool> {
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = ?1) AS has_notes,
                    EXISTS (
                      SELECT 1 FROM notes n
                       LEFT JOIN folders f ON f.id = n.folder_id
                       WHERE n.meeting_id = ?1 AND {visible}
                    ) AS has_visible"
        );
        let (has_notes, has_visible): (bool, bool) = conn
            .query_row(&sql, rusqlite::params![meeting_id], |r| {
                Ok((r.get::<_, i64>(0)? != 0, r.get::<_, i64>(1)? != 0))
            })
            .map_err(map_err)?;
        Ok(!has_notes || has_visible)
    }
}

/// One note row needed to seal/unseal a folder.
#[derive(Debug, Clone)]
pub struct SealableNote {
    pub meeting_id: String,
    pub provider_id: String,
    pub markdown: String,
    pub exported_path: Option<String>,
    pub content_blob: Option<Vec<u8>>,
}

/// Build the SQL predicate (no params) that selects notes whose folder is open or
/// session-unlocked. `alias` is the notes-table alias (`n`); a sibling `folders f` join is
/// assumed for the alias. The unlocked ids are inlined as quoted literals — safe because they
/// are app-generated UUIDs, but we still escape single quotes defensively.
fn visibility_clause(_alias: &str, unlocked: &HashSet<String>) -> String {
    let mut clause = String::from("(f.locked IS NULL OR f.locked = 0");
    if !unlocked.is_empty() {
        let ids = unlocked
            .iter()
            .map(|id| format!("'{}'", id.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(", ");
        clause.push_str(&format!(" OR f.id IN ({ids})"));
    }
    clause.push(')');
    clause
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

/// Maps `(id, name, path, parent_id, locked, created_at)` → `Folder`.
fn row_to_folder(row: &Row<'_>) -> rusqlite::Result<Folder> {
    Ok(Folder {
        id: row.get(0)?,
        name: row.get(1)?,
        path: row.get(2)?,
        parent_id: row.get(3)?,
        locked: row.get::<_, i64>(4)? != 0,
        created_at: row.get(5)?,
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

#[cfg(test)]
mod lock_tests {
    //! File-backed (tempfile via `open_with_key` + a FIXED test key) tests for the per-folder
    //! seal/unseal lifecycle. These NEVER touch the real Keychain — both the SQLCipher DEK and
    //! the lock KEK are explicit literals here. They reproduce the exact seal/unseal/remove
    //! sequence the Stage-C commands run (db helpers + `crate::crypto`), so a regression in the
    //! lifecycle fails here even though the command wrappers need a Tauri `State`.

    use super::*;
    use crate::storage::models::{Folder, Meeting, MeetingStatus, NoteRecord};

    /// Fixed SQLCipher key for file-backed test DBs (NOT the Keychain DEK).
    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn temp_db_path(label: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "meetnotes-lock-test-{}-{}-{}.sqlite",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
    }

    fn file_db(label: &str) -> Db {
        Db::open_with_key(&temp_db_path(label), TEST_DEK).unwrap()
    }

    fn seed_folder(db: &Db, id: &str, name: &str) -> Folder {
        let f = Folder {
            id: id.to_string(),
            name: name.to_string(),
            path: name.to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        };
        db.insert_folder(&f).unwrap();
        f
    }

    fn seed_note(db: &Db, meeting_id: &str, markdown: &str, folder_id: Option<&str>) {
        db.insert_meeting(&Meeting {
            id: meeting_id.to_string(),
            started_at: "2026-06-26T09:00:00Z".to_string(),
            ended_at: None,
            title: Some(format!("title-{meeting_id}")),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: meeting_id.to_string(),
            provider_id: "claude_code".to_string(),
            markdown: markdown.to_string(),
            created_at: "2026-06-26T09:05:00Z".to_string(),
            exported_path: Some(format!("/vault/{meeting_id}.md")),
        })
        .unwrap();
        db.set_note_folder(meeting_id, folder_id).unwrap();
    }

    /// Add a second provider row (distinct markdown) to an existing meeting — models the
    /// re-summarize-with-another-provider state (e.g. `ollama` then `anthropic`).
    fn add_provider_note(db: &Db, meeting_id: &str, provider_id: &str, markdown: &str) {
        db.upsert_note(&NoteRecord {
            meeting_id: meeting_id.to_string(),
            provider_id: provider_id.to_string(),
            markdown: markdown.to_string(),
            created_at: "2026-06-26T09:06:00Z".to_string(),
            exported_path: Some(format!("/vault/{meeting_id}-{provider_id}.md")),
        })
        .unwrap();
        // Keep the new row in the same folder as its siblings.
        let folder_id = db
            .lock()
            .query_row(
                "SELECT folder_id FROM notes WHERE meeting_id = ?1 AND folder_id IS NOT NULL LIMIT 1",
                rusqlite::params![meeting_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .unwrap()
            .flatten();
        db.set_note_folder(meeting_id, folder_id.as_deref()).unwrap();
    }

    /// Mirror of `lock_folder`: generate CK, KEK-wrap, encrypt+verify each note (PER provider
    /// row), seal. One blob per (meeting, provider) — distinct provider markdown must not collide.
    fn seal_folder(db: &Db, folder_id: &str, kek: &[u8; 32]) {
        let ck = crate::crypto::random_key().unwrap();
        let wrapped = crate::crypto::encrypt(kek, &ck).unwrap();
        let notes = db.notes_in_folder(folder_id).unwrap();
        let mut blobs = Vec::new();
        for n in &notes {
            let blob = crate::crypto::encrypt(&ck, n.markdown.as_bytes()).unwrap();
            // Verify decryptable BEFORE blanking (the command's atomicity rule).
            assert_eq!(crate::crypto::decrypt(&ck, &blob).unwrap(), n.markdown.as_bytes());
            blobs.push((n.meeting_id.clone(), n.provider_id.clone(), blob));
        }
        db.set_folder_locked(folder_id, true, Some(&wrapped)).unwrap();
        for (mid, pid, blob) in &blobs {
            db.seal_note(mid, pid, blob).unwrap();
        }
    }

    /// Mirror of `unlock_folder`: KEK→unwrap CK→decrypt each blob back into ITS OWN row.
    fn session_unlock(db: &Db, folder_id: &str, kek: &[u8; 32]) {
        let wrapped = db.folder_wrapped_key(folder_id).unwrap().unwrap();
        let ck_bytes = crate::crypto::decrypt(kek, &wrapped).unwrap();
        let ck: [u8; 32] = ck_bytes.as_slice().try_into().unwrap();
        let notes = db.notes_in_folder(folder_id).unwrap();
        for n in &notes {
            let blob = n.content_blob.as_ref().unwrap();
            let pt = crate::crypto::decrypt(&ck, blob).unwrap();
            db.restore_note_markdown(&n.meeting_id, &n.provider_id, &String::from_utf8(pt).unwrap())
                .unwrap();
        }
    }

    #[test]
    fn lock_unlock_round_trips_byte_identical() {
        let db = file_db("roundtrip");
        let kek = crate::crypto::random_key().unwrap();
        seed_folder(&db, "f1", "Secret");
        let md_a = "# Strategy\n\nbudget: 1_000_000 EUR\n- hire 3\n";
        let md_b = "## 1:1 with Sarah\n\nshe wants a raise — zażółć gęślą jaźń 🔒\n";
        seed_note(&db, "m1", md_a, Some("f1"));
        seed_note(&db, "m2", md_b, Some("f1"));

        // SEAL.
        seal_folder(&db, "f1", &kek);

        // After sealing: markdown column blank, content_blob present, exported_path NULL.
        let sealed = db.notes_in_folder("f1").unwrap();
        assert_eq!(sealed.len(), 2);
        for n in &sealed {
            assert_eq!(n.markdown, "", "markdown column must be blanked");
            assert!(n.content_blob.is_some(), "content_blob must be present");
            assert!(n.exported_path.is_none(), "exported_path must be cleared");
        }
        // The raw blob must NOT contain the plaintext (not recoverable without the CK).
        for (n, expected) in sealed.iter().zip([md_a, md_b]) {
            let blob = n.content_blob.as_ref().unwrap();
            assert!(
                !contains_subslice(blob, expected.as_bytes()),
                "ciphertext must not leak plaintext"
            );
        }
        // Folder is locked + carries a wrapped key.
        assert!(db.folder_by_id("f1").unwrap().unwrap().locked);
        assert!(db.folder_wrapped_key("f1").unwrap().is_some());

        // SESSION-UNLOCK → markdown byte-identical.
        session_unlock(&db, "f1", &kek);
        let unlocked = db.notes_in_folder("f1").unwrap();
        let by_id: std::collections::HashMap<_, _> =
            unlocked.iter().map(|n| (n.meeting_id.as_str(), n)).collect();
        assert_eq!(by_id["m1"].markdown, md_a, "m1 markdown must round-trip byte-identical");
        assert_eq!(by_id["m2"].markdown, md_b, "m2 markdown must round-trip byte-identical");
        // content_blob still present (folder is still locked on disk during a session unlock).
        assert!(by_id["m1"].content_blob.is_some());
    }

    #[test]
    fn multi_provider_seal_unlock_preserves_each_providers_markdown() {
        // REGRESSION: a meeting with TWO provider notes (re-summarized with a second provider)
        // must NOT collapse to a single shared blob on seal. Each (meeting, provider) row carries
        // its OWN distinct markdown; sealing then unlocking must round-trip BOTH byte-identical.
        // (Pre-fix: seal dedup'd by meeting → only the first provider's markdown was encrypted,
        //  then blanked + restored to all rows, destroying the second provider's content.)
        let db = file_db("multi-provider");
        let kek = crate::crypto::random_key().unwrap();
        seed_folder(&db, "f1", "Secret");
        let md_claude = "# Claude note\n\nstructured summary with action items\n";
        let md_ollama = "# Ollama note\n\nDIFFERENT local-model summary — must survive 🔒\n";
        seed_note(&db, "m1", md_claude, Some("f1")); // provider = claude_code
        add_provider_note(&db, "m1", "ollama", md_ollama);

        // Sanity: two distinct provider rows before sealing.
        let before = db.notes_in_folder("f1").unwrap();
        assert_eq!(before.len(), 2, "two provider rows expected");

        seal_folder(&db, "f1", &kek);

        // Each provider row sealed independently: markdown blanked, its own blob present.
        let sealed = db.notes_in_folder("f1").unwrap();
        assert_eq!(sealed.len(), 2);
        for n in &sealed {
            assert_eq!(n.markdown, "", "markdown blanked");
            assert!(n.content_blob.is_some(), "each provider row keeps its own blob");
        }
        // The two blobs must differ (distinct plaintext → distinct ciphertext).
        let blob_claude = sealed.iter().find(|n| n.provider_id == "claude_code").unwrap();
        let blob_ollama = sealed.iter().find(|n| n.provider_id == "ollama").unwrap();
        assert_ne!(
            blob_claude.content_blob, blob_ollama.content_blob,
            "distinct provider markdown must NOT share one blob (content-loss guard)"
        );

        // Unlock → BOTH providers' markdown returns byte-identical.
        session_unlock(&db, "f1", &kek);
        let unlocked = db.notes_in_folder("f1").unwrap();
        let by_provider: std::collections::HashMap<_, _> =
            unlocked.iter().map(|n| (n.provider_id.as_str(), n)).collect();
        assert_eq!(
            by_provider["claude_code"].markdown, md_claude,
            "claude_code markdown must round-trip"
        );
        assert_eq!(
            by_provider["ollama"].markdown, md_ollama,
            "ollama markdown must round-trip (NOT overwritten by the sibling provider)"
        );
    }

    #[test]
    fn mcp_visibility_filter() {
        let db = file_db("visibility");
        let kek = crate::crypto::random_key().unwrap();
        seed_folder(&db, "secret", "Secret");
        seed_note(&db, "open1", "# open note about apples", None); // root, always visible
        seed_note(&db, "sealed1", "# secret note about bananas", Some("secret"));

        let empty: HashSet<String> = HashSet::new();

        // Before sealing both are visible.
        assert!(db
            .list_meetings_visible(50, &empty)
            .unwrap()
            .iter()
            .any(|m| m.id == "sealed1"));

        // SEAL → the sealed note is invisible to MCP, the open one stays visible.
        seal_folder(&db, "secret", &kek);
        let visible_ids: HashSet<String> = db
            .list_meetings_visible(50, &empty)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert!(visible_ids.contains("open1"), "open note stays visible");
        assert!(!visible_ids.contains("sealed1"), "sealed note is hidden");

        // search_visible: a query that ONLY matches the sealed note's content returns nothing
        // (its markdown is blanked + folder hidden), while the open note is found.
        assert!(db.search_visible("bananas", 20, &empty).unwrap().is_empty());
        assert!(!db.search_visible("apples", 20, &empty).unwrap().is_empty());

        // get_meeting visibility gate.
        assert!(!db.meeting_is_visible("sealed1", &empty).unwrap());
        assert!(db.get_note_if_visible("sealed1", &empty).unwrap().is_none());
        assert!(db.meeting_is_visible("open1", &empty).unwrap());

        // SESSION-UNLOCK → the sealed note becomes visible again.
        session_unlock(&db, "secret", &kek);
        let mut unlocked = HashSet::new();
        unlocked.insert("secret".to_string());
        assert!(db.meeting_is_visible("sealed1", &unlocked).unwrap());
        assert!(db.get_note_if_visible("sealed1", &unlocked).unwrap().is_some());
        assert!(!db.search_visible("bananas", 20, &unlocked).unwrap().is_empty());
        let visible_after: HashSet<String> = db
            .list_meetings_visible(50, &unlocked)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert!(visible_after.contains("sealed1"));
    }

    #[test]
    fn remove_lock_re_plaintexts() {
        let db = file_db("remove");
        let kek = crate::crypto::random_key().unwrap();
        seed_folder(&db, "f1", "Secret");
        let md = "# permanent\n\nback to plaintext\n";
        seed_note(&db, "m1", md, Some("f1"));
        seal_folder(&db, "f1", &kek);

        // Sanity: sealed.
        assert!(db.folder_by_id("f1").unwrap().unwrap().locked);
        assert_eq!(db.notes_in_folder("f1").unwrap()[0].markdown, "");

        // Mirror of remove_lock: KEK→unwrap CK→decrypt→restore plaintext→clear blob→unlock folder.
        let wrapped = db.folder_wrapped_key("f1").unwrap().unwrap();
        let ck_bytes = crate::crypto::decrypt(&kek, &wrapped).unwrap();
        let ck: [u8; 32] = ck_bytes.as_slice().try_into().unwrap();
        for n in db.notes_in_folder("f1").unwrap() {
            let pt = crate::crypto::decrypt(&ck, n.content_blob.as_ref().unwrap()).unwrap();
            let markdown = String::from_utf8(pt).unwrap();
            db.restore_note_markdown(&n.meeting_id, &n.provider_id, &markdown)
                .unwrap();
            db.clear_note_content_blob(&n.meeting_id).unwrap();
        }
        db.set_folder_locked("f1", false, None).unwrap();

        // Now: plaintext back, blob NULL, locked=0, wrapped_key NULL.
        let after = db.notes_in_folder("f1").unwrap();
        assert_eq!(after[0].markdown, md, "markdown restored byte-identical");
        assert!(after[0].content_blob.is_none(), "content_blob cleared");
        assert!(!db.folder_by_id("f1").unwrap().unwrap().locked);
        assert!(db.folder_wrapped_key("f1").unwrap().is_none());

        // Visible to MCP again with an empty session set.
        assert!(db
            .meeting_is_visible("m1", &HashSet::new())
            .unwrap());
    }

    /// Naive subslice search for the leak assertion.
    fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() || needle.len() > haystack.len() {
            return false;
        }
        haystack.windows(needle.len()).any(|w| w == needle)
    }
}
