//! Per-folder LOCK / UNLOCK / RELOCK / REMOVE-LOCK Tauri commands — extracted VERBATIM from
//! `commands` (God-file split, a PURE MOVE — every seal/verify/blank/unseal ORDERING is
//! byte-identical, only relocated). These commands are the verify-before-destroy CALLERS, the
//! highest-stakes lock-model surface in the repo:
//!
//! - `lock_folder` (+ `lock_folder_inner`) / `seal_folder_extras` (STAYS in `mod.rs`): seal each
//!   note/transcript/timeline blob under the folder CK and VERIFY it decrypts back BEFORE
//!   `blank_sealed_notes_in_folders` blanks the plaintext column or the vault `.md` is deleted;
//!   the audio WAV seals via `encrypt_file` (verify-before-destroy inside) and only THEN is the
//!   plaintext WAV removed. Not one character of that ordering changed in this move.
//! - `unlock_folder` / `unlock_meeting`: Touch-ID KEK → unwrap CK → decrypt transcript+timeline
//!   back into the plaintext columns for the session → materialize a playable session WAV → add
//!   the folder id to the session unlock set. The DB is never re-exported.
//! - `relock_folder` / `relock_all` (+ `relock_all_inner`): re-blank the plaintext + drop the
//!   decrypted session WAV; the `.enc` + `*_blob` columns stay untouched.
//! - `remove_lock` (+ `remove_lock_inner`): PERMANENTLY unseal — decrypt every
//!   note/transcript/timeline/audio back to plaintext and re-export the `.md`, never losing audio.
//! - `discard_unrecoverable_folder_lock` / `discard_unrecoverable_meeting_lock` (+ inner): the
//!   escape hatch that PROVES non-recoverability (`discard_proof_complete`) before discarding a
//!   sealed payload whose KEK is gone — never wipes on an unproven key absence.
//!
//! The SHARED seal/unseal/audio/AAD helper web (`seal_folder_extras`, `unseal_folder_extras`,
//! `unseal_folder_extras_permanent`, `reblank_folder_extras`, `seal_meeting_extras`, the `aad_*`
//! family, `StreamRole`, `meeting_is_unlocked`, `folder_is_unlocked`, `unlocked_snapshot`,
//! `lifecycle_guard`, `bump_seal_epoch`, `session_folder_ck`, `assert_in_vault`, `vault_path`,
//! `rederive_links_for_folder`, `remove_rollup_export_files`, `emit_audit_updated_after_purge`)
//! all STAY in `commands/mod.rs` — several are also called by the retained `move_note`/
//! `delete_folder`/`delete_meeting` clusters — and are reached here through `use super::*`
//! (the private ones promoted to `pub(crate)`, bodies byte-identical). Bound in `mod.rs` as
//! `lock_commands` via `#[path]`, glob-re-exported so every command resolves UNCHANGED at
//! `crate::commands::…` for `generate_handler!` and every caller.

use super::*;

/// SEAL a folder: generate a content key, KEK-wrap it, encrypt every governed note's markdown
/// into `content_blob`, then (after a DB commit) blank the markdown + delete the vault `.md`.
/// Atomicity: each note's blob is verified decryptable BEFORE we blank/delete; a crash after the
/// DB write but before the `.md` delete leaves a stale plaintext `.md` (reconcilable) — never
/// lost content.
#[tauri::command]
pub fn lock_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    folder_id: String,
) -> Result<(), AppError> {
    lock_folder_inner(state.inner(), folder_id)?;
    // The seal purged ALL pending audit findings — ping the FE inbox (count-only).
    emit_audit_updated_after_purge(&app, state.inner());
    Ok(())
}

/// Inner of [`lock_folder`] taking `&AppState` (so the lifecycle stress test can drive it without a
/// `tauri::State`). Holds the [`AppState::lifecycle`] guard for the whole seal.
pub(crate) fn lock_folder_inner(state: &AppState, folder_id: String) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    // The reserved Notes root is the always-open home for unfiled notes — it can NEVER be sealed
    // (unfiled notes are deliberately plaintext; sealing requires filing into a lockable folder). Refuse
    // rather than silently no-op so the FE can guide the user (2026-07-14).
    if state.db.folder_is_root(&folder_id)? {
        return Err(AppError::InvalidArg(
            "the Notes root can't be locked — move notes into a folder to seal them".into(),
        ));
    }
    if folder.locked {
        return Ok(()); // already sealed — idempotent.
    }
    // R7: advance the seal epoch at ENTRY (before any seal work) — see `bump_seal_epoch`.
    bump_seal_epoch(state);

    // Prefer the SESSION-CACHED KEK (set by a successful unlock — possibly a RECOVERED key): it
    // keeps every folder sealed this session convergent on the key that demonstrably unwraps the
    // existing ones, and skips a redundant Touch ID prompt. Only fall through to the keychain when
    // nothing is cached. Minting a fresh KEK is then allowed ONLY when nothing is sealed yet: with
    // sealed folders present, a missing keychain item must be an ERROR (a fresh KEK would fork the
    // key the folders depend on — the 2026-07-05 field incident sealed folders under divergent
    // mints).
    let cached: Option<Zeroizing<[u8; 32]>> = {
        let g = state
            .master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        g.clone()
    };
    let kek = match cached {
        Some(k) => k,
        None => {
            let any_sealed = state.db.any_locked_folder()?;
            Zeroizing::new(crate::secrets::master_kek_with_policy(
                "Lock this folder",
                !any_sealed,
            )?)
        }
    };
    let ck = Zeroizing::new(crate::crypto::random_key()?);
    // Wrapped CK is AAD-bound to the folder id (B7): the wrapped key cannot be lifted onto a
    // different folder row and unwrapped there.
    let wrapped = crate::crypto::encrypt(&kek, &*ck, &aad_wrapped_ck(&folder_id))?;

    // Gather the notes to seal. A meeting may have MULTIPLE provider rows (e.g. re-summarized
    // with ollama then anthropic) each with DISTINCT markdown — seal EVERY (meeting, provider)
    // row into its OWN blob. Collapsing to one blob per meeting would destroy every provider's
    // content but the first (the PRIME-DIRECTIVE content-loss bug this guards against).
    let notes = state.db.notes_in_folder(&folder_id)?;
    let mut sealed_rows: Vec<(String, String, Vec<u8>)> = Vec::new();
    for n in &notes {
        // Encrypt this row's markdown bound to (folder|meeting|provider|note|v) and VERIFY it
        // reads back before we touch the plaintext.
        let aad = aad_content(&folder_id, &n.meeting_id, &n.provider_id, "note");
        let blob = crate::crypto::encrypt(&ck, n.markdown.as_bytes(), &aad)?;
        let check = crate::crypto::decrypt(&ck, &blob, &aad)?;
        if check != n.markdown.as_bytes() {
            return Err(AppError::Storage(
                "seal verification failed (decrypted blob mismatch)".into(),
            ));
        }
        sealed_rows.push((n.meeting_id.clone(), n.provider_id.clone(), blob));
    }

    // Capture every governed note's .md path BEFORE any seal_note nulls exported_path.
    let exported_paths: Vec<String> = notes
        .iter()
        .filter_map(|n| n.exported_path.clone())
        .collect();

    // Persist: mark the folder locked (+ wrapped key) and write every sealed blob per provider
    // row (markdown blanked, exported_path cleared). Each write is guarded by the verification
    // above, so a crash mid-loop leaves already-sealed rows recoverable and not-yet-sealed rows
    // with intact plaintext — never lost content.
    state
        .db
        .set_folder_locked(&folder_id, true, Some(&wrapped))?;
    for (meeting_id, provider_id, blob) in &sealed_rows {
        state.db.seal_note(meeting_id, provider_id, blob)?;
    }

    // Phase 0.5 — seal the TRANSCRIPT + TIMELINE (defense-in-depth in the OPEN db) and the AUDIO
    // WAV at rest, all under the SAME folder CK. Verify-before-destroy inside (no transcript /
    // audio loss). Done after the note seal so a partial-seal crash still leaves recoverable blobs.
    seal_folder_extras(state, &folder_id, &ck)?;
    drop(kek); // explicit: KEK zeroized when this Zeroizing drops here.
    drop(ck); // explicit: CK zeroized after sealing all extras.

    // Phase 2a LOCK-SAFETY: purge plaintext-derived semantic chunks + their (invertible) vectors
    // for every meeting now sealed in this folder — a vector is PII derived from the plaintext, so
    // it must not survive at rest in a locked folder. Done AFTER the seal so the index is dropped
    // only once the recoverable blobs exist. Re-index-on-unlock is a separate later step; until it
    // lands a locked-then-unlocked folder is simply not semantically searchable (degraded, not
    // leaky).
    let sealed_meeting_ids = state.db.meeting_ids_in_folder(&folder_id)?;
    let sealed_document_ids = state.db.document_ids_in_folder(&folder_id)?;
    // Brain-v3 audit Fix 4: BEFORE the link-row purge deletes the edges that name each affected
    // source→sealed-item pair, strip the just-sealed items' `[[Title]]` markers from every VISIBLE
    // source note's MACHINE-OWNED managed block (the DB plaintext), and re-export those sources' `.md`
    // so neither the DB nor the vault file names the now-sealed neighbour. The sealed items' titles are
    // still readable here (seal blanks body content, never the title). Re-materialized on unlock from
    // the preserved accepted rows. Runs BEFORE the purges below (which drop the naming edges).
    match state
        .db
        .strip_sealed_neighbour_markers(&sealed_meeting_ids, &sealed_document_ids)
    {
        Ok(changed) => reexport_stripped_marker_sources(state, &changed),
        Err(e) => tracing::warn!(target: "links", error = %e, "sealed-neighbour marker strip failed"),
    }
    // The purge tx also drops ALL memory rollups (cross-meeting synthesis that may paraphrase the
    // just-sealed facts; regenerated from visible facts on the next hourly pass) and returns their
    // exported vault paths — deleted below alongside the sealed notes' `.md`s.
    let rollup_exports = state.db.purge_chunks_for_meetings(&sealed_meeting_ids)?;
    remove_rollup_export_files(&rollup_exports);
    // Document ingestion LOCK-SAFETY: purge the (now-sealed) documents' plaintext-derived chunks +
    // their invertible vectors too — a doc vector is PII derived from the plaintext, so it must not
    // survive at rest in a locked folder. Re-embeddable on unlock (the text seal is restorable).
    state
        .db
        .purge_doc_chunks_for_documents(&sealed_document_ids)?;

    // AFTER the column writes, delete the vault `.md` files (a leftover .md is reconcilable;
    // lost content is not — so this is last).
    // Export-collision guard: the seal delete deliberately does NOT preserve an external-edit
    // sibling — privacy wins over preservation. A plaintext sibling of a to-be-sealed note would
    // be exactly the leak the lock exists to prevent. External edits to a locked note's `.md`
    // are accepted-loss by design; the DB blob remains the canonical recoverable copy.
    // Phase-0 follow-up: siblings ALREADY preserved by an earlier overwrite are user-authored
    // files we never delete — but they are plaintext that survives this seal on disk, so WARN
    // (counts only) so the exposure is at least visible in the log.
    let doc_export_rows = state.db.note_exported_path_rows_in_folder(&folder_id)?;
    warn_external_edit_siblings(
        "lock",
        exported_paths
            .iter()
            .chain(doc_export_rows.iter().map(|(_, p)| p)),
    );
    for p in exported_paths {
        let _ = std::fs::remove_file(&p);
    }

    // NOTES: an authored note's markdown is sealed by the (kind-agnostic) document seal leg in
    // `seal_folder_extras` above — but its vault `.md` (a note-only concern; documents have no
    // `exported_path`) must be deleted on lock exactly like a meeting note's `.md`, so a sealed
    // note leaves no plaintext on disk. PER ROW (residual W5, extended to the INITIAL seal by the
    // R2 hardening, 2026-07-10): each row's `exported_path` is cleared ONLY after its `.md` was
    // actually deleted (or is already absent) — a FAILED delete keeps that row's path recorded so
    // the next relock/startup pass retries the file (the pre-fix bulk clear forgot the leaked
    // `.md` forever). The lock itself still completes (the DB blob is the recoverable copy).
    // Count-only log — never paths (they embed note titles).
    for (doc_id, p) in doc_export_rows {
        let removed = match std::fs::remove_file(&p) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => {
                tracing::warn!(
                    target: "lock",
                    error = %e,
                    "lock: deleting a note .md failed — keeping exported_path for retry"
                );
                false
            }
        };
        if removed {
            state.db.set_note_doc_exported_path(&doc_id, None)?;
        }
    }

    // Belt-and-braces RAM hygiene: with no recording active, drop any stale live-caption buffer at
    // the moment a folder seals (post clear-on-Stop it is normally already empty; idempotent).
    clear_stale_live_transcript(state);
    Ok(())
}

/// SESSION-unlock a sealed folder: KEK → unwrap CK → decrypt each note's `content_blob` back into
/// the plaintext markdown column for the session, and add the folder id to the session unlock set.
/// Does NOT re-export to the vault. Returns the refreshed folder node.
#[tauri::command]
pub async fn unlock_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    folder_id: String,
) -> Result<FolderNode, AppError> {
    // v0.3.2 — the master KEK is a BIOMETRIC-GATED keychain item. Reading it makes macOS present the
    // Touch ID / passcode sheet directly (with our reason string) and hand back the key — THAT single
    // sheet IS the unlock auth, so there is no separate app-side authentication step (which would
    // double-prompt: Touch ID, then a keychain-password dialog). Result: exactly ONE Touch ID prompt,
    // no "app wants to use keychain, enter password" dialog, no "Always Allow".
    //
    // The `lock_require_biometric` preference (K_LOCK_REQUIRE_BIOMETRIC, default true) is INFORMATIONAL
    // only: the biometric requirement is enforced by the keychain item's kSecAttrAccessControl (an
    // OS-level gate), not by any app-side `if`. An app boolean cannot waive the OS access control —
    // even with the flag false, reading the gated item still presents the system sheet. It is NOT read
    // here precisely because it cannot change this code path; it is surfaced in settings so the user
    // can see the guarantee, and is retained on the config DTO for forward-compat.

    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    if !folder.locked {
        return Err(AppError::InvalidArg("folder is not locked".into()));
    }
    let wrapped = state
        .db
        .folder_wrapped_key(&folder_id)?
        .ok_or_else(|| AppError::Storage("locked folder has no wrapped key".into()))?;

    // Reuse the KEK cached from an earlier unlock in this session so repeated unlocks do NOT
    // re-prompt for Touch ID (the cache is zeroized on relock-all). Only fall through to the
    // biometric-gated keychain read — the single Touch ID prompt — when nothing is cached.
    let kek: Zeroizing<[u8; 32]> = {
        let cached: Option<Zeroizing<[u8; 32]>> = {
            let g = state
                .master_kek
                .lock()
                .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
            g.clone()
        };
        match cached {
            Some(k) => k,
            None => {
                // The biometric-gated keychain read BLOCKS while the Touch ID sheet is up, so run it
                // on the blocking pool — never on an async-runtime worker thread. This is the single
                // Touch ID prompt. `allow_mint = false`: a locked folder EXISTS (we are unlocking
                // it), so a missing keychain item must NEVER be papered over with a fresh KEK — that
                // orphans every sealed folder (2026-07-05 field incident).
                let resolved = tokio::task::spawn_blocking(|| {
                    crate::secrets::master_kek_with_policy("Unlock this folder", false)
                })
                .await
                .map_err(|e| AppError::Auth(format!("master-kek task join failed: {e}")))?;
                match resolved {
                    Ok(bytes) => Zeroizing::new(bytes),
                    Err(resolve_err) => {
                        // LAST RESORT: even the primary release failed (e.g. an authoritatively
                        // missing item — or a read shape that lies on this macOS). Enumerate every
                        // candidate the stores hold and try each against THIS folder's wrapped CK;
                        // a winner proceeds exactly like a released KEK. Read-only.
                        tracing::warn!(
                            target: "lock",
                            folder = %folder_id,
                            error = %resolve_err,
                            "unlock_folder: master-KEK release failed — trying candidate recovery"
                        );
                        let candidates = tokio::task::spawn_blocking(|| {
                            crate::secrets::list_master_kek_candidates("Recover the folder key")
                        })
                        .await
                        .map_err(|e| AppError::Auth(format!("kek-recovery task join failed: {e}")))?
                        .unwrap_or_else(|_| Zeroizing::new(Vec::new()));
                        match try_unwrap_ck_with_candidates(&candidates, &wrapped, &folder_id, None)
                        {
                            Some((_bytes, winner, idx)) => {
                                tracing::warn!(
                                    target: "lock",
                                    folder = %folder_id,
                                    candidates = candidates.len(),
                                    winner_index = idx,
                                    "unlock_folder: RECOVERED the master KEK from the candidate set (primary release had failed)"
                                );
                                winner
                            }
                            None => return Err(resolve_err),
                        }
                    }
                }
            }
        }
    };
    // Wrapped CK is bound to the folder id (legacy folders fall back to empty AAD transparently).
    // A failure HERE with a successfully-released KEK means the CK was wrapped under a DIFFERENT
    // KEK than the one just read (store divergence / replaced item). RECOVERY: on machines where
    // the no-UI keychain probe lied, several KEK generations can coexist — enumerate every
    // candidate in the stores and try each against the wrapped CK before giving up. Read-only;
    // the winning KEK is adopted for the session (cached below) but nothing is rewritten.
    let (ck_bytes, kek) = match crate::crypto::decrypt(&kek, &wrapped, &aad_wrapped_ck(&folder_id))
    {
        Ok(b) => (Zeroizing::new(b), kek),
        Err(primary_err) => {
            tracing::warn!(
                target: "lock",
                folder = %folder_id,
                error = %primary_err,
                "unlock_folder: content-key unwrap failed with the primary master KEK — trying every keychain candidate"
            );
            let candidates = tokio::task::spawn_blocking(|| {
                crate::secrets::list_master_kek_candidates("Recover the folder key")
            })
            .await
            .map_err(|e| AppError::Auth(format!("kek-recovery task join failed: {e}")))?
            .unwrap_or_else(|_| Zeroizing::new(Vec::new()));
            match try_unwrap_ck_with_candidates(&candidates, &wrapped, &folder_id, Some(&*kek)) {
                Some((bytes, winner, idx)) => {
                    tracing::warn!(
                        target: "lock",
                        folder = %folder_id,
                        candidates = candidates.len(),
                        winner_index = idx,
                        "unlock_folder: RECOVERED the content key with a non-primary master-KEK candidate"
                    );
                    (Zeroizing::new(bytes), winner)
                }
                None => {
                    tracing::error!(
                        target: "lock",
                        folder = %folder_id,
                        candidates = candidates.len(),
                        "unlock_folder: NO keychain candidate unwraps this folder's content key"
                    );
                    return Err(primary_err);
                }
            }
        }
    };
    let ck: Zeroizing<[u8; 32]> = Zeroizing::new(
        ck_bytes
            .as_slice()
            .try_into()
            .map_err(|_| AppError::Storage("unwrapped content key has wrong length".into()))?,
    );

    // The rest of the restore (decrypt every note/segment/timeline blob, materialize the session
    // WAV, re-embed the folder's meetings) is synchronous CPU/AES/Candle-Metal work that used to
    // run INLINE on this async command's Tokio worker — for a folder with several long-meeting
    // recordings this can stall the whole IPC layer for seconds to low minutes (2026-07-13 perf
    // audit finding, HIGH severity). Moved to the blocking pool, routed through the shared
    // heavy-inference gate (perf::run_heavy) since it re-embeds via Candle/Metal. The `AppHandle`
    // re-fetch pattern (`app.state::<AppState>()`) is how this codebase already gets a `'static`
    // handle to `AppState` inside a spawn_blocking closure (see `ask_vault_agentic_attempt`) — a
    // bare `&AppState` from `State<'_, AppState>` cannot be captured by a `'static` closure.
    let heavy_inference = state.heavy_inference.clone();
    let app_for_restore = app.clone();
    let folder_for_restore = folder.clone();
    let folder_id_for_restore = folder_id.clone();
    let restored: FolderNode = crate::perf::run_heavy(&heavy_inference, move || -> Result<FolderNode, AppError> {
        let state = app_for_restore.state::<AppState>();
        let folder = folder_for_restore;
        let folder_id = folder_id_for_restore;

        // BLK-1: from here on we MUTATE plaintext columns (restore markdown / segments /
        // timeline). Acquire the lifecycle guard for the whole synchronous restore so a
        // concurrent `relock_all_inner` (screen-share / lifecycle) cannot blank these rows
        // mid-restore. This closure runs entirely on the blocking pool with no `.await` inside
        // it, so the guard never crosses a suspend point (same invariant the old inline code
        // relied on, preserved by construction — a spawn_blocking closure body is plain sync code).
        let _lifecycle = lifecycle_guard(&state);

        // Decrypt EACH sealed provider row's own blob back into its own markdown column for the
        // session (no dedup by meeting — every provider's distinct content is restored
        // independently). Bound to (folder|meeting|provider|note); legacy blobs fall back to
        // empty AAD.
        let notes = state.db.notes_in_folder(&folder_id)?;
        for n in &notes {
            let Some(blob) = &n.content_blob else {
                continue; // open note (shouldn't happen in a sealed folder) — skip.
            };
            let aad = aad_content(&folder_id, &n.meeting_id, &n.provider_id, "note");
            let pt = crate::crypto::decrypt(&ck, blob, &aad)?;
            let markdown = String::from_utf8(pt)
                .map_err(|_| AppError::Storage("decrypted note is not valid UTF-8".into()))?;
            state
                .db
                .restore_note_markdown(&n.meeting_id, &n.provider_id, &markdown)?;
        }

        // Phase 0.5 — decrypt the TRANSCRIPT + TIMELINE back into their plaintext columns and
        // materialize a playable WAV (decrypt .enc → file) for the session, under the SAME CK.
        // The model-gated meeting embedder (Some only when the REAL e5 model is present → never
        // stub vectors) re-indexes the folder's meetings so semantic / related-meetings recover
        // in-session.
        let meeting_embedder =
            crate::embed::embed_model_present().then(crate::embed::active_embedder);
        unseal_folder_extras(&state, &folder_id, &ck, meeting_embedder.as_deref())?;

        // Cache the KEK for the session (zeroized on relock-all + on drop) + add to the unlock set.
        {
            let mut g = state
                .master_kek
                .lock()
                .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
            *g = Some(kek.clone());
        }
        {
            let mut g = state
                .unlocked_folders
                .lock()
                .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
            g.insert(folder_id.clone());
        }
        tracing::info!(target: "lock", folder = %folder_id, "unlock_folder: session unlock complete");

        // Return the refreshed node. This folder was just added to the unlock set above, so its
        // own count is legitimately visible — pass the current unlock set through the same gate
        // every other caller uses (count_notes_per_folder).
        let unlocked = {
            state
                .unlocked_folders
                .lock()
                .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
                .clone()
        };
        // Brain v3 PR-3 — RE-DERIVE the LINK ENGINE for the just-unsealed items: their `links` rows
        // were purged on seal, so re-run the wikilink pass (deterministic) + the semantic pass
        // (model-gated inside) for every meeting + note in this folder, using the now-updated unlock
        // set. Best-effort — a re-derive failure never fails the unlock (the content is restored).
        rederive_links_for_folder(&state, &folder_id, &unlocked);
        let counts = state.db.count_notes_per_folder(&unlocked)?;
        let kind = state
            .db
            .folder_kind(&folder.id)?
            .unwrap_or_else(|| "meeting".to_string());
        Ok(FolderNode {
            id: folder.id.clone(),
            name: folder.name.clone(),
            parent_id: folder.parent_id.clone(),
            note_count: counts.get(&folder.id).copied().unwrap_or(0),
            locked: true,
            unlocked: unlocked.contains(&folder.id),
            kind,
            children: Vec::new(),
        })
    })
    .await?;
    Ok(restored)
}

/// Re-seal a session-unlocked folder for the rest of this session: re-blank the plaintext
/// markdown of its sealed notes and drop the folder from the unlock set. The `content_blob`
/// stays — the folder is still `locked=1` on disk.
#[tauri::command]
pub fn relock_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    folder_id: String,
) -> Result<(), AppError> {
    // BLK-1: serialize with the rest of the lock state machine (it re-blanks the same columns
    // `remove_lock` is mid-restoring).
    let _lifecycle = lifecycle_guard(state.inner());
    // R7: advance the seal epoch at ENTRY — see `bump_seal_epoch`.
    bump_seal_epoch(state.inner());
    {
        let mut g = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        g.remove(&folder_id);
    }
    // Brain v2 L5 — drop the SESSION verify cache on relock: cached findings paraphrase live
    // connector values about note lines and must not outlive the session unlock. Cleared WHOLE
    // (conservative — a per-folder filter would need a meeting→folder walk for no security gain).
    // A poisoned lock only skips the clear-by-mutex; the cache is RAM-only either way.
    if let Ok(mut cache) = state.verify_cache.lock() {
        cache.clear();
    }
    let mut one = std::collections::HashSet::new();
    one.insert(folder_id.clone());
    // Brain-v3 audit Fix 4: strip the just-re-sealed items' `[[Title]]` markers from VISIBLE sources
    // (in other still-open folders) + re-export their `.md`, BEFORE the relock purge drops the naming
    // edges.
    strip_and_reexport_markers_for_folders(state.inner(), &one);
    // The re-blank tx also purges ALL memory rollups (may paraphrase the re-sealed facts) and
    // returns their exported vault paths — the files are removed here (command layer).
    let rollup_exports = state.db.blank_sealed_notes_in_folders(&one)?;
    remove_rollup_export_files(&rollup_exports);
    // Phase 0.5 — re-blank the transcript + timeline plaintext and drop the decrypted session WAV
    // (the .enc + the *_blob columns stay; the folder is still locked=1 on disk).
    reblank_folder_extras(state.inner(), &folder_id)?;
    // The relock purged ALL pending audit findings — ping the FE inbox (count-only).
    emit_audit_updated_after_purge(&app, state.inner());
    Ok(())
}

/// Relock ALL session-unlocked folders + zeroize the cached KEK (called on screen-share start in
/// Stage E, and exposed as a command). Re-blanks the plaintext markdown of every sealed note.
#[tauri::command]
pub fn relock_all(app: AppHandle, state: State<'_, AppState>) -> Result<(), AppError> {
    relock_all_inner(&state)?;
    // The relock-all purged ALL pending audit findings — ping the FE inbox (count-only). The
    // off-thread `relock_all_inner` callers emit from their own handles (screen-share) or are
    // app teardown (window-close/exit — nothing left to notify).
    emit_audit_updated_after_purge(&app, state.inner());
    Ok(())
}

/// Inner relock-all usable without a command boundary (Stage E screen-share watcher, window-close,
/// app-exit). BLK-1: this is the OFF-THREAD blanker that races `remove_lock`; it acquires the
/// [`AppState::lifecycle`] guard FIRST so its re-blank can never land between `remove_lock`'s
/// restore-plaintext (Step 1) and clear-`content_blob` (Step 2). All three off-thread callers and
/// the `relock_all` command funnel through here, so the guard lives HERE (the `relock_all` command
/// must NOT take it separately — a std `Mutex` is non-reentrant and would self-deadlock).
pub(crate) fn relock_all_inner(state: &AppState) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    // R7: advance the seal epoch at ENTRY — see `bump_seal_epoch`.
    bump_seal_epoch(state);
    // Clear the session set.
    {
        let mut g = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        g.clear();
    }
    // Brain v2 L5 — drop the SESSION verify cache on relock-all (screen-share auto-relock,
    // window-close, app-exit, manual "Lock all"): cached findings paraphrase note lines and must
    // not outlive the session unlock. Best-effort on a poisoned lock (RAM-only cache).
    if let Ok(mut cache) = state.verify_cache.lock() {
        cache.clear();
    }
    // Zeroize the cached KEK copy (C5: use zeroize::Zeroize, not a hand byte-loop the optimizer
    // could elide — `Zeroize::zeroize` is a guaranteed, non-elidable wipe). Taking the `Zeroizing`
    // out and dropping it ALSO wipes it; the explicit call makes the intent unmistakable.
    {
        let mut g = state
            .master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        if let Some(mut k) = g.take() {
            k.zeroize();
        }
    }
    // Re-blank every sealed note across all locked folders. The tx also purges ALL memory rollups
    // (may paraphrase the re-sealed facts) — their exported vault `.md`s are removed here.
    let locked: std::collections::HashSet<String> =
        state.db.locked_folder_ids()?.into_iter().collect();
    // Brain-v3 audit Fix 4: BEFORE the relock purge drops the naming edges, strip the just-re-sealed
    // items' `[[Title]]` markers from every VISIBLE source note (a source in a DIFFERENT still-open
    // folder that was materialized while this folder was session-unlocked) + re-export those sources'
    // `.md`. Resolve the relocked folders' meeting + document ids first.
    strip_and_reexport_markers_for_folders(state, &locked);
    let rollup_exports = state.db.blank_sealed_notes_in_folders(&locked)?;
    remove_rollup_export_files(&rollup_exports);
    // Phase 0.5 — re-blank the transcript + timeline + drop the decrypted session WAVs for every
    // locked folder too (the .enc + *_blob columns stay).
    for fid in &locked {
        reblank_folder_extras(state, fid)?;
    }
    // B12: checkpoint + truncate the WAL so the just-re-blanked plaintext does not linger in the
    // sidecar. Best-effort — a busy checkpoint is logged, not fatal to the relock.
    if let Err(e) = state.db.checkpoint_truncate() {
        tracing::warn!(target: "lock", error = %e, "wal_checkpoint(TRUNCATE) on relock_all failed");
    }
    // Belt-and-braces RAM hygiene: with no recording active, drop any stale live-caption buffer on
    // relock-all (manual "Lock all", screen-share auto-relock, window-close, app-exit). Never
    // clears mid-recording — the in-flight buffer stays, gated by visibility at injection time.
    clear_stale_live_transcript(state);
    Ok(())
}

/// PERMANENTLY remove a folder's lock: KEK → unwrap CK → decrypt each note back to plaintext
/// markdown, clear `content_blob`, set `locked=0` + `wrapped_key=NULL`, and re-export each note's
/// `.md` to the vault. The folder returns to the default OPEN state.
#[tauri::command]
pub fn remove_lock(state: State<'_, AppState>, folder_id: String) -> Result<(), AppError> {
    remove_lock_inner(state.inner(), folder_id)
}

/// Inner of [`remove_lock`] taking `&AppState` (so the BLK-1 lifecycle stress test can drive it
/// without a `tauri::State`). BLK-1: holds the [`AppState::lifecycle`] guard across the ENTIRE
/// restore→clear sequence (Step 1 decrypt-plaintext-into-`markdown`, Step 2 clear `content_blob`),
/// so the off-thread `relock_all_inner` blanker can never blank `markdown` to `''` in the window
/// between the two steps — the exact `markdown='' + content_blob=NULL` permanent-loss race.
pub(crate) fn remove_lock_inner(state: &AppState, folder_id: String) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    if !folder.locked {
        return Ok(()); // already open — idempotent.
    }
    // R7: advance the seal epoch at ENTRY — remove-lock rewrites the same lock-surface columns
    // the consolidation pass must not interleave with. See `bump_seal_epoch`.
    bump_seal_epoch(state);
    let wrapped = state
        .db
        .folder_wrapped_key(&folder_id)?
        .ok_or_else(|| AppError::Storage("locked folder has no wrapped key".into()))?;
    // Prefer the session-cached KEK (possibly a RECOVERED key), then the keychain with the strict
    // no-mint policy (this folder IS sealed — a fresh mint can never unwrap it). On an unwrap
    // failure, run the same candidate RECOVERY as `unlock_folder` before giving up.
    let cached: Option<Zeroizing<[u8; 32]>> = {
        let g = state
            .master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        g.clone()
    };
    let kek = match cached {
        Some(k) => k,
        None => Zeroizing::new(crate::secrets::master_kek_with_policy(
            "Remove this folder's lock",
            false,
        )?),
    };
    let ck_bytes = match crate::crypto::decrypt(&kek, &wrapped, &aad_wrapped_ck(&folder_id)) {
        Ok(b) => Zeroizing::new(b),
        Err(primary_err) => {
            tracing::warn!(
                target: "lock",
                folder = %folder_id,
                error = %primary_err,
                "remove_lock: content-key unwrap failed with the primary master KEK — trying every keychain candidate"
            );
            let candidates = crate::secrets::list_master_kek_candidates("Recover the folder key")
                .unwrap_or_else(|_| Zeroizing::new(Vec::new()));
            match try_unwrap_ck_with_candidates(&candidates, &wrapped, &folder_id, Some(&*kek)) {
                Some((bytes, winner, idx)) => {
                    tracing::warn!(
                        target: "lock",
                        folder = %folder_id,
                        candidates = candidates.len(),
                        winner_index = idx,
                        "remove_lock: RECOVERED the content key with a non-primary master-KEK candidate"
                    );
                    // Cache the winner so subsequent lock ops this session converge on the key
                    // that demonstrably unwraps existing folders (and skip re-enumeration).
                    if let Ok(mut g) = state.master_kek.lock() {
                        *g = Some(winner);
                    }
                    Zeroizing::new(bytes)
                }
                None => {
                    tracing::error!(
                        target: "lock",
                        folder = %folder_id,
                        candidates = candidates.len(),
                        "remove_lock: NO keychain candidate unwraps this folder's content key"
                    );
                    return Err(primary_err);
                }
            }
        }
    };
    let ck: Zeroizing<[u8; 32]> = Zeroizing::new(
        ck_bytes
            .as_slice()
            .try_into()
            .map_err(|_| AppError::Storage("unwrapped content key has wrong length".into()))?,
    );

    let vault = vault_path(state);
    let notes = state.db.notes_in_folder(&folder_id)?;

    // Step 1: restore EVERY provider row's plaintext from ITS OWN blob (or keep the in-memory
    // markdown if the folder is session-unlocked and the blob is absent). This must happen for
    // every row BEFORE any blob is cleared — otherwise a sibling provider's content is lost.
    for n in &notes {
        let markdown = if let Some(blob) = &n.content_blob {
            let aad = aad_content(&folder_id, &n.meeting_id, &n.provider_id, "note");
            let pt = crate::crypto::decrypt(&ck, blob, &aad)?;
            String::from_utf8(pt)
                .map_err(|_| AppError::Storage("decrypted note is not valid UTF-8".into()))?
        } else {
            n.markdown.clone()
        };
        state
            .db
            .restore_note_markdown(&n.meeting_id, &n.provider_id, &markdown)?;
    }

    // Step 2: per meeting, clear the blobs (all rows now hold plaintext) and re-export ONE `.md`
    // (the latest provider's note — matching how the rest of the app treats "the note" for a
    // meeting). All provider rows for that meeting share the re-exported path.
    let mut seen = std::collections::HashSet::new();
    for n in &notes {
        if !seen.insert(n.meeting_id.clone()) {
            continue;
        }
        state.db.clear_note_content_blob(&n.meeting_id)?;

        let Some(vault) = vault.as_deref() else {
            continue;
        };
        let latest = match state.db.get_latest_note_for_meeting(&n.meeting_id)? {
            Some(l) => l,
            None => continue,
        };
        let meeting = state.db.get_meeting(&n.meeting_id)?;
        let (title, date) = match meeting {
            Some(m) => (
                m.title.clone().unwrap_or_else(|| "Untitled".into()),
                m.started_at.clone(),
            ),
            None => ("Untitled".to_string(), chrono::Utc::now().to_rfc3339()),
        };
        let sub = if folder.path.is_empty() {
            None
        } else {
            Some(folder.path.as_str())
        };
        if let Ok(path) = crate::export::write_note(
            std::path::Path::new(vault),
            sub,
            &title,
            &date,
            &latest.markdown,
        ) {
            state.db.set_note_exported_path(
                &n.meeting_id,
                &latest.provider_id,
                &path.to_string_lossy(),
            )?;
            // Export-collision guard: the pre-lock baseline is stale (the `.md` was deleted on
            // seal) — re-stamp it FRESH from the markdown this re-export just wrote. A file that
            // already existed at the target with different content was collision-suffixed by
            // `write_note`, never overwritten, so the file at `path` equals `latest.markdown`.
            state.db.set_note_exported_hash(
                &n.meeting_id,
                &latest.provider_id,
                Some(&crate::export::note_content_hash(&latest.markdown)),
            )?;
        }
    }

    // Phase 0.5 — permanently restore the TRANSCRIPT + TIMELINE plaintext (clear *_blob columns)
    // and the AUDIO WAV (decrypt .enc → file, drop .enc) under the SAME CK. Never lose audio. The
    // model-gated meeting embedder (Some only when the REAL e5 model is present → never stub vectors)
    // re-indexes the now-open folder's meetings so semantic / related-meetings work again.
    let meeting_embedder = crate::embed::embed_model_present().then(crate::embed::active_embedder);
    unseal_folder_extras_permanent(state, &folder_id, &ck, meeting_embedder.as_deref())?;

    // Flip the folder back to OPEN + drop it from the session set.
    state.db.set_folder_locked(&folder_id, false, None)?;
    {
        let mut g = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        g.remove(&folder_id);
    }

    // NIT-8 (link lifecycle): the folder's `links` rows were purged on the ORIGINAL seal
    // (`purge_links_tx`). The SESSION unlock (`unlock_folder`) re-derives them; the PERMANENT unseal
    // must too, or a permanently-unlocked folder stays link-empty until the next note edit. Mirror
    // `unlock_folder`: re-run the wikilink pass (deterministic) + the semantic pass (model-gated
    // inside) for every meeting + note now that the folder is fully OPEN. The folder is `locked=0`
    // above, so it is visible under ANY unlock set; add its id to the live snapshot to match the
    // session path exactly. Best-effort — a re-derive failure never fails the permanent unlock (the
    // content is already restored).
    let live_set = {
        let mut s = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
            .clone();
        s.insert(folder_id.clone());
        s
    };
    rederive_links_for_folder(state, &folder_id, &live_set);
    Ok(())
}

/// `discard_unrecoverable_folder_lock(folder_id) -> FolderNode` — the ESCAPE HATCH for a folder whose
/// master KEK is GENUINELY gone (keychain wiped, Mac migrated without the login keychain, or the KEK
/// item truly deleted — e.g. the 2026-07-05 pre-0.7.4 delete-and-replace destroyed the original).
/// Such a folder is otherwise permanently bricked: it cannot be unlocked, its lock cannot be removed,
/// and its content is undecryptable.
///
/// SAFETY (the whole point): this PROVES non-recoverability before destroying anything, via a STRICT
/// candidate enumeration (session-cached KEK, then every keychain candidate) against this folder's
/// wrapped content key. If ANY candidate unwraps it, the folder is RECOVERABLE and this command
/// REFUSES (routing the user to a normal unlock). A biometric cancel / transient keychain error /
/// any failure to COMPLETE the enumeration also aborts — an absence that could not be authoritatively
/// established is never treated as proof. ONLY a completed enumeration that unwraps nothing lets it
/// discard — and even then it discards ONLY the UNRECOVERABLE SEALED payload: a never-sealed buffer
/// (readable plaintext with a NULL blob) is PRESERVED (`Db::discard_folder_seal`).
#[tauri::command]
pub async fn discard_unrecoverable_folder_lock(
    app: AppHandle,
    state: State<'_, AppState>,
    folder_id: String,
) -> Result<FolderNode, AppError> {
    let node = discard_unrecoverable_folder_lock_inner(state.inner(), folder_id).await?;
    // The discard purged ALL pending audit findings — ping the FE inbox (count-only).
    emit_audit_updated_after_purge(&app, state.inner());
    Ok(node)
}

pub(crate) async fn discard_unrecoverable_folder_lock_inner(
    state: &AppState,
    folder_id: String,
) -> Result<FolderNode, AppError> {
    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    if !folder.locked {
        return Err(AppError::InvalidArg("folder is not locked".into()));
    }
    let wrapped = state
        .db
        .folder_wrapped_key(&folder_id)?
        .ok_or_else(|| AppError::Storage("locked folder has no wrapped key".into()))?;

    let aad = aad_wrapped_ck(&folder_id);
    let recoverable = "this folder's key was found — unlock it normally instead of discarding (its content is intact)";

    // ── SAFETY GATE: prove UNRECOVERABLE before destroying anything ──────────────────────────────
    // The discard may proceed ONLY when we have AFFIRMATIVELY established that no key can unwrap this
    // folder. Any inability to COMPLETE that proof — a cancelled/failed Touch ID, a transient
    // keychain fault — MUST abort (never wipe a folder whose key we simply could not read).
    //
    // 1. session-cached KEK (possibly a key recovered earlier this session).
    let cached: Option<Zeroizing<[u8; 32]>> = {
        let g = state
            .master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        g.clone()
    };
    if let Some(k) = &cached {
        if crate::crypto::decrypt(k, &wrapped, &aad).is_ok() {
            return Err(AppError::InvalidArg(recoverable.into()));
        }
    }
    // 2. STRICT enumeration of EVERY keychain candidate (`kSecMatchLimitAll`, one Touch ID —
    //    a superset of the single primary read). Unlike the lenient recovery enumeration, this
    //    PROPAGATES a read/enumeration failure via `??`: a cancelled Touch ID or a transient fault
    //    aborts the discard here, and is NEVER mistaken for "the keychain holds no key". Only a
    //    successfully-completed enumeration that returns NO unwrapping candidate proves the folder
    //    unrecoverable (2026-07-05 lock-security finding — the previous lenient enumeration could
    //    swallow a cancelled second Touch ID and wrongly wipe a recoverable folder).
    let enumeration = tokio::task::spawn_blocking(|| {
        crate::secrets::list_master_kek_candidates_strict(
            "Confirm this folder's key is unrecoverable",
        )
    })
    .await
    .map_err(|e| AppError::Auth(format!("kek-enumeration task join failed: {e}")))?;
    if !discard_proof_complete(enumeration, &wrapped, &folder_id)? {
        return Err(AppError::InvalidArg(recoverable.into()));
    }

    // ── PROVEN unrecoverable → discard THIS folder's sealed payload ──────────────────────────────
    // Serialize with the rest of the lock state machine (acquired AFTER the awaits above so the guard
    // never crosses a suspend point).
    let _lifecycle = lifecycle_guard(state);
    // `discard_folder_seal` returns ONLY the SEALED `.enc` audio paths (a never-sealed plaintext WAV
    // is readable content and is preserved, both on disk and in the DB). Best-effort unlink each.
    let enc_paths = state.db.discard_folder_seal(&folder_id)?;
    let enc_count = enc_paths.len();
    for p in &enc_paths {
        let _ = std::fs::remove_file(p);
    }
    {
        let mut g = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        g.remove(&folder_id);
    }
    tracing::warn!(
        target: "lock",
        folder = %folder_id,
        sealed_audio = enc_count,
        "discard_unrecoverable_folder_lock: key proven unrecoverable — discarded the sealed payload and reopened the folder (never-sealed plaintext preserved)"
    );

    // This folder was just reopened (discard_folder_seal cleared its lock), so its own count is
    // legitimately visible under the (now-empty-for-this-folder) unlock gate — pass the current
    // session unlock set through the same gate every other caller uses.
    let unlocked = {
        state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
            .clone()
    };
    let counts = state.db.count_notes_per_folder(&unlocked)?;
    let kind = state
        .db
        .folder_kind(&folder.id)?
        .unwrap_or_else(|| "meeting".to_string());
    Ok(FolderNode {
        id: folder.id.clone(),
        name: folder.name.clone(),
        parent_id: folder.parent_id.clone(),
        note_count: counts.get(&folder.id).copied().unwrap_or(0),
        locked: false,
        unlocked: false,
        kind,
        children: Vec::new(),
    })
}

/// SESSION-unlock the folder OWNING a meeting (so the FE can unlock straight from the locked
/// Detail view). Resolves the meeting's folder, then delegates to the existing biometric
/// `unlock_folder` path (Touch ID → KEK → unwrap CK → decrypt note + transcript + timeline + audio
/// for the session). A meeting at the vault root or in an open folder is already unlocked → no-op
/// (returns `None`); a sealed folder returns the refreshed `FolderNode`.
#[tauri::command]
pub async fn unlock_meeting(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Option<FolderNode>, AppError> {
    let Some(folder_id) = state.db.folder_for_meeting(&meeting_id)? else {
        return Ok(None); // vault root — nothing to unlock.
    };
    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    if !folder.locked {
        return Ok(None); // open folder — already visible.
    }
    // Reuse the SAME biometric unlock path (do not fork the lifecycle).
    unlock_folder(app, state, folder_id).await.map(Some)
}

/// `discard_unrecoverable_meeting_lock(meeting_id) -> Option<FolderNode>` — the meeting-aware entry to
/// the escape hatch (mirrors `unlock_meeting`): resolves the meeting's owning folder and, if it is
/// locked, runs `discard_unrecoverable_folder_lock` on it (which PROVES non-recoverability and
/// REFUSES if the folder is actually recoverable). `None` when the meeting is at the vault root or the
/// folder is already open.
#[tauri::command]
pub async fn discard_unrecoverable_meeting_lock(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Option<FolderNode>, AppError> {
    let Some(folder_id) = state.db.folder_for_meeting(&meeting_id)? else {
        return Ok(None);
    };
    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    if !folder.locked {
        return Ok(None);
    }
    let node = discard_unrecoverable_folder_lock_inner(state.inner(), folder_id).await?;
    // The discard purged ALL pending audit findings — ping the FE inbox (count-only).
    emit_audit_updated_after_purge(&app, state.inner());
    Ok(Some(node))
}
