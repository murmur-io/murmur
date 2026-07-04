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

/// Basenames of the helper binaries the app spawns to CAPTURE audio. At app startup nothing is
/// recording yet, so any of these still alive is an ORPHAN from a previous session that died
/// without a clean Stop — a crash, a force-quit, or (in dev) a `tauri dev` hot-rebuild SIGKILLing
/// the app mid-recording. An orphan reparents to launchd (ppid 1) and keeps capturing system audio
/// to its temp WAV until its own 4h self-limit — gigabytes of dead-session audio.
const CAPTURE_HELPERS: [&str; 3] = [
    "meetnotes-sysaudio",
    "meetnotes-audiocap",
    "meetnotes-aeccap",
];

/// SIGTERM any ORPHANED capture helper (a [`CAPTURE_HELPERS`] binary reparented to launchd) and
/// delete its scratch WAV. Best-effort, called ONCE at startup. This closes the gap
/// [`sweep_stale_scratch`] can't: a *live* orphan keeps its WAV mtime fresh, so the file-age sweep
/// never reclaims it, and the child runs for hours.
///
/// PRECISE + SAFE: it targets ONLY processes with `ppid == 1` (launchd) — a helper freshly spawned
/// by THIS running app is a child of our pid (never launchd), and a concurrently-running Murmur's
/// live helper is a child of *its* pid — so neither is ever touched. Only a truly-orphaned,
/// parentless helper matches.
pub fn reap_orphaned_capture_helpers() {
    let Ok(out) = std::process::Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
    else {
        return;
    };
    let mut reaped = 0u32;
    for (pid, wav) in parse_orphan_helpers(&String::from_utf8_lossy(&out.stdout)) {
        // SIGTERM (not SIGKILL): let the helper's signal handler close its file, then reclaim it.
        let killed = std::process::Command::new("/bin/kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if killed {
            reaped += 1;
            if let Some(path) = wav {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    if reaped > 0 {
        // WARN, not INFO: an orphan reaching startup means a prior session leaked a capture helper.
        tracing::warn!(target: "audio", reaped, "reaped orphaned capture helper(s) at startup");
    }
}

/// Pure parser for `ps -axo pid=,ppid=,command=` output: return `(pid, scratch_wav)` for every line
/// that is an ORPHANED (`ppid == 1`) capture helper. `scratch_wav` is the helper's capture-scratch
/// argument (`meetnotes-sys-*.wav` / `meetnotes-aec-*.wav`), when present, so the caller can delete
/// it after the kill. Isolated from the process-spawning so the ppid==1 safety filter is unit-
/// testable without live processes. (Assumes the scratch/binary paths carry no embedded spaces —
/// true for the OS temp dir + the app resource dir; this is best-effort startup cleanup.)
fn parse_orphan_helpers(ps_output: &str) -> Vec<(i32, Option<std::path::PathBuf>)> {
    let mut out = Vec::new();
    for line in ps_output.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 3 {
            continue;
        }
        let (Ok(pid), Ok(ppid)) = (tokens[0].parse::<i32>(), tokens[1].parse::<i32>()) else {
            continue;
        };
        if ppid != 1 {
            continue; // NOT an orphan — a child of a live app (ours or another). Never touch it.
        }
        // A command token whose basename IS one of our capture helpers.
        let is_helper = tokens.iter().any(|t| {
            let base = t.rsplit('/').next().unwrap_or(t);
            CAPTURE_HELPERS.contains(&base)
        });
        if !is_helper {
            continue;
        }
        // The scratch WAV arg (same pattern sweep_stale_scratch reclaims), if the helper carries one.
        let wav = tokens
            .iter()
            .find(|t| {
                let base = t.rsplit('/').next().unwrap_or(t);
                (base.starts_with("meetnotes-sys-") || base.starts_with("meetnotes-aec-"))
                    && base.ends_with(".wav")
            })
            .map(std::path::PathBuf::from);
        out.push((pid, wav));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // A realistic `ps -axo pid=,ppid=,command=` fixture: the actual orphan we found (audiocap,
    // ppid 1) + a legit LIVE helper (child of a running app, ppid 32431) that MUST be spared +
    // an unrelated launchd child + a sysaudio orphan with no scratch arg.
    const PS: &str = "\
 5916     1 /Users/x/target/debug/meetnotes-audiocap /var/folders/sl/T/meetnotes-sys-83191e88.wav 14400
 7001 32431 /Users/x/target/debug/meetnotes-audiocap /var/folders/sl/T/meetnotes-sys-live-abc.wav 14400
  412     1 /usr/libexec/somethingd
 8080     1 /Users/x/target/debug/meetnotes-sysaudio";

    #[test]
    fn selects_only_orphaned_helpers_and_extracts_scratch() {
        let got = parse_orphan_helpers(PS);
        // The audiocap orphan (with its scratch WAV) and the sysaudio orphan (no scratch arg).
        assert_eq!(
            got,
            vec![
                (
                    5916,
                    Some(PathBuf::from(
                        "/var/folders/sl/T/meetnotes-sys-83191e88.wav"
                    ))
                ),
                (8080, None),
            ]
        );
    }

    #[test]
    fn never_reaps_a_live_child_helper() {
        // SAFETY INVARIANT: pid 7001 is a live capture helper of a RUNNING app (ppid 32431). It is
        // a byte-for-byte twin of the orphan except for its parent — it must NEVER be selected.
        let ids: Vec<i32> = parse_orphan_helpers(PS)
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert!(
            !ids.contains(&7001),
            "must not reap a helper still owned by a live app"
        );
    }

    #[test]
    fn ignores_non_helper_and_malformed_lines() {
        assert!(parse_orphan_helpers("  412     1 /usr/libexec/somethingd").is_empty());
        assert!(parse_orphan_helpers("garbage\n\n123 abc def").is_empty());
        assert!(parse_orphan_helpers("").is_empty());
    }
}
