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
}

impl SystemAudioRecorder {
    /// Spawn the sidecar writing to `wav_path`. Errors only if the sidecar can't be
    /// spawned at all; a *capture* failure (no permission) surfaces later in [`stop`].
    pub fn start(wav_path: PathBuf) -> Result<Self> {
        let bin = sidecar_path()
            .ok_or_else(|| AppError::Audio("system-audio sidecar not built".into()))?;
        let child = Command::new(&bin)
            .arg(&wav_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AppError::Audio(format!("failed to spawn system-audio sidecar: {e}")))?;
        Ok(Self { child, wav_path })
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
