//! Notion export NORMALIZER — pure `bytes -> pages` transforms with NO `AppState`, NO DB, NO
//! filesystem writes and NO network. Everything here is a deterministic function of an in-memory
//! export, so the whole module is provable by `cargo test --lib` against synthetic fixtures — no
//! real Mac, no signed build, no live Notion workspace.
//!
//! WHAT A NOTION EXPORT LOOKS LIKE (empirical — Notion documents none of this):
//! - Every page is `<Title> <32-hex-id>.md`; a page with children also has a sibling DIRECTORY of
//!   the same name holding them, so the directory tree mirrors the page tree.
//! - Internal links are relative, URL-encoded, and carry the id:
//!   `[label](My%20Other%20Page%20abc…ef.md)` — optionally with a `#anchor`.
//! - Full-page databases export as `.csv` (often BOTH a view-filtered file and an unfiltered
//!   `…_all.csv` twin, which is the SAME data twice).
//! - A large workspace arrives as a ZIP **containing more ZIPs** (`Export-<id>-Part-1.zip`).
//!
//! WHY THE ID IS THE IDENTITY: Notion truncates long filenames mid-word, and duplicate titles are
//! the norm rather than the exception (one real export produced 865 colliding names). The 32-hex id
//! is the only stable key, so it — not the filename — is what we store as provenance and what link
//! rewriting resolves against.
//!
//! SECURITY: archives are read ENTIRELY IN MEMORY and never extracted to disk, so zip-slip is
//! structurally impossible here rather than merely guarded against. Nested archives share ONE
//! decompression budget (a per-level budget is the bug, not the guard) and are bounded by a depth
//! cap.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::Path;

use crate::error::{AppError, Result};

/// Decompressed-bytes ceiling for ONE scan/import of a Notion export, shared across every nesting
/// level. Deliberately the same ceiling the document extractor uses.
const MAX_EXPORT_DECOMPRESSED_BYTES: u64 = crate::extract::MAX_EXTRACT_DECOMPRESSED_BYTES;

/// How deep we follow `Export-…-Part-N.zip` inside a zip. Notion nests exactly one level; 3 leaves
/// slack without letting a crafted archive recurse without bound.
const MAX_ARCHIVE_DEPTH: usize = 3;

/// Hard cap on pages accepted from one export. A workspace larger than this should be imported in
/// parts — Tana caps at 1500 for the same reason. Refusing loudly beats a multi-hour silent run.
pub(crate) const MAX_PAGES_PER_IMPORT: usize = 5_000;

/// Longest title we keep. Notion itself truncates; we re-truncate on a char boundary so a
/// pathological name cannot produce an absurd row or an unwritable filename.
const MAX_TITLE_CHARS: usize = 200;

/// Separator packing a non-page entry's declared size into its recorded path. A NUL can never occur
/// in a real archive entry name, so the pairing is unambiguous.
const SIZE_SEP: char = '\u{0}';

/// One page recovered from an export, ready to become a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotionPage {
    /// The 32-hex Notion page id, when the filename carried one. `None` for a hand-made `.md`.
    pub notion_id: Option<String>,
    /// Display title: the first `# heading` when present, else the id-stripped filename.
    pub title: String,
    /// Ancestor titles, outermost first — the page tree, mirrored as folders on import.
    pub parents: Vec<String>,
    /// The page body, verbatim except for link rewriting applied later.
    pub markdown: String,
}

/// What a scan found, WITHOUT writing anything. This is the dry-run contract the UI renders before
/// the user commits to an import.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct NotionScan {
    pub pages: Vec<NotionPage>,
    /// Non-page files that ship alongside (images, PDFs, …) — counted, never imported in v1.
    pub attachments: usize,
    pub attachment_bytes: u64,
    /// Database exports. `csv_all_duplicates` are the `…_all.csv` twins we deliberately ignore.
    pub databases: usize,
    pub csv_all_duplicates: usize,
    /// Nested `Export-…-Part-N.zip` archives we descended into.
    pub nested_archives: usize,
    /// Titles that appear more than once — the caller disambiguates by folder path.
    pub title_collisions: Vec<String>,
    /// True when [`MAX_PAGES_PER_IMPORT`] cut the scan short.
    pub truncated: bool,
}

/// Strip Notion's trailing 32-hex id from a file stem, returning the clean title and the id.
///
/// Mirrors the regexes Obsidian's importer arrived at, deliberately including its two non-obvious
/// choices: hyphens are removed FIRST (so a dashed UUID form matches too), and the charset is
/// `[a-z0-9]` rather than `[0-9a-f]` — Notion ids are lowercase hex in practice, but matching
/// loosely costs nothing and a stricter pattern silently fails open on an unexpected id.
///
/// The `any(is_ascii_digit)` requirement is the guard against a false positive: a 32-letter English
/// phrase with the spaces removed would otherwise be mistaken for an id.
pub(crate) fn strip_notion_id(stem: &str) -> (String, Option<String>) {
    let dehyphenated: String = stem.chars().filter(|c| *c != '-').collect();
    if dehyphenated.len() >= 32 {
        let split = dehyphenated.len() - 32;
        // Only split on a char boundary — a multi-byte tail is not an id.
        if dehyphenated.is_char_boundary(split) {
            let (head, tail) = dehyphenated.split_at(split);
            let is_id = tail.chars().count() == 32
                && tail
                    .chars()
                    .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
                && tail.chars().any(|c| c.is_ascii_digit());
            if is_id {
                return (head.trim().to_string(), Some(tail.to_string()));
            }
        }
    }
    (stem.trim().to_string(), None)
}

/// Truncate to [`MAX_TITLE_CHARS`] on a char boundary.
fn clamp_title(title: &str) -> String {
    let trimmed = title.trim();
    if trimmed.chars().count() <= MAX_TITLE_CHARS {
        return trimmed.to_string();
    }
    trimmed.chars().take(MAX_TITLE_CHARS).collect()
}

/// Percent-decode a relative link target. Hand-rolled rather than pulling a crate for ~20 lines;
/// an invalid escape is left verbatim (fail-open on the byte, never panic).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The display title for a page: the first ATX `# ` heading when the body has one (Notion always
/// writes it, and it is the FULL title — the filename is the truncated one), else the id-stripped
/// stem.
fn title_from_body_or_stem(markdown: &str, stem_title: &str) -> String {
    for line in markdown.lines().take(10) {
        if let Some(rest) = line.trim().strip_prefix("# ") {
            let heading = rest.trim();
            if !heading.is_empty() {
                return clamp_title(heading);
            }
        }
    }
    let fallback = clamp_title(stem_title);
    if fallback.is_empty() {
        crate::storage::db::UNTITLED_TITLE.to_string()
    } else {
        fallback
    }
}

/// Rewrite Notion's relative internal links to Obsidian `[[wikilinks]]`.
///
/// `[label](Some%20Page%20<id>.md)` becomes `[[Resolved Title|label]]`, and `[[Resolved Title]]`
/// when the label already equals the title (avoiding a noisy `[[X|X]]`). A `#anchor` is preserved.
/// Targets we cannot resolve — a page that was not part of this export — are left EXACTLY as they
/// were: a dangling relative link is honest, whereas a wikilink to a note that does not exist would
/// silently invent a broken edge in the graph.
///
/// Deliberately hand-written rather than regex-driven: the pattern needs balanced-bracket awareness
/// for labels containing `]`, which a regex handles badly.
pub(crate) fn rewrite_notion_links(
    markdown: &str,
    titles_by_id: &HashMap<String, String>,
) -> String {
    let src: Vec<char> = markdown.chars().collect();
    let mut out = String::with_capacity(markdown.len());
    let mut i = 0usize;
    while i < src.len() {
        // Only a `[` that is not already a wikilink (`[[`) can start an inline link.
        if src[i] != '[' || src.get(i + 1) == Some(&'[') {
            out.push(src[i]);
            i += 1;
            continue;
        }
        let is_image = i > 0 && src[i - 1] == '!';
        let Some((label, target, next)) = parse_inline_link(&src, i) else {
            out.push(src[i]);
            i += 1;
            continue;
        };
        // An image keeps its inline form — an attachment is not a note, and v1 imports no files.
        if is_image {
            out.push_str(&render_verbatim(&label, &target));
            i = next;
            continue;
        }
        match resolve_target(&target, titles_by_id) {
            Some((title, anchor)) => {
                let body = format!("{title}{anchor}");
                if label.trim() == title.trim() && anchor.is_empty() {
                    out.push_str(&format!("[[{body}]]"));
                } else {
                    out.push_str(&format!("[[{body}|{label}]]"));
                }
            }
            None => out.push_str(&render_verbatim(&label, &target)),
        }
        i = next;
    }
    out
}

fn render_verbatim(label: &str, target: &str) -> String {
    format!("[{label}]({target})")
}

/// Parse `[label](target)` starting at `open` (the index of `[`). Returns the label, the raw target
/// and the index just past the closing `)`. Bails on anything that is not a well-formed one-line
/// inline link.
fn parse_inline_link(src: &[char], open: usize) -> Option<(String, String, usize)> {
    let mut depth = 0usize;
    let mut close_bracket = None;
    for (offset, c) in src.iter().enumerate().skip(open) {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close_bracket = Some(offset);
                    break;
                }
            }
            '\n' => return None,
            _ => {}
        }
    }
    let close_bracket = close_bracket?;
    if src.get(close_bracket + 1) != Some(&'(') {
        return None;
    }
    let mut close_paren = None;
    for (offset, c) in src.iter().enumerate().skip(close_bracket + 2) {
        match c {
            ')' => {
                close_paren = Some(offset);
                break;
            }
            '\n' => return None,
            _ => {}
        }
    }
    let close_paren = close_paren?;
    let label: String = src[open + 1..close_bracket].iter().collect();
    let target: String = src[close_bracket + 2..close_paren].iter().collect();
    Some((label, target, close_paren + 1))
}

/// Resolve a relative `.md` target to a known page title, preserving any `#anchor`.
fn resolve_target(
    target: &str,
    titles_by_id: &HashMap<String, String>,
) -> Option<(String, String)> {
    let lower = target.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
    {
        return None;
    }
    let (path, anchor) = match target.split_once('#') {
        Some((p, a)) => (p, format!("#{a}")),
        None => (target, String::new()),
    };
    if !path.to_ascii_lowercase().ends_with(".md") {
        return None;
    }
    let decoded = percent_decode(path);
    let stem = Path::new(&decoded).file_stem()?.to_str()?;
    let (_, id) = strip_notion_id(stem);
    titles_by_id.get(&id?).map(|t| (t.clone(), anchor))
}

/// One raw entry from an export container, already inflated into memory. A NON-page entry carries
/// no bytes: its path is recorded as `path\0<declared-size>` so the plan can weigh it without ever
/// inflating it — refusing to inflate is what keeps a bomb cheap.
struct RawEntry {
    path: String,
    bytes: Vec<u8>,
}

impl RawEntry {
    /// A page: real bytes, plain path.
    fn page(path: String, bytes: Vec<u8>) -> Self {
        RawEntry { path, bytes }
    }
    /// A non-page: size only.
    fn sized(path: &str, size: u64) -> Self {
        RawEntry {
            path: format!("{path}{SIZE_SEP}{size}"),
            bytes: Vec::new(),
        }
    }
}

/// A running decompressed-bytes budget shared across EVERY nesting level of an export.
struct Budget {
    remaining: u64,
}

impl Budget {
    fn new() -> Self {
        Budget {
            remaining: MAX_EXPORT_DECOMPRESSED_BYTES,
        }
    }
    /// Charge `n` bytes, failing closed when the shared ceiling is exhausted.
    fn charge(&mut self, n: u64) -> Result<()> {
        if n > self.remaining {
            return Err(AppError::InvalidArg(crate::errcode::tag(
                crate::errcode::DOC_TOO_LARGE,
                "export too large / possible zip bomb",
            )));
        }
        self.remaining -= n;
        Ok(())
    }
}

/// Scan a Notion export — either an unpacked DIRECTORY or a `.zip` — into a dry-run plan.
/// Writes nothing, touches no state.
pub(crate) fn scan_export(path: &Path) -> Result<NotionScan> {
    let mut budget = Budget::new();
    let entries = if path.is_dir() {
        read_directory(path, &mut budget)?
    } else {
        let bytes = std::fs::read(path)
            .map_err(|e| AppError::InvalidArg(format!("could not read export: {e}")))?;
        read_archive(bytes, 0, &mut budget)?
    };
    Ok(build_scan(entries))
}

/// Recursively read an unpacked export directory into memory.
fn read_directory(root: &Path, budget: &mut Budget) -> Result<Vec<RawEntry>> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let read = std::fs::read_dir(&dir)
            .map_err(|e| AppError::InvalidArg(format!("could not read export folder: {e}")))?;
        for entry in read {
            let entry =
                entry.map_err(|e| AppError::InvalidArg(format!("could not read entry: {e}")))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            // Dotfiles are noise (`.DS_Store` and friends), never Notion content.
            if name.starts_with('.') {
                continue;
            }
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let file_type = entry
                .file_type()
                .map_err(|e| AppError::InvalidArg(format!("could not stat entry: {e}")))?;
            // Never follow a symlink out of the export tree.
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push((entry.path(), rel));
                continue;
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if is_markdown(&rel) {
                budget.charge(size)?;
                let bytes = std::fs::read(entry.path())
                    .map_err(|e| AppError::InvalidArg(format!("could not read page: {e}")))?;
                out.push(RawEntry::page(rel, bytes));
            } else {
                out.push(RawEntry::sized(&rel, size));
            }
        }
    }
    Ok(out)
}

/// Read a zip archive from memory, descending into nested `Export-…-Part-N.zip` entries under the
/// SHARED budget. Nothing is ever written to disk, so a `../` entry name cannot escape anywhere —
/// and `enclosed_name` drops such an entry regardless.
fn read_archive(bytes: Vec<u8>, depth: usize, budget: &mut Budget) -> Result<Vec<RawEntry>> {
    if depth > MAX_ARCHIVE_DEPTH {
        return Err(AppError::InvalidArg(
            "export archive nests too deeply".into(),
        ));
    }
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| AppError::InvalidArg(format!("not a valid export archive: {e}")))?;
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let file = zip
            .by_index(i)
            .map_err(|e| AppError::InvalidArg(format!("corrupt archive entry: {e}")))?;
        if file.is_dir() {
            continue;
        }
        // `enclosed_name` is the sanitized form; an entry that refuses to produce one is hostile
        // (absolute path or `..` traversal) and is dropped rather than trusted.
        let Some(name) = file.enclosed_name() else {
            continue;
        };
        let rel = name.to_string_lossy().replace('\\', "/");
        if rel.rsplit('/').next().is_some_and(|n| n.starts_with('.')) {
            continue;
        }
        let nested = rel.to_ascii_lowercase().ends_with(".zip");
        if !nested && !is_markdown(&rel) {
            out.push(RawEntry::sized(&rel, file.size()));
            continue;
        }
        // Bounded read: `take(remaining + 1)` so a lying header cannot force a huge allocation.
        let limit = budget.remaining.saturating_add(1);
        let mut buf = Vec::new();
        file.take(limit)
            .read_to_end(&mut buf)
            .map_err(|e| AppError::InvalidArg(format!("could not decode archive entry: {e}")))?;
        budget.charge(buf.len() as u64)?;
        if nested {
            out.push(RawEntry::sized(&rel, 0));
            let mut inner = read_archive(buf, depth + 1, budget)?;
            out.append(&mut inner);
        } else {
            out.push(RawEntry::page(rel, buf));
        }
    }
    Ok(out)
}

fn is_markdown(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".md")
}

/// Turn raw entries into the dry-run plan: classify, build pages, detect title collisions.
fn build_scan(entries: Vec<RawEntry>) -> NotionScan {
    let mut scan = NotionScan::default();
    let mut titles: BTreeMap<String, usize> = BTreeMap::new();
    for entry in entries {
        if let Some((path, size)) = entry.path.split_once(SIZE_SEP) {
            let size: u64 = size.parse().unwrap_or(0);
            let lower = path.to_ascii_lowercase();
            if lower.ends_with(".zip") {
                scan.nested_archives += 1;
            } else if lower.ends_with("_all.csv") {
                scan.csv_all_duplicates += 1;
            } else if lower.ends_with(".csv") {
                scan.databases += 1;
            } else {
                scan.attachments += 1;
                scan.attachment_bytes += size;
            }
            continue;
        }
        if scan.pages.len() >= MAX_PAGES_PER_IMPORT {
            scan.truncated = true;
            continue;
        }
        // A page whose bytes are not valid UTF-8 is skipped rather than lossily mangled.
        let Ok(markdown) = String::from_utf8(entry.bytes) else {
            continue;
        };
        let segments: Vec<&str> = entry.path.split('/').collect();
        let Some(file) = segments.last() else {
            continue;
        };
        let stem = file.strip_suffix(".md").unwrap_or(file);
        let (stem_title, notion_id) = strip_notion_id(stem);
        let title = title_from_body_or_stem(&markdown, &stem_title);
        // Ancestor directories become the page path, with their own ids stripped.
        let parents: Vec<String> = segments[..segments.len() - 1]
            .iter()
            .map(|s| strip_notion_id(s).0)
            .filter(|s| !s.is_empty())
            .collect();
        *titles.entry(title.clone()).or_insert(0) += 1;
        scan.pages.push(NotionPage {
            notion_id,
            title,
            parents,
            markdown,
        });
    }
    let collisions: BTreeSet<String> = titles
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(t, _)| t)
        .collect();
    scan.title_collisions = collisions.into_iter().collect();
    scan
}

/// The id → title map used for link rewriting, built from every page in the scan.
pub(crate) fn titles_by_id(pages: &[NotionPage]) -> HashMap<String, String> {
    pages
        .iter()
        .filter_map(|p| p.notion_id.clone().map(|id| (id, p.title.clone())))
        .collect()
}

#[cfg(test)]
mod tests;
