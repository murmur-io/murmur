//! Obsidian vault NORMALIZER — pure `folder → pages`, no DB, no network, no writes.
//!
//! An Obsidian vault is already the format Murmur exports TO: plain `.md` with YAML front-matter and
//! `[[wikilinks]]`. So this importer is deliberately the least clever of the three — it walks the
//! folder, reads each file **verbatim**, and touches nothing. Rewriting links here would be actively
//! wrong: they are already in the target form, and our own link indexer resolves them by title.
//!
//! IDENTITY is the vault-relative PATH. A vault has no per-note id, and the path is the only key
//! that is stable across re-imports, which is what makes a second run update rather than duplicate.
//! Its weakness is honest and worth stating: move a note inside the vault and the next import treats
//! it as new. Notion's 32-hex id has no such problem, which is exactly why that source uses it.
//!
//! WHAT IS SKIPPED: `.obsidian/` (workspace config, plugins, hotkeys — never content), `.trash/`
//! (deleted notes; importing them would resurrect what the user threw away), and every dotfile.

use std::path::Path;

use super::{title_from_body_or_stem, ImportScan, ImportedPage, MAX_PAGES_PER_IMPORT};
use crate::error::{AppError, Result};

/// Per-file ceiling. A vault note is prose; anything past this is not a note we can usefully embed,
/// and reading it whole would be the memory risk.
const MAX_NOTE_BYTES: u64 = 8 * 1024 * 1024;

/// Directory names that are vault plumbing rather than content.
const SKIPPED_DIRS: &[&str] = &[".obsidian", ".trash", ".git"];

/// Scan an Obsidian vault folder into a dry-run plan. Writes nothing.
pub(crate) fn scan_vault(root: &Path) -> Result<ImportScan> {
    if !root.is_dir() {
        return Err(AppError::InvalidArg(
            "pick the vault FOLDER — an Obsidian vault is a directory of .md files".into(),
        ));
    }
    let mut scan = ImportScan::default();
    let mut stack = vec![(root.to_path_buf(), Vec::<String>::new())];
    while let Some((dir, parents)) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| AppError::InvalidArg(format!("could not read vault folder: {e}")))?;
        for entry in entries {
            let entry =
                entry.map_err(|e| AppError::InvalidArg(format!("could not read entry: {e}")))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || SKIPPED_DIRS.contains(&name.as_str()) {
                continue;
            }
            let file_type = entry
                .file_type()
                .map_err(|e| AppError::InvalidArg(format!("could not stat entry: {e}")))?;
            // Never follow a symlink: a vault can legitimately contain one pointing anywhere, and
            // following it would import files from outside the folder the user chose.
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                let mut child = parents.clone();
                child.push(name);
                stack.push((entry.path(), child));
                continue;
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if !name.to_ascii_lowercase().ends_with(".md") {
                // Attachments in a vault are real files the user owns; count them so the plan is
                // honest that this import brings the prose and leaves the binaries where they are.
                scan.attachments += 1;
                scan.attachment_bytes += size;
                continue;
            }
            if scan.pages.len() >= MAX_PAGES_PER_IMPORT {
                scan.truncated = true;
                continue;
            }
            if size > MAX_NOTE_BYTES {
                continue;
            }
            let Ok(markdown) = std::fs::read_to_string(entry.path()) else {
                // Not valid UTF-8 — skip rather than mangle. A `.md` that is not text is not a note.
                continue;
            };
            let stem = name.strip_suffix(".md").unwrap_or(&name).to_string();
            let external_id = relative_id(&parents, &stem);
            scan.pages.push(ImportedPage {
                external_id: Some(external_id),
                title: title_from_body_or_stem(&markdown, &stem),
                parents: parents.clone(),
                markdown,
            });
        }
    }
    scan.finish();
    Ok(scan)
}

/// The vault-relative path used as the stable identity, e.g. `Projects/Q4/Plan`.
fn relative_id(parents: &[String], stem: &str) -> String {
    if parents.is_empty() {
        return stem.to_string();
    }
    format!("{}/{}", parents.join("/"), stem)
}

#[cfg(test)]
#[path = "obsidian/tests.rs"]
mod tests;
