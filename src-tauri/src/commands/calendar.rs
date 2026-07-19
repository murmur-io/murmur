//! CALENDAR connector command surface (local, zero-OAuth, on-device — NOT sealed content).
//!
//! Extracted verbatim from `commands/mod.rs` (God-file split, PURE MOVE — every body is
//! byte-identical, only relocated). These three commands read the user's LOCAL macOS Calendar
//! (via `osascript` for the next-event probe, or the bundled `meetnotes-calendar` EventKit sidecar
//! for the windowed reads) — they never touch sealed meeting/note content, so there is NO
//! `meeting_is_unlocked` / `visibility_clause` gate here (nothing to gate). No NETWORK egress: the
//! calendar stays on device; any downstream cloud use of a `CalendarContext` MUST ride the existing
//! `make_provider` redaction firewall + consent (the same path the transcript takes) — these
//! commands open NO new egress path.
//!
//! Reached from `commands/mod.rs` via `pub use calendar_commands::*;` so every path resolves
//! UNCHANGED at `crate::commands::…` for `generate_handler!` in `lib.rs` and every caller. The
//! shared imports (`AppError`, `AppHandle`, the `Calendar*` model DTOs) come in via `use super::*`.

use super::*;

/// Best-effort: the soonest macOS Calendar event in the next 60 minutes (title only). Returns
/// None if Calendar access is denied or there's nothing upcoming — never errors the UI.
#[tauri::command]
pub async fn next_calendar_event() -> Result<Option<CalendarEvent>, AppError> {
    let script = r#"set now to (current date)
set laterT to now + (60 * minutes)
set out to ""
try
  tell application "Calendar"
    repeat with c in calendars
      repeat with e in (every event of c whose start date is greater than or equal to now and start date is less than or equal to laterT)
        set out to out & (summary of e) & linefeed
      end repeat
    end repeat
  end tell
end try
return out"#;
    let res = tokio::task::spawn_blocking(move || {
        std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()
    })
    .await;
    let stdout = match res {
        Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return Ok(None),
    };
    let title = stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string);
    Ok(title.map(|title| CalendarEvent { title, start: None }))
}

/// CALENDAR source (local, zero-OAuth, on-device): list the user's events in a window around now
/// via the bundled `meetnotes-calendar` EventKit sidecar — title, attendees, agenda. GRACEFUL on
/// every failure: sidecar missing / Calendar permission denied / timeout / malformed output →
/// an empty list, never an error, never a block. No network egress: reading the local calendar
/// stays on device.
#[tauri::command]
pub async fn list_calendar_events(app: AppHandle) -> Result<Vec<CalendarEventFull>, AppError> {
    // Default window: now-1h .. now+12h (60 back, 720 forward minutes).
    Ok(crate::calendar::fetch_events(&app, 60, 720).await)
}

/// Build a compact [`CalendarContext`] (title + attendees + agenda) for one event so the existing
/// pre-meeting brief / note pre-analysis can consume it (the brain already takes context). Looks
/// the event up by id in the same window the sidecar surfaces. Returns `None` if the event isn't
/// found (expired from the window, or Calendar access denied) — never an error.
///
/// IMPORTANT: the returned text is on-device context. If it is later fed to a CLOUD provider it
/// MUST ride the existing `make_provider` redaction firewall + consent (the same path the
/// transcript takes) — this command opens NO new egress path.
#[tauri::command]
pub async fn calendar_context_for(
    app: AppHandle,
    event_id: String,
) -> Result<Option<CalendarContext>, AppError> {
    if event_id.trim().is_empty() {
        return Err(AppError::InvalidArg("event_id is empty".into()));
    }
    let events = crate::calendar::fetch_events(&app, 60, 720).await;
    Ok(events
        .iter()
        .find(|e| e.id == event_id)
        .map(CalendarContext::from_event))
}
