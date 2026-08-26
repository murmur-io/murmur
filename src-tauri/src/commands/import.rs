//! BULK IMPORT commands — turning an external knowledge base into ordinary Murmur notes.
//!
//! ## Why these are notes, not documents
//!
//! `import_document` produces a `kind='document'` row: excellent retrieval material, but it has no
//! title, is never exported to the vault (`get_note_row` filters `kind='note'`), is not editable in
//! the note editor, its front-matter is never typed — and, decisively, `insert_document` writes
//! `text_blob = NULL`, i.e. it does NOT birth-seal. Importing a whole workspace through that seam
//! would multiply every one of those gaps by the page count. So an imported page goes through the
//! SAME funnel an authored note does: `create_note_inner` (which birth-seals into a locked folder)
//! followed by `update_note_doc_inner_with` (gate → seal-on-write → gated vault export → re-index).
//! No new write path, no new read path, and therefore no new seal or visibility surface to audit.
//!
//! ## One orchestration, three sources
//!
//! Everything below is source-agnostic: `crate::import` answers "what pages are in there", and this
//! module answers "how do pages become notes safely". Adding a fourth source is a normalizer plus
//! two match arms, not another importer.
//!
//! ## Zero egress
//!
//! Every source reads something already on this Mac — a downloaded export, a vault folder, or the
//! local Notes app over Apple events. No network, no provider, no consent prompt, no redaction
//! firewall involvement, no egress-ledger row. This is deliberately the opposite of
//! `crate::connectors::notion`, which is a live cloud search.
//!
//! ## Two passes, and why
//!
//! Wikilink indexing resolves `[[Title]]` to a TARGET ID so links survive a rename. During a batch
//! the target of page 1 usually does not exist until page 400, so a single pass would leave most
//! edges unresolved. Pass 1 writes every note; pass 2 re-runs the derived projections once every
//! title exists. Embedding is deliberately deferred in BOTH passes (chunk-only, `None` embedder):
//! keyword retrieval works immediately, and vectors are filled by the existing repair tick or a
//! manual Reindex rather than by pinning the single heavy permit for the length of the import.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::error::AppError;
use crate::import::{self, ImportScan, ImportSource, ImportedPage};
use crate::state::AppState;

/// Cooperative cancel for an in-flight bulk import. A module-level flag rather than `AppState`
/// because exactly one import may run at a time (the heavy permit enforces that anyway) and the
/// flag must be reachable from the blocking closure without borrowing state.
static IMPORT_CANCEL: AtomicBool = AtomicBool::new(false);

/// The dry-run plan — what an import WOULD do. Nothing is written to produce this.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportScanReport {
    /// Pages that would be imported or updated.
    pub pages: usize,
    /// Of those, how many already exist here from a previous run (they update, never duplicate).
    pub already_imported: usize,
    /// Files that are not pages — images, PDFs. Counted and weighed, never imported yet.
    pub attachments: usize,
    pub attachment_bytes: u64,
    /// Database CSV exports (Notion only). Not imported yet.
    pub databases: usize,
    /// `…_all.csv` twins Notion ships alongside each database view — the same data again, skipped.
    pub csv_all_duplicates: usize,
    /// Nested `Export-…-Part-N.zip` archives descended into automatically (Notion only).
    pub nested_archives: usize,
    /// Titles that occur more than once in the source; the import disambiguates them by folder.
    pub title_collisions: Vec<String>,
    /// A handful of titles for the preview, so the user can confirm this is the right source.
    pub sample_titles: Vec<String>,
    /// `true` when the source exceeded the per-import page cap and the plan was cut short.
    pub truncated: bool,
    /// `true` when the chosen Obsidian folder IS the vault Murmur exports to — importing it would
    /// read Murmur's own notes back in as copies of themselves.
    pub is_murmur_vault: bool,
}

/// What an import actually did. Counters plus the titles that failed, so a partial run is legible
/// instead of a silent one.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub imported: usize,
    pub updated: usize,
    pub skipped: usize,
    pub failed: usize,
    /// Up to 20 failure lines, each `title: reason`. Shown in the UI, never written to a log.
    pub failures: Vec<String>,
    pub folders_created: usize,
    /// `true` when the user cancelled: everything already written stays, nothing is rolled back.
    pub cancelled: bool,
    /// `true` when vectors were deferred — keyword search works now, semantic search after a
    /// Reindex. Surfaced so the UI never implies the brain is fully caught up when it is not.
    pub embedding_deferred: bool,
}

/// Resolve the wire source name, failing closed on anything unknown.
fn parse_source(raw: &str) -> Result<ImportSource, AppError> {
    ImportSource::parse(raw).ok_or_else(|| AppError::InvalidArg("unknown import source".to_string()))
}

/// Read the chosen source into a scan. The ONE place that knows which normalizer to call.
fn scan_source(source: ImportSource, path: Option<&str>) -> Result<ImportScan, AppError> {
    match source {
        ImportSource::Notion => {
            let path = require_path(path)?;
            import::notion::scan_export(std::path::Path::new(&path))
        }
        ImportSource::Obsidian => {
            let path = require_path(path)?;
            import::obsidian::scan_vault(std::path::Path::new(&path))
        }
        // Apple Notes has nothing to pick: the library IS the source.
        ImportSource::AppleNotes => import::apple_notes::scan_notes(),
    }
}

fn require_path(path: Option<&str>) -> Result<String, AppError> {
    match path {
        Some(p) if !p.trim().is_empty() => Ok(p.to_string()),
        _ => Err(AppError::InvalidArg(
            "choose what to import from first".into(),
        )),
    }
}

/// DRY-RUN an import: report what it would do, WITHOUT writing anything.
#[tauri::command]
pub async fn scan_import(
    app: AppHandle,
    state: State<'_, AppState>,
    source: String,
    path: Option<String>,
) -> Result<ImportScanReport, AppError> {
    let source = parse_source(&source)?;
    let sem = std::sync::Arc::clone(&state.inner().heavy_inference);
    crate::perf::run_heavy(&sem, move || {
        crate::events::emit_bulk_import(&app, "scanning", 0, 0);
        let scan = scan_source(source, path.as_deref())?;
        let state = app.state::<AppState>();
        let st = state.inner();

        // How many of these pages we already hold from an earlier run. Reported up front so the
        // preview can say "42 update, 8 new" rather than implying 50 duplicates are coming.
        let mut already_imported = 0usize;
        for page in &scan.pages {
            if let Some(id) = page.external_id.as_deref() {
                if st.db.note_by_external_id(source.as_str(), id)?.is_some() {
                    already_imported += 1;
                }
            }
        }

        // Importing Murmur's OWN vault would read our exported notes back in as copies. Detect it
        // here rather than letting the user discover it as a duplicated library.
        let is_murmur_vault = matches!(source, ImportSource::Obsidian)
            && path
                .as_deref()
                .zip(crate::commands::vault_path(st))
                .is_some_and(|(chosen, vault)| same_dir(chosen, &vault));

        let report = ImportScanReport {
            pages: scan.pages.len(),
            already_imported,
            attachments: scan.attachments,
            attachment_bytes: scan.attachment_bytes,
            databases: scan.databases,
            csv_all_duplicates: scan.csv_all_duplicates,
            nested_archives: scan.nested_archives,
            title_collisions: scan.title_collisions,
            sample_titles: scan.pages.iter().take(8).map(|p| p.title.clone()).collect(),
            truncated: scan.truncated,
            is_murmur_vault,
        };
        // Counts only — never a title (titles are user content).
        tracing::info!(
            target: "import",
            source = source.as_str(),
            pages = report.pages,
            already = report.already_imported,
            attachments = report.attachments,
            "import source scanned"
        );
        crate::events::emit_bulk_import(&app, "done", report.pages, report.pages);
        Ok(report)
    })
    .await
}

/// Whether two paths name the same directory, resolving symlinks where possible.
fn same_dir(a: &str, b: &str) -> bool {
    let canon = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| std::path::PathBuf::from(p));
    canon(a) == canon(b)
}

/// IMPORT into `folder_id` (or the always-open Notes root when absent).
///
/// WRITE-GATED per page through the authored-note funnel, so a folder that is sealed and not
/// session-unlocked refuses the write and the run reports how far it got. `mirror_hierarchy`
/// recreates the source's own tree as nested note folders; when false, everything lands flat.
#[tauri::command]
pub async fn run_import(
    app: AppHandle,
    state: State<'_, AppState>,
    source: String,
    path: Option<String>,
    folder_id: Option<String>,
    mirror_hierarchy: bool,
) -> Result<ImportReport, AppError> {
    let source = parse_source(&source)?;
    let sem = std::sync::Arc::clone(&state.inner().heavy_inference);
    IMPORT_CANCEL.store(false, Ordering::SeqCst);
    crate::perf::run_heavy(&sem, move || {
        let state = app.state::<AppState>();
        let st = state.inner();
        run_import_inner(
            Some(&app),
            st,
            source,
            path.as_deref(),
            folder_id.as_deref(),
            mirror_hierarchy,
        )
    })
    .await
}

/// Ask an in-flight import to stop after the page it is on. Already-written notes stay — a bulk
/// import is not a transaction, and pretending otherwise would mean deleting the user's content to
/// honour a cancel.
#[tauri::command]
pub fn cancel_import() -> Result<(), AppError> {
    IMPORT_CANCEL.store(true, Ordering::SeqCst);
    Ok(())
}

/// The synchronous body of [`run_import`], taking `&AppState` so the whole orchestration is
/// unit-testable against a temp SQLCipher database without a Tauri runtime.
pub(crate) fn run_import_inner(
    app: Option<&AppHandle>,
    state: &AppState,
    source: ImportSource,
    path: Option<&str>,
    folder_id: Option<&str>,
    mirror_hierarchy: bool,
) -> Result<ImportReport, AppError> {
    emit(app, "scanning", 0, 0);
    let scan = scan_source(source, path)?;
    let total = scan.pages.len();

    // Notion links are relative paths that only become wikilinks once every page in the export is
    // known, so the map is built from the WHOLE scan before a single note is written. The other
    // sources need no rewriting: a vault already has wikilinks, and Notes has no links at all.
    let titles = import::notion::titles_by_id(&scan.pages);

    // Resolve the target root ONCE and gate it up front, so a sealed destination fails fast instead
    // of after 400 successful writes.
    let root_folder = match folder_id {
        Some(f) if !f.is_empty() => {
            if state.db.note_folder_by_id(f)?.is_none() {
                return Err(AppError::InvalidArg(format!("no note folder {f}")));
            }
            f.to_string()
        }
        _ => state.db.ensure_notes_root()?,
    };

    let mut report = ImportReport {
        imported: 0,
        updated: 0,
        skipped: 0,
        failed: 0,
        failures: Vec::new(),
        folders_created: 0,
        cancelled: false,
        embedding_deferred: true,
    };
    // (note id, title, markdown) for the second pass.
    let mut written: Vec<(String, String, String)> = Vec::with_capacity(total);
    let mut folders = FolderTree::load(state, &root_folder)?;

    for (index, page) in scan.pages.iter().enumerate() {
        if IMPORT_CANCEL.load(Ordering::SeqCst) {
            report.cancelled = true;
            break;
        }
        emit(app, "importing", index, total);

        let markdown = match source {
            ImportSource::Notion => import::notion::rewrite_notion_links(&page.markdown, &titles),
            ImportSource::Obsidian | ImportSource::AppleNotes => page.markdown.clone(),
        };
        let target = if mirror_hierarchy && !page.parents.is_empty() {
            match folders.ensure_path(state, &page.parents, &mut report.folders_created) {
                Ok(id) => id,
                Err(e) => {
                    record_failure(&mut report, &page.title, &e);
                    continue;
                }
            }
        } else {
            root_folder.clone()
        };

        match import_one_page(state, source, page, &markdown, &target) {
            Ok(Outcome::Created(id)) => {
                report.imported += 1;
                written.push((id, page.title.clone(), markdown));
            }
            Ok(Outcome::Updated(id)) => {
                report.updated += 1;
                written.push((id, page.title.clone(), markdown));
            }
            Err(e) => record_failure(&mut report, &page.title, &e),
        }
    }

    // PASS 2 — now that every title exists, re-resolve the derived projections so `[[Title]]`
    // edges point at real ids. Chunk-only (no embedder): vectors are the repair tick's job.
    let linked = written.len();
    for (index, (id, title, markdown)) in written.iter().enumerate() {
        if IMPORT_CANCEL.load(Ordering::SeqCst) {
            report.cancelled = true;
            break;
        }
        emit(app, "linking", index, linked);
        crate::commands::refresh_note_doc_derived_best_effort(state, id, title, markdown, None);
    }

    emit(app, "done", linked, total);
    // Counts only — a title is user content and never reaches a log line.
    tracing::info!(
        target: "import",
        source = source.as_str(),
        imported = report.imported,
        updated = report.updated,
        failed = report.failed,
        folders = report.folders_created,
        cancelled = report.cancelled,
        "import finished"
    );
    Ok(report)
}

/// Emit a progress tick when there is a window to emit into. `None` is the unit-test path, where
/// the whole orchestration runs against a temp database with no Tauri runtime.
fn emit(app: Option<&AppHandle>, stage: &str, done: usize, total: usize) {
    if let Some(app) = app {
        crate::events::emit_bulk_import(app, stage, done, total);
    }
}

/// Whether a page became a new note or refreshed one imported earlier.
enum Outcome {
    Created(String),
    Updated(String),
}

/// Write ONE page through the authored-note funnel.
///
/// Idempotency: a page whose source id we have seen before UPDATES that note wherever it now lives,
/// rather than creating a second copy. The update deliberately re-gates against the note's CURRENT
/// folder (inside `update_note_doc_inner_with`), not the import target — the user may have moved it
/// into a folder that is now locked, and writing there ungated would resurrect plaintext behind a
/// lock.
fn import_one_page(
    state: &AppState,
    source: ImportSource,
    page: &ImportedPage,
    markdown: &str,
    target_folder: &str,
) -> Result<Outcome, AppError> {
    if let Some(external_id) = page.external_id.as_deref() {
        if let Some((existing_id, _folder)) =
            state.db.note_by_external_id(source.as_str(), external_id)?
        {
            crate::commands::update_note_doc_inner_with(
                state,
                &existing_id,
                &page.title,
                markdown,
                None,
            )?;
            return Ok(Outcome::Updated(existing_id));
        }
    }
    // `create_note_inner` owns the write-gate AND the birth-seal: a note created in a
    // session-unlocked LOCKED folder is sealed from birth, never left as a blob-less plaintext row.
    let id = crate::commands::create_note_inner(state, Some(target_folder), &page.title)?;
    // Stamp provenance BEFORE the body write, so a crash between the two still leaves a row the
    // next run recognizes and updates instead of duplicating.
    state
        .db
        .set_document_provenance(&id, source.as_str(), page.external_id.as_deref())?;
    crate::commands::update_note_doc_inner_with(state, &id, &page.title, markdown, None)?;
    Ok(Outcome::Created(id))
}

/// Record a per-page failure for the UI. The title is user content: it goes in the report the user
/// reads, and never into a log line.
fn record_failure(report: &mut ImportReport, title: &str, error: &AppError) {
    report.failed += 1;
    if report.failures.len() < 20 {
        report.failures.push(format!("{title}: {error}"));
    }
    tracing::warn!(target: "import", "page import failed");
}

/// Find-or-create cache for the note-folder tree, so mirroring a deep source does not re-query the
/// folder list per page.
struct FolderTree {
    /// `(parent id, lowercased child name) -> child id`. Lowercased because APFS is
    /// case-insensitive by default: `Team` and `team` are the same directory, and treating them as
    /// two folders would produce a vault path collision.
    children: std::collections::HashMap<(String, String), String>,
    root: String,
}

impl FolderTree {
    fn load(state: &AppState, root: &str) -> Result<Self, AppError> {
        let mut children = std::collections::HashMap::new();
        for folder in state.db.list_note_folders()? {
            if let Some(parent) = folder.parent_id.clone() {
                children.insert((parent, folder.name.to_lowercase()), folder.id.clone());
            }
        }
        Ok(FolderTree {
            children,
            root: root.to_string(),
        })
    }

    /// Resolve `path` (outermost first) beneath the import root, creating the missing levels.
    ///
    /// A level created under a LOCKED parent is born sealed by `create_note_folder_inner` — the
    /// same guard the manual "New folder" takes, reused rather than re-implemented.
    fn ensure_path(
        &mut self,
        state: &AppState,
        path: &[String],
        created: &mut usize,
    ) -> Result<String, AppError> {
        let mut current = self.root.clone();
        for raw in path {
            let name = raw.trim();
            if name.is_empty() {
                continue;
            }
            let key = (current.clone(), name.to_lowercase());
            if let Some(existing) = self.children.get(&key) {
                current = existing.clone();
                continue;
            }
            let folder = crate::commands::create_note_folder_inner(state, name, Some(&current))?;
            *created += 1;
            self.children.insert(key, folder.id.clone());
            current = folder.id;
        }
        Ok(current)
    }
}

// The parent binds this file as `import_commands` via `#[path]` (to avoid colliding with the
// crate-level `crate::import` normalizer), so the child module path is spelled out explicitly
// rather than inferred from the module NAME, which would look under `commands/import_commands/`.
#[cfg(test)]
#[path = "import/tests.rs"]
mod tests;
