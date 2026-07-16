use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

// ── Filename derivation ─────────────────────────────────────────────────────

/// Characters that are illegal in filenames on macOS/Obsidian or that Obsidian
/// reserves for wiki-link / tag syntax. We strip/replace them so the produced
/// filename is always safe and round-trips as a clean note title.
pub fn sanitize_title(title: &str) -> String {
    // Replace path separators and reserved characters with a space, collapse
    // runs of whitespace, then trim. Obsidian forbids: * " \ / < > : | ? and #
    // and ^ [ ] are link/anchor syntax that break titles.
    let replaced: String = title
        .chars()
        .map(|c| match c {
            '*' | '"' | '\\' | '/' | '<' | '>' | ':' | '|' | '?' | '#' | '^' | '[' | ']' => ' ',
            // Control chars (incl. newlines/tabs) → space.
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();

    // Collapse whitespace runs to a single space and trim ends.
    let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");

    // Trailing dots/spaces are stripped by some filesystems; remove them.
    let trimmed = collapsed.trim_matches(|c: char| c == '.' || c.is_whitespace());

    if trimmed.is_empty() {
        "Untitled".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Convert an ISO date/time string into the `YYYY-MM-DD HHmm` filename prefix.
///
/// Accepts either a date-only `"2026-06-24"` (time defaults to `0000`) or a full
/// ISO 8601 timestamp like `"2026-06-24T14:30:05Z"` / `"2026-06-24 14:30"`.
fn date_prefix(date_iso: &str) -> Result<String> {
    let s = date_iso.trim();
    if s.is_empty() {
        return Err(AppError::Export("empty date_iso".to_string()));
    }

    // Split off the date part (before 'T' or the first space).
    let (date_part, time_part) = match s.find(['T', ' ']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };

    // Validate YYYY-MM-DD shape.
    let date_bits: Vec<&str> = date_part.split('-').collect();
    if date_bits.len() != 3
        || date_bits[0].len() != 4
        || !date_bits
            .iter()
            .all(|b| b.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(AppError::Export(format!(
            "date_iso is not YYYY-MM-DD: {date_iso}"
        )));
    }
    let ymd = format!("{}-{}-{}", date_bits[0], date_bits[1], date_bits[2]);

    // Derive HHmm from the time part if present, else 0000.
    let hhmm = match time_part {
        Some(t) => {
            // Strip timezone suffix / fractional seconds: keep up to "HH:MM".
            let t = t.trim();
            let core = t
                .trim_end_matches('Z')
                .split(['+', 'Z'])
                .next()
                .unwrap_or(t);
            let tbits: Vec<&str> = core.split(':').collect();
            if tbits.len() >= 2
                && tbits[0].len() <= 2
                && tbits[1].len() >= 2
                && tbits[0].chars().all(|c| c.is_ascii_digit())
                && tbits[1][..2].chars().all(|c| c.is_ascii_digit())
            {
                format!("{:0>2}{}", tbits[0], &tbits[1][..2])
            } else {
                "0000".to_string()
            }
        }
        None => "0000".to_string(),
    };

    Ok(format!("{ymd} {hhmm}"))
}

/// Build the base file stem (without `.md`): `YYYY-MM-DD HHmm - title`.
fn base_stem(title: &str, date_iso: &str) -> Result<String> {
    let prefix = date_prefix(date_iso)?;
    let clean_title = sanitize_title(title);
    Ok(format!("{prefix} - {clean_title}"))
}

// ── Atomic write ─────────────────────────────────────────────────────────────

/// Atomically write `markdown` into `vault_dir` (optionally `subfolder`) as a uniquely
/// named .md file derived from `title` + `date_iso`. Writes to a dotfile `.tmp` then
/// renames. On name collision appends " (N)". Returns the final path written.
pub fn write_note(
    vault_dir: &Path,
    subfolder: Option<&str>,
    title: &str,
    date_iso: &str,
    markdown: &str,
) -> Result<PathBuf> {
    if vault_dir.as_os_str().is_empty() {
        return Err(AppError::Export("empty vault_dir".to_string()));
    }

    // Resolve the target directory (vault + optional subfolder) and ensure it exists.
    let target_dir = match subfolder {
        Some(sub) if !sub.trim().is_empty() => vault_dir.join(sub),
        _ => vault_dir.to_path_buf(),
    };
    std::fs::create_dir_all(&target_dir)
        .map_err(|e| AppError::Export(format!("create vault dir failed: {e}")))?;

    let stem = base_stem(title, date_iso)?;

    // Find a non-colliding final path. The note is idempotent for identical
    // (date, title, content): if a file with the exact base name already exists
    // and its content is byte-identical to `markdown`, return it without writing
    // a duplicate. Otherwise, suffix " (N)".
    let final_path = resolve_unique_path(&target_dir, &stem, markdown.as_bytes())?;

    // If resolve returned an existing identical file, we're done (idempotent).
    if final_path_is_existing_identical(&final_path, markdown)? {
        return Ok(final_path);
    }

    // Atomic write: write to a hidden temp dotfile in the SAME directory (so the
    // rename is a same-filesystem atomic operation), fsync, then rename over the
    // final path. The temp name is unique to avoid clobbering a concurrent write.
    let tmp_name = format!(".{}.{}.murmur.tmp", sanitize_for_tmp(&stem), std::process::id());
    let tmp_path = target_dir.join(tmp_name);

    write_and_sync(&tmp_path, markdown).inspect_err(|_| {
        // Best-effort cleanup of the temp file on failure.
        let _ = std::fs::remove_file(&tmp_path);
    })?;

    std::fs::rename(&tmp_path, &final_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        AppError::Export(format!("atomic rename failed: {e}"))
    })?;

    Ok(final_path)
}

/// Returns true if `path` already exists and its bytes equal `markdown`.
fn final_path_is_existing_identical(path: &Path, markdown: &str) -> Result<bool> {
    final_path_is_existing_identical_bytes(path, markdown.as_bytes())
}

/// Byte-slice core of [`final_path_is_existing_identical`] (the external-edit preservation path
/// compares RAW bytes — an externally-edited file is not guaranteed to be UTF-8).
fn final_path_is_existing_identical_bytes(path: &Path, content: &[u8]) -> Result<bool> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes == content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(AppError::Export(format!("read existing note failed: {e}"))),
    }
}

/// Pick the destination path: `<dir>/<stem>.md`, or `<stem> (N).md` on collision.
/// If `<stem>.md` already exists with identical content, that path is returned
/// (idempotent re-export). If it exists with DIFFERENT content, we look for an
/// identical sibling `<stem> (N).md`; if found, return it; else allocate the next
/// free `(N)` slot.
fn resolve_unique_path(dir: &Path, stem: &str, content: &[u8]) -> Result<PathBuf> {
    let base = dir.join(format!("{stem}.md"));
    if !path_exists(&base)? {
        return Ok(base);
    }
    if final_path_is_existing_identical_bytes(&base, content)? {
        return Ok(base);
    }

    // Base is taken by different content; scan/allocate a "(N)" variant.
    for n in 1..=10_000 {
        let candidate = dir.join(format!("{stem} ({n}).md"));
        if !path_exists(&candidate)? {
            return Ok(candidate);
        }
        if final_path_is_existing_identical_bytes(&candidate, content)? {
            // Identical content already exported under this suffix → idempotent.
            return Ok(candidate);
        }
    }
    Err(AppError::Export(
        "exhausted collision suffixes (>10000) for note name".to_string(),
    ))
}

fn path_exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(AppError::Export(format!("stat failed: {e}"))),
    }
}

/// Make a stem safe to embed in the temp dotfile name (no path separators).
fn sanitize_for_tmp(stem: &str) -> String {
    stem.chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// Write `contents` to `path` and fsync both the file and its parent directory so
/// the subsequent rename is durable.
fn write_and_sync(path: &Path, contents: &str) -> Result<()> {
    write_and_sync_bytes(path, contents.as_bytes())
}

/// Byte-slice core of [`write_and_sync`] — the external-edit preservation path copies the CURRENT
/// file bytes verbatim (not guaranteed UTF-8), with the same durability discipline.
fn write_and_sync_bytes(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| AppError::Export(format!("open temp file failed: {e}")))?;

    file.write_all(contents)
        .map_err(|e| AppError::Export(format!("write temp file failed: {e}")))?;
    file.sync_all()
        .map_err(|e| AppError::Export(format!("fsync temp file failed: {e}")))?;
    drop(file);

    // fsync the directory so the rename's metadata change is durable.
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

// ── Export-collision guard: never clobber an external vault edit ─────────────

/// SHA-256 (lowercase hex) of the EXACT bytes Murmur writes to an exported vault `.md`. Stored in
/// `notes.exported_hash` / `documents.exported_hash` after every Murmur write, and compared against
/// the CURRENT file bytes before the next full overwrite — a mismatch means the user (or their own
/// vault-side agent) edited the file externally, and [`preserve_external_edit_if_any`] copies their
/// version aside before Murmur's DB-derived markdown lands.
pub fn note_content_hash(md: &str) -> String {
    sha256_hex(md.as_bytes())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Export-collision guard: if the file at `path` was edited EXTERNALLY since Murmur last wrote it
/// (its current bytes hash differently from `expected_hash`), copy the CURRENT bytes to a sibling
/// `<stem> (external edit YYYY-MM-DD HHMM).md` in the same directory BEFORE the caller overwrites
/// the canonical file. Returns `Some(sibling_path)` when an external edit was preserved (or already
/// is, byte-identical), `None` otherwise.
///
/// No sibling is created when:
/// - `expected_hash` is `None` — a LEGACY row exported before the guard shipped (grandfathered:
///   there is no baseline to compare against, so treat the file as Murmur's own);
/// - the file does not exist (nothing to preserve);
/// - the current bytes hash to `expected_hash` (untouched since Murmur's last write).
///
/// Naming reuses [`resolve_unique_path`]'s `" (N)"` collision logic, so a second preservation in
/// the same minute suffixes rather than clobbers, and a byte-identical existing sibling is reused
/// (no duplicate). The sibling is written with the same tmp+fsync+rename discipline as
/// [`write_note`], so it is durably on disk before the caller truncates the canonical file.
pub fn preserve_external_edit_if_any(
    path: &Path,
    expected_hash: Option<&str>,
) -> Result<Option<PathBuf>> {
    let stamp = chrono::Local::now().format("%Y-%m-%d %H%M").to_string();
    preserve_external_edit_with_stamp(path, expected_hash, &stamp)
}

/// Testable core of [`preserve_external_edit_if_any`] with the local-time minute stamp injected
/// (mirrors the injectable-`now` pattern of [`sweep_export_tmp_dir`], so the `" (N)"` collision
/// behavior is provable without racing a real minute boundary).
fn preserve_external_edit_with_stamp(
    path: &Path,
    expected_hash: Option<&str>,
    stamp: &str,
) -> Result<Option<PathBuf>> {
    let Some(expected) = expected_hash else {
        return Ok(None); // legacy row (pre-guard export) — grandfathered.
    };
    let current = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(AppError::Export(format!(
                "read note before overwrite failed: {e}"
            )))
        }
    };
    if sha256_hex(&current) == expected {
        return Ok(None); // untouched since Murmur's last write.
    }

    // External edit detected — preserve the CURRENT bytes as a sibling BEFORE any overwrite.
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Export("note path has no parent".into()))?;
    let stem = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or_else(|| AppError::Export("note path has no file stem".into()))?;
    let sibling_stem = format!("{stem} (external edit {stamp})");
    let sibling = resolve_unique_path(parent, &sibling_stem, &current)?;
    if final_path_is_existing_identical_bytes(&sibling, &current)? {
        // Already preserved byte-identical (e.g. two overwrites in one minute with no edit in
        // between the preservations) — no duplicate sibling.
        return Ok(Some(sibling));
    }
    let tmp_name = format!(
        ".{}.{}.murmur.tmp",
        sanitize_for_tmp(&sibling_stem),
        std::process::id()
    );
    let tmp_path = parent.join(tmp_name);
    write_and_sync_bytes(&tmp_path, &current).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp_path);
    })?;
    std::fs::rename(&tmp_path, &sibling).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        AppError::Export(format!("atomic rename failed: {e}"))
    })?;
    Ok(Some(sibling))
}

// ── Vault title listing ──────────────────────────────────────────────────────

/// List existing note titles (file stems of *.md) in the vault for [[link]] suggestions.
///
/// Recurses into subfolders but skips Obsidian's internal `.obsidian` config
/// directory and any other dotfolders / hidden files. Titles are the file stems
/// (filename without the `.md` extension), which is exactly how Obsidian resolves
/// `[[wiki-links]]`.
pub fn list_vault_titles(vault_dir: &Path) -> Result<Vec<String>> {
    let mut titles = Vec::new();
    if !path_exists(vault_dir)? {
        return Ok(titles);
    }
    collect_md_stems(vault_dir, &mut titles)?;
    titles.sort();
    titles.dedup();
    Ok(titles)
}

fn collect_md_stems(dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(AppError::Export(format!("read_dir failed: {e}"))),
    };

    for entry in entries {
        let entry = entry.map_err(|e| AppError::Export(format!("dir entry failed: {e}")))?;
        let path = entry.path();

        // Skip hidden files/dirs (covers `.obsidian`, `.trash`, our `.tmp` files).
        if let Some(name) = path.file_name().and_then(OsStr::to_str) {
            if name.starts_with('.') {
                continue;
            }
        }

        let file_type = entry
            .file_type()
            .map_err(|e| AppError::Export(format!("file_type failed: {e}")))?;

        if file_type.is_dir() {
            collect_md_stems(&path, out)?;
        } else if file_type.is_file() && path.extension().and_then(OsStr::to_str) == Some("md") {
            if let Some(stem) = path.file_stem().and_then(OsStr::to_str) {
                out.push(stem.to_string());
            }
        }
    }
    Ok(())
}

/// Immediate subdirectory names of the vault (skips hidden / `.obsidian`), used as
/// existing-folder hints for AI thematic filing.
pub fn list_subfolders(vault_dir: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(vault_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(AppError::Export(format!("read_dir failed: {e}"))),
    };
    for entry in entries {
        let entry = entry.map_err(|e| AppError::Export(format!("dir entry failed: {e}")))?;
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if let Some(name) = entry.file_name().to_str() {
                if !name.starts_with('.') {
                    out.push(name.to_string());
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Overwrite the note at `path` with `markdown` in place (atomic temp-write + rename).
/// Used when editing a note in-app so the SAME vault file is updated, not duplicated.
pub fn overwrite_note(path: &Path, markdown: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Export("note path has no parent".into()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| AppError::Export(format!("create note dir failed: {e}")))?;
    let tmp_path = parent.join(format!(".edit.{}.murmur.tmp", std::process::id()));
    write_and_sync(&tmp_path, markdown).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp_path);
    })?;
    std::fs::rename(&tmp_path, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        AppError::Export(format!("atomic rename failed: {e}"))
    })?;
    Ok(())
}

// ── Export temp-dotfile sweep ────────────────────────────────────────────────

/// A `.tmp` export dotfile older than this cannot belong to a live export (both `write_note` and
/// `overwrite_note` rename within a single synchronous call), so mtime is the fallback liveness
/// signal when the embedded PID has been recycled onto an unrelated process.
const STALE_EXPORT_TMP_AGE_SECS: u64 = 3600;

/// R2 — best-effort startup sweep of orphaned export temp DOTFILES in the vault. `write_note` writes
/// `.<stem>.<pid>.murmur.tmp` and `overwrite_note` writes `.edit.<pid>.murmur.tmp`, renaming
/// atomically over the final `.md`; `remove_file` fires only on the ERROR branch, so a SIGKILL
/// between the fsync and the rename orphans the dotfile in the user's vault (`collect_md_stems` skips
/// dotfiles but never deletes them). This reclaims that residue. The `.murmur.tmp` marker makes the
/// sweep provably OURS-only — a foreign third-party dotfile is never touched.
///
/// SAFE against a concurrent LIVE export: an entry is removed ONLY when its embedded PID is not a
/// live process, OR its mtime is older than [`STALE_EXPORT_TMP_AGE_SECS`]. So a `.tmp` written by
/// THIS still-running process (its own pid is live + mtime fresh) is never raced. Recurses into
/// subfolders but SKIPS `.obsidian` / `.trash` (and every other dotfolder). No PII: logs a COUNT
/// only — never a stem/path (they embed note titles).
pub fn sweep_stale_export_tmp(vault_dir: &Path) {
    let removed = sweep_export_tmp_dir(vault_dir, std::time::SystemTime::now());
    if removed > 0 {
        tracing::warn!(target: "export", removed, "swept orphaned export temp dotfiles at startup");
    }
}

/// Recursive worker for [`sweep_stale_export_tmp`], with `now` injected so the age check is testable.
/// Returns the count removed. Never errors (best-effort startup cleanup).
fn sweep_export_tmp_dir(dir: &Path, now: std::time::SystemTime) -> u32 {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let mut removed = 0u32;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            // Never descend into Obsidian's config / trash (or any dotfolder) — they are not ours.
            if name.starts_with('.') {
                continue;
            }
            removed += sweep_export_tmp_dir(&path, now);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        // Only OUR export temp shape: a dotfile ending `.tmp` whose penultimate `.`-segment is a PID.
        let Some(pid) = export_tmp_pid(name) else {
            continue;
        };
        // Remove only when the PID is dead OR the file is stale — never race a live export.
        let pid_live = pid_is_live(pid);
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .map(|age| age.as_secs() > STALE_EXPORT_TMP_AGE_SECS)
            .unwrap_or(false);
        if (!pid_live || stale) && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Parse the PID out of an export temp dotfile name, or `None` if it is not our shape. Matches both
/// `write_note`'s `.<stem>.<pid>.tmp` and `overwrite_note`'s `.edit.<pid>.tmp`: a name that STARTS
/// with `.`, ENDS with `.tmp`, and whose token immediately before `.tmp` parses as a u32 pid. A
/// stem may itself contain dots, so we key off the LAST two dot-segments only.
fn export_tmp_pid(name: &str) -> Option<u32> {
    // Require the Murmur-specific `.murmur.tmp` marker so the sweep only ever reclaims OUR export
    // temps — NEVER a foreign third-party dotfile of a similar `.<x>.<n>.tmp` shape in the vault
    // (the theoretical over-delete both reviewers flagged). Only `write_note`/`overwrite_note`
    // produce this marker.
    if !name.starts_with('.') || !name.ends_with(".murmur.tmp") {
        return None;
    }
    // Strip the `.murmur.tmp` marker, then the segment after the final '.' is the pid.
    let without_marker = name.strip_suffix(".murmur.tmp")?;
    let pid_seg = without_marker.rsplit('.').next()?;
    // Guard against a missing pid (`.murmur.tmp` alone): require a non-empty numeric segment AND at
    // least one more '.' before it (so `.<something>.<pid>.murmur.tmp`).
    if pid_seg.is_empty() || !without_marker.contains('.') {
        return None;
    }
    pid_seg.parse::<u32>().ok()
}

/// Whether `pid` is a currently-live process (best-effort, macOS-first). Uses `/bin/kill -0 <pid>`
/// — signal 0 sends NO signal, it is the canonical liveness probe: exit 0 ⇒ the process exists (or
/// exists but is owned by another user — still live), non-zero ⇒ gone (ESRCH). A dead pid means the
/// export that wrote the temp file is gone, so its orphan is safe to reclaim. No new deps (mirrors
/// the `/bin/kill -TERM` pattern the audio helpers already use); on a spawn failure we conservatively
/// treat the pid as LIVE so a temp file is never mistakenly reclaimed (mtime staleness still catches
/// it). NOTE: `kill -0` reports EPERM as a NON-zero exit on macOS, so we additionally fall back to
/// the mtime staleness check at the call site — this probe only needs to avoid a false "dead".
fn pid_is_live(pid: u32) -> bool {
    std::process::Command::new("/bin/kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        // Conservative: if we can't even probe, assume LIVE (don't race a real export); the mtime
        // staleness check at the call site still reclaims a genuinely-old orphan.
        .unwrap_or(true)
}

// ── Deep links + pinned moments ─────────────────────────────────────────────

/// Percent-encode a value for an `obsidian://` URL query parameter.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Build an `obsidian://open?vault=…&file=…` deep link to `note_path` inside `vault_dir`.
pub fn build_open_url(vault_dir: &Path, note_path: &Path) -> String {
    let vault_name = vault_dir
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("vault");
    let rel = note_path.strip_prefix(vault_dir).unwrap_or(note_path);
    let file = rel.with_extension("");
    format!(
        "obsidian://open?vault={}&file={}",
        percent_encode(vault_name),
        percent_encode(&file.to_string_lossy())
    )
}

/// Append a pinned-moment anchor line to a note's markdown (under a "## Pinned moments"
/// section, always at the end so the section stays contiguous). Pure — returns new markdown.
pub fn append_pin(markdown: &str, mmss: &str, label: &str, block_id: &str) -> String {
    let label = label.trim();
    let line = if label.is_empty() {
        format!("- **{mmss}** ^{block_id}")
    } else {
        format!("- **{mmss}** {label} ^{block_id}")
    };
    let mut md = markdown.to_string();
    if !md.ends_with('\n') {
        md.push('\n');
    }
    if !md.contains("## Pinned moments") {
        md.push_str("\n## Pinned moments\n");
    }
    md.push_str(&line);
    md.push('\n');
    md
}

// ── Re-Truth: append-only supersession stamps ───────────────────────────────

/// The managed heading Re-Truth stamps live under. Created once per note (idempotent) so repeated
/// stamps stay contiguous and never duplicate the heading.
pub const RETRUTH_SECTION: &str = "## Re-Truth updates";

/// APPEND a `[!superseded]` callout to a SOURCE note under the managed [`RETRUTH_SECTION`]. Pure +
/// APPEND-ONLY: no existing byte is touched (the safe verify-before-destroy shape — the caller
/// snapshots the pre-image, and undo restores it byte-identical). Idempotent: if the exact callout
/// BODY (predicate/old/new/link — everything but the date) is already present, the markdown is
/// returned UNCHANGED, so applying twice never double-stamps. `superseding_stem`, when present, adds
/// a `· see [[stem]]` wikilink to the note that superseded this fact (omitted when the superseding
/// note is sealed — never leak a locked meeting's title into an open note).
pub fn append_supersession_callout(
    markdown: &str,
    date: &str,
    predicate: &str,
    old_value: &str,
    new_value: &str,
    superseding_stem: Option<&str>,
) -> String {
    let link = superseding_stem
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!(" · see [[{s}]]"))
        .unwrap_or_default();
    let body = format!("> **{predicate}** — {old_value} → {new_value}{link}");
    let block = format!("> [!superseded] {date}\n{body}\n");
    append_under_section(markdown, &body, &block)
}

/// APPEND a `[!supersedes]` backlink callout to the SUPERSEDING note under the managed
/// [`RETRUTH_SECTION`] — the mirror of [`append_supersession_callout`], recording that THIS note's
/// fact supersedes an older one in `source_stem`. Pure, append-only, idempotent on the body line.
pub fn append_supersedes_callout(
    markdown: &str,
    date: &str,
    predicate: &str,
    old_value: &str,
    new_value: &str,
    source_stem: &str,
) -> String {
    let body = format!("> **{predicate}** — supersedes {old_value} → {new_value} in [[{source_stem}]]");
    let block = format!("> [!supersedes] {date}\n{body}\n");
    append_under_section(markdown, &body, &block)
}

/// Shared append-under-managed-section core. `marker` is the stable idempotence guard (the callout
/// body without its date); `block` is the full multi-line callout to append. Append-only: the input
/// markdown is never rewritten, only extended. Creates [`RETRUTH_SECTION`] once if absent.
fn append_under_section(markdown: &str, marker: &str, block: &str) -> String {
    // Idempotent: the exact body line already present → nothing to do (applying twice is a no-op).
    if markdown.contains(marker) {
        return markdown.to_string();
    }
    let mut out = markdown.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.contains(RETRUTH_SECTION) {
        out.push_str(&format!("\n{RETRUTH_SECTION}\n"));
    }
    out.push('\n');
    out.push_str(block);
    out
}

// ── Vault detection (from ~/Library/Application Support/obsidian/obsidian.json) ──

/// A detected Obsidian vault.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DetectedVault {
    /// The vault's display name (the final path component of its directory).
    pub name: String,
    /// Absolute filesystem path to the vault directory.
    pub path: String,
    /// Whether Obsidian currently has this vault open.
    pub is_open: bool,
}

/// Shape of `obsidian.json` (only the fields we read). Obsidian stores a map of
/// vault-id → `{ path, ts, open? }` under the `vaults` key.
#[derive(Debug, Deserialize)]
struct ObsidianConfig {
    #[serde(default)]
    vaults: std::collections::HashMap<String, ObsidianVaultEntry>,
}

#[derive(Debug, Deserialize)]
struct ObsidianVaultEntry {
    path: String,
    #[serde(default)]
    open: bool,
}

/// Default location of Obsidian's global config on macOS.
fn obsidian_config_path() -> Option<PathBuf> {
    // ~/Library/Application Support/obsidian/obsidian.json
    dirs::config_dir().map(|c| c.join("obsidian").join("obsidian.json"))
}

/// Detect Obsidian vaults registered on this machine by parsing Obsidian's global
/// `obsidian.json`. Returns vaults whose directory still exists on disk. If the
/// config file is missing or unreadable, returns an empty list (NOT an error) so
/// the UI can fall back to a manual folder pick.
pub fn detect_vaults() -> Result<Vec<DetectedVault>> {
    let Some(config_path) = obsidian_config_path() else {
        return Ok(Vec::new());
    };
    detect_vaults_from(&config_path)
}

/// Testable core of [`detect_vaults`]: parse a specific `obsidian.json` path.
pub fn detect_vaults_from(config_path: &Path) -> Result<Vec<DetectedVault>> {
    let bytes = match std::fs::read(config_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(AppError::Export(format!("read obsidian.json failed: {e}"))),
    };

    let config: ObsidianConfig = serde_json::from_slice(&bytes)
        .map_err(|e| AppError::Export(format!("parse obsidian.json failed: {e}")))?;

    let mut vaults: Vec<DetectedVault> = config
        .vaults
        .into_values()
        .filter_map(|entry| {
            let path = PathBuf::from(&entry.path);
            // Only surface vaults that still exist as directories on disk.
            if !path.is_dir() {
                return None;
            }
            let name = path
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or(&entry.path)
                .to_string();
            Some(DetectedVault {
                name,
                path: entry.path,
                is_open: entry.open,
            })
        })
        .collect();

    // Stable ordering: open vaults first, then alphabetical by name.
    vaults.sort_by(|a, b| b.is_open.cmp(&a.is_open).then_with(|| a.name.cmp(&b.name)));
    Ok(vaults)
}

// ── Provenance frontmatter injection (Phase 5) ──────────────────────────────

/// Inject model-provenance keys (`ai-provider:` and `ai-model:`) into the YAML frontmatter of a
/// Murmur note. The note is LLM-generated and always starts with a `---` / `---` YAML fence. If
/// the frontmatter is absent or malformed, the markdown is returned UNCHANGED (byte-identical).
///
/// **Rules:**
/// - `ai-provider`: the provider id (e.g. `"gateway"`, `"anthropic"`, `"claude_code"`). Always
///   included when `provider` is non-empty.
/// - `ai-model`: prefer `model_served` (what the API actually served); fall back to
///   `model_requested` (what we asked for). Omitted when neither is available.
/// - Both keys are omitted when the note already contains them (idempotent re-export).
/// - When `provider` is empty and both model fields are `None`, the markdown is returned unchanged.
///
/// Pure (no I/O, no state). The returned string has identical bytes to the input when no injection
/// is needed, so callers may compare identity cheaply.
pub fn inject_provenance_frontmatter(
    markdown: &str,
    provider: &str,
    model_requested: Option<&str>,
    model_served: Option<&str>,
) -> String {
    let provider = provider.trim();
    let effective_model = model_served.or(model_requested);

    // Nothing to inject — preserve byte identity.
    if provider.is_empty() && effective_model.is_none() {
        return markdown.to_string();
    }

    // The note must start with `---\n` to have a frontmatter block.
    let Some(rest_after_open) = markdown.strip_prefix("---\n") else {
        return markdown.to_string();
    };

    // Find the closing `---` line.
    let Some(close_pos) = rest_after_open.find("\n---\n").or_else(|| {
        // The block may end at the very last line with `---` followed by no body.
        if rest_after_open.ends_with("\n---") {
            Some(rest_after_open.len() - 4)
        } else {
            None
        }
    }) else {
        return markdown.to_string();
    };

    let fm_content = &rest_after_open[..close_pos]; // the YAML lines between the fences

    // Idempotent: if both keys are already present, nothing to do.
    let already_has_provider = fm_content.lines().any(|l| l.starts_with("ai-provider:"));
    let already_has_model = fm_content.lines().any(|l| l.starts_with("ai-model:"));
    if already_has_provider && already_has_model {
        return markdown.to_string();
    }

    // Build the new frontmatter content by appending only the missing keys.
    let mut new_fm = fm_content.to_string();
    if !new_fm.ends_with('\n') && !new_fm.is_empty() {
        new_fm.push('\n');
    }
    if !already_has_provider && !provider.is_empty() {
        new_fm.push_str(&format!("ai-provider: {}\n", provider));
    }
    if !already_has_model {
        if let Some(model) = effective_model {
            let trimmed = model.trim();
            if !trimmed.is_empty() {
                new_fm.push_str(&format!("ai-model: {}\n", trimmed));
            }
        }
    }

    // Reconstruct the full note.
    let after_close = &rest_after_open[close_pos..]; // starts with `\n---`
    format!("---\n{new_fm}{after_close}")
}

/// Stamp a content-free **PRIVACY RECEIPT** into a note's YAML front-matter — an HONEST
/// self-report of what left the device to produce this note.
///
/// This is a plain self-declared record, **not** a cryptographic attestation and **not** a
/// verifiable/provable claim: it is exactly as trustworthy as the app that wrote it. Its value is
/// that a local-only summary can state, in one screenshot-able line, that nothing egressed.
///
/// Mirrors [`inject_provenance_frontmatter`] byte-for-byte in structure (strip the opening
/// `---\n`, find the closing fence, skip keys already present, append the missing keys before the
/// closing fence, reconstruct). Pure — no I/O, no state — and byte-identical to the input when
/// there is nothing to inject or the note has no front-matter block.
///
/// Keys (all content-FREE — booleans / integer counts / non-PII host labels, NEVER note text,
/// transcript, attendee names, titles, keys, or DEK/KEK/CK material):
/// - `privacy-cloud-calls: 0` — stamped **only** when `local_only` (nothing left the device: a
///   loopback-ollama / on-device-reasoner summary). This is the strong local headline; `0` is
///   truthful exactly because [`egress_is_cloud`](crate::summarize::egress_is_cloud) — the SAME
///   classifier the consent gate uses — reports local.
/// - `privacy-egress-host: <host>` — for a cloud summary, the non-PII destination label
///   (`api.anthropic.com`, `claude_code (Anthropic CLI)`, a gateway `host:port`, …). Its presence
///   is the honest signal that the summary DID leave the device, and where.
/// - `privacy-pii-redacted: <n>` — for a cloud summary with a known count, how many PII items the
///   redaction firewall scrubbed before egress. Omitted when the count is unknown.
///
/// A numeric cloud-CALL count `> 0` is deliberately NOT stamped. The egress ledger is a global
/// rolling log (per-entry `meeting_id` is `None`), so a call count is not per-note attributable,
/// and stamping `1` would UNDER-count total cloud activity (entity-extraction / auto-organize also
/// call the cloud) — the dangerous direction for a privacy claim. The local-vs-host signal is the
/// honest headline; the numeric receipt for the cloud case is the redaction count, not a call
/// count. Values need no YAML quoting (host labels carry no `": "` colon-space; unquoted style
/// matches `inject_provenance_frontmatter`).
pub fn inject_privacy_receipt_frontmatter(
    markdown: &str,
    local_only: bool,
    egress_host: Option<&str>,
    redacted_pii: Option<u32>,
) -> String {
    // The content-free receipt key(s) to (potentially) inject, in stable order.
    let mut wanted: Vec<(&str, String)> = Vec::new();
    if local_only {
        // Strong local headline: nothing left the device to produce this note.
        wanted.push(("privacy-cloud-calls", "0".to_string()));
    } else {
        // Cloud summary: declare WHERE it went + how much PII the firewall scrubbed. No call COUNT
        // (not per-note attributable + would under-count — see the doc comment).
        if let Some(host) = egress_host.map(str::trim).filter(|h| !h.is_empty()) {
            wanted.push(("privacy-egress-host", host.to_string()));
        }
        if let Some(n) = redacted_pii {
            wanted.push(("privacy-pii-redacted", n.to_string()));
        }
    }

    // Nothing to inject — preserve byte identity.
    if wanted.is_empty() {
        return markdown.to_string();
    }

    // The note must start with `---\n` to have a frontmatter block.
    let Some(rest_after_open) = markdown.strip_prefix("---\n") else {
        return markdown.to_string();
    };

    // Find the closing `---` line (same logic as `inject_provenance_frontmatter`).
    let Some(close_pos) = rest_after_open.find("\n---\n").or_else(|| {
        if rest_after_open.ends_with("\n---") {
            Some(rest_after_open.len() - 4)
        } else {
            None
        }
    }) else {
        return markdown.to_string();
    };

    let fm_content = &rest_after_open[..close_pos]; // the YAML lines between the fences

    // Idempotent: keep only keys NOT already present (a defensive double-call within one export is
    // a no-op; a fresh (re)summarize always builds new markdown, so keys are stamped each time).
    let missing: Vec<(&str, String)> = wanted
        .into_iter()
        .filter(|(k, _)| {
            let prefix = format!("{k}:");
            !fm_content.lines().any(|l| l.starts_with(&prefix))
        })
        .collect();
    if missing.is_empty() {
        return markdown.to_string();
    }

    // Append the missing keys before the closing fence.
    let mut new_fm = fm_content.to_string();
    if !new_fm.ends_with('\n') && !new_fm.is_empty() {
        new_fm.push('\n');
    }
    for (k, v) in &missing {
        new_fm.push_str(&format!("{k}: {v}\n"));
    }

    // Reconstruct the full note.
    let after_close = &rest_after_open[close_pos..]; // starts with `\n---`
    format!("---\n{new_fm}{after_close}")
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(label: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "meetnotes-export-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn date_prefix_date_only() {
        assert_eq!(date_prefix("2026-06-24").unwrap(), "2026-06-24 0000");
    }

    #[test]
    fn date_prefix_full_timestamp() {
        assert_eq!(
            date_prefix("2026-06-24T14:30:05Z").unwrap(),
            "2026-06-24 1430"
        );
        assert_eq!(date_prefix("2026-06-24 09:05").unwrap(), "2026-06-24 0905");
    }

    #[test]
    fn date_prefix_rejects_garbage() {
        assert!(date_prefix("not-a-date").is_err());
        assert!(date_prefix("").is_err());
    }

    #[test]
    fn sanitize_strips_reserved_chars() {
        assert_eq!(
            sanitize_title("Q3 Planning / Roadmap"),
            "Q3 Planning Roadmap"
        );
        assert_eq!(sanitize_title("a:b|c?d*e"), "a b c d e");
        assert_eq!(sanitize_title("  weird   spaces  "), "weird spaces");
        assert_eq!(sanitize_title(""), "Untitled");
        assert_eq!(sanitize_title("###"), "Untitled");
    }

    #[test]
    fn write_note_creates_expected_filename() {
        let dir = tmp_dir("fname");
        let path = write_note(&dir, None, "Team Sync", "2026-06-24T14:30:00Z", "# body").unwrap();
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            "2026-06-24 1430 - Team Sync.md"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# body");
    }

    #[test]
    fn write_note_into_subfolder() {
        let dir = tmp_dir("sub");
        let path = write_note(&dir, Some("Meetings"), "Standup", "2026-06-24", "content").unwrap();
        assert!(path.starts_with(dir.join("Meetings")));
        assert!(path.exists());
    }

    #[test]
    fn write_note_idempotent_same_content() {
        let dir = tmp_dir("idem");
        let p1 = write_note(&dir, None, "Sync", "2026-06-24", "same").unwrap();
        let p2 = write_note(&dir, None, "Sync", "2026-06-24", "same").unwrap();
        assert_eq!(p1, p2, "identical re-export must not create a duplicate");
        // Only one .md file should exist.
        let count = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .and_then(OsStr::to_str)
                    == Some("md")
            })
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn write_note_collision_different_content_suffixes() {
        let dir = tmp_dir("collide");
        let p1 = write_note(&dir, None, "Sync", "2026-06-24", "first").unwrap();
        let p2 = write_note(&dir, None, "Sync", "2026-06-24", "second").unwrap();
        assert_ne!(p1, p2);
        assert_eq!(
            p2.file_name().unwrap().to_str().unwrap(),
            "2026-06-24 0000 - Sync (1).md"
        );
        // A third identical-to-second export reuses the (1) file.
        let p3 = write_note(&dir, None, "Sync", "2026-06-24", "second").unwrap();
        assert_eq!(p2, p3);
    }

    #[test]
    fn no_temp_files_left_behind() {
        let dir = tmp_dir("clean");
        write_note(&dir, None, "Sync", "2026-06-24", "body").unwrap();
        let has_tmp = std::fs::read_dir(&dir).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_str()
                .map(|n| n.ends_with(".tmp"))
                .unwrap_or(false)
        });
        assert!(!has_tmp, "temp dotfile must be renamed away");
    }

    /// R2 helper: does the dir still contain any `.tmp` dotfile?
    fn dir_has_tmp(dir: &Path) -> bool {
        std::fs::read_dir(dir)
            .unwrap()
            .any(|e| e.unwrap().file_name().to_str().is_some_and(|n| n.ends_with(".tmp")))
    }

    /// R2 `export_tmp_pid` shape parser: matches BOTH `write_note` and `overwrite_note` temp shapes,
    /// rejects non-ours names, and tolerates a stem that itself contains dots.
    #[test]
    fn export_tmp_pid_recognizes_our_shapes_only() {
        // Our MARKED shapes parse:
        assert_eq!(export_tmp_pid(".2026-06-24 1430 - Sync.99999.murmur.tmp"), Some(99999));
        assert_eq!(export_tmp_pid(".edit.12345.murmur.tmp"), Some(12345));
        assert_eq!(export_tmp_pid(".a.b.c.777.murmur.tmp"), Some(777)); // stem with dots
        // Not our shape — REJECTED (the `.murmur.tmp` marker makes the sweep Murmur-only):
        assert_eq!(export_tmp_pid(".2026-06-24 1430 - Sync.99999.tmp"), None); // no marker (old/foreign)
        assert_eq!(export_tmp_pid(".foo.12345.tmp"), None); // a FOREIGN third-party temp — must NOT match
        assert_eq!(export_tmp_pid("Sync.md"), None); // a real note
        assert_eq!(export_tmp_pid(".hidden"), None); // dotfile, not a temp
        assert_eq!(export_tmp_pid(".notapid.murmur.tmp"), None); // trailing seg not numeric
        assert_eq!(export_tmp_pid(".murmur.tmp"), None); // no pid / no stem
        assert_eq!(export_tmp_pid("noleadingdot.123.murmur.tmp"), None); // not a dotfile
    }

    /// R2 (RED-before-GREEN): a SIGKILL between fsync and the atomic rename orphans a `.tmp` dotfile
    /// in the vault; `collect_md_stems` skips it but nothing deletes it. The startup sweep must
    /// reclaim it — while leaving real `.md` files and never descending into `.obsidian`/`.trash`.
    /// Mirrors the `no_temp_files_left_behind` / `has_tmp` assertion shape.
    #[test]
    fn sweep_removes_orphaned_export_tmp_keeps_md_and_skips_dotfolders() {
        let dir = tmp_dir("sweep-tmp");

        // A real exported note — must survive.
        std::fs::write(dir.join("2026-06-24 1430 - Sync.md"), "# body").unwrap();

        // Orphaned export temp dotfiles with a DEAD pid (99999 is not a live process) — must go.
        let orphan_write = dir.join(".2026-06-24 1430 - Sync.99999.murmur.tmp");
        std::fs::write(&orphan_write, "half-written").unwrap();
        let orphan_edit = dir.join(".edit.99999.murmur.tmp");
        std::fs::write(&orphan_edit, "half-edited").unwrap();

        // A FOREIGN third-party dotfile of a similar shape but WITHOUT our `.murmur.tmp` marker,
        // dead pid — the sweep must NEVER touch it (it is not ours).
        let foreign = dir.join(".foo.99999.tmp");
        std::fs::write(&foreign, "not ours").unwrap();

        // A `.murmur.tmp` inside `.obsidian` — the sweep must NOT descend there (not ours).
        std::fs::create_dir_all(dir.join(".obsidian")).unwrap();
        let obsidian_tmp = dir.join(".obsidian").join(".foo.99999.murmur.tmp");
        std::fs::write(&obsidian_tmp, "config-temp").unwrap();

        // A temp whose pid is THIS live process + fresh mtime — must be spared (a live export).
        let live_tmp = dir.join(format!(".edit.{}.murmur.tmp", std::process::id()));
        std::fs::write(&live_tmp, "in-progress").unwrap();

        sweep_stale_export_tmp(&dir);

        assert!(!orphan_write.exists(), "dead-pid write_note temp orphan must be swept");
        assert!(!orphan_edit.exists(), "dead-pid overwrite_note temp orphan must be swept");
        assert!(
            dir.join("2026-06-24 1430 - Sync.md").exists(),
            "a real exported .md must never be touched"
        );
        assert!(
            obsidian_tmp.exists(),
            "the sweep must not descend into .obsidian"
        );
        assert!(
            live_tmp.exists(),
            "a live process's fresh export temp must not be raced/removed"
        );
        assert!(
            foreign.exists(),
            "a foreign dotfile WITHOUT our .murmur.tmp marker must never be swept"
        );
        // The live temp (and the untouched foreign file) remain at the top level; orphans are gone.
        assert!(dir_has_tmp(&dir), "the live temp is intentionally left");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R2 mtime fallback: an OLD `.tmp` whose embedded PID happens to be alive again (pid recycled)
    /// is still reclaimed via the > 1 h staleness check, so a recycled pid can't strand residue
    /// forever. Uses the injectable `now` on the private worker (age = 2 h).
    #[test]
    fn sweep_removes_stale_tmp_even_if_pid_recycled_live() {
        let dir = tmp_dir("sweep-stale");
        // A temp whose pid is THIS (live) process — but we age `now` forward 2 h so it counts stale.
        let recycled = dir.join(format!(".edit.{}.murmur.tmp", std::process::id()));
        std::fs::write(&recycled, "orphaned-but-pid-recycled").unwrap();

        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(2 * 3600);
        let removed = sweep_export_tmp_dir(&dir, future);

        assert_eq!(removed, 1, "a stale (> 1 h) temp is reclaimed even with a live pid");
        assert!(!recycled.exists(), "the stale recycled-pid temp is removed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_titles_skips_dotfolders_and_recurses() {
        let dir = tmp_dir("titles");
        std::fs::write(dir.join("Alpha.md"), "a").unwrap();
        std::fs::write(dir.join("notanote.txt"), "x").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("Beta.md"), "b").unwrap();
        std::fs::create_dir_all(dir.join(".obsidian")).unwrap();
        std::fs::write(dir.join(".obsidian").join("Config.md"), "c").unwrap();

        let titles = list_vault_titles(&dir).unwrap();
        assert_eq!(titles, vec!["Alpha".to_string(), "Beta".to_string()]);
    }

    #[test]
    fn list_titles_missing_vault_is_empty() {
        let missing = std::env::temp_dir().join("meetnotes-does-not-exist-xyz");
        assert!(list_vault_titles(&missing).unwrap().is_empty());
    }

    #[test]
    fn detect_vaults_missing_config_is_empty() {
        let missing = std::env::temp_dir().join("meetnotes-no-obsidian-json-xyz.json");
        assert!(detect_vaults_from(&missing).unwrap().is_empty());
    }

    #[test]
    fn detect_vaults_parses_and_filters_to_existing_dirs() {
        let root = tmp_dir("detect");
        let vault_a = root.join("Personal");
        let vault_b = root.join("Work");
        std::fs::create_dir_all(&vault_a).unwrap();
        std::fs::create_dir_all(&vault_b).unwrap();

        let config = serde_json::json!({
            "vaults": {
                "id1": { "path": vault_a.to_str().unwrap(), "ts": 1, "open": false },
                "id2": { "path": vault_b.to_str().unwrap(), "ts": 2, "open": true },
                "id3": { "path": root.join("Deleted").to_str().unwrap(), "ts": 3 }
            }
        });
        let config_path = root.join("obsidian.json");
        std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

        let vaults = detect_vaults_from(&config_path).unwrap();
        // "Deleted" dir doesn't exist → filtered out.
        assert_eq!(vaults.len(), 2);
        // Open vault first.
        assert_eq!(vaults[0].name, "Work");
        assert!(vaults[0].is_open);
        assert_eq!(vaults[1].name, "Personal");
    }

    // ── Phase 5: inject_provenance_frontmatter ──────────────────────────────

    /// A well-formed note with no provenance keys yet receives both keys injected.
    #[test]
    fn inject_provenance_adds_keys_to_clean_frontmatter() {
        let md = "---\ntitle: Sprint Planning\ndate: 2026-06-30\n---\n# Sprint Planning\n\nBody.\n";
        let out =
            inject_provenance_frontmatter(md, "gateway", Some("gpt-4o"), Some("gpt-4o-2024-11-20"));
        assert!(
            out.contains("ai-provider: gateway"),
            "provider injected: {out}"
        );
        // model_served takes precedence over model_requested.
        assert!(
            out.contains("ai-model: gpt-4o-2024-11-20"),
            "served model injected: {out}"
        );
        // Original keys preserved.
        assert!(
            out.contains("title: Sprint Planning"),
            "original key preserved: {out}"
        );
        // Still a valid YAML fence.
        assert!(out.starts_with("---\n"), "fence preserved");
        assert!(out.contains("\n---\n"), "closing fence preserved");
    }

    /// `model_served` is preferred; when absent, `model_requested` is used.
    #[test]
    fn inject_provenance_falls_back_to_model_requested_when_served_absent() {
        let md = "---\ntitle: T\n---\nBody.";
        let out = inject_provenance_frontmatter(md, "anthropic", Some("claude-opus-4-8"), None);
        assert!(
            out.contains("ai-model: claude-opus-4-8"),
            "fallback to requested: {out}"
        );
        assert!(out.contains("ai-provider: anthropic"), "provider: {out}");
    }

    /// When both model fields are `None`, only `ai-provider` is injected.
    #[test]
    fn inject_provenance_provider_only_when_no_model() {
        let md = "---\ndate: 2026-06-30\n---\nBody.";
        let out = inject_provenance_frontmatter(md, "claude_code", None, None);
        assert!(
            out.contains("ai-provider: claude_code"),
            "provider injected: {out}"
        );
        assert!(
            !out.contains("ai-model:"),
            "no model key when both absent: {out}"
        );
    }

    /// When provider is empty and both model fields are `None`, the markdown is returned UNCHANGED.
    #[test]
    fn inject_provenance_noop_when_nothing_to_inject() {
        let md = "---\ntitle: T\n---\nBody.";
        let out = inject_provenance_frontmatter(md, "", None, None);
        assert_eq!(out, md, "byte-identical when nothing to inject");
    }

    /// Idempotent: already-present keys are NOT duplicated on a second call.
    #[test]
    fn inject_provenance_is_idempotent() {
        let md = "---\ntitle: T\n---\nBody.";
        let once = inject_provenance_frontmatter(md, "gateway", Some("gpt-4o"), None);
        let twice = inject_provenance_frontmatter(&once, "gateway", Some("gpt-4o"), None);
        assert_eq!(once, twice, "second inject is a no-op");
        // Only one occurrence of each key.
        assert_eq!(
            once.matches("ai-provider:").count(),
            1,
            "no duplicate provider key"
        );
        assert_eq!(
            once.matches("ai-model:").count(),
            1,
            "no duplicate model key"
        );
    }

    /// Notes WITHOUT a `---` frontmatter block are returned UNCHANGED.
    #[test]
    fn inject_provenance_leaves_notes_without_frontmatter_unchanged() {
        let md = "# Just a heading\n\nNo frontmatter.";
        let out = inject_provenance_frontmatter(md, "anthropic", Some("claude-sonnet-4-6"), None);
        assert_eq!(out, md, "no frontmatter → unchanged");
    }

    /// The injected keys appear INSIDE the frontmatter block, not after the closing `---`.
    #[test]
    fn inject_provenance_keys_are_inside_the_frontmatter_block() {
        let md = "---\ntitle: T\ndate: 2026-06-30\n---\n# Body\n";
        let out = inject_provenance_frontmatter(md, "anthropic", None, Some("claude-opus-4-8"));
        // The structure must be: ---\n...<keys>...\n---\n<body>
        let close = out.find("\n---\n").expect("closing fence present");
        let fm_end = close;
        let fm = &out[..fm_end];
        assert!(
            fm.contains("ai-provider: anthropic"),
            "provider key inside fm: {fm}"
        );
        assert!(
            fm.contains("ai-model: claude-opus-4-8"),
            "model key inside fm: {fm}"
        );
        // Body untouched.
        assert!(out.ends_with("# Body\n"), "body unchanged: {out}");
    }

    // ── Tier 4c: inject_privacy_receipt_frontmatter (per-note egress self-report) ────────────

    /// LOCAL summary ⇒ only the honest `privacy-cloud-calls: 0` headline is stamped. Even if a
    /// host / count are (defensively) passed, a local note NEVER stamps a host or pii key.
    #[test]
    fn privacy_receipt_local_stamps_zero_cloud_calls_only() {
        let md = "---\ntitle: T\ndate: 2026-07-03\n---\n# T\n\nBody.\n";
        let out = inject_privacy_receipt_frontmatter(md, true, Some("api.anthropic.com"), Some(9));
        assert!(
            out.contains("privacy-cloud-calls: 0"),
            "local headline present: {out}"
        );
        assert!(
            !out.contains("privacy-egress-host"),
            "no host for a local note: {out}"
        );
        assert!(
            !out.contains("privacy-pii-redacted"),
            "no pii key for a local note: {out}"
        );
    }

    /// CLOUD summary ⇒ the non-PII destination host + the real redaction count are stamped, and no
    /// `privacy-cloud-calls` integer is claimed (not per-note attributable — see the fn doc).
    #[test]
    fn privacy_receipt_cloud_stamps_host_and_pii_count() {
        let md = "---\ntitle: T\n---\nBody.";
        let out =
            inject_privacy_receipt_frontmatter(md, false, Some("api.anthropic.com"), Some(14));
        assert!(
            out.contains("privacy-egress-host: api.anthropic.com"),
            "host: {out}"
        );
        assert!(out.contains("privacy-pii-redacted: 14"), "pii count: {out}");
        assert!(
            !out.contains("privacy-cloud-calls"),
            "no cloud-call count is claimed for a cloud note (would under-count): {out}"
        );
    }

    /// CONTENT-FREE & NON-NO-OP: a note whose BODY carries PII must have that PII preserved as
    /// opaque passthrough, and the injector must NEVER copy any body text into a `privacy-*` key.
    /// The ONLY new lines vs the input are `privacy-*` keys.
    #[test]
    fn privacy_receipt_is_content_free_and_non_noop() {
        let md = "---\ntitle: Board Sync\n---\n# Board Sync\n\nContact bob@example.com or call +1 415 555 0199.\n";
        let out = inject_privacy_receipt_frontmatter(md, false, Some("api.anthropic.com"), Some(3));
        // It actually stamped something (not a no-op).
        assert_ne!(out, md, "the receipt was injected");
        assert!(
            out.contains("privacy-egress-host: api.anthropic.com"),
            "host stamped: {out}"
        );
        assert!(
            out.contains("privacy-pii-redacted: 3"),
            "count stamped: {out}"
        );
        // The body PII survives untouched (passthrough) — but NEVER inside an injected key.
        assert!(
            out.contains("bob@example.com"),
            "body PII preserved as passthrough"
        );
        for line in out.lines().filter(|l| l.starts_with("privacy-")) {
            assert!(
                !line.contains("bob@example.com"),
                "no email in a privacy key: {line}"
            );
            assert!(
                !line.contains("555 0199"),
                "no phone in a privacy key: {line}"
            );
            assert!(
                !line.contains("Board Sync"),
                "no title/body text in a privacy key: {line}"
            );
        }
        // The ONLY lines present in the output but not the input are `privacy-*` keys.
        let input_lines: std::collections::HashSet<&str> = md.lines().collect();
        for line in out.lines() {
            if !input_lines.contains(line) {
                assert!(
                    line.starts_with("privacy-"),
                    "the only injected lines are privacy-* keys, got: {line}"
                );
            }
        }
    }

    /// The injected keys appear INSIDE the frontmatter fence (before the closing `---`), body
    /// untouched. Also exercises a host label containing spaces/parens (needs no YAML quoting).
    #[test]
    fn privacy_receipt_keys_are_inside_the_frontmatter_block() {
        let md = "---\ntitle: T\ndate: 2026-07-03\n---\n# Body\n";
        let out = inject_privacy_receipt_frontmatter(
            md,
            false,
            Some("claude_code (Anthropic CLI)"),
            Some(2),
        );
        let close = out.find("\n---\n").expect("closing fence present");
        let fm = &out[..close];
        assert!(
            fm.contains("privacy-egress-host: claude_code (Anthropic CLI)"),
            "host key inside fm: {fm}"
        );
        assert!(
            fm.contains("privacy-pii-redacted: 2"),
            "pii key inside fm: {fm}"
        );
        assert!(out.ends_with("# Body\n"), "body unchanged: {out}");
    }

    /// Idempotent: injecting twice equals injecting once — a re-export never duplicates the keys.
    #[test]
    fn privacy_receipt_is_idempotent() {
        let md = "---\ntitle: T\n---\nBody.";
        let once =
            inject_privacy_receipt_frontmatter(md, false, Some("api.anthropic.com"), Some(7));
        let twice =
            inject_privacy_receipt_frontmatter(&once, false, Some("api.anthropic.com"), Some(7));
        assert_eq!(once, twice, "second inject is a no-op");
        assert_eq!(
            once.matches("privacy-egress-host:").count(),
            1,
            "no duplicate host key"
        );
        assert_eq!(
            once.matches("privacy-pii-redacted:").count(),
            1,
            "no duplicate pii key"
        );
    }

    /// Notes WITHOUT a `---` frontmatter block are returned byte-identical (even with PII in body).
    #[test]
    fn privacy_receipt_leaves_notes_without_frontmatter_unchanged() {
        let md = "# Just a heading\n\nNo frontmatter, mentions bob@example.com.";
        let out = inject_privacy_receipt_frontmatter(md, true, None, None);
        assert_eq!(out, md, "no frontmatter → byte-identical");
    }

    /// A cloud note with an UNKNOWN host and count is a no-op (byte-identical) — never a bogus
    /// empty stamp. Guards the `wanted.is_empty()` early return.
    #[test]
    fn privacy_receipt_cloud_without_facts_is_noop() {
        let md = "---\ntitle: T\n---\nBody.";
        let out = inject_privacy_receipt_frontmatter(md, false, None, None);
        assert_eq!(
            out, md,
            "cloud with no host/count → nothing to honestly stamp"
        );
    }

    // ── Export-collision guard: preserve_external_edit_if_any ───────────────

    /// The hash helper is a stable lowercase-hex SHA-256 of the exact bytes.
    #[test]
    fn note_content_hash_is_lowercase_hex_sha256() {
        // sha256("abc") — a well-known vector.
        assert_eq!(
            note_content_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_ne!(note_content_hash("a"), note_content_hash("b"));
    }

    /// A mismatching hash (external edit) preserves the CURRENT bytes as a timestamped sibling; the
    /// canonical file itself is untouched by the preservation (the caller overwrites it after).
    #[test]
    fn preserve_creates_sibling_on_hash_mismatch() {
        let dir = tmp_dir("preserve-mismatch");
        let path = dir.join("2026-07-16 0900 - Sync.md");
        std::fs::write(&path, "externally edited").unwrap();

        let murmur_last_wrote = note_content_hash("murmur content");
        let sibling = preserve_external_edit_if_any(&path, Some(&murmur_last_wrote))
            .unwrap()
            .expect("external edit must be preserved");

        let name = sibling.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("2026-07-16 0900 - Sync (external edit "),
            "sibling name carries the marker: {name}"
        );
        assert!(name.ends_with(".md"), "sibling is a .md: {name}");
        assert_eq!(
            std::fs::read_to_string(&sibling).unwrap(),
            "externally edited",
            "sibling carries EXACTLY the external bytes"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "externally edited",
            "the canonical file is not touched by the preservation itself"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A matching hash (no external edit) creates NO sibling.
    #[test]
    fn preserve_noop_when_hash_matches() {
        let dir = tmp_dir("preserve-match");
        let path = dir.join("Sync.md");
        std::fs::write(&path, "murmur content").unwrap();
        let h = note_content_hash("murmur content");
        assert_eq!(preserve_external_edit_if_any(&path, Some(&h)).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `None` expected hash (legacy row exported before the guard) is grandfathered: no sibling.
    #[test]
    fn preserve_noop_for_legacy_null_hash() {
        let dir = tmp_dir("preserve-legacy");
        let path = dir.join("Sync.md");
        std::fs::write(&path, "anything at all").unwrap();
        assert_eq!(preserve_external_edit_if_any(&path, None).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A missing file is a no-op (nothing to preserve), never an error.
    #[test]
    fn preserve_noop_when_file_missing() {
        let dir = tmp_dir("preserve-missing");
        let path = dir.join("Gone.md");
        let h = note_content_hash("whatever");
        assert_eq!(preserve_external_edit_if_any(&path, Some(&h)).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two preservations in the same minute (fixed stamp via the injectable core): identical
    /// external bytes REUSE the existing sibling (no duplicate); different external bytes get a
    /// `" (N)"` suffix — the first sibling is never clobbered.
    #[test]
    fn preserve_sibling_name_collision_suffixes_or_dedups() {
        let dir = tmp_dir("preserve-collide");
        let path = dir.join("Sync.md");
        let stale = note_content_hash("what murmur last wrote");
        let stamp = "2026-07-16 0930"; // one fixed minute — the collision case by construction.

        // First preservation.
        std::fs::write(&path, "external v1").unwrap();
        let s1 = preserve_external_edit_with_stamp(&path, Some(&stale), stamp)
            .unwrap()
            .expect("first preservation");
        assert_eq!(
            s1.file_name().unwrap().to_str().unwrap(),
            "Sync (external edit 2026-07-16 0930).md"
        );

        // Same external bytes again (a retry / a second overwrite before any new edit): the
        // byte-identical existing sibling is reused, not duplicated.
        let s1_again = preserve_external_edit_with_stamp(&path, Some(&stale), stamp)
            .unwrap()
            .expect("still an external edit");
        assert_eq!(s1, s1_again, "byte-identical sibling is reused");

        // A DIFFERENT external edit in the same minute must NOT clobber the first sibling — it
        // takes the next " (N)" slot.
        std::fs::write(&path, "external v2").unwrap();
        let s2 = preserve_external_edit_with_stamp(&path, Some(&stale), stamp)
            .unwrap()
            .expect("second preservation");
        assert_eq!(
            s2.file_name().unwrap().to_str().unwrap(),
            "Sync (external edit 2026-07-16 0930) (1).md",
            "second distinct edit is collision-suffixed"
        );
        assert_eq!(std::fs::read_to_string(&s1).unwrap(), "external v1");
        assert_eq!(std::fs::read_to_string(&s2).unwrap(), "external v2");

        // No temp residue from the preservation writes.
        assert!(!dir_has_tmp(&dir), "preservation leaves no temp dotfiles");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Re-Truth: append-only supersession callouts ─────────────────────────

    /// A supersession stamp APPENDS a `[!superseded]` callout under a freshly-created
    /// `## Re-Truth updates` section, referencing the superseding note — and touches NO existing byte
    /// (the input is a strict prefix of the output: append-only).
    #[test]
    fn supersession_callout_appends_under_managed_section() {
        let md = "---\ntitle: Kickoff\n---\n# Kickoff\n\nAtlas is in progress.\n";
        let out = append_supersession_callout(
            md,
            "2026-07-05",
            "status",
            "in-progress",
            "shipped",
            Some("2026-07-04 1000 - Launch review"),
        );
        assert!(out.starts_with(md), "append-only: input is a prefix of output");
        assert!(out.contains("## Re-Truth updates"), "section created: {out}");
        assert!(out.contains("> [!superseded] 2026-07-05"), "callout: {out}");
        assert!(
            out.contains("> **status** — in-progress → shipped · see [[2026-07-04 1000 - Launch review]]"),
            "body + wikilink: {out}"
        );
    }

    /// Applying the SAME supersession twice is a no-op — the callout body (predicate/old/new/link) is
    /// the stable idempotence marker, so a re-apply returns byte-identical markdown (no double-stamp).
    #[test]
    fn supersession_callout_is_idempotent() {
        let md = "---\ntitle: T\n---\n# T\n";
        let once = append_supersession_callout(md, "2026-07-05", "status", "a", "b", Some("Later"));
        let twice =
            append_supersession_callout(&once, "2026-07-05", "status", "a", "b", Some("Later"));
        assert_eq!(once, twice, "second apply is a no-op");
        assert_eq!(
            once.matches("> **status** — a → b").count(),
            1,
            "no duplicate callout body"
        );
    }

    /// A second, DIFFERENT supersession reuses the existing section (created once) and appends below.
    #[test]
    fn supersession_callout_reuses_section_for_second_stamp() {
        let md = "# T\n";
        let one = append_supersession_callout(md, "2026-07-05", "status", "a", "b", None);
        let two = append_supersession_callout(&one, "2026-07-06", "owner", "X", "Y", None);
        assert_eq!(
            two.matches("## Re-Truth updates").count(),
            1,
            "section heading created exactly once"
        );
        assert!(two.contains("> **status** — a → b"), "first stamp kept");
        assert!(two.contains("> **owner** — X → Y"), "second stamp appended");
    }

    /// With no superseding stem, the callout omits the `· see [[…]]` wikilink entirely (never an empty
    /// `[[]]` link) — the leak-safe path when the superseding note is sealed.
    #[test]
    fn supersession_callout_omits_link_when_no_stem() {
        let out = append_supersession_callout("# T\n", "2026-07-05", "status", "a", "b", None);
        assert!(out.contains("> **status** — a → b\n"), "body without link: {out}");
        assert!(!out.contains("see [["), "no dangling wikilink: {out}");
    }

    /// The superseding-note backlink is the mirror callout, referencing the SOURCE note, append-only.
    #[test]
    fn supersedes_backlink_appends_and_references_source() {
        let md = "# Launch review\n";
        let out = append_supersedes_callout(md, "2026-07-05", "status", "in-progress", "shipped", "Kickoff");
        assert!(out.starts_with(md), "append-only");
        assert!(out.contains("> [!supersedes] 2026-07-05"), "callout: {out}");
        assert!(
            out.contains("> **status** — supersedes in-progress → shipped in [[Kickoff]]"),
            "backlink body: {out}"
        );
    }
}
