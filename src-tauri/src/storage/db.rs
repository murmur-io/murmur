use std::path::Path;
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};

use regex::Regex;
use rusqlite::{Connection, OptionalExtension, Row};

use crate::error::{AppError, Result};
use std::collections::HashSet;

use crate::embed::Embedder;
use crate::storage::models::{
    Analytics, BacklinkSource, Commitment, CorrectionRecord, DayCount, DocChunkHit,
    DocOutlineEntry, DocumentInfo, DocumentSummary, EntityKind, GraphNode, Meeting,
    MeetingActionSummary, MeetingStatus, NoteCitation, NoteTemplate, NoteTemplateSection,
    PendingShareAccept, PeopleList, PersonCard, PropertyKind, PropertyValue, RecipeRecord,
    SavedView, SearchHit, StatusCount, StoredTranscriptSegment, TranscriptSegmentHit,
    VisibleSpeakerLabel,
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

/// Return shape of [`Db::reblank_locked_folders_at_rest`]: the locked meetings' audio to
/// re-seal, the rollup vault exports to delete, `(note_id, exported_path)` of every authored note
/// whose session-re-exported `.md` must be reconciled (audit F2, W5 semantics). Sealed-neighbour
/// export cleanup is carried by the independent SQLCipher outbox, not a volatile return value.
pub type LockedAtRestCleanup = (Vec<LockedMeetingAudio>, Vec<String>, Vec<(String, String)>);

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
    /// `pub(crate)` (promoted from private, God-file split) so the meeting-row writers now in
    /// `storage::meetings_store` can format the `status` column cross-file — same widening the
    /// sibling [`EntityKind::as_str`] already carries.
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
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
pub(crate) fn map_err(e: rusqlite::Error) -> AppError {
    AppError::Storage(e.to_string())
}

/// Insert one content-free outbound-share egress row inside the caller's transaction.
///
/// The dispatch id is the durable idempotency key shared with the state-machine write. Keeping
/// both writes in the same transaction means a failed/rolled-back dispatch never leaves a phantom
/// ledger row, while the partial unique index rejects a second non-NULL record for the same
/// dispatch. The row carries metadata only: never a URL, key, title, or note body.
pub(crate) fn insert_share_egress_dispatch_tx(
    tx: &rusqlite::Transaction<'_>,
    ts: i64,
    host: &str,
    kind: &str,
    bytes: usize,
    dispatch_id: &str,
) -> Result<i64> {
    if dispatch_id.trim().is_empty() {
        return Err(AppError::InvalidArg(
            "share egress dispatch id must not be blank".into(),
        ));
    }
    if host.trim().is_empty() {
        return Err(AppError::InvalidArg(
            "share egress host must not be blank".into(),
        ));
    }
    if kind.trim().is_empty() {
        return Err(AppError::InvalidArg(
            "share egress kind must not be blank".into(),
        ));
    }
    let byte_count = i64::try_from(bytes).map_err(|_| {
        AppError::InvalidArg("share egress byte count exceeds SQLite INTEGER range".into())
    })?;
    tx.execute(
        "INSERT INTO share_egress_log (ts, host, kind, byte_count, dispatch_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![ts, host, kind, byte_count, dispatch_id],
    )
    .map_err(map_err)?;
    Ok(tx.last_insert_rowid())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn insert_org_read_egress_dispatch_tx(
    tx: &rusqlite::Transaction<'_>,
    ts: i64,
    host: &str,
    kind: &str,
    dispatch_id: &str,
    org_id: &str,
    doc_id: &str,
    since_seq: u64,
    limit: u32,
) -> Result<i64> {
    if org_id.trim().is_empty() || doc_id.trim().is_empty() || limit == 0 {
        return Err(AppError::InvalidArg(
            "org recovery read witness must be complete".into(),
        ));
    }
    let row_id = insert_share_egress_dispatch_tx(tx, ts, host, kind, 0, dispatch_id)?;
    let since_seq = i64::try_from(since_seq)
        .map_err(|_| AppError::InvalidArg("org recovery cursor exceeds SQLite range".into()))?;
    tx.execute(
        "UPDATE share_egress_log
            SET org_id = ?2, doc_id = ?3, since_seq = ?4, page_limit = ?5
          WHERE id = ?1 AND dispatch_id = ?6",
        rusqlite::params![row_id, org_id, doc_id, since_seq, limit as i64, dispatch_id],
    )
    .map_err(map_err)?;
    Ok(row_id)
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

/// TOCTOU seal re-check for a `documents`-table row (a note OR an uploaded document), run INSIDE the
/// caller's write transaction. The seal (`seal_document`, `blank_sealed_notes_in_folders`'s document
/// leg) blanks the plaintext `text` into `text_blob` (`text=''`, `text_blob` kept) — a
/// session-independent, DB-side sealed-at-rest invariant. When a `lock_folder` commits mid-embed the
/// indexer's slow embedding already ran against the now-stale plaintext; keying the refusal on this
/// invariant (not a caller's `unlocked` snapshot) stops the derived plaintext chunks/vectors from
/// landing at rest behind the lock. Returns `true` ⇒ the caller must refuse the write (rollback via
/// drop). UNSEAL/session-unlock un-blanks `text` before re-indexing, so this reads `false` there and
/// the write proceeds — the same contract the meeting/note-`notes` re-check carries.
pub(crate) fn doc_sealed_at_rest_tx(
    tx: &rusqlite::Transaction<'_>,
    document_id: &str,
) -> Result<bool> {
    tx.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM documents
            WHERE id = ?1
              AND text_blob IS NOT NULL
              AND (text IS NULL OR text = '')
         )",
        rusqlite::params![document_id],
        |r| Ok(r.get::<_, i64>(0)? != 0),
    )
    .map_err(map_err)
}

/// Embed a batch of augmented chunk texts in small sub-batches instead of ONE call sized to the
/// whole meeting/document/topic list. A long meeting or document can produce dozens of chunks
/// (topic chunks merge to a 60s floor — a 1h+ recording can have 40-60; transcript chunks target
/// ~1000 chars each — a 1h+ recording's transcript alone can produce a comparable or larger
/// count), and `CandleBertEmbedder::embed` builds ONE rectangular Candle/Metal tensor per call
/// sized to the whole input batch — so without this, one long meeting/document still drives an
/// unbounded-size Metal burst regardless of any caller-side "how many ITEMS per run" cap
/// (2026-07-13: the launch-freeze fix for topic chunks; the identical gap in transcript/note
/// chunk indexing — the far more common path, since it runs on every recording Stop, not just
/// the startup catch-up — is closed here too, both callers now sharing this one helper).
fn embed_in_sub_batches(embedder: &dyn Embedder, texts: &[String]) -> Result<Vec<Vec<f32>>> {
    embed_in_sub_batches_progress(embedder, texts, &no_embed_progress)
}

/// A per-sub-batch embed-progress callback: `(sub_batches_done, sub_batches_total)`. Used by the
/// document-import path so the FE can render "Embedding k/M" during a large document's embed loop
/// (Brain v3 PR-4, Fix 3). A no-op ([`no_embed_progress`]) is used everywhere else (meeting/topic
/// indexing, tests) so no other caller changes behavior.
pub(crate) type EmbedProgressFn<'a> = dyn Fn(usize, usize) + 'a;

/// The no-op [`EmbedProgressFn`] — the default for every embed caller that doesn't report progress.
pub(crate) fn no_embed_progress(_done: usize, _total: usize) {}

/// [`embed_in_sub_batches`] with a per-sub-batch progress callback. Same sub-batching + pacing; after
/// each sub-batch completes it reports `(done, total)` sub-batch counts. `total` is `1` for the
/// single-call small path so a small document still reports 1/1 at completion.
fn embed_in_sub_batches_progress(
    embedder: &dyn Embedder,
    texts: &[String],
    progress: &EmbedProgressFn<'_>,
) -> Result<Vec<Vec<f32>>> {
    const SUB_BATCH: usize = 8;
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    if texts.len() <= SUB_BATCH {
        let v = embedder.embed_passage(texts)?;
        progress(1, 1);
        return Ok(v);
    }
    let total = texts.len().div_ceil(SUB_BATCH);
    let mut vectors = Vec::with_capacity(texts.len());
    for (i, sub_batch) in texts.chunks(SUB_BATCH).enumerate() {
        if i > 0 {
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        vectors.extend(embedder.embed_passage(sub_batch)?);
        progress(i + 1, total);
    }
    Ok(vectors)
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

/// Content-free local witness for an outbound share whose setup or remote revocation has not yet
/// converged. The owner binding prevents another signed-in account from seeing or retrying it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutboundCleanupPendingRow {
    pub share_id: String,
    pub meeting_id: Option<String>,
    pub document_id: Option<String>,
    pub mode: String,
    pub rev: u32,
    pub created_at: String,
}

/// Thread-safe SQLite wrapper (internal Mutex<rusqlite::Connection>).
pub struct Db {
    conn: Mutex<Connection>,
}

/// Wait accounting for the ONE `Mutex<Connection>` every DB access funnels through.
///
/// The whole app shares a single SQLCipher connection, so a long write (a delete cascade, an org
/// sync) serialises every concurrent read behind it. Before this existed the cost was unobservable:
/// nothing recorded how long anyone waited, so "the app freezes while X runs" could only ever be
/// argued from feel. These counters make the before/after a NUMBER — and they are the reason a
/// contention claim in this repo can be measured instead of asserted.
///
/// Non-PII by construction: counts and microseconds only, never a statement, table, or row.
#[derive(Debug, Default)]
pub(crate) struct DbLockStats {
    contended: std::sync::atomic::AtomicU64,
    uncontended: std::sync::atomic::AtomicU64,
    total_wait_us: std::sync::atomic::AtomicU64,
    max_wait_us: std::sync::atomic::AtomicU64,
}

impl DbLockStats {
    fn record_wait(&self, waited: std::time::Duration) {
        use std::sync::atomic::Ordering::Relaxed;
        let us = waited.as_micros().min(u64::MAX as u128) as u64;
        self.contended.fetch_add(1, Relaxed);
        self.total_wait_us.fetch_add(us, Relaxed);
        self.max_wait_us.fetch_max(us, Relaxed);
    }

    fn record_immediate(&self) {
        self.uncontended
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// `(contended, uncontended, total_wait_us, max_wait_us)`.
    pub(crate) fn snapshot(&self) -> (u64, u64, u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        (
            self.contended.load(Relaxed),
            self.uncontended.load(Relaxed),
            self.total_wait_us.load(Relaxed),
            self.max_wait_us.load(Relaxed),
        )
    }
}

/// Process-global because the counters describe the ONE shared connection, not a `Db` value.
///
/// Deliberately NO reset: `cargo test --lib` runs the suite in ONE process, so a reset would race
/// every other test's DB work and the failure would be invisible under `cargo nextest` (a process
/// per test) — which is what CI runs. Assert on a BEFORE/AFTER delta instead; that is correct under
/// both runners.
pub(crate) fn db_lock_stats() -> &'static DbLockStats {
    static STATS: OnceLock<DbLockStats> = OnceLock::new();
    STATS.get_or_init(DbLockStats::default)
}

/// A wait at least this long is worth a line in the log: it is long enough for a person to see the
/// UI stop responding.
const SLOW_DB_LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

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
                          // `temp_store = MEMORY` is the load-bearing half of the old B2/B10 pair and STAYS: temp
                          // tables / indices / materialized subqueries live in RAM and are never spilled to an
                          // UNENCRYPTED temp FILE. It must follow `PRAGMA key` on EVERY connection (the keyed handle
                          // is what it hardens). Enforce FK cascades (segments/notes → meetings) and use WAL for
                          // concurrent reads while a write is in progress.
                          //
                          // `cipher_memory_security = ON` was REMOVED here on 2026-08-31, deliberately and with the
                          // owner's decision on the trade-off. What it did: swap SQLite's allocator for SQLCipher's,
                          // which `mlock()`s every allocation and `memset(0)` + `munlock()`s every free (see
                          // `sqlcipher_mem_malloc`/`sqlcipher_mem_free` in the vendored SQLCipher 4.5.7 amalgamation).
                          // SQLite allocates continuously, so that is two syscalls plus a wipe on the hottest path in
                          // the app. Measured on the 399-test `storage::db` module: 124.2 s with it vs 24.0 s without
                          // (5.2x); across the whole 3548-test suite 699.1 s -> 199.1 s together with the dev-profile
                          // SQLCipher optimisation. Production pays the same multiplier on every DB read and write.
                          //
                          // What is GIVEN UP — and it is NOT what it looks like. SQLCipher's OWN buffers (codec key,
                          // HMAC key, keyspec, raw pass, KDF salt, the per-page cipher scratch) are memset + mlock'd
                          // UNCONDITIONALLY by `sqlcipher_malloc`/`sqlcipher_free`, which never read this flag. The
                          // flag gates only SQLite's GENERAL allocator, so what actually loses its wipe is SQLite's
                          // general heap: the page cache each decrypted page is copied into, the VDBE values carrying
                          // note markdown / transcript text / timeline JSON out to a caller, and the in-RAM temp pages
                          // `temp_store = MEMORY` keeps there. Freed heap may therefore retain USER CONTENT until
                          // reused, and — no longer being mlock'd — is swappable.
                          //
                          // One residue this specifically re-opens: `pragma_update` formats the FULL statement
                          // `PRAGMA key = 'x''<64 hex>'''` into a plain `String` and `sqlite3_prepare_v2` copies that
                          // SQL text into SQLite-allocated memory. Those copies used to be wiped on free by the
                          // SQLCipher allocator; now nothing wipes them, so the hex DEK can linger in freed heap. The
                          // `Zeroizing` above covers only our own value string, not rusqlite's statement text.
                          //
                          // What is UNCHANGED: the database stays fully SQLCipher-encrypted at rest; the per-folder
                          // CK/KEK seal is untouched; macOS encrypts swap by default; SQLCipher keeps the DEK in
                          // `ctx->pass` (mlock'd, always wiped on free) for the connection's life anyway; and every
                          // secret WE hold — master KEK, content keys — lives in Rust memory under `Zeroize`, which
                          // this allocator never covered. Reaching any of the above needs process-memory access, and
                          // a sealed-not-unlocked folder still needs the biometric KEK.
        conn.execute_batch(
            "PRAGMA temp_store = MEMORY;
             PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;",
        )
        .map_err(map_err)?;
        // Root-cause fix (2026-07-15, note-save contention bug): this app has SEVERAL real
        // concurrent-write background jobs against the SAME on-disk DB file (brain re-index, the
        // org-feed sync tick, memory consolidation, note autosave, the MCP reader thread's own
        // connection) all contending for SQLite's single writer lock. WAL lets readers proceed
        // concurrently with a writer, but two WRITERS still serialize at the SQLite level — and
        // with NO busy handler installed, a lock collision surfaced IMMEDIATELY as
        // `SQLITE_BUSY`/"database is locked" (mapped by `map_err` to a generic
        // `AppError::Storage`) instead of a brief internal wait. That is what an unfiled-note
        // autosave hitting a concurrent background writer saw as an opaque "Save failed" in the
        // FE. `busy_timeout` installs SQLite's native busy handler on this connection so a
        // blocked writer polls/backs off internally up to the timeout before giving up, instead
        // of erroring on the very first collision. 5s is generous for this app's writer holds
        // (all single, short, local statements/transactions) without risking a genuinely wedged
        // connection hanging the UI thread indefinitely.
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(map_err)?;
        let db = Db {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        Ok(db)
    }

    /// Idempotent CREATE TABLE IF NOT EXISTS migrations. Every schema/data step after the
    /// read-only legacy preflight runs in one outer transaction: a late migration failure must
    /// never leave an earlier table, trigger, column, or backfill looking successfully installed.
    pub fn migrate(&self) -> Result<()> {
        let mut conn = self.lock();
        let ask_dispatch_state_existed = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='ask_dispatch_state'",
                [],
                |_| Ok(()),
            )
            .optional()
            .map_err(map_err)?
            .is_some();
        let dispatch_stamp_column_exists = |table: &str, column: &str| -> Result<bool> {
            conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name=?2",
                rusqlite::params![table, column],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count != 0)
            .map_err(map_err)
        };
        if ask_dispatch_state_existed {
            let (row_count, valid_count) = conn
                .query_row(
                    "SELECT COUNT(*),
                            COALESCE(SUM(CASE WHEN singleton=1
                                               AND typeof(generation)='integer'
                                               AND generation>=0
                                              THEN 1 ELSE 0 END),0)
                       FROM ask_dispatch_state",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .map_err(map_err)?;
            if (row_count, valid_count) != (1, 1) {
                return Err(AppError::Storage(
                    "Ask dispatch generation is unavailable".into(),
                ));
            }
        } else {
            if dispatch_stamp_column_exists("ask_conversations", "ask_dispatch_generation")?
                || dispatch_stamp_column_exists(
                    "dashboard_tiles",
                    "living_answer_ask_dispatch_generation",
                )?
            {
                return Err(AppError::Storage(
                    "Ask dispatch generation is unavailable".into(),
                ));
            }
        }
        let conn = conn.transaction().map_err(map_err)?;
        if !ask_dispatch_state_existed {
            conn.execute_batch(
                "CREATE TABLE ask_dispatch_state (
                   singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                   generation INTEGER NOT NULL CHECK(typeof(generation)='integer' AND generation >= 0)
                 );
                 INSERT INTO ask_dispatch_state(singleton,generation) VALUES (1,0);",
            )
            .map_err(map_err)?;
        }
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
             CREATE INDEX IF NOT EXISTS idx_meetings_started_at ON meetings(started_at);
             CREATE TABLE IF NOT EXISTS segments (
               meeting_id TEXT NOT NULL,
               idx INTEGER NOT NULL,
               start_s REAL NOT NULL,
               end_s REAL NOT NULL,
               text TEXT NOT NULL,
               echo_suppressed INTEGER NOT NULL DEFAULT 0,
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
             -- User-authored NOTE TEMPLATES (Granola-style named sections). CONTENT-FREE,
             -- single-user metadata (mirrors `saved_recipes`): a note SHAPE only — tone +
             -- ordered sections (JSON) + extra front-matter keys (JSON) — never meeting content,
             -- so it is not visibility-gated. Selected by id via the note-style selector and
             -- rendered into the summarizer system prompt by `summarize::template::build_template`.
             CREATE TABLE IF NOT EXISTS note_templates (
               id TEXT PRIMARY KEY,
               name TEXT NOT NULL,
               tone TEXT NOT NULL DEFAULT '',
               sections TEXT NOT NULL DEFAULT '[]',
               extra_frontmatter_keys TEXT NOT NULL DEFAULT '[]',
               created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS saved_views (
               id TEXT PRIMARY KEY,
               scope TEXT NOT NULL,
               name TEXT NOT NULL,
               layout TEXT NOT NULL,
               config TEXT NOT NULL,
               sort_order INTEGER NOT NULL DEFAULT 0,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_saved_views_scope ON saved_views(scope, sort_order);
             CREATE TABLE IF NOT EXISTS note_folder_schemas (
               folder_id TEXT PRIMARY KEY REFERENCES folders(id) ON DELETE CASCADE,
               schema_json TEXT NOT NULL DEFAULT '[]',
               updated_at INTEGER NOT NULL
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
             -- Durable Ask Brain conversations. These are deliberately separate from the
             -- recording-time assistant_interactions schema: they are normalized, scope-bound,
             -- bounded on every reader, and v1 content is conservatively global-derived because
             -- current Ask provenance does not provide typed IDs for every accessed source.
             CREATE TABLE IF NOT EXISTS ask_history_state (
               singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
               visibility_generation INTEGER NOT NULL CHECK(visibility_generation >= 0)
             );
             INSERT OR IGNORE INTO ask_history_state(singleton, visibility_generation)
               VALUES (1, 0);
             -- Global authorization epoch for every Ask/provider dispatch. This is deliberately
             -- separate from content visibility and dashboard generations: provider identity,
             -- endpoint, model or consent may change while the source corpus stays byte-identical.
             CREATE TABLE IF NOT EXISTS ask_dispatch_state (
               singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
               generation INTEGER NOT NULL CHECK(typeof(generation)='integer' AND generation >= 0)
             );
             CREATE TABLE IF NOT EXISTS ask_conversations (
               id TEXT PRIMARY KEY,
               scope_kind TEXT NOT NULL CHECK(scope_kind IN ('vault', 'note', 'meeting')),
               scope_ref TEXT,
               title TEXT NOT NULL,
               selected_sources_json TEXT NOT NULL DEFAULT '[]',
               provenance_mode TEXT NOT NULL CHECK(provenance_mode = 'globalDerived'),
               visibility_generation INTEGER NOT NULL DEFAULT 0 CHECK(visibility_generation >= 0),
               revision INTEGER NOT NULL DEFAULT 0 CHECK(revision >= 0),
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               CHECK((scope_kind = 'vault' AND scope_ref IS NULL) OR
                     (scope_kind IN ('note', 'meeting') AND length(scope_ref) > 0))
             );
             CREATE INDEX IF NOT EXISTS idx_ask_conversations_scope_updated
               ON ask_conversations(scope_kind, scope_ref, updated_at DESC, id DESC);
             CREATE TABLE IF NOT EXISTS ask_conversation_messages (
               id TEXT PRIMARY KEY,
               conversation_id TEXT NOT NULL,
               ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
               role TEXT NOT NULL CHECK(role IN ('user', 'assistant')),
               content TEXT NOT NULL CHECK(length(trim(content)) > 0),
               sources_json TEXT NOT NULL DEFAULT '[]',
               citations_json TEXT NOT NULL DEFAULT '[]',
               created_at TEXT NOT NULL,
               UNIQUE(conversation_id, ordinal),
               FOREIGN KEY (conversation_id) REFERENCES ask_conversations(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_ask_conversation_messages_order
               ON ask_conversation_messages(conversation_id, ordinal);
             CREATE TABLE IF NOT EXISTS ask_conversation_dependencies (
               conversation_id TEXT NOT NULL,
               dependency_kind TEXT NOT NULL CHECK(dependency_kind = 'folder'),
               dependency_ref TEXT NOT NULL CHECK(length(dependency_ref) > 0),
               PRIMARY KEY (conversation_id, dependency_kind, dependency_ref),
               FOREIGN KEY (conversation_id) REFERENCES ask_conversations(id) ON DELETE CASCADE
             );
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

             -- MEM-1 reversible memory-import supersede. When an import_memories run reconciles a
             -- pasted export against EXISTING user facts and a same-key/different-object collision
             -- CLOSES a pre-existing OPEN fact anchored to ANOTHER meeting, we record the link here so
             -- deleting the synthetic Memory-Import meeting (the undo affordance) can REOPEN those
             -- facts. Without it, delete_meeting purge_user_facts_tx deletes only the import's
             -- OWN Adds (meeting_id = import_id) and the superseded pre-existing facts stay closed
             -- FOREVER — a partial undo that silently loses prior memories. `superseded_valid_to` is
             -- the exact `valid_to` we stamped, so the reopen only reverts OUR closure (idempotent /
             -- conflict-safe). No FK on `superseded_fact_id` (the fact row survives — only its
             -- valid_to changed); the import_meeting_id side is cleaned by `delete_meeting`.
             CREATE TABLE IF NOT EXISTS user_fact_import_supersedes (
               import_meeting_id  TEXT NOT NULL,
               superseded_fact_id TEXT NOT NULL,
               superseded_valid_to TEXT NOT NULL,
               PRIMARY KEY (import_meeting_id, superseded_fact_id)
             );
             CREATE INDEX IF NOT EXISTS idx_ufis_import
               ON user_fact_import_supersedes(import_meeting_id);

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
             -- THE FACT LEDGER, SEALED. Facts, user facts and supersessions are DELETED when a
             -- folder seals, because their `subject`/`predicate`/`object` are plaintext derived
             -- from the meeting -- keeping them readable at rest would defeat the seal. But they
             -- were never re-derived on unlock either, so locking a folder for an afternoon
             -- destroyed the ledger permanently.
             --
             -- Re-extraction is not a recovery: it costs a provider call, and it CANNOT restore the
             -- bitemporal history. `valid_from`/`valid_to` and the supersession chain record WHEN a
             -- fact stopped being true, and nothing in the current note text says that. Knowledge
             -- diff, dossiers and the entity timeline are exactly the surfaces built on that
             -- history.
             --
             -- So the ledger is SEALED-AND-RESTORED like the note markdown, not purged like a chunk:
             -- one ciphertext per meeting holding its rows, written (verify-before-destroy) before
             -- the rows are deleted, and re-inserted on unlock. The rows themselves still leave the
             -- database on seal, so the at-rest guarantee is exactly what it was.
             CREATE TABLE IF NOT EXISTS sealed_fact_ledgers (
               meeting_id TEXT PRIMARY KEY,
               data_blob  BLOB NOT NULL,
               sealed_at  TEXT NOT NULL,
               FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
             );
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
        // Ask history hardening: additive generation + optimistic revision columns for databases
        // created before durable history gained crash-proof invalidation and resume CAS.
        Self::add_column_if_missing(
            &conn,
            "ask_conversations",
            "visibility_generation",
            "INTEGER NOT NULL DEFAULT 0 CHECK(visibility_generation >= 0)",
        )?;
        Self::add_column_if_missing(
            &conn,
            "ask_conversations",
            "revision",
            "INTEGER NOT NULL DEFAULT 0 CHECK(revision >= 0)",
        )?;
        // Guarded ALTERs — notes gain a folder association + a sealed-content blob (AES-GCM
        // markdown when the folder is locked; NULL when open). migrate() re-runs each launch and
        // `ALTER ADD COLUMN` errors if the column already exists, so check pragma_table_info first.
        Self::add_column_if_missing(&conn, "notes", "folder_id", "TEXT")?;
        Self::add_column_if_missing(&conn, "notes", "content_blob", "BLOB")?;
        // 2026-07-13 perf audit (MODERATE): `folder_id` has no index, so `notes_in_folder`/
        // `meeting_ids_in_folder` — called on every folder lock/unlock/relock — full-scan the
        // whole `notes` table. `CREATE INDEX IF NOT EXISTS` after the guarded ALTER above (the
        // column must exist first); safe on an existing index (no-op) and additive-only per the
        // migration discipline (no data touched).
        conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_notes_folder_id ON notes(folder_id);")
            .map_err(map_err)?;
        // Phase B: 2-way stream attribution ("me"/"others"). Guarded ALTER (same idempotent
        // pattern) — NULL for legacy rows transcribed before dual-stream, which read back as
        // `speaker: None` (unattributed). NOT per-remote-person diarization; see types::Segment.
        Self::add_column_if_missing(&conn, "segments", "speaker", "TEXT")?;
        // Explicit presentation-only echo provenance. It is written only by the ingest path after
        // measured acoustic leak evidence. Legacy rows default visible; read-time renderers never
        // infer this flag from text or timestamps.
        Self::add_column_if_missing(
            &conn,
            "segments",
            "echo_suppressed",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
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
        // Workspace filing is owned by the RECORDING, not by a provider note that may not exist
        // yet.  Legacy databases stored placement only on `notes.folder_id`; backfill only when all
        // non-NULL provider rows agree. Ambiguous rows deliberately stay NULL and continue through
        // the legacy visibility fallback until the user files them explicitly.
        Self::add_column_if_missing(&conn, "meetings", "folder_id", "TEXT")?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_meetings_folder_id ON meetings(folder_id);
             UPDATE meetings
                SET folder_id = (
                      SELECT MIN(n.folder_id) FROM notes n
                       WHERE n.meeting_id = meetings.id AND n.folder_id IS NOT NULL
                    )
              WHERE folder_id IS NULL
                AND 1 = (
                      SELECT COUNT(DISTINCT n.folder_id) FROM notes n
                       WHERE n.meeting_id = meetings.id AND n.folder_id IS NOT NULL
                    );
             UPDATE notes
                SET folder_id = (SELECT m.folder_id FROM meetings m WHERE m.id=notes.meeting_id)
              WHERE EXISTS (SELECT 1 FROM meetings m
                             WHERE m.id=notes.meeting_id AND m.folder_id IS NOT NULL)
                AND folder_id IS NOT (SELECT m.folder_id FROM meetings m
                                      WHERE m.id=notes.meeting_id);",
        )
        .map_err(map_err)?;
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
        // Export-collision guard (2026-07-16): `exported_hash` = SHA-256 (lowercase hex) of the
        // EXACT markdown Murmur last wrote to the exported vault `.md` at `exported_path`. Before
        // every full DB-derived overwrite, the guard compares the CURRENT file bytes against this
        // baseline — a mismatch means an EXTERNAL edit (the user or their own vault-side agent),
        // which is preserved as a sibling file instead of silently clobbered
        // (`export::preserve_external_edit_if_any`). NULL = legacy row exported before the guard
        // shipped (grandfathered: no sibling until the next Murmur write stamps a baseline).
        // NON-CONTENT metadata (a digest, never words): like `exported_path` it is not
        // sealed/blanked — it rides the SQLCipher-at-rest layer. Additive + guarded (idempotent).
        Self::add_column_if_missing(&conn, "notes", "exported_hash", "TEXT")?;
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
        // Vault Audit v1 — deterministic vault-health findings + run bookkeeping (see
        // `crate::audit`). Additive + guarded so migrate() stays idempotent.
        Self::migrate_audit(&conn)?;
        // Recording durability v2 — content-free, SQLCipher-protected ownership/lease and artifact
        // proof ledger. Capture bytes stay in stable-handle files; this table stores only identity,
        // length, digest and forward-only lifecycle state.
        Self::migrate_recording_generations(&conn)?;
        // M6 Shared Brain — the local org state + the outbound org-share state machine (mirrors
        // `outbound_shares`). NOT the org_items/chunks ingest tables (a later slice owns those).
        // Additive + guarded so migrate() stays idempotent.
        Self::migrate_orgs(&conn)?;
        // M6 Shared Brain (sync/ingest slice) — the DECRYPTED-REPLICA + local RETRIEVAL tables for
        // the org feed: `org_items` (the decrypted replica of each feed item), `org_chunks` (its
        // plaintext chunks), `org_vec_chunks` (vec0 **int8[EMBED_DIM]** KNN — 3.7× smaller than f32,
        // holds in-budget at 300k chunks per the scale spike), and `fts_org_chunks` (keyword leg).
        // Additive + guarded so migrate() stays idempotent. Runs AFTER migrate_orgs.
        Self::migrate_org_ingest(&conn)?;
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
        // Org Tasks — structured org-only projections plus device-private references. Task source
        // documents use `kind='task'` in a hidden always-open folder, so they stay out of Notes and
        // the vault while reusing the crash-safe stable-document share state machine.
        Self::migrate_tasks(&conn)?;
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS org_shares_closure_insert_guard
             BEFORE INSERT ON org_shares
             WHEN NEW.state NOT IN ('revoke_pending','revoked') AND (
               EXISTS(SELECT 1 FROM org_share_closures c WHERE
                 (c.scope_kind='meeting' AND c.scope_id=NEW.meeting_id) OR
                 (c.scope_kind='document' AND c.scope_id=NEW.document_id)) OR
               EXISTS(SELECT 1 FROM org_share_closures c WHERE c.scope_kind='folder' AND (
                 (NEW.document_id IS NOT NULL AND EXISTS(
                   SELECT 1 FROM documents d WHERE d.id=NEW.document_id AND d.folder_id=c.scope_id)) OR
                 (NEW.meeting_id IS NOT NULL AND EXISTS(
                   SELECT 1 FROM notes n WHERE n.meeting_id=NEW.meeting_id
                    AND n.folder_id=c.scope_id))))
             ) BEGIN SELECT RAISE(ABORT,'org share source is closing'); END;
             CREATE TRIGGER IF NOT EXISTS org_shares_closure_update_guard
             BEFORE UPDATE ON org_shares
             WHEN NEW.state NOT IN ('revoke_pending','revoked') AND (
               EXISTS(SELECT 1 FROM org_share_closures c WHERE
                 (c.scope_kind='meeting' AND c.scope_id=NEW.meeting_id) OR
                 (c.scope_kind='document' AND c.scope_id=NEW.document_id)) OR
               EXISTS(SELECT 1 FROM org_share_closures c WHERE c.scope_kind='folder' AND (
                 (NEW.document_id IS NOT NULL AND EXISTS(
                   SELECT 1 FROM documents d WHERE d.id=NEW.document_id AND d.folder_id=c.scope_id)) OR
                 (NEW.meeting_id IS NOT NULL AND EXISTS(
                   SELECT 1 FROM notes n WHERE n.meeting_id=NEW.meeting_id
                    AND n.folder_id=c.scope_id))))
             ) BEGIN SELECT RAISE(ABORT,'org share source is closing'); END;
             ",
        )
        .map_err(map_err)?;
        // Canonical meeting placement is now owned by `meetings.folder_id`, including the
        // pre-note interval. The legacy guards above remain in place for additive migration
        // safety; these v2 guards close the canonical path without DROP/recreate and retain the
        // note-derived fallback only for historical NULL canonical rows.
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS org_shares_closure_insert_guard_v2
             BEFORE INSERT ON org_shares
             WHEN NEW.state NOT IN ('revoke_pending','revoked') AND
               NEW.meeting_id IS NOT NULL AND EXISTS(
                 SELECT 1 FROM meetings m
                  JOIN org_share_closures c ON c.scope_kind='folder'
                   AND (m.folder_id=c.scope_id OR (m.folder_id IS NULL AND EXISTS(
                     SELECT 1 FROM notes n WHERE n.meeting_id=m.id AND n.folder_id=c.scope_id)))
                 WHERE m.id=NEW.meeting_id
               )
             BEGIN SELECT RAISE(ABORT,'org share source is closing'); END;
             CREATE TRIGGER IF NOT EXISTS org_shares_closure_update_guard_v2
             BEFORE UPDATE ON org_shares
             WHEN NEW.state NOT IN ('revoke_pending','revoked') AND
               NEW.meeting_id IS NOT NULL AND EXISTS(
                 SELECT 1 FROM meetings m
                  JOIN org_share_closures c ON c.scope_kind='folder'
                   AND (m.folder_id=c.scope_id OR (m.folder_id IS NULL AND EXISTS(
                     SELECT 1 FROM notes n WHERE n.meeting_id=m.id AND n.folder_id=c.scope_id)))
                 WHERE m.id=NEW.meeting_id
               )
             BEGIN SELECT RAISE(ABORT,'org share source is closing'); END;",
        )
        .map_err(map_err)?;
        // Freeze the exact plaintext source while a destructive revoke is in flight: a cleanup may
        // either revoke+delete the snapshot or observe a prior edit and abandon, but it can never
        // revoke an old snapshot and then keep a newly-edited source locally.
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS closing_document_content_update_guard
             BEFORE UPDATE OF folder_id,name,kind,title,text,created_at ON documents
             WHEN EXISTS(SELECT 1 FROM org_share_closures c WHERE
               c.scope_kind='document' AND c.scope_id=OLD.id)
             BEGIN SELECT RAISE(ABORT,'document source is closing'); END;
             CREATE TRIGGER IF NOT EXISTS closing_meeting_note_insert_guard
             BEFORE INSERT ON notes
             WHEN EXISTS(SELECT 1 FROM org_share_closures c WHERE
               c.scope_kind='meeting' AND c.scope_id=NEW.meeting_id)
             BEGIN SELECT RAISE(ABORT,'meeting source is closing'); END;
             CREATE TRIGGER IF NOT EXISTS closing_meeting_note_update_guard
             BEFORE UPDATE OF folder_id,markdown,created_at,provider_id ON notes
             WHEN EXISTS(SELECT 1 FROM org_share_closures c WHERE
               c.scope_kind='meeting' AND c.scope_id=OLD.meeting_id)
             BEGIN SELECT RAISE(ABORT,'meeting source is closing'); END;
             CREATE TRIGGER IF NOT EXISTS closing_meeting_note_delete_guard
             BEFORE DELETE ON notes
             WHEN EXISTS(SELECT 1 FROM org_share_closures c WHERE
               c.scope_kind='meeting' AND c.scope_id=OLD.meeting_id)
               AND EXISTS(SELECT 1 FROM meetings m WHERE m.id=OLD.meeting_id)
             BEGIN SELECT RAISE(ABORT,'meeting source is closing'); END;
             CREATE TRIGGER IF NOT EXISTS closing_meeting_title_update_guard
             BEFORE UPDATE OF title ON meetings
             WHEN EXISTS(SELECT 1 FROM org_share_closures c WHERE
               c.scope_kind='meeting' AND c.scope_id=OLD.id)
             BEGIN SELECT RAISE(ABORT,'meeting source is closing'); END;
             CREATE TRIGGER IF NOT EXISTS closing_document_identity_update_guard_v2
             BEFORE UPDATE OF name,kind ON documents
             WHEN EXISTS(SELECT 1 FROM org_share_closures c WHERE
               c.scope_kind='document' AND c.scope_id=OLD.id)
             BEGIN SELECT RAISE(ABORT,'document source is closing'); END;",
        )
        .map_err(map_err)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS org_source_versions (
               source_kind TEXT NOT NULL CHECK(source_kind IN ('meeting','document')),
               source_id TEXT NOT NULL,
               version INTEGER NOT NULL DEFAULT 0 CHECK(version >= 0),
               PRIMARY KEY(source_kind, source_id)
             );",
        )
        .map_err(map_err)?;
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS org_share_note_source_version_au
               AFTER UPDATE OF markdown, created_at, provider_id ON notes
               WHEN OLD.markdown IS NOT NEW.markdown OR OLD.created_at IS NOT NEW.created_at
                 OR OLD.provider_id IS NOT NEW.provider_id
               BEGIN
                 UPDATE org_shares SET source_version = source_version + 1
                  WHERE meeting_id = NEW.meeting_id;
                 INSERT INTO org_source_versions(source_kind,source_id,version)
                   VALUES('meeting',NEW.meeting_id,1)
                   ON CONFLICT(source_kind,source_id) DO UPDATE SET version=version+1;
               END;
             CREATE TRIGGER IF NOT EXISTS org_share_note_source_version_ai AFTER INSERT ON notes
               BEGIN
                 UPDATE org_shares SET source_version = source_version + 1
                  WHERE meeting_id = NEW.meeting_id;
                 INSERT INTO org_source_versions(source_kind,source_id,version)
                   VALUES('meeting',NEW.meeting_id,1)
                   ON CONFLICT(source_kind,source_id) DO UPDATE SET version=version+1;
               END;
             CREATE TRIGGER IF NOT EXISTS org_share_note_source_version_ad AFTER DELETE ON notes
               BEGIN
                 UPDATE org_shares SET source_version = source_version + 1
                  WHERE meeting_id = OLD.meeting_id;
                 INSERT INTO org_source_versions(source_kind,source_id,version)
                   VALUES('meeting',OLD.meeting_id,1)
                   ON CONFLICT(source_kind,source_id) DO UPDATE SET version=version+1;
               END;
             CREATE TRIGGER IF NOT EXISTS org_share_meeting_source_version_au
               AFTER UPDATE OF title ON meetings WHEN OLD.title IS NOT NEW.title
               BEGIN
                 UPDATE org_shares SET source_version = source_version + 1
                  WHERE meeting_id = NEW.id;
                 INSERT INTO org_source_versions(source_kind,source_id,version)
                   VALUES('meeting',NEW.id,1)
                   ON CONFLICT(source_kind,source_id) DO UPDATE SET version=version+1;
               END;",
        )
        .map_err(map_err)?;
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
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS org_share_document_source_version_au
               AFTER UPDATE OF title, text ON documents
               WHEN OLD.title IS NOT NEW.title OR OLD.text IS NOT NEW.text
               BEGIN
                 UPDATE org_shares SET source_version = source_version + 1 WHERE document_id = NEW.id;
                 INSERT INTO org_source_versions(source_kind,source_id,version) VALUES('document',NEW.id,1)
                   ON CONFLICT(source_kind,source_id) DO UPDATE SET version=version+1;
               END;
             CREATE TRIGGER IF NOT EXISTS org_share_document_source_version_ai AFTER INSERT ON documents
               BEGIN
                 INSERT INTO org_source_versions(source_kind,source_id,version) VALUES('document',NEW.id,1)
                   ON CONFLICT(source_kind,source_id) DO UPDATE SET version=version+1;
               END;
             CREATE TRIGGER IF NOT EXISTS org_share_document_source_version_ad AFTER DELETE ON documents
               BEGIN
                 UPDATE org_shares SET source_version = source_version + 1 WHERE document_id = OLD.id;
                 INSERT INTO org_source_versions(source_kind,source_id,version) VALUES('document',OLD.id,1)
                   ON CONFLICT(source_kind,source_id) DO UPDATE SET version=version+1;
               END;
             CREATE TRIGGER IF NOT EXISTS org_share_document_identity_source_version_au_v2
               AFTER UPDATE OF folder_id,name,kind,created_at ON documents
               WHEN OLD.folder_id IS NOT NEW.folder_id OR OLD.name IS NOT NEW.name
                 OR OLD.kind IS NOT NEW.kind OR OLD.created_at IS NOT NEW.created_at
               BEGIN
                 UPDATE org_shares SET source_version=source_version+1 WHERE document_id=NEW.id;
                 INSERT INTO org_source_versions(source_kind,source_id,version)
                   VALUES('document',NEW.id,1)
                   ON CONFLICT(source_kind,source_id) DO UPDATE SET version=version+1;
               END;",
        )
        .map_err(map_err)?;
        // Export-collision guard (2026-07-16): the authored-note twin of `notes.exported_hash` —
        // the SHA-256 (lowercase hex) of the text Murmur last wrote to this note's exported vault
        // `.md`. Refreshed on every vault (re)export; NULL for legacy rows (grandfathered). See the
        // `notes.exported_hash` migration comment for the full contract. Additive + guarded.
        Self::add_column_if_missing(&conn, "documents", "exported_hash", "TEXT")?;
        // NOTES feature — separate the Notes folder tree from the Meetings tree. `kind` defaults
        // 'meeting' so every existing folder + all meeting behavior stays byte-identical; note
        // folders are created with kind='note'. Lock/seal/CK machinery is folder-id-keyed and
        // kind-agnostic → reused verbatim. Additive + guarded (idempotent).
        Self::add_column_if_missing(&conn, "folders", "kind", "TEXT NOT NULL DEFAULT 'meeting'")?;
        // ADDITIVE (2026-07-14): `is_root` marks the ONE reserved always-open note-folder that backs
        // the "Notes" section root — new UNFILED notes land there, it can never be locked, and the FE
        // hides it from the folder tree (it IS the section, not a nested child). Exactly one row has
        // is_root=1 (see `ensure_notes_root`). Guarded/idempotent; every existing folder defaults to 0.
        Self::add_column_if_missing(&conn, "folders", "is_root", "INTEGER NOT NULL DEFAULT 0")?;
        // WORKSPACE HIERARCHY (2026-08-22) — the CONTAINER LEVEL. A Project is a `folders` row with
        // `level='project'` and `parent_id IS NULL`; a Folder is `level='folder'` under one. Reusing
        // `folders` rather than a new table is the load-bearing decision of the hierarchy design:
        // `commands/lock.rs::lock_folder_inner_with_visibility_notice` seals a container's notes,
        // documents, attachments, transcript, timeline and audio with NO predicate on `folders.kind`,
        // so a project row inherits the whole verified seal machinery — and because a project lock
        // cascades by locking each child folder in its own right, every item's `folder_id` still
        // points at a row whose `locked` bit is correct. That is why `visibility_clause`, all 47
        // `*_visible` readers and every AAD binding (`aad_wrapped_ck`/`aad_content`/…, all of which
        // bind `folder=<id>`) stay UNTOUCHED by the hierarchy. See
        // `docs/superpowers/specs/2026-08-22-workspace-hierarchy-design.md` §2.
        //
        // `emoji`/`tint` mirror the columns `dashboards` already carries, so project identity reuses
        // the existing visual vocabulary. `position` is new because `folders` has NO ordering column
        // at all today (`list_folders` falls back to `created_at, name`).
        //
        // All four are additive + guarded, so migrate() stays idempotent and no existing row's
        // `path`, `locked`, `wrapped_key` or any `*_blob` column is read or written. The DEFAULT
        // 'folder' means every pre-existing row keeps behaving exactly as it does today until the
        // separate `hierarchy_v1` data migration runs.
        // The CHECK is the enforcement, not the comment: without it any string persists, and both the
        // tree reader and every later consumer would have to defend against a `level` nobody defined.
        // SQLite permits a CHECK on ADD COLUMN (unlike PRIMARY KEY/UNIQUE) and applies it to new
        // writes only, so an existing row is never re-validated and the ALTER cannot fail on real data.
        Self::add_column_if_missing(
            &conn,
            "folders",
            "level",
            "TEXT NOT NULL DEFAULT 'folder' CHECK (level IN ('project', 'folder'))",
        )?;
        Self::add_column_if_missing(&conn, "folders", "emoji", "TEXT")?;
        Self::add_column_if_missing(&conn, "folders", "tint", "TEXT")?;
        Self::add_column_if_missing(&conn, "folders", "position", "INTEGER NOT NULL DEFAULT 0")?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_folders_level ON folders(level);
             CREATE INDEX IF NOT EXISTS idx_folders_parent_position
               ON folders(parent_id, position);",
        )
        .map_err(map_err)?;
        // The one-time adoption of every existing container into a default project. Runs here,
        // immediately after the columns it depends on exist, and before any content surface.
        Self::migrate_hierarchy_v1(&conn)?;
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
               owner_user_id TEXT,
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
               byte_count INTEGER NOT NULL DEFAULT 0,
               dispatch_id TEXT,
               org_id      TEXT,
               doc_id      TEXT,
               since_seq   INTEGER,
               page_limit  INTEGER
             );",
        )
        .map_err(map_err)?;
        Self::add_column_if_missing(&conn, "share_egress_log", "dispatch_id", "TEXT")?;
        Self::add_column_if_missing(&conn, "share_egress_log", "org_id", "TEXT")?;
        Self::add_column_if_missing(&conn, "share_egress_log", "doc_id", "TEXT")?;
        Self::add_column_if_missing(&conn, "share_egress_log", "since_seq", "INTEGER")?;
        Self::add_column_if_missing(&conn, "share_egress_log", "page_limit", "INTEGER")?;
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_share_egress_log_dispatch_id
               ON share_egress_log(dispatch_id)
             WHERE dispatch_id IS NOT NULL",
            [],
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
        Self::add_column_if_missing(&conn, "outbound_shares", "owner_user_id", "TEXT")?;
        // Exact content/delete dispatch witness for the 1:1 share socket boundary. Commitments are
        // SHA-256 values over encrypted wire data and the local source lifecycle tuple; no title,
        // plaintext, URL fragment, key, recipient address, or source id enters the egress ledger.
        Self::add_column_if_missing(&conn, "outbound_shares", "dispatch_id", "TEXT")?;
        Self::add_column_if_missing(&conn, "outbound_shares", "content_commitment", "BLOB")?;
        Self::add_column_if_missing(&conn, "outbound_shares", "source_commitment", "BLOB")?;
        // The durable close barrier governs every remote sharing mode. Create these guards only
        // after the additive `outbound_shares.document_id` migration exists on both fresh and
        // upgraded databases.
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS outbound_shares_closure_insert_guard
             BEFORE INSERT ON outbound_shares
             WHEN NEW.state NOT IN ('revoke_pending','revoked') AND (
               EXISTS(SELECT 1 FROM org_share_closures c WHERE
                 (c.scope_kind='meeting' AND c.scope_id=NEW.meeting_id) OR
                 (c.scope_kind='document' AND c.scope_id=NEW.document_id)) OR
               EXISTS(SELECT 1 FROM org_share_closures c WHERE c.scope_kind='folder' AND (
                 (NEW.document_id IS NOT NULL AND EXISTS(
                   SELECT 1 FROM documents d WHERE d.id=NEW.document_id AND d.folder_id=c.scope_id)) OR
                 (NEW.meeting_id <> '' AND EXISTS(
                   SELECT 1 FROM notes n WHERE n.meeting_id=NEW.meeting_id
                    AND n.folder_id=c.scope_id))))
             ) BEGIN SELECT RAISE(ABORT,'share source is closing'); END;
             CREATE TRIGGER IF NOT EXISTS outbound_shares_closure_update_guard
             BEFORE UPDATE ON outbound_shares
             WHEN NEW.state NOT IN ('revoke_pending','revoked') AND (
               EXISTS(SELECT 1 FROM org_share_closures c WHERE
                 (c.scope_kind='meeting' AND c.scope_id=NEW.meeting_id) OR
                 (c.scope_kind='document' AND c.scope_id=NEW.document_id)) OR
               EXISTS(SELECT 1 FROM org_share_closures c WHERE c.scope_kind='folder' AND (
                 (NEW.document_id IS NOT NULL AND EXISTS(
                   SELECT 1 FROM documents d WHERE d.id=NEW.document_id AND d.folder_id=c.scope_id)) OR
                 (NEW.meeting_id <> '' AND EXISTS(
                   SELECT 1 FROM notes n WHERE n.meeting_id=NEW.meeting_id
                    AND n.folder_id=c.scope_id))))
             ) BEGIN SELECT RAISE(ABORT,'share source is closing'); END;
             ",
        )
        .map_err(map_err)?;
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS outbound_shares_closure_insert_guard_v2
             BEFORE INSERT ON outbound_shares
             WHEN NEW.state NOT IN ('revoke_pending','revoked') AND NEW.meeting_id <> '' AND EXISTS(
               SELECT 1 FROM meetings m
                JOIN org_share_closures c ON c.scope_kind='folder'
                 AND (m.folder_id=c.scope_id OR (m.folder_id IS NULL AND EXISTS(
                   SELECT 1 FROM notes n WHERE n.meeting_id=m.id AND n.folder_id=c.scope_id)))
               WHERE m.id=NEW.meeting_id
             )
             BEGIN SELECT RAISE(ABORT,'share source is closing'); END;
             CREATE TRIGGER IF NOT EXISTS outbound_shares_closure_update_guard_v2
             BEFORE UPDATE ON outbound_shares
             WHEN NEW.state NOT IN ('revoke_pending','revoked') AND NEW.meeting_id <> '' AND EXISTS(
               SELECT 1 FROM meetings m
                JOIN org_share_closures c ON c.scope_kind='folder'
                 AND (m.folder_id=c.scope_id OR (m.folder_id IS NULL AND EXISTS(
                   SELECT 1 FROM notes n WHERE n.meeting_id=m.id AND n.folder_id=c.scope_id)))
               WHERE m.id=NEW.meeting_id
             )
             BEGIN SELECT RAISE(ABORT,'share source is closing'); END;",
        )
        .map_err(map_err)?;

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

        // First-class Murmur Reminders. This runs after the meetings/notes/segments/documents
        // substrates exist because its derived-suggestion invalidation triggers attach to all four.
        // The durable reminder rows are an independent SQLCipher domain; only the future Smart
        // audit cache/pending-suggestion tables are source-derived and trigger-purged.
        Self::migrate_reminders(&conn)?;

        // Note image attachments — canonical bytes live inside SQLCipher. Folder-locked owners
        // additionally use the per-folder `data_blob` seal managed by the lock lifecycle.
        // Runs after documents + org_items exist so every FK target is available.
        Self::migrate_attachments(&conn)?;

        // Brain v3 PR-3 — the LINK ENGINE table (wikilink/companion/semantic edges between
        // meetings/notes/documents) + its one-time companion backfill. Additive + guarded so
        // migrate() stays idempotent. Runs LAST so the companion backfill can read the (now-migrated)
        // `documents.meeting_id` column. See `migrate_links`.
        Self::migrate_links(&conn)?;
        Self::migrate_filing_projection_journal(&conn)?;
        // Durable exact-body witness for marker cleanup of session-unlocked sealed notes. The
        // filesystem publish happens after the seal transaction; this hash lets its acknowledgement
        // advance the export-integrity baseline without pretending blank at-rest plaintext is the
        // canonical body. Legacy in-flight rows remain NULL and therefore fail closed.
        Self::add_column_if_missing(
            &conn,
            "lock_marker_export_cleanup",
            "expected_hash",
            "TEXT",
        )?;

        // Dashboards — user-composed boards of tiles over EXISTING sources. Additive + guarded so
        // migrate() stays idempotent. Runs last: a tile's `ref_id` points at a meeting/document/
        // entity/… row, so every referenceable table must already exist. NOTHING here stores
        // meeting CONTENT — a tile holds only a kind + a reference, and every tile READ is
        // re-gated at read time (`visibility_clause` / `meeting_is_unlocked`), so a board can
        // never become an ungated back door into a sealed folder.
        Self::migrate_dashboards(&conn)?;
        // Trash — the 30-day recoverable holding area. Additive + guarded. Runs last for the same
        // reason as dashboards: an entry's `source_folder_id` anchors to `folders`, so that table
        // must already exist. NOTHING here is a second copy of live content — a row appears only
        // when the user deletes something, and it is governed by its source folder's lock
        // (`commands::trash::seal_trash_in_folder`), so it can never become an ungated back door
        // into a sealed folder.
        Self::migrate_trash(&conn)?;
        conn.commit().map_err(map_err)?;
        Ok(())
    }

    /// Idempotent DASHBOARD schema (2026-08-03).
    ///
    /// - `dashboards` — one board per row. Cosmetic-only columns (`emoji`, `tint`) plus ordering.
    /// - `dashboard_tiles` — the tiles on a board. `kind` selects the renderer; `ref_id` is the
    ///   OPTIONAL anchor into an existing row (meeting / document / entity id) and is deliberately
    ///   NOT an FK: a tile whose target was deleted must degrade to a "missing source" tile rather
    ///   than cascade-delete a user's board layout. `config` is a small JSON bag for per-kind
    ///   options. Living answers use dedicated backend-owned content/provenance columns instead of
    ///   caller-writable `config`. `position` is a dense integer order.
    ///
    /// The tables carry no source note/transcript/title copy. The Living-answer cache is hydrated
    /// only after its folder stamp + exact corpus witness pass the command-layer gate.
    fn migrate_dashboards(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS dashboards (
               id TEXT PRIMARY KEY,
               title TEXT NOT NULL,
               emoji TEXT,
               tint TEXT,
               pinned INTEGER NOT NULL DEFAULT 0,
               position INTEGER NOT NULL DEFAULT 0,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_dashboards_position ON dashboards(position);
             CREATE TABLE IF NOT EXISTS dashboard_tiles (
               id TEXT PRIMARY KEY,
               dashboard_id TEXT NOT NULL,
               kind TEXT NOT NULL,
               ref_id TEXT,
               title TEXT,
               span INTEGER NOT NULL DEFAULT 4,
               position INTEGER NOT NULL DEFAULT 0,
               config TEXT,
               created_at TEXT NOT NULL,
               FOREIGN KEY (dashboard_id) REFERENCES dashboards(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_dashboard_tiles_board
               ON dashboard_tiles(dashboard_id, position);
             CREATE TABLE IF NOT EXISTS dashboard_context_state (
               dashboard_id TEXT PRIMARY KEY,
               generation INTEGER NOT NULL DEFAULT 0,
               structural_generation INTEGER NOT NULL DEFAULT 0,
               exists_now INTEGER NOT NULL DEFAULT 0
             );
             INSERT OR IGNORE INTO dashboard_context_state (dashboard_id, generation, exists_now)
               SELECT id, 0, 1 FROM dashboards;",
        )
        .map_err(map_err)?;
        Self::add_column_if_missing(conn, "ask_conversations", "dashboard_id", "TEXT")?;
        Self::add_column_if_missing(
            conn,
            "ask_conversations",
            "dashboard_context_generation",
            "INTEGER",
        )?;

        // A dashboard's container, and the blobs its seal writes. Added HERE, after the
        // dashboards DDL above, because `add_column_if_missing` inspects a table that must
        // already exist — placing them with the `folders` columns made the very first launch
        // fail with "no such table: dashboards", which is a refusal to START, not a bad column.
        //
        // `folder_id` is NULLABLE on purpose: every board that exists today is unfiled, and
        // unfiled is a normal state for a board rather than a defect to repair. It is also the
        // board's LOCK anchor — before it, a board had no folder, so no lock could cover it.
        //
        // The blobs hold what the seal takes: the title names the thing, and a tile's `ref_id`
        // plus `config` say which meeting or note the board is built from, which is exactly
        // what a locked folder exists to stop disclosing.
        Self::add_column_if_missing(conn, "dashboards", "folder_id", "TEXT")?;
        // A task's LOCAL placement. Nullable because placement is optional and because every task
        // that predates the hierarchy has none.
        //
        // LOCAL-ONLY, and that is load-bearing: an `org_tasks` row is the SQLCipher projection of
        // an E2EE envelope, and the bytes that egress are `envelope_json`, built from
        // `TaskEnvelope`. This column is not a field of that struct and no code path copies it in,
        // so a user's private folder structure — which is exactly the kind of thing a shared task
        // must not carry to an org — cannot leave the device. Adding it to `TaskEnvelope` would
        // silently make it egress; do not.
        Self::add_column_if_missing(conn, "org_tasks", "container_id", "TEXT")?;
        Self::add_column_if_missing(conn, "dashboards", "title_blob", "BLOB")?;
        Self::add_column_if_missing(conn, "dashboard_tiles", "config_blob", "BLOB")?;

        Self::add_column_if_missing(
            conn,
            "ask_conversations",
            "dashboard_context_digest",
            "TEXT",
        )?;
        Self::add_column_if_missing(
            conn,
            "ask_conversations",
            "ask_dispatch_generation",
            "INTEGER",
        )?;
        Self::add_column_if_missing(
            conn,
            "dashboard_tiles",
            "question_readable_folders_json",
            "TEXT",
        )?;
        Self::add_column_if_missing(
            conn,
            "dashboard_tiles",
            "answer_readable_folders_json",
            "TEXT",
        )?;
        Self::add_column_if_missing(conn, "dashboard_tiles", "living_question", "TEXT")?;
        Self::add_column_if_missing(conn, "dashboard_tiles", "living_answer", "TEXT")?;
        Self::add_column_if_missing(conn, "dashboard_tiles", "living_answered_at", "TEXT")?;
        Self::add_column_if_missing(
            conn,
            "dashboard_tiles",
            "living_answer_context_generation",
            "INTEGER",
        )?;
        Self::add_column_if_missing(
            conn,
            "dashboard_tiles",
            "living_answer_context_digest",
            "TEXT",
        )?;
        Self::add_column_if_missing(
            conn,
            "dashboard_tiles",
            "living_answer_context_budget",
            "INTEGER",
        )?;
        Self::add_column_if_missing(
            conn,
            "dashboard_tiles",
            "living_answer_ask_dispatch_generation",
            "INTEGER",
        )?;
        Self::add_column_if_missing(
            conn,
            "dashboard_context_state",
            "structural_generation",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
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
        // Brain v3 PR-2 — HIERARCHICAL doc chunks (additive, guarded, idempotent). A document's
        // chunks form a 3-level tree in the SAME `doc_chunks` table (rows still ride the exact
        // seal/unseal/purge/gating already proven for the flat layout):
        //   - `level`        0 = leaf (embedded + FTS), 1 = section-parent (FTS + fetch-by-id ONLY,
        //                    NOT embedded — no `doc_vec_chunks` row), 2 = doc-summary/outline
        //                    (embedded + FTS). Legacy/flat rows default to 0.
        //   - `parent_id`    a leaf's L1 section-parent row id (NULL for L1/L2 and legacy leaves).
        //   - `section_path` the heading trail ("A › B") this chunk sits under (NULL when none).
        //   - `page_no`      1-based page/slide (PDF/PPTX); NULL for flow formats.
        // All four are pure PROVENANCE/structure over already-derived plaintext — nothing new is
        // sealed. The vec0 table is UNCHANGED (only L0+L2 rows get a `doc_vec_chunks` entry).
        Self::add_column_if_missing(conn, "doc_chunks", "level", "INTEGER NOT NULL DEFAULT 0")?;
        Self::add_column_if_missing(conn, "doc_chunks", "parent_id", "INTEGER")?;
        Self::add_column_if_missing(conn, "doc_chunks", "section_path", "TEXT")?;
        Self::add_column_if_missing(conn, "doc_chunks", "page_no", "INTEGER")?;
        // `kind` distinguishes an UPLOADED file ('document') from a TYPED brain note ('note'). Additive
        // + guarded so migrate() stays idempotent; legacy rows default to 'document'. Both kinds ride
        // the SAME seal/unseal/purge/gating — `kind` is a presentation split for the Brain page only.
        Self::add_column_if_missing(
            conn,
            "documents",
            "kind",
            "TEXT NOT NULL DEFAULT 'document'",
        )?;
        // Recording-time COMPANION NOTE (2026-07-16): the authoritative, STRUCTURED link from a
        // standalone note (kind='note') back to the meeting it was jotted during. Nullable — only
        // companion notes carry it; every other note/document stays NULL. Drives navigation
        // (by id, never by a fragile title string), the meeting's backlinks, and survives
        // meeting rename/auto-title. Additive + guarded so migrate() stays idempotent; legacy rows
        // default to NULL. NON-content (an id) — rides the SQLCipher-at-rest layer, never sealed.
        Self::add_column_if_missing(conn, "documents", "meeting_id", "TEXT")?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_documents_meeting_id ON documents(meeting_id);",
        )
        .map_err(map_err)?;
        // IMPORT PROVENANCE (2026-08-25): `source` names where a row came from ('notion'; NULL for
        // everything authored inside Murmur) and `external_id` is that source's own stable key —
        // for Notion, the 32-hex page id its export embeds in every filename. Together they make a
        // re-import IDEMPOTENT: a second run UPDATES the row it created last time instead of
        // silently duplicating it, which is the loudest recurring complaint in every Notion
        // importer's issue tracker. The pair is deliberately indexed NON-uniquely and de-duplicated
        // in the application: a partial UNIQUE index is the tighter guarantee, but it changes what
        // `migrate()` accepts on a database that already holds duplicates, and an additive
        // migration must never fail on existing user rows. NON-content (a label + an opaque id) —
        // rides the SQLCipher-at-rest layer, never sealed. Additive + guarded, so migrate() stays
        // idempotent.
        Self::add_column_if_missing(conn, "documents", "source", "TEXT")?;
        Self::add_column_if_missing(conn, "documents", "external_id", "TEXT")?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_documents_source_external
               ON documents(source, external_id);",
        )
        .map_err(map_err)?;
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
            "CREATE VIRTUAL TABLE IF NOT EXISTS fts_doc_chunks USING fts5(
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
             {backfill}"
        );
        // The caller-wide migration transaction owns CREATE + backfill atomicity.
        conn.execute_batch(&batch).map_err(map_err)?;
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
        // The caller-wide migration transaction owns CREATE + one-time backfill atomicity.
        conn.execute_batch(CREATE).map_err(map_err)?;
        conn.execute_batch(
            "INSERT INTO fts_user_facts(rowid, subject, predicate, object)
               SELECT rowid, subject, predicate, object FROM user_facts;",
        )
        .map_err(map_err)?;
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

    // `migrate_briefs` moved to `storage::brief_store` (God-file split) alongside the
    // `brief_schedules` / `brief_runs` CRUD it schemas — still called `Self::migrate_briefs(&conn)`
    // from `migrate()` above, cross-file inherent-impl.

    // `migrate_audit` moved to `storage::audit_store` (God-file split) alongside the
    // `audit_findings` / `audit_runs` CRUD it schemas — still called `Self::migrate_audit(&conn)`
    // from `migrate()` above, cross-file inherent-impl.

    // `migrate_mcp_servers` (the idempotent `mcp_servers` config schema) moved to
    // `storage::mcp_store` (God-file split) — still called above as `Self::migrate_mcp_servers`.

    /// M6 Shared Brain — local org bookkeeping. Two tables, additive + guarded, mirroring the
    /// `outbound_shares` conventions:
    ///
    /// - `org_state` — one row per org the user has JOINED (create/status caches it): the org id,
    ///   display name, the caller's role, join time, the local consent flag, and the last synced feed
    ///   `seq` cursor. No content — just membership metadata.
    /// - `org_shares` — the OUTBOUND share state machine (one row per "Share to Brain" action):
    ///   `queued → uploaded` (published to the feed) or `→ failed`; `revoke_pending → revoked` (server
    ///   tombstone). Carries the local anchor (`meeting_id` XOR `document_id`), the item `kind`, the
    ///   `content_sha256` (self-share dedup key), the server `item_id` once published, and the last
    ///   error string (non-PII). NO note title/body/OCK ever lands here. NOT the org_items/chunks
    ///   ingest tables (a later slice owns the decrypted-replica + retrieval side).
    fn migrate_orgs(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS org_state (
               org_id     TEXT PRIMARY KEY,
               name       TEXT NOT NULL,
               role       TEXT NOT NULL,
               joined_at  TEXT NOT NULL,
               consented  INTEGER NOT NULL DEFAULT 0,
               last_seq   INTEGER NOT NULL DEFAULT 0,
               generation INTEGER NOT NULL DEFAULT 1
             );
             -- The org's member identity keys, as this device learned them. PUBLIC key material
             -- only: `pk_enc`/`pk_sig` are exactly what `POST /v1/keys/lookup` publishes, plus the
             -- fingerprint the safety-word check already shows the user. No OCK, no private key,
             -- no note content ever lands here.
             --
             -- It exists because rotation needs EVERY remaining member's key at once and the only
             -- directory is the email-keyed lookup, capped at 20 calls a day against orgs of up to
             -- 50 members. Learning each key once, at the invite that already looks it up, turns a
             -- rotation from a burst of quota-limited lookups into a local read.
             CREATE TABLE IF NOT EXISTS org_member_keys (
               org_id      TEXT NOT NULL,
               user_id     TEXT NOT NULL,
               email       TEXT,
               pk_enc      BLOB NOT NULL,
               pk_sig      BLOB NOT NULL,
               fingerprint TEXT NOT NULL,
               updated_at  TEXT NOT NULL,
               PRIMARY KEY (org_id, user_id)
             );
             -- One row per org that OWES a key rotation: a member was removed and the new
             -- generation has not been committed yet. Written BEFORE the removal call, so an
             -- interruption anywhere after it is re-drivable rather than silently forgotten --
             -- the difference between a rotation that is merely late and one that never happens,
             -- leaving the removed member holding a working key. Carries no member identity: the
             -- row says only that this org owes a rotation; the members list at drive time says to
             -- whom.
             CREATE TABLE IF NOT EXISTS org_rotation_pending (
               org_id          TEXT PRIMARY KEY,
               requested_at    TEXT NOT NULL,
               attempts        INTEGER NOT NULL DEFAULT 0,
               last_error      TEXT,
               -- When the retry may next run. Without it a debt that can never settle -- a member
               -- whose account was deactivated while their membership stayed active, say -- is
               -- re-driven every 60s forever, and each attempt spends one of the owner's 20 daily
               -- key lookups on the same doomed member. An attempt that LEARNS something resets
               -- this to now, so a slow-but-progressing rotation is never throttled.
               next_attempt_at TEXT
             );
             CREATE TABLE IF NOT EXISTS org_shares (
               id             TEXT PRIMARY KEY,
               org_id         TEXT NOT NULL,
               meeting_id     TEXT,
               document_id    TEXT,
               kind           TEXT NOT NULL,
               title          TEXT,
               rev            INTEGER NOT NULL DEFAULT 1,
               generation     INTEGER NOT NULL DEFAULT 1,
               content_sha256 BLOB,
               item_id        TEXT,
               scrub          INTEGER NOT NULL DEFAULT 1 CHECK(scrub IN (0,1)),
               state          TEXT NOT NULL DEFAULT 'queued'
                              CHECK (state IN ('queued','uploaded','failed','revoke_pending','revoked')),
               last_error     TEXT,
               created_at     TEXT NOT NULL,
               updated_at     TEXT NOT NULL,
               dispatch_id    TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_org_shares_org ON org_shares(org_id);
             CREATE INDEX IF NOT EXISTS idx_org_shares_state ON org_shares(state);
             CREATE INDEX IF NOT EXISTS idx_org_shares_item ON org_shares(item_id);",
        )
        .map_err(map_err)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS org_share_closures (
               scope_kind TEXT NOT NULL CHECK(scope_kind IN ('meeting','document','folder')),
               scope_id TEXT NOT NULL,
               phase TEXT NOT NULL CHECK(phase IN ('closing','closed')),
               created_at TEXT NOT NULL,
               PRIMARY KEY(scope_kind,scope_id)
             );",
        )
        .map_err(map_err)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS org_access_attempts (
               seq INTEGER PRIMARY KEY AUTOINCREMENT,
               dispatch_id TEXT NOT NULL UNIQUE,
               org_id TEXT NOT NULL,
               doc_id TEXT NOT NULL,
               old_access TEXT NOT NULL CHECK(old_access IN ('view','edit')),
               new_access TEXT NOT NULL CHECK(new_access IN ('view','edit')),
               actor_user_id TEXT NOT NULL,
               owner_user_id TEXT NOT NULL,
               state TEXT NOT NULL DEFAULT 'pending'
                 CHECK(state IN ('pending','applied','failed')),
               created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_org_access_attempts_document
               ON org_access_attempts(org_id,doc_id,seq);",
        )
        .map_err(map_err)?;
        Self::add_column_if_missing(conn, "org_shares", "dispatch_id", "TEXT")?;
        Self::add_column_if_missing(
            conn,
            "org_shares",
            "republish_dirty",
            "INTEGER NOT NULL DEFAULT 0 CHECK(republish_dirty >= 0)",
        )?;
        Self::add_column_if_missing(
            conn,
            "org_shares",
            "source_version",
            "INTEGER NOT NULL DEFAULT 0 CHECK(source_version >= 0)",
        )?;
        Self::add_column_if_missing(
            conn,
            "org_shares",
            "republish_deferred",
            "INTEGER NOT NULL DEFAULT 0 CHECK(republish_deferred IN (0,1))",
        )?;
        conn.execute(
            "UPDATE org_shares SET republish_dirty=republish_dirty+1, republish_deferred=0
              WHERE republish_deferred=1 AND state IN ('queued','uploaded','failed')",
            [],
        ).map_err(map_err)?;
        // Per-instance org toggle: which JOINED orgs actually contribute content on THIS install
        // (Settings → Organization). Default enabled (1) so every existing membership stays active
        // pre-upgrade. Guarded/additive per the migration rule.
        Self::add_column_if_missing(
            conn,
            "org_state",
            "context_enabled",
            "INTEGER NOT NULL DEFAULT 1",
        )?;
        // ANTI-ENTROPY RECONCILE CURSOR (2026-07-26). A SECOND, deliberately slow cursor that is
        // INDEPENDENT of the live `last_seq` pull cursor: it restarts from 0 and walks the whole feed
        // in small bounded steps so a record whose `seq` sits BELOW `last_seq` (e.g. a server-side
        // tombstone that never got a fresh seq) is still observed and applied. Additive + guarded, so
        // an already-migrated DB just gains the columns; existing rows start a fresh pass at 0.
        //   - `reconcile_seq`     — how far the slow walk has got in the CURRENT pass (0 = at the start).
        //   - `reconcile_pass_at` — RFC3339 stamp of the last COMPLETED full pass (NULL = never).
        // Neither column is ever read by the live pull; nothing here can rewind `last_seq`.
        Self::add_column_if_missing(
            conn,
            "org_state",
            "reconcile_seq",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        Self::add_column_if_missing(conn, "org_state", "reconcile_pass_at", "TEXT")
    }

    /// M6 Shared Brain (sync/ingest slice) — the local DECRYPTED REPLICA of the org feed + its
    /// RETRIEVAL layer. Additive + guarded so migrate() stays idempotent.
    ///
    /// - `org_items` — one row per feed item, keyed by the SERVER item id. The decrypted
    ///   `title`/`markdown`/`author_hint` (opened from the OCK-sealed envelope), the feed `seq`
    ///   cursor position, the item `rev`/`generation`, and a `content_sha256` (the PLAINTEXT hash,
    ///   for self-share dedup). `tombstoned` = the item was revoked at the server (its chunks are
    ///   evicted but the row is kept as a tombstone so a re-pull is idempotent).
    /// - `org_chunks` — plaintext chunks DERIVED from an item's markdown (the embed source + snippet
    ///   store), 1:1 with `org_vec_chunks` by `id`. CASCADE on the parent item.
    /// - `org_vec_chunks` — the vec0 KNN table, **`int8[EMBED_DIM]`** (the scale spike's load-bearing
    ///   finding: int8 is 3.7× smaller than f32 and holds a 300k-chunk org in the 100–400 ms query
    ///   budget). Values are scalar-quantized from the f32 embedding and bound via `vec_int8(?)`.
    /// - `fts_org_chunks` — external-content FTS5 over `org_chunks.text`, same
    ///   `unicode61 remove_diacritics 2` tokenizer + `_ai`/`_ad`/`_au` trigger trio as the meeting/doc
    ///   indexes, so org text is keyword-retrievable on a DEFAULT install (no e5 model).
    ///
    /// LOCK-DOMAIN NOTE (spec §"Trust model"): org items are DELIBERATELY org-disclosed content living
    /// OUTSIDE the folder-lock domain, in these dedicated `org_*` tables — no folder seal/gate applies
    /// (there is no sealed state for an org item). They are protected at rest by whole-DB SQLCipher.
    fn migrate_org_ingest(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS org_items (
               item_id        TEXT PRIMARY KEY,
               org_id         TEXT NOT NULL,
               seq            INTEGER NOT NULL,
               author_hint    TEXT NOT NULL DEFAULT '',
               title          TEXT NOT NULL DEFAULT '',
               markdown       TEXT NOT NULL DEFAULT '',
               created_at     TEXT NOT NULL DEFAULT '',
               rev            INTEGER NOT NULL DEFAULT 1,
               generation     INTEGER NOT NULL DEFAULT 1,
               content_sha256 BLOB,
               is_current     INTEGER NOT NULL DEFAULT 0,
               tombstoned     INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS idx_org_items_org ON org_items(org_id);
             CREATE INDEX IF NOT EXISTS idx_org_items_sha ON org_items(content_sha256);
             CREATE TABLE IF NOT EXISTS org_chunks (
               id         INTEGER PRIMARY KEY,
               item_id    TEXT NOT NULL,
               chunk_idx  INTEGER NOT NULL,
               text       TEXT NOT NULL,
               FOREIGN KEY (item_id) REFERENCES org_items(item_id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_org_chunks_item ON org_chunks(item_id);",
        )
        .map_err(map_err)?;
        // Local provenance for explicit "Add to Space" copies. The org replica remains untouched;
        // this additive mapping makes the snapshot's origin durable without pretending the local
        // copy is still a live org-owned object.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS local_org_imports (
               local_kind TEXT NOT NULL CHECK(local_kind IN ('note','meeting')),
               local_id   TEXT PRIMARY KEY,
               org_id     TEXT NOT NULL,
               item_id    TEXT NOT NULL,
               created_at TEXT NOT NULL,
               UNIQUE(item_id, local_id)
             );
             CREATE INDEX IF NOT EXISTS idx_local_org_imports_item
               ON local_org_imports(item_id);",
        )
        .map_err(map_err)?;
        // ADDITIVE: `source_kind` (`"document"` | `"meeting"` | NULL) — the item's SOURCE type, opened
        // straight off the `OrgEnvelope` wire field once a peer publishes on the v2 wire format (see
        // `share::org_envelope::OrgSourceKind`). NULL for every row ingested before this column existed,
        // and for any item still arriving from a peer on an old client (v1 envelope, no wire signal) —
        // both are honestly "unclassified", never guessed. Guarded (`add_column_if_missing`) so an
        // already-migrated DB just gets the new column with existing rows defaulting to NULL.
        Self::add_column_if_missing(conn, "org_items", "source_kind", "TEXT")?;
        // ADDITIVE: `author_user_id` — the SERVER account id of the item's author, taken straight off the
        // feed entry (`OrgItemEntry.author_user_id`) at ingest. Lets ANY of the author's own machines
        // recognise their own item and offer edit-in-place, even one that never had the local `org_shares`
        // anchor (the machine that first shared it). NULL for rows ingested before this column existed and
        // for the local-replica upserts done at share/republish time (the next feed sync fills it in;
        // those machines already edit via their local source). Guarded so a re-migrated DB is a no-op.
        Self::add_column_if_missing(conn, "org_items", "author_user_id", "TEXT")?;
        // Stable opaque document identity + server-enforced collaboration metadata. Legacy rows
        // remain readable with NULL doc/owner and default view; they are not durable link targets.
        Self::add_column_if_missing(conn, "org_items", "doc_id", "TEXT")?;
        Self::add_column_if_missing(
            conn,
            "org_items",
            "access",
            "TEXT NOT NULL DEFAULT 'view' CHECK(access IN ('view','edit'))",
        )?;
        Self::add_column_if_missing(conn, "org_items", "document_owner_user_id", "TEXT")?;
        Self::add_column_if_missing(
            conn,
            "org_items",
            "is_current",
            "INTEGER NOT NULL DEFAULT 0 CHECK(is_current IN (0,1))",
        )?;
        Self::add_column_if_missing(conn, "org_items", "projection_sha256", "BLOB")?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_org_items_doc ON org_items(org_id, doc_id, rev DESC)",
            [],
        )
        .map_err(map_err)?;
        Self::add_column_if_missing(conn, "org_shares", "doc_id", "TEXT")?;
        Self::add_column_if_missing(
            conn,
            "org_shares",
            "access",
            "TEXT NOT NULL DEFAULT 'view' CHECK(access IN ('view','edit'))",
        )?;
        // Persist the caller's scrub choice so a crash/ambiguous POST retry seals the same canonical
        // plaintext. Existing rows default fail-safe to scrubbed.
        Self::add_column_if_missing(
            conn,
            "org_shares",
            "scrub",
            "INTEGER NOT NULL DEFAULT 1 CHECK(scrub IN (0,1))",
        )?;
        Self::add_column_if_missing(conn, "org_shares", "expected_actor_user_id", "TEXT")?;
        Self::add_column_if_missing(conn, "org_shares", "expected_owner_user_id", "TEXT")?;
        conn.execute(
            "UPDATE org_shares SET last_error='recovery_witness_missing'
              WHERE state='failed'
                AND last_error IN ('initial_post_pending','initial_post_replayable','direct_put_pending',
                                   'republish_put_pending','republish_post_pending',
                                   'projection_pending')
                AND (dispatch_id IS NULL OR trim(dispatch_id)='' OR length(dispatch_id)!=36
                  OR substr(dispatch_id,9,1)!='-' OR substr(dispatch_id,14,1)!='-'
                  OR substr(dispatch_id,19,1)!='-' OR substr(dispatch_id,24,1)!='-'
                  OR doc_id IS NULL OR trim(doc_id)='' OR length(doc_id)!=36
                  OR substr(doc_id,9,1)!='-' OR substr(doc_id,14,1)!='-'
                  OR substr(doc_id,19,1)!='-' OR substr(doc_id,24,1)!='-'
                  OR trim(org_id)='' OR length(org_id)!=36
                  OR substr(org_id,9,1)!='-' OR substr(org_id,14,1)!='-'
                  OR substr(org_id,19,1)!='-' OR substr(org_id,24,1)!='-'
                  OR content_sha256 IS NULL OR length(content_sha256)!=32
                  OR expected_actor_user_id IS NULL OR trim(expected_actor_user_id)=''
                  OR expected_owner_user_id IS NULL OR trim(expected_owner_user_id)=''
                  OR access NOT IN ('view','edit') OR rev < 1 OR generation < 1
                  OR (meeting_id IS NOT NULL AND document_id IS NOT NULL)
                  OR (last_error IN ('initial_post_pending','initial_post_replayable','republish_put_pending',
                                     'republish_post_pending')
                      AND meeting_id IS NULL AND document_id IS NULL)
                  OR (last_error IN ('direct_put_pending','republish_put_pending')
                      AND (item_id IS NULL OR trim(item_id)='')))",
            [],
        )
        .map_err(map_err)?;
        // vec0 int8 KNN table (width = EMBED_DIM; compile-time const, no user input). int8 per the
        // scale spike — see the doc comment. Values are inserted via `vec_int8(?)` (a raw i8 blob).
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS org_vec_chunks USING vec0(
                 chunk_id INTEGER PRIMARY KEY,
                 embedding int8[{dim}]
             );",
            dim = crate::embed::EMBED_DIM
        ))
        .map_err(map_err)?;
        // External-content FTS5 over org_chunks.text + the production trigger trio (mirrors
        // migrate_doc_fts). No one-time backfill is needed (org_chunks only ever appear via the
        // trigger-covered ingest path, never predate the index), so this is a plain CREATE.
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS fts_org_chunks USING fts5(
                 text,
                 content='org_chunks',
                 content_rowid='id',
                 tokenize = 'unicode61 remove_diacritics 2'
             );
             CREATE TRIGGER IF NOT EXISTS fts_org_chunks_ai AFTER INSERT ON org_chunks BEGIN
                 INSERT INTO fts_org_chunks(rowid, text) VALUES (new.id, new.text);
             END;
             CREATE TRIGGER IF NOT EXISTS fts_org_chunks_ad AFTER DELETE ON org_chunks BEGIN
                 INSERT INTO fts_org_chunks(fts_org_chunks, rowid, text)
                   VALUES ('delete', old.id, old.text);
             END;
             CREATE TRIGGER IF NOT EXISTS fts_org_chunks_au AFTER UPDATE ON org_chunks BEGIN
                 INSERT INTO fts_org_chunks(fts_org_chunks, rowid, text)
                   VALUES ('delete', old.id, old.text);
                 INSERT INTO fts_org_chunks(rowid, text) VALUES (new.id, new.text);
             END;",
        )
        .map_err(map_err)?;
        Self::migrate_shared_containers(conn)
    }

    /// SHARED CONTAINERS (2026-08-29) — publishing a whole Folder or Space to an org.
    ///
    /// Container structure is CONTENT: it travels inside the OCK-sealed envelope as a
    /// `ContainerEnvelope` manifest (`share/container_envelope.rs`), so the relay stores one more
    /// opaque blob and learns nothing about the shape of anyone's vault. That is why this whole
    /// feature is client-side and needs no server table, endpoint or authorization rule.
    ///
    /// These tables join the SAME lock domain the rest of `org_*` already occupies: org items are
    /// deliberately org-disclosed content living OUTSIDE the folder-seal domain, protected at rest
    /// by whole-DB SQLCipher. Nothing here adds a gate, and nothing here is sealed.
    ///
    /// All additive + guarded, so `migrate()` stays idempotent and no existing row is read or
    /// rewritten.
    fn migrate_shared_containers(conn: &Connection) -> Result<()> {
        // OUTBOUND journal — one row per (org, local container) this device publishes.
        //
        // `is_root = 1` marks the container the user actually picked; descendants get their own
        // rows with `is_root = 0`. That is what lets unsharing the root cascade, and what stops a
        // descendant from being unshared on its own while its root is still live.
        //
        // Mirrors `org_shares`'s state vocabulary on purpose: the launch sweep already knows how to
        // read `queued`/`failed` as "retry me", and a manifest publish is recoverable the same way
        // a note publish is.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS org_container_shares (
               id             TEXT PRIMARY KEY,
               org_id         TEXT NOT NULL,
               folder_id      TEXT NOT NULL,
               container_id   TEXT NOT NULL,
               access         TEXT NOT NULL DEFAULT 'view' CHECK(access IN ('view','edit')),
               scrub          INTEGER NOT NULL DEFAULT 1 CHECK(scrub IN (0,1)),
               is_root        INTEGER NOT NULL DEFAULT 0 CHECK(is_root IN (0,1)),
               state          TEXT NOT NULL DEFAULT 'queued'
                              CHECK (state IN ('queued','published','failed','revoke_pending','revoked')),
               item_id        TEXT,
               rev            INTEGER NOT NULL DEFAULT 1,
               generation     INTEGER NOT NULL DEFAULT 1,
               content_sha256 BLOB,
               position       INTEGER NOT NULL DEFAULT 0,
               last_error     TEXT,
               created_at     TEXT NOT NULL,
               updated_at     TEXT NOT NULL,
               UNIQUE(org_id, folder_id)
             );
             CREATE INDEX IF NOT EXISTS idx_org_container_shares_org
               ON org_container_shares(org_id);
             CREATE INDEX IF NOT EXISTS idx_org_container_shares_container
               ON org_container_shares(org_id, container_id);",
        )
        .map_err(map_err)?;

        // INBOUND replica — the decrypted manifest of a container someone shared with this user.
        // Keyed by the CLIENT-generated `container_id` (stable across revisions), not the server
        // item id, because a rename publishes a new item under the same document.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS org_containers (
               org_id                 TEXT NOT NULL,
               container_id           TEXT NOT NULL,
               item_id                TEXT NOT NULL,
               level                  TEXT NOT NULL CHECK(level IN ('space','folder')),
               name                   TEXT NOT NULL DEFAULT '',
               emoji                  TEXT,
               tint                   TEXT,
               parent_container_id    TEXT,
               position               INTEGER NOT NULL DEFAULT 0,
               access                 TEXT NOT NULL DEFAULT 'view' CHECK(access IN ('view','edit')),
               author_hint            TEXT NOT NULL DEFAULT '',
               author_user_id         TEXT,
               document_owner_user_id TEXT,
               seq                    INTEGER NOT NULL DEFAULT 0,
               rev                    INTEGER NOT NULL DEFAULT 1,
               generation             INTEGER NOT NULL DEFAULT 1,
               created_at             TEXT NOT NULL DEFAULT '',
               tombstoned             INTEGER NOT NULL DEFAULT 0 CHECK(tombstoned IN (0,1)),
               PRIMARY KEY (org_id, container_id)
             );
             CREATE INDEX IF NOT EXISTS idx_org_containers_parent
               ON org_containers(org_id, parent_container_id);
             CREATE INDEX IF NOT EXISTS idx_org_containers_item
               ON org_containers(item_id);",
        )
        .map_err(map_err)?;

        // The recipient's PRIVATE arrangement. A row here changes where a received object is DRAWN
        // in this user's sidebar and nothing else: it never leaves the device, never reaches the
        // relay, and never alters ownership — the content keeps updating from the org feed exactly
        // as before. It gives an org item no `folder_id`, so it cannot pull org content into a
        // local folder's seal domain.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS org_local_placements (
               placement_key   TEXT PRIMARY KEY,
               org_id          TEXT NOT NULL,
               target_kind     TEXT NOT NULL CHECK(target_kind IN ('container','doc')),
               target_id       TEXT NOT NULL,
               local_parent_id TEXT,
               position        INTEGER NOT NULL DEFAULT 0,
               updated_at      TEXT NOT NULL,
               UNIQUE(org_id, target_kind, target_id)
             );
             CREATE INDEX IF NOT EXISTS idx_org_local_placements_parent
               ON org_local_placements(local_parent_id);",
        )
        .map_err(map_err)?;

        // A received document's placement, opened straight off the v4 envelope. NULL means "no
        // container" — the honest state for every item published before containers existed and for
        // every standalone share. It is never guessed into a container.
        Self::add_column_if_missing(conn, "org_items", "parent_container_id", "TEXT")?;
        Self::add_column_if_missing(conn, "org_items", "position", "INTEGER NOT NULL DEFAULT 0")?;

        // The outbound twin: which container this device published the document under, and whether
        // the user asked for this share themselves.
        //
        // `explicit` is what makes unsharing a container safe. Rows the container sweep created
        // (`explicit = 0`) are withdrawn with it; rows the user shared deliberately (`explicit = 1`)
        // merely lose their `parent_container_id` and stay live. DEFAULT 1 is correct for every
        // pre-existing row — each of them came from someone pressing "Add to Org Brain".
        Self::add_column_if_missing(conn, "org_shares", "parent_container_id", "TEXT")?;
        Self::add_column_if_missing(conn, "org_shares", "position", "INTEGER NOT NULL DEFAULT 0")?;
        Self::add_column_if_missing(
            conn,
            "org_shares",
            "explicit",
            "INTEGER NOT NULL DEFAULT 1 CHECK(explicit IN (0,1))",
        )?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_org_items_container
               ON org_items(org_id, parent_container_id);
             CREATE INDEX IF NOT EXISTS idx_org_shares_container
               ON org_shares(org_id, parent_container_id);",
        )
        .map_err(map_err)?;
        Self::repair_containers_mis_ingested_as_items(conn)?;
        Self::backfill_placement_from_the_outbound_journal(conn)
    }

    /// REPAIR (2.1.2): restore the container placement of documents this device published.
    ///
    /// 2.1.1 taught the live pull to WRITE placement, but only for items it ingests from then on.
    /// An item already in the replica is "converged" — its stored content hash matches the feed's —
    /// so neither the live pull nor the anti-entropy sweep ever re-reads it, and the placement it
    /// was ingested without is never filled in. The visible result is a shared Space that arrives
    /// EMPTY while its documents sit loose in Shared Brains.
    ///
    /// For a document THIS device published, the outbound journal already knows the answer, so the
    /// repair is a local join — no network, no re-ingest. A document published by another member
    /// has no local journal; its placement arrives with that member's next publish.
    fn backfill_placement_from_the_outbound_journal(conn: &Connection) -> Result<()> {
        conn.execute(
            "UPDATE org_items
                SET parent_container_id = (
                      SELECT s.parent_container_id FROM org_shares s
                       WHERE s.item_id = org_items.item_id
                         AND s.parent_container_id IS NOT NULL
                       LIMIT 1),
                    position = COALESCE((
                      SELECT s.position FROM org_shares s
                       WHERE s.item_id = org_items.item_id
                         AND s.parent_container_id IS NOT NULL
                       LIMIT 1), position)
              WHERE parent_container_id IS NULL
                AND EXISTS (
                      SELECT 1 FROM org_shares s
                       WHERE s.item_id = org_items.item_id
                         AND s.parent_container_id IS NOT NULL)",
            [],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// REPAIR (2.1.1): move container manifests that 2.1.0 wrote into `org_items` into
    /// `org_containers`, where they belong.
    ///
    /// 2.1.0 added the container branch to the anti-entropy reconcile sweep but NOT to the live
    /// feed pull, and the live pull is the path that actually runs on a healthy replica. A manifest
    /// arriving that way was written as an ordinary note, so a shared Space appeared in Shared
    /// Brains as a note named after the folder instead of appearing in the sidebar as a Space.
    ///
    /// This is data-preserving in both directions: the manifest becomes the container it was always
    /// meant to be, and the mis-ingested row is TOMBSTONED rather than deleted, so a later feed
    /// replay of the same server item is idempotent instead of resurrecting the note. A row whose
    /// markdown will not parse is left exactly as it is — a repair that cannot understand a row has
    /// no business rewriting it.
    fn repair_containers_mis_ingested_as_items(conn: &Connection) -> Result<()> {
        // A named record rather than a ten-wide tuple: the tuple is unreadable at the call site and
        // is exactly the `clippy::type_complexity` this repo has paid for before.
        struct MisIngested {
            item_id: String,
            org_id: String,
            markdown: String,
            author_hint: String,
            seq: i64,
            access: String,
            created_at: String,
            rev: i64,
            generation: i64,
            owner: String,
        }
        let rows: Vec<MisIngested> = {
            let mut stmt = conn
                .prepare(
                    "SELECT item_id, org_id, markdown, COALESCE(author_hint,''),
                            COALESCE(seq,0), COALESCE(access,'view'),
                            COALESCE(created_at,''), COALESCE(rev,1), COALESCE(generation,1),
                            COALESCE(document_owner_user_id,'')
                       FROM org_items
                      WHERE source_kind = 'container' AND tombstoned = 0",
                )
                .map_err(map_err)?;
            let mapped = stmt
                .query_map([], |r| {
                    Ok(MisIngested {
                        item_id: r.get(0)?,
                        org_id: r.get(1)?,
                        markdown: r.get(2)?,
                        author_hint: r.get(3)?,
                        seq: r.get(4)?,
                        access: r.get(5)?,
                        created_at: r.get(6)?,
                        rev: r.get(7)?,
                        generation: r.get(8)?,
                        owner: r.get(9)?,
                    })
                })
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            mapped
        };
        for row in rows {
            let MisIngested {
                item_id,
                org_id,
                markdown,
                author_hint,
                seq,
                access,
                created_at,
                rev,
                generation,
                owner,
            } = row;
            let Ok(manifest) =
                crate::share::container_envelope::ContainerEnvelope::from_json(&markdown)
            else {
                continue;
            };
            conn.execute(
                "INSERT INTO org_containers
                   (org_id, container_id, item_id, level, name, emoji, tint, parent_container_id,
                    position, access, author_hint, author_user_id, document_owner_user_id, seq, rev,
                    generation, created_at, tombstoned)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,NULL,?12,?13,?14,?15,?16,0)
                 ON CONFLICT(org_id, container_id) DO UPDATE SET
                   item_id=excluded.item_id, level=excluded.level, name=excluded.name,
                   emoji=excluded.emoji, tint=excluded.tint,
                   parent_container_id=excluded.parent_container_id, position=excluded.position,
                   access=excluded.access, seq=excluded.seq, rev=excluded.rev,
                   generation=excluded.generation, tombstoned=0",
                rusqlite::params![
                    org_id,
                    manifest.container_id,
                    item_id,
                    manifest.level.as_str(),
                    manifest.name,
                    manifest.emoji,
                    manifest.tint,
                    manifest.parent_container_id,
                    manifest.position,
                    access,
                    author_hint,
                    (!owner.is_empty()).then_some(owner),
                    seq,
                    rev,
                    generation,
                    created_at,
                ],
            )
            .map_err(map_err)?;
            conn.execute(
                "UPDATE org_items SET tombstoned = 1 WHERE item_id = ?1",
                rusqlite::params![item_id],
            )
            .map_err(map_err)?;
            conn.execute(
                "DELETE FROM org_chunks WHERE item_id = ?1",
                rusqlite::params![item_id],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    /// Add `column` to `table` if it is not already present (idempotent migration guard).
    pub(crate) fn add_column_if_missing(
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

    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        // A poisoned lock means a prior writer panicked mid-statement; recover the
        // guard so the DB stays usable rather than cascading the panic.
        //
        // `try_lock` first so the overwhelmingly common uncontended case pays NOTHING: no clock
        // read, no atomic beyond one counter. Only a genuine wait is timed, which is also the only
        // case worth measuring.
        match self.conn.try_lock() {
            Ok(guard) => {
                db_lock_stats().record_immediate();
                return guard;
            }
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                db_lock_stats().record_immediate();
                return poisoned.into_inner();
            }
            Err(std::sync::TryLockError::WouldBlock) => {}
        }
        let started = std::time::Instant::now();
        let guard = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let waited = started.elapsed();
        db_lock_stats().record_wait(waited);
        if waited >= SLOW_DB_LOCK_WAIT {
            // One slow wait is noise; the running totals are what say whether this is a pattern —
            // which is the whole reason the counters are not test-only.
            // IDs, stages, counts and durations only — never a statement or a row (rust-tauri §8).
            let (contended, _uncontended, total_wait_us, max_wait_us) = db_lock_stats().snapshot();
            tracing::warn!(
                target: "db",
                wait_ms = waited.as_millis() as u64,
                contended_total = contended,
                total_wait_ms = total_wait_us / 1_000,
                max_wait_ms = max_wait_us / 1_000,
                "waited for the shared SQLite connection",
            );
        }
        guard
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
    // `insert_egress` moved to `storage::egress_store` (God-file split) — still callable as inherent
    // `db.method()` cross-file. Content-free `egress_log` row (counts/ids/labels/bytes/tokens only).

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

    /// Durable pre-dispatch journal for a mode-A link create. A lost response therefore still
    /// leaves enough source identity for folder/source revoke to DELETE the remote share id before
    /// sealing or destroying local plaintext.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_outbound_share_attempt(
        &self,
        share_id: &str,
        meeting_id: Option<&str>,
        document_id: Option<&str>,
        mode: &str,
        rev: u32,
        owner_user_id: &str,
        created_at: &str,
    ) -> Result<bool> {
        if meeting_id.is_some() == document_id.is_some() {
            return Err(crate::error::AppError::InvalidArg(
                "exactly one outbound share source is required".into(),
            ));
        }
        let conn = self.lock();
        conn.execute(
            "INSERT INTO outbound_shares
               (share_id,meeting_id,document_id,mode,rev,state,owner_user_id,created_at)
             SELECT ?1,COALESCE(?2,''),?3,?4,?5,'create_pending',?6,?7
              WHERE NOT EXISTS(
                SELECT 1 FROM outbound_shares
                 WHERE state='create_pending' AND mode=?4 AND owner_user_id=?6
                   AND ((?2 IS NOT NULL AND meeting_id=?2)
                     OR (?3 IS NOT NULL AND document_id=?3))
              )",
            rusqlite::params![
                share_id, meeting_id, document_id, mode, rev as i64, owner_user_id, created_at
            ],
        )
        .map_err(map_err)
        .map(|changed| changed == 1)
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

    /// Owner-bound local cleanup journals, including pre-capability creates and rows no longer
    /// returned by the relay. They remain user-visible until the exact id is reserved and a
    /// verified DELETE completes; absence from a list response is never deletion proof.
    pub(crate) fn outbound_cleanup_pending_for_owner(
        &self,
        owner_user_id: &str,
    ) -> Result<Vec<OutboundCleanupPendingRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT share_id,NULLIF(meeting_id,''),document_id,mode,rev,created_at
                   FROM outbound_shares
                  WHERE state IN ('create_pending','revoke_pending') AND owner_user_id=?1
                  ORDER BY created_at DESC,share_id ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([owner_user_id], |row| {
                let rev = row.get::<_, i64>(4)?;
                Ok(OutboundCleanupPendingRow {
                    share_id: row.get(0)?,
                    meeting_id: row.get(1)?,
                    document_id: row.get(2)?,
                    mode: row.get(3)?,
                    rev: u32::try_from(rev).map_err(|_| {
                        rusqlite::Error::IntegralValueOutOfRange(4, rev)
                    })?,
                    created_at: row.get(5)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_err)?);
        }
        Ok(out)
    }

    /// The immutable owner/mode plus current cleanup phase for one locally-created share.
    pub(crate) fn outbound_share_cleanup_context(
        &self,
        share_id: &str,
    ) -> Result<Option<(String, String, String, u32)>> {
        self.lock()
            .query_row(
                "SELECT owner_user_id,mode,state,rev FROM outbound_shares WHERE share_id=?1",
                [share_id],
                |row| {
                    let rev = row.get::<_, i64>(3)?;
                    Ok((
                        row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        row.get(1)?,
                        row.get(2)?,
                        u32::try_from(rev)
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, rev))?,
                    ))
                },
            )
            .optional()
            .map_err(map_err)
    }

    /// Persist the exact encrypted content dispatch witness and its content-free ledger row in one
    /// transaction. The row must still be the owner-bound pre-create journal reserved by the relay.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn persist_outbound_content_dispatch(
        &self,
        share_id: &str,
        owner_user_id: &str,
        mode: &str,
        rev: u32,
        dispatch_id: &str,
        content_commitment: &[u8; 32],
        source_commitment: &[u8; 32],
        ts: i64,
        host: &str,
        kind: &str,
        byte_count: usize,
    ) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let changed = tx
            .execute(
                "UPDATE outbound_shares
                    SET dispatch_id=?5,content_commitment=?6,source_commitment=?7
                  WHERE share_id=?1 AND owner_user_id=?2 AND mode=?3 AND rev=?4
                    AND state='create_pending'",
                rusqlite::params![
                    share_id,
                    owner_user_id,
                    mode,
                    rev as i64,
                    dispatch_id,
                    content_commitment.as_slice(),
                    source_commitment.as_slice()
                ],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Ok(false);
        }
        insert_share_egress_dispatch_tx(
            &tx,
            ts,
            host,
            kind,
            byte_count,
            dispatch_id,
        )?;
        tx.commit().map_err(map_err)?;
        Ok(true)
    }

    /// Persist `revoke_pending` plus the exact owner/mode/share-id DELETE dispatch and content-free
    /// ledger in one transaction. A stale owner, mode, or terminal row cannot mint a socket permit.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn persist_outbound_delete_dispatch(
        &self,
        share_id: &str,
        owner_user_id: &str,
        mode: &str,
        rev: u32,
        dispatch_id: &str,
        ts: i64,
        host: &str,
    ) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let changed = tx
            .execute(
                "UPDATE outbound_shares
                    SET state='revoke_pending',dispatch_id=?5,
                        content_commitment=NULL,source_commitment=NULL
                  WHERE share_id=?1 AND owner_user_id=?2 AND mode=?3 AND rev=?4
                    AND state<>'revoked'",
                rusqlite::params![share_id, owner_user_id, mode, rev as i64, dispatch_id],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Ok(false);
        }
        insert_share_egress_dispatch_tx(
            &tx,
            ts,
            host,
            "share_revoke",
            0,
            dispatch_id,
        )?;
        tx.commit().map_err(map_err)?;
        Ok(true)
    }

    // `insert_share_egress` / `count_share_egress_by_kind` moved to `storage::egress_store`
    // (God-file split) alongside the other egress-ledger writers — still callable as inherent
    // `db.method()` cross-file. Content-free `share_egress_log` rows.

    // ── M6 Shared Brain: local org state + the outbound org-share state machine ──────────────────

    // `upsert_org_state` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `map_org_state` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `get_org_state` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `list_org_states` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_org_consented` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_org_context_enabled` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_org_last_seq` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_org_generation` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `delete_org_state` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `insert_org_share` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_org_share_uploaded` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_org_share_failed` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_org_share_state` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `map_org_share` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `get_org_share` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `org_share_by_item` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `org_shares_for_source` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `uploaded_org_shares_for_source_in_org` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `duplicate_uploaded_org_shares` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `cancel_pending_org_shares_for_source_in_org` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `find_reusable_org_share` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `reset_org_share_for_retry` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `list_org_shares_in_state` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `list_live_org_shares` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `list_org_shares_for_org` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `active_org_shares_for_folder` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    /// The folder's ACTIVE 1:1 shares (LINK + Murmur↔Murmur USER) as `(share_id, mode)`, joined to
    /// the folder through the shared meeting/document. Powers the lock×shares dialog + bulk-revoke —
    /// closing the pre-existing hole where `lock_folder` never surfaced live 1:1 shares. Mirrors
    /// [`Self::active_org_shares_for_folder`]. Mode-A LINK shares use `state = 'active'`
    /// ([`Self::insert_outbound_share`] / [`Self::insert_outbound_note_share`]); mode-B Murmur↔Murmur
    /// USER shares NEVER carry `'active'` — [`Self::insert_outbound_user_share`] writes `'sent'`
    /// (recipient already registered) or `'awaiting_key'` (pending, later flipped to `'sent'` by
    /// `share_rewrap_pending`) — so all three live states must match or every real mode-B row is
    /// silently excluded. `'revoked'` (terminal, [`Self::set_outbound_share_state`]) stays excluded.
    pub fn active_link_user_shares_for_folder(
        &self,
        folder_id: &str,
    ) -> Result<Vec<(String, String)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT s.share_id, s.mode
                   FROM outbound_shares s
                  WHERE s.state <> 'revoked'
                    AND ((s.document_id IS NOT NULL AND EXISTS(
                          SELECT 1 FROM documents d WHERE d.id=s.document_id AND d.folder_id=?1))
                      OR (s.meeting_id <> '' AND EXISTS(
                          SELECT 1 FROM meetings m WHERE m.id=s.meeting_id AND
                            (m.folder_id=?1 OR (m.folder_id IS NULL AND EXISTS(
                              SELECT 1 FROM notes n WHERE n.meeting_id=m.id AND n.folder_id=?1))))))",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// True when any local remote-share journal (link, directed user, or organization) still
    /// represents potentially-readable server ciphertext for this exact source. All non-terminal
    /// rows count, including ambiguous create attempts: absence of a success response is not proof
    /// that the relay did not commit the mutation.
    pub(crate) fn source_has_active_remote_share(
        &self,
        meeting_id: Option<&str>,
        document_id: Option<&str>,
    ) -> Result<bool> {
        if meeting_id.is_some() == document_id.is_some() {
            return Err(crate::error::AppError::InvalidArg(
                "exactly one share source is required".into(),
            ));
        }
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM outbound_shares
                WHERE state <> 'revoked'
                  AND ((?1 IS NOT NULL AND meeting_id=?1)
                    OR (?2 IS NOT NULL AND document_id=?2))
               UNION ALL
               SELECT 1 FROM org_shares
                WHERE state <> 'revoked'
                  AND ((?1 IS NOT NULL AND meeting_id=?1)
                    OR (?2 IS NOT NULL AND document_id=?2))
             )",
            rusqlite::params![meeting_id, document_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(map_err)
        .map(|exists| exists != 0)
    }

    pub(crate) fn active_outbound_shares_for_source(
        &self,
        meeting_id: Option<&str>,
        document_id: Option<&str>,
    ) -> Result<Vec<String>> {
        if meeting_id.is_some() == document_id.is_some() {
            return Err(crate::error::AppError::InvalidArg(
                "exactly one share source is required".into(),
            ));
        }
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT share_id FROM outbound_shares
                  WHERE state <> 'revoked'
                    AND ((?1 IS NOT NULL AND meeting_id=?1)
                      OR (?2 IS NOT NULL AND document_id=?2))
                  ORDER BY share_id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![meeting_id, document_id], |row| row.get(0))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_err)?);
        }
        Ok(out)
    }

    pub(crate) fn outbound_shares_in_state(&self, state: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT share_id FROM outbound_shares WHERE state=?1 ORDER BY share_id")
            .map_err(map_err)?;
        let rows = stmt.query_map([state], |row| row.get(0)).map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_err)?);
        }
        Ok(out)
    }

    pub(crate) fn folder_has_active_remote_share(&self, folder_id: &str) -> Result<bool> {
        if !self.active_link_user_shares_for_folder(folder_id)?.is_empty() {
            return Ok(true);
        }
        Ok(!self.active_org_share_ids_for_folder(folder_id)?.is_empty())
    }

    // `active_org_share_ids_for_folder` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

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
        owner_user_id: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO outbound_shares
               (share_id, meeting_id, mode, rev, state, created_at,
                nk_wrapped, recipient_acct_id, recipient_email, content_hash, owner_user_id)
             VALUES (?1, ?2, 'user', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                share_id,
                meeting_id,
                rev as i64,
                state,
                created_at,
                nk_wrapped,
                recipient_acct_id,
                recipient_email,
                content_hash,
                owner_user_id
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Add the encrypted mode-B retry material to an already durable `create_pending` journal.
    /// The recipient address lives only in the SQLCipher database (never in the content-free
    /// egress ledger). The phase intentionally stays `create_pending` until the POST returns 201;
    /// a crash before that point is recovered by reservation + DELETE, never by redispatching
    /// ciphertext.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_outbound_user_share_attempt(
        &self,
        share_id: &str,
        meeting_id: &str,
        rev: u32,
        nk_wrapped: &[u8],
        recipient_acct_id: &str,
        recipient_email: &str,
        content_hash: &[u8],
        owner_user_id: &str,
    ) -> Result<bool> {
        let conn = self.lock();
        conn.execute(
            "UPDATE outbound_shares
                SET nk_wrapped=?3, recipient_acct_id=?4, recipient_email=?5, content_hash=?6
              WHERE share_id=?1 AND meeting_id=?2 AND document_id IS NULL
                AND mode='user' AND rev=?7 AND state='create_pending'
                AND owner_user_id=?8",
            rusqlite::params![
                share_id,
                meeting_id,
                nk_wrapped,
                recipient_acct_id,
                recipient_email,
                content_hash,
                rev as i64,
                owner_user_id,
            ],
        )
        .map_err(map_err)
        .map(|changed| changed == 1)
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

    // `egress_summary` moved to `storage::egress_store` (God-file split) alongside the
    // `egress_log` / `share_egress_log` writers — still callable as inherent `db.method()`
    // cross-file. Read-only aggregate over `egress_log`; content-free.

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
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let sql = format!(
            "SELECT cl.id, cl.kind, cl.input, cl.model_output, cl.final_output, cl.accepted,
                    cl.owner_id, cl.created_at, cl.meeting_id
               FROM correction_log cl
              WHERE cl.kind = ?1
                AND cl.meeting_id IS NOT NULL
                AND EXISTS (
                      SELECT 1 FROM meetings m
                       WHERE m.id = cl.meeting_id AND {meeting_visible}
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

    // `insert_meeting` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `update_meeting_status` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `transition_meeting_status` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `stuck_recording_ids` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `reconcile_stuck_recordings` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `reconcile_stuck_recordings_except` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `finalize_meeting` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_meeting_title` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

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

    // `set_manual_notes` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_manual_notes_sealed` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `get_manual_notes` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `raw_manual_notes` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `seal_manual_notes` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `clear_manual_notes_blob` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    /// Delete a meeting and (via ON DELETE CASCADE) its segments, notes, and timeline.
    /// The caller must preflight nonterminal recording generations before unlinking any file; the
    /// transaction repeats that guard and purges terminal ledger rows before the meeting row.
    ///
    /// Returns the `exported_path`s of the memory rollups purged in the same transaction (a rollup
    /// may paraphrase the deleted meeting's facts — see `purge_memory_rollups_tx`); the CALLER
    /// deletes those vault `.md` files (same layering as the note/audio file removal above).
    pub fn delete_meeting(&self, id: &str) -> Result<Vec<String>> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let has_share_closure = Self::source_closure_ready_for_delete_tx(&tx, "meeting", id)?;
        Self::refuse_nonterminal_recording_generation_tx(&tx, id)?;
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
        // MEM-1 (reversible import supersede): BEFORE purging this meeting's own facts, REOPEN every
        // pre-existing fact that a synthetic Memory-Import superseded (set valid_to back to NULL) and
        // drop the link rows — so "delete the import ⇒ undo" restores prior memories instead of
        // leaving them permanently closed. A non-import meeting has no link rows ⇒ a no-op. Runs
        // before `purge_user_facts_tx` so the reopen targets facts that survive (the import's OWN
        // Adds are then deleted below); the two never touch the same rows.
        Self::reopen_import_superseded_facts_tx(&tx, id)?;
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
        // Vault Audit: purge any PENDING finding whose source OR target is this meeting in the
        // same tx — its `evidence_md` quotes the deleted meeting's note/title (resolved rows were
        // blanked on resolve and carry no content).
        Self::purge_pending_audit_findings_tx(&tx, &[id.to_string()])?;
        // Brain v3 PR-3: purge every `links` row whose SRC OR DST is this deleted meeting in the same
        // tx — a link to a gone meeting is a dangling edge (and would name a now-absent neighbour). A
        // permanent DELETE keeps NO decision row (`preserve_decisions=false`): the endpoint is gone.
        Self::purge_links_tx(&tx, &[id.to_string()], &[], false)?;
        // Brain v2 L2.1: purge ALL memory rollups in this same tx — a rollup may paraphrase the
        // deleted meeting's (now-gone) facts; the survivors regenerate on the next hourly pass
        // from the remaining visible facts only. The caller deletes the exported `.md`s.
        let rollup_exports = Self::purge_memory_rollups_tx(&tx)?;
        // Scoped to THIS meeting's folder. Deleting one meeting used to destroy every durable Ask
        // conversation in the vault, because the sweep's predicate matched every `globalDerived`
        // row — a conversation about an unrelated folder did not survive somebody tidying up a
        // recording. An unfiled meeting has no folder row for a dependency to name, so it still
        // takes the global sweep; that is the case the sweep exists for.
        let ask_scope = Self::ask_scope_for_meetings_tx(&tx, &[id.to_string()])?;
        Self::purge_ask_conversations_for_scope_tx(&tx, ask_scope.as_ref())?;
        Self::purge_retired_recording_generations_tx(&tx, id)?;
        tx.execute("DELETE FROM meetings WHERE id = ?1", rusqlite::params![id])
            .map_err(map_err)?;
        if has_share_closure {
            tx.execute(
                "UPDATE org_share_closures SET phase='closed'
                  WHERE scope_kind='meeting' AND scope_id=?1 AND phase='closing'",
                rusqlite::params![id],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(rollup_exports)
    }

    // `get_meeting` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `latest_meeting` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `list_meetings` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

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
                        COALESCE(m.folder_id, (SELECT MIN(nf.folder_id) FROM notes nf \
                          WHERE nf.meeting_id = m.id AND nf.folder_id IS NOT NULL \
                          HAVING COUNT(DISTINCT nf.folder_id) = 1)) AS folder_id
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
        self.index_meeting_chunks_at_background_epoch(meeting_id, segments, embedder, None)
            .map(|_| ())
    }

    pub(crate) fn index_meeting_chunks_background(
        &self,
        meeting_id: &str,
        segments: &[Segment],
        embedder: &dyn Embedder,
        epoch: u64,
    ) -> Result<bool> {
        self.index_meeting_chunks_at_background_epoch(meeting_id, segments, embedder, Some(epoch))
            .map(|committed| committed.is_some())
    }

    fn index_meeting_chunks_at_background_epoch(
        &self,
        meeting_id: &str,
        segments: &[Segment],
        embedder: &dyn Embedder,
        background_epoch: Option<u64>,
    ) -> Result<Option<()>> {
        // Resolve title + date (plaintext = visible metadata). Note markdown is optional — a meeting
        // may have transcript segments but no note yet (or vice versa); each class indexes on its own.
        let meeting = self.get_meeting(meeting_id)?;
        let Some(meeting) = meeting else {
            return Ok(Some(())); // unknown meeting — nothing to index.
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
        let note_vectors = embed_in_sub_batches(embedder, &note_aug)?;
        let transcript_vectors = embed_in_sub_batches(embedder, &transcript_aug)?;

        // Always purge this meeting's prior rows first (clean replace of BOTH classes), then insert the
        // fresh set in ONE transaction.
        let this_meeting = [meeting_id.to_string()];
        let commit = || -> Result<()> {
            let mut conn = self.lock();
            let tx = conn.transaction().map_err(map_err)?;
            // TOCTOU re-check INSIDE the write tx (mirrors `index_meeting_topic_chunks_reporting`): the
            // gated/visible read above ran BEFORE the slow embed, and any caller `unlocked` snapshot can be
            // stale by now. A `lock_folder` committing mid-embed blanks the note plaintext (`markdown=''`,
            // `content_blob` kept) — that DB-side sealed-at-rest invariant is session-independent, so key the
            // re-check on it: if the meeting is sealed at rest RIGHT NOW, inserting its derived plaintext
            // chunks/vectors would leave sealed content at rest until the next relock/startup reconcile.
            // Refuse (rollback via drop) — a benign no-op for every best-effort caller. UNSEAL/session-unlock
            // paths un-blank `markdown` before re-indexing, so `sealed_at_rest` is false there and the write
            // proceeds (defense-in-depth beyond the purge-on-seal, not a new refusal for legitimate work).
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
                return Ok(()); // sealed-at-rest mid-flight: never persist its plaintext chunks.
            }
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
        };
        match background_epoch {
            Some(epoch) => crate::perf::with_current_background_epoch(epoch, commit),
            None => commit().map(Some),
        }
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
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let sql = format!(
            "SELECT e.name FROM entities e
               JOIN entity_mentions em ON em.entity_id = e.id
               JOIN meetings m ON m.id = em.meeting_id
              WHERE em.meeting_id = ?1
                AND {meeting_visible}
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
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let sql = format!(
            "SELECT ft.subject, ft.predicate, ft.object
               FROM facts ft
               JOIN meetings m ON m.id = ft.meeting_id
              WHERE ft.meeting_id = ?1
                AND {meeting_visible}
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
        self.index_meeting_topic_chunks_reporting(meeting_id, segments, embedder, unlocked, None)
            .map(|_wrote| ())
    }

    pub(crate) fn index_meeting_topic_chunks_background(
        &self,
        meeting_id: &str,
        segments: &[Segment],
        embedder: &dyn Embedder,
        unlocked: &HashSet<String>,
        epoch: u64,
    ) -> Result<bool> {
        self.index_meeting_topic_chunks_reporting(
            meeting_id,
            segments,
            embedder,
            unlocked,
            Some(epoch),
        )
    }

    /// Same as [`Self::index_meeting_topic_chunks`] but reports whether it actually (re)wrote
    /// chunks (`true`) vs. was a no-op — sealed/missing meeting, or the idempotency probe found
    /// nothing changed (`false`). [`Self::backfill_topic_chunks_idempotent`] uses this to cap
    /// how much REAL embedding work (not idempotent skips) one run performs.
    fn index_meeting_topic_chunks_reporting(
        &self,
        meeting_id: &str,
        segments: &[Segment],
        embedder: &dyn Embedder,
        unlocked: &HashSet<String>,
        background_epoch: Option<u64>,
    ) -> Result<bool> {
        if !self.meeting_is_visible(meeting_id, unlocked)? {
            return Ok(false); // sealed-not-unlocked: never index its plaintext.
        }
        let Some(meeting) = self.get_meeting(meeting_id)? else {
            return Ok(false);
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
                return Ok(false);
            }
        }

        // Embed in small sub-batches rather than one call for the WHOLE meeting — see
        // `embed_in_sub_batches`'s doc comment. Topic chunks merge to >= TOPIC_MERGE_MIN_DURATION_S
        // (60s), so a single long meeting (a 1h+ recording can easily produce 40-60 chunks) would
        // otherwise build ONE rectangular Candle/Metal tensor sized (chunk_count,
        // longest_chunk_tokens) in one blocking forward pass, untouched by the per-run
        // meeting-count cap above (2026-07-13: a reported launch freeze persisted after that cap
        // because the freezing vault had a 1h+ recording).
        let vectors = embed_in_sub_batches(embedder, &augs)?;

        // PURGE-then-INSERT this meeting's topic rows in ONE transaction (clean replace).
        let commit = || -> Result<bool> {
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
                return Ok(false);
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
            Ok(true)
        };
        match background_epoch {
            Some(epoch) => crate::perf::with_current_background_epoch(epoch, commit)
                .map(|committed| committed.unwrap_or(false)),
            None => commit(),
        }
    }

    /// Brain v2 L1.1 — STARTUP BACKFILL: index topic chunks for every VISIBLE meeting that has
    /// segments, in batches of 20 (with a short pause between batches
    /// so the shared connection lock breathes). Content-hash idempotent — an already-indexed
    /// meeting is a cheap probe, so re-running on every launch is fine. Runs under an EMPTY unlock
    /// set (nothing is session-unlocked at startup; sealed meetings are skipped by the gate and
    /// their topic rows are re-derived on unlock via `reindex_meetings_after_unseal`). Returns how
    /// many meetings were (re)indexed. Per-meeting failures WARN (ids only, no PII) and continue.
    ///
    /// CAPPED at [`TOPIC_BACKFILL_MAX_REEMBED_PER_RUN`] REAL (re)embeds per call — a vault-wide
    /// hash change (Brain freshly enabled, or a segmentation/augmentation version bump) would
    /// otherwise re-embed the ENTIRE vault in one unthrottled pass on a single app launch, pegging
    /// CPU/Metal for however long that takes before the user can do anything (the 2026-07-13
    /// launch-freeze incident). The idempotency probe means the cap just defers the remainder to
    /// the NEXT launch — no cursor needed, each run picks up wherever the hash still differs.
    pub fn backfill_topic_chunks_idempotent(&self, embedder: &dyn Embedder) -> Result<usize> {
        self.backfill_topic_chunks_idempotent_at_epoch(embedder, None)
    }

    pub(crate) fn backfill_topic_chunks_idempotent_background(
        &self,
        embedder: &dyn Embedder,
        epoch: u64,
    ) -> Result<usize> {
        self.backfill_topic_chunks_idempotent_at_epoch(embedder, Some(epoch))
    }

    fn backfill_topic_chunks_idempotent_at_epoch(
        &self,
        embedder: &dyn Embedder,
        background_epoch: Option<u64>,
    ) -> Result<usize> {
        const TOPIC_BACKFILL_BATCH: usize = 20;
        const TOPIC_BACKFILL_MAX_REEMBED_PER_RUN: usize = 50;
        let unlocked: HashSet<String> = HashSet::new();
        let meetings = self.list_meetings_visible(100_000, &unlocked)?;
        let mut indexed = 0usize;
        let mut reembedded = 0usize;
        'outer: for batch in meetings.chunks(TOPIC_BACKFILL_BATCH) {
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
                match self.index_meeting_topic_chunks_reporting(
                    &m.id,
                    &segments,
                    embedder,
                    &unlocked,
                    background_epoch,
                ) {
                    Ok(wrote) => {
                        indexed += 1;
                        if wrote {
                            reembedded += 1;
                            // Breathing room between REAL embeds specifically (idempotent no-ops
                            // are free and stay unpaced) — a batch that's entirely fresh work
                            // (e.g. Brain just enabled) would otherwise fire up to
                            // TOPIC_BACKFILL_BATCH Candle/Metal calls back-to-back with zero
                            // scheduler gap before the batch-level sleep below ever runs.
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                    }
                    Err(e) => {
                        tracing::warn!(target: "rag", error = %e, "topic backfill: indexing one meeting failed (skipped)");
                    }
                }
                if reembedded >= TOPIC_BACKFILL_MAX_REEMBED_PER_RUN {
                    tracing::info!(
                        target: "rag",
                        reembedded,
                        "topic backfill: per-run cap reached, deferring remainder to next launch"
                    );
                    break 'outer;
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
        // Vault Audit LOCK-SAFETY: purge ALL pending findings in this SAME seal tx — the
        // memory-rollups posture (adversarial HIGH: evidence may cite THIRD-PARTY titles with no
        // id to match, e.g. a stale finding's `see [[superseding note]]`; a seal anywhere
        // invalidates the pass's visibility snapshot). Resolved rows were blanked on resolve.
        Self::purge_all_pending_audit_findings_tx(&tx)?;
        // GLOBAL on purpose, and this is the one place in this file where that is not laziness.
        //
        // This helper is reached from `seal_moved_note`, which runs AFTER
        // `move_meeting_with_attachments_sealed` has already committed
        // `UPDATE meetings SET folder_id = <destination>`. Deriving a scope from `meetings.folder_id`
        // here therefore reads the folder the meeting has just been moved INTO, never the one it
        // came FROM — so a conversation that depended on the source folder survived a move into a
        // locked folder and went on paraphrasing content the user had just put behind the biometric
        // gate. Scoping this call site was an ACTIVE LEAK, caught in review before it shipped.
        //
        // Nothing is lost by staying global: `finish_folder_lock_after_seal` follows this with an
        // unconditional `purge_all_ask_conversations` four lines later, so the scope was inert on
        // the lock path anyway. "I cannot name the scope" is the honest answer whenever an earlier
        // step in the same operation may have moved the content — see
        // `purge_ask_conversations_for_scope_tx`.
        Self::purge_all_ask_conversations_tx(&tx)?;
        // Brain v3 PR-3 LINK-ENGINE LOCK-SAFETY: purge every DERIVED `links` row whose SRC OR DST is a
        // just-sealed meeting in this SAME seal tx — a link names a neighbour (its title/existence
        // reveals a possibly-sealed item), so it must not survive at rest for a sealed endpoint.
        // Re-derived on unlock (wikilink + semantic pass). Document endpoints of these folders are
        // covered by the `purge_doc_chunks_for_documents` leg the lock caller runs alongside. A SEAL
        // preserves the user's decision rows (`preserve_decisions=true`, Fix 1).
        Self::purge_links_tx(&tx, meeting_ids, &[], true)?;
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
    pub(crate) fn purge_memory_rollups_tx(tx: &rusqlite::Transaction<'_>) -> Result<Vec<String>> {
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
        tx.execute("DELETE FROM memory_rollups", [])
            .map_err(map_err)?;
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
    pub(crate) fn purge_pending_brief_runs_tx(
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
    /// than key-seal. Dropped by design; the rows are RECOVERABLE from `sealed_fact_ledgers`, which the seal writes before this purge runs; the underlying transcript is
    /// still sealed + restorable, and a later re-summarize re-derives facts.
    pub(crate) fn purge_facts_tx(
        tx: &rusqlite::Transaction<'_>,
        meeting_ids: &[String],
    ) -> Result<()> {
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
    pub(crate) fn purge_supersessions_tx(
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
    pub(crate) fn purge_user_facts_tx(
        tx: &rusqlite::Transaction<'_>,
        meeting_ids: &[String],
    ) -> Result<()> {
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
    pub(crate) fn purge_speaker_voiceprints_tx(
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
    pub(crate) fn purge_chunks_tx(
        tx: &rusqlite::Transaction<'_>,
        meeting_ids: &[String],
    ) -> Result<()> {
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
    pub(crate) fn purge_corrections_tx(
        tx: &rusqlite::Transaction<'_>,
        meeting_ids: &[String],
    ) -> Result<()> {
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
    ///
    /// `min_cosine` (S1) is an OPT-IN relevance FLOOR over the vector leg: a candidate whose cosine
    /// (mapped from the vec0 L2 distance over UNIT vectors via [`crate::links::cosine_from_l2_distance`],
    /// since `|a-b|² = 2 − 2·cos`) is BELOW it is dropped as noise. Sentinel `0.0` = NO floor
    /// (behaviour-identical to before S1). Applied strictly AFTER the visibility gate, so it can only
    /// ever REMOVE rows — never widen or reorder a leg around its gate.
    pub fn search_semantic_visible(
        &self,
        query_vec: &[f32],
        k: i64,
        min_cosine: f32,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<SearchHit>> {
        if query_vec.is_empty() || k <= 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        // KNN is isolated to the vec0 table in a CTE (only a single MATCH+k constraint is allowed on
        // a vec0 query); visibility + meeting columns are joined OUTSIDE it.
        let sql = format!(
            "WITH knn(chunk_id, distance) AS (
                 SELECT chunk_id, distance FROM vec_chunks
                  WHERE embedding MATCH ?1 AND k = ?2
                  ORDER BY distance
             )
             SELECT m.id, m.started_at, m.ended_at, m.title, m.duration_s, m.audio_path, m.status,
                    COALESCE(m.folder_id, (SELECT MIN(nf.folder_id) FROM notes nf
                      WHERE nf.meeting_id = m.id AND nf.folder_id IS NOT NULL
                      HAVING COUNT(DISTINCT nf.folder_id) = 1)) AS folder_id,
                    nc.text, knn.distance
               FROM knn
               JOIN note_chunks nc ON nc.id = knn.chunk_id
               JOIN meetings m ON m.id = nc.meeting_id
              WHERE {meeting_visible}
              ORDER BY knn.distance ASC, m.id ASC"
        );
        let blob = crate::embed::vec_to_blob(query_vec);
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![blob, k], |row| {
                let meeting = row_to_meeting(row)?;
                let snippet: String = row.get(8)?;
                let distance: f64 = row.get(9)?;
                Ok((meeting, snippet, distance))
            })
            .map_err(map_err)?;

        let mut seen: HashSet<String> = HashSet::new();
        let mut hits = Vec::new();
        for r in rows {
            let (meeting, snippet, distance) = r.map_err(map_err)?;
            // S1 relevance floor: drop a below-floor neighbour (noise on a tiny/irrelevant corpus).
            // Applied AFTER the SQL visibility gate — it can only REMOVE rows.
            if min_cosine > 0.0
                && crate::links::cosine_from_l2_distance(distance as f32) < min_cosine
            {
                continue;
            }
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
        // No S1 floor here (0.0) — related-meetings is behaviour-preserving; only the MCP search arms floor.
        let mut hits = self.search_semantic_visible(&centroid, k + 1, 0.0, unlocked)?;
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
        let Some(and_expr) = fts_match_query(q) else {
            return Ok(Vec::new());
        };
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
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
              WHERE {meeting_visible}{date}
              ORDER BY r.rank ASC, m.started_at DESC, m.id DESC
              LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        // Run the (already-gated) FTS body with a given match expression. The visibility predicate is
        // baked into the SQL, so swapping AND→OR only ever changes WHICH visible rows match.
        let run = |stmt: &mut rusqlite::Statement, expr: &str| -> Result<Vec<(String, f64)>> {
            let rows = stmt
                .query_map(rusqlite::params![expr, limit], |r| {
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
        };
        // S2 AND→OR fallback: implicit-AND matched nothing ⇒ retry with the content-word OR twin
        // (stopwords/<3-char dropped). Only fires on an EMPTY AND result, so it never widens a
        // successful query; stays exact-word lexical.
        let mut out = run(&mut stmt, &and_expr)?;
        if out.is_empty() {
            if let Some(any_expr) = fts_match_query_any(q) {
                if any_expr != and_expr {
                    out = run(&mut stmt, &any_expr)?;
                }
            }
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
        min_cosine: f32,
        unlocked: &HashSet<String>,
        date_filter: Option<&(String, String)>,
    ) -> Result<Vec<(String, f64)>> {
        if query_vec.is_empty() || k <= 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
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
              WHERE {meeting_visible}{date}
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
            let (id, d) = r.map_err(map_err)?;
            // S1 relevance floor over the vector leg (opt-in; 0.0 = none). Applied AFTER the SQL
            // visibility gate, so it can only REMOVE below-floor rows — never widen or reorder.
            if min_cosine > 0.0 && crate::links::cosine_from_l2_distance(d as f32) < min_cosine {
                continue;
            }
            out.push((id, d));
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
    ///
    /// `min_cosine` (S1) is the vector-leg relevance FLOOR, threaded to BOTH vector legs
    /// (`search_semantic_visible` for the snippet leg, `knn_meeting_distances` for the score leg);
    /// the FTS and graph legs are NEVER floored. Sentinel `0.0` = no floor. When the floor empties
    /// the KNN leg on an irrelevant corpus, `score_fuse`'s empty-leg redistribution rescales the
    /// surviving FTS + graph legs, so an exact-word FTS hit still surfaces (recall safety).
    pub fn search_hybrid_visible(
        &self,
        query: &str,
        query_vec: &[f32],
        limit: i64,
        min_cosine: f32,
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
        let semantic = self.search_semantic_visible(query_vec, limit, min_cosine, unlocked)?;

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
        let knn_scored =
            self.knn_meeting_distances(query_vec, limit, min_cosine, unlocked, range)?;
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
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let date = date_clause(Some(range));
        let sql = format!(
            "SELECT m.id, m.started_at, m.ended_at, m.title, m.duration_s, m.audio_path, m.status,
                    m.folder_id
               FROM meetings m
              WHERE {meeting_visible}{date}
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
                   WHERE folder_id = ?1 AND kind IN ('note','document')
                   ORDER BY created_at DESC, name",
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
        // Meetings — use the canonical/legacy-conservative visibility oracle.
        let m_visible = meeting_visibility_clause("m", unlocked);
        let meeting_count: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM meetings m WHERE {m_visible}"
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

    /// `(chunk_count, vector_count)` for ONE document's `doc_chunks` / `doc_vec_chunks` — COUNTS
    /// ONLY, never content (no text leaves this probe). Production caller: the startup repair tick
    /// (`backfill_missing_brain_indexes`), which passes ONLY ids already returned by the gated
    /// [`Db::visible_document_ids`] — the same posture as [`Db::document_has_chunks`].
    pub fn doc_chunk_vector_counts(&self, document_id: &str) -> Result<(i64, i64)> {
        let conn = self.lock();
        conn.query_row(
            "SELECT
               (SELECT COUNT(*) FROM doc_chunks WHERE document_id = ?1),
               (SELECT COUNT(*) FROM doc_vec_chunks WHERE chunk_id IN
                  (SELECT id FROM doc_chunks WHERE document_id = ?1))",
            rusqlite::params![document_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(map_err)
    }

    /// `(chunk_count, vector_count)` for ONE meeting's `note_chunks` / `vec_chunks` — COUNTS ONLY,
    /// never content. The MEETING analogue of [`Db::doc_chunk_vector_counts`], powering the startup
    /// repair tick's needs-a-reindex probe; callers pass ONLY ids already returned by the gated
    /// `list_meetings_visible`.
    pub fn meeting_chunk_vector_counts(&self, meeting_id: &str) -> Result<(i64, i64)> {
        let conn = self.lock();
        conn.query_row(
            "SELECT
               (SELECT COUNT(*) FROM note_chunks WHERE meeting_id = ?1),
               (SELECT COUNT(*) FROM vec_chunks v
                  JOIN note_chunks nc ON nc.id = v.chunk_id
                 WHERE nc.meeting_id = ?1)",
            rusqlite::params![meeting_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(map_err)
    }

    /// The `text` column of every `note_chunks` row for a MEETING (insertion order). Test-only
    /// reader: lets the edit/rename re-index regressions assert stale text is GONE from the index
    /// without reaching the private connection.
    #[cfg(test)]
    pub(crate) fn note_chunk_texts(&self, meeting_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT text FROM note_chunks WHERE meeting_id = ?1 ORDER BY id ASC")
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

    /// The `text` column of every `doc_chunks` row for a document (insertion order). Test-only
    /// reader for the reindex kind-routing regression (front-matter must never reach the chunks).
    #[cfg(test)]
    pub(crate) fn doc_chunk_texts(&self, document_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT text FROM doc_chunks WHERE document_id = ?1 ORDER BY id ASC")
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![document_id], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
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
    ///
    /// Brain v3 PR-2: `documents.text` may store a PR-2 upload's block STRUCTURE (page/heading markers,
    /// control-char sentinels invisible to any human text). This getter returns the RAW stored text
    /// (structure intact) so re-index can reconstruct the hierarchy; the RENDERED display text is
    /// produced by the command layer via [`crate::extract::render_display_text`] before it reaches the
    /// FE. (md/txt/note/legacy rows have no markers → raw == display.)
    pub fn get_document(&self, id: &str) -> Result<Option<(String, String, String)>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT folder_id, name, text FROM documents
              WHERE id = ?1 AND kind IN ('note','document')",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(map_err)
    }

    /// GATED full-body read of ONE document/note by id (Feature D — the `get_document` tool). A
    /// STRUCTURAL CLONE of [`Db::search_doc_chunks_fts_visible`]'s gate: the same
    /// `documents d JOIN folders f ON f.id = d.folder_id` + `WHERE d.id = ?1 AND {visible}`
    /// predicate, so a document in a sealed-and-not-session-unlocked folder resolves to `None` — a
    /// FULL None, never a masked partial (so the tool's "No data for document" sentinel is
    /// indistinguishable from a never-existed id). Reads BOTH `kind='note'` and `kind='document'`
    /// (and explicitly excludes internal `kind='task'` source journals). The body is the plaintext
    /// `documents.text`; while a folder is sealed
    /// that column is blanked and the row is invisible here anyway, so no sealed
    /// ciphertext-behind-a-blank leaks. The JOIN is INNER (not LEFT) — matching the doc-search
    /// readers — because `documents.folder_id` is `NOT NULL` with a `folders(id)` FK, so every
    /// document has exactly one real folder to gate on (unlike `notes`, whose nullable root folder
    /// needs a LEFT JOIN); an INNER JOIN is fail-closed (no folder row ⇒ no result).
    pub fn get_document_if_visible(
        &self,
        id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<DocumentSummary>> {
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let sql = format!(
            "SELECT d.id, d.folder_id, d.kind, d.name, d.title, COALESCE(d.text, ''), d.created_at, d.updated_at
               FROM documents d
               JOIN folders f ON f.id = d.folder_id
              WHERE d.id = ?1 AND d.kind IN ('note','document') AND {visible}"
        );
        conn.query_row(&sql, rusqlite::params![id], |r| {
            let raw: String = r.get(5)?;
            Ok(DocumentSummary {
                id: r.get(0)?,
                folder_id: r.get(1)?,
                kind: r.get(2)?,
                name: r.get(3)?,
                title: r.get(4)?,
                // Brain v3 PR-2: render clean display text (strip the block-structure markers a PR-2
                // upload stores). A note / legacy row has no markers → unchanged.
                markdown: crate::extract::render_display_text(&raw),
                created_at: r.get(6)?,
                updated_at: r.get(7)?,
            })
        })
        .optional()
        .map_err(map_err)
    }

    pub(crate) fn get_document_if_visible_kind(
        &self,
        id: &str,
        kind: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<DocumentSummary>> {
        if !matches!(kind, "note" | "document") {
            return Err(AppError::InvalidArg(
                "invalid dashboard document kind".into(),
            ));
        }
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let sql = format!(
            "SELECT d.id,d.folder_id,d.kind,d.name,d.title,COALESCE(d.text,''),d.created_at,d.updated_at
               FROM documents d JOIN folders f ON f.id=d.folder_id
              WHERE d.id=?1 AND d.kind=?2 AND {visible}"
        );
        conn.query_row(&sql, rusqlite::params![id, kind], |row| {
            let raw: String = row.get(5)?;
            Ok(DocumentSummary {
                id: row.get(0)?,
                folder_id: row.get(1)?,
                kind: row.get(2)?,
                name: row.get(3)?,
                title: row.get(4)?,
                markdown: crate::extract::render_display_text(&raw),
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })
        .optional()
        .map_err(map_err)
    }

    pub(crate) fn document_is_visible(&self, id: &str, unlocked: &HashSet<String>) -> Result<bool> {
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        conn.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM documents d JOIN folders f ON f.id=d.folder_id WHERE d.id=?1 AND d.kind='document' AND {visible})"),
            [id], |row| row.get(0),
        ).map_err(map_err)
    }

    // `folder_for_document` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `document_ids_in_folder` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `raw_documents_in_folder` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `seal_document` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_document_text` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `clear_document_blob` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    /// Permanently delete a document (its `doc_chunks` + `doc_vec_chunks` go first in the same tx —
    /// `doc_vec_chunks` is a vec0 virtual table with no FK so the `documents` ON DELETE CASCADE
    /// reaches `doc_chunks` but NOT `doc_vec_chunks`; deleting them explicitly avoids orphan vectors,
    /// mirroring `delete_meeting`). Idempotent on an unknown id.
    pub fn delete_document(&self, id: &str) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let is_task = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM documents WHERE id=?1 AND kind='task')",
                [id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(map_err)?
            != 0;
        if is_task {
            return Err(AppError::InvalidArg(
                "task sources must be deleted through the task lifecycle".into(),
            ));
        }
        let has_share_closure = Self::source_closure_ready_for_delete_tx(&tx, "document", id)?;
        Self::purge_doc_chunks_tx(&tx, &[id.to_string()])?;
        // Brain v3 PR-3: purge every `links` row whose SRC OR DST is this deleted document/note in the
        // same tx — a note id IS a document id, so both kinds are covered by `purge_links_tx`. A
        // permanent DELETE keeps NO decision row (`preserve_decisions=false`).
        Self::purge_links_tx(&tx, &[], &[id.to_string()], false)?;
        // Vault Audit: a pending finding sourcing or targeting this document/note quotes its
        // content/title — drop it in the same delete tx (mirrors `delete_meeting`'s purge).
        Self::purge_pending_audit_findings_tx(&tx, &[id.to_string()])?;
        // Resolved BEFORE the row goes: after the DELETE there is no `folder_id` left to read.
        let ask_scope = Self::ask_scope_for_documents_tx(&tx, &[id.to_string()])?;
        Self::purge_ask_conversations_for_scope_tx(&tx, ask_scope.as_ref())?;
        tx.execute("DELETE FROM documents WHERE id = ?1", rusqlite::params![id])
            .map_err(map_err)?;
        if has_share_closure {
            tx.execute(
                "UPDATE org_share_closures SET phase='closed'
                  WHERE scope_kind='document' AND scope_id=?1 AND phase='closing'",
                rusqlite::params![id],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// If a destructive share barrier exists, deletion may cross it only after every local
    /// journal for the exact source is terminal. The caller's parent DELETE, attachment cascade,
    /// and closure phase transition then commit in the same SQLite transaction.
    fn source_closure_ready_for_delete_tx(
        tx: &rusqlite::Transaction<'_>,
        source_kind: &str,
        source_id: &str,
    ) -> Result<bool> {
        let has_closure = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM org_share_closures
                  WHERE scope_kind=?1 AND scope_id=?2 AND phase='closing')",
                rusqlite::params![source_kind, source_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(map_err)?
            != 0;
        let nonterminal: i64 = match source_kind {
            "meeting" => tx
                .query_row(
                    "SELECT
                       (SELECT COUNT(*) FROM outbound_shares
                         WHERE meeting_id=?1 AND state<>'revoked') +
                       (SELECT COUNT(*) FROM org_shares
                         WHERE meeting_id=?1 AND state<>'revoked')",
                    [source_id],
                    |row| row.get(0),
                )
                .map_err(map_err)?,
            "document" => tx
                .query_row(
                    "SELECT
                       (SELECT COUNT(*) FROM outbound_shares
                         WHERE document_id=?1 AND state<>'revoked') +
                       (SELECT COUNT(*) FROM org_shares
                         WHERE document_id=?1 AND state<>'revoked')",
                    [source_id],
                    |row| row.get(0),
                )
                .map_err(map_err)?,
            _ => {
                return Err(crate::error::AppError::InvalidArg(
                    "invalid share closure source".into(),
                ));
            }
        };
        if nonterminal != 0 {
            return Err(crate::error::AppError::Unavailable(
                if has_closure {
                    "remote share revocation is not yet durably complete"
                } else {
                    "source deletion requires a durable share closure before remote revocation"
                }
                .into(),
            ));
        }
        Ok(has_closure)
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
        self.index_document_chunks_progress(document_id, embedder, &no_embed_progress)
    }

    /// [`Db::index_document_chunks`] with a per-sub-batch embed-progress callback (Brain v3 PR-4,
    /// Fix 3): reports `(done, total)` embed sub-batches so the import path can stream "Embedding k/M"
    /// to the FE. Identical behavior otherwise — the no-op default preserves every existing caller.
    pub fn index_document_chunks_progress(
        &self,
        document_id: &str,
        embedder: Option<&dyn Embedder>,
        embed_progress: &EmbedProgressFn<'_>,
    ) -> Result<()> {
        self.index_document_chunks_progress_at_epoch(document_id, embedder, embed_progress, None)
            .map(|_| ())
    }

    pub(crate) fn index_document_chunks_background(
        &self,
        document_id: &str,
        embedder: Option<&dyn Embedder>,
        epoch: u64,
    ) -> Result<bool> {
        self.index_document_chunks_progress_at_epoch(
            document_id,
            embedder,
            &no_embed_progress,
            Some(epoch),
        )
        .map(|committed| committed.is_some())
    }

    fn index_document_chunks_progress_at_epoch(
        &self,
        document_id: &str,
        embedder: Option<&dyn Embedder>,
        embed_progress: &EmbedProgressFn<'_>,
        background_epoch: Option<u64>,
    ) -> Result<Option<()>> {
        let Some((_folder_id, name, text)) = self.get_document(document_id)? else {
            return Ok(Some(())); // unknown document — nothing to index.
        };
        // Brain v3 PR-2 — HIERARCHICAL chunking. Reconstruct the extracted BLOCKS (page/heading) from
        // the stored text (lossless for a PR-2 upload; a legacy/flat row reconstructs as one block →
        // identical leaves + a summary), then build the L0/L1/L2 tree. `name` provenance carries into
        // the deterministic contextual header on the embed text.
        let blocks = crate::extract::blocks_from_stored_text(&text);
        // READ-TIME REFLOW (doc-preview fix): de-fragment pathologically letter-spaced PDF text on a
        // COPY before chunking, so (re)indexed chunks/embeddings retrieve on clean words
        // (`"Fron\nt\nend"` → `"Frontend"`) instead of shattered glyph fragments. The gate is
        // conservative — md/txt/note/legacy + clean PDF pages are byte-identical no-ops, so a normal
        // document's chunk input is unchanged. `documents.text` at rest is NEVER mutated: only the
        // in-memory block text fed to the chunker is reflowed.
        let blocks: Vec<crate::extract::ExtractedBlock> = blocks
            .into_iter()
            .map(|b| crate::extract::ExtractedBlock {
                text: crate::extract::reflow::reflow_fragmented_text(&b.text),
                ..b
            })
            .collect();
        let hier = crate::embed::chunk_document_hierarchical(&name, &blocks);
        // Embed ONLY the embed-worthy chunks (L0 leaves + L2 summary; L1 parents are FTS-only). We
        // sub-batch to bound the per-call Metal tensor for a large PDF (mirror index_meeting_chunks).
        // Build the parallel embed-text list, remembering which HierChunk index each maps to.
        let embed_indices: Vec<usize> = hier
            .iter()
            .enumerate()
            .filter(|(_, c)| c.embed)
            .map(|(i, _)| i)
            .collect();
        let embed_texts: Vec<String> = embed_indices
            .iter()
            .map(|&i| hier[i].embed_text.clone())
            .collect();
        let embed_vecs: Vec<Vec<f32>> = match embedder {
            Some(e) if !embed_texts.is_empty() => {
                embed_in_sub_batches_progress(e, &embed_texts, embed_progress)?
            }
            _ => Vec::new(), // model absent → chunk-only (FTS still covers it); vectors come later.
        };
        // Map HierChunk-index → its vector (only for embed-worthy chunks that actually got embedded).
        let mut vec_by_hier: std::collections::HashMap<usize, &Vec<f32>> =
            std::collections::HashMap::new();
        for (slot, &hi) in embed_indices.iter().enumerate() {
            if let Some(v) = embed_vecs.get(slot) {
                vec_by_hier.insert(hi, v);
            }
        }

        let this_doc = [document_id.to_string()];
        let commit = || -> Result<()> {
            let mut conn = self.lock();
            let tx = conn.transaction().map_err(map_err)?;
            if doc_sealed_at_rest_tx(&tx, document_id)? {
                return Ok(()); // sealed-at-rest mid-flight: never persist its plaintext chunks.
            }
            Self::purge_doc_chunks_tx(&tx, &this_doc)?;
            {
                let mut ins_chunk = tx
                    .prepare(
                        "INSERT INTO doc_chunks
                       (document_id, chunk_index, text, level, parent_id, section_path, page_no)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    )
                    .map_err(map_err)?;
                let mut ins_vec = tx
                    .prepare("INSERT INTO doc_vec_chunks(chunk_id, embedding) VALUES (?1, ?2)")
                    .map_err(map_err)?;
                // HierChunk-index → the inserted row id, so a leaf can point `parent_id` at its L1 row.
                // The chunker emits every parent BEFORE its children, so the parent id is always known.
                let mut row_id_by_hier: Vec<i64> = vec![0; hier.len()];
                for (idx, c) in hier.iter().enumerate() {
                    let parent_row: Option<i64> = c.parent.map(|pi| row_id_by_hier[pi]);
                    ins_chunk
                        .execute(rusqlite::params![
                            document_id,
                            idx as i64,
                            c.raw,
                            c.level,
                            parent_row,
                            c.section_path,
                            c.page_no,
                        ])
                        .map_err(map_err)?;
                    let chunk_id = tx.last_insert_rowid();
                    row_id_by_hier[idx] = chunk_id;
                    // Vector ONLY for embed-worthy chunks that were actually embedded (L0+L2, model
                    // present). L1 parents NEVER get a vec0 row — the vector count stays flat.
                    if let Some(vector) = vec_by_hier.get(&idx) {
                        let blob = crate::embed::vec_to_blob(vector);
                        ins_vec
                            .execute(rusqlite::params![chunk_id, blob])
                            .map_err(map_err)?;
                    }
                }
            }
            tx.commit().map_err(map_err)?;
            Ok(())
        };
        match background_epoch {
            Some(epoch) => crate::perf::with_current_background_epoch(epoch, commit),
            None => commit().map(Some),
        }
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
        self.index_note_chunks_progress(note_id, title, body, embedder, &no_embed_progress)
    }

    /// [`Db::index_note_chunks`] with a per-sub-batch embed-progress callback (Brain v3 PR-4, Fix 3):
    /// an imported NOTE document streams "Embedding k/M" like an uploaded document. Identical behavior
    /// otherwise — the no-op default preserves every existing caller (recording Stop, reindex).
    pub fn index_note_chunks_progress(
        &self,
        note_id: &str,
        title: &str,
        body: &str,
        embedder: Option<&dyn Embedder>,
        embed_progress: &EmbedProgressFn<'_>,
    ) -> Result<()> {
        self.index_note_chunks_progress_at_epoch(
            note_id,
            title,
            body,
            embedder,
            embed_progress,
            None,
        )
        .map(|_| ())
    }

    pub(crate) fn index_note_chunks_background(
        &self,
        note_id: &str,
        title: &str,
        body: &str,
        embedder: Option<&dyn Embedder>,
        epoch: u64,
    ) -> Result<bool> {
        self.index_note_chunks_progress_at_epoch(
            note_id,
            title,
            body,
            embedder,
            &no_embed_progress,
            Some(epoch),
        )
        .map(|committed| committed.is_some())
    }

    fn index_note_chunks_progress_at_epoch(
        &self,
        note_id: &str,
        title: &str,
        body: &str,
        embedder: Option<&dyn Embedder>,
        embed_progress: &EmbedProgressFn<'_>,
        background_epoch: Option<u64>,
    ) -> Result<Option<()>> {
        let chunks = crate::embed::chunk_note(title, "", body);
        // Sub-batch to bound the per-call Metal tensor for a long note (mirror
        // index_meeting_chunks/index_document_chunks — this indexer was the one remaining
        // whole-item single-call embed, brain-v3 audit H3).
        let vectors = match embedder {
            Some(e) if !chunks.is_empty() => {
                embed_in_sub_batches_progress(e, &chunks, embed_progress)?
            }
            _ => Vec::new(), // model absent → chunk-only (FTS still covers it); vectors come later.
        };
        let this_doc = [note_id.to_string()];
        let commit = || -> Result<()> {
            let mut conn = self.lock();
            let tx = conn.transaction().map_err(map_err)?;
            if doc_sealed_at_rest_tx(&tx, note_id)? {
                return Ok(()); // sealed-at-rest mid-flight: never persist its plaintext chunks.
            }
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
        };
        match background_epoch {
            Some(epoch) => crate::perf::with_current_background_epoch(epoch, commit),
            None => commit().map(Some),
        }
    }

    // `reparent_note_folder` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

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
        // Brain v3 PR-3 LINK-ENGINE LOCK-SAFETY: purge every DERIVED `links` row whose SRC OR DST is a
        // just-sealed document/note (a note id IS a document id) in this SAME seal tx — a link names
        // a neighbour, so it must not survive at rest for a sealed endpoint. Re-derived on unlock. A
        // SEAL preserves the user's decision rows (`preserve_decisions=true`, Fix 1).
        Self::purge_links_tx(&tx, &[], document_ids, true)?;
        // Vault Audit LOCK-SAFETY: the callers are seal-side (`lock_folder`'s document leg, the
        // relock reblank) — purge ALL pending findings in this SAME tx (rollup posture; evidence
        // may cite third-party titles no document id can match). Findings are cheap re-derivable
        // rows — the next pass re-stages anything still true (never content loss).
        Self::purge_all_pending_audit_findings_tx(&tx)?;
        // GLOBAL, for the same reason as `purge_chunks_for_meetings` above: this is a seal-side
        // helper, and a future document-move path reusing it the way `seal_moved_note` reuses that
        // one would re-derive its scope from an already-reassigned `folder_id`. The equivalent
        // note-move path (`move_note_with_attachments_sealed`) deliberately calls the global sweep
        // directly today; keeping this one global stops the two from disagreeing.
        Self::purge_all_ask_conversations_tx(&tx)?;
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Delete doc-chunk rows for `document_ids` within an EXISTING transaction (so the purge lands in
    /// the same atomic unit as the plaintext blanking on lock). vec0 first (its FK-less rowid mirrors
    /// doc_chunks.id), then the source rows. Mirrors [`Db::purge_chunks_tx`].
    pub(crate) fn purge_doc_chunks_tx(
        tx: &rusqlite::Transaction<'_>,
        document_ids: &[String],
    ) -> Result<()> {
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

    /// Map one doc-chunk candidate row (the shared 10-column SELECT of the two doc readers) into a
    /// [`DocChunkHit`] carrying the chunk's hierarchy metadata. `sibling_hits` starts at 0 — it is
    /// populated by [`Db::fold_doc_candidates`] for the winner of each document's dedup.
    fn doc_candidate_from_row(row: &Row<'_>) -> rusqlite::Result<DocChunkHit> {
        Ok(DocChunkHit {
            document_id: row.get(0)?,
            name: row.get(1)?,
            folder_id: row.get(2)?,
            snippet: row.get(3)?,
            kind: row.get(4)?,
            chunk_id: row.get(5)?,
            parent_id: row.get(6)?,
            section_path: row.get(7)?,
            page_no: row.get(8)?,
            level: row.get(9)?,
            sibling_hits: 0,
        })
    }

    /// Fold a BEST-FIRST pre-dedup doc-chunk candidate stream into the per-document deduped hit
    /// list. The first candidate seen for a document WINS (exactly the nearest-KNN / best-bm25
    /// dedup the callers have always had — ranking and snippets stay byte-identical), and later
    /// candidates only feed the winner's `sibling_hits`: the count of distinct L0 leaves under the
    /// winning chunk's L1 parent present in the candidate set (winner included). `limit` caps how
    /// many DOCUMENTS are collected (`None` = all); candidates past the limit still corroborate
    /// siblings for already-collected winners. Audit Fix 1: this is what makes parent expansion
    /// hit-aligned and sibling-gated instead of query-independent.
    fn fold_doc_candidates(
        candidates: impl Iterator<Item = rusqlite::Result<DocChunkHit>>,
        limit: Option<usize>,
    ) -> Result<Vec<DocChunkHit>> {
        let mut idx_by_doc: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut hits: Vec<DocChunkHit> = Vec::new();
        for cand in candidates {
            let cand = cand.map_err(map_err)?;
            match idx_by_doc.get(&cand.document_id) {
                None => {
                    if limit.map_or(true, |l| hits.len() < l) {
                        // MSRV 1.77: no Option::is_none_or (stable 1.82)
                        let mut winner = cand;
                        // The winner counts itself when it is a leaf under a real parent.
                        winner.sibling_hits =
                            u32::from(winner.level == 0 && winner.parent_id.is_some());
                        idx_by_doc.insert(winner.document_id.clone(), hits.len());
                        hits.push(winner);
                    }
                }
                Some(&i) => {
                    let w = &mut hits[i];
                    if cand.level == 0
                        && w.parent_id.is_some()
                        && cand.parent_id == w.parent_id
                        && cand.chunk_id != w.chunk_id
                    {
                        w.sibling_hits += 1;
                    }
                }
            }
        }
        Ok(hits)
    }

    /// GATED semantic (vector KNN) search over DOCUMENT chunks. Runs a `doc_vec_chunks` KNN for the
    /// top-`k` nearest chunks, then applies EXACTLY the `visibility_clause` predicate (joined
    /// doc_chunks → documents → folders) so a chunk in a sealed-and-not-session-unlocked folder is
    /// EXCLUDED even if a stray chunk survived purge — the same defense-in-depth as
    /// `search_semantic_visible`. Dedups to one hit per document (best/nearest) while counting the
    /// winner's `sibling_hits` across the pre-dedup KNN candidates ([`Db::fold_doc_candidates`]).
    /// Returns the chunk snippet + the document name + its folder id + the winning chunk's
    /// hierarchy metadata (NO meeting — documents are not meetings).
    ///
    /// `min_cosine` (S1) is the OPT-IN vector-leg relevance floor (cosine mapped from the vec0 L2
    /// distance via [`crate::links::cosine_from_l2_distance`]); a below-floor candidate is dropped
    /// as noise. Sentinel `0.0` = NO floor. Applied AFTER the SQL visibility gate, so it can only
    /// ever REMOVE rows.
    pub fn search_doc_chunks_visible(
        &self,
        query_vec: &[f32],
        k: i64,
        min_cosine: f32,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<DocChunkHit>> {
        if query_vec.is_empty() || k <= 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        // KNN isolated to the vec0 table in a CTE; visibility + document columns joined OUTSIDE it.
        // The trailing `knn.distance` column feeds the S1 relevance floor (dropped below-floor).
        let sql = format!(
            "WITH knn(chunk_id, distance) AS (
                 SELECT chunk_id, distance FROM doc_vec_chunks
                  WHERE embedding MATCH ?1 AND k = ?2
                  ORDER BY distance
             )
             SELECT d.id, d.name, d.folder_id, dc.text, d.kind,
                    dc.id, dc.parent_id, dc.section_path, dc.page_no, dc.level, knn.distance
               FROM knn
               JOIN doc_chunks dc ON dc.id = knn.chunk_id
               JOIN documents d ON d.id = dc.document_id
               JOIN folders f ON f.id = d.folder_id
              WHERE d.kind IN ('note','document') AND {visible}
              ORDER BY knn.distance ASC, d.id ASC"
        );
        let blob = crate::embed::vec_to_blob(query_vec);
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![blob, k], |row| {
                let hit = Self::doc_candidate_from_row(row)?;
                let distance: f64 = row.get(10)?;
                Ok((hit, distance))
            })
            .map_err(map_err)?;
        // Drop below-floor candidates (0.0 = no floor) BEFORE the per-document dedup/fold, so the
        // fold's winner is the nearest SURVIVING chunk. The k-row KNN CTE bounds the candidate set.
        let filtered = rows.filter_map(|r| match r {
            Ok((hit, distance)) => {
                if min_cosine > 0.0
                    && crate::links::cosine_from_l2_distance(distance as f32) < min_cosine
                {
                    None
                } else {
                    Some(Ok(hit))
                }
            }
            Err(e) => Some(Err(e)),
        });
        Self::fold_doc_candidates(filtered, None)
    }

    /// GATED keyword (FTS5/BM25) search over DOCUMENT chunks — the model-free twin of
    /// [`Db::search_doc_chunks_visible`], so documents/brain notes are reachable on a DEFAULT
    /// install (no e5 model, semantic flag off). Applies EXACTLY the `visibility_clause` predicate
    /// (joined doc_chunks → documents → folders) so a chunk in a sealed-and-not-session-unlocked
    /// folder is EXCLUDED even if a stray chunk survived purge — defense-in-depth on top of the
    /// trigger-purged FTS index. Dedups to one hit per document (best bm25), capped at `limit`;
    /// the scan continues past the cap (bounded at 10×`limit` rows) ONLY to count the winners'
    /// `sibling_hits` — the returned documents, order, and snippets are unchanged.
    pub fn search_doc_chunks_fts_visible(
        &self,
        query: &str,
        limit: i64,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<DocChunkHit>> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let q = query.trim();
        let Some(and_expr) = fts_match_query(q) else {
            return Ok(Vec::new()); // punctuation-only / empty query → no hits, never an FTS error.
        };
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let sql = format!(
            "SELECT d.id, d.name, d.folder_id, dc.text, d.kind,
                    dc.id, dc.parent_id, dc.section_path, dc.page_no, dc.level
               FROM fts_doc_chunks
               JOIN doc_chunks dc ON dc.id = fts_doc_chunks.rowid
               JOIN documents d ON d.id = dc.document_id
               JOIN folders f ON f.id = d.folder_id
              WHERE fts_doc_chunks MATCH ?1
                AND d.kind IN ('note','document') AND {visible}
              ORDER BY bm25(fts_doc_chunks) ASC, d.id ASC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        // Bound the sibling-count scan so a stop-word-ish query over a large corpus stays cheap;
        // sibling counts are then a LOWER bound, which can only under-trigger expansion (safe).
        let scan_cap = (limit as usize).saturating_mul(10).max(64);
        // Collect the (already-gated) candidate rows for a given match expression, bounded by scan_cap.
        let run = |stmt: &mut rusqlite::Statement, expr: &str| -> Result<Vec<DocChunkHit>> {
            let rows = stmt
                .query_map(rusqlite::params![expr], Self::doc_candidate_from_row)
                .map_err(map_err)?;
            let mut cands = Vec::new();
            for r in rows.take(scan_cap) {
                cands.push(r.map_err(map_err)?);
            }
            Ok(cands)
        };
        // S2 AND→OR fallback: implicit-AND matched nothing ⇒ retry with the content-word OR twin.
        // Fires only on an empty AND result — never widens a successful query.
        let mut cands = run(&mut stmt, &and_expr)?;
        if cands.is_empty() {
            if let Some(any_expr) = fts_match_query_any(q) {
                if any_expr != and_expr {
                    cands = run(&mut stmt, &any_expr)?;
                }
            }
        }
        Self::fold_doc_candidates(cands.into_iter().map(Ok), Some(limit as usize))
    }

    /// Brain v3 audit Fix 1 — HIT-ALIGNED, SIBLING-GATED parent expansion (LlamaIndex auto-merging
    /// semantics). For each of the given top fused doc `hits`, return the text of the WINNING
    /// chunk's OWN L1 section-parent (`doc_chunks WHERE id = hit.parent_id`) — but ONLY when the
    /// retrieval corroborated that section (`sibling_hits >= 2`: at least two distinct L0 leaves of
    /// the same parent in the pre-dedup candidate set). A single-leaf hit yields no expansion row
    /// (the caller keeps the leaf snippet), and a FLAT hit (`section_path` `None`) NEVER expands —
    /// the flat L1 is the doc head, not a real section. The pre-fix dominant-by-leaf-count
    /// expansion replaced relevant retrieved snippets with an unrelated section's text.
    ///
    /// GATING (lock-model, load-bearing): each parent lookup applies EXACTLY the same
    /// `visibility_clause` predicate over `doc_chunks → documents → folders` as the doc-search
    /// readers — a document in a sealed-and-not-session-unlocked folder yields NOTHING here even
    /// for a STALE pre-seal hit (its `level=1` rows are purged while sealed anyway, and the gate is
    /// defense-in-depth on top). The parent row is additionally pinned to the hit's own document
    /// (`p.document_id = hit.document_id`) and to `p.level = 1`, so a stale/foreign `parent_id`
    /// can never fetch across documents or levels. Parents are DERIVED content — a pure gated READ.
    ///
    /// Returns one [`DocChunkHit`] per EXPANDED hit (`snippet` = the L1 parent text, already capped
    /// at ingest to ~6000 chars); non-expanded hits are simply absent.
    pub fn expand_doc_parents_visible(
        &self,
        hits: &[DocChunkHit],
        unlocked: &HashSet<String>,
    ) -> Result<Vec<DocChunkHit>> {
        if hits.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let sql = format!(
            "SELECT p.text
               FROM doc_chunks p
               JOIN documents d ON d.id = p.document_id
               JOIN folders f ON f.id = d.folder_id
              WHERE p.id = ?1 AND p.document_id = ?2 AND p.level = 1
                AND d.kind IN ('note','document') AND {visible}"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let mut out: Vec<DocChunkHit> = Vec::new();
        for h in hits {
            if h.sibling_hits < 2 || h.section_path.is_none() {
                continue; // uncorroborated or flat — the leaf snippet stays.
            }
            let Some(parent_id) = h.parent_id else {
                continue; // an L1/L2/legacy winner has no parent to expand to.
            };
            let text: Option<String> = stmt
                .query_row(rusqlite::params![parent_id, h.document_id], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(map_err)?;
            let Some(text) = text else {
                continue; // gated out (sealed-not-unlocked) or a vanished parent row.
            };
            if text.trim().is_empty() {
                continue;
            }
            out.push(DocChunkHit {
                document_id: h.document_id.clone(),
                name: h.name.clone(),
                folder_id: h.folder_id.clone(),
                snippet: text,
                kind: h.kind.clone(),
                chunk_id: parent_id,
                parent_id: None,
                section_path: h.section_path.clone(),
                page_no: h.page_no,
                level: 1,
                sibling_hits: h.sibling_hits,
            });
        }
        Ok(out)
    }

    /// Brain v3 audit Fix 3(b) — a document's structural OUTLINE: its section-parent (L1) + doc
    /// summary (L2) `doc_chunks` rows, in document order, carrying `section_path` + `page_no` (NOT
    /// the section body text — an outline is a MAP, not content). Deterministic; the agent reads it
    /// to plan targeted `get_document(offset, maxChars)` reads instead of blind char paging.
    ///
    /// GATING (lock-model, load-bearing): applies EXACTLY the same `visibility_clause` predicate over
    /// `doc_chunks → documents → folders` as every other doc reader, so a document in a
    /// sealed-and-not-session-unlocked folder yields an EMPTY outline (indistinguishable from a
    /// never-existed id / a flat legacy doc — never leaks locked-vs-absent). While sealed a folder's
    /// `doc_chunks` rows are purged anyway; the gate is defense-in-depth on top. A pure gated READ —
    /// no plaintext content leaves the DB (headings ARE user content, but they are the SAME
    /// section_path already surfaced on a search hit, and are gated identically).
    ///
    /// BOUNDED: at most `cap` entries (a huge deck can't blow the tool result). Legacy/flat docs (all
    /// rows level 0) have no L1/L2 rows → an empty outline, which the tool renders as "no outline".
    pub fn get_document_outline_if_visible(
        &self,
        id: &str,
        unlocked: &HashSet<String>,
        cap: usize,
    ) -> Result<Vec<DocOutlineEntry>> {
        if cap == 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        // L1 (section-parent) + L2 (doc summary) rows only — the heading/structure tree, never the
        // L0 leaf body NOR the synthetic L3 contact digest (which is a retrieval aid, not structure).
        // Ordered by document position (`chunk_index`) so the outline reads top-down.
        let sql = format!(
            "SELECT dc.level, dc.section_path, dc.page_no
               FROM doc_chunks dc
               JOIN documents d ON d.id = dc.document_id
               JOIN folders f ON f.id = d.folder_id
              WHERE dc.document_id = ?1 AND dc.level IN (1, 2)
                AND d.kind IN ('note','document') AND {visible}
              ORDER BY dc.chunk_index ASC
              LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![id, cap as i64], |r| {
                Ok(DocOutlineEntry {
                    level: r.get(0)?,
                    section_path: r.get(1)?,
                    page_no: r.get(2)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    // ── M6 Shared Brain — org feed INGEST + local RETRIEVAL ─────────────────────────────────────
    //
    // `upsert_org_item` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `tombstone_org_item` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `purge_org_replica` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `purge_org_item_chunks_tx` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `org_last_seq_for` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `search_org_chunks_knn` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `search_org_chunks_fts` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `dedup_org_hits_by_item` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `get_org_item` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_org_item_author` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `org_item_ids_with_null_author` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `org_item_edit_ctx` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `list_org_items` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `count_org_items` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `all_org_shared_content_hashes` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `org_items_needing_embed` moved to `storage::org_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // ── NOTES (authored `documents(kind='note')`) ───────────────────────────────────────────────

    // `insert_note` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `insert_note_sealed` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `get_note_row` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_document_meeting_id` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `companion_note_for_meeting` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `update_note_row` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `update_note_row_sealed` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `move_note_row_sealed` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_note_doc_exported_path` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `get_note_doc_exported_hash` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_note_doc_exported_hash` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `note_exported_paths_in_folder` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `note_exported_path_rows_in_folder` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `clear_note_exported_paths_in_folder` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `note_ids_in_folder` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_note_doc_folder` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `list_notes_visible` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // ── Feature C — TYPED note front-matter properties (note-folder schemas) ─────────────────────

    // `get_note_folder_schema` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_note_folder_schema` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `list_notes_visible_typed` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `note_markdown_if_visible` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // ── NOTE FOLDERS (`folders` with `kind='note'`) ──────────────────────────────────────────────

    // `ensure_default_note_folder` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `ensure_notes_root` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `note_root_id` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `folder_is_root` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_folder_is_root` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `first_free_note_root_path` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `insert_note_root` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `insert_note_folder` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `list_note_folders` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `note_folder_by_id` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `note_folder_by_name_or_id` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `folder_kind` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    /// Ids of every VISIBLE document (its folder open or session-unlocked), oldest-first. The
    /// reindex-backfill corpus: a sealed-and-not-unlocked folder's documents are NEVER returned, so
    /// their (blank) plaintext is never chunked and their index rows STAY purged.
    pub fn visible_document_ids(&self, unlocked: &HashSet<String>) -> Result<Vec<String>> {
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let sql = format!(
            "SELECT d.id FROM documents d
               JOIN folders f ON f.id = d.folder_id
              WHERE d.kind IN ('note','document') AND {visible}
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

    /// ATOMICALLY replace a meeting's transcript with `segments` — delete + insert in ONE
    /// transaction, so either the full fresh set lands or the previous rows survive untouched
    /// (never a half-replaced transcript, never a loss window). Needed by the from-disk
    /// re-transcription path (`retry_transcription` / disk salvage): a keyed
    /// `INSERT OR REPLACE` alone would leave a STALE TAIL when the fresh run yields fewer
    /// segments than a prior partial run (old idx 12..40 interleaved into the new transcript).
    /// The `_ad`/`_ai` FTS triggers fire inside the same transaction, so the search index stays
    /// consistent with the swap.
    pub fn replace_segments(&self, meeting_id: &str, segments: &[Segment]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        tx.execute(
            "DELETE FROM segments WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
        )
        .map_err(map_err)?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO segments
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

    /// Atomically persist every raw capture-lane row plus explicit ingest-time echo provenance.
    ///
    /// The provenance bit affects only the default merged presentation. Raw mic/system readers
    /// retain every row and stable raw index. Callers without measured acoustic evidence continue
    /// using [`Self::replace_segments`], whose omitted column resets to the legacy-safe default 0.
    pub fn replace_segments_with_echo_provenance(
        &self,
        meeting_id: &str,
        segments: &[StoredTranscriptSegment],
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        tx.execute(
            "DELETE FROM segments WHERE meeting_id = ?1",
            rusqlite::params![meeting_id],
        )
        .map_err(map_err)?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO segments
                       (meeting_id, idx, start_s, end_s, text, speaker, confidence, echo_suppressed)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )
                .map_err(map_err)?;
            for stored in segments {
                let segment = &stored.segment;
                stmt.execute(rusqlite::params![
                    meeting_id,
                    segment.idx,
                    segment.start_s,
                    segment.end_s,
                    segment.text,
                    segment.speaker,
                    segment.confidence,
                    i64::from(stored.echo_suppressed),
                ])
                .map_err(map_err)?;
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    // `delete_unsealed_segments` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    /// All segments for a meeting, ordered by `idx`.
    pub fn get_segments(&self, meeting_id: &str) -> Result<Vec<Segment>> {
        Ok(self
            .get_segments_with_echo_provenance(meeting_id)?
            .into_iter()
            .map(|stored| stored.segment)
            .collect())
    }

    /// All raw stored segment rows plus their explicit ingest-time echo provenance.
    ///
    /// The additive column defaults to false for every legacy row. This reader is intentionally
    /// ungated like `get_segments`; callers must first pass the meeting visibility boundary.
    pub fn get_segments_with_echo_provenance(
        &self,
        meeting_id: &str,
    ) -> Result<Vec<StoredTranscriptSegment>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT idx, start_s, end_s, text, speaker, confidence, echo_suppressed
                   FROM segments WHERE meeting_id = ?1 ORDER BY idx",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![meeting_id], |row| {
                Ok(StoredTranscriptSegment {
                    segment: Segment {
                        idx: row.get(0)?,
                        start_s: row.get(1)?,
                        end_s: row.get(2)?,
                        text: row.get(3)?,
                        // NULL (legacy / unattributed rows) → None.
                        speaker: row.get(4)?,
                        // NULL (legacy / Fast-path rows) → None; a stored REAL → Some(f32).
                        confidence: row.get(5)?,
                    },
                    echo_suppressed: row.get::<_, i64>(6)? != 0,
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

    // `upsert_note` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `upsert_note_sealed` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `get_note` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `latest_note` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `latest_note_visible` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `get_latest_note_for_meeting` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_note_exported_path` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `get_note_exported_hash` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_note_exported_hash` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

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

    /// Visibility-gated timeline read for MCP chapter navigation.
    ///
    /// Topic labels and speaker names are derived note content. The plaintext timeline being
    /// blanked on seal is only defense-in-depth; this query independently applies the same meeting
    /// visibility predicate as search and `get_meeting`. Locked and absent meetings both return
    /// `None`, so the caller cannot turn chapter availability into an existence oracle.
    pub fn get_timeline_data_visible(
        &self,
        meeting_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<String>> {
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let sql = format!(
            "SELECT t.data
               FROM timelines t
               JOIN meetings m ON m.id = t.meeting_id
              WHERE m.id = ?1
                AND {meeting_visible}"
        );
        conn.query_row(&sql, rusqlite::params![meeting_id], |row| row.get(0))
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

    // `set_timeline_data_sealed` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // ── meeting tags ─────────────────────────────────────────────────────────

    // `set_meeting_tags` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `get_meeting_tags` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `list_all_tags` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `list_meetings_by_tag` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

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

    // ── note templates (user-authored named sections) ────────────────────────
    //
    // CONTENT-FREE, single-user metadata (mirrors `saved_recipes`): a note SHAPE only, never
    // meeting content, so these paths are NOT visibility-gated. `sections` and
    // `extra_frontmatter_keys` are stored as JSON TEXT and parsed here; a corrupt/legacy row
    // falls back to an empty list rather than failing the whole read.

    pub fn list_note_templates(&self) -> Result<Vec<NoteTemplate>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, name, tone, sections, extra_frontmatter_keys, created_at \
                 FROM note_templates ORDER BY created_at DESC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                let sections_json: String = r.get(3)?;
                let extra_json: String = r.get(4)?;
                Ok(NoteTemplate {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    tone: r.get(2)?,
                    sections: serde_json::from_str::<Vec<NoteTemplateSection>>(&sections_json)
                        .unwrap_or_default(),
                    extra_frontmatter_keys: serde_json::from_str::<Vec<String>>(&extra_json)
                        .unwrap_or_default(),
                    created_at: r.get(5)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    pub fn insert_note_template(&self, t: &NoteTemplate) -> Result<()> {
        let sections_json = serde_json::to_string(&t.sections)
            .map_err(|e| AppError::Storage(format!("serialize note-template sections: {e}")))?;
        let extra_json = serde_json::to_string(&t.extra_frontmatter_keys)
            .map_err(|e| AppError::Storage(format!("serialize note-template keys: {e}")))?;
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO note_templates \
             (id, name, tone, sections, extra_frontmatter_keys, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                t.id,
                t.name,
                t.tone,
                sections_json,
                extra_json,
                t.created_at
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub fn delete_note_template(&self, id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM note_templates WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    // ── saved views (Feature B) ──────────────────────────────────────────────
    //
    // CONTENT-FREE, single-user metadata (mirrors `saved_recipes`): these store only a VIEW
    // DEFINITION (an opaque FE-owned `config` JSON blob + presentation fields), never meeting
    // content. They are therefore NOT visibility-gated — there is nothing sealed to leak. The
    // ACTUAL content aggregation the meetings surface renders (`list_meeting_action_summaries`,
    // below) IS gated, exactly like `list_open_commitments`.

    /// All saved views for one list `scope`, ordered as the user arranged them (sort_order, then
    /// creation for stability). No visibility gate — view metadata only.
    pub fn list_saved_views(&self, scope: &str) -> Result<Vec<SavedView>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, scope, name, layout, config, sort_order, created_at, updated_at
                   FROM saved_views
                  WHERE scope = ?1
                  ORDER BY sort_order, created_at",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![scope], |r| {
                Ok(SavedView {
                    id: r.get(0)?,
                    scope: r.get(1)?,
                    name: r.get(2)?,
                    layout: r.get(3)?,
                    config: r.get(4)?,
                    sort_order: r.get(5)?,
                    created_at: r.get(6)?,
                    updated_at: r.get(7)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Insert-or-replace a saved view (create on a fresh id, edit on an existing one).
    pub fn upsert_saved_view(&self, v: &SavedView) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO saved_views
               (id, scope, name, layout, config, sort_order, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                v.id,
                v.scope,
                v.name,
                v.layout,
                v.config,
                v.sort_order,
                v.created_at,
                v.updated_at,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Delete a saved view by id (idempotent).
    pub fn delete_saved_view(&self, id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM saved_views WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Persist a user reordering: assign `sort_order = position` to each id in `ordered_ids`, scoped
    /// to `scope` (so a stray id from another scope can't be moved). Batched in ONE transaction so a
    /// crash mid-reorder leaves the previous order intact (all-or-nothing).
    pub fn reorder_saved_views(&self, scope: &str, ordered_ids: &[String]) -> Result<()> {
        let conn = self.lock();
        let tx = conn.unchecked_transaction().map_err(map_err)?;
        for (pos, id) in ordered_ids.iter().enumerate() {
            tx.execute(
                "UPDATE saved_views SET sort_order = ?1 WHERE id = ?2 AND scope = ?3",
                rusqlite::params![pos as i64, id, scope],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Per-meeting open/done action-item counts for the saved-views meetings surface. GATED exactly
    /// like [`Self::list_open_commitments`]: only VISIBLE meetings are enumerated
    /// (`list_meetings_visible`) and only the VISIBLE note is read (`get_note_if_visible` → `None`
    /// for sealed-and-not-session-unlocked). A sealed meeting contributes NO row at all (aggregate
    /// posture — NOT a masked/zeroed row), so its existence, title, and task counts never leak.
    pub fn list_meeting_action_summaries(
        &self,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<MeetingActionSummary>> {
        let mut out: Vec<MeetingActionSummary> = Vec::new();
        // GATE 1: only VISIBLE meetings. GATE 2: only the VISIBLE note (None for sealed-not-unlocked).
        for m in self.list_meetings_visible(1000, unlocked)? {
            let Some(note) = self.get_note_if_visible(&m.id, unlocked)? else {
                continue; // sealed-and-not-unlocked → no row (aggregate posture).
            };
            let mut open_count = 0i64;
            let mut done_count = 0i64;
            for item in crate::summarize::action_items::parse_action_items(&note.markdown) {
                if item.done {
                    done_count += 1;
                } else {
                    open_count += 1;
                }
            }
            out.push(MeetingActionSummary {
                meeting_id: m.id,
                open_count,
                done_count,
            });
        }
        Ok(out)
    }

    // ── settings k/v table ───────────────────────────────────────────────────
    // `get_setting` / `set_setting` / `all_settings` moved to `storage::settings_store` (God-file
    // split) — still callable as inherent `db.method()` cross-file. The `settings` schema stays
    // inline in `Db::migrate()` above (created there with its seeded default rows).

    // ── analytics ──────────────────────────────────────────────────────────────

    /// Aggregate stats for the dashboard + Analytics tab. VISIBLE-content only: mirrors the
    /// `list_meetings_visible`/`brain_counts` predicate: canonical ownership governs first; a
    /// canonical-NULL legacy row is visible only when truly unfiled or every provider agrees on one
    /// existing open/session-unlocked folder. Sealed, dangling and ambiguous ownership therefore
    /// contributes nothing to totals/durations/status-breakdown/per-day activity.
    pub fn analytics(&self, unlocked: &HashSet<String>) -> Result<Analytics> {
        let conn = self.lock();
        let visible_meeting = meeting_visibility_clause("m", unlocked);
        // One canonical/fail-closed meeting visibility oracle is reused by every aggregate below.

        let total_meetings: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM meetings m WHERE {visible_meeting}"),
                [],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        let total_duration_s: i64 = conn
            .query_row(
                &format!(
                    "SELECT COALESCE(SUM(m.duration_s), 0) FROM meetings m WHERE {visible_meeting}"
                ),
                [],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        let longest_duration_s: i64 = conn
            .query_row(
                &format!(
                    "SELECT COALESCE(MAX(m.duration_s), 0) FROM meetings m WHERE {visible_meeting}"
                ),
                [],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        // Provider-note count follows the meeting's canonical/fail-closed visibility, not the
        // potentially stale per-provider `notes.folder_id`. This prevents a canonical locked owner
        // with an open legacy note row — or an ambiguous legacy provider split — from leaking a
        // count while the meeting itself is hidden.
        let notes_count: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM notes n
                       JOIN meetings m ON m.id=n.meeting_id
                      WHERE {visible_meeting}"
                ),
                [],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        let first_meeting_at: Option<String> = conn
            .query_row(
                &format!("SELECT MIN(m.started_at) FROM meetings m WHERE {visible_meeting}"),
                [],
                |r| r.get(0),
            )
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
                &format!(
                    "SELECT COUNT(*) FROM meetings m
                      WHERE m.started_at >= ?1 AND {visible_meeting}"
                ),
                rusqlite::params![cutoff_7d],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        let duration_7d_s: i64 = conn
            .query_row(
                &format!(
                    "SELECT COALESCE(SUM(m.duration_s), 0) FROM meetings m
                      WHERE m.started_at >= ?1 AND {visible_meeting}"
                ),
                rusqlite::params![cutoff_7d],
                |r| r.get(0),
            )
            .map_err(map_err)?;

        let by_status = {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT m.status, COUNT(*) FROM meetings m
                      WHERE {visible_meeting} GROUP BY m.status"
                ))
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
                .prepare(&format!(
                    "SELECT substr(m.started_at, 1, 10) AS d, COUNT(*), COALESCE(SUM(m.duration_s), 0)
                       FROM meetings m WHERE m.started_at >= ?1 AND {visible_meeting}
                       GROUP BY d ORDER BY d"
                ))
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

    // `insert_folder` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `list_folders` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `folder_kinds` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `folder_by_id` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `folder_by_path` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_folder_locked` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `folder_wrapped_key` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `any_locked_folder` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `discard_folder_seal` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `child_folders` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `rename_folder` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

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
        // A recording can be filed before its first provider note exists. The command layer must
        // rehome those canonical meeting owners before deleting the container, just as it already
        // rehomes note-backed meetings. Refuse here as the final loss/leak boundary: deleting the
        // folder row would otherwise leave a dangling `meetings.folder_id` that fails every read
        // gate and makes the recording disappear from the workspace tree.
        let meetings_remaining: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM meetings m
                  WHERE m.folder_id = ?1
                     OR (m.folder_id IS NULL AND EXISTS(
                          SELECT 1 FROM notes n
                           WHERE n.meeting_id=m.id AND n.folder_id=?1
                        ))",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        if meetings_remaining > 0 {
            return Err(AppError::Storage(format!(
                "refusing to delete folder {id}: {meetings_remaining} meeting(s) still assigned \
                 — reparent them first"
            )));
        }
        // NOTES-1 (2026-07-11 audit, CRITICAL data loss): AUTHORED notes (`documents(kind='note')`)
        // must have been REPARENTED to the default note-folder by the command layer BEFORE we get
        // here — the FE promises "delete folder" MOVES its notes, never destroys them. If any authored
        // note STILL references this folder, REFUSE (never blanket-DELETE an authored note). The
        // pre-fix `DELETE FROM documents WHERE folder_id` permanently destroyed every authored note in
        // the folder. Uploaded/ingested documents (`kind != 'note'`) — which are DERIVED brain sources,
        // not user-authored primary content — are still cleaned up here as before.
        let authored_remaining: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM documents WHERE folder_id = ?1 AND kind = 'note'",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        if authored_remaining > 0 {
            return Err(AppError::Storage(format!(
                "refusing to delete folder {id}: {authored_remaining} authored note(s) still assigned \
                 — reparent them first (never destroy authored notes on folder delete)"
            )));
        }
        // Only DERIVED (uploaded/ingested) documents remain — purge their chunks + rows.
        let document_ids: Vec<String> = {
            let mut stmt = tx
                .prepare("SELECT id FROM documents WHERE folder_id = ?1 AND kind != 'note'")
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
        // NIT-3 (link lifecycle): the derived documents about to be deleted may be an endpoint of a
        // `links` row (e.g. a semantic edge to another doc, or a wikilink pointing AT one of them).
        // Purge every edge incident on a to-be-deleted document id in the SAME tx so no link row is
        // left dangling to a row that no longer exists. Only the deleted derived-document ids need
        // purging: authored notes + meetings are REPARENTED out of the folder (above / by the command
        // layer), never deleted here, so their edges stay valid. Same choke-point as the seal purge. A
        // permanent DELETE keeps NO decision row (`preserve_decisions=false`).
        Self::purge_links_tx(&tx, &[], &document_ids, false)?;
        // Explicit (rather than FK-cascade) so the delete is deterministic and trigger-visible. Scoped
        // to non-authored documents — authored notes were reparented out above (and refused if not).
        tx.execute(
            "DELETE FROM documents WHERE folder_id = ?1 AND kind != 'note'",
            rusqlite::params![id],
        )
        .map_err(map_err)?;
        // BOARDS: demote to unfiled in the SAME transaction, exactly as authored notes are
        // reparented above. A board left pointing at a deleted folder is invisible in the tree
        // (its `LEFT JOIN folders` yields no row, so neither the unfiled branch nor the visible
        // branch holds) while still appearing in the flat list — a row the user can open but
        // cannot find, outside any future lock.
        //
        // Refuse first if a board here still holds CIPHERTEXT. The command layer removes the lock
        // before deleting, so by this point boards should be plaintext; a blob surviving means the
        // unseal did not run, and demoting would strand a board whose content key is about to be
        // discarded with the folder. Losing content is worse than refusing a delete.
        let sealed_boards: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM dashboards d
                  WHERE d.folder_id = ?1
                    AND (d.title_blob IS NOT NULL
                         OR EXISTS(SELECT 1 FROM dashboard_tiles t
                                    WHERE t.dashboard_id = d.id AND t.config_blob IS NOT NULL))",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        if sealed_boards > 0 {
            return Err(AppError::Storage(format!(
                "refusing to delete folder {id}: {sealed_boards} board(s) still hold sealed \
                 content — remove the lock first (never strand a sealed board)"
            )));
        }
        tx.execute(
            "UPDATE dashboards SET folder_id = NULL WHERE folder_id = ?1",
            rusqlite::params![id],
        )
        .map_err(map_err)?;
        // TASKS: same demotion, same reason — a task pointing at a deleted container is a row the
        // tree cannot show and the user cannot find. Unfiled, it is back in the Tasks view.
        tx.execute(
            "UPDATE org_tasks SET container_id = NULL WHERE container_id = ?1",
            rusqlite::params![id],
        )
        .map_err(map_err)?;
        // The folder AND its descendants, resolved BEFORE the delete — afterwards the tree is
        // unwalkable. Descendants count because their content goes away with the parent.
        let ask_scope = Self::ask_scope_for_folder_tree_tx(&tx, id)?;
        let n = tx
            .execute("DELETE FROM folders WHERE id = ?1", rusqlite::params![id])
            .map_err(map_err)?;
        Self::purge_ask_conversations_for_scope_tx(&tx, Some(&ask_scope))?;
        tx.commit().map_err(map_err)?;
        Ok(n)
    }

    /// Count of notes assigned to each folder id (only folders with ≥1 VISIBLE note appear).
    ///
    /// Gated the same way as `analytics`'s `notes_count` / `list_entities_visible`: a folder that
    /// is sealed and NOT session-unlocked contributes ZERO to its own count, never the true
    /// sealed count. `seal_note` blanks a note's markdown/content_blob on lock but never deletes
    /// or reparents the `notes` row, so an ungated `COUNT(*) GROUP BY folder_id` (the pre-fix
    /// query) leaked the exact sealed-note count into the folder tree (`FolderNode.note_count`)
    /// even though the lock model's invariant is that a sealed-and-not-unlocked folder leaks
    /// NOTHING — see `.claude/rules/lock-model.md`.
    pub fn count_notes_per_folder(
        &self,
        unlocked: &HashSet<String>,
    ) -> Result<std::collections::HashMap<String, usize>> {
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let sql = format!(
            "SELECT COALESCE(m.folder_id, n.folder_id) AS governing_folder_id, COUNT(*)
               FROM notes n
               JOIN meetings m ON m.id = n.meeting_id
              WHERE COALESCE(m.folder_id, n.folder_id) IS NOT NULL
                AND {meeting_visible}
              GROUP BY governing_folder_id"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
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

    /// Whether `folder_id` participates in a meeting ownership shape that cannot be sealed or
    /// unsealed under one content key.
    ///
    /// Valid shapes are deliberately narrow: either the meeting is canonically owned by this
    /// folder and every provider row agrees, or it is a legacy canonical-NULL meeting whose every
    /// provider row points at this one folder. Any NULL/mismatched sibling provider or a canonical
    /// owner elsewhere is ambiguous. Lock lifecycle callers refuse this before the first mutation,
    /// preventing one meeting's provider rows/extras from being encrypted under mixed folder keys.
    pub fn folder_has_ambiguous_meeting_governance(&self, folder_id: &str) -> Result<bool> {
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM meetings m
                WHERE (m.folder_id=?1 OR EXISTS(
                         SELECT 1 FROM notes nt
                          WHERE nt.meeting_id=m.id AND nt.folder_id=?1
                      ))
                  AND (
                    (m.folder_id IS NOT NULL AND m.folder_id IS NOT ?1)
                    OR EXISTS(
                         SELECT 1 FROM notes ns
                          WHERE ns.meeting_id=m.id AND ns.folder_id IS NOT ?1
                    )
                  )
             )",
            rusqlite::params![folder_id],
            |row| row.get(0),
        )
        .map_err(map_err)
    }

    // `set_meeting_folder` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_note_folder` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `notes_in_folder` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `sealable_notes_for_meeting` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `seal_note` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `restore_note_markdown` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `clear_note_content_blob` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `blank_sealed_notes_in_folders` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `reblank_locked_folders_at_rest` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `locked_folder_ids` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // ── transcript + timeline sealing (Phase 0.5 full per-folder lock) ─────────
    //
    // Segments + timelines live IN the SQLCipher DB (encrypted at rest already), but were NOT
    // gated in-app: a meeting in a locked-and-not-unlocked folder still returned its transcript +
    // timeline. These helpers add the SAME defense-in-depth the note markdown already has — an
    // AES-GCM blob under the folder CK in an OPEN db, with the plaintext column blanked while
    // sealed, reversed on session-unlock / re-blanked on relock / permanently restored on
    // remove-lock. All keyed off the meeting's canonical folder, with conservative legacy fallback.

    // `meeting_ids_in_folder` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `folder_for_meeting` moved to `storage::folders_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    /// Meetings whose audio may be auto-pruned: every meeting NOT in a locked canonical/legacy
    /// folder, OLDEST FIRST. A locked folder's audio
    /// is exempt — it is the sealed `.enc` at rest and must never be deleted by prune.
    pub fn prunable_audio_candidates(&self) -> Result<Vec<PrunableAudio>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT m.id, m.started_at, m.audio_path, m.mic_master_path, m.sys_master_path
                   FROM meetings m
                  WHERE (
                    (m.folder_id IS NOT NULL AND EXISTS (
                       SELECT 1 FROM folders f
                        WHERE f.id=m.folder_id AND f.locked=0
                    )) OR (
                      m.folder_id IS NULL AND (
                        NOT EXISTS (
                          SELECT 1 FROM notes n
                           WHERE n.meeting_id=m.id AND n.folder_id IS NOT NULL
                        ) OR (
                          NOT EXISTS (
                            SELECT 1 FROM notes n
                             WHERE n.meeting_id=m.id AND n.folder_id IS NULL
                          )
                          AND 1 = (
                            SELECT COUNT(DISTINCT n.folder_id) FROM notes n
                             WHERE n.meeting_id=m.id AND n.folder_id IS NOT NULL
                          )
                          AND EXISTS (
                            SELECT 1 FROM notes n
                            JOIN folders f ON f.id=n.folder_id
                             WHERE n.meeting_id=m.id AND f.locked=0
                          )
                        )
                      )
                    )
                  )
                  ORDER BY m.started_at ASC, m.id ASC",
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

    // `raw_segments` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `seal_segment` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `restore_segment_text` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `clear_segment_blobs` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `raw_timeline` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `seal_timeline` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `restore_timeline_data` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `clear_timeline_blob` moved to `storage::seal_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_meeting_audio_path` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `get_meeting_master_paths` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_meeting_mic_master_path` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `set_meeting_sys_master_path` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `clear_meeting_audio_path_if` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `clear_meeting_mic_master_path_if` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `clear_meeting_sys_master_path_if` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

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

    /// Visibility-gated transcript FTS hits that retain their stored segment id.
    ///
    /// Unlike `search_visible_impl`, this does not collapse a meeting to one snippet. It first
    /// applies the persisted channel predicate before selecting at most `max_meetings` recent
    /// visible meetings with a transcript match, then returns at most `max_segments` matching
    /// segment ids inside those meetings. Independent `max_meetings + 1` and `max_segments + 1`
    /// sentinels report meeting-set and raw-row truncation. The tool layer joins those ids against
    /// the canonical channel projection and must disclose either bound rather than presenting a
    /// bounded post-projection count as the corpus-wide total.
    pub(crate) fn search_transcript_segments_visible(
        &self,
        query: &str,
        meeting_id: Option<&str>,
        channel: crate::audio::merge::RenderChannel,
        max_meetings: i64,
        max_segments: usize,
        unlocked: &HashSet<String>,
    ) -> Result<(Vec<TranscriptSegmentHit>, bool, bool)> {
        let Some(and_expr) = fts_match_query(query.trim()) else {
            return Ok((Vec::new(), false, false));
        };
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let channel_filter = match channel {
            crate::audio::merge::RenderChannel::Merged => "AND s.echo_suppressed = 0",
            crate::audio::merge::RenderChannel::Mic => "AND s.speaker = 'me'",
            crate::audio::merge::RenderChannel::System => {
                crate::audio::merge::SYSTEM_SPEAKER_SQL_PREDICATE
            }
        };
        let scope = if meeting_id.is_some() {
            "AND m.id = :meeting_id"
        } else {
            ""
        };
        let sql = format!(
            "WITH segment_hits(rid, meeting_id, idx) AS (
                 SELECT s.rowid, s.meeting_id, s.idx
                   FROM fts_segments
                   JOIN segments s ON s.rowid = fts_segments.rowid
                  WHERE fts_segments MATCH :query
                    AND s.text <> ''
                    {channel_filter}
             ),
             candidate_visible_hit_meetings(id, started_at) AS (
                 SELECT m.id, m.started_at
                   FROM segment_hits h
                   JOIN meetings m ON m.id = h.meeting_id
                  WHERE {meeting_visible}
                    {scope}
                  GROUP BY m.id, m.started_at
                  ORDER BY m.started_at DESC, m.id DESC
                  LIMIT :meeting_probe_limit
             ),
             visible_hit_meetings(id, started_at) AS (
                 SELECT id, started_at
                   FROM candidate_visible_hit_meetings
                  ORDER BY started_at DESC, id DESC
                  LIMIT :max_meetings
             )
             SELECT s.meeting_id,
                    COALESCE(m.title, '(untitled)'),
                    s.idx,
                    (
                      SELECT COUNT(*) > :max_meetings
                        FROM candidate_visible_hit_meetings
                    )
               FROM segment_hits h
               JOIN visible_hit_meetings vm ON vm.id = h.meeting_id
               JOIN segments s ON s.rowid = h.rid
               JOIN meetings m ON m.id = s.meeting_id
              ORDER BY vm.started_at DESC, s.meeting_id ASC, s.idx ASC
              LIMIT :row_probe_limit"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok((
                TranscriptSegmentHit {
                    meeting_id: row.get(0)?,
                    meeting_title: row.get(1)?,
                    seg_idx: row.get(2)?,
                },
                row.get::<_, i64>(3)? != 0,
            ))
        };
        let max_meetings = max_meetings.clamp(1, 20);
        let meeting_probe_limit = max_meetings.saturating_add(1);
        let max_segments = max_segments.clamp(1, 5_000);
        let row_probe_limit = i64::try_from(max_segments.saturating_add(1)).unwrap_or(5_001);
        let rows = match meeting_id {
            Some(mid) => stmt
                .query_map(
                    rusqlite::named_params! {
                        ":query": and_expr,
                        ":meeting_id": mid,
                        ":meeting_probe_limit": meeting_probe_limit,
                        ":max_meetings": max_meetings,
                        ":row_probe_limit": row_probe_limit,
                    },
                    map_row,
                )
                .map_err(map_err)?
                .collect::<std::result::Result<Vec<_>, _>>(),
            None => stmt
                .query_map(
                    rusqlite::named_params! {
                        ":query": and_expr,
                        ":meeting_probe_limit": meeting_probe_limit,
                        ":max_meetings": max_meetings,
                        ":row_probe_limit": row_probe_limit,
                    },
                    map_row,
                )
                .map_err(map_err)?
                .collect::<std::result::Result<Vec<_>, _>>(),
        };
        let mut rows = rows.map_err(map_err)?;
        let meeting_truncated = rows
            .first()
            .map(|(_, meeting_truncated)| *meeting_truncated)
            .unwrap_or(false);
        let raw_rows_truncated = rows.len() > max_segments;
        rows.truncate(max_segments);
        Ok((
            rows.into_iter().map(|(hit, _)| hit).collect(),
            meeting_truncated,
            raw_rows_truncated,
        ))
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
        let Some(and_expr) = fts_match_query(q) else {
            return Ok(Vec::new());
        };
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
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
                    m.folder_id
               FROM ranked r
               JOIN meetings m ON m.id = r.meeting_id
              WHERE {meeting_visible}{date}
              ORDER BY r.rank ASC, m.started_at DESC, m.id DESC
              LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        // Run the (already-gated) FTS body with a given match expression; only WHICH visible rows
        // match changes when we swap AND→OR.
        let run = |stmt: &mut rusqlite::Statement, expr: &str| -> Result<Vec<Meeting>> {
            let rows = stmt
                .query_map(rusqlite::params![expr, limit], row_to_meeting)
                .map_err(map_err)?;
            let mut meetings = Vec::new();
            for r in rows {
                meetings.push(r.map_err(map_err)??);
            }
            Ok(meetings)
        };
        // S2 AND→OR fallback: implicit-AND matched nothing ⇒ retry with the content-word OR twin
        // (stopwords/<3-char dropped). Fires ONLY on an empty AND result, so it never widens a
        // successful query and stays exact-word lexical (the crisp "no match" is preserved).
        let mut meetings = run(&mut stmt, &and_expr)?;
        if meetings.is_empty() {
            if let Some(any_expr) = fts_match_query_any(q) {
                if any_expr != and_expr {
                    meetings = run(&mut stmt, &any_expr)?;
                }
            }
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

    // `list_meetings_visible` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `meeting_by_title_visible` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `meeting_by_title_folded_visible` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    /// Live keystroke-prefix title match over VISIBLE notes + meetings, for the inline `[[` /
    /// slash-menu link-insertion autocomplete (distinct from [`resolve_wikilink`]'s exact-title
    /// resolve and from `gather_note_enhance_citations`'s SELECTION+semantic retrieval — this is a
    /// lightweight `LIKE prefix%` title scan, the right shape for filtering-as-you-type). GATED
    /// identically to every other title/content read: `visibility_clause` on both legs — on the
    /// page queries AND their COUNT twins — so a sealed-and-not-session-unlocked note/meeting
    /// neither appears as a candidate nor inflates the pagination totals. An empty/blank `prefix`
    /// returns the most-recently-updated visible notes+meetings (so the popover has something to
    /// show the instant it opens, before the user has typed anything).
    ///
    /// PAGINATED (2026-07-17 — the picker scrolls the whole vault now, not a fixed top-8):
    /// returns the `limit`-sized page starting at `offset` of ONE stable combined ordering,
    /// [all matching notes, newest-updated first] ++ [all matching meetings, newest-started
    /// first] (notes first mirrors `resolve_wikilink`'s note-first preference), plus the TOTAL
    /// number of matching local rows across both legs — `commands::list_link_candidates` needs
    /// that total to know how far into the org (Shared Brain) leg it folds in after the local
    /// rows that earlier pages have already consumed. Both legs run under ONE connection lock so
    /// the counts and the page rows are a single consistent snapshot.
    pub fn list_link_candidates_visible(
        &self,
        prefix: &str,
        limit: i64,
        offset: i64,
        unlocked: &HashSet<String>,
    ) -> Result<(Vec<NoteCitation>, i64)> {
        let prefix = prefix.trim();
        let limit = limit.max(0);
        let offset = offset.max(0);
        // `None` ⇒ the `:pat IS NULL` arm short-circuits the LIKE, so ONE SQL string serves
        // both the empty-prefix (recency browse) and the typed-prefix (filter) shapes.
        let pat: Option<String> = if prefix.is_empty() {
            None
        } else {
            Some(format!("{}%", escape_like(prefix)))
        };
        let mut out: Vec<NoteCitation> = Vec::new();
        let conn = self.lock();

        // Notes leg — the COUNT twin shares the page query's exact WHERE body, so the
        // offset arithmetic below can never drift from what the page query returns.
        let visible_notes = visibility_clause("f", unlocked);
        // Exclude the never-named "Untitled" sentinel (`UNTITLED_TITLE`): a screen of identical
        // "Untitled" rows is useless and an unnamed note is not a meaningful link target (2026-07-20).
        // On `notes_where` so the COUNT twin and the page query stay in lockstep (offset arithmetic).
        let notes_where = format!(
            "d.kind = 'note' AND {visible_notes}
             AND LOWER(COALESCE(NULLIF(TRIM(d.title), ''), d.name)) != LOWER(:untitled)
             AND (:pat IS NULL OR COALESCE(NULLIF(TRIM(d.title), ''), d.name) LIKE :pat ESCAPE '\\')"
        );
        let note_count: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*)
                       FROM documents d
                       JOIN folders f ON f.id = d.folder_id
                      WHERE {notes_where}"
                ),
                rusqlite::named_params! { ":pat": pat, ":untitled": UNTITLED_TITLE },
                |r| r.get(0),
            )
            .map_err(map_err)?;
        if offset < note_count && (out.len() as i64) < limit {
            let sql = format!(
                "SELECT d.id, COALESCE(NULLIF(TRIM(d.title), ''), d.name)
                   FROM documents d
                   JOIN folders f ON f.id = d.folder_id
                  WHERE {notes_where}
                  ORDER BY d.updated_at DESC, d.id ASC
                  LIMIT :limit OFFSET :offset"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_err)?;
            let rows = stmt
                .query_map(
                    rusqlite::named_params! {
                        ":pat": pat,
                        ":limit": limit,
                        ":offset": offset,
                        ":untitled": UNTITLED_TITLE,
                    },
                    |r: &Row<'_>| -> rusqlite::Result<(String, String)> {
                        Ok((r.get(0)?, r.get(1)?))
                    },
                )
                .map_err(map_err)?;
            for r in rows {
                let (id, title) = r.map_err(map_err)?;
                out.push(NoteCitation {
                    kind: "note".into(),
                    id,
                    title,
                    snippet: String::new(),
                });
            }
        }

        // Meetings leg — starts where the notes leg ends in the combined ordering: pages
        // that fell entirely inside the notes leg read meetings from offset 0; pages past
        // it skip exactly the meeting rows earlier pages consumed.
        let visible_meetings = meeting_visibility_clause("m", unlocked);
        let meetings_where = format!(
            "m.title IS NOT NULL AND TRIM(m.title) != ''
             AND (:pat IS NULL OR m.title LIKE :pat ESCAPE '\\')
             AND {visible_meetings}"
        );
        let meeting_count: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM meetings m WHERE {meetings_where}"),
                rusqlite::named_params! { ":pat": pat },
                |r| r.get(0),
            )
            .map_err(map_err)?;
        let remaining = limit - out.len() as i64;
        let meeting_offset = (offset - note_count).max(0);
        if remaining > 0 && meeting_offset < meeting_count {
            let sql = format!(
                "SELECT m.id, m.title
                   FROM meetings m
                  WHERE {meetings_where}
                  ORDER BY m.started_at DESC, m.id DESC
                  LIMIT :limit OFFSET :offset"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_err)?;
            let rows = stmt
                .query_map(
                    rusqlite::named_params! {
                        ":pat": pat,
                        ":limit": remaining,
                        ":offset": meeting_offset,
                    },
                    |r: &Row<'_>| -> rusqlite::Result<(String, Option<String>)> {
                        Ok((r.get(0)?, r.get(1)?))
                    },
                )
                .map_err(map_err)?;
            for r in rows {
                let (id, title) = r.map_err(map_err)?;
                out.push(NoteCitation {
                    kind: "meeting".into(),
                    id,
                    title: title.unwrap_or_else(|| "Meeting".into()),
                    snippet: String::new(),
                });
            }
        }

        // Documents leg — LAST in the combined ordering (so existing notes/meetings page
        // positions are untouched; a doc-free vault reports document_count == 0). Mirrors the
        // notes leg exactly but for `d.kind = 'document'`, and titles on the SAME
        // COALESCE(title, name) so a filename (e.g. `Oskar_Orlowski_CV.pdf`, no `title`) is
        // searchable/displayable. The COUNT twin shares the page query's WHERE body, so a
        // sealed-and-not-unlocked document neither appears NOR inflates the total.
        let visible_docs = visibility_clause("f", unlocked);
        let docs_where = format!(
            "d.kind = 'document' AND {visible_docs}
             AND (:pat IS NULL OR COALESCE(NULLIF(TRIM(d.title), ''), d.name) LIKE :pat ESCAPE '\\')"
        );
        let document_count: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*)
                       FROM documents d
                       JOIN folders f ON f.id = d.folder_id
                      WHERE {docs_where}"
                ),
                rusqlite::named_params! { ":pat": pat },
                |r| r.get(0),
            )
            .map_err(map_err)?;
        let remaining = limit - out.len() as i64;
        let doc_offset = (offset - note_count - meeting_count).max(0);
        if remaining > 0 && doc_offset < document_count {
            let sql = format!(
                "SELECT d.id, COALESCE(NULLIF(TRIM(d.title), ''), d.name)
                   FROM documents d
                   JOIN folders f ON f.id = d.folder_id
                  WHERE {docs_where}
                  ORDER BY COALESCE(d.updated_at, d.created_at) DESC, d.id ASC
                  LIMIT :limit OFFSET :offset"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_err)?;
            let rows = stmt
                .query_map(
                    rusqlite::named_params! {
                        ":pat": pat,
                        ":limit": remaining,
                        ":offset": doc_offset,
                    },
                    |r: &Row<'_>| -> rusqlite::Result<(String, String)> {
                        Ok((r.get(0)?, r.get(1)?))
                    },
                )
                .map_err(map_err)?;
            for r in rows {
                let (id, title) = r.map_err(map_err)?;
                out.push(NoteCitation {
                    kind: "document".into(),
                    id,
                    title,
                    snippet: String::new(),
                });
            }
        }
        Ok((out, note_count + meeting_count + document_count))
    }

    // `get_note_if_visible` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // `meeting_is_visible` moved to `storage::meetings_store` (God-file split) — still callable as inherent `db.method()` cross-file.

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

    /// LOCK-SAFETY: delete every `assistant_interactions` row for `meeting_ids` within an EXISTING
    /// transaction, so the purge lands in the SAME atomic unit as the plaintext blanking on a seal
    /// (and on the startup reconcile). The Q&A log is plaintext-derived convenience data that mirrors
    /// content of a sealed meeting (the user's spoken question + the answer grounded on the vault); a
    /// sealed meeting must surface NOTHING, so — exactly like `correction_log` / `note_chunks` — we
    /// DELETE rather than seal. This is INTENTIONAL: the Q&A log is dropped on seal by design and is
    /// not recoverable (it was never keyed); the underlying transcript is still sealed + restorable.
    pub(crate) fn purge_assistant_interactions_tx(
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
    /// DELETE rather than key-seal. Dropped by design; the rows are RECOVERABLE from `sealed_fact_ledgers`, which the seal writes before this purge runs; the
    /// underlying transcript is still sealed + restorable.
    pub(crate) fn purge_live_bullets_tx(
        tx: &rusqlite::Transaction<'_>,
        meeting_ids: &[String],
    ) -> Result<()> {
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
        // Normalize BOTH sides of the comparison. Doing it at READ time (not only at ingest) is what
        // repairs an EXISTING vault: every note already on disk carries the raw
        // `Miles (others-9)` / `others-10 -> Miles` forms, and re-summarizing them is not an option.
        let owner_lc = owner
            .map(|o| crate::summarize::action_items::normalize_owner(o).to_lowercase())
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
                        Some(o)
                            if crate::summarize::action_items::normalize_owner(o)
                                .to_lowercase()
                                == want => {}
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
    ///
    /// `open_commitment_count` MUST use the exact same predicate as the person-dossier's "who owes
    /// what" section (`summarize::dossier::build_dossier_data`) — an open item belongs to this
    /// person iff its owner name-matches this person (case-insensitive) — otherwise the card badge
    /// and the dossier opened from that same card can disagree. B4 fix (2026-07-25): the predicate
    /// was narrowed from "VISIBLE mentioning meeting OR owner-name match" to owner-only, so a
    /// co-participant's commitment sharing a mentioning meeting is no longer falsely attributed to
    /// this person (badge == dossier count still holds — both are now owner-only).
    ///
    /// Returns [`PeopleList`], not a bare `Vec`, because the candidate set is itself capped
    /// UPSTREAM by `list_entities_visible`'s `MAX_VISIBLE_ENTITIES` (500, ordered by mention
    /// count across ALL kinds) — on a vault with >500 visible entities, some visible Persons can
    /// be trimmed before the `EntityKind::Person` filter even runs, with zero signal on the old
    /// bare-`Vec` return (added 2026-07-13: `total_visible_people` is the TRUE Person count so the
    /// FE's "Show all N people" expander can disclose the cap instead of presenting the trimmed
    /// roster as complete).
    pub fn list_people(&self, unlocked: &HashSet<String>) -> Result<PeopleList> {
        // GATE: the visible-only entity set, Persons only. A sealed-only person is absent here.
        let people: Vec<GraphNode> = self
            .list_entities_visible(unlocked)?
            .into_iter()
            .filter(|n| n.kind == EntityKind::Person)
            .collect();
        // The full VISIBLE open-commitment rollup, computed ONCE and reused per person below —
        // mirrors `build_dossier_data`'s "who owes what" (owner-name match only).
        let all_commitments = self.list_open_commitments(unlocked, None)?;
        let mut out: Vec<PersonCard> = Vec::with_capacity(people.len());
        for p in people {
            // meeting_count + last_talked: VISIBLE mentions only, newest first.
            let mentions = self.entity_mentions_visible(&p.id, unlocked)?;
            let meeting_count = mentions.len() as i64;
            let last_talked = mentions.first().map(|m| m.started_at.clone());
            // current_fact_count: currently-valid facts about this person from VISIBLE meetings.
            let current_fact_count = self.list_facts_visible(&p.id, unlocked)?.len() as i64;
            // open_commitment_count: same predicate as the dossier's "who owes what" — an open item
            // OWNED BY this person (name match, case-insensitive). B4 fix (2026-07-25): owner-only,
            // so a co-participant's commitment from a shared mentioning meeting is not attributed here.
            let name_lc = p.name.trim().to_lowercase();
            let open_commitment_count = all_commitments
                .iter()
                .filter(|c| {
                    c.owner
                        .as_deref()
                        .map(|o| o.trim().to_lowercase() == name_lc)
                        .unwrap_or(false)
                })
                .count() as i64;
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
        let total_visible_people =
            self.count_entities_visible(unlocked, Some(EntityKind::Person))?;
        Ok(PeopleList {
            people: out,
            total_visible_people,
        })
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
    // The `brief_schedules` / `brief_runs` CRUD (`list_brief_schedules` / `insert_brief_schedule` /
    // `update_brief_schedule` / `delete_brief_schedule` / `set_brief_schedule_last_run` /
    // `insert_brief_run` / `list_pending_brief_runs` / `get_brief_run` / `accept_brief_run` /
    // `delete_brief_run`) + `row_to_brief_schedule` / `row_to_brief_run` moved to
    // `storage::brief_store` (God-file split) — still callable as inherent `db.method()` cross-file.

    // ── Brain v2 L5 — MCP server config rows ────────────────────────────────────────────────────
    // The `mcp_servers` table CRUD (`list_mcp_servers` / `get_mcp_server` / `insert_mcp_server` /
    // `delete_mcp_server` / `set_mcp_server_consented`) + `row_to_mcp_server` moved to
    // `storage::mcp_store` (God-file split) — still callable as inherent `db.method()` cross-file.

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
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let sql = format!(
            "SELECT vp.id, vp.meeting_id, vp.cluster_index, vp.label, vp.dim, vp.embedding, \
                    vp.created_at \
               FROM speaker_voiceprints vp \
               JOIN meetings m ON m.id = vp.meeting_id \
              WHERE {meeting_visible} \
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

    /// Visibility-gated speaker names for one meeting, without loading biometric embeddings.
    ///
    /// A voiceprint label is personal content. This query applies the same meeting gate as the full
    /// voiceprint reader and returns only the latest non-empty label for each cluster. The MCP
    /// transcript renderer never needs, reads, or serializes the CAM++ embedding blob.
    pub fn list_visible_speaker_labels_for_meeting(
        &self,
        meeting_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<VisibleSpeakerLabel>> {
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let sql = format!(
            "SELECT vp.cluster_index, vp.label
               FROM speaker_voiceprints vp
               JOIN meetings m ON m.id = vp.meeting_id
              WHERE vp.meeting_id = ?1
                AND TRIM(COALESCE(vp.label, '')) <> ''
                AND NOT EXISTS (
                      SELECT 1
                        FROM speaker_voiceprints newer
                       WHERE newer.meeting_id = vp.meeting_id
                         AND newer.cluster_index = vp.cluster_index
                         AND TRIM(COALESCE(newer.label, '')) <> ''
                         AND (
                              newer.created_at > vp.created_at
                           OR (newer.created_at = vp.created_at AND newer.id > vp.id)
                         )
                    )
                AND {meeting_visible}
              ORDER BY vp.cluster_index ASC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![meeting_id], |row| {
                Ok(VisibleSpeakerLabel {
                    cluster_index: row.get(0)?,
                    label: row.get(1)?,
                })
            })
            .map_err(map_err)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_err)
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

    /// The DISTINCT entity ids mentioned by ONE meeting (the orphan pass's Jaccard substrate).
    /// Non-content metadata (opaque ids); the caller only ever passes ids already gated into its
    /// visible corpus. Crate-internal on purpose — not a general read surface.
    pub(crate) fn entity_ids_for_meeting(&self, meeting_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT entity_id FROM entity_mentions
                   WHERE meeting_id = ?1 ORDER BY entity_id",
            )
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

    // `note_is_visible` moved to `storage::notes_store` (God-file split) — still callable as inherent `db.method()` cross-file.
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
    static SWEEP: std::sync::Once = std::sync::Once::new();
    // Reclaim fixtures abandoned by EARLIER test processes before minting a new one. Callers own a
    // bare `PathBuf` at 73 sites across 36 files, so nothing here can drop-clean the CURRENT run —
    // sweeping on entry bounds the steady state at one run's worth instead of unbounded growth.
    SWEEP
        .call_once(|| sweep_stale_temp_fixtures(std::env::temp_dir().as_path(), STALE_FIXTURE_AGE));
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{pid}-{nanos}-{n}.{ext}"))
}

/// Only fixtures older than this are swept, so a CONCURRENT test process can never have its live
/// files deleted: an entry younger than the longest possible test run is always left alone.
#[cfg(test)]
pub(crate) const STALE_FIXTURE_AGE: std::time::Duration =
    std::time::Duration::from_secs(2 * 60 * 60);

/// Remove `murmur-*` / `meetnotes-*` fixtures under `dir` last modified more than `min_age` ago.
///
/// `unique_temp_path` leaked every file it ever handed out: 1,599 `.sqlite` plus 216 `-wal`,
/// 216 `-shm` and assorted `.docx`/`.md`/`.wav` fixtures were live in one `TMPDIR`, and the
/// harness's per-task private `TMPDIR` multiplied that into 67.8 GB of evidence-store scratch.
/// Both prefix families are swept, and both files and directories (callers pass `ext = "dir"`).
/// Every failure is ignored on purpose — a fixture another process is mid-write, a permission
/// error, or a racing sweep must never fail the test that merely wanted a temp path.
#[cfg(test)]
pub(crate) fn sweep_stale_temp_fixtures(dir: &std::path::Path, min_age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("murmur-") && !name.starts_with("meetnotes-") {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= min_age);
        if !stale {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
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
/// Regex matching an Obsidian-native `[[wikilink]]` opener, capturing the raw TARGET up to the first
/// `]`, `|` (alias), or `#` (heading anchor) — so `[[Title|alias]]` and `[[Title#heading]]` both
/// degrade to the bare `Title`. Lazy `OnceLock` per the repo's static-regex convention (mirrors
/// `summarize::redact::email_re`; NOT `LazyLock` — MSRV/clippy on ci.sh).
fn wikilink_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\[\[([^\]\|#]+)").unwrap())
}

/// Extract the DISTINCT `[[Title]]` wikilink targets from a note/markdown body, in first-seen order.
/// PURE (no DB). `[[Title|alias]]` and `[[Title#heading]]` yield the bare `Title`; each target is
/// trimmed; duplicates are dropped keeping the first occurrence. An empty/no-match body yields `[]`.
pub(crate) fn extract_wikilink_titles(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for cap in wikilink_re().captures_iter(text) {
        // `cap[1]` is guaranteed by the single capture group in the pattern.
        let title = cap[1].trim();
        if title.is_empty() {
            continue;
        }
        if !out.iter().any(|t| t == title) {
            out.push(title.to_string());
        }
    }
    out
}

/// The sentinel display title auto-assigned to a note the user never named. `create_note_inner`
/// writes the literal `"Untitled"` when the user supplies no title, and the auto-title guard in
/// `commands::notes` treats `== "Untitled"` as "not yet named". It is NOT a unique identity — a real
/// vault has MANY notes sharing it — so it must not be surfaced as a linkable identity: the link
/// candidate picker (`list_link_candidates_visible`) does not OFFER it, and the Vault Audit mention
/// scan (`find_unlinked_mention_line` / `orphan_pass`) never SUGGESTS `[[Untitled]]`. (The backlink
/// fan-out itself — an untitled note claiming mentions it never earned — is fixed separately by the
/// resolve-by-id guard in `backlinks_for_visible` (#417), which keeps `[[Untitled]]` resolving to one
/// note; this const governs ONLY the picker + audit surfaces #417 does not touch.) Kept as one const
/// so the write site and these read guards can never drift.
pub(crate) const UNTITLED_TITLE: &str = "Untitled";

/// True when `title` is the never-named sentinel (see [`UNTITLED_TITLE`]) — trimmed + ASCII-case
/// -folded. Keeps an unnamed note out of the link-candidate picker and the audit link suggestions.
pub(crate) fn is_untitled_title(title: &str) -> bool {
    title.trim().eq_ignore_ascii_case(UNTITLED_TITLE)
}

/// A comparable newest-first sort key (epoch-millis) for a backlink chip. Both legs emit an RFC3339
/// `timestamp` (meeting `started_at`, note `updated_at` rendered to RFC3339), so a single parse
/// suffices. Unparseable → `i64::MIN` (sorts oldest), keeping the order total + deterministic
/// without panicking.
pub(crate) fn backlink_sort_key(b: &BacklinkSource) -> i64 {
    chrono::DateTime::parse_from_rfc3339(&b.timestamp)
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(i64::MIN)
}

/// Brain v3 PR-3 — the raw `links` row shape read by [`Db::links_for_visible`] before it is resolved
/// into a [`crate::storage::models::LinkEdge`]: `(id, src_kind, src_id, dst_kind, dst_id, edge_type,
/// created_by, status, score, created_at)`. Aliased so the reader's row `Vec` stays under clippy's
/// type-complexity bar.
pub(crate) type LinkRowRaw = (
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    f64,
    i64,
);

/// Per-kind render cap for the full-brain graph (`build_full_graph`, DESIGN §PR-4). Meetings, notes,
/// and documents are each capped independently (entities keep their own 500 cap inside
/// `list_entities_visible`) so the payload stays bounded on a large vault; visibility is enforced in
/// WHERE, so the cap trims magnitude only — it can never widen what is visible. Mirrors the
/// `MAX_GRAPH_EDGES`/`MAX_VISIBLE_ENTITIES` posture, disclosed via `total_visible_nodes`. Consumed by
/// `storage::graph_store::build_full_graph`; kept here (with the graph tests in `mod tests`) so both
/// see it via one `pub(crate)` symbol.
pub(crate) const MAX_FULL_GRAPH_PER_KIND: usize = 500;

/// Bound for the `links`-derived edge leg of the full-brain graph (`full_graph_links`, PR-9 F2).
/// `links` is the fastest-growing edge table (every wikilink/companion/semantic-suggestion is a row)
/// and was previously read UNBOUNDED while the node legs were each capped — the payload the caps
/// exist for was unenforced on its highest-cardinality leg. Strongest-score edges survive the cut;
/// the trim is DISCLOSED via `FullGraphData::edges_truncated`. Comfortably above what a laid-out
/// ≤140-node scene can render, so it trims magnitude only on a very large vault.
pub(crate) const MAX_FULL_GRAPH_LINK_EDGES: usize = 4000;

pub(crate) fn visibility_clause(_alias: &str, unlocked: &HashSet<String>) -> String {
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

/// Complete visibility predicate for a `meetings` alias. Canonical `meetings.folder_id` governs a
/// recording even before any note exists; NULL rows retain the conservative legacy note-owned
/// rule so an ambiguous historical provider split is never assigned to an arbitrary folder.
pub(crate) fn meeting_visibility_clause(alias: &str, unlocked: &HashSet<String>) -> String {
    // `visibility_clause` is historically hard-bound to alias `f`; keep that alias local to each
    // subquery instead of trusting its ignored argument.
    let visible = visibility_clause("f", unlocked);
    format!(
        "(({alias}.folder_id IS NOT NULL AND EXISTS (
              SELECT 1 FROM folders f WHERE f.id={alias}.folder_id AND {visible}
           )) OR ({alias}.folder_id IS NULL AND (
              NOT EXISTS (
                SELECT 1 FROM notes mn
                 WHERE mn.meeting_id={alias}.id AND mn.folder_id IS NOT NULL
              )
              OR (
                NOT EXISTS (
                  SELECT 1 FROM notes mn
                   WHERE mn.meeting_id={alias}.id AND mn.folder_id IS NULL
                )
                AND 1 = (
                  SELECT COUNT(DISTINCT mn.folder_id) FROM notes mn
                   WHERE mn.meeting_id={alias}.id AND mn.folder_id IS NOT NULL
                )
                AND NOT EXISTS (
                  SELECT 1 FROM notes mn LEFT JOIN folders f ON f.id=mn.folder_id
                   WHERE mn.meeting_id={alias}.id AND mn.folder_id IS NOT NULL
                     AND (f.id IS NULL OR NOT {visible})
                )
              )
           )))"
    )
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
    let t = unquote(raw.trim())
        .trim_start_matches('#')
        .trim()
        .to_string();
    if !t.is_empty() && !tags.contains(&t) {
        tags.push(t);
    }
}

/// Feature C — COERCE a raw front-matter scalar into a typed [`PropertyValue`] against a schema
/// column's declared `kind`. PURE + unit-testable (no DB). NEVER drops a value: a raw string that
/// cannot be coerced to the declared kind (a malformed number/date/bool, or a `Select` value not in
/// `options`) is PRESERVED as [`PropertyValue::Text`] — the user's data survives even when it does
/// not match the schema.
///
/// Coercion rules:
/// - `Checkbox` — `true`/`1`/`yes` (case-insensitive) ⇒ `true`; `false`/`0`/`no` ⇒ `false`; else
///   preserved as `Text`.
/// - `Number` — a parseable `f64` ⇒ `Number`; else `Text`.
/// - `Date` — an ISO-ish `YYYY-MM-DD` (optionally with a time suffix) ⇒ `Date`; else `Text`.
/// - `Select` — the exact raw value when it is one of `options` (case-insensitive match, canonical
///   option casing kept) ⇒ `Select`; otherwise the raw value PRESERVED as `Text`.
/// - `Text` — the raw value verbatim.
pub fn coerce_property_value(raw: &str, kind: PropertyKind, options: &[String]) -> PropertyValue {
    let v = raw.trim();
    match kind {
        PropertyKind::Text => PropertyValue::Text(v.to_string()),
        PropertyKind::Checkbox => match v.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => PropertyValue::Checkbox(true),
            "false" | "0" | "no" => PropertyValue::Checkbox(false),
            _ => PropertyValue::Text(v.to_string()), // not a bool → preserve, never drop.
        },
        PropertyKind::Number => match v.parse::<f64>() {
            Ok(n) if n.is_finite() => PropertyValue::Number(n),
            _ => PropertyValue::Text(v.to_string()),
        },
        PropertyKind::Date => {
            if is_iso_ish_date(v) {
                PropertyValue::Date(v.to_string())
            } else {
                PropertyValue::Text(v.to_string())
            }
        }
        PropertyKind::Select => {
            // A Select value not in the declared options is PRESERVED as Text (never dropped).
            match options.iter().find(|o| o.eq_ignore_ascii_case(v)) {
                Some(canonical) => PropertyValue::Select(canonical.clone()),
                None => PropertyValue::Text(v.to_string()),
            }
        }
    }
}

/// Is `s` an ISO-ish date — `YYYY-MM-DD`, optionally followed by a `T`/space time (`YYYY-MM-DDThh…`)?
/// Deliberately LENIENT on the time part (any suffix after the date is accepted) but STRICT on the
/// `YYYY-MM-DD` head (4-2-2 digits, dash-separated, plausible month/day ranges) so a plain number or
/// free text never coerces to Date.
fn is_iso_ish_date(s: &str) -> bool {
    // Split off any time/zone suffix at the first 'T' or space; validate the date head only.
    let head = s.split_once(['T', ' ']).map(|(d, _)| d).unwrap_or(s);
    let parts: Vec<&str> = head.split('-').collect();
    if parts.len() != 3 {
        return false;
    }
    let (y, m, d) = (parts[0], parts[1], parts[2]);
    if y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return false;
    }
    if !y.chars().all(|c| c.is_ascii_digit())
        || !m.chars().all(|c| c.is_ascii_digit())
        || !d.chars().all(|c| c.is_ascii_digit())
    {
        return false;
    }
    // Plausible ranges (no calendar-exact day count — the goal is "looks like a date", not validity).
    let month: u8 = m.parse().unwrap_or(0);
    let day: u8 = d.parse().unwrap_or(0);
    (1..=12).contains(&month) && (1..=31).contains(&day)
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
pub(crate) const MIN_ENTITY_NAME_LEN: usize = 3;

/// Lowercase + tokenize on Unicode non-alphanumeric boundaries (Polish-safe via
/// `char::is_alphanumeric`). Empty tokens are dropped. Used by the deterministic QUERY→ENTITY
/// resolver so matching is on whole tokens, never arbitrary substrings.
pub(crate) fn tokenize_lower(s: &str) -> Vec<String> {
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
pub(crate) fn name_matches_query_tokens(query_tokens: &[String], name_ci: &str) -> bool {
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

pub(crate) fn row_to_meeting(row: &Row<'_>) -> rusqlite::Result<Result<Meeting>> {
    // Read every column as a rusqlite result first (so `?` here yields rusqlite::Error),
    // then fold the status-string parse (which yields AppError) into the inner Result.
    let id: String = row.get(0)?;
    let started_at: String = row.get(1)?;
    let ended_at: Option<String> = row.get(2)?;
    let title: Option<String> = row.get(3)?;
    let duration_s: i64 = row.get(4)?;
    let audio_path: Option<String> = row.get(5)?;
    let status_str: String = row.get(6)?;
    // Trailing column: the meeting's canonical folder (NULL = unfiled / conservative legacy).
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

// `row_to_audit_finding` moved to `storage::audit_store` (God-file split) alongside the
// `audit_findings` readers that are its only callers.

// `row_to_brief_schedule` / `row_to_brief_run` moved to `storage::brief_store` (God-file split)
// alongside the `brief_schedules` / `brief_runs` readers that are their only callers.

// `row_to_mcp_server` moved to `storage::mcp_store` (God-file split) alongside the `mcp_servers`
// readers that are its only callers.

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
pub(crate) fn fts_match_query(q: &str) -> Option<String> {
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

/// Canonical content terms under the exact FTS tokenizer used by every production content index.
///
/// Rust splitting/lowercasing cannot model `unicode61 remove_diacritics 2`: distinct strings such
/// as precomposed `résumé`, decomposed `re\u{301}sume\u{301}`, and `resume` are one FTS token and
/// must never receive multiple fallback coverage votes. A connection-local TEMP FTS table tokenizes
/// the ORIGINAL query, while `fts5vocab(..., 'instance')` exposes canonical tokens in occurrence
/// order. Tokens that are not Unicode-alphanumeric or are too short are discarded; the survivors
/// are stopword-filtered, first-seen deduplicated, and bounded before callers build MATCH SQL.
pub(crate) fn fts_unicode61_content_terms(
    conn: &Connection,
    q: &str,
    max_terms: usize,
) -> Result<Vec<String>> {
    if max_terms == 0 {
        return Ok(Vec::new());
    }
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS temp.murmur_query_tokenizer USING fts5(
             text,
             tokenize = 'unicode61 remove_diacritics 2'
         );
         CREATE VIRTUAL TABLE IF NOT EXISTS temp.murmur_query_vocab_instance USING fts5vocab(
             murmur_query_tokenizer,
             'instance'
         );",
    )
    .map_err(map_err)?;

    // Keep query text transaction-local. Any early error rolls the TEMP insert back on Drop; the
    // successful path explicitly rolls back too, so no prior query survives for a later read.
    let tx = conn.unchecked_transaction().map_err(map_err)?;
    let result = (|| -> Result<Vec<String>> {
        let mut canonical_terms = Vec::new();
        let mut seen = HashSet::new();
        tx.execute("DELETE FROM temp.murmur_query_tokenizer", [])
            .map_err(map_err)?;
        tx.execute(
            "INSERT INTO temp.murmur_query_tokenizer(text) VALUES (?1)",
            rusqlite::params![q],
        )
        .map_err(map_err)?;

        let mut stmt = tx
            .prepare(
                "SELECT term
                   FROM temp.murmur_query_vocab_instance
                  ORDER BY doc, offset",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(map_err)?;
        for row in rows {
            let canonical = row.map_err(map_err)?;
            if canonical.chars().count() < 3
                || !canonical.chars().all(|ch| ch.is_alphanumeric())
                || crate::summarize::related_context::is_stopword(&canonical)
            {
                continue;
            }
            if seen.insert(canonical.clone()) {
                canonical_terms.push(canonical);
                if canonical_terms.len() == max_terms {
                    break;
                }
            }
        }
        Ok(canonical_terms)
    })();
    tx.rollback().map_err(map_err)?;
    result
}

fn fts_match_content_terms(terms: &[String], separator: &str) -> Option<String> {
    if terms.is_empty() {
        return None;
    }
    Some(
        terms
            .iter()
            .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(separator),
    )
}

/// Exact-token OR expression over an already-normalized content-term list.
pub(crate) fn fts_match_content_terms_any(terms: &[String]) -> Option<String> {
    fts_match_content_terms(terms, " OR ")
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
pub(crate) fn fts_match_query_any(q: &str) -> Option<String> {
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
mod echo_provenance_regression_tests {
    use super::*;

    fn mem_db() -> Db {
        register_vec_extension();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        let db = Db {
            conn: Mutex::new(conn),
        };
        db.migrate().unwrap();
        db
    }

    fn meeting(id: &str) -> Meeting {
        Meeting {
            id: id.to_string(),
            started_at: "2026-07-29T09:00:00Z".to_string(),
            ended_at: None,
            title: Some("Echo provenance regression".to_string()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Transcribed,
            folder_id: None,
        }
    }

    fn stored(
        idx: i64,
        start_s: f64,
        end_s: f64,
        text: &str,
        speaker: Option<&str>,
        confidence: Option<f32>,
        echo_suppressed: bool,
    ) -> StoredTranscriptSegment {
        StoredTranscriptSegment {
            segment: Segment {
                idx,
                start_s,
                end_s,
                text: text.to_string(),
                speaker: speaker.map(str::to_string),
                confidence,
            },
            echo_suppressed,
        }
    }

    fn canonical_stored(rows: &[StoredTranscriptSegment]) -> Vec<u8> {
        serde_json::to_vec(rows).expect("stored transcript rows serialize")
    }

    #[test]
    fn echo_provenance_round_trips_exact_sparse_rows_in_idx_order() {
        let db = mem_db();
        db.insert_meeting(&meeting("m-echo-roundtrip")).unwrap();
        let rows = vec![
            stored(
                42,
                90_001.125,
                90_004.875,
                "Zażółć gęślą jaźń 🫧",
                None,
                None,
                true,
            ),
            stored(
                3,
                0.000_976_562_5,
                1.234_567_890_125,
                "先に届いた system row",
                Some("remote-custom"),
                Some(0.8125),
                false,
            ),
            stored(
                900,
                123_456.789_062_5,
                123_460.000_000_25,
                "local sparse tail",
                Some("me"),
                Some(0.0),
                false,
            ),
        ];
        db.replace_segments_with_echo_provenance("m-echo-roundtrip", &rows)
            .unwrap();

        let actual = db
            .get_segments_with_echo_provenance("m-echo-roundtrip")
            .unwrap();
        let expected = vec![rows[1].clone(), rows[0].clone(), rows[2].clone()];
        assert_eq!(
            canonical_stored(&actual),
            canonical_stored(&expected),
            "sparse ids, f64 timestamps, Unicode, nullable fields, ordering, and both provenance \
             flags must round-trip exactly"
        );
    }

    #[test]
    fn echo_provenance_replace_rolls_back_delete_and_partial_insert_on_constraint_failure() {
        let db = mem_db();
        db.insert_meeting(&meeting("m-echo-rollback")).unwrap();
        let before = vec![
            stored(
                7,
                1.25,
                2.5,
                "original system",
                Some("remote-custom"),
                Some(0.75),
                false,
            ),
            stored(19, 3.75, 5.5, "original marked mic", Some("me"), None, true),
        ];
        db.replace_segments_with_echo_provenance("m-echo-rollback", &before)
            .unwrap();

        let duplicate_idx = vec![
            stored(
                4,
                10.0,
                11.0,
                "first fresh row",
                Some("others"),
                None,
                false,
            ),
            stored(
                4,
                12.0,
                13.0,
                "duplicate primary key",
                Some("me"),
                None,
                true,
            ),
        ];
        assert!(
            db.replace_segments_with_echo_provenance("m-echo-rollback", &duplicate_idx)
                .is_err(),
            "the second duplicate idx must fail after the transaction deleted and inserted once"
        );

        let after = db
            .get_segments_with_echo_provenance("m-echo-rollback")
            .unwrap();
        assert_eq!(
            canonical_stored(&after),
            canonical_stored(&before),
            "the failed replacement must roll back both DELETE and partial INSERT byte-exactly"
        );
    }

    #[test]
    fn system_search_uses_the_same_non_null_non_me_rule_as_rendering() {
        let db = mem_db();
        db.insert_meeting(&meeting("m-noncanonical-system"))
            .unwrap();
        db.replace_segments_with_echo_provenance(
            "m-noncanonical-system",
            &[stored(
                8,
                1.0,
                2.0,
                "noncanonical predicate sentinel",
                Some("remote-custom"),
                None,
                false,
            )],
        )
        .unwrap();

        let (hits, meetings_truncated, rows_truncated) = db
            .search_transcript_segments_visible(
                "noncanonical predicate sentinel",
                None,
                crate::audio::merge::RenderChannel::System,
                20,
                100,
                &HashSet::new(),
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].meeting_id, "m-noncanonical-system");
        assert_eq!(hits[0].seg_idx, 8);
        assert!(!meetings_truncated && !rows_truncated);
    }
}

#[cfg(test)]
#[path = "db_tests/search_helper_tests.rs"]
mod search_helper_tests;

// `row_to_note` moved to `storage::notes_store` (God-file split) alongside the note readers that are its only callers.

// `row_to_folder` moved to `storage::folders_store` (God-file split) alongside the folder readers that are its only callers.

// `row_to_note_folder` moved to `storage::folders_store` (God-file split) alongside the folder readers that are its only callers.

// `row_to_note_row` moved to `storage::notes_store` (God-file split) alongside the note readers that are its only callers.

#[cfg(test)]
#[path = "db_tests/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "db_tests/lock_tests.rs"]
mod lock_tests;

#[cfg(test)]
#[path = "db_tests/lock_contention_tests.rs"]
mod lock_contention_tests;

#[cfg(test)]
#[path = "db_tests/graph_tests.rs"]
mod graph_tests;

#[cfg(test)]
#[path = "db_tests/reminder_tests.rs"]
mod reminder_tests;

#[cfg(test)]
#[path = "db_tests/dashboard_tests.rs"]
mod dashboard_tests;
