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
            return Err(AppError::InvalidArg(crate::errcode::tag(
                crate::errcode::DOC_UNSUPPORTED,
                "import md, txt, pdf, docx, pptx, xlsx, html, or an image (png/jpg/heic/tiff/bmp/gif)",
            )))
        }
    };

    // EXTRACT to blocks (page/heading preserved), then serialize to the storable text form. A
    // non-UTF-8 / unreadable / malformed / scanned-PDF / zip-bomb / over-size file fails closed inside
    // `extract_blocks` (the OOXML/XLSX decompression-ratio guard + the universal extracted-text
    // ceiling live there — see extract/mod.rs + extract/ooxml.rs). `progress` streams per-page counts.
    let blocks = crate::extract::extract_blocks(p, &ext, progress)?;
    if blocks.is_empty() || blocks.iter().all(|b| b.text.trim().is_empty()) {
        return Err(AppError::InvalidArg(crate::errcode::tag(
            crate::errcode::DOC_NO_TEXT,
            "this document has no extractable text",
        )));
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
            return Err(AppError::Locked(crate::errcode::tag(
                crate::errcode::DOC_LOCKED,
                "unlock this folder to add to the brain",
            )));
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
        return Err(AppError::Locked(crate::errcode::tag(
            crate::errcode::DOC_LOCKED,
            "unlock this folder to add to the brain",
        )));
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
        return Err(AppError::Locked(crate::errcode::tag(
            crate::errcode::DOC_LOCKED,
            "unlock this folder to add to the brain",
        )));
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
pub async fn list_documents(
    app: AppHandle,
    folder_id: String,
) -> Result<Vec<DocumentInfo>, AppError> {
    offload_read(app, move |state| {
        list_documents_inner(state, &folder_id)
    })
    .await
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
    delete_document_inner_notifying(state.inner(), &id, Some(&app)).await?;
    emit_ask_history_invalidated_fail_closed(&app);
    if was_note {
        crate::events::emit_content_deleted(&app, "note", &id);
    }
    // The delete purged its audit findings (id-matched) — ping the FE inbox (count-only).
    emit_audit_updated_after_purge(&app, state.inner());
    Ok(())
}

/// Inner of [`delete_document`] taking `&AppState` (unit-testable gate). `async` for the org-share
/// revoke cascade (network round-trip); the gate + DB delete themselves stay synchronous internally.
#[cfg(test)]
pub(crate) async fn delete_document_inner(state: &AppState, id: &str) -> Result<(), AppError> {
    delete_document_inner_notifying(state, id, None).await
}

async fn delete_document_inner_notifying(
    state: &AppState,
    id: &str,
    app: Option<&AppHandle>,
) -> Result<(), AppError> {
    let Some(folder_id) = state.db.folder_for_document(id)? else {
        return Ok(()); // unknown id → idempotent no-op.
    };
    if !folder_is_unlocked(state, &folder_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
            crate::errcode::DOC_LOCKED,
            "unlock this folder to delete a document",
        )));
    }
    // `documents(kind='task')` is a hidden crash-recovery source for the dedicated Task lifecycle.
    // Refuse it BEFORE installing a closure or sending a remote revoke: the storage-layer delete
    // guard alone is too late because this command performs irreversible network work first.
    if state.db.get_document(id)?.is_none() {
        return Err(AppError::InvalidArg(
            "task sources must be deleted through the task lifecycle".into(),
        ));
    }
    let _org_mutation = state.lock_org_mutation().await;
    state.db.begin_org_source_closure("document", id)?;
    // REVOKE-BEFORE-DELETE (Bug A root cause): tear down every LIVE org share of this exact source
    // BEFORE the local row disappears, so the background org-sync tick can never re-pull a still-live
    // server item back into the local replica after the user asked to delete it. Fails LOUD: a revoke
    // failure (e.g. offline) aborts the delete rather than silently leaving a dangling live share.
    revoke_org_shares_for_source_notifying(state, None, Some(id), app).await?;
    // The generic surface can also delete a `kind='note'` document. Re-check the gate under the
    // lifecycle mutex after the network await and remove every tracked image while its row still
    // carries retry metadata. Plain imported documents have no attachment owner and skip this leg.
    let _lifecycle = lifecycle_guard(state);
    let Some(folder_id) = state.db.folder_for_document(id)? else {
        return Ok(());
    };
    if !folder_is_unlocked(state, &folder_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
            crate::errcode::DOC_LOCKED,
            "unlock this folder to delete a document",
        )));
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
    bump_seal_epoch(state);
    state.db.delete_document(id)?;
    tracing::info!(target: "documents", document_id = %id, "document deleted");
    Ok(())
}

// ── SMART-NOTE ENGINE (2026-07-25) — turn an ingested document/photo into a readable Obsidian note ──
//
// We already extract flat text on-device (`extract::extract_blocks` → `documents.text`), but ingest
// only ever produced RETRIEVAL text, never a readable note. `generate_note_from_document` takes the
// EXISTING extracted text of a `documents` row and produces a NEW `kind='note'` documents row → `.md`
// through the provider seam, in TWO selectable recipe shapes (see `summarize::recipes::NoteRecipe`):
// SYNTHESIS (`provider.complete()` over one fixed anti-slop template) and STRUCTURE-MIRROR
// (`provider.complete_json()` into a generic opaque-string schema + a DETERMINISTIC Rust renderer).
//
// LOCK-MODEL: the source doc's folder is BOTH the read source and the write target, so ONE gate
// (`folder_is_unlocked`, refuse `AppError::Locked`) covers both — exactly `import_document`'s posture.
// The note is birthed seal-aware (SEAL-THEN-INSERT for a session-unlocked LOCKED folder, mirroring
// `create_note_inner`), then written through the authored-note funnel `update_note_doc_inner`
// (seal-on-write + gated `.md` export + chunk/wikilink re-index). Any embedded source image is
// re-materialized under the NEW note's owner through the existing E2EE bundle seam
// (`materialize_attachment_bundle`, seal-on-birth + verify) — the source's bytes are only READ; its
// AAD is never weakened. The current/last meeting link is gated (a sealed-latest meeting contributes
// neither its title nor an edge). §10: no money/arithmetic — the structure schema is opaque strings.
//
// EGRESS: the note text goes out through the SAME `provider_for(Role::Notes, …)` factory as every
// other note — the consent gate + `RedactingProvider` firewall + egress ledger all live inside it,
// so ONLY REDACTED TEXT egresses. No image bytes are ever sent (there is no multimodal path here).

/// The assembled, ready-to-persist smart note — the output of [`prepare_smart_note`] (the async
/// egress phase) and the input to [`persist_generated_note`] (the sync, off-thread write phase).
#[derive(Debug)]
pub(crate) struct PreparedNote {
    /// The source document's folder (also the new note's folder — one gate covers read + write).
    pub folder_id: String,
    /// The note's display title (drives the `.md` filename + the front-matter `title:`).
    pub title: String,
    /// The complete note markdown: `---` front-matter FIRST, then the H1 + recipe body + (when a
    /// visible last meeting exists) a `[[Title]]` wikilink footer. NO image markers yet — those are
    /// appended in [`persist_generated_note`] once the attachments are materialized under the new id.
    pub markdown: String,
    /// The SOURCE document id (its image attachments, if any, are re-embedded into the note).
    pub source_document_id: String,
    /// The VISIBLE current/last meeting id to link the note to, or `None` (no meeting, or the latest
    /// meeting is sealed-not-unlocked — then neither its title nor an edge surfaces).
    pub meeting_id: Option<String>,
}

/// Filesystem/display TITLE for a smart note: the source file's stem (never a full path — no PII in
/// logs), capped, falling back to a generic label. Deterministic + pure.
fn smart_note_title(source_name: &str) -> String {
    let stem = std::path::Path::new(source_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Smart note");
    stem.chars().take(120).collect()
}

/// Drop a leading YAML front-matter block a synthesis model may have emitted despite the prompt (the
/// command prepends its OWN deterministic front-matter — two blocks would be invalid). A body with no
/// leading `---` is returned trimmed but otherwise unchanged.
fn strip_leading_front_matter(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with("---") {
        let (_yaml, body) = crate::storage::db::split_front_matter(trimmed);
        let body = body.trim();
        if !body.is_empty() {
            return body.to_string();
        }
    }
    trimmed.to_string()
}

/// Provider dispatch for one recipe → the note BODY markdown (no front-matter). SYNTHESIS runs
/// `complete()` over the anti-slop template; STRUCTURE-MIRROR runs `complete_json()` into the generic
/// opaque-string schema, then the DETERMINISTIC Rust renderer. This is the ONLY egress on the path.
async fn build_smart_note_body(
    provider: &dyn crate::summarize::provider::SummarizerProvider,
    recipe: crate::summarize::recipes::NoteRecipe,
    source_name: &str,
    text: &str,
    note_language: &str,
) -> Result<String, AppError> {
    use crate::summarize::recipes::{self, NoteRecipe};
    match recipe {
        NoteRecipe::Synthesis => {
            let (system, user) = recipes::build_synthesis_prompt(source_name, text, note_language);
            let raw = provider.complete(&system, &user).await?;
            let body = strip_leading_front_matter(&raw);
            if body.trim().is_empty() {
                return Err(AppError::Summarize(
                    "the note model returned an empty synthesis".into(),
                ));
            }
            Ok(body)
        }
        NoteRecipe::StructureMirror => {
            let (system, user) = recipes::build_structure_prompt(source_name, text, note_language);
            let schema = recipes::structure_mirror_schema();
            let value = provider.complete_json(&system, &user, &schema).await?;
            let doc: recipes::StructuredDoc = serde_json::from_value(value).map_err(|e| {
                AppError::Summarize(format!(
                    "structure-mirror: invalid JSON shape from provider: {e}"
                ))
            })?;
            Ok(recipes::render_structure_markdown(&doc))
        }
    }
}

/// The async EGRESS phase: gate the source folder, read + clean the extracted text, run the recipe
/// through the provider, and assemble the complete `---`-first note markdown (front-matter + H1 +
/// body + a GATED last-meeting wikilink footer). Returns a [`PreparedNote`] for the sync write phase.
/// Refuses a sealed-not-unlocked folder (`AppError::Locked`) and an empty/unknown source
/// (`AppError::InvalidArg`) BEFORE any provider call.
pub(crate) async fn prepare_smart_note(
    state: &AppState,
    provider: &dyn crate::summarize::provider::SummarizerProvider,
    document_id: &str,
    recipe: crate::summarize::recipes::NoteRecipe,
    note_language: &str,
) -> Result<PreparedNote, AppError> {
    // Read the source row (raw stored text) + resolve its folder.
    let (folder_id, source_name, stored_text) = state
        .db
        .get_document(document_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no document {document_id}")))?;
    // GATE (read + write, one folder): refuse a sealed-and-NOT-session-unlocked folder — the same
    // posture as `import_document`'s write-gate. A blanked (sealed) source has no text to read either.
    if !folder_is_unlocked(state, &folder_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
            crate::errcode::DOC_LOCKED,
            "unlock this folder to make a note from this document",
        )));
    }
    // Clean display text (strips the PR-2 block-structure markers; md/txt/note rows pass through).
    let display_text = crate::extract::render_display_text(&stored_text);
    if display_text.trim().is_empty() {
        return Err(AppError::InvalidArg(crate::errcode::tag(
            crate::errcode::DOC_NO_TEXT,
            "this document has no extractable text to turn into a note",
        )));
    }

    // The current/last meeting — GATED: a sealed-not-unlocked latest meeting contributes neither its
    // title (leak) nor a link. `meeting_is_unlocked` treats a folderless/open meeting as visible.
    let last_meeting = state.db.latest_meeting()?;
    let visible_meeting = match &last_meeting {
        Some(m) if meeting_is_unlocked(state, &m.id)? => Some(m.clone()),
        _ => None,
    };

    // The ONLY egress — redaction firewall applied inside the provider wrapper.
    let body = build_smart_note_body(provider, recipe, &source_name, &display_text, note_language).await?;

    let title = smart_note_title(&source_name);
    let date_iso = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let front =
        crate::summarize::template::smart_note_front_matter(&title, &date_iso, &source_name, recipe.as_str());
    // `---` front-matter FIRST (the load-bearing Obsidian invariant), then the H1 + body.
    let mut markdown = format!("{front}\n# {title}\n\n{body}\n");
    if let Some(meeting_title) = visible_meeting
        .as_ref()
        .and_then(|m| m.title.as_deref())
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        // A `[[wikilink]]` back to the meeting (indexed rename-proof on save); the manual edge is
        // created in the persist phase. Only ever a VISIBLE meeting's title reaches here.
        markdown.push_str(&format!("\n---\n\nGenerated from meeting [[{meeting_title}]].\n"));
    }

    Ok(PreparedNote {
        folder_id,
        title,
        markdown,
        source_document_id: document_id.to_string(),
        meeting_id: visible_meeting.map(|m| m.id),
    })
}

/// Best-effort: re-embed the SOURCE document's image attachments into the NEW note. Reads the source
/// bytes READ-ONLY (never mutates/deletes them) and creates a fresh sealed copy under the NEW note's
/// owner through the existing E2EE bundle seam ([`materialize_attachment_bundle`] — seal-on-birth +
/// verify), so nothing about the source's AAD is weakened. Only normalized WebP/PNG images (the only
/// shapes the seam accepts) are carried; the returned markdown gains a canonical
/// `![](murmur-attachment://<new-id>)` marker per embedded image. No source attachments ⇒ the
/// markdown is returned unchanged.
fn embed_source_attachments(
    state: &AppState,
    source_document_id: &str,
    new_note_id: &str,
    markdown: &str,
) -> Result<String, AppError> {
    let src_owner = crate::storage::AttachmentOwner::Document {
        document_id: source_document_id.to_string(),
    };
    let rows = state.db.list_attachments(&src_owner)?;
    if rows.is_empty() {
        return Ok(markdown.to_string());
    }
    let mut incoming: Vec<crate::storage::IncomingAttachment> = Vec::new();
    let mut markers = String::new();
    for row in rows.iter().take(crate::storage::MAX_ATTACHMENTS_PER_OWNER) {
        // `materialize_attachment_bundle` only accepts a normalized WebP/PNG image; skip anything else.
        if !matches!(
            (row.mime_type.as_str(), row.extension.as_str()),
            ("image/webp", "webp") | ("image/png", "png")
        ) {
            continue;
        }
        let data = plaintext_attachment_data(state, row)?;
        let new_att_id = uuid::Uuid::new_v4().to_string();
        // The canonical marker form parsed by `commands::attachments::parse_attachment_markers`
        // (`murmur-attachment://<UUIDv4>`) — kept in sync with that module's `ATTACHMENT_URI_PREFIX`.
        markers.push_str(&format!("\n\n![](murmur-attachment://{new_att_id})"));
        incoming.push(crate::storage::IncomingAttachment {
            id: new_att_id,
            mime_type: row.mime_type.clone(),
            extension: row.extension.clone(),
            width: row.width,
            height: row.height,
            sha256: row.sha256,
            data,
        });
    }
    if incoming.is_empty() {
        return Ok(markdown.to_string());
    }
    let new_owner = crate::storage::AttachmentOwner::Document {
        document_id: new_note_id.to_string(),
    };
    materialize_attachment_bundle(state, &new_owner, &incoming)?;
    Ok(format!("{markdown}{markers}"))
}

/// The sync WRITE phase (run off the runtime thread behind the heavy permit): birth a seal-aware
/// `kind='note'` documents row in the source's folder, best-effort re-embed the source's images under
/// the new note, write the body through the authored-note funnel `update_note_doc_inner` (seal-on-
/// write + gated `.md` export + chunk/wikilink re-index), and (best-effort, gated) link the note to
/// the current/last meeting. Returns the new note id. Refuses a sealed-not-unlocked folder.
pub(crate) fn persist_generated_note(
    state: &AppState,
    prepared: &PreparedNote,
) -> Result<String, AppError> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp_millis();
    let name = crate::export::sanitize_title(&prepared.title);
    // BIRTH — seal-aware empty note in the source's folder (mirrors `create_note_inner`'s F1/W3
    // SEAL-THEN-INSERT birth-seal: a session-unlocked LOCKED folder gets a verified `text_blob` from
    // birth so there is never a blob-less plaintext row behind the lock; an open folder plain-inserts).
    {
        let _lifecycle = lifecycle_guard(state);
        // Re-check the gate under the lifecycle guard: a relock racing between `prepare_smart_note`
        // and here must refuse, never land plaintext at rest behind a lock.
        if !folder_is_unlocked(state, &prepared.folder_id)? {
            return Err(AppError::Locked(crate::errcode::tag(
                crate::errcode::FOLDER_LOCKED,
                "unlock this folder to add a note",
            )));
        }
        let locked = state
            .db
            .folder_by_id(&prepared.folder_id)?
            .map(|f| f.locked)
            .unwrap_or(false);
        if locked {
            let blob = sealed_document_blob(state, &prepared.folder_id, &id, "")?;
            state
                .db
                .insert_note_sealed(&id, &prepared.folder_id, &name, &prepared.title, "", &blob, now)?;
        } else {
            state
                .db
                .insert_note(&id, &prepared.folder_id, &name, &prepared.title, "", now)?;
        }
        index_wikilinks_best_effort_under_lifecycle(
            state,
            &_lifecycle,
            crate::links::LinkKind::Note,
            &id,
            "",
        );
    }

    // ATTACHMENTS (best-effort — a failure keeps the text note, never loses the untouched source).
    let final_markdown = match embed_source_attachments(state, &prepared.source_document_id, &id, &prepared.markdown) {
        Ok(md) => md,
        Err(e) => {
            tracing::warn!(target: "documents", error = %e, "smart-note: attachment embed skipped (text kept)");
            prepared.markdown.clone()
        }
    };

    // WRITE the body through the authored-note funnel (seal-on-write + gated `.md` export + chunk +
    // wikilink re-index). Re-gates the folder itself — fail-closed on a mid-flight relock.
    update_note_doc_inner(state, &id, &prepared.title, &final_markdown)?;

    // LINK note → current/last meeting (best-effort, gated). A locked/absent meeting silently skips.
    if let Some(meeting_id) = prepared.meeting_id.as_deref() {
        if let Err(e) = link_items_inner(state, "note", &id, "meeting", meeting_id) {
            tracing::warn!(target: "documents", error = %e, "smart-note: meeting link skipped");
        }
    }

    tracing::info!(
        target: "documents",
        note_id = %id,
        folder_id = %prepared.folder_id,
        source_document_id = %prepared.source_document_id,
        "smart note generated from document"
    );
    Ok(id)
}

/// Turn an already-ingested `documents` row into a formatted Obsidian note (`kind='note'` row → `.md`)
/// through the provider seam, in one of two selectable recipe shapes: `"synthesis"` (flagship
/// free-form) or `"structure-mirror"` (deterministic form/table transpile). Returns the NEW note id.
///
/// LOCK: refuses a sealed-not-unlocked folder (`AppError::Locked`). EGRESS: only redacted text leaves
/// the device (the redaction firewall lives inside `provider_for`); no image bytes are ever sent.
#[tauri::command]
pub async fn generate_note_from_document(
    app: AppHandle,
    state: State<'_, AppState>,
    document_id: String,
    recipe: String,
) -> Result<String, AppError> {
    let recipe = crate::summarize::recipes::NoteRecipe::parse(&recipe).ok_or_else(|| {
        AppError::InvalidArg(format!(
            "unknown note recipe {recipe:?} (expected \"synthesis\" or \"structure-mirror\")"
        ))
    })?;
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    // The provider factory owns the consent gate + redaction firewall + egress ledger.
    let provider = crate::summarize::provider_for(
        crate::summarize::roles::Role::Notes,
        &config,
        &state.heavy_inference,
    )?;

    // EGRESS phase (async, on the runtime task) — gate + read + provider + assemble.
    let prepared =
        prepare_smart_note(state.inner(), provider.as_ref(), &document_id, recipe, &config.note_language)
            .await?;

    // WRITE phase off the runtime thread behind the heavy permit (birth-seal + attachment re-seal +
    // note re-embed are Candle/Metal + crypto work — mirror `update_note_doc`'s H3 offload). A bare
    // `&AppState` cannot cross into a `'static` closure, so re-fetch it from the `AppHandle` inside.
    let heavy = state.heavy_inference.clone();
    let app_for_persist = app.clone();
    let new_id = crate::perf::run_heavy(&heavy, move || {
        let state = app_for_persist.state::<AppState>();
        persist_generated_note(&state, &prepared)
    })
    .await?;
    Ok(new_id)
}

#[cfg(test)]
mod smart_note_tests {
    use super::*;
    use crate::settings::AppConfig;
    use crate::storage::models::{Folder, Meeting, MeetingStatus};
    use crate::storage::Db;
    use crate::summarize::provider::{Availability, SummarizeRequest, SummarizerProvider};
    use crate::summarize::recipes::NoteRecipe;
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    const DB_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn tmp_db_path(tag: &str) -> std::path::PathBuf {
        let p = crate::storage::db::unique_temp_path(&format!("murmur-smartnote-{tag}"), "sqlite");
        let _ = std::fs::remove_file(&p);
        p
    }

    /// A minimal [`AppState`] backed by a real temp SQLCipher DB (no Keychain, no Tauri, no vault) —
    /// the same shape `commands/tests` uses. Enough to exercise the gate + note write end-to-end.
    fn build_state(tag: &str) -> AppState {
        let db = Arc::new(Db::open_with_key(&tmp_db_path(tag), DB_KEY).unwrap());
        AppState {
            recorder: Mutex::new(None),
            recording_stop: Mutex::new(None),
            voice_listener: Mutex::new(None),
            voice_listener_lifecycle: Mutex::new(()),
            recording_starting: std::sync::atomic::AtomicBool::new(false),
            voice_command_capture: Mutex::new(None),
            pending_manual_command: Mutex::new(None),
            live_running: std::sync::atomic::AtomicBool::new(false),
            db,
            config: Arc::new(Mutex::new(AppConfig::default())),
            reasoner: crate::reason::ReasonerCell::fixed(Arc::new(crate::reason::StubReasoner)),
            current_meeting: Mutex::new(None),
            focus_meeting: Mutex::new(None),
            live_transcript: Mutex::new(String::new()),
            live_bullets: Mutex::new(String::new()),
            live_bullets_tracker: Mutex::new(crate::transcribe::bullets::BulletsTracker::default()),
            capped_notified: std::sync::atomic::AtomicBool::new(false),
            capture_fault_notified: std::sync::atomic::AtomicBool::new(false),
            reactions_shadow_count: std::sync::atomic::AtomicU64::new(0),
            reactions_emitted: Mutex::new(HashSet::new()),
            in_flight_turns: Mutex::new(std::collections::HashMap::new()),
            user_turn_in_progress: std::sync::atomic::AtomicBool::new(false),
            verify_cache: Mutex::new(std::collections::HashMap::new()),
            unlocked_folders: Arc::new(Mutex::new(HashSet::new())),
            master_kek: Mutex::new(None),
            org_ock_cache: Mutex::new(std::collections::HashMap::new()),
            account_session: Mutex::new(None),
            lifecycle: Mutex::new(()),
            active_salvages: Mutex::new(HashSet::new()),
            share_refresh_lock: tokio::sync::Mutex::new(()),
            org_share_mutation_lock: tokio::sync::Mutex::new(()),
            seal_epoch: std::sync::atomic::AtomicU64::new(0),
            heavy_inference: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    fn seed_folder(db: &Db, id: &str, locked: bool) {
        db.insert_folder(&Folder {
            id: id.to_string(),
            name: id.to_string(),
            path: id.to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-25T00:00:00Z".to_string(),
        })
        .unwrap();
        if locked {
            db.set_folder_locked(id, true, None).unwrap();
        }
    }

    fn seed_document(db: &Db, id: &str, folder_id: &str, name: &str, text: &str) {
        db.insert_document(id, folder_id, name, text, "document", 1_700_000_000_000)
            .unwrap();
    }

    /// A stub provider: `complete` returns a fixed SYNTHESIS body; `complete_json_with_meta` returns a
    /// fixed STRUCTURE-MIRROR payload. No egress, no model — the note plumbing is what's under test.
    struct StubProvider;

    #[async_trait::async_trait]
    impl SummarizerProvider for StubProvider {
        fn id(&self) -> &str {
            "stub"
        }
        async fn availability(&self) -> Availability {
            Availability::Available
        }
        async fn summarize(&self, _req: &SummarizeRequest) -> Result<String, AppError> {
            Ok(String::new())
        }
        async fn complete(&self, _system: &str, _user: &str) -> Result<String, AppError> {
            Ok("## Summary\nA fixed synthesis body.\n\n## Outline\n- point one\n\n## Action items\n- None"
                .to_string())
        }
        async fn complete_json_with_meta(
            &self,
            _system: &str,
            _user: &str,
            _schema: &serde_json::Value,
        ) -> Result<(serde_json::Value, crate::summarize::meta::CallMeta), AppError> {
            Ok((
                serde_json::json!({
                    "fields": [{"key": "Invoice #", "value": "1042"}],
                    "tables": [{"title": "Line items", "rows": [["Item", "Amount"], ["Widget", "$30.00"]]}],
                    "sections": []
                }),
                crate::summarize::meta::CallMeta::default(),
            ))
        }
    }

    /// SYNTHESIS: a fixed extracted-text fixture becomes a valid, FRONT-MATTER-FIRST `.md` note whose
    /// body carries the section skeleton, persisted as a `kind='note'` row.
    #[test]
    fn synthesis_produces_a_front_matter_first_note() {
        let state = build_state("synth");
        seed_folder(&state.db, "f-open", false);
        seed_document(
            &state.db,
            "doc1",
            "f-open",
            "whiteboard.png",
            "Q3 goals: ship v2. Anna owns QA by 2026-08-01.",
        );

        let prepared = block_on(prepare_smart_note(
            &state,
            &StubProvider,
            "doc1",
            NoteRecipe::Synthesis,
            "auto",
        ))
        .expect("prepare must succeed for an open folder");
        assert!(
            prepared.markdown.starts_with("---\n"),
            "note must be front-matter-first: {}",
            prepared.markdown
        );
        assert!(prepared.markdown.contains("recipe: synthesis"));
        assert!(prepared.markdown.contains("## Summary"));
        assert!(prepared.markdown.contains("## Action items"));

        let id = persist_generated_note(&state, &prepared).expect("persist must succeed");
        let row = state.db.get_note_row(&id).unwrap().expect("note row exists");
        assert!(
            row.text.starts_with("---\n"),
            "persisted note text must be front-matter-first: {}",
            row.text
        );
        assert!(row.text.contains("## Summary"));
        // It is a real `kind='note'` row, gate-anchored and folder-anchored to the source folder.
        let anchor = state.db.note_gate_anchor(&id).unwrap().expect("gate anchor");
        assert_eq!(anchor.0, "f-open");
    }

    /// STRUCTURE-MIRROR: the same path renders the opaque-string payload DETERMINISTICALLY into a
    /// front-matter-first `.md` — a valid table + fields, amounts copied verbatim, NEVER summed (§10).
    #[test]
    fn structure_mirror_produces_a_deterministic_front_matter_first_note() {
        let state = build_state("struct");
        seed_folder(&state.db, "f-open", false);
        seed_document(&state.db, "doc1", "f-open", "invoice.pdf", "Invoice 1042. Widget $30.00.");

        let prepared = block_on(prepare_smart_note(
            &state,
            &StubProvider,
            "doc1",
            NoteRecipe::StructureMirror,
            "auto",
        ))
        .expect("prepare must succeed");
        assert!(prepared.markdown.starts_with("---\n"), "{}", prepared.markdown);
        assert!(prepared.markdown.contains("recipe: structure-mirror"));
        assert!(prepared.markdown.contains("## Details"));
        assert!(prepared.markdown.contains("| Item | Amount |"));
        assert!(prepared.markdown.contains("$30.00"));
        assert!(
            !prepared.markdown.contains("$60.00"),
            "structure-mirror must never sum amounts (§10): {}",
            prepared.markdown
        );

        let id = persist_generated_note(&state, &prepared).unwrap();
        let row = state.db.get_note_row(&id).unwrap().expect("note row exists");
        assert!(row.text.starts_with("---\n"));
        assert!(row.text.contains("## Details"));
    }

    /// A visible last meeting is linked (a `[[Title]]` wikilink footer + a manual note→meeting edge).
    #[test]
    fn links_the_note_to_the_visible_last_meeting() {
        let state = build_state("link");
        seed_folder(&state.db, "f-open", false);
        seed_document(&state.db, "doc1", "f-open", "notes.txt", "Some plan for the launch.");
        state
            .db
            .insert_meeting(&Meeting {
                id: "m1".to_string(),
                started_at: "2026-07-24T09:00:00Z".to_string(),
                ended_at: None,
                title: Some("Launch Sync".to_string()),
                duration_s: 60,
                audio_path: None,
                status: MeetingStatus::Summarized,
                folder_id: None,
            })
            .unwrap();

        let prepared = block_on(prepare_smart_note(
            &state,
            &StubProvider,
            "doc1",
            NoteRecipe::Synthesis,
            "auto",
        ))
        .unwrap();
        assert_eq!(prepared.meeting_id.as_deref(), Some("m1"));
        assert!(prepared.markdown.contains("[[Launch Sync]]"));

        let id = persist_generated_note(&state, &prepared).unwrap();
        let unlocked = HashSet::new();
        let links = state
            .db
            .links_for_visible(crate::links::LinkKind::Note, &id, &unlocked)
            .unwrap();
        assert!(
            links
                .iter()
                .any(|e| e.other_kind == "meeting" && e.other_id == "m1"),
            "the note must carry a link to the last meeting"
        );
    }

    /// A sealed-and-NOT-session-unlocked source folder is REFUSED (`AppError::Locked`) BEFORE any
    /// provider call — never read a sealed doc, never land a note behind the lock. RED before the gate.
    #[test]
    fn sealed_folder_is_refused() {
        let state = build_state("sealed");
        seed_folder(&state.db, "f-lock", true); // locked, NOT session-unlocked
        seed_document(&state.db, "doc1", "f-lock", "secret.pdf", "confidential text");

        let err = block_on(prepare_smart_note(
            &state,
            &StubProvider,
            "doc1",
            NoteRecipe::Synthesis,
            "auto",
        ))
        .expect_err("a sealed folder must be refused");
        assert!(
            matches!(err, AppError::Locked(_)),
            "expected AppError::Locked, got {err:?}"
        );
    }

    /// An unknown document id is a clean `InvalidArg` (never a panic / empty note).
    #[test]
    fn unknown_document_is_invalid_arg() {
        let state = build_state("unknown");
        let err = block_on(prepare_smart_note(
            &state,
            &StubProvider,
            "nope",
            NoteRecipe::Synthesis,
            "auto",
        ))
        .expect_err("unknown id must error");
        assert!(matches!(err, AppError::InvalidArg(_)), "got {err:?}");
    }
}
