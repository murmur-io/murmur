use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

use crate::audio::listener::VoiceListener;
use crate::audio::source::ActiveRecording;
use crate::error::{AppError, Result};
use crate::settings::AppConfig;
use crate::storage::Db;

static NEXT_MANUAL_CAPTURE_GENERATION: AtomicU64 = AtomicU64::new(1);

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
    /// Distinguishes a re-arm from the capture whose one-shot transcription is still in flight.
    /// Content-free; used only for compare-before-clear so a stale worker cannot erase a new arm.
    pub generation: u64,
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
    /// Absolute source-frame cap for this command. Production arms it from the recorder's retained
    /// history budget, so thermal tick stretching or failed ASR can never listen indefinitely.
    pub max_end_sample: Option<usize>,
    /// Set true by `end_voice_command` (the user's "stop" click). On the next tick the live loop
    /// dispatches the FULL accumulated post-click utterance (or surfaces "nothing_heard" if silent).
    pub ended: bool,
}

/// One finalized manual voice command whose audio has already been transcribed, but whose
/// assistant turn has not yet completed. The slot is deliberately single-entry: the UI can arm
/// only one capture at a time, and [`crate::commands::begin_voice_command`] refuses a new arm while
/// this owned handoff exists. Keeping the exact recording token alongside the opaque meeting id
/// lets Stop carry a command across Live -> Draining without ever dispatching it under a stale or
/// unrelated recording session.
#[derive(Clone)]
pub(crate) struct PendingManualCommand {
    pub meeting_id: String,
    pub capture_generation: u64,
    pub command: String,
    pub recording_token: crate::perf::RecordingSessionToken,
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
            generation: NEXT_MANUAL_CAPTURE_GENERATION
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            budget: Self::DEFAULT_BUDGET,
            start_sample: None,
            max_end_sample: None,
            ended: false,
        }
    }

    /// A freshly-armed capture that latches the recorder's total-sample `offset` at arm time, so the
    /// live loop transcribes only the POST-CLICK utterance.
    pub fn armed_from(offset: usize, max_end_sample: usize) -> Self {
        Self {
            generation: NEXT_MANUAL_CAPTURE_GENERATION
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            budget: Self::DEFAULT_BUDGET,
            start_sample: Some(offset),
            max_end_sample: Some(max_end_sample),
            ended: false,
        }
    }
}

/// Session-cached Organization Content Keys, keyed by `(org_id, generation)`. RAM-only, each value
/// `Zeroizing` so it is wiped on drop/replace (see [`AppState::org_ock_cache`]).
pub type OrgOckCache = std::collections::HashMap<(String, u32), zeroize::Zeroizing<[u8; 32]>>;

#[derive(Clone, Debug)]
pub struct RecordingStopResult {
    pub meeting_id: String,
}

/// Shared content-free result cell for idempotent Stop. Every concurrent caller observes the same
/// detached operation; no caller can steal/drop ActiveRecording or receive a misleading "not
/// recording". The flight intentionally retains only the opaque meeting id — never generated
/// markdown or an exported vault path — because waiter-owned `Arc`s may outlive a later relock.
pub struct RecordingStopFlight {
    result: Mutex<Option<std::result::Result<RecordingStopResult, String>>>,
    notify: tokio::sync::Notify,
}

impl RecordingStopFlight {
    pub fn new() -> Self {
        Self {
            result: Mutex::new(None),
            notify: tokio::sync::Notify::new(),
        }
    }

    pub fn complete(&self, result: std::result::Result<RecordingStopResult, String>) {
        let mut slot = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(result);
            self.notify.notify_waiters();
        }
    }

    pub async fn wait(&self) -> std::result::Result<RecordingStopResult, String> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            // `notify_waiters` does not retain a permit for a future waiter. Register this waiter
            // before checking the result so completion in the check->await window cannot be lost.
            notified.as_mut().enable();
            let ready = {
                self.result
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            };
            if let Some(result) = ready {
                return result;
            }
            notified.as_mut().await;
        }
    }
}

impl Default for RecordingStopFlight {
    fn default() -> Self {
        Self::new()
    }
}

pub struct AppState {
    /// The sole owner of every component for one in-flight recording: paused/active cpal stream,
    /// fixed resident ring, durable mic spool + SQLCipher lease, and optional system helper.
    pub(crate) recorder: Mutex<Option<ActiveRecording>>,
    pub recording_stop: Mutex<Option<Arc<RecordingStopFlight>>>,
    /// Some while the voice-trigger listener is running.
    pub voice_listener: Mutex<Option<VoiceListener>>,
    /// Serializes voice-listener start/stop transitions. Kept separate from `voice_listener` so
    /// the potentially blocking worker join is never hidden behind the option-slot mutex alone.
    pub voice_listener_lifecycle: Mutex<()>,
    /// Set from the first synchronous instruction of `start_recording` until the authoritative
    /// recorder owner is installed (or Start unwinds). `restart_voice_listener` checks it while
    /// holding `voice_listener_lifecycle`, closing the listener-start vs recording-start TOCTOU.
    pub recording_starting: AtomicBool,
    /// MANUAL voice-command capture (the button trigger): `Some` while the user has clicked
    /// "ask the assistant" and the live loop is collecting the next spoken utterance as a command —
    /// NO wake word required. Armed by [`crate::commands::begin_voice_command`], consumed + cleared
    /// by the live loop (`transcribe::live`). The budget is a small N-tick countdown rather than a
    /// wall-clock deadline (this codebase has no `Instant` capture pattern): each live tick decrements
    /// it, and on a recognized intent OR budget exhaustion the captured tail is dispatched over the
    /// SAME gated `handle_voice_action` path as the wake trigger. Opt-in PER CLICK — independent of
    /// the `realtime_reactions` toggle.
    pub voice_command_capture: Mutex<Option<CaptureState>>,
    /// Bounded ownership handoff for a finalized manual command. A Live worker leaves the entry in
    /// place until its lifecycle lease completes; if Draining wins admission, Stop takes the same
    /// entry after live quiescence and dispatches it with the exact Postprocess token.
    pub(crate) pending_manual_command: Mutex<Option<PendingManualCommand>>,
    /// TP-F1 — `true` while the LIVE-caption loop (`transcribe::live::run`) is actually running: the
    /// ONLY consumer of a [`voice_command_capture`](Self::voice_command_capture). Set only after the
    /// resident live model loads, then cleared on EVERY exit by an RAII guard. A click during load
    /// or after load failure is refused rather than arming a consumer-less capture. The recorder being
    /// present is NOT sufficient — on the fresh-install heavy-model default (`large-v3-turbo-q8_0`,
    /// pinned `small` absent) `start_recording` sets `live_model = None` and spawns NO live loop, so a
    /// voice command armed against a live-less recording would WEDGE with no consumer/backstop.
    /// `begin_voice_command_inner` gates on THIS (not just the recorder) so it refuses cleanly
    /// ("voice needs the live model") instead of arming a stuck capture. No PII (a bare flag).
    pub live_running: AtomicBool,
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
    /// Rising-edge latch for a terminal capture/storage fault. The meter poll emits one typed,
    /// content-free event so the UI can auto-finalize the exact durable prefix.
    pub capture_fault_notified: AtomicBool,
    /// Realtime Reactions SHADOW-mode counter (spec §4.2): how many contradiction cards WOULD have
    /// fired this recording while the `brain_contradiction_cards` sub-toggle is OFF. Lets the FE offer
    /// "the brain would have flagged N — enable?" — user-local calibration, no telemetry. Reset to 0
    /// at each `start_recording`; incremented by the reactions worker when in shadow mode.
    pub reactions_shadow_count: AtomicU64,
    /// Per-recording SESSION dedup of already-surfaced whisper cards (key = entity|predicate|old-value).
    /// Prevents the same contradiction re-emitting every ~21 s scan (and re-inflating the shadow count)
    /// — the "does not resurface this session" contract (deep-review). Cleared at each `start_recording`.
    pub reactions_emitted: Mutex<std::collections::HashSet<String>>,
    /// Brain v2 L4 — the RUNNING LIVE BULLETS of the recording IN PROGRESS (RAM, capped at
    /// `transcribe::bullets::MAX_BULLETS_CHARS`, front-trimmed on line boundaries). Updated by the
    /// reactions worker (`transcribe::bullets::bullets_tick`, behind `reactions_busy`); consumed
    /// by the reactions substrate (`brain_reactions::reaction_window`), the gated live-question
    /// inject (`transcribe::live::compose_live_inject`), and — via the crash-recovery
    /// `live_bullets` DB row — the Stop-time note (`SummarizeRequest::live_bullets`). Cleared at
    /// recording start + Stop + the lock-surface idle hygiene (mirrors `live_transcript`); prompt
    /// reads are gated on the scope meeting's visibility (`gated_live_bullets`, fail-closed).
    pub live_bullets: Mutex<String>,
    /// Brain v2 L4 — the bullets' delta position over `live_transcript` (offset + anchor, see
    /// `transcribe::bullets::BulletsTracker`), advanced by the WORKER at its own busy-gated pace
    /// so a skipped scan's text reaches the next update. Reset with `live_bullets`.
    pub live_bullets_tracker: Mutex<crate::transcribe::bullets::BulletsTracker>,
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
    /// Brain v2 L5 — the SESSION verify cache: meeting id → the last verify-pass findings, so a
    /// re-open of the same note this session re-renders the panel WITHOUT a second Jira egress.
    /// RAM-ONLY by design (never persisted — findings paraphrase live connector values about note
    /// lines), and CLEARED on every relock (`relock_folder` / `relock_all_inner`) so a re-sealed
    /// meeting's verification detail can't outlive its session unlock. Keys are opaque meeting
    /// ids — the gate is re-checked by `verify_note_sources` BEFORE the cache is read.
    pub verify_cache: Mutex<std::collections::HashMap<String, Vec<crate::verify::VerifyFinding>>>,
    /// Folder ids unlocked in the current session: sealed folders decrypted for in-app view +
    /// MCP until relock (cleared on screen-share start or app exit). Arc so the MCP server
    /// thread shares the SAME set as the command surface.
    pub unlocked_folders: Arc<Mutex<std::collections::HashSet<String>>>,
    /// Master KEK released by biometric; None until first unlock, zeroized on relock. Held in a
    /// `Zeroizing` so the bytes are wiped from RAM whenever the cached copy is dropped/replaced (C4),
    /// in addition to the explicit `zeroize()` on relock-all.
    pub master_kek: Mutex<Option<zeroize::Zeroizing<[u8; 32]>>>,
    /// M6 Shared Brain — the Organization Content Keys (OCKs) unwrapped for THIS session, keyed by
    /// `(org_id, generation)`. RAM-ONLY by design (spec §"Trust model"): the OCK is unwrapped on
    /// demand from the member's server-relayed grant (via `e2ee::org::open_own_grant`, gated on the
    /// account MK session) and cached here so repeated org seals/opens in one session don't re-fetch +
    /// re-unwrap. NEVER persisted to SQLite/Keychain, NEVER logged. Held in `Zeroizing` so each cached
    /// OCK is wiped from RAM on drop/replace; cleared wholesale on logout.
    pub org_ock_cache: Mutex<OrgOckCache>,
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
    /// Serializes organization mutation/recovery/revoke flows across network awaits. Destructive
    /// source/folder closures acquire this before becoming durable, so an already-dispatched share
    /// finishes first and no later mutation can cross the closure barrier.
    pub org_share_mutation_lock: tokio::sync::Mutex<()>,
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
    /// Meetings whose from-disk salvage pipeline is currently in flight. The retry may retain a
    /// one-shot folder CK across long ASR/provider awaits so a concurrent session relock can still
    /// seal the exact output. Key-changing/destructive commands consult this set under
    /// [`Self::lifecycle`] and refuse permanent unlock, fresh lock, move, or delete until the
    /// salvage guard drops. Session relock remains allowed and revokes visibility immediately.
    /// Opaque meeting ids only; no content or key material lives here.
    pub(crate) active_salvages: Mutex<std::collections::HashSet<String>>,
    /// SEAL EPOCH (L2 follow-up, 2026-07-10) — a monotonic counter bumped at the ENTRY of every
    /// lock-surface mutation (`lock_folder` / `relock_folder` / `relock_all` / `remove_lock`, via
    /// `commands::bump_seal_epoch`). The hourly memory-consolidation job snapshots it before
    /// reading facts and re-checks it before EVERY rollup write (`memory::run_consolidation_pass`):
    /// a mismatch means a seal/relock interleaved mid-pass, so the pass aborts silently instead of
    /// resurrecting just-sealed content into a rollup (the pass-vs-seal TOCTOU). Content-free
    /// (a bare counter) — no PII, nothing to seal.
    pub seal_epoch: AtomicU64,
    /// 2026-07-13 — GLOBAL "one heavy inference at a time" gate. A single permit shared across
    /// every native-runtime call site that loads/runs a heavy ML model (whisper ASR, the
    /// diarizer, the Candle embedder/NER, the brain sidecar dispatch). `spawn_blocking` alone gets
    /// CPU-bound native work off the async runtime but is NOT a concurrency limiter — nothing
    /// stops N heavy calls from landing on the blocking pool simultaneously and fighting each
    /// other for the same RAM/Metal context (the exact mechanism behind the whisper/diarizer
    /// co-residency bug fixed this session, generalized: per-call-site RAM checks alone can't
    /// close the TOCTOU window between "checked headroom" and "actually allocated peak" the way a
    /// semaphore does by construction). Acquire via [`crate::perf::run_heavy`] — do not bypass by
    /// calling `spawn_blocking` directly for a heavy native call (same "one shared chokepoint"
    /// discipline as `meeting_is_unlocked` for content reads).
    pub heavy_inference: Arc<tokio::sync::Semaphore>,
}

type ContentDispatchValidator = dyn Fn(&AppState) -> Result<()> + Send + Sync + 'static;

/// An owned authorization capability for a model/provider dispatch that derives from visible
/// content. The production gate retains the [`tauri::AppHandle`] so every asynchronous poll can
/// resolve the one managed [`AppState`], take its non-reentrant lifecycle mutex, and revalidate the
/// exact caller snapshot before the provider is allowed to make progress.
///
/// The guard intentionally covers ONE `Future::poll` only. It is released whenever the provider
/// yields `Pending`, then reacquired + revalidated before the next continuation. A relock can
/// therefore revoke a future suspended in NER, process startup, HTTP readiness, or any other await
/// without pinning the lifecycle mutex across network/model latency.
#[derive(Clone)]
pub struct ContentDispatchAdmission {
    gate: Arc<ContentDispatchGate>,
}

enum ContentDispatchGate {
    App {
        app: tauri::AppHandle,
        validate: Arc<ContentDispatchValidator>,
    },
    #[cfg(test)]
    Test {
        lifecycle: Arc<Mutex<()>>,
        validate: Arc<dyn Fn() -> Result<()> + Send + Sync + 'static>,
    },
}

impl ContentDispatchAdmission {
    pub(crate) fn new(
        app: &tauri::AppHandle,
        validate: impl Fn(&AppState) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            gate: Arc::new(ContentDispatchGate::App {
                app: app.clone(),
                validate: Arc::new(validate),
            }),
        }
    }

    /// Headless oracle seam. Production cannot construct an admission without an `AppHandle`.
    #[cfg(test)]
    pub(crate) fn for_test(
        lifecycle: Arc<Mutex<()>>,
        validate: impl Fn() -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            gate: Arc::new(ContentDispatchGate::Test {
                lifecycle,
                validate: Arc::new(validate),
            }),
        }
    }

    fn with_authorization<T>(&self, f: impl FnOnce() -> T) -> Result<T> {
        match self.gate.as_ref() {
            ContentDispatchGate::App { app, validate } => {
                use tauri::Manager;
                let state = app.state::<AppState>();
                let _lifecycle = state
                    .lifecycle
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                validate(state.inner())?;
                Ok(f())
            }
            #[cfg(test)]
            ContentDispatchGate::Test {
                lifecycle,
                validate,
            } => {
                let _lifecycle = lifecycle
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                validate()?;
                Ok(f())
            }
        }
    }

    /// Validate a synchronous/local dispatch, releasing the lifecycle guard before model work.
    /// Local inference does not egress; the stronger every-poll wrapper is reserved for async
    /// provider futures whose poll is the actual external-dispatch boundary.
    pub(crate) fn validate(&self) -> Result<()> {
        self.with_authorization(|| ())
    }

    /// Construct and drive one provider future while validating under the lifecycle mutex before
    /// the factory is invoked AND on every poll. The mutex is never retained across
    /// `Pending`/`.await`. Keeping the factory inside the first authorized interval also covers a
    /// trait implementation whose method-call boundary performs synchronous dispatch setup before
    /// returning its future.
    pub(crate) async fn run<F, T>(&self, factory: impl FnOnce() -> F) -> Result<T>
    where
        F: std::future::Future<Output = Result<T>>,
    {
        let mut factory = Some(factory);
        let mut future = None;
        std::future::poll_fn(|cx| {
            self.with_authorization(|| {
                if future.is_none() {
                    let Some(make_future) = factory.take() else {
                        return std::task::Poll::Ready(Err(AppError::Other(anyhow::anyhow!(
                            "provider future factory state was unavailable"
                        ))));
                    };
                    future = Some(Box::pin(make_future()));
                }
                match future.as_mut() {
                    Some(future) => future.as_mut().poll(cx),
                    None => std::task::Poll::Ready(Err(AppError::Other(anyhow::anyhow!(
                        "provider future was unavailable after construction"
                    )))),
                }
            })
            .unwrap_or_else(|error| std::task::Poll::Ready(Err(error)))
        })
        .await
    }
}

impl AppState {
    /// An [`AppState`] backed by a caller-supplied temp SQLCipher DB: no Keychain, no Tauri, no
    /// recorder, default config (no vault, so nothing touches the filesystem).
    ///
    /// Lives here rather than in each test module because the struct has forty-odd fields and every
    /// suite that duplicated the literal had to be edited whenever one was added — which is how a
    /// test file ends up pinned to a stale shape.
    #[cfg(test)]
    pub(crate) fn for_tests(db: Db) -> Self {
        use std::collections::HashSet;
        Self {
            recorder: Mutex::new(None),
            recording_stop: Mutex::new(None),
            voice_listener: Mutex::new(None),
            voice_listener_lifecycle: Mutex::new(()),
            recording_starting: AtomicBool::new(false),
            voice_command_capture: Mutex::new(None),
            pending_manual_command: Mutex::new(None),
            live_running: AtomicBool::new(false),
            db: Arc::new(db),
            config: Arc::new(Mutex::new(AppConfig::default())),
            reasoner: crate::reason::ReasonerCell::fixed(Arc::new(crate::reason::StubReasoner)),
            current_meeting: Mutex::new(None),
            focus_meeting: Mutex::new(None),
            live_transcript: Mutex::new(String::new()),
            live_bullets: Mutex::new(String::new()),
            live_bullets_tracker: Mutex::new(crate::transcribe::bullets::BulletsTracker::default()),
            capped_notified: AtomicBool::new(false),
            capture_fault_notified: AtomicBool::new(false),
            reactions_shadow_count: AtomicU64::new(0),
            reactions_emitted: Mutex::new(HashSet::new()),
            in_flight_turns: Mutex::new(std::collections::HashMap::new()),
            user_turn_in_progress: AtomicBool::new(false),
            verify_cache: Mutex::new(std::collections::HashMap::new()),
            unlocked_folders: Arc::new(Mutex::new(HashSet::new())),
            master_kek: Mutex::new(None),
            org_ock_cache: Mutex::new(std::collections::HashMap::new()),
            account_session: Mutex::new(None),
            lifecycle: Mutex::new(()),
            active_salvages: Mutex::new(HashSet::new()),
            share_refresh_lock: tokio::sync::Mutex::new(()),
            org_share_mutation_lock: tokio::sync::Mutex::new(()),
            seal_epoch: AtomicU64::new(0),
            heavy_inference: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    /// Return the exact Live-session token only when `meeting_id` names the capture currently held
    /// in the recorder slot. Transitional Starting/Draining/Postprocess phases and mismatched or
    /// stale meeting ids fail closed instead of borrowing recording priority for unrelated work.
    pub(crate) fn live_model_token_for_meeting(
        &self,
        meeting_id: &str,
    ) -> Result<Option<crate::perf::RecordingSessionToken>> {
        let recorder = self
            .recorder
            .lock()
            .map_err(|_| AppError::Unavailable("recorder mutex poisoned".into()))?;
        match recorder.as_ref() {
            Some(active) if active.meeting_id == meeting_id => active.live_model_token().map(Some),
            Some(_) => Err(AppError::Unavailable(
                "assistant scope does not match the active recording".into(),
            )),
            None if crate::perf::recording_has_priority() => Err(AppError::Unavailable(
                "recording model session is not accepting live work".into(),
            )),
            None => Ok(None),
        }
    }

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

        // GLOBAL "one heavy inference at a time" gate — constructed BEFORE the reasoner so it can
        // be threaded into `ReasonerCell::new` (see the field's doc comment below for the full
        // rationale).
        let heavy_inference = std::sync::Arc::new(tokio::sync::Semaphore::new(1));

        // The live reasoner dispatch shares the config handle, so consent/provider/backend changes
        // written by the settings commands reach the very next reasoning call — no restart. Cheap +
        // panic-free: backends resolve lazily per call; a missing/failed local model degrades to
        // the StubReasoner.
        let reasoner =
            crate::reason::ReasonerCell::new(Arc::clone(&config), Arc::clone(&heavy_inference));

        // A previous process may have died after publishing a filing target but before the
        // canonical bundle transaction. Drain its SQLCipher identities before any lock repair or
        // app surface can treat those plaintext paths as governed.
        match crate::commands::reconcile_filing_projection_journal_for_startup(&db)? {
            crate::commands::FilingReconcileOutcome::Clean => {}
            crate::commands::FilingReconcileOutcome::UserCollision(issue) => {
                // Exact external occupancy is not database corruption and must not brick Murmur.
                // Every malformed identity/path/SQL state still propagated through `?` above.
                // The conflicting occupant is never overwritten and the durable row remains for an
                // explicit token-bound decision. Log only counts + fixed kind, never token/path.
                let (attempts, projections, source_snapshots) =
                    db.filing_recovery_counts().unwrap_or((0, 0, 0));
                tracing::warn!(
                    target: "recording_filing",
                    attempts,
                    projections,
                    source_snapshots,
                    issue_kind = issue.issue_kind,
                    "startup filing recovery is degraded; journal retained for user resolution"
                );
            }
        }

        // Re-assert the at-rest sealed shape before any AppState/window/MCP surface exists. A crash
        // during an initial seal can leave hidden plaintext without a blob; that shape must be
        // authenticated and completed with the wrapped CK, never blindly blanked/deleted. Failure is
        // fatal-to-startup (the outer setup shows the existing graceful dialog and leaves data intact).
        reconcile_locked_at_rest(&db)?;

        tracing::info!(target: "state", "app state initialized");

        Ok(Self {
            recorder: Mutex::new(None),
            recording_stop: Mutex::new(None),
            voice_listener: Mutex::new(None),
            voice_listener_lifecycle: Mutex::new(()),
            recording_starting: AtomicBool::new(false),
            voice_command_capture: Mutex::new(None),
            pending_manual_command: Mutex::new(None),
            live_running: AtomicBool::new(false),
            db,
            config,
            reasoner,
            current_meeting: Mutex::new(None),
            focus_meeting: Mutex::new(None),
            live_transcript: Mutex::new(String::new()),
            live_bullets: Mutex::new(String::new()),
            live_bullets_tracker: Mutex::new(crate::transcribe::bullets::BulletsTracker::default()),
            capped_notified: AtomicBool::new(false),
            capture_fault_notified: AtomicBool::new(false),
            reactions_shadow_count: AtomicU64::new(0),
            reactions_emitted: Mutex::new(std::collections::HashSet::new()),
            in_flight_turns: Mutex::new(std::collections::HashMap::new()),
            user_turn_in_progress: AtomicBool::new(false),
            verify_cache: Mutex::new(std::collections::HashMap::new()),
            unlocked_folders: Arc::new(Mutex::new(std::collections::HashSet::new())),
            master_kek: Mutex::new(None),
            org_ock_cache: Mutex::new(std::collections::HashMap::new()),
            account_session: Mutex::new(None),
            share_refresh_lock: tokio::sync::Mutex::new(()),
            org_share_mutation_lock: tokio::sync::Mutex::new(()),
            lifecycle: Mutex::new(()),
            active_salvages: Mutex::new(std::collections::HashSet::new()),
            seal_epoch: AtomicU64::new(0),
            heavy_inference,
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

/// Complete or re-assert every locked folder's at-rest shape before any content surface starts.
/// Residual plaintext from an interrupted initial seal is never deleted on ciphertext existence
/// alone: the wrapped content key is recovered strictly and the exact plaintext is re-encrypted +
/// verified first. Fully sealed folders avoid a keychain prompt.
fn reconcile_locked_at_rest(db: &Db) -> Result<()> {
    sweep_locked_audio_crypto_stages(db)?;
    let repair_folders = locked_folders_requiring_authenticated_repair(db)?;
    if repair_folders.is_empty() {
        return finalize_locked_at_rest_cleanup(db);
    }
    // Repair never destroys the only copy: a missing/partial candidate set
    // leaves residue intact and aborts below. The lenient source also lets a
    // debug MURMUR_DEV_KEK finish its isolated interrupted seal without
    // touching the release Keychain.
    let candidates = crate::secrets::list_master_kek_candidates(
        "Repair locked content after an interrupted session",
    )?;
    repair_and_finalize_locked_at_rest(db, &repair_folders, &candidates)
}

/// Keychain-free entry used by state tests: candidates are injected, while production always enters
/// through [`reconcile_locked_at_rest`] and read-only recovery enumeration.
#[cfg(test)]
fn reconcile_locked_at_rest_with_candidates(db: &Db, candidates: &[[u8; 32]]) -> Result<()> {
    sweep_locked_audio_crypto_stages(db)?;
    let repair_folders = locked_folders_requiring_authenticated_repair(db)?;
    repair_and_finalize_locked_at_rest(db, &repair_folders, candidates)
}

fn locked_folders_requiring_authenticated_repair(db: &Db) -> Result<Vec<String>> {
    let mut repair = Vec::new();
    for folder_id in db.locked_folder_ids()? {
        if crate::commands::locked_folder_requires_authenticated_repair(db, &folder_id)? {
            repair.push(folder_id);
        }
    }
    Ok(repair)
}

fn repair_and_finalize_locked_at_rest(
    db: &Db,
    repair_folders: &[String],
    candidates: &[[u8; 32]],
) -> Result<()> {
    for folder_id in repair_folders {
        let wrapped = db
            .folder_wrapped_key(folder_id)?
            .ok_or_else(|| AppError::Storage("locked folder repair has no wrapped key".into()))?;
        let (ck_bytes, _winning_kek, _winner_index) =
            crate::commands::try_unwrap_ck_with_candidates(candidates, &wrapped, folder_id, None)
                .ok_or_else(|| {
                AppError::Auth("no keychain key can repair interrupted locked content".into())
            })?;
        let ck: zeroize::Zeroizing<[u8; 32]> = zeroize::Zeroizing::new(
            ck_bytes
                .as_slice()
                .try_into()
                .map_err(|_| AppError::Storage("unwrapped content key has wrong length".into()))?,
        );
        crate::commands::repair_locked_folder_at_rest(db, folder_id, &ck)?;
        if crate::commands::locked_folder_requires_authenticated_repair(db, folder_id)? {
            return Err(AppError::Storage(
                "locked-folder startup repair did not reach a sealed at-rest shape".into(),
            ));
        }
    }
    finalize_locked_at_rest_cleanup(db)
}

fn sweep_locked_audio_crypto_stages(db: &Db) -> Result<()> {
    let mut by_dir: std::collections::HashMap<
        std::path::PathBuf,
        std::collections::HashSet<String>,
    > = std::collections::HashMap::new();
    for folder_id in db.locked_folder_ids()? {
        for meeting_id in db.meeting_ids_in_folder(&folder_id)? {
            let playback = db
                .get_meeting(&meeting_id)?
                .and_then(|meeting| meeting.audio_path);
            let (mic, system) = db.get_meeting_master_paths(&meeting_id)?;
            for path in [playback, mic, system].into_iter().flatten() {
                let path = std::path::Path::new(&path);
                let (Some(parent), Some(name)) = (
                    path.parent(),
                    path.file_name().and_then(|name| name.to_str()),
                ) else {
                    return Err(AppError::Storage(
                        "locked audio path cannot be reconciled safely".into(),
                    ));
                };
                let targets = by_dir.entry(parent.to_path_buf()).or_default();
                targets.insert(name.to_string());
                if let Some(plain) = name.strip_suffix(".enc") {
                    targets.insert(plain.to_string());
                } else {
                    targets.insert(format!("{name}.enc"));
                }
            }
        }
    }
    let mut removed = 0usize;
    for (directory, targets) in by_dir {
        removed += crate::crypto::sweep_atomic_stages(&directory, &targets)?;
    }
    if removed > 0 {
        tracing::warn!(target: "state", removed, "startup reconciliation removed abandoned audio crypto stages");
    }
    Ok(())
}

fn finalize_locked_at_rest_cleanup(db: &Db) -> Result<()> {
    crate::commands::remove_rollup_exports_before_seal_purge(db)?;
    let (audio_rows, rollup_exports, note_md_exports) = db.reblank_locked_folders_at_rest()?;
    let attachment_exports = db.reblank_locked_attachments_at_rest()?;
    crate::commands::delete_attachment_exports_with_retry(db, attachment_exports)?;
    for path in &rollup_exports {
        crate::crypto::remove_file_verified_absent(
            std::path::Path::new(path),
            "remove purged memory-rollup export during startup reconciliation",
        )?;
    }
    for (document_id, path) in &note_md_exports {
        crate::crypto::remove_file_verified_absent(
            std::path::Path::new(path),
            "remove re-exported authored note during startup reconciliation",
        )?;
        db.set_note_doc_exported_path(document_id, None)?;
    }
    for row in audio_rows {
        for (path, stream) in [
            (row.audio_path.as_deref(), "playback"),
            (row.mic_master_path.as_deref(), "mic"),
            (row.sys_master_path.as_deref(), "system"),
        ] {
            assert_locked_audio_at_rest(path, stream)?;
        }
    }
    // Drain LAST: locked-folder exports/audio/derived rows above must reach a safe at-rest shape
    // even when an unrelated visible source export is a symlink, hardlink, or concurrently edited.
    // Failure retains the encrypted outbox and aborts startup before content surfaces appear.
    crate::commands::drain_lock_marker_export_cleanup(db)
}

fn assert_locked_audio_at_rest(path: Option<&str>, stream: &str) -> Result<()> {
    let Some(path) = path else { return Ok(()) };
    if !path.ends_with(".enc") {
        return Err(AppError::Storage(format!(
            "locked {stream} audio still points at plaintext after startup repair"
        )));
    }
    if !crate::crypto::owned_regular_file_exists(
        std::path::Path::new(path),
        "assert encrypted audio after startup repair",
    )? {
        return Err(AppError::Storage(format!(
            "locked {stream} audio has no encrypted artifact after startup repair"
        )));
    }
    if crate::crypto::owned_regular_file_exists(
        std::path::Path::new(path.trim_end_matches(".enc")),
        "assert plaintext audio absent after startup repair",
    )? {
        return Err(AppError::Storage(format!(
            "locked {stream} audio plaintext survived startup repair"
        )));
    }
    Ok(())
}

/// Would minting a fresh DEK orphan an existing database?
///
/// True when `path` is a file that is NOT plaintext SQLite — i.e. an already-SQLCipher-encrypted
/// database, whose only key is the DEK we are about to replace. A plaintext file is deliberately
/// NOT a refusal: that is the pre-encryption shape, and `storage::migration` encrypts it with the
/// newly-minted key on purpose.
///
/// Split from [`encrypted_db_exists`] so the decision is testable without reaching for the real
/// app-data directory.
pub(crate) fn minting_would_orphan_db(path: &std::path::Path) -> bool {
    path.is_file() && !is_plaintext_sqlite_file(path)
}

/// Is there already an encrypted database in the app-data directory?
///
/// Read-only on purpose: unlike `Db::db_path` this never creates the directory, because it runs on
/// the pre-mint path where creating anything would be a side effect of asking a question.
pub(crate) fn encrypted_db_exists() -> bool {
    let Some(base) = dirs::data_dir() else {
        // No resolvable app-data directory means no database to orphan. Returning false here is the
        // permissive answer, and it is the right one: a fresh install on a machine this improbable
        // still deserves to work, and there is provably nothing to lose.
        return false;
    };
    minting_would_orphan_db(&base.join(app_dir_name()).join(DB_FILE))
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
    const TEST_KEK: [u8; 32] = [0x4b; 32];
    const TEST_CK: [u8; 32] = [0x63; 32];

    fn mark_folder_locked_recoverable(db: &Db, folder_id: &str) {
        let wrapped = crate::crypto::encrypt(
            &TEST_KEK,
            &TEST_CK,
            &crate::commands::aad_wrapped_ck(folder_id),
        )
        .unwrap();
        db.set_folder_locked(folder_id, true, Some(&wrapped))
            .unwrap();
    }

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

    /// A private DIRECTORY for a test that writes locked-audio fixtures.
    ///
    /// `repair_and_finalize_locked_at_rest` scans the PARENT of each locked audio path and refuses
    /// any staging artifact it cannot account for. With fixtures written straight into
    /// `std::env::temp_dir()`, that parent is the shared temp root, so a concurrently-running test's
    /// artifact fails this one — intermittently, and with whichever test happens to lose the race.
    /// Owning the directory is what makes the scan see only this test's own files.
    fn tmp_audio_dir(tag: &str) -> PathBuf {
        let dir = crate::storage::db::unique_temp_path(&format!("murmur-state-{tag}-dir"), "d");
        let _ = std::fs::remove_file(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
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

    /// 2026-07-10 lock-audit F2 (RED-before-GREEN): a session unlock re-exports each authored note's
    /// vault `.md` (`reexport_notes_in_folder`) — a CRASH while unlocked then leaves that plaintext
    /// `.md` on disk with `documents.exported_path` still set. The STARTUP reconcile must delete the
    /// file and clear the column (the clean-relock path already does; the reconcile did not).
    #[test]
    fn reconcile_at_rest_deletes_reexported_note_md() {
        use crate::storage::models::Folder;
        let db = Db::open_with_key(&tmp_path("note-md-reconcile"), GOOD_KEY).unwrap();
        db.insert_folder(&Folder {
            id: "f1".to_string(),
            name: "Secret".to_string(),
            path: "Notes/Secret".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-10T08:00:00Z".to_string(),
        })
        .unwrap();
        db.insert_note("n1", "f1", "plan", "Plan", "", 1).unwrap();
        // The crash-while-unlocked shape: a re-exported plaintext .md + a recorded exported_path.
        let md = std::env::temp_dir().join(format!(
            "murmur-reconcile-note-{}-{}.md",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let exported = "# secret plan\ninternal launch April";
        std::fs::write(&md, exported).unwrap();
        db.set_note_doc_exported_path("n1", Some(md.to_str().unwrap()))
            .unwrap();
        db.set_note_doc_exported_hash("n1", Some(&crate::export::note_content_hash(exported)))
            .unwrap();
        mark_folder_locked_recoverable(&db, "f1");

        reconcile_locked_at_rest_with_candidates(&db, &[TEST_KEK]).unwrap();

        assert!(
            !md.exists(),
            "startup reconcile must delete the re-exported plaintext note .md of a locked folder"
        );
        assert!(
            db.note_exported_paths_in_folder("f1").unwrap().is_empty(),
            "startup reconcile must clear documents.exported_path for the locked folder"
        );
    }

    #[test]
    fn reconcile_wrong_candidate_preserves_hidden_plaintext_and_cleanup_authority() {
        use crate::storage::models::Folder;

        let db = Db::open_with_key(&tmp_path("repair-wrong-kek"), GOOD_KEY).unwrap();
        db.insert_folder(&Folder {
            id: "f1".to_string(),
            name: "Secret".to_string(),
            path: "Notes/Secret".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-10T08:00:00Z".to_string(),
        })
        .unwrap();
        db.insert_note("n1", "f1", "secret", "Secret", "secret body", 1)
            .unwrap();
        let md = tmp_path("repair-wrong-kek-export").with_extension("md");
        std::fs::write(&md, "# secret body").unwrap();
        db.set_note_doc_exported_path("n1", Some(md.to_str().unwrap()))
            .unwrap();
        mark_folder_locked_recoverable(&db, "f1");

        assert!(
            reconcile_locked_at_rest_with_candidates(&db, &[[0x99; 32]]).is_err(),
            "startup must fail closed when no candidate unwraps the repair CK"
        );
        let row = db.get_note_row("n1").unwrap().unwrap();
        assert_eq!(
            row.text, "secret body",
            "canonical plaintext must remain intact"
        );
        assert_eq!(row.exported_path.as_deref(), md.to_str());
        assert!(
            md.exists(),
            "no unauthenticated cleanup may remove the export"
        );
        let _ = std::fs::remove_file(md);
    }

    /// Residual W5 (RED-before-GREEN, startup half): when the reconcile FAILS to delete a
    /// re-exported note `.md`, that note's `exported_path` must be KEPT so the next startup pass
    /// retries — before the fix the bulk clear NULLed every locked-folder path regardless,
    /// forgetting the leaked plaintext `.md` forever. Startup now fails closed before surfaces.
    #[test]
    fn reconcile_keeps_exported_path_when_md_delete_fails() {
        use crate::storage::models::Folder;
        let db = Db::open_with_key(&tmp_path("note-md-keep-path"), GOOD_KEY).unwrap();
        db.insert_folder(&Folder {
            id: "f1".to_string(),
            name: "Secret".to_string(),
            path: "Notes/Secret".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-10T08:00:00Z".to_string(),
        })
        .unwrap();
        db.insert_note("n-keep", "f1", "undeletable", "Undeletable", "", 1)
            .unwrap();
        // A DIRECTORY at the recorded path makes verified file removal fail with a non-NotFound
        // error — the undeletable crash shape.
        let dir = std::env::temp_dir().join(format!(
            "murmur-reconcile-undeletable-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        db.set_note_doc_exported_path("n-keep", Some(dir.to_str().unwrap()))
            .unwrap();
        mark_folder_locked_recoverable(&db, "f1");

        assert!(
            reconcile_locked_at_rest_with_candidates(&db, &[TEST_KEK]).is_err(),
            "an undeletable managed plaintext export must abort startup"
        );

        let keep_row = db.get_note_row("n-keep").unwrap().unwrap();
        assert_eq!(
            keep_row.exported_path.as_deref(),
            dir.to_str(),
            "a FAILED .md delete must keep exported_path recorded for the next startup retry"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
                 CREATE TABLE meetings(id TEXT PRIMARY KEY, started_at TEXT, title TEXT, audio_path TEXT);
                 CREATE TABLE segments(meeting_id TEXT, idx INTEGER, text TEXT);
                 CREATE TABLE notes(meeting_id TEXT, markdown TEXT);
                 INSERT INTO meetings VALUES('m1','2026-07-01','Sync',NULL);
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

        // A locked folder canonically governing meeting m1.
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
        mark_folder_locked_recoverable(&db, "f1");

        // Sibling files derived from the unique db path so concurrent test runs never collide.
        // Fixtures live in a directory this test OWNS — see `tmp_audio_dir`.
        let base = tmp_audio_dir("reconcile")
            .join("audio")
            .to_string_lossy()
            .to_string();
        let mic_plain = format!("{base}.m1.mic.wav");
        let mic_enc = format!("{base}.m1.mic.wav.enc");
        let sys_plain = format!("{base}.m1.sys.wav");
        let sys_enc = format!("{base}.m1.sys.wav.enc");

        // Crash shape A (mic): plaintext STILL present + sibling .enc present.
        std::fs::write(&mic_plain, b"PLAINTEXT-MIC-MASTER").unwrap();
        std::fs::write(&mic_enc, b"INTERRUPTED-STAGE-WILL-BE-REPLACED").unwrap();
        // Crash shape B (sys): plaintext ALREADY GONE, only the .enc survives, column still dangles.
        let sys_source = format!("{base}.m1.sys.source.wav");
        std::fs::write(&sys_source, b"SYSTEM-MASTER").unwrap();
        crate::crypto::encrypt_file(
            &TEST_CK,
            std::path::Path::new(&sys_source),
            std::path::Path::new(&sys_enc),
            b"murmur:audio:v1|meeting=m1|folder=f1|stream=sys",
        )
        .unwrap();
        std::fs::remove_file(&sys_source).unwrap();
        assert!(
            !std::path::Path::new(&sys_plain).exists(),
            "sys plaintext is gone (crash shape B)"
        );

        db.set_meeting_mic_master_path("m1", Some(&mic_plain))
            .unwrap();
        db.set_meeting_sys_master_path("m1", Some(&sys_plain))
            .unwrap();

        // RECONCILE — the production startup pass.
        reconcile_locked_at_rest_with_candidates(&db, &[TEST_KEK]).unwrap();

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
        mark_folder_locked_recoverable(&db, "f1");

        let note_blob = crate::crypto::encrypt(
            &TEST_CK,
            b"",
            &crate::commands::aad_content("f1", "m1", "claude_code", "note"),
        )
        .unwrap();
        db.seal_note("m1", "claude_code", &note_blob).unwrap();

        // Fixtures live in a directory this test OWNS — see `tmp_audio_dir`.
        let base = tmp_audio_dir("reconcile")
            .join("audio")
            .to_string_lossy()
            .to_string();
        let mic_enc = format!("{base}.m1.mic.wav.enc");
        let mic_source = format!("{base}.m1.mic.source.wav");
        std::fs::write(&mic_source, b"MIC-MASTER").unwrap();
        crate::crypto::encrypt_file(
            &TEST_CK,
            std::path::Path::new(&mic_source),
            std::path::Path::new(&mic_enc),
            b"murmur:audio:v1|meeting=m1|folder=f1|stream=mic",
        )
        .unwrap();
        std::fs::remove_file(&mic_source).unwrap();
        db.set_meeting_mic_master_path("m1", Some(&mic_enc))
            .unwrap();

        reconcile_locked_at_rest_with_candidates(&db, &[]).unwrap();

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

    #[tokio::test]
    async fn recording_stop_flight_replays_one_terminal_result_to_all_waiters() {
        let flight = Arc::new(RecordingStopFlight::new());
        let first = flight.clone();
        let second = flight.clone();
        let first_waiter = tokio::spawn(async move { first.wait().await });
        let second_waiter = tokio::spawn(async move { second.wait().await });
        flight.complete(Err("durable finalization failed".into()));
        assert_eq!(
            first_waiter.await.unwrap().unwrap_err(),
            "durable finalization failed"
        );
        assert_eq!(
            second_waiter.await.unwrap().unwrap_err(),
            "durable finalization failed"
        );
        assert_eq!(
            flight.wait().await.unwrap_err(),
            "durable finalization failed"
        );
    }

    /// Successful Stop is replayable to concurrent and late waiters without retaining generated
    /// markdown or a vault path in any waiter-owned `Arc`. The exhaustive pattern is intentional:
    /// adding another field to the internal result makes this oracle fail to compile.
    #[tokio::test]
    async fn recording_stop_flight_replays_content_free_success_to_all_waiters() {
        let flight = Arc::new(RecordingStopFlight::new());
        let first = flight.clone();
        let second = flight.clone();
        let first_waiter = tokio::spawn(async move { first.wait().await });
        let second_waiter = tokio::spawn(async move { second.wait().await });
        flight.complete(Ok(RecordingStopResult {
            meeting_id: "meeting-42".into(),
        }));

        for result in [
            first_waiter.await.unwrap(),
            second_waiter.await.unwrap(),
            flight.wait().await,
        ] {
            let RecordingStopResult { meeting_id } = result.unwrap();
            assert_eq!(meeting_id, "meeting-42");
        }
    }
}

#[cfg(test)]
mod dek_mint_guard_tests {
    use super::minting_would_orphan_db;

    /// The three cases the mint decision has to tell apart — and getting any of them wrong is a
    /// different disaster.
    ///
    /// An ENCRYPTED database must refuse: minting there replaces the only key its contents have,
    /// which is silent, total, unrecoverable loss of every meeting. A PLAINTEXT database must NOT
    /// refuse: that is the pre-encryption shape and `storage::migration` encrypts it with the newly
    /// minted key on purpose, so refusing would brick every upgrading install. And NO file must not
    /// refuse: that is a fresh install, where refusing would mean the app never starts at all.
    ///
    /// A single "does the file exist" check would conflate the first two; a single "is it missing"
    /// check would conflate the last two. That is why this asserts each separately rather than
    /// asserting one predicate twice.
    #[test]
    fn only_an_encrypted_database_refuses_the_mint() {
        let dir = std::env::temp_dir().join(format!("murmur-dek-guard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let missing = dir.join("absent.sqlite");
        assert!(
            !minting_would_orphan_db(&missing),
            "a fresh install has no database — refusing here would stop the app from ever starting"
        );

        let plaintext = dir.join("plain.sqlite");
        std::fs::write(&plaintext, b"SQLite format 3\0and then some payload").unwrap();
        assert!(
            !minting_would_orphan_db(&plaintext),
            "a plaintext database is the pre-encryption shape; migration encrypts it WITH the new \
             key, so refusing would brick every upgrading install"
        );

        let encrypted = dir.join("encrypted.sqlite");
        // A SQLCipher file opens with random salt, so the plaintext magic is absent.
        std::fs::write(&encrypted, b"\x9f\x2c\x7e\x01random salt then ciphertext").unwrap();
        assert!(
            minting_would_orphan_db(&encrypted),
            "an encrypted database's ONLY key is the DEK about to be replaced — minting here is \
             silent, total, unrecoverable loss"
        );

        // A directory is not a database, however it is named.
        let dir_named_like_a_db = dir.join("looks-like.sqlite");
        std::fs::create_dir_all(&dir_named_like_a_db).unwrap();
        assert!(
            !minting_would_orphan_db(&dir_named_like_a_db),
            "a directory is not a file to orphan"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
