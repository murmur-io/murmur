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
///
/// Prunes first, so what the view shows is exactly what is still kept: the retention window is enforced
/// on DISK before anything is read out of it, and a stale entry can never be rendered as if it had
/// survived. A prune failure is not allowed to withhold the log — the read proceeds and the
/// hourly tick will try again.
#[tauri::command]
pub fn read_app_log(session: String, limit: Option<usize>) -> Result<AppLog> {
    let target = LogSession::parse(&session)?;
    if let Err(error) = applog::prune_expired() {
        tracing::warn!(target: "applog", %error, "log prune before read failed");
    }
    applog::read(target, limit)
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


/// Assemble a shareable diagnostics bundle and return its absolute path.
///
/// A bug report that says "it froze" is unanswerable; one that carries the app version, the OS, and
/// the last two log generations usually answers itself. This writes exactly that, as one plain-text
/// file the user can attach, next to the logs it is made from — the existing "reveal in Finder"
/// command then puts them at it.
///
/// NO PII, BY WHAT IT IS ALLOWED TO READ rather than by filtering afterwards. Filtering assumes you
/// can recognise personal content in arbitrary text, which is exactly the assumption that turns a
/// redactor into a leak. Instead every part has a non-PII contract of its own: the log carries IDs,
/// stages, counts and durations only (the rule this module already states); `AppInfo` is
/// compile-time constants; the model section lists FILE NAMES and byte sizes, never file contents.
/// Nothing here touches a note, transcript, timeline, title, attendee, path-with-content, or the
/// database — so there is no read for `meeting_is_unlocked` / `visibility_clause` to gate, for the
/// same reason the rest of this module needs none.
#[tauri::command]
pub fn export_diagnostics_bundle() -> Result<String> {
    if let Err(error) = applog::prune_expired() {
        tracing::warn!(target: "applog", %error, "log prune before bundle failed");
    }
    let dir = applog::log_dir()?;
    let mut out = String::with_capacity(64 * 1024);
    let info = crate::update::app_info();
    out.push_str("# Murmur diagnostics bundle\n\n");
    out.push_str(&format!("app: {} {}\n", info.name, info.version));
    out.push_str(&format!("os: {}\n", std::env::consts::OS));
    out.push_str(&format!("arch: {}\n", std::env::consts::ARCH));
    out.push_str(&format!("profile: {}\n", crate::state::app_dir_name()));
    out.push_str(&format!(
        "log retention: {} h, cap {} MiB\n\n",
        applog::RETENTION_HOURS,
        applog::MAX_LOG_BYTES / (1024 * 1024)
    ));

    out.push_str("## models on disk (names and sizes only)\n\n");
    match crate::transcribe::model::models_dir().and_then(|d| {
        crate::transcribe::model::models_dir_file_names(&d)
            .map_err(|e| AppError::Storage(format!("list models dir: {e}")))
    }) {
        Ok(names) if names.is_empty() => out.push_str("(none)\n"),
        Ok(names) => {
            for name in names {
                out.push_str(&format!("- {name}\n"));
            }
        }
        Err(e) => out.push_str(&format!("(unavailable: {e})\n")),
    }

    for (label, session) in [("previous", LogSession::Previous), ("current", LogSession::Current)] {
        out.push_str(&format!("\n## log: {label}\n\n"));
        match applog::read(session, Some(applog::MAX_ENTRIES)) {
            Ok(log) if !log.exists => out.push_str("(no such generation)\n"),
            Ok(log) => {
                for entry in &log.entries {
                    out.push_str(&format!(
                        "{} {} {} {}\n",
                        entry.timestamp.as_deref().unwrap_or("-"),
                        entry.level,
                        entry.target,
                        entry.message
                    ));
                }
            }
            Err(e) => out.push_str(&format!("(unreadable: {e})\n")),
        }
    }

    let path = dir.join("murmur-diagnostics.txt");
    std::fs::write(&path, out)
        .map_err(|e| AppError::Storage(format!("write diagnostics bundle: {e}")))?;
    Ok(path.to_string_lossy().into_owned())
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

    /// The bundle stays PII-free by what it is ALLOWED TO READ, and this pins the mechanism.
    ///
    /// `export_diagnostics_bundle` takes NO `State<AppState>`. That is not an accident of style —
    /// it is the guarantee: without the state there is no `Db`, so no note, transcript, timeline,
    /// title or attendee can reach the file, and nothing has to be recognised and filtered out
    /// afterwards. Filtering assumes you can spot personal content in arbitrary text, which is the
    /// assumption that turns a redactor into a leak.
    ///
    /// So the check is a source ALLOWLIST rather than a hunt for forbidden words: a scan for
    /// "suspicious" strings is always cheaper to slip past than to defend, while adding a state
    /// parameter is a visible, deliberate act that fails here immediately.
    ///
    /// RED CONTROL (run 2026-09-03): giving the command a `state: State<'_, AppState>` parameter
    /// fails this test.
    #[test]
    fn the_diagnostics_bundle_cannot_reach_the_database() {
        let source = std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("commands")
                .join("devtools.rs"),
        )
        .expect("devtools.rs must be readable");
        let sig_at = source
            .find("pub fn export_diagnostics_bundle(")
            .expect("the command is gone — update this oracle deliberately");
        let open = sig_at + source[sig_at..].find('(').unwrap();
        let close = sig_at + source[sig_at..].find(')').unwrap();
        let params = &source[open + 1..close];
        assert!(
            params.trim().is_empty(),
            "the bundle command must take NO parameters — it has `{params}`. A `State<AppState>` \
             here would put the whole database one call away from a file the user mails out."
        );

        // And the body must not reach the DB by any other route.
        let body_start = close;
        let body_end = source[body_start..]
            .find("\n}\n")
            .map(|i| body_start + i)
            .unwrap_or(source.len());
        let body = &source[body_start..body_end];
        for forbidden in ["AppState", "state.db", "Db::", "list_meetings", "get_note"] {
            assert!(
                !body.contains(forbidden),
                "the bundle body references `{forbidden}` — the no-PII contract rests on it not \
                 having a route to meeting content at all"
            );
        }
    }
}
