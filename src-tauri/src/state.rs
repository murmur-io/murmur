use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

use crate::audio::listener::VoiceListener;
use crate::audio::system::SystemAudioRecorder;
use crate::audio::Recorder;
use crate::error::{AppError, Result};
use crate::settings::AppConfig;
use crate::storage::Db;

/// SQLite database filename inside the app-data folder.
const DB_FILE: &str = "meetnotes.sqlite";

/// App-data folder name for the DB + recorded audio, isolated PER BUILD PROFILE.
///
/// A DEBUG build (`tauri dev`, `cargo test`) — or any run with an explicit dev DEK
/// (`MURMUR_DEV_DEK`) — uses `MeetNotes-dev`; the notarized RELEASE build keeps `MeetNotes`.
/// `cfg!(debug_assertions)` is true for `tauri dev` and false for the release build, so the
/// isolation is automatic with NO env var required; the `MURMUR_DEV_DEK` clause also covers
/// an explicit dev-DEK run of a release-profile binary.
///
/// WHY: dev and release key the whole-DB SQLCipher file with DIFFERENT DEKs (release pulls the
/// real keychain DEK; dev uses the fixed `MURMUR_DEV_DEK`/debug hatch — see lock-model). Sharing
/// one app-data dir means each build opens the other's DB with the wrong key and fails read-only
/// — the recurring "Murmur can't open your library" on `npm run dev`. A separate dir keeps each
/// build's library intact. This is the ONLY source of truth for the DB + audio dir name; do not
/// re-derive the `cfg!` logic elsewhere.
///
/// NOTE: the whisper models dir (`transcribe::model::models_dir`) deliberately stays on the
/// SHARED `MeetNotes` name on purpose — the ~3GB model is not sensitive and must not be
/// re-downloaded for every dev run — so it does NOT call this helper.
pub fn app_dir_name() -> &'static str {
    if cfg!(debug_assertions) || std::env::var_os("MURMUR_DEV_DEK").is_some() {
        "MeetNotes-dev"
    } else {
        "MeetNotes"
    }
}

/// Live state of a MANUAL voice-command capture (the button trigger). Held in
/// `AppState::voice_command_capture` while the user is "asking the assistant".
///
/// CLICK-TO-STOP: the user controls when they are done. At arm time we latch the recorder's current
/// total-sample offset (`start_sample`); each live tick transcribes ONLY the audio captured SINCE
/// that offset (a GROWING window) — the whole post-click utterance, never the rolling tail of prior
/// speech — but we do NOT auto-dispatch on hearing speech. We keep listening (accumulating) until
/// the user clicks "stop" (`end_voice_command` flips `ended` to true → dispatch the FULL accumulated
/// utterance). `budget` is now a generous BACKSTOP cap (a countdown of live ticks, each ≈3s, see
/// `transcribe::live::TICK`): the primary end is the user's click; the cap only prevents listening
/// forever if no stop ever arrives. An all-silent / garbage-only capture is NEVER dispatched — it
/// ends gracefully as "nothing_heard".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureState {
    /// Remaining live ticks of BACKSTOP before we auto-stop a capture the user never explicitly ended
    /// (so it can't listen forever). Decremented EVERY tick. Armed to [`Self::DEFAULT_BUDGET`] by
    /// `begin_voice_command`. The PRIMARY end is the user's `end_voice_command` click; this is just
    /// the safety cap.
    pub budget: u32,
    /// The recorder's total-sample offset latched at arm time. Each tick transcribes the audio
    /// captured since this offset (`Recorder::snapshot_from`), so the command = what was said AFTER
    /// the click. `None` only in degenerate paths (no recorder / poisoned lock at arm time), in which
    /// case the loop falls back to the rolling-tail window so the capture still functions.
    pub start_sample: Option<usize>,
    /// Set true by `end_voice_command` (the user's "stop" click). On the next tick the live loop
    /// dispatches the FULL accumulated post-click utterance (or surfaces "nothing_heard" if silent).
    pub ended: bool,
}

impl CaptureState {
    /// BACKSTOP cap: how many live ticks (≈3s each) a manual capture may listen with NO explicit stop
    /// before it auto-stops + dispatches so it can never listen forever. ~20 ticks ≈ 60s is generous —
    /// the user's `end_voice_command` click is the primary end; this is only the safety backstop and
    /// must not feel like a cutoff for a normal-length question.
    pub const DEFAULT_BUDGET: u32 = 20;

    /// A freshly-armed capture with the default backstop and no latched offset (the offset is set
    /// by `armed_from` at the call site that has the recorder). Kept for the headless tests that
    /// don't have a recorder; the live loop falls back to the rolling tail when `start_sample` is None.
    pub fn armed() -> Self {
        Self {
            budget: Self::DEFAULT_BUDGET,
            start_sample: None,
            ended: false,
        }
    }

    /// A freshly-armed capture that latches the recorder's total-sample `offset` at arm time, so the
    /// live loop transcribes only the POST-CLICK utterance.
    pub fn armed_from(offset: usize) -> Self {
        Self {
            budget: Self::DEFAULT_BUDGET,
            start_sample: Some(offset),
            ended: false,
        }
    }
}

pub struct AppState {
    /// Some while recording.
    pub recorder: Mutex<Option<Recorder>>,
    /// Some while recording AND system-audio capture is enabled + available.
    pub system_recorder: Mutex<Option<SystemAudioRecorder>>,
    /// Some while recording AND echo-cancellation (VPIO AEC) capture is enabled + available.
    pub aec_recorder: Mutex<Option<crate::audio::aec::AecRecorder>>,
    /// Some while recording: the STAGE-2 crash-salvage spill writer, mirroring the RAM mic buffer to
    /// an on-disk spill so a crash mid-record is recoverable at next launch (see `audio::spill`). Its
    /// `Drop` deletes the plaintext spill + sidecar, so `stop_recording` just `take()`s it into a
    /// guard that drops on every exit path (clean Stop ⇒ spill gone; only a crash leaves it behind).
    pub spill_writer: Mutex<Option<crate::audio::spill::SpillWriter>>,
    /// Some while the voice-trigger listener is running.
    pub voice_listener: Mutex<Option<VoiceListener>>,
    /// MANUAL voice-command capture (the button trigger): `Some` while the user has clicked
    /// "ask the assistant" and the live loop is collecting the next spoken utterance as a command —
    /// NO wake word required. Armed by [`crate::commands::begin_voice_command`], consumed + cleared
    /// by the live loop (`transcribe::live`). The budget is a small N-tick countdown rather than a
    /// wall-clock deadline (this codebase has no `Instant` capture pattern): each live tick decrements
    /// it, and on a recognized intent OR budget exhaustion the captured tail is dispatched over the
    /// SAME gated `handle_voice_action` path as the wake trigger. Opt-in PER CLICK — independent of
    /// the `realtime_reactions` toggle.
    pub voice_command_capture: Mutex<Option<CaptureState>>,
    /// Db is internally Send+Sync (Mutex<Connection>). Held in an `Arc` so the egress-ledger
    /// sink (`DbEgressSink`) can hold a cheaply-cloned handle without requiring a second keychain
    /// access or separate connection (the ledger writes go through the same locked `Mutex<Conn>`).
    ///
    /// INVARIANT: never hold the Db lock across a provider/await call — the egress sink re-locks
    /// the same non-reentrant Mutex<Connection> inside `DbEgressSink::record`; holding it across
    /// any provider `await` point would self-deadlock.
    pub db: Arc<Db>,
    /// In-memory cache of the settings table. `Arc` because [`AppState::reasoner`] holds the SAME
    /// handle: every settings/consent write here is what the per-call reasoner dispatch reads, so
    /// consent grants/revocations, provider switches, and `brain_backend` flips take effect on the
    /// next reasoning call without an app restart.
    pub config: Arc<Mutex<AppConfig>>,
    /// The LIVE reasoning dispatch ([`crate::reason::ReasonerCell`]): each `current()` call
    /// re-resolves Cloud/Local/Off from the CURRENT config (it shares [`AppState::config`]'s
    /// handle), caching only the expensive local GGUF instance. Consumers call
    /// `state.reasoner.current()` per turn — never hold a resolved reasoner across turns.
    pub reasoner: crate::reason::ReasonerCell,
    /// The RECORDING pointer: `Some` ONLY while a recording is in flight — set at
    /// `start_recording`, `take()`-cleared at `stop_recording`. It is NOT a "what the user is
    /// looking at" pointer; a past meeting the user opens while idle (or while a DIFFERENT meeting
    /// records) is NOT here. Using it as the brain's "this meeting" scope was the root of the
    /// wrong-meeting fragility — that role now belongs to [`AppState::focus_meeting`] (Phase 6),
    /// with `current_meeting` demoted to the last-resort recording fallback in
    /// [`crate::transcribe::live::resolve_scope_meeting`].
    pub current_meeting: Mutex<Option<uuid::Uuid>>,
    /// PHASE 6 — the FOCUS pointer: the meeting the user is currently VIEWING / anchored to (a
    /// meeting-detail or conversation the FE opened), INDEPENDENT of recording. The FE sets it via
    /// `set_focus_meeting(Some(id))` when it opens a meeting view and clears it (`None`) when it
    /// closes — so the brain's Tier-1 "this meeting" scope is deterministic even when nothing is
    /// recording AND when a different meeting is recording. Precedence
    /// (`resolve_scope_meeting`): an explicit FE-sent `meeting_id` (a bound thread) wins over
    /// `focus_meeting`, which wins over `current_meeting` (recording). This is ONLY an id (never
    /// meeting content), so it needs no
    /// seal/clear-on-relock — a relock re-masks the focused meeting's CONTENT through the same
    /// `meeting_is_visible` gate `gated_live_context` already fail-closes on; the stale id itself
    /// leaks nothing. Blank/whitespace counts as absent. No PII (opaque id only).
    pub focus_meeting: Mutex<Option<String>>,
    /// Accumulated rough transcript of the recording IN PROGRESS, built from the live captions
    /// (`transcribe::live`). Segments aren't persisted until Stop, so this is the ONLY in-flight
    /// view of "what's being said right now" — the in-meeting assistant injects it so it can answer
    /// questions about the current meeting. Cleared at each recording start; bounded in size.
    pub live_transcript: Mutex<String>,
    /// RISING-EDGE dedup for the 4h [`crate::audio::recorder::MAX_RECORDING_SECONDS`] cap notice.
    /// The status poll (`recording_level`) checks `Recorder::cap_reached()` on every tick; this flag
    /// makes the resulting [`crate::events::EVENT_RECORDING_CAPPED`] fire EXACTLY ONCE per recording.
    /// Reset to `false` at each `start_recording`; latched `true` the first tick the cap is reached.
    /// Distinct from the byte/size storage cap — this is the wall-clock TIME cap.
    pub capped_notified: AtomicBool,
    /// Realtime Reactions SHADOW-mode counter (spec §4.2): how many contradiction cards WOULD have
    /// fired this recording while the `brain_contradiction_cards` sub-toggle is OFF. Lets the FE offer
    /// "the brain would have flagged N — enable?" — user-local calibration, no telemetry. Reset to 0
    /// at each `start_recording`; incremented by the reactions worker when in shadow mode.
    pub reactions_shadow_count: AtomicU64,
    /// Per-recording SESSION dedup of already-surfaced whisper cards (key = entity|predicate|old-value).
    /// Prevents the same contradiction re-emitting every ~21 s scan (and re-inflating the shadow count)
    /// — the "does not resurface this session" contract (deep-review). Cleared at each `start_recording`.
    pub reactions_emitted: Mutex<std::collections::HashSet<String>>,
    /// Brain v2 P0.3 — per-meeting IN-FLIGHT assistant-turn counter: how many assistant turns are
    /// currently running for each scope meeting (key = the FE-sent meeting id, "" for the unscoped
    /// voice/wake path). `spawn_assistant_turn` DEDUPS on it: a second turn for the same key while one
    /// is in flight is dropped (the overlapping-wake / double-click pile-up guard), so at most one
    /// generation per scope contends for Metal at a time. Keys are OPAQUE meeting ids — no PII.
    /// Incremented via `try_begin_turn`, decremented by the turn's panic-safe RAII guard.
    pub in_flight_turns: Mutex<std::collections::HashMap<String, u32>>,
    /// Brain v2 P0.3 — USER-TURN PRIORITY flag: `true` while ANY user-initiated assistant turn is in
    /// flight (set at turn start, cleared by the same RAII guard on every exit path incl. panic). The
    /// background Realtime-Reactions scan checks it and DEFERS its light-model extraction, so a
    /// user-facing answer never competes with a background scan for the on-device engine. No PII.
    pub user_turn_in_progress: std::sync::atomic::AtomicBool,
    /// Folder ids unlocked in the current session: sealed folders decrypted for in-app view +
    /// MCP until relock (cleared on screen-share start or app exit). Arc so the MCP server
    /// thread shares the SAME set as the command surface.
    pub unlocked_folders: Arc<Mutex<std::collections::HashSet<String>>>,
    /// Master KEK released by biometric; None until first unlock, zeroized on relock. Held in a
    /// `Zeroizing` so the bytes are wiped from RAM whenever the cached copy is dropped/replaced (C4),
    /// in addition to the explicit `zeroize()` on relock-all.
    pub master_kek: Mutex<Option<zeroize::Zeroizing<[u8; 32]>>>,
    /// M3-CLIENT (spec §3/§4) — the logged-in sharing account for THIS session, or `None` when logged
    /// out. Holds the account id, the unwrapped account master key `MK` (zeroized on drop), the cached
    /// device id, and the current identity generation. The MK never touches SQLite (it is unwrapped
    /// from the server-stored `mk_wrap_pw` at login via the OPAQUE `export_key`); the session tokens
    /// live in the Keychain (source of truth) — this cache only holds MK + non-secret metadata so a
    /// share can be sealed without re-prompting for the password. Cleared on logout.
    pub account_session: Mutex<Option<crate::share::AccountSession>>,
    /// SINGLE-FLIGHT guard for the OPAQUE access-token refresh (`/v1/auth/refresh`). A refresh token is
    /// single-use — two share ops racing to refresh with the SAME token would trip the server's reuse
    /// detection and revoke the whole family (logging the user out mid-share). This async mutex
    /// serializes the refresh critical section (held ACROSS the network call, unlike the std `Mutex`es
    /// here which are never held across an `await`); the winner rotates + persists the new pair, losers
    /// re-read the freshened session token. `()` because it guards a section, not state.
    pub share_refresh_lock: tokio::sync::Mutex<()>,
    /// BLK-1 coarse LIFECYCLE lock. Serializes the folder-lock state machine
    /// (`lock_folder` / `unlock_folder` / `relock_folder` / `relock_all_inner` / `remove_lock` /
    /// the seal half of `move_note`) so two of them can NEVER interleave their multi-step
    /// restore→clear / blank sequences. Without it, the off-thread `relock_all_inner` (screen-share
    /// watcher, window-close, app-exit) could blank a note's `markdown` to `''` in the window
    /// `remove_lock` opens between restoring plaintext (Step 1) and clearing `content_blob`
    /// (Step 2) → `markdown='' + content_blob=NULL` = PERMANENT, IRREVERSIBLE content loss. It is a
    /// `Mutex<()>` used purely as a critical-section guard: a poisoned `()` carries no invalid
    /// state, so acquirers recover via `into_inner()` rather than bricking all future lock ops.
    pub lifecycle: Mutex<()>,
}

impl AppState {
    /// Open DB at the app-data dir, run migrations, load config. Called once in `lib::run`.
    ///
    /// Returns `Err` (never panics) on any failure so the caller can show a graceful dialog and
    /// exit cleanly: a [`AppError::KeychainDenied`] when macOS refused keychain access, or a
    /// storage/migration error when the database itself could not be opened (key mismatch /
    /// corruption). On EVERY failure path the on-disk database and its `.pre-encrypt.bak` backup
    /// are left byte-for-byte intact — opening with a wrong key fails read-only (proven by
    /// `init_with_wrong_key_errors_and_preserves_db`), and migration only swaps after a verified
    /// encrypted copy exists (see `storage::migration`).
    pub fn init() -> Result<Self> {
        let db_path = Self::db_path()?;
        // SQLCipher at-rest: fetch (or create) the DEK once (Keychain). A denial surfaces as a
        // typed AppError::KeychainDenied here — propagated, not unwrapped.
        let dek = crate::secrets::get_or_create_db_dek()?;
        Self::init_at(&db_path, &dek)
    }

    /// Core of [`AppState::init`] with the DB path + DEK injected, so it can be unit-tested
    /// against a temp database without touching the real Keychain or app-data dir. Migrates any
    /// existing PLAINTEXT DB to encrypted (safe: the original is untouched until a verified atomic
    /// swap), then opens the keyed connection. A wrong/garbage key makes `Db::open_with_key` fail
    /// before any write, so the file is left unchanged. `pub(crate)` ONLY so other modules'
    /// tests (e.g. the pipeline stale-consent regression) can build a real `AppState` headless —
    /// production code must keep entering through [`AppState::init`].
    pub(crate) fn init_at(db_path: &std::path::Path, dek: &str) -> Result<Self> {
        // B4 startup sweep: remove any PLAINTEXT-era `*.pre-encrypt.bak` snapshot an older build may
        // have left in the app-data dir (the live at-rest leak this audit closes). Runs BEFORE the
        // migration so the fresh KEYED backup that `encrypt_in_place` writes survives. LEAVES
        // `*.session-encrypted.bak` untouched — those are ENCRYPTED user data, not a leak.
        if let Some(dir) = db_path.parent() {
            sweep_pre_encrypt_baks(dir);
        }
        if crate::storage::migration::needs_encryption(db_path)? {
            tracing::info!(target: "state", "plaintext DB detected — encrypting at rest");
            crate::storage::migration::encrypt_in_place(db_path, dek)?;
        }
        let db = Arc::new(Db::open_with_key(db_path, dek)?);
        let config = Arc::new(Mutex::new(AppConfig::load(&db)?));

        // The live reasoner dispatch shares the config handle, so consent/provider/backend changes
        // written by the settings commands reach the very next reasoning call — no restart. Cheap +
        // panic-free: backends resolve lazily per call; a missing/failed local model degrades to
        // the StubReasoner.
        let reasoner = crate::reason::ReasonerCell::new(Arc::clone(&config));

        // SHOULD-FIX startup reconciliation: re-assert the at-rest sealed shape of every locked
        // folder. If the app crashed (or was force-quit) WHILE a folder was session-unlocked, the
        // plaintext markdown/transcript/timeline + a decrypted WAV may still be on disk. Re-blank
        // those columns (the blobs remain the source of truth) and re-seal any stray plaintext WAV
        // whose `.enc` already exists, so plaintext never survives a crash into the next session.
        // Best-effort: a reconciliation error is logged, never fatal to startup.
        reconcile_locked_at_rest(&db);

        tracing::info!(target: "state", "app state initialized");

        Ok(Self {
            recorder: Mutex::new(None),
            system_recorder: Mutex::new(None),
            aec_recorder: Mutex::new(None),
            spill_writer: Mutex::new(None),
            voice_listener: Mutex::new(None),
            voice_command_capture: Mutex::new(None),
            db,
            config,
            reasoner,
            current_meeting: Mutex::new(None),
            focus_meeting: Mutex::new(None),
            live_transcript: Mutex::new(String::new()),
            capped_notified: AtomicBool::new(false),
            reactions_shadow_count: AtomicU64::new(0),
            reactions_emitted: Mutex::new(std::collections::HashSet::new()),
            in_flight_turns: Mutex::new(std::collections::HashMap::new()),
            user_turn_in_progress: AtomicBool::new(false),
            unlocked_folders: Arc::new(Mutex::new(std::collections::HashSet::new())),
            master_kek: Mutex::new(None),
            account_session: Mutex::new(None),
            share_refresh_lock: tokio::sync::Mutex::new(()),
            lifecycle: Mutex::new(()),
        })
    }

    /// `<app-data>/<app_dir_name()>/meetnotes.sqlite`, creating the directory if absent.
    /// The folder is `MeetNotes` for release and `MeetNotes-dev` for dev/debug ([`app_dir_name`]).
    fn db_path() -> Result<PathBuf> {
        let base = dirs::data_dir()
            .ok_or_else(|| AppError::Storage("could not resolve app-data directory".into()))?;
        let dir = base.join(app_dir_name());
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppError::Storage(format!("create app-data dir: {e}")))?;
        Ok(dir.join(DB_FILE))
    }
}

/// SHOULD-FIX startup reconciliation (filesystem half of [`Db::reblank_locked_folders_at_rest`]).
/// Re-blanks the locked folders' plaintext columns at the DB level, then re-seals stray plaintext
/// audio on disk for ALL THREE per-stream files of each locked meeting — the playback WAV
/// (`audio_path`) AND the two hi-res masters (`mic_master_path` / `sys_master_path`). A
/// crash-while-unlocked decrypts EVERY sealed stream, so reconciling only the playback copy would
/// leave `{id}.mic.wav` / `{id}.sys.wav` plaintext on disk forever (B1).
///
/// For each path: if it is a PLAINTEXT file (not `.enc`) with a sibling `<file>.enc` present, drop
/// the plaintext and re-point the column at the `.enc`. This covers both crash shapes — the
/// plaintext still on disk (drop it) and the plaintext already gone but the column still pointing at
/// it (`remove_file` no-ops, the dangling column is re-pointed at the surviving `.enc`). We only
/// re-point when the encrypted twin exists (never destroy the only copy, and never ENCRYPT here —
/// there is no content key at startup). Best-effort and panic-free: every failure is logged, never
/// fatal to launch.
fn reconcile_locked_at_rest(db: &Db) {
    let (rows, rollup_exports) = match db.reblank_locked_folders_at_rest() {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(target: "state", error = %e, "startup reconciliation: re-blank of locked folders failed");
            return;
        }
    };
    // Brain v2 L2.1 LOCK-SAFETY (filesystem half): when any folder is locked, the reconcile tx
    // purged EVERY memory-rollup row (a rollup may paraphrase sealed facts) — remove their exported
    // vault `.md`s here. Only ever the recorded exported paths; a missing file is fine. The rollups
    // regenerate from visible facts on the next hourly pass.
    for p in &rollup_exports {
        let _ = std::fs::remove_file(p);
    }
    for row in rows {
        let crate::storage::LockedMeetingAudio {
            meeting_id,
            audio_path,
            mic_master_path,
            sys_master_path,
        } = row;
        reseal_stray_audio(audio_path.as_deref(), "playback WAV", |enc| {
            db.set_meeting_audio_path(&meeting_id, Some(enc))
        });
        reseal_stray_audio(mic_master_path.as_deref(), "mic master", |enc| {
            db.set_meeting_mic_master_path(&meeting_id, Some(enc))
        });
        reseal_stray_audio(sys_master_path.as_deref(), "sys master", |enc| {
            db.set_meeting_sys_master_path(&meeting_id, Some(enc))
        });
    }
}

/// Re-seal one at-rest audio column left by a crash-while-unlocked. If `path` is a plaintext file
/// (not `.enc`) whose sibling `<path>.enc` (the durable encrypted copy) exists, remove the stray
/// plaintext and call `repoint` to re-point the column at the `.enc`. No-op when the column is
/// absent, already `.enc`, or has no encrypted twin (never destroy the only copy). `repoint` is the
/// matching DB setter (`set_meeting_audio_path` / `…_mic_master_path` / `…_sys_master_path`); a
/// setter error is logged, never fatal. `label` names the stream for the log line only.
fn reseal_stray_audio(
    path: Option<&str>,
    label: &str,
    repoint: impl FnOnce(&str) -> crate::error::Result<()>,
) {
    const ENC_SUFFIX: &str = ".enc";
    let Some(path) = path else { return };
    if path.ends_with(ENC_SUFFIX) {
        return; // already sealed at rest.
    }
    let enc_path = format!("{path}{ENC_SUFFIX}");
    // Only re-point when the encrypted twin already exists (the durable copy).
    if !std::path::Path::new(&enc_path).exists() {
        return;
    }
    let _ = std::fs::remove_file(path); // no-op if the plaintext is already gone (dangling column).
    if let Err(e) = repoint(&enc_path) {
        tracing::warn!(target: "state", error = %e, stream = label, "startup reconciliation: re-point of stray plaintext audio to .enc failed");
    } else {
        tracing::warn!(target: "state", stream = label, "startup reconciliation: re-sealed a stray plaintext audio file left by a crash while unlocked");
    }
}

/// Does `path` start with the plaintext-SQLite magic ("SQLite format 3\0")? A SQLCipher-encrypted
/// file begins with random salt, so the magic is absent. Used by the B4 sweep to delete ONLY the
/// genuinely-plaintext recovery snapshots an older build leaked — never a keyed (encrypted) one.
fn is_plaintext_sqlite_file(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut buf = [0u8; 16];
    matches!(f.read(&mut buf), Ok(16)) && &buf == b"SQLite format 3\0"
}

/// B4 startup sweep: delete every PLAINTEXT `*.pre-encrypt.bak` snapshot in `dir` (the live at-rest
/// leak older builds wrote during migration). A `*.pre-encrypt.bak` that is KEYED/encrypted (the
/// modern recovery copy) is LEFT IN PLACE — it is not a leak. `*.session-encrypted.bak` files are
/// NEVER touched (they are encrypted user data). Best-effort: a failed remove is logged, not fatal.
fn sweep_pre_encrypt_baks(dir: &std::path::Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        // Only `*.pre-encrypt.bak`. Explicitly skip `*.session-encrypted.bak` (encrypted user data).
        if !name.ends_with(".pre-encrypt.bak") {
            continue;
        }
        // Only remove the genuinely-plaintext leak; preserve a keyed recovery backup.
        if !is_plaintext_sqlite_file(&path) {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => tracing::warn!(
                target: "state",
                "B4 sweep: removed a leaked plaintext pre-encrypt backup"
            ),
            Err(e) => tracing::warn!(
                target: "state",
                error = %e,
                "B4 sweep: failed to remove a plaintext pre-encrypt backup"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    const GOOD_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const WRONG_KEY: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

    /// The dev/release app-data dir split: a DEBUG build (the test binary always is one) must
    /// resolve to the ISOLATED `MeetNotes-dev` so `npm run dev` can never collide with the
    /// installed release library; a release build with no dev-DEK keeps the shared `MeetNotes`.
    #[test]
    fn app_dir_name_isolates_dev_from_release() {
        if cfg!(debug_assertions) {
            assert_eq!(
                app_dir_name(),
                "MeetNotes-dev",
                "debug/dev build must use the isolated app-data dir"
            );
        } else if std::env::var_os("MURMUR_DEV_DEK").is_none() {
            assert_eq!(
                app_dir_name(),
                "MeetNotes",
                "release build keeps the installed library dir name"
            );
        }
    }

    fn tmp_path(tag: &str) -> PathBuf {
        let p = crate::storage::db::unique_temp_path(&format!("murmur-state-{tag}"), "sqlite");
        let _ = std::fs::remove_file(&p);
        p
    }

    /// Build a real SQLCipher-encrypted DB (keyed with `GOOD_KEY`) by seeding a plaintext DB and
    /// running the production encrypt-in-place path.
    fn seed_encrypted_db(path: &std::path::Path) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE meetings(id TEXT PRIMARY KEY, started_at TEXT, title TEXT);
             CREATE TABLE segments(meeting_id TEXT, idx INTEGER, text TEXT);
             CREATE TABLE notes(meeting_id TEXT, markdown TEXT);
             INSERT INTO meetings VALUES('m1','2026-07-01','Sync'),('m2','2026-07-02','Review');
             PRAGMA user_version=7;",
        )
        .unwrap();
        drop(c);
        crate::storage::migration::encrypt_in_place(path, GOOD_KEY).unwrap();
        // Fold any sidecar WAL/SHM into the main file so the byte-equality check below is stable.
        assert!(path.exists());
    }

    /// B4 sweep: a PLAINTEXT `*.pre-encrypt.bak` (the live at-rest leak older builds wrote) is
    /// removed at startup; a KEYED (encrypted) `*.pre-encrypt.bak` and any `*.session-encrypted.bak`
    /// (encrypted user data) are LEFT IN PLACE.
    #[test]
    fn b4_sweep_removes_plaintext_bak_keeps_encrypted_and_session() {
        let dir = std::env::temp_dir().join(format!(
            "murmur-sweep-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // (a) a genuinely-plaintext pre-encrypt backup → MUST be swept.
        let plaintext_bak = dir.join("meetnotes.sqlite.pre-encrypt.bak");
        {
            let c = Connection::open(&plaintext_bak).unwrap();
            c.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES(1);")
                .unwrap();
        }
        assert!(
            is_plaintext_sqlite_file(&plaintext_bak),
            "fixture is plaintext"
        );

        // (b) a KEYED (encrypted) pre-encrypt backup → MUST be kept (not a leak).
        let keyed_bak = dir.join("other.sqlite.pre-encrypt.bak");
        {
            let c = Connection::open(&keyed_bak).unwrap();
            c.pragma_update(
                None,
                "key",
                "x'00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff'",
            )
            .unwrap();
            c.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES(1);")
                .unwrap();
        }
        assert!(
            !is_plaintext_sqlite_file(&keyed_bak),
            "keyed backup is not plaintext"
        );

        // (c) a session-encrypted backup (encrypted user data) → MUST be kept regardless of content.
        let session_bak = dir.join("meetnotes.sqlite.session-encrypted.bak");
        std::fs::write(
            &session_bak,
            b"SQLite format 3\0 but this is session-encrypted user data",
        )
        .unwrap();

        sweep_pre_encrypt_baks(&dir);

        assert!(
            !plaintext_bak.exists(),
            "plaintext pre-encrypt backup must be swept (B4)"
        );
        assert!(
            keyed_bak.exists(),
            "a keyed pre-encrypt backup is encrypted at rest — keep it"
        );
        assert!(
            session_bak.exists(),
            "session-encrypted backups are user data — never swept"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `encrypt_in_place` (run via `init_at`) must NEVER leave a plaintext `.pre-encrypt.bak` — the
    /// recovery backup is keyed. After a full init, no plaintext bak exists in the dir.
    #[test]
    fn init_leaves_no_plaintext_pre_encrypt_bak() {
        let p = tmp_path("nobak");
        // Seed a PLAINTEXT db so init_at performs the migration (which writes the keyed backup).
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE meetings(id TEXT PRIMARY KEY, started_at TEXT, title TEXT);
                 CREATE TABLE segments(meeting_id TEXT, idx INTEGER, text TEXT);
                 CREATE TABLE notes(meeting_id TEXT, markdown TEXT);
                 INSERT INTO meetings VALUES('m1','2026-07-01','Sync');
                 PRAGMA user_version=7;",
            )
            .unwrap();
        }
        let _state = AppState::init_at(&p, GOOD_KEY).expect("init should encrypt + open");

        // The main DB is now encrypted, and the recovery backup (if present) is NOT plaintext.
        let bak = {
            let mut s = p.as_os_str().to_os_string();
            s.push(".pre-encrypt.bak");
            PathBuf::from(s)
        };
        if bak.exists() {
            let mut f = std::fs::File::open(&bak).unwrap();
            use std::io::Read;
            let mut buf = [0u8; 16];
            let n = f.read(&mut buf).unwrap();
            assert!(
                !(n == 16 && &buf == b"SQLite format 3\0"),
                "B4: init must never leave a PLAINTEXT pre-encrypt backup"
            );
        }

        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&bak);
    }

    /// A wrong/garbage DEK must make `init_at` return `Err` (NOT panic) AND leave the on-disk
    /// database byte-for-byte unchanged — init fails read-only and never destroys data.
    #[test]
    fn init_with_wrong_key_errors_and_preserves_db() {
        let p = tmp_path("wrongkey-init");
        seed_encrypted_db(&p);

        let before = std::fs::read(&p).unwrap();

        let res = AppState::init_at(&p, WRONG_KEY);
        assert!(
            res.is_err(),
            "wrong key must return Err, never panic or yield an empty/garbage DB"
        );

        let after = std::fs::read(&p).unwrap();
        assert_eq!(
            before, after,
            "a failed open must leave the database file byte-for-byte intact"
        );

        // Proof the data survived: the CORRECT key still decrypts the untouched file.
        let conn = Connection::open(&p).unwrap();
        conn.pragma_update(None, "key", format!("x'{GOOD_KEY}'"))
            .unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM meetings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "rows intact after the failed wrong-key open");

        let _ = std::fs::remove_file(&p);
    }

    /// The PRODUCTION wiring of the stale-reasoner fix: `init_at` gives `state.config` and
    /// `state.reasoner` ONE shared live handle, so the exact mutations the settings/consent
    /// commands perform (under `state.config.lock()`) reach the very next `current()` dispatch —
    /// consent grant unblocks, backend flip re-routes, all without a restart. Uses a bogus
    /// provider id so "past the consent gate" is observable as `InvalidArg` (vs the gate's
    /// `Unavailable`) with zero network/keychain/CLI.
    #[test]
    fn reasoner_dispatch_follows_live_config_changes_without_restart() {
        let p = tmp_path("live-reasoner");
        let state = AppState::init_at(&p, GOOD_KEY).unwrap();

        // Fresh install defaults: Cloud backend, consent OFF (fail-closed). Point at a bogus
        // provider id through the SAME lock the settings commands write.
        {
            let mut c = state.config.lock().unwrap();
            assert!(
                !c.cloud_egress_consented,
                "fresh DB defaults to consent OFF"
            );
            c.provider_id = "no_such_provider_for_gate_probe".into();
        }
        match state.reasoner.current().reason("s", "u") {
            Err(AppError::Unavailable(_)) => {} // the consent gate refuses (fail-closed)
            other => panic!("no-consent call must be refused by the gate, got {other:?}"),
        }

        // Grant consent EXACTLY as `consent_to_cloud_egress` does (persist + in-memory cache).
        state
            .config
            .lock()
            .unwrap()
            .grant_cloud_egress_consent(&state.db)
            .unwrap();
        match state.reasoner.current().reason("s", "u") {
            Err(AppError::InvalidArg(_)) => {} // past the gate — the bogus id is now the error
            other => {
                panic!("post-grant call must pass the consent gate without restart, got {other:?}")
            }
        }

        // Flip the brain off (as a settings save would): the next dispatch is the stub.
        state.config.lock().unwrap().brain_backend = crate::settings::BrainBackend::Off;
        assert_eq!(
            state.reasoner.current().id(),
            "stub",
            "a backend flip must re-route the next dispatch without restart"
        );

        let _ = std::fs::remove_file(&p);
    }

    /// B1 regression: a crash WHILE a locked folder was session-unlocked can strand PLAINTEXT audio
    /// on disk for every stream that had been decrypted — not just the playback WAV but BOTH hi-res
    /// masters. Startup reconciliation must re-seal all three: drop the stray plaintext and re-point
    /// the column at the surviving `.enc`. This mirrors the playback-WAV reconciliation, exercised
    /// here for `mic_master_path` + `sys_master_path` across BOTH crash shapes (plaintext still on
    /// disk; plaintext already gone but the column still dangling at it).
    #[test]
    fn reconcile_reseals_stray_master_plaintext_for_locked_meeting() {
        let p = tmp_path("reconcile-masters");
        let db = Db::open_with_key(&p, GOOD_KEY).unwrap();

        // A locked folder governing meeting m1 (reconcile keys off notes.folder_id + folders.locked).
        db.insert_folder(&crate::storage::Folder {
            id: "f1".into(),
            name: "Secret".into(),
            path: "Secret".into(),
            parent_id: None,
            locked: true,
            created_at: "2026-06-26T00:00:00Z".into(),
        })
        .unwrap();
        db.insert_meeting(&crate::storage::Meeting {
            id: "m1".into(),
            started_at: "2026-06-26T09:00:00Z".into(),
            ended_at: None,
            title: Some("t".into()),
            duration_s: 60,
            audio_path: None,
            status: crate::storage::MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&crate::storage::NoteRecord {
            meeting_id: "m1".into(),
            provider_id: "claude_code".into(),
            markdown: String::new(),
            created_at: "2026-06-26T09:05:00Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder("m1", Some("f1")).unwrap();

        // Sibling files derived from the unique db path so concurrent test runs never collide.
        let base = p.to_string_lossy().to_string();
        let mic_plain = format!("{base}.m1.mic.wav");
        let mic_enc = format!("{base}.m1.mic.wav.enc");
        let sys_plain = format!("{base}.m1.sys.wav");
        let sys_enc = format!("{base}.m1.sys.wav.enc");

        // Crash shape A (mic): plaintext STILL present + sibling .enc present.
        std::fs::write(&mic_plain, b"PLAINTEXT-MIC-MASTER").unwrap();
        std::fs::write(&mic_enc, b"ENC-MIC").unwrap();
        // Crash shape B (sys): plaintext ALREADY GONE, only the .enc survives, column still dangles.
        std::fs::write(&sys_enc, b"ENC-SYS").unwrap();
        assert!(
            !std::path::Path::new(&sys_plain).exists(),
            "sys plaintext is gone (crash shape B)"
        );

        db.set_meeting_mic_master_path("m1", Some(&mic_plain))
            .unwrap();
        db.set_meeting_sys_master_path("m1", Some(&sys_plain))
            .unwrap();

        // RECONCILE — the production startup pass.
        reconcile_locked_at_rest(&db);

        // mic: stray plaintext dropped; sys: dangling column re-pointed. Both columns now at the .enc.
        assert!(
            !std::path::Path::new(&mic_plain).exists(),
            "stray plaintext mic master removed"
        );
        let (mic_after, sys_after) = db.get_meeting_master_paths("m1").unwrap();
        assert_eq!(
            mic_after.as_deref(),
            Some(mic_enc.as_str()),
            "mic master re-pointed at .enc"
        );
        assert_eq!(
            sys_after.as_deref(),
            Some(sys_enc.as_str()),
            "sys master dangling column re-pointed at .enc"
        );
        // The encrypted masters (the durable copies) are never touched.
        assert!(
            std::path::Path::new(&mic_enc).exists(),
            "encrypted mic master preserved"
        );
        assert!(
            std::path::Path::new(&sys_enc).exists(),
            "encrypted sys master preserved"
        );

        let _ = std::fs::remove_file(&mic_enc);
        let _ = std::fs::remove_file(&sys_enc);
        let _ = std::fs::remove_file(&p);
    }

    /// A locked meeting whose master is ALREADY sealed at rest (column points at the `.enc`, no
    /// plaintext on disk) must be left exactly as-is by reconciliation — it must not churn the column
    /// or fabricate a missing plaintext file.
    #[test]
    fn reconcile_leaves_already_sealed_masters_untouched() {
        let p = tmp_path("reconcile-sealed");
        let db = Db::open_with_key(&p, GOOD_KEY).unwrap();
        db.insert_folder(&crate::storage::Folder {
            id: "f1".into(),
            name: "Secret".into(),
            path: "Secret".into(),
            parent_id: None,
            locked: true,
            created_at: "2026-06-26T00:00:00Z".into(),
        })
        .unwrap();
        db.insert_meeting(&crate::storage::Meeting {
            id: "m1".into(),
            started_at: "2026-06-26T09:00:00Z".into(),
            ended_at: None,
            title: Some("t".into()),
            duration_s: 60,
            audio_path: None,
            status: crate::storage::MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&crate::storage::NoteRecord {
            meeting_id: "m1".into(),
            provider_id: "claude_code".into(),
            markdown: String::new(),
            created_at: "2026-06-26T09:05:00Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder("m1", Some("f1")).unwrap();

        let base = p.to_string_lossy().to_string();
        let mic_enc = format!("{base}.m1.mic.wav.enc");
        std::fs::write(&mic_enc, b"ENC-MIC").unwrap();
        db.set_meeting_mic_master_path("m1", Some(&mic_enc))
            .unwrap();

        reconcile_locked_at_rest(&db);

        let (mic_after, _sys_after) = db.get_meeting_master_paths("m1").unwrap();
        assert_eq!(
            mic_after.as_deref(),
            Some(mic_enc.as_str()),
            "already-sealed master left as-is"
        );
        assert!(
            !std::path::Path::new(&format!("{base}.m1.mic.wav")).exists(),
            "no plaintext fabricated"
        );

        let _ = std::fs::remove_file(&mic_enc);
        let _ = std::fs::remove_file(&p);
    }
}
