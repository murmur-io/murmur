//! System-audio capture via the ScreenCaptureKit Swift sidecar (compiled by `build.rs`).
//!
//! ⚠️ RUNTIME-UNVERIFIED in a headless build: actually capturing system audio needs an
//! interactive desktop session + the Screen Recording (TCC) permission + live audio. The
//! pieces verified here are: the sidecar compiles, spawn/stop plumbing, and graceful
//! degrade-to-None when the sidecar can't capture (e.g. permission denied — it exits 3).

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use crate::error::{AppError, Result};

/// Path to the sidecar binary compiled by `build.rs`, if Swift compilation succeeded
/// and the binary still exists.
pub fn sidecar_path() -> Option<PathBuf> {
    option_env!("SYSAUDIO_BIN")
        .map(PathBuf::from)
        .filter(|p| p.exists())
}

/// Whether this build has a usable system-audio sidecar.
pub fn is_available() -> bool {
    sidecar_path().is_some()
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
    pub fn start(wav_path: PathBuf) -> Result<Self> {
        let bin = sidecar_path()
            .ok_or_else(|| AppError::Audio("system-audio sidecar not built".into()))?;
        let mut cmd = Command::new(&bin);
        cmd.arg(&wav_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        // F2: start from an EMPTY environment and re-add only the minimal non-secret vars the
        // sidecar needs, so MURMUR_DEV_* / API keys / tokens in the app environment can never be
        // inherited by this child. PATH is a fixed system list (the sidecar runs no helpers), HOME
        // is needed for the macOS per-user TCC/container context.
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
        for key in ["HOME", "USER", "LOGNAME", "TMPDIR"] {
            if let Ok(val) = std::env::var(key) {
                cmd.env(key, val);
            }
        }
        let child = cmd
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
