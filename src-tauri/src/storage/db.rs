use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, Row};

use crate::error::{AppError, Result};
use std::collections::HashSet;

use crate::embed::Embedder;
use crate::storage::models::{
    Analytics, AssistantInteraction, AssistantThreadRow, Commitment, CorrectionRecord, DayCount,
    DocChunkHit, DocumentInfo, EntityDetail, EntityKind, EntityNeighbor, Folder, GraphData,
    GraphEdge, GraphEntity, GraphNode, Meeting, MeetingStatus, NoteFolder, NoteRecord, NoteSummary,
    PendingShareAccept, PersonCard, RecipeRecord, SearchHit, StatusCount, VaultSource,
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

/// One meeting eligible for storage prune: NOT in a locked folder, with its three audio paths.
/// Ordered oldest-first by [`Db::prunable_audio_candidates`]. Any column may be `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunableAudio {
    pub meeting_id: String,
    pub started_at: String,
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
            other => Err(AppError::Storage(format!(
                "unknown meeting status: {other}"
            ))),
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

/// Process-global, one-time registration of the sqlite-vec `vec0` virtual-table module through
/// SQLite's auto-extension hook, so EVERY connection opened afterwards (the main keyed handle, the
/// MCP reader thread, file-backed test DBs) can CREATE and query `vec_chunks`. MUST run BEFORE
/// `Connection::open`: the auto-extension list is consulted at connection-open time, and a module
/// registered after a handle is open does not attach to it (the macOS sqlite-vec #169 footgun).
/// Registering only installs a vtab module + scalar fns — it reads NO database pages — so a caller
/// that runs this immediately before `Connection::open` still has `PRAGMA key` as the first SQL on
/// the keyed handle.
/// Stable FNV-1a 64-bit hash of a chunk's text, stored as `note_chunks.content_hash` so a later
/// incremental re-index can skip unchanged chunks. Deterministic (no `DefaultHasher` seed).
fn chunk_hash(text: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn register_vec_extension() {
    use std::sync::Once;
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        // SAFETY: `sqlite3_vec_init` has the C `xEntryPoint` ABI that `sqlite3_auto_extension`
        // expects; the transmute reinterprets the bare fn pointer as that signature (the exact
        // wiring proven by the de-risking spike). sqlite-vec only installs a virtual-table module
        // + scalar functions, so no page is read and the bundled SQLCipher key flow is untouched.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut std::os::raw::c_char,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> std::os::raw::c_int,
            >(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
    });
}

// ── Egress ledger summary types (Phase 6) ───────────────────────────────────────────────────────
//
// Internal aggregate types returned by `Db::egress_summary`. The IPC-facing DTOs in `commands.rs`
// hold a 1:1 mapping of these (camelCase on the wire). No content fields — counts/ids/labels only.

/// Per-model token-usage roll-up within a time window.
///
/// Fields are `u64` (not `u32`) so an all-time SUM over a large egress history cannot
/// silently overflow (a single meeting can use ~100 k tokens; 2^32 ≈ 4 M tokens total).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressModelUsage {
    pub model: String,
    pub calls: u64,
    pub tokens: u64,
}

/// Per-day token-usage roll-up within a time window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressDayUsage {
    /// ISO-8601 date string ("YYYY-MM-DD") in UTC.
    pub day: String,
    pub tokens: u64,
}

/// Summed redaction counts over all rows in the time window.
///
/// Fields are `u64` so a large corpus cannot truncate the aggregate (same rationale as the
/// token totals above).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EgressRedactionTotals {
    pub email: u64,
    pub card: u64,
    pub phone: u64,
    pub name: u64,
}

/// One recent row from `egress_log` (last ≤20, newest first), content-free.
#[derive(Debug, Clone)]
pub struct EgressRecentRow {
    pub ts: i64,
    pub provider_id: String,
    pub destination: String,
    pub model_served: Option<String>,
    pub total_tokens: Option<u32>,
    pub redactions: EgressRedactionTotals,
}

/// Full aggregated egress ledger for a rolling time window. Returned by `Db::egress_summary`.
/// Handles an empty table gracefully — all totals are zero, all vecs are empty.
#[derive(Debug, Clone)]
pub struct EgressLedger {
    pub total_calls: u64,
    pub total_tokens: u64,
    pub by_model: Vec<EgressModelUsage>,
    pub by_day: Vec<EgressDayUsage>,
    pub total_redactions: EgressRedactionTotals,
    /// Last ≤20 rows, newest first.
    pub recent: Vec<EgressRecentRow>,
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
        // Phase 2a: install the sqlite-vec `vec0` virtual-table module via SQLite's auto-extension
        // hook BEFORE opening the connection (the list is consulted at open time; registering after
        // open does not attach to that handle — the macOS #169 footgun). Idempotent + page-free, so
        // `PRAGMA key` remains the first SQL to touch the keyed handle.
        register_vec_extension();
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
             CREATE INDEX IF NOT EXISTS idx_entities_kind ON entities(kind);
             CREATE TABLE IF NOT EXISTS correction_log (
               id INTEGER PRIMARY KEY,
               kind TEXT NOT NULL,
               input TEXT NOT NULL,
               model_output TEXT NOT NULL,
               final_output TEXT,
               accepted INTEGER NOT NULL DEFAULT 0,
               owner_id TEXT NOT NULL DEFAULT 'local',
               created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_correction_log_kind_created
               ON correction_log(kind, created_at);
             CREATE TABLE IF NOT EXISTS notes_asides (
               id INTEGER PRIMARY KEY,
               meeting_id TEXT NOT NULL,
               text TEXT NOT NULL,
               created_at TEXT NOT NULL,
               FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_notes_asides_meeting ON notes_asides(meeting_id);
             CREATE TABLE IF NOT EXISTS assistant_interactions (
               id INTEGER PRIMARY KEY,
               meeting_id TEXT NOT NULL,
               command TEXT NOT NULL,
               answer TEXT NOT NULL,
               citations TEXT NOT NULL DEFAULT '[]',
               status TEXT NOT NULL,
               source_label TEXT,
               created_at TEXT NOT NULL,
               FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_assistant_interactions_meeting
               ON assistant_interactions(meeting_id);
             -- Brain v2 L4: CRASH-RECOVERY row for the incremental live bullets of a recording in
             -- progress (`transcribe::bullets`). RAM (`AppState::live_bullets`) is authoritative
             -- during the recording; this row lets a crash-salvaged meeting still feed its bullets
             -- into the Stop-time note (`SummarizeRequest::live_bullets`). DERIVED meeting content
             -- (the L2 lesson): PURGED on every seal path (`purge_chunks_for_meetings`,
             -- `blank_sealed_notes_in_folders`, `reblank_locked_folders_at_rest`,
             -- `discard_folder_seal`) and on `delete_meeting` (explicit + FK CASCADE); the write
             -- refuses in-tx when the meeting is sealed at rest (`upsert_live_bullets`); consumed
             -- + cleared by the note pipeline at Stop. Never read by an FE command.
             CREATE TABLE IF NOT EXISTS live_bullets (
               meeting_id TEXT PRIMARY KEY,
               bullets_md TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
             );
             -- brain2 R2: the BITEMPORAL FACTS layer. One row per entity·predicate·object
             -- assertion with TWO time axes: valid_from/valid_to (valid time — when the fact was
             -- true; valid_to NULL = currently valid, set when superseded) and recorded_at
             -- (transaction time — when we learned it). Superseded facts are CLOSED (valid_to set),
             -- never deleted, so history is preserved. `meeting_id` is the gating + purge anchor:
             -- facts are DERIVED content (like note_chunks / correction_log / assistant_interactions)
             -- → PURGED on seal in the same atomic tx and visibility-gated on every read. FK CASCADE
             -- on both entity_id and meeting_id so a deleted entity/meeting drops its facts.
             CREATE TABLE IF NOT EXISTS facts (
               id TEXT PRIMARY KEY,
               entity_id TEXT NOT NULL,
               subject TEXT NOT NULL,
               predicate TEXT NOT NULL,
               object TEXT NOT NULL,
               valid_from TEXT NOT NULL,
               valid_to TEXT,
               recorded_at TEXT NOT NULL,
               meeting_id TEXT,
               confidence REAL NOT NULL DEFAULT 1.0,
               FOREIGN KEY (entity_id) REFERENCES entities(id) ON DELETE CASCADE,
               FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_facts_entity ON facts(entity_id);
             CREATE INDEX IF NOT EXISTS idx_facts_meeting ON facts(meeting_id);
             -- CROSS-MEETING USER MEMORY (Phase 3). The SAME bitemporal shape as `facts`
             -- (subject·predicate·object with valid_from/valid_to — invalidate-not-delete via
             -- reconcile) but USER-SCOPED, not entity-scoped: these are durable facts/preferences/
             -- commitments about the USER accumulated across ALL meetings (prefers Polish replies,
             -- works on Project Atlas, deadline Q3 = 15.09). Deliberately a SEPARATE table (no
             -- entity FK) so the entity-graph fact reads (`list_facts_visible`) can NEVER surface a
             -- user fact and vice-versa. `meeting_id` is the provenance + gating + purge anchor:
             -- user facts are DERIVED content tied to the meeting they were learned from, so — exactly
             -- like `facts` / `note_chunks` / `correction_log` — they are PURGED on seal in the same
             -- atomic tx (`purge_user_facts_tx`) and every USER-FACING read is visibility-gated
             -- (`list_user_facts_visible`). FK CASCADE on meeting_id so a deleted meeting drops its
             -- user facts too. NULL meeting_id (legacy/imperative-without-source) reads back as NOT
             -- visible (fail-closed via the INNER JOIN in the gated reader).
             CREATE TABLE IF NOT EXISTS user_facts (
               id TEXT PRIMARY KEY,
               subject TEXT NOT NULL,
               predicate TEXT NOT NULL,
               object TEXT NOT NULL,
               valid_from TEXT NOT NULL,
               valid_to TEXT,
               recorded_at TEXT NOT NULL,
               meeting_id TEXT,
               confidence REAL NOT NULL DEFAULT 1.0,
               FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_user_facts_meeting ON user_facts(meeting_id);

             -- On-device VOICE BIOMETRICS for diarized remote speakers (opt-in, default off). One row
             -- per diarized others-{cluster_index} cluster of a meeting: the L2-normalized CAM++
             -- speaker embedding (little-endian f32 BLOB), plus the bound person `label` once the user
             -- enrolls the cluster by rename (NULL until then). `meeting_id` is the provenance +
             -- gating + purge anchor: a voiceprint is DERIVED content of the meeting it was captured
             -- from, so — exactly like `user_facts` / `facts` / `note_chunks` — it is PURGED on seal in
             -- the same atomic tx (`purge_speaker_voiceprints_tx`) and every read is visibility-gated
             -- (`list_voiceprints_visible`). A voiceprint derived from a sealed meeting must NEVER be
             -- read or matched. FK CASCADE on meeting_id so a deleted meeting drops its voiceprints.
             -- PRIVACY: these embeddings are never egressed; capturing a non-consenting participant's
             -- voiceprint is an explicit opt-in (untested under BIPA/CIPA).
             CREATE TABLE IF NOT EXISTS speaker_voiceprints (
               id TEXT PRIMARY KEY,
               meeting_id TEXT NOT NULL,
               cluster_index INTEGER NOT NULL,
               label TEXT,
               dim INTEGER NOT NULL,
               embedding BLOB NOT NULL,
               created_at TEXT NOT NULL,
               FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_voiceprints_meeting ON speaker_voiceprints(meeting_id);

             -- Re-Truth (the vault heals itself). One row per SUPERSESSION: a fact asserted in
             -- `superseding_meeting_id` invalidated an older fact sourced in `source_meeting_id`.
             -- Surfaced for REVIEW; on apply we APPEND an Obsidian callout to the source note
             -- (append-only = never mangling prose) and snapshot the exact pre-image bytes of each
             -- stamped note (`*_pre_image`) so undo restores them byte-identical. `applied_at` NULL =
             -- pending; a stamp instant once applied. DELIBERATELY carries NO foreign key: a row
             -- references TWO meetings, so a single FK can't cover both — `delete_meeting` purges
             -- rows referencing EITHER meeting via `purge_supersessions_tx` instead. The pre-images
             -- hold plaintext note bytes, so a sealed source contributes NONE (the read command is
             -- folder-lock + unlock gated, and rows are only ever recorded for open-folder sources).
             CREATE TABLE IF NOT EXISTS supersessions (
               id TEXT PRIMARY KEY,
               superseding_meeting_id TEXT NOT NULL,
               source_meeting_id TEXT NOT NULL,
               entity TEXT NOT NULL,
               predicate TEXT NOT NULL,
               old_value TEXT NOT NULL,
               new_value TEXT NOT NULL,
               created_at TEXT NOT NULL,
               applied_at TEXT,
               source_pre_image BLOB,
               superseding_pre_image BLOB
             );
             CREATE INDEX IF NOT EXISTS idx_supersessions_superseding
               ON supersessions(superseding_meeting_id);
             CREATE INDEX IF NOT EXISTS idx_supersessions_source
               ON supersessions(source_meeting_id);",
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
        // Phase F0 LOCK-SAFETY: link each correction-log example to the meeting it derived from, so
        // the gated reader can join meetings→folders for a visibility check and the seal/delete paths
        // can purge a sealed meeting's rows. Guarded ALTER (idempotent); NULL for legacy/unattributed
        // rows, which the gated `list_corrections` treats as NOT visible (fail-closed). `folder_id` is
        // DERIVED via the meetings/notes join, never stored.
        Self::add_column_if_missing(&conn, "correction_log", "meeting_id", "TEXT")?;
        // Phase 0.5 — full per-folder lock (defense-in-depth beyond SQLCipher-at-rest):
        // sealed-folder transcripts + timelines carry an AES-GCM blob under the folder CK while
        // sealed; the plaintext `text`/`data` column is blanked. Reversed on session-unlock.
        // Guarded ALTER (idempotent); NULL for every row in an open folder.
        Self::add_column_if_missing(&conn, "segments", "text_blob", "BLOB")?;
        Self::add_column_if_missing(&conn, "timelines", "data_blob", "BLOB")?;
        // Tier 3b/A — per-segment ASR confidence (mean token probability × (1−no_speech), computed on
        // the Accurate batch path). Guarded ALTER (idempotent, ADDITIVE); NULL for every legacy row
        // and for `Fast`-path rows → reads back as `confidence: None`. NON-CONTENT metadata (a
        // probability, never words): seal-blanking (`UPDATE segments SET text=''`) leaves it, exactly
        // like `start_s`/`end_s`/`speaker`, so it never survives as leaked content.
        Self::add_column_if_missing(&conn, "segments", "confidence", "REAL")?;
        // Rec #3: faithful per-stream float32 MASTER archives (mic native + system 48k), opt-in.
        // Each is sealed at rest exactly like `audio_path` (→ `<file>.enc`). NULL for every meeting
        // recorded without `keep_hires_masters` → zero change for existing / non-opted users. These
        // live OFF the `Meeting` struct (targeted reads only) so they can never leak through a
        // masked DTO; they are export-only via gated commands.
        Self::add_column_if_missing(&conn, "meetings", "mic_master_path", "TEXT")?;
        Self::add_column_if_missing(&conn, "meetings", "sys_master_path", "TEXT")?;
        // brain2 realtime notes: the user's free-text notes typed DURING a meeting — the live
        // editable buffer, ONE per meeting, and the DURABLE CANONICAL STORE of those typed notes.
        // It is (a) injected into the in-meeting brain's system prompt while recording, and (b)
        // re-read on EVERY (re)summarize and FOLDED into the note (`## My notes`) — so a resummarize
        // never drops the typed notes. The buffer is USER-AUTHORED PRIMARY content, so it is
        // SEALED-AND-RESTORED exactly like the note markdown / transcript / timeline (NEVER
        // blanked-and-lost): on lock the plaintext is encrypted under the folder CK →
        // `manual_notes_blob` with VERIFY-BEFORE-DESTROY, then the plaintext is blanked; on
        // session-unlock / remove-lock the blob is decrypted back. The plaintext is re-blanked on
        // relock/reconcile ONLY when the blob exists (never destroy the only copy). Guarded ALTER
        // (idempotent); NULL for every legacy meeting → reads back as "" (zero behavior change). Kept
        // OFF the `Meeting` struct + every meeting SELECT so it can never leak through a masked DTO —
        // read only via the gated commands.
        Self::add_column_if_missing(&conn, "meetings", "manual_notes", "TEXT")?;
        Self::add_column_if_missing(&conn, "meetings", "manual_notes_blob", "BLOB")?;
        // Phase 5 — per-note model provenance. Three additive TEXT columns (NULL-safe: legacy notes
        // that predate this migration read back as `None` via `row_to_note`; `upsert_note` persists
        // them when the pipeline passes them in). Content-free: these are model IDs / host strings,
        // never transcript or note text, so they are NOT sealed/blanked during folder-lock — they
        // ride the SQLCipher-at-rest layer only.
        Self::add_column_if_missing(&conn, "notes", "model_requested", "TEXT")?;
        Self::add_column_if_missing(&conn, "notes", "model_served", "TEXT")?;
        Self::add_column_if_missing(&conn, "notes", "gateway_host", "TEXT")?;
        // @brain THREADS: scope each persisted assistant exchange to a conversation thread.
        // `thread_id` is an OPAQUE id (FE-supplied for an @brain thread, backend-generated UUID for
        // the voice/wake path); `anchor_text` is the note text an @brain thread was anchored to
        // (row content, purged on seal with the rest of the row — see
        // `purge_assistant_interactions_tx`). Guarded ALTERs (idempotent); NULL for legacy rows,
        // which the gated thread reader EXCLUDES (`list_assistant_threads_visible`).
        Self::add_column_if_missing(&conn, "assistant_interactions", "thread_id", "TEXT")?;
        Self::add_column_if_missing(&conn, "assistant_interactions", "anchor_text", "TEXT")?;
        // Phase 1 — FTS5/BM25 full-text retrieval over the three text sources
        // (meeting titles, transcript segments, note markdown). Replaces the prior
        // substring LIKE search so word-order/term retrieval works ("alpha beta" == "beta alpha")
        // and ranking uses bm25(). SQLCipher is built with FTS5 compiled in (bundled-sqlcipher) —
        // ZERO new deps. Runs on the same locked connection as the rest of migrate().
        Self::migrate_fts(&conn)?;
        // Brain v2 L2.2 — relevance-filtered memory brief: external-content FTS5 over `user_facts`
        // (subject/predicate/object) kept in sync by the same _ai/_ad/_au trigger trio as the other
        // FTS tables. Additive + guarded so migrate() stays idempotent. Lock model: the index only
        // mirrors `user_facts`, which is PURGED on seal via a direct DELETE (`purge_user_facts_tx` /
        // `reblank_locked_folders_at_rest`) — that DELETE fires the _ad trigger, so no sealed
        // fact's tokens survive in the index; every READ goes through the gated
        // `search_user_facts_visible` (same visibility predicate as `list_user_facts_visible`).
        Self::migrate_user_facts_fts(&conn)?;
        // Brain v2 L2.1 — memory consolidation tables (see `crate::memory`). Additive + guarded.
        Self::migrate_memory(&conn)?;
        // Brain v2 L5 — scheduled-brief tables (see `crate::brief_runner`) + the MCP-server config
        // table (see `crate::connectors::mcp`). Additive + guarded so migrate() stays idempotent.
        Self::migrate_briefs(&conn)?;
        Self::migrate_mcp_servers(&conn)?;
        // Phase 2a — vector retrieval layer (note_chunks + the vec0 KNN table). Additive + guarded
        // (CREATE TABLE / CREATE VIRTUAL TABLE IF NOT EXISTS) so migrate() stays idempotent.
        Self::migrate_vector(&conn)?;
        // Brain v2 L1.2 — contextual augmentation: the AUGMENTED text a note chunk was embedded
        // from ("<title> | <date> | <attendees> | <facts>\n<raw>"). The raw `text` column stays for
        // snippets. Additive + guarded; NULL on legacy rows (they re-fill on the next re-index).
        Self::add_column_if_missing(&conn, "note_chunks", "aug_text", "TEXT")?;
        // Brain v2 L1.1 — topic-segment retrieval layer (topic_chunks + its vec0 KNN table + its
        // external-content FTS5 index). Additive + guarded so migrate() stays idempotent. Lock
        // model: topic chunks are plaintext DERIVED from transcript segments (like note_chunks),
        // so they exist ONLY for visible content — purged in the SAME `purge_chunks_tx` choke
        // point that covers note_chunks on every seal path.
        Self::migrate_topic(&conn)?;
        // Document ingestion — PARALLEL doc tables (documents + doc_chunks + the doc_vec0 KNN table),
        // deliberately separate from note_chunks so the load-bearing meeting-gating joins stay
        // untouched. Additive + guarded so migrate() stays idempotent.
        Self::migrate_documents(&conn)?;
        // NOTES feature — AUTHORED `documents(kind='note')` rows gain an authoring layer over the
        // existing document substrate (seal/gate/brain-index reused verbatim). Three additive,
        // guarded columns (idempotent; NULL-safe for every legacy row):
        //   • `title`         — the display title (may contain spaces/emoji); NULL ⇒ fall back to
        //                        `name` (the filesystem-safe slug). NON-CONTENT metadata, but it CAN
        //                        reveal the topic → the gated list/get DTOs MASK it ("🔒 Locked") for
        //                        a sealed-not-unlocked note, exactly like a masked meeting title.
        //   • `updated_at`    — epoch-ms last-edit time; NULL ⇒ fall back to `created_at`.
        //   • `exported_path` — the vault `.md` path (NULL when never exported / sealed). Captured
        //                        before lock so the seal path can delete the on-disk `.md` (mirrors
        //                        `notes.exported_path`), and re-set on unlock/remove-lock re-export.
        // The `text` column still stores the FULL markdown incl. YAML front-matter (owned-file). These
        // columns are non-content and are NOT sealed/blanked — they ride the SQLCipher-at-rest layer;
        // only `text` is sealed (kind-agnostic document seal leg).
        Self::add_column_if_missing(&conn, "documents", "title", "TEXT")?;
        Self::add_column_if_missing(&conn, "documents", "updated_at", "INTEGER")?;
        Self::add_column_if_missing(&conn, "documents", "exported_path", "TEXT")?;
        // NOTES feature — separate the Notes folder tree from the Meetings tree. `kind` defaults
        // 'meeting' so every existing folder + all meeting behavior stays byte-identical; note
        // folders are created with kind='note'. Lock/seal/CK machinery is folder-id-keyed and
        // kind-agnostic → reused verbatim. Additive + guarded (idempotent).
        Self::add_column_if_missing(&conn, "folders", "kind", "TEXT NOT NULL DEFAULT 'meeting'")?;
        // Phase 2b — content-free egress audit log. One row per cloud provider call written by
        // `DbEgressSink`. The table carries ONLY counts, ids, labels, byte sizes, and token counts —
        // NEVER transcript, prompt, scrubbed values, API keys, or any meeting content (§8: no PII
        // in logs). Additive + guarded so migrate() stays idempotent.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS egress_log (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               ts INTEGER NOT NULL,
               provider_id TEXT NOT NULL,
               destination TEXT NOT NULL,
               model_requested TEXT,
               model_served TEXT,
               call_kind TEXT NOT NULL,
               prompt_tokens INTEGER,
               completion_tokens INTEGER,
               total_tokens INTEGER,
               cached_tokens INTEGER,
               redactions_email INTEGER NOT NULL DEFAULT 0,
               redactions_card INTEGER NOT NULL DEFAULT 0,
               redactions_phone INTEGER NOT NULL DEFAULT 0,
               redactions_name INTEGER NOT NULL DEFAULT 0,
               system_bytes INTEGER NOT NULL DEFAULT 0,
               user_bytes INTEGER NOT NULL DEFAULT 0,
               meeting_id TEXT
             );",
        )
        .map_err(map_err)?;
        // Keyword (FTS5/BM25) retrieval over doc chunks so documents/brain notes are reachable on a
        // DEFAULT install (no e5 model, semantic flag off). Additive + guarded so migrate() stays
        // idempotent. Runs AFTER migrate_documents (the triggers reference doc_chunks).
        Self::migrate_doc_fts(&conn)?;

        // TIER 1: one-time flip of the on-device semantic-search default to ON for the INSTALLED base
        // (fresh installs already default ON via `AppConfig::default()`; this reaches DBs that persisted
        // the historical default-off). Sentinel-guarded so it runs EXACTLY once and never re-fires — a
        // user who turns semantic search off AFTER this migration stays off. Uses the HELD `conn`
        // directly: calling self.get_setting/set_setting here would re-lock `self.lock()` and DEADLOCK.
        // Config-only (a settings key), additive, idempotent, and fully reversible via the Settings
        // toggle — it touches NO meeting content, crypto, or seal state.
        let semantic_default_applied: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'semantic_default_v1'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_err)?;
        if semantic_default_applied.is_none() {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('semantic_search_enabled', 'true')
                 ON CONFLICT(key) DO UPDATE SET value = 'true'",
                [],
            )
            .map_err(map_err)?;
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('semantic_default_v1', '1')",
                [],
            )
            .map_err(map_err)?;
        }

        // M3-CLIENT (spec §7) — local bookkeeping for OUTBOUND server shares. Stores ONLY the
        // client-minted `share_id` + the local `meeting_id` (+ mode/rev/state/ts). There is
        // DELIBERATELY NO title column: the share list derives the title via the GATED meeting read
        // (`meeting_is_unlocked`), so a sealed meeting's title can never leak from this table. It
        // never holds the link key `L`, `NK`, ciphertext, or any note text — those live server-side
        // (encrypted) and in the URL fragment (which is never persisted here). Additive + guarded so
        // migrate() stays idempotent.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS outbound_shares (
               share_id   TEXT PRIMARY KEY,
               meeting_id TEXT NOT NULL,
               mode       TEXT NOT NULL,
               rev        INTEGER NOT NULL DEFAULT 1,
               state      TEXT NOT NULL DEFAULT 'active',
               created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_outbound_shares_meeting ON outbound_shares(meeting_id);
             -- Content-free egress ledger for SHARE uploads (§7 inv. 4): host + byte sizes only.
             -- NEVER the share URL, the fragment key L, a title, or any note text.
             CREATE TABLE IF NOT EXISTS share_egress_log (
               id         INTEGER PRIMARY KEY AUTOINCREMENT,
               ts         INTEGER NOT NULL,
               host       TEXT NOT NULL,
               kind       TEXT NOT NULL,
               byte_count INTEGER NOT NULL DEFAULT 0
             );",
        )
        .map_err(map_err)?;

        // M5-CLIENT (spec §4.8/§7) — Murmur↔Murmur (mode B) local bookkeeping.
        //
        // `pinned_contacts` = TOFU key pins. Keyed on a STABLE `account_id` (NOT email, so a future
        // email change doesn't strand the pin — spec §4.8) with the safety-word `fingerprint` as the
        // pinned VALUE, so a CHANGED fingerprint for a known contact is detectable → BLOCKING re-verify
        // (never click-through). Carries no key bytes, no note content.
        //
        // `inbound_shares` = the idempotency + provenance record for an ACCEPTED share (share_id →
        // local meeting_id). A re-accept of the same share_id is a no-op (never a duplicate vault note).
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS pinned_contacts (
               account_id  TEXT PRIMARY KEY,
               email       TEXT,
               fingerprint TEXT NOT NULL,
               pinned_at   TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS inbound_shares (
               share_id       TEXT PRIMARY KEY,
               meeting_id     TEXT NOT NULL,
               sender_acct_id TEXT,
               accepted_at    TEXT NOT NULL
             );
             -- Durable RESUME record for a mode-B accept whose server row was flipped to `accepted`
             -- but whose local verify+ingest has not yet committed (spec §7). Written between the
             -- server flip and the vault write, dropped the instant ingest commits — so a post-flip
             -- failure is recoverable (the server no longer lists an accepted share; a re-accept
             -- 404s), never a stranded share. All bytes are opaque + SQLCipher-encrypted at rest.
             CREATE TABLE IF NOT EXISTS pending_share_accepts (
               share_id         TEXT PRIMARY KEY,
               blob_id          TEXT NOT NULL,
               target_folder_id TEXT NOT NULL,
               sender_user_id   TEXT NOT NULL,
               sender_fingerprint TEXT NOT NULL,
               wrapped_key      BLOB NOT NULL,
               grant_sig        BLOB NOT NULL,
               rev              INTEGER NOT NULL,
               key_generation   INTEGER NOT NULL,
               created_at       TEXT NOT NULL
             );",
        )
        .map_err(map_err)?;
        // Additive columns on `outbound_shares` for mode B (spec §7 schema: `nk BLOB`,
        // `recipient_acct_id?`). The retained NK + `content_hash` let `share_rewrap_pending` re-wrap
        // to a newly-registered recipient WITHOUT re-reading meeting content (only key material).
        // Guarded so migrate() stays idempotent.
        //
        // `nk` (legacy, pre-0.7): the retained NK stored RAW, protected only by the whole-DB
        // SQLCipher DEK — a re-locked live session could still decrypt an already-shared envelope from
        // it. `nk_wrapped` (0.7 security fast-follow): the retained NK wrapped under the account MK
        // (`e2ee::wrap_key32`, share-scoped AAD). New shares write `nk_wrapped` and leave `nk` NULL;
        // legacy rows keep their raw `nk` and still re-wrap (unwrap = identity). ADDITIVE only — no
        // DROP, no rewrite of existing rows.
        Self::add_column_if_missing(&conn, "outbound_shares", "nk", "BLOB")?;
        Self::add_column_if_missing(&conn, "outbound_shares", "nk_wrapped", "BLOB")?;
        Self::add_column_if_missing(&conn, "outbound_shares", "recipient_acct_id", "TEXT")?;
        Self::add_column_if_missing(&conn, "outbound_shares", "recipient_email", "TEXT")?;
        Self::add_column_if_missing(&conn, "outbound_shares", "content_hash", "BLOB")?;
        // NOTES sharing (WP6): a share can anchor on an authored NOTE instead of a meeting. Additive
        // nullable — a note share stores its `documents(kind='note')` id here and leaves the NOT NULL
        // `meeting_id` as '' (empty) so the meeting-title join in `list_my_shares` skips it and
        // resolves the NOTE title (gated on the note's folder) instead. Legacy meeting shares keep
        // `document_id` NULL → byte-identical behavior.
        Self::add_column_if_missing(&conn, "outbound_shares", "document_id", "TEXT")?;

        // Re-Truth: guarded ALTERs for the supersession undo pre-images. The columns are also in the
        // `CREATE TABLE IF NOT EXISTS supersessions` above (so a fresh DB gets them there and these are
        // no-ops), but a dev DB that created the table in an EARLIER iteration of this branch — before
        // the pre-image columns existed — would otherwise lack them and error "no such column" in
        // `store_supersession_pre_images`. Additive + idempotent, per the migration rule.
        Self::add_column_if_missing(&conn, "supersessions", "source_pre_image", "BLOB")?;
        Self::add_column_if_missing(&conn, "supersessions", "superseding_pre_image", "BLOB")?;

        // WS8 / capture-default: one-time flip of the SYSTEM-AUDIO-CAPTURE default to ON for the
        // INSTALLED base (fresh installs already default ON via `AppConfig::default().capture_system_audio`
        // — the default flipped OFF→ON in #167). This reaches DBs that persisted the historical 'false'.
        // RATIONALE: that stored 'false' comes from onboarding round-tripping the OLD (pre-#167) default,
        // NOT a deliberate opt-out — so, exactly as the `semantic_default_v1` precedent did for a
        // historical default, we flip it ON once. Sentinel-guarded (`capture_default_v1`) so it runs
        // EXACTLY once and never re-fires: a user who turns capture OFF *after* this migration stays off.
        // Uses the HELD `conn` directly (self.get_setting/set_setting would re-lock `self.lock()` and
        // DEADLOCK). Config-only settings key, additive, idempotent, and reversible via Settings → Audio
        // — it touches NO meeting content, crypto, or seal state.
        let capture_default_applied: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'capture_default_v1'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_err)?;
        if capture_default_applied.is_none() {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('capture_system_audio', 'true')
                 ON CONFLICT(key) DO UPDATE SET value = 'true'",
                [],
            )
            .map_err(map_err)?;
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('capture_default_v1', '1')",
                [],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    /// Idempotent DOCUMENT-ingestion schema, kept PARALLEL to the meeting note layer so the
    /// load-bearing meeting-gating joins (`note_chunks` ↔ `meetings`) are never destabilized.
    ///
    /// - `documents` — one uploaded md/txt per row, ANCHORED to a `folders` row (the lock gate). The
    ///   plaintext `text` mirrors the note `markdown`: it is BLANKED while the folder is sealed and
    ///   the AES-GCM copy lives in `text_blob` (sealed under the folder CK, restored on unlock /
    ///   remove-lock) — SEALED-AND-RESTORED exactly like the note `content_blob` / `manual_notes`.
    /// - `doc_chunks` — plaintext chunks DERIVED from a document's text (the embed source + snippet
    ///   store), 1:1 with `doc_vec_chunks` by `id`. CASCADE on the parent document.
    /// - `doc_vec_chunks` — the `vec0` KNN table whose `chunk_id` mirrors `doc_chunks.id`.
    ///
    /// Lock model: chunks/vectors are invertible PII derived from the plaintext, so they exist ONLY
    /// for VISIBLE documents — PURGED in the same transaction that seals a folder (mirrors
    /// `purge_chunks_tx`), and re-embeddable on unlock. The gated read `search_doc_chunks_visible`
    /// re-applies `visibility_clause` (defense-in-depth) so a stray chunk can never surface a sealed
    /// folder's document.
    fn migrate_documents(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS documents (
               id TEXT PRIMARY KEY,
               folder_id TEXT NOT NULL,
               name TEXT NOT NULL,
               text TEXT NOT NULL DEFAULT '',
               text_blob BLOB,
               created_at INTEGER NOT NULL,
               FOREIGN KEY (folder_id) REFERENCES folders(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_documents_folder ON documents(folder_id);
             CREATE TABLE IF NOT EXISTS doc_chunks (
               id INTEGER PRIMARY KEY,
               document_id TEXT NOT NULL,
               chunk_index INTEGER NOT NULL,
               text TEXT NOT NULL,
               FOREIGN KEY (document_id) REFERENCES documents(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_doc_chunks_document ON doc_chunks(document_id);",
        )
        .map_err(map_err)?;
        // `kind` distinguishes an UPLOADED file ('document') from a TYPED brain note ('note'). Additive
        // + guarded so migrate() stays idempotent; legacy rows default to 'document'. Both kinds ride
        // the SAME seal/unseal/purge/gating — `kind` is a presentation split for the Brain page only.
        Self::add_column_if_missing(
            conn,
            "documents",
            "kind",
            "TEXT NOT NULL DEFAULT 'document'",
        )?;
        // The vec0 column width is the embedder's EMBED_DIM (== the note vec_chunks width). Format the
        // DDL (no user input). Parallel to `vec_chunks` but keyed to `doc_chunks.id`.
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS doc_vec_chunks USING vec0(
                 chunk_id INTEGER PRIMARY KEY,
                 embedding float[{dim}]
             );",
            dim = crate::embed::EMBED_DIM
        ))
        .map_err(map_err)?;
        Ok(())
    }

    /// Idempotent FTS5 index over `doc_chunks` (the SAME external-content + trigger pattern and the
    /// SAME `unicode61 remove_diacritics 2` tokenizer as [`Db::migrate_fts`]), so documents and
    /// typed brain notes are keyword-retrievable WITHOUT the e5 model.
    ///
    /// Lock model: the FTS index is DERIVED from `doc_chunks.text`, whose canonical copy while
    /// sealed is the document's `text_blob` — so purging the index destroys nothing. The seal path
    /// deletes a sealed folder's `doc_chunks` rows (`purge_doc_chunks_tx`), which fires the `_ad`
    /// trigger and removes the sealed tokens from this index in the SAME statement; unseal
    /// re-chunks, and the `_ai` trigger re-indexes. Gated reads go through
    /// [`Db::search_doc_chunks_fts_visible`] (visibility_clause defense-in-depth on top).
    fn migrate_doc_fts(conn: &Connection) -> Result<()> {
        let already_built: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='fts_doc_chunks'",
                [],
                |_| Ok(true),
            )
            .optional()
            .map_err(map_err)?
            .unwrap_or(false);

        // One-time backfill from rows that predate the index (a model-present install already has
        // doc_chunks). Only on first creation — later launches are trigger-maintained no-ops, so
        // migrate() stays idempotent. The backfill rides the SAME transaction as the CREATEs: a
        // crash between "table exists" and "backfill ran" would otherwise leave pre-existing chunks
        // permanently keyword-dark (the `already_built` guard would skip the backfill forever).
        let backfill = if already_built {
            ""
        } else {
            "INSERT INTO fts_doc_chunks(rowid, text) SELECT id, text FROM doc_chunks;"
        };
        let batch = format!(
            "BEGIN IMMEDIATE;
             CREATE VIRTUAL TABLE IF NOT EXISTS fts_doc_chunks USING fts5(
                 text,
                 content='doc_chunks',
                 content_rowid='id',
                 tokenize = 'unicode61 remove_diacritics 2'
             );

             -- doc_chunks → fts_doc_chunks. Chunk text is write-once (purge-then-reinsert), but the
             -- _au trigger is kept for parity with the meeting FTS trio (an UPDATE can never leave
             -- stale tokens behind).
             CREATE TRIGGER IF NOT EXISTS fts_doc_chunks_ai AFTER INSERT ON doc_chunks BEGIN
                 INSERT INTO fts_doc_chunks(rowid, text) VALUES (new.id, new.text);
             END;
             CREATE TRIGGER IF NOT EXISTS fts_doc_chunks_ad AFTER DELETE ON doc_chunks BEGIN
                 INSERT INTO fts_doc_chunks(fts_doc_chunks, rowid, text)
                   VALUES ('delete', old.id, old.text);
             END;
             CREATE TRIGGER IF NOT EXISTS fts_doc_chunks_au AFTER UPDATE ON doc_chunks BEGIN
                 INSERT INTO fts_doc_chunks(fts_doc_chunks, rowid, text)
                   VALUES ('delete', old.id, old.text);
                 INSERT INTO fts_doc_chunks(rowid, text) VALUES (new.id, new.text);
             END;
             {backfill}
             COMMIT;"
        );
        let res = conn.execute_batch(&batch);
        if res.is_err() {
            // A failed batch leaves the explicit transaction open on this connection — roll it
            // back so the error path hands later code a clean connection state.
            let _ = conn.execute_batch("ROLLBACK;");
        }
        res.map_err(map_err)?;
        Ok(())
    }

    /// Idempotent vector-layer schema: the `note_chunks` plaintext-chunk table (the embed source +
    /// snippet store) and the `vec_chunks` sqlite-vec `vec0` KNN table whose rowid/`chunk_id` maps
    /// 1:1 to `note_chunks.id`. `vec0` requires the auto-extension module to be registered on this
    /// connection (done in `open_with_key` / the test helper BEFORE `Connection::open`).
    ///
    /// Lock model: `note_chunks.text` is plaintext DERIVED from a note, and an embedding is
    /// invertible, so chunks/vectors exist ONLY for visible content — they are PURGED in the same
    /// transaction that blanks a folder's plaintext on lock (see `purge_chunks_for_meetings`,
    /// wired into `blank_sealed_notes_in_folders` + `reblank_locked_folders_at_rest` + `lock_folder`).
    fn migrate_vector(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS note_chunks (
               id INTEGER PRIMARY KEY,
               meeting_id TEXT NOT NULL,
               provider_id TEXT NOT NULL,
               chunk_idx INTEGER NOT NULL,
               source_type TEXT NOT NULL DEFAULT 'voice',
               text TEXT NOT NULL,
               content_hash TEXT,
               FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_note_chunks_meeting ON note_chunks(meeting_id);",
        )
        .map_err(map_err)?;
        // The vec0 column width is the embedder's EMBED_DIM. Format the DDL (no user input).
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS vec_chunks USING vec0(
                 chunk_id INTEGER PRIMARY KEY,
                 embedding float[{dim}]
             );",
            dim = crate::embed::EMBED_DIM
        ))
        .map_err(map_err)?;
        Ok(())
    }

    /// Brain v2 L1.1 — idempotent TOPIC-CHUNK schema: `topic_chunks` (the plaintext topic-segment
    /// store: span + raw text + augmented text + content hash), `topic_vec_chunks` (the vec0 KNN
    /// table, 1:1 by `chunk_id == topic_chunks.id`), and `fts_topic_chunks` (external-content FTS5
    /// over `aug_text`, kept exact by the standard `_ai`/`_ad`/`_au` trigger trio — mirrors
    /// `migrate_fts`; the DELETE trigger purges tokens when `purge_chunks_tx` drops the base rows
    /// on seal, so no sealed-content token survives the index).
    ///
    /// Lock model: rows are DERIVED plaintext and exist ONLY for visible meetings — indexed by the
    /// gated `index_meeting_topic_chunks`, purged on seal/delete via `purge_chunks_tx`.
    fn migrate_topic(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS topic_chunks (
               id INTEGER PRIMARY KEY,
               meeting_id TEXT NOT NULL,
               seg_index INTEGER NOT NULL,
               start_s REAL NOT NULL,
               end_s REAL NOT NULL,
               text TEXT NOT NULL,
               aug_text TEXT NOT NULL,
               content_hash TEXT,
               FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_topic_chunks_meeting ON topic_chunks(meeting_id);",
        )
        .map_err(map_err)?;
        // The vec0 column width is the embedder's EMBED_DIM (compile-time const — no user input).
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS topic_vec_chunks USING vec0(
                 chunk_id INTEGER PRIMARY KEY,
                 embedding float[{dim}]
             );",
            dim = crate::embed::EMBED_DIM
        ))
        .map_err(map_err)?;
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS fts_topic_chunks USING fts5(
                 aug_text,
                 content='topic_chunks',
                 content_rowid='id',
                 tokenize = 'unicode61 remove_diacritics 2'
             );
             CREATE TRIGGER IF NOT EXISTS fts_topic_chunks_ai AFTER INSERT ON topic_chunks BEGIN
                 INSERT INTO fts_topic_chunks(rowid, aug_text) VALUES (new.id, new.aug_text);
             END;
             CREATE TRIGGER IF NOT EXISTS fts_topic_chunks_ad AFTER DELETE ON topic_chunks BEGIN
                 INSERT INTO fts_topic_chunks(fts_topic_chunks, rowid, aug_text)
                   VALUES ('delete', old.id, old.aug_text);
             END;
             CREATE TRIGGER IF NOT EXISTS fts_topic_chunks_au AFTER UPDATE ON topic_chunks BEGIN
                 INSERT INTO fts_topic_chunks(fts_topic_chunks, rowid, aug_text)
                   VALUES ('delete', old.id, old.aug_text);
                 INSERT INTO fts_topic_chunks(rowid, aug_text) VALUES (new.id, new.aug_text);
             END;",
        )
        .map_err(map_err)?;
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

    /// Brain v2 L2.2 — idempotent FTS5 setup for `user_facts`: ONE external-content table over the
    /// three content-bearing columns (`subject`/`predicate`/`object` — together the searchable
    /// tokens of "<subject> <predicate>: <object>"), kept exact by the standard `_ai`/`_ad`/`_au`
    /// trigger trio, plus a one-time backfill from existing rows. `user_facts` has a TEXT `id`
    /// PRIMARY KEY, so it is a normal rowid table and `content_rowid='rowid'` mirrors it 1:1
    /// (exactly like `fts_meetings`). Same Polish-safe tokenizer as the other FTS tables.
    ///
    /// LOCK MODEL: user facts are purged on seal via DIRECT `DELETE`s (`purge_user_facts_tx`,
    /// `reblank_locked_folders_at_rest`) — those fire the `_ad` trigger, so a sealed meeting's fact
    /// tokens are removed from the index in the SAME transaction. `delete_meeting` purges
    /// explicitly too (not only via FK cascade) for the same trigger guarantee.
    ///
    /// CRASH SAFETY: the first-time CREATE (table + triggers) and the backfill run in ONE
    /// transaction, so a crash mid-migration can never strand an EMPTY index that looks "already
    /// built" — either everything lands (index complete) or nothing does (retried next launch).
    fn migrate_user_facts_fts(conn: &Connection) -> Result<()> {
        let already_built: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='fts_user_facts'",
                [],
                |_| Ok(true),
            )
            .optional()
            .map_err(map_err)?
            .unwrap_or(false);

        const CREATE: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS fts_user_facts USING fts5(
                 subject, predicate, object,
                 content='user_facts',
                 content_rowid='rowid',
                 tokenize = 'unicode61 remove_diacritics 2'
             );
             CREATE TRIGGER IF NOT EXISTS fts_user_facts_ai AFTER INSERT ON user_facts BEGIN
                 INSERT INTO fts_user_facts(rowid, subject, predicate, object)
                   VALUES (new.rowid, new.subject, new.predicate, new.object);
             END;
             CREATE TRIGGER IF NOT EXISTS fts_user_facts_ad AFTER DELETE ON user_facts BEGIN
                 INSERT INTO fts_user_facts(fts_user_facts, rowid, subject, predicate, object)
                   VALUES ('delete', old.rowid, old.subject, old.predicate, old.object);
             END;
             CREATE TRIGGER IF NOT EXISTS fts_user_facts_au AFTER UPDATE ON user_facts BEGIN
                 INSERT INTO fts_user_facts(fts_user_facts, rowid, subject, predicate, object)
                   VALUES ('delete', old.rowid, old.subject, old.predicate, old.object);
                 INSERT INTO fts_user_facts(rowid, subject, predicate, object)
                   VALUES (new.rowid, new.subject, new.predicate, new.object);
             END;";

        if already_built {
            // Idempotent re-run: everything below IF-NOT-EXISTS no-ops; no backfill.
            conn.execute_batch(CREATE).map_err(map_err)?;
            return Ok(());
        }
        // First build: create + one-time backfill ATOMICALLY (rollback-on-drop guards a crash).
        let tx = conn.unchecked_transaction().map_err(map_err)?;
        tx.execute_batch(CREATE).map_err(map_err)?;
        tx.execute_batch(
            "INSERT INTO fts_user_facts(rowid, subject, predicate, object)
               SELECT rowid, subject, predicate, object FROM user_facts;",
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Brain v2 L2.1 — memory-consolidation tables (see `crate::memory` for the job semantics).
    ///
    /// `memory_scores` — one row per OPEN user fact: the deterministic recency/importance/relevance
    /// components + the composite. FK `ON DELETE CASCADE` to `user_facts(id)` so BOTH the
    /// purge-on-seal (`purge_user_facts_tx`'s direct DELETE) and `delete_meeting`'s cascade drop the
    /// score with its fact — a sealed/deleted meeting leaves no score row behind. (Scores are
    /// CONTENT-FREE — floats + ids — but the cascade keeps the store consistent.)
    ///
    /// `memory_rollups` — one row per reflection scope (`entity:<id>` / `weekly:<YYYY-WNN>`):
    /// cross-meeting SYNTHESIS text. LOCK MODEL (two layers, both required):
    ///   1. PURGED on EVERY seal path — `purge_memory_rollups_tx` runs inside the seal transactions
    ///      (`purge_chunks_for_meetings` / `blank_sealed_notes_in_folders` /
    ///      `reblank_locked_folders_at_rest` / `delete_meeting`) and the CALLER deletes the exported
    ///      vault `.md`s from the returned `exported_path`s. Rollups are cheap re-derivable synthesis;
    ///      they regenerate on the next hourly pass FROM VISIBLE FACTS ONLY.
    ///   2. Per-pass regeneration/GC — `fact_set_hash` records the SORTED visible-open-fact id set a
    ///      rollup was synthesized from; the hourly pass deletes no-longer-eligible scopes and
    ///      re-reflects any scope whose hash changed (superseded/forgotten facts age out even without
    ///      a seal). See `crate::memory::run_consolidation_pass`.
    ///
    /// `scope` is UNIQUE so the upsert is idempotent per scope. `fact_set_hash` is added guarded
    /// (`add_column_if_missing`) for DBs that ran the earlier shape of this migration.
    ///
    /// Also adds `facts.importance` (guarded, additive): the light reasoner's batch-assessed 1–10
    /// importance of an ENTITY fact, persisted so steady-state passes stay LLM-free. It backs the
    /// spec's "or any fact with importance ≥ 7" reflection-eligibility arm. Content-free (a float).
    fn migrate_memory(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memory_scores (
               fact_id TEXT PRIMARY KEY,
               scope TEXT NOT NULL DEFAULT 'user',
               recency REAL NOT NULL,
               importance REAL NOT NULL,
               relevance REAL NOT NULL,
               composite REAL NOT NULL,
               scored_at TEXT NOT NULL,
               FOREIGN KEY (fact_id) REFERENCES user_facts(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_memory_scores_composite
               ON memory_scores(composite);
             CREATE TABLE IF NOT EXISTS memory_rollups (
               id TEXT PRIMARY KEY,
               scope TEXT NOT NULL UNIQUE,
               content TEXT NOT NULL,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               exported_path TEXT
             );",
        )
        .map_err(map_err)?;
        Self::add_column_if_missing(conn, "memory_rollups", "fact_set_hash", "TEXT")?;
        Self::add_column_if_missing(conn, "facts", "importance", "REAL")?;
        Ok(())
    }

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
    fn migrate_briefs(conn: &Connection) -> Result<()> {
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

    /// Brain v2 L5 — idempotent MCP-SERVER config schema. One row per user-configured external MCP
    /// server: transport + endpoint + args + the ENABLED and per-server CONSENTED flags (consent
    /// default 0 — fail-closed; flipped only by the dedicated consent commands). Connection config
    /// only — never query text or results.
    fn migrate_mcp_servers(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS mcp_servers (
               id TEXT PRIMARY KEY,
               label TEXT NOT NULL,
               transport TEXT NOT NULL,
               endpoint TEXT NOT NULL,
               args TEXT NOT NULL DEFAULT '[]',
               enabled INTEGER NOT NULL DEFAULT 1,
               consented INTEGER NOT NULL DEFAULT 0,
               created_at TEXT NOT NULL
             );",
        )
        .map_err(map_err)
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

    // ── egress audit log ────────────────────────────────────────────────────

    /// Insert one content-free audit row into `egress_log`. Called by `DbEgressSink::record`.
    ///
    /// `ts` is a Unix epoch (seconds) computed by the caller (`SystemTime::now()`). The row
    /// carries ONLY counts, ids, labels, byte sizes, and token counts — NO content (§8).
    pub fn insert_egress(
        &self,
        ts: i64,
        e: &crate::summarize::egress_log::EgressEntry,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO egress_log (
               ts, provider_id, destination, model_requested, model_served, call_kind,
               prompt_tokens, completion_tokens, total_tokens, cached_tokens,
               redactions_email, redactions_card, redactions_phone, redactions_name,
               system_bytes, user_bytes, meeting_id
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            rusqlite::params![
                ts,
                e.provider_id,
                e.destination,
                e.model_requested,
                e.meta.model_served.as_deref(),
                e.call_kind,
                e.meta.prompt_tokens.map(|v| v as i64),
                e.meta.completion_tokens.map(|v| v as i64),
                e.meta.total_tokens.map(|v| v as i64),
                e.meta.cached_tokens.map(|v| v as i64),
                e.redactions.email as i64,
                e.redactions.card as i64,
                e.redactions.phone as i64,
                e.redactions.name as i64,
                e.system_bytes as i64,
                e.user_bytes as i64,
                e.meeting_id.as_deref(),
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    // ── M3-CLIENT: outbound server-share bookkeeping + share egress ledger (spec §7) ─────────────

    /// Record a newly-created OUTBOUND link share. Stores ONLY `share_id` + `meeting_id` (+
    /// mode/rev/state/ts) — NO title (derived via the gated meeting read), no `L`, no ciphertext.
    pub fn insert_outbound_share(
        &self,
        share_id: &str,
        meeting_id: &str,
        mode: &str,
        rev: u32,
        created_at: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO outbound_shares (share_id, meeting_id, mode, rev, state, created_at)
             VALUES (?1, ?2, ?3, ?4, 'active', ?5)",
            rusqlite::params![share_id, meeting_id, mode, rev as i64, created_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Record an outbound NOTE share (WP6). The share anchors on a `documents(kind='note')` id in the
    /// additive `document_id` column; the NOT NULL `meeting_id` is stored as '' so the meeting-title
    /// join skips it and `list_my_shares` resolves the NOTE title instead. Mirrors
    /// [`Db::insert_outbound_share`] otherwise.
    pub fn insert_outbound_note_share(
        &self,
        share_id: &str,
        document_id: &str,
        mode: &str,
        rev: u32,
        created_at: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO outbound_shares
               (share_id, meeting_id, document_id, mode, rev, state, created_at)
             VALUES (?1, '', ?2, ?3, ?4, 'active', ?5)",
            rusqlite::params![share_id, document_id, mode, rev as i64, created_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The local NOTE `document_id` for an outbound share (so the share list can gate on the note's
    /// folder lock state before revealing its title). `None` for a meeting share or an unknown share.
    pub fn outbound_share_document(&self, share_id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT document_id FROM outbound_shares WHERE share_id = ?1",
            rusqlite::params![share_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(map_err)
        .map(Option::flatten)
    }

    /// True iff a NOTE (`document_id`) has at least one ACTIVE (non-revoked) outbound share. Drives
    /// the `shared` flag on the note DTOs. A bare boolean — leaks nothing.
    pub fn note_has_active_share(&self, document_id: &str) -> Result<bool> {
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM outbound_shares
                            WHERE document_id = ?1 AND state = 'active')",
            rusqlite::params![document_id],
            |r| r.get::<_, i64>(0),
        )
        .map_err(map_err)
        .map(|n| n != 0)
    }

    /// The set of NOTE `document_id`s with at least one ACTIVE outbound share (batch form of
    /// [`Db::note_has_active_share`] for the list DTO — one query instead of N). Leaks nothing.
    pub fn notes_with_active_share(&self) -> Result<HashSet<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT document_id FROM outbound_shares
                  WHERE document_id IS NOT NULL AND state = 'active'",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        let mut out = HashSet::new();
        for r in rows {
            out.insert(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// The local `meeting_id` for an outbound share (so the share list can gate on the meeting's lock
    /// state before revealing its title). `None` if we never created this share locally.
    pub fn outbound_share_meeting(&self, share_id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT meeting_id FROM outbound_shares WHERE share_id = ?1",
            rusqlite::params![share_id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(map_err)
    }

    /// Flip an outbound share's local state (e.g. `'revoked'`). Idempotent; a no-op if the share_id
    /// is unknown locally (a share created on another device).
    pub fn set_outbound_share_state(&self, share_id: &str, state: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE outbound_shares SET state = ?2 WHERE share_id = ?1",
            rusqlite::params![share_id, state],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Append one CONTENT-FREE share-egress ledger row (§7 inv. 4): host + byte size + a `kind` label
    /// (`"share_create"` / `"share_revoke"` / `"account_login"` …). NEVER the URL, `L`, a title, or
    /// any note text. `ts` is a Unix epoch (seconds).
    pub fn insert_share_egress(
        &self,
        ts: i64,
        host: &str,
        kind: &str,
        byte_count: usize,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO share_egress_log (ts, host, kind, byte_count) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![ts, host, kind, byte_count as i64],
        )
        .map_err(map_err)?;
        Ok(())
    }

    // ── M5-CLIENT: TOFU pins, mode-B outbound bookkeeping, inbound accept idempotency (spec §4.8/§7) ──

    /// TOFU-pin a contact's identity fingerprint under a STABLE `account_id` (spec §4.8: pin on
    /// account_id, not email). Idempotent upsert — the CALLER decides whether to pin (first contact) or
    /// BLOCK (an existing pin with a different fingerprint), never a silent overwrite of a changed key.
    pub fn pin_contact(
        &self,
        account_id: &str,
        email: Option<&str>,
        fingerprint: &str,
        pinned_at: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO pinned_contacts (account_id, email, fingerprint, pinned_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(account_id) DO UPDATE SET
               email = excluded.email, fingerprint = excluded.fingerprint, pinned_at = excluded.pinned_at",
            rusqlite::params![account_id, email, fingerprint, pinned_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The pinned `(email, fingerprint)` for a contact `account_id`, or `None` if never pinned
    /// (first contact). The safety-word compare + the blocking key-change detection read this.
    pub fn get_pinned_contact(&self, account_id: &str) -> Result<Option<(Option<String>, String)>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT email, fingerprint FROM pinned_contacts WHERE account_id = ?1",
            rusqlite::params![account_id],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(map_err)
    }

    /// Record a mode-B OUTBOUND share. Stores the retained note key WRAPPED under the account MK
    /// (`nk_wrapped`, via `e2ee::wrap_key32` — NOT the raw key) + the `content_hash` (SHA-256 of the
    /// sealed cell) so a later `share_rewrap_pending` (which holds the MK session) can unwrap + re-wrap
    /// to a newly-registered recipient WITHOUT re-reading meeting content — plus `share_id`+`meeting_id`
    /// for the gated title derivation (NO title column, spec §7). `state` = `'sent'` (registered) or
    /// `'awaiting_key'` (invited/unregistered). New rows leave the legacy raw `nk` column NULL; the
    /// wrapped key means a re-locked session (no MK) can no longer decrypt an already-shared envelope.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_outbound_user_share(
        &self,
        share_id: &str,
        meeting_id: &str,
        rev: u32,
        created_at: &str,
        state: &str,
        nk_wrapped: &[u8],
        recipient_acct_id: &str,
        recipient_email: &str,
        content_hash: &[u8],
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO outbound_shares
               (share_id, meeting_id, mode, rev, state, created_at,
                nk_wrapped, recipient_acct_id, recipient_email, content_hash)
             VALUES (?1, ?2, 'user', ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                share_id,
                meeting_id,
                rev as i64,
                state,
                created_at,
                nk_wrapped,
                recipient_acct_id,
                recipient_email,
                content_hash
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Every mode-B outbound share still `'awaiting_key'` (the recipient was unregistered at share
    /// time). Returns `(share_id, rev, nk_bytes, nk_is_wrapped, recipient_email, content_hash)` for the
    /// on-launch re-wrap. `nk_is_wrapped` = the bytes are the MK-wrapped NK (`nk_wrapped`, new rows) vs
    /// the legacy RAW NK (`nk`, pre-0.7 rows — the caller treats unwrap as identity). New rows are
    /// preferred; a legacy row is read only when `nk_wrapped` is absent, so existing shares still
    /// re-wrap after the migration.
    #[allow(clippy::type_complexity)]
    pub fn list_awaiting_rewrap(
        &self,
    ) -> Result<Vec<(String, u32, Vec<u8>, bool, String, Vec<u8>)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT share_id, rev, nk, nk_wrapped, recipient_email, content_hash
                 FROM outbound_shares
                 WHERE mode = 'user' AND state = 'awaiting_key'
                   AND (nk IS NOT NULL OR nk_wrapped IS NOT NULL)
                   AND recipient_email IS NOT NULL AND content_hash IS NOT NULL",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                let nk: Option<Vec<u8>> = r.get(2)?;
                let nk_wrapped: Option<Vec<u8>> = r.get(3)?;
                // Prefer the MK-wrapped key; fall back to the legacy raw NK (unwrap = identity).
                let (bytes, is_wrapped) = match nk_wrapped {
                    Some(w) => (w, true),
                    None => (nk.unwrap_or_default(), false),
                };
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)? as u32,
                    bytes,
                    is_wrapped,
                    r.get::<_, String>(4)?,
                    r.get::<_, Vec<u8>>(5)?,
                ))
            })
            .map_err(map_err)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Record an ACCEPTED inbound share (idempotency + provenance). A duplicate `share_id` is ignored
    /// (INSERT OR IGNORE) so a re-accept never writes a second vault note.
    pub fn insert_inbound_share(
        &self,
        share_id: &str,
        meeting_id: &str,
        sender_acct_id: &str,
        accepted_at: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR IGNORE INTO inbound_shares (share_id, meeting_id, sender_acct_id, accepted_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![share_id, meeting_id, sender_acct_id, accepted_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The local `meeting_id` a share was already accepted into, or `None` if never accepted. Drives
    /// the `accept_share` idempotency check (spec §7 inv. 2: idempotent on `share_id`).
    pub fn inbound_share_meeting(&self, share_id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT meeting_id FROM inbound_shares WHERE share_id = ?1",
            rusqlite::params![share_id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(map_err)
    }

    /// Persist the durable RESUME record for a mode-B accept whose server row was just flipped to
    /// `accepted`, BEFORE the local verify+ingest. `INSERT OR REPLACE` (idempotent on `share_id`) so a
    /// retry that re-flips is harmless. Dropped by [`delete_pending_share_accept`] once ingest commits.
    pub fn insert_pending_share_accept(&self, p: &PendingShareAccept) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO pending_share_accepts
               (share_id, blob_id, target_folder_id, sender_user_id, sender_fingerprint,
                wrapped_key, grant_sig, rev, key_generation, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                p.share_id,
                p.blob_id,
                p.target_folder_id,
                p.sender_user_id,
                p.sender_fingerprint,
                p.wrapped_key,
                p.grant_sig,
                p.rev as i64,
                p.key_generation as i64,
                p.created_at,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The durable resume record for a mode-B accept that flipped `accepted` server-side but never
    /// finished ingesting locally, or `None`. Drives the `accept_share` RESUME path (re-fetch + finish
    /// without a fresh inbox item, closing the post-flip strand).
    pub fn get_pending_share_accept(&self, share_id: &str) -> Result<Option<PendingShareAccept>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT share_id, blob_id, target_folder_id, sender_user_id, sender_fingerprint,
                    wrapped_key, grant_sig, rev, key_generation, created_at
               FROM pending_share_accepts WHERE share_id = ?1",
            rusqlite::params![share_id],
            |r| {
                Ok(PendingShareAccept {
                    share_id: r.get(0)?,
                    blob_id: r.get(1)?,
                    target_folder_id: r.get(2)?,
                    sender_user_id: r.get(3)?,
                    sender_fingerprint: r.get(4)?,
                    wrapped_key: r.get(5)?,
                    grant_sig: r.get(6)?,
                    rev: r.get::<_, i64>(7)? as u32,
                    key_generation: r.get::<_, i64>(8)? as u32,
                    created_at: r.get(9)?,
                })
            },
        )
        .optional()
        .map_err(map_err)
    }

    /// Drop the resume record once the accept's ingest has committed (the strand window is closed).
    /// Idempotent (`DELETE` of a missing row is a no-op).
    pub fn delete_pending_share_accept(&self, share_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM pending_share_accepts WHERE share_id = ?1",
            rusqlite::params![share_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Aggregate the `egress_log` table over the last `days` calendar days and return a rich
    /// summary for the "Egress & Usage" Analytics panel.
    ///
    /// The time window is `[now_unix - days*86400, now_unix]`. A `days <= 0` value returns ALL
    /// rows. An empty table (no cloud calls yet) returns all-zero totals and empty vecs — never
    /// an error.
    ///
    /// Read-only: touches `egress_log` only; no content columns. (§6: egress_log has none.)
    pub fn egress_summary(&self, days: i64) -> Result<EgressLedger> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let since = if days > 0 {
            now_unix - days * 86_400
        } else {
            0
        };

        let conn = self.lock();

        // ── total calls + total tokens ──────────────────────────────────────
        // Cast to u64 so an all-time SUM cannot wrap (i64→u64 is safe for non-negative sums).
        let (total_calls, total_tokens): (u64, u64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(total_tokens),0)
                   FROM egress_log
                  WHERE ts >= ?1",
                rusqlite::params![since],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
            )
            .map_err(map_err)?;

        // ── total redactions ────────────────────────────────────────────────
        let total_redactions: EgressRedactionTotals = conn
            .query_row(
                "SELECT COALESCE(SUM(redactions_email),0),
                        COALESCE(SUM(redactions_card),0),
                        COALESCE(SUM(redactions_phone),0),
                        COALESCE(SUM(redactions_name),0)
                   FROM egress_log
                  WHERE ts >= ?1",
                rusqlite::params![since],
                |r| {
                    Ok(EgressRedactionTotals {
                        email: r.get::<_, i64>(0)? as u64,
                        card: r.get::<_, i64>(1)? as u64,
                        phone: r.get::<_, i64>(2)? as u64,
                        name: r.get::<_, i64>(3)? as u64,
                    })
                },
            )
            .map_err(map_err)?;

        // ── by_model (GROUP BY model label, tokens DESC) ────────────────────
        let by_model = {
            let mut stmt = conn
                .prepare(
                    // NULLIF guards: an empty string '' in model_served or model_requested
                    // (the default when no model is sent by claude_code/anthropic) must bucket
                    // under '(unknown)' rather than producing a blank label in the Settings UI.
                    "SELECT COALESCE(NULLIF(model_served,''), NULLIF(model_requested,''), '(unknown)') AS model,
                            COUNT(*) AS calls,
                            COALESCE(SUM(total_tokens), 0) AS tokens
                       FROM egress_log
                      WHERE ts >= ?1
                      GROUP BY model
                      ORDER BY tokens DESC",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![since], |r| {
                    Ok(EgressModelUsage {
                        model: r.get(0)?,
                        calls: r.get::<_, i64>(1)? as u64,
                        tokens: r.get::<_, i64>(2)? as u64,
                    })
                })
                .map_err(map_err)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(map_err)?);
            }
            out
        };

        // ── by_day (GROUP BY UTC date, ascending) ──────────────────────────
        let by_day = {
            let mut stmt = conn
                .prepare(
                    "SELECT date(ts, 'unixepoch') AS day,
                            COALESCE(SUM(total_tokens), 0) AS tokens
                       FROM egress_log
                      WHERE ts >= ?1
                      GROUP BY day
                      ORDER BY day ASC",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![since], |r| {
                    Ok(EgressDayUsage {
                        day: r.get(0)?,
                        tokens: r.get::<_, i64>(1)? as u64,
                    })
                })
                .map_err(map_err)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(map_err)?);
            }
            out
        };

        // ── recent rows (last ≤20, newest first) ───────────────────────────
        let recent = {
            let mut stmt = conn
                .prepare(
                    "SELECT ts, provider_id, destination, model_served, total_tokens,
                            redactions_email, redactions_card, redactions_phone, redactions_name
                       FROM egress_log
                      WHERE ts >= ?1
                      ORDER BY ts DESC
                      LIMIT 20",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![since], |r| {
                    Ok(EgressRecentRow {
                        ts: r.get(0)?,
                        provider_id: r.get(1)?,
                        destination: r.get(2)?,
                        model_served: r.get(3)?,
                        total_tokens: r.get::<_, Option<i64>>(4)?.map(|v| v as u32),
                        redactions: EgressRedactionTotals {
                            email: r.get::<_, i64>(5)? as u64,
                            card: r.get::<_, i64>(6)? as u64,
                            phone: r.get::<_, i64>(7)? as u64,
                            name: r.get::<_, i64>(8)? as u64,
                        },
                    })
                })
                .map_err(map_err)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(map_err)?);
            }
            out
        };

        Ok(EgressLedger {
            total_calls,
            total_tokens,
            by_model,
            by_day,
            total_redactions,
            recent,
        })
    }

    // ── correction-log flywheel ──────────────────────────────────────────────

    /// Append one model-output→correction example to the local `correction_log` (the dataset
    /// substrate for later on-device LoRA fine-tuning). Returns the new row id (the passed `rec.id`
    /// is ignored — SQLite assigns the autoincrement key). Entirely local + SQLCipher-encrypted;
    /// nothing here egresses.
    pub fn log_correction(&self, rec: &CorrectionRecord) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO correction_log
               (kind, input, model_output, final_output, accepted, owner_id, created_at, meeting_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                rec.kind,
                rec.input,
                rec.model_output,
                rec.final_output,
                rec.accepted as i64,
                rec.owner_id,
                rec.created_at,
                rec.meeting_id,
            ],
        )
        .map_err(map_err)?;
        Ok(conn.last_insert_rowid())
    }

    /// GATED read-back of the most-recent `limit` correction examples of `kind`, newest first. This
    /// is an EXPORT-SHAPED reader over sealed-content-derived data, so it is visibility-gated exactly
    /// like `search_visible`: a row is returned ONLY when its `meeting_id` is non-NULL AND that
    /// meeting has at least one VISIBLE note (folder open/NULL OR session-unlocked, via
    /// `visibility_clause`). A correction for a sealed-and-not-unlocked meeting — and any row with a
    /// NULL `meeting_id` (legacy/unattributed) — is EXCLUDED (fail-closed). The seal/delete paths also
    /// purge a meeting's rows, so this is defense-in-depth on top of that.
    pub fn list_corrections(
        &self,
        kind: &str,
        limit: i64,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<CorrectionRecord>> {
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT cl.id, cl.kind, cl.input, cl.model_output, cl.final_output, cl.accepted,
                    cl.owner_id, cl.created_at, cl.meeting_id
               FROM correction_log cl
              WHERE cl.kind = ?1
                AND cl.meeting_id IS NOT NULL
                AND EXISTS (
                      SELECT 1 FROM notes n
                       LEFT JOIN folders f ON f.id = n.folder_id
                       WHERE n.meeting_id = cl.meeting_id AND {visible}
                    )
              ORDER BY cl.created_at DESC, cl.id DESC
              LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![kind, limit], |row| {
                Ok(CorrectionRecord {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    input: row.get(2)?,
                    model_output: row.get(3)?,
                    final_output: row.get(4)?,
                    accepted: row.get::<_, i64>(5)? != 0,
                    owner_id: row.get(6)?,
                    created_at: row.get(7)?,
                    meeting_id: row.get(8)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
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
        let conn = self.lock();
        // Collect the ghost ids first (UUIDs — not PII) so the reconcile is auditable in the log.
        let ids: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT id FROM meetings WHERE status = ?1")
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![MeetingStatus::Recording.as_str()], |r| {
                    r.get::<_, String>(0)
                })
                .map_err(map_err)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?
        };
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
        let conn = self.lock();
        conn.execute(
            "UPDATE meetings SET title = ?2 WHERE id = ?1",
            rusqlite::params![id, title],
        )
        .map_err(map_err)?;
        Ok(())
    }

    // ── brain2 realtime typed @brain notes (the `meetings.manual_notes` durable buffer) ────────
    //
    // One free-text buffer per meeting that the user types DURING recording — the DURABLE CANONICAL
    // STORE of those typed notes (re-folded into the note on every (re)summarize; never the only
    // copy is destroyed). The plaintext column is SEALED-AND-RESTORED under the folder CK exactly
    // like the note `content_blob` / transcript / timeline: `seal_manual_notes` (verify-before-
    // destroy at the call site) → `raw_manual_notes` (read for unseal/reblank) → `set_manual_notes`
    // (restore plaintext) → `clear_manual_notes_blob` (permanent remove-lock). These are LOW-LEVEL
    // helpers with NO lock gating — the COMMAND layer (`save_manual_notes`/`get_manual_notes`) does
    // the `meeting_is_unlocked` gating; the internal seal/unseal paths drive these directly.

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
    pub fn set_manual_notes_sealed(
        &self,
        meeting_id: &str,
        text: &str,
        blob: &[u8],
    ) -> Result<()> {
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

    /// Delete a meeting and (via ON DELETE CASCADE) its segments, notes, and timeline.
    /// Audio + vault files are removed by the caller before this.
    ///
    /// Returns the `exported_path`s of the memory rollups purged in the same transaction (a rollup
    /// may paraphrase the deleted meeting's facts — see `purge_memory_rollups_tx`); the CALLER
    /// deletes those vault `.md` files (same layering as the note/audio file removal above).
    pub fn delete_meeting(&self, id: &str) -> Result<Vec<String>> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        // Drop derived chunks/vectors FIRST, in the same tx. `vec_chunks` is a vec0 virtual table
        // with no foreign key, so the `meetings` ON DELETE CASCADE reaches `note_chunks` but NOT
        // `vec_chunks` — without this the deleted meeting's (invertible) embeddings would persist
        // orphaned at rest, and a future rowid reuse could PK-conflict on the stale chunk_id.
        Self::purge_chunks_tx(&tx, &[id.to_string()])?;
        // Phase F0: drop this meeting's correction-log rows too (same tx) — a deleted meeting leaves
        // no plaintext-derived training data behind.
        Self::purge_corrections_tx(&tx, &[id.to_string()])?;
        // Re-Truth: drop supersession rows referencing this meeting on EITHER side (no FK — see
        // purge_supersessions_tx). Their pre-images hold plaintext note bytes; a deleted meeting must
        // leave none behind.
        Self::purge_supersessions_tx(&tx, &[id.to_string()])?;
        // Brain v2 L4: drop this meeting's live-bullets crash-recovery row EXPLICITLY in the same
        // tx (the meetings FK CASCADE also covers it — the explicit delete keeps the purge visible
        // and test-bound alongside the other derived artifacts).
        Self::purge_live_bullets_tx(&tx, &[id.to_string()])?;
        // Brain v2 L2.2: purge this meeting's user facts EXPLICITLY (direct DELETE) rather than
        // relying on the meetings FK cascade alone — the direct DELETE reliably fires the
        // `fts_user_facts_ad` trigger, so the deleted facts' tokens leave the FTS index in this
        // same tx (their `memory_scores` rows cascade off the user_facts FK). This also makes
        // "delete the synthetic Memory Import meeting ⇒ the import is undone" structural.
        Self::purge_user_facts_tx(&tx, &[id.to_string()])?;
        // Brain v2 L5: purge any PENDING scheduled-brief row referencing this meeting in the same
        // tx — its `note_md` paraphrases the deleted meeting's note (accepted rows were consumed
        // on accept and keep only ids + timestamps).
        Self::purge_pending_brief_runs_tx(&tx, &[id.to_string()])?;
        // Brain v2 L2.1: purge ALL memory rollups in this same tx — a rollup may paraphrase the
        // deleted meeting's (now-gone) facts; the survivors regenerate on the next hourly pass
        // from the remaining visible facts only. The caller deletes the exported `.md`s.
        let rollup_exports = Self::purge_memory_rollups_tx(&tx)?;
        tx.execute("DELETE FROM meetings WHERE id = ?1", rusqlite::params![id])
            .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(rollup_exports)
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

    // ── vector retrieval (Phase 2a) ───────────────────────────────────────────
    //
    // note_chunks holds plaintext chunks DERIVED from a visible note; vec_chunks (vec0) holds the
    // matching embeddings (1:1 by id). Both exist ONLY for visible content — purged on lock. The
    // semantic read is GATED by exactly the `search_visible` visibility predicate as defense-in-
    // depth, so even a stray chunk that escaped purge can never surface a sealed meeting.

    /// (Re)index a VISIBLE meeting's latest note into `note_chunks` + `vec_chunks`. Old rows for the
    /// meeting are deleted first (so re-summarize/re-index is a clean replace, keyed by
    /// (meeting_id, provider_id, chunk_idx)). Caller MUST only invoke this for visible/unlocked
    /// content — a sealed note's plaintext is blank, so this becomes a no-op if it is ever called on
    /// one (nothing to chunk), but the contract is "visible only".
    /// Index a meeting's plaintext into the semantic vector layer as two chunk classes, BOTH written
    /// in ONE clean-replace transaction (purge-then-reinsert so a re-index replaces both classes):
    ///   - `source_type = 'voice'` — the note-summary chunks ([`crate::embed::chunk_note`]);
    ///   - `source_type = 'transcript'` — speaker-turn / sliding-window chunks of the SEGMENTS
    ///     ([`crate::embed::chunk_transcript`]), so paraphrase queries about things SAID-but-not-
    ///     summarized are retrievable (the note-only chunks miss them; keyword over the transcript
    ///     already works via the segments FTS table).
    ///
    /// `segments` are the meeting's transcript segments; the caller passes the RESTORED/unsealed
    /// plaintext (a sealed meeting is never indexed — see the gated callers). Vectors are the e5
    /// `passage:` embedding of both chunk classes; NEVER a stub vector at rest — the CALLERS only reach
    /// this when the real embed model is present (`embed_model_present()`; mirrors the note path and
    /// `reindex_meetings_after_unseal`). A meeting with no note AND no segments ends with zero chunks.
    pub fn index_meeting_chunks(
        &self,
        meeting_id: &str,
        segments: &[Segment],
        embedder: &dyn Embedder,
    ) -> Result<()> {
        // Resolve title + date (plaintext = visible metadata). Note markdown is optional — a meeting
        // may have transcript segments but no note yet (or vice versa); each class indexes on its own.
        let meeting = self.get_meeting(meeting_id)?;
        let Some(meeting) = meeting else {
            return Ok(()); // unknown meeting — nothing to index.
        };
        let title = meeting
            .title
            .clone()
            .unwrap_or_else(|| "(untitled)".to_string());
        let date = meeting
            .started_at
            .split(['T', ' '])
            .next()
            .unwrap_or("")
            .to_string();

        // The note is optional now (transcript chunks index even without a note). `provider_id` is
        // needed for the note_chunks row; when there is no note we tag chunks with a stable sentinel.
        let note = self.get_latest_note_for_meeting(meeting_id)?;
        let provider_id = note
            .as_ref()
            .map(|n| n.provider_id.clone())
            .unwrap_or_else(|| "transcript".to_string());

        // NOTE-SUMMARY chunks (source_type='voice') — unchanged chunking + header.
        let note_chunks = match note.as_ref() {
            Some(n) => crate::embed::chunk_note(&title, &date, &n.markdown),
            None => Vec::new(),
        };
        // TRANSCRIPT chunks (source_type='transcript') — speaker-turn / sliding-window with provenance.
        let transcript_chunks = crate::embed::chunk_transcript(&title, &date, segments);

        // Brain v2 L1.2 — contextual augmentation: batch-read the meeting's gated attendees + facts
        // ONCE, then situate every chunk with the `<title> | <date> | <attendees> | <facts>` header.
        // FAIL-CLOSED: the reads run under an EMPTY unlock set, so a sealed folder's entity/fact
        // context is NEVER persisted into an index row (a session-unlocked sealed meeting simply
        // gets an empty header — strictly safer; open-folder meetings, the common case, get the
        // full header). The RAW `text` column keeps the un-augmented chunk for snippet display.
        let (attendees, facts) =
            self.augment_header_inputs(meeting_id, &std::collections::HashSet::new())?;
        let aug = |chunks: &[String]| -> Vec<String> {
            chunks
                .iter()
                .map(|c| crate::embed::augment_chunk_text(&title, &date, &attendees, &facts, c))
                .collect()
        };
        let note_aug = aug(&note_chunks);
        let transcript_aug = aug(&transcript_chunks);

        // Chunks are passages → e5 `passage:` prefix convention. The stub ignores the prefix; the real
        // CandleBertEmbedder needs it for retrieval recall. Embed each class only when non-empty —
        // on the AUGMENTED text (L1.2: the situating header rides the embedding AND the FTS legs).
        let note_vectors = if note_aug.is_empty() {
            Vec::new()
        } else {
            embedder.embed_passage(&note_aug)?
        };
        let transcript_vectors = if transcript_aug.is_empty() {
            Vec::new()
        } else {
            embedder.embed_passage(&transcript_aug)?
        };

        // Always purge this meeting's prior rows first (clean replace of BOTH classes), then insert the
        // fresh set in ONE transaction.
        let this_meeting = [meeting_id.to_string()];
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        Self::purge_chunks_tx(&tx, &this_meeting)?;
        {
            let mut ins_chunk = tx
                .prepare(
                    "INSERT INTO note_chunks
                       (meeting_id, provider_id, chunk_idx, source_type, text, aug_text, content_hash)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .map_err(map_err)?;
            let mut ins_vec = tx
                .prepare("INSERT INTO vec_chunks(chunk_id, embedding) VALUES (?1, ?2)")
                .map_err(map_err)?;
            // `chunk_idx` is a per-class ordinal; the two classes are distinguished by `source_type`.
            let classes = [
                ("voice", &note_chunks, &note_aug, &note_vectors),
                (
                    "transcript",
                    &transcript_chunks,
                    &transcript_aug,
                    &transcript_vectors,
                ),
            ];
            for (source_type, chunks, augs, vectors) in classes {
                for (idx, ((text, aug_text), vector)) in chunks
                    .iter()
                    .zip(augs.iter())
                    .zip(vectors.iter())
                    .enumerate()
                {
                    // The hash covers the AUGMENTED text (the embedded bytes) so a facts/attendee
                    // change re-embeds on the next re-index.
                    let content_hash = format!("{:016x}", chunk_hash(aug_text));
                    ins_chunk
                        .execute(rusqlite::params![
                            meeting_id,
                            provider_id,
                            idx as i64,
                            source_type,
                            text,
                            aug_text,
                            content_hash
                        ])
                        .map_err(map_err)?;
                    let chunk_id = tx.last_insert_rowid();
                    let blob = crate::embed::vec_to_blob(vector);
                    ins_vec
                        .execute(rusqlite::params![chunk_id, blob])
                        .map_err(map_err)?;
                }
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Brain v2 L1.2 — the gated AUGMENTATION-HEADER inputs for one meeting, batched (ONE read per
    /// meeting, never per chunk): up to [`crate::embed::AUG_MAX_ATTENDEES`] visible entity names
    /// (person-kind first) and up to [`crate::embed::AUG_MAX_FACTS`] visible facts rendered as
    /// `subject predicate: object`. Both reads apply the SAME visibility predicate as every other
    /// graph/fact read, so a sealed-and-not-in-`unlocked` meeting yields an EMPTY header — the
    /// L1.2 fail-closed contract.
    fn augment_header_inputs(
        &self,
        meeting_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<(Vec<String>, Vec<String>)> {
        let attendees = self.list_entities_for_meeting_visible(meeting_id, unlocked)?;
        let facts = self.facts_for_meeting_visible(meeting_id, unlocked)?;
        Ok((attendees, facts))
    }

    /// Gated NARROW reader (Brain v2 L1.2): the names of entities mentioned in ONE meeting, capped
    /// at [`crate::embed::AUG_MAX_ATTENDEES`], person-kind first then alphabetical. The meeting
    /// must be visible under the standard predicate (`EXISTS(visible note) OR NOT EXISTS(any
    /// note)`) — a sealed-and-not-unlocked meeting returns an EMPTY list, never its attendees.
    pub fn list_entities_for_meeting_visible(
        &self,
        meeting_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<String>> {
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT e.name FROM entities e
               JOIN entity_mentions em ON em.entity_id = e.id
               JOIN meetings m ON m.id = em.meeting_id
              WHERE em.meeting_id = ?1
                AND (NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id)
                     OR EXISTS (SELECT 1 FROM notes n
                                 LEFT JOIN folders f ON f.id = n.folder_id
                                WHERE n.meeting_id = m.id AND {visible}))
              ORDER BY CASE WHEN e.kind = 'person' THEN 0 ELSE 1 END, e.name ASC
              LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(
                rusqlite::params![meeting_id, crate::embed::AUG_MAX_ATTENDEES as i64],
                |r| r.get::<_, String>(0),
            )
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Gated NARROW reader (Brain v2 L1.2): the facts derived from ONE meeting, rendered
    /// `subject predicate: object`, current (open `valid_to`) first, capped at
    /// [`crate::embed::AUG_MAX_FACTS`]. Applies the SAME visibility predicate as
    /// [`Self::list_facts_visible`], keyed on the fact's source meeting — a
    /// sealed-and-not-unlocked meeting surfaces NOTHING (and its facts are purged on seal anyway;
    /// this is the defense-in-depth read gate).
    pub fn facts_for_meeting_visible(
        &self,
        meeting_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<String>> {
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT ft.subject, ft.predicate, ft.object
               FROM facts ft
               JOIN meetings m ON m.id = ft.meeting_id
              WHERE ft.meeting_id = ?1
                AND (NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id)
                     OR EXISTS (SELECT 1 FROM notes n
                                 LEFT JOIN folders f ON f.id = n.folder_id
                                WHERE n.meeting_id = m.id AND {visible}))
              ORDER BY (ft.valid_to IS NULL) DESC, ft.valid_from DESC, ft.id DESC
              LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(
                rusqlite::params![meeting_id, crate::embed::AUG_MAX_FACTS as i64],
                |r| {
                    let s: String = r.get(0)?;
                    let p: String = r.get(1)?;
                    let o: String = r.get(2)?;
                    Ok(format!("{s} {p}: {o}"))
                },
            )
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Brain v2 L1.1 — (re)index one VISIBLE meeting's TOPIC segments into `topic_chunks` +
    /// `topic_vec_chunks` (+ `fts_topic_chunks` via triggers): [`crate::embed::segment_topics`]
    /// over the plaintext segments, each topic AUGMENTED with the gated
    /// `<title> | <date> | <attendees> | <facts>` header ([`crate::embed::augment_chunk_text`])
    /// before embedding/FTS.
    ///
    /// GATED: a sealed-and-not-in-`unlocked` meeting is a NO-OP (`meeting_is_visible` — the same
    /// predicate as every read; its plaintext is never chunked). IDEMPOTENT by `content_hash`
    /// (over the AUGMENTED text): when the stored hash sequence equals the fresh one, the call
    /// returns without re-embedding — what makes the startup backfill cheap on every launch.
    /// Otherwise: PURGE-then-INSERT of this meeting's topic rows in ONE transaction (clean
    /// replace). Call AFTER [`Self::index_meeting_chunks`] at shared call sites — its clean
    /// replace purges ALL chunk classes (the shared `purge_chunks_tx` choke point), topic rows
    /// included. Vectors follow the no-stub-vector-at-rest policy: callers only pass a real
    /// embedder (same contract as `index_meeting_chunks`).
    pub fn index_meeting_topic_chunks(
        &self,
        meeting_id: &str,
        segments: &[Segment],
        embedder: &dyn Embedder,
        unlocked: &HashSet<String>,
    ) -> Result<()> {
        if !self.meeting_is_visible(meeting_id, unlocked)? {
            return Ok(()); // sealed-not-unlocked: never index its plaintext.
        }
        let Some(meeting) = self.get_meeting(meeting_id)? else {
            return Ok(());
        };
        let title = meeting
            .title
            .clone()
            .unwrap_or_else(|| "(untitled)".to_string());
        let date = meeting
            .started_at
            .split(['T', ' '])
            .next()
            .unwrap_or("")
            .to_string();

        let topics = crate::embed::segment_topics(segments);
        let (attendees, facts) = self.augment_header_inputs(meeting_id, unlocked)?;
        let augs: Vec<String> = topics
            .iter()
            .map(|t| crate::embed::augment_chunk_text(&title, &date, &attendees, &facts, &t.text))
            .collect();
        let hashes: Vec<String> = augs
            .iter()
            .map(|a| format!("{:016x}", chunk_hash(a)))
            .collect();

        // Idempotency probe: identical stored hash sequence ⇒ nothing to do (no re-embed).
        {
            let conn = self.lock();
            let mut stmt = conn
                .prepare(
                    "SELECT content_hash FROM topic_chunks WHERE meeting_id = ?1 ORDER BY seg_index",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![meeting_id], |r| {
                    r.get::<_, Option<String>>(0)
                })
                .map_err(map_err)?;
            let mut existing: Vec<String> = Vec::new();
            for r in rows {
                existing.push(r.map_err(map_err)?.unwrap_or_default());
            }
            if existing == hashes {
                return Ok(());
            }
        }

        let vectors = if augs.is_empty() {
            Vec::new()
        } else {
            embedder.embed_passage(&augs)?
        };

        // PURGE-then-INSERT this meeting's topic rows in ONE transaction (clean replace).
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        // TOCTOU re-check INSIDE the write tx: the visibility gate above ran BEFORE the slow
        // embed, and the caller's `unlocked` snapshot can be stale by now. A `lock_folder`
        // committing mid-embed blanks the note plaintext (`markdown=''`, `content_blob` kept) —
        // that DB-side sealed-at-rest invariant is session-independent, so key the re-check on
        // it instead of the snapshot: if the meeting is sealed at rest RIGHT NOW, writing its
        // derived plaintext topic rows would leave sealed content on disk until the next
        // relock/startup reconcile. Refuse (rollback via drop) instead.
        let sealed_at_rest: bool = tx
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM notes
                    WHERE meeting_id = ?1
                      AND content_blob IS NOT NULL
                      AND (markdown IS NULL OR markdown = '')
                 )",
                rusqlite::params![meeting_id],
                |r| Ok(r.get::<_, i64>(0)? != 0),
            )
            .map_err(map_err)?;
        if sealed_at_rest {
            return Ok(());
        }
        tx.execute(
            "DELETE FROM topic_vec_chunks WHERE chunk_id IN
               (SELECT id FROM topic_chunks WHERE meeting_id = ?1)",
            rusqlite::params![meeting_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "DELETE FROM topic_chunks WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
        )
        .map_err(map_err)?;
        {
            let mut ins_chunk = tx
                .prepare(
                    "INSERT INTO topic_chunks
                       (meeting_id, seg_index, start_s, end_s, text, aug_text, content_hash)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .map_err(map_err)?;
            let mut ins_vec = tx
                .prepare("INSERT INTO topic_vec_chunks(chunk_id, embedding) VALUES (?1, ?2)")
                .map_err(map_err)?;
            for (idx, ((topic, aug_text), vector)) in topics
                .iter()
                .zip(augs.iter())
                .zip(vectors.iter())
                .enumerate()
            {
                ins_chunk
                    .execute(rusqlite::params![
                        meeting_id,
                        idx as i64,
                        topic.start_s,
                        topic.end_s,
                        topic.text,
                        aug_text,
                        hashes[idx]
                    ])
                    .map_err(map_err)?;
                let chunk_id = tx.last_insert_rowid();
                let blob = crate::embed::vec_to_blob(vector);
                ins_vec
                    .execute(rusqlite::params![chunk_id, blob])
                    .map_err(map_err)?;
            }
        }
        tx.commit().map_err(map_err)?;
        tracing::debug!(
            target: "rag",
            meeting_id,
            topics = topics.len(),
            "topic chunks indexed"
        );
        Ok(())
    }

    /// Brain v2 L1.1 — STARTUP BACKFILL: index topic chunks for every VISIBLE meeting that has
    /// segments, in batches of 20 (with a short pause between batches
    /// so the shared connection lock breathes). Content-hash idempotent — an already-indexed
    /// meeting is a cheap probe, so re-running on every launch is fine. Runs under an EMPTY unlock
    /// set (nothing is session-unlocked at startup; sealed meetings are skipped by the gate and
    /// their topic rows are re-derived on unlock via `reindex_meetings_after_unseal`). Returns how
    /// many meetings were (re)indexed. Per-meeting failures WARN (ids only, no PII) and continue.
    pub fn backfill_topic_chunks_idempotent(&self, embedder: &dyn Embedder) -> Result<usize> {
        const TOPIC_BACKFILL_BATCH: usize = 20;
        let unlocked: HashSet<String> = HashSet::new();
        let meetings = self.list_meetings_visible(100_000, &unlocked)?;
        let mut indexed = 0usize;
        for batch in meetings.chunks(TOPIC_BACKFILL_BATCH) {
            for m in batch {
                let segments = match self.get_segments(&m.id) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(target: "rag", error = %e, "topic backfill: segments read failed (skipped)");
                        continue;
                    }
                };
                if segments.is_empty() {
                    continue;
                }
                match self.index_meeting_topic_chunks(&m.id, &segments, embedder, &unlocked) {
                    Ok(()) => indexed += 1,
                    Err(e) => {
                        tracing::warn!(target: "rag", error = %e, "topic backfill: indexing one meeting failed (skipped)");
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        Ok(indexed)
    }

    /// Purge (delete) every `note_chunks` + `vec_chunks` row for the given meetings. The vec0 row is
    /// deleted by its `chunk_id` (== note_chunks.id) BEFORE the note_chunks row, then the note_chunks
    /// rows go. Used standalone (lock_folder) and inside the relock transactions.
    ///
    /// Returns the `exported_path`s of the memory rollups purged in the same transaction (see
    /// `purge_memory_rollups_tx`) — the CALLER must delete those vault `.md` files (same layering
    /// as the sealed-note `.md` deletion: DB rows in-tx here, filesystem at the command layer).
    pub fn purge_chunks_for_meetings(&self, meeting_ids: &[String]) -> Result<Vec<String>> {
        if meeting_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        Self::purge_chunks_tx(&tx, meeting_ids)?;
        // Phase F0: the seal-into-locked callers (lock_folder, move-into-locked) also drop the
        // sealed meetings' correction-log rows in the SAME tx — a sealed meeting feeds no flywheel.
        Self::purge_corrections_tx(&tx, meeting_ids)?;
        // Voice-assistant Q&A log is plaintext-derived convenience data mirroring sealed content —
        // drop it in the SAME seal tx (purge-on-seal, like corrections). Dropped by design.
        Self::purge_assistant_interactions_tx(&tx, meeting_ids)?;
        // Brain v2 L4 LOCK-SAFETY: the live-bullets crash-recovery row is plaintext-derived
        // running notes of the meeting — drop it in the SAME seal tx (purge-on-seal, like the
        // assistant interactions above). Dropped by design; the transcript stays sealed+restorable.
        Self::purge_live_bullets_tx(&tx, meeting_ids)?;
        // brain2 R2: bitemporal facts are DERIVED content tied to the meeting (plaintext at rest);
        // drop them in the SAME seal tx so a sealed meeting contributes no fact — same purge-on-seal
        // contract as corrections / chunks / assistant interactions.
        Self::purge_facts_tx(&tx, meeting_ids)?;
        // Phase 3 CROSS-MEETING USER MEMORY: user-scoped facts are DERIVED content tied to the source
        // meeting (plaintext at rest); drop them in the SAME seal tx so a sealed meeting contributes
        // no user memory — identical purge-on-seal contract as `facts` above. The injected brief is
        // derived data (never sealed) → the next read regenerates it from the remaining VISIBLE
        // sources only (design spec D3).
        Self::purge_user_facts_tx(&tx, meeting_ids)?;
        // VOICEPRINT LOCK-SAFETY: drop the (opt-in) voice biometrics captured for these meetings in
        // the SAME seal tx — a sealed meeting's remote-speaker voiceprint must not linger at rest,
        // same purge-on-seal contract as `user_facts` above. Re-derivable on a later re-diarize.
        Self::purge_speaker_voiceprints_tx(&tx, meeting_ids)?;
        // Re-Truth LOCK-SAFETY: an APPLIED supersession stores the PLAINTEXT note pre-image (undo
        // scratch). Drop every supersession referencing a sealed meeting (either side) in the SAME
        // seal tx so no plaintext note content lingers at rest for a sealed folder — identical
        // purge-on-seal contract as `facts` / `user_facts` above. The stamp itself rides INSIDE the
        // sealed note content and returns (with the stamp) on unlock/remove-lock; only the undo
        // scratch is dropped.
        Self::purge_supersessions_tx(&tx, meeting_ids)?;
        // Brain v2 L5 LOCK-SAFETY: a PENDING scheduled-brief row (`brief_runs.note_md`) is a
        // cross-meeting synthesis of the referenced meetings' notes — purge any pending run
        // referencing a just-sealed meeting in this SAME seal tx (accepted rows were consumed on
        // accept and carry no content). Same purge-on-seal contract as the rollups below.
        Self::purge_pending_brief_runs_tx(&tx, meeting_ids)?;
        // Brain v2 L2.1 LOCK-SAFETY: memory ROLLUPS are cross-meeting synthesis that may paraphrase
        // the just-sealed facts — purge ALL of them in this SAME seal tx (cheap, re-derivable: the
        // next hourly pass regenerates from the still-VISIBLE facts only). The caller deletes the
        // returned exported vault `.md`s.
        let rollup_exports = Self::purge_memory_rollups_tx(&tx)?;
        tx.commit().map_err(map_err)?;
        Ok(rollup_exports)
    }

    /// Brain v2 L2.1 LOCK-SAFETY: delete EVERY `memory_rollups` row within an EXISTING transaction
    /// and return the recorded `exported_path`s so the CALLER can remove the exported vault `.md`s
    /// (never any other file — only the paths this table recorded at export time; a missing file is
    /// fine). ALL rollups go, not just the sealed scope's: a rollup is cross-meeting synthesis with
    /// no single source meeting, so precision-purging is impossible — and rollups are cheap
    /// re-derivable synthesis that the next hourly pass regenerates FROM VISIBLE FACTS ONLY.
    fn purge_memory_rollups_tx(tx: &rusqlite::Transaction<'_>) -> Result<Vec<String>> {
        let mut stmt = tx
            .prepare("SELECT exported_path FROM memory_rollups WHERE exported_path IS NOT NULL")
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        let mut paths = Vec::new();
        for r in rows {
            paths.push(r.map_err(map_err)?);
        }
        drop(stmt);
        tx.execute("DELETE FROM memory_rollups", []).map_err(map_err)?;
        Ok(paths)
    }

    /// Brain v2 L5 LOCK-SAFETY (lock-security LEAK fix, 2026-07-10): delete every PENDING
    /// `brief_runs` row whose `meeting_ids` references any of `meeting_ids`, within an EXISTING
    /// transaction. A pending brief's `note_md` is a cross-meeting SYNTHESIS of the referenced
    /// meetings' notes — the same derived-plaintext class as memory rollups, one layer removed —
    /// so it must not survive the seal (or deletion) of any source meeting: un-purged it stays
    /// readable via `list_brief_runs` and exportable via `accept_brief`, outside every gate.
    /// ACCEPTED rows are left alone: their `note_md` was CONSUMED on accept (blanked — the
    /// exported vault `.md` became the copy), so the row holds only ids + timestamps.
    ///
    /// Matching (documented choice): `meeting_ids` is a JSON TEXT array of quote-delimited UUID
    /// strings, so the per-id `LIKE '%"<id>"%'` intersection is exact — a UUID (hex + hyphens,
    /// no `%`/`_`/`"`) can never partially match inside another quoted id. Simpler than parsing
    /// the JSON in Rust and it stays a pure per-id statement inside the caller's seal tx.
    fn purge_pending_brief_runs_tx(
        tx: &rusqlite::Transaction<'_>,
        meeting_ids: &[String],
    ) -> Result<()> {
        for mid in meeting_ids {
            tx.execute(
                "DELETE FROM brief_runs WHERE status = 'pending' \
                   AND meeting_ids LIKE '%\"' || ?1 || '\"%'",
                rusqlite::params![mid],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    /// brain2 R2 LOCK-SAFETY: delete every `facts` row for `meeting_ids` within an EXISTING
    /// transaction, so the purge lands in the SAME atomic unit as the plaintext blanking on a seal
    /// (and on `delete_meeting` / the startup reconcile). Facts are plaintext-derived (entity ·
    /// predicate · object) content that mirrors a meeting; a sealed meeting must surface NOTHING, so
    /// — exactly like `correction_log` / `note_chunks` / `assistant_interactions` — we DELETE rather
    /// than key-seal. Dropped by design + not recoverable (never keyed); the underlying transcript is
    /// still sealed + restorable, and a later re-summarize re-derives facts.
    fn purge_facts_tx(tx: &rusqlite::Transaction<'_>, meeting_ids: &[String]) -> Result<()> {
        for mid in meeting_ids {
            tx.execute(
                "DELETE FROM facts WHERE meeting_id = ?1",
                rusqlite::params![mid],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    /// Re-Truth cascade: delete every `supersessions` row referencing `meeting_ids` on EITHER side
    /// (superseding OR source) within an EXISTING transaction, so a deleted meeting leaves no dangling
    /// supersession behind. The table carries no foreign key (a row references two meetings), so this
    /// explicit purge — mirroring `purge_facts_tx` — is what keeps `delete_meeting` clean. Idempotent.
    fn purge_supersessions_tx(
        tx: &rusqlite::Transaction<'_>,
        meeting_ids: &[String],
    ) -> Result<()> {
        for mid in meeting_ids {
            tx.execute(
                "DELETE FROM supersessions
                   WHERE superseding_meeting_id = ?1 OR source_meeting_id = ?1",
                rusqlite::params![mid],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    /// Phase 3 CROSS-MEETING USER MEMORY LOCK-SAFETY: delete every `user_facts` row derived from
    /// `meeting_ids` within an EXISTING transaction, so the purge lands in the SAME atomic unit as
    /// the plaintext blanking on a seal (and on `delete_meeting` / the startup reconcile). User facts
    /// are plaintext-derived (subject · predicate · object) memory that mirrors a meeting; a sealed
    /// meeting must surface NOTHING and inject NOTHING, so — exactly like `facts` / `correction_log`
    /// / `note_chunks` / `assistant_interactions` — we DELETE rather than key-seal. Dropped by design
    /// and not recoverable (never keyed); the underlying transcript is still sealed + restorable, and a
    /// later re-summarize re-derives user facts. The (derived) memory brief is regenerated on the
    /// next read from the remaining VISIBLE user facts only.
    fn purge_user_facts_tx(tx: &rusqlite::Transaction<'_>, meeting_ids: &[String]) -> Result<()> {
        for mid in meeting_ids {
            tx.execute(
                "DELETE FROM user_facts WHERE meeting_id = ?1",
                rusqlite::params![mid],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    /// VOICEPRINT LOCK-SAFETY: delete every `speaker_voiceprints` row derived from `meeting_ids`
    /// within an EXISTING transaction, so the purge lands in the SAME atomic unit as the plaintext
    /// blanking on a seal (and on `delete_meeting` / the startup reconcile). A voiceprint is a voice
    /// BIOMETRIC derived from the meeting's system audio; a sealed meeting must surface NOTHING, so —
    /// exactly like `user_facts` / `facts` / `note_chunks` — we DELETE rather than key-seal. Dropped by
    /// design and not recoverable from the row (never keyed); the underlying audio is still sealed +
    /// restorable, and a later re-diarize (with the opt-in on) re-derives the voiceprint. This is the
    /// stricter-safe choice: a biometric of a locked speaker must not linger at rest.
    fn purge_speaker_voiceprints_tx(
        tx: &rusqlite::Transaction<'_>,
        meeting_ids: &[String],
    ) -> Result<()> {
        for mid in meeting_ids {
            tx.execute(
                "DELETE FROM speaker_voiceprints WHERE meeting_id = ?1",
                rusqlite::params![mid],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    /// Delete chunk rows for `meeting_ids` within an EXISTING transaction (so the purge lands in the
    /// same atomic unit as the plaintext blanking on lock — no window where a vector outlives the
    /// sealed plaintext it was derived from).
    fn purge_chunks_tx(tx: &rusqlite::Transaction<'_>, meeting_ids: &[String]) -> Result<()> {
        for mid in meeting_ids {
            // vec0 first (its FK-less rowid mirrors note_chunks.id), then the source rows.
            tx.execute(
                "DELETE FROM vec_chunks WHERE chunk_id IN
                   (SELECT id FROM note_chunks WHERE meeting_id = ?1)",
                rusqlite::params![mid],
            )
            .map_err(map_err)?;
            tx.execute(
                "DELETE FROM note_chunks WHERE meeting_id = ?1",
                rusqlite::params![mid],
            )
            .map_err(map_err)?;
            // Brain v2 L1.1 — TOPIC chunks ride the SAME choke point, so every seal path
            // (lock_folder / blank_sealed_notes_in_folders / reblank_locked_folders_at_rest /
            // delete_meeting) purges them atomically with the note chunks. vec0 first (FK-less),
            // then the base rows (whose `_ad` FTS trigger purges the aug_text tokens).
            tx.execute(
                "DELETE FROM topic_vec_chunks WHERE chunk_id IN
                   (SELECT id FROM topic_chunks WHERE meeting_id = ?1)",
                rusqlite::params![mid],
            )
            .map_err(map_err)?;
            tx.execute(
                "DELETE FROM topic_chunks WHERE meeting_id = ?1",
                rusqlite::params![mid],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    /// Phase F0 LOCK-SAFETY: delete every `correction_log` row for `meeting_ids` within an EXISTING
    /// transaction, so the purge lands in the SAME atomic unit as the plaintext blanking on a seal
    /// (and on `delete_meeting`). The flywheel is plaintext-derived training data; a sealed meeting
    /// must contribute NOTHING to it. This is INTENTIONAL and privacy-first: sealed meetings are
    /// simply not used for fine-tuning, and the data is not recoverable from the blob (it was never
    /// keyed) — so we delete rather than seal. Note: this is deliberately NOT folded into
    /// `purge_chunks_tx`, because that helper also runs on the (non-seal) re-index "clean replace"
    /// path where corrections must survive.
    fn purge_corrections_tx(tx: &rusqlite::Transaction<'_>, meeting_ids: &[String]) -> Result<()> {
        for mid in meeting_ids {
            tx.execute(
                "DELETE FROM correction_log WHERE meeting_id = ?1",
                rusqlite::params![mid],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    /// GATED semantic (vector KNN) search. Runs a `vec0` KNN for the top-`k` nearest chunks, then
    /// applies EXACTLY the `search_visible` visibility predicate (a meeting is kept iff it has a
    /// VISIBLE note row — open/NULL folder OR session-unlocked) so a sealed-and-not-unlocked meeting
    /// is excluded even if a stray chunk survived. Dedups to one hit per meeting (best/nearest).
    pub fn search_semantic_visible(
        &self,
        query_vec: &[f32],
        k: i64,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<SearchHit>> {
        if query_vec.is_empty() || k <= 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        // KNN is isolated to the vec0 table in a CTE (only a single MATCH+k constraint is allowed on
        // a vec0 query); visibility + meeting columns are joined OUTSIDE it.
        let sql = format!(
            "WITH knn(chunk_id, distance) AS (
                 SELECT chunk_id, distance FROM vec_chunks
                  WHERE embedding MATCH ?1 AND k = ?2
                  ORDER BY distance
             )
             SELECT m.id, m.started_at, m.ended_at, m.title, m.duration_s, m.audio_path, m.status,
                    (SELECT nf.folder_id FROM notes nf
                      WHERE nf.meeting_id = m.id AND nf.folder_id IS NOT NULL LIMIT 1) AS folder_id,
                    nc.text, knn.distance
               FROM knn
               JOIN note_chunks nc ON nc.id = knn.chunk_id
               JOIN meetings m ON m.id = nc.meeting_id
              WHERE EXISTS (
                      SELECT 1 FROM notes n
                       LEFT JOIN folders f ON f.id = n.folder_id
                       WHERE n.meeting_id = m.id AND {visible}
                    )
              ORDER BY knn.distance ASC, m.id ASC"
        );
        let blob = crate::embed::vec_to_blob(query_vec);
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![blob, k], |row| {
                let meeting = row_to_meeting(row)?;
                let snippet: String = row.get(8)?;
                Ok((meeting, snippet))
            })
            .map_err(map_err)?;

        let mut seen: HashSet<String> = HashSet::new();
        let mut hits = Vec::new();
        for r in rows {
            let (meeting, snippet) = r.map_err(map_err)?;
            let meeting = meeting?;
            if !seen.insert(meeting.id.clone()) {
                continue; // already have a nearer chunk for this meeting.
            }
            hits.push(SearchHit {
                meeting,
                snippet,
                matched_in: "semantic".to_string(),
            });
        }
        Ok(hits)
    }

    /// Meetings most semantically similar to `meeting_id`, GATED through `search_semantic_visible`
    /// (which applies `visibility_clause`, so a sealed-not-session-unlocked neighbour is excluded).
    ///
    /// The query vector is RE-EMBEDDED from this meeting's own plaintext `note_chunks.text` (a plain
    /// table SELECT — we deliberately do NOT read embeddings back out of the vec0 table) and reduced
    /// to its L2-normalized mean (centroid). `embed_passage` matches the convention used to index the
    /// chunks in `index_meeting_chunks`.
    ///
    /// Natural gating: chunks are PURGED on lock, so a sealed `meeting_id` has zero chunk texts ⇒
    /// `Ok(vec![])` (nothing to embed, no leak). Self is filtered out (a meeting is always its own
    /// nearest neighbour) and the list is truncated to `k`. NEVER panics; logs only id/count.
    ///
    /// The embedder is injected (not pulled from `active_embedder` here) so gating tests can pass a
    /// deterministic `StubEmbedder`; the command layer passes `active_embedder().as_ref()`.
    pub fn related_meetings_visible(
        &self,
        meeting_id: &str,
        embedder: &dyn crate::embed::Embedder,
        k: i64,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<SearchHit>> {
        if k <= 0 {
            return Ok(Vec::new());
        }
        // Read THIS meeting's own chunk plaintext. Purged-on-lock ⇒ a sealed meeting yields zero
        // rows ⇒ empty result (no leak, no need for an explicit unlock check on the source).
        let texts: Vec<String> = {
            let conn = self.lock();
            let mut stmt = conn
                .prepare("SELECT text FROM note_chunks WHERE meeting_id = ?1 ORDER BY chunk_idx")
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![meeting_id], |row| row.get::<_, String>(0))
                .map_err(map_err)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(map_err)?);
            }
            out
        };
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Query vector = L2-normalized centroid of the per-chunk passage embeddings.
        let vectors = embedder.embed_passage(&texts)?;
        let dim = embedder.dim();
        let mut centroid = vec![0f32; dim];
        let mut counted = 0usize;
        for v in &vectors {
            if v.len() != dim {
                continue; // defensive: skip a malformed vector rather than panic.
            }
            for (acc, x) in centroid.iter_mut().zip(v.iter()) {
                *acc += *x;
            }
            counted += 1;
        }
        if counted == 0 {
            return Ok(Vec::new());
        }
        for x in centroid.iter_mut() {
            *x /= counted as f32;
        }
        let norm = centroid.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm == 0.0 {
            return Ok(Vec::new()); // degenerate (all-zero) centroid — no meaningful direction.
        }
        for x in centroid.iter_mut() {
            *x /= norm;
        }

        // GATED KNN. Ask for k+1 because self is always the nearest hit; then drop self + truncate.
        let mut hits = self.search_semantic_visible(&centroid, k + 1, unlocked)?;
        hits.retain(|h| h.meeting.id != meeting_id);
        hits.truncate(k as usize);
        tracing::debug!(
            target: "embed",
            meeting_id,
            returned = hits.len(),
            "related_meetings_visible"
        );
        Ok(hits)
    }

    /// The FTS leg with RAW SCORES (Brain v2 L1.3): best-per-meeting `-bm25` (HIGHER = better)
    /// over ALL FOUR lexical sources — `fts_meetings` ∪ `fts_segments` ∪ `fts_notes` ∪ the
    /// topic-chunk `fts_topic_chunks` (whose AUGMENTED text carries the attendee/fact header).
    /// Gated by the SAME visibility predicate as `search_visible`; optional `started_at` window.
    fn fts_meeting_scores(
        &self,
        query: &str,
        limit: i64,
        unlocked: &HashSet<String>,
        date_filter: Option<&(String, String)>,
    ) -> Result<Vec<(String, f64)>> {
        let q = query.trim();
        let Some(match_expr) = fts_match_query(q) else {
            return Ok(Vec::new());
        };
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let date = date_clause(date_filter);
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
                 UNION ALL
                 SELECT tc.meeting_id, bm25(fts_topic_chunks)
                   FROM fts_topic_chunks
                   JOIN topic_chunks tc ON tc.id = fts_topic_chunks.rowid
                  WHERE fts_topic_chunks MATCH ?1
             ),
             ranked(meeting_id, rank) AS (
                 SELECT meeting_id, MIN(rank) FROM hits GROUP BY meeting_id
             )
             SELECT m.id, r.rank
               FROM ranked r
               JOIN meetings m ON m.id = r.meeting_id
              WHERE (NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id)
                 OR EXISTS (
                      SELECT 1 FROM notes n
                       LEFT JOIN folders f ON f.id = n.folder_id
                       WHERE n.meeting_id = m.id AND {visible}
                    )){date}
              ORDER BY r.rank ASC, m.started_at DESC, m.id DESC
              LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![match_expr, limit], |r| {
                let id: String = r.get(0)?;
                let rank: f64 = r.get(1)?;
                Ok((id, -rank)) // FTS5 bm25() is lower/more-negative = better ⇒ negate to higher-better.
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// The KNN leg with RAW DISTANCES (Brain v2 L1.3): best-per-meeting (smallest) vec0 distance
    /// over BOTH vector tables — `vec_chunks` (note/transcript chunks) ∪ `topic_vec_chunks`
    /// (topic chunks). LOWER = better; `score_fuse` inverts via `1/(1+d)`. Gated by the SAME
    /// visibility predicate as `search_semantic_visible`; optional `started_at` window.
    fn knn_meeting_distances(
        &self,
        query_vec: &[f32],
        k: i64,
        unlocked: &HashSet<String>,
        date_filter: Option<&(String, String)>,
    ) -> Result<Vec<(String, f64)>> {
        if query_vec.is_empty() || k <= 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let date = date_clause(date_filter);
        // Each vec0 table gets its own single-MATCH CTE (a vec0 query allows exactly one MATCH+k
        // constraint); the union + visibility + window join happen OUTSIDE the KNN CTEs.
        let sql = format!(
            "WITH knn_note(chunk_id, distance) AS (
                 SELECT chunk_id, distance FROM vec_chunks
                  WHERE embedding MATCH ?1 AND k = ?2
             ),
             knn_topic(chunk_id, distance) AS (
                 SELECT chunk_id, distance FROM topic_vec_chunks
                  WHERE embedding MATCH ?1 AND k = ?2
             ),
             hits(meeting_id, distance) AS (
                 SELECT nc.meeting_id, kn.distance
                   FROM knn_note kn JOIN note_chunks nc ON nc.id = kn.chunk_id
                 UNION ALL
                 SELECT tc.meeting_id, kt.distance
                   FROM knn_topic kt JOIN topic_chunks tc ON tc.id = kt.chunk_id
             ),
             best(meeting_id, distance) AS (
                 SELECT meeting_id, MIN(distance) FROM hits GROUP BY meeting_id
             )
             SELECT m.id, b.distance
               FROM best b
               JOIN meetings m ON m.id = b.meeting_id
              WHERE EXISTS (
                      SELECT 1 FROM notes n
                       LEFT JOIN folders f ON f.id = n.folder_id
                       WHERE n.meeting_id = m.id AND {visible}
                    ){date}
              ORDER BY b.distance ASC, m.id ASC"
        );
        let blob = crate::embed::vec_to_blob(query_vec);
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![blob, k], |r| {
                let id: String = r.get(0)?;
                let d: f64 = r.get(1)?;
                Ok((id, d))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Hybrid retrieval (GraphRAG-lite, Phase 2d; SCORE FUSION since Brain v2 L1.3): blend THREE
    /// already-visibility-gated legs — keyword FTS (now incl. the augmented topic-chunk index),
    /// vector KNN (now over note ∪ topic vectors), and the entity-graph neighbourhood
    /// (`meetings_mentioning_entities_visible` for the entities the query names) — via
    /// [`crate::embed::score_fuse`] over RAW scores (per-leg min-max, weights 0.4/0.4/0.2), dedup
    /// by meeting, return up to `limit` hits best-first. [`crate::embed::rrf_fuse`] remains the
    /// FALLBACK when no raw-scored leg produced anything but the plain hit lists did (defensive).
    ///
    /// `date_filter` (Brain v2 L1.5) is an optional `(from_iso, to_iso_exclusive)` `started_at`
    /// window applied to EVERY leg (FTS + KNN in SQL; the graph leg + the fallback in Rust) — a
    /// temporal query retrieves only in-window meetings. With a window and NO lexical FTS match,
    /// the window itself becomes the FTS leg (visible meetings in range, newest-first) — the
    /// "what did we discuss last week" shape. All legs route through the SAME
    /// `visibility_clause`, so the fused output stays gated.
    pub fn search_hybrid_visible(
        &self,
        query: &str,
        query_vec: &[f32],
        limit: i64,
        unlocked: &HashSet<String>,
        date_filter: Option<(String, String)>,
    ) -> Result<Vec<SearchHit>> {
        let range = date_filter.as_ref();
        let in_range = |m: &Meeting| -> bool {
            match range {
                Some((from, to)) => {
                    m.started_at.as_str() >= from.as_str() && m.started_at.as_str() < to.as_str()
                }
                None => true,
            }
        };

        // Snippet-bearing hit lists (each already gated); ordering comes from the scored legs.
        let fts = self.search_visible_impl(query, limit, unlocked, range)?;
        let semantic = self.search_semantic_visible(query_vec, limit, unlocked)?;

        // GraphRAG-lite leg: resolve the query to known VISIBLE entities (deterministic, no LLM),
        // then gather their co-mention neighbourhood. Both the resolver and the neighbour reader
        // apply the same visibility predicate; the temporal window is applied here in Rust.
        let matched_entities = self.entities_matching_query(query, unlocked)?;
        let mut graph = self.meetings_mentioning_entities_visible(&matched_entities, unlocked)?;
        graph.retain(|m| in_range(m));

        // RAW-SCORED legs (L1.3). FTS: -bm25 higher-better over all four lexical sources. KNN:
        // raw distances over both vector tables. Graph: 1/rank of the neighbourhood ordering.
        let mut fts_scored = self.fts_meeting_scores(query, limit, unlocked, range)?;
        if fts_scored.is_empty() && range.is_some() {
            // Temporal fallback (L1.5): no lexical match inside the window ⇒ the window IS the
            // query — visible in-range meetings, newest-first, positionally scored.
            let in_window = self.meetings_in_range_visible(range, limit, unlocked)?;
            fts_scored = in_window
                .iter()
                .enumerate()
                .map(|(i, m)| (m.id.clone(), 1.0 / (i as f64 + 1.0)))
                .collect();
        }
        let knn_scored = self.knn_meeting_distances(query_vec, limit, unlocked, range)?;
        let graph_scored: Vec<(String, f64)> = graph
            .iter()
            .enumerate()
            .map(|(i, m)| (m.id.clone(), 1.0 / (i as f64 + 1.0)))
            .collect();

        // One hit per meeting id. Insert the graph leg FIRST (lowest snippet priority): a meeting
        // also hit by FTS/vector keeps the lexical/semantic snippet the user actually queried;
        // a graph-only neighbour carries an "entity" marker + its title as the snippet.
        let mut by_id: std::collections::HashMap<String, SearchHit> =
            std::collections::HashMap::new();
        for m in graph {
            let snippet = m.title.clone().unwrap_or_default();
            by_id.insert(
                m.id.clone(),
                SearchHit {
                    meeting: m,
                    snippet,
                    matched_in: "entity".to_string(),
                },
            );
        }
        for h in semantic {
            by_id.insert(h.meeting.id.clone(), h);
        }
        for h in &fts {
            by_id.insert(h.meeting.id.clone(), h.clone());
        }

        let mut fused = crate::embed::score_fuse(&fts_scored, &knn_scored, &graph_scored);
        if fused.is_empty() {
            // Defensive RRF fallback: raw-scored legs empty but a plain hit list is not (should
            // not happen — same queries — but never return less than the pre-L1.3 behavior).
            // Respect the window: semantic hits are filtered in Rust (their SQL has no date arm).
            let fts_ids: Vec<String> = fts.iter().map(|h| h.meeting.id.clone()).collect();
            let sem_ids: Vec<String> = by_id
                .values()
                .filter(|h| h.matched_in == "semantic" && in_range(&h.meeting))
                .map(|h| h.meeting.id.clone())
                .collect();
            fused = crate::embed::rrf_fuse(&[fts_ids, sem_ids], crate::embed::RRF_K);
        }

        let cap = if limit < 0 { 0 } else { limit as usize };
        let mut out = Vec::new();
        for (id, _score) in fused.into_iter().take(cap) {
            if let Some(hit) = by_id.remove(&id) {
                out.push(hit);
                continue;
            }
            // A meeting surfaced ONLY by a topic leg (augmented FTS / topic KNN) or the temporal
            // fallback has no snippet-bearing hit yet — synthesize one. The id came from a GATED
            // leg, so reading the meeting row + a topic snippet here is gated-by-construction.
            if let Some(meeting) = self.get_meeting(&id)? {
                let snippet = self
                    .first_topic_snippet(&id)?
                    .or_else(|| meeting.title.clone())
                    .unwrap_or_default();
                out.push(SearchHit {
                    meeting,
                    snippet,
                    matched_in: "topic".to_string(),
                });
            }
        }
        Ok(out)
    }

    /// First topic-chunk RAW text for a meeting (snippet synthesis for topic-leg-only hybrid
    /// hits). Rows exist only for visible meetings (purged on seal); callers reach here only with
    /// ids from gated legs.
    fn first_topic_snippet(&self, meeting_id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT text FROM topic_chunks WHERE meeting_id = ?1 ORDER BY seg_index LIMIT 1",
            rusqlite::params![meeting_id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(map_err)
    }

    /// Visible meetings whose `started_at` falls in the half-open `(from, to)` window,
    /// newest-first (the L1.5 temporal-fallback corpus). Same visibility predicate as
    /// [`Self::list_meetings_visible`]. `None` window ⇒ empty (callers only reach here with one).
    fn meetings_in_range_visible(
        &self,
        date_filter: Option<&(String, String)>,
        limit: i64,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<Meeting>> {
        let Some(range) = date_filter else {
            return Ok(Vec::new());
        };
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let date = date_clause(Some(range));
        let sql = format!(
            "SELECT m.id, m.started_at, m.ended_at, m.title, m.duration_s, m.audio_path, m.status,
                    (SELECT nf.folder_id FROM notes nf
                      WHERE nf.meeting_id = m.id AND nf.folder_id IS NOT NULL LIMIT 1)
                      AS folder_id
               FROM meetings m
              WHERE (NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id)
                 OR EXISTS (
                      SELECT 1 FROM notes n
                       LEFT JOIN folders f ON f.id = n.folder_id
                       WHERE n.meeting_id = m.id AND {visible}
                    )){date}
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

    // ── document ingestion (PARALLEL doc layer) ───────────────────────────────
    //
    // `documents` rows are uploaded md/txt files anchored to a FOLDER (the lock gate). The plaintext
    // `text` is SEALED-AND-RESTORED exactly like the note `markdown` (encrypt → `text_blob`, blank
    // plaintext on lock, decrypt back on unlock). `doc_chunks`/`doc_vec_chunks` are invertible PII
    // derived from the plaintext, so they exist ONLY for visible documents — purged on lock,
    // re-embeddable on unlock. Every read here is folder-gated by the COMMAND layer
    // (`folder_is_unlocked`) or by `visibility_clause` (`search_doc_chunks_visible`).

    /// Insert a `documents` row (plaintext `text`). The `id` + `created_at` are caller-supplied (the
    /// command generates a UUID + an epoch-millis timestamp). The folder must exist (FK).
    pub fn insert_document(
        &self,
        id: &str,
        folder_id: &str,
        name: &str,
        text: &str,
        kind: &str,
        created_at: i64,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO documents (id, folder_id, name, text, kind, text_blob, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
            rusqlite::params![id, folder_id, name, text, kind, created_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Lightweight metadata (NO text) for every document in a folder, newest-first. The COMMAND layer
    /// gates the folder before calling this (a sealed-not-unlocked folder returns the masked/empty
    /// list there) — this is a low-level read with no gating of its own.
    pub fn documents_in_folder(&self, folder_id: &str) -> Result<Vec<DocumentInfo>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, name, kind, created_at FROM documents
                   WHERE folder_id = ?1 ORDER BY created_at DESC, name",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |r| {
                Ok(DocumentInfo {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    kind: r.get(2)?,
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

    /// VISIBLE-content counts for the Brain page: `(meeting_count, document_count, note_count,
    /// indexed_chunk_count)`. Meetings + documents/notes are gated by `visibility_clause` (a
    /// sealed-not-unlocked folder's items are NOT counted). Chunks are PURGED on lock, so a plain
    /// total counts only what is currently indexed (= visible) and is leak-free (a bare number).
    pub fn brain_counts(&self, unlocked: &HashSet<String>) -> Result<(i64, i64, i64, i64)> {
        let conn = self.lock();
        // Meetings — mirror `list_meetings_visible`: visible if no notes OR any visible note.
        let m_visible = visibility_clause("f", unlocked);
        let meeting_count: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM meetings m
                       WHERE NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id)
                          OR EXISTS (SELECT 1 FROM notes n
                                       LEFT JOIN folders f ON f.id = n.folder_id
                                      WHERE n.meeting_id = m.id AND {m_visible})"
                ),
                [],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        // Documents + notes — gated by folder visibility, split by kind.
        let d_visible = visibility_clause("f", unlocked);
        let count_kind = |kind: &str| -> Result<i64> {
            conn.query_row(
                &format!(
                    "SELECT COUNT(*) FROM documents d
                       JOIN folders f ON f.id = d.folder_id
                      WHERE d.kind = ?1 AND {d_visible}"
                ),
                rusqlite::params![kind],
                |r| r.get(0),
            )
            .map_err(map_err)
        };
        let document_count = count_kind("document")?;
        let note_count = count_kind("note")?;
        // Chunks — purged on lock, so a bare total reflects only currently-indexed (visible) content.
        let note_chunks: i64 = conn
            .query_row("SELECT COUNT(*) FROM note_chunks", [], |r| r.get(0))
            .map_err(map_err)?;
        let doc_chunks: i64 = conn
            .query_row("SELECT COUNT(*) FROM doc_chunks", [], |r| r.get(0))
            .map_err(map_err)?;
        Ok((
            meeting_count,
            document_count,
            note_count,
            note_chunks + doc_chunks,
        ))
    }

    /// Number of `doc_chunks` rows currently indexed for a document (0 when sealed/purged or never
    /// embedded). A non-leaky count read — used by the lock tests to assert purge-on-lock /
    /// re-embed-on-unlock without reaching the private connection. Test-only (no production caller).
    #[cfg(test)]
    pub(crate) fn doc_chunk_count(&self, document_id: &str) -> Result<i64> {
        let conn = self.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM doc_chunks WHERE document_id = ?1",
            rusqlite::params![document_id],
            |r| r.get(0),
        )
        .map_err(map_err)
    }

    /// Number of `doc_vec_chunks` rows currently indexed for a document. Lets the tests assert the
    /// no-stub-vector contract (chunk-only indexing writes ZERO vectors). Test-only.
    #[cfg(test)]
    pub(crate) fn doc_vec_count(&self, document_id: &str) -> Result<i64> {
        let conn = self.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM doc_vec_chunks WHERE chunk_id IN
               (SELECT id FROM doc_chunks WHERE document_id = ?1)",
            rusqlite::params![document_id],
            |r| r.get(0),
        )
        .map_err(map_err)
    }

    /// Number of `note_chunks` rows currently indexed for a MEETING (0 when never indexed or purged
    /// on lock). The meeting analogue of [`Db::doc_chunk_count`] — used by the lock tests to assert
    /// purge-on-lock / re-index-on-unlock without reaching the private connection. Test-only.
    #[cfg(test)]
    pub(crate) fn note_chunk_count(&self, meeting_id: &str) -> Result<i64> {
        let conn = self.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM note_chunks WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
            |r| r.get(0),
        )
        .map_err(map_err)
    }

    /// Number of `vec_chunks` rows currently indexed for a MEETING. Lets the tests assert the absolute
    /// no-stub-vector contract on the unlock re-index path (model-absent ⇒ ZERO meeting vectors). The
    /// count JOINs through `note_chunks` (vec0 is FK-less) and is meeting-scoped. Test-only.
    #[cfg(test)]
    pub(crate) fn note_vec_count(&self, meeting_id: &str) -> Result<i64> {
        let conn = self.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM vec_chunks v
               JOIN note_chunks nc ON nc.id = v.chunk_id
              WHERE nc.meeting_id = ?1",
            rusqlite::params![meeting_id],
            |r| r.get(0),
        )
        .map_err(map_err)
    }

    /// A document's `(folder_id, name, plaintext text)`, or `None` if unknown. The COMMAND layer gates
    /// the folder before surfacing the text to the FE.
    pub fn get_document(&self, id: &str) -> Result<Option<(String, String, String)>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT folder_id, name, text FROM documents WHERE id = ?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(map_err)
    }

    /// The owning folder id for a document, or `None` if unknown. The folder-lock gate anchor.
    pub fn folder_for_document(&self, id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT folder_id FROM documents WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(map_err)
    }

    /// Distinct document ids governed by a folder's lock (its `documents` rows). Used to seal/unseal/
    /// purge each document's text + chunks.
    pub fn document_ids_in_folder(&self, folder_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id FROM documents WHERE folder_id = ?1")
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

    /// Raw `(text, text_blob)` for every document in a folder — the seal/unseal source-of-truth read
    /// (mirrors [`Db::raw_manual_notes`]). `text` is "" once sealed; `text_blob` carries the sealed
    /// copy. Used by the seal (encrypt+verify the plaintext), unseal (decrypt the blob), and reblank
    /// (re-blank only WHERE the blob exists) paths.
    pub fn raw_documents_in_folder(&self, folder_id: &str) -> Result<Vec<RawDocument>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id, COALESCE(text, ''), text_blob FROM documents WHERE folder_id = ?1")
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |r| {
                Ok(RawDocument {
                    id: r.get(0)?,
                    text: r.get(1)?,
                    blob: r.get(2)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
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

    /// Permanently delete a document (its `doc_chunks` + `doc_vec_chunks` go first in the same tx —
    /// `doc_vec_chunks` is a vec0 virtual table with no FK so the `documents` ON DELETE CASCADE
    /// reaches `doc_chunks` but NOT `doc_vec_chunks`; deleting them explicitly avoids orphan vectors,
    /// mirroring `delete_meeting`). Idempotent on an unknown id.
    pub fn delete_document(&self, id: &str) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        Self::purge_doc_chunks_tx(&tx, &[id.to_string()])?;
        tx.execute("DELETE FROM documents WHERE id = ?1", rusqlite::params![id])
            .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// (Re)index a VISIBLE document's plaintext into `doc_chunks` (always — the `fts_doc_chunks`
    /// triggers keep keyword retrieval alive on a default install) and `doc_vec_chunks` (only when
    /// `embedder` is `Some`, i.e. the REAL e5 model is present — `None` writes NO vectors, never
    /// stub ones). Old rows for the document are purged first (clean replace). Caller MUST only
    /// invoke this for visible/unlocked content — a sealed document's plaintext is blank, so
    /// chunking yields zero chunks (the purge still runs, leaving the index dark).
    /// Mirrors [`Db::index_meeting_chunks`]; uses the same `chunk_note` + `embed_passage` convention
    /// so doc vectors are comparable to note vectors in the shared embedding space.
    pub fn index_document_chunks(
        &self,
        document_id: &str,
        embedder: Option<&dyn Embedder>,
    ) -> Result<()> {
        let Some((_folder_id, name, text)) = self.get_document(document_id)? else {
            return Ok(()); // unknown document — nothing to index.
        };
        // Header carries the document name as provenance (the date axis is N/A for an upload, so the
        // header is just the name — `chunk_note` tolerates an empty date).
        let chunks = crate::embed::chunk_note(&name, "", &text);
        let vectors = match embedder {
            Some(e) if !chunks.is_empty() => e.embed_passage(&chunks)?,
            _ => Vec::new(), // model absent → chunk-only (FTS still covers it); vectors come later.
        };

        let this_doc = [document_id.to_string()];
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        Self::purge_doc_chunks_tx(&tx, &this_doc)?;
        {
            let mut ins_chunk = tx
                .prepare(
                    "INSERT INTO doc_chunks (document_id, chunk_index, text)
                     VALUES (?1, ?2, ?3)",
                )
                .map_err(map_err)?;
            let mut ins_vec = tx
                .prepare("INSERT INTO doc_vec_chunks(chunk_id, embedding) VALUES (?1, ?2)")
                .map_err(map_err)?;
            for (idx, text) in chunks.iter().enumerate() {
                ins_chunk
                    .execute(rusqlite::params![document_id, idx as i64, text])
                    .map_err(map_err)?;
                if let Some(vector) = vectors.get(idx) {
                    let chunk_id = tx.last_insert_rowid();
                    let blob = crate::embed::vec_to_blob(vector);
                    ins_vec
                        .execute(rusqlite::params![chunk_id, blob])
                        .map_err(map_err)?;
                }
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// (Re)index an authored NOTE's BODY into `doc_chunks` (+ FTS triggers) and `doc_vec_chunks`
    /// (only with a real `embedder`). Differs from [`Db::index_document_chunks`] ONLY in that the
    /// caller passes the pre-stripped BODY + the display TITLE explicitly (a note's `text` column
    /// carries YAML front-matter, which must NOT be embedded — DESIGN §1a) rather than re-reading
    /// the raw `text`. The title becomes the chunk header for provenance (the date axis is N/A →
    /// empty, like a document). Old chunks are purged first (clean replace) in the same tx.
    pub fn index_note_chunks(
        &self,
        note_id: &str,
        title: &str,
        body: &str,
        embedder: Option<&dyn Embedder>,
    ) -> Result<()> {
        let chunks = crate::embed::chunk_note(title, "", body);
        let vectors = match embedder {
            Some(e) if !chunks.is_empty() => e.embed_passage(&chunks)?,
            _ => Vec::new(), // model absent → chunk-only (FTS still covers it); vectors come later.
        };
        let this_doc = [note_id.to_string()];
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        Self::purge_doc_chunks_tx(&tx, &this_doc)?;
        {
            let mut ins_chunk = tx
                .prepare(
                    "INSERT INTO doc_chunks (document_id, chunk_index, text) VALUES (?1, ?2, ?3)",
                )
                .map_err(map_err)?;
            let mut ins_vec = tx
                .prepare("INSERT INTO doc_vec_chunks(chunk_id, embedding) VALUES (?1, ?2)")
                .map_err(map_err)?;
            for (idx, text) in chunks.iter().enumerate() {
                ins_chunk
                    .execute(rusqlite::params![note_id, idx as i64, text])
                    .map_err(map_err)?;
                if let Some(vector) = vectors.get(idx) {
                    let chunk_id = tx.last_insert_rowid();
                    let blob = crate::embed::vec_to_blob(vector);
                    ins_vec
                        .execute(rusqlite::params![chunk_id, blob])
                        .map_err(map_err)?;
                }
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Reparent a note-folder + rewrite the `path` of it and EVERY descendant (a prefix rewrite, so
    /// the subtree moves as a unit and `path` stays UNIQUE). Additive UPDATE only. `old_path` /
    /// `new_path` are the folder's vault-relative paths; `parent_id` is the new parent (NULL for a
    /// root note-folder). All in one tx.
    pub fn reparent_note_folder(
        &self,
        id: &str,
        old_path: &str,
        new_path: &str,
        parent_id: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        // Descendants first: rewrite every `<old_path>/…` prefix to `<new_path>/…`.
        let like = format!("{}/%", old_path.replace('!', "!!").replace('%', "!%").replace('_', "!_"));
        tx.execute(
            "UPDATE folders
                SET path = ?1 || substr(path, ?2)
              WHERE kind = 'note' AND path LIKE ?3 ESCAPE '!'",
            rusqlite::params![new_path, (old_path.len() + 1) as i64, like],
        )
        .map_err(map_err)?;
        // Then the folder itself: its own path + parent link.
        tx.execute(
            "UPDATE folders SET path = ?2, parent_id = ?3 WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id, new_path, parent_id],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Purge (delete) every `doc_chunks` + `doc_vec_chunks` row for the given documents. The vec0 row
    /// is deleted by its `chunk_id` (== doc_chunks.id) BEFORE the doc_chunks row. Used on lock (seal),
    /// on the index "clean replace", and on document delete. Mirrors [`Db::purge_chunks_for_meetings`].
    pub fn purge_doc_chunks_for_documents(&self, document_ids: &[String]) -> Result<()> {
        if document_ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        Self::purge_doc_chunks_tx(&tx, document_ids)?;
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Delete doc-chunk rows for `document_ids` within an EXISTING transaction (so the purge lands in
    /// the same atomic unit as the plaintext blanking on lock). vec0 first (its FK-less rowid mirrors
    /// doc_chunks.id), then the source rows. Mirrors [`Db::purge_chunks_tx`].
    fn purge_doc_chunks_tx(tx: &rusqlite::Transaction<'_>, document_ids: &[String]) -> Result<()> {
        for did in document_ids {
            tx.execute(
                "DELETE FROM doc_vec_chunks WHERE chunk_id IN
                   (SELECT id FROM doc_chunks WHERE document_id = ?1)",
                rusqlite::params![did],
            )
            .map_err(map_err)?;
            tx.execute(
                "DELETE FROM doc_chunks WHERE document_id = ?1",
                rusqlite::params![did],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    /// GATED semantic (vector KNN) search over DOCUMENT chunks. Runs a `doc_vec_chunks` KNN for the
    /// top-`k` nearest chunks, then applies EXACTLY the `visibility_clause` predicate (joined
    /// doc_chunks → documents → folders) so a chunk in a sealed-and-not-session-unlocked folder is
    /// EXCLUDED even if a stray chunk survived purge — the same defense-in-depth as
    /// `search_semantic_visible`. Dedups to one hit per document (best/nearest). Returns the chunk
    /// snippet + the document name + its folder id (NO meeting — documents are not meetings).
    pub fn search_doc_chunks_visible(
        &self,
        query_vec: &[f32],
        k: i64,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<DocChunkHit>> {
        if query_vec.is_empty() || k <= 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        // KNN isolated to the vec0 table in a CTE; visibility + document columns joined OUTSIDE it.
        let sql = format!(
            "WITH knn(chunk_id, distance) AS (
                 SELECT chunk_id, distance FROM doc_vec_chunks
                  WHERE embedding MATCH ?1 AND k = ?2
                  ORDER BY distance
             )
             SELECT d.id, d.name, d.folder_id, dc.text, knn.distance
               FROM knn
               JOIN doc_chunks dc ON dc.id = knn.chunk_id
               JOIN documents d ON d.id = dc.document_id
               JOIN folders f ON f.id = d.folder_id
              WHERE {visible}
              ORDER BY knn.distance ASC, d.id ASC"
        );
        let blob = crate::embed::vec_to_blob(query_vec);
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![blob, k], |row| {
                Ok(DocChunkHit {
                    document_id: row.get(0)?,
                    name: row.get(1)?,
                    folder_id: row.get(2)?,
                    snippet: row.get(3)?,
                })
            })
            .map_err(map_err)?;
        let mut seen: HashSet<String> = HashSet::new();
        let mut hits = Vec::new();
        for r in rows {
            let hit = r.map_err(map_err)?;
            if !seen.insert(hit.document_id.clone()) {
                continue; // already have a nearer chunk for this document.
            }
            hits.push(hit);
        }
        Ok(hits)
    }

    /// GATED keyword (FTS5/BM25) search over DOCUMENT chunks — the model-free twin of
    /// [`Db::search_doc_chunks_visible`], so documents/brain notes are reachable on a DEFAULT
    /// install (no e5 model, semantic flag off). Applies EXACTLY the `visibility_clause` predicate
    /// (joined doc_chunks → documents → folders) so a chunk in a sealed-and-not-session-unlocked
    /// folder is EXCLUDED even if a stray chunk survived purge — defense-in-depth on top of the
    /// trigger-purged FTS index. Dedups to one hit per document (best bm25), capped at `limit`.
    pub fn search_doc_chunks_fts_visible(
        &self,
        query: &str,
        limit: i64,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<DocChunkHit>> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let Some(match_expr) = fts_match_query(query.trim()) else {
            return Ok(Vec::new()); // punctuation-only / empty query → no hits, never an FTS error.
        };
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let sql = format!(
            "SELECT d.id, d.name, d.folder_id, dc.text, bm25(fts_doc_chunks) AS rank
               FROM fts_doc_chunks
               JOIN doc_chunks dc ON dc.id = fts_doc_chunks.rowid
               JOIN documents d ON d.id = dc.document_id
               JOIN folders f ON f.id = d.folder_id
              WHERE fts_doc_chunks MATCH ?1 AND {visible}
              ORDER BY rank ASC, d.id ASC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![match_expr], |row| {
                Ok(DocChunkHit {
                    document_id: row.get(0)?,
                    name: row.get(1)?,
                    folder_id: row.get(2)?,
                    snippet: row.get(3)?,
                })
            })
            .map_err(map_err)?;
        let mut seen: HashSet<String> = HashSet::new();
        let mut hits = Vec::new();
        for r in rows {
            let hit = r.map_err(map_err)?;
            if !seen.insert(hit.document_id.clone()) {
                continue; // already have a better-ranked chunk for this document.
            }
            hits.push(hit);
            if hits.len() as i64 >= limit {
                break;
            }
        }
        Ok(hits)
    }

    // ── NOTES (authored `documents(kind='note')`) ───────────────────────────────────────────────

    /// Insert an authored note row (`documents` with `kind='note'`). Separate from
    /// [`Db::insert_document`] only to persist the authoring columns (`title`/`updated_at`);
    /// `text_blob`/`exported_path` start NULL. The COMMAND layer gates the folder first.
    pub fn insert_note(
        &self,
        id: &str,
        folder_id: &str,
        name: &str,
        title: &str,
        text: &str,
        created_at: i64,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO documents
               (id, folder_id, name, title, text, kind, text_blob, created_at, updated_at, exported_path)
             VALUES (?1, ?2, ?3, ?4, ?5, 'note', NULL, ?6, ?6, NULL)",
            rusqlite::params![id, folder_id, name, title, text, created_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Read ONE note's raw row (every column the DTO builders need), or `None` if the id is unknown
    /// OR the row is not `kind='note'`. The COMMAND layer gates the folder before surfacing the text.
    pub fn get_note_row(&self, id: &str) -> Result<Option<NoteRow>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, folder_id, name, title, COALESCE(text, ''), created_at, updated_at,
                    exported_path, (text_blob IS NOT NULL)
               FROM documents WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id],
            row_to_note_row,
        )
        .optional()
        .map_err(map_err)
    }

    /// Update an authored note's `title` + `text` + `updated_at` (write path, OPEN folders only).
    /// Leaves `text_blob` alone. A write into a session-unlocked LOCKED folder must NOT come here —
    /// the relock reblank discards the plaintext and restores the stale blob (content loss) — it goes
    /// through the command layer's `reseal_document_if_locked` → [`Db::update_note_row_sealed`],
    /// which re-seals the fresh text into `text_blob` in the same write (2026-07-10 audit F1).
    /// Idempotent on an unknown id / non-note row (0 rows affected).
    pub fn update_note_row(&self, id: &str, title: &str, text: &str, updated_at: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE documents SET title = ?2, text = ?3, updated_at = ?4
               WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id, title, text, updated_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// SEAL-ON-WRITE twin of [`Db::update_note_row`] for a session-unlocked LOCKED folder: persist
    /// the fresh plaintext (session-visible) AND its freshly-encrypted `text_blob` in ONE atomic
    /// statement, so the at-rest seal always matches the newest text (relock re-blanks the plaintext
    /// and the next unlock restores THIS write, never a stale lock-time copy). The CALLER must have
    /// verified the blob decrypts back byte-identical BEFORE calling this (verify-before-destroy) —
    /// exactly like [`Db::seal_document`]. Idempotent on an unknown id / non-note row.
    pub fn update_note_row_sealed(
        &self,
        id: &str,
        title: &str,
        text: &str,
        text_blob: &[u8],
        updated_at: i64,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE documents SET title = ?2, text = ?3, text_blob = ?4, updated_at = ?5
               WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id, title, text, text_blob, updated_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Persist (or clear with `None`) an authored NOTE's exported vault `.md` path. Set on
    /// export/unlock re-export; cleared (NULL) when the folder seals and the vault file is deleted.
    /// Named `_doc` to disambiguate from the MEETING-note [`Db::set_note_exported_path`].
    pub fn set_note_doc_exported_path(&self, id: &str, path: Option<&str>) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE documents SET exported_path = ?2 WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id, path],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The vault `.md` paths of every authored NOTE in a folder that has one (`exported_path`
    /// non-NULL). Captured BEFORE seal so `lock_folder` can delete the on-disk `.md` (mirrors the
    /// meeting-notes `.md` deletion). No text — just paths (never PII-in-a-log; the caller doesn't
    /// log these).
    pub fn note_exported_paths_in_folder(&self, folder_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT exported_path FROM documents
                   WHERE folder_id = ?1 AND kind = 'note' AND exported_path IS NOT NULL",
            )
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

    /// NULL the `exported_path` of every authored NOTE in a folder (called on seal, after the vault
    /// `.md` files are deleted — a sealed note has no on-disk export). Re-set on unlock re-export.
    pub fn clear_note_exported_paths_in_folder(&self, folder_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE documents SET exported_path = NULL
               WHERE folder_id = ?1 AND kind = 'note'",
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Ids of every authored NOTE in a folder (its `documents(kind='note')` rows). Used to re-export
    /// each note's vault `.md` on unlock/remove-lock. Mirrors [`Db::document_ids_in_folder`] but
    /// scoped to notes.
    pub fn note_ids_in_folder(&self, folder_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id FROM documents WHERE folder_id = ?1 AND kind = 'note'")
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

    /// Reassign an authored NOTE to a different note-folder (the gate/seal anchor). The COMMAND
    /// layer gates both the source and target folder. Idempotent on an unknown id / non-note row.
    /// Named `_doc` to disambiguate from the MEETING-note [`Db::set_note_folder`].
    pub fn set_note_doc_folder(&self, id: &str, folder_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE documents SET folder_id = ?2 WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id, folder_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// GATED list of note summaries. `folder_id = Some(fid)` scopes to one note-folder; `None` lists
    /// every VISIBLE note across all note-folders. The `visibility_clause` is applied IN THE QUERY
    /// (never a per-row skip) so a sealed-and-not-session-unlocked note's row is EXCLUDED entirely —
    /// its title/topic never leaks. Newest-updated first. Snippet/tags are derived from the (visible)
    /// plaintext markdown; a locked note is simply absent here, so no masking is needed at this layer.
    pub fn list_notes_visible(
        &self,
        folder_id: Option<&str>,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<NoteSummary>> {
        // WP6 — the set of notes with an ACTIVE outbound share (one query; drives `shared`). Computed
        // BEFORE the main lock (it re-locks internally) so the connection guard is held only once.
        let shared_set = self.notes_with_active_share()?;
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let folder_pred = if folder_id.is_some() {
            " AND d.folder_id = ?1"
        } else {
            ""
        };
        let sql = format!(
            "SELECT d.id, d.folder_id, d.name, d.title, COALESCE(d.text, ''),
                    d.created_at, d.updated_at
               FROM documents d
               JOIN folders f ON f.id = d.folder_id
              WHERE d.kind = 'note' AND {visible}{folder_pred}
              ORDER BY COALESCE(d.updated_at, d.created_at) DESC, d.id ASC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let map_row = |r: &Row<'_>| -> rusqlite::Result<NoteSummary> {
            let id: String = r.get(0)?;
            let folder_id: String = r.get(1)?;
            let name: String = r.get(2)?;
            let title: Option<String> = r.get(3)?;
            let text: String = r.get(4)?;
            let created_at: i64 = r.get(5)?;
            let updated_at: Option<i64> = r.get(6)?;
            let (tags, _props) = parse_front_matter(&text);
            let shared = shared_set.contains(&id);
            Ok(NoteSummary {
                id,
                title: title.filter(|t| !t.is_empty()).unwrap_or(name),
                folder_id,
                snippet: note_snippet(&text),
                tags,
                updated_at: updated_at.unwrap_or(created_at),
                created_at,
                locked: false, // a visible note is unlocked by construction (gated in the query).
                shared,
            })
        };
        let rows = if let Some(fid) = folder_id {
            stmt.query_map(rusqlite::params![fid], map_row)
        } else {
            stmt.query_map([], map_row)
        }
        .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    // ── NOTE FOLDERS (`folders` with `kind='note'`) ──────────────────────────────────────────────

    /// Ensure the root note-folder exists (name "Notes", `kind='note'`, path "Notes") and return its
    /// id. Idempotent: on the SECOND+ call it finds the existing row by its unique `path` and returns
    /// that id (never a duplicate). Every note anchors on a note-folder (the gate anchor).
    pub fn ensure_default_note_folder(&self) -> Result<String> {
        if let Some(f) = self.folder_by_path("Notes")? {
            return Ok(f.id);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let conn = self.lock();
        // Guard against a race / a pre-existing "Notes" path created between the check and here:
        // INSERT OR IGNORE on the UNIQUE path, then read the id back (ours or the winner's).
        conn.execute(
            "INSERT OR IGNORE INTO folders (id, name, path, parent_id, locked, wrapped_key, created_at, kind)
             VALUES (?1, 'Notes', 'Notes', NULL, 0, NULL, ?2, 'note')",
            rusqlite::params![id, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(map_err)?;
        conn.query_row(
            "SELECT id FROM folders WHERE path = 'Notes'",
            [],
            |r| r.get::<_, String>(0),
        )
        .map_err(map_err)
    }

    /// Insert a note-folder (`kind='note'`). Mirrors [`Db::insert_folder`] but stamps the kind.
    pub fn insert_note_folder(&self, f: &NoteFolder, created_at: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO folders (id, name, path, parent_id, locked, wrapped_key, created_at, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, 'note')",
            rusqlite::params![
                f.id,
                f.name,
                f.path,
                f.parent_id,
                f.locked as i64,
                created_at,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// All note-folders (`kind='note'`), creation order. The Notes tree; the Meetings tree
    /// (`kind != 'note'`) is served by [`Db::list_folders`], which is unchanged.
    pub fn list_note_folders(&self) -> Result<Vec<NoteFolder>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, name, path, parent_id, locked, kind
                   FROM folders WHERE kind = 'note' ORDER BY created_at, name",
            )
            .map_err(map_err)?;
        let rows = stmt.query_map([], row_to_note_folder).map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// A note-folder by id (`kind='note'` enforced), or `None`. Used to gate note-folder ops so a
    /// meeting-folder id can't be driven through a note-folder command.
    pub fn note_folder_by_id(&self, id: &str) -> Result<Option<NoteFolder>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, name, path, parent_id, locked, kind
               FROM folders WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![id],
            row_to_note_folder,
        )
        .optional()
        .map_err(map_err)
    }

    /// The `kind` of a folder (or `None` if unknown). Lets the command layer reject cross-tree ops.
    pub fn folder_kind(&self, id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT COALESCE(kind, 'meeting') FROM folders WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(map_err)
    }

    /// Ids of every VISIBLE document (its folder open or session-unlocked), oldest-first. The
    /// reindex-backfill corpus: a sealed-and-not-unlocked folder's documents are NEVER returned, so
    /// their (blank) plaintext is never chunked and their index rows STAY purged.
    pub fn visible_document_ids(&self, unlocked: &HashSet<String>) -> Result<Vec<String>> {
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let sql = format!(
            "SELECT d.id FROM documents d
               JOIN folders f ON f.id = d.folder_id
              WHERE {visible}
              ORDER BY d.created_at ASC, d.id ASC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// True iff a document currently has `doc_chunks` rows. Lets the model-absent reindex backfill
    /// SKIP documents that are already chunked (a purge-then-reinsert without vectors would DESTROY
    /// their existing real vectors — chunk-only backfill is for write-only rows, never a downgrade).
    pub fn document_has_chunks(&self, document_id: &str) -> Result<bool> {
        let conn = self.lock();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM doc_chunks WHERE document_id = ?1",
                rusqlite::params![document_id],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        Ok(n > 0)
    }

    // ── segments ────────────────────────────────────────────────────────────

    pub fn insert_segments(&self, meeting_id: &str, segments: &[Segment]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT OR REPLACE INTO segments
                       (meeting_id, idx, start_s, end_s, text, speaker, confidence)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
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
                    seg.confidence,
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
                "SELECT idx, start_s, end_s, text, speaker, confidence
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
                    // NULL (legacy / Fast-path rows) → None; a stored REAL → Some(f32).
                    confidence: row.get(5)?,
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
               (meeting_id, provider_id, markdown, created_at, exported_path,
                model_requested, model_served, gateway_host)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(meeting_id, provider_id) DO UPDATE SET
               markdown = excluded.markdown,
               created_at = excluded.created_at,
               exported_path = excluded.exported_path,
               model_requested = excluded.model_requested,
               model_served = excluded.model_served,
               gateway_host = excluded.gateway_host",
            rusqlite::params![
                note.meeting_id,
                note.provider_id,
                note.markdown,
                note.created_at,
                note.exported_path,
                note.model_requested,
                note.model_served,
                note.gateway_host,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// SEAL-ON-WRITE twin of [`Db::upsert_note`] for a meeting whose folder is LOCKED (and
    /// session-unlocked — the command layer gates first): persist the fresh markdown (session-
    /// visible) AND its freshly-encrypted `content_blob` AND the governing `folder_id` in ONE atomic
    /// statement. Setting `folder_id` on insert keeps a NEW provider row governed by the meeting's
    /// lock (a bare insert would leave it NULL → ungoverned → visible). The CALLER must have verified
    /// the blob decrypts back byte-identical BEFORE calling this (verify-before-destroy) — exactly
    /// like [`Db::seal_note`]. 2026-07-10 audit F1.
    pub fn upsert_note_sealed(
        &self,
        note: &NoteRecord,
        content_blob: &[u8],
        folder_id: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO notes
               (meeting_id, provider_id, markdown, created_at, exported_path,
                model_requested, model_served, gateway_host, content_blob, folder_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(meeting_id, provider_id) DO UPDATE SET
               markdown = excluded.markdown,
               created_at = excluded.created_at,
               exported_path = excluded.exported_path,
               model_requested = excluded.model_requested,
               model_served = excluded.model_served,
               gateway_host = excluded.gateway_host,
               content_blob = excluded.content_blob,
               folder_id = excluded.folder_id",
            rusqlite::params![
                note.meeting_id,
                note.provider_id,
                note.markdown,
                note.created_at,
                note.exported_path,
                note.model_requested,
                note.model_served,
                note.gateway_host,
                content_blob,
                folder_id,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub fn get_note(&self, meeting_id: &str, provider_id: &str) -> Result<Option<NoteRecord>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT meeting_id, provider_id, markdown, created_at, exported_path,
                    model_requested, model_served, gateway_host
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
            "SELECT meeting_id, provider_id, markdown, created_at, exported_path,
                    model_requested, model_served, gateway_host
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
            "SELECT n.meeting_id, n.provider_id, n.markdown, n.created_at, n.exported_path,
                    n.model_requested, n.model_served, n.gateway_host
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
            "SELECT meeting_id, provider_id, markdown, created_at, exported_path,
                    model_requested, model_served, gateway_host
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
            .query_row(
                "SELECT COALESCE(SUM(duration_s), 0) FROM meetings",
                [],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        let longest_duration_s: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(duration_s), 0) FROM meetings",
                [],
                |r| r.get(0),
            )
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

    /// Does ANY sealed folder exist? Drives the master-KEK mint guard: while sealed content exists,
    /// a missing KEK keychain item must NEVER be silently replaced by a freshly-minted one (the
    /// fresh KEK cannot unwrap the existing folders' content keys — 2026-07-05 field incident).
    pub fn any_locked_folder(&self) -> Result<bool> {
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM folders WHERE locked = 1)",
            [],
            |r| r.get::<_, bool>(0),
        )
        .map_err(map_err)
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
        // Meetings that belong to THIS folder (folder_id lives on the notes row — the same join the
        // seal/relock paths use). Documents anchor on the folder row directly.
        const FOLDER_MEETINGS: &str = "SELECT DISTINCT meeting_id FROM notes WHERE folder_id = ?1";
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

        // Notes: blank plaintext + export path ONLY of rows that WERE sealed (blob present); a
        // never-sealed row (blob NULL) keeps its readable plaintext. Then drop the unrecoverable blob.
        tx.execute(
            "UPDATE notes SET markdown = '', exported_path = NULL WHERE folder_id = ?1 AND content_blob IS NOT NULL",
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
    /// Delete a folder row + its documents' derived index rows in ONE tx (2026-07-10 audit F3).
    /// The `documents` FK CASCADE reaches `doc_chunks`, but (a) `doc_vec_chunks` is a FK-less vec0
    /// table (its vectors would orphan) and (b) a cascade DELETE does not fire the
    /// `fts_doc_chunks_ad` trigger (`recursive_triggers` is unset) — leaving searchable FTS tokens
    /// of the deleted content behind. So the documents' chunk rows are purged EXPLICITLY (which
    /// fires the trigger and removes the vec0 rows), mirroring `delete_document`. Meetings are NOT
    /// deleted here — `delete_folder_inner` reassigns every note to the vault root first, so the
    /// meeting-side `vec_chunks` have no cascade hole on this path.
    pub fn delete_folder(&self, id: &str) -> Result<usize> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let document_ids: Vec<String> = {
            let mut stmt = tx
                .prepare("SELECT id FROM documents WHERE folder_id = ?1")
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![id], |r| r.get::<_, String>(0))
                .map_err(map_err)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(map_err)?);
            }
            out
        };
        Self::purge_doc_chunks_tx(&tx, &document_ids)?;
        // Explicit (rather than FK-cascade) so the delete is deterministic and trigger-visible.
        tx.execute(
            "DELETE FROM documents WHERE folder_id = ?1",
            rusqlite::params![id],
        )
        .map_err(map_err)?;
        let n = tx
            .execute("DELETE FROM folders WHERE id = ?1", rusqlite::params![id])
            .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
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
    pub fn seal_note(
        &self,
        meeting_id: &str,
        provider_id: &str,
        content_blob: &[u8],
    ) -> Result<()> {
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
                .prepare("SELECT DISTINCT meeting_id FROM notes WHERE folder_id = ?1")
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
            let mut document_ids: Vec<String> = Vec::new();
            for id in folder_ids {
                let rows = dids
                    .query_map(rusqlite::params![id], |r| r.get::<_, String>(0))
                    .map_err(map_err)?;
                for r in rows {
                    document_ids.push(r.map_err(map_err)?);
                }
            }
            drop(dids);
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
    /// The third tuple element (2026-07-10 audit F2) is the recorded `exported_path` of every
    /// authored NOTE in a locked folder: a session unlock re-exports each note's vault `.md`
    /// (`reexport_notes_in_folder`), so a crash-while-unlocked leaves that plaintext `.md` on disk.
    /// The caller ([`crate::state`] `reconcile_locked_at_rest`) deletes those files and THEN clears
    /// the column via [`Db::clear_note_exported_paths_in_locked_folders`] (delete-then-clear, so a
    /// crash between the two keeps the path recorded and the next startup retries). Mirrors the
    /// clean-relock cleanup in `reblank_folder_extras`.
    pub fn reblank_locked_folders_at_rest(
        &self,
    ) -> Result<(Vec<LockedMeetingAudio>, Vec<String>, Vec<String>)> {
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
        // 2026-07-10 audit F2: surface the authored notes' re-exported vault `.md` paths for every
        // LOCKED folder (a crash while session-unlocked leaves them plaintext on disk with the
        // column still set). Paths only — the caller deletes the files then clears the column
        // (`clear_note_exported_paths_in_locked_folders`), never text (no PII here).
        let note_md_exports = {
            let mut stmt = tx
                .prepare(
                    "SELECT exported_path FROM documents \
                       WHERE kind = 'note' AND exported_path IS NOT NULL \
                         AND folder_id IN (SELECT id FROM folders WHERE locked = 1)",
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
        tx.commit().map_err(map_err)?;
        Ok((audio, rollup_exports, note_md_exports))
    }

    /// NULL the `exported_path` of every authored NOTE in EVERY locked folder — the startup-
    /// reconcile companion of [`Db::clear_note_exported_paths_in_folder`], run AFTER the caller has
    /// deleted the on-disk `.md` files surfaced by [`Db::reblank_locked_folders_at_rest`]
    /// (delete-then-clear; 2026-07-10 audit F2).
    pub fn clear_note_exported_paths_in_locked_folders(&self) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE documents SET exported_path = NULL \
               WHERE kind = 'note' \
                 AND folder_id IN (SELECT id FROM folders WHERE locked = 1)",
            [],
        )
        .map_err(map_err)?;
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

    /// Meetings whose audio may be auto-pruned: every meeting NOT in a locked folder
    /// (a meeting's folder = its `notes.folder_id`), OLDEST FIRST. A locked folder's audio
    /// is exempt — it is the sealed `.enc` at rest and must never be deleted by prune.
    pub fn prunable_audio_candidates(&self) -> Result<Vec<PrunableAudio>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, started_at, audio_path, mic_master_path, sys_master_path \
                   FROM meetings \
                  WHERE id NOT IN ( \
                      SELECT DISTINCT meeting_id FROM notes \
                       WHERE folder_id IN (SELECT id FROM folders WHERE locked = 1) \
                  ) \
                  ORDER BY started_at ASC, id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(PrunableAudio {
                    meeting_id: r.get(0)?,
                    started_at: r.get(1)?,
                    audio_path: r.get(2)?,
                    mic_master_path: r.get(3)?,
                    sys_master_path: r.get(4)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
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
        self.search_visible_impl(query, limit, unlocked, None)
    }

    /// Brain v2 L1.5 — [`Self::search_visible`] with an optional `started_at` window
    /// (`(from_iso, to_iso_exclusive)`, from `summarize::temporal`). TEMPORAL FALLBACK: when a
    /// window is present and the lexical FTS match finds nothing inside it (the common shape of a
    /// pure "what did we discuss last week?" query — no content token survives the implicit-AND
    /// match), the window ITSELF becomes the query: the visible meetings in range are returned
    /// newest-first, `matched_in: "temporal"`. Same visibility gate on both paths.
    pub fn search_visible_in_range(
        &self,
        query: &str,
        limit: i64,
        unlocked: &HashSet<String>,
        date_filter: Option<(String, String)>,
    ) -> Result<Vec<SearchHit>> {
        let hits = self.search_visible_impl(query, limit, unlocked, date_filter.as_ref())?;
        if !hits.is_empty() || date_filter.is_none() {
            return Ok(hits);
        }
        let range = date_filter.as_ref();
        let meetings = self.meetings_in_range_visible(range, limit, unlocked)?;
        Ok(meetings
            .into_iter()
            .map(|m| {
                let snippet = m.title.clone().unwrap_or_default();
                SearchHit {
                    meeting: m,
                    snippet,
                    matched_in: "temporal".to_string(),
                }
            })
            .collect())
    }

    fn search_visible_impl(
        &self,
        query: &str,
        limit: i64,
        unlocked: &HashSet<String>,
        date_filter: Option<&(String, String)>,
    ) -> Result<Vec<SearchHit>> {
        let q = query.trim();
        let Some(match_expr) = fts_match_query(q) else {
            return Ok(Vec::new());
        };
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let date = date_clause(date_filter);
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
              WHERE (NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id)
                 OR EXISTS (
                      SELECT 1 FROM notes n
                       LEFT JOIN folders f ON f.id = n.folder_id
                       WHERE n.meeting_id = m.id AND {visible}
                    )){date}
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
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT m.id, m.started_at, m.ended_at, m.title, m.duration_s, m.audio_path, m.status,
                    (SELECT nf.folder_id FROM notes nf
                      WHERE nf.meeting_id = m.id AND nf.folder_id IS NOT NULL LIMIT 1)
                      AS folder_id
               FROM meetings m
              WHERE m.title = ?1
                AND (NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id)
                     OR EXISTS (
                          SELECT 1 FROM notes n
                           LEFT JOIN folders f ON f.id = n.folder_id
                           WHERE n.meeting_id = m.id AND {visible}
                        ))
              ORDER BY m.started_at DESC, m.id DESC
              LIMIT 1"
        );
        conn.query_row(&sql, rusqlite::params![title], row_to_meeting)
            .optional()
            .map_err(map_err)?
            .transpose()
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
            "SELECT n.meeting_id, n.provider_id, n.markdown, n.created_at, n.exported_path,
                    n.model_requested, n.model_served, n.gateway_host
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
    pub fn meeting_is_visible(&self, meeting_id: &str, unlocked: &HashSet<String>) -> Result<bool> {
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

    /// Phase E (Flow B, `NoteAside`) — record a spoken aside against a meeting in the additive
    /// `notes_asides` store. PURELY ADDITIVE: it never touches the note `markdown`/`content_blob`,
    /// so it can never blank or clobber sealed content. The CALLER gates on the live unlocked set
    /// (`meeting_is_visible`) before calling, so an aside is only ever recorded for a meeting the
    /// session can see — the in-progress recording is foldered/sealed only later, so the live
    /// meeting is trivially visible. `text` is stored verbatim (it is the user's own dictated note).
    pub fn insert_note_aside(&self, meeting_id: &str, text: &str, created_at: &str) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO notes_asides (meeting_id, text, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![meeting_id, text, created_at],
        )
        .map_err(map_err)?;
        Ok(conn.last_insert_rowid())
    }

    /// Read every aside recorded for `meeting_id`, oldest first. Used by the detail view / note
    /// finalization to surface the asides captured live. Returns `(text, created_at)` tuples.
    pub fn list_note_asides(&self, meeting_id: &str) -> Result<Vec<(String, String)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT text, created_at FROM notes_asides
                  WHERE meeting_id = ?1 ORDER BY id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![meeting_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

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

    /// LOCK-SAFETY: delete every `assistant_interactions` row for `meeting_ids` within an EXISTING
    /// transaction, so the purge lands in the SAME atomic unit as the plaintext blanking on a seal
    /// (and on the startup reconcile). The Q&A log is plaintext-derived convenience data that mirrors
    /// content of a sealed meeting (the user's spoken question + the answer grounded on the vault); a
    /// sealed meeting must surface NOTHING, so — exactly like `correction_log` / `note_chunks` — we
    /// DELETE rather than seal. This is INTENTIONAL: the Q&A log is dropped on seal by design and is
    /// not recoverable (it was never keyed); the underlying transcript is still sealed + restorable.
    fn purge_assistant_interactions_tx(
        tx: &rusqlite::Transaction<'_>,
        meeting_ids: &[String],
    ) -> Result<()> {
        for mid in meeting_ids {
            tx.execute(
                "DELETE FROM assistant_interactions WHERE meeting_id = ?1",
                rusqlite::params![mid],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    // ── Brain v2 L4 — the `live_bullets` crash-recovery row (transcribe::bullets) ───────────────

    /// Upsert the running live bullets for the recording in progress (crash recovery for the
    /// Stop-time note input). Written by the reactions worker only while the meeting records;
    /// consumed + cleared by the note pipeline at Stop.
    ///
    /// TOCTOU LOCK-SAFETY (lock-security W2, 2026-07-10 — the same shape as the in-tx re-check in
    /// `index_meeting_topic_chunks`): a `lock_folder` can commit BETWEEN the worker's
    /// `current_meeting` check and this write — its seal tx purged the row, and a plaintext
    /// re-upsert would leave sealed-meeting running notes at rest until the next relock /
    /// Stop-consume / startup reconcile. So the write re-checks the SESSION-INDEPENDENT DB-side
    /// sealed-at-rest invariant (a `notes` row with `content_blob` present and blank `markdown`)
    /// inside its own transaction and REFUSES silently (`Ok(())` — the worker is best-effort; the
    /// RAM copy is cleared by the lock surface anyway).
    pub fn upsert_live_bullets(
        &self,
        meeting_id: &str,
        bullets_md: &str,
        updated_at: &str,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let sealed_at_rest: bool = tx
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM notes
                    WHERE meeting_id = ?1
                      AND content_blob IS NOT NULL
                      AND (markdown IS NULL OR markdown = '')
                 )",
                rusqlite::params![meeting_id],
                |r| Ok(r.get::<_, i64>(0)? != 0),
            )
            .map_err(map_err)?;
        if sealed_at_rest {
            // Rollback via drop — nothing written. IDs only in the log (no bullet text — PII rule).
            tracing::debug!(target: "bullets", meeting_id, "live-bullets upsert refused: meeting is sealed at rest");
            return Ok(());
        }
        tx.execute(
            "INSERT INTO live_bullets (meeting_id, bullets_md, updated_at)
                  VALUES (?1, ?2, ?3)
             ON CONFLICT(meeting_id) DO UPDATE
                    SET bullets_md = excluded.bullets_md, updated_at = excluded.updated_at",
            rusqlite::params![meeting_id, bullets_md, updated_at],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// The stored live bullets for `meeting_id`, or `None`. INTERNAL PRODUCER READ ONLY — the note
    /// pipeline consumes it while producing the meeting's own note plaintext (the same
    /// ungated-by-design classification as `get_manual_notes` there); it must NEVER back an FE
    /// command without a `meeting_is_unlocked` gate. A sealed meeting has no row anyway
    /// (purge-on-seal — `purge_live_bullets_tx`), so this is defense-in-depth layering, not the
    /// gate itself.
    pub fn get_live_bullets(&self, meeting_id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT bullets_md FROM live_bullets WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(map_err)
    }

    /// Drop the live-bullets row for `meeting_id` (the Stop-time consume). Idempotent.
    pub fn clear_live_bullets(&self, meeting_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM live_bullets WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// LOCK-SAFETY (the L2 lesson): delete every `live_bullets` row for `meeting_ids` within an
    /// EXISTING transaction, so the purge lands in the SAME atomic unit as the plaintext blanking
    /// on a seal (and on `delete_meeting` / the startup reconcile). Live bullets are
    /// plaintext-DERIVED running notes mirroring the meeting's transcript; a sealed meeting must
    /// surface NOTHING, so — exactly like `assistant_interactions` / `facts` / `note_chunks` — we
    /// DELETE rather than key-seal. Dropped by design + not recoverable (never keyed); the
    /// underlying transcript is still sealed + restorable.
    fn purge_live_bullets_tx(tx: &rusqlite::Transaction<'_>, meeting_ids: &[String]) -> Result<()> {
        for mid in meeting_ids {
            tx.execute(
                "DELETE FROM live_bullets WHERE meeting_id = ?1",
                rusqlite::params![mid],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    /// OPEN-COMMITMENTS rollup (deterministic, no model): every OPEN (`- [ ]`) action item across
    /// the VISIBLE meetings, with its meeting context. DOUBLE-GATED — the meeting list is filtered
    /// by `list_meetings_visible(unlocked)` and each note is re-fetched through
    /// `get_note_if_visible(unlocked)`, so a sealed-and-not-session-unlocked meeting contributes
    /// NOTHING (its note markdown is never read here). Checked-off (`- [x]`) items are dropped.
    /// `owner`, when Some, filters case-insensitively. Sorted by due date (None last), then recency.
    pub fn list_open_commitments(
        &self,
        unlocked: &HashSet<String>,
        owner: Option<&str>,
    ) -> Result<Vec<Commitment>> {
        let owner_lc = owner
            .map(|o| o.trim().to_lowercase())
            .filter(|o| !o.is_empty());
        let mut out: Vec<Commitment> = Vec::new();
        // GATE 1: only VISIBLE meetings. GATE 2: only the VISIBLE note (None for sealed-not-unlocked).
        for m in self.list_meetings_visible(1000, unlocked)? {
            let Some(note) = self.get_note_if_visible(&m.id, unlocked)? else {
                continue;
            };
            let title = m.title.clone().unwrap_or_else(|| "(untitled)".to_string());
            for item in crate::summarize::action_items::parse_action_items(&note.markdown) {
                if item.done {
                    continue; // `- [x]` — already done, not an open commitment.
                }
                if let Some(want) = owner_lc.as_deref() {
                    match item.owner.as_deref() {
                        Some(o) if o.trim().to_lowercase() == want => {}
                        _ => continue,
                    }
                }
                out.push(Commitment {
                    meeting_id: m.id.clone(),
                    meeting_title: title.clone(),
                    started_at: m.started_at.clone(),
                    owner: item.owner,
                    due_date: item.due_date,
                    text: item.text,
                });
            }
        }
        // Due date ascending (soonest first), items with no date last; ties broken by recency.
        out.sort_by(
            |a, b| match (a.due_date.as_deref(), b.due_date.as_deref()) {
                (Some(x), Some(y)) => x.cmp(y).then_with(|| b.started_at.cmp(&a.started_at)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => b.started_at.cmp(&a.started_at),
            },
        );
        Ok(out)
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
        // F5: bound the quadratic co-occurrence self-join — return only the strongest edges. The
        // graph UI (brain-map, ≤60 nodes) consumes only the heaviest connections, so an unbounded
        // ORDER BY over every co-mentioned entity pair is wasted serialization on a large vault.
        // Visibility is already enforced in WHERE; a trailing LIMIT trims magnitude only — it can
        // never widen what is visible.
        const MAX_GRAPH_EDGES: usize = 600;
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
              ORDER BY weight DESC
              LIMIT {MAX_GRAPH_EDGES}"
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
    pub fn entity_is_visible(&self, entity_id: &str, unlocked: &HashSet<String>) -> Result<bool> {
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

    /// QUERY→ENTITY resolution for GraphRAG-lite (Phase 2d) — DETERMINISTIC, no LLM. Returns the
    /// ids of VISIBLE entities whose name appears as a whole-token match (see
    /// [`name_matches_query_tokens`]) inside `query`, case-insensitively. Gated by EXACTLY the
    /// `list_entities_visible` predicate: an entity mentioned ONLY in a sealed-and-not-unlocked
    /// folder is never resolved (its name lived only in encrypted markdown, so resolving it would
    /// leak its existence). A name shorter than [`MIN_ENTITY_NAME_LEN`] chars is skipped (noise
    /// guard). Empty query or no match → empty vec, leaving the hybrid path unchanged.
    pub fn entities_matching_query(
        &self,
        query: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<String>> {
        let q_tokens = tokenize_lower(query);
        if q_tokens.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        // Same visibility predicate as `list_entities_visible`: keep an entity iff it has ≥1
        // mention in a meeting that is open/NULL-folder OR session-unlocked (or note-less).
        let sql = format!(
            "SELECT DISTINCT e.id, e.name_ci
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
                    )"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                let id: String = r.get(0)?;
                let name_ci: String = r.get(1)?;
                Ok((id, name_ci))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            let (id, name_ci) = r.map_err(map_err)?;
            if name_ci.chars().count() >= MIN_ENTITY_NAME_LEN
                && name_matches_query_tokens(&q_tokens, &name_ci)
            {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// GATED entity-neighbour candidates for GraphRAG-lite (Phase 2d): the VISIBLE meetings
    /// mentioning ANY of `entity_ids`, ranked by how many of the matched entities they touch (desc)
    /// then recency. Uses EXACTLY the `list_meetings_visible`/graph visibility predicate, so a
    /// sealed-and-not-unlocked meeting NEVER appears even if it mentions a matched entity. Empty
    /// input → empty vec.
    pub fn meetings_mentioning_entities_visible(
        &self,
        entity_ids: &[String],
        unlocked: &HashSet<String>,
    ) -> Result<Vec<Meeting>> {
        if entity_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let placeholders = (1..=entity_ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT m.id, m.started_at, m.ended_at, m.title, m.duration_s, m.audio_path, m.status, \
                    (SELECT nf.folder_id FROM notes nf \
                      WHERE nf.meeting_id = m.id AND nf.folder_id IS NOT NULL LIMIT 1) AS folder_id \
               FROM entity_mentions em \
               JOIN meetings m ON m.id = em.meeting_id \
              WHERE em.entity_id IN ({placeholders}) \
                AND ( \
                      NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id) \
                   OR EXISTS ( \
                        SELECT 1 FROM notes n \
                         LEFT JOIN folders f ON f.id = n.folder_id \
                         WHERE n.meeting_id = m.id AND {visible} \
                      ) \
                    ) \
              GROUP BY m.id \
              ORDER BY COUNT(DISTINCT em.entity_id) DESC, m.started_at DESC, m.id DESC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let params = rusqlite::params_from_iter(entity_ids.iter());
        let rows = stmt.query_map(params, row_to_meeting).map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)??);
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
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT ft.id, ft.entity_id, ft.subject, ft.predicate, ft.object, ft.valid_from, \
                    ft.valid_to, ft.recorded_at, ft.meeting_id, ft.confidence \
               FROM facts ft \
               JOIN meetings m ON m.id = ft.meeting_id \
              WHERE ft.entity_id = ?1 \
                AND ( \
                      NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id) \
                   OR EXISTS ( \
                        SELECT 1 FROM notes n \
                         LEFT JOIN folders f ON f.id = n.folder_id \
                         WHERE n.meeting_id = m.id AND {visible} \
                      ) \
                    ) \
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

    /// GATED `/people` personal-CRM rollup: one [`PersonCard`] per VISIBLE Person entity, built
    /// ENTIRELY from the existing gated graph/facts/commitment readers — NO new or ungated query.
    ///
    /// LOCK MODEL: the candidate set is `list_entities_visible` filtered to `EntityKind::Person`,
    /// so a person mentioned ONLY in sealed-and-not-session-unlocked meetings is already dropped
    /// (its `HAVING cnt > 0` count is 0 while sealed) and never surfaces here. Each per-person count
    /// then reuses a gate that pushes the SAME `unlocked` set through `visibility_clause`:
    /// `entity_mentions_visible` (meeting_count + last_talked), `list_facts_visible` (current facts),
    /// and `list_open_commitments` (owner-scoped, name match) — so every count reflects VISIBLE
    /// sources only; a sealed source contributes nothing to any of them. Ordered by most-recent
    /// contact (last_talked DESC), then name.
    pub fn list_people(&self, unlocked: &HashSet<String>) -> Result<Vec<PersonCard>> {
        // GATE: the visible-only entity set, Persons only. A sealed-only person is absent here.
        let people: Vec<GraphNode> = self
            .list_entities_visible(unlocked)?
            .into_iter()
            .filter(|n| n.kind == EntityKind::Person)
            .collect();
        // Owner-scoped commitments are cheap to compute ONCE (the full visible rollup) and bucket
        // by lowercased owner name, rather than re-scanning every note per person.
        let mut commitments_by_owner: std::collections::HashMap<String, i64> =
            std::collections::HashMap::new();
        for c in self.list_open_commitments(unlocked, None)? {
            if let Some(owner) = c.owner.as_deref() {
                let key = owner.trim().to_lowercase();
                if !key.is_empty() {
                    *commitments_by_owner.entry(key).or_insert(0) += 1;
                }
            }
        }
        let mut out: Vec<PersonCard> = Vec::with_capacity(people.len());
        for p in people {
            // meeting_count + last_talked: VISIBLE mentions only, newest first.
            let mentions = self.entity_mentions_visible(&p.id, unlocked)?;
            let meeting_count = mentions.len() as i64;
            let last_talked = mentions.first().map(|m| m.started_at.clone());
            // current_fact_count: currently-valid facts about this person from VISIBLE meetings.
            let current_fact_count = self.list_facts_visible(&p.id, unlocked)?.len() as i64;
            // open_commitment_count: open action items owned by this person (case-insensitive name).
            let open_commitment_count = commitments_by_owner
                .get(&p.name.trim().to_lowercase())
                .copied()
                .unwrap_or(0);
            out.push(PersonCard {
                id: p.id,
                name: p.name,
                meeting_count,
                last_talked,
                open_commitment_count,
                current_fact_count,
            });
        }
        // Most-recent contact first (None last), ties broken by name.
        out.sort_by(
            |a, b| match (a.last_talked.as_deref(), b.last_talked.as_deref()) {
                (Some(x), Some(y)) => y
                    .cmp(x)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            },
        );
        Ok(out)
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
                    tx.execute(
                        "UPDATE user_facts SET valid_to = ?2 WHERE id = ?1 AND valid_to IS NULL",
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
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT uf.id, uf.subject, uf.predicate, uf.object, uf.valid_from, \
                    uf.valid_to, uf.recorded_at, uf.meeting_id, uf.confidence \
               FROM user_facts uf \
               JOIN meetings m ON m.id = uf.meeting_id \
              WHERE uf.valid_to IS NULL \
                AND ( \
                      NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id) \
                   OR EXISTS ( \
                        SELECT 1 FROM notes n \
                         LEFT JOIN folders f ON f.id = n.folder_id \
                         WHERE n.meeting_id = m.id AND {visible} \
                      ) \
                    ) \
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
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT uf.id, uf.subject, uf.predicate, uf.object, uf.valid_from, \
                    uf.valid_to, uf.recorded_at, uf.meeting_id, uf.confidence \
               FROM fts_user_facts \
               JOIN user_facts uf ON uf.rowid = fts_user_facts.rowid \
               JOIN meetings m ON m.id = uf.meeting_id \
              WHERE fts_user_facts MATCH ?1 \
                AND uf.valid_to IS NULL \
                AND ( \
                      NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id) \
                   OR EXISTS ( \
                        SELECT 1 FROM notes n \
                         LEFT JOIN folders f ON f.id = n.folder_id \
                         WHERE n.meeting_id = m.id AND {visible} \
                      ) \
                    ) \
              ORDER BY bm25(fts_user_facts) ASC, uf.valid_from DESC, uf.id DESC \
              LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(
                rusqlite::params![match_expr, k as i64],
                row_to_user_fact,
            )
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    // ── Brain v2 L2.1 — memory consolidation store (see `crate::memory`) ─────────────────────────
    //
    // `memory_scores` rows are CONTENT-FREE (ids + floats) and cascade off `user_facts` (FK), so
    // purge-on-seal / delete-meeting are transitive. `memory_rollups` carry SYNTHESIS text derived
    // ONLY from VISIBLE facts (the job reads through the gated `list_user_facts_visible` /
    // `list_facts_visible` with the empty unlock set); they are ALL PURGED inside every seal tx
    // (`purge_memory_rollups_tx` — the caller deletes the exported `.md`s) AND hash-tracked
    // (`fact_set_hash`) so the hourly pass re-reflects/GCs any rollup whose visible fact set changed.

    /// Upsert ONE memory score row (idempotent per `fact_id` — the job re-scores every pass).
    #[allow(clippy::too_many_arguments)] // a flat score row: id + scope + 4 floats + instant.
    pub fn upsert_memory_score(
        &self,
        fact_id: &str,
        scope: &str,
        recency: f64,
        importance: f64,
        relevance: f64,
        composite: f64,
        scored_at: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO memory_scores \
               (fact_id, scope, recency, importance, relevance, composite, scored_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(fact_id) DO UPDATE SET \
               scope = excluded.scope, recency = excluded.recency, \
               importance = excluded.importance, relevance = excluded.relevance, \
               composite = excluded.composite, scored_at = excluded.scored_at",
            rusqlite::params![fact_id, scope, recency, importance, relevance, composite, scored_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// All persisted memory scores as `(fact_id, importance)` — the job's "already assessed" set,
    /// so the light-reasoner importance call runs ONLY for never-scored facts (steady-state passes
    /// are LLM-free). Content-free read.
    pub fn memory_importance_map(&self) -> Result<std::collections::HashMap<String, f64>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT fact_id, importance FROM memory_scores")
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

    /// Drop score rows whose fact is CLOSED (`valid_to` set — forgotten/superseded). Purged/deleted
    /// facts cascade off the FK; closed facts are UPDATEs, so the job sweeps their scores here.
    pub fn delete_memory_scores_for_closed_facts(&self) -> Result<usize> {
        let conn = self.lock();
        let n = conn
            .execute(
                "DELETE FROM memory_scores WHERE fact_id IN \
                   (SELECT id FROM user_facts WHERE valid_to IS NOT NULL)",
                [],
            )
            .map_err(map_err)?;
        Ok(n)
    }

    /// Upsert ONE rollup by `scope` (idempotent — a re-reflection replaces the content, keeps the
    /// row id + `created_at`, bumps `updated_at`, and resets `exported_path` until re-exported).
    /// `fact_set_hash` is the deterministic hash of the SORTED visible-open-fact id set the content
    /// was synthesized from (`crate::memory::fact_set_hash`) — the hourly pass compares it to decide
    /// re-reflection.
    pub fn upsert_memory_rollup(
        &self,
        scope: &str,
        content: &str,
        fact_set_hash: &str,
        now: &str,
    ) -> Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        let conn = self.lock();
        conn.execute(
            "INSERT INTO memory_rollups \
               (id, scope, content, created_at, updated_at, exported_path, fact_set_hash) \
             VALUES (?1, ?2, ?3, ?4, ?4, NULL, ?5) \
             ON CONFLICT(scope) DO UPDATE SET \
               content = excluded.content, updated_at = excluded.updated_at, \
               exported_path = NULL, fact_set_hash = excluded.fact_set_hash",
            rusqlite::params![id, scope, content, now, fact_set_hash],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Delete ONE rollup by `scope` (the hourly pass's GC of a no-longer-eligible scope), returning
    /// its recorded `exported_path` so the caller can remove the exported vault `.md` (only ever
    /// that recorded path). `Ok(None)` when the scope has no row or was never exported.
    pub fn delete_memory_rollup(&self, scope: &str) -> Result<Option<String>> {
        let conn = self.lock();
        let path: Option<Option<String>> = conn
            .query_row(
                "SELECT exported_path FROM memory_rollups WHERE scope = ?1",
                rusqlite::params![scope],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_err)?;
        conn.execute(
            "DELETE FROM memory_rollups WHERE scope = ?1",
            rusqlite::params![scope],
        )
        .map_err(map_err)?;
        Ok(path.flatten())
    }

    /// The persisted `facts.importance` assessments as `(fact_id, importance)` — the reflection
    /// job's "already assessed" set for ENTITY facts (only never-assessed facts hit the reasoner,
    /// so steady-state passes are LLM-free). Content-free read (ids + floats).
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

    /// Stamp the vault path a rollup was exported to (after the atomic `.md` write succeeded).
    pub fn set_memory_rollup_exported(&self, scope: &str, path: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE memory_rollups SET exported_path = ?2 WHERE scope = ?1",
            rusqlite::params![scope, path],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// All rollups, stable scope order (the export pass + tests).
    pub fn list_memory_rollups(&self) -> Result<Vec<crate::storage::models::MemoryRollup>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, scope, content, created_at, updated_at, exported_path, fact_set_hash \
                   FROM memory_rollups ORDER BY scope ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(crate::storage::models::MemoryRollup {
                    id: r.get(0)?,
                    scope: r.get(1)?,
                    content: r.get(2)?,
                    created_at: r.get(3)?,
                    updated_at: r.get(4)?,
                    exported_path: r.get(5)?,
                    fact_set_hash: r.get(6)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// All memory scores (tests + diagnostics). Content-free rows.
    pub fn list_memory_scores(&self) -> Result<Vec<crate::storage::models::MemoryScore>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT fact_id, scope, recency, importance, relevance, composite, scored_at \
                   FROM memory_scores ORDER BY fact_id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(crate::storage::models::MemoryScore {
                    fact_id: r.get(0)?,
                    scope: r.get(1)?,
                    recency: r.get(2)?,
                    importance: r.get(3)?,
                    relevance: r.get(4)?,
                    composite: r.get(5)?,
                    scored_at: r.get(6)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    // ── Brain v2 L5 — scheduled briefs (config + propose-accept staging) ────────────────────────

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

    // ── Brain v2 L5 — MCP server config rows ────────────────────────────────────────────────────

    /// All configured MCP servers (connection config only — never query text or results).
    pub fn list_mcp_servers(&self) -> Result<Vec<crate::storage::models::McpServer>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, label, transport, endpoint, args, enabled, consented, created_at \
                   FROM mcp_servers ORDER BY created_at ASC, id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt.query_map([], row_to_mcp_server).map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// One MCP server by id.
    pub fn get_mcp_server(&self, id: &str) -> Result<Option<crate::storage::models::McpServer>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, label, transport, endpoint, args, enabled, consented, created_at \
               FROM mcp_servers WHERE id = ?1",
            [id],
            row_to_mcp_server,
        )
        .optional()
        .map_err(map_err)
    }

    /// Insert one MCP server row (caller validates transport/endpoint — see `add_mcp_server`).
    pub fn insert_mcp_server(&self, s: &crate::storage::models::McpServer) -> Result<()> {
        let args = serde_json::to_string(&s.args)
            .map_err(|e| AppError::Storage(format!("mcp args serialize: {e}")))?;
        let conn = self.lock();
        conn.execute(
            "INSERT INTO mcp_servers (id, label, transport, endpoint, args, enabled, consented, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                s.id,
                s.label,
                s.transport,
                s.endpoint,
                args,
                s.enabled as i64,
                s.consented as i64,
                s.created_at,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Remove one MCP server row (revokes its tool exposure on the next registry/spec build).
    pub fn delete_mcp_server(&self, id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM mcp_servers WHERE id = ?1", [id])
            .map_err(map_err)?;
        Ok(())
    }

    /// Flip one MCP server's per-server egress consent (the ONLY writer of `consented`).
    pub fn set_mcp_server_consented(&self, id: &str, consented: bool) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE mcp_servers SET consented = ?2 WHERE id = ?1",
            rusqlite::params![id, consented as i64],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Persist ONE voiceprint (opt-in voice biometric) for a diarized cluster of `meeting_id`.
    /// `embedding` is the L2-normalized CAM++ vector; it is stored as a little-endian f32 BLOB.
    /// `label` is NULL initially (bound later on enroll-by-rename). NO PII is logged (the embedding,
    /// label, and meeting id are never logged). The row is provenance-anchored to `meeting_id`, so it
    /// is gated by `list_voiceprints_visible` and purged on seal / cascade-deleted with the meeting.
    pub fn insert_voiceprint(
        &self,
        id: &str,
        meeting_id: &str,
        cluster_index: i64,
        label: Option<&str>,
        embedding: &[f32],
        created_at: &str,
    ) -> Result<()> {
        let blob = crate::transcribe::diarize::embedding_to_blob(embedding);
        let conn = self.lock();
        conn.execute(
            "INSERT INTO speaker_voiceprints \
               (id, meeting_id, cluster_index, label, dim, embedding, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                id,
                meeting_id,
                cluster_index,
                label,
                embedding.len() as i64,
                blob,
                created_at,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// GATED read of stored voiceprints: only those whose source meeting is VISIBLE (its note-folder
    /// is open or session-`unlocked`). A voiceprint derived from a sealed-and-not-unlocked meeting is
    /// EXCLUDED — it must never be read or matched (a voice biometric of a locked speaker stays
    /// invisible). Fail-closed: an INNER JOIN on `meetings` drops a NULL/orphaned meeting_id, and the
    /// visibility clause drops a sealed folder. Mirrors `list_user_facts_visible` exactly. A blob that
    /// fails to decode is skipped defensively (never surfaced malformed).
    pub fn list_voiceprints_visible(&self, unlocked: &HashSet<String>) -> Result<Vec<Voiceprint>> {
        let conn = self.lock();
        let visible = visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT vp.id, vp.meeting_id, vp.cluster_index, vp.label, vp.dim, vp.embedding, \
                    vp.created_at \
               FROM speaker_voiceprints vp \
               JOIN meetings m ON m.id = vp.meeting_id \
              WHERE ( \
                      NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id) \
                   OR EXISTS ( \
                        SELECT 1 FROM notes n \
                         LEFT JOIN folders f ON f.id = n.folder_id \
                         WHERE n.meeting_id = m.id AND {visible} \
                      ) \
                    ) \
              ORDER BY vp.created_at DESC, vp.id DESC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], |row| {
                let blob: Vec<u8> = row.get(5)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    blob,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            let (id, meeting_id, cluster_index, label, dim, blob, created_at) =
                r.map_err(map_err)?;
            // Defensive: a malformed blob (not a multiple of 4) is skipped, never surfaced.
            let embedding = match crate::transcribe::diarize::blob_to_embedding(&blob) {
                Some(e) => e,
                None => continue,
            };
            out.push(Voiceprint {
                id,
                meeting_id,
                cluster_index,
                label,
                dim,
                embedding,
                created_at,
            });
        }
        Ok(out)
    }

    /// ENROLL (Phase 2): bind a person `label` to the voiceprint of ONE diarized cluster of
    /// `meeting_id` (the `others-{cluster_index}` cluster). Called from the gated `rename_speaker`
    /// command when a diarized-cluster label is renamed to a person name — so the next meeting can
    /// re-identify the same voice. Idempotent: re-labeling overwrites. Returns the number of rows
    /// updated (0 if this meeting produced no voiceprint for that cluster — e.g. the recording
    /// predates the opt-in, so enroll is simply a no-op). NO PII is logged by the caller.
    ///
    /// GATE NOTE: the WRITE target is anchored to `meeting_id`; the CALLER (`rename_speaker`) has
    /// already refused a locked meeting, so this only ever writes for a visible meeting. No read of
    /// any other meeting's voiceprint happens here.
    pub fn set_voiceprint_label_for_cluster(
        &self,
        meeting_id: &str,
        cluster_index: i64,
        label: &str,
    ) -> Result<usize> {
        let conn = self.lock();
        let n = conn
            .execute(
                "UPDATE speaker_voiceprints SET label = ?3 \
                   WHERE meeting_id = ?1 AND cluster_index = ?2",
                rusqlite::params![meeting_id, cluster_index, label],
            )
            .map_err(map_err)?;
        Ok(n)
    }

    /// FORGET one voiceprint by id — a HARD delete (a voice biometric is not history worth keeping;
    /// mirror the `purge_speaker_voiceprints_tx` delete-not-invalidate discipline). Idempotent
    /// (deleting a missing id is a no-op). Returns true iff a row was removed.
    pub fn delete_voiceprint(&self, id: &str) -> Result<bool> {
        let conn = self.lock();
        let n = conn
            .execute(
                "DELETE FROM speaker_voiceprints WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(map_err)?;
        Ok(n > 0)
    }

    /// CLEAR every stored voiceprint (the "forget all captured voices" affordance). HARD delete of
    /// the whole table — a voice biometric store the user asked to erase. Returns the count removed.
    pub fn clear_voiceprints(&self) -> Result<usize> {
        let conn = self.lock();
        let n = conn
            .execute("DELETE FROM speaker_voiceprints", [])
            .map_err(map_err)?;
        Ok(n)
    }

    /// FORGET one user fact by id (bitemporal INVALIDATE, never a silent delete): close the row at
    /// `at` if it is still open. Idempotent (a already-closed row is untouched). History is preserved
    /// — the fact simply stops being current, so it drops out of `list_user_facts_visible` and the
    /// regenerated brief. Returns `true` iff a row was closed by this call.
    pub fn forget_user_fact(&self, id: &str, at: &str) -> Result<bool> {
        let conn = self.lock();
        let n = conn
            .execute(
                "UPDATE user_facts SET valid_to = ?2 WHERE id = ?1 AND valid_to IS NULL",
                rusqlite::params![id, at],
            )
            .map_err(map_err)?;
        Ok(n > 0)
    }

    /// CLEAR all user memory: bitemporal-close EVERY currently-open user fact at `at` (invalidate,
    /// never delete — closed history stays for the record). After this the brief regenerates empty and
    /// the audit view is empty. Returns the number of facts closed.
    pub fn clear_user_facts(&self, at: &str) -> Result<usize> {
        let conn = self.lock();
        let n = conn
            .execute(
                "UPDATE user_facts SET valid_to = ?1 WHERE valid_to IS NULL",
                rusqlite::params![at],
            )
            .map_err(map_err)?;
        Ok(n)
    }
}

/// Collision-proof unique temp path for file-backed tests.
///
/// `cargo test` runs the tests in ONE process as parallel THREADS, so `process::id()` is identical
/// across every test and `SystemTime::now().as_nanos()` can repeat within a single OS clock tick —
/// two tests then build the SAME `.sqlite` path and race `migrate()` on one file
/// (`Storage("database is locked")` / `duplicate column name`). The process-unique monotone
/// `COUNTER` guarantees no two calls in this process ever collide, regardless of the clock.
#[cfg(test)]
pub(crate) fn unique_temp_path(prefix: &str, ext: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{pid}-{nanos}-{n}.{ext}"))
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

/// A meeting's typed in-meeting notes in either seal state (brain2 realtime-notes lock lifecycle).
/// `text` is the plaintext column (blank while sealed); `blob` is the AES-GCM ciphertext under the
/// folder CK (present while sealed). Mirrors [`RawTimeline`].
#[derive(Debug, Clone)]
pub struct RawManualNotes {
    pub text: String,
    pub blob: Option<Vec<u8>>,
}

/// An uploaded document's plaintext in either seal state (document-ingestion lock lifecycle).
/// `text` is the plaintext column (blank while sealed); `blob` is the AES-GCM ciphertext under the
/// folder CK (present while sealed). Mirrors [`RawManualNotes`]. `id` is the document id.
#[derive(Debug, Clone)]
pub struct RawDocument {
    pub id: String,
    pub text: String,
    pub blob: Option<Vec<u8>>,
}

/// The raw column read for one authored note (`documents(kind='note')`) — the low-level shape the
/// gated command layer turns into a [`NoteDoc`]/[`NoteSummary`]. `title`/`updated_at` are the new
/// nullable authoring columns (NULL ⇒ fall back to `name`/`created_at` at the DTO layer). `sealed`
/// is true when a `text_blob` exists (the folder is or was locked) — used only as a hint; the actual
/// mask decision is the command-layer session-unlock check.
#[derive(Debug, Clone)]
pub struct NoteRow {
    pub id: String,
    pub folder_id: String,
    pub name: String,
    pub title: Option<String>,
    pub text: String,
    pub created_at: i64,
    pub updated_at: Option<i64>,
    pub exported_path: Option<String>,
    pub sealed: bool,
}

/// One stored speaker voiceprint row (opt-in voice biometric for a diarized "others" cluster).
/// `embedding` is the decoded, L2-normalized CAM++ vector (`dim` floats). `label` is the bound
/// person name once the cluster is enrolled by rename (NULL until then). Read ONLY through the gated
/// `list_voiceprints_visible` — never surface one whose source meeting is sealed.
#[derive(Debug, Clone)]
pub struct Voiceprint {
    pub id: String,
    pub meeting_id: String,
    pub cluster_index: i64,
    pub label: Option<String>,
    pub dim: i64,
    pub embedding: Vec<f32>,
    pub created_at: String,
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

/// Split a note's full markdown into `(front_matter_yaml, body)`. A leading `---\n … \n---` block
/// is the YAML front-matter (Obsidian-native properties); everything after the closing `---` is the
/// BODY. When there is no well-formed front-matter block the whole string is the body and the yaml
/// is empty. Hand-rolled (no `serde_yaml` dep) — mirrors the frontmatter detection in
/// `verify::extract_issue_keys` and `export::obsidian::inject_provenance_frontmatter`.
pub(crate) fn split_front_matter(markdown: &str) -> (String, String) {
    // Walk lines by BYTE OFFSET (robust to `\r\n` and a final line without a trailing newline).
    // `rest` is the remaining input; `offset` is its start position in `markdown`.
    let mut offset = 0usize;
    let mut fm_lines: Vec<&str> = Vec::new();
    let mut saw_open = false;
    while offset < markdown.len() {
        let rest = &markdown[offset..];
        // Length of this line INCLUDING its line terminator (\n or \r\n), and the trimmed content.
        let (line, advance) = match rest.find('\n') {
            Some(nl) => (rest[..nl].trim_end_matches('\r'), nl + 1),
            None => (rest.trim_end_matches('\r'), rest.len()),
        };
        if !saw_open {
            // The FIRST line must be a `---` fence, else there is no front-matter.
            if line.trim() != "---" {
                return (String::new(), markdown.to_string());
            }
            saw_open = true;
            offset += advance;
            continue;
        }
        if line.trim() == "---" {
            // Closing fence: the body is whatever follows, with one leading newline trimmed.
            let body = &markdown[offset + advance..];
            let body = body.strip_prefix('\n').unwrap_or(body);
            return (fm_lines.join("\n"), body.to_string());
        }
        fm_lines.push(line);
        offset += advance;
    }
    // Opened but never closed → not valid front-matter; the whole thing is body.
    (String::new(), markdown.to_string())
}

/// Parse a note's YAML front-matter into `(tags, properties)`. `tags` = the `tags:` list (either a
/// flow list `[a, b]` or a `- item` block, or a single scalar); `properties` = every OTHER scalar
/// `key: value` pair (excluding `tags`). Nested/complex YAML is best-effort: only top-level scalar
/// keys and the `tags` list are recognized (no `serde_yaml` — additive, dep-free). Values are
/// unquoted + trimmed. Used to build the leak-free note DTOs.
pub(crate) fn parse_front_matter(
    markdown: &str,
) -> (Vec<String>, std::collections::BTreeMap<String, String>) {
    let (yaml, _body) = split_front_matter(markdown);
    let mut tags: Vec<String> = Vec::new();
    let mut props: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let mut pending_tags_block = false; // inside a `tags:` block-list (`- item` lines)
    for raw in yaml.lines() {
        let line = raw.trim_end();
        // Continuation of a `tags:` block list — a `- value` line under `tags:`.
        if pending_tags_block {
            let t = line.trim_start();
            if let Some(item) = t.strip_prefix("- ") {
                push_tag(&mut tags, item);
                continue;
            }
            if let Some(item) = t.strip_prefix('-') {
                push_tag(&mut tags, item);
                continue;
            }
            // A non-`-` line ends the tags block; fall through to normal key parsing.
            pending_tags_block = false;
        }
        // Only top-level keys (no leading indentation) are parsed as scalars.
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.eq_ignore_ascii_case("tags") {
            if value.is_empty() {
                pending_tags_block = true; // a `tags:` header → block list follows.
            } else if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
                for part in inner.split(',') {
                    push_tag(&mut tags, part);
                }
            } else {
                push_tag(&mut tags, value); // single scalar tag
            }
            continue;
        }
        if key.is_empty() || value.is_empty() {
            continue;
        }
        props.insert(key.to_string(), unquote(value));
    }
    (tags, props)
}

/// Push a cleaned, non-empty tag (unquoted, `#`-stripped, trimmed) — de-duplicated.
fn push_tag(tags: &mut Vec<String>, raw: &str) {
    let t = unquote(raw.trim()).trim_start_matches('#').trim().to_string();
    if !t.is_empty() && !tags.contains(&t) {
        tags.push(t);
    }
}

/// Strip a single pair of matching surrounding quotes (`"…"` or `'…'`) from a YAML scalar.
fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// The first ~180 chars of a note's BODY (front-matter stripped), collapsed to single-spaced,
/// markdown heading/emphasis markers softened, for a leak-free list snippet. Empty for an empty body.
pub(crate) fn note_snippet(markdown: &str) -> String {
    let (_yaml, body) = split_front_matter(markdown);
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 180 {
        flat
    } else {
        let truncated: String = flat.chars().take(180).collect();
        format!("{}…", truncated.trim_end())
    }
}

/// Brain v2 L1.5 — the optional `started_at` window predicate (half-open `[from, to)`), rendered
/// as an ` AND …` suffix for the gated retrieval readers. Empty for `None`. The bounds are
/// app-generated ISO `YYYY-MM-DD` strings (from `summarize::temporal`) — compared
/// lexicographically against the ISO-8601 `started_at` column — but single quotes are still
/// escaped defensively (mirrors `visibility_clause`).
fn date_clause(date_filter: Option<&(String, String)>) -> String {
    match date_filter {
        Some((from, to)) => format!(
            " AND m.started_at >= '{}' AND m.started_at < '{}'",
            from.replace('\'', "''"),
            to.replace('\'', "''")
        ),
        None => String::new(),
    }
}

/// Minimum entity-name length (in chars) eligible for QUERY→ENTITY resolution (GraphRAG-lite).
/// Names shorter than this are too noisy as whole-query tokens (e.g. 2-letter initials) and are
/// never resolved.
const MIN_ENTITY_NAME_LEN: usize = 3;

/// Lowercase + tokenize on Unicode non-alphanumeric boundaries (Polish-safe via
/// `char::is_alphanumeric`). Empty tokens are dropped. Used by the deterministic QUERY→ENTITY
/// resolver so matching is on whole tokens, never arbitrary substrings.
fn tokenize_lower(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// Whole-token match: `name_ci`'s tokens must appear as a CONTIGUOUS, in-order run inside
/// `query_tokens` (already lowercased). So "atlas" matches the query "atlas status" but NOT the
/// substring "atlasian"; a multi-token name like "anna kowalska" matches only when both tokens are
/// adjacent and ordered. Guards the entity expansion against spurious substring noise.
fn name_matches_query_tokens(query_tokens: &[String], name_ci: &str) -> bool {
    let name_tokens: Vec<&str> = name_ci
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    if name_tokens.is_empty() || name_tokens.len() > query_tokens.len() {
        return false;
    }
    query_tokens
        .windows(name_tokens.len())
        .any(|w| w.iter().zip(name_tokens.iter()).all(|(a, b)| a == b))
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

/// Map an `mcp_servers` row (id, label, transport, endpoint, args JSON, enabled, consented,
/// created_at) to a [`crate::storage::models::McpServer`]. Malformed `args` JSON degrades to no
/// args (fail-quiet on config, never a crash).
fn row_to_mcp_server(row: &Row<'_>) -> rusqlite::Result<crate::storage::models::McpServer> {
    let args_json: String = row.get(4)?;
    Ok(crate::storage::models::McpServer {
        id: row.get(0)?,
        label: row.get(1)?,
        transport: row.get(2)?,
        endpoint: row.get(3)?,
        args: serde_json::from_str(&args_json).unwrap_or_default(),
        enabled: row.get::<_, i64>(5)? != 0,
        consented: row.get::<_, i64>(6)? != 0,
        created_at: row.get(7)?,
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

/// Escape LIKE wildcards so user input is matched literally (paired with `ESCAPE '\'`).
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
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

/// The OR-joined twin of [`fts_match_query`]: the same tokenize-and-quote defusal, but terms are
/// joined with `OR` instead of implicit AND. Used for RELEVANCE filtering (Brain v2 L2.2), where the
/// "query" is a whole natural-language question and a short fact row should match on ANY shared
/// content word — an AND over the full question would almost never match a 5-word fact. BM25 still
/// ranks multi-term matches above single-term ones.
///
/// CONTENT WORDS ONLY: stopwords (the shared EN+PL list, `related_context::is_stopword` — one
/// source of truth) and tokens shorter than 3 chars are DROPPED before the OR is built. Without
/// this, a question sharing only "the"/"co"/"is" with an irrelevant fact produces a non-empty hit
/// set that DISPLACES the caller's full-list fallback (the reproduced brief-displacement bug —
/// unicode61 has no stopword list and BM25 cannot rescue a hit set whose only members are stopword
/// matches). Empty / punctuation-only / all-stopword input yields `None` — the caller's fallback
/// owns that case.
fn fts_match_query_any(q: &str) -> Option<String> {
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
    // Keep only content words: lowercase (FTS unicode61 case-folds anyway), then drop stopwords
    // and <3-char tokens so a function-word overlap can never produce a "relevant" hit.
    let terms: Vec<String> = terms
        .into_iter()
        .map(|t| t.to_lowercase())
        .filter(|t| t.chars().count() >= 3 && !crate::summarize::related_context::is_stopword(t))
        .collect();
    if terms.is_empty() {
        return None;
    }
    Some(
        terms
            .iter()
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

/// Build a `(snippet, matched_in)` pair for a search hit, reusing the open connection.
fn search_snippet(conn: &Connection, m: &Meeting, q: &str, like: &str) -> Result<(String, String)> {
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
        assert_eq!(
            fts_match_query("alpha beta"),
            Some("\"alpha\" \"beta\"".into())
        );
        // Order is just term order; the conjunction is order-independent at the SQL level.
        assert_eq!(
            fts_match_query("beta alpha"),
            Some("\"beta\" \"alpha\"".into())
        );
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
        model_requested: row.get(5)?,
        model_served: row.get(6)?,
        gateway_host: row.get(7)?,
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

/// Column order: `id, name, path, parent_id, locked, kind`.
fn row_to_note_folder(row: &Row<'_>) -> rusqlite::Result<NoteFolder> {
    Ok(NoteFolder {
        id: row.get(0)?,
        name: row.get(1)?,
        path: row.get(2)?,
        parent_id: row.get(3)?,
        locked: row.get::<_, i64>(4)? != 0,
        kind: row.get::<_, Option<String>>(5)?.unwrap_or_else(|| "note".into()),
    })
}

/// Column order: `id, folder_id, name, title, text, created_at, updated_at, exported_path, sealed`.
fn row_to_note_row(row: &Row<'_>) -> rusqlite::Result<NoteRow> {
    Ok(NoteRow {
        id: row.get(0)?,
        folder_id: row.get(1)?,
        name: row.get(2)?,
        title: row.get(3)?,
        text: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        exported_path: row.get(7)?,
        sealed: row.get::<_, i64>(8)? != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{Meeting, MeetingStatus, NoteRecord};

    fn mem_db() -> Db {
        // In-memory DB shares the same open/migrate path as on-disk. The vec0 module must be
        // auto-registered BEFORE the connection is opened (migrate() creates the vec_chunks vtab),
        // exactly as `open_with_key` does for real handles.
        register_vec_extension();
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

    /// NOTES feature — the additive columns exist after migrate() and re-migrating is a no-op
    /// (idempotent guard). `documents.title/updated_at/exported_path` + `folders.kind` are present,
    /// and existing folders default to `kind='meeting'`.
    #[test]
    fn notes_migration_adds_columns_and_is_idempotent() {
        let db = mem_db();
        let has_col = |table: &str, col: &str| -> bool {
            let conn = db.lock();
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            cols.iter().any(|c| c == col)
        };
        for col in ["title", "updated_at", "exported_path"] {
            assert!(has_col("documents", col), "documents.{col} added");
        }
        assert!(has_col("folders", "kind"), "folders.kind added");

        // A folder inserted via the legacy 6-column path reads back kind='meeting' (the DEFAULT).
        db.insert_folder(&Folder {
            id: "leg".into(),
            name: "Legacy".into(),
            path: "Legacy".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-10T00:00:00Z".into(),
        })
        .unwrap();
        assert_eq!(
            db.folder_kind("leg").unwrap().as_deref(),
            Some("meeting"),
            "existing folders default to kind='meeting' (Meetings tree unchanged)"
        );
        assert!(
            db.list_note_folders().unwrap().is_empty(),
            "a meeting folder is NOT a note-folder"
        );

        // Re-migrate: still fine (idempotent), columns still present.
        db.migrate().unwrap();
        assert!(has_col("documents", "exported_path"));
    }

    /// NOTES — front-matter parsing: `tags` (flow list, block list, single scalar), scalar
    /// `properties` (excl. tags), and the body snippet (front-matter stripped). No new deps.
    #[test]
    fn front_matter_parse_and_snippet() {
        // Flow-list tags + scalar props.
        let md = "---\ntags: [alpha, \"beta gamma\"]\nstatus: draft\ndate: 2026-07-10\n---\nBody text here.";
        let (tags, props) = parse_front_matter(md);
        assert_eq!(tags, vec!["alpha".to_string(), "beta gamma".to_string()]);
        assert_eq!(props.get("status"), Some(&"draft".to_string()));
        assert_eq!(props.get("date"), Some(&"2026-07-10".to_string()));
        assert!(!props.contains_key("tags"), "tags excluded from properties");
        assert_eq!(note_snippet(md), "Body text here.");
        let (_yaml, body) = split_front_matter(md);
        assert_eq!(body, "Body text here.", "front-matter stripped from body");

        // Block-list tags.
        let md2 = "---\ntags:\n  - one\n  - two\n---\n# Heading\nMore.";
        let (tags2, _p2) = parse_front_matter(md2);
        assert_eq!(tags2, vec!["one".to_string(), "two".to_string()]);

        // No front-matter: whole string is body, no tags/props.
        let md3 = "Just a plain note, no YAML.";
        let (tags3, props3) = parse_front_matter(md3);
        assert!(tags3.is_empty() && props3.is_empty());
        assert_eq!(split_front_matter(md3).1, md3);

        // Snippet truncation at ~180 chars.
        let long = format!("---\ntags: []\n---\n{}", "word ".repeat(100));
        assert!(
            note_snippet(&long).chars().count() <= 181,
            "snippet capped ~180 chars (+ellipsis)"
        );
    }

    /// TIER 1 installed-base flip: `migrate()` sets `semantic_search_enabled='true'` + the
    /// `semantic_default_v1` sentinel exactly ONCE; a later opt-out (`false`) survives a re-migrate
    /// (the sentinel guards the block so it never re-fires). Config-only, idempotent, reversible.
    #[test]
    fn tier1_semantic_default_migration_runs_once_and_opt_out_persists() {
        let db = mem_db();
        db.migrate().unwrap();
        assert_eq!(
            db.get_setting("semantic_default_v1").unwrap().as_deref(),
            Some("1"),
            "sentinel is set after the migration"
        );
        assert_eq!(
            db.get_setting("semantic_search_enabled")
                .unwrap()
                .as_deref(),
            Some("true"),
            "installed base is flipped ON once"
        );
        // A user turns semantic OFF after the migration; re-running migrate() must NOT flip it back.
        db.set_setting("semantic_search_enabled", "false").unwrap();
        db.migrate().unwrap();
        assert_eq!(
            db.get_setting("semantic_search_enabled")
                .unwrap()
                .as_deref(),
            Some("false"),
            "sentinel-guarded: a post-migration opt-out persists across re-migrate"
        );
    }

    /// WS8 installed-base flip (mirrors the `semantic_default_v1` precedent): `migrate()` sets
    /// `capture_system_audio='true'` + the `capture_default_v1` sentinel exactly ONCE; a later opt-out
    /// (`false`) survives a re-migrate (the sentinel guards the block so it never re-fires). Config-only,
    /// idempotent, reversible.
    #[test]
    fn capture_default_migration_runs_once_and_opt_out_persists() {
        let db = mem_db();
        db.migrate().unwrap();
        assert_eq!(
            db.get_setting("capture_default_v1").unwrap().as_deref(),
            Some("1"),
            "sentinel is set after the migration"
        );
        assert_eq!(
            db.get_setting("capture_system_audio").unwrap().as_deref(),
            Some("true"),
            "installed base is flipped ON once"
        );
        // A user turns system-audio capture OFF after the migration; re-running migrate() must NOT
        // flip it back.
        db.set_setting("capture_system_audio", "false").unwrap();
        db.migrate().unwrap();
        assert_eq!(
            db.get_setting("capture_system_audio").unwrap().as_deref(),
            Some("false"),
            "sentinel-guarded: a post-migration opt-out persists across re-migrate"
        );
    }

    // ── Phase 2b: egress_log ─────────────────────────────────────────────────

    fn sample_egress_entry() -> crate::summarize::egress_log::EgressEntry {
        use crate::summarize::egress_log::EgressEntry;
        use crate::summarize::meta::{CallMeta, RedactionCounts};
        EgressEntry {
            provider_id: "anthropic".to_string(),
            destination: "api.anthropic.com".to_string(),
            model_requested: "claude-opus-4-8".to_string(),
            call_kind: "complete",
            meta: CallMeta {
                model_served: Some("claude-opus-4-8-20251001".to_string()),
                prompt_tokens: Some(100),
                completion_tokens: Some(50),
                total_tokens: Some(150),
                cached_tokens: None,
                redactions: None,
            },
            redactions: RedactionCounts {
                email: 1,
                card: 0,
                phone: 1,
                name: 2,
            },
            system_bytes: 512,
            user_bytes: 1024,
            meeting_id: Some("m1".to_string()),
        }
    }

    /// After migrate(), `egress_log` exists; insert_egress then SELECT COUNT(*) == 1.
    #[test]
    fn egress_log_table_exists_after_migrate_and_insert_works() {
        let db = mem_db();
        let entry = sample_egress_entry();
        db.insert_egress(1_700_000_000, &entry).unwrap();

        let conn = db.lock();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM egress_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "one row must have been inserted into egress_log");
    }

    /// migrate() is idempotent even with the new egress_log table (re-running migrate() on an
    /// already-migrated DB must not error).
    #[test]
    fn egress_log_migrate_idempotent() {
        let db = mem_db(); // migrate() already ran once
        db.migrate().unwrap(); // second run must succeed
    }

    /// insert_egress stores token counts and redaction counts correctly.
    #[test]
    fn egress_log_insert_round_trips_counts() {
        let db = mem_db();
        let entry = sample_egress_entry();
        db.insert_egress(9999, &entry).unwrap();

        let conn = db.lock();
        let (ts, prompt_tokens, redactions_email, redactions_name, system_bytes): (
            i64,
            Option<i64>,
            i64,
            i64,
            i64,
        ) = conn
            .query_row(
                "SELECT ts, prompt_tokens, redactions_email, redactions_name, system_bytes
                   FROM egress_log LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(ts, 9999);
        assert_eq!(prompt_tokens, Some(100));
        assert_eq!(redactions_email, 1);
        assert_eq!(redactions_name, 2);
        assert_eq!(system_bytes, 512);
    }

    // ── Phase 6: egress_summary ───────────────────────────────────────────────

    /// Helper: build an `EgressEntry` with the given model_served, total_tokens, and redaction
    /// email count (other fields are stable across tests).
    fn egress_entry_for_summary(
        model_served: &str,
        total_tokens: u32,
        redaction_email: u32,
        redaction_name: u32,
    ) -> crate::summarize::egress_log::EgressEntry {
        use crate::summarize::egress_log::EgressEntry;
        use crate::summarize::meta::{CallMeta, RedactionCounts};
        EgressEntry {
            provider_id: "anthropic".to_string(),
            destination: "api.anthropic.com".to_string(),
            model_requested: model_served.to_string(),
            call_kind: "complete",
            meta: CallMeta {
                model_served: Some(model_served.to_string()),
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: Some(total_tokens),
                cached_tokens: None,
                redactions: None,
            },
            redactions: RedactionCounts {
                email: redaction_email,
                card: 0,
                phone: 0,
                name: redaction_name,
            },
            system_bytes: 0,
            user_bytes: 0,
            meeting_id: None,
        }
    }

    /// `egress_summary(30)` on an EMPTY table returns all-zero totals and empty vecs — not an error.
    /// RED → verify the function exists and handles the zero case before we insert any rows.
    #[test]
    fn egress_summary_empty_table_returns_zeros() {
        let db = mem_db();
        let ledger = db
            .egress_summary(30)
            .expect("egress_summary must not error on empty table");
        assert_eq!(
            ledger.total_calls, 0,
            "total_calls should be 0 on empty table"
        );
        assert_eq!(
            ledger.total_tokens, 0,
            "total_tokens should be 0 on empty table"
        );
        assert!(
            ledger.by_model.is_empty(),
            "by_model should be empty on empty table"
        );
        assert!(
            ledger.by_day.is_empty(),
            "by_day should be empty on empty table"
        );
        assert_eq!(ledger.total_redactions.email, 0);
        assert_eq!(ledger.total_redactions.name, 0);
        assert!(
            ledger.recent.is_empty(),
            "recent should be empty on empty table"
        );
    }

    /// Insert 3 rows — 2 models ("claude-opus", "gpt-4o"), 2 distinct UTC dates, known redaction
    /// counts — and assert `egress_summary(30)` aggregates them correctly.
    ///
    /// Row layout:
    ///   ts=1_700_000_000 (2023-11-14 UTC) — claude-opus, 100 tokens, email=1, name=0
    ///   ts=1_700_086_400 (2023-11-15 UTC) — gpt-4o,     200 tokens, email=0, name=1
    ///   ts=1_700_086_401 (2023-11-15 UTC) — claude-opus,  50 tokens, email=2, name=1
    #[test]
    fn egress_summary_aggregates_correctly() {
        let db = mem_db();

        // Insert outside the 30-day window: ts=0 should be excluded once `since` is computed.
        // (We use absolute timestamps well in the past — window uses `now - 30*86400` which will
        // exclude ts=0. But we can't control "now" in tests so use days <= 0 for all-rows, or
        // override the window. Here we use days=0 which means "all rows" per our implementation.)
        let entry1 = egress_entry_for_summary("claude-opus", 100, 1, 0);
        let entry2 = egress_entry_for_summary("gpt-4o", 200, 0, 1);
        let entry3 = egress_entry_for_summary("claude-opus", 50, 2, 1);

        // Use realistic timestamps on the same "relative past" — for the rolling window test we
        // use days=0 (all-rows mode: since=0) to avoid clock skew in CI.
        db.insert_egress(1_700_000_000, &entry1).unwrap();
        db.insert_egress(1_700_086_400, &entry2).unwrap();
        db.insert_egress(1_700_086_401, &entry3).unwrap();

        let ledger = db.egress_summary(0).expect("egress_summary must not error");

        // ── totals ────────────────────────────────────────────────────────
        assert_eq!(ledger.total_calls, 3, "total_calls must be 3");
        assert_eq!(
            ledger.total_tokens, 350,
            "total_tokens must be 100+200+50=350"
        );

        // ── by_model (ordered tokens DESC: gpt-4o=200, claude-opus=150) ──
        assert_eq!(ledger.by_model.len(), 2, "by_model must have 2 entries");
        let gpt = ledger
            .by_model
            .iter()
            .find(|m| m.model == "gpt-4o")
            .expect("gpt-4o entry must exist");
        assert_eq!(gpt.calls, 1, "gpt-4o: 1 call");
        assert_eq!(gpt.tokens, 200, "gpt-4o: 200 tokens");

        let opus = ledger
            .by_model
            .iter()
            .find(|m| m.model == "claude-opus")
            .expect("claude-opus entry must exist");
        assert_eq!(opus.calls, 2, "claude-opus: 2 calls");
        assert_eq!(opus.tokens, 150, "claude-opus: 100+50=150 tokens");

        // First entry is the one with more tokens (gpt-4o=200 > claude-opus=150)
        assert_eq!(
            ledger.by_model[0].model, "gpt-4o",
            "by_model[0] must be gpt-4o (most tokens)"
        );

        // ── by_day (2 distinct UTC days, ascending) ───────────────────────
        assert_eq!(ledger.by_day.len(), 2, "by_day must have 2 entries");
        // 1_700_000_000 = 2023-11-14 UTC; 1_700_086_400/1_700_086_401 = 2023-11-15 UTC
        assert_eq!(
            ledger.by_day[0].day, "2023-11-14",
            "by_day[0] must be 2023-11-14"
        );
        assert_eq!(ledger.by_day[0].tokens, 100, "2023-11-14: 100 tokens");
        assert_eq!(
            ledger.by_day[1].day, "2023-11-15",
            "by_day[1] must be 2023-11-15"
        );
        assert_eq!(
            ledger.by_day[1].tokens, 250,
            "2023-11-15: 200+50=250 tokens"
        );

        // ── total_redactions: email=1+0+2=3, name=0+1+1=2 ────────────────
        assert_eq!(
            ledger.total_redactions.email, 3,
            "total email redactions must be 3"
        );
        assert_eq!(ledger.total_redactions.card, 0);
        assert_eq!(ledger.total_redactions.phone, 0);
        assert_eq!(
            ledger.total_redactions.name, 2,
            "total name redactions must be 2"
        );

        // ── recent: 3 rows, newest first ─────────────────────────────────
        assert_eq!(ledger.recent.len(), 3, "recent must have 3 rows");
        assert_eq!(
            ledger.recent[0].ts, 1_700_086_401,
            "recent[0] must be the newest row"
        );
        assert_eq!(
            ledger.recent[2].ts, 1_700_000_000,
            "recent[2] must be the oldest row"
        );
    }

    /// `egress_summary(30)` excludes rows outside the time window. Two rows in the past (ts≈0 =
    /// 1970, well before any 30-day window); one row at `now_unix - 1` (inside the window).
    /// We verify that only the in-window row is counted when days=30.
    #[test]
    fn egress_summary_respects_time_window() {
        let db = mem_db();

        // A row far in the past — should be excluded from any 30-day window.
        let old = egress_entry_for_summary("old-model", 9999, 5, 5);
        db.insert_egress(1_000, &old).unwrap(); // 1970, definitley outside 30d window

        // A row "now" — should be included.
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let recent = egress_entry_for_summary("new-model", 42, 0, 0);
        db.insert_egress(now_unix, &recent).unwrap();

        let ledger = db
            .egress_summary(30)
            .expect("egress_summary must not error");
        assert_eq!(
            ledger.total_calls, 1,
            "only 1 in-window row should be counted"
        );
        assert_eq!(
            ledger.total_tokens, 42,
            "only in-window tokens should be counted"
        );
        assert_eq!(
            ledger.by_model.len(),
            1,
            "by_model should only contain new-model"
        );
        assert_eq!(ledger.by_model[0].model, "new-model");
        // Redaction totals from the old row must NOT appear.
        assert_eq!(
            ledger.total_redactions.email, 0,
            "old row redactions must be excluded"
        );
    }

    /// FIX 2: a row where BOTH `model_served` and `model_requested` are empty strings (the common
    /// case for claude_code / anthropic default-model calls before the gateway provider was added)
    /// must appear in `by_model` under the label `'(unknown)'`, not as a blank `""` string.
    #[test]
    fn egress_summary_blank_model_fields_bucket_under_unknown() {
        use crate::summarize::egress_log::EgressEntry;
        use crate::summarize::meta::{CallMeta, RedactionCounts};

        let db = mem_db();

        // Row with both model fields empty (default-model call; model_requested="" stored verbatim).
        let entry_no_model = EgressEntry {
            provider_id: "claude_code".to_string(),
            destination: "claude_code (Anthropic CLI)".to_string(),
            model_requested: String::new(), // "" — the real default
            call_kind: "complete",
            meta: CallMeta {
                model_served: None, // stored as NULL
                prompt_tokens: Some(10),
                completion_tokens: Some(5),
                total_tokens: Some(15),
                cached_tokens: None,
                redactions: None,
            },
            redactions: RedactionCounts::default(),
            system_bytes: 0,
            user_bytes: 0,
            meeting_id: None,
        };

        db.insert_egress(0, &entry_no_model).unwrap();

        let ledger = db.egress_summary(0).expect("egress_summary must not error");
        assert_eq!(ledger.by_model.len(), 1, "must have one by_model entry");
        assert_eq!(
            ledger.by_model[0].model, "(unknown)",
            "empty model_served + empty model_requested must bucket under '(unknown)', not ''"
        );
        assert_eq!(ledger.by_model[0].tokens, 15);
    }

    /// brain2 realtime notes: the `manual_notes` buffer round-trips (set → get), defaults to "" for a
    /// meeting that never set it (NULL legacy column), and `set("")` clears it. The buffer is DURABLE
    /// (canonical store) — it is not destroyed by any plain getter/setter.
    #[test]
    fn manual_notes_round_trip_and_default_empty() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-06-30T09:00:00Z"))
            .unwrap();

        // Default: never set ⇒ "" (NULL column reads back empty, no behavior change for legacy rows).
        assert_eq!(db.get_manual_notes("m1").unwrap(), "");
        // Unknown meeting ⇒ "" (no row), never an error.
        assert_eq!(db.get_manual_notes("nope").unwrap(), "");

        // Set → get round-trips verbatim.
        db.set_manual_notes("m1", "ship the deck by Friday; Anna owns QA")
            .unwrap();
        assert_eq!(
            db.get_manual_notes("m1").unwrap(),
            "ship the deck by Friday; Anna owns QA"
        );

        // Overwrite replaces the whole buffer (FE owns the full text).
        db.set_manual_notes("m1", "rewritten").unwrap();
        assert_eq!(db.get_manual_notes("m1").unwrap(), "rewritten");
    }

    /// SEAL ROUND-TRIP (mirrors `seal_transcript_timeline_round_trips_byte_identical`): the typed
    /// notes seal to a `manual_notes_blob` (plaintext blanked, ciphertext does NOT leak), and unlock
    /// restores the plaintext BYTE-IDENTICAL. Uses the DB seal/restore primitives + crypto directly.
    #[test]
    fn seal_manual_notes_round_trips_byte_identical() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-06-30T09:00:00Z"))
            .unwrap();
        let typed = "zażółć gęślą jaźń 🔒 — DECISION: ship Friday; Anna owns QA";
        db.set_manual_notes("m1", typed).unwrap();

        let ck = crate::crypto::random_key().unwrap();
        // SEAL: encrypt → verify-before-destroy → blank plaintext (the seal_meeting_extras pattern).
        let rn = db.raw_manual_notes("m1").unwrap().unwrap();
        let blob = crate::crypto::encrypt(&ck, rn.text.as_bytes(), b"aad").unwrap();
        assert_eq!(
            crate::crypto::decrypt(&ck, &blob, b"aad").unwrap(),
            rn.text.as_bytes()
        );
        db.seal_manual_notes("m1", &blob).unwrap();

        // At rest while sealed: plaintext blanked, blob present, ciphertext doesn't leak the plaintext.
        let sealed = db.raw_manual_notes("m1").unwrap().unwrap();
        assert_eq!(sealed.text, "", "plaintext blanked while sealed");
        assert!(
            sealed.blob.is_some(),
            "manual_notes_blob present while sealed"
        );
        assert_eq!(
            db.get_manual_notes("m1").unwrap(),
            "",
            "the gated reader sees blank while sealed"
        );
        let cipher = sealed.blob.as_ref().unwrap();
        let leaks = cipher.windows(typed.len()).any(|w| w == typed.as_bytes());
        assert!(!leaks, "manual-notes ciphertext must not leak plaintext");

        // UNLOCK: decrypt the blob → restore plaintext byte-identical.
        let blob = sealed.blob.unwrap();
        let pt = String::from_utf8(crate::crypto::decrypt(&ck, &blob, b"aad").unwrap()).unwrap();
        db.set_manual_notes("m1", &pt).unwrap();
        assert_eq!(
            db.get_manual_notes("m1").unwrap(),
            typed,
            "typed notes round-trip byte-identical"
        );

        // PERMANENT remove-lock: clear the blob after the plaintext is back.
        db.clear_manual_notes_blob("m1").unwrap();
        assert!(
            db.raw_manual_notes("m1").unwrap().unwrap().blob.is_none(),
            "blob cleared on remove-lock"
        );
        assert_eq!(
            db.get_manual_notes("m1").unwrap(),
            typed,
            "plaintext survives the blob clear"
        );
    }

    /// LOCK-SAFETY (verify-before-destroy): startup reconciliation re-blanks the typed-notes
    /// plaintext of a locked meeting ONLY when the sealed `manual_notes_blob` exists. A buffer that
    /// was NEVER sealed (no blob) is LEFT INTACT — reconciliation must never destroy the only copy.
    #[test]
    fn reconcile_reblanks_manual_notes_only_when_blob_present() {
        let db = mem_db();
        seed_folder(&db, "f-lock", "Secret");
        // Meeting A: sealed blob present + plaintext stranded by a crash-while-unlocked → MUST re-blank.
        db.insert_meeting(&sample_meeting("m-sealed", "2026-06-30T09:00:00Z"))
            .unwrap();
        note_for(&db, "m-sealed", "claude_code", "");
        db.set_note_folder("m-sealed", Some("f-lock")).unwrap();
        db.seal_note("m-sealed", "claude_code", b"ciphertext")
            .unwrap();
        db.seal_manual_notes("m-sealed", b"ck-ciphertext-blob")
            .unwrap(); // blob present
        db.set_manual_notes("m-sealed", "restored plaintext stranded by the crash")
            .unwrap();

        // Meeting B: NO blob (buffer typed but never sealed) → MUST be left intact (no encrypted copy).
        db.insert_meeting(&sample_meeting("m-unsealed", "2026-06-30T09:30:00Z"))
            .unwrap();
        note_for(&db, "m-unsealed", "claude_code", "note");
        db.set_note_folder("m-unsealed", Some("f-lock")).unwrap();
        db.set_manual_notes(
            "m-unsealed",
            "typed but never sealed — must not be destroyed",
        )
        .unwrap();

        db.set_folder_locked("f-lock", true, Some(b"wrapped"))
            .unwrap();
        db.reblank_locked_folders_at_rest().unwrap();

        assert_eq!(
            db.get_manual_notes("m-sealed").unwrap(),
            "",
            "sealed meeting's stranded plaintext re-blanked"
        );
        assert_eq!(
            db.get_manual_notes("m-unsealed").unwrap(),
            "typed but never sealed — must not be destroyed",
            "an unsealed buffer (no blob) must NEVER be blanked — that would destroy the only copy"
        );
    }

    /// Test-only builder for a correction example tied to a meeting.
    fn corr_rec(
        kind: &str,
        input: &str,
        model_output: &str,
        fin: Option<&str>,
        accepted: bool,
        at: &str,
        meeting_id: Option<&str>,
    ) -> crate::storage::models::CorrectionRecord {
        crate::storage::models::CorrectionRecord {
            id: 0,
            kind: kind.to_string(),
            input: input.to_string(),
            model_output: model_output.to_string(),
            final_output: fin.map(str::to_string),
            accepted,
            owner_id: "local".to_string(),
            created_at: at.to_string(),
            meeting_id: meeting_id.map(str::to_string),
        }
    }

    #[test]
    fn correction_log_round_trips_and_filters_by_kind() {
        let db = mem_db();
        // Two visible meetings in an OPEN folder so their corrections are returned by the gate.
        seed_folder(&db, "f-open", "Open");
        for id in ["m1", "m2", "m3"] {
            db.insert_meeting(&sample_meeting(id, "2026-06-24T10:00:00Z"))
                .unwrap();
            note_for(&db, id, "claude_code", "note");
            db.set_note_folder(id, Some("f-open")).unwrap();
        }
        let nothing = std::collections::HashSet::new();

        // Insert two NER examples (one accepted-as-is, one edited) and one of another kind.
        let id1 = db
            .log_correction(&corr_rec(
                "ner",
                "in-1",
                "out-1",
                None,
                true,
                "2026-06-28T10:00:00Z",
                Some("m1"),
            ))
            .unwrap();
        let id2 = db
            .log_correction(&corr_rec(
                "ner",
                "in-2",
                "out-2",
                Some("fixed-2"),
                false,
                "2026-06-28T10:01:00Z",
                Some("m2"),
            ))
            .unwrap();
        db.log_correction(&corr_rec(
            "timeline",
            "in-3",
            "out-3",
            None,
            true,
            "2026-06-28T10:02:00Z",
            Some("m3"),
        ))
        .unwrap();
        assert!(id2 > id1);

        // Only the matching kind comes back, newest first.
        let ner = db.list_corrections("ner", 10, &nothing).unwrap();
        assert_eq!(ner.len(), 2);
        assert_eq!(ner[0].id, id2);
        assert_eq!(ner[0].input, "in-2");
        assert_eq!(ner[0].final_output.as_deref(), Some("fixed-2"));
        assert!(!ner[0].accepted);
        assert_eq!(ner[0].meeting_id.as_deref(), Some("m2"));
        assert_eq!(ner[1].id, id1);
        assert!(ner[1].accepted);
        assert_eq!(ner[1].final_output, None);
        assert_eq!(ner[1].owner_id, "local");

        // Limit is honoured.
        assert_eq!(db.list_corrections("ner", 1, &nothing).unwrap().len(), 1);
        // A kind with no rows yields empty (not an error).
        assert!(db
            .list_corrections("does-not-exist", 10, &nothing)
            .unwrap()
            .is_empty());
    }

    /// GATE: a correction row for a sealed-and-not-unlocked meeting is EXCLUDED; the same kind's row
    /// for a visible meeting is INCLUDED. Session-unlocking the sealed folder makes it reappear.
    /// (RED before the gate: the un-gated reader returned both rows regardless of seal state.)
    #[test]
    fn list_corrections_excludes_sealed_meeting() {
        let db = mem_db();
        seed_folder(&db, "f-open", "Open");
        seed_folder(&db, "f-locked", "Secret");
        db.set_folder_locked("f-locked", true, Some(b"wrapped"))
            .unwrap();
        // Visible meeting in the open folder.
        db.insert_meeting(&sample_meeting("m-open", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "m-open", "claude_code", "note");
        db.set_note_folder("m-open", Some("f-open")).unwrap();
        // Sealed meeting in the locked folder.
        db.insert_meeting(&sample_meeting("m-sealed", "2026-06-24T11:00:00Z"))
            .unwrap();
        note_for(&db, "m-sealed", "claude_code", "note");
        db.set_note_folder("m-sealed", Some("f-locked")).unwrap();

        db.log_correction(&corr_rec(
            "ner",
            "in-o",
            "out-o",
            None,
            true,
            "2026-06-28T10:00:00Z",
            Some("m-open"),
        ))
        .unwrap();
        db.log_correction(&corr_rec(
            "ner",
            "in-s",
            "out-s",
            None,
            true,
            "2026-06-28T10:01:00Z",
            Some("m-sealed"),
        ))
        .unwrap();

        let nothing = std::collections::HashSet::new();
        let visible = db.list_corrections("ner", 10, &nothing).unwrap();
        assert_eq!(
            visible.len(),
            1,
            "sealed meeting's correction leaked through the gate"
        );
        assert_eq!(visible[0].meeting_id.as_deref(), Some("m-open"));

        // Session-unlock the locked folder → its correction reappears.
        let mut unlocked = std::collections::HashSet::new();
        unlocked.insert("f-locked".to_string());
        let both = db.list_corrections("ner", 10, &unlocked).unwrap();
        assert_eq!(
            both.len(),
            2,
            "session-unlocked meeting's correction must reappear"
        );
    }

    /// FAIL-CLOSED: a correction row with a NULL `meeting_id` (legacy/unattributed) is never returned
    /// by the gated reader, even with nothing locked.
    #[test]
    fn list_corrections_excludes_null_meeting_id() {
        let db = mem_db();
        db.log_correction(&corr_rec(
            "ner",
            "in-x",
            "out-x",
            None,
            true,
            "2026-06-28T10:00:00Z",
            None,
        ))
        .unwrap();
        let nothing = std::collections::HashSet::new();
        assert!(
            db.list_corrections("ner", 10, &nothing).unwrap().is_empty(),
            "NULL-meeting_id correction must be excluded (fail-closed)"
        );
    }

    /// PURGE-ON-SEAL: a correction row tied to a meeting in a folder is GONE after the folder is
    /// sealed (relock blanker). (RED before the purge: the row survived at rest.)
    #[test]
    fn correction_log_purged_on_lock() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "note");
        db.set_note_folder("m1", Some("f-locked")).unwrap();
        db.log_correction(&corr_rec(
            "ner",
            "in-1",
            "out-1",
            None,
            true,
            "2026-06-28T10:00:00Z",
            Some("m1"),
        ))
        .unwrap();
        assert_eq!(
            correction_count(&db, "m1"),
            1,
            "expected a correction row before seal"
        );

        // Seal: blank the note (content_blob present so blank_sealed_notes_in_folders acts), then run
        // the relock blanker for the folder.
        db.seal_note("m1", "claude_code", b"ciphertext").unwrap();
        let mut folders = std::collections::HashSet::new();
        folders.insert("f-locked".to_string());
        db.blank_sealed_notes_in_folders(&folders).unwrap();

        assert_eq!(
            correction_count(&db, "m1"),
            0,
            "correction_log row must be purged on seal (no flywheel data for sealed meetings)"
        );
    }

    /// PURGE-ON-DELETE: deleting a meeting also removes its correction-log rows.
    #[test]
    fn correction_log_purged_on_delete_meeting() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        db.log_correction(&corr_rec(
            "ner",
            "in-1",
            "out-1",
            None,
            true,
            "2026-06-28T10:00:00Z",
            Some("m1"),
        ))
        .unwrap();
        assert_eq!(correction_count(&db, "m1"), 1);
        db.delete_meeting("m1").unwrap();
        assert_eq!(
            correction_count(&db, "m1"),
            0,
            "delete_meeting must purge the meeting's correction_log rows"
        );
    }

    fn correction_count(db: &Db, meeting_id: &str) -> i64 {
        db.lock()
            .query_row(
                "SELECT COUNT(*) FROM correction_log WHERE meeting_id = ?1",
                rusqlite::params![meeting_id],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn interaction_count(db: &Db, meeting_id: &str) -> i64 {
        db.lock()
            .query_row(
                "SELECT COUNT(*) FROM assistant_interactions WHERE meeting_id = ?1",
                rusqlite::params![meeting_id],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// PERSIST → READ round-trips a voice-assistant interaction (command + answer + citations +
    /// status + source_label) for a VISIBLE (unfoldered) meeting.
    #[test]
    fn assistant_interaction_round_trips_for_visible_meeting() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        db.insert_assistant_interaction(
            "m1",
            "Klaudku, sprawdź jaka była pogoda",
            "Wczoraj było słonecznie. Zobacz [[Notatka o pogodzie]].",
            &[
                "[[Notatka o pogodzie]]".to_string(),
                "(web) Weather — http://x".to_string(),
            ],
            "ok",
            Some("research"),
            None,
            None,
            "2026-06-24T10:05:00Z",
        )
        .unwrap();

        let nothing = std::collections::HashSet::new();
        let got = db
            .list_assistant_interactions_visible("m1", &nothing)
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].command, "Klaudku, sprawdź jaka była pogoda");
        assert!(got[0].answer.contains("słonecznie"));
        assert_eq!(
            got[0].citations,
            vec![
                "[[Notatka o pogodzie]]".to_string(),
                "(web) Weather — http://x".to_string()
            ]
        );
        assert_eq!(got[0].status, "ok");
        assert_eq!(got[0].source_label.as_deref(), Some("research"));
        assert_eq!(got[0].created_at, "2026-06-24T10:05:00Z");
    }

    /// GATE: a sealed-and-NOT-session-unlocked meeting's interactions are NEVER returned by the gated
    /// read (empty), even if a row exists at rest. RED-able: drop the `meeting_is_visible` guard in
    /// `list_assistant_interactions_visible` and this fails (the row leaks through the read).
    #[test]
    fn assistant_interactions_gated_for_sealed_meeting() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "note"); // gives the meeting a note in the folder
        db.set_note_folder("m1", Some("f-locked")).unwrap();
        db.insert_assistant_interaction(
            "m1",
            "secret command",
            "secret answer",
            &[],
            "ok",
            Some("research"),
            None,
            None,
            "2026-06-24T10:05:00Z",
        )
        .unwrap();
        // Seal the folder (visibility_clause keys off folders.locked); session-unlock set is empty.
        db.set_folder_locked("f-locked", true, Some(b"wrapped"))
            .unwrap();

        let empty = std::collections::HashSet::new();
        assert!(
            db.list_assistant_interactions_visible("m1", &empty)
                .unwrap()
                .is_empty(),
            "a sealed-not-unlocked meeting must surface NO interactions through the gated read"
        );
        // …and once the folder is session-unlocked, the row is visible again.
        let mut unlocked = std::collections::HashSet::new();
        unlocked.insert("f-locked".to_string());
        assert_eq!(
            db.list_assistant_interactions_visible("m1", &unlocked)
                .unwrap()
                .len(),
            1,
            "a session-unlocked folder's interactions ARE visible"
        );
    }

    /// PURGE-ON-SEAL: the interactions of a meeting in a folder are GONE after the seal purge tx
    /// (`purge_chunks_for_meetings`, which lock_folder runs). RED-able: drop the
    /// `purge_assistant_interactions_tx` call and the row survives at rest in a locked folder.
    #[test]
    fn assistant_interactions_purged_on_seal() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "note");
        db.set_note_folder("m1", Some("f-locked")).unwrap();
        db.insert_assistant_interaction(
            "m1",
            "cmd",
            "answer",
            &[],
            "ok",
            Some("research"),
            None,
            None,
            "2026-06-24T10:05:00Z",
        )
        .unwrap();
        assert_eq!(interaction_count(&db, "m1"), 1, "row present before seal");

        // The seal purge runs in the SAME tx that drops chunks + corrections for the sealed meetings.
        db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
        assert_eq!(
            interaction_count(&db, "m1"),
            0,
            "assistant_interactions must be purged on seal (Q&A log dropped on seal by design)"
        );
    }

    /// PURGE-ON-DELETE: deleting a meeting also removes its interactions (FK ON DELETE CASCADE).
    #[test]
    fn assistant_interactions_purged_on_delete_meeting() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        db.insert_assistant_interaction(
            "m1",
            "cmd",
            "answer",
            &[],
            "ok",
            Some("research"),
            None,
            None,
            "2026-06-24T10:05:00Z",
        )
        .unwrap();
        assert_eq!(interaction_count(&db, "m1"), 1);
        db.delete_meeting("m1").unwrap();
        assert_eq!(
            interaction_count(&db, "m1"),
            0,
            "delete_meeting must purge the meeting's assistant_interactions rows"
        );
    }

    // ── Brain v2 L4 — live_bullets: round-trip + purge on EVERY seal path (the L2 lesson) ──────

    fn bullets_row(db: &Db, meeting_id: &str) -> Option<String> {
        db.get_live_bullets(meeting_id).unwrap()
    }

    #[test]
    fn live_bullets_round_trip_upsert_get_clear() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
            .unwrap();
        assert_eq!(bullets_row(&db, "m1"), None, "no row before the first upsert");
        db.upsert_live_bullets("m1", "- [a]: one", "2026-07-10T10:01:00Z")
            .unwrap();
        assert_eq!(bullets_row(&db, "m1").as_deref(), Some("- [a]: one"));
        // Upsert REPLACES (one row per meeting — the RAM buffer is the accumulator).
        db.upsert_live_bullets("m1", "- [a]: one\n- [b]: two", "2026-07-10T10:02:00Z")
            .unwrap();
        assert_eq!(
            bullets_row(&db, "m1").as_deref(),
            Some("- [a]: one\n- [b]: two")
        );
        db.clear_live_bullets("m1").unwrap();
        assert_eq!(bullets_row(&db, "m1"), None, "cleared (Stop-time consume)");
        db.clear_live_bullets("m1").unwrap(); // idempotent
    }

    /// PURGE-ON-SEAL (RED-shaped: content seeded, seal, assert blank). Dropping the
    /// `purge_live_bullets_tx` call from `purge_chunks_for_meetings` leaves the row at rest in a
    /// locked folder and fails this.
    #[test]
    fn live_bullets_purged_on_seal() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "note");
        db.set_note_folder("m1", Some("f-locked")).unwrap();
        db.upsert_live_bullets("m1", "- [deal]: pricing agreed", "2026-07-10T10:05:00Z")
            .unwrap();
        assert!(bullets_row(&db, "m1").is_some(), "row present before seal");

        // The seal purge runs in the SAME tx that drops chunks + interactions for sealed meetings.
        db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
        assert_eq!(
            bullets_row(&db, "m1"),
            None,
            "live_bullets must be purged on seal (running notes are derived plaintext)"
        );
    }

    /// PURGE-ON-RELOCK: the relock re-blank tx (`blank_sealed_notes_in_folders`) drops the row too
    /// — a session-unlocked folder that recorded new bullets must not keep them at rest after
    /// relock.
    #[test]
    fn live_bullets_purged_on_relock_reblank() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "note");
        db.set_note_folder("m1", Some("f-locked")).unwrap();
        db.upsert_live_bullets("m1", "- [deal]: pricing agreed", "2026-07-10T10:05:00Z")
            .unwrap();

        let mut folders = HashSet::new();
        folders.insert("f-locked".to_string());
        db.blank_sealed_notes_in_folders(&folders).unwrap();
        assert_eq!(
            bullets_row(&db, "m1"),
            None,
            "relock must purge the live-bullets row"
        );
    }

    /// PURGE-AT-REST (startup reconcile): a crash mid-recording into a since-locked folder leaves
    /// the row behind — `reblank_locked_folders_at_rest` must drop it like the Q&A log.
    #[test]
    fn live_bullets_purged_by_at_rest_reconcile() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "note");
        db.set_note_folder("m1", Some("f-locked")).unwrap();
        db.set_folder_locked("f-locked", true, None).unwrap();
        db.upsert_live_bullets("m1", "- [deal]: pricing agreed", "2026-07-10T10:05:00Z")
            .unwrap();

        db.reblank_locked_folders_at_rest().unwrap();
        assert_eq!(
            bullets_row(&db, "m1"),
            None,
            "startup reconciliation must purge a locked folder's live-bullets row"
        );
    }

    /// TOCTOU (lock-security W2, 2026-07-10 — mirrors
    /// `topic_index_refuses_write_when_sealed_at_rest_mid_flight`): a `lock_folder` committing
    /// BETWEEN the worker's `current_meeting` check and the row upsert purged the row and sealed
    /// the note at rest (markdown blanked, `content_blob` kept) — the upsert's own in-tx
    /// sealed-at-rest re-check must then REFUSE to re-write plaintext bullets for the sealed
    /// meeting. RED before the fix: the plain upsert wrote the row unconditionally.
    #[test]
    fn live_bullets_upsert_refuses_when_sealed_at_rest_mid_flight() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "note");
        db.set_note_folder("m1", Some("f-locked")).unwrap();
        db.set_folder_locked("f-locked", true, None).unwrap();
        // "lock_folder committed mid-model-call": note sealed at rest — exactly what
        // `blank_sealed_notes_in_folders` leaves behind.
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE notes SET markdown = '', content_blob = X'00' WHERE meeting_id = 'm1'",
                [],
            )
            .unwrap();
        }
        db.upsert_live_bullets("m1", "- [deal]: pricing agreed", "2026-07-10T10:05:00Z")
            .unwrap();
        assert_eq!(
            bullets_row(&db, "m1"),
            None,
            "the in-tx sealed-at-rest re-check must refuse re-upserting plaintext bullets"
        );
    }

    /// PURGE-ON-DISCARD (lock-security W3, 2026-07-10): `discard_folder_seal` purges every other
    /// derived table — the live-bullets row must go with them (contract consistency; RED before
    /// `live_bullets` joined the discard purge list).
    #[test]
    fn live_bullets_purged_on_discard_folder_seal() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "note");
        db.set_note_folder("m1", Some("f-locked")).unwrap();
        db.upsert_live_bullets("m1", "- [deal]: pricing agreed", "2026-07-10T10:05:00Z")
            .unwrap();
        db.set_folder_locked("f-locked", true, None).unwrap();

        db.discard_folder_seal("f-locked").unwrap();
        assert_eq!(
            bullets_row(&db, "m1"),
            None,
            "discard_folder_seal must purge the live-bullets row like every other derived table"
        );
    }

    // ── Brain v2 L5 — PENDING `brief_runs` purge-on-seal (lock-security LEAK fix, 2026-07-10) ────

    /// Seed one `brief_runs` row directly. `note_md` carries a RECOGNIZABLE marker so the leak
    /// asserts can prove the synthesized content is gone, not just a row id.
    fn seed_brief_run(db: &Db, id: &str, status: &str, note_md: &str, meeting_ids: &[&str]) {
        db.insert_brief_run(&crate::storage::models::BriefRun {
            id: id.to_string(),
            schedule_id: "sched1".to_string(),
            status: status.to_string(),
            note_md: note_md.to_string(),
            meeting_ids: meeting_ids.iter().map(|s| s.to_string()).collect(),
            proposed_at: "2026-07-10T09:00:00Z".to_string(),
            accepted_at: None,
        })
        .unwrap();
    }

    /// Every `brief_runs` (id, note_md) AT REST — the raw table, not the pending-only listing, so
    /// a test asserts what actually survives a seal.
    fn brief_rows_at_rest(db: &Db) -> Vec<(String, String)> {
        let conn = db.lock();
        let mut stmt = conn
            .prepare("SELECT id, note_md FROM brief_runs ORDER BY id")
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    /// Seed the leak shape: meeting `m1` (in `f-locked`) + open meeting `m2`; a PENDING run whose
    /// synthesis references the soon-sealed `m1`, a PENDING run over only `m2`, and an ACCEPTED
    /// (consumed — `note_md` blanked on accept) run referencing `m1`.
    fn seed_brief_leak_shape(db: &Db) {
        seed_folder(db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
            .unwrap();
        note_for(db, "m1", "claude_code", "note");
        db.set_note_folder("m1", Some("f-locked")).unwrap();
        db.insert_meeting(&sample_meeting("m2", "2026-07-10T11:00:00Z"))
            .unwrap();
        seed_brief_run(db, "r-leak", "pending", "SYNTH-LEAK pricing agreed", &["m1", "m2"]);
        seed_brief_run(db, "r-other", "pending", "other-scope synthesis", &["m2"]);
        seed_brief_run(db, "r-done", "accepted", "", &["m1"]);
    }

    /// Assert the post-seal shape: the intersecting PENDING run (and its synthesized content) is
    /// GONE; the unrelated pending run and the accepted (consumed) run SURVIVE.
    fn assert_brief_leak_purged(db: &Db) {
        let rows = brief_rows_at_rest(db);
        assert!(
            !rows.iter().any(|(id, _)| id == "r-leak"),
            "the pending run referencing the sealed meeting must be purged: {rows:?}"
        );
        assert!(
            !rows.iter().any(|(_, md)| md.contains("SYNTH-LEAK")),
            "no synthesized content survives the seal: {rows:?}"
        );
        assert!(
            rows.iter().any(|(id, _)| id == "r-other"),
            "a pending run over other meetings survives: {rows:?}"
        );
        assert!(
            rows.iter().any(|(id, _)| id == "r-done"),
            "an accepted (consumed) run survives: {rows:?}"
        );
    }

    /// PURGE-ON-SEAL (RED-before-GREEN, the reproduced L5 leak): a PENDING brief run whose
    /// `meeting_ids` references a just-sealed meeting paraphrases that meeting's note — the seal
    /// tx (`purge_chunks_for_meetings`, which `lock_folder` runs) must delete it, exactly like the
    /// memory rollups it mirrors.
    #[test]
    fn pending_brief_runs_purged_on_seal_accepted_and_unrelated_survive() {
        let db = mem_db();
        seed_brief_leak_shape(&db);
        db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
        assert_brief_leak_purged(&db);
    }

    /// PURGE-ON-RELOCK: the relock re-blank tx (`blank_sealed_notes_in_folders`) drops the
    /// intersecting pending run too — a brief proposed during a session-unlock must not keep its
    /// synthesis at rest after relock.
    #[test]
    fn pending_brief_runs_purged_on_relock_reblank() {
        let db = mem_db();
        seed_brief_leak_shape(&db);
        let mut folders = HashSet::new();
        folders.insert("f-locked".to_string());
        db.blank_sealed_notes_in_folders(&folders).unwrap();
        assert_brief_leak_purged(&db);
    }

    /// PURGE-AT-REST (startup reconcile): a crash after the 09:00 schedule staged a brief over a
    /// since-locked folder leaves the row behind — `reblank_locked_folders_at_rest` must drop it
    /// like every other derived-plaintext table.
    #[test]
    fn pending_brief_runs_purged_by_at_rest_reconcile() {
        let db = mem_db();
        seed_brief_leak_shape(&db);
        db.set_folder_locked("f-locked", true, None).unwrap();
        db.reblank_locked_folders_at_rest().unwrap();
        assert_brief_leak_purged(&db);
    }

    /// PURGE-ON-DELETE: deleting a meeting drops any PENDING brief run referencing it (its
    /// `note_md` paraphrases the deleted note); the accepted/unrelated rows survive.
    #[test]
    fn pending_brief_runs_purged_on_delete_meeting() {
        let db = mem_db();
        seed_brief_leak_shape(&db);
        db.delete_meeting("m1").unwrap();
        assert_brief_leak_purged(&db);
    }

    // ── 2026-07-10 lock-audit F3/F5: folder-delete purge + the relock-tx derived families ─────────

    /// Raw at-rest scalar (COUNT) — what actually survives, independent of any gated read.
    fn count_raw(db: &Db, sql: &str) -> i64 {
        let conn = db.lock();
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// F3 (RED-before-GREEN): deleting a folder must purge its documents' `doc_chunks` +
    /// `doc_vec_chunks` + `fts_doc_chunks` tokens IN the delete tx. Before the fix `delete_folder`
    /// was a bare `DELETE FROM folders`: the `documents`→`doc_chunks` FK CASCADE does NOT fire the
    /// `fts_doc_chunks_ad` trigger (cascade actions skip triggers without `recursive_triggers`), and
    /// `doc_vec_chunks` is a FK-less vec0 table — leaving searchable tokens + invertible vectors of
    /// the deleted content at rest.
    #[test]
    fn delete_folder_purges_doc_vec_chunks_and_fts() {
        let db = mem_db();
        seed_folder(&db, "f-docs", "Research");
        db.insert_document("d1", "f-docs", "spec.md", "unicornbudget approved", "document", 1)
            .unwrap();
        db.index_document_chunks("d1", None).unwrap(); // chunks + FTS (model-less: no vectors)
        // Attach a vector to the chunk directly (no embedder on CI) — the orphan the fix must purge.
        {
            let conn = db.lock();
            let chunk_id: i64 = conn
                .query_row(
                    "SELECT id FROM doc_chunks WHERE document_id = 'd1' LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let blob = crate::embed::vec_to_blob(&one_hot(0));
            conn.execute(
                "INSERT INTO doc_vec_chunks(chunk_id, embedding) VALUES (?1, ?2)",
                rusqlite::params![chunk_id, blob],
            )
            .unwrap();
        }

        db.delete_folder("f-docs").unwrap();

        assert_eq!(
            count_raw(&db, "SELECT COUNT(*) FROM documents WHERE folder_id = 'f-docs'"),
            0,
            "documents gone with the folder"
        );
        assert_eq!(
            count_raw(&db, "SELECT COUNT(*) FROM doc_chunks WHERE document_id = 'd1'"),
            0,
            "doc_chunks purged with the folder"
        );
        assert_eq!(
            count_raw(&db, "SELECT COUNT(*) FROM doc_vec_chunks"),
            0,
            "no orphan doc vector survives the folder delete (FK-less vec0)"
        );
        assert_eq!(
            count_raw(
                &db,
                "SELECT COUNT(*) FROM fts_doc_chunks WHERE fts_doc_chunks MATCH 'unicornbudget'"
            ),
            0,
            "no searchable FTS token of the deleted content survives (cascade skips the _ad trigger)"
        );
    }

    /// F5 (RED-before-GREEN): the RELOCK tx (`blank_sealed_notes_in_folders`, both the
    /// `relock_folder` and `relock_all_inner` legs) must purge the four derived-content families the
    /// LOCK tx (`purge_chunks_for_meetings`) and the STARTUP reconcile
    /// (`reblank_locked_folders_at_rest`) already purge: `facts`, `user_facts`,
    /// `assistant_interactions`, `speaker_voiceprints`. Before the fix, rows derived DURING a
    /// session unlock survived the relock at rest.
    #[test]
    fn relock_purges_facts_user_facts_interactions_and_voiceprints() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "note");
        db.set_note_folder("m1", Some("f-locked")).unwrap();

        // Rows re-derived while the folder was session-unlocked (the relock must drop them).
        let anna = db.upsert_entity("Anna", EntityKind::Person).unwrap();
        db.apply_fact_ops(&[crate::facts::FactOp::Add(crate::facts::NewFact {
            entity_id: anna.clone(),
            subject: "Anna".into(),
            predicate: "role".into(),
            object: "QA lead".into(),
            valid_from: "2026-07-10T10:00:00Z".into(),
            recorded_at: "2026-07-10T10:00:00Z".into(),
            confidence: 1.0,
            meeting_id: Some("m1".into()),
        })])
        .unwrap();
        db.apply_user_fact_ops(&[crate::facts::FactOp::Add(crate::facts::NewFact {
            entity_id: "user".into(),
            subject: "user".into(),
            predicate: "prefers".into(),
            object: "Polish replies".into(),
            valid_from: "2026-07-10T10:00:00Z".into(),
            recorded_at: "2026-07-10T10:00:00Z".into(),
            confidence: 1.0,
            meeting_id: Some("m1".into()),
        })])
        .unwrap();
        db.insert_assistant_interaction(
            "m1",
            "what did Anna say",
            "she owns QA sign-off",
            &[],
            "answered",
            None,
            None,
            None,
            "2026-07-10T10:05:00Z",
        )
        .unwrap();
        db.insert_voiceprint("vp1", "m1", 0, Some("Anna"), &[1.0, 0.0, 0.0], "2026-07-10T10:06:00Z")
            .unwrap();

        // RELOCK tx.
        let mut folders = HashSet::new();
        folders.insert("f-locked".to_string());
        db.blank_sealed_notes_in_folders(&folders).unwrap();

        assert_eq!(
            count_raw(&db, "SELECT COUNT(*) FROM facts WHERE meeting_id = 'm1'"),
            0,
            "relock must purge the sealed meeting's facts"
        );
        assert_eq!(
            count_raw(&db, "SELECT COUNT(*) FROM user_facts WHERE meeting_id = 'm1'"),
            0,
            "relock must purge the sealed meeting's user facts"
        );
        assert_eq!(
            count_raw(
                &db,
                "SELECT COUNT(*) FROM assistant_interactions WHERE meeting_id = 'm1'"
            ),
            0,
            "relock must purge the sealed meeting's Q&A log"
        );
        assert_eq!(
            count_raw(
                &db,
                "SELECT COUNT(*) FROM speaker_voiceprints WHERE meeting_id = 'm1'"
            ),
            0,
            "relock must purge the sealed meeting's voiceprints"
        );
    }

    /// PURGE-ON-DELETE: deleting a meeting removes its live-bullets row (explicit purge + FK).
    #[test]
    fn live_bullets_purged_on_delete_meeting() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-07-10T10:00:00Z"))
            .unwrap();
        db.upsert_live_bullets("m1", "- [deal]: pricing agreed", "2026-07-10T10:05:00Z")
            .unwrap();
        db.delete_meeting("m1").unwrap();
        assert_eq!(
            bullets_row(&db, "m1"),
            None,
            "delete_meeting must purge the live-bullets row"
        );
    }

    /// THREAD round-trip: exchanges persisted WITH a thread identity read back through the gated
    /// thread reader — `thread_id` + `anchor_text` intact, `command` = the user's LATEST message
    /// of each exchange, ordered by id ASC — while legacy rows (NULL `thread_id`) are EXCLUDED.
    /// RED before PR D: `insert_assistant_interaction` had no thread params and
    /// `list_assistant_threads_visible` did not exist (thread identity was FE-RAM-only).
    #[test]
    fn assistant_threads_round_trip_ordered_and_exclude_legacy() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-07-02T10:00:00Z"))
            .unwrap();
        // A legacy-shaped voice row (no thread) — must NOT surface in the thread reader.
        db.insert_assistant_interaction(
            "m1",
            "legacy voice cmd",
            "a0",
            &[],
            "ok",
            Some("research"),
            None,
            None,
            "2026-07-02T10:01:00Z",
        )
        .unwrap();
        // An @brain thread: two exchanges, the first anchored to a note.
        db.insert_assistant_interaction(
            "m1",
            "what did we decide on pricing?",
            "Tiered pricing.",
            &["[[Pricing sync]]".to_string()],
            "ok",
            Some("research"),
            Some("t-1"),
            Some("• pricing: tiered, ship Friday"),
            "2026-07-02T10:02:00Z",
        )
        .unwrap();
        db.insert_assistant_interaction(
            "m1",
            "and the timeline?",
            "Ships Friday.",
            &[],
            "ok",
            Some("research"),
            Some("t-1"),
            None,
            "2026-07-02T10:03:00Z",
        )
        .unwrap();

        let rows = db
            .list_assistant_threads_visible("m1", &std::collections::HashSet::new())
            .unwrap();
        assert_eq!(
            rows.len(),
            2,
            "only thread-carrying rows; the legacy NULL row is excluded"
        );
        assert_eq!(rows[0].thread_id, "t-1");
        assert_eq!(
            rows[0].anchor_text.as_deref(),
            Some("• pricing: tiered, ship Friday")
        );
        assert_eq!(
            rows[0].command, "what did we decide on pricing?",
            "command is the LATEST user message of the exchange, never the rendered history"
        );
        assert_eq!(rows[0].answer, "Tiered pricing.");
        assert_eq!(rows[0].citations, vec!["[[Pricing sync]]".to_string()]);
        assert_eq!(rows[0].status, "ok");
        assert_eq!(rows[0].created_at, "2026-07-02T10:02:00Z");
        // Ordered by id ASC — the follow-up comes second.
        assert_eq!(rows[1].command, "and the timeline?");
        assert_eq!(rows[1].thread_id, "t-1");
        assert!(rows[1].anchor_text.is_none());
    }

    /// GATE: a sealed-and-NOT-session-unlocked meeting's threads are NEVER returned by the gated
    /// thread reader (EMPTY, never an error) — even with rows at rest; a session-unlocked folder's
    /// threads ARE. RED-able: drop the `meeting_is_visible` guard in
    /// `list_assistant_threads_visible` and the first assertion fails (the row leaks).
    #[test]
    fn assistant_threads_gated_for_sealed_meeting() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("m1", "2026-07-02T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "note");
        db.set_note_folder("m1", Some("f-locked")).unwrap();
        db.insert_assistant_interaction(
            "m1",
            "secret thread question",
            "secret answer",
            &[],
            "ok",
            Some("research"),
            Some("t-secret"),
            Some("secret anchor"),
            "2026-07-02T10:05:00Z",
        )
        .unwrap();
        db.set_folder_locked("f-locked", true, Some(b"wrapped"))
            .unwrap();

        let empty = std::collections::HashSet::new();
        assert!(
            db.list_assistant_threads_visible("m1", &empty)
                .unwrap()
                .is_empty(),
            "a sealed-not-unlocked meeting must surface NO threads through the gated read"
        );
        let mut unlocked = std::collections::HashSet::new();
        unlocked.insert("f-locked".to_string());
        assert_eq!(
            db.list_assistant_threads_visible("m1", &unlocked)
                .unwrap()
                .len(),
            1,
            "a session-unlocked folder's threads ARE visible"
        );
    }

    /// PURGE-ON-SEAL: thread rows ride `purge_assistant_interactions_tx` (no new purge code) — after
    /// the seal purge the raw rows are GONE and the thread reader returns EMPTY even for a session
    /// that can see the meeting. RED-able: exclude thread-carrying rows from the purge DELETE and
    /// both assertions fail (thread content would survive a seal at rest).
    #[test]
    fn assistant_threads_purged_on_seal() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("m1", "2026-07-02T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "note");
        db.set_note_folder("m1", Some("f-locked")).unwrap();
        db.insert_assistant_interaction(
            "m1",
            "thread cmd",
            "thread answer",
            &[],
            "ok",
            Some("research"),
            Some("t-1"),
            Some("anchor"),
            "2026-07-02T10:05:00Z",
        )
        .unwrap();
        assert_eq!(interaction_count(&db, "m1"), 1, "row present before seal");

        // The seal purge runs in the SAME tx that drops chunks + corrections for sealed meetings.
        db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
        assert_eq!(
            interaction_count(&db, "m1"),
            0,
            "raw thread rows gone after the seal purge"
        );
        let mut unlocked = std::collections::HashSet::new();
        unlocked.insert("f-locked".to_string());
        assert!(
            db.list_assistant_threads_visible("m1", &unlocked)
                .unwrap()
                .is_empty(),
            "the thread reader has nothing to return after the purge"
        );
    }

    /// Phase 4 durable thread→meeting binding: a thread row persisted against a PAST meeting
    /// (the resolved FE-bound scope, `fe_id.or(current_meeting)` in `run_assistant_query`) is
    /// retrievable under THAT meeting and is INVISIBLE under a DIFFERENT (e.g. currently-recording)
    /// meeting — proving a bound thread durably answers about its own meeting, not whatever is
    /// recording. The binding needs NO schema change: it rides the existing `meeting_id` column
    /// (the resolved scope is what `persist_interaction` stores). RED if `run_assistant_query` had
    /// kept binding to `current_meeting` while the FE viewed a past meeting.
    #[test]
    fn thread_binds_to_its_own_meeting_not_a_different_recording() {
        let db = mem_db();
        // The PAST meeting the FE thread is bound to, and a DIFFERENT meeting that is "recording".
        db.insert_meeting(&sample_meeting("m-past", "2026-07-01T09:00:00Z"))
            .unwrap();
        db.insert_meeting(&sample_meeting("m-recording", "2026-07-06T09:00:00Z"))
            .unwrap();
        // Persist the exchange against the RESOLVED scope (the FE-bound past meeting) — this is what
        // `run_assistant_query` now does with
        // `resolve_scope_meeting(Some("m-past"), None, Some("m-recording"))` (Phase 6: fe > focus > recording).
        db.insert_assistant_interaction(
            "m-past",
            "o czym to spotkanie",
            "It was the Q3 budget review.",
            &["[[Budget review]]".to_string()],
            "ok",
            Some("research"),
            Some("t-past"),
            Some("budget review"),
            "2026-07-06T09:05:00Z",
        )
        .unwrap();

        let nothing = std::collections::HashSet::new();
        // The bound past meeting owns the thread.
        let past = db
            .list_assistant_threads_visible("m-past", &nothing)
            .unwrap();
        assert_eq!(
            past.len(),
            1,
            "the bound past meeting must own its thread row"
        );
        assert_eq!(past[0].thread_id, "t-past");
        // The recording meeting must NOT see it — the binding is durable to the past meeting.
        assert!(
            db.list_assistant_threads_visible("m-recording", &nothing)
                .unwrap()
                .is_empty(),
            "a different (recording) meeting must NOT surface the bound thread"
        );
    }

    /// PR D migration: `assistant_interactions` carries the THREAD columns (`thread_id` +
    /// `anchor_text`). RED before the guarded ALTERs land ("no such column"), GREEN after;
    /// `migrate_is_idempotent` covers the re-run. Legacy rows read back NULL/NULL.
    #[test]
    fn assistant_interactions_have_thread_columns() {
        let db = mem_db();
        let n: i64 = db
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM assistant_interactions
                  WHERE thread_id IS NOT NULL OR anchor_text IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            n, 0,
            "fresh table: no thread rows yet, but the columns must exist"
        );
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
    fn reconcile_stuck_recordings_flips_recording_to_error_and_is_idempotent() {
        let db = mem_db();

        // A ghost: inserted RECORDING and never stopped (crash mid-capture).
        db.insert_meeting(&sample_meeting("ghost", "2026-07-04T09:00:00Z"))
            .unwrap();
        db.update_meeting_status("ghost", MeetingStatus::Recording)
            .unwrap();

        // A healthy, finished meeting that must be left alone.
        db.insert_meeting(&sample_meeting("done", "2026-07-04T10:00:00Z"))
            .unwrap();
        db.update_meeting_status("done", MeetingStatus::Summarized)
            .unwrap();

        // An already-terminal ERROR row must NOT be re-counted (guards against a broad WHERE).
        db.insert_meeting(&sample_meeting("failed", "2026-07-04T11:00:00Z"))
            .unwrap();
        db.update_meeting_status("failed", MeetingStatus::Error)
            .unwrap();

        // First reconcile flips exactly the one stuck RECORDING row to ERROR.
        assert_eq!(db.reconcile_stuck_recordings().unwrap(), 1);
        assert_eq!(
            db.get_meeting("ghost").unwrap().unwrap().status,
            MeetingStatus::Error
        );
        // Non-recording rows are untouched.
        assert_eq!(
            db.get_meeting("done").unwrap().unwrap().status,
            MeetingStatus::Summarized
        );
        assert_eq!(
            db.get_meeting("failed").unwrap().unwrap().status,
            MeetingStatus::Error
        );

        // Idempotent: with no live recording a second call reconciles nothing.
        assert_eq!(db.reconcile_stuck_recordings().unwrap(), 0);
        assert_eq!(
            db.get_meeting("ghost").unwrap().unwrap().status,
            MeetingStatus::Error
        );
    }

    /// The load-bearing skip invariant the STAGE-2 crash-salvage ordering depends on:
    /// `reconcile_stuck_recordings_except(&[claimed])` must LEAVE the claimed ghost in RECORDING
    /// (salvage owns its final status), while every OTHER stuck RECORDING ghost still flips to ERROR.
    /// Without this skip, reconcile would clobber a claimed row to ERROR in the window before the async
    /// salvage worker transitions it — corrupting the salvage.
    #[test]
    fn reconcile_stuck_recordings_except_leaves_the_claimed_row_recording() {
        let db = mem_db();

        // Two crash ghosts, both stuck in RECORDING.
        db.insert_meeting(&sample_meeting("claimed", "2026-07-04T09:00:00Z"))
            .unwrap();
        db.update_meeting_status("claimed", MeetingStatus::Recording)
            .unwrap();
        db.insert_meeting(&sample_meeting("unclaimed", "2026-07-04T09:05:00Z"))
            .unwrap();
        db.update_meeting_status("unclaimed", MeetingStatus::Recording)
            .unwrap();

        // Reconcile, EXCEPTing the salvage-claimed id: exactly the one un-claimed ghost flips.
        let claimed = vec!["claimed".to_string()];
        assert_eq!(db.reconcile_stuck_recordings_except(&claimed).unwrap(), 1);

        // The claimed row is SKIPPED — still RECORDING, so the async salvage worker owns its fate.
        assert_eq!(
            db.get_meeting("claimed").unwrap().unwrap().status,
            MeetingStatus::Recording,
            "the claimed ghost must stay RECORDING for salvage to own its final status"
        );
        // The un-claimed ghost is reconciled to the terminal ERROR exactly as before.
        assert_eq!(
            db.get_meeting("unclaimed").unwrap().unwrap().status,
            MeetingStatus::Error
        );

        // Idempotent: re-running with the same exclusion reconciles nothing (the claimed row is
        // still RECORDING but skipped, the unclaimed one is already ERROR).
        assert_eq!(db.reconcile_stuck_recordings_except(&claimed).unwrap(), 0);
        assert_eq!(
            db.get_meeting("claimed").unwrap().unwrap().status,
            MeetingStatus::Recording
        );
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
                confidence: None,
            },
            Segment {
                idx: 1,
                start_s: 1.5,
                end_s: 3.0,
                text: "world".into(),
                speaker: None,
                confidence: None,
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

    /// Tier 3b/A: `segments.confidence` persists through `insert_segments` → `get_segments`. A stored
    /// `Some(0.42)` round-trips (within f32 epsilon), a `None` writes NULL and reads back `None`, and
    /// the additive column never disturbs the other fields.
    #[test]
    fn segments_confidence_round_trips() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("mc", "2026-07-03T10:00:00Z"))
            .unwrap();
        let segs = vec![
            Segment {
                idx: 0,
                start_s: 0.0,
                end_s: 1.0,
                text: "clear speech".into(),
                speaker: Some("me".into()),
                confidence: Some(0.42),
            },
            Segment {
                idx: 1,
                start_s: 1.0,
                end_s: 2.0,
                text: "unknown".into(),
                speaker: Some("others".into()),
                confidence: None,
            },
        ];
        db.insert_segments("mc", &segs).unwrap();
        let read = db.get_segments("mc").unwrap();
        assert_eq!(read.len(), 2);
        let c0 = read[0].confidence.expect("Some(0.42) must round-trip");
        assert!((c0 - 0.42).abs() < 1e-6, "confidence drifted: {c0}");
        assert_eq!(
            read[1].confidence, None,
            "NULL confidence must read back as None"
        );
        // The additive column did not perturb the other fields.
        assert_eq!(read[0].text, "clear speech");
        assert_eq!(read[1].speaker.as_deref(), Some("others"));
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
            model_requested: None,
            model_served: None,
            gateway_host: None,
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

    /// Phase 5 — provenance round-trip: model_requested / model_served / gateway_host are
    /// persisted via `upsert_note` and read back correctly by every note-fetch path.
    #[test]
    fn note_provenance_round_trips() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("prov1", "2026-06-30T10:00:00Z"))
            .unwrap();

        let note = NoteRecord {
            meeting_id: "prov1".into(),
            provider_id: "gateway".into(),
            markdown: "---\ntitle: T\n---\nBody.".into(),
            created_at: "2026-06-30T10:05:00Z".into(),
            exported_path: None,
            model_requested: Some("gpt-4o".into()),
            model_served: Some("gpt-4o-2024-11-20".into()),
            gateway_host: Some("gw.example.com".into()),
        };
        db.upsert_note(&note).unwrap();

        let got = db.get_note("prov1", "gateway").unwrap().unwrap();
        assert_eq!(got.model_requested.as_deref(), Some("gpt-4o"));
        assert_eq!(got.model_served.as_deref(), Some("gpt-4o-2024-11-20"));
        assert_eq!(got.gateway_host.as_deref(), Some("gw.example.com"));

        let got2 = db.get_latest_note_for_meeting("prov1").unwrap().unwrap();
        assert_eq!(got2.model_served.as_deref(), Some("gpt-4o-2024-11-20"));
    }

    /// Phase 5 — legacy notes (columns NULL) read back as `None` provenance fields.
    #[test]
    fn note_provenance_legacy_rows_read_as_none() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("leg1", "2026-06-30T10:00:00Z"))
            .unwrap();
        // A note with no provenance (as all pre-Phase-5 notes would be after migration).
        db.upsert_note(&NoteRecord {
            meeting_id: "leg1".into(),
            provider_id: "claude_code".into(),
            markdown: "# Legacy note".into(),
            created_at: "2026-06-30T10:01:00Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        let got = db.get_note("leg1", "claude_code").unwrap().unwrap();
        assert!(
            got.model_requested.is_none(),
            "legacy: model_requested is None"
        );
        assert!(got.model_served.is_none(), "legacy: model_served is None");
        assert!(got.gateway_host.is_none(), "legacy: gateway_host is None");
    }

    /// Phase 5 — `migrate()` twice is still a no-op (idempotent migration covering the new columns).
    /// The existing `migrate_is_idempotent` test covers the full table list; this one checks
    /// specifically that the three provenance columns are present after `migrate()` runs once (they
    /// would be absent in a real pre-Phase-5 DB and must be added by the first migrate).
    #[test]
    fn migrate_adds_provenance_columns_to_notes() {
        let db = mem_db(); // mem_db() calls migrate() once during open_with_key
        let conn = db.lock();
        // PRAGMA table_info returns one row per column; verify all three are present.
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(notes)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        for col in &["model_requested", "model_served", "gateway_host"] {
            assert!(
                cols.iter().any(|c| c == col),
                "column {col} must be present after migrate(); columns: {cols:?}"
            );
        }
    }

    #[test]
    fn settings_kv_round_trip() {
        let db = mem_db();
        assert!(db.get_setting("vault_path").unwrap().is_none());
        db.set_setting("vault_path", "/vault").unwrap();
        db.set_setting("provider_id", "claude_code").unwrap();
        // overwrite
        db.set_setting("vault_path", "/vault2").unwrap();
        assert_eq!(
            db.get_setting("vault_path").unwrap().as_deref(),
            Some("/vault2")
        );

        // Only the keys THIS test set — migrate() also seeds the Tier 1 semantic-default keys
        // (semantic_search_enabled / semantic_default_v1), which are not this KV test's concern.
        let all: Vec<(String, String)> = db
            .all_settings()
            .unwrap()
            .into_iter()
            .filter(|(k, _)| k == "provider_id" || k == "vault_path")
            .collect();
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
            model_requested: None,
            model_served: None,
            gateway_host: None,
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
        assert_eq!(
            folders,
            vec![Some("f1".to_string()), Some("f1".to_string())]
        );

        // list_meetings reports a single consistent folder_id (LIMIT 1 subselect, no dup rows).
        let listed = db.list_meetings(50).unwrap();
        let m1_rows: Vec<&Meeting> = listed.iter().filter(|m| m.id == "m1").collect();
        assert_eq!(
            m1_rows.len(),
            1,
            "one meeting row despite two provider notes"
        );
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
        note_for(
            &db,
            "m1",
            "claude_code",
            "the alpha and the beta of the plan",
        );

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
        for q in [
            "",
            "   ",
            "\"",
            "*",
            "AND OR NOT",
            "(",
            ":",
            "^foo",
            "a* b\"c(",
        ] {
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
        note_for(
            &db,
            "sealed",
            "claude_code",
            "ACQUISITION zarządzanie tajemnica",
        );
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
        db.set_folder_locked("f-locked", true, Some(&wrapped))
            .unwrap();
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

    // ── Phase 2a: vector retrieval (note_chunks + vec0) ───────────────────────

    /// Insert a controlled `note_chunks` + `vec_chunks` pair directly (bypassing the embedder) so
    /// ordering tests can use known vectors. Returns the new chunk_id.
    fn insert_known_chunk(db: &Db, meeting_id: &str, text: &str, vector: &[f32]) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO note_chunks (meeting_id, provider_id, chunk_idx, source_type, text)
             VALUES (?1, 'claude_code', 0, 'voice', ?2)",
            rusqlite::params![meeting_id, text],
        )
        .unwrap();
        let chunk_id = conn.last_insert_rowid();
        let blob = crate::embed::vec_to_blob(vector);
        conn.execute(
            "INSERT INTO vec_chunks(chunk_id, embedding) VALUES (?1, ?2)",
            rusqlite::params![chunk_id, blob],
        )
        .unwrap();
        chunk_id
    }

    /// A one-hot EMBED_DIM vector with `1.0` at `dim` (controlled distinct directions for KNN).
    fn one_hot(dim: usize) -> Vec<f32> {
        let mut v = vec![0f32; crate::embed::EMBED_DIM];
        v[dim] = 1.0;
        v
    }

    /// KNN ordering: the chunk whose vector equals the query is nearest; a near-aligned one is
    /// next; an orthogonal one is farthest.
    #[test]
    fn vec_knn_orders_nearest_first() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m-near", "2026-06-24T10:00:00Z"))
            .unwrap();
        db.insert_meeting(&sample_meeting("m-mid", "2026-06-24T11:00:00Z"))
            .unwrap();
        db.insert_meeting(&sample_meeting("m-far", "2026-06-24T12:00:00Z"))
            .unwrap();
        // Every meeting needs a (visible, open-folder) note so the gate admits it.
        note_for(&db, "m-near", "claude_code", "near");
        note_for(&db, "m-mid", "claude_code", "mid");
        note_for(&db, "m-far", "claude_code", "far");

        // query == one_hot(0). near = one_hot(0) (identical), mid = mix of dim 0+1, far = one_hot(2).
        let query = one_hot(0);
        insert_known_chunk(&db, "m-near", "near", &one_hot(0));
        let mut mid = vec![0f32; crate::embed::EMBED_DIM];
        mid[0] = 0.9;
        mid[1] = 0.1;
        insert_known_chunk(&db, "m-mid", "mid", &mid);
        insert_known_chunk(&db, "m-far", "far", &one_hot(2));

        let nothing = std::collections::HashSet::new();
        let hits = db.search_semantic_visible(&query, 3, &nothing).unwrap();
        let order: Vec<&str> = hits.iter().map(|h| h.meeting.id.as_str()).collect();
        assert_eq!(
            order,
            vec!["m-near", "m-mid", "m-far"],
            "KNN must return nearest-first"
        );
        assert!(hits.iter().all(|h| h.matched_in == "semantic"));
    }

    /// GATE: a sealed-and-not-session-unlocked meeting is ABSENT from semantic results with an
    /// empty unlock set, and PRESENT when its folder id is in the set. (Mirrors the FTS gate test;
    /// here the chunk row deliberately still EXISTS so exclusion comes from the gate, not purge.)
    #[test]
    fn vec_semantic_search_is_gated_by_visibility() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("sealed", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "sealed", "claude_code", "secret body");
        db.set_note_folder("sealed", Some("f-locked")).unwrap();
        insert_known_chunk(&db, "sealed", "secret body", &one_hot(0));
        // Flip the folder to locked=1 (visibility_clause keys off folders.locked). The chunk row
        // still exists — so exclusion can ONLY come from the gate here.
        db.set_folder_locked("f-locked", true, None).unwrap();

        let query = one_hot(0);
        // Empty unlock set → excluded.
        let nothing = std::collections::HashSet::new();
        let hidden = db.search_semantic_visible(&query, 10, &nothing).unwrap();
        assert!(
            !hidden.iter().any(|h| h.meeting.id == "sealed"),
            "sealed-not-unlocked meeting leaked through the semantic gate"
        );
        // Folder session-unlocked → present.
        let mut unlocked = std::collections::HashSet::new();
        unlocked.insert("f-locked".to_string());
        let shown = db.search_semantic_visible(&query, 10, &unlocked).unwrap();
        assert!(
            shown.iter().any(|h| h.meeting.id == "sealed"),
            "session-unlocked meeting must reappear in semantic results"
        );
    }

    /// GATE (hybrid): the fused FTS+semantic+graph reader also excludes a sealed-not-session-unlocked
    /// meeting — with BOTH the FTS query term AND a query vector that match its (deliberately-surviving)
    /// chunk, so exclusion can ONLY come from the shared `visibility_clause`, not purge. Companion to
    /// `vec_semantic_search_is_gated_by_visibility`; asserts the unlock re-index change did not weaken
    /// the hybrid gate. RED if the gate inside `search_semantic_visible`/`search_visible` were removed.
    #[test]
    fn vec_hybrid_search_is_gated_by_visibility() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("sealed", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "sealed", "claude_code", "quarterly budget secret body");
        db.set_note_folder("sealed", Some("f-locked")).unwrap();
        insert_known_chunk(&db, "sealed", "quarterly budget secret body", &one_hot(0));
        db.set_folder_locked("f-locked", true, None).unwrap();

        let query_vec = one_hot(0);
        // Empty unlock set → the sealed meeting is absent through the whole fused reader.
        let nothing = std::collections::HashSet::new();
        let hidden = db
            .search_hybrid_visible("budget", &query_vec, 10, &nothing, None)
            .unwrap();
        assert!(
            !hidden.iter().any(|h| h.meeting.id == "sealed"),
            "sealed-not-unlocked meeting leaked through the hybrid gate"
        );

        // Session-unlock → it reappears in the fused results.
        let mut unlocked = std::collections::HashSet::new();
        unlocked.insert("f-locked".to_string());
        let shown = db
            .search_hybrid_visible("budget", &query_vec, 10, &unlocked, None)
            .unwrap();
        assert!(
            shown.iter().any(|h| h.meeting.id == "sealed"),
            "session-unlocked meeting must reappear in hybrid results"
        );
    }

    // ── document ingestion (documents + doc_chunks + doc_vec0) ────────────────

    /// IMPORT/INDEX round-trip at the DB layer: an inserted document chunks + embeds (via the stub so
    /// vectors are deterministic), the metadata read returns it (no text), and the full-text read
    /// returns the plaintext. Purging the doc removes its chunks AND vectors (1:1).
    #[test]
    fn document_index_and_purge_round_trip() {
        let db = mem_db();
        seed_folder(&db, "f-open", "Project");
        db.insert_document(
            "d1",
            "f-open",
            "spec.md",
            "budget planning for the quarter",
            "document",
            100,
        )
        .unwrap();
        db.index_document_chunks("d1", Some(&crate::embed::StubEmbedder))
            .unwrap();

        // Metadata (no text) + full text read back.
        let listed = db.documents_in_folder("f-open").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "d1");
        assert_eq!(listed[0].name, "spec.md");
        let (folder, name, text) = db.get_document("d1").unwrap().unwrap();
        assert_eq!(folder, "f-open");
        assert_eq!(name, "spec.md");
        assert_eq!(text, "budget planning for the quarter");

        // Chunks + 1:1 vectors exist.
        let count = |sql: &str| -> i64 { db.lock().query_row(sql, [], |r| r.get(0)).unwrap() };
        let chunks = count("SELECT COUNT(*) FROM doc_chunks WHERE document_id = 'd1'");
        let vecs = count(
            "SELECT COUNT(*) FROM doc_vec_chunks WHERE chunk_id IN \
               (SELECT id FROM doc_chunks WHERE document_id = 'd1')",
        );
        assert!(chunks >= 1, "document must be chunked");
        assert_eq!(chunks, vecs, "doc_vec_chunks is 1:1 with doc_chunks");

        // Purge drops BOTH chunks and vectors.
        db.purge_doc_chunks_for_documents(&["d1".to_string()])
            .unwrap();
        assert_eq!(
            count("SELECT COUNT(*) FROM doc_chunks WHERE document_id = 'd1'"),
            0
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM doc_vec_chunks"),
            0,
            "vectors purged with chunks"
        );
        // The document row + its plaintext survive the chunk purge (re-embeddable).
        assert_eq!(
            db.get_document("d1").unwrap().unwrap().2,
            "budget planning for the quarter"
        );
    }

    /// GATE: a document in a sealed-and-not-session-unlocked folder is ABSENT from
    /// `search_doc_chunks_visible` with an empty unlock set, and PRESENT when its folder is in the
    /// set. The doc-chunk row deliberately STILL EXISTS, so exclusion can ONLY come from the gate —
    /// RED if the `visibility_clause` inside `search_doc_chunks_visible` were removed.
    #[test]
    fn doc_chunk_search_is_gated_by_visibility() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_document(
            "d1",
            "f-locked",
            "secret.md",
            "launch date is the 14th",
            "document",
            100,
        )
        .unwrap();
        db.index_document_chunks("d1", Some(&crate::embed::StubEmbedder))
            .unwrap();
        // Query vector = the stub passage embedding of the doc's own chunk text → guaranteed nearest.
        let chunk_text: String = db
            .lock()
            .query_row(
                "SELECT text FROM doc_chunks WHERE document_id = 'd1' ORDER BY chunk_index LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let query = crate::embed::StubEmbedder
            .embed_passage(std::slice::from_ref(&chunk_text))
            .unwrap()
            .into_iter()
            .next()
            .unwrap();

        // Open folder → visible.
        let nothing = std::collections::HashSet::new();
        assert!(
            db.search_doc_chunks_visible(&query, 10, &nothing)
                .unwrap()
                .iter()
                .any(|h| h.document_id == "d1"),
            "open-folder document chunk must be visible to search"
        );

        // Seal the folder (chunk row deliberately survives) → INVISIBLE with empty unlock set.
        db.set_folder_locked("f-locked", true, None).unwrap();
        let hidden = db.search_doc_chunks_visible(&query, 10, &nothing).unwrap();
        assert!(
            !hidden.iter().any(|h| h.document_id == "d1"),
            "sealed-not-unlocked document chunk leaked through the gate"
        );

        // Session-unlock → present again.
        let mut unlocked = std::collections::HashSet::new();
        unlocked.insert("f-locked".to_string());
        let shown = db.search_doc_chunks_visible(&query, 10, &unlocked).unwrap();
        assert!(
            shown.iter().any(|h| h.document_id == "d1"),
            "session-unlocked document chunk must reappear in search"
        );
    }

    /// ALWAYS-CHUNK contract (PR B): indexing with NO embedder (`None` — the model-less default
    /// install) stores `doc_chunks` rows and ZERO vectors, and the chunk text is immediately
    /// keyword-findable via the gated FTS leg. Deterministic regardless of whether the dev machine
    /// has the real e5 model.
    #[test]
    fn index_document_chunks_none_embedder_chunks_no_vectors_fts_finds() {
        let db = mem_db();
        seed_folder(&db, "f-open", "Project");
        db.insert_document(
            "d1",
            "f-open",
            "spec.md",
            "the pistachio launch is in March",
            "document",
            100,
        )
        .unwrap();
        db.index_document_chunks("d1", None).unwrap();

        assert!(
            db.doc_chunk_count("d1").unwrap() >= 1,
            "chunk rows stored without a model"
        );
        assert_eq!(
            db.doc_vec_count("d1").unwrap(),
            0,
            "no vectors written without a model"
        );

        let nothing = std::collections::HashSet::new();
        let hits = db
            .search_doc_chunks_fts_visible("pistachio", 10, &nothing)
            .unwrap();
        assert!(
            hits.iter()
                .any(|h| h.document_id == "d1" && h.snippet.contains("pistachio")),
            "chunk-only document must be keyword-findable: {hits:?}"
        );
        // Punctuation-only query defuses to no hits (never an FTS syntax error).
        assert!(db
            .search_doc_chunks_fts_visible("?!*(", 10, &nothing)
            .unwrap()
            .is_empty());
    }

    /// GATE twin of `doc_chunk_search_is_gated_by_visibility` for the KEYWORD leg: a doc chunk row
    /// that deliberately SURVIVES in a sealed-not-unlocked folder is EXCLUDED by
    /// `search_doc_chunks_fts_visible` (defense-in-depth `visibility_clause`) and reappears only
    /// with the session unlock set. RED if the clause were dropped from the FTS join.
    #[test]
    fn doc_chunk_fts_search_is_gated_by_visibility() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_document(
            "d1",
            "f-locked",
            "secret.md",
            "launch date is the 14th",
            "document",
            100,
        )
        .unwrap();
        db.index_document_chunks("d1", None).unwrap();

        let nothing = std::collections::HashSet::new();
        assert!(
            db.search_doc_chunks_fts_visible("launch", 10, &nothing)
                .unwrap()
                .iter()
                .any(|h| h.document_id == "d1"),
            "open-folder document chunk must be keyword-visible"
        );

        // Seal the folder (chunk row deliberately survives) → INVISIBLE with empty unlock set.
        db.set_folder_locked("f-locked", true, None).unwrap();
        assert!(
            !db.search_doc_chunks_fts_visible("launch", 10, &nothing)
                .unwrap()
                .iter()
                .any(|h| h.document_id == "d1"),
            "sealed-not-unlocked document chunk leaked through the FTS gate"
        );

        // Session-unlock → present again.
        let mut unlocked = std::collections::HashSet::new();
        unlocked.insert("f-locked".to_string());
        assert!(
            db.search_doc_chunks_fts_visible("launch", 10, &unlocked)
                .unwrap()
                .iter()
                .any(|h| h.document_id == "d1"),
            "session-unlocked document chunk must reappear in FTS search"
        );
    }

    /// SEAL-DARKNESS at the INDEX level: purging a document's chunks (what the lock path does)
    /// removes its tokens from `fts_doc_chunks` in the same statement (the `_ad` trigger), so no
    /// sealed token survives in the inverted index at rest; re-indexing restores them. Mirrors
    /// `sealed_tokens_purged_from_fts_after_blank` for the meeting FTS trio.
    #[test]
    fn doc_fts_tokens_purged_with_chunks_and_restored_on_reindex() {
        let db = mem_db();
        seed_folder(&db, "f", "F");
        db.insert_document(
            "d1",
            "f",
            "n.md",
            "unicornfeather budget detail",
            "note",
            100,
        )
        .unwrap();
        db.index_document_chunks("d1", None).unwrap();

        let fts_count = |db: &Db| -> i64 {
            db.lock()
                .query_row(
                    "SELECT COUNT(*) FROM fts_doc_chunks WHERE fts_doc_chunks MATCH '\"unicornfeather\"'",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert!(fts_count(&db) >= 1, "token indexed after chunking");

        db.purge_doc_chunks_for_documents(&["d1".to_string()])
            .unwrap();
        assert_eq!(
            fts_count(&db),
            0,
            "sealed/purged token must not survive in the FTS index"
        );
        let nothing = std::collections::HashSet::new();
        assert!(db
            .search_doc_chunks_fts_visible("unicornfeather", 10, &nothing)
            .unwrap()
            .is_empty());

        db.index_document_chunks("d1", None).unwrap();
        assert!(fts_count(&db) >= 1, "re-index restores the keyword index");
    }

    /// POLISH DIACRITICS: the doc FTS uses the SAME `unicode61 remove_diacritics 2` tokenizer as
    /// the meeting tables, so an ASCII-folded query matches the diacritic original and vice versa
    /// for NFD-decomposable marks (ż/ź/ę/ś/ą/ó/ć/ń — note ł, a stroked letter with NO Unicode
    /// decomposition, is genuinely NOT foldable by any FTS5 remove_diacritics mode).
    #[test]
    fn doc_fts_matches_polish_diacritics_folded() {
        let db = mem_db();
        seed_folder(&db, "f", "F");
        db.insert_document(
            "d1",
            "f",
            "pl.md",
            "gęślą jaźń — budżet kwartalny",
            "note",
            100,
        )
        .unwrap();
        db.index_document_chunks("d1", None).unwrap();

        let nothing = std::collections::HashSet::new();
        for q in ["budzet", "jazn", "gesla"] {
            assert!(
                db.search_doc_chunks_fts_visible(q, 10, &nothing)
                    .unwrap()
                    .iter()
                    .any(|h| h.document_id == "d1"),
                "ASCII query {q:?} must match the diacritic original (remove_diacritics 2)"
            );
        }
        assert!(
            db.search_doc_chunks_fts_visible("gęślą", 10, &nothing)
                .unwrap()
                .iter()
                .any(|h| h.document_id == "d1"),
            "diacritic query must match too"
        );
    }

    /// SEAL ROUND-TRIP (verify-before-destroy, byte-identical): a document's text encrypts under a CK
    /// (AAD-less here for the unit, mirroring the `seal_*_round_trips_byte_identical` pattern), blanks,
    /// and decrypts back byte-identical via `seal_document` / `set_document_text` / `clear_document_blob`.
    #[test]
    fn seal_document_round_trips_byte_identical() {
        let db = mem_db();
        seed_folder(&db, "f", "F");
        let original = "zażółć gęślą jaźń — DECISION: ship Friday";
        db.insert_document("d1", "f", "n.md", original, "document", 100)
            .unwrap();

        let ck = crate::crypto::random_key().unwrap();
        // Encrypt + VERIFY decryptable BEFORE sealing (the command's verify-before-destroy rule).
        let blob = crate::crypto::encrypt(&ck, original.as_bytes(), b"").unwrap();
        assert_eq!(
            crate::crypto::decrypt(&ck, &blob, b"").unwrap(),
            original.as_bytes()
        );
        db.seal_document("d1", &blob).unwrap();
        // Plaintext blanked, blob present.
        let raw = db.raw_documents_in_folder("f").unwrap();
        assert_eq!(raw[0].text, "");
        let stored_blob = raw[0].blob.clone().unwrap();
        // Decrypt the STORED blob → byte-identical to the original.
        let restored = crate::crypto::decrypt(&ck, &stored_blob, b"").unwrap();
        assert_eq!(
            restored,
            original.as_bytes(),
            "sealed document round-trips byte-identical"
        );
        // Restore + clear the blob (remove-lock shape).
        db.set_document_text("d1", &String::from_utf8(restored).unwrap())
            .unwrap();
        db.clear_document_blob("d1").unwrap();
        let raw2 = db.raw_documents_in_folder("f").unwrap();
        assert_eq!(raw2[0].text, original);
        assert!(raw2[0].blob.is_none());
    }

    /// delete_document drops the row AND cascades its chunks/vectors (mirrors delete_meeting).
    #[test]
    fn delete_document_drops_row_and_chunks() {
        let db = mem_db();
        seed_folder(&db, "f", "F");
        db.insert_document("d1", "f", "n.md", "alpha bravo charlie", "document", 100)
            .unwrap();
        db.index_document_chunks("d1", Some(&crate::embed::StubEmbedder))
            .unwrap();
        db.delete_document("d1").unwrap();
        assert!(
            db.get_document("d1").unwrap().is_none(),
            "document row deleted"
        );
        let count: i64 = db
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM doc_chunks WHERE document_id = 'd1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "doc chunks cascade-deleted");
    }

    /// RELATED-BY-MEANING GATE: `related_meetings_visible` re-embeds the SOURCE meeting's own chunk
    /// text into a centroid, runs the gated KNN, and must NEVER surface a sealed-not-session-unlocked
    /// neighbour — even when that neighbour's chunk row deliberately survives (so exclusion can ONLY
    /// come from the visibility gate, not from purge). Session-unlocking its folder admits it again.
    /// RED if the gate inside `search_semantic_visible` were removed.
    #[test]
    fn related_meetings_visible_is_gated_by_visibility() {
        let db = mem_db();
        seed_folder(&db, "f-open", "Open");
        seed_folder(&db, "f-locked", "Secret");

        // Source (open) + an open target with the SAME note text (near in stub space) + a sealed
        // meeting with the SAME text (would be near). All indexed via the stub so vectors are real.
        let body = "shared budget planning topic for the quarter";
        for id in ["source", "target", "sealed"] {
            db.insert_meeting(&sample_meeting(id, "2026-06-24T10:00:00Z"))
                .unwrap();
            note_for(&db, id, "claude_code", body);
        }
        db.set_note_folder("source", Some("f-open")).unwrap();
        db.set_note_folder("target", Some("f-open")).unwrap();
        db.set_note_folder("sealed", Some("f-locked")).unwrap();
        for id in ["source", "target", "sealed"] {
            db.index_meeting_chunks(id, &[], &crate::embed::StubEmbedder)
                .unwrap();
        }
        // Lock the sealed folder WITHOUT purging — its chunk row survives, so any exclusion must be
        // the gate doing its job.
        db.set_folder_locked("f-locked", true, None).unwrap();
        assert!(
            chunk_count(&db, "sealed") > 0,
            "sealed chunk must survive for a true gate test"
        );

        let stub = crate::embed::StubEmbedder;

        // Empty unlock set → the sealed neighbour is ABSENT (no hit, no snippet); the open target IS.
        let nothing = std::collections::HashSet::new();
        let hidden = db
            .related_meetings_visible("source", &stub, 5, &nothing)
            .unwrap();
        assert!(
            hidden.iter().any(|h| h.meeting.id == "target"),
            "open semantically-near target must be returned"
        );
        assert!(
            !hidden.iter().any(|h| h.meeting.id == "sealed"),
            "sealed-not-unlocked neighbour leaked through related_meetings_visible"
        );

        // Session-unlock the sealed folder → it now appears.
        let mut unlocked = std::collections::HashSet::new();
        unlocked.insert("f-locked".to_string());
        let shown = db
            .related_meetings_visible("source", &stub, 5, &unlocked)
            .unwrap();
        assert!(
            shown.iter().any(|h| h.meeting.id == "sealed"),
            "session-unlocked neighbour must reappear in related results"
        );
    }

    /// SELF-EXCLUSION: the source meeting is never present in its own related results, even though it
    /// is (trivially) its own nearest neighbour in the KNN.
    #[test]
    fn related_meetings_visible_excludes_self() {
        let db = mem_db();
        let body = "atlas roadmap and hiring plan";
        for id in ["self", "other"] {
            db.insert_meeting(&sample_meeting(id, "2026-06-24T10:00:00Z"))
                .unwrap();
            note_for(&db, id, "claude_code", body);
            db.index_meeting_chunks(id, &[], &crate::embed::StubEmbedder)
                .unwrap();
        }
        let stub = crate::embed::StubEmbedder;
        let nothing = std::collections::HashSet::new();
        let hits = db
            .related_meetings_visible("self", &stub, 5, &nothing)
            .unwrap();
        assert!(
            !hits.iter().any(|h| h.meeting.id == "self"),
            "a meeting must never be in its own related results"
        );
        assert!(
            hits.iter().any(|h| h.meeting.id == "other"),
            "the other meeting (same text) should still be returned"
        );
    }

    /// EMPTY: a meeting with no `note_chunks` (never indexed, or chunks purged on lock) yields an
    /// empty result — never an error, never a panic.
    #[test]
    fn related_meetings_visible_empty_without_chunks() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("bare", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(
            &db,
            "bare",
            "claude_code",
            "has a note but was never indexed",
        );
        // No index_meeting_chunks call → zero chunk rows.
        assert_eq!(chunk_count(&db, "bare"), 0);
        let stub = crate::embed::StubEmbedder;
        let nothing = std::collections::HashSet::new();
        let hits = db
            .related_meetings_visible("bare", &stub, 5, &nothing)
            .unwrap();
        assert!(hits.is_empty(), "no chunks ⇒ empty related result");
    }

    /// PURGE-ON-LOCK: index a meeting's chunks while visible, then re-blank its folder (the relock
    /// path) → no `note_chunks`/`vec_chunks` row for that meeting survives at rest.
    #[test]
    fn vec_chunks_purged_on_lock() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(
            &db,
            "m1",
            "claude_code",
            "First budget paragraph.\n\nSecond hiring paragraph.",
        );
        db.set_note_folder("m1", Some("f-locked")).unwrap();

        // Index while visible (open folder) with the deterministic stub embedder.
        db.index_meeting_chunks("m1", &[], &crate::embed::StubEmbedder)
            .unwrap();
        assert!(chunk_count(&db, "m1") > 0, "expected chunks after indexing");
        assert!(vec_count(&db, "m1") > 0, "expected vectors after indexing");

        // Seal: blank the note (content_blob present so blank_sealed_notes_in_folders acts), then
        // run the relock blanker for the folder.
        db.seal_note("m1", "claude_code", b"ciphertext").unwrap();
        let mut folders = std::collections::HashSet::new();
        folders.insert("f-locked".to_string());
        db.blank_sealed_notes_in_folders(&folders).unwrap();

        assert_eq!(
            chunk_count(&db, "m1"),
            0,
            "note_chunks must be purged on lock (no plaintext chunk at rest)"
        );
        assert_eq!(
            vec_count(&db, "m1"),
            0,
            "vec_chunks must be purged on lock (no invertible vector at rest)"
        );
    }

    // ── transcript chunks (source_type='transcript') ──────────────────────────

    /// Count `note_chunks` of a given `source_type` for a meeting.
    fn chunk_count_of(db: &Db, meeting_id: &str, source_type: &str) -> i64 {
        db.lock()
            .query_row(
                "SELECT COUNT(*) FROM note_chunks WHERE meeting_id = ?1 AND source_type = ?2",
                rusqlite::params![meeting_id, source_type],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn tseg(idx: i64, start: f64, end: f64, speaker: &str, text: &str) -> Segment {
        Segment {
            idx,
            start_s: start,
            end_s: end,
            text: text.to_string(),
            speaker: Some(speaker.to_string()),
            confidence: None,
        }
    }

    /// `index_meeting_chunks` writes BOTH classes: note-summary (`voice`) AND transcript
    /// (`transcript`), 1:1-paired with vec rows, in one clean-replace tx.
    #[test]
    fn index_meeting_chunks_writes_transcript_class() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(
            &db,
            "m1",
            "claude_code",
            "Summary paragraph about the budget.",
        );
        let segs = [
            tseg(0, 0.0, 3.0, "me", "so what did we decide on the migration"),
            tseg(
                1,
                3.0,
                8.0,
                "others",
                "we agreed to defer it to next quarter",
            ),
        ];
        db.index_meeting_chunks("m1", &segs, &crate::embed::StubEmbedder)
            .unwrap();

        assert!(
            chunk_count_of(&db, "m1", "voice") > 0,
            "note-summary chunks must be written"
        );
        assert!(
            chunk_count_of(&db, "m1", "transcript") > 0,
            "transcript chunks must be written from segments"
        );
        // vec rows are 1:1 with note_chunks rows (both classes).
        assert_eq!(
            chunk_count(&db, "m1"),
            vec_count(&db, "m1"),
            "every chunk (both classes) has a vector"
        );
    }

    /// RE-INDEX is a CLEAN REPLACE of BOTH classes: re-running with different segments/note leaves no
    /// stale rows of either class.
    #[test]
    fn index_meeting_chunks_reindex_replaces_both_classes() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "First note.");
        let segs1 = [tseg(0, 0.0, 3.0, "me", "first pass transcript content")];
        db.index_meeting_chunks("m1", &segs1, &crate::embed::StubEmbedder)
            .unwrap();
        let v1 = chunk_count_of(&db, "m1", "transcript");
        assert!(v1 > 0);

        // Re-index with EMPTY segments → transcript class must go to zero (clean replace, no orphans).
        db.index_meeting_chunks("m1", &[], &crate::embed::StubEmbedder)
            .unwrap();
        assert_eq!(
            chunk_count_of(&db, "m1", "transcript"),
            0,
            "re-index with no segments must leave zero stale transcript chunks"
        );
        assert!(
            chunk_count_of(&db, "m1", "voice") > 0,
            "note class still present after re-index"
        );
        assert_eq!(
            chunk_count(&db, "m1"),
            vec_count(&db, "m1"),
            "1:1 vec pairing preserved after re-index"
        );
    }

    /// EMPTY segments → no transcript chunks (the "sealed meeting is never chunked" property in the
    /// db layer: the gated callers pass the RESTORED plaintext; a sealed meeting whose segments are
    /// blanked yields the empty slice ⇒ zero transcript chunks).
    #[test]
    fn index_meeting_chunks_no_segments_writes_no_transcript_class() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "Only a note, no transcript.");
        db.index_meeting_chunks("m1", &[], &crate::embed::StubEmbedder)
            .unwrap();
        assert_eq!(
            chunk_count_of(&db, "m1", "transcript"),
            0,
            "blank/sealed transcript ⇒ no transcript chunks"
        );
        assert!(
            chunk_count_of(&db, "m1", "voice") > 0,
            "note class still indexes independently"
        );
    }

    /// GATE (semantic): a sealed-and-not-session-unlocked meeting's TRANSCRIPT chunks surface ZERO
    /// through `search_semantic_visible`. Mirrors `vec_semantic_search_is_gated_by_visibility` but
    /// with a real transcript-source chunk that is deliberately left in place (folder flipped to
    /// locked WITHOUT purge) — so exclusion can ONLY come from `visibility_clause`. RED if the gate
    /// were removed OR if transcript chunks bypassed the shared reader.
    #[test]
    fn transcript_chunks_are_gated_by_visibility_semantic() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("sealed", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "sealed", "claude_code", "note body");
        db.set_note_folder("sealed", Some("f-locked")).unwrap();
        // Insert a REAL transcript-source chunk (source_type='transcript') + its vector, then lock the
        // folder WITHOUT purging — the row survives so any exclusion is the gate.
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO note_chunks (meeting_id, provider_id, chunk_idx, source_type, text)
                 VALUES ('sealed', 'transcript', 0, 'transcript', ?1)",
                rusqlite::params!["Secret · 2026-06-24\n[00:00-00:05] (others)\nthe secret merger price is confidential"],
            )
            .unwrap();
            let chunk_id = conn.last_insert_rowid();
            let blob = crate::embed::vec_to_blob(&one_hot(0));
            conn.execute(
                "INSERT INTO vec_chunks(chunk_id, embedding) VALUES (?1, ?2)",
                rusqlite::params![chunk_id, blob],
            )
            .unwrap();
        }
        db.set_folder_locked("f-locked", true, None).unwrap();

        let query = one_hot(0);
        let nothing = std::collections::HashSet::new();
        let hidden = db.search_semantic_visible(&query, 10, &nothing).unwrap();
        assert!(
            !hidden.iter().any(|h| h.meeting.id == "sealed"),
            "sealed meeting's TRANSCRIPT chunk leaked through the semantic gate"
        );
        // Session-unlock → it reappears (proves the row + gate, not purge).
        let mut unlocked = std::collections::HashSet::new();
        unlocked.insert("f-locked".to_string());
        let shown = db.search_semantic_visible(&query, 10, &unlocked).unwrap();
        assert!(
            shown.iter().any(|h| h.meeting.id == "sealed"),
            "session-unlocked meeting's transcript chunk must reappear in semantic results"
        );
    }

    /// GATE (hybrid): the same sealed meeting's transcript chunk is ALSO absent through the fused
    /// FTS+semantic+graph reader — with BOTH an FTS term AND a matching query vector — so exclusion
    /// is the shared `visibility_clause`, not purge. Companion to `vec_hybrid_search_is_gated_by_visibility`.
    #[test]
    fn transcript_chunks_are_gated_by_visibility_hybrid() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("sealed", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "sealed", "claude_code", "quarterly merger note body");
        db.set_note_folder("sealed", Some("f-locked")).unwrap();
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO note_chunks (meeting_id, provider_id, chunk_idx, source_type, text)
                 VALUES ('sealed', 'transcript', 0, 'transcript', ?1)",
                rusqlite::params!["Secret · 2026-06-24\n[00:00-00:05] (others)\nthe merger budget is confidential"],
            )
            .unwrap();
            let chunk_id = conn.last_insert_rowid();
            let blob = crate::embed::vec_to_blob(&one_hot(0));
            conn.execute(
                "INSERT INTO vec_chunks(chunk_id, embedding) VALUES (?1, ?2)",
                rusqlite::params![chunk_id, blob],
            )
            .unwrap();
        }
        db.set_folder_locked("f-locked", true, None).unwrap();

        let query_vec = one_hot(0);
        let nothing = std::collections::HashSet::new();
        let hidden = db
            .search_hybrid_visible("merger", &query_vec, 10, &nothing, None)
            .unwrap();
        assert!(
            !hidden.iter().any(|h| h.meeting.id == "sealed"),
            "sealed meeting's TRANSCRIPT chunk leaked through the hybrid gate"
        );
        let mut unlocked = std::collections::HashSet::new();
        unlocked.insert("f-locked".to_string());
        let shown = db
            .search_hybrid_visible("merger", &query_vec, 10, &unlocked, None)
            .unwrap();
        assert!(
            shown.iter().any(|h| h.meeting.id == "sealed"),
            "session-unlocked meeting's transcript chunk must reappear in hybrid results"
        );
    }

    /// PURGE-ON-SEAL covers transcript chunks: index BOTH classes while visible, then seal the folder
    /// → ZERO transcript-source chunks remain at rest AND the sealed meeting surfaces ZERO through
    /// both `search_semantic_visible` and `search_hybrid_visible`. RED if the seal purge missed the
    /// transcript class.
    #[test]
    fn transcript_chunks_purged_on_seal() {
        let db = mem_db();
        seed_folder(&db, "f-locked", "Secret");
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "budget summary note");
        db.set_note_folder("m1", Some("f-locked")).unwrap();
        let segs = [
            tseg(
                0,
                0.0,
                4.0,
                "me",
                "budget discussion said aloud but not summarized",
            ),
            tseg(
                1,
                4.0,
                9.0,
                "others",
                "we will cut the marketing line item next quarter",
            ),
        ];
        db.index_meeting_chunks("m1", &segs, &crate::embed::StubEmbedder)
            .unwrap();
        assert!(
            chunk_count_of(&db, "m1", "transcript") > 0,
            "transcript chunks present before seal"
        );

        // Seal the folder (blank note + relock blanker → purge_chunks_tx).
        db.seal_note("m1", "claude_code", b"ciphertext").unwrap();
        let mut folders = std::collections::HashSet::new();
        folders.insert("f-locked".to_string());
        db.blank_sealed_notes_in_folders(&folders).unwrap();

        assert_eq!(
            chunk_count_of(&db, "m1", "transcript"),
            0,
            "transcript chunks must be purged on seal (no said-content at rest)"
        );
        assert_eq!(
            chunk_count(&db, "m1"),
            0,
            "ALL chunk classes purged on seal"
        );
        assert_eq!(vec_count(&db, "m1"), 0, "all vectors purged on seal");

        // And they surface nowhere through either gated reader.
        let query = one_hot(0);
        let nothing = std::collections::HashSet::new();
        assert!(
            !db.search_semantic_visible(&query, 10, &nothing)
                .unwrap()
                .iter()
                .any(|h| h.meeting.id == "m1"),
            "sealed meeting must not surface via semantic search after purge"
        );
        assert!(
            !db.search_hybrid_visible("marketing", &query, 10, &nothing, None)
                .unwrap()
                .iter()
                .any(|h| h.meeting.id == "m1"),
            "sealed meeting must not surface via hybrid search after purge"
        );
    }

    /// `delete_meeting` must also purge the vec0 layer. `vec_chunks` is an FK-less vec0 vtab, so the
    /// `meetings` ON DELETE CASCADE reaches `note_chunks` but NOT `vec_chunks` — without an explicit
    /// purge the deleted meeting's invertible embeddings ORPHAN at rest (and a reused rowid could
    /// later PK-conflict). Counts the RAW `vec_chunks` table on purpose: a JOIN-through-note_chunks
    /// count would false-green, since the cascade already removed the note_chunks rows. (Closes
    /// lock-security-review finding 2.)
    #[test]
    fn vec_chunks_purged_on_delete_meeting() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(
            &db,
            "m1",
            "claude_code",
            "First budget paragraph.\n\nSecond hiring paragraph.",
        );
        db.index_meeting_chunks("m1", &[], &crate::embed::StubEmbedder)
            .unwrap();
        let raw_vecs = |db: &Db| -> i64 {
            db.lock()
                .query_row("SELECT COUNT(*) FROM vec_chunks", [], |r| r.get(0))
                .unwrap()
        };
        assert!(raw_vecs(&db) > 0, "expected vectors after indexing");

        db.delete_meeting("m1").unwrap();

        assert_eq!(
            chunk_count(&db, "m1"),
            0,
            "note_chunks gone after delete_meeting (FK cascade)"
        );
        assert_eq!(
            raw_vecs(&db),
            0,
            "vec_chunks must be purged on delete_meeting — no orphaned invertible vector at rest"
        );
    }

    /// HYBRID RRF fusion over real FTS + vector inputs: a meeting strong in BOTH lists ranks above
    /// one strong in only one; dedup is one hit per meeting.
    #[test]
    fn vec_hybrid_fuses_fts_and_vector() {
        let db = mem_db();
        for (id, ts) in [
            ("both", "2026-06-24T10:00:00Z"),
            ("fts_only", "2026-06-24T11:00:00Z"),
            ("vec_only", "2026-06-24T12:00:00Z"),
        ] {
            db.insert_meeting(&sample_meeting(id, ts)).unwrap();
        }
        // FTS term "alpha": present in `both` and `fts_only` notes.
        note_for(&db, "both", "claude_code", "alpha shared topic");
        note_for(&db, "fts_only", "claude_code", "alpha only here");
        note_for(&db, "vec_only", "claude_code", "unrelated lexical");

        // Vector dim 0: `both` and `vec_only` chunks align with the query; `fts_only` is orthogonal.
        let query = one_hot(0);
        insert_known_chunk(&db, "both", "alpha shared topic", &one_hot(0));
        insert_known_chunk(&db, "vec_only", "unrelated lexical", &one_hot(0));
        insert_known_chunk(&db, "fts_only", "alpha only here", &one_hot(5));

        let nothing = std::collections::HashSet::new();
        let hits = db
            .search_hybrid_visible("alpha", &query, 10, &nothing, None)
            .unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.meeting.id.as_str()).collect();
        // One hit per meeting (dedup).
        let mut uniq = ids.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), ids.len(), "hybrid must dedup by meeting");
        // `both` is in BOTH ranked lists → must be the top fused result.
        assert_eq!(
            ids.first(),
            Some(&"both"),
            "meeting strong in both lists must rank first"
        );
        assert!(ids.contains(&"fts_only") && ids.contains(&"vec_only"));
    }

    /// Insert a LOCKED folder directly (the `mod tests` `seed_folder` makes an OPEN one).
    fn seed_locked_folder(db: &Db, id: &str, name: &str) {
        db.insert_folder(&Folder {
            id: id.to_string(),
            name: name.to_string(),
            path: name.to_string(),
            parent_id: None,
            locked: true,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
    }

    /// GRAPHRAG-LITE: the entity-graph leg surfaces a co-mentioned meeting that BOTH the FTS and
    /// vector legs miss. Meeting `B` mentions the same entity as `A` but its note shares no query
    /// term and it has no vector chunk — so only the entity-neighbour expansion can reach it.
    #[test]
    fn graph_leg_surfaces_co_mentioned_meeting_fts_and_vector_miss() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("A", "2026-06-24T10:00:00Z"))
            .unwrap();
        db.insert_meeting(&sample_meeting("B", "2026-06-24T11:00:00Z"))
            .unwrap();
        note_for(&db, "A", "claude_code", "atlas project kickoff notes");
        // B shares NO token with the query "atlas status" and gets no vector chunk.
        note_for(&db, "B", "claude_code", "quarterly logistics review");
        let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
        db.add_mention(&atlas, "A").unwrap();
        db.add_mention(&atlas, "B").unwrap();

        let nothing = std::collections::HashSet::new();
        let empty_vec: Vec<f32> = Vec::new();
        // B is absent from FTS (note shares no query token) ...
        assert!(
            !db.search_visible("atlas status", 10, &nothing)
                .unwrap()
                .iter()
                .any(|h| h.meeting.id == "B"),
            "FTS must miss B"
        );
        // ... and absent from vector (no chunk at all → empty KNN).
        assert!(
            db.search_semantic_visible(&empty_vec, 10, &nothing)
                .unwrap()
                .is_empty(),
            "vector must miss B"
        );
        // The query names entity Atlas → graph leg pulls in its neighbour B.
        let hits = db
            .search_hybrid_visible("atlas status", &empty_vec, 10, &nothing, None)
            .unwrap();
        assert!(
            hits.iter().any(|h| h.meeting.id == "B"),
            "co-mentioned B must surface via the entity-graph leg"
        );
    }

    /// GATE: an entity-neighbour meeting in a SEALED-and-not-unlocked folder is EXCLUDED from the
    /// expansion with an empty unlock set, and reappears once its folder id is unlocked. Also
    /// covers the resolver gate: an entity mentioned ONLY in the sealed folder does not resolve.
    /// (RED on an ungated neighbour query — both the resolver and the neighbour reader must gate.)
    #[test]
    fn graph_expansion_respects_lock_gate() {
        let db = mem_db();
        seed_locked_folder(&db, "f-secret", "Secret");
        db.insert_meeting(&sample_meeting("A", "2026-06-24T10:00:00Z"))
            .unwrap();
        db.insert_meeting(&sample_meeting("B", "2026-06-24T11:00:00Z"))
            .unwrap();
        note_for(&db, "A", "claude_code", "atlas open meeting");
        note_for(&db, "B", "claude_code", "atlas sealed meeting");
        db.set_note_folder("B", Some("f-secret")).unwrap();

        // Atlas is visible (mentioned in open A); Phantom is mentioned ONLY in sealed B.
        let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
        db.add_mention(&atlas, "A").unwrap();
        db.add_mention(&atlas, "B").unwrap();
        let phantom = db.upsert_entity("Phantom", EntityKind::Project).unwrap();
        db.add_mention(&phantom, "B").unwrap();

        let empty = std::collections::HashSet::new();
        // Neighbour reader: B (sealed) excluded, A (open) kept.
        let nbrs = db
            .meetings_mentioning_entities_visible(std::slice::from_ref(&atlas), &empty)
            .unwrap();
        assert!(nbrs.iter().any(|m| m.id == "A"), "open neighbour A present");
        assert!(
            !nbrs.iter().any(|m| m.id == "B"),
            "sealed-folder neighbour B must be excluded with empty unlock set"
        );
        // Resolver: Phantom (sealed-only) does NOT resolve while locked.
        assert!(
            db.entities_matching_query("phantom report", &empty)
                .unwrap()
                .is_empty(),
            "an entity mentioned only in a sealed folder must not resolve"
        );

        // Unlock f-secret → both reappear.
        let mut unlocked = std::collections::HashSet::new();
        unlocked.insert("f-secret".to_string());
        let nbrs_u = db
            .meetings_mentioning_entities_visible(&[atlas], &unlocked)
            .unwrap();
        assert!(
            nbrs_u.iter().any(|m| m.id == "B"),
            "sealed neighbour B reappears when its folder is unlocked"
        );
        assert_eq!(
            db.entities_matching_query("phantom report", &unlocked)
                .unwrap(),
            vec![phantom],
            "Phantom resolves once its folder is unlocked"
        );
    }

    /// NOISE GUARD: a query that names no known entity leaves the hybrid result byte-identical to
    /// the FTS∪vector RRF fusion (the graph leg is empty, no spurious expansion).
    #[test]
    fn no_entity_match_leaves_hybrid_identical_to_two_leg_fusion() {
        let db = mem_db();
        for (id, ts) in [
            ("m1", "2026-06-24T10:00:00Z"),
            ("m2", "2026-06-24T11:00:00Z"),
        ] {
            db.insert_meeting(&sample_meeting(id, ts)).unwrap();
        }
        note_for(&db, "m1", "claude_code", "budget planning notes");
        note_for(&db, "m2", "claude_code", "budget review summary");
        // An entity exists, but the query below does NOT name it.
        let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
        db.add_mention(&atlas, "m1").unwrap();

        let query = one_hot(0);
        insert_known_chunk(&db, "m1", "budget planning notes", &one_hot(0));
        insert_known_chunk(&db, "m2", "budget review summary", &one_hot(3));

        let nothing = std::collections::HashSet::new();
        // The query names no entity → resolver empty.
        assert!(
            db.entities_matching_query("budget planning", &nothing)
                .unwrap()
                .is_empty(),
            "query must not resolve any entity"
        );

        // Expected = RRF over EXACTLY the two legs.
        let fts = db.search_visible("budget planning", 10, &nothing).unwrap();
        let sem = db.search_semantic_visible(&query, 10, &nothing).unwrap();
        let fts_ids: Vec<String> = fts.iter().map(|h| h.meeting.id.clone()).collect();
        let sem_ids: Vec<String> = sem.iter().map(|h| h.meeting.id.clone()).collect();
        let expected: Vec<String> =
            crate::embed::rrf_fuse(&[fts_ids, sem_ids], crate::embed::RRF_K)
                .into_iter()
                .map(|(id, _)| id)
                .collect();

        let got: Vec<String> = db
            .search_hybrid_visible("budget planning", &query, 10, &nothing, None)
            .unwrap()
            .into_iter()
            .map(|h| h.meeting.id)
            .collect();
        assert_eq!(
            got, expected,
            "no-entity hybrid must equal the 2-leg fusion"
        );
    }

    /// 3-WAY RRF: a meeting present in ALL THREE legs (FTS + vector + entity-graph) ranks first,
    /// and every meeting appears exactly once (dedup by meeting id across the legs).
    #[test]
    fn three_leg_rrf_ranks_multi_leg_first_and_dedups() {
        let db = mem_db();
        for (id, ts) in [
            ("all3", "2026-06-24T10:00:00Z"),
            ("fts_only", "2026-06-24T11:00:00Z"),
            ("graph_only", "2026-06-24T12:00:00Z"),
        ] {
            db.insert_meeting(&sample_meeting(id, ts)).unwrap();
        }
        // all3 + fts_only match the FTS query "atlas budget"; graph_only does not.
        note_for(&db, "all3", "claude_code", "atlas budget plan");
        note_for(&db, "fts_only", "claude_code", "atlas budget review");
        note_for(&db, "graph_only", "claude_code", "logistics offsite recap");
        // Entity Atlas mentioned in all3 + graph_only → both in the graph leg.
        let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
        db.add_mention(&atlas, "all3").unwrap();
        db.add_mention(&atlas, "graph_only").unwrap();
        // Only all3 has a vector chunk aligned with the query → vector leg = {all3}.
        let query = one_hot(0);
        insert_known_chunk(&db, "all3", "atlas budget plan", &one_hot(0));

        let nothing = std::collections::HashSet::new();
        let hits = db
            .search_hybrid_visible("atlas budget", &query, 10, &nothing, None)
            .unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.meeting.id.as_str()).collect();
        // Dedup: each meeting once.
        let mut uniq = ids.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), ids.len(), "3-leg fusion must dedup by meeting");
        // all3 (in all three legs) outranks single-leg meetings.
        assert_eq!(
            ids.first(),
            Some(&"all3"),
            "meeting in all 3 legs ranks first"
        );
        assert!(ids.contains(&"fts_only") && ids.contains(&"graph_only"));
    }

    fn chunk_count(db: &Db, meeting_id: &str) -> i64 {
        let conn = db.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM note_chunks WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn vec_count(db: &Db, meeting_id: &str) -> i64 {
        let conn = db.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM vec_chunks v
               JOIN note_chunks nc ON nc.id = v.chunk_id
              WHERE nc.meeting_id = ?1",
            rusqlite::params![meeting_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    // ── Brain v2 L1: topic chunks + temporal filter ───────────────────────────

    fn topic_chunk_count(db: &Db, meeting_id: &str) -> i64 {
        let conn = db.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM topic_chunks WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn topic_vec_count(db: &Db, meeting_id: &str) -> i64 {
        let conn = db.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM topic_vec_chunks v
               JOIN topic_chunks tc ON tc.id = v.chunk_id
              WHERE tc.meeting_id = ?1",
            rusqlite::params![meeting_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn long_seg(idx: i64, start: f64, end: f64, text: &str) -> Segment {
        Segment {
            idx,
            start_s: start,
            end_s: end,
            text: text.into(),
            speaker: Some("me".into()),
            confidence: None,
        }
    }

    /// L1.1 ROUND-TRIP + IDEMPOTENCY: indexing writes topic rows 1:1 with vec0 rows, the aug_text
    /// carries the `<title> | <date>` header, a same-content re-index is a NO-OP (no row rewrite —
    /// the content-hash probe), and changed segments produce a clean replace.
    #[test]
    fn topic_index_round_trips_and_is_idempotent() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m1", "2026-06-24T10:00:00Z"))
            .unwrap();
        note_for(&db, "m1", "claude_code", "budget notes");
        let segs = vec![long_seg(
            0,
            0.0,
            120.0,
            "we planned the quarterly budget in detail",
        )];
        db.insert_segments("m1", &segs).unwrap();
        let nothing = std::collections::HashSet::new();

        db.index_meeting_topic_chunks("m1", &segs, &crate::embed::StubEmbedder, &nothing)
            .unwrap();
        assert!(
            topic_chunk_count(&db, "m1") > 0,
            "topic rows must be written"
        );
        assert_eq!(
            topic_chunk_count(&db, "m1"),
            topic_vec_count(&db, "m1"),
            "topic chunks and vectors must be 1:1"
        );
        let aug: String = {
            let conn = db.lock();
            conn.query_row(
                "SELECT aug_text FROM topic_chunks WHERE meeting_id = 'm1' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert!(
            aug.starts_with("(untitled) | 2026-06-24\n"),
            "aug_text must carry the `<title> | <date>` header, got: {aug:?}"
        );

        // Same content ⇒ NO rewrite (the content-hash probe short-circuits before any delete).
        // Sentinel: mutate a column OUTSIDE the hash — a purge-then-reinsert would reset it, a
        // true no-op preserves it.
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE topic_chunks SET start_s = 999.0 WHERE meeting_id = 'm1'",
                [],
            )
            .unwrap();
        }
        db.index_meeting_topic_chunks("m1", &segs, &crate::embed::StubEmbedder, &nothing)
            .unwrap();
        let sentinel: f64 = {
            let conn = db.lock();
            conn.query_row(
                "SELECT MIN(start_s) FROM topic_chunks WHERE meeting_id = 'm1'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            sentinel, 999.0,
            "same-content re-index must be a no-op (content_hash idempotency)"
        );

        // Changed content ⇒ clean replace (old text gone, new text present, still 1:1 with vec0).
        let segs2 = vec![long_seg(
            0,
            0.0,
            120.0,
            "completely different hiring topic now",
        )];
        db.insert_segments("m1", &segs2).unwrap();
        db.index_meeting_topic_chunks("m1", &segs2, &crate::embed::StubEmbedder, &nothing)
            .unwrap();
        let texts: Vec<String> = {
            let conn = db.lock();
            let mut stmt = conn
                .prepare("SELECT text FROM topic_chunks WHERE meeting_id = 'm1' ORDER BY seg_index")
                .unwrap();
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            rows
        };
        assert!(
            texts.iter().all(|t| t.contains("hiring")) && !texts.is_empty(),
            "changed content must clean-replace the topic rows, got: {texts:?}"
        );
        assert!(
            texts.iter().all(|t| !t.contains("budget")),
            "old topic text must be gone after the clean replace"
        );
        assert_eq!(topic_chunk_count(&db, "m1"), topic_vec_count(&db, "m1"));
    }

    /// L1.5 RED-contrast: WITHOUT a date filter an out-of-window meeting that matches lexically IS
    /// returned by hybrid search (the pre-L1.5 behavior — the RED half); WITH the window it is
    /// EXCLUDED while the in-window match stays (the GREEN half). FTS-only hybrid (empty query
    /// vector) so the assertion isolates the lexical leg.
    #[test]
    fn hybrid_date_filter_excludes_out_of_window_lexical_match() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m-in", "2026-06-24T10:00:00Z"))
            .unwrap();
        db.insert_meeting(&sample_meeting("m-out", "2026-05-01T10:00:00Z"))
            .unwrap();
        note_for(&db, "m-in", "claude_code", "budżet kwartalny omówiony");
        note_for(&db, "m-out", "claude_code", "budżet roczny omówiony");
        let nothing = std::collections::HashSet::new();

        // RED baseline (no filter): BOTH lexical matches surface.
        let unfiltered = db
            .search_hybrid_visible("budżet", &[], 10, &nothing, None)
            .unwrap();
        assert!(unfiltered.iter().any(|h| h.meeting.id == "m-in"));
        assert!(
            unfiltered.iter().any(|h| h.meeting.id == "m-out"),
            "without a window the out-of-window lexical match must still be returned"
        );

        // GREEN: the "last week of the 2026-06-29 anchor" window excludes m-out on EVERY leg.
        let window = Some(("2026-06-22".to_string(), "2026-06-29".to_string()));
        let filtered = db
            .search_hybrid_visible("budżet", &[], 10, &nothing, window)
            .unwrap();
        assert!(
            filtered.iter().any(|h| h.meeting.id == "m-in"),
            "the in-window lexical match must survive the filter"
        );
        assert!(
            !filtered.iter().any(|h| h.meeting.id == "m-out"),
            "date-filtered hybrid search must exclude an out-of-window meeting that matches lexically"
        );
    }

    /// L1.5 temporal FALLBACK: a query whose tokens match NOTHING lexically but that carries a
    /// window returns the visible meetings IN the window (`matched_in: "temporal"`), newest-first —
    /// and never an out-of-window one.
    #[test]
    fn search_visible_in_range_falls_back_to_temporal_window() {
        let db = mem_db();
        db.insert_meeting(&sample_meeting("m-in", "2026-06-24T10:00:00Z"))
            .unwrap();
        db.insert_meeting(&sample_meeting("m-out", "2026-05-01T10:00:00Z"))
            .unwrap();
        note_for(&db, "m-in", "claude_code", "retro zespołu wnioski");
        note_for(&db, "m-out", "claude_code", "kickoff projektu");
        let nothing = std::collections::HashSet::new();

        let window = Some(("2026-06-22".to_string(), "2026-06-29".to_string()));
        // No token of this query appears in any note ⇒ pure window fallback.
        let hits = db
            .search_visible_in_range("what did we discuss", 10, &nothing, window)
            .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "only the in-window meeting may be returned: {hits:?}"
        );
        assert_eq!(hits[0].meeting.id, "m-in");
        assert_eq!(hits[0].matched_in, "temporal");

        // Without a window the same no-match query returns nothing (unchanged FTS behavior).
        let none = db
            .search_visible_in_range("what did we discuss", 10, &nothing, None)
            .unwrap();
        assert!(
            none.is_empty(),
            "no window + no lexical match ⇒ empty, as before"
        );
    }

    #[test]
    fn prunable_candidates_are_oldest_first_and_exclude_locked_folders() {
        const GOOD_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let p = unique_temp_path("murmur-prunable", "sqlite");
        let _ = std::fs::remove_file(&p);
        let db = Db::open_with_key(&p, GOOD_KEY).unwrap();

        // Two meetings in an OPEN vault-root (folder_id NULL), one in a LOCKED folder.
        for (id, at) in [
            ("old", "2026-01-01T00:00:00Z"),
            ("new", "2026-06-01T00:00:00Z"),
            ("secret", "2026-03-01T00:00:00Z"),
        ] {
            db.insert_meeting(&crate::storage::Meeting {
                id: id.into(),
                started_at: at.into(),
                ended_at: None,
                title: Some("t".into()),
                duration_s: 1,
                audio_path: Some(format!("/a/{id}.wav")),
                status: crate::storage::MeetingStatus::Summarized,
                folder_id: None,
            })
            .unwrap();
            db.upsert_note(&crate::storage::NoteRecord {
                meeting_id: id.into(),
                provider_id: "claude_code".into(),
                markdown: "m".into(),
                created_at: at.into(),
                exported_path: None,
                model_requested: None,
                model_served: None,
                gateway_host: None,
            })
            .unwrap();
        }
        db.insert_folder(&crate::storage::Folder {
            id: "f".into(),
            name: "Secret".into(),
            path: "Secret".into(),
            parent_id: None,
            locked: true,
            created_at: "2026-01-01T00:00:00Z".into(),
        })
        .unwrap();
        db.set_note_folder("secret", Some("f")).unwrap();

        let cands = db.prunable_audio_candidates().unwrap();
        let ids: Vec<&str> = cands.iter().map(|c| c.meeting_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["old", "new"],
            "locked 'secret' excluded; oldest-first order"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// Race-safety regression (TOCTOU: prune snapshots a plaintext path, a concurrent seal
    /// re-points the column to `.enc`, prune must NOT null the sealed pointer). The conditional
    /// clear only nulls when the column still holds the snapshotted plaintext path.
    #[test]
    fn clear_meeting_audio_path_if_only_nulls_a_matching_path() {
        let db = mem_db();
        db.insert_meeting(&crate::storage::Meeting {
            id: "M".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: None,
            title: Some("t".into()),
            duration_s: 1,
            audio_path: Some("/d/M.wav".into()),
            status: crate::storage::MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();

        // Sealed-in-between case: the column was re-pointed to `/d/M.wav.enc` by a concurrent
        // seal; the prune's stale snapshot `/d/M.wav.enc` != current would wrongly match the OLD
        // unconditional NULL. The conditional clear (expected != current plaintext) must NO-OP.
        db.clear_meeting_audio_path_if("M", "/d/M.wav.enc").unwrap();
        assert_eq!(
            db.get_meeting("M").unwrap().unwrap().audio_path,
            Some("/d/M.wav".to_string()),
            "mismatched expected path must NOT null the column (sealed-in-between case)"
        );

        // Matching snapshot (no concurrent seal) → clears.
        db.clear_meeting_audio_path_if("M", "/d/M.wav").unwrap();
        assert_eq!(db.get_meeting("M").unwrap().unwrap().audio_path, None);
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
        super::unique_temp_path(&format!("meetnotes-lock-test-{label}"), "sqlite")
    }

    fn file_db(label: &str) -> Db {
        Db::open_with_key(&temp_db_path(label), TEST_DEK).unwrap()
    }

    /// #5 (0.7 security fast-follow): the durable accept-resume record round-trips (insert → get →
    /// idempotent overwrite → delete → None). Proves the additive `pending_share_accepts` table +
    /// its methods — the persistence that makes a post-flip accept RECOVERABLE, not stranded.
    #[test]
    fn pending_share_accept_round_trips_and_deletes() {
        let db = file_db("pending-accept");
        assert!(db.get_pending_share_accept("s-1").unwrap().is_none());
        let p = PendingShareAccept {
            share_id: "s-1".to_string(),
            blob_id: "blob-9".to_string(),
            target_folder_id: "f-shared".to_string(),
            sender_user_id: "u-sender".to_string(),
            sender_fingerprint: "ABCDE-FGHIJ".to_string(),
            wrapped_key: vec![1, 2, 3, 4],
            grant_sig: vec![9, 8, 7],
            rev: 2,
            key_generation: 3,
            created_at: "2026-07-04T10:00:00Z".to_string(),
        };
        db.insert_pending_share_accept(&p).unwrap();
        let got = db.get_pending_share_accept("s-1").unwrap().unwrap();
        assert_eq!(got.blob_id, "blob-9");
        assert_eq!(got.target_folder_id, "f-shared");
        assert_eq!(got.sender_user_id, "u-sender");
        assert_eq!(got.sender_fingerprint, "ABCDE-FGHIJ");
        assert_eq!(got.wrapped_key, vec![1, 2, 3, 4]);
        assert_eq!(got.grant_sig, vec![9, 8, 7]);
        assert_eq!(got.rev, 2);
        assert_eq!(got.key_generation, 3);
        // Idempotent overwrite on the same share_id (a retry that re-flips is harmless).
        db.insert_pending_share_accept(&p).unwrap();
        assert!(db.get_pending_share_accept("s-1").unwrap().is_some());
        // Drop it (commit) → gone; delete is idempotent.
        db.delete_pending_share_accept("s-1").unwrap();
        assert!(db.get_pending_share_accept("s-1").unwrap().is_none());
        db.delete_pending_share_accept("s-1").unwrap();
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
            model_requested: None,
            model_served: None,
            gateway_host: None,
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
            model_requested: None,
            model_served: None,
            gateway_host: None,
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
        db.set_note_folder(meeting_id, folder_id.as_deref())
            .unwrap();
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
            assert_eq!(
                crate::crypto::decrypt(&ck, &blob, b"").unwrap(),
                n.markdown.as_bytes()
            );
            blobs.push((n.meeting_id.clone(), n.provider_id.clone(), blob));
        }
        db.set_folder_locked(folder_id, true, Some(&wrapped))
            .unwrap();
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
            db.restore_note_markdown(
                &n.meeting_id,
                &n.provider_id,
                &String::from_utf8(pt).unwrap(),
            )
            .unwrap();
        }
    }

    #[test]
    fn list_open_commitments_aggregates_attaches_context_and_gates() {
        let db = file_db("commitments");
        seed_folder(&db, "f-lock", "Secret");
        // open1: one open item w/ owner+due, one DONE item, one loose open item (no owner/date).
        seed_note(
            &db,
            "open1",
            "## Action items\n- [ ] Anna — ship the deck 2026-07-01\n- [x] Bob — done thing\n- [ ] just a loose task\n",
            None,
        );
        // open2: one open item, an earlier due date (sorts first).
        seed_note(&db, "open2", "- [ ] Carol — review 2026-06-15\n", None);
        // sealed: in a folder we lock → must contribute NOTHING until session-unlocked.
        seed_note(
            &db,
            "sealed",
            "- [ ] Dave — secret task 2026-07-02\n",
            Some("f-lock"),
        );
        db.set_folder_locked("f-lock", true, None).unwrap();

        // GATED: folder locked, not session-unlocked → sealed meeting excluded; DONE item excluded.
        let open = db.list_open_commitments(&HashSet::new(), None).unwrap();
        assert!(
            open.iter().all(|c| c.meeting_id != "sealed"),
            "sealed-not-unlocked meeting leaked into the rollup (gate violation)"
        );
        assert!(
            open.iter().all(|c| !c.text.contains("done thing")),
            "checked `- [x]` item must be excluded"
        );
        assert_eq!(
            open.len(),
            3,
            "two open meetings → 3 open items (Carol, Anna, loose)"
        );

        // Sort: due dates ascending, then None last.
        assert_eq!(open[0].due_date.as_deref(), Some("2026-06-15"));
        assert_eq!(open[0].owner.as_deref(), Some("Carol"));
        assert_eq!(
            open[0].meeting_title, "title-open2",
            "meeting context attached"
        );
        assert_eq!(open[1].due_date.as_deref(), Some("2026-07-01"));
        assert_eq!(open[1].owner.as_deref(), Some("Anna"));
        assert_eq!(open[2].due_date, None, "the dateless loose task sorts last");
        assert_eq!(open[2].owner, None);

        // Session-unlock → the sealed meeting's open commitment reappears.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let all = db.list_open_commitments(&unlocked, None).unwrap();
        assert!(
            all.iter()
                .any(|c| c.meeting_id == "sealed" && c.text.contains("secret task")),
            "unlocked folder's commitment must reappear"
        );

        // Owner filter (case-insensitive) keeps only Anna's item.
        let anna = db.list_open_commitments(&unlocked, Some("ANNA")).unwrap();
        assert_eq!(anna.len(), 1);
        assert!(anna[0].text.contains("ship the deck"));
        assert_eq!(anna[0].meeting_title, "title-open1");
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
        let by_id: std::collections::HashMap<_, _> = unlocked
            .iter()
            .map(|n| (n.meeting_id.as_str(), n))
            .collect();
        assert_eq!(
            by_id["m1"].markdown, md_a,
            "m1 markdown must round-trip byte-identical"
        );
        assert_eq!(
            by_id["m2"].markdown, md_b,
            "m2 markdown must round-trip byte-identical"
        );
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
            assert!(
                n.content_blob.is_some(),
                "each provider row keeps its own blob"
            );
        }
        // The two blobs must differ (distinct plaintext → distinct ciphertext).
        let blob_claude = sealed
            .iter()
            .find(|n| n.provider_id == "claude_code")
            .unwrap();
        let blob_ollama = sealed.iter().find(|n| n.provider_id == "ollama").unwrap();
        assert_ne!(
            blob_claude.content_blob, blob_ollama.content_blob,
            "distinct provider markdown must NOT share one blob (content-loss guard)"
        );

        // Unlock → BOTH providers' markdown returns byte-identical.
        session_unlock(&db, "f1", &kek);
        let unlocked = db.notes_in_folder("f1").unwrap();
        let by_provider: std::collections::HashMap<_, _> = unlocked
            .iter()
            .map(|n| (n.provider_id.as_str(), n))
            .collect();
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
        seed_note(
            &db,
            "sealed1",
            "# secret note about bananas",
            Some("secret"),
        );

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
        assert!(db
            .get_note_if_visible("sealed1", &unlocked)
            .unwrap()
            .is_some());
        assert!(!db
            .search_visible("bananas", 20, &unlocked)
            .unwrap()
            .is_empty());
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
        assert!(db.meeting_is_visible("m1", &HashSet::new()).unwrap());
    }

    // ── Phase 0.5 full-lock helpers (transcript + timeline + audio) ──────────────

    use crate::transcribe::types::Segment;

    /// Seed transcript segments + a cached timeline JSON for a meeting (open state).
    fn seed_transcript_and_timeline(
        db: &Db,
        meeting_id: &str,
        texts: &[&str],
        timeline_json: &str,
    ) {
        let segs: Vec<Segment> = texts
            .iter()
            .enumerate()
            .map(|(i, t)| Segment {
                idx: i as i64,
                start_s: i as f64,
                end_s: (i + 1) as f64,
                text: t.to_string(),
                speaker: if i % 2 == 0 {
                    Some("me".into())
                } else {
                    Some("others".into())
                },
                confidence: None,
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
                assert_eq!(
                    crate::crypto::decrypt(ck, &blob, b"").unwrap(),
                    s.text.as_bytes()
                );
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
                    let text =
                        String::from_utf8(crate::crypto::decrypt(ck, blob, b"").unwrap()).unwrap();
                    db.restore_segment_text(&mid, s.idx, &text).unwrap();
                }
            }
            if let Some(tl) = db.raw_timeline(&mid).unwrap() {
                if let Some(blob) = &tl.data_blob {
                    let data =
                        String::from_utf8(crate::crypto::decrypt(ck, blob, b"").unwrap()).unwrap();
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
        let texts = [
            "zażółć gęślą jaźń 🔒",
            "second segment with budget 1_000_000 EUR",
            "",
        ];
        let timeline =
            r#"{"turns":[{"speaker":"me","topic":"secret topic","start_s":0.0,"end_s":1.0}]}"#;
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
        assert!(db
            .get_segments("m1")
            .unwrap()
            .iter()
            .all(|s| s.text.is_empty()));
        assert_eq!(db.get_timeline_data("m1").unwrap().as_deref(), Some(""));

        // UNLOCK → byte-identical round-trip of EVERY segment + the timeline.
        unseal_extras(&db, "f1", &ck);
        let restored = db.get_segments("m1").unwrap();
        let restored_texts: Vec<&str> = restored.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            restored_texts, texts,
            "transcript round-trips byte-identical"
        );
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
        db.set_meeting_audio_path("m1", Some(&wav.to_string_lossy()))
            .unwrap();

        let ck = crate::crypto::random_key().unwrap();
        let wrapped = crate::crypto::encrypt(&kek, &ck, b"").unwrap();
        db.set_folder_locked("f1", true, Some(&wrapped)).unwrap();

        // SEAL → .enc written, plaintext removed, audio_path re-pointed at the .enc.
        seal_extras(&db, "f1", &ck);
        assert!(!wav.exists(), "plaintext WAV removed while sealed");
        let enc_path = db.get_meeting("m1").unwrap().unwrap().audio_path.unwrap();
        assert!(
            enc_path.ends_with(".enc"),
            "audio_path points at the encrypted file"
        );
        assert!(std::path::Path::new(&enc_path).exists(), ".enc exists");
        let blob = std::fs::read(&enc_path).unwrap();
        assert!(
            !contains_subslice(&blob, &payload),
            "encrypted audio must not leak plaintext"
        );

        // UNLOCK → plaintext WAV materialized again, byte-identical.
        unseal_extras(&db, "f1", &ck);
        let plain_path = db.get_meeting("m1").unwrap().unwrap().audio_path.unwrap();
        assert!(
            !plain_path.ends_with(".enc"),
            "audio_path re-points at the plaintext WAV"
        );
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
        assert!(
            !meeting_unlocked(&db, "m1", &empty),
            "gate: meeting is locked"
        );
        assert!(
            db.get_segments("m1")
                .unwrap()
                .iter()
                .all(|s| s.text.is_empty()),
            "transcript empty while locked"
        );
        assert_eq!(db.get_timeline_data("m1").unwrap().as_deref(), Some(""));
        assert!(
            db.get_latest_note_for_meeting("m1")
                .unwrap()
                .unwrap()
                .markdown
                .is_empty(),
            "note markdown blank while locked"
        );

        // SESSION-UNLOCK (add folder id to the set + decrypt back) → full content.
        let mut unlocked = HashSet::new();
        unlocked.insert("f1".to_string());
        session_unlock(&db, "f1", &kek); // note markdown
        unseal_extras(&db, "f1", &folder_ck); // transcript + timeline
        assert!(
            meeting_unlocked(&db, "m1", &unlocked),
            "gate: meeting unlocked"
        );
        assert_eq!(db.get_segments("m1").unwrap()[0].text, "secret words");
        assert_eq!(
            db.get_timeline_data("m1").unwrap().as_deref(),
            Some(r#"{"turns":[]}"#)
        );
        assert_eq!(
            db.get_latest_note_for_meeting("m1")
                .unwrap()
                .unwrap()
                .markdown,
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
        db.set_meeting_audio_path("m1", Some(&wav.to_string_lossy()))
            .unwrap();
        let ck = crate::crypto::random_key().unwrap();
        let wrapped = crate::crypto::encrypt(&kek, &ck, b"").unwrap();
        db.set_folder_locked("f1", true, Some(&wrapped)).unwrap();
        seal_extras(&db, "f1", &ck);

        let empty: HashSet<String> = HashSet::new();
        // LOCKED → export refused (the command early-returns AppError::Locked when the gate is
        // false). There is also no plaintext WAV on disk to copy.
        assert!(
            !meeting_unlocked(&db, "m1", &empty),
            "export refused while locked"
        );
        let enc = db.get_meeting("m1").unwrap().unwrap().audio_path.unwrap();
        assert!(enc.ends_with(".enc"));
        assert!(!std::path::Path::new(enc.trim_end_matches(".enc")).exists());

        // UNLOCKED → allowed.
        unseal_extras(&db, "f1", &ck);
        let mut unlocked = HashSet::new();
        unlocked.insert("f1".to_string());
        assert!(
            meeting_unlocked(&db, "m1", &unlocked),
            "export allowed once unlocked"
        );
        let plain = db.get_meeting("m1").unwrap().unwrap().audio_path.unwrap();
        assert!(
            std::path::Path::new(&plain).exists(),
            "plaintext WAV available for export"
        );

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

    // ── brain2 R2 bitemporal facts: persistence + gating + purge ──────────────

    use crate::facts::{FactCandidate, FactOp, NewFact};

    fn add_op(
        entity_id: &str,
        predicate: &str,
        object: &str,
        valid_from: &str,
        meeting_id: &str,
    ) -> FactOp {
        FactOp::Add(NewFact {
            entity_id: entity_id.to_string(),
            subject: "Atlas".to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            valid_from: valid_from.to_string(),
            recorded_at: valid_from.to_string(),
            confidence: 1.0,
            meeting_id: Some(meeting_id.to_string()),
        })
    }

    // ── Brain v2 L1.1/L1.2: topic chunks under the lock model ─────────────────

    fn topic_counts(db: &Db, meeting_id: &str) -> (i64, i64) {
        let conn = db.lock();
        let chunks: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM topic_chunks WHERE meeting_id = ?1",
                rusqlite::params![meeting_id],
                |r| r.get(0),
            )
            .unwrap();
        let vecs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM topic_vec_chunks v
                   JOIN topic_chunks tc ON tc.id = v.chunk_id
                  WHERE tc.meeting_id = ?1",
                rusqlite::params![meeting_id],
                |r| r.get(0),
            )
            .unwrap();
        (chunks, vecs)
    }

    /// Raw fts_topic_chunks MATCH count for a term — proves the `_ad` trigger purged the sealed
    /// content's tokens from the FTS index, not just the base rows.
    fn topic_fts_matches(db: &Db, term: &str) -> i64 {
        let conn = db.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM fts_topic_chunks WHERE fts_topic_chunks MATCH ?1",
            rusqlite::params![format!("\"{term}\"")],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn one_topic_segment(text: &str) -> Vec<Segment> {
        vec![Segment {
            idx: 0,
            start_s: 0.0,
            end_s: 120.0,
            text: text.to_string(),
            speaker: Some("me".to_string()),
            confidence: None,
        }]
    }

    /// PURGE-ON-SEAL (L1.1, lock-critical): sealing a folder via `blank_sealed_notes_in_folders`
    /// (the same tx that blanks the plaintext) must drop the meeting's topic_chunks AND their
    /// topic_vec_chunks rows AND the aug_text tokens from fts_topic_chunks. RED-before-GREEN:
    /// without the topic deletes in `purge_chunks_tx` the rows + tokens survive the seal.
    #[test]
    fn seal_purges_topic_chunks_vectors_and_fts_tokens() {
        let db = file_db("topic-purge-seal");
        seed_folder(&db, "f-lock", "Secret");
        seed_note(&db, "m1", "sealed content", Some("f-lock"));
        let segs = one_topic_segment("tajny wątek fuzji zebra omawiany szczegółowo");
        db.insert_segments("m1", &segs).unwrap();
        let nothing = HashSet::new();
        db.index_meeting_topic_chunks("m1", &segs, &crate::embed::StubEmbedder, &nothing)
            .unwrap();
        let (chunks, vecs) = topic_counts(&db, "m1");
        assert!(chunks > 0 && vecs == chunks, "indexed before seal");
        assert!(
            topic_fts_matches(&db, "zebra") > 0,
            "aug tokens indexed before seal"
        );

        // Seal: flip the folder + run the shared blank/purge tx (the lock_folder path).
        db.set_folder_locked("f-lock", true, None).unwrap();
        let mut folders = HashSet::new();
        folders.insert("f-lock".to_string());
        db.blank_sealed_notes_in_folders(&folders).unwrap();

        let (chunks, vecs) = topic_counts(&db, "m1");
        assert_eq!(chunks, 0, "topic_chunks must be purged on seal");
        assert_eq!(vecs, 0, "topic_vec_chunks must be purged on seal");
        assert_eq!(
            topic_fts_matches(&db, "zebra"),
            0,
            "sealed aug_text tokens must be purged from fts_topic_chunks"
        );
    }

    /// STARTUP RECONCILE (crash-while-unlocked): `reblank_locked_folders_at_rest` must purge topic
    /// rows of every locked folder's meeting — a crash after a session re-index cannot leave a
    /// sealed meeting's topic vectors at rest. Mirrors the note_chunks reconcile test.
    #[test]
    fn reblank_at_rest_purges_topic_chunks() {
        let db = file_db("topic-purge-reblank");
        seed_folder(&db, "f-lock", "Secret");
        seed_note(&db, "m1", "sealed content", Some("f-lock"));
        let segs = one_topic_segment("poufny plan przejęcia gepard w toku");
        db.insert_segments("m1", &segs).unwrap();
        let nothing = HashSet::new();
        db.index_meeting_topic_chunks("m1", &segs, &crate::embed::StubEmbedder, &nothing)
            .unwrap();
        assert!(topic_counts(&db, "m1").0 > 0);

        // Simulate the crash-while-unlocked shape: folder locked on disk, derived rows present.
        db.set_folder_locked("f-lock", true, None).unwrap();
        db.reblank_locked_folders_at_rest().unwrap();

        let (chunks, vecs) = topic_counts(&db, "m1");
        assert_eq!(
            chunks, 0,
            "startup reconcile must purge sealed topic chunks"
        );
        assert_eq!(vecs, 0, "startup reconcile must purge sealed topic vectors");
        assert_eq!(topic_fts_matches(&db, "gepard"), 0);
    }

    /// L1.2 GATING: the augmentation header carries the meeting's VISIBLE attendees + facts; a
    /// sealed-and-not-unlocked meeting is a NO-OP for the topic indexer (nothing is chunked at
    /// all — the visibility gate fires before any read).
    #[test]
    fn topic_aug_header_is_gated_and_sealed_meeting_is_never_indexed() {
        let db = file_db("topic-aug-gate");
        seed_folder(&db, "f-lock", "Secret");
        seed_note(&db, "m1", "atlas planning", Some("f-lock"));
        let anna = db.upsert_entity("Anna Nowak", EntityKind::Person).unwrap();
        db.add_mention(&anna, "m1").unwrap();
        db.apply_fact_ops(&[add_op(
            &anna,
            "deadline",
            "Q3",
            "2026-06-01T00:00:00Z",
            "m1",
        )])
        .unwrap();
        let segs = one_topic_segment("omawiamy harmonogram projektu atlas");
        db.insert_segments("m1", &segs).unwrap();
        let nothing = HashSet::new();

        // OPEN folder: the header carries the gated attendee + fact.
        db.index_meeting_topic_chunks("m1", &segs, &crate::embed::StubEmbedder, &nothing)
            .unwrap();
        let aug: String = {
            let conn = db.lock();
            conn.query_row(
                "SELECT aug_text FROM topic_chunks WHERE meeting_id = 'm1' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        let header = aug.lines().next().unwrap_or_default().to_string();
        assert!(
            header.contains("Anna Nowak"),
            "attendee missing from header: {header:?}"
        );
        assert!(
            header.contains("deadline: Q3"),
            "fact missing from header: {header:?}"
        );

        // Seal + purge, then try to index again while NOT unlocked: must stay empty (gate no-op).
        db.set_folder_locked("f-lock", true, None).unwrap();
        db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
        assert_eq!(topic_counts(&db, "m1").0, 0);
        db.index_meeting_topic_chunks("m1", &segs, &crate::embed::StubEmbedder, &nothing)
            .unwrap();
        assert_eq!(
            topic_counts(&db, "m1").0,
            0,
            "a sealed-not-unlocked meeting must never be topic-indexed"
        );

        // Session-unlocked: indexing is allowed again (the unlock re-index path).
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        db.index_meeting_topic_chunks("m1", &segs, &crate::embed::StubEmbedder, &unlocked)
            .unwrap();
        assert!(
            topic_counts(&db, "m1").0 > 0,
            "session-unlocked meeting indexes again"
        );
    }

    /// TOCTOU (lock-security finding, PR 3): a `lock_folder` committing BETWEEN the indexer's
    /// pre-embed visibility gate and its write transaction must not leave freshly-written
    /// sealed-meeting plaintext at rest. Simulated by sealing the note at rest (markdown blanked,
    /// `content_blob` kept — exactly what `blank_sealed_notes_in_folders` leaves) while the
    /// CALLER'S unlocked snapshot is stale and still names the folder: the pre-gate passes, and
    /// only the in-tx sealed-at-rest re-check can refuse the write.
    #[test]
    fn topic_index_refuses_write_when_sealed_at_rest_mid_flight() {
        let db = file_db("topic-toctou");
        seed_folder(&db, "f-lock", "Secret");
        seed_note(&db, "m1", "atlas planning", Some("f-lock"));
        let segs = one_topic_segment("omawiamy harmonogram projektu atlas");
        db.insert_segments("m1", &segs).unwrap();

        // "lock_folder committed mid-embed": folder locked + note sealed at rest…
        db.set_folder_locked("f-lock", true, None).unwrap();
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE notes SET markdown = '', content_blob = X'00' WHERE meeting_id = 'm1'",
                [],
            )
            .unwrap();
        }
        // …while the caller's snapshot is STALE and still says the folder is unlocked.
        let mut stale = HashSet::new();
        stale.insert("f-lock".to_string());
        db.index_meeting_topic_chunks("m1", &segs, &crate::embed::StubEmbedder, &stale)
            .unwrap();
        assert_eq!(
            topic_counts(&db, "m1").0,
            0,
            "the in-tx sealed-at-rest re-check must refuse writing topic plaintext"
        );
    }

    /// apply_fact_ops persists an open fact; a later reconcile of a CHANGED object closes the old
    /// (valid_to set) and opens the new — both rows survive (bitemporal history). RED-before-GREEN:
    /// without the Invalidate UPDATE the old fact stays open (two open rows), failing the assertions.
    #[test]
    fn facts_apply_and_bitemporal_history_round_trips() {
        let db = file_db("facts-bitemporal");
        seed_note(&db, "m1", "Atlas is in progress", None);
        let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
        db.add_mention(&atlas, "m1").unwrap();

        // First meeting records: status = in-progress (open).
        db.apply_fact_ops(&[add_op(
            &atlas,
            "status",
            "in-progress",
            "2026-06-01T00:00:00Z",
            "m1",
        )])
        .unwrap();

        // Second meeting says: status = shipped → reconcile.
        let existing = db.facts_for_entities(std::slice::from_ref(&atlas)).unwrap();
        assert_eq!(existing.len(), 1);
        let cands = vec![FactCandidate {
            entity_id: atlas.clone(),
            subject: "Atlas".to_string(),
            predicate: "status".to_string(),
            object: "shipped".to_string(),
            confidence: 1.0,
        }];
        let at = "2026-06-20T00:00:00Z";
        let mut ops = crate::facts::reconcile_facts(&existing, &cands, at);
        crate::facts::set_meeting_id(&mut ops, "m1");
        db.apply_fact_ops(&ops).unwrap();

        // Both rows present: old closed at `at`, new open.
        let all = db.facts_for_entities(std::slice::from_ref(&atlas)).unwrap();
        assert_eq!(
            all.len(),
            2,
            "history preserved — old fact kept, not overwritten"
        );
        let open: Vec<_> = all.iter().filter(|f| f.valid_to.is_none()).collect();
        let closed: Vec<_> = all.iter().filter(|f| f.valid_to.is_some()).collect();
        assert_eq!(open.len(), 1, "exactly one currently-valid fact");
        assert_eq!(open[0].object, "shipped");
        assert_eq!(open[0].valid_from, at);
        assert_eq!(closed.len(), 1, "exactly one superseded fact");
        assert_eq!(closed[0].object, "in-progress");
        assert_eq!(
            closed[0].valid_to.as_deref(),
            Some(at),
            "old fact closed at the supersession instant"
        );

        // The gated read returns both (open first), since m1 is in an open folder.
        let facts = db.list_facts_visible(&atlas, &HashSet::new()).unwrap();
        assert_eq!(facts.len(), 2);
        assert!(
            facts[0].valid_to.is_none(),
            "open (current) fact ordered first"
        );
    }

    /// list_facts_visible GATE: a fact whose source meeting is in a sealed-and-not-unlocked folder is
    /// INVISIBLE, and reappears once the folder is session-unlocked. Uses set_folder_locked directly
    /// (NOT lock_folder) so the row survives at rest — this proves the READ GATE, independent of the
    /// purge-on-seal. RED-before-GREEN: drop the meetings-JOIN visibility predicate → the sealed
    /// fact leaks.
    #[test]
    fn list_facts_visible_excludes_sealed_meeting() {
        let db = file_db("facts-gate");
        seed_folder(&db, "f-lock", "Secret");
        seed_note(&db, "secret1", "Atlas acquisition", Some("f-lock"));
        let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
        db.add_mention(&atlas, "secret1").unwrap();
        db.apply_fact_ops(&[add_op(
            &atlas,
            "price",
            "10M",
            "2026-06-01T00:00:00Z",
            "secret1",
        )])
        .unwrap();

        // Open folder → fact visible.
        assert_eq!(
            db.list_facts_visible(&atlas, &HashSet::new())
                .unwrap()
                .len(),
            1
        );

        // Seal the folder flag directly (no purge) → the row survives at rest but must be GATED OUT.
        db.set_folder_locked("f-lock", true, None).unwrap();
        assert!(
            db.list_facts_visible(&atlas, &HashSet::new())
                .unwrap()
                .is_empty(),
            "a sealed-not-unlocked meeting's facts must not surface (gate violation)"
        );

        // Session-unlock → the fact reappears.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        assert_eq!(
            db.list_facts_visible(&atlas, &unlocked).unwrap().len(),
            1,
            "facts reappear once the folder is session-unlocked"
        );
    }

    /// PURGE-ON-SEAL: the same atomic tx that purges chunks/corrections/assistant-interactions also
    /// DELETES the meeting's facts (purge_facts_tx). RED-before-GREEN: without the purge_facts_tx
    /// call the fact row survives the seal.
    #[test]
    fn seal_purges_facts() {
        let db = file_db("facts-purge");
        seed_note(&db, "m1", "Atlas shipped", None);
        let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
        db.add_mention(&atlas, "m1").unwrap();
        db.apply_fact_ops(&[add_op(
            &atlas,
            "status",
            "shipped",
            "2026-06-01T00:00:00Z",
            "m1",
        )])
        .unwrap();
        assert_eq!(
            db.facts_for_entities(std::slice::from_ref(&atlas))
                .unwrap()
                .len(),
            1
        );

        // The seal purge (chunks + corrections + assistant interactions + FACTS) in one tx.
        db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
        assert!(
            db.facts_for_entities(std::slice::from_ref(&atlas))
                .unwrap()
                .is_empty(),
            "facts must be purged on seal (drop-on-seal, like correction_log / note_chunks)"
        );
    }

    /// delete_meeting cascades to facts (FK ON DELETE CASCADE).
    #[test]
    fn delete_meeting_cascades_to_facts() {
        let db = file_db("facts-cascade");
        seed_note(&db, "m1", "Atlas", None);
        let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
        db.add_mention(&atlas, "m1").unwrap();
        db.apply_fact_ops(&[add_op(
            &atlas,
            "status",
            "shipped",
            "2026-06-01T00:00:00Z",
            "m1",
        )])
        .unwrap();
        db.delete_meeting("m1").unwrap();
        assert!(
            db.facts_for_entities(&[atlas]).unwrap().is_empty(),
            "FK CASCADE drops facts"
        );
    }

    // ── Re-Truth (supersessions): record/dedup/lifecycle + purge-on-delete + purge-on-seal ──

    fn supersession_row(
        id: &str,
        superseding: &str,
        source: &str,
    ) -> crate::storage::models::SupersessionRow {
        crate::storage::models::SupersessionRow {
            id: id.to_string(),
            superseding_meeting_id: superseding.to_string(),
            source_meeting_id: source.to_string(),
            entity: "Atlas".to_string(),
            predicate: "status".to_string(),
            old_value: "in-progress".to_string(),
            new_value: "shipped".to_string(),
            created_at: "2026-06-20T00:00:00Z".to_string(),
            applied_at: None,
            source_pre_image: None,
            superseding_pre_image: None,
        }
    }

    /// record → unapplied read → DEDUP on natural key → mark applied (with pre-images) → applied row
    /// leaves the unapplied set → clear (undo) restores it as pending + drops the pre-images.
    #[test]
    fn supersessions_record_dedup_and_lifecycle() {
        let db = file_db("retruth-lifecycle");
        assert_eq!(
            db.record_supersessions(&[supersession_row("s1", "m2", "m1")])
                .unwrap(),
            1
        );
        // Re-record the SAME natural key → deduped (0 new), one row total.
        assert_eq!(
            db.record_supersessions(&[supersession_row("s1-dup", "m2", "m1")])
                .unwrap(),
            0,
            "identical natural key is deduped"
        );
        let pending = db.unapplied_supersessions_for("m2").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "s1");
        assert!(pending[0].applied_at.is_none());

        // Durably store a pre-image (undo scratch), THEN mark applied (applied_at is the last write).
        db.store_supersession_pre_images("s1", Some(&b"ORIGINAL"[..]), None)
            .unwrap();
        db.mark_supersession_applied("s1", "2026-06-21T00:00:00Z")
            .unwrap();
        assert!(
            db.unapplied_supersessions_for("m2").unwrap().is_empty(),
            "an applied row is no longer pending"
        );
        let got = db.get_supersession("s1").unwrap().unwrap();
        assert_eq!(got.applied_at.as_deref(), Some("2026-06-21T00:00:00Z"));
        assert_eq!(got.source_pre_image.as_deref(), Some(&b"ORIGINAL"[..]));

        // Clear (undo) → pending again, pre-image dropped.
        db.clear_supersession_applied("s1").unwrap();
        let after = db.get_supersession("s1").unwrap().unwrap();
        assert!(after.applied_at.is_none());
        assert!(after.source_pre_image.is_none());
        assert_eq!(db.unapplied_supersessions_for("m2").unwrap().len(), 1);
    }

    /// delete_meeting purges supersessions referencing that meeting on EITHER side (no FK — the
    /// explicit `purge_supersessions_tx`). RED-before-GREEN: without the purge the row survives.
    #[test]
    fn delete_meeting_purges_supersessions_either_side() {
        let db = file_db("retruth-delete");
        // Row where the deleted meeting is the SOURCE.
        db.record_supersessions(&[supersession_row("s-src", "m2", "m1")])
            .unwrap();
        db.delete_meeting("m1").unwrap();
        assert!(
            db.get_supersession("s-src").unwrap().is_none(),
            "deleting the source meeting purges its supersession"
        );
        // Row where the deleted meeting is the SUPERSEDING side.
        db.record_supersessions(&[supersession_row("s-sup", "m3", "m4")])
            .unwrap();
        db.delete_meeting("m3").unwrap();
        assert!(
            db.get_supersession("s-sup").unwrap().is_none(),
            "deleting the superseding meeting purges its supersession"
        );
    }

    /// PURGE-ON-SEAL: the seal purge tx (`purge_chunks_for_meetings`) also drops supersessions
    /// referencing the sealed meeting — an applied row's PLAINTEXT note pre-image must not linger at
    /// rest for a sealed folder. RED-before-GREEN: without the `purge_supersessions_tx` call the row
    /// (and its plaintext pre-image) survives the seal.
    #[test]
    fn seal_purges_supersessions() {
        let db = file_db("retruth-seal");
        db.record_supersessions(&[supersession_row("s1", "m2", "m1")])
            .unwrap();
        // Simulate apply having stored the plaintext pre-image + marked applied.
        db.store_supersession_pre_images("s1", Some(&b"PLAINTEXT NOTE"[..]), None)
            .unwrap();
        db.mark_supersession_applied("s1", "2026-06-21T00:00:00Z")
            .unwrap();
        // Seal the source meeting's folder → the seal purge tx runs for m1.
        db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
        assert!(
            db.get_supersession("s1").unwrap().is_none(),
            "a sealed meeting's supersession (+ its plaintext pre-image) must be purged on seal"
        );
    }

    /// STARTUP crash-while-unlocked reconcile: `reblank_locked_folders_at_rest` must purge every
    /// supersession referencing a locked meeting on EITHER side — so an applied row's plaintext note
    /// pre-images + fact-value strings do not linger at rest behind the lock after a restart. Mirrors
    /// `seal_purges_supersessions` but drives the STARTUP path. RED-before-GREEN: without the
    /// supersessions DELETE in that fn, both rows survive the reblank.
    #[test]
    fn reblank_at_rest_purges_supersessions() {
        let db = file_db("retruth-reblank");
        seed_folder(&db, "f-src", "Work");
        seed_folder(&db, "f-open", "Reviews");
        seed_note(&db, "m1", "source note", Some("f-src"));
        seed_note(&db, "m2", "other note", Some("f-open"));
        // Row A: the to-be-locked meeting m1 is the SOURCE. Row B: m1 is the SUPERSEDING side.
        db.record_supersessions(&[supersession_row("s-src", "m2", "m1")])
            .unwrap();
        db.record_supersessions(&[supersession_row("s-sup", "m1", "m2")])
            .unwrap();
        // Simulate both APPLIED with plaintext pre-images (the leak-at-rest scratch).
        for id in ["s-src", "s-sup"] {
            db.store_supersession_pre_images(id, Some(&b"PLAINTEXT NOTE"[..]), None)
                .unwrap();
            db.mark_supersession_applied(id, "2026-06-21T00:00:00Z")
                .unwrap();
        }
        assert!(db.get_supersession("s-src").unwrap().is_some());
        assert!(db.get_supersession("s-sup").unwrap().is_some());

        // Lock m1's folder on disk, then run the startup at-rest reblank (crash-while-unlocked path).
        db.set_folder_locked("f-src", true, Some(&b"wrapped"[..]))
            .unwrap();
        db.reblank_locked_folders_at_rest().unwrap();

        assert!(
            db.get_supersession("s-src").unwrap().is_none(),
            "source-side supersession (+ plaintext pre-image) purged at rest"
        );
        assert!(
            db.get_supersession("s-sup").unwrap().is_none(),
            "superseding-side supersession (+ plaintext pre-image) purged at rest"
        );
    }

    // ── Phase 3 CROSS-MEETING USER MEMORY: persistence + reconcile + gating + forget + purge ──

    fn user_add_op(predicate: &str, object: &str, valid_from: &str, meeting_id: &str) -> FactOp {
        FactOp::Add(NewFact {
            entity_id: crate::user_memory::USER_SCOPE.to_string(),
            subject: "You".to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            valid_from: valid_from.to_string(),
            recorded_at: valid_from.to_string(),
            confidence: 1.0,
            meeting_id: Some(meeting_id.to_string()),
        })
    }

    /// ROUND-TRIP persist/reconcile (task C5.ii): apply an open user fact; a later reconcile of a
    /// CHANGED object for the SAME predicate closes the old (valid_to set) and opens the new — both
    /// rows survive (bitemporal history), and only the current one is visible. RED-before-GREEN:
    /// without the Invalidate UPDATE two open rows survive, failing the single-open assertion.
    #[test]
    fn user_facts_apply_and_reconcile_round_trips() {
        let db = file_db("user-facts-roundtrip");
        seed_note(&db, "m1", "note", None);
        db.apply_user_fact_ops(&[user_add_op(
            "prefer",
            "English replies",
            "2026-06-01T00:00:00Z",
            "m1",
        )])
        .unwrap();

        // A later meeting supersedes the preference: prefer = Polish replies.
        seed_note(&db, "m2", "note2", None);
        let existing = db.user_facts_all().unwrap();
        assert_eq!(existing.len(), 1);
        let cands = vec![FactCandidate {
            entity_id: crate::user_memory::USER_SCOPE.to_string(),
            subject: "You".to_string(),
            predicate: "prefer".to_string(),
            object: "Polish replies".to_string(),
            confidence: 1.0,
        }];
        let at = "2026-06-20T00:00:00Z";
        let mut ops = crate::facts::reconcile_facts(&existing, &cands, at);
        crate::facts::set_meeting_id(&mut ops, "m2");
        db.apply_user_fact_ops(&ops).unwrap();

        let all = db.user_facts_all().unwrap();
        assert_eq!(
            all.len(),
            2,
            "history preserved — old user fact kept, not overwritten"
        );
        let open: Vec<_> = all.iter().filter(|f| f.valid_to.is_none()).collect();
        assert_eq!(open.len(), 1, "exactly one currently-valid user fact");
        assert_eq!(open[0].object, "Polish replies");

        // The gated read returns only the OPEN, VISIBLE fact.
        let visible = db.list_user_facts_visible(&HashSet::new()).unwrap();
        assert_eq!(visible.len(), 1, "only the current preference is visible");
        assert_eq!(visible[0].object, "Polish replies");
        assert_eq!(
            visible[0].meeting_id.as_deref(),
            Some("m2"),
            "provenance = the source meeting"
        );
    }

    /// GATE (task C5.i, DB layer): a user fact whose source meeting is sealed-and-not-unlocked is
    /// INVISIBLE and reappears once the folder is session-unlocked. Uses set_folder_locked directly
    /// (NOT lock_folder) so the row survives at rest — this proves the READ GATE, independent of the
    /// purge-on-seal. RED-before-GREEN: drop the meetings-JOIN visibility predicate → the sealed user
    /// fact leaks into the audit view AND the brief.
    #[test]
    fn list_user_facts_visible_excludes_sealed_meeting() {
        let db = file_db("user-facts-gate");
        seed_folder(&db, "f-lock", "Secret");
        seed_note(&db, "secret1", "private", Some("f-lock"));
        db.apply_user_fact_ops(&[user_add_op(
            "salary",
            "confidential",
            "2026-06-01T00:00:00Z",
            "secret1",
        )])
        .unwrap();

        assert_eq!(
            db.list_user_facts_visible(&HashSet::new()).unwrap().len(),
            1
        );
        db.set_folder_locked("f-lock", true, None).unwrap();
        assert!(
            db.list_user_facts_visible(&HashSet::new())
                .unwrap()
                .is_empty(),
            "a sealed-not-unlocked meeting's user facts must not surface (gate violation)"
        );
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        assert_eq!(
            db.list_user_facts_visible(&unlocked).unwrap().len(),
            1,
            "user facts reappear once the folder is session-unlocked"
        );
    }

    /// FORGET (task C5.iii): forget_user_fact bitemporally CLOSES the row (never deletes) so it drops
    /// out of the gated read; a second forget is a no-op. clear_user_facts closes ALL open facts.
    #[test]
    fn forget_and_clear_user_facts() {
        let db = file_db("user-facts-forget");
        seed_note(&db, "m1", "note", None);
        db.apply_user_fact_ops(&[
            user_add_op("prefer", "Polish", "2026-06-01T00:00:00Z", "m1"),
            user_add_op("role", "PM", "2026-06-01T00:00:00Z", "m1"),
        ])
        .unwrap();
        let visible = db.list_user_facts_visible(&HashSet::new()).unwrap();
        assert_eq!(visible.len(), 2);
        let target = visible[0].id.clone();

        // Forget one → it is closed and drops out.
        assert!(
            db.forget_user_fact(&target, "2026-06-02T00:00:00Z")
                .unwrap(),
            "closed one open fact"
        );
        assert!(
            !db.forget_user_fact(&target, "2026-06-02T00:00:00Z")
                .unwrap(),
            "re-forget is a no-op"
        );
        let after = db.list_user_facts_visible(&HashSet::new()).unwrap();
        assert_eq!(
            after.len(),
            1,
            "the forgotten fact drops out of the gated read"
        );
        assert!(after.iter().all(|f| f.id != target));
        // The row still EXISTS (closed, not deleted) — history preserved.
        assert_eq!(
            db.user_facts_all().unwrap().len(),
            2,
            "forget is an invalidate, not a delete"
        );

        // Clear all → nothing visible; every open fact closed.
        let n = db.clear_user_facts("2026-06-03T00:00:00Z").unwrap();
        assert_eq!(n, 1, "one remaining open fact closed");
        assert!(
            db.list_user_facts_visible(&HashSet::new())
                .unwrap()
                .is_empty(),
            "no user memory after clear"
        );
    }

    /// PURGE-ON-SEAL (task C2.a, DB layer): the same atomic seal tx that purges facts also DELETES the
    /// meeting's user facts (purge_user_facts_tx). RED-before-GREEN: without the purge_user_facts_tx
    /// call the user-fact row survives the seal at rest.
    #[test]
    fn seal_purges_user_facts() {
        let db = file_db("user-facts-purge");
        seed_note(&db, "m1", "note", None);
        db.apply_user_fact_ops(&[user_add_op(
            "prefer",
            "Polish",
            "2026-06-01T00:00:00Z",
            "m1",
        )])
        .unwrap();
        assert_eq!(db.user_facts_all().unwrap().len(), 1);
        // The seal purge (chunks + corrections + assistant interactions + facts + USER facts) in one tx.
        db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
        assert!(
            db.user_facts_all().unwrap().is_empty(),
            "user facts must be purged on seal (drop-on-seal, like facts / note_chunks)"
        );
    }

    /// delete_meeting cascades to user_facts (FK ON DELETE CASCADE).
    #[test]
    fn delete_meeting_cascades_to_user_facts() {
        let db = file_db("user-facts-cascade");
        seed_note(&db, "m1", "note", None);
        db.apply_user_fact_ops(&[user_add_op(
            "prefer",
            "Polish",
            "2026-06-01T00:00:00Z",
            "m1",
        )])
        .unwrap();
        db.delete_meeting("m1").unwrap();
        assert!(
            db.user_facts_all().unwrap().is_empty(),
            "FK CASCADE drops user facts"
        );
    }

    /// Brain v2 L2.2 (RED-first): `search_user_facts_visible` is BM25-relevance-filtered AND runs
    /// the SAME visibility gate as `list_user_facts_visible` — a sealed-not-unlocked source
    /// meeting's fact never matches, a session unlock re-admits it, only OPEN facts return, and an
    /// empty/punctuation-only query returns nothing (the caller's fallback owns that case).
    #[test]
    fn search_user_facts_visible_is_relevance_filtered_and_gated() {
        let db = file_db("user-facts-search");
        seed_folder(&db, "f-s", "Secret");
        seed_note(&db, "m-open", "open note", None);
        seed_note(&db, "m-priv", "private note", Some("f-s"));
        db.apply_user_fact_ops(&[
            user_add_op("works on", "Project Atlas", "2026-07-01T00:00:00Z", "m-open"),
            user_add_op("prefer", "Polish replies", "2026-07-01T00:00:00Z", "m-open"),
            user_add_op("salary", "Atlas bonus", "2026-07-01T00:00:00Z", "m-priv"),
        ])
        .unwrap();
        let none = HashSet::new();

        // Relevance: an "atlas" query matches the two Atlas facts, not the Polish-replies one.
        let hits = db.search_user_facts_visible("what about atlas?", 8, &none).unwrap();
        assert_eq!(hits.len(), 2, "both atlas facts match while all folders are open");
        assert!(hits.iter().all(|f| f.object.contains("Atlas")));

        // Empty / punctuation-only query ⇒ no hits (fallback belongs to the caller).
        assert!(db.search_user_facts_visible("", 8, &none).unwrap().is_empty());
        assert!(db
            .search_user_facts_visible("?!():", 8, &none)
            .unwrap()
            .is_empty());
        // FIX 3: a STOPWORD-ONLY query is no query at all — it must return zero hits (so the
        // caller's full-list fallback owns it), never a stopword-overlap "match".
        assert!(db
            .search_user_facts_visible("what about the", 8, &none)
            .unwrap()
            .is_empty());

        // GATE: seal the private folder — its fact drops out of the SAME query.
        db.set_folder_locked("f-s", true, None).unwrap();
        let hits = db.search_user_facts_visible("atlas", 8, &none).unwrap();
        assert_eq!(hits.len(), 1, "the sealed source's fact must not match");
        assert_eq!(hits[0].meeting_id.as_deref(), Some("m-open"));

        // A session unlock re-admits it (reversible gate).
        let mut unlocked = HashSet::new();
        unlocked.insert("f-s".to_string());
        assert_eq!(
            db.search_user_facts_visible("atlas", 8, &unlocked).unwrap().len(),
            2
        );

        // Only OPEN facts: forget the open-folder Atlas fact → it stops matching.
        let open_fact_id = db
            .list_user_facts_visible(&none)
            .unwrap()
            .into_iter()
            .find(|f| f.object == "Project Atlas")
            .unwrap()
            .id;
        db.forget_user_fact(&open_fact_id, "2026-07-02T00:00:00Z")
            .unwrap();
        assert!(
            db.search_user_facts_visible("atlas", 8, &none).unwrap().is_empty(),
            "a closed (forgotten) fact must not match"
        );
    }

    /// Brain v2 L2.2 LOCK-AT-REST: the purge-on-seal DELETE and `delete_meeting` both remove the
    /// fact's tokens from the `fts_user_facts` index (the `_ad` trigger fires on the direct
    /// DELETE) — no sealed/deleted fact text survives as a searchable token. Mirrors
    /// `sealed_tokens_purged_from_fts_after_blank` for the user-facts index.
    #[test]
    fn user_fact_tokens_purged_from_fts_on_seal_and_delete() {
        let db = file_db("user-facts-fts-purge");
        seed_note(&db, "m1", "note", None);
        seed_note(&db, "m2", "note2", None);
        db.apply_user_fact_ops(&[
            user_add_op("codename", "Zenith", "2026-07-01T00:00:00Z", "m1"),
            user_add_op("codename2", "Quasar", "2026-07-01T00:00:00Z", "m2"),
        ])
        .unwrap();
        let fts_count = |term: &str| -> i64 {
            let conn = db.lock();
            conn.query_row(
                "SELECT count(*) FROM fts_user_facts WHERE fts_user_facts MATCH ?1",
                rusqlite::params![format!("\"{term}\"")],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(fts_count("zenith"), 1);
        assert_eq!(fts_count("quasar"), 1);

        // Purge-on-seal path (the direct DELETE inside the seal tx).
        db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
        assert_eq!(fts_count("zenith"), 0, "sealed fact tokens must leave the index");

        // delete_meeting path (explicit purge in-tx).
        db.delete_meeting("m2").unwrap();
        assert_eq!(fts_count("quasar"), 0, "deleted fact tokens must leave the index");
    }

    /// FIX 1a (CRITICAL leak — the DB half): every seal/delete transaction purges ALL
    /// `memory_rollups` rows and returns their `exported_path`s for the caller's file deletion —
    /// `purge_chunks_for_meetings` (lock_folder / move-into-locked), `blank_sealed_notes_in_folders`
    /// (relock), `reblank_locked_folders_at_rest` (startup reconcile — purges ONLY when a locked
    /// folder exists, so an open-only DB keeps its rollups across restarts), and `delete_meeting`.
    /// RED on the pre-fix code: none of these touched `memory_rollups`.
    #[test]
    fn seal_paths_purge_memory_rollup_rows_and_return_export_paths() {
        let db = file_db("rollup-purge-paths");
        seed_folder(&db, "f1", "Secret");
        seed_note(&db, "m1", "note", Some("f1"));
        let seed_rollup = |db: &Db| {
            db.upsert_memory_rollup("entity:e1", "synth", "h", "2026-07-09T12:00:00Z")
                .unwrap();
            db.set_memory_rollup_exported("entity:e1", "/vault/brain/memory/entity-e1.md")
                .unwrap();
        };

        // lock_folder / move-into-locked chain.
        seed_rollup(&db);
        let paths = db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
        assert_eq!(paths, vec!["/vault/brain/memory/entity-e1.md".to_string()]);
        assert!(db.list_memory_rollups().unwrap().is_empty());

        // relock chain.
        seed_rollup(&db);
        let mut folders = HashSet::new();
        folders.insert("f1".to_string());
        let paths = db.blank_sealed_notes_in_folders(&folders).unwrap();
        assert_eq!(paths.len(), 1);
        assert!(db.list_memory_rollups().unwrap().is_empty());

        // Startup reconcile with NO locked folder ⇒ rollups SURVIVE (nothing sealed, no leak).
        seed_rollup(&db);
        let (_, paths, _) = db.reblank_locked_folders_at_rest().unwrap();
        assert!(paths.is_empty());
        assert_eq!(
            db.list_memory_rollups().unwrap().len(),
            1,
            "an open-only DB keeps its rollups across restarts"
        );

        // Startup reconcile WITH a locked folder ⇒ purged.
        db.set_folder_locked("f1", true, None).unwrap();
        let (_, paths, _) = db.reblank_locked_folders_at_rest().unwrap();
        assert_eq!(paths.len(), 1);
        assert!(db.list_memory_rollups().unwrap().is_empty());

        // delete_meeting.
        seed_rollup(&db);
        let paths = db.delete_meeting("m1").unwrap();
        assert_eq!(paths.len(), 1);
        assert!(db.list_memory_rollups().unwrap().is_empty());
    }

    // ── Voiceprints: at-rest storage + LOCK invariants (mirror the user_facts tests exactly) ──────

    /// A voiceprint stored via `insert_voiceprint` round-trips byte-exact through the BLOB and reads
    /// back through the gated reader with its embedding, cluster index, and NULL label intact.
    #[test]
    fn voiceprint_round_trips_through_gated_reader() {
        let db = file_db("voiceprint-round-trip");
        seed_note(&db, "m1", "note", None);
        let emb = vec![0.1f32, -0.2, 0.3, 0.4, -0.5];
        db.insert_voiceprint("vp1", "m1", 0, None, &emb, "2026-07-01T00:00:00Z")
            .unwrap();

        let got = db.list_voiceprints_visible(&HashSet::new()).unwrap();
        assert_eq!(
            got.len(),
            1,
            "the voiceprint is visible for an open meeting"
        );
        assert_eq!(got[0].id, "vp1");
        assert_eq!(got[0].meeting_id, "m1");
        assert_eq!(got[0].cluster_index, 0);
        assert_eq!(got[0].dim, emb.len() as i64);
        assert!(got[0].label.is_none(), "label is NULL until enrolled");
        assert_eq!(
            got[0].embedding, emb,
            "embedding round-trips byte-exact through the BLOB"
        );
    }

    /// GATE: a voiceprint whose source meeting is SEALED (its folder locked, not session-unlocked)
    /// must NOT surface from `list_voiceprints_visible` — a voice biometric of a locked speaker stays
    /// invisible. RED-before-GREEN: with an ungated SELECT the row would surface while sealed.
    #[test]
    fn list_voiceprints_visible_excludes_sealed_meeting() {
        let db = file_db("voiceprint-gate");
        seed_folder(&db, "f-lock", "Secret");
        seed_note(&db, "secret1", "private", Some("f-lock"));
        db.insert_voiceprint(
            "vp1",
            "secret1",
            1,
            None,
            &[0.5f32, 0.5, 0.5, 0.5],
            "2026-07-01T00:00:00Z",
        )
        .unwrap();

        assert_eq!(
            db.list_voiceprints_visible(&HashSet::new()).unwrap().len(),
            1
        );
        db.set_folder_locked("f-lock", true, None).unwrap();
        assert!(
            db.list_voiceprints_visible(&HashSet::new())
                .unwrap()
                .is_empty(),
            "a sealed-not-unlocked meeting's voiceprint must not surface (gate violation)"
        );
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        assert_eq!(
            db.list_voiceprints_visible(&unlocked).unwrap().len(),
            1,
            "the voiceprint reappears once the folder is session-unlocked"
        );
    }

    /// PURGE-ON-SEAL (DB layer): the same atomic seal tx that purges user facts also DELETES the
    /// meeting's voiceprints (purge_speaker_voiceprints_tx). RED-before-GREEN: without the purge call
    /// the voiceprint row survives the seal at rest.
    #[test]
    fn seal_purges_voiceprints() {
        let db = file_db("voiceprint-purge");
        seed_note(&db, "m1", "note", None);
        db.insert_voiceprint(
            "vp1",
            "m1",
            0,
            None,
            &[0.1f32, 0.2, 0.3],
            "2026-07-01T00:00:00Z",
        )
        .unwrap();
        // Present before the seal (visible for the open meeting).
        assert_eq!(
            db.list_voiceprints_visible(&HashSet::new()).unwrap().len(),
            1
        );
        // The seal purge (chunks + corrections + assistant interactions + facts + user facts +
        // VOICEPRINTS) in one tx.
        db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();
        assert!(
            db.list_voiceprints_visible(&HashSet::new())
                .unwrap()
                .is_empty(),
            "voiceprints must be purged on seal (drop-on-seal, like user facts)"
        );
    }

    /// At-rest reconcile (crash-while-unlocked recovery): `reblank_locked_folders_at_rest` purges the
    /// voiceprints of every meeting in a LOCKED folder in the same reconciliation tx. RED-before-GREEN:
    /// without the reconcile DELETE a voiceprint re-derived while unlocked would survive a restart.
    #[test]
    fn reconcile_purges_voiceprints_in_locked_folder() {
        let db = file_db("voiceprint-reconcile");
        seed_folder(&db, "f-lock", "Secret");
        seed_note(&db, "secret1", "private", Some("f-lock"));
        db.set_folder_locked("f-lock", true, None).unwrap();
        // Simulate a crash-while-unlocked leftover: a voiceprint persisted against a since-locked
        // meeting (the folder is locked at rest, so this row must not survive the reconcile).
        db.insert_voiceprint(
            "vp1",
            "secret1",
            0,
            None,
            &[0.9f32, 0.1],
            "2026-07-01T00:00:00Z",
        )
        .unwrap();

        db.reblank_locked_folders_at_rest().unwrap();
        // Even with the folder session-unlocked, the row is GONE (reconcile deleted it at rest).
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        assert!(
            db.list_voiceprints_visible(&unlocked).unwrap().is_empty(),
            "the at-rest reconcile must purge a locked folder's voiceprints"
        );
    }

    /// delete_meeting cascades to speaker_voiceprints (FK ON DELETE CASCADE).
    #[test]
    fn delete_meeting_cascades_to_voiceprints() {
        let db = file_db("voiceprint-cascade");
        seed_note(&db, "m1", "note", None);
        db.insert_voiceprint("vp1", "m1", 0, None, &[0.1f32, 0.2], "2026-07-01T00:00:00Z")
            .unwrap();
        db.delete_meeting("m1").unwrap();
        assert!(
            db.list_voiceprints_visible(&HashSet::new())
                .unwrap()
                .is_empty(),
            "FK CASCADE drops voiceprints"
        );
    }

    /// ENROLL (Phase 2): binding a person label to a cluster's voiceprint sets `label` on exactly
    /// that (meeting, cluster) row, leaves others untouched, and is idempotent (overwrite).
    #[test]
    fn set_voiceprint_label_for_cluster_enrolls_one_row() {
        let db = file_db("voiceprint-enroll");
        seed_note(&db, "m1", "note", None);
        db.insert_voiceprint("vp0", "m1", 0, None, &[1.0f32, 0.0], "2026-07-01T00:00:00Z")
            .unwrap();
        db.insert_voiceprint("vp1", "m1", 1, None, &[0.0f32, 1.0], "2026-07-01T00:00:00Z")
            .unwrap();

        let n = db
            .set_voiceprint_label_for_cluster("m1", 0, "Sarah")
            .unwrap();
        assert_eq!(n, 1, "exactly the cluster-0 row is labeled");

        let got = db.list_voiceprints_visible(&HashSet::new()).unwrap();
        let c0 = got.iter().find(|v| v.cluster_index == 0).unwrap();
        let c1 = got.iter().find(|v| v.cluster_index == 1).unwrap();
        assert_eq!(c0.label.as_deref(), Some("Sarah"), "enroll bound the label");
        assert!(c1.label.is_none(), "the other cluster is untouched");

        // Idempotent overwrite.
        assert_eq!(
            db.set_voiceprint_label_for_cluster("m1", 0, "Sara")
                .unwrap(),
            1
        );
        let got2 = db.list_voiceprints_visible(&HashSet::new()).unwrap();
        assert_eq!(
            got2.iter()
                .find(|v| v.cluster_index == 0)
                .unwrap()
                .label
                .as_deref(),
            Some("Sara")
        );

        // No voiceprint for that cluster → no-op (0 rows), never an error (pre-opt-in recordings).
        assert_eq!(
            db.set_voiceprint_label_for_cluster("m1", 9, "Nobody")
                .unwrap(),
            0
        );
    }

    /// FORGET one voiceprint by id removes exactly that row; deleting a missing id is a no-op.
    #[test]
    fn delete_voiceprint_removes_one_row() {
        let db = file_db("voiceprint-forget-one");
        seed_note(&db, "m1", "note", None);
        db.insert_voiceprint("vp0", "m1", 0, None, &[1.0f32, 0.0], "2026-07-01T00:00:00Z")
            .unwrap();
        db.insert_voiceprint("vp1", "m1", 1, None, &[0.0f32, 1.0], "2026-07-01T00:00:00Z")
            .unwrap();

        assert!(db.delete_voiceprint("vp0").unwrap(), "removed");
        let got = db.list_voiceprints_visible(&HashSet::new()).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "vp1", "only the requested row is gone");
        assert!(
            !db.delete_voiceprint("missing").unwrap(),
            "no-op on a missing id"
        );
    }

    /// CLEAR removes every voiceprint (the "forget all captured voices" affordance).
    #[test]
    fn clear_voiceprints_removes_all() {
        let db = file_db("voiceprint-clear");
        seed_note(&db, "m1", "note", None);
        seed_note(&db, "m2", "note2", None);
        db.insert_voiceprint("vp0", "m1", 0, None, &[1.0f32, 0.0], "2026-07-01T00:00:00Z")
            .unwrap();
        db.insert_voiceprint("vp1", "m2", 0, None, &[0.0f32, 1.0], "2026-07-01T00:00:00Z")
            .unwrap();
        assert_eq!(db.clear_voiceprints().unwrap(), 2, "both rows cleared");
        assert!(db
            .list_voiceprints_visible(&HashSet::new())
            .unwrap()
            .is_empty());
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
        super::unique_temp_path(&format!("meetnotes-graph-test-{label}"), "sqlite")
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
            model_requested: None,
            model_served: None,
            gateway_host: None,
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
        db.set_folder_locked(folder_id, true, Some(&wrapped))
            .unwrap();
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

        let id1 = db
            .upsert_entity("Anna Kowalska", EntityKind::Person)
            .unwrap();
        let id2 = db
            .upsert_entity("anna kowalska", EntityKind::Person)
            .unwrap();
        assert_eq!(id1, id2, "case-insensitive dedup → same entity id");

        // First-seen casing is preserved (the lowercase re-insert must NOT overwrite it).
        let ent = db.get_entity(&id1).unwrap().unwrap();
        assert_eq!(ent.name, "Anna Kowalska", "first-seen casing kept");

        // Same name, DIFFERENT kind → a distinct entity row (the (name_ci, kind) unique index).
        let proj = db
            .upsert_entity("Anna Kowalska", EntityKind::Project)
            .unwrap();
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
        assert_eq!(
            anna.mention_count, 2,
            "two distinct meetings, idempotent repeat"
        );
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
        let secret_p = db
            .upsert_entity("Secret Person", EntityKind::Person)
            .unwrap();
        db.add_mention(&secret_p, "sealed1").unwrap();
        // "Shared Project" mentioned in BOTH the open and the sealed meeting.
        let shared = db
            .upsert_entity("Shared Project", EntityKind::Project)
            .unwrap();
        db.add_mention(&shared, "open1").unwrap();
        db.add_mention(&shared, "sealed1").unwrap();

        let empty: HashSet<String> = HashSet::new();

        // BEFORE sealing: both entities present; Shared has count 2.
        let before = db.list_entities_visible(&empty).unwrap();
        assert!(before.iter().any(|n| n.id == secret_p));
        assert_eq!(
            before
                .iter()
                .find(|n| n.id == shared)
                .unwrap()
                .mention_count,
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
        assert!(
            graph.has_hidden,
            "a sealed-not-unlocked folder sets has_hidden"
        );
        assert!(!graph.nodes.iter().any(|n| n.id == secret_p));
        // entity_mentions_visible: the secret entity has zero visible backlinks while sealed.
        assert!(db
            .entity_mentions_visible(&secret_p, &empty)
            .unwrap()
            .is_empty());

        // SESSION-UNLOCK the folder id → the sealed contribution reappears.
        let unlocked = unlocked_set(&["secret"]);
        let nodes_u = db.list_entities_visible(&unlocked).unwrap();
        assert!(
            nodes_u.iter().any(|n| n.id == secret_p),
            "entity reappears once its folder id is in the unlocked set"
        );
        assert_eq!(
            nodes_u
                .iter()
                .find(|n| n.id == shared)
                .unwrap()
                .mention_count,
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
            db.entity_mentions_visible(&secret_p, &unlocked)
                .unwrap()
                .len(),
            1
        );
    }

    /// `/people` CRM GATE: a Person mentioned ONLY in a sealed-and-not-session-unlocked meeting is
    /// ABSENT from `list_people`, and every count on a visible Person reflects VISIBLE sources only.
    /// A Person seen in BOTH an open and a sealed meeting keeps only the open-source counts while the
    /// folder is sealed, and the sealed source's meeting/fact/commitment all reappear once the folder
    /// id is session-unlocked. RED-before-GREEN: drop the `list_entities_visible` filter (or the
    /// per-count gated readers) and the secret person / sealed counts leak.
    #[test]
    fn list_people_excludes_sealed_person_and_counts_visible_only() {
        use crate::facts::{FactOp, NewFact};
        let db = file_db("people-gate");
        let kek = crate::crypto::random_key().unwrap();
        seed_folder(&db, "secret", "Secret");

        // OPEN meeting: mentions Bob, has an open commitment Bob owns, no facts here.
        seed_note(
            &db,
            "open1",
            "## Action items\n- [ ] Bob — ship the deck 2026-07-01\n",
            None,
        );
        // SEALED meeting: mentions Bob AGAIN + a Secret-Person-only mention, plus Bob's sealed
        // commitment and a sealed fact about Bob.
        seed_note(
            &db,
            "sealed1",
            "## Action items\n- [ ] Bob — secret task 2026-07-05\n",
            Some("secret"),
        );

        let bob = db.upsert_entity("Bob", EntityKind::Person).unwrap();
        db.add_mention(&bob, "open1").unwrap();
        db.add_mention(&bob, "sealed1").unwrap();
        let secret_p = db
            .upsert_entity("Secret Person", EntityKind::Person)
            .unwrap();
        db.add_mention(&secret_p, "sealed1").unwrap();

        // One VISIBLE-source fact about Bob (open meeting) + one SEALED-source fact (sealed meeting).
        let add = |predicate: &str, object: &str, meeting_id: &str| {
            FactOp::Add(NewFact {
                entity_id: bob.clone(),
                subject: "Bob".to_string(),
                predicate: predicate.to_string(),
                object: object.to_string(),
                valid_from: "2026-06-01T00:00:00Z".to_string(),
                recorded_at: "2026-06-01T00:00:00Z".to_string(),
                confidence: 1.0,
                meeting_id: Some(meeting_id.to_string()),
            })
        };
        db.apply_fact_ops(&[add("role", "PM", "open1"), add("team", "Growth", "sealed1")])
            .unwrap();

        // SEAL the folder; session NOT unlocked.
        seal_folder(&db, "secret", &kek);

        let empty: HashSet<String> = HashSet::new();
        let sealed_view = db.list_people(&empty).unwrap();

        // (1) A person known only through the sealed meeting must NOT surface.
        assert!(
            !sealed_view.iter().any(|p| p.id == secret_p),
            "a person mentioned only in a sealed-not-unlocked meeting leaked into /people"
        );

        // (2) Bob surfaces via his OPEN meeting, but every count reflects VISIBLE sources only.
        let bob_card = sealed_view
            .iter()
            .find(|p| p.id == bob)
            .expect("Bob is visible via his open meeting");
        assert_eq!(bob_card.name, "Bob");
        assert_eq!(
            bob_card.meeting_count, 1,
            "only the open meeting counts while sealed"
        );
        assert_eq!(
            bob_card.last_talked.as_deref(),
            Some("2026-06-26T09:00:00Z+open1"),
            "last_talked = the open meeting's start (the sealed one is invisible)"
        );
        assert_eq!(
            bob_card.open_commitment_count, 1,
            "only the open-meeting commitment counts; the sealed task is hidden"
        );
        assert_eq!(
            bob_card.current_fact_count, 1,
            "only the open-source fact counts; the sealed fact is hidden"
        );

        // (3) Session-unlock the folder → the sealed contributions reappear. A real `unlock_folder`
        // decrypts the sealed note markdown back into the plaintext column for the session; the
        // `seal_folder` test helper only seals, so mirror that restore here (the CK->markdown decrypt
        // is exercised by the lock round-trip tests) before asserting the note-derived commitment
        // count. Mentions + facts need no restore — their rows persist through sealing.
        db.restore_note_markdown(
            "sealed1",
            "claude_code",
            "## Action items\n- [ ] Bob — secret task 2026-07-05\n",
        )
        .unwrap();
        let unlocked = unlocked_set(&["secret"]);
        let unlocked_view = db.list_people(&unlocked).unwrap();
        assert!(
            unlocked_view.iter().any(|p| p.id == secret_p),
            "the secret person reappears once its folder id is in the unlocked set"
        );
        let bob_u = unlocked_view.iter().find(|p| p.id == bob).unwrap();
        assert_eq!(
            bob_u.meeting_count, 2,
            "both meetings visible when unlocked"
        );
        assert_eq!(
            bob_u.last_talked.as_deref(),
            Some("2026-06-26T09:00:00Z+sealed1"),
            "last_talked advances to the now-visible sealed meeting (later suffix sorts last)"
        );
        assert_eq!(
            bob_u.open_commitment_count, 2,
            "both commitments visible when unlocked"
        );
        assert_eq!(
            bob_u.current_fact_count, 2,
            "both facts visible when unlocked"
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
            let folder_locked = match db
                .get_meeting(meeting_id)
                .unwrap()
                .and_then(|m| m.folder_id)
            {
                Some(fid) => db
                    .folder_by_id(&fid)
                    .unwrap()
                    .map(|f| f.locked)
                    .unwrap_or(false),
                None => false,
            };
            if !folder_locked {
                crate::export::entity_stub::ensure_entity_backlink(
                    &vault,
                    "People",
                    person,
                    &format!("title-{meeting_id}"),
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
        for (e, m) in [
            (&anna, "m1"),
            (&anna, "m2"),
            (&atlas, "m1"),
            (&atlas, "m2"),
            (&bob, "m1"),
        ] {
            db.add_mention(e, m).unwrap();
        }
        let detail = db
            .build_entity_detail(&anna, &HashSet::new(), 12)
            .unwrap()
            .unwrap();
        assert_eq!(detail.entity.name, "Anna");
        assert_eq!(detail.meetings.len(), 2, "Anna backlinks m1 + m2");
        assert_eq!(
            detail.neighbors.first().unwrap().id,
            atlas,
            "Atlas is the top neighbor (shared 2)"
        );
        assert_eq!(detail.neighbors.first().unwrap().shared_meetings, 2);
        assert!(detail.neighbors.iter().any(|n| n.id == bob));
        // Unknown id → None.
        assert!(db
            .build_entity_detail("nope", &HashSet::new(), 12)
            .unwrap()
            .is_none());
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

        let secret_p = db
            .upsert_entity("Secret Person", EntityKind::Person)
            .unwrap();
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
        assert_eq!(
            detail.meetings.len(),
            1,
            "the (now visible) sealed meeting backlinks"
        );
    }
}
