use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, Row};

use crate::error::{AppError, Result};
use crate::storage::models::{
    Analytics, DayCount, Meeting, MeetingStatus, NoteRecord, StatusCount,
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
