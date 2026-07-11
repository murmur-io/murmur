//! HOST side of the on-device brain sidecar — the [`LocalReasoner`] that drives the killable
//! `meetnotes-brain` child process over the NDJSON protocol in `brain_ipc.rs`.
//!
//! ## Why a child process (the whole point)
//! mistralrs holds a multi-GB model resident, has documented drop-leaks (#723/#865) so the old
//! in-process code could NEVER evict, and its generation is not cancellable. Running it in a child
//! turns all three into non-problems: the host reclaims ALL of the child's RAM by simply killing it
//! (idle-unload / per-request timeout / app-quit), and a hung generation is cancelled by SIGKILL.
//! This module owns the child's LIFECYCLE — spawn-on-first-use, a persistent resident child across
//! many requests, host-authoritative idle-kill, per-request timeout = KILL+RESPAWN, and a crash/EOF
//! reap+respawn — and never blocks the app or aborts on a sidecar failure (every failure degrades to
//! the deterministic floor / Cloud via a mapped `AppError`).
//!
//! ## The wire types are the CHILD's — included, not copied
//! `brain_ipc.rs` is `#[path]`-included below so a wire-format drift between host and child is a
//! COMPILE error, not a runtime desync. It is FROZEN — this module never edits it.
//!
//! ## The three lifecycle BLOCKERS this design solves
//! 1. **PERSISTENT child** (NOT afm's per-call `wait_with_output`): the `ChildStdin` + a persistent
//!    reader thread over `ChildStdout` live in [`SidecarState`] across requests; the child stays alive
//!    between calls and is `wait()`ed only after an explicit `kill()`.
//! 2. **PIPE-DEADLOCK avoidance**: note-gen writes the WHOLE transcript (can exceed the 64 KB pipe
//!    buffer) and the response can too. The request is pushed on a dedicated scoped WRITER thread while
//!    a PERSISTENT reader thread drains `ChildMsg` lines into an mpsc channel the dispatcher
//!    `recv_timeout`s on — so neither side ever blocks on a full pipe, and the deadline is honored
//!    WITHOUT the dispatcher ever blocking in a raw pipe read.
//! 3. **Per-request timeout = KILL+RESPAWN** (replaces the old leaked-worker-on-timeout): on the
//!    deadline the child is `kill()+wait()`ed — TRUE cancellation + full RAM reclaim — and the next
//!    call respawns. The persistent reader thread ends when the killed child's pipe closes (there is
//!    only ever ONE reader thread per child, reaped by the pipe EOF — never an accumulating leak).
//!
//! ## Privacy / hardening (audited by the lock-security reviewer)
//! The child is spawned with `env_clear()` + a fixed PATH (only HOME/USER/LOGNAME/TMPDIR re-added),
//! so NO DEK/KEK/token/`MURMUR_DEV_*` can reach it (mirrors `afm::hardened_command`). The
//! transcript/prompt rides ONLY the child's stdin PIPE — never argv, never a temp file, never a log
//! line. The only argv is the GGUF model PATH (not personal content) + the idle-timeout number.
//! Logs carry `target: "reason"` + stages/kinds/durations only, never prompt/response content.
//!
//! ## Honest scope
//! Everything here is COMPILE + fake-child-tested headless. Real inference, real RAM-actually-returns
//! on kill, and the real child's model load only run on a Mac with a GGUF present — a green
//! `cargo test --lib` proves the lifecycle plumbing, NOT that on-device inference works.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{AppError, Result};
use crate::reason::{parse_first_json, GenOptions, LocalReasoner};

/// The FROZEN NDJSON wire protocol, shared verbatim with the child (`crates/murmur-brain`). Included
/// (not re-declared) so any drift is a compile error. Three `../` from `src-tauri/src/reason/` reach
/// the workspace root.
#[path = "../../../crates/murmur-brain/src/brain_ipc.rs"]
mod brain_ipc;

use brain_ipc::{ChildMsg, ErrorKind, GenOptsWire, HostMsg};

/// Filename of the child binary — inside `Contents/Resources` of a shipped `.app` and staged at
/// `binaries/` for dev (build.rs emits `BRAIN_BIN`).
const SIDECAR_NAME: &str = "meetnotes-brain";

/// Default host-authoritative idle window (s): after this long with no request AND nothing in
/// flight, the host kills the child to reclaim its model RAM. Overridable per-instance from config.
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 300;

/// Default bounded wait (s) for the child's `Ready` handshake after spawn. A model load on a cold
/// disk / first Metal shader compile can be slow, so this is generous; on timeout the child is
/// killed and the caller degrades — the app never blocks forever.
const DEFAULT_READY_TIMEOUT_SECS: u64 = 90;

/// Default HARD wall-clock cap (s) on ONE generation when the caller's `GenOptions.timeout` is
/// `None` (note-gen / the FullyLocal Ask floor). Unlike the old in-process path — which ran those
/// UNBOUNDED — a child can wedge and hold model RAM forever, so we cap it (generously) to guarantee
/// the app can always reclaim the RAM. Deadline hit ⇒ KILL+RESPAWN.
const DEFAULT_HARD_CAP_SECS: u64 = 180;

/// Cooldown (s) after a ready-timeout / crash: while `failed_until` is in the future, dispatch
/// returns the degrade error IMMEDIATELY (no 90s-ready + respawn storm on a broken sidecar).
const BACKOFF_SECS: u64 = 30;

/// Bounded wait (s) for a killed child to reap during app-exit — a killed child reparents to launchd
/// and the startup reaper (`aec::reap_orphaned_capture_helpers`) cleans it, so we never block quit.
const QUIT_REAP_SECS: u64 = 2;

/// DEV/TEST-ONLY runtime override: an absolute path to a `meetnotes-brain`-compatible executable
/// (e.g. a fixture shell script speaking the NDJSON protocol). Checked FIRST in [`resolve_bin`], but
/// ONLY under `test`/`debug_assertions` — a signed RELEASE never reads it (mirrors the
/// `MURMUR_DEV_DEK`/`MURMUR_AFM_SIDECAR` precedent), so a release can't be pointed at a
/// bring-your-own child via the process env.
#[cfg(any(test, debug_assertions))]
const ENV_OVERRIDE: &str = "MURMUR_BRAIN_SIDECAR";

// ---------------------------------------------------------------------------------------------------
// RAM pre-check — MOVED from `reason/mistral.rs`. The host refuses-to-Cloud BEFORE spawning a child
// that would swap-death the machine; the child ALSO self-checks (belt), but the host check avoids
// even paying the spawn.
// ---------------------------------------------------------------------------------------------------

/// KV / activation / runtime headroom factor applied to a GGUF's on-disk (weights) size to estimate
/// its true peak resident footprint. `1.5×` is DELIBERATELY CONSERVATIVE (low, so we do not
/// false-refuse a healthy machine). Integer-scaled as `× 3 / 2` to stay in `u64`.
const MODEL_RAM_HEADROOM_NUM: u64 = 3;
const MODEL_RAM_HEADROOM_DEN: u64 = 2;

/// Only guard loads for models at least this large on disk. Tiny GGUFs never move the needle on a
/// memory-pressure kill, so a small model always loads (fail-open by size).
const MODEL_RAM_GUARD_MIN_DISK_BYTES: u64 = 1_500_000_000; // ~1.5 GB

/// Decide whether there is enough FREE system RAM to load a model of `model_disk_bytes`. PURE +
/// injectable (no OS probe here) so it is unit-testable.
///
/// - `free_bytes = None` ⇒ the OS probe FAILED — FAIL OPEN (return `true`).
/// - a small model (`< MODEL_RAM_GUARD_MIN_DISK_BYTES`) ⇒ always `true`.
/// - otherwise ⇒ `true` iff `free_bytes >= model_disk_bytes × headroom`.
fn ram_permits_load(free_bytes: Option<u64>, model_disk_bytes: u64) -> bool {
    if model_disk_bytes < MODEL_RAM_GUARD_MIN_DISK_BYTES {
        return true; // tiny model — never the OOM driver; don't risk a false refuse.
    }
    let Some(free) = free_bytes else {
        return true; // probe failed → fail OPEN (never break a working machine on a broken probe).
    };
    let needed =
        model_disk_bytes.saturating_mul(MODEL_RAM_HEADROOM_NUM) / MODEL_RAM_HEADROOM_DEN;
    free >= needed
}

/// Best-effort AVAILABLE (free + reclaimable) system RAM in bytes, macOS, via `vm_stat` (no new
/// crate/FFI). Sums the page classes macOS can hand to a new allocation WITHOUT swapping. Returns
/// `None` on ANY parse/exec failure so the caller FAILS OPEN.
fn available_ram_bytes() -> Option<u64> {
    let out = std::process::Command::new("vm_stat").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let page_size = text
        .lines()
        .next()
        .and_then(|l| l.split("page size of ").nth(1))
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(4096);
    let pages = |label: &str| -> u64 {
        text.lines()
            .find(|l| l.trim_start().starts_with(label))
            .and_then(|l| l.rsplit(':').next())
            .map(|v| v.trim().trim_end_matches('.').replace(',', ""))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    };
    let free = pages("Pages free");
    let inactive = pages("Pages inactive");
    let speculative = pages("Pages speculative");
    let purgeable = pages("Pages purgeable");
    let avail_pages = free
        .saturating_add(inactive)
        .saturating_add(speculative)
        .saturating_add(purgeable);
    if avail_pages == 0 {
        return None; // couldn't read any class → probe failure (fail open).
    }
    Some(avail_pages.saturating_mul(page_size))
}

/// The on-disk size (bytes) of the GGUF at `path`, or `None` if it can't be stat'd — a stat failure
/// yields `None` ⇒ the guard fails OPEN.
fn model_disk_bytes(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

// ---------------------------------------------------------------------------------------------------
// Timeout config — the 3 additive `AppConfig` values threaded through, with a captured global
// snapshot so `SidecarReasoner::new` (built deep in the reasoner resolution) can read them without
// plumbing config through every call site.
// ---------------------------------------------------------------------------------------------------

/// The per-instance timeout policy, snapshotted from `AppConfig` at reasoner-construction time.
#[derive(Debug, Clone, Copy)]
pub struct SidecarTimeouts {
    /// Host-authoritative idle-kill window (s).
    pub idle_secs: u64,
    /// Bounded `Ready`-handshake wait (s) after spawn.
    pub ready_secs: u64,
    /// Hard wall-clock cap (s) for a generation whose `GenOptions.timeout` is `None`.
    pub hard_cap_secs: u64,
}

impl Default for SidecarTimeouts {
    fn default() -> Self {
        Self {
            idle_secs: DEFAULT_IDLE_TIMEOUT_SECS,
            ready_secs: DEFAULT_READY_TIMEOUT_SECS,
            hard_cap_secs: DEFAULT_HARD_CAP_SECS,
        }
    }
}

/// Process-wide snapshot of the timeout policy, set once from config at reasoner construction and
/// read by the process-global dispatcher (the single resident child is process-global, so its
/// idle/ready/hard-cap policy is too). `Local`-backend resolution rebuilds the reasoner when the
/// model path changes, so this stays current with the config the resolver last saw.
fn timeouts_snapshot() -> &'static Mutex<SidecarTimeouts> {
    static SNAP: OnceLock<Mutex<SidecarTimeouts>> = OnceLock::new();
    SNAP.get_or_init(|| Mutex::new(SidecarTimeouts::default()))
}

fn set_timeouts(t: SidecarTimeouts) {
    if let Ok(mut g) = timeouts_snapshot().lock() {
        *g = t;
    }
}

/// Refresh the process-global timeout snapshot from OUTSIDE a `SidecarReasoner::new` build. The live
/// dispatch caches the loaded `SidecarReasoner` keyed by GGUF path (see `ReasonerCell::local_cached`);
/// on a cache HIT the constructor — the only other `set_timeouts` caller — is skipped, so without this
/// a Settings change to the 3 brain timeouts would not take effect until the model path changed. The
/// cache-hit path calls this so a timeout-only config change applies on the NEXT dispatch.
pub fn apply_timeouts(t: SidecarTimeouts) {
    set_timeouts(t);
}

fn current_timeouts() -> SidecarTimeouts {
    timeouts_snapshot().lock().map(|g| *g).unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------------
// The process-global resident child + single-flight dispatch mutex.
// ---------------------------------------------------------------------------------------------------

/// The live child + its stdin + the persistent-reader channel + bookkeeping. Guarded by ONE
/// process-global mutex ([`sidecar`]) that serializes BOTH spawn and generation — so there is never a
/// double-spawn race and never two concurrent generations against the one-request-at-a-time child.
struct SidecarState {
    /// The resident child, `None` when not yet spawned / after a kill+clear.
    child: Option<Child>,
    /// The child's stdin (we push `HostMsg::Generate` lines here).
    stdin: Option<ChildStdin>,
    /// The receiving end of the PERSISTENT reader thread's channel — every `ChildMsg` the child emits
    /// (parsed off stdout) arrives here. The dispatcher `recv_timeout`s on it, so the deadline is
    /// honored WITHOUT the dispatcher ever blocking in a raw pipe read. `None` = no live reader.
    rx: Option<Receiver<ReaderEvent>>,
    /// The GGUF path this child was spawned for. A change means "kill + respawn for the new model".
    model_path: PathBuf,
    /// Last successful activity — drives the host-authoritative idle-kill.
    last_used: Instant,
    /// Cooldown: while `Some(t)` and `t` is in the future, dispatch degrades immediately (no respawn
    /// storm after a ready-timeout / crash).
    failed_until: Option<Instant>,
}

/// What the persistent reader thread hands the dispatcher: a parsed protocol message, or EOF (the
/// child closed stdout / died). An unparseable line is dropped by the reader (the child's stdout is
/// pure NDJSON; a defensive skip keeps one bad line from wedging the channel).
enum ReaderEvent {
    Msg(ChildMsg),
    Eof,
}

impl SidecarState {
    fn empty() -> Self {
        Self {
            child: None,
            stdin: None,
            rx: None,
            model_path: PathBuf::new(),
            last_used: Instant::now(),
            failed_until: None,
        }
    }

    /// True iff a live child is resident with usable stdin + reader channel.
    fn is_live(&self) -> bool {
        self.child.is_some() && self.stdin.is_some() && self.rx.is_some()
    }

    /// KILL the resident child (if any) and WAIT to reap it, then clear all handles. SIGKILL is fine —
    /// the child has nothing to flush (its output is throwaway NDJSON, no file). EVERY kill is paired
    /// with a `wait()` so we never leave a zombie. Also used when the child self-exited (EOF): the
    /// `wait()` reaps the already-dead process before we clear. Dropping `stdin` + `rx` closes the
    /// child's stdin (read-end EOF) and lets the persistent reader thread END on its own stdout EOF —
    /// there is only ever ONE reader thread per child, so nothing accumulates.
    fn kill_and_clear(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait(); // reap — no zombie.
        }
        self.stdin = None;
        self.rx = None; // dropping the receiver lets the reader thread's send fail → it ends.
    }
}

/// The single process-global dispatch mutex. `OnceLock<Mutex<..>>` mirrors `model_cache()`'s shape.
fn sidecar() -> &'static Mutex<SidecarState> {
    static STATE: OnceLock<Mutex<SidecarState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(SidecarState::empty()))
}

// ---------------------------------------------------------------------------------------------------
// Binary resolution + hardened spawn (mirrors afm.rs).
// ---------------------------------------------------------------------------------------------------

/// Resolve the `meetnotes-brain` child binary, or `None`. Order (each filtered by `.exists()`):
/// 1. `MURMUR_BRAIN_SIDECAR` runtime override (dev/test — the fake-child fixture path);
/// 2. `<current_exe dir>/../Resources/<name>` — the shipped `.app` layout;
/// 3. the compile-time `BRAIN_BIN` (`build.rs`) DEV fallback (the staged `binaries/<name>`).
///
/// NEVER panics; a missing binary is `None`, which the caller maps to `Unavailable` (degrade).
fn resolve_bin() -> Option<PathBuf> {
    #[cfg(any(test, debug_assertions))]
    if let Ok(p) = std::env::var(ENV_OVERRIDE) {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("..").join("Resources").join(SIDECAR_NAME);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    option_env!("BRAIN_BIN")
        .map(PathBuf::from)
        .filter(|p| p.exists())
}

/// Build a hardened [`Command`] for the child: `env_clear()` + a fixed PATH, with only
/// HOME/USER/LOGNAME/TMPDIR passed through. Mirrors `afm::hardened_command` so no DEK/KEK/token/
/// `MURMUR_DEV_*` can be inherited by the child.
fn hardened_command(bin: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env_clear().env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    for key in ["HOME", "USER", "LOGNAME", "TMPDIR"] {
        if let Ok(val) = std::env::var(key) {
            cmd.env(key, val);
        }
    }
    cmd
}

/// Map a child [`ErrorKind`] to the host [`AppError`]. Every mapping degrades the brain call
/// gracefully (the caller floors); an on-device brain failure NEVER aborts the app.
fn map_error_kind(kind: ErrorKind, message: String) -> AppError {
    match kind {
        // Model unavailable / could-not-complete / OOM ⇒ degrade to the floor / Cloud.
        ErrorKind::Unavailable | ErrorKind::Oom => AppError::Unavailable(message),
        ErrorKind::Summarize => AppError::Summarize(message),
        ErrorKind::InvalidArg => AppError::InvalidArg(message),
    }
}

/// Spawn the persistent reader thread: it blocks on `read_line` over the child's stdout, parses each
/// NDJSON line into a [`ChildMsg`], and sends it down `tx`. On EOF (the child closed stdout / died)
/// it sends [`ReaderEvent::Eof`] and returns. A send failure (the dispatcher dropped `rx` on a kill)
/// also ends it. There is exactly ONE reader thread per child — reaped by the stdout EOF the kill
/// produces — so nothing accumulates across respawns.
fn spawn_reader_thread(stdout: ChildStdout, tx: mpsc::Sender<ReaderEvent>) {
    std::thread::Builder::new()
        .name("murmur-brain-reader".into())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        let _ = tx.send(ReaderEvent::Eof);
                        return;
                    }
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<ChildMsg>(trimmed) {
                            Ok(msg) => {
                                if tx.send(ReaderEvent::Msg(msg)).is_err() {
                                    return; // the dispatcher dropped rx (kill) — stop reading.
                                }
                            }
                            Err(_) => {
                                // A malformed line (should never happen — child stdout is pure NDJSON).
                                // Skip it; no PII logged (no content).
                                tracing::warn!(target: "reason", "skipping unparseable brain sidecar line");
                            }
                        }
                    }
                    Err(_) => {
                        let _ = tx.send(ReaderEvent::Eof);
                        return;
                    }
                }
            }
        })
        .ok(); // a reader-thread spawn failure surfaces as a ready-timeout below (nothing ever arrives).
}

// ---------------------------------------------------------------------------------------------------
// Spawn + ready-handshake.
// ---------------------------------------------------------------------------------------------------

/// Spawn the child for `model_path`, start its persistent reader thread, and block (bounded by
/// `ready_secs`) for its `Ready` handshake over the channel. On success the child + stdin + rx are
/// installed into `state` and `last_used` reset. On ANY failure (no binary / spawn error /
/// ready-timeout / child error line / EOF) the child is killed+reaped, `failed_until` is set
/// (backoff), and an `Err` is returned so the caller degrades. NEVER panics, NEVER blocks forever.
fn spawn_and_wait_ready(state: &mut SidecarState, model_path: &Path, ready_secs: u64) -> Result<()> {
    let bin = resolve_bin()
        .ok_or_else(|| AppError::Unavailable("on-device brain sidecar binary not found".into()))?;

    let idle = current_timeouts().idle_secs;
    let mut cmd = hardened_command(&bin);
    // ARGV: the model PATH (not PII) + the child's idle-self-exit belt. The transcript NEVER touches
    // argv — it rides stdin only.
    cmd.arg("--model")
        .arg(model_path)
        .arg("--max-idle-seconds")
        .arg(idle.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::Unavailable(format!("brain sidecar spawn failed: {e}")))?;

    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(AppError::Unavailable("brain sidecar pipes unavailable".into()));
    };

    let (tx, rx) = mpsc::channel::<ReaderEvent>();
    spawn_reader_thread(stdout, tx);

    // Bounded wait for the FIRST ChildMsg (must be `Ready`). A model load on a cold disk / first
    // Metal shader compile can be slow, hence the generous `ready_secs`; a wedged/failed child is
    // killed at the deadline so the caller degrades instead of hanging forever.
    let deadline = Instant::now() + Duration::from_secs(ready_secs);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = child.kill();
            let _ = child.wait();
            state.failed_until = Some(Instant::now() + Duration::from_secs(BACKOFF_SECS));
            tracing::warn!(target: "reason", ready_s = ready_secs, "brain sidecar ready-handshake timed out");
            return Err(AppError::Unavailable(
                "brain sidecar failed to become ready".into(),
            ));
        }
        match rx.recv_timeout(remaining) {
            Ok(ReaderEvent::Msg(ChildMsg::Ready { .. })) => {
                state.child = Some(child);
                state.stdin = Some(stdin);
                state.rx = Some(rx);
                state.model_path = model_path.to_path_buf();
                state.last_used = Instant::now();
                state.failed_until = None;
                tracing::info!(target: "reason", "brain sidecar ready");
                return Ok(());
            }
            // The child reported a load error (bad path / OOM refusal). Map it, kill+reap, back off.
            Ok(ReaderEvent::Msg(ChildMsg::Error { kind, message, .. })) => {
                let _ = child.kill();
                let _ = child.wait();
                state.failed_until = Some(Instant::now() + Duration::from_secs(BACKOFF_SECS));
                tracing::warn!(target: "reason", kind = ?kind, "brain sidecar reported an error before ready");
                return Err(map_error_kind(kind, message));
            }
            // Any pre-ready non-Ready/non-Error line (a stray heartbeat/done) — keep waiting.
            Ok(ReaderEvent::Msg(_)) => continue,
            // EOF: the child exited before Ready (crash / non-zero exit). Reap, back off, degrade.
            Ok(ReaderEvent::Eof) => {
                let _ = child.wait();
                state.failed_until = Some(Instant::now() + Duration::from_secs(BACKOFF_SECS));
                return Err(AppError::Unavailable(
                    "brain sidecar failed to become ready".into(),
                ));
            }
            // Deadline elapsed with no message — kill+reap, back off, degrade.
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                state.failed_until = Some(Instant::now() + Duration::from_secs(BACKOFF_SECS));
                tracing::warn!(target: "reason", ready_s = ready_secs, "brain sidecar ready-handshake timed out");
                return Err(AppError::Unavailable(
                    "brain sidecar failed to become ready".into(),
                ));
            }
            // The reader thread hung up (spawn failed / it ended) — treat as a failed spawn.
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                let _ = child.wait();
                state.failed_until = Some(Instant::now() + Duration::from_secs(BACKOFF_SECS));
                return Err(AppError::Unavailable(
                    "brain sidecar failed to become ready".into(),
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// Dispatch — the single-flight generation path.
// ---------------------------------------------------------------------------------------------------

/// Monotonic request-id source. The child echoes the id on every reply; we only need it to be
/// non-repeating within a run so stray lines can't be mismatched.
fn next_request_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Serialize a [`GenOptions`] into the wire [`GenOptsWire`] (only the sampler fields; the wall-clock
/// timeout is enforced HOST-side by killing the child, never sent).
fn to_wire(opts: &GenOptions) -> GenOptsWire {
    GenOptsWire {
        max_tokens: opts.max_tokens,
        temperature: opts.temperature,
        enable_thinking: opts.enable_thinking,
        use_grammar_constraint: opts.use_grammar_constraint,
    }
}

/// The terminal outcome of the dispatch read loop.
enum Outcome {
    Done(String),
    ChildError(ErrorKind, String),
    Eof,
    Timeout,
}

/// The core dispatch: run one generation against the resident child, spawning it lazily and enforcing
/// idle-kill, backoff, the per-request deadline (= KILL+RESPAWN), and crash/EOF reap+respawn. Returns
/// the WHOLE-string result. On ANY failure returns a mapped `AppError` so the caller degrades.
///
/// PIPE-DEADLOCK AVOIDANCE: the `HostMsg::Generate` line (which can carry a >64 KB transcript) is
/// pushed on a dedicated scoped WRITER thread while the PERSISTENT reader thread drains `ChildMsg`
/// lines into the channel THIS thread `recv_timeout`s on — so neither side ever blocks on a full OS
/// pipe buffer, and the dispatcher never blocks in a raw pipe read (the deadline is always honored).
///
/// TIMEOUT = KILL+RESPAWN: the deadline is `opts.timeout` (or the `hard_cap_secs` when `None`). On the
/// deadline the child is `kill()+wait()`ed (TRUE cancellation + full RAM reclaim) and the exact
/// message `"on-device brain generation timed out"` returned (KEPT verbatim so existing callers/tests
/// are unchanged). The next call respawns.
fn dispatch(
    model_path: &Path,
    system: &str,
    user: &str,
    opts: GenOptions,
    json_schema: Option<&Value>,
) -> Result<String> {
    let t = current_timeouts();
    let mut state = sidecar()
        .lock()
        .map_err(|_| AppError::Summarize("brain sidecar mutex poisoned".into()))?;

    // Backoff: a recent ready-timeout / crash short-circuits to degrade (no 90s-ready storm).
    if let Some(until) = state.failed_until {
        if Instant::now() < until {
            return Err(AppError::Unavailable(
                "on-device brain sidecar is cooling down after a failure".into(),
            ));
        }
    }

    // Host-authoritative idle-kill: if the resident child has been idle past the window (and nothing
    // is in flight — we hold the single dispatch lock, so nothing is), reclaim its RAM. A fresh
    // request then respawns below.
    if state.is_live()
        && Instant::now().duration_since(state.last_used) > Duration::from_secs(t.idle_secs)
    {
        tracing::info!(target: "reason", idle_s = t.idle_secs, "idle-killing brain sidecar to reclaim RAM");
        state.kill_and_clear();
    }

    // Model changed under us (user selected a different GGUF) ⇒ kill the old resident child.
    if state.is_live() && state.model_path != model_path {
        tracing::info!(target: "reason", "brain model changed; respawning sidecar");
        state.kill_and_clear();
    }

    // Spawn lazily on first use / after any kill. The RAM pre-check refuses-to-Cloud BEFORE paying a
    // spawn that would swap-death the machine (the child self-checks too; this avoids the fork).
    if !state.is_live() {
        if let Some(bytes) = model_disk_bytes(model_path) {
            if !ram_permits_load(available_ram_bytes(), bytes) {
                let gb = bytes as f64 / 1_073_741_824.0;
                tracing::warn!(
                    target: "reason",
                    model_gb = format_args!("{gb:.1}"),
                    "refusing to spawn brain sidecar: insufficient free memory"
                );
                return Err(AppError::Unavailable(format!(
                    "not enough free memory to load this on-device model ({gb:.1} GB) — \
                     switch the brain to Cloud in Settings or pick a smaller model"
                )));
            }
        }
        spawn_and_wait_ready(&mut state, model_path, t.ready_secs)?;
    }

    // The per-request deadline: the caller's wall-clock bound, else the hard cap so a wedged child
    // can never hold the app's model RAM forever.
    let budget = opts
        .timeout
        .unwrap_or_else(|| Duration::from_secs(t.hard_cap_secs));
    let id = next_request_id();
    let req = HostMsg::Generate {
        id,
        system: system.to_string(),
        user: user.to_string(),
        opts: to_wire(&opts),
        json_schema: json_schema.cloned(),
    };
    let line = match serde_json::to_string(&req) {
        Ok(l) => l,
        Err(e) => return Err(AppError::Summarize(format!("brain request serialize: {e}"))),
    };

    // Take stdin OUT for the scoped writer to own; the reader channel stays in `state`.
    let mut stdin = match state.stdin.take() {
        Some(s) => s,
        None => return Err(AppError::Summarize("brain sidecar stdin missing".into())),
    };
    let rx = match state.rx.as_ref() {
        Some(r) => r,
        None => {
            state.stdin = Some(stdin);
            return Err(AppError::Summarize("brain sidecar channel missing".into()));
        }
    };

    let deadline = Instant::now() + budget;

    // WRITER on a scoped thread: push the (possibly huge) request line + newline, then flush. A
    // BrokenPipe (child already died) is ignored — the reader/deadline below owns the outcome.
    // Meanwhile THIS thread reads replies from the channel, so a >64 KB request never deadlocks
    // against a >64 KB response.
    let (outcome, recovered_stdin) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            let _ = stdin.write_all(line.as_bytes());
            let _ = stdin.write_all(b"\n");
            let _ = stdin.flush();
            stdin // hand it back so the caller can keep the resident pipe
        });

        // Read replies for THIS id until Done / Error / EOF / deadline.
        let outcome = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break Outcome::Timeout;
            }
            match rx.recv_timeout(remaining) {
                Ok(ReaderEvent::Msg(ChildMsg::Done { id: rid, text })) if rid == id => {
                    break Outcome::Done(text);
                }
                Ok(ReaderEvent::Msg(ChildMsg::Error {
                    id: rid,
                    kind,
                    message,
                })) if rid == id => {
                    break Outcome::ChildError(kind, message);
                }
                // A heartbeat for OUR id: liveness. The `deadline` is the hard ceiling (the caller's
                // budget / hard cap), so a productive child that keeps beating simply continues until
                // it Dones — we do not extend PAST the ceiling (a wedged child that somehow beats can't
                // outlast the cap). Non-matching lines are stray late replies from a prior request.
                Ok(ReaderEvent::Msg(_)) => continue,
                Ok(ReaderEvent::Eof) => break Outcome::Eof,
                Err(mpsc::RecvTimeoutError::Timeout) => break Outcome::Timeout,
                // The reader thread ended (child gone) — treat as EOF.
                Err(mpsc::RecvTimeoutError::Disconnected) => break Outcome::Eof,
            }
        };

        // Join the writer to recover stdin. It NEVER blocks indefinitely: either it finished the
        // write, or the child died and its stdin read-end closed (BrokenPipe → the write returns).
        let recovered = writer.join().ok();
        (outcome, recovered)
    });

    // Resolve the outcome, re-installing the resident pipes ONLY when the child is still healthy.
    match outcome {
        Outcome::Done(text) => {
            state.stdin = recovered_stdin;
            state.last_used = Instant::now();
            state.failed_until = None;
            if json_schema.is_some() {
                // Structured: recover the object from the (possibly noisy) text via the SAME robust
                // extractor the old in-process + cloud paths use. A parse failure is a real error.
                parse_first_json::<Value>(&text)?;
            }
            Ok(text)
        }
        Outcome::ChildError(kind, message) => {
            // A generation error, but the child is still alive — keep it resident, degrade this call.
            state.stdin = recovered_stdin;
            state.last_used = Instant::now();
            Err(map_error_kind(kind, message))
        }
        Outcome::Eof => {
            // The child died mid-request. Reap the process, clear, degrade.
            state.stdin = recovered_stdin; // put it back so kill_and_clear's Drop closes it cleanly
            state.kill_and_clear();
            Err(AppError::Summarize("brain sidecar exited".into()))
        }
        Outcome::Timeout => {
            // TRUE cancellation: KILL+WAIT reclaims ALL model RAM and stops the wedged decode. The
            // next call respawns. KEEP this exact message (existing callers/tests match it).
            state.stdin = recovered_stdin;
            state.kill_and_clear();
            tracing::warn!(target: "reason", budget_s = budget.as_secs(), "brain sidecar generation timed out; killed + will respawn");
            Err(AppError::Unavailable(
                "on-device brain generation timed out".into(),
            ))
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// The public reasoner + app-exit hook.
// ---------------------------------------------------------------------------------------------------

/// A [`LocalReasoner`] that drives the on-device brain through the killable `meetnotes-brain` child.
/// Holds only the resolved GGUF path + an id (the child + RAM live in the process-global
/// [`sidecar`]), so it is CHEAP to build per resolution — mirroring how the old `MistralReasoner` held
/// only paths. All the lifecycle (spawn / idle-kill / timeout-kill / respawn) is process-global.
pub struct SidecarReasoner {
    /// Stable id, e.g. `sidecar:Qwen_Qwen3-4B-Instruct-2507-Q4_K_M.gguf`.
    id: String,
    /// Full GGUF path the child is (or will be) spawned for.
    model_path: PathBuf,
}

impl SidecarReasoner {
    /// Build a reasoner for the GGUF at `model_path`, snapshotting the timeout policy from config.
    /// CHEAP + non-blocking: no child is spawned until the first `reason`/`structured` call. Returns
    /// `Err` (never panics) if the path has no filename.
    pub fn new(model_path: PathBuf, timeouts: SidecarTimeouts) -> Result<Self> {
        let file = model_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| AppError::Summarize("brain model path has no filename".into()))?;
        // Snapshot the timeout policy for the process-global dispatcher (the resident child is
        // process-global, so its policy is too). The Local-backend resolver rebuilds this reasoner
        // when the model path changes, keeping the snapshot current with config.
        set_timeouts(timeouts);
        Ok(Self {
            id: format!("sidecar:{file}"),
            model_path,
        })
    }

    fn reason_inner(&self, system: &str, user: &str, opts: GenOptions) -> Result<String> {
        dispatch(&self.model_path, system, user, opts, None)
    }

    fn structured_inner(
        &self,
        system: &str,
        user: &str,
        json_schema: &Value,
        opts: GenOptions,
    ) -> Result<Value> {
        // The child instructs the schema in the prompt (matching the old in-process path) and, when
        // opted-in with a tiny schema, tries a grammar-constrained decode with a graceful fallback —
        // ALL host-transparent. We pass the schema so the child prompts on it, and recover the object
        // here via the SAME robust extractor the old path used.
        let text = dispatch(&self.model_path, system, user, opts, Some(json_schema))?;
        parse_first_json(&text)
    }
}

impl LocalReasoner for SidecarReasoner {
    fn id(&self) -> &str {
        &self.id
    }

    fn reason(&self, system: &str, user: &str) -> Result<String> {
        self.reason_inner(system, user, GenOptions::default())
    }

    fn reason_with(&self, system: &str, user: &str, opts: GenOptions) -> Result<String> {
        self.reason_inner(system, user, opts)
    }

    fn structured(&self, system: &str, user: &str, json_schema: &Value) -> Result<Value> {
        self.structured_inner(system, user, json_schema, GenOptions::default())
    }

    fn structured_with(
        &self,
        system: &str,
        user: &str,
        json_schema: &Value,
        opts: GenOptions,
    ) -> Result<Value> {
        self.structured_inner(system, user, json_schema, opts)
    }
}

/// App-exit hook (wired from `lib.rs` `RunEvent::ExitRequested`): KILL the resident child + a BOUNDED
/// reap so app-exit is never blocked. A killed child reparents to launchd and the startup reaper
/// (`aec::reap_orphaned_capture_helpers`) SIGTERMs it next launch, so a slow reap here is safe to
/// abandon. Best-effort + panic-free.
pub fn kill_on_quit() {
    let Ok(mut state) = sidecar().lock() else {
        return; // poisoned — the OS reaps the child on process teardown anyway.
    };
    if let Some(mut child) = state.child.take() {
        let _ = child.kill();
        // BOUNDED try_wait loop — never block quit indefinitely.
        let deadline = Instant::now() + Duration::from_secs(QUIT_REAP_SECS);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break, // reaped
                Ok(None) if Instant::now() >= deadline => {
                    tracing::warn!(target: "reason", "brain sidecar did not reap within the quit budget; leaving to the startup reaper");
                    break;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }
    }
    state.stdin = None;
    state.rx = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_073_741_824;

    // ---- RAM pre-check (MOVED verbatim from mistral.rs) ---------------------------------------

    /// The pure RAM decision: a healthy machine permits a big model; a pressured one refuses it.
    #[test]
    fn ram_permits_load_ok_when_free_and_refuses_under_pressure() {
        let big_model = 6 * GB + GB / 2; // ~6.3 GB heavy model (Bielik-class)
        assert!(
            ram_permits_load(Some(24 * GB), big_model),
            "a healthy machine with 24 GB free must permit a 6.3 GB model"
        );
        assert!(
            !ram_permits_load(Some(4 * GB), big_model),
            "4 GB free must REFUSE a 6.3 GB model (would swap-death the machine)"
        );
        let needed = big_model * MODEL_RAM_HEADROOM_NUM / MODEL_RAM_HEADROOM_DEN;
        assert!(ram_permits_load(Some(needed), big_model));
        assert!(!ram_permits_load(Some(needed - 1), big_model));
    }

    /// FAIL OPEN: a failed RAM probe (`None`) must NEVER block a load.
    #[test]
    fn ram_permits_load_fails_open_on_broken_probe() {
        assert!(
            ram_permits_load(None, 9 * GB),
            "a broken RAM probe must fail OPEN (never break a working setup)"
        );
    }

    /// A SMALL model is never guarded — it loads even under pressure.
    #[test]
    fn ram_permits_load_small_model_always_ok() {
        let tiny = MODEL_RAM_GUARD_MIN_DISK_BYTES - 1;
        assert!(ram_permits_load(Some(100 * 1024 * 1024), tiny));
        assert!(!ram_permits_load(
            Some(1024 * 1024 * 1024),
            MODEL_RAM_GUARD_MIN_DISK_BYTES + GB
        ));
    }

    /// The RAM probe is crash-safe: `Some(>0)` or `None`, never a panic, never `Some(0)`.
    #[test]
    fn available_ram_probe_is_crash_safe() {
        if let Some(b) = available_ram_bytes() {
            assert!(b > 0, "a Some result must be a positive byte count");
        }
    }

    /// The wire mapping: every child ErrorKind lands on a degrade-able AppError variant.
    #[test]
    fn error_kind_maps_to_degradeable_apperror() {
        assert!(matches!(
            map_error_kind(ErrorKind::Unavailable, "x".into()),
            AppError::Unavailable(_)
        ));
        assert!(matches!(
            map_error_kind(ErrorKind::Oom, "x".into()),
            AppError::Unavailable(_)
        ));
        assert!(matches!(
            map_error_kind(ErrorKind::Summarize, "x".into()),
            AppError::Summarize(_)
        ));
        assert!(matches!(
            map_error_kind(ErrorKind::InvalidArg, "x".into()),
            AppError::InvalidArg(_)
        ));
    }

    // ---- fake-child fixture harness (a shell script speaking the NDJSON protocol) --------------

    /// Reset the process-global sidecar state + timeouts between tests (these tests share the ONE
    /// process-global child, so each must start clean and hold a test-serialization lock).
    fn reset_state() {
        if let Ok(mut s) = sidecar().lock() {
            s.kill_and_clear();
            s.failed_until = None;
            s.model_path = PathBuf::new();
            s.last_used = Instant::now();
        }
    }

    /// Serialize the fake-child tests: they share the ONE process-global resident child + the
    /// `MURMUR_BRAIN_SIDECAR` override, so they must not run concurrently.
    static SIDECAR_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Write an executable fake-child shell script to a temp path and point `MURMUR_BRAIN_SIDECAR`
    /// at it. `body` is the script AFTER the shebang. Returns the path (cleaned up by the caller).
    #[cfg(unix)]
    fn write_fake_child(tag: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-brain-fake-{tag}-{}-{}.sh",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let script = format!("#!/bin/sh\n{body}");
        std::fs::write(&p, script).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var(ENV_OVERRIDE, &p);
        p
    }

    /// A dummy GGUF path — small enough (a few bytes) that the RAM guard never refuses it, so the
    /// spawn always happens. The fake child ignores `--model` entirely.
    #[cfg(unix)]
    fn dummy_model() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-brain-fake-model-{}-{}.gguf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&p, b"GGUF").unwrap();
        p
    }

    #[cfg(unix)]
    fn cleanup(script: &Path, model: &Path) {
        std::env::remove_var(ENV_OVERRIDE);
        let _ = std::fs::remove_file(script);
        let _ = std::fs::remove_file(model);
        reset_state();
    }

    /// GENERATE → DONE happy path: a fake child that emits Ready then echoes a Done reply. Proves the
    /// spawn + ready-handshake + write-request + read-Done round-trip end to end. The fixture echoes
    /// the SAME id the host stamped, parsed out of the request line, so the reply always matches.
    #[cfg(unix)]
    #[test]
    fn generate_round_trips_ready_then_done() {
        let _g = SIDECAR_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_state();
        // Emit Ready, then for every stdin line parse its "id" and echo Done with that id.
        let script = write_fake_child("done", ECHO_DONE_BODY);
        let model = dummy_model();
        let r = SidecarReasoner::new(model.clone(), SidecarTimeouts::default()).unwrap();
        assert!(r.id().starts_with("sidecar:"));
        let got = r.reason("system prompt", "user prompt").unwrap();
        assert_eq!(got, "fake answer");
        cleanup(&script, &model);
    }

    /// READY-TIMEOUT → DEGRADE: a fake child that NEVER emits Ready (sleeps) must, under a tiny
    /// ready timeout, be killed and surface `Unavailable("brain sidecar failed to become ready")`.
    #[cfg(unix)]
    #[test]
    fn ready_timeout_degrades_and_kills() {
        let _g = SIDECAR_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_state();
        let script = write_fake_child("noready", "sleep 30\n");
        let model = dummy_model();
        let t = SidecarTimeouts {
            ready_secs: 1,
            ..SidecarTimeouts::default()
        };
        let r = SidecarReasoner::new(model.clone(), t).unwrap();
        let started = Instant::now();
        match r.reason("s", "u") {
            Err(AppError::Unavailable(msg)) => {
                assert!(msg.contains("failed to become ready"), "got {msg}");
            }
            other => panic!("expected ready-timeout Unavailable, got {other:?}"),
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the ready-timeout must bound the wait (killed at ~1s), not hang"
        );
        cleanup(&script, &model);
    }

    /// PER-REQUEST TIMEOUT → KILL → RESPAWN: a fake child that goes Ready but then NEVER answers a
    /// Generate (it reads + sleeps unless a marker exists) hits the per-request deadline; the host
    /// kills it and returns the EXACT `"on-device brain generation timed out"` message. A SECOND call
    /// then RESPAWNS a fresh child (which now sees the marker) and succeeds.
    #[cfg(unix)]
    #[test]
    fn per_request_timeout_kills_then_respawns() {
        let _g = SIDECAR_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_state();
        let marker = marker_path("timeout");
        let _ = std::fs::remove_file(&marker);
        let body = format!(
            r#"printf '{{"type":"ready","model_id":"fake"}}\n'
while IFS= read -r line; do
  ID=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -z "$ID" ] && ID=1
  if [ -f "{marker}" ]; then
    printf '{{"type":"done","id":%s,"text":"after respawn"}}\n' "$ID"
  else
    sleep 30
  fi
done
"#,
            marker = marker.display()
        );
        let script = write_fake_child("timeout-respawn", &body);
        let model = dummy_model();

        // 1st call: no marker → the child sleeps → per-request timeout → KILL, exact message.
        {
            let opts = GenOptions {
                timeout: Some(Duration::from_secs(1)),
                ..GenOptions::default()
            };
            let r = SidecarReasoner::new(model.clone(), SidecarTimeouts::default()).unwrap();
            match r.reason_with("s", "u", opts) {
                Err(AppError::Unavailable(msg)) => {
                    assert_eq!(msg, "on-device brain generation timed out");
                }
                other => panic!("expected the exact timeout message, got {other:?}"),
            }
        }
        // The child was killed; the resident slot is clear (respawn on next call).
        assert!(
            !sidecar().lock().unwrap().is_live(),
            "a per-request timeout must KILL the child (clear the resident slot)"
        );

        // The RESPAWNED fixture echoes id 1 regardless; create the marker so it answers immediately.
        std::fs::write(&marker, b"go").unwrap();
        {
            let r = SidecarReasoner::new(model.clone(), SidecarTimeouts::default()).unwrap();
            let got = r.reason("s", "u").unwrap();
            assert_eq!(
                got, "after respawn",
                "the 2nd call must respawn a fresh child and succeed"
            );
        }

        let _ = std::fs::remove_file(&marker);
        cleanup(&script, &model);
    }

    /// EOF / CRASH mid-request → REAP → RESPAWN: a fake child that goes Ready then EXITS on the first
    /// request (EOF mid-request) surfaces `Summarize("brain sidecar exited")`, reaps the process
    /// (no zombie, slot cleared), and a 2nd call respawns + succeeds.
    #[cfg(unix)]
    #[test]
    fn eof_mid_request_reaps_then_respawns() {
        let _g = SIDECAR_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_state();
        let marker = marker_path("eof");
        let _ = std::fs::remove_file(&marker);
        let body = format!(
            r#"printf '{{"type":"ready","model_id":"fake"}}\n'
while IFS= read -r line; do
  ID=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -z "$ID" ] && ID=1
  if [ -f "{marker}" ]; then
    printf '{{"type":"done","id":%s,"text":"after crash respawn"}}\n' "$ID"
  else
    exit 0
  fi
done
"#,
            marker = marker.display()
        );
        let script = write_fake_child("eof-respawn", &body);
        let model = dummy_model();

        {
            let r = SidecarReasoner::new(model.clone(), SidecarTimeouts::default()).unwrap();
            match r.reason("s", "u") {
                Err(AppError::Summarize(msg)) => assert_eq!(msg, "brain sidecar exited"),
                other => panic!("expected EOF Summarize, got {other:?}"),
            }
        }
        assert!(
            !sidecar().lock().unwrap().is_live(),
            "an EOF mid-request must reap + clear the resident slot"
        );

        std::fs::write(&marker, b"go").unwrap();
        {
            let r = SidecarReasoner::new(model.clone(), SidecarTimeouts::default()).unwrap();
            let got = r.reason("s", "u").unwrap();
            assert_eq!(got, "after crash respawn");
        }

        let _ = std::fs::remove_file(&marker);
        cleanup(&script, &model);
    }

    /// IDLE-KILL RECLAIMS: after a successful call, an idle window of 0s means the NEXT dispatch
    /// idle-kills the resident child before spawning a fresh one — proven by a DIFFERENT pid across
    /// the two calls (the resident child was reclaimed + respawned), both succeeding.
    #[cfg(unix)]
    #[test]
    fn idle_kill_reclaims_then_next_call_respawns() {
        let _g = SIDECAR_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_state();
        let script = write_fake_child("idle", ECHO_DONE_BODY);
        let model = dummy_model();
        let t = SidecarTimeouts {
            idle_secs: 0, // any elapsed time > 0s counts as idle
            ..SidecarTimeouts::default()
        };

        let r = SidecarReasoner::new(model.clone(), t).unwrap();
        assert_eq!(r.reason("s", "u").unwrap(), "fake answer");
        let pid1 = sidecar().lock().unwrap().child.as_ref().map(|c| c.id());
        assert!(pid1.is_some(), "a live child after the 1st call");

        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(r.reason("s", "u").unwrap(), "fake answer");
        let pid2 = sidecar().lock().unwrap().child.as_ref().map(|c| c.id());
        assert!(pid2.is_some());
        assert_ne!(
            pid1, pid2,
            "idle-kill must have reclaimed + respawned a fresh child"
        );

        cleanup(&script, &model);
    }

    /// PIPE-DEADLOCK PROOF (the BLOCKER test): a >128 KB PROMPT AND a >128 KB RESPONSE must NOT
    /// deadlock — and this test is RED-if-the-writer-thread-is-collapsed onto the dispatch thread.
    ///
    /// The bind: the fake child reads only a SMALL PREFIX of stdin (`head -c 4096`, enough to parse the
    /// early `"id":N`) and then emits a >128 KB `Done` WITHOUT draining the rest of the (>150 KB)
    /// request. So the OS pipes are in a mutual-fill state:
    ///   - the request (>150 KB) far exceeds the ~64 KB stdin pipe buffer, and the child stops reading
    ///     stdin after 4 KB — so a WRITER that writes the whole request before reading blocks once the
    ///     buffer fills;
    ///   - the response (>128 KB) far exceeds the ~64 KB stdout pipe buffer, and the child fills it —
    ///     so a host that is blocked writing (not reading stdout) can never drain it.
    ///
    /// A hypothetical single-threaded host that WROTE-then-READ would therefore DEADLOCK here (writer
    /// blocked on a full stdin pipe, child blocked on a full stdout pipe, neither draining the other).
    /// The shipped design — a scoped WRITER thread doing the blocking write while THIS (dispatch) thread
    /// drains the persistent-reader channel — completes. If the writer were collapsed onto the dispatch
    /// thread, this test would hang (and the 6 s hard-cap below turns the hang into a FAILED assertion,
    /// not an infinite test).
    #[cfg(unix)]
    #[test]
    fn huge_prompt_and_huge_response_do_not_deadlock() {
        let _g = SIDECAR_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_state();
        let big_len = 200_000usize; // > 128 KB, > the 64 KB pipe buffer
        // Read ONLY a 4 KB prefix of stdin (the `"id":N` sits near the front of the request line), then
        // emit the huge Done WITHOUT reading the remaining ~146 KB — leaving the stdin pipe full so a
        // write-then-read host would wedge. No `while`/`read` loop: draining the rest of stdin would
        // UNBIND the test.
        let body = format!(
            r#"printf '{{"type":"ready","model_id":"fake"}}\n'
PREFIX=$(head -c 4096)
ID=$(printf '%s' "$PREFIX" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p' | head -c 20)
[ -z "$ID" ] && ID=1
BIG=$(awk 'BEGIN{{s="";for(i=0;i<{n};i++)s=s "x";print s}}')
printf '{{"type":"done","id":%s,"text":"%s"}}\n' "$ID" "$BIG"
# Do NOT drain the rest of stdin: keep the stdin pipe FULL so a single-threaded write-then-read
# host would deadlock. Sleep so the child stays resident until the host reaps it.
sleep 20
"#,
            n = big_len
        );
        let script = write_fake_child("huge", &body);
        let model = dummy_model();

        // A >128 KB PROMPT: the host writes this whole thing on the scoped writer thread.
        let huge_user = "u".repeat(150_000);
        // A hard 6 s per-request cap so a REGRESSION (writer collapsed → deadlock) surfaces as a
        // Timeout-mapped `Unavailable`, i.e. a FAILED assertion below, rather than an infinite hang.
        let opts = GenOptions {
            timeout: Some(Duration::from_secs(6)),
            ..GenOptions::default()
        };
        let r = SidecarReasoner::new(model.clone(), SidecarTimeouts::default()).unwrap();
        let got = r
            .reason_with("system", &huge_user, opts)
            .expect("a >128 KB request + >128 KB response must complete on the writer-thread design");
        assert!(
            got.len() >= big_len,
            "the >128 KB response must come back whole (got {} bytes)",
            got.len()
        );
        assert!(got.bytes().all(|b| b == b'x'), "response body intact");
        cleanup(&script, &model);
    }

    /// A fixture body that emits Ready, then for each request line PARSES the host-stamped `"id":N`
    /// out of the request and answers `Done{id:N,text:"fake answer"}`. Echoing the SAME id matters:
    /// the host uses a process-global MONOTONIC id (never reset across tests), so a hardcoded id would
    /// mismatch and the dispatcher would loop to timeout. The `sed` pulls the first `"id":<digits>`.
    #[cfg(unix)]
    const ECHO_DONE_BODY: &str = r#"printf '{"type":"ready","model_id":"fake"}\n'
while IFS= read -r line; do
  ID=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -z "$ID" ] && ID=1
  printf '{"type":"done","id":%s,"text":"fake answer"}\n' "$ID"
done
"#;

    #[cfg(unix)]
    fn marker_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "murmur-brain-fake-marker-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
