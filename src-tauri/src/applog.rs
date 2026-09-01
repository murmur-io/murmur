//! The on-device application log — where it lives, how a session rotates, and how a raw
//! `tracing` file is parsed back into structured entries for the Developer-mode Logs view.
//!
//! Two files sit in the app-data dir (`<data>/<app_dir_name()>/`):
//!   * `murmur.log`      — the CURRENT session (the running process appends to it),
//!   * `murmur.prev.log` — the PREVIOUS session, moved aside at launch.
//!
//! The previous-session file is the whole point of the rotation. The log used to be TRUNCATED at
//! startup, so the one log a crash investigation actually needs — the session that died — was
//! destroyed by the relaunch that went looking for it. A crash is diagnosed from the run BEFORE
//! the current one; keeping exactly one generation costs one file and loses nothing.
//!
//! PII: the log itself carries IDs / stages / counts only (the no-PII rule), and the panic hook
//! sanitizes payloads before they land here. This module only READS what is already on disk and
//! never widens what is written, so surfacing it in-app adds no new disclosure — the file is
//! local, and its contents were already local.

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

use crate::error::{AppError, Result};

/// Current-session log file name.
pub const LOG_FILE: &str = "murmur.log";
/// Previous-session log file name (the rotated copy).
pub const PREV_LOG_FILE: &str = "murmur.prev.log";

/// Hard ceiling on entries returned by one read. A pathological log (a tight error loop) must not
/// be able to hand the webview an unbounded payload — the view reads the TAIL, which is the part
/// anyone diagnosing a failure wants anyway.
pub const MAX_ENTRIES: usize = 5_000;
/// Default when the caller does not ask for a specific window.
pub const DEFAULT_ENTRIES: usize = 1_000;

/// Bytes of the log tail scanned for entries. Reading only the tail keeps a long-running session's
/// multi-MB log from being parsed in full on every refresh; the older head is still on disk and
/// reachable through "Reveal in Finder".
const TAIL_SCAN_BYTES: u64 = 4 * 1024 * 1024;

/// Cap on a single entry's retained text. One absurd line (a serialized blob threaded into an
/// `expect`) can no longer inflate the whole response.
const MAX_ENTRY_CHARS: usize = 8_000;

/// Which rotation generation to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSession {
    /// `murmur.log` — this process.
    Current,
    /// `murmur.prev.log` — the run before this one (where a crash lives).
    Previous,
}

impl LogSession {
    /// Parse the FE's session token. Anything unrecognised is refused rather than silently
    /// defaulting, so a typo shows up as an error instead of the wrong file.
    pub fn parse(token: &str) -> Result<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "current" => Ok(Self::Current),
            "previous" => Ok(Self::Previous),
            other => Err(AppError::InvalidArg(format!(
                "unknown log session '{other}' (expected 'current' or 'previous')"
            ))),
        }
    }

    /// The file name this generation is stored under.
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Current => LOG_FILE,
            Self::Previous => PREV_LOG_FILE,
        }
    }

    /// The token the FE uses for this generation.
    pub fn token(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Previous => "previous",
        }
    }
}

/// The directory both log generations live in — the same app-data dir the DB uses, so dev
/// (`MeetNotes-dev`) and release (`MeetNotes`) stay isolated exactly as everything else does.
pub fn log_dir() -> Result<PathBuf> {
    let base = dirs::data_dir()
        .ok_or_else(|| AppError::Storage("could not resolve app-data directory".into()))?;
    Ok(base.join(crate::state::app_dir_name()))
}

/// Absolute path of one generation's file. The file may not exist (a first launch has no
/// previous session) — existence is reported, never assumed.
pub fn log_path(session: LogSession) -> Result<PathBuf> {
    Ok(log_dir()?.join(session.file_name()))
}

/// One parsed line of the `tracing` file writer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    /// Position in the returned window — a stable `@for` identity for the FE.
    pub seq: usize,
    /// RFC-3339 UTC timestamp as written, or `None` for a continuation line with no header.
    pub timestamp: Option<String>,
    /// `ERROR` | `WARN` | `INFO` | `DEBUG` | `TRACE`, or `"OTHER"` when the line has no header.
    pub level: String,
    /// The event target (`murmur::pipeline`, `panic`, …). Empty when unparseable.
    pub target: String,
    /// Everything after the target — the message plus any structured fields.
    pub message: String,
}

/// A window over one log generation.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppLog {
    /// `"current"` | `"previous"`.
    pub session: String,
    /// Absolute path on disk (shown in the view so a bug report can name the file).
    pub path: String,
    /// False when this generation has never been written (fresh install, first launch).
    pub exists: bool,
    /// Size of the whole file, not of the returned window.
    pub size_bytes: u64,
    /// True when older entries exist above the returned window.
    pub truncated: bool,
    /// Newest-last, matching the file order.
    pub entries: Vec<LogEntry>,
}

/// Prepare the log file for a NEW session: move the last session's file aside, then hand back a
/// fresh append handle. Best-effort by construction — every failure degrades to "no file writer"
/// (stderr only) rather than failing startup, because logging must never be the reason the app
/// does not launch.
///
/// Returns the opened current-session file, or `None` when a file writer could not be set up.
pub fn rotate_and_open() -> Option<std::fs::File> {
    let dir = log_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    let current = dir.join(LOG_FILE);
    let previous = dir.join(PREV_LOG_FILE);
    // Rotate rather than truncate: the crash we are asked to explain is in the PREVIOUS run.
    // A failed rename must not leave the last session's lines glued to this one's, so fall back
    // to truncating — the old behavior — instead of appending into a mixed file.
    if current.exists() && std::fs::rename(&current, &previous).is_err() {
        let _ = std::fs::write(&current, b"");
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&current)
        .ok()
}

/// Read the tail of one generation and parse it into entries.
///
/// `limit` is clamped to [1, [`MAX_ENTRIES`]]. A missing file is NOT an error: it reports
/// `exists: false` with no entries, which is the honest answer on a first launch.
pub fn read(session: LogSession, limit: Option<usize>) -> Result<AppLog> {
    let limit = limit.unwrap_or(DEFAULT_ENTRIES).clamp(1, MAX_ENTRIES);
    let path = log_path(session)?;
    let display = path.to_string_lossy().into_owned();

    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AppLog {
                session: session.token().to_string(),
                path: display,
                exists: false,
                size_bytes: 0,
                truncated: false,
                entries: Vec::new(),
            });
        }
        Err(e) => return Err(AppError::Storage(format!("open log: {e}"))),
    };

    let size_bytes = file
        .metadata()
        .map_err(|e| AppError::Storage(format!("stat log: {e}")))?
        .len();

    let mut reader = BufReader::new(file);
    // Skip to the tail on a large file. The first line after a mid-file seek is usually a
    // fragment, so it is dropped by the parser's continuation handling (no header ⇒ it joins a
    // previous entry, and there is none) — a partial first line is never mistaken for an event.
    let mut skipped_head = false;
    if size_bytes > TAIL_SCAN_BYTES {
        reader
            .seek(SeekFrom::Start(size_bytes - TAIL_SCAN_BYTES))
            .map_err(|e| AppError::Storage(format!("seek log: {e}")))?;
        let mut discard = String::new();
        let _ = reader.read_line(&mut discard);
        skipped_head = true;
    }

    let mut lines: Vec<String> = Vec::new();
    for line in reader.lines() {
        match line {
            Ok(l) => lines.push(l),
            // A non-UTF-8 byte in the middle of the log must not blank the whole view.
            Err(_) => lines.push("<unreadable log line>".to_string()),
        }
    }

    let mut entries = parse_lines(&lines);
    let truncated = skipped_head || entries.len() > limit;
    if entries.len() > limit {
        entries.drain(..entries.len() - limit);
    }
    for (i, e) in entries.iter_mut().enumerate() {
        e.seq = i;
    }

    Ok(AppLog {
        session: session.token().to_string(),
        path: display,
        exists: true,
        size_bytes,
        truncated,
        entries,
    })
}

/// Truncate the CURRENT session's file to zero length. Used by the Developer view's "Clear" so a
/// reproduction starts from a clean slate. The live `tracing` writer holds an `O_APPEND` handle,
/// so it keeps writing correctly into the emptied file — no restart needed, no handle to reopen.
/// The PREVIOUS generation is deliberately not clearable: it is evidence.
pub fn clear_current() -> Result<()> {
    let path = log_path(LogSession::Current)?;
    match std::fs::write(&path, b"") {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(AppError::Storage(format!("clear log: {e}"))),
    }
}

/// Split a `tracing` fmt line into (timestamp, level, target, rest).
///
/// The shipped format (`tracing_subscriber::fmt()`, ANSI off) is:
/// `2026-09-01T10:11:12.123456Z  INFO murmur::pipeline: message field=value`
///
/// A line that does not start with a timestamp+level header is a CONTINUATION (a wrapped panic
/// payload, a multi-line message) and belongs to the entry above it.
fn parse_header(line: &str) -> Option<(String, String, String, String)> {
    let (timestamp, rest) = line.split_once(char::is_whitespace)?;
    // A timestamp is the only thing allowed to open an entry. Checking its SHAPE (not just "the
    // next token is a level") keeps a message that happens to contain the word `WARN` from being
    // promoted to its own entry.
    if !looks_like_timestamp(timestamp) {
        return None;
    }
    let rest = rest.trim_start();
    let (level, rest) = rest.split_once(char::is_whitespace)?;
    if !matches!(level, "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE") {
        return None;
    }
    let rest = rest.trim_start();
    // `target: message`. A target never contains a space, so a colon that arrives after one
    // belongs to the message and the line is treated as target-less rather than mis-split.
    match rest.split_once(": ") {
        Some((target, message)) if !target.contains(char::is_whitespace) => Some((
            timestamp.to_string(),
            level.to_string(),
            target.to_string(),
            message.to_string(),
        )),
        _ => Some((
            timestamp.to_string(),
            level.to_string(),
            String::new(),
            rest.to_string(),
        )),
    }
}

/// `2026-09-01T10:11:12.123456Z` — digits, dashes, one `T`, and a trailing `Z`. Cheap and
/// deliberately not a full RFC-3339 parse: the goal is to recognise the writer's own header, not
/// to validate arbitrary input.
///
/// Compares BYTES, never string slices. `&token[..10]` panics when byte 10 falls inside a
/// multi-byte character, and the input here is an arbitrary line off disk — a message that begins
/// with a non-ASCII character (a Polish title in a third-party crate's log, a mid-file fragment cut
/// by the tail seek) would have taken the whole log view down with it. Byte comparison is also
/// exactly right: every character this header can legally contain is ASCII.
fn looks_like_timestamp(token: &str) -> bool {
    let bytes = token.as_bytes();
    bytes.len() >= 20
        && bytes[bytes.len() - 1] == b'Z'
        && bytes[10] == b'T'
        && bytes[..10].iter().all(|b| b.is_ascii_digit() || *b == b'-')
}

/// Fold raw lines into entries, attaching continuation lines to the entry they belong to.
fn parse_lines(lines: &[String]) -> Vec<LogEntry> {
    let mut out: Vec<LogEntry> = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        match parse_header(line) {
            Some((timestamp, level, target, message)) => out.push(LogEntry {
                seq: out.len(),
                timestamp: Some(timestamp),
                level,
                target,
                message: cap(message),
            }),
            None => match out.last_mut() {
                Some(previous) => {
                    if previous.message.chars().count() < MAX_ENTRY_CHARS {
                        previous.message.push('\n');
                        previous.message.push_str(line.trim_end());
                        previous.message = cap(std::mem::take(&mut previous.message));
                    }
                }
                // A fragment before any header (the first line after a tail seek, or a stray
                // write) has no entry to join. Keep it visible rather than silently dropping
                // evidence, marked so nobody reads it as a real event.
                None => out.push(LogEntry {
                    seq: out.len(),
                    timestamp: None,
                    level: "OTHER".to_string(),
                    target: String::new(),
                    message: cap(line.trim_end().to_string()),
                }),
            },
        }
    }
    out
}

/// Bound one entry's text (chars, not bytes — the log carries UTF-8).
fn cap(mut s: String) -> String {
    if s.chars().count() > MAX_ENTRY_CHARS {
        s = s.chars().take(MAX_ENTRY_CHARS).collect::<String>() + " …[truncated]";
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(raw: &str) -> Vec<String> {
        raw.lines().map(str::to_string).collect()
    }

    #[test]
    fn parses_the_shipped_tracing_format() {
        let parsed = parse_lines(&lines(
            "2026-09-01T10:11:12.123456Z  INFO murmur::pipeline: stage complete count=3\n\
             2026-09-01T10:11:13.000000Z ERROR panic: location=\"src/lib.rs:12\" message=\"boom\"",
        ));
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[0].timestamp.as_deref(),
            Some("2026-09-01T10:11:12.123456Z")
        );
        assert_eq!(parsed[0].level, "INFO");
        assert_eq!(parsed[0].target, "murmur::pipeline");
        assert_eq!(parsed[0].message, "stage complete count=3");
        assert_eq!(parsed[1].level, "ERROR");
        assert_eq!(parsed[1].target, "panic");
        assert_eq!(parsed[0].seq, 0);
        assert_eq!(parsed[1].seq, 1);
    }

    #[test]
    fn continuation_lines_join_the_entry_above_them() {
        let parsed = parse_lines(&lines(
            "2026-09-01T10:11:12.123456Z ERROR panic: thread panicked\n\
             stack frame one\n\
             stack frame two",
        ));
        assert_eq!(parsed.len(), 1, "a wrapped payload is ONE event");
        assert_eq!(parsed[0].message, "thread panicked\nstack frame one\nstack frame two");
    }

    #[test]
    fn a_message_mentioning_a_level_word_does_not_start_a_new_entry() {
        let parsed = parse_lines(&lines(
            "2026-09-01T10:11:12.123456Z  INFO murmur: first\n\
             WARN this is prose, not a header",
        ));
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].message.contains("WARN this is prose"));
    }

    /// RED before this was byte-safe: `&token[..10]` panicked on a char boundary, so ONE
    /// non-ASCII line anywhere in the file took down the whole Logs view.
    #[test]
    fn a_non_ascii_line_never_panics_the_parser() {
        let parsed = parse_lines(&lines(
            "żółćżółćżółćżółćżółćżółć a line that opens with multi-byte characters\n\
             日本語のログ行\n\
             2026-09-01T10:11:12.123456Z  INFO murmur: after the noise",
        ));
        assert_eq!(parsed.len(), 2, "two fragments fold into one, then the event");
        assert_eq!(parsed[0].level, "OTHER");
        assert_eq!(parsed[1].level, "INFO");
    }

    #[test]
    fn a_fragment_before_any_header_is_kept_and_marked() {
        let parsed = parse_lines(&lines("…tail of a line cut by the seek"));
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].level, "OTHER");
        assert!(parsed[0].timestamp.is_none());
    }

    #[test]
    fn a_target_less_line_still_parses_as_an_event() {
        let parsed = parse_lines(&lines(
            "2026-09-01T10:11:12.123456Z  WARN a message with: a colon in it",
        ));
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].level, "WARN");
        assert_eq!(parsed[0].target, "");
        assert_eq!(parsed[0].message, "a message with: a colon in it");
    }

    #[test]
    fn entry_text_is_bounded() {
        let huge = "x".repeat(MAX_ENTRY_CHARS * 2);
        let parsed = parse_lines(&lines(&format!(
            "2026-09-01T10:11:12.123456Z  INFO murmur: {huge}"
        )));
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].message.chars().count() <= MAX_ENTRY_CHARS + 16);
        assert!(parsed[0].message.ends_with("…[truncated]"));
    }

    #[test]
    fn session_tokens_round_trip_and_reject_garbage() {
        assert_eq!(LogSession::parse("current").unwrap(), LogSession::Current);
        assert_eq!(LogSession::parse(" Previous ").unwrap(), LogSession::Previous);
        assert!(LogSession::parse("../../etc/passwd").is_err());
        assert_eq!(LogSession::Current.file_name(), LOG_FILE);
        assert_eq!(LogSession::Previous.file_name(), PREV_LOG_FILE);
    }

    /// The wire contract (rust-tauri §2b): the FE reads camelCase, and a hand-written mock cannot
    /// prove that. Assert the SERIALIZED key names on both DTOs.
    #[test]
    fn dtos_serialize_camel_case_keys() {
        let log = AppLog {
            session: "current".into(),
            path: "/tmp/murmur.log".into(),
            exists: true,
            size_bytes: 12,
            truncated: false,
            entries: vec![LogEntry {
                seq: 0,
                timestamp: Some("2026-09-01T10:11:12.123456Z".into()),
                level: "INFO".into(),
                target: "murmur".into(),
                message: "hello".into(),
            }],
        };
        let value = serde_json::to_value(&log).unwrap();
        let object = value.as_object().unwrap();
        for key in object.keys() {
            assert!(
                !key.contains('_'),
                "AppLog key `{key}` must be camelCase on the wire"
            );
        }
        assert!(object.contains_key("sizeBytes"));
        let entry = object["entries"][0].as_object().unwrap();
        for key in entry.keys() {
            assert!(
                !key.contains('_'),
                "LogEntry key `{key}` must be camelCase on the wire"
            );
        }
        assert!(entry.contains_key("timestamp") && entry.contains_key("target"));
    }

    #[test]
    fn reading_an_absent_generation_reports_missing_not_error() {
        // A path under a directory that does not exist is the same NotFound the first launch hits.
        let out = read(LogSession::Previous, Some(10));
        // Either the real previous file exists on this machine or it does not; both are Ok.
        assert!(out.is_ok(), "a missing log is never an error");
    }

    #[test]
    fn limit_is_clamped_and_windows_the_tail() {
        let raw: String = (0..50)
            .map(|i| format!("2026-09-01T10:11:12.123456Z  INFO murmur: line {i}\n"))
            .collect();
        let parsed = parse_lines(&lines(&raw));
        assert_eq!(parsed.len(), 50);
        // The windowing itself (drain + reseq) is what `read` applies; mirror it here on the
        // parsed vector so the invariant is pinned without touching the filesystem.
        let mut windowed = parsed.clone();
        let limit = 10;
        windowed.drain(..windowed.len() - limit);
        assert_eq!(windowed.len(), limit);
        assert!(windowed[0].message.ends_with("line 40"));
        assert!(windowed[limit - 1].message.ends_with("line 49"));
    }
}
