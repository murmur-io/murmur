//! HOST side of the on-device brain sidecar — the [`LocalReasoner`] that drives the killable
//! `murmur-brain` child process over the NDJSON protocol in `brain_ipc.rs`.
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
//!    between calls and is reaped with bounded `try_wait()` polling after an explicit `kill()`.
//! 2. **PIPE-DEADLOCK avoidance**: note-gen writes the WHOLE transcript (can exceed the 64 KB pipe
//!    buffer) and the response can too. Child stdin is non-blocking and the dispatcher pumps partial
//!    writes while a PERSISTENT reader thread drains `ChildMsg` lines into an mpsc channel. There is
//!    no writer thread to join, so timeout/kill/reap stays authoritative even when the child stops
//!    reading halfway through a request.
//! 3. **Per-request timeout = KILL+RESPAWN** (replaces the old leaked-worker-on-timeout): on the
//!    deadline the child is killed and reaped with bounded `try_wait()` polling — TRUE cancellation
//!    + full RAM reclaim — and the next call respawns. The persistent reader thread ends when the
//!    killed child's pipe closes (there is
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

use std::io::{BufRead, BufReader, ErrorKind as IoErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{AppError, Result};
use crate::reason::{parse_first_json, GenOptions, LocalReasoner, StructuredObservation};

/// The FROZEN NDJSON wire protocol, shared verbatim with the child (`crates/murmur-brain`). Included
/// (not re-declared) so any drift is a compile error. Three `../` from `src-tauri/src/reason/` reach
/// the workspace root.
#[path = "../../../crates/murmur-brain/src/brain_ipc.rs"]
mod brain_ipc;

use brain_ipc::{ChildMsg, ErrorKind, GenOptsWire, HostMsg};

/// Filename of the child binary — inside `Contents/Resources` of a shipped `.app` and staged at
/// `binaries/` for dev (build.rs emits `BRAIN_BIN`).
const SIDECAR_NAME: &str = "murmur-brain";

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

/// Bounded wait (s) for a killed child to reap during app-exit. We retain the `Child` until this
/// attempt finishes; if the whole app is then force-killed, the helper's parent-death watchdog
/// exits even during a stuck generation and the next recording admission detects any survivor
/// fail-closed.
const QUIT_REAP_SECS: u64 = 2;

/// Poll slice while a non-blocking request write is waiting for pipe capacity. Replies are checked
/// between slices, so a child producing output while its stdin pipe is full cannot deadlock us.
const PIPE_POLL_SLICE: Duration = Duration::from_millis(10);

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn make_stdin_nonblocking(stdin: &ChildStdin) -> Result<()> {
    use std::os::fd::AsRawFd;

    const F_GETFL: i32 = 3;
    const F_SETFL: i32 = 4;
    #[cfg(target_os = "macos")]
    const O_NONBLOCK: i32 = 0x0004;
    #[cfg(target_os = "linux")]
    const O_NONBLOCK: i32 = 0x0800;

    extern "C" {
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }

    let fd = stdin.as_raw_fd();
    // SAFETY: `fd` is owned by the live `ChildStdin`; `fcntl` is a C function that reports failure
    // with `-1`/errno and cannot raise an Objective-C exception.
    let flags = unsafe { fcntl(fd, F_GETFL) };
    if flags == -1 {
        return Err(AppError::Unavailable(format!(
            "brain sidecar stdin flags unavailable: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: same live descriptor; this only adds O_NONBLOCK and preserves every existing flag.
    if unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) } == -1 {
        return Err(AppError::Unavailable(format!(
            "brain sidecar stdin could not be made non-blocking: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn make_stdin_nonblocking(_stdin: &ChildStdin) -> Result<()> {
    Err(AppError::Unavailable(
        "brain sidecar non-blocking stdin is unsupported on this platform".into(),
    ))
}

/// DEV/TEST-ONLY runtime override: an absolute path to a `murmur-brain`-compatible executable
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
    let needed = model_disk_bytes.saturating_mul(MODEL_RAM_HEADROOM_NUM) / MODEL_RAM_HEADROOM_DEN;
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
    /// Unique host ownership proof for `child`. Kept until `try_wait` actually reaps it, so the PID
    /// cannot be reused while a cancellation handle still targets it.
    child_identity: Option<Arc<ChildIdentity>>,
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
            child_identity: None,
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

    /// KILL the resident child (if any), poll `try_wait()` within `timeout`, then clear all handles.
    /// SIGKILL is fine — the child has nothing to flush (its output is throwaway NDJSON, no file).
    /// A timed-out child remains owned in this state so its PID cannot be reused behind our back and
    /// a second child cannot spawn. Dropping `stdin` + `rx` after a confirmed reap closes the child's
    /// stdin and lets the persistent reader thread end on stdout EOF.
    fn kill_and_clear_bounded(&mut self, timeout: Duration) -> bool {
        if let Some(child) = self.child.as_mut() {
            let Some(identity) = self.child_identity.as_ref() else {
                return false;
            };
            if !kill_and_reap_child_bounded(child, identity, timeout) {
                return false;
            }
        }
        self.child = None;
        if let Some(identity) = self.child_identity.take() {
            clear_active_sidecar(&identity);
        }
        self.stdin = None;
        self.rx = None; // dropping the receiver lets the reader thread's send fail → it ends.
        true
    }
}

#[derive(Debug)]
struct ChildIdentity;

struct ActiveKillHandle {
    pid: u32,
    owner: Weak<ChildIdentity>,
}

fn active_kill_handle() -> &'static Mutex<Option<ActiveKillHandle>> {
    static HANDLE: OnceLock<Mutex<Option<ActiveKillHandle>>> = OnceLock::new();
    HANDLE.get_or_init(|| Mutex::new(None))
}

struct SpawnPidGuard {
    identity: Arc<ChildIdentity>,
    armed: bool,
}

impl SpawnPidGuard {
    fn install(pid: u32) -> Self {
        let identity = Arc::new(ChildIdentity);
        let mut handle = active_kill_handle()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *handle = Some(ActiveKillHandle {
            pid,
            owner: Arc::downgrade(&identity),
        });
        drop(handle);
        Self {
            identity,
            armed: true,
        }
    }

    fn disarm(mut self) -> Arc<ChildIdentity> {
        self.armed = false;
        Arc::clone(&self.identity)
    }
}

impl Drop for SpawnPidGuard {
    fn drop(&mut self) {
        if self.armed {
            clear_active_sidecar(&self.identity);
        }
    }
}

fn clear_active_sidecar(identity: &Arc<ChildIdentity>) {
    let mut handle = active_kill_handle()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let same_owner = handle
        .as_ref()
        .and_then(|h| h.owner.upgrade())
        .is_some_and(|owner| Arc::ptr_eq(&owner, identity));
    if same_owner {
        *handle = None;
    }
}

fn signal_active_sidecar() -> Result<bool> {
    signal_active_sidecar_with(signal_process_immediately)
}

#[cfg(unix)]
fn signal_process_immediately(pid: u32) -> Result<bool> {
    const SIGKILL: i32 = 9;
    extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    let pid = i32::try_from(pid)
        .map_err(|_| AppError::Unavailable("brain sidecar pid is out of range".into()))?;
    // SAFETY: numeric PID ownership is held by `signal_active_sidecar_with` across this call. POSIX
    // kill is non-blocking and reports failure through errno; no pointer crosses the FFI boundary.
    if unsafe { kill(pid, SIGKILL) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    // ESRCH means there is no process left to signal. The Child handle is still retained and the
    // bounded try_wait path below remains responsible for the authoritative reap proof.
    if error.raw_os_error() == Some(3) {
        Ok(true)
    } else {
        Err(AppError::Unavailable(format!(
            "brain sidecar kill failed: {error}"
        )))
    }
}

#[cfg(not(unix))]
fn signal_process_immediately(pid: u32) -> Result<bool> {
    Command::new("kill")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .map_err(|error| AppError::Unavailable(format!("brain sidecar kill failed: {error}")))
}

fn signal_active_sidecar_with(signal: impl FnOnce(u32) -> Result<bool>) -> Result<bool> {
    // Hold the handle mutex through `/bin/kill`: the dispatch path cannot clear/reap this identity
    // between validation and signal. Every Child::try_wait/reap takes THIS SAME authority, so the
    // OS cannot recycle the numeric PID while the signal sink is using it.
    let mut handle = active_kill_handle()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(active) = handle.as_ref() else {
        return Ok(false);
    };
    if active.owner.upgrade().is_none() {
        *handle = None;
        return Ok(false);
    }
    signal(active.pid)
}

fn with_active_child_authority<T>(
    identity: &Arc<ChildIdentity>,
    action: impl FnOnce(&mut Option<ActiveKillHandle>) -> T,
) -> Option<T> {
    let mut handle = active_kill_handle()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let same_owner = handle
        .as_ref()
        .and_then(|active| active.owner.upgrade())
        .is_some_and(|owner| Arc::ptr_eq(&owner, identity));
    same_owner.then(|| action(&mut handle))
}

/// Kill/reap under the SAME exclusive identity authority used by numeric-PID signaling. A PID is
/// reusable only after `try_wait` returns `Some`; the handle is cleared while this mutex is still
/// held, so no signaler can validate the old identity and then hit a recycled process.
fn kill_and_reap_child_bounded(
    child: &mut Child,
    identity: &Arc<ChildIdentity>,
    timeout: Duration,
) -> bool {
    with_active_child_authority(identity, |handle| {
        let _ = child.kill();
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    *handle = None;
                    return true;
                }
                Ok(None) if Instant::now() >= deadline => return false,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => return false,
            }
        }
    })
    .unwrap_or(false)
}

fn retain_unreaped_child(state: &mut SidecarState, child: Child, identity: Arc<ChildIdentity>) {
    state.child = Some(child);
    state.child_identity = Some(identity);
    state.stdin = None;
    state.rx = None;
}

/// The single process-global dispatch mutex. `OnceLock<Mutex<..>>` mirrors `model_cache()`'s shape.
fn sidecar() -> &'static Mutex<SidecarState> {
    static STATE: OnceLock<Mutex<SidecarState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(SidecarState::empty()))
}

/// Cancel and reap the resident Brain before capture starts. Dispatch deliberately holds its mutex
/// for the whole request, so cancellation first validates a dedicated weak ownership handle and
/// SIGKILLs its content-free PID through `/bin/kill` WITHOUT that mutex. Holding the handle lock
/// across validation+signal prevents a reap/PID-reuse race. The dispatch reader wakes on EOF;
/// this function then takes the lock only to prove the child is gone and clear idle handles.
/// Returns `Ok(false)` on a bounded reap timeout; the caller refuses Start rather than record beside
/// a still-resident multi-GB child.
pub(crate) fn kill_for_recording(timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        // Re-read inside the loop: Start can race a dispatch that already owns the mutex but has not
        // reached `Command::spawn` yet. As soon as that PID appears, cancel it without waiting for
        // the dispatch mutex.
        let _ = signal_active_sidecar()?;
        match sidecar().try_lock() {
            Ok(mut state) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let had_child = state.child.is_some();
                let reaped = state.kill_and_clear_bounded(remaining);
                if reaped && had_child {
                    tracing::info!(target: "reason", "reclaimed brain sidecar before recording");
                }
                return Ok(reaped);
            }
            Err(std::sync::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(std::sync::TryLockError::WouldBlock) => return Ok(false),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                let mut state = poisoned.into_inner();
                return Ok(state
                    .kill_and_clear_bounded(deadline.saturating_duration_since(Instant::now())));
            }
        }
    }
}

/// Async-safe wrapper for commands/tasks. PID polling and bounded reap stay entirely on Tokio's
/// blocking pool; callers never sleep an async runtime worker.
pub(crate) async fn kill_for_recording_async(timeout: Duration) -> Result<bool> {
    tokio::task::spawn_blocking(move || kill_for_recording(timeout))
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("brain sidecar reap worker panicked: {e}")))?
}

// ---------------------------------------------------------------------------------------------------
// Binary resolution + hardened spawn (mirrors afm.rs).
// ---------------------------------------------------------------------------------------------------

/// Resolve the `murmur-brain` child binary, or `None`. Order (each filtered by `.exists()`):
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
fn spawn_and_wait_ready(
    state: &mut SidecarState,
    model_path: &Path,
    ready_secs: u64,
) -> Result<()> {
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
    let pid_guard = SpawnPidGuard::install(child.id());

    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
        if !kill_and_reap_child_bounded(&mut child, &pid_guard.identity, Duration::from_secs(2)) {
            retain_unreaped_child(state, child, pid_guard.disarm());
        }
        return Err(AppError::Unavailable(
            "brain sidecar pipes unavailable".into(),
        ));
    };
    if let Err(error) = make_stdin_nonblocking(&stdin) {
        if !kill_and_reap_child_bounded(&mut child, &pid_guard.identity, Duration::from_secs(2)) {
            retain_unreaped_child(state, child, pid_guard.disarm());
        }
        return Err(error);
    }

    let (tx, rx) = mpsc::channel::<ReaderEvent>();
    spawn_reader_thread(stdout, tx);

    // Bounded wait for the FIRST ChildMsg (must be `Ready`). A model load on a cold disk / first
    // Metal shader compile can be slow, hence the generous `ready_secs`; a wedged/failed child is
    // killed at the deadline so the caller degrades instead of hanging forever.
    let deadline = Instant::now() + Duration::from_secs(ready_secs);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            if !kill_and_reap_child_bounded(&mut child, &pid_guard.identity, Duration::from_secs(2))
            {
                retain_unreaped_child(state, child, pid_guard.disarm());
            }
            state.failed_until = Some(Instant::now() + Duration::from_secs(BACKOFF_SECS));
            tracing::warn!(target: "reason", ready_s = ready_secs, "brain sidecar ready-handshake timed out");
            return Err(AppError::Unavailable(
                "brain sidecar failed to become ready".into(),
            ));
        }
        match rx.recv_timeout(remaining) {
            Ok(ReaderEvent::Msg(ChildMsg::Ready { .. })) => {
                state.child = Some(child);
                state.child_identity = Some(pid_guard.disarm());
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
                if !kill_and_reap_child_bounded(
                    &mut child,
                    &pid_guard.identity,
                    Duration::from_secs(2),
                ) {
                    retain_unreaped_child(state, child, pid_guard.disarm());
                }
                state.failed_until = Some(Instant::now() + Duration::from_secs(BACKOFF_SECS));
                tracing::warn!(target: "reason", kind = ?kind, "brain sidecar reported an error before ready");
                return Err(map_error_kind(kind, message));
            }
            // Any pre-ready non-Ready/non-Error line (a stray heartbeat/done) — keep waiting.
            Ok(ReaderEvent::Msg(_)) => continue,
            // EOF: the child exited before Ready (crash / non-zero exit). Reap, back off, degrade.
            Ok(ReaderEvent::Eof) => {
                if !kill_and_reap_child_bounded(
                    &mut child,
                    &pid_guard.identity,
                    Duration::from_secs(2),
                ) {
                    retain_unreaped_child(state, child, pid_guard.disarm());
                }
                state.failed_until = Some(Instant::now() + Duration::from_secs(BACKOFF_SECS));
                return Err(AppError::Unavailable(
                    "brain sidecar failed to become ready".into(),
                ));
            }
            // Deadline elapsed with no message — kill+reap, back off, degrade.
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !kill_and_reap_child_bounded(
                    &mut child,
                    &pid_guard.identity,
                    Duration::from_secs(2),
                ) {
                    retain_unreaped_child(state, child, pid_guard.disarm());
                }
                state.failed_until = Some(Instant::now() + Duration::from_secs(BACKOFF_SECS));
                tracing::warn!(target: "reason", ready_s = ready_secs, "brain sidecar ready-handshake timed out");
                return Err(AppError::Unavailable(
                    "brain sidecar failed to become ready".into(),
                ));
            }
            // The reader thread hung up (spawn failed / it ended) — treat as a failed spawn.
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if !kill_and_reap_child_bounded(
                    &mut child,
                    &pid_guard.identity,
                    Duration::from_secs(2),
                ) {
                    retain_unreaped_child(state, child, pid_guard.disarm());
                }
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
    WriteError(String),
    Eof,
    Timeout,
}

/// The core dispatch: run one generation against the resident child, spawning it lazily and enforcing
/// idle-kill, backoff, the per-request deadline (= KILL+RESPAWN), and crash/EOF reap+respawn. Returns
/// the WHOLE-string result. On ANY failure returns a mapped `AppError` so the caller degrades.
///
/// PIPE-DEADLOCK AVOIDANCE: the `HostMsg::Generate` line (which can carry a >64 KB transcript) is
/// written in non-blocking chunks while the PERSISTENT reader thread drains `ChildMsg` lines into
/// the channel this thread polls between writes. The dispatcher owns stdin throughout; no scoped
/// writer can trap it in `join()` before timeout/kill/reap.
///
/// TIMEOUT = KILL+RESPAWN: the deadline is `opts.timeout` (or the `hard_cap_secs` when `None`). On the
/// deadline the child is killed and reaped with bounded `try_wait()` polling (TRUE cancellation +
/// full RAM reclaim) and the exact message `"on-device brain generation timed out"` returned (KEPT verbatim so existing callers/tests
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
        if !state.kill_and_clear_bounded(Duration::from_secs(2)) {
            return Err(AppError::Unavailable(
                "on-device brain sidecar is still terminating".into(),
            ));
        }
    }

    // Model changed under us (user selected a different GGUF) ⇒ kill the old resident child.
    if state.is_live() && state.model_path != model_path {
        tracing::info!(target: "reason", "brain model changed; respawning sidecar");
        if !state.kill_and_clear_bounded(Duration::from_secs(2)) {
            return Err(AppError::Unavailable(
                "on-device brain sidecar is still terminating".into(),
            ));
        }
    }

    // Spawn lazily on first use / after any kill. The RAM pre-check refuses-to-Cloud BEFORE paying a
    // spawn that would swap-death the machine (the child self-checks too; this avoids the fork).
    // 2026-07-13: also consult the kernel's OWN pressure signal (crate::perf::heavy_op_permitted)
    // alongside the vm_stat-derived floor — a different question ("does the kernel already think
    // the whole system is under pressure") than "does this job's footprint fit the free/inactive
    // arithmetic". Refuses only on CRITICAL kernel pressure; fails open on a broken probe either way.
    if !state.is_live() {
        if state.child.is_some() && !state.kill_and_clear_bounded(Duration::from_millis(100)) {
            return Err(AppError::Unavailable(
                "on-device brain sidecar is still terminating".into(),
            ));
        }
        if let Some(bytes) = model_disk_bytes(model_path) {
            if !crate::perf::heavy_op_permitted(ram_permits_load(available_ram_bytes(), bytes)) {
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
    let mut line = match serde_json::to_string(&req) {
        Ok(l) => l,
        Err(e) => return Err(AppError::Summarize(format!("brain request serialize: {e}"))),
    };
    line.push('\n');

    // Take stdin OUT so this dispatch exclusively owns the non-blocking request pump; the reader
    // channel stays in `state`.
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
    let bytes = line.as_bytes();
    let mut written = 0usize;

    // Pump request bytes and replies under ONE deadline. When the pipe is full, wait only a short
    // slice for a reply before retrying the write. A terminal reply received before the request was
    // fully consumed is a protocol failure: accepting its text would silently build a note/reply
    // from a truncated prompt. The child is torn down below because unread request bytes would also
    // corrupt the next generation.
    let outcome = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break Outcome::Timeout;
        }

        let mut write_blocked = false;
        if written < bytes.len() {
            match stdin.write(&bytes[written..]) {
                Ok(0) => break Outcome::WriteError("stdin pipe closed".into()),
                Ok(count) => written = written.saturating_add(count),
                Err(error) if error.kind() == IoErrorKind::Interrupted => continue,
                Err(error) if error.kind() == IoErrorKind::WouldBlock => write_blocked = true,
                Err(error) => break Outcome::WriteError(error.to_string()),
            }
        }

        let event = if written == bytes.len() {
            rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        } else if write_blocked {
            rx.recv_timeout(PIPE_POLL_SLICE.min(deadline.saturating_duration_since(Instant::now())))
        } else {
            match rx.try_recv() {
                Ok(event) => Ok(event),
                Err(mpsc::TryRecvError::Empty) => continue,
                Err(mpsc::TryRecvError::Disconnected) => Err(mpsc::RecvTimeoutError::Disconnected),
            }
        };

        match event {
            Ok(ReaderEvent::Msg(ChildMsg::Done { id: rid, text })) if rid == id => {
                break Outcome::Done(text);
            }
            Ok(ReaderEvent::Msg(ChildMsg::Error {
                id: rid,
                kind,
                message,
            })) if rid == id => break Outcome::ChildError(kind, message),
            // Heartbeats and stray replies never extend the caller's hard deadline.
            Ok(ReaderEvent::Msg(_)) => continue,
            Ok(ReaderEvent::Eof) => break Outcome::Eof,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= deadline {
                    break Outcome::Timeout;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break Outcome::Eof,
        }
    };
    let request_complete = written == bytes.len();

    // Resolve the outcome, re-installing the resident pipes ONLY when the child is still healthy.
    match outcome {
        Outcome::Done(text) => {
            if !request_complete {
                tracing::warn!(target: "reason", "brain sidecar replied before consuming its request; terminating protocol stream");
                if !state.kill_and_clear_bounded(Duration::from_secs(2)) {
                    return Err(AppError::Unavailable(
                        "on-device brain returned before the full request and could not be proven stopped"
                            .into(),
                    ));
                }
                return Err(AppError::Summarize(
                    "on-device brain returned before the full request was delivered".into(),
                ));
            }
            state.stdin = Some(stdin);
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
            // A generation error after a complete request leaves the child reusable. An early error
            // tears it down because unread bytes would desynchronise the next NDJSON request.
            if request_complete {
                state.stdin = Some(stdin);
            } else {
                let _ = state.kill_and_clear_bounded(Duration::from_secs(2));
            }
            state.last_used = Instant::now();
            Err(map_error_kind(kind, message))
        }
        Outcome::WriteError(error) => {
            let _ = state.kill_and_clear_bounded(Duration::from_secs(2));
            Err(AppError::Summarize(format!(
                "brain sidecar request write failed: {error}"
            )))
        }
        Outcome::Eof => {
            // The child died mid-request. Reap the process, clear, degrade.
            let _ = state.kill_and_clear_bounded(Duration::from_secs(2));
            Err(AppError::Summarize("brain sidecar exited".into()))
        }
        Outcome::Timeout => {
            // TRUE cancellation: KILL+WAIT reclaims ALL model RAM and stops the wedged decode. The
            // next call respawns. KEEP this exact message (existing callers/tests match it).
            let _ = state.kill_and_clear_bounded(Duration::from_secs(2));
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

/// A [`LocalReasoner`] that drives the on-device brain through the killable `murmur-brain` child.
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
        Ok(self
            .structured_observation_inner(system, user, json_schema, opts)?
            .value)
    }

    fn structured_observation_inner(
        &self,
        system: &str,
        user: &str,
        json_schema: &Value,
        opts: GenOptions,
    ) -> Result<StructuredObservation> {
        // The child instructs the schema in the prompt (matching the old in-process path) and, when
        // opted-in with a tiny schema, tries a grammar-constrained decode with a graceful fallback —
        // ALL host-transparent. We pass the schema so the child prompts on it, and recover the object
        // here via the SAME robust extractor the old path used.
        let text = dispatch(&self.model_path, system, user, opts, Some(json_schema))?;
        Ok(StructuredObservation {
            value: parse_first_json(&text)?,
            raw_text: Some(text),
        })
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

    fn structured_with_observation(
        &self,
        system: &str,
        user: &str,
        json_schema: &Value,
        opts: GenOptions,
    ) -> Result<StructuredObservation> {
        self.structured_observation_inner(system, user, json_schema, opts)
    }
}

/// App-exit hook (wired from `lib.rs` `RunEvent::ExitRequested`): KILL the resident child + a BOUNDED
/// reap so app-exit is never blocked. A force-killed parent cannot safely signal a later PID by
/// observation alone; the child's own parent-death watchdog exits from inside the exact process,
/// even during a stuck generation, and the next recording admission refuses to overlap any
/// detected survivor. Best-effort + panic-free.
pub fn kill_on_quit() {
    let Ok(mut state) = sidecar().lock() else {
        return; // poisoned — the OS reaps the child on process teardown anyway.
    };
    if !state.kill_and_clear_bounded(Duration::from_secs(QUIT_REAP_SECS)) {
        tracing::warn!(target: "reason", "brain sidecar did not reap within the quit budget; retaining child ownership until process exit");
    }
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

    #[test]
    fn production_sidecar_name_is_murmur_brain() {
        assert_eq!(SIDECAR_NAME, "murmur-brain");
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
            let _ = s.kill_and_clear_bounded(Duration::from_secs(2));
            s.failed_until = None;
            s.model_path = PathBuf::new();
            s.last_used = Instant::now();
        }
    }

    fn active_owned_pid() -> Option<u32> {
        active_kill_handle()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|handle| handle.owner.upgrade().is_some())
            .map(|handle| handle.pid)
    }

    /// Serialize the fake-child tests: they share the ONE process-global resident child + the
    /// `MURMUR_BRAIN_SIDECAR` override, so they must not run concurrently.
    static SIDECAR_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn sidecar_test_guards() -> (
        std::sync::MutexGuard<'static, ()>,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let sidecar = SIDECAR_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let residency = crate::perf::model_lifecycle_test_guard();
        (sidecar, residency)
    }

    /// RED-before-GREEN for the PID-reuse race: once a signaler has validated an identity and
    /// entered the injected signal sink, the reap authority must remain blocked. Therefore the OS
    /// cannot recycle that PID before the signal returns. The old split-lock design allowed
    /// `Child::try_wait` to reap between validation and `/bin/kill`.
    #[test]
    fn validated_pid_cannot_be_reaped_until_signal_sink_returns() {
        let (_g, _residency) = sidecar_test_guards();
        reset_state();
        let identity = Arc::new(ChildIdentity);
        *active_kill_handle()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ActiveKillHandle {
            pid: 42_424,
            owner: Arc::downgrade(&identity),
        });

        let (signal_entered_tx, signal_entered_rx) = std::sync::mpsc::channel();
        let (release_signal_tx, release_signal_rx) = std::sync::mpsc::channel();
        let signaler = std::thread::spawn(move || {
            signal_active_sidecar_with(|pid| {
                assert_eq!(pid, 42_424);
                signal_entered_tx.send(()).unwrap();
                release_signal_rx.recv().unwrap();
                Ok(true)
            })
            .unwrap()
        });
        signal_entered_rx.recv().unwrap();

        let identity_for_reap = Arc::clone(&identity);
        let (reap_at_lock_tx, reap_at_lock_rx) = std::sync::mpsc::channel();
        let (blocked_result_tx, blocked_result_rx) = std::sync::mpsc::channel();
        let (reap_entered_tx, reap_entered_rx) = std::sync::mpsc::channel();
        let reaper = std::thread::spawn(move || {
            // Prove this thread was scheduled and reached the exact authority mutex boundary;
            // `try_lock` gives a deterministic blocked result instead of a timeout-based guess.
            reap_at_lock_tx.send(()).unwrap();
            let blocked = matches!(
                active_kill_handle().try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            );
            blocked_result_tx.send(blocked).unwrap();
            with_active_child_authority(&identity_for_reap, |handle| {
                reap_entered_tx.send(()).unwrap();
                *handle = None;
            })
        });
        reap_at_lock_rx.recv().unwrap();
        assert!(
            blocked_result_rx.recv().unwrap(),
            "reaper reached the authority mutex but it was not held by the validated signal sink"
        );

        release_signal_tx.send(()).unwrap();
        assert!(signaler.join().unwrap());
        reap_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("reap authority must resume after the signal sink returns");
        assert!(reaper.join().unwrap().is_some());
        assert!(active_owned_pid().is_none());
    }

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
        let (_g, _residency) = sidecar_test_guards();
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
        let (_g, _residency) = sidecar_test_guards();
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
        let (_g, _residency) = sidecar_test_guards();
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

    /// Recording priority cancels an in-flight killable generation immediately instead of waiting
    /// for its ordinary request timeout / the generic native drain budget.
    #[cfg(unix)]
    #[test]
    fn recording_start_kills_and_reaps_an_inflight_sidecar_promptly() {
        let (_g, _residency) = sidecar_test_guards();
        reset_state();
        let script = write_fake_child(
            "recording-cancel",
            "printf '{\"type\":\"ready\",\"model_id\":\"fake\"}\\n'\nwhile IFS= read -r _line; do\n  exec sleep 30\ndone\n",
        );
        let model = dummy_model();
        let reasoner = SidecarReasoner::new(model.clone(), SidecarTimeouts::default()).unwrap();
        let generation = std::thread::spawn(move || reasoner.reason("s", "u"));

        let pid_deadline = Instant::now() + Duration::from_secs(2);
        while active_owned_pid().is_none() && Instant::now() < pid_deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(active_owned_pid().is_some());

        let started = Instant::now();
        assert!(kill_for_recording(Duration::from_secs(2)).unwrap());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "recording cancellation must not wait for the 30s fake generation"
        );
        assert!(generation.join().unwrap().is_err());
        assert!(active_owned_pid().is_none());

        cleanup(&script, &model);
    }

    /// EOF / CRASH mid-request → REAP → RESPAWN: a fake child that goes Ready then EXITS on the first
    /// request (EOF mid-request) surfaces `Summarize("brain sidecar exited")`, reaps the process
    /// (no zombie, slot cleared), and a 2nd call respawns + succeeds.
    #[cfg(unix)]
    #[test]
    fn eof_mid_request_reaps_then_respawns() {
        let (_g, _residency) = sidecar_test_guards();
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
        let (_g, _residency) = sidecar_test_guards();
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

    /// PIPE-DEADLOCK + TRUNCATION PROOF: an early >128 KB response while a >128 KB prompt remains
    /// only partly delivered must terminate promptly as an ERROR, never be accepted as if the child
    /// had reasoned over the full prompt. This test is also RED if stdin becomes blocking again.
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
    /// A blocking write-then-read host would DEADLOCK here. The shipped design pumps a non-blocking
    /// stdin while the reader thread drains stdout, rejects the incomplete-prompt result, and kills
    /// the now-desynchronised child. The 6 s cap turns a regression into a bounded failure.
    #[cfg(unix)]
    #[test]
    fn early_huge_response_to_partial_huge_prompt_is_bounded_error() {
        let (_g, _residency) = sidecar_test_guards();
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

        // A >128 KB PROMPT: the host pumps this through non-blocking stdin.
        let huge_user = "u".repeat(150_000);
        // A hard 6 s per-request cap so a REGRESSION (writer collapsed → deadlock) surfaces as a
        // Timeout-mapped `Unavailable`, i.e. a FAILED assertion below, rather than an infinite hang.
        let opts = GenOptions {
            timeout: Some(Duration::from_secs(6)),
            ..GenOptions::default()
        };
        let r = SidecarReasoner::new(model.clone(), SidecarTimeouts::default()).unwrap();
        let started = Instant::now();
        let result = r.reason_with("system", &huge_user, opts);
        assert!(
            result.is_err(),
            "an answer to a partial prompt must be rejected"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "partial-write protocol failure must stay bounded"
        );
        assert!(
            !sidecar().lock().unwrap().is_live(),
            "the desynchronised child must be reaped"
        );
        cleanup(&script, &model);
    }

    /// Full-drain twin: both directions exceed ordinary pipe capacity, but the child consumes the
    /// complete NDJSON request before replying. Concurrent pumping must return the whole response.
    #[cfg(unix)]
    #[test]
    fn fully_drained_huge_prompt_and_response_succeed() {
        let (_g, _residency) = sidecar_test_guards();
        reset_state();
        let big_len = 200_000usize;
        let body = format!(
            r#"printf '{{"type":"ready","model_id":"fake"}}\n'
BIG=$(awk 'BEGIN{{s="";for(i=0;i<{n};i++)s=s "x";print s}}')
while IFS= read -r line; do
  ID=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -z "$ID" ] && ID=1
  printf '{{"type":"done","id":%s,"text":"%s"}}\n' "$ID" "$BIG"
done
"#,
            n = big_len
        );
        let script = write_fake_child("huge-full-drain", &body);
        let model = dummy_model();
        let huge_user = "u".repeat(150_000);
        let opts = GenOptions {
            timeout: Some(Duration::from_secs(6)),
            ..GenOptions::default()
        };
        let r = SidecarReasoner::new(model.clone(), SidecarTimeouts::default()).unwrap();
        let got = r
            .reason_with("system", &huge_user, opts)
            .expect("fully drained >pipe-cap request/response must succeed");
        assert_eq!(got.len(), big_len);
        assert!(got.bytes().all(|byte| byte == b'x'));
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
