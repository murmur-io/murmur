//! Developer-mode commands — the diagnostics surface behind the Settings → Developer toggle.
//!
//! These are thin wrappers over [`crate::applog`]: read a window of the on-device log, clear the
//! current session's file, reveal it in Finder. They touch NO meeting content — no note, no
//! transcript, no timeline, no audio — so there is nothing here for `meeting_is_unlocked` /
//! `visibility_clause` to gate: the log carries IDs, stages, counts and durations only (the
//! no-PII rule), and it is read straight off the local filesystem rather than out of the DB.
//!
//! The Developer-mode toggle is a FE affordance (it decides what the UI offers), never a security
//! boundary — nothing here would be unsafe to call with the toggle off, which is what keeps the
//! toggle honest about what it is.

use crate::applog::{self, AppLog, LogSession};
use crate::error::{AppError, Result};

/// Read a window of one log generation: `"current"` (this session) or `"previous"` (the run
/// before this one — where a crash's last words are).
///
/// `limit` is the number of most-recent entries to return, clamped to
/// [1, [`applog::MAX_ENTRIES`]]; omit it for [`applog::DEFAULT_ENTRIES`]. A generation that has
/// never been written returns `exists: false` with no entries rather than an error.
#[tauri::command]
pub fn read_app_log(session: String, limit: Option<usize>) -> Result<AppLog> {
    applog::read(LogSession::parse(&session)?, limit)
}

/// Empty the CURRENT session's log so a reproduction starts from a clean slate. The previous
/// generation is evidence and is deliberately not clearable from here.
#[tauri::command]
pub fn clear_app_log() -> Result<()> {
    applog::clear_current()
}

/// Reveal the log folder in Finder (macOS `open`), so a bug report can attach the raw file.
/// Opens the DIRECTORY, not the file — a `.log` has no sensible default handler.
#[tauri::command]
pub fn reveal_app_log() -> Result<()> {
    let dir = applog::log_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| AppError::Storage(format!("log dir: {e}")))?;
    std::process::Command::new("open")
        .arg(&dir)
        .spawn()
        .map_err(|e| AppError::Storage(format!("reveal log dir: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_session_is_refused_as_invalid_arg() {
        let err = read_app_log("../secrets".into(), None).unwrap_err();
        assert!(
            matches!(err, AppError::InvalidArg(_)),
            "a bogus session token is an argument error, not a storage error: {err:?}"
        );
    }

    #[test]
    fn both_real_sessions_read_without_error() {
        // Neither file is guaranteed to exist on a test machine; absence must still be `Ok`.
        assert!(read_app_log("current".into(), Some(5)).is_ok());
        assert!(read_app_log("previous".into(), Some(5)).is_ok());
    }
}
