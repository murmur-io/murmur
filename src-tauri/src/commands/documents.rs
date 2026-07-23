//! DOCUMENT-INGEST command surface — upload/import/list/read/delete of brain `documents`
//! (`kind='document'` uploaded files + `kind='note'` typed brain notes), extracted VERBATIM from
//! `commands` (God-file split, a PURE MOVE — every read-gate / write-gate / mask body is UNCHANGED,
//! only relocated). This is the document domain: `import_document` (async off-thread
//! extract→chunk→embed behind the heavy permit + RAM floor), `import_text`, `list_documents`,
//! `get_document`, `delete_document`, plus the document-only extract/insert/gate helpers
//! (`extract_document_text`, `insert_extracted_document`, `import_document_write_gate`,
//! `ingest_into_folder`) and the `DOC_ALLOWED_EXTS` allowlist.
//!
//! LOCK-MODEL (byte-identical to the pre-move form): every WRITE (`import_document`/`import_text`)
//! WRITE-GATES the target folder via `super::folder_is_unlocked` (and, on the async import, RE-checks
//! the gate from the cloned `unlocked_folders` handle right before the plaintext INSERT — a relock
//! racing the extract can never land plaintext at rest behind the lock). Every READ gates: a
//! sealed-and-NOT-session-unlocked folder makes `list_documents` return an EMPTY list (no name leak)
//! and `get_document` return "" (no text leak) — exactly the masked-DTO posture. `delete_document`
//! refuses a sealed-not-unlocked folder (`AppError::Locked`) and REVOKES every live org share BEFORE
//! dropping the local row (no resurrection via the org feed). The document text is
//! sealed-and-restored + purged-on-lock with the folder; no new seal path is introduced here.
//!
//! The SHARED helpers (`folder_is_unlocked`, `revoke_org_shares_for_source`,
//! `emit_audit_updated_after_purge`, `index_document_row_kind_routed`,
//! `index_document_row_kind_routed_progress`, the `crate::extract`/`crate::embed`/`crate::events`
//! facades) STAY in `commands/mod.rs` (or their crate module) — this module reads them through
//! `use super::*` (a `commands` submodule sees its parent's private items). The moved `*_inner` cores
//! (plus `insert_extracted_document`/`import_document_write_gate`) stay `pub(crate)` so the STAYING
//! test modules keep calling them via the `pub use documents_commands::*;` re-export. Every symbol
//! keeps its EXACT prior body + signature; nothing changed except its file — no gate/mask body changed.

use super::*;

/// The extensions document ingestion accepts (Brain v3 PR-2). Text (md/txt) plus the extracted
/// formats: PDF (macOS PDFKit, scanned-PDF pages fall back to on-device Vision OCR), DOCX/PPTX
/// (pure-Rust OOXML), XLSX (calamine), HTML, and — Brain v3 OCR — direct image import
/// (png/jpg/jpeg/heic/tiff/tif/bmp/gif) via on-device Apple Vision. Dispatch + extraction live in
/// `crate::extract`; anything else is rejected with `InvalidArg`.
const DOC_ALLOWED_EXTS: &[&str] = &[
    "md", "txt", "pdf", "docx", "pptx", "xlsx", "html", "htm", "png", "jpg", "jpeg", "heic",
    "tiff", "tif", "bmp", "gif",
];

/// Document ingestion — upload a local file INTO a folder so its EXTRACTED text is chunked + embedded
/// into the on-device vector layer and the brain/Ask can retrieve it. Returns the new document id.
/// ASYNC (Brain v3 PR-2): a large PDF's extract+chunk+embed runs off the UI thread behind the shared
/// heavy-inference permit + the RAM floor, emitting counts-only progress via [`EVENT_DOC_IMPORT`].
///
/// LOCK-MODEL:
/// - WRITE-GATE: refuse a sealed-and-NOT-session-unlocked folder (`AppError::Locked`) — an ungated
///   write would land plaintext at rest behind the lock (mirrors `save_manual_notes`'s gate).
/// - Extension allowlist — reject anything else with `AppError::InvalidArg`.
/// - We store the EXTRACTED TEXT only (`documents.text`), never the source binary — no new seal path.
/// - EMBED only when the REAL e5 model is present (`embed_model_present()`): otherwise the chunks are
///   stored WITHOUT vectors (no stub vectors polluting the index — mirrors `should_auto_index`).
/// - The text is SEALED-AND-RESTORED with the folder on lock/unlock; its chunks are PURGED on lock,
///   re-embeddable on unlock.
#[tauri::command]
pub async fn import_document(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    folder_id: String,
) -> Result<String, AppError> {
    // 0) WRITE-GATE up front, on the async task (touches the borrowed `&AppState`): a
    //    sealed-and-NOT-session-unlocked folder is refused BEFORE any file work so the caller fails
    //    fast (`AppError::Locked`), and an unknown folder is `InvalidArg`. The gate is RE-CHECKED
    //    inside the blocking closure right before the plaintext INSERT (below) using the cloned
    //    `unlocked_folders` handle, so a relock racing between here and the insert can't land
    //    plaintext at rest behind the lock.
    import_document_write_gate(state.inner(), &folder_id)?;

    // Clone ONLY the `Arc` handles that cross into the blocking closure — `AppState` itself is not
    // `Clone`. `db` for insert+index, `unlocked_folders` for the pre-insert gate re-check,
    // `heavy_inference` for the ONE heavy permit. Everything past this point is `'static`.
    let db = std::sync::Arc::clone(&state.inner().db);
    let unlocked = std::sync::Arc::clone(&state.inner().unlocked_folders);
    let sem = std::sync::Arc::clone(&state.inner().heavy_inference);

    // RAM floor: under memory pressure, do the (still off-thread) extract+insert but SKIP the embed —
    // the chunks + FTS are durable (keyword retrieval works) and the idempotent repair tick / a later
    // Reindex fills the vectors. Never fail the import over a busy machine. Decided here (on the async
    // task) and passed INTO the closure so the whole heavy pipeline stays inside ONE `run_heavy` scope.
    let ram_permits = crate::transcribe::model::topic_backfill_ram_permits_now();

    // The WHOLE pipeline — extract (whole-file read + zip/XML parse for OOXML/XLSX, or the per-page
    // PDFKit loop; multi-second on a large doc), insert the row + chunk (FTS durable), then embed —
    // runs OFF the Tokio worker behind the ONE heavy-inference permit (rust-tauri rule: long-running
    // work never blocks the runtime thread). Progress events fire from inside across the stages
    // (extracting → chunking → embedding → done); the `AppHandle` is `Clone` + `Send` so the emitter
    // reaches the FE from the blocking thread. Best-effort per stage: the row/chunks stay durable even
    // if the embed fails.
    let app2 = app.clone();
    let path2 = path.clone();
    let folder2 = folder_id.clone();
    crate::perf::run_heavy(&sem, move || {
        crate::events::emit_doc_import(&app2, "", "extracting", 0, 0);
        // EXTRACT (pure `path → text`, no DB/state). The progress closure translates the extractor's
        // per-page signal into `EVENT_DOC_IMPORT` "extracting done/total" events (Fix 3: real counts)
        // and records an OCR-cap truncation so the "done" event can flag a partial import (Fix 2).
        let ocr_truncated = std::cell::Cell::new(false);
        let (name, stored) = {
            let app_p = &app2;
            let extract_progress = |p: crate::extract::ExtractProgress| match p {
                crate::extract::ExtractProgress::Page { done, total } => {
                    crate::events::emit_doc_import(app_p, "", "extracting", done, total);
                }
                crate::extract::ExtractProgress::OcrTruncated { .. } => {
                    ocr_truncated.set(true);
                }
            };
            extract_document_text(&path2, &extract_progress)?
        };

        crate::events::emit_doc_import(&app2, "", "chunking", 0, 0);
        // GATE (re-checked from the cloned handle) + INSERT + CHUNK-ONLY index (FTS durable now).
        let id = insert_extracted_document(&db, &unlocked, &folder2, &name, &stored)?;

        // EMBED only if the RAM floor permitted (else defer to the repair tick). Best-effort: a
        // failure logs (no PII) and leaves the durable chunks/FTS + row in place. Per-sub-batch
        // progress (Fix 3) streams "embedding done/total" as each embed sub-batch completes.
        if ram_permits {
            let embedder = crate::embed::active_persistence_embedder_if_available();
            crate::events::emit_doc_import(&app2, &id, "embedding", 0, 0);
            let id_for_progress = id.clone();
            let embed_progress = |done: usize, total: usize| {
                crate::events::emit_doc_import(&app2, &id_for_progress, "embedding", done, total);
            };
            if let Err(e) =
                index_document_row_kind_routed_progress(&db, &id, embedder.as_deref(), &embed_progress)
            {
                tracing::warn!(target: "rag", error = %e, document_id = %id, "import: embed failed (content stored)");
            }
        } else {
            tracing::info!(target: "documents", document_id = %id, "import: RAM floor — deferring embed to repair tick");
        }
        // DONE — carry the OCR-cap truncation flag so the FE can surface a partial-import notice.
        crate::events::emit_doc_import_done(&app2, &id, ocr_truncated.get());
        Ok(id)
    })
    .await
}

/// Inner of [`import_document`] taking `&AppState` (so the gate + allowlist + EXTRACTION are
/// unit-testable without a `tauri::State`). EXTRACTS the file to blocks, stores the serialized text,
/// inserts the row (write-gated), and DOES NOT embed (the async wrapper embeds behind the RAM floor /
/// permit; a unit test with the model absent still gets chunk-only indexing via `ingest_into_folder`).
///
/// This runs SYNCHRONOUSLY (the async command wrapper does the same extract→insert→embed pipeline
/// entirely inside `perf::run_heavy` / `spawn_blocking` — see [`import_document`]); this seam exists
/// only so the extract + gate + insert are testable without a `tauri::State` or a runtime. It has no
/// production caller (the async wrapper composes `extract_document_text` + `insert_extracted_document`
/// directly), so it is compiled only under `cfg(test)`.
#[cfg(test)]
pub(crate) fn import_document_inner(
    state: &AppState,
    path: &str,
    folder_id: &str,
) -> Result<String, AppError> {
    // EXTRACT (allowlist + `path → text`, pure, no DB/state).
    let (name, stored) = extract_document_text(path, &crate::extract::no_progress)?;
    // GATE + INSERT + chunk-only index (the shared seam the async wrapper also uses).
    insert_extracted_document(
        &state.db,
        &state.unlocked_folders,
        folder_id,
        &name,
        &stored,
    )
}

/// EXTRACT a supported document to its storable text form — PURE `path → (display_name, text)`, with
/// NO `AppState`/DB/keychain touch, so the whole (potentially multi-second: whole-file read + zip
/// decompress + XML parse for OOXML/XLSX, or the per-page PDFKit loop) extraction can run OFF the
/// Tokio runtime inside `run_heavy`. Fails CLOSED with `AppError::InvalidArg` for an unsupported /
/// extension-less / unreadable / no-extractable-text file. Returns the file-name component as the
/// display name (never an on-disk path — no PII in the stored name/logs).
fn extract_document_text(
    path: &str,
    progress: &crate::extract::ProgressFn<'_>,
) -> Result<(String, String), AppError> {
    // Extension allowlist. Lowercased; an extension-less path is rejected.
    let p = std::path::Path::new(path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let ext = match ext {
        Some(e) if DOC_ALLOWED_EXTS.contains(&e.as_str()) => e,
        _ => {
            return Err(AppError::InvalidArg(
                "unsupported document type — import md, txt, pdf, docx, pptx, xlsx, html, or an image (png/jpg/heic/tiff/bmp/gif)".into(),
            ))
        }
    };

    // EXTRACT to blocks (page/heading preserved), then serialize to the storable text form. A
    // non-UTF-8 / unreadable / malformed / scanned-PDF / zip-bomb / over-size file fails closed inside
    // `extract_blocks` (the OOXML/XLSX decompression-ratio guard + the universal extracted-text
    // ceiling live there — see extract/mod.rs + extract/ooxml.rs). `progress` streams per-page counts.
    let blocks = crate::extract::extract_blocks(p, &ext, progress)?;
    if blocks.is_empty() || blocks.iter().all(|b| b.text.trim().is_empty()) {
        return Err(AppError::InvalidArg(
            "this document has no extractable text".into(),
        ));
    }
    let stored = crate::extract::blocks_to_stored_text(&blocks);

    // The display name = the file name (component only — never an on-disk path with personal content
    // in logs). Fallback to "document" if the path has no file-name component.
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "document".to_string());
    Ok((name, stored))
}

/// The WRITE-GATE for an uploaded document, evaluated from `Arc` handles (not a `&AppState`), so it
/// can be RE-CHECKED inside the blocking closure right before the plaintext INSERT — a relock racing
/// between the up-front async gate and the insert can't land plaintext at rest behind a lock. Mirrors
/// [`ingest_into_folder_opts`]'s gate exactly: unknown folder ⇒ `InvalidArg`; sealed-and-NOT-session-
/// unlocked ⇒ `AppError::Locked`; open OR session-unlocked ⇒ Ok.
pub(crate) fn insert_extracted_document(
    db: &std::sync::Arc<crate::storage::Db>,
    unlocked_folders: &std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    folder_id: &str,
    name: &str,
    stored: &str,
) -> Result<String, AppError> {
    // The folder must exist (so the FK holds + the gating has an anchor).
    let folder = db
        .folder_by_id(folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;

    // WRITE-GATE: a sealed-and-not-session-unlocked folder is refused (never resurrect plaintext at
    // rest behind a lock). Same predicate as `folder_is_unlocked`, evaluated from the cloned handle.
    if folder.locked {
        let session_unlocked = unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
            .contains(folder_id);
        if !session_unlocked {
            return Err(AppError::Locked(
                "this folder is locked — unlock it to add to the brain".into(),
            ));
        }
    }

    let id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().timestamp_millis();
    db.insert_document(&id, folder_id, name, stored, "document", created_at)?;

    // CHUNK-ONLY inline (FTS durable now); the caller embeds under the RAM floor + heavy permit. A
    // failure logs (no PII) and does NOT fail the ingest (the row + plaintext are durable; a later
    // unlock re-chunk / reindex recovers the index).
    if let Err(e) = index_document_row_kind_routed(db, &id, None) {
        tracing::warn!(target: "rag", error = %e, "ingest: chunk failed (content stored)");
    }

    // PII rule: log only ids, the kind, and byte counts — never the text/name.
    tracing::info!(
        target: "documents",
        document_id = %id,
        folder_id = %folder_id,
        kind = "document",
        bytes = stored.len(),
        "ingested document into brain"
    );
    Ok(id)
}

/// The WRITE-GATE for an uploaded document evaluated from a borrowed `&AppState` (the up-front,
/// fail-fast check on the async task before any file work). Same predicate as
/// [`insert_extracted_document`]'s re-check; delegates to the existing `folder_is_unlocked`.
pub(crate) fn import_document_write_gate(
    state: &AppState,
    folder_id: &str,
) -> Result<(), AppError> {
    let folder = state
        .db
        .folder_by_id(folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    if folder.locked && !folder_is_unlocked(state, folder_id)? {
        return Err(AppError::Locked(
            "this folder is locked — unlock it to add to the brain".into(),
        ));
    }
    Ok(())
}

/// Ingest TYPED text as a brain `note` (the Brain page "+ Add note"). Same gated ingest path + seal
/// + vector indexing as an uploaded document, just `kind="note"` and no file/extension step.
#[tauri::command]
pub fn import_text(
    state: State<'_, AppState>,
    name: String,
    text: String,
    folder_id: String,
) -> Result<String, AppError> {
    import_text_inner(state.inner(), &name, &text, &folder_id)
}

/// Inner of [`import_text`] taking `&AppState` (unit-testable gate). Empty text is refused.
pub(crate) fn import_text_inner(
    state: &AppState,
    name: &str,
    text: &str,
    folder_id: &str,
) -> Result<String, AppError> {
    if text.trim().is_empty() {
        return Err(AppError::InvalidArg("note text is empty".into()));
    }
    let name = if name.trim().is_empty() {
        "note"
    } else {
        name.trim()
    };
    ingest_into_folder(state, folder_id, name, text, "note")
}

/// The SINGLE gated ingest path for a typed note (`kind="note"`): look up the folder, WRITE-GATE it
/// (a sealed-not-unlocked folder is refused so content can never appear at rest behind a lock),
/// insert the `documents` row, and index its chunks into the vector layer ONLY when the REAL e5 model
/// is present (never stub vectors — mirrors `should_auto_index`). The row is sealed-and-restored +
/// purged-on-lock identically to an uploaded document. Returns the new id.
///
/// An uploaded DOCUMENT (`kind="document"`) takes the sibling [`insert_extracted_document`] seam
/// instead — same gate + insert + chunk-only index, but reachable from `Arc` handles so the whole
/// extract→insert→embed pipeline runs off the Tokio runtime inside `run_heavy` (Brain v3 PR-2).
fn ingest_into_folder(
    state: &AppState,
    folder_id: &str,
    name: &str,
    text: &str,
    kind: &str,
) -> Result<String, AppError> {
    // The folder must exist (so the FK holds + the gating has an anchor).
    let folder = state
        .db
        .folder_by_id(folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;

    // WRITE-GATE: a sealed-and-not-session-unlocked folder is refused (never resurrect plaintext at
    // rest behind a lock). One gate for every ingest path.
    if folder.locked && !folder_is_unlocked(state, folder_id)? {
        return Err(AppError::Locked(
            "this folder is locked — unlock it to add to the brain".into(),
        ));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().timestamp_millis();
    state
        .db
        .insert_document(&id, folder_id, name, text, kind, created_at)?;

    // ALWAYS chunk (doc_chunks + the fts_doc_chunks triggers) so keyword retrieval works on a
    // DEFAULT install — an ingested note must never be write-only memory. Vectors ONLY when the
    // REAL e5 model is present (never write stub vectors). Best-effort: a failure logs (no PII) and
    // does NOT fail the ingest (the row + plaintext are durable; a later unlock re-chunk / reindex
    // recovers the index).
    //
    // KIND-ROUTED (PR-1 finding 4): a pasted note (`kind='note'`) can carry YAML front-matter in its
    // raw `text`, which must NEVER be embedded/indexed (DESIGN §1a — tags/properties pollute the
    // vectors + snippets). Route through the ONE front-matter-stripping seam so the ingest matches
    // every other note-index path.
    let embedder = crate::embed::active_persistence_embedder_if_available();
    if let Err(e) = index_document_row_kind_routed(&state.db, &id, embedder.as_deref()) {
        tracing::warn!(target: "rag", error = %e, "ingest: chunk/embed failed (content stored)");
    }

    // PII rule: log only ids, the kind, and byte/char counts — never the text/name.
    tracing::info!(
        target: "documents",
        document_id = %id,
        folder_id = %folder_id,
        kind = %kind,
        bytes = text.len(),
        "ingested into brain"
    );
    Ok(id)
}

/// List a folder's documents (metadata only — NO text). GATED: a sealed-and-NOT-session-unlocked
/// folder returns an EMPTY list (masked — never surface even a document name behind the lock),
/// exactly like the masked detail DTO drops note/segments.
#[tauri::command]
pub fn list_documents(
    state: State<'_, AppState>,
    folder_id: String,
) -> Result<Vec<DocumentInfo>, AppError> {
    list_documents_inner(state.inner(), &folder_id)
}

/// Inner of [`list_documents`] taking `&AppState` (unit-testable gate).
pub(crate) fn list_documents_inner(
    state: &AppState,
    folder_id: &str,
) -> Result<Vec<DocumentInfo>, AppError> {
    if !folder_is_unlocked(state, folder_id)? {
        return Ok(Vec::new()); // sealed-not-unlocked ⇒ masked (no ids, no names).
    }
    state.db.documents_in_folder(folder_id)
}

/// Read ONE document's full text. GATED: a sealed-and-NOT-session-unlocked folder returns "" (masked
/// — never leak the document text), exactly like `get_manual_notes`.
#[tauri::command]
pub fn get_document(state: State<'_, AppState>, id: String) -> Result<String, AppError> {
    get_document_inner(state.inner(), &id)
}

/// Inner of [`get_document`] taking `&AppState` (unit-testable gate).
pub(crate) fn get_document_inner(state: &AppState, id: &str) -> Result<String, AppError> {
    // Serialize gate + content read with relock, and resolve only the content-free folder anchor
    // before authorization. A locked row may retain residual plaintext after interrupted cleanup;
    // that plaintext must not enter the process merely to discover its governing folder.
    let _lifecycle = lifecycle_guard(state);
    let Some(folder_id) = state.db.folder_for_document(id)? else {
        return Ok(String::new()); // unknown id → nothing.
    };
    if !folder_is_unlocked(state, &folder_id)? {
        return Ok(String::new()); // sealed-not-unlocked ⇒ masked, never the stored text.
    }
    let Some((_folder_id, _name, text)) = state.db.get_document(id)? else {
        return Ok(String::new());
    };
    // Brain v3 PR-2: strip the block-structure markers a PR-2 upload stores in `text` — the FE gets
    // clean readable text (a note / md / txt / legacy row has no markers → unchanged).
    Ok(crate::extract::render_display_text(&text))
}

/// Permanently delete a document and cascade-delete its chunks + vectors. GATED: a
/// sealed-and-NOT-session-unlocked folder is refused (`AppError::Locked`) so the lock state can't be
/// mutated from behind the gate (consistent with `import_document`'s write-gate).
///
/// DELETE-CASCADE FIX (2026-07-15): `delete_document` is generic over BOTH `kind='document'`
/// (imported/ingested files) and `kind='note'` rows (`brain.component.ts`'s `removeItem` can reach
/// either), so it needs the SAME org-share revoke cascade as [`delete_note`] before dropping the local
/// row — a `kind='note'` document deleted through THIS surface must not resurrect via the org feed
/// either. `async` (was sync) because the revoke is a network round-trip.
#[tauri::command]
pub async fn delete_document(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError> {
    // Only a `kind='note'` row is ever tab-tracked (a plain ingested `kind='document'` has no tab —
    // see `TabKind` in `tab-keys.ts`), so only fire the delete-fan-out event for that case: emitting
    // it for an id nothing tracks is harmless, but this stays precise about what was actually deleted.
    let was_note = matches!(state.db.note_gate_anchor(&id), Ok(Some(_)));
    delete_document_inner(state.inner(), &id).await?;
    if was_note {
        crate::events::emit_content_deleted(&app, "note", &id);
    }
    // The delete purged its audit findings (id-matched) — ping the FE inbox (count-only).
    emit_audit_updated_after_purge(&app, state.inner());
    Ok(())
}

/// Inner of [`delete_document`] taking `&AppState` (unit-testable gate). `async` for the org-share
/// revoke cascade (network round-trip); the gate + DB delete themselves stay synchronous internally.
pub(crate) async fn delete_document_inner(state: &AppState, id: &str) -> Result<(), AppError> {
    let Some(folder_id) = state.db.folder_for_document(id)? else {
        return Ok(()); // unknown id → idempotent no-op.
    };
    if !folder_is_unlocked(state, &folder_id)? {
        return Err(AppError::Locked(
            "this folder is locked — unlock it to delete a document".into(),
        ));
    }
    // REVOKE-BEFORE-DELETE (Bug A root cause): tear down every LIVE org share of this exact source
    // BEFORE the local row disappears, so the background org-sync tick can never re-pull a still-live
    // server item back into the local replica after the user asked to delete it. Fails LOUD: a revoke
    // failure (e.g. offline) aborts the delete rather than silently leaving a dangling live share.
    revoke_org_shares_for_source(state, None, Some(id)).await?;
    // The generic surface can also delete a `kind='note'` document. Re-check the gate under the
    // lifecycle mutex after the network await and remove every tracked image while its row still
    // carries retry metadata. Plain imported documents have no attachment owner and skip this leg.
    let _lifecycle = lifecycle_guard(state);
    let Some(folder_id) = state.db.folder_for_document(id)? else {
        return Ok(());
    };
    if !folder_is_unlocked(state, &folder_id)? {
        return Err(AppError::Locked(
            "this folder is locked — unlock it to delete a document".into(),
        ));
    }
    if state.db.note_gate_anchor(id)?.is_some() {
        let attachment_owner = crate::storage::AttachmentOwner::Document {
            document_id: id.to_string(),
        };
        let attachments = state.db.list_attachments(&attachment_owner)?;
        remove_attachment_exports(
            &attachments,
            "could not remove an exported image before deleting the note",
        )?;
    }
    state.db.delete_document(id)?;
    tracing::info!(target: "documents", document_id = %id, "document deleted");
    Ok(())
}
