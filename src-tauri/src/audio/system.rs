//! System-audio capture via the ScreenCaptureKit Swift sidecar (compiled by `build.rs`).
//!
//! ⚠️ RUNTIME-UNVERIFIED in a headless build: actually capturing system audio needs an
//! interactive desktop session + the Screen Recording (TCC) permission + live audio. The
//! pieces verified here are: the sidecar compiles, spawn/stop plumbing, and graceful
//! degrade-to-None when the sidecar can't capture (e.g. permission denied — it exits 3).

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use tauri::{AppHandle, Manager};

use crate::error::{AppError, Result};

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
    wav_path: PathBuf,
    /// Host wall-clock instant when the sidecar was spawned (capture start). The mic (cpal) and
    /// this ScreenCaptureKit stream run on INDEPENDENT clocks, so the wall-clock merge anchors
    /// each stream's segments to its own host start instead of aligning by sample count (which
    /// drifts seconds/hour). See `audio::merge`.
    started_at: std::time::Instant,
}

impl SystemAudioRecorder {
    /// Spawn the sidecar writing to `wav_path`. Errors only if the sidecar can't be
    /// spawned at all; a *capture* failure (no permission) surfaces later in [`stop`].
    pub fn start(app: &AppHandle, wav_path: PathBuf) -> Result<Self> {
        let bin = select_helper(app)
            .ok_or_else(|| AppError::Audio("no system-audio helper available".into()))?;
        let child = Command::new(&bin)
            .arg(&wav_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AppError::Audio(format!("failed to spawn system-audio sidecar: {e}")))?;
        Ok(Self {
            child,
            wav_path,
            started_at: std::time::Instant::now(),
        })
    }

    /// Host wall-clock instant when this system-audio stream started capturing (for the merge).
    pub fn started_at(&self) -> std::time::Instant {
        self.started_at
    }

    /// SIGTERM the sidecar so it finalizes the WAV, wait for it, and return the WAV path
    /// if it captured anything. Returns `Ok(None)` on sidecar failure (e.g. permission
    /// denied → exit 3) so the caller proceeds mic-only rather than failing the recording.
    pub fn stop(mut self) -> Result<Option<PathBuf>> {
        // Use `/bin/kill -TERM` (not `Child::kill`, which is SIGKILL and would truncate
        // the WAV) so the sidecar's signal handler can flush + close the file.
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status();

        let status = self
            .child
            .wait()
            .map_err(|e| AppError::Audio(format!("waiting on system-audio sidecar: {e}")))?;

        if status.success() && self.wav_path.exists() {
            Ok(Some(self.wav_path))
        } else {
            tracing::warn!(
                target: "audio",
                code = ?status.code(),
                "system-audio sidecar produced no track (likely no Screen Recording permission)"
            );
            Ok(None)
        }
    }
}
