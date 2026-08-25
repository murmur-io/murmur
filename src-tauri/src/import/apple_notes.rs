//! Apple Notes NORMALIZER — the odd one out, and worth explaining before the code.
//!
//! Notes.app has **no bulk export**. Its store is a Core Data SQLite file whose bodies are gzipped
//! protobuf; reading it directly would mean Full Disk Access plus a reverse-engineered format that
//! Apple changes at will. The supported route is the scripting interface, so this module shells out
//! to `osascript` and asks Notes for its own content.
//!
//! Three consequences, all of them honest limitations rather than bugs:
//!
//! 1. **It needs a TCC grant.** The first run raises the macOS "Murmur wants to control Notes"
//!    prompt. Refusing it is a normal outcome, mapped to a clear [`AppError::Unavailable`] rather
//!    than a crash. On a dev build the ad-hoc signature changes every rebuild, so the prompt can
//!    reappear; a signed build asks once.
//! 2. **It cannot be proven in CI.** Everything here that talks to Notes is unverifiable headlessly.
//!    That is why the PARSER is split out as a pure function over the script's output and tested on
//!    its own — the untestable part is reduced to "spawn a process and read stdout".
//! 3. **Bodies arrive as HTML**, which is rendered to text with the same `html2text` the document
//!    extractor uses. Notes has no wikilinks and no page tree beyond its folders, so there is no
//!    link rewriting to do.
//!
//! IDENTITY is the note's Core Data id (`x-coredata://…`), which is stable across renames and
//! moves — the best key of the three sources.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{clamp_title, ImportScan, ImportedPage, MAX_PAGES_PER_IMPORT};
use crate::error::{AppError, Result};

/// Field separator inside one record (ASCII US) and record separator (ASCII RS). Control characters
/// chosen because note text can contain any printable delimiter one might otherwise pick.
const UNIT_SEP: char = '\u{1F}';
const RECORD_SEP: char = '\u{1E}';

/// How long to wait for Notes to answer before giving up and killing the helper. Generous, because
/// the first call also waits for the user to answer the permission prompt — but bounded, because a
/// hung `osascript` would otherwise hold the shared heavy-work permit indefinitely.
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(180);

/// Wrap width for rendering a note's HTML body to text. Matches `extract::html`.
const RENDER_WIDTH: usize = 100;

/// The script asked of Notes. Emits `id US name US folder US body RS` per note.
///
/// `try` around the container read: a note that sits directly under an account has no folder, and
/// without the guard that one note aborts the whole run.
const LIST_NOTES_SCRIPT: &str = r#"
set us to (ASCII character 31)
set rs to (ASCII character 30)
set out to ""
tell application "Notes"
  repeat with n in notes
    set noteFolder to ""
    try
      set noteFolder to (name of container of n) as text
    end try
    set out to out & ((id of n) as text) & us & ((name of n) as text) & us & noteFolder & us & ((body of n) as text) & rs
  end repeat
end tell
return out
"#;

/// Read the local Notes library into a dry-run plan. Writes nothing to Murmur, and nothing leaves
/// the machine — `osascript` talks to the app over local Apple events.
pub(crate) fn scan_notes() -> Result<ImportScan> {
    let raw = run_list_script()?;
    Ok(parse_export(&raw))
}

/// Spawn `osascript`, feed it the script on stdin, and collect stdout under a deadline.
fn run_list_script() -> Result<String> {
    let mut child = Command::new("/usr/bin/osascript")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            AppError::Unavailable(format!("could not start the Notes helper (osascript): {e}"))
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        // A write failure here means the helper died early; the exit status below reports why.
        let _ = stdin.write_all(LIST_NOTES_SCRIPT.as_bytes());
    }

    // Read stdout on a worker so a wedged helper cannot block us forever, then enforce the deadline
    // on the process itself.
    let mut stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel::<std::io::Result<String>>();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let result = match stdout.as_mut() {
            Some(out) => out.read_to_string(&mut buf).map(|_| buf),
            None => Ok(String::new()),
        };
        let _ = tx.send(result);
    });

    let deadline = Instant::now() + SCRIPT_TIMEOUT;
    let output = loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Ok(text)) => break text,
            Ok(Err(e)) => {
                let _ = child.kill();
                return Err(AppError::Unavailable(format!(
                    "could not read from the Notes helper: {e}"
                )));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break String::new(),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err(AppError::Unavailable(
                        "Notes did not respond. If macOS asked for permission, grant it in \
                         System Settings › Privacy & Security › Automation, then try again."
                            .into(),
                    ));
                }
            }
        }
    };

    let status = child
        .wait()
        .map_err(|e| AppError::Unavailable(format!("the Notes helper failed: {e}")))?;
    if !status.success() {
        let mut err = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut err);
        }
        // -1743 is the documented "not authorized to send Apple events" code. Everything else is
        // reported as-is, minus any note content (stderr here carries script errors, not bodies).
        if err.contains("-1743") || err.to_lowercase().contains("not authorized") {
            return Err(AppError::Unavailable(
                "Murmur is not allowed to read Notes. Grant it in System Settings › Privacy & \
                 Security › Automation › Murmur › Notes, then try again."
                    .into(),
            ));
        }
        return Err(AppError::Unavailable(format!(
            "Notes could not be read: {}",
            err.trim()
        )));
    }
    Ok(output)
}

/// PURE parser for the script's output — the testable half of this module.
///
/// Malformed records are skipped rather than failing the run: one odd note must not cost the user
/// the other nine hundred.
pub(crate) fn parse_export(raw: &str) -> ImportScan {
    let mut scan = ImportScan::default();
    for record in raw.split(RECORD_SEP) {
        if record.trim().is_empty() {
            continue;
        }
        let mut fields = record.splitn(4, UNIT_SEP);
        let (Some(id), Some(name), Some(folder), Some(body)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        if scan.pages.len() >= MAX_PAGES_PER_IMPORT {
            scan.truncated = true;
            break;
        }
        let markdown = html_to_text(body);
        // A note with neither a title nor a body carries nothing; importing it would add an empty
        // row for every stray tap in Notes.
        let title = clamp_title(name.trim());
        if title.is_empty() && markdown.trim().is_empty() {
            continue;
        }
        let title = if title.is_empty() {
            crate::storage::db::UNTITLED_TITLE.to_string()
        } else {
            title
        };
        let parents = if folder.trim().is_empty() {
            Vec::new()
        } else {
            vec![folder.trim().to_string()]
        };
        scan.pages.push(ImportedPage {
            external_id: Some(id.trim().to_string()),
            title,
            parents,
            markdown,
        });
    }
    scan.finish();
    scan
}

/// Render a note's HTML body to plain text. A render failure yields the raw body rather than
/// dropping the note — losing formatting beats losing content.
fn html_to_text(body: &str) -> String {
    match html2text::from_read(body.as_bytes(), RENDER_WIDTH) {
        Ok(text) => text.trim().to_string(),
        Err(_) => body.trim().to_string(),
    }
}

#[cfg(test)]
#[path = "apple_notes/tests.rs"]
mod tests;
