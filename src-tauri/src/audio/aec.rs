//! AEC microphone capture via the VoiceProcessingIO Swift helper (`aeccap`).
//!
//! EXPERIMENTAL + opt-in (`aec_enabled`, default off). Runs IN PARALLEL with the primary cpal mic
//! — cpal stays the level meter / mute / live-captions / archive source and the fallback; the
//! AEC'd WAV this helper produces becomes the mic ASR feed when present. Any failure (incl. the
//! cpal/VPIO coexistence not working on the real device) leaves the recording on the raw cpal mic.
//!
//! ⚠️ The cpal/VPIO coexistence on one mic + real echo cancellation need a SIGNED build on a real
//! Mac with a live call — they are NOT verifiable headless (see `aeccap/aeccap.swift`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use tauri::{AppHandle, Manager};

use crate::error::{AppError, Result};

const AEC_HELPER_NAME: &str = "meetnotes-aeccap";

/// Hard wall-clock cap (seconds) on a single AEC capture, passed to the helper so it self-stops.
/// A meeting longer than this loses AEC and falls back to the raw cpal mic for ASR (the pipeline's
/// duration guard does the same) — far better than an UNBOUNDED VPIO capture, which once stranded a
/// 91 GB scratch WAV after a stuck/never-stopped session. 4 h covers any realistic meeting.
const MAX_CAPTURE_SECONDS: u64 = 4 * 60 * 60;

fn safe_scratch_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(crate) fn is_legacy_system_scratch_filename(name: &str) -> bool {
    name.strip_prefix("meetnotes-sys-")
        .and_then(|value| value.strip_suffix(".wav"))
        .is_some_and(safe_scratch_id)
}

fn is_capture_scratch_filename(name: &str) -> bool {
    is_legacy_system_scratch_filename(name)
        || name
            .strip_prefix("meetnotes-aec-")
            .and_then(|value| value.strip_suffix(".wav"))
            .is_some_and(safe_scratch_id)
}

/// Exact-path exemptions for legacy recovery-owned temp WAVs. Paths are canonicalized before they
/// enter the set and again before a sweep compares them; no basename or prefix grants protection.
/// Any unreadable sidecar/path ambiguity flips `preserve_all`, because a missed orphan is safer than
/// deleting the only historical far-side recording.
#[derive(Clone, Default)]
pub(crate) struct StaleScratchProtection {
    exact_canonical_paths: HashSet<PathBuf>,
    preserve_all: bool,
}

impl StaleScratchProtection {
    pub(crate) fn protect_existing(&mut self, path: &Path) -> Result<()> {
        match std::fs::canonicalize(path) {
            Ok(canonical) => {
                self.exact_canonical_paths.insert(canonical);
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => {
                self.preserve_all = true;
                Err(AppError::Audio(format!(
                    "canonicalize protected legacy scratch: {error}"
                )))
            }
        }
    }

    pub(crate) fn preserve_all(&mut self) {
        self.preserve_all = true;
    }

    /// Reserve an exact canonical candidate even while it is absent. Legacy sidecars name a temp
    /// path before a helper necessarily publishes it; without this reservation, an old-mtime file
    /// appearing after preflight but before the startup sweep could be deleted as unrelated. The
    /// caller must have validated that the parent is the canonical selected temp root and the leaf
    /// is one accepted direct-child scratch name.
    pub(crate) fn protect_validated_candidate(&mut self, canonical_candidate: &Path) -> Result<()> {
        if !canonical_candidate.is_absolute()
            || canonical_candidate
                .file_name()
                .and_then(|name| name.to_str())
                .map_or(true, |name| !is_legacy_system_scratch_filename(name))
        {
            self.preserve_all = true;
            return Err(AppError::Audio(
                "legacy scratch protection candidate is invalid".into(),
            ));
        }
        self.exact_canonical_paths
            .insert(canonical_candidate.to_path_buf());
        Ok(())
    }

    fn protects(&self, canonical: &Path) -> bool {
        self.preserve_all || self.exact_canonical_paths.contains(canonical)
    }
}

/// Best-effort: delete stale capture scratch WAVs (`meetnotes-aec-*.wav` / `meetnotes-sys-*.wav`)
/// left in the temp dir by a previous crashed/stuck/never-stopped session. Called only after helper
/// detection has populated `protection`; any live/ambiguous helper preserves every scratch file.
/// Touches ONLY the OS temp dir, never the app-data audio dir; logs IDs/counts only, no PII.
pub(crate) fn sweep_stale_scratch(protection: &StaleScratchProtection) {
    let dir = std::env::temp_dir();
    let removed = sweep_stale_scratch_in(&dir, std::time::SystemTime::now(), protection);
    if removed > 0 {
        tracing::info!(target: "audio", removed, "swept stale capture scratch WAVs at startup");
    }
}

pub(crate) fn sweep_stale_scratch_in(
    dir: &Path,
    now: std::time::SystemTime,
    protection: &StaleScratchProtection,
) -> u32 {
    if protection.preserve_all {
        tracing::warn!(target: "audio", "stale scratch sweep skipped because legacy recovery ownership was ambiguous");
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0u32;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !is_capture_scratch_filename(&name) {
            continue;
        }
        // `sweep_stale_scratch` passes the real OS temp root. Keeping the selected root injectable
        // lets recovery tests use an isolated directory instead of scanning/deleting unrelated
        // files in the developer's live $TMPDIR; the same direct-child/canonical/no-symlink proof
        // applies to that selected root.
        let Some(canonical) = verified_temp_scratch_path_in(&entry.path(), dir) else {
            continue;
        };
        if protection.protects(&canonical) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .map(|age| age.as_secs() > 3600)
            .unwrap_or(false);
        if stale && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Basenames of long-lived helper binaries the app has shipped. Current helpers receive an exact
/// parent-lifetime pipe and hard-exit within five seconds of owner loss; this scan still covers the
/// short handoff window, a broken helper, and legacy builds that had only the 4 h cap. Whether a
/// survivor is orphaned is decided per-parent by [`helper_verdict`]; a helper owned by a LIVE Murmur
/// process is never touched.
const CAPTURE_HELPERS: [&str; 5] = [
    "meetnotes-sysaudio",
    "meetnotes-audiocap",
    "meetnotes-aeccap",
    // The on-device brain sidecar: a launchd-reparented `murmur-brain` (the app was SIGKILL'd
    // mid-generation, or `kill_on_quit`'s bounded reap abandoned a slow-dying child) keeps a
    // multi-GB model resident until its child-owned stdin-HUP watchdog observes the parent-owned
    // pipe close. Detection blocks a new Start during that handoff; this scanner never signals it.
    "murmur-brain",
    // Legacy orphan compatibility only: builds before the rename spawned this basename. Never
    // resolve or spawn it, but detect/block a survivor left by an older app process.
    "meetnotes-brain",
];

/// `ww` is load-bearing for dev/hot-rebuild paths: Darwin otherwise truncates `comm` to display
/// width and can remove the trailing helper basename, turning a real orphan into a false clean scan.
const PROCESS_SNAPSHOT_ARGS: [&str; 3] = ["-axww", "-o", "pid=,ppid=,comm="];

// Intentionally NOT in `CAPTURE_HELPERS`: `meetnotes-afm`. The Swift source does not exist and the
// binary is not bundled today (`build.rs`'s AFM build is an explicit no-op). Debug fixture children
// may use arbitrary override basenames and are owned/reaped by `reason::afm`'s retained-Child gate;
// adding a basename here before a real shipped executable exists would promise coverage we do not
// actually have. Add it together with the source + bundle resource when that sidecar becomes real.

/// Detect every cross-launch orphan and fail closed before a new recording starts. This function
/// deliberately never signals a pid or deletes its scratch: Darwin has no pidfd, and without a
/// helper-published audit token a recheck followed by `kill(pid)` still has an unavoidable ABA
/// window. Shipped capture helpers watch an exact parent-owned stdin pipe and hard-exit within five
/// seconds of EOF; their 4 h wall cap remains defense in depth. The brain watches the same exact
/// capability and exits even during a hung generation. Current-process children are retained and
/// reaped through their `Child` handles.
/// Any orphan or identity ambiguity disables the age sweep so historical scratch is preserved.
/// This closes the gap [`sweep_stale_scratch`]
/// can't: a *live* orphan keeps its WAV mtime fresh, so the file-age sweep never reclaims it, and
/// the child runs for hours (a real audiocap orphan once outlived its parent by 7h20m, writing
/// 2+ GB — the old `ppid == 1`-only filter plus the once-at-launch schedule let it survive).
///
/// A helper is judged by [`helper_verdict`] on what we know about its PARENT: reparented to
/// launchd, a dead ppid, or a live-but-not-Murmur ppid all mean BLOCK; a helper owned by a LIVE
/// Murmur process (a genuinely concurrent instance, or this process) is SPARED from signalling but
/// still reported active, so recovery/new capture never overlaps it.
///
/// `Err` means an orphan/ambiguity exists. Callers must fail closed before capture side effects.
pub(crate) fn detect_surviving_capture_helpers(
    mut scratch_protection: Option<&mut StaleScratchProtection>,
) -> Result<()> {
    // Candidate names come from `comm=` — the executable path ONLY, no arguments — so a process
    // that
    // merely MENTIONS a helper name in its args (`grep -r meetnotes-audiocap …`) can never be
    // selected, and a parent at a path with an embedded space (`/Applications/Murmur 2.app/…`,
    // Finder "keep both") still resolves to its true basename. Parsing the args-bearing
    // `command=` column token-wise for identity was the root cause of both 2026-07-16 review
    // findings (live-Murmur helpers killed / innocent processes killed).
    let out = match std::process::Command::new("/bin/ps")
        .args(PROCESS_SNAPSHOT_ARGS)
        .output()
    {
        Ok(out) => out,
        Err(error) => {
            if let Some(protection) = scratch_protection.as_deref_mut() {
                protection.preserve_all();
            }
            return Err(AppError::Audio(format!(
                "orphan-helper process scan failed: {error}"
            )));
        }
    };
    if !out.status.success() {
        if let Some(protection) = scratch_protection.as_deref_mut() {
            protection.preserve_all();
        }
        return Err(AppError::Audio(format!(
            "orphan-helper process scan exited with status {}",
            out.status
        )));
    }
    let snapshot = match String::from_utf8(out.stdout) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            if let Some(protection) = scratch_protection.as_deref_mut() {
                protection.preserve_all();
            }
            return Err(AppError::Audio(format!(
                "orphan-helper process scan was not UTF-8: {error}"
            )));
        }
    };
    if !capture_snapshot_is_unambiguous(&snapshot) {
        if let Some(protection) = scratch_protection.as_deref_mut() {
            protection.preserve_all();
        }
        return Err(AppError::Audio(
            "orphan-helper process scan contained a malformed helper row".into(),
        ));
    }
    let self_exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    let decisions = helper_decisions(&snapshot, self_exe.as_deref(), pid_alive);
    let mut orphans = 0u32;
    let mut active = 0u32;
    for (helper, verdict) in decisions {
        match verdict {
            HelperVerdict::Spare => {
                active += 1;
                // We intentionally do not inspect args/WAV ownership across process snapshots.
                // Any live helper disables the age sweep AND blocks recovery/new Start so a
                // concurrent recording cannot lose scratch or overlap expensive model/audio work.
                if let Some(protection) = scratch_protection.as_deref_mut() {
                    protection.preserve_all();
                }
                // Ids only (no paths/PII). Never signal: this helper belongs to a live app.
                tracing::warn!(
                    target: "audio",
                    helper_pid = helper.pid,
                    parent_pid = helper.ppid,
                    "helper owned by a live Murmur process; leaving it untouched and deferring recovery/capture"
                );
            }
            HelperVerdict::Block => {
                orphans += 1;
                if let Some(protection) = scratch_protection.as_deref_mut() {
                    protection.preserve_all();
                }
                tracing::error!(
                    target: "audio",
                    helper_pid = helper.pid,
                    "orphan helper detected; generation-bound signalling unavailable, capture blocked and scratch preserved"
                );
            }
        }
    }
    if orphans > 0 {
        return Err(AppError::Unavailable(format!(
            "{orphans} orphaned helper process(es) detected; wait for them to exit before recording"
        )));
    }
    if active > 0 {
        return Err(AppError::Unavailable(format!(
            "{active} helper process(es) belong to another active Murmur session; close that session before recording"
        )));
    }
    Ok(())
}

/// One surviving capture-helper process from the `ps` snapshot.
#[derive(Debug, PartialEq, Eq, Clone)]
struct HelperProc {
    pid: i32,
    ppid: i32,
}

/// The fate of one surviving capture helper.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum HelperVerdict {
    /// Orphaned (or adopted by a non-Murmur process) — block Start and preserve all scratch.
    Block,
    /// A live Murmur process owns it (a genuinely concurrent instance, or us) — never touch it.
    Spare,
}

/// Return the canonical identity of a deletable capture scratch only when both spellings prove it
/// is a DIRECT child of `temp_root`: the lexical parent is exactly the root and the canonical
/// parent is exactly the canonical root. Symlinks and the exact helper-owned basename pattern are
/// rejected/required respectively. Any missing/unreadable/ambiguous component fails closed.
fn verified_temp_scratch_path_in(path: &Path, temp_root: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    if !is_capture_scratch_filename(name) || !path.is_absolute() || !temp_root.is_absolute() {
        return None;
    }

    if path.parent() != Some(temp_root) {
        return None;
    }
    let mut components = path.strip_prefix(temp_root).ok()?.components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return None;
    }
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() {
        return None;
    }

    let canonical_root = std::fs::canonicalize(temp_root).ok()?;
    let canonical_path = std::fs::canonicalize(path).ok()?;
    if canonical_path.parent() != Some(canonical_root.as_path()) {
        return None;
    }
    Some(canonical_path)
}

/// PURE verdict for ONE surviving capture helper, given what we know about its parent.
/// Unit-testable without live processes.
///
/// BLOCK when the parent is gone or was never ours:
///   - `ppid == 1` — reparented to launchd, the classic orphan;
///   - `!ppid_alive` — the parent pid no longer exists (`kill(ppid, 0)` → ESRCH): a dead-or-dying
///     parent whose child has not (yet) reparented — the window the old `ppid == 1`-only filter
///     missed;
///   - `!ppid_is_murmur` — the ppid is alive but is NOT a Murmur process (a recycled pid, or an
///     adopter that does not own the helper lifecycle).
///
/// SPARE only when a LIVE Murmur process owns it.
fn helper_verdict(ppid: i32, ppid_alive: bool, ppid_is_murmur: bool) -> HelperVerdict {
    if ppid == 1 || !ppid_alive || !ppid_is_murmur {
        HelperVerdict::Block
    } else {
        HelperVerdict::Spare
    }
}

/// Testable core of [`detect_surviving_capture_helpers`]: parse the `comm=` snapshot and attach a
/// [`HelperVerdict`] to every surviving capture helper. `ppid_alive_probe` is the injected
/// liveness oracle (`kill -0` in production); parent liveness is the probe OR-ed with the parent's
/// presence in the SAME snapshot, so a broken probe binary can never misread a live concurrent
/// Murmur's parent as dead and block its mid-recording helper (the spare-biased direction).
fn helper_decisions<F: Fn(i32) -> bool>(
    comm_snapshot: &str,
    self_exe: Option<&str>,
    ppid_alive_probe: F,
) -> Vec<(HelperProc, HelperVerdict)> {
    let helpers = parse_capture_helpers(comm_snapshot);
    if helpers.is_empty() {
        return Vec::new();
    }
    let basenames = parse_process_basenames(comm_snapshot);
    helpers
        .into_iter()
        .map(|helper| {
            let alive = basenames.contains_key(&helper.ppid) || ppid_alive_probe(helper.ppid);
            let murmur = basenames
                .get(&helper.ppid)
                .map(|b| is_murmur_basename(b, self_exe))
                .unwrap_or(false);
            let verdict = helper_verdict(helper.ppid, alive, murmur);
            (helper, verdict)
        })
        .collect()
}

/// Split one `ps` line into its leading numeric columns and the final free-text column. The final
/// column (`comm`/`command`) is the ONLY variable-width field and it is LAST, so everything after
/// the fixed numeric prefix — embedded spaces included — belongs to it unambiguously.
fn split_numeric_prefix(line: &str, numeric_cols: usize) -> Option<(Vec<i32>, &str)> {
    let mut rest = line.trim_start();
    let mut nums = Vec::with_capacity(numeric_cols);
    for _ in 0..numeric_cols {
        let (tok, tail) = rest.split_once(char::is_whitespace)?;
        nums.push(tok.parse::<i32>().ok()?);
        rest = tail.trim_start();
    }
    let text = rest.trim_end();
    if text.is_empty() {
        return None;
    }
    Some((nums, text))
}

/// Whether a process basename is a Murmur app process: the shipped/dev bin name (`Murmur`) or the
/// basename of OUR OWN executable (covers a renamed dev profile). NOTE: `ps` renders a zombie's
/// `comm` specially — the BSD man page documents a parenthesized name (`(Murmur)`), but Darwin 25
/// actually renders `<defunct>` — and NEITHER equals the live basename, so a zombie parent
/// correctly reads as dead under both renderings.
fn is_murmur_basename(base: &str, self_exe: Option<&str>) -> bool {
    base == "Murmur" || self_exe == Some(base)
}

/// `kill -0 <pid>`: probe process existence without sending a signal. Exit 0 = the pid exists and
/// is signalable by us — and only a SAME-USER process can be the Murmur that owns a helper, so an
/// EPERM failure (another user's recycled pid) correctly reads as "not our live parent".
fn pid_alive(pid: i32) -> bool {
    std::process::Command::new("/bin/kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Pure parser for `ps -axo pid=,ppid=,comm=` output: every line whose EXECUTABLE basename is one
/// of the [`CAPTURE_HELPERS`]. `comm` is the executable path alone — no arguments — so a process
/// that merely mentions a helper name in its args (`grep -r meetnotes-audiocap …`) can never
/// match, and a helper installed at a path with an embedded space still resolves correctly (the
/// path is the whole final column; see [`split_numeric_prefix`]).
fn parse_capture_helpers(comm_snapshot: &str) -> Vec<HelperProc> {
    let mut out = Vec::new();
    for line in comm_snapshot.lines() {
        let Some((nums, comm)) = split_numeric_prefix(line, 2) else {
            continue;
        };
        let base = comm.rsplit('/').next().unwrap_or(comm);
        if !CAPTURE_HELPERS.contains(&base) {
            continue;
        }
        out.push(HelperProc {
            pid: nums[0],
            ppid: nums[1],
        });
    }
    out
}

fn capture_snapshot_is_unambiguous(comm_snapshot: &str) -> bool {
    comm_snapshot.lines().all(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return true;
        }
        let basename = trimmed.rsplit('/').next().unwrap_or(trimmed);
        !CAPTURE_HELPERS.contains(&basename) || split_numeric_prefix(line, 2).is_some()
    })
}

/// Pure parser for the SAME `comm=` snapshot: `pid → executable basename` for every process, so a
/// helper's ppid can be resolved to the process that owns it (and its liveness read off the
/// snapshot) without a second `ps` round-trip. Because `comm` carries no arguments, a Murmur at
/// `/Applications/Murmur 2.app/Contents/MacOS/Murmur` resolves to `Murmur` — the 2026-07-16
/// review proved the old args-bearing token parse resolved it wrong and targeted live helpers.
fn parse_process_basenames(comm_snapshot: &str) -> std::collections::HashMap<i32, String> {
    let mut out = std::collections::HashMap::new();
    for line in comm_snapshot.lines() {
        let Some((nums, comm)) = split_numeric_prefix(line, 2) else {
            continue;
        };
        let base = comm.rsplit('/').next().unwrap_or(comm);
        out.insert(nums[0], base.to_string());
    }
    out
}

/// Path to the bundled VPIO AEC helper (resource dir, then the dev `AECCAP_BIN` fallback).
pub fn aec_helper_path(app: &AppHandle) -> Option<PathBuf> {
    if let Ok(p) = app
        .path()
        .resolve(AEC_HELPER_NAME, tauri::path::BaseDirectory::Resource)
    {
        if p.exists() {
            return Some(p);
        }
    }
    option_env!("AECCAP_BIN")
        .map(PathBuf::from)
        .filter(|p| p.exists())
}

/// Whether the AEC helper is bundled (VPIO itself is macOS 10.15+; the app floor is already 13.4).
pub fn is_available(app: &AppHandle) -> bool {
    aec_helper_path(app).is_some()
}

/// Spawns the VPIO helper writing the AEC'd mic to `wav_path`. [`stop`] SIGTERMs + finalizes it.
pub struct AecRecorder {
    child: Child,
    /// Exact parent-lifetime capability. The helper exits when this writer's inherited read end
    /// reaches EOF. It must remain live through normal TERM + wait so clean WAV finalization wins.
    lifetime_pipe: Option<std::process::ChildStdin>,
    wav_path: PathBuf,
    started_at: Instant,
}

/// Give the helper a bounded grace period to flush on TERM, then force-kill and reap it. The
/// recorder deliberately retains its stdin writer around this entire call so EOF never races the
/// normal signal-driven finalizer. `Option` keeps Drop panic-free if process inspection fails.
fn terminate_and_reap_aec(child: &mut Child) -> Option<std::process::ExitStatus> {
    let _ = Command::new("/bin/kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status();
    let deadline = Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                return child.wait().ok();
            }
            Err(error) => {
                // A failed state probe is not fresh process-generation proof. Avoid signalling a
                // numeric pid from that ambiguous state; caller teardown closes the exact stdin
                // capability, which makes the helper exit on EOF.
                tracing::warn!(target: "audio", error = %error, "AEC child state unavailable; closing lifetime pipe without another signal");
                return None;
            }
        }
    }
}

impl AecRecorder {
    pub fn start(app: &AppHandle, wav_path: PathBuf) -> Result<Self> {
        let bin =
            aec_helper_path(app).ok_or_else(|| AppError::Audio("AEC helper not bundled".into()))?;
        let mut cmd = Command::new(&bin);
        cmd.arg(&wav_path)
            // Wall-clock cap so a stuck/never-stopped helper self-terminates instead of growing an
            // unbounded scratch WAV (the 91 GB incident). Normal Stop SIGTERMs it long before this.
            .arg(MAX_CAPTURE_SECONDS.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        // F2 (mirror of `SystemAudioRecorder::start`): start from an EMPTY environment and re-add
        // only the minimal non-secret vars the sidecar needs, so MURMUR_DEV_* / API keys / tokens in
        // the app environment can never be inherited by this child. PATH is a fixed system list (the
        // sidecar runs no helpers), HOME is needed for the macOS per-user TCC/container context.
        cmd.env_clear().env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
        for key in ["HOME", "USER", "LOGNAME", "TMPDIR"] {
            if let Ok(val) = std::env::var(key) {
                cmd.env(key, val);
            }
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| AppError::Audio(format!("failed to spawn AEC helper: {e}")))?;
        let Some(lifetime_pipe) = child.stdin.take() else {
            // Exact parent ownership is mandatory: never leave a helper capturing for up to four
            // hours when the write end needed to signal owner death was not created.
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::Audio(
                "AEC helper started without its parent-lifetime pipe".into(),
            ));
        };
        Ok(Self {
            child,
            lifetime_pipe: Some(lifetime_pipe),
            wav_path,
            started_at: Instant::now(),
        })
    }

    /// Host instant the AEC helper was spawned (≈ recording start; the mic ASR feed reuses the
    /// cpal mic anchor, so this is informational).
    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    /// SIGTERM the helper, allow up to three seconds for WAV finalization, then force-kill + reap
    /// it if necessary. Returns the WAV path only after a successful helper exit; `Ok(None)` on
    /// failure so the caller falls back to the raw cpal mic for ASR.
    ///
    /// `self` implements `Drop` (C1), so the teardown runs under [`std::mem::ManuallyDrop`] to
    /// SUPPRESS the drop-guard's second teardown — a clean `stop` reaps the child exactly once (no
    /// double-SIGTERM / recycled-pid risk). Fields are then dropped explicitly (no leak).
    pub fn stop(self) -> Result<Option<PathBuf>> {
        let mut this = std::mem::ManuallyDrop::new(self);
        let status = terminate_and_reap_aec(&mut this.child);

        // Move the fields out of the ManuallyDrop so they drop normally (the recorder's own `Drop`
        // stays suppressed → the child is reaped exactly once above).
        // SAFETY: each field is read exactly once and never touched again.
        let wav_path = unsafe { std::ptr::read(&this.wav_path) };
        let _child = unsafe { std::ptr::read(&this.child) };
        // Extract/drop only after bounded reap: earlier EOF would race signal-driven finalization.
        let lifetime_pipe = unsafe { std::ptr::read(&this.lifetime_pipe) };
        drop(lifetime_pipe);
        let _started_at = unsafe { std::ptr::read(&this.started_at) };

        if status.as_ref().is_some_and(|value| value.success()) && wav_path.exists() {
            Ok(Some(wav_path))
        } else {
            tracing::warn!(
                target: "audio",
                code = ?status.as_ref().and_then(|value| value.code()),
                "AEC helper produced no track; using the raw mic for ASR"
            );
            Ok(None)
        }
    }
}

/// C1 — best-effort teardown for an [`AecRecorder`] DROPPED without a clean [`AecRecorder::stop`]
/// (app-quit mid-recording, a panic, a `take()` into a discard). A std [`Child`] only DETACHES on
/// drop, so this guard sends SIGTERM for clean finalization and reaps it; parent-pipe EOF is the
/// crash-only hard fallback and intentionally does not prove a finalized WAV. After three seconds a
/// stuck helper is SIGKILLed and reaped. After
/// a normal `stop(mut self)` (which CONSUMES self) this never runs on that value → no double-SIGTERM.
/// Panic-free: [`terminate_and_reap_aec`] converts teardown failures to `None`.
impl Drop for AecRecorder {
    fn drop(&mut self) {
        // `lifetime_pipe` auto-drops only after this method returns, so EOF cannot beat bounded
        // TERM→SIGKILL→reap finalization.
        let _ = terminate_and_reap_aec(&mut self.child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // A realistic `ps -axo pid=,ppid=,comm=` fixture (executable path ONLY — no args) covering
    // every verdict class:
    //   32431 — a live Murmur app process (the parent that MUST protect its helper);
    //    5916 — the classic launchd-reparented audiocap orphan (the one we actually found);
    //    7001 — a LIVE helper owned by the running Murmur 32431: MUST be spared;
    //     412 — an unrelated launchd child (not a helper);
    //    8080 — a sysaudio orphan with no scratch arg;
    //    9100 — an audiocap whose parent 4242 is GONE (absent from the snapshot, probe dead) —
    //           the dead-but-not-yet-reparented window the old `ppid == 1` filter missed;
    //    9200 — an aeccap adopted by a live NON-Murmur process (zsh, 5555): kill;
    //    5555 — the live non-Murmur adopter;
    //    6001 — a grep whose ARGUMENTS mention a helper name: must never
    //           be selected — `comm` is `/usr/bin/grep`.
    const PS: &str = "\
32431     1 /Applications/Murmur.app/Contents/MacOS/Murmur
 5916     1 /Users/x/target/debug/meetnotes-audiocap
 7001 32431 /Users/x/target/debug/meetnotes-audiocap
  412     1 /usr/libexec/somethingd
 8080     1 /Users/x/target/debug/meetnotes-sysaudio
 9100  4242 /Users/x/target/debug/meetnotes-audiocap
 9200  5555 /Users/x/target/debug/meetnotes-aeccap
 5555     1 /bin/zsh
 6001  5555 /usr/bin/grep";

    /// The injected liveness probe for the fixture: every pid PRESENT in the snapshot is alive
    /// (matches reality — `ps` and `kill -0` agree for the fixture's processes), 4242 is gone.
    fn fixture_probe(pid: i32) -> bool {
        pid != 4242
    }

    fn verdict_of(pid: i32) -> HelperVerdict {
        helper_decisions(PS, None, fixture_probe)
            .into_iter()
            .find(|(h, _)| h.pid == pid)
            .map(|(_, v)| v)
            .expect("helper pid present in fixture")
    }

    #[test]
    fn detects_launchd_reparented_orphans_without_inspecting_args() {
        let decisions = helper_decisions(PS, None, fixture_probe);
        let blocked: Vec<i32> = decisions
            .into_iter()
            .filter(|(_, v)| *v == HelperVerdict::Block)
            .map(|(h, _)| h.pid)
            .collect();
        assert!(blocked.contains(&5916));
        assert!(blocked.contains(&8080));
    }

    #[test]
    fn detects_helper_whose_parent_is_dead_but_not_yet_reparented() {
        // REGRESSION (the 7h20m incident's third defect): pid 9100's parent 4242 is gone
        // (`kill -0` → ESRCH, absent from the snapshot) but the helper's ppid is still 4242 —
        // the old `ppid == 1`-only filter skipped it forever.
        assert_eq!(verdict_of(9100), HelperVerdict::Block);
    }

    #[test]
    fn detects_helper_adopted_by_a_live_non_murmur_process() {
        // pid 9200's parent 5555 is alive but is /bin/zsh — an adopter that does not own the
        // helper lifecycle, so a new recording must remain blocked.
        assert_eq!(verdict_of(9200), HelperVerdict::Block);
    }

    #[test]
    fn never_blocks_a_live_child_helper() {
        // SAFETY INVARIANT: pid 7001 is a live capture helper of a RUNNING Murmur (ppid 32431).
        // It is a byte-for-byte twin of the orphan except for its parent — it must be spared.
        assert_eq!(verdict_of(7001), HelperVerdict::Spare);
    }

    #[test]
    fn spares_helper_of_a_live_murmur_even_when_the_probe_binary_fails() {
        // Spare-bias: parent 32431 IS in the snapshot as Murmur, so even a probe that reports
        // everything dead (a broken /bin/kill) must not block a live recording's helper.
        let v = helper_decisions(PS, None, |_| false)
            .into_iter()
            .find(|(h, _)| h.pid == 7001)
            .map(|(_, v)| v)
            .unwrap();
        assert_eq!(v, HelperVerdict::Spare);
    }

    #[test]
    fn spares_live_murmur_helper_when_the_murmur_path_has_spaces() {
        // REGRESSION (2026-07-16 review, HIGH): a live Murmur at a spaced path (Finder
        // "keep both" → `Murmur 2.app`) must still resolve to basename `Murmur` — the old
        // args-bearing token parse resolved the parent wrong and targeted its mid-recording helper.
        let comm = "\
77001     1 /Applications/Murmur 2.app/Contents/MacOS/Murmur
77002 77001 /Applications/Murmur 2.app/Contents/Resources/meetnotes-audiocap
77003 77001 /Applications/Murmur 2.app/Contents/Resources/murmur-brain";
        let decisions = helper_decisions(comm, None, |_| true);
        assert_eq!(
            decisions.len(),
            2,
            "both helpers of the spaced-path Murmur found"
        );
        for (h, v) in decisions {
            assert_eq!(
                v,
                HelperVerdict::Spare,
                "helper {} of a LIVE spaced-path Murmur must be spared",
                h.pid
            );
        }
    }

    #[test]
    fn unlimited_width_snapshot_keeps_long_dev_helper_basename_visible() {
        assert!(PROCESS_SNAPSHOT_ARGS.contains(&"-axww"));
        let long_prefix = "x".repeat(400);
        let snapshot = format!(
            "77001     1 /Applications/Murmur.app/Contents/MacOS/Murmur\n77002 77001 /var/folders/{long_prefix}/target/debug/meetnotes-audiocap"
        );
        let decisions = helper_decisions(&snapshot, None, |_| true);
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].0.pid, 77002);
        assert_eq!(decisions[0].1, HelperVerdict::Spare);
    }

    #[test]
    fn recognizes_legacy_brain_name_only_for_orphan_detection() {
        // Compatibility fixture for a helper stranded by a pre-rename Murmur build. Production
        // resolution/spawn uses only `murmur-brain`; this parser-only legacy name prevents old
        // multi-GB orphan processes from surviving an upgrade.
        let comm = "88003     1 /Applications/Murmur.app/Contents/Resources/meetnotes-brain";
        let decisions = helper_decisions(comm, None, |_| false);
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].0.pid, 88003);
        assert_eq!(decisions[0].1, HelperVerdict::Block);
    }

    #[test]
    fn never_selects_a_process_that_merely_mentions_a_helper_in_its_args() {
        // REGRESSION (2026-07-16 review, MEDIUM): `grep -r meetnotes-audiocap …` (pid 6001,
        // live non-Murmur parent) must not be selected as a helper — the old any-token match on
        // the args-bearing `command=` column made it a target. Detection now comes from `comm=`.
        let decisions = helper_decisions(PS, None, fixture_probe);
        assert!(
            !decisions.iter().any(|(h, _)| h.pid == 6001),
            "a process mentioning a helper name in its ARGS is not a capture helper"
        );
        assert!(parse_capture_helpers(PS).iter().all(|h| h.pid != 6001));
    }

    #[test]
    fn verdict_truth_table() {
        use HelperVerdict::*;
        assert_eq!(helper_verdict(1, true, false), Block); // launchd orphan
        assert_eq!(helper_verdict(4242, false, false), Block); // dead parent
        assert_eq!(helper_verdict(5555, true, false), Block); // live non-Murmur adopter
        assert_eq!(helper_verdict(32431, true, true), Spare); // live Murmur owner
                                                              // A dead-but-recycled pid that LOOKS like Murmur by name yet fails the liveness probe
                                                              // AND snapshot: still blocked (alive is required for a spare).
        assert_eq!(helper_verdict(7777, false, true), Block);
    }

    #[test]
    fn murmur_basename_matching() {
        assert!(is_murmur_basename("Murmur", None));
        assert!(is_murmur_basename("murmur-dev", Some("murmur-dev")));
        assert!(!is_murmur_basename("murmur", None)); // exact case — no fuzzy matching
                                                      // A zombie parent is a dead parent — BOTH zombie renderings must fail the match:
        assert!(!is_murmur_basename("(Murmur)", None)); // the BSD-man-page-documented form
        assert!(!is_murmur_basename("<defunct>", None)); // what Darwin 25 actually renders
        assert!(!is_murmur_basename("zsh", Some("Murmur")));
    }

    #[test]
    fn ignores_non_helper_and_malformed_lines() {
        assert!(parse_capture_helpers("  412     1 /usr/libexec/somethingd").is_empty());
        assert!(parse_capture_helpers("garbage\n\n123 abc def").is_empty());
        assert!(parse_capture_helpers("").is_empty());
        assert!(helper_decisions("", None, |_| true).is_empty());
        assert!(!capture_snapshot_is_unambiguous(
            "not-a-pid /tmp/meetnotes-aeccap"
        ));
        assert!(capture_snapshot_is_unambiguous(
            "not-a-pid /usr/bin/unrelated"
        ));
    }

    #[test]
    fn basename_map_resolves_parents() {
        let map = parse_process_basenames(PS);
        assert_eq!(map.get(&32431).map(String::as_str), Some("Murmur"));
        assert_eq!(map.get(&5555).map(String::as_str), Some("zsh"));
        assert!(!map.contains_key(&4242)); // the dead parent is absent from the snapshot
                                           // Spaced executable path: the WHOLE final column is the path (comm carries no args).
        let spaced = parse_process_basenames(
            "  617     1 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        );
        assert_eq!(map.get(&6001).map(String::as_str), Some("grep"));
        assert_eq!(spaced.get(&617).map(String::as_str), Some("Google Chrome"));
    }

    #[test]
    fn scratch_cleanup_accepts_only_direct_regular_child_of_selected_temp_root() {
        let sandbox =
            std::env::temp_dir().join(format!("murmur-aec-path-contract-{}", uuid::Uuid::new_v4()));
        let allowed_root = sandbox.join("allowed");
        let outside_root = sandbox.join("outside");
        std::fs::create_dir_all(&allowed_root).expect("create allowed temp fixture root");
        std::fs::create_dir_all(&outside_root).expect("create outside temp fixture root");
        let valid = allowed_root.join("meetnotes-aec-valid.wav");
        let outside = outside_root.join("meetnotes-aec-outside.wav");
        let nested_dir = allowed_root.join("nested");
        let nested = nested_dir.join("meetnotes-aec-nested.wav");
        let escaped = nested_dir.join("..").join("meetnotes-aec-valid.wav");
        let symlink = allowed_root.join("meetnotes-aec-symlink.wav");
        let symlink_parent = allowed_root.join("linked-parent");
        let through_symlink_parent = symlink_parent.join("meetnotes-aec-parent-link.wav");
        std::fs::create_dir_all(&nested_dir).expect("create nested fixture root");
        std::fs::write(&valid, b"valid").expect("write valid temp scratch fixture");
        std::fs::write(&outside, b"outside").expect("write outside scratch fixture");
        std::fs::write(&nested, b"nested").expect("write nested scratch fixture");
        std::os::unix::fs::symlink(&outside, &symlink).expect("create final-component symlink");
        std::os::unix::fs::symlink(&outside_root, &symlink_parent)
            .expect("create parent-directory symlink");

        assert!(verified_temp_scratch_path_in(&valid, &allowed_root).is_some());
        assert!(verified_temp_scratch_path_in(&outside, &allowed_root).is_none());
        assert!(verified_temp_scratch_path_in(&nested, &allowed_root).is_none());
        assert!(verified_temp_scratch_path_in(&escaped, &allowed_root).is_none());
        assert!(verified_temp_scratch_path_in(&symlink, &allowed_root).is_none());
        assert!(verified_temp_scratch_path_in(&through_symlink_parent, &allowed_root).is_none());
        assert!(
            outside.exists(),
            "validation must not mutate rejected targets"
        );

        std::fs::remove_dir_all(sandbox).expect("remove scratch path fixtures");
    }

    /// C1 (RED-before-GREEN): an `AecRecorder` DROPPED without a clean `stop()` (app-quit
    /// mid-recording) must boundedly TERM→SIGKILL→REAP its VPIO helper child. Parent-pipe EOF alone
    /// bounds the helper but cannot reap it from the still-live host. Proof: after drop the pid has
    /// no process-table entry at all.
    #[test]
    fn drop_without_stop_boundedly_reaps_the_aec_child() {
        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn a dummy long-lived child");
        let lifetime_pipe = child.stdin.take().expect("dummy lifetime pipe");
        let pid = child.id();
        let rec = AecRecorder {
            child,
            lifetime_pipe: Some(lifetime_pipe),
            wav_path: PathBuf::from("/nonexistent/dummy.wav"),
            started_at: Instant::now(),
        };
        assert!(
            pid_alive(pid as i32),
            "the dummy child is alive before drop"
        );

        drop(rec); // no stop() — the app-quit-mid-recording path.

        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !pid_alive(pid as i32),
            "dropping without stop() must boundedly reap the AEC helper — no orphan, no zombie"
        );
    }

    #[test]
    fn aec_helper_uses_stdin_lifetime_capability_not_pid_watcher() {
        let source = include_str!("../../aeccap/aeccap.swift");
        assert!(source.contains("read(STDIN_FILENO"));
        assert!(source.contains("errno == EINTR"));
        assert!(source.contains("CapturePhase"));
        assert!(source.contains("_exit(3)"));
        assert!(source.contains("_exit(6)"));
        assert!(source.contains("sigQueue.async { requestStop(0) }"));
        assert!(source.contains("markCaptureReady()"));
        assert!(!source.contains("makeProcessSource"));
        assert!(!source.contains("parentWatch"));
    }
}
