//! System-audio capture via the ScreenCaptureKit Swift sidecar (compiled by `build.rs`).
//!
//! ⚠️ RUNTIME-UNVERIFIED in a headless build: actually capturing system audio needs an
//! interactive desktop session + the Screen Recording (TCC) permission + live audio. The
//! pieces verified here are: the sidecar compiles, spawn/stop plumbing, and graceful
//! degrade-to-None when the sidecar can't capture (e.g. permission denied — it exits 3).

use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};

use crate::error::{AppError, Result};

/// One stderr line from a system-capture helper announcing its FIRST audio buffer — the true
/// capture-start marker (the process-spawn instant precedes SCK/tap setup by hundreds of ms).
pub(crate) fn is_first_frame_line(line: &str) -> bool {
    line.trim_end().ends_with("first-frame")
}

/// Filename of the sidecar — both inside `Contents/Resources` of a shipped `.app` and at the
/// dev `OUT_DIR`.
const SIDECAR_NAME: &str = "meetnotes-sysaudio";

/// Path to the system-audio sidecar binary. Resolution order:
///
/// 1. The bundled resource inside the distributed `.app`
///    (`Contents/Resources/meetnotes-sysaudio`), resolved at RUNTIME via Tauri's resource dir
///    — this is the ONLY path that exists in a shipped, notarized build.
/// 2. The compile-time `SYSAUDIO_BIN` (an absolute `OUT_DIR` path baked in by `build.rs`) —
///    a DEV-ONLY fallback; it does not exist on an end-user machine, which is exactly why (1)
///    is required for production. (Shipping with only (2) was the latent mic-only regression.)
pub fn sidecar_path(app: &AppHandle) -> Option<PathBuf> {
    if let Ok(p) = app
        .path()
        .resolve(SIDECAR_NAME, tauri::path::BaseDirectory::Resource)
    {
        if p.exists() {
            return Some(p);
        }
    }
    option_env!("SYSAUDIO_BIN")
        .map(PathBuf::from)
        .filter(|p| p.exists())
}

/// Whether this build can capture system audio — the Core Audio tap (macOS 14.4+) OR the
/// ScreenCaptureKit sidecar (13–14.3), bundled resource or dev fallback.
pub fn is_available(app: &AppHandle) -> bool {
    crate::audio::tap::is_available(app) || sidecar_path(app).is_some()
}

/// Pick the system-audio helper binary: prefer the Core Audio process tap (macOS 14.4+,
/// app-scoped global-minus-self), else the ScreenCaptureKit sidecar (13–14.3). Both helpers
/// share the `<wav_path> [maxSeconds]` + SIGTERM-to-finalize protocol, so the spawn path is
/// identical regardless of which is chosen.
fn select_helper(app: &AppHandle) -> Option<PathBuf> {
    if crate::audio::tap::is_available(app) {
        if let Some(p) = crate::audio::tap::tap_helper_path(app) {
            tracing::info!(target: "audio", "system capture path: Core Audio process tap");
            return Some(p);
        }
    }
    if let Some(p) = sidecar_path(app) {
        tracing::info!(target: "audio", "system capture path: ScreenCaptureKit sidecar");
        return Some(p);
    }
    None
}

/// Spawns the sidecar to capture system audio into `wav_path`. Call [`stop`] to finalize.
pub struct SystemAudioRecorder {
    child: Child,
    /// Parent-lifetime capability. The helper blocks on its inherited stdin read end and treats
    /// EOF as exact owner death. Keep this writer open through TERM + reap so normal finalization
    /// cannot race the parent-death path.
    lifetime_pipe: Option<ChildStdin>,
    wav_path: PathBuf,
    /// Host wall-clock instant when the sidecar was spawned (capture start). The mic (cpal) and
    /// this ScreenCaptureKit stream run on INDEPENDENT clocks, so the wall-clock merge anchors
    /// each stream's segments to its own host start instead of aligning by sample count (which
    /// drifts seconds/hour). See `audio::merge`.
    started_at: std::time::Instant,
    /// True capture-start anchor: `Instant::now()` taken when the helper's `first-frame` stderr
    /// line arrives. The sidecar's boot + SCK/tap setup (~100–500 ms) precedes the first buffer,
    /// so this is a much tighter merge/mix anchor than the spawn instant. Set once.
    first_frame_at: Arc<OnceLock<std::time::Instant>>,
    /// Drains the sidecar's stderr (captures the anchor + prevents the pipe buffer ever blocking
    /// the child). Joined in [`stop`].
    stderr_reader: Option<std::thread::JoinHandle<()>>,
    stderr_done: Option<mpsc::Receiver<()>>,
}

/// Owned teardown result. Even a non-zero helper exit or forced reap returns the canonical path so
/// the recording coordinator must either adopt a verified partial WAV or proof-delete that exact
/// inode. Helper failures can therefore never degrade to an untracked `Ok(None)` orphan.
pub struct SystemAudioStopOutcome {
    path: PathBuf,
    started_at: Instant,
    helper_succeeded: bool,
    /// Positive protocol proof that the helper reached its ready phase and ran the clean finalizer.
    /// Exit 5 is a finalized capture with a latched I/O fault; exit 3 / signal / unknown status is
    /// pre-ready or unproven and its WAV must never be adopted merely because it parses.
    helper_finalized: bool,
}

fn helper_exit_proves_finalized(success: bool, code: Option<i32>) -> bool {
    success || code == Some(5)
}

fn terminate_and_reap(child: &mut Child) -> Option<std::process::ExitStatus> {
    let _ = Command::new("/bin/kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                return child.wait().ok();
            }
            Err(error) => {
                // Once process inspection itself fails we no longer have fresh proof that the
                // numeric pid still denotes this child. Do not turn an error into a raw signal;
                // returning drops the exact stdin capability and the helper self-terminates.
                tracing::warn!(target: "audio", error = %error, "system-audio child state unavailable; closing lifetime pipe without another signal");
                return None;
            }
        }
    }
}

impl SystemAudioStopOutcome {
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }
    pub(crate) fn started_at(&self) -> Instant {
        self.started_at
    }
    pub(crate) fn helper_succeeded(&self) -> bool {
        self.helper_succeeded
    }
    pub(crate) fn helper_finalized(&self) -> bool {
        self.helper_finalized
    }
}

impl SystemAudioRecorder {
    /// Spawn the sidecar writing to `wav_path`. Errors only if the sidecar can't be
    /// spawned at all; a *capture* failure (no permission) surfaces later in [`stop`].
    pub fn start(app: &AppHandle, wav_path: PathBuf) -> Result<Self> {
        let bin = select_helper(app)
            .ok_or_else(|| AppError::Audio("no system-audio helper available".into()))?;
        let mut cmd = Command::new(&bin);
        // Pass the protocol's optional `[maxSeconds]` so the helper self-limits to the SAME hard
        // cap as the cpal mic (`recorder::MAX_RECORDING_SECONDS`). Parent-pipe EOF is the primary
        // crash bound; this independent cap is defense in depth and keeps both streams aligned.
        cmd.arg(&wav_path)
            .arg(crate::audio::recorder::MAX_RECORDING_SECONDS.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        // F2: start from an EMPTY environment and re-add only the minimal non-secret vars the
        // sidecar needs, so MURMUR_DEV_* / API keys / tokens in the app environment can never be
        // inherited by this child. PATH is a fixed system list (the sidecar runs no helpers), HOME
        // is needed for the macOS per-user TCC/container context.
        cmd.env_clear().env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
        for key in ["HOME", "USER", "LOGNAME", "TMPDIR"] {
            if let Ok(val) = std::env::var(key) {
                cmd.env(key, val);
            }
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| AppError::Audio(format!("failed to spawn system-audio sidecar: {e}")))?;
        let Some(lifetime_pipe) = child.stdin.take() else {
            // The helper cannot prove exact ownership without this capability. Fail closed and
            // reap it immediately rather than leaving a 4h capture with only a heuristic watcher.
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::Audio(
                "system-audio sidecar started without its parent-lifetime pipe".into(),
            ));
        };
        let first_frame_at: Arc<OnceLock<std::time::Instant>> = Arc::new(OnceLock::new());
        // Drain stderr on a dedicated thread: (a) capture the first-frame anchor, (b) prevent the
        // 64 KB pipe buffer from ever blocking the helper (stderr was piped-but-unread before).
        let (stderr_done_tx, stderr_done_rx) = mpsc::channel();
        let stderr_reader = child.stderr.take().map(|stderr| {
            let anchor = first_frame_at.clone();
            std::thread::spawn(move || {
                let reader = std::io::BufReader::new(stderr);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    if is_first_frame_line(&line) {
                        let _ = anchor.set(std::time::Instant::now());
                    }
                    // Never log helper lines verbatim beyond known markers (no-PII rule).
                }
                let _ = stderr_done_tx.send(());
            })
        });
        Ok(Self {
            child,
            lifetime_pipe: Some(lifetime_pipe),
            wav_path,
            started_at: std::time::Instant::now(),
            first_frame_at,
            stderr_reader,
            stderr_done: Some(stderr_done_rx),
        })
    }

    /// Host wall-clock instant when this system-audio stream started CAPTURING (for the merge).
    /// Prefers the helper's first-frame anchor; falls back to the spawn instant (a helper that
    /// died before capturing, or an old helper without the line).
    pub fn started_at(&self) -> std::time::Instant {
        self.first_frame_at
            .get()
            .copied()
            .unwrap_or(self.started_at)
    }

    /// SIGTERM the sidecar so it finalizes the WAV, wait for it, and always return owned teardown
    /// metadata. A failed helper can still leave a valid partial WAV; the recording coordinator
    /// must adopt or proof-delete that exact artifact.
    ///
    /// `self` implements `Drop` (C1), so we run the whole teardown under [`std::mem::ManuallyDrop`]
    /// and SUPPRESS the drop-guard's second teardown — a clean `stop` reaps the child exactly once
    /// (no double-SIGTERM, no recycled-pid risk). The fields are then dropped explicitly (no leak).
    pub fn stop(self) -> SystemAudioStopOutcome {
        let mut this = std::mem::ManuallyDrop::new(self);
        let capture_started_at = this.started_at();
        // Use `/bin/kill -TERM` (not `Child::kill`, which is SIGKILL and would truncate
        // the WAV) so the sidecar's signal handler can flush + close the file.
        let status = terminate_and_reap(&mut this.child);

        // Join the stderr drainer (the child is gone, so its stderr is at EOF → the thread ends).
        let stderr_finished = this
            .stderr_done
            .take()
            .and_then(|done| done.recv_timeout(Duration::from_millis(500)).ok())
            .is_some();
        if stderr_finished {
            if let Some(handle) = this.stderr_reader.take() {
                let _ = handle.join();
            }
        }

        // Take the fields out of the ManuallyDrop, then let them drop normally at end of scope —
        // the recorder's `Drop` never runs (suppressed), so the child is reaped exactly once here.
        // SAFETY: each field is read exactly once out of the ManuallyDrop and never used again.
        let wav_path = unsafe { std::ptr::read(&this.wav_path) };
        let _child = unsafe { std::ptr::read(&this.child) };
        // Deliberately extracted only AFTER `terminate_and_reap`: dropping it earlier would send
        // EOF and race the helper's clean signal-driven WAV finalization.
        let lifetime_pipe = unsafe { std::ptr::read(&this.lifetime_pipe) };
        drop(lifetime_pipe);
        let _first_frame_at = unsafe { std::ptr::read(&this.first_frame_at) };
        // `stderr_reader` was already `.take()`n above (now `None`); read + drop it too.
        let _stderr_reader = unsafe { std::ptr::read(&this.stderr_reader) };
        let _stderr_done = unsafe { std::ptr::read(&this.stderr_done) };

        let helper_succeeded = status.as_ref().is_some_and(|value| value.success());
        let helper_finalized = helper_exit_proves_finalized(
            helper_succeeded,
            status.as_ref().and_then(|value| value.code()),
        );
        if !helper_succeeded {
            tracing::warn!(
                target: "audio",
                code = ?status.as_ref().and_then(|value| value.code()),
                "system-audio sidecar ended with a capture fault; canonical artifact requires adoption"
            );
        }
        SystemAudioStopOutcome {
            path: wav_path,
            started_at: capture_started_at,
            helper_succeeded,
            helper_finalized,
        }
    }
}

/// C1 — best-effort teardown for a recorder that is DROPPED without a clean [`SystemAudioRecorder::stop`]
/// (app-quit mid-recording, a panic, a `take()` into a discard). A std [`Child`] merely DETACHES on
/// drop, so this guard sends SIGTERM for clean WAV finalization and reaps it (no `<defunct>` zombie).
/// Closing the exact lifetime pipe remains the crash fallback, but it cannot reap a child while this
/// host is still alive and its hard exit intentionally does not prove WAV finalization.
///
/// After a normal `stop()` (which runs teardown under `ManuallyDrop` and suppresses this guard) this
/// Drop never runs on that value, so there is no double-SIGTERM / recycled-pid risk. Panic-free:
/// every step is `let _ =`.
impl Drop for SystemAudioRecorder {
    fn drop(&mut self) {
        // `lifetime_pipe` remains a live field until this method returns, after TERM + reap.
        let _ = terminate_and_reap(&mut self.child);
        let stderr_finished = self
            .stderr_done
            .take()
            .and_then(|done| done.recv_timeout(Duration::from_millis(500)).ok())
            .is_some();
        if stderr_finished {
            if let Some(handle) = self.stderr_reader.take() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_frame_line_is_recognized() {
        assert!(is_first_frame_line("sysaudio: first-frame"));
        assert!(is_first_frame_line("audiocap: first-frame\n".trim()));
        assert!(!is_first_frame_line("sysaudio: capturing"));
        assert!(!is_first_frame_line(
            "audiocap: tap stuck silent — rebuilding (1)"
        ));
        assert!(!is_first_frame_line(""));
    }

    #[test]
    fn only_clean_ready_phase_exit_codes_prove_wav_finalization() {
        assert!(helper_exit_proves_finalized(true, Some(0)));
        assert!(helper_exit_proves_finalized(false, Some(5)));
        assert!(!helper_exit_proves_finalized(false, Some(3)));
        assert!(!helper_exit_proves_finalized(false, Some(6)));
        assert!(!helper_exit_proves_finalized(false, None));
    }

    /// Whether `pid` still has a process-table entry (any state). None once fully reaped.
    fn ps_present(pid: u32) -> bool {
        std::process::Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false)
    }

    /// Build a `SystemAudioRecorder` around a real long-lived dummy child (`sleep`) WITHOUT going
    /// through `start()` (which needs an AppHandle + a real sidecar). Mirrors the struct `start()`
    /// produces: no stderr reader, spawn instant as the anchor.
    fn recorder_over_dummy_child() -> (SystemAudioRecorder, u32) {
        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn a dummy long-lived child");
        let lifetime_pipe = child.stdin.take().expect("dummy lifetime pipe");
        let pid = child.id();
        let rec = SystemAudioRecorder {
            child,
            lifetime_pipe: Some(lifetime_pipe),
            wav_path: std::path::PathBuf::from("/nonexistent/dummy.wav"),
            started_at: std::time::Instant::now(),
            first_frame_at: Arc::new(OnceLock::new()),
            stderr_reader: None,
            stderr_done: None,
        };
        (rec, pid)
    }

    /// C1 (RED-before-GREEN): a `SystemAudioRecorder` DROPPED without a clean `stop()` (app-quit
    /// mid-recording) must SIGTERM + REAP its capture-helper child. Closing the lifetime pipe alone
    /// eventually stops the helper but does not let the live host reap it or prove clean finalization.
    /// Proof: after the drop the pid has no process-table entry at all.
    #[test]
    fn drop_without_stop_reaps_the_capture_child() {
        let (rec, pid) = recorder_over_dummy_child();
        assert!(ps_present(pid), "the dummy child is alive before drop");

        drop(rec); // no stop() — exercises the app-quit-mid-recording path.

        // Give the SIGTERM + wait() a moment to complete.
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !ps_present(pid),
            "dropping without stop() must SIGTERM + reap the child — no orphan, no zombie"
        );
    }

    #[test]
    fn system_helpers_use_stdin_lifetime_capability_not_pid_watchers() {
        for source in [
            include_str!("../../sysaudio/sysaudio.swift"),
            include_str!("../../audiocap/audiocap.swift"),
        ] {
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
}
