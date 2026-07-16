//! AEC microphone capture via the VoiceProcessingIO Swift helper (`aeccap`).
//!
//! EXPERIMENTAL + opt-in (`aec_enabled`, default off). Runs IN PARALLEL with the primary cpal mic
//! — cpal stays the level meter / mute / live-captions / archive source and the fallback; the
//! AEC'd WAV this helper produces becomes the mic ASR feed when present. Any failure (incl. the
//! cpal/VPIO coexistence not working on the real device) leaves the recording on the raw cpal mic.
//!
//! ⚠️ The cpal/VPIO coexistence on one mic + real echo cancellation need a SIGNED build on a real
//! Mac with a live call — they are NOT verifiable headless (see `aeccap/aeccap.swift`).

use std::path::PathBuf;
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

/// Best-effort: delete stale capture scratch WAVs (`meetnotes-aec-*.wav` / `meetnotes-sys-*.wav`)
/// left in the temp dir by a previous crashed/stuck/never-stopped session. Called once at startup,
/// where nothing is recording yet, so any scratch older than an hour is an orphan — removing it
/// reclaims disk (a stuck VPIO helper once left a 91 GB file). Touches ONLY the OS temp dir, never
/// the app-data audio dir; logs IDs/counts only, no PII.
pub fn sweep_stale_scratch() {
    let dir = std::env::temp_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0u32;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_scratch = (name.starts_with("meetnotes-aec-") || name.starts_with("meetnotes-sys-"))
            && name.ends_with(".wav");
        if !is_scratch {
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
    if removed > 0 {
        tracing::info!(target: "audio", removed, "swept stale capture scratch WAVs at startup");
    }
}

/// Basenames of the helper binaries the app spawns to CAPTURE audio. Any of these that survives
/// its parent is an ORPHAN from a session that died without a clean Stop — a crash, a force-quit,
/// or (in dev) a `tauri dev` hot-rebuild SIGKILLing the app mid-recording. An orphan keeps
/// capturing system audio to its temp WAV until its own 4h self-cap — gigabytes of dead-session
/// audio (a real one survived 7h20m / 2+ GB). Whether a surviving helper is an orphan is decided
/// per-parent by [`helper_verdict`]; a helper owned by a LIVE Murmur process is never touched.
const CAPTURE_HELPERS: [&str; 4] = [
    "meetnotes-sysaudio",
    "meetnotes-audiocap",
    "meetnotes-aeccap",
    // The on-device brain sidecar: a launchd-reparented `meetnotes-brain` (the app was SIGKILL'd
    // mid-generation, or `kill_on_quit`'s bounded reap abandoned a slow-dying child) keeps a
    // multi-GB model resident until its own idle self-exit. It carries NO scratch WAV arg, so the
    // `wav` field is `None` for it (nothing to delete) — the SIGTERM alone reclaims its RAM.
    "meetnotes-brain",
];

/// SIGTERM any ORPHANED capture helper and delete its scratch WAV. Best-effort, called at startup
/// and again at the top of every `start_recording`. This closes the gap [`sweep_stale_scratch`]
/// can't: a *live* orphan keeps its WAV mtime fresh, so the file-age sweep never reclaims it, and
/// the child runs for hours (a real audiocap orphan once outlived its parent by 7h20m, writing
/// 2+ GB — the old `ppid == 1`-only filter plus the once-at-launch schedule let it survive).
///
/// A helper is judged by [`helper_verdict`] on what we know about its PARENT: reparented to
/// launchd, a dead ppid, or a live-but-not-Murmur ppid all mean KILL; only a helper owned by a
/// LIVE Murmur process (a genuinely concurrent instance, or this process) is spared — so a
/// mid-recording helper of ours or of another running Murmur is never touched.
pub fn reap_orphaned_capture_helpers() {
    // Identity comes from `comm=` — the executable path ONLY, no arguments — so a process that
    // merely MENTIONS a helper name in its args (`grep -r meetnotes-audiocap …`) can never be
    // selected, and a parent at a path with an embedded space (`/Applications/Murmur 2.app/…`,
    // Finder "keep both") still resolves to its true basename. Parsing the args-bearing
    // `command=` column token-wise for identity was the root cause of both 2026-07-16 review
    // findings (live-Murmur helpers killed / innocent processes killed).
    let Ok(out) = std::process::Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,comm="])
        .output()
    else {
        return;
    };
    let snapshot = String::from_utf8_lossy(&out.stdout);
    let self_exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    // The scratch-WAV path only exists in the args-bearing `command=` view; it is fetched lazily
    // (only when a helper actually survived) and consulted ONLY for pids already identified as
    // helpers via `comm=` — never for identity.
    let command_snapshot = || {
        std::process::Command::new("/bin/ps")
            .args(["-axo", "pid=,command="])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let mut reaped = 0u32;
    for (helper, verdict) in reap_decisions(&snapshot, command_snapshot, self_exe.as_deref(), pid_alive) {
        match verdict {
            HelperVerdict::Spare => {
                // Ids only (no paths/PII). DEBUG, not WARN: a spared helper is ROUTINE state —
                // our own live capture children and the long-lived `meetnotes-brain` sidecar land
                // here on every sweep.
                tracing::debug!(
                    target: "audio",
                    helper_pid = helper.pid,
                    parent_pid = helper.ppid,
                    "capture helper owned by a live Murmur process — leaving it alone"
                );
            }
            HelperVerdict::Kill => {
                // SIGTERM (not SIGKILL): let the helper's signal handler close its file, then
                // reclaim the scratch.
                let killed = std::process::Command::new("/bin/kill")
                    .arg("-TERM")
                    .arg(helper.pid.to_string())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if killed {
                    reaped += 1;
                    if let Some(path) = helper.wav {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
        }
    }
    if reaped > 0 {
        // WARN, not INFO: an orphan reaching a sweep means a prior session leaked a capture helper.
        tracing::warn!(target: "audio", reaped, "reaped orphaned capture helper(s)");
    }
}

/// One surviving capture-helper process from the `ps` snapshot.
#[derive(Debug, PartialEq, Eq, Clone)]
struct HelperProc {
    pid: i32,
    ppid: i32,
    /// The helper's capture-scratch argument (`meetnotes-sys-*.wav` / `meetnotes-aec-*.wav`),
    /// when present, so the caller can delete it after a kill.
    wav: Option<std::path::PathBuf>,
}

/// The fate of one surviving capture helper.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum HelperVerdict {
    /// Orphaned (or adopted by a non-Murmur process) — SIGTERM it + delete its scratch WAV.
    Kill,
    /// A live Murmur process owns it (a genuinely concurrent instance, or us) — never touch it.
    Spare,
}

/// PURE verdict for ONE surviving capture helper, given what we know about its parent.
/// Unit-testable without live processes.
///
/// KILL when the parent is gone or was never ours:
///   - `ppid == 1` — reparented to launchd, the classic orphan;
///   - `!ppid_alive` — the parent pid no longer exists (`kill(ppid, 0)` → ESRCH): a dead-or-dying
///     parent whose child has not (yet) reparented — the window the old `ppid == 1`-only filter
///     missed;
///   - `!ppid_is_murmur` — the ppid is alive but is NOT a Murmur process (a recycled pid, or an
///     adopter that will never SIGTERM the helper).
///
/// SPARE only when a LIVE Murmur process owns it.
fn helper_verdict(ppid: i32, ppid_alive: bool, ppid_is_murmur: bool) -> HelperVerdict {
    if ppid == 1 || !ppid_alive || !ppid_is_murmur {
        HelperVerdict::Kill
    } else {
        HelperVerdict::Spare
    }
}

/// Testable core of [`reap_orphaned_capture_helpers`]: parse the `comm=` snapshot and attach a
/// [`HelperVerdict`] to every surviving capture helper. `ppid_alive_probe` is the injected
/// liveness oracle (`kill -0` in production); parent liveness is the probe OR-ed with the parent's
/// presence in the SAME snapshot, so a broken probe binary can never misread a live concurrent
/// Murmur's parent as dead and kill its mid-recording helper (the spare-biased direction — a
/// missed orphan is retried at the next sweep; a wrongly killed live helper loses a recording).
/// `command_snapshot` (the args-bearing `ps -axo pid=,command=` view) is invoked lazily, only when
/// a helper survived, and ONLY to recover the scratch-WAV argument — never for identity.
fn reap_decisions<F: Fn(i32) -> bool, G: FnOnce() -> String>(
    comm_snapshot: &str,
    command_snapshot: G,
    self_exe: Option<&str>,
    ppid_alive_probe: F,
) -> Vec<(HelperProc, HelperVerdict)> {
    let helpers = parse_capture_helpers(comm_snapshot);
    if helpers.is_empty() {
        return Vec::new();
    }
    let basenames = parse_process_basenames(comm_snapshot);
    let wavs = parse_scratch_wavs(&command_snapshot());
    helpers
        .into_iter()
        .map(|mut h| {
            h.wav = wavs.get(&h.pid).cloned();
            let alive = basenames.contains_key(&h.ppid) || ppid_alive_probe(h.ppid);
            let murmur = basenames
                .get(&h.ppid)
                .map(|b| is_murmur_basename(b, self_exe))
                .unwrap_or(false);
            let verdict = helper_verdict(h.ppid, alive, murmur);
            (h, verdict)
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
/// basename of OUR OWN executable (covers a renamed dev profile). NOTE: `ps` renders a zombie as
/// `(Murmur)` — parenthesized, so it correctly does NOT match (a zombie parent is a dead parent).
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
/// path is the whole final column; see [`split_numeric_prefix`]). The `wav` field is filled later
/// from the separate `command=` snapshot ([`parse_scratch_wavs`]).
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
            wav: None,
        });
    }
    out
}

/// Pure parser for the SAME `comm=` snapshot: `pid → executable basename` for every process, so a
/// helper's ppid can be resolved to the process that owns it (and its liveness read off the
/// snapshot) without a second `ps` round-trip. Because `comm` carries no arguments, a Murmur at
/// `/Applications/Murmur 2.app/Contents/MacOS/Murmur` resolves to `Murmur` — the 2026-07-16
/// review proved the old args-bearing token parse resolved it wrong and KILLED that live Murmur's
/// mid-recording helpers.
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

/// Pure parser for `ps -axo pid=,command=` output: `pid → scratch-WAV argument` (the same
/// `meetnotes-sys-*.wav` / `meetnotes-aec-*.wav` pattern `sweep_stale_scratch` reclaims), for
/// deleting a killed helper's scratch. Consulted ONLY for pids already identified as helpers via
/// the `comm=` snapshot — never for identity. (Token scan assumes the scratch path itself carries
/// no embedded spaces — true for the OS temp dir; this is best-effort cleanup.)
fn parse_scratch_wavs(command_snapshot: &str) -> std::collections::HashMap<i32, std::path::PathBuf> {
    let mut out = std::collections::HashMap::new();
    for line in command_snapshot.lines() {
        let Some((nums, command)) = split_numeric_prefix(line, 1) else {
            continue;
        };
        let wav = command.split_whitespace().find(|t| {
            let base = t.rsplit('/').next().unwrap_or(t);
            (base.starts_with("meetnotes-sys-") || base.starts_with("meetnotes-aec-"))
                && base.ends_with(".wav")
        });
        if let Some(wav) = wav {
            out.insert(nums[0], std::path::PathBuf::from(wav));
        }
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
    wav_path: PathBuf,
    started_at: Instant,
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
            .stdin(Stdio::null())
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
        let child = cmd
            .spawn()
            .map_err(|e| AppError::Audio(format!("failed to spawn AEC helper: {e}")))?;
        Ok(Self {
            child,
            wav_path,
            started_at: Instant::now(),
        })
    }

    /// Host instant the AEC helper was spawned (≈ recording start; the mic ASR feed reuses the
    /// cpal mic anchor, so this is informational).
    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    /// SIGTERM the helper, wait, and return the WAV path if it captured anything. `Ok(None)` on any
    /// failure so the caller falls back to the raw cpal mic for ASR.
    ///
    /// `self` implements `Drop` (C1), so the teardown runs under [`std::mem::ManuallyDrop`] to
    /// SUPPRESS the drop-guard's second teardown — a clean `stop` reaps the child exactly once (no
    /// double-SIGTERM / recycled-pid risk). Fields are then dropped explicitly (no leak).
    pub fn stop(self) -> Result<Option<PathBuf>> {
        let mut this = std::mem::ManuallyDrop::new(self);
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(this.child.id().to_string())
            .status();
        let wait_res = this.child.wait();

        // Move the fields out of the ManuallyDrop so they drop normally (the recorder's own `Drop`
        // stays suppressed → the child is reaped exactly once above).
        // SAFETY: each field is read exactly once and never touched again.
        let wav_path = unsafe { std::ptr::read(&this.wav_path) };
        let _child = unsafe { std::ptr::read(&this.child) };
        let _started_at = unsafe { std::ptr::read(&this.started_at) };

        let status =
            wait_res.map_err(|e| AppError::Audio(format!("waiting on AEC helper: {e}")))?;
        if status.success() && wav_path.exists() {
            Ok(Some(wav_path))
        } else {
            tracing::warn!(
                target: "audio",
                code = ?status.code(),
                "AEC helper produced no track; using the raw mic for ASR"
            );
            Ok(None)
        }
    }
}

/// C1 — best-effort teardown for an [`AecRecorder`] DROPPED without a clean [`AecRecorder::stop`]
/// (app-quit mid-recording, a panic, a `take()` into a discard). A std [`Child`] only DETACHES on
/// drop, so without this the VPIO helper reparents to launchd and keeps writing an unbounded scratch
/// WAV until its 4h self-cap (the 91 GB incident). SIGTERM (not SIGKILL) so it flushes + closes the
/// WAV exactly as `stop()` does, then `wait()` to reap. After a normal `stop(mut self)` (which
/// CONSUMES self) this never runs on that value → no double-SIGTERM. Panic-free (`let _ =`).
impl Drop for AecRecorder {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status();
        let _ = self.child.wait();
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
    //    6001 — a grep whose ARGUMENTS mention a helper name (in PS_COMMAND below): must never
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

    // The matching args-bearing `ps -axo pid=,command=` view — consulted ONLY for scratch-WAV
    // recovery of already-identified helpers, never for identity.
    const PS_COMMAND: &str = "\
32431 /Applications/Murmur.app/Contents/MacOS/Murmur
 5916 /Users/x/target/debug/meetnotes-audiocap /var/folders/sl/T/meetnotes-sys-83191e88.wav 14400
 7001 /Users/x/target/debug/meetnotes-audiocap /var/folders/sl/T/meetnotes-sys-live-abc.wav 14400
  412 /usr/libexec/somethingd
 8080 /Users/x/target/debug/meetnotes-sysaudio
 9100 /Users/x/target/debug/meetnotes-audiocap /var/folders/sl/T/meetnotes-sys-dead.wav 14400
 9200 /Users/x/target/debug/meetnotes-aeccap /var/folders/sl/T/meetnotes-aec-zzz.wav 14400
 5555 -zsh
 6001 grep -r meetnotes-audiocap /Users/x/backup/meetnotes-sys-precious.wav";

    /// The injected liveness probe for the fixture: every pid PRESENT in the snapshot is alive
    /// (matches reality — `ps` and `kill -0` agree for the fixture's processes), 4242 is gone.
    fn fixture_probe(pid: i32) -> bool {
        pid != 4242
    }

    fn verdict_of(pid: i32) -> HelperVerdict {
        reap_decisions(PS, || PS_COMMAND.to_string(), None, fixture_probe)
            .into_iter()
            .find(|(h, _)| h.pid == pid)
            .map(|(_, v)| v)
            .expect("helper pid present in fixture")
    }

    #[test]
    fn kills_launchd_reparented_orphans_and_extracts_scratch() {
        let decisions = reap_decisions(PS, || PS_COMMAND.to_string(), None, fixture_probe);
        let killed: Vec<(i32, Option<PathBuf>)> = decisions
            .into_iter()
            .filter(|(_, v)| *v == HelperVerdict::Kill)
            .map(|(h, _)| (h.pid, h.wav))
            .collect();
        assert!(killed.contains(&(
            5916,
            Some(PathBuf::from("/var/folders/sl/T/meetnotes-sys-83191e88.wav"))
        )));
        assert!(killed.contains(&(8080, None)));
    }

    #[test]
    fn kills_helper_whose_parent_is_dead_but_not_yet_reparented() {
        // REGRESSION (the 7h20m incident's third defect): pid 9100's parent 4242 is gone
        // (`kill -0` → ESRCH, absent from the snapshot) but the helper's ppid is still 4242 —
        // the old `ppid == 1`-only filter skipped it forever.
        assert_eq!(verdict_of(9100), HelperVerdict::Kill);
    }

    #[test]
    fn kills_helper_adopted_by_a_live_non_murmur_process() {
        // pid 9200's parent 5555 is alive but is /bin/zsh — an adopter that will never SIGTERM
        // the helper. Kill it.
        assert_eq!(verdict_of(9200), HelperVerdict::Kill);
    }

    #[test]
    fn never_reaps_a_live_child_helper() {
        // SAFETY INVARIANT: pid 7001 is a live capture helper of a RUNNING Murmur (ppid 32431).
        // It is a byte-for-byte twin of the orphan except for its parent — it must NEVER be killed.
        assert_eq!(verdict_of(7001), HelperVerdict::Spare);
    }

    #[test]
    fn spares_helper_of_a_live_murmur_even_when_the_probe_binary_fails() {
        // Spare-bias: parent 32431 IS in the snapshot as Murmur, so even a probe that reports
        // everything dead (a broken /bin/kill) must not turn a live recording's helper into a kill.
        let v = reap_decisions(PS, || PS_COMMAND.to_string(), None, |_| false)
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
        // args-bearing token parse resolved the parent wrong and KILLED its mid-recording helper
        // (and the app's own live brain sidecar on every start_recording).
        let comm = "\
77001     1 /Applications/Murmur 2.app/Contents/MacOS/Murmur
77002 77001 /Applications/Murmur 2.app/Contents/Resources/meetnotes-audiocap
77003 77001 /Applications/Murmur 2.app/Contents/Resources/meetnotes-brain";
        let command = "\
77001 /Applications/Murmur 2.app/Contents/MacOS/Murmur
77002 /Applications/Murmur 2.app/Contents/Resources/meetnotes-audiocap /var/folders/sl/T/meetnotes-sys-x.wav 14400
77003 /Applications/Murmur 2.app/Contents/Resources/meetnotes-brain";
        let decisions = reap_decisions(comm, || command.to_string(), None, |_| true);
        assert_eq!(decisions.len(), 2, "both helpers of the spaced-path Murmur found");
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
    fn never_selects_a_process_that_merely_mentions_a_helper_in_its_args() {
        // REGRESSION (2026-07-16 review, MEDIUM): `grep -r meetnotes-audiocap …` (pid 6001,
        // live non-Murmur parent) must not be selected as a helper — the old any-token match on
        // the args-bearing `command=` column made it a Kill (SIGTERM of an innocent process, plus
        // deletion of its matching `.wav` ARGUMENT). Identity now comes from `comm=` only.
        let decisions = reap_decisions(PS, || PS_COMMAND.to_string(), None, fixture_probe);
        assert!(
            !decisions.iter().any(|(h, _)| h.pid == 6001),
            "a process mentioning a helper name in its ARGS is not a capture helper"
        );
        assert!(parse_capture_helpers(PS).iter().all(|h| h.pid != 6001));
    }

    #[test]
    fn verdict_truth_table() {
        use HelperVerdict::*;
        assert_eq!(helper_verdict(1, true, false), Kill); // launchd orphan
        assert_eq!(helper_verdict(4242, false, false), Kill); // dead parent
        assert_eq!(helper_verdict(5555, true, false), Kill); // live non-Murmur adopter
        assert_eq!(helper_verdict(32431, true, true), Spare); // live Murmur owner
        // A dead-but-recycled pid that LOOKS like Murmur by name yet fails the liveness probe
        // AND snapshot: still a kill (alive is required for a spare).
        assert_eq!(helper_verdict(7777, false, true), Kill);
    }

    #[test]
    fn murmur_basename_matching() {
        assert!(is_murmur_basename("Murmur", None));
        assert!(is_murmur_basename("murmur-dev", Some("murmur-dev")));
        assert!(!is_murmur_basename("murmur", None)); // exact case — no fuzzy matching
        assert!(!is_murmur_basename("(Murmur)", None)); // a zombie parent is a dead parent
        assert!(!is_murmur_basename("zsh", Some("Murmur")));
    }

    #[test]
    fn ignores_non_helper_and_malformed_lines() {
        assert!(parse_capture_helpers("  412     1 /usr/libexec/somethingd").is_empty());
        assert!(parse_capture_helpers("garbage\n\n123 abc def").is_empty());
        assert!(parse_capture_helpers("").is_empty());
        assert!(reap_decisions("", String::new, None, |_| true).is_empty());
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
    fn scratch_wavs_extracted_by_pid_from_the_command_view() {
        let wavs = parse_scratch_wavs(PS_COMMAND);
        assert_eq!(
            wavs.get(&5916),
            Some(&PathBuf::from("/var/folders/sl/T/meetnotes-sys-83191e88.wav"))
        );
        assert!(!wavs.contains_key(&8080)); // sysaudio with no scratch arg
        assert!(!wavs.contains_key(&412)); // non-helper without a matching arg
    }

    /// Whether `pid` still has a process-table entry (any state). None once fully reaped.
    fn ps_present(pid: u32) -> bool {
        std::process::Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false)
    }

    /// C1 (RED-before-GREEN): an `AecRecorder` DROPPED without a clean `stop()` (app-quit
    /// mid-recording) must SIGTERM + REAP its VPIO helper child. A std `Child` merely DETACHES on
    /// drop — so before the added `impl Drop` the child would keep running (reparented to launchd,
    /// growing an unbounded scratch WAV up to the 4h cap — the 91 GB incident) and never be reaped.
    /// Proof: after the drop the pid has no process-table entry at all.
    #[test]
    fn drop_without_stop_reaps_the_aec_child() {
        let child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn a dummy long-lived child");
        let pid = child.id();
        let rec = AecRecorder {
            child,
            wav_path: PathBuf::from("/nonexistent/dummy.wav"),
            started_at: Instant::now(),
        };
        assert!(ps_present(pid), "the dummy child is alive before drop");

        drop(rec); // no stop() — the app-quit-mid-recording path.

        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !ps_present(pid),
            "dropping without stop() must SIGTERM + reap the AEC helper — no orphan, no zombie"
        );
    }
}
