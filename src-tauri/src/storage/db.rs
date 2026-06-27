use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, Row};

use crate::error::{AppError, Result};
use std::collections::HashSet;

use crate::storage::models::{
    Analytics, DayCount, EntityDetail, EntityKind, EntityNeighbor, Folder, GraphData, GraphEdge,
    GraphEntity, GraphNode, Meeting, MeetingStatus, NoteRecord, RecipeRecord, SearchHit,
    StatusCount, VaultSource,
};
use crate::transcribe::types::Segment;

/// The at-rest audio columns of one locked-folder meeting, surfaced by
/// [`Db::reblank_locked_folders_at_rest`] for the startup re-seal pass
/// (`state::reconcile_locked_at_rest`). All THREE per-stream paths are carried — the playback WAV
/// plus the two hi-res masters — because a crash-while-unlocked decrypts every sealed stream, so
/// each must be reconciled, not just the playback copy (B1). Any column may be `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedMeetingAudio {
    pub meeting_id: String,
    pub audio_path: Option<String>,
    pub mic_master_path: Option<String>,
    pub sys_master_path: Option<String>,
}

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

impl EntityKind {
    /// Stable lowercase string used as the on-disk `entities.kind` column value.
    /// Kept in sync with the serde `rename_all = "camelCase"` on the enum.
    fn as_str(&self) -> &'static str {
        match self {
            EntityKind::Person => "person",
            EntityKind::Project => "project",
        }
    }
}

impl FromStr for EntityKind {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "person" => Ok(EntityKind::Person),
            "project" => Ok(EntityKind::Project),
            other => Err(AppError::Storage(format!("unknown entity kind: {other}"))),
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
        // SQLCipher: key the connection FIRST (raw 32-byte key as a hex blob ⇒ no KDF). Hold the
        // formatted `PRAGMA key` string in a Zeroizing buffer (C6) so the hex key is wiped from the
        // stack as soon as the pragma runs — it is the most sensitive transient in this function.
        let key_pragma = zeroize::Zeroizing::new(format!("x'{dek_hex}'"));
        conn.pragma_update(None, "key", key_pragma.as_str())
            .map_err(map_err)?;
        drop(key_pragma); // explicit: wipe the hex key string now.
        // Harden SQLCipher's transient memory (B2/B10): keep temp tables / indices / materialized
        // subqueries in RAM (never spilled to an unencrypted temp FILE), and have SQLCipher wipe its
        // internal allocations (page buffers, KDF state) when freed. These MUST follow `PRAGMA key`
        // on EVERY connection (the keyed handle is what they harden). Enforce FK cascades
        // (segments/notes → meetings) and use WAL for concurrent reads while a write is in progress.
        conn.execute_batch(
            "PRAGMA cipher_memory_security = ON;
             PRAGMA temp_store = MEMORY;
             PRAGMA foreign_keys = ON;
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
             CREATE INDEX IF NOT EXISTS idx_folders_parent ON folders(parent_id);
             CREATE TABLE IF NOT EXISTS entities (
               id TEXT PRIMARY KEY,
               name TEXT NOT NULL,
               name_ci TEXT NOT NULL,
               kind TEXT NOT NULL,
               created_at TEXT NOT NULL,
               UNIQUE (name_ci, kind)
             );
             CREATE TABLE IF NOT EXISTS entity_mentions (
               entity_id TEXT NOT NULL,
               meeting_id TEXT NOT NULL,
               created_at TEXT NOT NULL,
               PRIMARY KEY (entity_id, meeting_id),
               FOREIGN KEY (entity_id) REFERENCES entities(id) ON DELETE CASCADE,
               FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_entity_mentions_meeting ON entity_mentions(meeting_id);
             CREATE INDEX IF NOT EXISTS idx_entity_mentions_entity ON entity_mentions(entity_id);
             CREATE INDEX IF NOT EXISTS idx_entities_kind ON entities(kind);",
        )
        .map_err(map_err)?;
        // Guarded ALTERs — notes gain a folder association + a sealed-content blob (AES-GCM
        // markdown when the folder is locked; NULL when open). migrate() re-runs each launch and
        // `ALTER ADD COLUMN` errors if the column already exists, so check pragma_table_info first.
        Self::add_column_if_missing(&conn, "notes", "folder_id", "TEXT")?;
        Self::add_column_if_missing(&conn, "notes", "content_blob", "BLOB")?;
        // Phase B: 2-way stream attribution ("me"/"others"). Guarded ALTER (same idempotent
        // pattern) — NULL for legacy rows transcribed before dual-stream, which read back as
        // `speaker: None` (unattributed). NOT per-remote-person diarization; see types::Segment.
        Self::add_column_if_missing(&conn, "segments", "speaker", "TEXT")?;
        // Phase 0.5 — full per-folder lock (defense-in-depth beyond SQLCipher-at-rest):
        // sealed-folder transcripts + timelines carry an AES-GCM blob under the folder CK while
        // sealed; the plaintext `text`/`data` column is blanked. Reversed on session-unlock.
        // Guarded ALTER (idempotent); NULL for every row in an open folder.
        Self::add_column_if_missing(&conn, "segments", "text_blob", "BLOB")?;
        Self::add_column_if_missing(&conn, "timelines", "data_blob", "BLOB")?;
        // Rec #3: faithful per-stream float32 MASTER archives (mic native + system 48k), opt-in.
        // Each is sealed at rest exactly like `audio_path` (→ `<file>.enc`). NULL for every meeting
        // recorded without `keep_hires_masters` → zero change for existing / non-opted users. These
        // live OFF the `Meeting` struct (targeted reads only) so they can never leak through a
        // masked DTO; they are export-only via gated commands.
        Self::add_column_if_missing(&conn, "meetings", "mic_master_path", "TEXT")?;
        Self::add_column_if_missing(&conn, "meetings", "sys_master_path", "TEXT")?;
        // Phase 1 — FTS5/BM25 full-text retrieval over the three text sources
        // (meeting titles, transcript segments, note markdown). Replaces the prior
        // substring LIKE search so word-order/term retrieval works ("alpha beta" == "beta alpha")
        // and ranking uses bm25(). SQLCipher is built with FTS5 compiled in (bundled-sqlcipher) —
        // ZERO new deps. Runs on the same locked connection as the rest of migrate().
        Self::migrate_fts(&conn)?;
        Ok(())
    }

    /// Idempotent FTS5 setup: three external-content FTS tables (one per text source) kept in sync
    /// by INSERT/UPDATE/DELETE triggers, plus a one-time backfill from existing rows.
    ///
    /// Why THREE external-content tables instead of one aggregate table: external-content FTS5
    /// (`content='meetings'` / `content='segments'` / `content='notes'`) mirrors each base table's
    /// implicit `rowid` 1:1, so the standard `_ai`/`_ad`/`_au` trigger trio keeps the index exact
    /// with no re-aggregation and no manual rowid bookkeeping. Critically for the lock model: when a
    /// folder is sealed, `seal_note`/`seal_segment` BLANK the plaintext column
    /// (`UPDATE notes SET markdown=''` / `UPDATE segments SET text=''`). That UPDATE fires the `_au`
    /// trigger, which deletes the OLD tokens from the FTS index and inserts the now-empty value — so
    /// NO stale tokens of sealed content survive in the index. (Round-trip verified by
    /// `sealed_tokens_purged_from_fts_after_blank`.)
    ///
    /// Tokenizer: `unicode61 remove_diacritics 2` — Unicode-aware word boundaries and full
    /// diacritic folding (Unicode 6.1 NFD), so Polish ("zażółć"/"zazolc") and English fold/match
    /// alike. `remove_diacritics 2` is the strict mode that strips diacritics even from combined
    /// codepoints (the `1` mode misses some), which matters for Polish ł/ż/ó/ą/ę/ś/ć/ń/ź.
    fn migrate_fts(conn: &Connection) -> Result<()> {
        let already_built: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='fts_meetings'",
                [],
                |_| Ok(true),
            )
            .optional()
            .map_err(map_err)?
            .unwrap_or(false);

        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS fts_meetings USING fts5(
                 title,
                 content='meetings',
                 content_rowid='rowid',
                 tokenize = 'unicode61 remove_diacritics 2'
             );
             CREATE VIRTUAL TABLE IF NOT EXISTS fts_segments USING fts5(
                 text,
                 content='segments',
                 content_rowid='rowid',
                 tokenize = 'unicode61 remove_diacritics 2'
             );
             CREATE VIRTUAL TABLE IF NOT EXISTS fts_notes USING fts5(
                 markdown,
                 content='notes',
                 content_rowid='rowid',
                 tokenize = 'unicode61 remove_diacritics 2'
             );

             -- meetings → fts_meetings (title only)
             CREATE TRIGGER IF NOT EXISTS fts_meetings_ai AFTER INSERT ON meetings BEGIN
                 INSERT INTO fts_meetings(rowid, title) VALUES (new.rowid, new.title);
             END;
             CREATE TRIGGER IF NOT EXISTS fts_meetings_ad AFTER DELETE ON meetings BEGIN
                 INSERT INTO fts_meetings(fts_meetings, rowid, title)
                   VALUES ('delete', old.rowid, old.title);
             END;
             CREATE TRIGGER IF NOT EXISTS fts_meetings_au AFTER UPDATE ON meetings BEGIN
                 INSERT INTO fts_meetings(fts_meetings, rowid, title)
                   VALUES ('delete', old.rowid, old.title);
                 INSERT INTO fts_meetings(rowid, title) VALUES (new.rowid, new.title);
             END;

             -- segments → fts_segments (text). UPDATE fires on seal-blanking (text='') → stale
             -- tokens deleted, empty value re-indexed: no sealed transcript survives the index.
             CREATE TRIGGER IF NOT EXISTS fts_segments_ai AFTER INSERT ON segments BEGIN
                 INSERT INTO fts_segments(rowid, text) VALUES (new.rowid, new.text);
             END;
             CREATE TRIGGER IF NOT EXISTS fts_segments_ad AFTER DELETE ON segments BEGIN
                 INSERT INTO fts_segments(fts_segments, rowid, text)
                   VALUES ('delete', old.rowid, old.text);
             END;
             CREATE TRIGGER IF NOT EXISTS fts_segments_au AFTER UPDATE ON segments BEGIN
                 INSERT INTO fts_segments(fts_segments, rowid, text)
                   VALUES ('delete', old.rowid, old.text);
                 INSERT INTO fts_segments(rowid, text) VALUES (new.rowid, new.text);
             END;

             -- notes → fts_notes (markdown). Same: seal-blanking (markdown='') purges note tokens.
             CREATE TRIGGER IF NOT EXISTS fts_notes_ai AFTER INSERT ON notes BEGIN
                 INSERT INTO fts_notes(rowid, markdown) VALUES (new.rowid, new.markdown);
             END;
             CREATE TRIGGER IF NOT EXISTS fts_notes_ad AFTER DELETE ON notes BEGIN
                 INSERT INTO fts_notes(fts_notes, rowid, markdown)
                   VALUES ('delete', old.rowid, old.markdown);
             END;
             CREATE TRIGGER IF NOT EXISTS fts_notes_au AFTER UPDATE ON notes BEGIN
                 INSERT INTO fts_notes(fts_notes, rowid, markdown)
                   VALUES ('delete', old.rowid, old.markdown);
                 INSERT INTO fts_notes(rowid, markdown) VALUES (new.rowid, new.markdown);
             END;",
        )
        .map_err(map_err)?;

        // One-time backfill from existing rows (only the first time the FTS tables are created — on
        // every later launch they already exist and the triggers have kept them current, so this is
        // a no-op and `migrate()` stays idempotent).
        if !already_built {
            conn.execute_batch(
                "INSERT INTO fts_meetings(rowid, title) SELECT rowid, title FROM meetings;
                 INSERT INTO fts_segments(rowid, text) SELECT rowid, text FROM segments;
                 INSERT INTO fts_notes(rowid, markdown) SELECT rowid, markdown FROM notes;",
            )
            .map_err(map_err)?;
        }
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

    /// B12: fold the WAL back into the main DB and TRUNCATE the `-wal` sidecar. Called on relock-all
    /// and on app quit so freshly-written-then-relocked plaintext does not linger in the unencrypted-
    /// at-the-OS-level WAL longer than necessary (the WAL is SQLCipher-encrypted, but checkpointing
    /// minimizes the window and shrinks the sidecar). Best-effort: a checkpoint failure (e.g. another
    /// reader holds the WAL) is returned so the caller can log it, but is never fatal.
    pub fn checkpoint_truncate(&self) -> Result<()> {
        let conn = self.lock();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(map_err)
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
            "SELECT id, started_at, ended_at, title, duration_s, audio_path, status,
                    (SELECT n.folder_id FROM notes n
                      WHERE n.meeting_id = meetings.id AND n.folder_id IS NOT NULL LIMIT 1)
                      AS folder_id
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
            "SELECT id, started_at, ended_at, title, duration_s, audio_path, status,
                    (SELECT n.folder_id FROM notes n
                      WHERE n.meeting_id = meetings.id AND n.folder_id IS NOT NULL LIMIT 1)
                      AS folder_id
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
                "SELECT id, started_at, ended_at, title, duration_s, audio_path, status,
                        (SELECT n.folder_id FROM notes n
                          WHERE n.meeting_id = meetings.id AND n.folder_id IS NOT NULL LIMIT 1)
                          AS folder_id
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
        let Some(match_expr) = fts_match_query(q) else {
            return Ok(Vec::new());
        };
        let conn = self.lock();
        // FTS5/BM25 over the three sources. Each FTS table is external-content (its `rowid` == the
        // base row's rowid), so we join the FTS rowid back to the owning meeting. A meeting can
        // match in >1 source; we keep its BEST (lowest = most relevant) bm25 score and rank by it,
        // tie-broken newest-first (mirrors the prior ORDER BY started_at DESC).
        let mut stmt = conn
            .prepare(
                "WITH hits(meeting_id, rank) AS (
                     SELECT m.id, bm25(fts_meetings)
                       FROM fts_meetings
                       JOIN meetings m ON m.rowid = fts_meetings.rowid
                      WHERE fts_meetings MATCH ?1
                     UNION ALL
                     SELECT s.meeting_id, bm25(fts_segments)
                       FROM fts_segments
                       JOIN segments s ON s.rowid = fts_segments.rowid
                      WHERE fts_segments MATCH ?1
                     UNION ALL
                     SELECT n.meeting_id, bm25(fts_notes)
                       FROM fts_notes
                       JOIN notes n ON n.rowid = fts_notes.rowid
                      WHERE fts_notes MATCH ?1
                 ),
                 ranked(meeting_id, rank) AS (
                     SELECT meeting_id, MIN(rank) FROM hits GROUP BY meeting_id
                 )
                 SELECT m.id, m.started_at, m.ended_at, m.title, m.duration_s, \
                        m.audio_path, m.status, \
                        (SELECT nf.folder_id FROM notes nf \
                          WHERE nf.meeting_id = m.id AND nf.folder_id IS NOT NULL LIMIT 1) \
                          AS folder_id
                   FROM ranked r
                   JOIN meetings m ON m.id = r.meeting_id
                  ORDER BY r.rank ASC, m.started_at DESC, m.id DESC
                  LIMIT ?2",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![match_expr, limit], row_to_meeting)
            .map_err(map_err)?;
        let mut meetings = Vec::new();
        for r in rows {
            meetings.push(r.map_err(map_err)??);
        }

        let like = format!("%{}%", escape_like(q));
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
                       (meeting_id, idx, start_s, end_s, text, speaker)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .map_err(map_err)?;
            for seg in segments {
                stmt.execute(rusqlite::params![
                    meeting_id,
                    seg.idx,
                    seg.start_s,
                    seg.end_s,
                    seg.text,
                    seg.speaker,
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
                "SELECT idx, start_s, end_s, text, speaker
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
                    // NULL (legacy / unattributed rows) → None.
                    speaker: row.get(4)?,
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

    /// The most recent VISIBLE note across all meetings (BLK-2b backing for `get_last_note`): a note
    /// whose folder is open/NULL or session-unlocked. A sealed-and-not-unlocked latest note is
    /// skipped so the recorder bar never surfaces its blanked (or, defensively, sealed) content.
    pub fn latest_note_visible(&self, unlocked: &HashSet<String>) -> Result<Option<NoteRecord>> {
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT n.meeting_id, n.provider_id, n.markdown, n.created_at, n.exported_path
               FROM notes n
               LEFT JOIN folders f ON f.id = n.folder_id
              WHERE {visible}
              ORDER BY n.created_at DESC LIMIT 1"
        );
        conn.query_row(&sql, [], row_to_note)
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
                        m.status, \
                        (SELECT nf.folder_id FROM notes nf \
                          WHERE nf.meeting_id = m.id AND nf.folder_id IS NOT NULL LIMIT 1) \
                          AS folder_id
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

    /// Look up a folder by its vault-relative `path` (the `path` column is `NOT NULL UNIQUE`). Used
    /// by the auto-organize seam to map a classifier-chosen subfolder name back to its folder row —
    /// so a note auto-filed into a LOCKED folder's on-disk dir is sealed/rejected (it would
    /// otherwise land plaintext with `folder_id = NULL`, which `lock_folder` + the at-rest reconcile
    /// both key off `folder_id` and miss).
    pub fn folder_by_path(&self, path: &str) -> Result<Option<Folder>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, name, path, parent_id, locked, created_at FROM folders WHERE path = ?1",
            rusqlite::params![path],
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

    /// Direct CHILD folders of `parent_id` (one level only — not transitive). Used by
    /// `rename_folder`/`delete_folder` to walk the subtree so a rename can re-prefix descendant
    /// paths and a delete can refuse a non-empty tree.
    pub fn child_folders(&self, parent_id: &str) -> Result<Vec<Folder>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, name, path, parent_id, locked, created_at
                   FROM folders WHERE parent_id = ?1 ORDER BY created_at, name",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![parent_id], row_to_folder)
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Rename a folder's display `name` AND its vault-relative `path` in one statement. The new
    /// `path` is composed by the caller (parent path + sanitized name), so this is a pure column
    /// update — it does NOT touch the on-disk vault dir or any note's `exported_path` (those are the
    /// caller's responsibility, sequenced so a crash can never lose content). Leaves `locked` /
    /// `wrapped_key` untouched: a locked-folder rename is metadata-only and never reaches sealed
    /// content.
    pub fn rename_folder(&self, id: &str, new_name: &str, new_path: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE folders SET name = ?2, path = ?3 WHERE id = ?1",
            rusqlite::params![id, new_name, new_path],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Delete a folder ROW by id (the `folders` table only). The caller MUST have already moved /
    /// unsealed its notes elsewhere — this does NOT reassign or delete any note, and a locked folder
    /// with sealed content must never reach here (the command refuses unless the lock was removed
    /// first). Returns the number of rows deleted (0 if the id was already gone — idempotent).
    pub fn delete_folder(&self, id: &str) -> Result<usize> {
        let conn = self.lock();
        let n = conn
            .execute(
                "DELETE FROM folders WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(map_err)?;
        Ok(n)
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

    /// Assign (or clear) a MEETING's folder. A meeting's folder = its note's folder, so this
    /// updates `folder_id` on EVERY provider row of the meeting (`WHERE meeting_id = ?1`) — the
    /// note moves as a unit and the seal/unlock lifecycle (which iterates provider rows) stays
    /// coherent (no row left in a stale folder). `None` clears the folder (move to vault root).
    pub fn set_meeting_folder(&self, meeting_id: &str, folder_id: Option<&str>) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE notes SET folder_id = ?2 WHERE meeting_id = ?1",
            rusqlite::params![meeting_id, folder_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Back-compat alias for [`Db::set_meeting_folder`] — a note's folder is the meeting's folder.
    pub fn set_note_folder(&self, meeting_id: &str, folder_id: Option<&str>) -> Result<()> {
        self.set_meeting_folder(meeting_id, folder_id)
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
    pub fn reblank_locked_folders_at_rest(&self) -> Result<Vec<LockedMeetingAudio>> {
        const LOCKED_MEETINGS: &str = "SELECT DISTINCT meeting_id FROM notes \
             WHERE folder_id IN (SELECT id FROM folders WHERE locked = 1)";
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
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
        tx.commit().map_err(map_err)?;
        Ok(audio)
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

    // ── transcript + timeline sealing (Phase 0.5 full per-folder lock) ─────────
    //
    // Segments + timelines live IN the SQLCipher DB (encrypted at rest already), but were NOT
    // gated in-app: a meeting in a locked-and-not-unlocked folder still returned its transcript +
    // timeline. These helpers add the SAME defense-in-depth the note markdown already has — an
    // AES-GCM blob under the folder CK in an OPEN db, with the plaintext column blanked while
    // sealed, reversed on session-unlock / re-blanked on relock / permanently restored on
    // remove-lock. All keyed off the meeting set of a folder (a meeting's folder = its notes'
    // folder, derived from `notes.folder_id`).

    /// Distinct meeting ids whose notes live in `folder_id` (the meetings governed by the
    /// folder's lock). Used to seal/unseal each meeting's transcript + timeline.
    pub fn meeting_ids_in_folder(&self, folder_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT DISTINCT meeting_id FROM notes WHERE folder_id = ?1")
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// The owning folder id for a meeting (its notes' `folder_id`), or `None` at the vault root.
    /// Drives the read-gate predicate `meeting_is_unlocked`.
    pub fn folder_for_meeting(&self, meeting_id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT folder_id FROM notes
              WHERE meeting_id = ?1 AND folder_id IS NOT NULL LIMIT 1",
            rusqlite::params![meeting_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(map_err)
        .map(Option::flatten)
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
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)),
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
        let Some(match_expr) = fts_match_query(q) else {
            return Ok(Vec::new());
        };
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        // FTS5/BM25 candidates (same UNION/MIN(bm25) ranking as `search`), THEN gated by exactly the
        // prior visibility predicate so a sealed-and-not-session-unlocked meeting is excluded: keep
        // a meeting iff it has NO note rows, OR it has at least one VISIBLE note row (folder
        // NULL/open OR session-unlocked). This mirrors the old LEFT JOIN notes/folders + {visible}
        // WHERE — and is defense-in-depth on top of seal-blanking + the FTS `_au` purge: even if a
        // stale token somehow survived in the index, a sealed-not-unlocked meeting can never pass
        // this clause. (Note's text content is itself purged from fts_notes on blanking, so it can't
        // even produce a candidate.)
        let sql = format!(
            "WITH hits(meeting_id, rank) AS (
                 SELECT m.id, bm25(fts_meetings)
                   FROM fts_meetings
                   JOIN meetings m ON m.rowid = fts_meetings.rowid
                  WHERE fts_meetings MATCH ?1
                 UNION ALL
                 SELECT s.meeting_id, bm25(fts_segments)
                   FROM fts_segments
                   JOIN segments s ON s.rowid = fts_segments.rowid
                  WHERE fts_segments MATCH ?1
                 UNION ALL
                 SELECT n.meeting_id, bm25(fts_notes)
                   FROM fts_notes
                   JOIN notes n ON n.rowid = fts_notes.rowid
                  WHERE fts_notes MATCH ?1
             ),
             ranked(meeting_id, rank) AS (
                 SELECT meeting_id, MIN(rank) FROM hits GROUP BY meeting_id
             )
             SELECT m.id, m.started_at, m.ended_at, m.title, m.duration_s, \
                    m.audio_path, m.status, \
                    (SELECT nf.folder_id FROM notes nf \
                      WHERE nf.meeting_id = m.id AND nf.folder_id IS NOT NULL LIMIT 1) \
                      AS folder_id
               FROM ranked r
               JOIN meetings m ON m.id = r.meeting_id
              WHERE NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id)
                 OR EXISTS (
                      SELECT 1 FROM notes n
                       LEFT JOIN folders f ON f.id = n.folder_id
                       WHERE n.meeting_id = m.id AND {visible}
                    )
              ORDER BY r.rank ASC, m.started_at DESC, m.id DESC
              LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![match_expr, limit], row_to_meeting)
            .map_err(map_err)?;
        let mut meetings = Vec::new();
        for r in rows {
            meetings.push(r.map_err(map_err)??);
        }
        let like = format!("%{}%", escape_like(q));
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
            "SELECT m.id, m.started_at, m.ended_at, m.title, m.duration_s, m.audio_path, m.status,
                    (SELECT nf.folder_id FROM notes nf
                      WHERE nf.meeting_id = m.id AND nf.folder_id IS NOT NULL LIMIT 1)
                      AS folder_id
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

    // ── self-assembling graph (entities + mentions) ───────────────────────────
    //
    // Sink A of the dual-sink: the encrypted DB is the source of truth for the in-app
    // graph. EVERY read below pushes the same `unlocked` set through `visibility_clause`
    // over `entity_mentions → meetings → notes n LEFT JOIN folders f`, replicating the
    // `EXISTS(visible note) OR NOT EXISTS(any note)` predicate of `list_meetings_visible`
    // verbatim — so a sealed-and-not-unlocked meeting contributes ZERO to nodes, edges,
    // and counts. The rows persist through sealing; they merely become invisible at read.

    /// Upsert an entity by `(name_ci, kind)`, case-insensitively de-duplicated. Keeps the
    /// FIRST-SEEN casing in `name` (a later "anna kowalska" does NOT overwrite "Anna Kowalska").
    /// `name_ci` uses full-Unicode `to_lowercase()` (NOT ASCII-only folding) so accented names
    /// dedup consistently. Returns the (new or existing) entity id. Race-safe: `INSERT OR IGNORE`
    /// then re-read, so a concurrent insert resolves to the single winning row.
    pub fn upsert_entity(&self, name: &str, kind: EntityKind) -> Result<String> {
        let conn = self.lock();
        let name = name.trim();
        let name_ci = name.to_lowercase();
        let kind_str = kind.as_str();
        let id = uuid::Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();
        // INSERT OR IGNORE: if `(name_ci, kind)` already exists the insert is a no-op (the
        // existing first-seen casing + id are kept). Either way we re-read the canonical id.
        conn.execute(
            "INSERT OR IGNORE INTO entities (id, name, name_ci, kind, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, name, name_ci, kind_str, created_at],
        )
        .map_err(map_err)?;
        let resolved: String = conn
            .query_row(
                "SELECT id FROM entities WHERE name_ci = ?1 AND kind = ?2",
                rusqlite::params![name_ci, kind_str],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        Ok(resolved)
    }

    /// Record that `entity_id` was mentioned in `meeting_id`. Idempotent via the PK
    /// `(entity_id, meeting_id)` — re-summarize / re-extract never double-counts.
    pub fn add_mention(&self, entity_id: &str, meeting_id: &str) -> Result<()> {
        let conn = self.lock();
        let created_at = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR IGNORE INTO entity_mentions (entity_id, meeting_id, created_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![entity_id, meeting_id, created_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// All entities that have ≥1 VISIBLE mention, with that visible mention count. An entity
    /// mentioned ONLY in sealed-and-not-unlocked meetings has count 0 → dropped by `HAVING`,
    /// so its name (which lived only in encrypted markdown) never reaches the renderer.
    pub fn list_entities_visible(&self, unlocked: &HashSet<String>) -> Result<Vec<GraphNode>> {
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT e.id, e.name, e.kind, COUNT(em.meeting_id) AS cnt
               FROM entities e
               JOIN entity_mentions em ON em.entity_id = e.id
               JOIN meetings m ON m.id = em.meeting_id
              WHERE (
                      NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id)
                   OR EXISTS (
                        SELECT 1 FROM notes n
                         LEFT JOIN folders f ON f.id = n.folder_id
                         WHERE n.meeting_id = m.id AND {visible}
                      )
                    )
              GROUP BY e.id, e.name, e.kind
             HAVING cnt > 0
              ORDER BY cnt DESC, e.name COLLATE NOCASE ASC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                let id: String = r.get(0)?;
                let name: String = r.get(1)?;
                let kind_str: String = r.get(2)?;
                let mention_count: i64 = r.get(3)?;
                Ok((id, name, kind_str, mention_count))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            let (id, name, kind_str, mention_count) = r.map_err(map_err)?;
            out.push(GraphNode {
                id,
                name,
                kind: EntityKind::from_str(&kind_str)?,
                mention_count,
            });
        }
        Ok(out)
    }

    /// The VISIBLE meetings mentioning `entity_id`, newest first, as `VaultSource` chips
    /// (the same shape the FE uses for backlink chips). Sealed-not-unlocked meetings excluded.
    pub fn entity_mentions_visible(
        &self,
        entity_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<VaultSource>> {
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT m.id, m.title, m.started_at
               FROM entity_mentions em
               JOIN meetings m ON m.id = em.meeting_id
              WHERE em.entity_id = ?1
                AND (
                      NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id)
                   OR EXISTS (
                        SELECT 1 FROM notes n
                         LEFT JOIN folders f ON f.id = n.folder_id
                         WHERE n.meeting_id = m.id AND {visible}
                      )
                    )
              ORDER BY m.started_at DESC, m.id DESC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![entity_id], |r| {
                let meeting_id: String = r.get(0)?;
                let title: Option<String> = r.get(1)?;
                let started_at: String = r.get(2)?;
                Ok(VaultSource {
                    meeting_id,
                    title: title.unwrap_or_default(),
                    started_at,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Entity↔entity co-occurrence edges: two entities sharing the SAME visible meeting, weighted
    /// by the number of shared visible meetings. Pair-deduped via `a.entity_id < b.entity_id`
    /// → exactly one undirected edge per pair, `source < target`. Both endpoints' meetings are
    /// gated by the visibility predicate, so a co-occurrence in a sealed meeting yields no edge.
    pub fn graph_edges_visible(&self, unlocked: &HashSet<String>) -> Result<Vec<GraphEdge>> {
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT a.entity_id, b.entity_id, COUNT(*) AS weight
               FROM entity_mentions a
               JOIN entity_mentions b
                 ON a.meeting_id = b.meeting_id AND a.entity_id < b.entity_id
               JOIN meetings m ON m.id = a.meeting_id
              WHERE (
                      NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id)
                   OR EXISTS (
                        SELECT 1 FROM notes n
                         LEFT JOIN folders f ON f.id = n.folder_id
                         WHERE n.meeting_id = m.id AND {visible}
                      )
                    )
              GROUP BY a.entity_id, b.entity_id
              ORDER BY weight DESC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(GraphEdge {
                    source: r.get(0)?,
                    target: r.get(1)?,
                    weight: r.get(2)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Whether `entity_id` has ≥1 VISIBLE mention right now — i.e. at least one mention lands in a
    /// meeting that is visible under the SAME predicate as `list_meetings_visible` / the other
    /// graph reads (`EXISTS(visible note) OR NOT EXISTS(any note)`). An entity mentioned ONLY in
    /// sealed-and-not-unlocked meetings returns `false`, so its name (which lived only in encrypted
    /// markdown) can never leak through `get_entity` / `build_entity_detail`. This is the gate the
    /// detail path was missing: `get_entity` itself reads the raw `entities` row with no visibility
    /// predicate, so callers that expose an entity to the FE MUST go through this check first.
    pub fn entity_is_visible(
        &self,
        entity_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<bool> {
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT EXISTS (
                      SELECT 1
                        FROM entity_mentions em
                        JOIN meetings m ON m.id = em.meeting_id
                       WHERE em.entity_id = ?1
                         AND (
                               NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id)
                            OR EXISTS (
                                 SELECT 1 FROM notes n
                                  LEFT JOIN folders f ON f.id = n.folder_id
                                  WHERE n.meeting_id = m.id AND {visible}
                               )
                             )
                    )"
        );
        let visible: bool = conn
            .query_row(&sql, rusqlite::params![entity_id], |r| {
                Ok(r.get::<_, i64>(0)? != 0)
            })
            .map_err(map_err)?;
        Ok(visible)
    }

    /// One entity row by id (`None` if absent), with its first-seen casing. NOTE: this reads the
    /// raw `entities` row WITHOUT a visibility predicate — it must NOT be exposed to the FE for an
    /// arbitrary id without first gating on [`entity_is_visible`] (see `build_entity_detail`).
    pub fn get_entity(&self, entity_id: &str) -> Result<Option<GraphEntity>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT id, name, kind, created_at FROM entities WHERE id = ?1",
                rusqlite::params![entity_id],
                |r| {
                    let id: String = r.get(0)?;
                    let name: String = r.get(1)?;
                    let kind_str: String = r.get(2)?;
                    let created_at: String = r.get(3)?;
                    Ok((id, name, kind_str, created_at))
                },
            )
            .optional()
            .map_err(map_err)?;
        match row {
            Some((id, name, kind_str, created_at)) => Ok(Some(GraphEntity {
                id,
                name,
                kind: EntityKind::from_str(&kind_str)?,
                created_at,
            })),
            None => Ok(None),
        }
    }

    /// The top-`limit` entities co-occurring with `entity_id` (the neighborhood satellites),
    /// ranked by shared VISIBLE meeting count. Both the anchor's and the neighbor's mention must
    /// land in a visible meeting, so sealed co-occurrences never surface a neighbor.
    pub fn entity_neighbors_visible(
        &self,
        entity_id: &str,
        unlocked: &HashSet<String>,
        limit: i64,
    ) -> Result<Vec<EntityNeighbor>> {
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT e.id, e.name, e.kind, COUNT(*) AS shared
               FROM entity_mentions a
               JOIN entity_mentions b ON a.meeting_id = b.meeting_id AND b.entity_id != a.entity_id
               JOIN entities e ON e.id = b.entity_id
               JOIN meetings m ON m.id = a.meeting_id
              WHERE a.entity_id = ?1
                AND (
                      NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id)
                   OR EXISTS (
                        SELECT 1 FROM notes n
                         LEFT JOIN folders f ON f.id = n.folder_id
                         WHERE n.meeting_id = m.id AND {visible}
                      )
                    )
              GROUP BY e.id, e.name, e.kind
              ORDER BY shared DESC, e.name COLLATE NOCASE ASC
              LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![entity_id, limit], |r| {
                let id: String = r.get(0)?;
                let name: String = r.get(1)?;
                let kind_str: String = r.get(2)?;
                let shared_meetings: i64 = r.get(3)?;
                Ok((id, name, kind_str, shared_meetings))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            let (id, name, kind_str, shared_meetings) = r.map_err(map_err)?;
            out.push(EntityNeighbor {
                id,
                name,
                kind: EntityKind::from_str(&kind_str)?,
                shared_meetings,
            });
        }
        Ok(out)
    }

    /// Whether ANY folder is sealed-and-not-unlocked right now (i.e. a locked folder whose id is
    /// NOT in the session `unlocked` set). Drives the FE's one honest "some entities hidden"
    /// disclosure banner — it never leaks how many or which.
    pub fn has_hidden_folders(&self, unlocked: &HashSet<String>) -> Result<bool> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id FROM folders WHERE locked = 1")
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        for r in rows {
            let id = r.map_err(map_err)?;
            if !unlocked.contains(&id) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Build the full graph payload (`get_graph`): all visible nodes + all visible edges +
    /// the hidden-folder disclosure flag, snapshotting the passed-in session `unlocked` set.
    pub fn build_graph(&self, unlocked: &HashSet<String>) -> Result<GraphData> {
        let nodes = self.list_entities_visible(unlocked)?;
        let edges = self.graph_edges_visible(unlocked)?;
        let has_hidden = self.has_hidden_folders(unlocked)?;
        Ok(GraphData {
            nodes,
            edges,
            has_hidden,
        })
    }

    /// Build the detail payload for one entity (`get_entity_detail`): the entity, its visible
    /// backlinked meetings, and its top co-occurring neighbors. `None` if the entity is unknown
    /// OR has ZERO visible mentions (mentioned only in sealed-not-unlocked meetings). The
    /// visibility gate is mandatory here: `get_entity` reads the raw `entities` row with NO
    /// predicate, so without this check a caller holding a stale entity id (cached from a prior
    /// open-folder `get_graph`, before the folder was sealed/auto-relocked) could read back the
    /// entity's `name` — which lived only in the sealed meeting's encrypted markdown. Routing
    /// through `entity_is_visible` keeps the detail path consistent with every other graph read.
    pub fn build_entity_detail(
        &self,
        entity_id: &str,
        unlocked: &HashSet<String>,
        neighbor_limit: i64,
    ) -> Result<Option<EntityDetail>> {
        // Anti-leak gate FIRST: a sealed-only entity is indistinguishable from an unknown id.
        if !self.entity_is_visible(entity_id, unlocked)? {
            return Ok(None);
        }
        let entity = match self.get_entity(entity_id)? {
            Some(e) => e,
            None => return Ok(None),
        };
        let meetings = self.entity_mentions_visible(entity_id, unlocked)?;
        let neighbors = self.entity_neighbors_visible(entity_id, unlocked, neighbor_limit)?;
        Ok(Some(EntityDetail {
            entity,
            meetings,
            neighbors,
        }))
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

/// One transcript segment row in either seal state (Phase 0.5 lock lifecycle). When the folder is
/// open `text_blob` is NULL and `text` is plaintext; when sealed-and-not-unlocked `text` is blank
/// and `text_blob` holds the AES-GCM ciphertext.
#[derive(Debug, Clone)]
pub struct RawSegment {
    pub idx: i64,
    pub text: String,
    pub text_blob: Option<Vec<u8>>,
}

/// A meeting's cached timeline JSON in either seal state (Phase 0.5 lock lifecycle).
#[derive(Debug, Clone)]
pub struct RawTimeline {
    pub data: String,
    pub data_blob: Option<Vec<u8>>,
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
    // Trailing column: the meeting's folder, derived from its note rows (NULL = vault root).
    // Every SELECT feeding this mapper appends a `folder_id` column in this position.
    let folder_id: Option<String> = row.get(7)?;

    Ok(MeetingStatus::from_str(&status_str).map(|status| Meeting {
        id,
        started_at,
        ended_at,
        title,
        duration_s,
        audio_path,
        status,
        folder_id,
    }))
}

/// Escape LIKE wildcards so user input is matched literally (paired with `ESCAPE '\'`).
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// Build a SAFE FTS5 `MATCH` expression from a raw user query.
///
/// FTS5's MATCH grammar treats bare `"`, `*`, `(`, `:`, `^`, `AND`/`OR`/`NOT`, etc. as operators,
/// so feeding raw user text into MATCH can raise "fts5: syntax error near …" and crash the query.
/// We defuse that completely: split the input into Unicode-alphanumeric tokens, drop everything
/// else (punctuation/operators), wrap EACH token in double quotes (an FTS5 string literal — never
/// an operator), and join with implicit AND (whitespace). The result is a conjunction of literal
/// terms, so word ORDER is irrelevant ("alpha beta" matches the same docs as "beta alpha"), and
/// empty / punctuation-only input yields `None` (→ caller returns no hits, never errors).
///
/// A double-quote inside a token is itself escaped by doubling it (`"` → `""`), per FTS5 quoting.
fn fts_match_query(q: &str) -> Option<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in q.chars() {
        if ch.is_alphanumeric() {
            cur.push(ch);
        } else if !cur.is_empty() {
            terms.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        terms.push(cur);
    }
    if terms.is_empty() {
        return None;
    }
    Some(
        terms
            .iter()
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" "),
    )
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
    use super::{escape_like, excerpt, fts_match_query};

    #[test]
    fn escape_like_escapes_wildcards() {
        // '%' → '\%', '_' → '\_', '\' → '\\'
        assert_eq!(escape_like("a%b_c\\d"), "a\\%b\\_c\\\\d");
    }

    #[test]
    fn fts_match_query_quotes_terms_and_drops_operators() {
        // Each alnum token becomes a quoted literal joined by implicit-AND whitespace.
        assert_eq!(fts_match_query("alpha beta"), Some("\"alpha\" \"beta\"".into()));
        // Order is just term order; the conjunction is order-independent at the SQL level.
        assert_eq!(fts_match_query("beta alpha"), Some("\"beta\" \"alpha\"".into()));
        // FTS5 operators / punctuation are stripped, leaving only the literal terms.
        assert_eq!(
            fts_match_query("a* b\"c( AND d:e"),
            Some("\"a\" \"b\" \"c\" \"AND\" \"d\" \"e\"".into())
        );
        // Unicode (Polish) is alphanumeric → preserved as a quoted term.
        assert_eq!(fts_match_query("budżet!"), Some("\"budżet\"".into()));
        // Empty / punctuation-only → None (caller returns no hits, never errors MATCH).
        assert_eq!(fts_match_query(""), None);
        assert_eq!(fts_match_query("   "), None);
        assert_eq!(fts_match_query("\"*():^"), None);
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
            folder_id: None,
        }
    }

    #[test]
    fn migrate_is_idempotent() {
        let db = mem_db();
        db.migrate().unwrap();
        db.migrate().unwrap();
    }

    #[test]
    fn master_paths_round_trip() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        // Default: both NULL (legacy / non-opted meetings → nothing for the seal lifecycle to do).
        assert_eq!(db.get_meeting_master_paths("m1").unwrap(), (None, None));
        db.set_meeting_mic_master_path("m1", Some("/a/m1.mic.wav"))
            .unwrap();
        db.set_meeting_sys_master_path("m1", Some("/a/m1.sys.wav"))
            .unwrap();
        assert_eq!(
            db.get_meeting_master_paths("m1").unwrap(),
            (
                Some("/a/m1.mic.wav".to_string()),
                Some("/a/m1.sys.wav".to_string())
            )
        );
        // Independent columns: clearing one leaves the other.
        db.set_meeting_mic_master_path("m1", None).unwrap();
        assert_eq!(
            db.get_meeting_master_paths("m1").unwrap(),
            (None, Some("/a/m1.sys.wav".to_string()))
        );
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
                speaker: Some("me".into()),
            },
            Segment {
                idx: 1,
                start_s: 1.5,
                end_s: 3.0,
                text: "world".into(),
                speaker: None,
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

        // Speaker attribution round-trips: "me" persists, None reads back as None.
        let read = db.get_segments("m1").unwrap();
        assert_eq!(read[0].speaker.as_deref(), Some("me"));
        assert_eq!(read[1].speaker, None);

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

    use crate::storage::models::Folder;

    fn seed_folder(db: &Db, id: &str, name: &str) {
        db.insert_folder(&Folder {
            id: id.to_string(),
            name: name.to_string(),
            path: name.to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
    }

    fn note_for(db: &Db, meeting_id: &str, provider_id: &str, markdown: &str) {
        db.upsert_note(&NoteRecord {
            meeting_id: meeting_id.to_string(),
            provider_id: provider_id.to_string(),
            markdown: markdown.to_string(),
            created_at: "2026-06-26T09:05:00Z".to_string(),
            exported_path: Some(format!("/vault/{meeting_id}-{provider_id}.md")),
        })
        .unwrap();
    }

    fn folder_id_of<'a>(meetings: &'a [Meeting], id: &str) -> &'a Option<String> {
        &meetings
            .iter()
            .find(|m| m.id == id)
            .expect("meeting present")
            .folder_id
    }

    #[test]
    fn meeting_folder_id_surfaces_in_list_and_search() {
        let db = mem_db();
        seed_folder(&db, "f1", "Secret");
        // Meeting WITH a note moved into a folder.
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "# planning the budget review");
        db.set_meeting_folder("m1", Some("f1")).unwrap();

        // Meeting with a note but NO folder (root) → folder_id None.
        db.insert_meeting(&sample_meeting("m2", "2026-06-24T11:00:00Z"))
            .unwrap();
        note_for(&db, "m2", "claude_code", "# budget at the root level");

        // Meeting with NO note at all → folder_id None (no note rows to derive from).
        db.insert_meeting(&sample_meeting("m3", "2026-06-24T12:00:00Z"))
            .unwrap();

        // list_meetings carries the derived folder_id.
        let listed = db.list_meetings(50).unwrap();
        assert_eq!(folder_id_of(&listed, "m1").as_deref(), Some("f1"));
        assert_eq!(*folder_id_of(&listed, "m2"), None);
        assert_eq!(*folder_id_of(&listed, "m3"), None);

        // get_meeting carries it too.
        assert_eq!(
            db.get_meeting("m1").unwrap().unwrap().folder_id.as_deref(),
            Some("f1")
        );
        assert_eq!(db.get_meeting("m2").unwrap().unwrap().folder_id, None);
        assert_eq!(db.get_meeting("m3").unwrap().unwrap().folder_id, None);

        // search hits carry the derived folder_id on the embedded meeting.
        let hits = db.search("budget", 50).unwrap();
        let h1 = hits.iter().find(|h| h.meeting.id == "m1").expect("m1 hit");
        let h2 = hits.iter().find(|h| h.meeting.id == "m2").expect("m2 hit");
        assert_eq!(h1.meeting.folder_id.as_deref(), Some("f1"));
        assert_eq!(h2.meeting.folder_id, None);
    }

    #[test]
    fn multi_provider_meeting_reports_one_consistent_folder_id() {
        // A meeting re-summarized with TWO providers has two note rows. set_meeting_folder
        // updates BOTH (WHERE meeting_id), so the correlated subselect returns a single,
        // consistent folder regardless of which row it picks.
        let db = mem_db();
        seed_folder(&db, "f1", "Secret");
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "# claude budget note");
        note_for(&db, "m1", "ollama", "# ollama budget note");
        db.set_meeting_folder("m1", Some("f1")).unwrap();

        // Both provider rows carry the same folder.
        let folders: Vec<Option<String>> = {
            let conn = db.lock();
            let mut stmt = conn
                .prepare("SELECT folder_id FROM notes WHERE meeting_id = 'm1' ORDER BY provider_id")
                .unwrap();
            let rows = stmt
                .query_map([], |r| r.get::<_, Option<String>>(0))
                .unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        assert_eq!(folders, vec![Some("f1".to_string()), Some("f1".to_string())]);

        // list_meetings reports a single consistent folder_id (LIMIT 1 subselect, no dup rows).
        let listed = db.list_meetings(50).unwrap();
        let m1_rows: Vec<&Meeting> = listed.iter().filter(|m| m.id == "m1").collect();
        assert_eq!(m1_rows.len(), 1, "one meeting row despite two provider notes");
        assert_eq!(m1_rows[0].folder_id.as_deref(), Some("f1"));

        // Clearing the folder (move to root) clears it for every provider row.
        db.set_meeting_folder("m1", None).unwrap();
        assert_eq!(db.get_meeting("m1").unwrap().unwrap().folder_id, None);
    }

    // ── Phase 1: FTS5/BM25 retrieval ──────────────────────────────────────────

    /// RED-on-LIKE / GREEN-on-FTS: a doc containing BOTH terms is returned for either word order.
    /// The old `LIKE '%alpha beta%'` only matched the contiguous substring, so "beta alpha" missed
    /// it. FTS5 indexes per-token, so order is irrelevant — both queries return the meeting.
    #[test]
    fn fts_word_order_symmetry() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "the alpha and the beta of the plan");

        let fwd = db.search("alpha beta", 50).unwrap();
        assert!(
            fwd.iter().any(|h| h.meeting.id == "m1"),
            "'alpha beta' must match the note containing both terms"
        );
        let rev = db.search("beta alpha", 50).unwrap();
        assert!(
            rev.iter().any(|h| h.meeting.id == "m1"),
            "'beta alpha' must ALSO match (word order is irrelevant under FTS5) — this is the bug \
             the old LIKE substring search had"
        );
    }

    /// Punctuation-only / empty queries must not crash the FTS MATCH parser — they yield no hits.
    #[test]
    fn fts_punctuation_and_empty_queries_are_safe() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "quarterly planning notes");
        // Operators / punctuation that would be FTS5 syntax if passed raw.
        for q in ["", "   ", "\"", "*", "AND OR NOT", "(", ":", "^foo", "a* b\"c("] {
            let hits = db.search(q, 50);
            assert!(hits.is_ok(), "query {q:?} must not error the FTS parser");
        }
        // A real term inside punctuation still matches.
        let hits = db.search("(planning)", 50).unwrap();
        assert!(hits.iter().any(|h| h.meeting.id == "m1"));
    }

    /// Sealed exclusion under FTS: a sealed-and-NOT-session-unlocked meeting does NOT appear in
    /// `search_visible` MATCH results, and once its note plaintext is blanked, its tokens are gone
    /// from the FTS index (the `_au` trigger purged them on the blanking UPDATE).
    #[test]
    fn fts_sealed_meeting_excluded_and_tokens_purged() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("sealed", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "sealed", "claude_code", "ACQUISITION zarządzanie tajemnica");
        db.set_note_folder("sealed", Some("f-locked")).unwrap();

        // Sanity: before sealing, with the folder session-unlocked it IS findable.
        let mut unlocked = std::collections::HashSet::new();
        unlocked.insert("f-locked".to_string());
        let pre = db.search_visible("ACQUISITION", 50, &unlocked).unwrap();
        assert!(pre.iter().any(|h| h.meeting.id == "sealed"));

        // Seal: blank the plaintext markdown (what seal_note does). The `_au` trigger re-syncs FTS.
        db.seal_note("sealed", "claude_code", b"ciphertext-not-real")
            .unwrap();

        // (a) NOT in search_visible when NOT session-unlocked (empty set).
        let nothing = std::collections::HashSet::new();
        let hidden = db.search_visible("ACQUISITION", 50, &nothing).unwrap();
        assert!(
            !hidden.iter().any(|h| h.meeting.id == "sealed"),
            "sealed-not-unlocked meeting leaked into search_visible MATCH results"
        );

        // (b) The token is GONE from the raw FTS note index after blanking (no stale plaintext).
        let raw_match: i64 = {
            let conn = db.lock();
            conn.query_row(
                "SELECT COUNT(*) FROM fts_notes WHERE fts_notes MATCH 'ACQUISITION'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            raw_match, 0,
            "sealed note's tokens must be purged from the FTS index after blanking"
        );

        // (c) Even with the folder session-unlocked, the now-blanked plaintext is no longer in the
        // index, so the term doesn't match — consistent with the at-rest sealed state (the real
        // content only returns after unlock decrypts content_blob back into markdown, which
        // re-fires the FTS trigger).
        let post = db.search_visible("ACQUISITION", 50, &unlocked).unwrap();
        assert!(!post.iter().any(|h| h.meeting.id == "sealed"));
    }

    /// Lock-GATE (not token-purge) closes the FTS title path. A meeting's TITLE is NOT blanked on
    /// seal — the real title stays plaintext-at-rest in `meetings.title` — so its title token
    /// survives in `fts_meetings` and still produces an FTS candidate after sealing. The
    /// `search_visible` visibility predicate, not the `_au` trigger, is what must exclude a
    /// sealed-and-not-session-unlocked meeting matched by its title. (Closes the coverage gap the
    /// lock-security review flagged: every prior sealed-exclusion test matched via the note branch
    /// and/or relied on token-purge with a never-actually-locked folder.)
    #[test]
    fn fts_sealed_meeting_title_match_excluded_by_gate() {
        let db = mem_db();
        let kek = [7u8; 32];
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&Meeting {
            id: "titled".to_string(),
            started_at: "2026-06-24T10:00:00Z".to_string(),
            ended_at: None,
            title: Some("Zebra Quarterly Sync".to_string()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        note_for(&db, "titled", "claude_code", "innocuous body text");
        db.set_note_folder("titled", Some("f-locked")).unwrap();

        // Sanity (folder not yet locked): the title token is findable with an empty unlock set.
        let nothing = std::collections::HashSet::new();
        assert!(
            db.search_visible("Zebra", 50, &nothing)
                .unwrap()
                .iter()
                .any(|h| h.meeting.id == "titled"),
            "title token should be findable before the folder is locked"
        );

        // Lock the folder (locked=1) + seal the note (blank markdown). The TITLE stays plaintext.
        let ck = crate::crypto::random_key().unwrap();
        let wrapped = crate::crypto::encrypt(&kek, &ck, b"").unwrap();
        db.set_folder_locked("f-locked", true, Some(&wrapped)).unwrap();
        let blob = crate::crypto::encrypt(&ck, b"innocuous body text", b"").unwrap();
        db.seal_note("titled", "claude_code", &blob).unwrap();

        // The title token IS still present in the raw FTS meetings index (titles are not blanked) —
        // so exclusion CANNOT come from token-purge here; it must come from the visibility gate.
        let title_in_index: i64 = {
            let conn = db.lock();
            conn.query_row(
                "SELECT COUNT(*) FROM fts_meetings WHERE fts_meetings MATCH 'Zebra'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            title_in_index, 1,
            "sealed meeting's plaintext title token must remain in the FTS index (titles aren't blanked)"
        );

        // GATE must exclude it with an empty unlock set, despite the surviving title token.
        let hidden = db.search_visible("Zebra", 50, &nothing).unwrap();
        assert!(
            !hidden.iter().any(|h| h.meeting.id == "titled"),
            "sealed-not-unlocked meeting leaked via its plaintext TITLE token through the FTS gate"
        );

        // Session-unlocking the folder admits it again (title still matches).
        let mut unlocked = std::collections::HashSet::new();
        unlocked.insert("f-locked".to_string());
        let shown = db.search_visible("Zebra", 50, &unlocked).unwrap();
        assert!(
            shown.iter().any(|h| h.meeting.id == "titled"),
            "session-unlocked sealed meeting should be findable by its title again"
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
            folder_id: None,
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
        let wrapped = crate::crypto::encrypt(kek, &ck, b"").unwrap();
        let notes = db.notes_in_folder(folder_id).unwrap();
        let mut blobs = Vec::new();
        for n in &notes {
            let blob = crate::crypto::encrypt(&ck, n.markdown.as_bytes(), b"").unwrap();
            // Verify decryptable BEFORE blanking (the command's atomicity rule).
            assert_eq!(crate::crypto::decrypt(&ck, &blob, b"").unwrap(), n.markdown.as_bytes());
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
        let ck_bytes = crate::crypto::decrypt(kek, &wrapped, b"").unwrap();
        let ck: [u8; 32] = ck_bytes.as_slice().try_into().unwrap();
        let notes = db.notes_in_folder(folder_id).unwrap();
        for n in &notes {
            let blob = n.content_blob.as_ref().unwrap();
            let pt = crate::crypto::decrypt(&ck, blob, b"").unwrap();
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
        let ck_bytes = crate::crypto::decrypt(&kek, &wrapped, b"").unwrap();
        let ck: [u8; 32] = ck_bytes.as_slice().try_into().unwrap();
        for n in db.notes_in_folder("f1").unwrap() {
            let pt = crate::crypto::decrypt(&ck, n.content_blob.as_ref().unwrap(), b"").unwrap();
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

    // ── Phase 0.5 full-lock helpers (transcript + timeline + audio) ──────────────

    use crate::transcribe::types::Segment;

    /// Seed transcript segments + a cached timeline JSON for a meeting (open state).
    fn seed_transcript_and_timeline(db: &Db, meeting_id: &str, texts: &[&str], timeline_json: &str) {
        let segs: Vec<Segment> = texts
            .iter()
            .enumerate()
            .map(|(i, t)| Segment {
                idx: i as i64,
                start_s: i as f64,
                end_s: (i + 1) as f64,
                text: t.to_string(),
                speaker: if i % 2 == 0 { Some("me".into()) } else { Some("others".into()) },
            })
            .collect();
        db.insert_segments(meeting_id, &segs).unwrap();
        db.set_timeline_data(meeting_id, timeline_json).unwrap();
    }

    /// Mirror of `seal_folder_extras`: seal every governed meeting's transcript + timeline under CK
    /// (verify-before-blank), and the audio file at `audio_path` → `<file>.enc` (then "remove" the
    /// plaintext + re-point audio_path), exactly like the command.
    fn seal_extras(db: &Db, folder_id: &str, ck: &[u8; 32]) {
        for mid in db.meeting_ids_in_folder(folder_id).unwrap() {
            // transcript
            let segs = db.raw_segments(&mid).unwrap();
            for s in &segs {
                if s.text_blob.is_some() && s.text.is_empty() {
                    continue;
                }
                let blob = crate::crypto::encrypt(ck, s.text.as_bytes(), b"").unwrap();
                assert_eq!(crate::crypto::decrypt(ck, &blob, b"").unwrap(), s.text.as_bytes());
                db.seal_segment(&mid, s.idx, &blob).unwrap();
            }
            // timeline
            if let Some(tl) = db.raw_timeline(&mid).unwrap() {
                if !(tl.data_blob.is_some() && tl.data.is_empty()) {
                    let blob = crate::crypto::encrypt(ck, tl.data.as_bytes(), b"").unwrap();
                    db.seal_timeline(&mid, &blob).unwrap();
                }
            }
            // audio
            if let Some(path) = db.get_meeting(&mid).unwrap().and_then(|m| m.audio_path) {
                if !path.ends_with(".enc") && std::path::Path::new(&path).exists() {
                    let enc = format!("{path}.enc");
                    crate::crypto::encrypt_file(
                        ck,
                        std::path::Path::new(&path),
                        std::path::Path::new(&enc),
                        b"",
                    )
                    .unwrap();
                    std::fs::remove_file(&path).unwrap();
                    db.set_meeting_audio_path(&mid, Some(&enc)).unwrap();
                }
            }
        }
    }

    /// Mirror of `unseal_folder_extras`: decrypt transcript + timeline back into plaintext columns
    /// and materialize a playable WAV for the session.
    fn unseal_extras(db: &Db, folder_id: &str, ck: &[u8; 32]) {
        for mid in db.meeting_ids_in_folder(folder_id).unwrap() {
            for s in db.raw_segments(&mid).unwrap() {
                if let Some(blob) = &s.text_blob {
                    let text = String::from_utf8(crate::crypto::decrypt(ck, blob, b"").unwrap()).unwrap();
                    db.restore_segment_text(&mid, s.idx, &text).unwrap();
                }
            }
            if let Some(tl) = db.raw_timeline(&mid).unwrap() {
                if let Some(blob) = &tl.data_blob {
                    let data = String::from_utf8(crate::crypto::decrypt(ck, blob, b"").unwrap()).unwrap();
                    db.restore_timeline_data(&mid, &data).unwrap();
                }
            }
            if let Some(enc) = db.get_meeting(&mid).unwrap().and_then(|m| m.audio_path) {
                if enc.ends_with(".enc") {
                    let plain = enc.trim_end_matches(".enc").to_string();
                    crate::crypto::decrypt_file(
                        ck,
                        std::path::Path::new(&enc),
                        std::path::Path::new(&plain),
                        b"",
                    )
                    .unwrap();
                    db.set_meeting_audio_path(&mid, Some(&plain)).unwrap();
                }
            }
        }
    }

    /// The READ-GATE predicate (`meeting_is_unlocked`): folder open/NULL OR folder id in the
    /// session set. The gated commands return masked/empty content when this is false.
    fn meeting_unlocked(db: &Db, meeting_id: &str, unlocked: &HashSet<String>) -> bool {
        match db.folder_for_meeting(meeting_id).unwrap() {
            None => true,
            Some(fid) => match db.folder_by_id(&fid).unwrap() {
                None => true,
                Some(f) => !f.locked || unlocked.contains(&fid),
            },
        }
    }

    #[test]
    fn seal_transcript_timeline_round_trips_byte_identical() {
        let db = file_db("extras-roundtrip");
        let kek = crate::crypto::random_key().unwrap();
        seed_folder(&db, "f1", "Secret");
        seed_note(&db, "m1", "# note", Some("f1"));
        let texts = ["zażółć gęślą jaźń 🔒", "second segment with budget 1_000_000 EUR", ""];
        let timeline = r#"{"turns":[{"speaker":"me","topic":"secret topic","start_s":0.0,"end_s":1.0}]}"#;
        seed_transcript_and_timeline(&db, "m1", &texts, timeline);
        let ck = crate::crypto::random_key().unwrap();
        // Wrap CK so the folder carries a real wrapped_key (parity with the command).
        let wrapped = crate::crypto::encrypt(&kek, &ck, b"").unwrap();
        db.set_folder_locked("f1", true, Some(&wrapped)).unwrap();

        // SEAL transcript + timeline.
        seal_extras(&db, "f1", &ck);

        // At rest while sealed: plaintext blanked, blobs present, ciphertext does NOT leak.
        let sealed = db.raw_segments("m1").unwrap();
        assert_eq!(sealed.len(), 3);
        for (s, expected) in sealed.iter().zip(texts) {
            assert_eq!(s.text, "", "segment text blanked while sealed");
            assert!(s.text_blob.is_some(), "segment text_blob present");
            if !expected.is_empty() {
                assert!(
                    !contains_subslice(s.text_blob.as_ref().unwrap(), expected.as_bytes()),
                    "segment ciphertext must not leak plaintext"
                );
            }
        }
        let raw_tl = db.raw_timeline("m1").unwrap().unwrap();
        assert_eq!(raw_tl.data, "", "timeline data blanked while sealed");
        assert!(raw_tl.data_blob.is_some());
        assert!(
            !contains_subslice(raw_tl.data_blob.as_ref().unwrap(), timeline.as_bytes()),
            "timeline ciphertext must not leak plaintext"
        );
        // The user-facing reads see blank while sealed.
        assert!(db.get_segments("m1").unwrap().iter().all(|s| s.text.is_empty()));
        assert_eq!(db.get_timeline_data("m1").unwrap().as_deref(), Some(""));

        // UNLOCK → byte-identical round-trip of EVERY segment + the timeline.
        unseal_extras(&db, "f1", &ck);
        let restored = db.get_segments("m1").unwrap();
        let restored_texts: Vec<&str> = restored.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(restored_texts, texts, "transcript round-trips byte-identical");
        assert_eq!(
            db.get_timeline_data("m1").unwrap().as_deref(),
            Some(timeline),
            "timeline round-trips byte-identical"
        );
        // speaker attribution survives (it is not sealed — only text is).
        assert_eq!(restored[0].speaker.as_deref(), Some("me"));
        assert_eq!(restored[1].speaker.as_deref(), Some("others"));
    }

    #[test]
    fn audio_encrypt_decrypt_round_trips_byte_identical() {
        // Encrypt a temp WAV under the CK, assert the plaintext is removed while sealed, decrypt,
        // assert byte-identical (mirrors the audio-at-rest seal lifecycle through the DB helpers).
        let db = file_db("extras-audio");
        let kek = crate::crypto::random_key().unwrap();
        seed_folder(&db, "f1", "Secret");
        seed_note(&db, "m1", "# note", Some("f1"));

        // A temp "WAV" file (opaque bytes — the crypto layer is content-agnostic).
        let wav = temp_db_path("audio").with_extension("wav");
        let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&wav, &payload).unwrap();
        db.set_meeting_audio_path("m1", Some(&wav.to_string_lossy())).unwrap();

        let ck = crate::crypto::random_key().unwrap();
        let wrapped = crate::crypto::encrypt(&kek, &ck, b"").unwrap();
        db.set_folder_locked("f1", true, Some(&wrapped)).unwrap();

        // SEAL → .enc written, plaintext removed, audio_path re-pointed at the .enc.
        seal_extras(&db, "f1", &ck);
        assert!(!wav.exists(), "plaintext WAV removed while sealed");
        let enc_path = db.get_meeting("m1").unwrap().unwrap().audio_path.unwrap();
        assert!(enc_path.ends_with(".enc"), "audio_path points at the encrypted file");
        assert!(std::path::Path::new(&enc_path).exists(), ".enc exists");
        let blob = std::fs::read(&enc_path).unwrap();
        assert!(
            !contains_subslice(&blob, &payload),
            "encrypted audio must not leak plaintext"
        );

        // UNLOCK → plaintext WAV materialized again, byte-identical.
        unseal_extras(&db, "f1", &ck);
        let plain_path = db.get_meeting("m1").unwrap().unwrap().audio_path.unwrap();
        assert!(!plain_path.ends_with(".enc"), "audio_path re-points at the plaintext WAV");
        assert_eq!(
            std::fs::read(&plain_path).unwrap(),
            payload,
            "audio round-trips byte-identical"
        );

        let _ = std::fs::remove_file(&enc_path);
        let _ = std::fs::remove_file(&plain_path);
    }

    #[test]
    fn locked_meeting_detail_is_masked() {
        // get_meeting_detail / get_segments / get_timeline return MASKED/EMPTY + the gate says
        // "locked" when the folder is sealed-and-not-unlocked; full content after the folder id is
        // added to the session unlock set.
        let db = file_db("masked-read");
        let kek = crate::crypto::random_key().unwrap();
        seed_folder(&db, "f1", "Secret");
        seed_note(&db, "m1", "# secret note", Some("f1"));
        seed_transcript_and_timeline(&db, "m1", &["secret words"], r#"{"turns":[]}"#);
        let ck = crate::crypto::random_key().unwrap();
        let wrapped = crate::crypto::encrypt(&kek, &ck, b"").unwrap();
        db.set_folder_locked("f1", true, Some(&wrapped)).unwrap();
        seal_folder(&db, "f1", &kek); // seals the note (markdown)
        // Re-seal extras under the folder's own CK (unwrap the wrapped we just set is the SAME CK
        // the note seal used? No — seal_folder mints its OWN CK). Use the folder's wrapped CK.
        let folder_wrapped = db.folder_wrapped_key("f1").unwrap().unwrap();
        let folder_ck: [u8; 32] = crate::crypto::decrypt(&kek, &folder_wrapped, b"")
            .unwrap()
            .as_slice()
            .try_into()
            .unwrap();
        seal_extras(&db, "f1", &folder_ck);

        let empty: HashSet<String> = HashSet::new();

        // SEALED-not-unlocked → masked: gate says locked, plaintext columns blank.
        assert!(!meeting_unlocked(&db, "m1", &empty), "gate: meeting is locked");
        assert!(
            db.get_segments("m1").unwrap().iter().all(|s| s.text.is_empty()),
            "transcript empty while locked"
        );
        assert_eq!(db.get_timeline_data("m1").unwrap().as_deref(), Some(""));
        assert!(
            db.get_latest_note_for_meeting("m1").unwrap().unwrap().markdown.is_empty(),
            "note markdown blank while locked"
        );

        // SESSION-UNLOCK (add folder id to the set + decrypt back) → full content.
        let mut unlocked = HashSet::new();
        unlocked.insert("f1".to_string());
        session_unlock(&db, "f1", &kek); // note markdown
        unseal_extras(&db, "f1", &folder_ck); // transcript + timeline
        assert!(meeting_unlocked(&db, "m1", &unlocked), "gate: meeting unlocked");
        assert_eq!(db.get_segments("m1").unwrap()[0].text, "secret words");
        assert_eq!(db.get_timeline_data("m1").unwrap().as_deref(), Some(r#"{"turns":[]}"#));
        assert_eq!(
            db.get_latest_note_for_meeting("m1").unwrap().unwrap().markdown,
            "# secret note"
        );
    }

    #[test]
    fn export_audio_refused_when_locked() {
        // The export_audio gate: refuse (Locked) while sealed-not-unlocked; allowed once the
        // folder id is in the session set. Mirrors the `meeting_is_unlocked` early-return.
        let db = file_db("export-audio-gate");
        let kek = crate::crypto::random_key().unwrap();
        seed_folder(&db, "f1", "Secret");
        seed_note(&db, "m1", "# note", Some("f1"));
        let wav = temp_db_path("export-audio").with_extension("wav");
        std::fs::write(&wav, b"RIFF....WAVEfmt fake-pcm").unwrap();
        db.set_meeting_audio_path("m1", Some(&wav.to_string_lossy())).unwrap();
        let ck = crate::crypto::random_key().unwrap();
        let wrapped = crate::crypto::encrypt(&kek, &ck, b"").unwrap();
        db.set_folder_locked("f1", true, Some(&wrapped)).unwrap();
        seal_extras(&db, "f1", &ck);

        let empty: HashSet<String> = HashSet::new();
        // LOCKED → export refused (the command early-returns AppError::Locked when the gate is
        // false). There is also no plaintext WAV on disk to copy.
        assert!(!meeting_unlocked(&db, "m1", &empty), "export refused while locked");
        let enc = db.get_meeting("m1").unwrap().unwrap().audio_path.unwrap();
        assert!(enc.ends_with(".enc"));
        assert!(!std::path::Path::new(enc.trim_end_matches(".enc")).exists());

        // UNLOCKED → allowed.
        unseal_extras(&db, "f1", &ck);
        let mut unlocked = HashSet::new();
        unlocked.insert("f1".to_string());
        assert!(meeting_unlocked(&db, "m1", &unlocked), "export allowed once unlocked");
        let plain = db.get_meeting("m1").unwrap().unwrap().audio_path.unwrap();
        assert!(std::path::Path::new(&plain).exists(), "plaintext WAV available for export");

        let _ = std::fs::remove_file(&enc);
        let _ = std::fs::remove_file(&plain);
    }

    /// Naive subslice search for the leak assertion.
    fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() || needle.len() > haystack.len() {
            return false;
        }
        haystack.windows(needle.len()).any(|w| w == needle)
    }
}

#[cfg(test)]
mod graph_tests {
    //! File-backed (tempfile via `open_with_key` + a FIXED test key) tests for the in-app
    //! self-assembling graph (entities + mentions). These NEVER touch the real Keychain — both
    //! the SQLCipher DEK and the per-folder lock KEK are explicit literals. They exercise the
    //! Sink-A DB helpers + the visibility predicate (the single highest-stakes anti-leak line),
    //! and the Sink-B `locked`-gate (disk-truth, not session-unlock) for the vault stub mirror.

    use super::*;
    use crate::storage::models::{EntityKind, Folder, Meeting, MeetingStatus, NoteRecord};

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn temp_db_path(label: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "meetnotes-graph-test-{}-{}-{}.sqlite",
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

    /// A scratch vault directory for Sink-B `.md` stub assertions (unique per test).
    fn temp_vault(label: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "meetnotes-graph-vault-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn seed_folder(db: &Db, id: &str, name: &str) {
        db.insert_folder(&Folder {
            id: id.to_string(),
            name: name.to_string(),
            path: name.to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
    }

    /// Seed a meeting + one note row, optionally filed into `folder_id`.
    fn seed_note(db: &Db, meeting_id: &str, markdown: &str, folder_id: Option<&str>) {
        db.insert_meeting(&Meeting {
            id: meeting_id.to_string(),
            started_at: format!("2026-06-26T09:00:00Z+{meeting_id}"),
            ended_at: None,
            title: Some(format!("title-{meeting_id}")),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
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

    /// Mirror of `lock_folder`: encrypt each note into a content blob, blank markdown, seal.
    fn seal_folder(db: &Db, folder_id: &str, kek: &[u8; 32]) {
        let ck = crate::crypto::random_key().unwrap();
        let wrapped = crate::crypto::encrypt(kek, &ck, b"").unwrap();
        let notes = db.notes_in_folder(folder_id).unwrap();
        let mut blobs = Vec::new();
        for n in &notes {
            let blob = crate::crypto::encrypt(&ck, n.markdown.as_bytes(), b"").unwrap();
            blobs.push((n.meeting_id.clone(), n.provider_id.clone(), blob));
        }
        db.set_folder_locked(folder_id, true, Some(&wrapped)).unwrap();
        for (mid, pid, blob) in &blobs {
            db.seal_note(mid, pid, blob).unwrap();
        }
    }

    fn unlocked_set(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn entity_dedup() {
        // Same name, different casing, same kind → ONE entity (case-insensitive on name_ci),
        // FIRST-SEEN casing kept. Mentions across distinct meetings accumulate; a repeat mention
        // is idempotent (PK). A same-name DIFFERENT kind is a distinct row.
        let db = file_db("dedup");
        seed_note(&db, "m1", "# note", None);
        seed_note(&db, "m2", "# note", None);

        let id1 = db.upsert_entity("Anna Kowalska", EntityKind::Person).unwrap();
        let id2 = db.upsert_entity("anna kowalska", EntityKind::Person).unwrap();
        assert_eq!(id1, id2, "case-insensitive dedup → same entity id");

        // First-seen casing is preserved (the lowercase re-insert must NOT overwrite it).
        let ent = db.get_entity(&id1).unwrap().unwrap();
        assert_eq!(ent.name, "Anna Kowalska", "first-seen casing kept");

        // Same name, DIFFERENT kind → a distinct entity row (the (name_ci, kind) unique index).
        let proj = db.upsert_entity("Anna Kowalska", EntityKind::Project).unwrap();
        assert_ne!(proj, id1, "same name + different kind = distinct entity");

        // Mentions accumulate across meetings; a duplicate mention is idempotent.
        db.add_mention(&id1, "m1").unwrap();
        db.add_mention(&id1, "m1").unwrap(); // idempotent — no double count
        db.add_mention(&id1, "m2").unwrap();

        let empty = HashSet::new();
        let nodes = db.list_entities_visible(&empty).unwrap();
        let anna = nodes
            .iter()
            .find(|n| n.id == id1)
            .expect("Anna present in visible nodes");
        assert_eq!(anna.mention_count, 2, "two distinct meetings, idempotent repeat");
        assert_eq!(anna.name, "Anna Kowalska");
    }

    #[test]
    fn graph_visibility_filter() {
        // The core anti-leak test. An entity mentioned ONLY in a SEALED folder's meeting is ABSENT
        // from get_graph/list_entities_visible while the folder is locked + not in the unlocked
        // set; it reappears when the folder id IS in the unlocked set. An entity also mentioned in
        // an OPEN meeting keeps only its VISIBLE count (never the true count).
        let db = file_db("visibility");
        let kek = crate::crypto::random_key().unwrap();
        seed_folder(&db, "secret", "Secret");
        seed_note(&db, "open1", "# open", None); // root → always visible
        seed_note(&db, "sealed1", "# sealed", Some("secret"));

        // "Secret Person" mentioned ONLY in the sealed meeting.
        let secret_p = db.upsert_entity("Secret Person", EntityKind::Person).unwrap();
        db.add_mention(&secret_p, "sealed1").unwrap();
        // "Shared Project" mentioned in BOTH the open and the sealed meeting.
        let shared = db.upsert_entity("Shared Project", EntityKind::Project).unwrap();
        db.add_mention(&shared, "open1").unwrap();
        db.add_mention(&shared, "sealed1").unwrap();

        let empty: HashSet<String> = HashSet::new();

        // BEFORE sealing: both entities present; Shared has count 2.
        let before = db.list_entities_visible(&empty).unwrap();
        assert!(before.iter().any(|n| n.id == secret_p));
        assert_eq!(
            before.iter().find(|n| n.id == shared).unwrap().mention_count,
            2
        );
        // An edge exists between the two (they co-occur in sealed1) — pre-seal.
        let (lo, hi) = if secret_p < shared {
            (secret_p.clone(), shared.clone())
        } else {
            (shared.clone(), secret_p.clone())
        };
        assert!(
            db.graph_edges_visible(&empty)
                .unwrap()
                .iter()
                .any(|e| e.source == lo && e.target == hi),
            "co-occurring entities have an edge before sealing"
        );

        // SEAL the folder, session NOT unlocked.
        seal_folder(&db, "secret", &kek);
        let nodes = db.list_entities_visible(&empty).unwrap();
        assert!(
            !nodes.iter().any(|n| n.id == secret_p),
            "entity only in a sealed-not-unlocked meeting must be ABSENT"
        );
        // Shared survives but with VISIBLE count 1 (only the open meeting), never the true 2.
        let shared_node = nodes
            .iter()
            .find(|n| n.id == shared)
            .expect("shared entity still visible via its open meeting");
        assert_eq!(
            shared_node.mention_count, 1,
            "visible count only — sealed mention drops out, never leaks count 2"
        );
        // No edge when the only co-occurrence is sealed.
        assert!(
            db.graph_edges_visible(&empty).unwrap().is_empty(),
            "co-occurrence in a sealed meeting yields no edge"
        );
        // build_graph reflects the same + flags hidden folders.
        let graph = db.build_graph(&empty).unwrap();
        assert!(graph.has_hidden, "a sealed-not-unlocked folder sets has_hidden");
        assert!(!graph.nodes.iter().any(|n| n.id == secret_p));
        // entity_mentions_visible: the secret entity has zero visible backlinks while sealed.
        assert!(db.entity_mentions_visible(&secret_p, &empty).unwrap().is_empty());

        // SESSION-UNLOCK the folder id → the sealed contribution reappears.
        let unlocked = unlocked_set(&["secret"]);
        let nodes_u = db.list_entities_visible(&unlocked).unwrap();
        assert!(
            nodes_u.iter().any(|n| n.id == secret_p),
            "entity reappears once its folder id is in the unlocked set"
        );
        assert_eq!(
            nodes_u.iter().find(|n| n.id == shared).unwrap().mention_count,
            2,
            "both mentions visible again when unlocked"
        );
        // Edge with weight 1 returns (one shared visible meeting).
        let edges_u = db.graph_edges_visible(&unlocked).unwrap();
        assert_eq!(edges_u.len(), 1, "exactly one deduped edge per pair");
        assert_eq!(edges_u[0].weight, 1);
        // build_graph no longer flags hidden (the only locked folder is now unlocked).
        assert!(!db.build_graph(&unlocked).unwrap().has_hidden);
        // The secret entity's visible backlinks return when unlocked.
        assert_eq!(
            db.entity_mentions_visible(&secret_p, &unlocked).unwrap().len(),
            1
        );
    }

    #[test]
    fn dual_sink_skips_vault_when_locked() {
        // Sink B gates on the meeting's folder `locked` flag (DISK truth, NOT session-unlock):
        // a meeting in a locked folder → DB rows still written (Sink A), but ZERO vault `.md`
        // stubs. An OPEN folder → both DB rows AND vault stubs. This mirrors the gate in
        // `commands::build_and_persist_entities` without invoking the LLM provider.
        let db = file_db("dualsink");
        let kek = crate::crypto::random_key().unwrap();
        let vault = temp_vault("dualsink");

        seed_folder(&db, "locked_f", "Locked");
        seed_folder(&db, "open_f", "Open");
        seed_note(&db, "m_locked", "# locked note", Some("locked_f"));
        seed_note(&db, "m_open", "# open note", Some("open_f"));
        seal_folder(&db, "locked_f", &kek); // locked_f now locked=true on disk

        // The dual-sink gate, replicated: Sink A always; Sink B only when folder NOT locked.
        let sink = |meeting_id: &str, person: &str| {
            // Sink A — always persist to the DB.
            let id = db.upsert_entity(person, EntityKind::Person).unwrap();
            db.add_mention(&id, meeting_id).unwrap();
            // Sink B — vault stub only if the meeting's folder is unsealed on disk.
            let folder_locked = match db.get_meeting(meeting_id).unwrap().and_then(|m| m.folder_id) {
                Some(fid) => db.folder_by_id(&fid).unwrap().map(|f| f.locked).unwrap_or(false),
                None => false,
            };
            if !folder_locked {
                crate::export::entity_stub::ensure_entity_backlink(
                    &vault, "People", person, &format!("title-{meeting_id}"),
                )
                .unwrap();
            }
        };

        sink("m_locked", "Locked Person");
        sink("m_open", "Open Person");

        // Sink A: BOTH entities are in the DB regardless of lock state.
        // (Read with the locked folder session-unlocked so both meetings are visible for the
        //  assertion — Sink A wrote rows for both either way.)
        let unlocked = unlocked_set(&["locked_f"]);
        let nodes = db.list_entities_visible(&unlocked).unwrap();
        assert!(
            nodes.iter().any(|n| n.name == "Locked Person"),
            "Sink A: DB row written even for a locked-folder meeting"
        );
        assert!(nodes.iter().any(|n| n.name == "Open Person"));

        // Sink B: the OPEN folder's entity has a vault stub; the LOCKED folder's does NOT.
        let open_stub = vault.join("People").join("Open Person.md");
        let locked_stub = vault.join("People").join("Locked Person.md");
        assert!(open_stub.exists(), "open folder → vault stub written");
        assert!(
            !locked_stub.exists(),
            "locked folder → NO vault stub (no plaintext leak to disk)"
        );
    }

    #[test]
    fn no_vault_configured_db_sink_still_works() {
        // Sink A must work with NO vault: no error, DB rows written, no stubs (no vault dir).
        let db = file_db("novault");
        seed_note(&db, "m1", "# note", None);
        let id = db.upsert_entity("Some Person", EntityKind::Person).unwrap();
        db.add_mention(&id, "m1").unwrap();
        let nodes = db.list_entities_visible(&HashSet::new()).unwrap();
        assert!(nodes.iter().any(|n| n.id == id));
    }

    #[test]
    fn cascade_prunes_mentions_and_entity_drops_out() {
        // delete_meeting cascades to entity_mentions (FK ON DELETE CASCADE); an entity with zero
        // remaining mentions disappears from list_entities_visible (HAVING count > 0).
        let db = file_db("cascade");
        seed_note(&db, "m1", "# note", None);
        let id = db.upsert_entity("Solo Person", EntityKind::Person).unwrap();
        db.add_mention(&id, "m1").unwrap();
        assert!(db
            .list_entities_visible(&HashSet::new())
            .unwrap()
            .iter()
            .any(|n| n.id == id));

        db.delete_meeting("m1").unwrap();
        assert!(
            !db.list_entities_visible(&HashSet::new())
                .unwrap()
                .iter()
                .any(|n| n.id == id),
            "entity with no remaining mentions drops out"
        );
        // The entity row itself remains (orphan), but contributes nothing — harmless.
        assert!(db.get_entity(&id).unwrap().is_some());
    }

    #[test]
    fn entity_detail_neighbors_and_backlinks() {
        // build_entity_detail returns the entity, its visible backlinked meetings, and its top
        // co-occurring neighbors ranked by shared visible meetings.
        let db = file_db("detail");
        seed_note(&db, "m1", "# note", None);
        seed_note(&db, "m2", "# note", None);
        let anna = db.upsert_entity("Anna", EntityKind::Person).unwrap();
        let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
        let bob = db.upsert_entity("Bob", EntityKind::Person).unwrap();
        // Anna+Atlas co-occur in m1 AND m2 (weight 2); Anna+Bob only in m1 (weight 1).
        for (e, m) in [(&anna, "m1"), (&anna, "m2"), (&atlas, "m1"), (&atlas, "m2"), (&bob, "m1")] {
            db.add_mention(e, m).unwrap();
        }
        let detail = db
            .build_entity_detail(&anna, &HashSet::new(), 12)
            .unwrap()
            .unwrap();
        assert_eq!(detail.entity.name, "Anna");
        assert_eq!(detail.meetings.len(), 2, "Anna backlinks m1 + m2");
        assert_eq!(detail.neighbors.first().unwrap().id, atlas, "Atlas is the top neighbor (shared 2)");
        assert_eq!(detail.neighbors.first().unwrap().shared_meetings, 2);
        assert!(detail.neighbors.iter().any(|n| n.id == bob));
        // Unknown id → None.
        assert!(db.build_entity_detail("nope", &HashSet::new(), 12).unwrap().is_none());
    }

    #[test]
    fn entity_detail_hidden_when_only_sealed() {
        // PRIME-DIRECTIVE anti-leak: an entity mentioned ONLY in a sealed-not-unlocked meeting
        // must NEVER surface via get_entity_detail. The leak this guards: the FE held the entity
        // id from a PRIOR open-folder get_graph; the folder is then sealed (or auto-relocked on
        // screen-share); a subsequent get_entity_detail(id) must NOT return the entity — its
        // `name` lived only in the sealed meeting's encrypted markdown. The detail returns None
        // while sealed, and reappears only once the folder id is in the session `unlocked` set.
        let db = file_db("detail-sealed");
        let kek = crate::crypto::random_key().unwrap();
        seed_folder(&db, "secret", "Secret");
        seed_note(&db, "sealed1", "# sealed", Some("secret"));

        let secret_p = db.upsert_entity("Secret Person", EntityKind::Person).unwrap();
        db.add_mention(&secret_p, "sealed1").unwrap();

        let empty: HashSet<String> = HashSet::new();

        // While OPEN: detail is available (sanity — proves the test wires a real, resolvable id).
        let open_detail = db.build_entity_detail(&secret_p, &empty, 12).unwrap();
        assert!(open_detail.is_some(), "open folder → detail resolves");

        // SEAL, session NOT unlocked.
        seal_folder(&db, "secret", &kek);
        assert!(
            db.build_entity_detail(&secret_p, &empty, 12).unwrap().is_none(),
            "entity only in a sealed-not-unlocked meeting must NOT surface via get_entity_detail \
             (its name lived only in the sealed meeting) — must be None, not an empty-backlink shell"
        );

        // SESSION-UNLOCK the folder id → the entity (and its visible backlinks) reappear.
        let unlocked = unlocked_set(&["secret"]);
        let detail = db
            .build_entity_detail(&secret_p, &unlocked, 12)
            .unwrap()
            .expect("entity detail reappears once its folder id is in the unlocked set");
        assert_eq!(detail.entity.name, "Secret Person");
        assert_eq!(detail.meetings.len(), 1, "the (now visible) sealed meeting backlinks");
    }
}
