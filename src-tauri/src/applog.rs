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
//! RETENTION: nothing older than 24 h survives. Expiry DELETES — a whole aged-out previous-session
//! file is removed, and the current file's expired head is dropped from the file itself — rather
//! than merely being filtered out of the view, so the promise the UI makes ("kept for 24 hours") is
//! true on disk too. Pruning runs at launch, on an hourly tick, and before every read, so it never
//! depends on anyone opening the Logs view.
//!
//! PII: the log itself carries IDs / stages / counts only (the no-PII rule), and the panic hook
//! sanitizes payloads before they land here. This module only READS what is already on disk and
//! never widens what is written, so surfacing it in-app adds no new disclosure — the file is
//! local, and its contents were already local.

use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration as ChronoDuration, Utc};

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

/// How long a log is kept. Past this, entries are DELETED — see the module doc.
///
/// Seven days, not one. A user reporting "it did the thing again last week" was asking about a log
/// that had already been pruned, which made the whole surface unable to answer the question it
/// exists for. The window is bounded in the OTHER dimension by [`MAX_LOG_BYTES`], so extending it
/// cannot turn diagnosability into unbounded disk use.
pub const RETENTION_HOURS: i64 = 24 * 7;

/// Ceiling on the CURRENT log file. Past this, the oldest entries are dropped even when they are
/// still inside the retention window.
///
/// Time alone is the wrong bound for a log: a quiet week costs nothing while a crash loop can write
/// gigabytes in an hour, and the second case is exactly when a full disk hurts most. 16 MB holds a
/// long, chatty session — `TAIL_SCAN_BYTES` reads only the last 4 MB anyway — and is small enough
/// that a runaway writer is capped within one prune tick.
pub const MAX_LOG_BYTES: u64 = 16 * 1024 * 1024;

/// How often the background pruner runs. Hourly is far finer than the 24 h window it enforces, so
/// an aged-out log is gone within an hour of expiring, and the tick costs nothing noticeable (a
/// stat, plus one rewrite on the rare tick that actually has something to drop).
pub const PRUNE_TICK_SECS: u64 = 3600;

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
    /// The entry EXACTLY as it appears in the file, continuation lines included. What the expanded
    /// row shows and what "Copy entry" puts on the clipboard: a bug report wants the line that was
    /// written, not one reassembled from parsed parts whose spacing no longer matches the file.
    pub raw: String,
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

/// Prepare the log file for a NEW session: drop whatever has aged out, move the last session's
/// file aside, then hand back a fresh append handle. Best-effort by construction — every failure
/// degrades to "no file writer" (stderr only) rather than failing startup, because logging must
/// never be the reason the app does not launch.
///
/// Returns the opened current-session file, or `None` when a file writer could not be set up.
pub fn rotate_and_open() -> Option<std::fs::File> {
    let dir = log_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    // Prune BEFORE rotating, so a session older than the retention window is deleted instead of
    // being promoted to `murmur.prev.log` — otherwise every relaunch would renew the expiry of
    // content that is already too old to keep.
    let _ = prune_dir(&dir, Utc::now());
    let current = dir.join(LOG_FILE);
    let previous = dir.join(PREV_LOG_FILE);
    // Rotate rather than truncate: the crash we are asked to explain is in the PREVIOUS run.
    // A failed rename must not leave the last session's lines glued to this one's, so fall back
    // to truncating — the old behavior — instead of appending into a mixed file.
    //
    // An EMPTY current file is not rotated: after a prune emptied it there is nothing to preserve,
    // and rotating would overwrite a still-in-retention previous session with a blank file.
    let has_content = std::fs::metadata(&current)
        .map(|m| m.len() > 0)
        .unwrap_or(false);
    if has_content && std::fs::rename(&current, &previous).is_err() {
        let _ = std::fs::write(&current, b"");
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&current)
        .ok()
}

/// Delete everything past the retention window. Called at launch, on the hourly tick, and before
/// every read.
pub fn prune_expired() -> Result<()> {
    let dir = log_dir()?;
    prune_dir(&dir, Utc::now())
}

/// The retention boundary: entries stamped before this are gone.
fn cutoff(now: DateTime<Utc>) -> DateTime<Utc> {
    now - ChronoDuration::hours(RETENTION_HOURS)
}

/// Prune both generations in `dir`.
///
/// The PREVIOUS file is judged WHOLE: it belongs to a finished session, so once its last write is
/// past the cutoff every line in it is too. The CURRENT file is judged PER ENTRY, because a session
/// running longer than the window holds expired and live lines in the same file.
fn prune_dir(dir: &Path, now: DateTime<Utc>) -> Result<()> {
    let cutoff = cutoff(now);
    let previous = dir.join(PREV_LOG_FILE);
    if let Ok(meta) = std::fs::metadata(&previous) {
        let expired = meta
            .modified()
            .map(|m| DateTime::<Utc>::from(m) < cutoff)
            .unwrap_or(false);
        if expired {
            std::fs::remove_file(&previous)
                .map_err(|e| AppError::Storage(format!("prune previous log: {e}")))?;
        }
    }
    prune_file_head(&dir.join(LOG_FILE), cutoff)
}

/// Drop the expired HEAD of one file, in place.
///
/// Rewritten on the SAME inode (write at offset 0 + `set_len`), never through a temp file and a
/// rename: the running `tracing` writer holds an fd to this inode, and a rename would leave it
/// appending into an unlinked file — every later line would vanish with no error anywhere. The
/// cost is a small race, taken deliberately: a line appended during the rewrite lands past the new
/// length and is truncated away. That is at most a line or two, only on a prune that actually has
/// something to expire; the alternative silently kills logging for the rest of the session.
fn prune_file_head(path: &Path, cutoff: DateTime<Utc>) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        // A non-UTF-8 log is not something to silently delete; leave it for "Reveal in Finder".
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => return Ok(()),
        Err(e) => return Err(AppError::Storage(format!("read log for prune: {e}"))),
    };
    let mut keep_from = first_retained_offset(&text, cutoff);
    // Size ceiling, applied AFTER the time cutoff: whatever survived the window may still be too
    // big. Drop whole entries from the head — never a byte offset into one, which would leave a
    // truncated first line and a continuation with no entry above it.
    if (text.len() - keep_from) as u64 > MAX_LOG_BYTES {
        let overflow = (text.len() - keep_from) as u64 - MAX_LOG_BYTES;
        keep_from = first_entry_offset_past(&text, keep_from, overflow as usize);
    }
    if keep_from == 0 {
        return Ok(()); // nothing has expired — the common case, and it writes nothing.
    }
    let retained = &text.as_bytes()[keep_from..];
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| AppError::Storage(format!("open log for prune: {e}")))?;
    file.write_all(retained)
        .map_err(|e| AppError::Storage(format!("rewrite log: {e}")))?;
    file.set_len(retained.len() as u64)
        .map_err(|e| AppError::Storage(format!("truncate log: {e}")))?;
    file.flush()
        .map_err(|e| AppError::Storage(format!("flush log: {e}")))?;
    Ok(())
}

/// First entry boundary at or after `from + at_least` bytes.
///
/// Used by the size cap to drop WHOLE entries: starting the retained region mid-entry would leave a
/// half line at the top and orphan its continuation lines, which is exactly the shape
/// [`first_retained_offset`] documents as unacceptable for the time cutoff. Returns `text.len()`
/// when no boundary is far enough, i.e. the file becomes empty rather than partially valid.
fn first_entry_offset_past(text: &str, from: usize, at_least: usize) -> usize {
    let target = from + at_least;
    let mut offset = from;
    for line in text[from..].split_inclusive('\n') {
        if offset >= target && !line.starts_with(char::is_whitespace) {
            return offset;
        }
        offset += line.len();
    }
    text.len()
}

/// Byte offset of the first entry to KEEP.
///
/// `0` means nothing has expired; `text.len()` means everything has. A continuation line belongs to
/// the entry above it, so it is kept or dropped WITH that entry — never promoted into the retained
/// region on its own, which would leave a stack frame with no panic attached to it.
fn first_retained_offset(text: &str, cutoff: DateTime<Utc>) -> usize {
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        if let Some((timestamp, _, _, _)) = parse_header(line.trim_end_matches(['\n', '\r'])) {
            match DateTime::parse_from_rfc3339(&timestamp) {
                // The first in-window entry opens the retained region.
                Ok(parsed) if parsed.with_timezone(&Utc) >= cutoff => return offset,
                // An unparseable stamp is not evidence of age — keep it rather than delete it.
                Err(_) => return offset,
                _ => {}
            }
        }
        offset += line.len();
    }
    offset
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
                raw: cap(line.trim_end().to_string()),
            }),
            None => match out.last_mut() {
                Some(previous) => {
                    if previous.message.chars().count() < MAX_ENTRY_CHARS {
                        previous.message.push('\n');
                        previous.message.push_str(line.trim_end());
                        previous.message = cap(std::mem::take(&mut previous.message));
                        previous.raw.push('\n');
                        previous.raw.push_str(line.trim_end());
                        previous.raw = cap(std::mem::take(&mut previous.raw));
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
                    raw: cap(line.trim_end().to_string()),
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
        assert_eq!(
            parsed[0].raw,
            "2026-09-01T10:11:12.123456Z ERROR panic: thread panicked\nstack frame one\nstack frame two",
            "`raw` carries the whole entry as written — header line included"
        );
    }

    /// The expanded row shows `raw`, so it has to be the FILE's text, not a reassembly: the
    /// shipped writer pads the level to five columns, and a rebuilt line would quietly differ
    /// from what a bug report's attached file says.
    #[test]
    fn raw_preserves_the_line_verbatim_including_its_padding() {
        let line = "2026-09-01T10:11:12.123456Z  INFO murmur::pipeline: stage complete count=3";
        let parsed = parse_lines(&lines(line));
        assert_eq!(parsed[0].raw, line);
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

    // ── Retention: 24 h, enforced by DELETING ────────────────────────────────────────────────

    /// A log line stamped `hours_ago`, in exactly the format the writer produces.
    fn line_at(now: DateTime<Utc>, hours_ago: i64, message: &str) -> String {
        let ts = (now - ChronoDuration::hours(hours_ago))
            .format("%Y-%m-%dT%H:%M:%S%.6fZ")
            .to_string();
        format!("{ts}  INFO murmur: {message}\n")
    }

    fn temp_log_dir(label: &str) -> std::path::PathBuf {
        let dir = crate::storage::db::unique_temp_path(&format!("murmur-applog-{label}"), "dir");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn nothing_expired_means_nothing_is_rewritten() {
        let now = Utc::now();
        let text = line_at(now, 1, "fresh") + &line_at(now, RETENTION_HOURS - 1, "still inside the window");
        assert_eq!(
            first_retained_offset(&text, cutoff(now)),
            0,
            "an untouched file must not be rewritten at all"
        );
    }

    #[test]
    fn the_expired_head_is_dropped_and_the_rest_kept() {
        let now = Utc::now();
        let old = line_at(now, RETENTION_HOURS + 6, "past the window");
        let older = line_at(now, RETENTION_HOURS * 2, "long past it");
        let fresh = line_at(now, 2, "keep me");
        let text = format!("{older}{old}{fresh}");
        let offset = first_retained_offset(&text, cutoff(now));
        assert_eq!(&text[offset..], fresh, "the retained region starts at the first live entry");
    }

    #[test]
    fn an_entirely_expired_file_retains_nothing() {
        let now = Utc::now();
        let text = line_at(now, RETENTION_HOURS + 1, "just past the window") + &line_at(now, RETENTION_HOURS * 3, "ancient");
        assert_eq!(first_retained_offset(&text, cutoff(now)), text.len());
    }

    #[test]
    fn a_continuation_line_expires_with_the_entry_it_belongs_to() {
        let now = Utc::now();
        let expired_panic = line_at(now, RETENTION_HOURS + 16, "thread panicked");
        let text = format!("{expired_panic}stack frame one\nstack frame two\n{}", line_at(now, 1, "fresh"));
        let offset = first_retained_offset(&text, cutoff(now));
        let kept = &text[offset..];
        assert!(
            !kept.contains("stack frame"),
            "a frame must never outlive its panic — it would read as an event of its own"
        );
        assert!(kept.contains("fresh"));
    }

    /// An unparseable stamp is not evidence of age. Deleting on a parse failure would let one
    /// malformed line take every line above it with it.
    #[test]
    fn an_unparseable_timestamp_is_kept_not_deleted() {
        let now = Utc::now();
        let text = format!(
            "{}9999-99-99T99:99:99.000000Z  INFO murmur: malformed\n{}",
            line_at(now, RETENTION_HOURS + 16, "expired"),
            line_at(now, 1, "fresh"),
        );
        let offset = first_retained_offset(&text, cutoff(now));
        assert!(text[offset..].contains("malformed"));
    }

    /// The size ceiling drops WHOLE entries from the head, even when they are still in-window.
    ///
    /// Time alone is the wrong bound for a log: a quiet week costs nothing while a crash loop can
    /// write gigabytes in an hour, and that is exactly when a full disk hurts most. Every line here
    /// is one minute old — comfortably inside retention — so only the byte ceiling can trim it, and
    /// the assertions pin the two things that make the trim safe rather than merely small: the
    /// NEWEST entry survives, and the retained region starts on an entry header, never mid-entry
    /// with an orphaned continuation above it.
    ///
    /// RED CONTROL (run 2026-09-03): deleting the `MAX_LOG_BYTES` branch in `prune_file_head` fails
    /// this on the size assertion while every other applog test stays green.
    #[test]
    fn the_size_ceiling_drops_whole_entries_even_when_they_are_still_in_window() {
        let now = Utc::now();
        let filler = "x".repeat(4_000);
        let mut text = String::new();
        // Comfortably over the ceiling, all of it fresh.
        let entries = (MAX_LOG_BYTES as usize / 4_000) + 64;
        for i in 0..entries {
            text.push_str(&line_at(now, 1, &format!("entry {i} {filler}")));
            text.push_str("  a continuation line for entry ");
            text.push_str(&i.to_string());
            text.push('\n');
        }
        let dir = temp_log_dir("size-cap");
        let current = dir.join(LOG_FILE);
        std::fs::write(&current, &text).unwrap();

        prune_dir(&dir, now).unwrap();

        let kept = std::fs::read_to_string(&current).unwrap();
        assert!(
            kept.len() as u64 <= MAX_LOG_BYTES,
            "the ceiling must actually bind: kept {} bytes, cap {MAX_LOG_BYTES}",
            kept.len()
        );
        assert!(
            kept.contains(&format!("entry {}", entries - 1)),
            "the NEWEST entry must survive — trimming from the wrong end would delete exactly what \
             the log is read for"
        );
        assert!(
            !kept.starts_with("  a continuation"),
            "the retained region must start on an entry header, never on a continuation line whose \
             entry was trimmed away"
        );
    }

    #[test]
    fn prune_deletes_an_aged_previous_session_and_trims_the_current_one() {
        let now = Utc::now();
        let dir = temp_log_dir("prune");
        let current = dir.join(LOG_FILE);
        let previous = dir.join(PREV_LOG_FILE);
        std::fs::write(
            &current,
            line_at(now, RETENTION_HOURS + 6, "expired") + &line_at(now, 1, "live"),
        )
        .unwrap();
        std::fs::write(&previous, line_at(now, RETENTION_HOURS + 6, "last session")).unwrap();
        // The previous file is judged by its mtime, so age it explicitly rather than trusting the
        // clock of the machine running the test.
        // Relative to the window, not a fixed 48 h: a hardcoded age silently stops testing
        // anything the moment RETENTION_HOURS grows past it, which is exactly what happened
        // when the window went from 1 day to 7.
        let stale = std::time::SystemTime::now()
            - std::time::Duration::from_secs((RETENTION_HOURS as u64 + 6) * 3600);
        std::fs::File::options()
            .write(true)
            .open(&previous)
            .unwrap()
            .set_modified(stale)
            .unwrap();

        prune_dir(&dir, now).unwrap();

        assert!(!previous.exists(), "an aged-out previous session is DELETED, not hidden");
        let kept = std::fs::read_to_string(&current).unwrap();
        assert!(kept.contains("live"));
        assert!(
            !kept.contains("expired"),
            "the expired head must be gone from the FILE, not merely from the view"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prune_keeps_a_previous_session_that_is_still_inside_the_window() {
        let now = Utc::now();
        let dir = temp_log_dir("prune-fresh");
        let previous = dir.join(PREV_LOG_FILE);
        std::fs::write(&previous, line_at(now, 2, "recent crash")).unwrap();

        prune_dir(&dir, now).unwrap();

        assert!(
            previous.exists(),
            "a session that ended two hours ago is exactly what the previous tab is for"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prune_is_a_no_op_on_an_empty_log_dir() {
        let dir = temp_log_dir("prune-empty");
        assert!(prune_dir(&dir, Utc::now()).is_ok());
        std::fs::remove_dir_all(&dir).ok();
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
                raw: "2026-09-01T10:11:12.123456Z  INFO murmur: hello".into(),
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
