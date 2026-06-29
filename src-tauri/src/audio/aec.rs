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
        let is_scratch = (name.starts_with("meetnotes-aec-")
            || name.starts_with("meetnotes-sys-"))
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
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
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
    pub fn stop(mut self) -> Result<Option<PathBuf>> {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status();
        let status = self
            .child
            .wait()
            .map_err(|e| AppError::Audio(format!("waiting on AEC helper: {e}")))?;
        if status.success() && self.wav_path.exists() {
            Ok(Some(self.wav_path))
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
