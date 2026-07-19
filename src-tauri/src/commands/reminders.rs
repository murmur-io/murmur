//! macOS Reminders commands — extracted verbatim from `commands` (God-file split, a PURE MOVE —
//! no behavior change). This is a small, self-contained, NON-content-gated domain: it shells out to
//! `osascript` to create a macOS Reminder from an action-item text + optional ISO due date. It reads
//! NO meeting content and touches no seal/unlock surface, so there is nothing to gate. Every symbol
//! keeps its EXACT prior body/signature and is re-exported at `crate::commands` via `pub use
//! reminders::*;` in `commands/mod.rs`, so `generate_handler![commands::add_reminder]` in `lib.rs`
//! and every `crate::commands::…` caller resolve UNCHANGED. `build_reminder_script` /
//! `add_reminder_blocking` stay `pub(crate)` so the voice-action dispatch in `commands/mod.rs`
//! still reaches them through the re-export.

use crate::error::AppError;

/// Escape a string for embedding inside an AppleScript `"…"` literal: backslash + double-quote are
/// escaped, and raw CR/LF are flattened to spaces (an AppleScript string literal cannot span lines).
/// This is what stops the item text from breaking out of the quoted literal or injecting extra
/// statements (`"`, `end tell`, …) into the osascript program.
fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\n', '\r'], " ")
}

/// Parse a strict ISO `YYYY-MM-DD` into `(year, month, day)`; `None` for anything else.
fn parse_iso_ymd(s: &str) -> Option<(i32, u32, u32)> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i32 = s.get(0..4)?.parse().ok()?;
    let m: u32 = s.get(5..7)?.parse().ok()?;
    let d: u32 = s.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((y, m, d))
}

/// Build the osascript program that creates a Reminder named `name`. When `due_date` is a valid
/// ISO `YYYY-MM-DD`, attach `remind me date`/`due date` (defaulted to 9am local) so the date
/// actually lands in Reminders — previously the date was dropped. The name is
/// `escape_applescript`-escaped so its text can never break out of the string literal. The date is
/// built by setting `day` to 1 FIRST (so a year/month change can't overflow the current day-of-month),
/// then year, then month, then the real day.
pub(crate) fn build_reminder_script(name: &str, due_date: Option<&str>) -> String {
    let esc = escape_applescript(name);
    match due_date.and_then(parse_iso_ymd) {
        Some((y, m, d)) => format!(
            "set theDate to current date\n\
             set day of theDate to 1\n\
             set year of theDate to {y}\n\
             set month of theDate to {m}\n\
             set day of theDate to {d}\n\
             set hours of theDate to 9\n\
             set minutes of theDate to 0\n\
             set seconds of theDate to 0\n\
             tell application \"Reminders\" to make new reminder with properties {{name:\"{esc}\", remind me date:theDate, due date:theDate}}"
        ),
        None => format!(
            "tell application \"Reminders\" to make new reminder with properties {{name:\"{esc}\"}}"
        ),
    }
}

/// Add a macOS Reminder (via osascript) for an action item. A denied Reminders permission
/// surfaces a clear, actionable error rather than crashing the UI. When the item carries an ISO
/// due date, it is set as the reminder's due/remind date (best-effort; verify on a real Mac).
#[tauri::command]
pub async fn add_reminder(text: String, due_date: Option<String>) -> Result<(), AppError> {
    let name = text.trim().to_string();
    if name.is_empty() {
        return Err(AppError::InvalidArg("empty reminder".into()));
    }
    let due = due_date.as_deref().filter(|d| !d.is_empty());
    let script = build_reminder_script(&name, due);
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
    })
    .await
    .map_err(|e| AppError::Unavailable(format!("reminder task failed: {e}")))?
    .map_err(|e| AppError::Unavailable(format!("osascript failed: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(AppError::Unavailable(format!(
            "Could not add to Reminders — grant access in System Settings ▸ Privacy & Security ▸ Reminders. ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// SYNCHRONOUS reminder creation for the off-thread voice-action dispatch (Flow B). Mirrors the
/// `add_reminder` command's osascript path, but blocking (it already runs on a detached task, so it
/// must not require an async runtime). Returns `Ok(())` on success, a typed `AppError` otherwise —
/// NEVER panics. NO PII logged by the caller; the reminder text is the user's own dictated note.
pub(crate) fn add_reminder_blocking(text: &str, due_date: Option<&str>) -> Result<(), AppError> {
    let name = text.trim();
    if name.is_empty() {
        return Err(AppError::InvalidArg("empty reminder".into()));
    }
    let due = due_date.filter(|d| !d.is_empty());
    let script = build_reminder_script(name, due);
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| AppError::Unavailable(format!("osascript failed: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(AppError::Unavailable(format!(
            "Could not add to Reminders — grant access in System Settings ▸ Privacy & Security ▸ Reminders. ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

#[cfg(test)]
mod reminder_script_tests {
    use super::{build_reminder_script, escape_applescript, parse_iso_ymd};

    #[test]
    fn parses_strict_iso_only() {
        assert_eq!(parse_iso_ymd("2026-07-01"), Some((2026, 7, 1)));
        assert_eq!(parse_iso_ymd(" 2026-12-31 "), Some((2026, 12, 31)));
        assert_eq!(parse_iso_ymd("2026-13-01"), None); // month out of range
        assert_eq!(parse_iso_ymd("2026-07-32"), None); // day out of range
        assert_eq!(parse_iso_ymd("2026/07/01"), None); // wrong separators
        assert_eq!(parse_iso_ymd("26-07-01"), None); // not 4-digit year
        assert_eq!(parse_iso_ymd(""), None);
    }

    #[test]
    fn due_date_sets_the_date_properties() {
        let s = build_reminder_script("Ship the deck", Some("2026-07-01"));
        // The date is actually attached now (the bug was: only `name` was set).
        assert!(s.contains("set year of theDate to 2026"));
        assert!(s.contains("set month of theDate to 7"));
        assert!(s.contains("set day of theDate to 1"));
        assert!(s.contains("remind me date:theDate"));
        assert!(s.contains("due date:theDate"));
        assert!(s.contains("name:\"Ship the deck\""));
        // `day` is reset to 1 BEFORE year/month so a month change can't overflow the day.
        let reset = s.find("set day of theDate to 1").unwrap();
        let yr = s.find("set year of theDate").unwrap();
        assert!(
            reset < yr,
            "day must be reset to 1 before changing year/month"
        );
    }

    #[test]
    fn no_due_date_is_name_only() {
        let s = build_reminder_script("Call Bob", None);
        assert!(s.contains("name:\"Call Bob\""));
        assert!(!s.contains("due date"));
        assert!(!s.contains("theDate"));
    }

    #[test]
    fn invalid_due_date_falls_back_to_name_only() {
        let s = build_reminder_script("Task", Some("not-a-date"));
        assert!(
            !s.contains("due date"),
            "an unparseable date must not produce date props"
        );
        assert!(s.contains("name:\"Task\""));
    }

    #[test]
    fn item_text_cannot_break_out_of_the_applescript_literal() {
        // A name carrying a quote + a forged statement must stay INSIDE the string literal: the
        // `"` is escaped to `\"`, so `end tell` / the injected `make` never become real statements.
        let evil =
            "pwn\", remind me date:theDate}\nend tell\ntell application \"Finder\" to delete";
        let esc = escape_applescript(evil);
        assert!(
            !esc.contains('\n'),
            "raw newlines flattened (literals can't span lines)"
        );
        // Every `"` in the payload is preceded by a backslash — no bare quote survives to close
        // the literal early. (Checked by scanning: each `"` byte has a `\` immediately before it.)
        let bytes = esc.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'"' {
                assert!(
                    i > 0 && bytes[i - 1] == b'\\',
                    "unescaped quote survived at {i}"
                );
            }
        }
        let s = build_reminder_script(evil, Some("2026-07-01"));
        // The ONE real `tell` statement (unescaped quotes around Reminders) is intact...
        assert!(
            s.contains("tell application \"Reminders\""),
            "the real Reminders statement must survive"
        );
        // ...and the injected Finder `tell` never becomes real code: its quotes are escaped, so it
        // stays as inert data inside the name literal (no `tell application "Finder"` with REAL quotes).
        assert!(
            !s.contains("tell application \"Finder\""),
            "injected statement must remain escaped data, not executable code"
        );
        // The whole program is a single line (newlines in the payload were flattened), so a forged
        // `end tell` can never start its own statement line.
        assert!(
            !s.lines().any(|l| l.trim() == "end tell"),
            "no standalone injected `end tell` statement line"
        );
        // Every embedded double-quote from the payload is backslash-escaped in the program.
        assert!(s.contains("\\\""), "payload quotes are escaped");
    }
}
