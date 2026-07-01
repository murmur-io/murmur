use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

// ── Filename derivation ─────────────────────────────────────────────────────

/// Characters that are illegal in filenames on macOS/Obsidian or that Obsidian
/// reserves for wiki-link / tag syntax. We strip/replace them so the produced
/// filename is always safe and round-trips as a clean note title.
fn sanitize_title(title: &str) -> String {
    // Replace path separators and reserved characters with a space, collapse
    // runs of whitespace, then trim. Obsidian forbids: * " \ / < > : | ? and #
    // and ^ [ ] are link/anchor syntax that break titles.
    let replaced: String = title
        .chars()
        .map(|c| match c {
            '*' | '"' | '\\' | '/' | '<' | '>' | ':' | '|' | '?' | '#' | '^' | '['
            | ']' => ' ',
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
        || !date_bits.iter().all(|b| b.chars().all(|c| c.is_ascii_digit()))
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
    let final_path = resolve_unique_path(&target_dir, &stem, markdown)?;

    // If resolve returned an existing identical file, we're done (idempotent).
    if final_path_is_existing_identical(&final_path, markdown)? {
        return Ok(final_path);
    }

    // Atomic write: write to a hidden temp dotfile in the SAME directory (so the
    // rename is a same-filesystem atomic operation), fsync, then rename over the
    // final path. The temp name is unique to avoid clobbering a concurrent write.
    let tmp_name = format!(
        ".{}.{}.tmp",
        sanitize_for_tmp(&stem),
        std::process::id()
    );
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
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes == markdown.as_bytes()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(AppError::Export(format!("read existing note failed: {e}"))),
    }
}

/// Pick the destination path: `<dir>/<stem>.md`, or `<stem> (N).md` on collision.
/// If `<stem>.md` already exists with identical content, that path is returned
/// (idempotent re-export). If it exists with DIFFERENT content, we look for an
/// identical sibling `<stem> (N).md`; if found, return it; else allocate the next
/// free `(N)` slot.
fn resolve_unique_path(dir: &Path, stem: &str, markdown: &str) -> Result<PathBuf> {
    let base = dir.join(format!("{stem}.md"));
    if !path_exists(&base)? {
        return Ok(base);
    }
    if final_path_is_existing_identical(&base, markdown)? {
        return Ok(base);
    }

    // Base is taken by different content; scan/allocate a "(N)" variant.
    for n in 1..=10_000 {
        let candidate = dir.join(format!("{stem} ({n}).md"));
        if !path_exists(&candidate)? {
            return Ok(candidate);
        }
        if final_path_is_existing_identical(&candidate, markdown)? {
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
        .map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c })
        .collect()
}

/// Write `contents` to `path` and fsync both the file and its parent directory so
/// the subsequent rename is durable.
fn write_and_sync(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| AppError::Export(format!("open temp file failed: {e}")))?;

    file.write_all(contents.as_bytes())
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
        let entry =
            entry.map_err(|e| AppError::Export(format!("dir entry failed: {e}")))?;
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
        } else if file_type.is_file() && path.extension().and_then(OsStr::to_str) == Some("md")
        {
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
    let tmp_path = parent.join(format!(".edit.{}.tmp", std::process::id()));
    write_and_sync(&tmp_path, markdown).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp_path);
    })?;
    std::fs::rename(&tmp_path, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        AppError::Export(format!("atomic rename failed: {e}"))
    })?;
    Ok(())
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
        Err(e) => {
            return Err(AppError::Export(format!(
                "read obsidian.json failed: {e}"
            )))
        }
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
    vaults.sort_by(|a, b| {
        b.is_open
            .cmp(&a.is_open)
            .then_with(|| a.name.cmp(&b.name))
    });
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
        assert_eq!(sanitize_title("Q3 Planning / Roadmap"), "Q3 Planning Roadmap");
        assert_eq!(sanitize_title("a:b|c?d*e"), "a b c d e");
        assert_eq!(sanitize_title("  weird   spaces  "), "weird spaces");
        assert_eq!(sanitize_title(""), "Untitled");
        assert_eq!(sanitize_title("###"), "Untitled");
    }

    #[test]
    fn write_note_creates_expected_filename() {
        let dir = tmp_dir("fname");
        let path = write_note(&dir, None, "Team Sync", "2026-06-24T14:30:00Z", "# body")
            .unwrap();
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            "2026-06-24 1430 - Team Sync.md"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# body");
    }

    #[test]
    fn write_note_into_subfolder() {
        let dir = tmp_dir("sub");
        let path = write_note(
            &dir,
            Some("Meetings"),
            "Standup",
            "2026-06-24",
            "content",
        )
        .unwrap();
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
        let out = inject_provenance_frontmatter(md, "gateway", Some("gpt-4o"), Some("gpt-4o-2024-11-20"));
        assert!(out.contains("ai-provider: gateway"), "provider injected: {out}");
        // model_served takes precedence over model_requested.
        assert!(out.contains("ai-model: gpt-4o-2024-11-20"), "served model injected: {out}");
        // Original keys preserved.
        assert!(out.contains("title: Sprint Planning"), "original key preserved: {out}");
        // Still a valid YAML fence.
        assert!(out.starts_with("---\n"), "fence preserved");
        assert!(out.contains("\n---\n"), "closing fence preserved");
    }

    /// `model_served` is preferred; when absent, `model_requested` is used.
    #[test]
    fn inject_provenance_falls_back_to_model_requested_when_served_absent() {
        let md = "---\ntitle: T\n---\nBody.";
        let out = inject_provenance_frontmatter(md, "anthropic", Some("claude-opus-4-8"), None);
        assert!(out.contains("ai-model: claude-opus-4-8"), "fallback to requested: {out}");
        assert!(out.contains("ai-provider: anthropic"), "provider: {out}");
    }

    /// When both model fields are `None`, only `ai-provider` is injected.
    #[test]
    fn inject_provenance_provider_only_when_no_model() {
        let md = "---\ndate: 2026-06-30\n---\nBody.";
        let out = inject_provenance_frontmatter(md, "claude_code", None, None);
        assert!(out.contains("ai-provider: claude_code"), "provider injected: {out}");
        assert!(!out.contains("ai-model:"), "no model key when both absent: {out}");
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
        assert_eq!(once.matches("ai-provider:").count(), 1, "no duplicate provider key");
        assert_eq!(once.matches("ai-model:").count(), 1, "no duplicate model key");
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
        assert!(fm.contains("ai-provider: anthropic"), "provider key inside fm: {fm}");
        assert!(fm.contains("ai-model: claude-opus-4-8"), "model key inside fm: {fm}");
        // Body untouched.
        assert!(out.ends_with("# Body\n"), "body unchanged: {out}");
    }
}
