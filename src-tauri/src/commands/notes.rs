//! Standalone-NOTES command surface — the authored-note CRUD + the Notes folder tree, extracted
//! VERBATIM from `commands` (God-file split, a PURE MOVE — every read-gate / write-gate / seal-on-write
//! / mask / vault-assert body is UNCHANGED, only relocated). This is the standalone-note domain:
//! create/read/list/update(+fast-autosave)/move/delete/export of authored notes (`documents` rows with
//! `kind='note'`), the on-device auto-title, and the note-folder create/rename/delete/move + typed
//! property-schema cluster. Companion notes, the note-assistant (WP4), documents, and share/org paths
//! STAY in `commands/mod.rs` — they are cross-domain and reach the moved `*_inner` cores through the
//! `pub use notes_commands::*;` re-export.
//!
//! LOCK-MODEL (byte-identical to the pre-move form): every read GATES on the note's FOLDER via
//! `super::folder_is_unlocked` (`get_note` returns `super::masked_note_doc` — title `🔒 Locked`, no
//! body/tags — for a sealed-not-unlocked note; `list_notes`/`list_notes_typed` filter through the gated
//! `Db::list_notes_visible*` so a sealed row is ABSENT, its title never leaking). Every WRITE
//! (`create_note`/`update_note_doc`/`save_note_text`/`move_note_doc`/`set_note_folder_schema`) refuses a
//! sealed-and-not-session-unlocked folder (`AppError::Locked`) BEFORE touching plaintext, and the
//! seal-on-write into a session-unlocked LOCKED folder goes through `super::sealed_document_blob` /
//! `super::reseal_document_if_locked` (encrypt+VERIFY, fail-closed on a missing session KEK) so a note
//! is sealed-from-birth and never resurrected behind a lock. `move_note_doc_inner` keeps its
//! SEAL-THEN-REASSIGN + DELETE-THEN-MOVE ordering verbatim (verify-before-destroy: the target-CK blob is
//! computed + verified BEFORE the old `.md` is removed and BEFORE the atomic reassign UPDATE — a
//! fail-closed seal mutates nothing). Note export goes through `super::export_note_to_vault` (gate FIRST)
//! and `super::assert_in_vault` (the in-vault D5 assertion) — both STAY in mod.rs and are reachable here
//! because this is a descendant module of `commands`.
//!
//! The shared DTO/mask/vault-write helpers (`note_display_title`, `note_doc_from_row`, `masked_note_doc`,
//! `export_note_to_vault`, `write_note_to_vault`, `index_note_body_chunks`) and every gate/seal helper
//! (`folder_is_unlocked`, `unlocked_snapshot`, `lifecycle_guard`, `sealed_document_blob`,
//! `reseal_document_if_locked`, `vault_path`, `assert_in_vault`, `index_wikilinks_best_effort`,
//! `auto_link_semantic_best_effort`, `emit_audit_updated_after_purge`, `revoke_org_shares_for_source`,
//! `republish_org_shares_for_source`, `emit_content_deleted`) STAY in `commands/mod.rs` — this module
//! reads them through `use super::*` (a `commands` submodule sees its parent's private items). The moved
//! `*_inner` cores stay `pub(crate)` (unchanged) so the STAYING companion / organize / link / share
//! clusters and the STAYING test modules keep calling them via the re-export. Every symbol keeps its
//! EXACT prior body + signature; nothing here changed except its file. No gate/mask/seal/export-gate
//! LOGIC changed — only relocation.

use super::*;

/// Create an EMPTY authored note. `folder_id = None` ⇒ the default note-folder (created on first
/// use). WRITE-GATED: a sealed-and-not-session-unlocked target folder is refused (never resurrect
/// plaintext behind a lock). Returns the new note id. (Unlike `import_text` the empty-text refusal
/// is relaxed — an empty note is the whole point of "New note".)
#[tauri::command]
pub fn create_note(
    state: State<'_, AppState>,
    folder_id: Option<String>,
    title: String,
) -> Result<String, AppError> {
    create_note_inner(state.inner(), folder_id.as_deref(), &title)
}

/// Inner of [`create_note`] taking `&AppState` (unit-testable gate).
pub(crate) fn create_note_inner(
    state: &AppState,
    folder_id: Option<&str>,
    title: &str,
) -> Result<String, AppError> {
    // BLK-1 / TOCTOU (2026-07-10 audit F4): hold the lifecycle guard across gate+insert so a
    // concurrent lock/relock cannot land between the unlock check and the row insert.
    let lifecycle = lifecycle_guard(state);
    create_note_under_lifecycle(state, &lifecycle, folder_id, title)
}

/// [`create_note_inner`] after the caller has already acquired the lifecycle guard. Companion-note
/// birth uses this so the sealed/open insert and structural `meeting_id` linkage share one
/// authorization interval. Calling it without the supplied live guard is a bug.
pub(crate) fn create_note_under_lifecycle(
    state: &AppState,
    lifecycle: &std::sync::MutexGuard<'_, ()>,
    folder_id: Option<&str>,
    title: &str,
) -> Result<String, AppError> {
    // Resolve the anchor note-folder. A None/empty selection ⇒ the reserved always-open Notes ROOT
    // (unfiled), NOT the old default "Notes" folder — so "New note" can NEVER fail because the default
    // folder is sealed (the 2026-07-14 "Couldn't create the note" dead-end). `ensure_notes_root` is
    // idempotent + never locked.
    let folder_id = match folder_id {
        Some(f) if !f.is_empty() => {
            // The unified hierarchy is mixed-content: any renderable user Space/folder accepts an
            // authored note. The shared target oracle excludes machine-owned containers.
            ensure_meeting_folder_target(&state.db, Some(f))?;
            f.to_string()
        }
        _ => state.db.ensure_notes_root()?,
    };

    // WRITE-GATE: a sealed-and-not-session-unlocked folder is refused.
    if !folder_is_unlocked(state, &folder_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
            crate::errcode::FOLDER_LOCKED,
            "unlock this folder to add a note",
        )));
    }
    let title = title.trim();
    // The stored default for a never-named note — coupled to `UNTITLED_TITLE` so the written value and
    // the picker/audit `is_untitled_title` guards can never drift (see `crate::storage::db`).
    let title = if title.is_empty() {
        crate::storage::db::UNTITLED_TITLE
    } else {
        title
    };
    // The `name` is the filesystem-safe slug; `title` is the display title.
    let name = crate::export::sanitize_title(title);

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp_millis();
    // Seal-on-write (2026-07-10 audit F1, tightened by residual W3 to SEAL-THEN-INSERT): a note
    // created in a session-unlocked LOCKED folder is sealed FROM BIRTH — the blob for the empty
    // body is computed + VERIFIED first (fail-closed on a missing session KEK, BEFORE any row
    // exists), then text + blob land in ONE atomic INSERT. The pre-fix insert-then-seal left a
    // blob-less plaintext row lingering in the locked folder when the birth-seal failed (the
    // relock reblank rightly refuses to blank a blob-less row — never destroy the only copy).
    // Open folders: the plain insert.
    let locked = state
        .db
        .folder_by_id(&folder_id)?
        .map(|f| f.locked)
        .unwrap_or(false);
    if locked {
        let blob = sealed_document_blob(state, &folder_id, &id, "")?;
        state
            .db
            .insert_note_sealed(&id, &folder_id, &name, title, "", &blob, now)?;
    } else {
        state
            .db
            .insert_note(&id, &folder_id, &name, title, "", now)?;
    }
    // No chunks to index for an empty body — `update_note` re-indexes once the user writes.
    // Brain v3 PR-3 — LINK ENGINE: index the (empty) body's wikilinks so a create with no body
    // establishes a clean empty edge set; the first `update_note_doc` re-indexes real `[[Title]]`s.
    // No semantic pass on birth (no vectors yet). Best-effort.
    index_wikilinks_best_effort_under_lifecycle(
        state,
        lifecycle,
        crate::links::LinkKind::Note,
        &id,
        "",
    );
    tracing::info!(target: "notes", note_id = %id, folder_id = %folder_id, "note created");
    Ok(id)
}

/// System prompt for the on-device auto-title (Feature B, 2026-07-14). Short + strict output so the
/// result is a clean headline, not a sentence.
const AUTO_TITLE_SYSTEM: &str = "You write a short, specific title (3 to 6 words) for a note. \
Output ONLY the title text — no surrounding quotes, no trailing punctuation, no 'Title:' prefix.";

/// The on-device summarizer for a LOCAL-ONLY, zero-egress title — `None` when the on-device model
/// isn't downloaded (the caller then uses the first-line heuristic). NEVER the cloud: an auto-title
/// must never egress the note. Mirrors the local-provider construction in `summarize::provider_for`.
fn local_title_provider(
    state: &AppState,
) -> Option<std::sync::Arc<dyn crate::summarize::provider::SummarizerProvider>> {
    let (configured_path, model_id, timeouts) = {
        let config = state.config.lock().ok()?;
        (
            config.brain_model_path.clone(),
            config.brain_model_id.clone(),
            crate::reason::sidecar::SidecarTimeouts {
                idle_secs: config.brain_idle_timeout_secs,
                ready_secs: config.brain_ready_timeout_secs,
                hard_cap_secs: config.brain_hard_cap_secs,
            },
        )
    };
    let configured = configured_path.as_deref().map(std::path::Path::new);
    let path = crate::reason::resolve_brain_model(configured, model_id.as_deref()).ok()??;
    let reasoner: std::sync::Arc<dyn crate::reason::LocalReasoner> =
        std::sync::Arc::new(crate::reason::sidecar::SidecarReasoner::new(path, timeouts).ok()?);
    Some(std::sync::Arc::new(
        crate::summarize::local::LocalSummarizerProvider::new(
            reasoner,
            std::sync::Arc::clone(&state.heavy_inference),
        ),
    ))
}

/// Clean an LLM title suggestion into a single tidy line, or `None` if it collapsed to nothing.
fn sanitize_title_suggestion(raw: &str) -> Option<String> {
    let line = raw.trim().lines().next().unwrap_or("").trim();
    let line = line
        .trim_matches('"')
        .trim_matches('\'')
        .trim_start_matches('#')
        .trim();
    let line = line.trim_end_matches(['.', ':', '-']).trim();
    if line.is_empty() {
        return None;
    }
    Some(line.chars().take(80).collect())
}

/// Fallback title: the first non-empty line of the body, stripped of leading markdown, capped.
fn first_line_title(body: &str) -> String {
    let line = body
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let line = line.trim_start_matches(['#', '-', '*', '>', ' ']).trim();
    let capped: String = line.chars().take(60).collect();
    if capped.is_empty() {
        "Untitled".to_string()
    } else {
        capped
    }
}

/// `suggest_note_title(note_id)` — auto-title an "Untitled" note from its body (Feature B): the
/// on-device model when present, else a first-line heuristic. LOCAL-ONLY (never egresses). Persists
/// the new title ONLY if the note is still "Untitled" (never clobbers a user's title) and skips a note
/// in a LOCKED folder (spec decision #3 — no auto-unlock / seal-on-write for a title). Returns the
/// title (the current one unchanged when it skips). Called fire-and-forget by the editor on close.
#[tauri::command]
pub async fn suggest_note_title(
    state: State<'_, AppState>,
    note_id: String,
) -> Result<String, AppError> {
    suggest_note_title_inner(state.inner(), &note_id).await
}

pub(crate) async fn suggest_note_title_inner(
    state: &AppState,
    note_id: &str,
) -> Result<String, AppError> {
    let background_epoch = crate::perf::background_epoch();
    // Resolve and gate on content-free metadata under the lifecycle mutex BEFORE loading the full
    // row. A sealed row may retain plaintext after an interrupted cleanup; reading it merely to find
    // the folder would already violate the lock boundary. A locked call returns only the public
    // placeholder, never the stored title.
    let (folder_id, current, row_updated_at, row_text) = {
        let _lifecycle = lifecycle_guard(state);
        let Some((folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(note_id)?
        else {
            return Err(AppError::InvalidArg(crate::errcode::tag(
                crate::errcode::NOTE_MISSING,
                format!("no note {note_id}"),
            )));
        };
        if !folder_is_unlocked(state, &folder_id)? {
            return Ok(crate::storage::db::UNTITLED_TITLE.to_string());
        }
        let Some(row) = state.db.get_note_row(note_id)? else {
            return Err(AppError::InvalidArg(crate::errcode::tag(
                crate::errcode::NOTE_MISSING,
                format!("no note {note_id}"),
            )));
        };
        // `NoteRow.title` is Option<String> (NULL-able column) — treat a missing title as "".
        let current = row.title.clone().unwrap_or_default();
        // Only ever fill in an "Untitled" note — never overwrite a title the user chose.
        let cur = current.trim();
        if !cur.is_empty() && cur != "Untitled" {
            return Ok(current);
        }
        // A session-unlocked but logically LOCKED folder is readable for the session, yet auto-title
        // intentionally does not mutate it (the title-only CAS is open-folder-only by contract).
        if state
            .db
            .folder_by_id(&folder_id)?
            .map(|folder| folder.locked)
            .unwrap_or(false)
        {
            return Ok(current);
        }
        if row.text.trim().is_empty() {
            return Ok(current);
        }
        (folder_id, current, row.updated_at, row.text)
    };
    if !crate::perf::background_epoch_is_current(background_epoch) {
        return Ok(current);
    }

    // On-device first (zero egress); first-line heuristic otherwise / on failure.
    let title = match local_title_provider(state) {
        Some(provider) => {
            let excerpt: String = row_text.chars().take(1200).collect();
            match provider.complete(AUTO_TITLE_SYSTEM, &excerpt).await {
                Ok(t) => {
                    sanitize_title_suggestion(&t).unwrap_or_else(|| first_line_title(&row_text))
                }
                Err(e) => {
                    tracing::info!(target: "notes", error = %e, "auto-title: on-device failed, using first line");
                    first_line_title(&row_text)
                }
            }
        }
        None => first_line_title(&row_text),
    };
    if title.trim().is_empty() || title == "Untitled" {
        return Ok(current);
    }

    // One title-only CAS under the recording-priority epoch. The SQL itself rechecks the exact body
    // revision, folder identity, OPEN lock state, absent seal blob and still-placeholder title.
    // Never re-write `text`: a concurrent editor save or folder seal therefore always wins, and a
    // stale background task cannot resurrect plaintext after the lock blanked it.
    // The model call awaited outside the lifecycle mutex. Reacquire it and revalidate the exact
    // folder gate before either committing OR returning the derived title; a concurrent relock must
    // turn the stale result into the harmless placeholder instead of leaking content-derived text.
    let _lifecycle = lifecycle_guard(state);
    let Some((current_folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(note_id)?
    else {
        return Err(AppError::InvalidArg(crate::errcode::tag(
            crate::errcode::NOTE_MISSING,
            format!("no note {note_id}"),
        )));
    };
    if current_folder_id != folder_id || !folder_is_unlocked(state, &current_folder_id)? {
        return Ok(current);
    }
    let now = chrono::Utc::now().timestamp_millis();
    let committed = crate::perf::with_current_background_epoch(background_epoch, || {
        state.db.set_auto_title_if_unchanged_and_open(
            note_id,
            &folder_id,
            row_updated_at,
            &row_text,
            &title,
            now,
        )
    })?;
    if committed == Some(true) {
        Ok(title)
    } else {
        // Do not fresh-read a possibly newly sealed/user-retitled row just to satisfy this
        // fire-and-forget response. The harmless placeholder observed while open is leak-free.
        Ok(current)
    }
}

/// Read ONE note (editor DTO). GATED: a sealed-and-not-session-unlocked note returns the MASKED DTO
/// (title "🔒 Locked", no body/tags), never the stored text.
#[tauri::command]
pub fn get_note(state: State<'_, AppState>, id: String) -> Result<NoteDoc, AppError> {
    get_note_inner(state.inner(), &id)
}

/// Inner of [`get_note`] taking `&AppState` (unit-testable gate).
pub(crate) fn get_note_inner(state: &AppState, id: &str) -> Result<NoteDoc, AppError> {
    let _lifecycle = lifecycle_guard(state);
    get_note_under_lifecycle_authorized(state, id)
}

/// Read one note while the caller holds the non-reentrant lifecycle mutex. Authorization is
/// resolved from content-free metadata before any title/body/export-path column is selected.
fn get_note_under_lifecycle_authorized(state: &AppState, id: &str) -> Result<NoteDoc, AppError> {
    let Some((folder_id, created_at, updated_at)) = state.db.note_gate_anchor(id)? else {
        return Err(AppError::InvalidArg(crate::errcode::tag(
            crate::errcode::NOTE_MISSING,
            format!("no note {id}"),
        )));
    };
    if !folder_is_unlocked(state, &folder_id)? {
        return Ok(masked_note_doc(id, &folder_id, created_at, updated_at));
    }
    let Some(row) = state.db.get_note_row(id)? else {
        return Err(AppError::InvalidArg(crate::errcode::tag(
            crate::errcode::NOTE_MISSING,
            format!("no note {id}"),
        )));
    };
    let mut doc = note_doc_from_row(&row);
    // Strip any machine-managed `murmur:links` block (retired — it went stale + rendered as raw junk
    // in the plain-text editor; the RELATED panel reads the live `links` table instead). The strip is
    // FENCE-DELIMITED and HEADER-GATED: `strip_managed_links_block` removes the `<!-- murmur:links
    // -->…<!-- /murmur:links -->` region ONLY when its first body line is the exact machine callout
    // header `> [!related]- Related notes`, so a fence a USER typed/pasted into their own prose (real
    // text between the markers, no `[!related]` header) is left BYTE-IDENTICAL — never eaten and then
    // persisted by the editor's debounced autosave (owned-file data loss). Never touches user prose
    // or front-matter. Editor + Preview see prose-only, and the FE editor's next save writes back the
    // stripped body → a real block leaves the DB naturally (no migration, no bulk rewrite). NOTE this
    // runs ONLY on the VISIBLE/unlocked path; the masked-sealed DTO already returned above with NO
    // body, so no strip is ever attempted on sealed text. `murmur:context` (the connector-context
    // block) is a DISTINCT fence and is left untouched — only the link block is stripped.
    doc.markdown = crate::enrich::strip_managed_links_block(&doc.markdown);
    doc.shared = state.db.note_has_active_share(&row.id)?; // WP6 — active-share flag.
    Ok(doc)
}

/// List note summaries (leak-free). `folder_id = None` ⇒ all VISIBLE notes; `Some(fid)` scopes to
/// one note-folder. Filtered IN THE QUERY by `visibility_clause` — a sealed-not-unlocked note is
/// ABSENT (its title never leaks), not per-row masked.
#[tauri::command]
pub async fn list_notes(
    app: AppHandle,
    folder_id: Option<String>,
) -> Result<Vec<NoteSummary>, AppError> {
    offload_read(app, move |state| {
        list_notes_inner(state, folder_id.as_deref())
    })
    .await
}

/// Inner of [`list_notes`] taking `&AppState` (unit-testable gate).
pub(crate) fn list_notes_inner(
    state: &AppState,
    folder_id: Option<&str>,
) -> Result<Vec<NoteSummary>, AppError> {
    let unlocked = unlocked_snapshot(state)?;
    state.db.list_notes_visible(folder_id, &unlocked)
}

/// Update a note's title + markdown (write path). WRITE-GATED. Bumps `updated_at`, PURGES the old
/// `doc_chunks` and re-chunks+re-embeds the new BODY (front-matter stripped) so the note stays a
/// first-class brain source, then re-exports the vault `.md`. Returns the fresh DTO.
///
/// COMMIT BOUNDARY (authored-note full save): after the local write succeeds, BEST-EFFORT re-publish
/// any org shares of this note (never freeze the org copy at share time). NOT the debounced
/// `save_note_text` autosave (OCK-seal + egress per keystroke is unacceptable) — only this commit path.
#[tauri::command]
pub async fn update_note_doc(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    title: String,
    markdown: String,
) -> Result<NoteDoc, AppError> {
    let visibility = capture_document_content_snapshot(state.inner(), &id)?;
    // PERF (brain-v3 audit H3): the inner runs Candle/Metal embedding + the semantic-link pass —
    // route the whole synchronous body through the shared heavy-inference gate on the blocking
    // pool, exactly like the meeting twin `update_note` (PR-1 #362). Re-fetch `AppState` inside
    // the closure via `app.state()` — a bare `&AppState` cannot be captured by a `'static` closure.
    let heavy_inference = state.heavy_inference.clone();
    let app_for_edit = app.clone();
    let id_for_edit = id.clone();
    let title_for_edit = title.clone();
    let markdown_for_edit = markdown.clone();
    let doc = crate::perf::run_heavy(&heavy_inference, move || -> Result<NoteDoc, AppError> {
        let state = app_for_edit.state::<AppState>();
        update_note_doc_inner(&state, &id_for_edit, &title_for_edit, &markdown_for_edit)
    })
    .await?;
    // See `update_note`: ping open org views when the edit re-published ≥1 org copy. Best-effort.
    if republish_org_shares_for_source_notifying(state.inner(), None, Some(&id), &app)
        .await
        .unwrap_or(0)
        > 0
    {
        crate::events::emit_org_feed_updated(&app, 1);
    }
    require_current_document_content_snapshot(state.inner(), &id, &visibility)?;
    Ok(doc)
}

/// Inner of [`update_note_doc`] taking `&AppState` (unit-testable gate). Resolves the model-gated
/// embedder and delegates to [`update_note_doc_inner_with`] (embedder injected for deterministic
/// tests — the [`update_note_inner`] precedent).
pub(crate) fn update_note_doc_inner(
    state: &AppState,
    id: &str,
    title: &str,
    markdown: &str,
) -> Result<NoteDoc, AppError> {
    let embedder = crate::embed::active_persistence_embedder_if_available();
    update_note_doc_inner_with(state, id, title, markdown, embedder.as_deref())
}

/// Core of [`update_note_doc_inner`] with the re-index embedder INJECTED (`None` = model absent →
/// chunk-only re-index, FTS still covers keyword retrieval; `Some` = fresh vectors too).
pub(crate) fn update_note_doc_inner_with(
    state: &AppState,
    id: &str,
    title: &str,
    markdown: &str,
    embedder: Option<&dyn crate::embed::Embedder>,
) -> Result<NoteDoc, AppError> {
    // BLK-1 / TOCTOU (2026-07-10 audit F4): hold the lifecycle guard across gate+write so a
    // concurrent relock/seal cannot land between the unlock check and the row write.
    let lifecycle = lifecycle_guard(state);
    let doc = update_note_doc_under_lifecycle_authorized(state, id, title, markdown)?;

    // PERF (brain-v3 audit H3, the `update_note_inner_with` twin): release the GLOBAL lifecycle
    // guard BEFORE the heavy leg — the note-body re-embed + semantic-link pass is multi-second.
    drop(lifecycle);
    refresh_note_doc_derived_best_effort(state, id, title, markdown, embedder);

    tracing::info!(target: "notes", note_id = %id, "note updated + re-indexed");
    Ok(doc)
}

/// Canonical authored-note write while the caller already holds `lifecycle_guard`. This is the
/// single gate + seal-on-write + attachment + vault-export + fresh-DTO seam shared by the ordinary
/// editor update and convert-to-note. It deliberately performs no embedding/link inference: callers
/// drop the global lifecycle mutex before [`refresh_note_doc_derived_best_effort`].
pub(crate) fn update_note_doc_under_lifecycle_authorized(
    state: &AppState,
    id: &str,
    title: &str,
    markdown: &str,
) -> Result<NoteDoc, AppError> {
    let Some((folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(id)? else {
        return Err(AppError::InvalidArg(crate::errcode::tag(
            crate::errcode::NOTE_MISSING,
            format!("no note {id}"),
        )));
    };
    // WRITE-GATE before the content read: refuse editing a sealed-and-not-session-unlocked note
    // without ever loading its title/body/export path.
    if !folder_is_unlocked(state, &folder_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
            crate::errcode::NOTE_LOCKED,
            "unlock the folder to edit this note",
        )));
    }
    let title = title.trim();
    let title = if title.is_empty() { "Untitled" } else { title };
    let attachment_owner = crate::storage::AttachmentOwner::Document {
        document_id: id.to_string(),
    };
    validate_attachment_references_before_save(state, &attachment_owner, markdown)?;
    let now = chrono::Utc::now().timestamp_millis();
    // Seal-on-write (audit F1): a session-unlocked LOCKED folder re-seals the fresh text into
    // `text_blob` in the same write; an open folder takes the plain update. Fail-closed on a
    // missing session KEK.
    reseal_document_if_locked(state, &folder_id, id, title, markdown, now)?;
    prune_unreferenced_attachments(state, &attachment_owner, markdown)?;

    // Re-export the vault `.md` (best-effort). A sealed folder has no export (gated above), so this
    // only runs for a visible note.
    if let Err(e) = export_note_to_vault_under_lifecycle_authorized(state, id) {
        tracing::warn!(target: "notes", error = %e, "note vault re-export failed (text saved)");
    }
    get_note_under_lifecycle_authorized(state, id)
}

/// Refresh the derived retrieval/link projections after a canonical authored-note write. The
/// caller must NOT hold `lifecycle_guard`; every indexer has its own sealed-at-rest admission.
pub(crate) fn refresh_note_doc_derived_best_effort(
    state: &AppState,
    id: &str,
    title: &str,
    markdown: &str,
    embedder: Option<&dyn crate::embed::Embedder>,
) {
    let title = title.trim();
    let title = if title.is_empty() { "Untitled" } else { title };
    // WP3 — the note is a first-class brain source: purge the old chunks and re-index the new BODY
    // (front-matter stripped inside `index_note_body_chunks`). Chunks + FTS come back
    // unconditionally (keyword works model-less); vectors ONLY when the REAL e5 model is present
    // (never stub vectors; mirrors `ingest_into_folder`/`should_auto_index`). Best-effort: a failure
    // logs (no PII) and does NOT fail the update — the plaintext is durable.
    if let Err(e) = index_note_body_chunks(state, id, title, markdown, embedder) {
        tracing::warn!(target: "rag", error = %e, "note re-index on update failed (text saved)");
    }

    // Brain v3 PR-3 — LINK ENGINE: (a) re-index this note's `[[Title]]` wikilink edges by resolved
    // TARGET id (rename-proof), and (b) refresh its semantic suggestions from the fresh vectors
    // (model-gated). Both best-effort — a link failure never fails the note save.
    index_wikilinks_best_effort(state, crate::links::LinkKind::Note, id, markdown);
    auto_link_semantic_best_effort(state, crate::links::LinkKind::Note, id);
}

/// FAST autosave path — persist the note's title + markdown + `updated_at` ONLY. NO re-chunk, NO
/// re-embed, NO vault re-export: those are the expensive parts (the e5 embedder saturates the
/// machine if run on every keystroke-pause) and are DEFERRED to [`update_note_doc`], which the FE
/// runs on the natural boundaries (editor blur/close, Preview, explicit export/share). The frequent
/// debounced autosave calls THIS so typing stays smooth even with the embed model present. WRITE-
/// GATED exactly like the full update — a sealed-and-not-session-unlocked note is refused (never
/// write plaintext behind a lock). The DB text is canonical, so nothing is lost if the app closes
/// before the next full update. Returns the new `updated_at` (epoch ms) so the FE reconciles its
/// indicator without a full DTO round-trip.
#[tauri::command]
pub fn save_note_text(
    state: State<'_, AppState>,
    id: String,
    title: String,
    markdown: String,
) -> Result<i64, AppError> {
    save_note_text_inner(state.inner(), &id, &title, &markdown)
}

/// Inner of [`save_note_text`] taking `&AppState` (unit-testable gate).
pub(crate) fn save_note_text_inner(
    state: &AppState,
    id: &str,
    title: &str,
    markdown: &str,
) -> Result<i64, AppError> {
    // BLK-1 / TOCTOU (2026-07-10 audit F4): hold the lifecycle guard across gate+write so a
    // concurrent relock/seal cannot land between the unlock check and the row write.
    let _lifecycle = lifecycle_guard(state);
    let Some((folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(id)? else {
        return Err(AppError::InvalidArg(crate::errcode::tag(
            crate::errcode::NOTE_MISSING,
            format!("no note {id}"),
        )));
    };
    if !folder_is_unlocked(state, &folder_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
            crate::errcode::NOTE_LOCKED,
            "unlock the folder to edit this note",
        )));
    }
    let title = title.trim();
    let title = if title.is_empty() { "Untitled" } else { title };
    let attachment_owner = crate::storage::AttachmentOwner::Document {
        document_id: id.to_string(),
    };
    validate_attachment_references_before_save(state, &attachment_owner, markdown)?;
    let now = chrono::Utc::now().timestamp_millis();
    // Seal-on-write (audit F1): a session-unlocked LOCKED folder re-seals the fresh text into
    // `text_blob` in the same write; an open folder takes the plain update. Fail-closed on a
    // missing session KEK.
    reseal_document_if_locked_with_mode(state, &folder_id, id, title, markdown, now, false)?;
    prune_unreferenced_attachments(state, &attachment_owner, markdown)?;
    Ok(now)
}

/// Move a note to a different note-folder. GATED on BOTH the source and target folder-unlock (never
/// leave plaintext in a locked folder, never move out of a sealed one). Re-exports into the new
/// folder path (the old vault file is removed by the export path's move). Idempotent.
#[tauri::command]
pub async fn move_note_doc(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    folder_id: String,
) -> Result<(), AppError> {
    let _share_mutation = state.org_share_mutation_lock.lock().await;
    let target_locked = state
        .db
        .folder_by_id(&folder_id)?
        .is_some_and(|folder| folder.locked);
    if target_locked {
        emit_ask_history_invalidated_fail_closed(&app);
    }
    move_note_doc_inner(state.inner(), &id, &folder_id)?;
    // A move INTO a locked folder seals + purges ALL pending audit findings; an open-target move
    // purges nothing — the count-only ping is correct (and cheap) either way.
    emit_audit_updated_after_purge(&app, state.inner());
    Ok(())
}

/// Inner of [`move_note_doc`] taking `&AppState` (unit-testable gate).
pub(crate) fn move_note_doc_inner(
    state: &AppState,
    id: &str,
    folder_id: &str,
) -> Result<(), AppError> {
    // BLK-1 / TOCTOU (2026-07-10 audit F4): hold the lifecycle guard across the double gate + the
    // reassign/seal writes so a concurrent lock/relock cannot land mid-move.
    let lifecycle = lifecycle_guard(state);
    move_note_doc_under_lifecycle(state, &lifecycle, id, folder_id)
}

/// [`move_note_doc_inner`] after the caller has already acquired the lifecycle guard. This is used
/// by reviewed batch filing so its scope/target revalidation and the canonical move share one
/// indivisible authorization interval. Calling it without that guard is a bug.
pub(crate) fn move_note_doc_under_lifecycle(
    state: &AppState,
    _lifecycle: &std::sync::MutexGuard<'_, ()>,
    id: &str,
    folder_id: &str,
) -> Result<(), AppError> {
    let Some((source_folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(id)? else {
        return Err(AppError::InvalidArg(crate::errcode::tag(
            crate::errcode::NOTE_MISSING,
            format!("no note {id}"),
        )));
    };
    // Source gate before the content read: refuse moving a sealed-not-unlocked note without loading
    // its blanked/residual plaintext row merely to discover the governing folder.
    if !folder_is_unlocked(state, &source_folder_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
            crate::errcode::NOTE_LOCKED,
            "unlock the folder to move this note",
        )));
    }
    let Some(row) = state.db.get_note_row(id)? else {
        return Err(AppError::InvalidArg(crate::errcode::tag(
            crate::errcode::NOTE_MISSING,
            format!("no note {id}"),
        )));
    };
    // Target must be a renderable user Space/folder and be unlocked (never land plaintext behind a
    // lock). We do not
    // support sealing-on-move into a locked note-folder in WP0 — refuse (the FE unlocks first).
    ensure_meeting_folder_target(&state.db, Some(folder_id))?;
    if !folder_is_unlocked(state, folder_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
            crate::errcode::FOLDER_LOCKED,
            "unlock the target folder to move a note there",
        )));
    }

    // Seal alignment (2026-07-10 audit F1, reordered by residual W2 to SEAL-THEN-REASSIGN): the
    // seal blob must always match the OWNING folder's CK. Moving INTO a session-unlocked LOCKED
    // folder seals the plaintext under the TARGET folder's CK FIRST — encrypt + verify (fail-closed
    // on a missing session KEK) BEFORE anything is mutated — then reassigns folder + fresh blob +
    // NULLed exported_path in ONE atomic UPDATE ([`Db::move_note_row_sealed`]). The pre-fix
    // reassign-then-seal left the note sitting in the locked target with a stale wrong-CK blob when
    // the reseal failed (the target's next unlock would fail on it); now a seal failure leaves the
    // note untouched in the SOURCE folder (old `.md` included — the seal is computed BEFORE the
    // R1 delete below, so a fail-closed seal mutates nothing at all).
    // Moving into an OPEN folder clears any stale blob from the previous folder's CK (the plaintext
    // column is canonical again — a stale blob would be undecryptable under a future lock of the
    // new folder and could shadow fresh content).
    //
    // R1 (#231 review residual) — DELETE-THEN-MOVE: the OLD vault `.md` is removed BEFORE the
    // move UPDATE clears/NULLs `exported_path` (but AFTER the fallible seal, so a seal failure
    // still touches nothing). INVARIANT: a plaintext `.md` of locked-folder content is never left
    // on disk with no `exported_path` record pointing at it — the pre-fix best-effort remove
    // AFTER the UPDATE orphaned the file when the delete failed (or the app crashed between the
    // two), untracked and unreconcilable. An already-absent file is success; any other delete
    // failure REFUSES the move with the row (folder, exported_path, seal state) untouched — the
    // user retries. The DB `text` column stays the canonical copy throughout, so a crash after
    // this delete loses no content (the re-export recreates the `.md`).
    let target_locked = state
        .db
        .folder_by_id(folder_id)?
        .map(|f| f.locked)
        .unwrap_or(false);
    let target_closing = state.db.org_folder_closure_exists(folder_id)?;
    if target_closing {
        return Err(AppError::Unavailable(
            "the destination folder is closing or locked for sharing; retry after reopening it"
                .into(),
        ));
    }
    if target_locked && state.db.source_has_active_remote_share(None, Some(id))? {
        return Err(AppError::Unavailable(
            "revoke this note's shares before moving it into a locked folder".into(),
        ));
    }
    let attachment_owner = crate::storage::AttachmentOwner::Document {
        document_id: id.to_string(),
    };
    let attachment_rows = state.db.list_attachments(&attachment_owner)?;
    let mut attachment_plaintext = std::collections::HashMap::with_capacity(attachment_rows.len());
    for attachment in &attachment_rows {
        attachment_plaintext.insert(
            attachment.id.clone(),
            plaintext_attachment_data(state, attachment)?,
        );
    }
    if target_locked {
        bump_seal_epoch(state);
        let title = row.title.clone().unwrap_or_else(|| row.name.clone());
        let updated_at = row.updated_at.unwrap_or(row.created_at);
        let blob = sealed_document_blob(state, folder_id, id, &row.text)?;
        let ck = session_folder_ck(state, folder_id)?;
        let mut attachment_seals =
            std::collections::HashMap::with_capacity(attachment_plaintext.len());
        for (attachment_id, data) in &attachment_plaintext {
            let aad = attachment_aad(folder_id, &attachment_owner, attachment_id);
            let attachment_blob = crate::crypto::encrypt(&ck, data, &aad)?;
            if crate::crypto::decrypt(&ck, &attachment_blob, &aad)? != *data {
                return Err(AppError::Storage(
                    "attachment move-seal verification failed".into(),
                ));
            }
            attachment_seals.insert(attachment_id.clone(), attachment_blob);
        }
        remove_note_export_before_move(row.exported_path.as_deref())?;
        remove_attachment_exports_before_move(&attachment_rows)?;
        state.db.move_note_with_attachments_sealed(
            id,
            folder_id,
            &title,
            &row.text,
            &blob,
            updated_at,
            &attachment_seals,
        )?;
        // The destination is session-unlocked. The atomic move first installs verified target-CK
        // blobs and blanks plaintext (crash-safe at rest); now rematerialize the already-verified
        // bytes for this session while retaining those blobs for the next relock.
        for (attachment_id, data) in &attachment_plaintext {
            state
                .db
                .restore_attachment_data(attachment_id, data, false)?;
        }
        // Vault Audit LOCK-SAFETY: a move-into-locked is a SEAL — purge ALL pending findings
        // (the rollup posture; this note's title may be cited in third-party evidence no id can
        // match). The other seal paths purge inside their chunk-purge txs; this path has none.
        state.db.purge_all_pending_audit_findings()?;
        state.db.purge_all_ask_conversations()?;
    } else {
        remove_note_export_before_move(row.exported_path.as_deref())?;
        remove_attachment_exports_before_move(&attachment_rows)?;
        state
            .db
            .move_note_with_attachments_open(id, folder_id, &attachment_plaintext)?;
    }
    if let Err(e) = export_note_to_vault_under_lifecycle_authorized(state, id) {
        tracing::warn!(target: "notes", error = %e, "note re-export after move failed (moved in db)");
    }
    tracing::info!(target: "notes", note_id = %id, folder_id = %folder_id, "note moved");
    Ok(())
}

/// R1 helper — delete a note's OLD exported vault `.md` as the DELETE-THEN-MOVE step of
/// [`move_note_doc_inner`]. `None` / an already-absent file is success; any other failure is a
/// clear [`AppError::Export`] the caller propagates to REFUSE the move (nothing mutated yet, the
/// user retries) — never a silently orphaned plaintext file.
fn remove_note_export_before_move(old_path: Option<&str>) -> Result<(), AppError> {
    let Some(old_path) = old_path else {
        return Ok(()); // never exported — nothing to remove.
    };
    match std::fs::remove_file(old_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(AppError::Export(format!(
            "could not remove the note's exported vault file before moving it: {e}"
        ))),
    }
}

/// Permanently delete a note (cascade its chunks + vectors + vault `.md`). GATED: a
/// sealed-and-not-session-unlocked folder is refused. Reuses `delete_document` semantics.
///
/// DELETE-CASCADE FIX (2026-07-15): the local hard-delete used to leave any live org share of this
/// note `uploaded` — the server ciphertext survived + the 60s background org-sync tick re-pulled it
/// back into the local replica, resurrecting a "deleted" note. Now revokes every live org share of
/// this exact note FIRST (fails loud on a revoke error — see `revoke_org_shares_for_source`), then
/// fans out a content-free delete event so any other open surface (the tab-strip) can prune itself.
/// `async` (was sync) because the revoke is a network round-trip.
#[tauri::command]
pub async fn delete_note(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError> {
    delete_note_inner_notifying(state.inner(), &id, Some(&app)).await?;
    emit_ask_history_invalidated_fail_closed(&app);
    crate::events::emit_content_deleted(&app, "note", &id);
    // The delete purged its audit findings (id-matched) — ping the FE inbox (count-only).
    emit_audit_updated_after_purge(&app, state.inner());
    Ok(())
}

/// Inner of [`delete_note`] taking `&AppState` (unit-testable gate). `async` for the org-share revoke
/// cascade (network round-trip); the gate + DB delete themselves stay synchronous internally.
#[cfg(test)]
pub(crate) async fn delete_note_inner(state: &AppState, id: &str) -> Result<(), AppError> {
    delete_note_inner_notifying(state, id, None).await
}

async fn delete_note_inner_notifying(
    state: &AppState,
    id: &str,
    app: Option<&AppHandle>,
) -> Result<(), AppError> {
    let Some((folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(id)? else {
        return Ok(()); // unknown id → idempotent no-op.
    };
    if !folder_is_unlocked(state, &folder_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
            crate::errcode::FOLDER_LOCKED,
            "unlock this folder to delete a note",
        )));
    }
    let _org_mutation = state.org_share_mutation_lock.lock().await;
    state.db.begin_org_source_closure("document", id)?;
    // REVOKE-BEFORE-DELETE (Bug A root cause): tear down every LIVE org share of this exact note
    // BEFORE the local row disappears, so the background org-sync tick can never re-pull a still-live
    // server item back into the local replica after the user asked to delete it. Fails LOUD: a revoke
    // failure (e.g. offline) aborts the delete rather than silently leaving a dangling live share.
    revoke_org_shares_for_source_notifying(state, None, Some(id), app).await?;
    // The revoke awaited the network. Re-check under the lifecycle mutex before touching plaintext
    // exports or rows; a concurrent relock may have changed the source gate meanwhile.
    let _lifecycle = lifecycle_guard(state);
    let Some((folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(id)? else {
        return Ok(());
    };
    if !folder_is_unlocked(state, &folder_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
            crate::errcode::FOLDER_LOCKED,
            "unlock this folder to delete a note",
        )));
    }
    let Some(row) = state.db.get_note_row(id)? else {
        return Ok(());
    };
    // TRASH CAPTURE — the last step before anything is destroyed, and the first that can abort the
    // delete (verify-before-destroy inside). The exported vault `.md` IS still removed below: the
    // markdown lives in SQLCipher, so a restore re-exports it, and leaving a plaintext `.md` behind
    // for a "deleted" note would contradict the delete — and leak, for a locked folder.
    let trash_entry_id = super::trash_commands::capture_note(state, id)?;

    // Same all-or-nothing rollback as the meeting path: the steps below are fallible and the
    // snapshot is already durable, so a failure here would leave a trash entry for a note that
    // still exists (and whose restore would then refuse with "already exists").
    let deleted = delete_note_after_capture(state, id, &row);
    if deleted.is_err() {
        let _ = state.db.delete_trash_entry(&trash_entry_id);
    }
    deleted
}

/// The destructive half of [`delete_note_inner_notifying`], split out so its caller can retire the
/// trash snapshot if any of it fails. Runs with the lifecycle guard held and the gates satisfied.
fn delete_note_after_capture(
    state: &AppState,
    id: &str,
    row: &crate::storage::NoteRow,
) -> Result<(), AppError> {
    let attachment_owner = crate::storage::AttachmentOwner::Document {
        document_id: id.to_string(),
    };
    let attachments = state.db.list_attachments(&attachment_owner)?;
    remove_attachment_exports(
        &attachments,
        "could not remove an exported image before deleting the note",
    )?;
    // Remove the vault file first (best-effort), then cascade-delete the row + its chunks/vectors.
    if let Some(path) = &row.exported_path {
        let _ = std::fs::remove_file(path);
    }
    bump_seal_epoch(state);
    state.db.delete_document(id)?;
    tracing::info!(target: "notes", note_id = %id, "note deleted");
    Ok(())
}

/// (Re)write a note's vault `.md` and return the path. GATED. Idempotent (atomic, collision-suffixed
/// `write_note`). Stores the path in `documents.exported_path`.
#[tauri::command]
pub fn export_note_doc(state: State<'_, AppState>, id: String) -> Result<String, AppError> {
    let path = export_note_to_vault(state.inner(), &id)?;
    path.ok_or_else(|| {
        AppError::Export("no vault configured — set your Obsidian vault first".into())
    })
}

/// List every note-folder (`kind='note'`). Lock state comes through unchanged from `folders.locked`.
#[tauri::command]
pub fn list_note_folders(state: State<'_, AppState>) -> Result<Vec<NoteFolder>, AppError> {
    let mut folders = state.db.list_note_folders()?;
    // Join the live session unlock set so a sealed-but-session-unlocked note folder reports
    // `unlocked: true` — mirrors `list_folders` for the Meetings tree. Without this the Notes
    // lock gate reads only the DB `locked` column (which never flips on session-unlock), so
    // "Unlock folder" appeared to do nothing (the gate never lifted). (2026-07-14 fix.)
    let unlocked = {
        state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
            .clone()
    };
    for f in &mut folders {
        f.unlocked = f.locked && unlocked.contains(&f.id);
    }
    Ok(folders)
}

// ── Feature C — TYPED note front-matter properties (note-folder schemas + Table/Board substrate) ──

/// Max property columns a note-folder schema may declare (a UI-scale bound, not a hard DB limit).
const NOTE_SCHEMA_MAX_FIELDS: usize = 40;
/// Max length of a schema property key.
const NOTE_SCHEMA_MAX_KEY_LEN: usize = 60;

/// Read a note-folder's typed property SCHEMA (Feature C). GATED (the SAFER choice): a
/// sealed-and-not-session-unlocked folder returns `Ok(vec![])` — we deliberately do NOT expose a
/// locked folder's schema (the schema is only needed to view/edit an UNLOCKED folder). `InvalidArg`
/// when `folder_id` is not a note-folder.
#[tauri::command]
pub fn get_note_folder_schema(
    state: State<'_, AppState>,
    folder_id: String,
) -> Result<Vec<PropertySchemaField>, AppError> {
    get_note_folder_schema_inner(state.inner(), &folder_id)
}

/// Inner of [`get_note_folder_schema`] taking `&AppState` (unit-testable gate).
pub(crate) fn get_note_folder_schema_inner(
    state: &AppState,
    folder_id: &str,
) -> Result<Vec<PropertySchemaField>, AppError> {
    if state.db.note_folder_by_id(folder_id)?.is_none() {
        return Err(AppError::InvalidArg(crate::errcode::tag(
            crate::errcode::NOTE_FOLDER_MISSING,
            format!("no note folder {folder_id}"),
        )));
    }
    // GATE: a locked-and-not-session-unlocked folder's schema is NOT exposed (return empty).
    if !folder_is_unlocked(state, folder_id)? {
        return Ok(Vec::new());
    }
    state.db.get_note_folder_schema(folder_id)
}

/// Set a note-folder's typed property SCHEMA (Feature C). WRITE-GATED: a sealed-and-not-session-
/// unlocked folder is refused (`AppError::Locked`) — never mutate metadata behind a lock. Validates:
/// ≤40 fields, each `key` non-empty/trimmed/≤60 chars, case-insensitively UNIQUE, never the reserved
/// `"tags"`, and every `Select` field carries ≥1 non-empty option.
#[tauri::command]
pub fn set_note_folder_schema(
    state: State<'_, AppState>,
    folder_id: String,
    fields: Vec<PropertySchemaField>,
) -> Result<(), AppError> {
    set_note_folder_schema_inner(state.inner(), &folder_id, fields)
}

/// Inner of [`set_note_folder_schema`] taking `&AppState` (unit-testable gate + validation).
pub(crate) fn set_note_folder_schema_inner(
    state: &AppState,
    folder_id: &str,
    fields: Vec<PropertySchemaField>,
) -> Result<(), AppError> {
    if state.db.note_folder_by_id(folder_id)?.is_none() {
        return Err(AppError::InvalidArg(crate::errcode::tag(
            crate::errcode::NOTE_FOLDER_MISSING,
            format!("no note folder {folder_id}"),
        )));
    }
    // WRITE-GATE (before any validation or write): a sealed-not-unlocked folder is refused so a
    // schema can never be edited behind a lock.
    if !folder_is_unlocked(state, folder_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
            crate::errcode::FOLDER_LOCKED,
            "unlock this folder to edit its properties",
        )));
    }
    if fields.len() > NOTE_SCHEMA_MAX_FIELDS {
        return Err(AppError::InvalidArg(format!(
            "too many properties (max {NOTE_SCHEMA_MAX_FIELDS})"
        )));
    }
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for f in &fields {
        let key = f.key.trim();
        if key.is_empty() {
            return Err(AppError::InvalidArg(
                "property key must not be empty".into(),
            ));
        }
        if key.len() > NOTE_SCHEMA_MAX_KEY_LEN {
            return Err(AppError::InvalidArg(format!(
                "property key too long (max {NOTE_SCHEMA_MAX_KEY_LEN} chars)"
            )));
        }
        // `tags` is reserved for the front-matter tag list (parsed separately, never a property).
        if key.eq_ignore_ascii_case("tags") {
            return Err(AppError::InvalidArg(
                "property key \"tags\" is reserved".into(),
            ));
        }
        if !seen.insert(key.to_ascii_lowercase()) {
            return Err(AppError::InvalidArg(format!(
                "duplicate property key \"{key}\""
            )));
        }
        if f.kind == PropertyKind::Select && !f.options.iter().any(|o| !o.trim().is_empty()) {
            return Err(AppError::InvalidArg(format!(
                "select property \"{key}\" needs at least one option"
            )));
        }
    }
    // Persist the TRIMMED keys (so the read-time coercion keys match the front-matter keys exactly).
    let normalized: Vec<PropertySchemaField> = fields
        .into_iter()
        .map(|f| PropertySchemaField {
            key: f.key.trim().to_string(),
            kind: f.kind,
            options: f
                .options
                .into_iter()
                .map(|o| o.trim().to_string())
                .filter(|o| !o.is_empty())
                .collect(),
        })
        .collect();
    state.db.set_note_folder_schema(folder_id, &normalized)?;
    tracing::info!(target: "notes", folder_id = %folder_id, fields = normalized.len(), "note-folder schema updated");
    Ok(())
}

/// List a note-folder's notes projected through its typed schema (Feature C — the Table/Board
/// substrate). GATED: a sealed-and-not-session-unlocked folder returns `[]` (no rows, never a masked
/// row) via [`Db::list_notes_visible_typed`], which is built on the gated `list_notes_visible`.
/// `InvalidArg` when `folder_id` is not a note-folder.
#[tauri::command]
pub async fn list_notes_typed(
    app: AppHandle,
    folder_id: String,
) -> Result<Vec<TypedNoteRow>, AppError> {
    offload_read(app, move |state| {
        list_notes_typed_inner(state, &folder_id)
    })
    .await
}

/// Inner of [`list_notes_typed`] taking `&AppState` (unit-testable gate).
pub(crate) fn list_notes_typed_inner(
    state: &AppState,
    folder_id: &str,
) -> Result<Vec<TypedNoteRow>, AppError> {
    if state.db.note_folder_by_id(folder_id)?.is_none() {
        return Err(AppError::InvalidArg(crate::errcode::tag(
            crate::errcode::NOTE_FOLDER_MISSING,
            format!("no note folder {folder_id}"),
        )));
    }
    let unlocked = unlocked_snapshot(state)?;
    state.db.list_notes_visible_typed(folder_id, &unlocked)
}

/// Create a note-capable folder (`kind='note'`). An explicit parent may be any renderable user
/// Space/folder in the unified hierarchy; `None` retains the legacy Notes-root default.
#[tauri::command]
pub fn create_note_folder(
    state: State<'_, AppState>,
    name: String,
    parent_id: Option<String>,
) -> Result<NoteFolder, AppError> {
    create_note_folder_inner(state.inner(), &name, parent_id.as_deref())
}

/// Inner of [`create_note_folder`] taking `&AppState`.
pub(crate) fn create_note_folder_inner(
    state: &AppState,
    name: &str,
    parent_id: Option<&str>,
) -> Result<NoteFolder, AppError> {
    let lifecycle = lifecycle_guard(state);
    create_note_folder_under_lifecycle(state, &lifecycle, name, parent_id)
}

/// [`create_note_folder_inner`] after the caller has acquired the lifecycle guard. Reviewed batch
/// filing uses this so final source admission, destination birth and the move are indivisible.
pub(crate) fn create_note_folder_under_lifecycle(
    state: &AppState,
    _lifecycle: &std::sync::MutexGuard<'_, ()>,
    name: &str,
    parent_id: Option<&str>,
) -> Result<NoteFolder, AppError> {
    let clean = crate::summarize::organize::sanitize_folder(name)
        .ok_or_else(|| AppError::InvalidArg("folder name is empty or invalid".into()))?;

    // Resolve the parent FIRST, then gate it, then compose the path from THAT container. These three
    // used to be decided separately, and they agreed only on a database where nothing was sealed:
    //
    //   - the seal was checked only for an EXPLICIT parent, so a defaulted one was never checked;
    //   - the path was composed from the literal "Notes", and the parent link came from
    //     `ensure_default_note_folder`, which returns whatever row holds the "Notes" path — creating
    //     it if absent, and returning it EVEN WHEN IT IS SEALED. So on a database where the user has
    //     locked "Notes", both the link and the directory landed inside a sealed container while the
    //     child itself was open: plaintext into a sealed tree, which is precisely what this step
    //     exists to prevent.
    //
    // `ensure_notes_root` is the resolver that cannot do that. It returns the RESERVED note root —
    // the one `lock_folder` refuses to seal — and when "Notes" is already taken by a locked container
    // it puts that root somewhere else ("Inbox N") rather than handing back the locked row.
    let parent_id_owned: String = match parent_id {
        Some(pid) => pid.to_string(),
        None => state.db.ensure_notes_root()?,
    };
    ensure_meeting_folder_target(&state.db, Some(&parent_id_owned))?;
    let parent = state
        .db
        .folder_by_id(&parent_id_owned)?
        .ok_or_else(|| AppError::InvalidArg(format!("no parent container {parent_id_owned}")))?;
    // One gate for both, so an explicit and a defaulted parent cannot be governed by different rules.
    let parent_seal = container_parent_seal(state, &parent_id_owned)?;
    // An empty parent path is the vault root (the workspace project's own path), so
    // composing blindly would yield "/Name". The meeting-side create already has
    // this branch; the two must agree, because a path is what the seal keys its
    // vault work off.
    let parent_path = parent.path;
    let rel_path = if parent_path.is_empty() {
        clean.clone()
    } else {
        format!("{parent_path}/{clean}")
    };

    // Create the vault subdirectory (only when a vault is configured); assert it stays in-vault.
    // The path is VALIDATED here and the directory is created at the very end, only
    // once the container is durably in the state it will be observed in.
    //
    // Creating it first is what made the undo hard: a failed seal then had to remove
    // a directory the seal itself may have written into, `remove_dir` is non-recursive
    // so it fails on any residue, and removing recursively would risk deleting
    // something that was never ours. Ordering it last removes the whole question —
    // there is nothing to undo, because nothing was made. A row without a directory is
    // recoverable (the export path creates one on demand); plaintext inside a sealed
    // tree is not.
    let dir: Option<std::path::PathBuf> = match vault_path(state) {
        Some(vault) => Some(assert_in_vault(
            std::path::Path::new(&vault),
            std::path::Path::new(&rel_path),
        )?),
        None => None,
    };

    let folder = NoteFolder {
        id: uuid::Uuid::new_v4().to_string(),
        name: clean,
        path: rel_path,
        parent_id: Some(parent_id_owned.clone()),
        locked: false,
        // A freshly created folder is never sealed, so never session-unlocked.
        unlocked: false,
        // Only the reserved Notes root is `is_root`; a user-created folder never is.
        is_root: false,
        kind: "note".into(),
    };
    let now = chrono::Utc::now().to_rfc3339();
    if matches!(parent_seal, ParentSeal::SealChild) {
        // Born sealed in one statement — see the meeting-side create for why this is not
        // "insert, then lock": the window between those two writes is the state this guard
        // exists to prevent, and no undo can close it.
        let wrapped = wrapped_key_for_new_sealed_container(state, &folder.id)?;
        state
            .db
            .insert_sealed_note_folder(&folder, &now, &wrapped)?;
        create_container_dir_or_undo(state, &folder.id, dir.as_deref())?;
        return Ok(NoteFolder {
            locked: true,
            ..folder
        });
    }
    state.db.insert_note_folder(&folder, &now)?;
    create_container_dir_or_undo(state, &folder.id, dir.as_deref())?;
    Ok(folder)
}

/// Rename a note-folder (reuses the meeting-folder rename machinery — folder-id based and
/// kind-agnostic; it rewrites the `path` + all descendant paths + moves the vault dir). Gated to a
/// note-folder id so a meeting folder can't be renamed through here.
#[tauri::command]
pub fn rename_note_folder(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> Result<(), AppError> {
    if state.db.note_folder_by_id(&id)?.is_none() {
        return Err(AppError::InvalidArg(format!("no note folder {id}")));
    }
    rename_folder_inner(state.inner(), id, name)?;
    Ok(())
}

/// Delete a note-folder (reuses the meeting-folder delete machinery). Gated to a note-folder id.
#[tauri::command]
pub async fn delete_note_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError> {
    let _share_mutation = state.org_share_mutation_lock.lock().await;
    if state.db.note_folder_by_id(&id)?.is_none() {
        return Err(AppError::InvalidArg(format!("no note folder {id}")));
    }
    delete_folder_inner(state.inner(), id)?;
    emit_ask_history_invalidated_fail_closed(&app);
    Ok(())
}

/// Reparent a note-folder. Gated to a note-folder id; the new parent (if any) must also be a
/// note-folder. Rewrites the folder's `path` (and every descendant's) so vault export + gating stay
/// coherent, holding the lifecycle guard so it can't interleave with a lock/unlock.
#[tauri::command]
pub async fn move_note_folder(
    state: State<'_, AppState>,
    id: String,
    parent_id: Option<String>,
) -> Result<(), AppError> {
    let _share_mutation = state.org_share_mutation_lock.lock().await;
    move_note_folder_inner(state.inner(), &id, parent_id.as_deref())
}

/// Refuse a move whose subtree contains ANY sealed container, session-unlocked or not.
///
/// Asks by PATH PREFIX, which is the same question the move's own rewrite asks. Walking
/// parent links instead would disagree with the operation it guards on exactly the rows
/// this step repairs — a shipped note container has a correct path and a NULL parent
/// link — so a locked descendant would be invisible here and moved anyway. One query
/// also has no depth to bound and no cycle to loop on.
fn refuse_sealed_subtree(state: &AppState, path: &str) -> Result<(), AppError> {
    if state.db.subtree_has_sealed_container(path)? {
        return Err(AppError::Locked(
            "this folder, or one inside it, is locked — remove the lock, move it, then lock \
             it again"
                .into(),
        ));
    }
    Ok(())
}

/// Inner of [`move_note_folder`] taking `&AppState`.
pub(crate) fn move_note_folder_inner(
    state: &AppState,
    id: &str,
    parent_id: Option<&str>,
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    let folder = state
        .db
        .note_folder_by_id(id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note folder {id}")))?;
    // A move rewrites the `path` columns the seal keys its vault work off and physically
    // relocates the directory, so the whole moved subtree must be OPEN — not merely
    // session-unlocked. `ensure_folder_subtree_unlocked` accepts a sealed container that
    // is unlocked for this session, which is right for a rename (the bytes stay where the
    // seal can find them) but not here: a sealed container's vault directory holds
    // ciphertext and blanked exports, and moving it re-points every recorded path while
    // its blobs stay bound to keys and rows the move does not touch.
    // The vault root is not movable, and it must be refused BEFORE the sealed-subtree
    // question: an empty path makes the prefix "/%", which matches nothing, so the guard
    // below would see an empty subtree and wave the move through. `rename_folder_inner`
    // refuses the same container for the same reason — it is the one row whose path IS the
    // vault, so composing filesystem work from it targets the vault itself.
    if folder.path.is_empty() {
        return Err(AppError::InvalidArg(
            "this container is the workspace root and cannot be moved".into(),
        ));
    }
    refuse_sealed_subtree(state, &folder.path)?;
    if parent_id == Some(id) {
        return Err(AppError::InvalidArg(
            "a folder cannot be its own parent".into(),
        ));
    }
    // Resolve the destination FIRST — explicit, or the reserved note root — then gate it, then
    // compose the path from THAT container. Identical to the creation path, and for the same reason:
    // deciding the three separately let them name different containers, and a defaulted destination
    // was never gated at all. `ensure_notes_root` is the resolver that cannot hand back a sealed row.
    let target_id: String = match parent_id {
        Some(pid) => pid.to_string(),
        None => state.db.ensure_notes_root()?,
    };
    let target = state
        .db
        .note_folder_by_id(&target_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no parent note folder {target_id}")))?;
    // The destination's seal binds this container exactly as it binds a new one.
    match container_parent_seal(state, &target_id)? {
        ParentSeal::Open => {}
        ParentSeal::SealChild => {
            return Err(AppError::Locked(
                "moving a folder into a sealed folder is not supported — remove the lock, move, \
                 then lock again"
                    .into(),
            ))
        }
    }
    // Same branch as both creates: an empty parent path is the vault root, so
    // composing blindly would yield "/Name". All three writers compose alike, because
    // a path is what the seal keys its vault work off.
    let parent_path = target.path;
    let new_path = if parent_path.is_empty() {
        folder.name.clone()
    } else {
        format!("{parent_path}/{}", folder.name)
    };
    // A no-op ONLY when the destination already holds this container in both senses.
    // Comparing paths alone let a re-parent silently do nothing on exactly the rows
    // this step exists to repair: every note container a shipped build created has a
    // correct path and a NULL parent link, so moving one to the container its path
    // already names matched here and returned before writing the link.
    if new_path == folder.path && folder.parent_id.as_deref() == Some(target_id.as_str()) {
        return Ok(());
    }
    // A note-folder cannot move under its own descendant (would orphan the subtree). Descendants
    // have a path prefixed by this folder's path + "/".
    if parent_path == folder.path || parent_path.starts_with(&format!("{}/", folder.path)) {
        return Err(AppError::InvalidArg(
            "cannot move a folder into its own descendant".into(),
        ));
    }
    // Move the vault directory (best-effort) + rewrite this folder's + descendants' paths in the DB.
    // The RESOLVED destination, not the caller's argument: a defaulted move used to pass None
    // straight through and blank the parent link, leaving a container the tree cannot reach.
    reparent_note_folder_paths(state, id, &folder.path, &new_path, Some(&target_id))?;
    Ok(())
}

/// Rewrite `folders.path` for a moved note-folder and EVERY descendant (prefix rewrite), reparent
/// the row, and move the vault directory on disk (best-effort). Path uniqueness is preserved by the
/// prefix rewrite (the whole subtree moves as a unit). Kept small + note-scoped (the meeting-folder
/// rename has its own richer machinery; a note-folder tree is simpler).
fn reparent_note_folder_paths(
    state: &AppState,
    id: &str,
    old_path: &str,
    new_path: &str,
    parent_id: Option<&str>,
) -> Result<(), AppError> {
    // Vault dir move first (best-effort; a leftover/absent dir is reconcilable, lost content is not,
    // but note content lives in the DB — the .md files are re-exportable).
    if let Some(vault) = vault_path(state) {
        let vault_root = std::path::Path::new(&vault);
        let src = assert_in_vault(vault_root, std::path::Path::new(old_path))?;
        let dst = assert_in_vault(vault_root, std::path::Path::new(new_path))?;
        if src.exists() {
            if let Some(parent) = dst.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::rename(&src, &dst);
        }
    }
    state
        .db
        .reparent_note_folder(id, old_path, new_path, parent_id)?;
    Ok(())
}
