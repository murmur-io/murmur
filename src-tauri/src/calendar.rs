//! Local Calendar context source — drives the bundled `meetnotes-calendar` EventKit sidecar.
//!
//! This is the CALENDAR source of the multi-source roadmap: local, ZERO-OAuth, on-device. It
//! surfaces meeting context (title, attendees, agenda) so the brain / pre-meeting brief can use
//! "who's in this meeting + the agenda". Slack/Jira stay deferred.
//!
//! Safety model: the sidecar is a SEPARATE process. The only ways it could hurt the app are a
//! hang or garbage output — and it does neither. The sidecar self-bounds with a watchdog and
//! ALWAYS prints a parseable `{"status":..,"events":[..]}` envelope on exit 0. On THIS side we
//! still apply our own bounded wait + parse, and degrade to an empty `Vec` on EVERY failure
//! (missing sidecar, denied permission, timeout, malformed JSON). It never crashes, never blocks.
//!
//! No new egress: reading the local calendar is on-device. If the resulting context text is later
//! fed to a cloud provider it MUST ride the existing `make_provider` redaction firewall + consent
//! (the same path as the transcript) — this module creates no network path of its own.
//!
//! ⚠️ RUNTIME-UNVERIFIED headless: that EventKit returns REAL events needs the Calendars (TCC)
//! permission granted on a SIGNED build on a real Mac. The verified surface here is: sidecar
//! resolution, the bounded spawn/read plumbing, JSON parsing, and graceful degradation.

use std::path::PathBuf;
use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::storage::models::{CalendarEventFull, CalendarSidecarEnvelope};

/// Filename of the sidecar — both inside `Contents/Resources` of a shipped `.app` and at the dev
/// `OUT_DIR`.
const SIDECAR_NAME: &str = "meetnotes-calendar";

/// Hard wall-clock cap on the sidecar invocation from the Rust side. The sidecar has its own
/// (shorter) internal watchdog; this is belt-and-suspenders so a wedged child can never block a
/// command. Slightly above the sidecar's 8s watchdog so the sidecar normally finishes first.
const SIDECAR_TIMEOUT: Duration = Duration::from_secs(10);

/// Resolve the calendar sidecar binary. Resolution order mirrors `audio::system::sidecar_path`:
/// 1. the bundled resource inside the distributed `.app` (the ONLY path in a shipped build),
/// 2. the compile-time `CALENDAR_BIN` (`OUT_DIR`) DEV-ONLY fallback.
pub fn sidecar_path(app: &AppHandle) -> Option<PathBuf> {
    if let Ok(p) = app
        .path()
        .resolve(SIDECAR_NAME, tauri::path::BaseDirectory::Resource)
    {
        if p.exists() {
            return Some(p);
        }
    }
    option_env!("CALENDAR_BIN")
        .map(PathBuf::from)
        .filter(|p| p.exists())
}

/// Parse the sidecar's stdout envelope into events. Returns an empty Vec for any non-`ok` status
/// or any malformed JSON — NEVER an error. Pure + deterministic so it's unit-testable headless.
pub fn parse_sidecar_output(stdout: &str) -> Vec<CalendarEventFull> {
    // The sidecar prints exactly one JSON object; tolerate trailing whitespace / a stray newline.
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<CalendarSidecarEnvelope>(trimmed) {
        Ok(env) if env.status == "ok" => env.events,
        Ok(_) => Vec::new(), // denied / empty / error → no events, by contract
        Err(e) => {
            tracing::warn!(target: "calendar", error = %e, "malformed calendar sidecar output");
            Vec::new()
        }
    }
}

/// Invoke the bundled sidecar over the window `[now - back_minutes, now + forward_minutes]` and
/// return the parsed events. GRACEFUL on every failure path: sidecar missing / spawn failure /
/// timeout / non-zero exit / denied / malformed → an empty Vec, never an error, never a block.
///
/// Runs the blocking child wait off the async runtime (caller is an async command).
pub async fn fetch_events(
    app: &AppHandle,
    back_minutes: u32,
    forward_minutes: u32,
) -> Vec<CalendarEventFull> {
    let Some(bin) = sidecar_path(app) else {
        tracing::info!(target: "calendar", "calendar sidecar unavailable; returning no events");
        return Vec::new();
    };
    let res = tokio::task::spawn_blocking(move || run_sidecar(&bin, back_minutes, forward_minutes))
        .await;
    match res {
        Ok(stdout) => parse_sidecar_output(&stdout),
        Err(e) => {
            tracing::warn!(target: "calendar", error = %e, "calendar sidecar task join failed");
            Vec::new()
        }
    }
}

/// Spawn the sidecar with a minimal, secret-free environment and a hard timeout. Returns the
/// child's stdout (possibly empty); the caller parses it. Mirrors `audio::system`'s `env_clear`
/// hardening so MURMUR_DEV_* / keys / tokens can never be inherited by the child.
fn run_sidecar(bin: &PathBuf, back_minutes: u32, forward_minutes: u32) -> String {
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(bin);
    cmd.arg(back_minutes.to_string())
        .arg(forward_minutes.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    cmd.env_clear().env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    // HOME is needed for the macOS per-user TCC/container context (the calendar store is per-user).
    for key in ["HOME", "USER", "LOGNAME", "TMPDIR"] {
        if let Ok(val) = std::env::var(key) {
            cmd.env(key, val);
        }
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(target: "calendar", error = %e, "failed to spawn calendar sidecar");
            return String::new();
        }
    };

    // Bounded wait: poll for completion, hard-kill on timeout so a wedged sidecar can't hang us.
    let deadline = std::time::Instant::now() + SIDECAR_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    tracing::warn!(target: "calendar", "calendar sidecar timed out; killing");
                    let _ = child.kill();
                    let _ = child.wait();
                    return String::new();
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                tracing::warn!(target: "calendar", error = %e, "calendar sidecar wait failed");
                let _ = child.kill();
                return String::new();
            }
        }
    }

    match child.wait_with_output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ok() -> &'static str {
        r#"{"status":"ok","events":[
            {"id":"E1","title":"Sprint Planning","start":"2026-06-28T10:00:00Z","end":"2026-06-28T11:00:00Z","attendees":["Alice","bob@example.com"],"notes":"Agenda:\n- velocity\n- scope"},
            {"id":"E2","title":"1:1","start":"2026-06-28T14:00:00Z","end":null,"attendees":[],"notes":""}
        ]}"#
    }

    #[test]
    fn parses_ok_envelope_into_events() {
        let events = parse_sidecar_output(sample_ok());
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, "E1");
        assert_eq!(events[0].title, "Sprint Planning");
        assert_eq!(events[0].start.as_deref(), Some("2026-06-28T10:00:00Z"));
        assert_eq!(events[0].end.as_deref(), Some("2026-06-28T11:00:00Z"));
        assert_eq!(events[0].attendees, vec!["Alice", "bob@example.com"]);
        assert!(events[0].notes.contains("velocity"));
        // Null end + empty attendees/notes survive.
        assert_eq!(events[1].end, None);
        assert!(events[1].attendees.is_empty());
        assert_eq!(events[1].notes, "");
    }

    #[test]
    fn denied_envelope_yields_empty_no_panic() {
        assert!(parse_sidecar_output(r#"{"status":"denied","events":[]}"#).is_empty());
    }

    #[test]
    fn empty_status_yields_empty() {
        assert!(parse_sidecar_output(r#"{"status":"empty","events":[]}"#).is_empty());
    }

    #[test]
    fn error_status_yields_empty() {
        assert!(parse_sidecar_output(r#"{"status":"error","events":[]}"#).is_empty());
    }

    #[test]
    fn ok_status_but_events_present_only_returned_on_ok() {
        // A non-ok status must NEVER surface events even if some were (wrongly) present.
        let s = r#"{"status":"denied","events":[{"id":"X","title":"leak?","start":null,"end":null,"attendees":[],"notes":""}]}"#;
        assert!(parse_sidecar_output(s).is_empty());
    }

    #[test]
    fn malformed_json_yields_empty_no_panic() {
        assert!(parse_sidecar_output("not json at all").is_empty());
        assert!(parse_sidecar_output("{\"status\":").is_empty());
        assert!(parse_sidecar_output("").is_empty());
        assert!(parse_sidecar_output("   \n  ").is_empty());
    }

    #[test]
    fn missing_optional_fields_default_gracefully() {
        // Minimal event missing end/attendees/notes still parses (serde defaults / Options).
        let s = r#"{"status":"ok","events":[{"id":"E","title":"T","start":null,"end":null,"attendees":[],"notes":""}]}"#;
        let events = parse_sidecar_output(s);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "E");
    }
}
