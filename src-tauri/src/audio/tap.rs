//! Core Audio PROCESS TAP system-audio capture (macOS 14.4+) — the premium path.
//!
//! Spawns the `audiocap` helper (`src-tauri/audiocap/audiocap.swift`), bundled into the `.app`
//! exactly like the ScreenCaptureKit `system` sidecar. The runtime selector in `audio::system`
//! prefers the tap on macOS 14.4+ and falls back to the SCK sidecar on 13–14.3.
//!
//! 14.4 is a DELIBERATE conservative shipping floor (the tap symbols ship from 14.2, but the
//! permission UX stabilised at 14.4). Real capture additionally needs the Audio-Recording (TCC)
//! grant at runtime — a denial surfaces as the helper exiting non-zero, and the recording falls
//! back to mic-only (handled by `SystemAudioRecorder::stop`).

use std::path::PathBuf;

use tauri::{AppHandle, Manager};

const TAP_HELPER_NAME: &str = "meetnotes-audiocap";

/// Path to the bundled Core Audio tap helper (resource dir, then the dev `AUDIOCAP_BIN` fallback).
pub fn tap_helper_path(app: &AppHandle) -> Option<PathBuf> {
    if let Ok(p) = app
        .path()
        .resolve(TAP_HELPER_NAME, tauri::path::BaseDirectory::Resource)
    {
        if p.exists() {
            return Some(p);
        }
    }
    option_env!("AUDIOCAP_BIN")
        .map(PathBuf::from)
        .filter(|p| p.exists())
}

/// Whether the Core Audio process tap is usable on THIS machine: macOS ≥ 14.4 AND the helper is
/// bundled. (Live capture still depends on the Audio-Recording TCC grant, resolved at runtime.)
pub fn is_available(app: &AppHandle) -> bool {
    macos_at_least_14_4() && tap_helper_path(app).is_some()
}

/// Best-effort macOS product-version check via `sw_vers` (a crash-safe subprocess — no FFI, so it
/// can never raise an ObjC exception across the boundary). Any failure → `false` (use SCK).
fn macos_at_least_14_4() -> bool {
    let Ok(out) = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
    else {
        return false;
    };
    parse_at_least_14_4(String::from_utf8_lossy(&out.stdout).trim())
}

/// `true` when the dotted version string is ≥ 14.4. Pure, so it's unit-testable.
fn parse_at_least_14_4(version: &str) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    major > 14 || (major == 14 && minor >= 4)
}

#[cfg(test)]
mod tests {
    use super::parse_at_least_14_4;

    #[test]
    fn version_gate_matches_14_4_floor() {
        assert!(parse_at_least_14_4("14.4"));
        assert!(parse_at_least_14_4("14.5"));
        assert!(parse_at_least_14_4("15.0"));
        assert!(parse_at_least_14_4("26.5"));
        assert!(!parse_at_least_14_4("14.3"));
        assert!(!parse_at_least_14_4("14.0"));
        assert!(!parse_at_least_14_4("13.6"));
        assert!(!parse_at_least_14_4("11.7.10"));
        // Garbage / empty → false (fall back to SCK), never a panic.
        assert!(!parse_at_least_14_4(""));
        assert!(!parse_at_least_14_4("not-a-version"));
    }
}
