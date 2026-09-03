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
/// TWO-LEVEL LOCK — seal `folder_id` AND every container inside it, deepest first.
///
/// Before containers could nest, every lockable thing was a leaf, and sealing exactly one folder
/// was exactly right. The moment a project can hold folders, that same behaviour means a user
/// locks a project, watches it render locked, and every note in every folder underneath is still
/// sitting in the database in plaintext — reachable through the tree, through search, through MCP.
/// The UI says sealed and the bytes say otherwise, which is the worst combination available,
/// because it is precisely when the user stops being careful.
///
/// Each container seals under its OWN content key, through the ordinary per-container path, so
/// every guard, epoch bump and verify-before-destroy that path owns applies to all of them — and
/// the per-container key separation that lets a single folder be unlocked on its own is untouched.
/// One MCP revocation, held by the caller, covers the whole cascade.
///
/// A container that is ALREADY sealed is not RE-KEYED: locking a project with one folder already
/// locked is an ordinary thing to do, and re-minting that folder's key would orphan the ciphertext
/// already written under the old one. It is still driven through the ordinary per-container path,
/// whose already-locked branch is an idempotent repair tail — it bumps the seal epoch, revokes
/// renderer visibility, drops the folder from the SESSION unlock set and repairs plaintext at rest.
///
/// Skipping that tail with a bare `continue` was a leak: a descendant the user had session-unlocked
/// stayed in `state.unlocked_folders` after its parent project was locked, so every gated reader
/// — the tree, search, MCP — kept admitting its rows while the UI said the project was sealed.
/// `locked = 1` is the durable seal JOURNAL, not proof the session-authority cleanup ever ran.
pub(crate) fn lock_container_subtree(
    state: &AppState,
    folder_id: &str,
    allow_live_remote_shares: bool,
    visibility_revoked: impl FnOnce(),
) -> Result<(), AppError> {
    let subtree = container_subtree_deepest_first(state, folder_id)?;
    // All-or-nothing preflight for the knowable mixed-key legacy condition. Without this pass, a
    // valid deepest child could seal before a later ambiguous sibling/parent refuses.
    for id in &subtree {
        ensure_unambiguous_meeting_governance(state, id)?;
    }
    // The first container that crosses the logical privacy boundary owns the one FE/cache notice.
    // In a cascade that is normally the deepest child. Keeping the callback only for the root meant
    // a child's post-revocation reconcile error could return before the renderer was invalidated.
    let visibility_notice = std::cell::RefCell::new(Some(visibility_revoked));
    for id in subtree {
        if id == folder_id {
            continue; // the target is sealed last, below, so its notice fires once at the end.
        }
        // An already-locked descendant is NOT skipped — see this function's doc comment. The
        // per-container path detects `folder.locked` itself and replays only its idempotent,
        // non-keyed repair tail, so the existing content key is never re-minted.
        lock_folder_inner_with_visibility_notice_policy(
            state,
            id,
            allow_live_remote_shares,
            || {
                if let Some(notice) = visibility_notice.borrow_mut().take() {
                    notice();
                }
            },
        )?;
    }
    lock_folder_inner_with_visibility_notice_policy(
        state,
        folder_id.to_string(),
        allow_live_remote_shares,
        || {
            if let Some(notice) = visibility_notice.borrow_mut().take() {
                notice();
            }
        },
    )
}

/// Open an org-share closure over EVERY container about to be sealed, returning the ones this
/// call created (so a failure clears exactly those and never someone else's).
///
/// The closure marker is what `move_note` / `move_note_doc` consult to refuse filing anything into
/// a folder that is closing. The single-folder lock always took one for its target, and the reason
/// is a race: between the moment sealing starts and the moment `locked = 1` is published, the
/// folder looks OPEN to every other writer. A concurrent move landing in that window puts plaintext
/// into a folder whose seal has already walked past it.
///
/// A cascade multiplies that window by the size of the subtree, so every container in it needs the
/// same marker — a descendant sealed without one is exactly the hole the target has been protected
/// from all along.
pub(crate) fn open_subtree_closures(
    state: &AppState,
    subtree: &[String],
) -> Result<Vec<String>, AppError> {
    let mut created = Vec::new();
    for id in subtree {
        if state.db.begin_org_folder_closure(id)? {
            created.push(id.clone());
        }
    }
    Ok(created)
}

/// Every container in `folder_id`'s subtree, DEEPEST FIRST, with the folder itself last.
///
/// The order is the whole point. Locking is not atomic across containers — each seals under its
/// own content key, in its own pass — so a failure part-way through leaves some sealed and some
/// not, and the ORDER decides which. Deepest-first can only ever leave an OUTER container still
/// open around sealed children: an over-lock the user can see and retry. Parent-first would leave
/// a container marked locked with a child still holding plaintext inside it, which is the exact
/// shape of a leak: the UI says sealed, the bytes say otherwise.
pub(crate) fn container_subtree_deepest_first(
    state: &AppState,
    folder_id: &str,
) -> Result<Vec<String>, AppError> {
    let mut out = Vec::new();
    for child in state.db.child_folders(folder_id)? {
        out.extend(container_subtree_deepest_first(state, &child.id)?);
    }
    out.push(folder_id.to_string());
    Ok(out)
}

#[tauri::command]
pub async fn lock_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    folder_id: String,
) -> Result<(), AppError> {
    if folder_id == crate::storage::tasks_store::TASK_FOLDER_ID {
        return Err(AppError::InvalidArg(
            "the task folder cannot be locked".into(),
        ));
    }
    let _org_mutation = state.org_share_mutation_lock.lock().await;
    // PRE-FLIGHT the whole subtree before anything seals. A container can hold containers now, so
    // locking one seals everything inside it (see `lock_container_subtree`) — and this refusal is
    // a property the caller can fix and would want to fix before any container changed state.
    // Discovering it half-way through would leave a partial lock for a reason knowable up front.
    let subtree = container_subtree_deepest_first(state.inner(), &folder_id)?;
    for id in &subtree {
        if state.db.folder_has_active_remote_share(id)? {
            return Err(AppError::Unavailable(if id == &folder_id {
                "revoke this folder's shares before locking it".into()
            } else {
                let name = state
                    .db
                    .folder_by_id(id)?
                    .map(|f| f.name)
                    .unwrap_or_else(|| id.clone());
                format!("revoke the shares on {name} before locking this project")
            }));
        }
        ensure_unambiguous_meeting_governance(state.inner(), id)?;
    }
    // EVERY container in the subtree, not just the target — see `open_subtree_closures`.
    let closures_created = open_subtree_closures(state.inner(), &subtree)?;
    // Initial sealing revokes content just as a session relock does. Shut down every registered
    // MCP content socket BEFORE waiting on the lifecycle mutex, otherwise a slow reader can keep
    // receiving a pre-lock payload after the command has made the folder private.
    let mcp_revocation = crate::mcp::begin_visibility_revocation(
        &app,
        crate::mcp::VisibilityRevokingEntrypoint::LockFolder,
    );
    let result = lock_folder_with_visibility_revocation_and_notice(
        state.inner(),
        &folder_id,
        mcp_revocation,
        || emit_reminder_visibility_invalidated_fail_closed(&app),
    );
    if let Err(error) = result {
        for id in &closures_created {
            state.db.clear_org_folder_closure(id)?;
        }
        return Err(error);
    }
    for id in &subtree {
        state.db.complete_org_closure("folder", id)?;
    }
    // The seal purged ALL pending audit findings — ping the FE inbox (count-only).
    emit_audit_updated_after_purge(&app, state.inner());
    Ok(())
}

/// Explicitly seal local plaintext while intentionally leaving remote recipients' ciphertext
/// readable. This is a separate command so the ordinary lock path can never accidentally inherit
/// the override. It still serializes against share mutations and uses the normal verified seal.
#[tauri::command]
pub async fn lock_folder_allow_remote_access(
    app: AppHandle,
    state: State<'_, AppState>,
    folder_id: String,
) -> Result<(), AppError> {
    if folder_id == crate::storage::tasks_store::TASK_FOLDER_ID {
        return Err(AppError::InvalidArg(
            "the task folder cannot be locked".into(),
        ));
    }
    let _org_mutation = state.org_share_mutation_lock.lock().await;
    // The WHOLE subtree, because this command now seals the whole subtree. A descendant already
    // mid-revocation is the same race the target's check exists to refuse, and the cascade would
    // otherwise walk straight into it. This command deliberately opens NO closure of its own — it
    // keeps remote copies readable by design — so refusing is the only move available here.
    let subtree = container_subtree_deepest_first(state.inner(), &folder_id)?;
    for id in &subtree {
        ensure_unambiguous_meeting_governance(state.inner(), id)?;
        if state.db.org_folder_closure_exists(id)? {
            return Err(AppError::Unavailable(if id == &folder_id {
                "this folder is already closing for verified share revocation".into()
            } else {
                let name = state
                    .db
                    .folder_by_id(id)?
                    .map(|f| f.name)
                    .unwrap_or_else(|| id.clone());
                format!("{name} is already closing for verified share revocation")
            }));
        }
    }
    let mcp_revocation = crate::mcp::begin_visibility_revocation(
        &app,
        crate::mcp::VisibilityRevokingEntrypoint::LockFolder,
    );
    let result = lock_folder_with_visibility_revocation_and_notice_policy(
        state.inner(),
        &folder_id,
        mcp_revocation,
        true,
        || emit_reminder_visibility_invalidated_fail_closed(&app),
    );
    if result.is_ok() {
        emit_audit_updated_after_purge(&app, state.inner());
    }
    result
}

/// A lock authority transition must synchronously revoke every FE reminder-title cache. If the
/// content-free Tauri event bus itself fails, tear down the renderer and terminate the app instead
/// of leaving a stale source title that could reappear when a merely hidden window is shown again.
fn emit_reminder_visibility_invalidated_fail_closed(app: &AppHandle) {
    // Ask caches carry full message/source content, so they share the same synchronous privacy
    // barrier as reminder source titles on every lock-authority transition.
    emit_ask_history_invalidated_fail_closed(app);
    if crate::events::emit_reminder_visibility_invalidated(app) {
        return;
    }
    if let Some(window) = app.get_webview_window("main") {
        if let Err(error) = window.hide() {
            tracing::error!(
                target: "reminders",
                error = %error,
                "failed to hide Murmur after reminder visibility invalidation failure"
            );
        }
        if let Err(error) = window.destroy() {
            tracing::error!(
                target: "reminders",
                error = %error,
                "failed to destroy Murmur renderer after reminder visibility invalidation failure"
            );
        }
    }
    // `exit` remains the terminal fallback even if the window manager rejected both operations.
    // The database is already fail-closed; exiting prevents any surviving renderer cache from
    // being presented after the visibility authority changed.
    app.exit(1);
}

/// Run the initial seal under an already-closed MCP response gate. Reopen admission only once the
/// folder is durably marked locked and absent from the session unlock set. If sealing fails before
/// that logical transition, dropping the incomplete revocation deliberately leaves the gate closed.
#[cfg(test)]
pub(crate) fn lock_folder_with_visibility_revocation(
    state: &AppState,
    folder_id: &str,
    mcp_revocation: crate::mcp::VisibilityRevocation,
) -> Result<(), AppError> {
    lock_folder_with_visibility_revocation_and_notice(state, folder_id, mcp_revocation, || {})
}

fn lock_folder_with_visibility_revocation_and_notice(
    state: &AppState,
    folder_id: &str,
    mcp_revocation: crate::mcp::VisibilityRevocation,
    visibility_revoked: impl FnOnce(),
) -> Result<(), AppError> {
    lock_folder_with_visibility_revocation_and_notice_policy(
        state,
        folder_id,
        mcp_revocation,
        false,
        visibility_revoked,
    )
}

fn lock_folder_with_visibility_revocation_and_notice_policy(
    state: &AppState,
    folder_id: &str,
    mcp_revocation: crate::mcp::VisibilityRevocation,
    allow_live_remote_shares: bool,
    visibility_revoked: impl FnOnce(),
) -> Result<(), AppError> {
    let result = lock_container_subtree(
        state,
        folder_id,
        allow_live_remote_shares,
        visibility_revoked,
    );
    let logically_revoked = state
        .db
        .folder_by_id(folder_id)
        .ok()
        .flatten()
        .is_some_and(|folder| folder.locked)
        && state
            .unlocked_folders
            .lock()
            .map(|folders| !folders.contains(folder_id))
            .unwrap_or(false);
    if logically_revoked {
        crate::mcp::finish_visibility_revocation(mcp_revocation);
    }
    result
}

/// Inner of [`lock_folder`] taking `&AppState` (so the lifecycle stress test can drive it without a
/// `tauri::State`). Holds the [`AppState::lifecycle`] guard for the whole seal.
#[cfg(test)]
pub(crate) fn lock_folder_inner(state: &AppState, folder_id: String) -> Result<(), AppError> {
    lock_folder_inner_with_visibility_notice(state, folder_id, || {})
}

#[cfg(test)]
pub(crate) fn lock_folder_inner_allow_remote_access(
    state: &AppState,
    folder_id: String,
) -> Result<(), AppError> {
    lock_folder_inner_with_visibility_notice_policy(state, folder_id, true, || {})
}

#[cfg(test)]
pub(crate) fn lock_folder_inner_with_visibility_notice(
    state: &AppState,
    folder_id: String,
    visibility_revoked: impl FnOnce(),
) -> Result<(), AppError> {
    lock_folder_inner_with_visibility_notice_policy(state, folder_id, false, visibility_revoked)
}

/// Mint a fresh content key and wrap it for `folder_id`.
///
/// THE one place a folder's content key comes into existence. `lock_folder` and the
/// born-sealed creation both call it, so there is a single random-key source, a single AEAD,
/// and a single AAD binding — and no way for a second mint site to drift from the audited
/// one, which is the only reason a second caller is acceptable at all.
///
/// The wrapped CK is AAD-bound to the folder id (B7): it cannot be lifted onto a different
/// folder row and unwrapped there.
pub(crate) fn mint_wrapped_ck(
    kek: &Zeroizing<[u8; 32]>,
    folder_id: &str,
) -> Result<(Zeroizing<[u8; 32]>, Vec<u8>), AppError> {
    let ck = Zeroizing::new(crate::crypto::random_key()?);
    let wrapped = crate::crypto::encrypt(kek, &*ck, &aad_wrapped_ck(folder_id))?;
    Ok((ck, wrapped))
}

/// The wrapped content key a container is BORN with, bound to the id it will carry.
///
/// The caller inserts the row already sealed, so this runs BEFORE the insert — which it can,
/// because the id is generated by the caller and the AAD binds nothing else.
///
/// Uses the KEK cached by the parent's own unlock and NEVER prompts. That is not a
/// convenience: the caller is only allowed here because the parent is unlocked for this
/// session, and creating a folder is not an operation the user authorised a Touch ID for. If
/// nothing is cached the creation is refused — which is the honest answer, because for the
/// purpose of sealing anything the parent is locked.
pub(crate) fn wrapped_key_for_new_sealed_container(
    state: &AppState,
    folder_id: &str,
) -> Result<Vec<u8>, AppError> {
    let kek: Zeroizing<[u8; 32]> = {
        let cached = state
            .master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        cached.clone().ok_or_else(|| {
            AppError::Locked("unlock the parent folder before creating inside it".into())
        })?
    };
    // The SAME mint `lock_folder` uses — not a second implementation of it. The content key
    // itself is dropped here, and zeroized with it: a container that did not exist a moment
    // ago has nothing to encrypt, so the wrapped copy on its row is the only one that must
    // survive.
    let (_ck, wrapped) = mint_wrapped_ck(&kek, folder_id)?;
    Ok(wrapped)
}

/// Refuse a folder whose meeting/provider rows do not agree on one governing container.
///
/// This is a PRE-MUTATION lock-lifecycle gate. Without it, locking folder A can seal one provider
/// row and the meeting extras under A's CK while folder B later seals a sibling provider under B's
/// CK. Unlocking/removing either lock would then materialize only part of one logical meeting. The
/// ambiguity must be resolved explicitly by filing the meeting, which synchronizes every provider
/// row to the canonical owner.
fn ensure_unambiguous_meeting_governance(
    state: &AppState,
    folder_id: &str,
) -> Result<(), AppError> {
    if state
        .db
        .folder_has_ambiguous_meeting_governance(folder_id)?
    {
        return Err(AppError::Locked(
            "this folder contains a legacy meeting assigned to multiple locations — move the meeting to one folder before changing its lock"
                .into(),
        ));
    }
    Ok(())
}

/// Read-only all-or-nothing preflight for a cascading session unlock.
///
/// Kept as one testable seam so the command cannot accidentally validate only its root while a
/// descendant later restores a mixed-key legacy meeting after the root has already materialized.
pub(crate) fn preflight_unlock_subtree(state: &AppState, folder_id: &str) -> Result<(), AppError> {
    for id in container_subtree_deepest_first(state, folder_id)? {
        ensure_unambiguous_meeting_governance(state, &id)?;
    }
    Ok(())
}

fn lock_folder_inner_with_visibility_notice_policy(
    state: &AppState,
    folder_id: String,
    allow_live_remote_shares: bool,
    visibility_revoked: impl FnOnce(),
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    if !allow_live_remote_shares && state.db.folder_has_active_remote_share(&folder_id)? {
        return Err(AppError::Unavailable(
            "remote shares appeared while preparing to lock this folder".into(),
        ));
    }
    // The reserved Notes root is the always-open home for unfiled notes — it can NEVER be sealed
    // (unfiled notes are deliberately plaintext; sealing requires filing into a lockable folder). Refuse
    // rather than silently no-op so the FE can guide the user (2026-07-14).
    if state.db.folder_is_root(&folder_id)? {
        return Err(AppError::InvalidArg(
            "the Notes root can't be locked — move notes into a folder to seal them".into(),
        ));
    }
    // Must precede the already-locked repair branch too: that branch bumps epochs, revokes
    // renderer visibility, edits the session set and may repair plaintext at rest.
    ensure_unambiguous_meeting_governance(state, &folder_id)?;
    if !folder.locked {
        // A from-disk salvage may hold this folder's current CK across long ASR/provider awaits so
        // a FRESH lock would mint a different CK and orphan that authority.
        ensure_no_active_salvage_in_folder(state, &folder_id)?;
        // Resume expired archived-generation cleanup before the final fail-closed reread. These
        // checks precede logical revocation because they are ordinary preflight refusals, not filing
        // recovery: the folder remains intentionally open when active capture still owns it.
        for meeting_id in state
            .db
            .nonterminal_recording_meetings_in_folder(&folder_id)?
        {
            reconcile_released_generation_cleanup(state, &meeting_id)?;
        }
        if state
            .db
            .folder_has_nonterminal_recording_generation(&folder_id)?
        {
            return Err(AppError::Locked(
                "this folder has a recording still being captured or recovered — wait for it to finish before locking"
                    .into(),
            ));
        }
    }

    if folder.locked {
        // The durable gate is already closed. Revoke session visibility and invalidate renderer
        // state before fallible journal/repair work so an interrupted retry remains fail-closed.
        bump_seal_epoch(state);
        {
            let mut unlocked = state
                .unlocked_folders
                .lock()
                .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
            unlocked.remove(&folder_id);
        }
        visibility_revoked();
        if let Ok(mut cache) = state.verify_cache.lock() {
            cache.clear();
        }
        if state.db.folder_has_pending_filing_sources(&folder_id)? {
            return Err(AppError::Unavailable(
                "resolve pending filing recovery before locking this folder".into(),
            ));
        }
        reconcile_filing_projection_journal_for_folders(
            &state.db,
            &std::iter::once(folder_id.clone()).collect(),
        )?;
        // `locked=1` is the durable seal journal, not proof that its non-keyed cleanup tail finished.
        // Always replay that idempotent tail. Only release the CK when primary plaintext/audio residue
        // proves the keyed portion itself was interrupted.
        let exported_paths: Vec<String> = state
            .db
            .meeting_note_export_rows_in_folder(&folder_id)?
            .into_iter()
            .map(|(_, _, path, _)| path)
            .collect();
        if locked_folder_requires_authenticated_repair(&state.db, &folder_id)? {
            let wrapped = state
                .db
                .folder_wrapped_key(&folder_id)?
                .ok_or_else(|| AppError::Storage("locked folder has no wrapped key".into()))?;
            // This is non-destructive recovery: a partial/failed enumeration
            // can only leave plaintext in place and return an error. Use the
            // lenient source so a debug MURMUR_DEV_KEK can finish its own
            // interrupted seal without touching the release Keychain.
            let candidates = crate::secrets::list_master_kek_candidates(
                "Finish securing this folder after an interrupted lock",
            )?;
            let (ck_bytes, _winning_kek, _winner_index) =
                try_unwrap_ck_with_candidates(&candidates, &wrapped, &folder_id, None).ok_or_else(
                    || {
                        AppError::Auth(
                            "no keychain key can finish the interrupted folder lock".into(),
                        )
                    },
                )?;
            let ck: Zeroizing<[u8; 32]> =
                Zeroizing::new(ck_bytes.as_slice().try_into().map_err(|_| {
                    AppError::Storage("unwrapped content key has wrong length".into())
                })?);
            repair_locked_folder_at_rest(&state.db, &folder_id, &ck)?;
            if locked_folder_requires_authenticated_repair(&state.db, &folder_id)? {
                return Err(AppError::Storage(
                    "interrupted folder lock repair did not reach a sealed at-rest shape".into(),
                ));
            }
        }
        finish_folder_lock_after_seal(state, &folder_id, &exported_paths)?;
        clear_stale_live_transcript(state);
        return Ok(());
    }
    // A source snapshot is plaintext recovery authority for its exact capture-time domain. Resolve
    // it as a separate open-state operation; sealing must not restore-and-clear the row in the same
    // transition because a companion can be governed by this folder while the attempt itself says
    // Unfiled -> some unrelated target.
    if state.db.folder_has_pending_filing_sources(&folder_id)? {
        return Err(AppError::Unavailable(
            "resolve pending filing recovery before locking this folder".into(),
        ));
    }
    // A fresh folder is still accurately open. Resolve only its scoped target projections before
    // starting the privacy transition; a collision must return with no false renderer revocation.
    // Source authority was refused above and therefore cannot be restored implicitly here.
    reconcile_filing_projection_journal_for_folders(
        &state.db,
        &std::iter::once(folder_id.clone()).collect(),
    )?;
    // R7: the seal epoch still advances before the first verified seal mutation, but only after
    // ordinary open-state preflights that can leave the folder intentionally readable.
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
    let (ck, wrapped) = mint_wrapped_ck(&kek, &folder_id)?;

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
    let doc_export_rows = state.db.note_exported_path_rows_in_folder(&folder_id)?;
    ensure_no_external_edit_siblings(
        exported_paths
            .iter()
            .chain(doc_export_rows.iter().map(|(_, path)| path)),
    )?;

    // Preflight every governed Markdown and image replica before deleting the first one. This keeps
    // an ordinary multi-file conflict atomic from the user's perspective: a changed sibling aborts
    // while all other vault files and the folder's open state are still intact.
    for note in &notes {
        if let Some(path) = note.exported_path.as_deref() {
            let expected = state
                .db
                .get_note_exported_hash(&note.meeting_id, &note.provider_id)?;
            verify_note_export_unchanged(
                path,
                expected.as_deref(),
                "verify meeting-note export before folder seal",
            )?;
        }
    }
    for (document_id, path) in &doc_export_rows {
        let expected = state.db.get_note_doc_exported_hash(document_id)?;
        verify_note_export_unchanged(
            path,
            expected.as_deref(),
            "verify authored-note export before folder seal",
        )?;
    }
    let attachment_rows = state.db.attachments_in_folder(&folder_id)?;
    verify_attachment_exports(
        &attachment_rows,
        "could not verify an exported image before locking the folder",
    )?;

    // Create and verify every attachment seal while the folder is still open. Startup can safely
    // blank these rows if the process dies after the locked bit is published.
    seal_attachments_in_folder(state, &folder_id, &ck)?;
    remove_attachment_exports(
        &attachment_rows,
        "could not remove an exported image before locking the folder",
    )?;

    // Claim and content-verify every Markdown export while the folder is still OPEN. A mismatch or
    // legacy row aborts before `locked=1`; matching files are atomically quarantined and removed,
    // with their canonical DB content still intact if a later seal step fails.
    for note in &notes {
        if let Some(path) = note.exported_path.as_deref() {
            let expected = state
                .db
                .get_note_exported_hash(&note.meeting_id, &note.provider_id)?;
            remove_note_export_if_unchanged(
                path,
                expected.as_deref(),
                "remove meeting-note export before folder seal",
            )?;
        }
    }
    for (document_id, path) in &doc_export_rows {
        let expected = state.db.get_note_doc_exported_hash(document_id)?;
        remove_note_export_if_unchanged(
            path,
            expected.as_deref(),
            "remove authored-note export before folder seal",
        )?;
        state.db.set_note_doc_exported_path(document_id, None)?;
    }

    // Persist: mark the folder locked (+ wrapped key) and write every sealed blob per provider
    // row (markdown blanked, exported_path cleared). Each write is guarded by the verification
    // above, so a crash mid-loop leaves already-sealed rows recoverable and not-yet-sealed rows
    // with intact plaintext — never lost content.
    state
        .db
        .publish_fresh_folder_lock_and_purge_reminder_derived(&folder_id, &wrapped)?;
    // `locked=1` is now the durable visibility gate. Notify before any subsequent fallible
    // plaintext cleanup. A renderer refetch blocks on lifecycle until session authority is removed.
    visibility_revoked();
    {
        let mut unlocked = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        unlocked.remove(&folder_id);
    }
    if let Ok(mut cache) = state.verify_cache.lock() {
        cache.clear();
    }
    for (meeting_id, provider_id, blob) in &sealed_rows {
        state.db.seal_note(meeting_id, provider_id, blob)?;
    }

    // Blobs above were verified before the locked bit was published. Blank image plaintext now;
    // a later transcript/audio failure must not leave readable images governed by a locked folder.
    reblank_attachments_in_folder(state, &folder_id)?;

    // Phase 0.5 — seal the TRANSCRIPT + TIMELINE (defense-in-depth in the OPEN db) and the AUDIO
    // WAV at rest, all under the SAME folder CK. Verify-before-destroy inside (no transcript /
    // audio loss). Done after the note seal so a partial-seal crash still leaves recoverable blobs.
    seal_folder_extras(&state.db, &folder_id, &ck)?;
    drop(kek); // explicit: KEK zeroized when this Zeroizing drops here.
    drop(ck); // explicit: CK zeroized after sealing all extras.

    finish_folder_lock_after_seal(state, &folder_id, &exported_paths)?;

    // Belt-and-braces RAM hygiene: with no recording active, drop any stale live-caption buffer at
    // the moment a folder seals (post clear-on-Stop it is normally already empty; idempotent).
    clear_stale_live_transcript(state);
    Ok(())
}

/// Complete the non-keyed cleanup shared by a fresh seal and an authenticated retry of an
/// interrupted seal. This begins only after every primary content artifact is recoverably sealed.
fn finish_folder_lock_after_seal(
    state: &AppState,
    folder_id: &str,
    meeting_export_paths: &[String],
) -> Result<(), AppError> {
    // Also covers an authenticated retry after a prior lock attempt published `locked=1` and then
    // failed before its image cleanup completed. Blanking requires an existing recoverable blob.
    reblank_attachments_in_folder(state, folder_id)?;
    let sealed_meeting_ids = state.db.meeting_ids_in_folder(folder_id)?;
    let sealed_document_ids = state.db.document_ids_in_folder(folder_id)?;
    state
        .db
        .strip_sealed_neighbour_markers(&sealed_meeting_ids, &sealed_document_ids)?;
    remove_rollup_exports_before_seal_purge(&state.db)?;
    let rollup_exports = state.db.purge_chunks_for_meetings(&sealed_meeting_ids)?;
    for path in &rollup_exports {
        crate::crypto::remove_file_verified_absent(
            std::path::Path::new(path),
            "remove memory-rollup export during folder seal",
        )?;
    }
    state
        .db
        .purge_doc_chunks_for_documents(&sealed_document_ids)?;
    // Both chunk purge helpers deliberately no-op on an empty id list. An already-locked empty
    // folder still represents a visibility authority transition, so close that hole explicitly
    // while the caller holds the lifecycle guard.
    state.db.purge_all_ask_conversations()?;

    let doc_export_rows = state.db.note_exported_path_rows_in_folder(folder_id)?;
    ensure_no_external_edit_siblings(
        meeting_export_paths
            .iter()
            .chain(doc_export_rows.iter().map(|(_, path)| path)),
    )?;
    for (document_id, path) in doc_export_rows {
        let expected = state.db.get_note_doc_exported_hash(&document_id)?;
        remove_note_export_if_unchanged(
            &path,
            expected.as_deref(),
            "remove authored-note export during folder seal",
        )?;
        state.db.set_note_doc_exported_path(&document_id, None)?;
    }
    // Drain LAST: an outside-source export error must never prevent this locked folder's own
    // plaintext exports/derived data from reaching their sealed at-rest shape.
    drain_lock_marker_export_cleanup(&state.db)
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
    // NOTE: `org_share_mutation_lock` is acquired LATER, just before the restore — see the comment
    // at that acquisition. Taking it here held it across the Touch ID sheet.
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
    // Read-only WHOLE-SUBTREE preflight before Keychain release, plaintext restore, cache/session
    // mutation or vault export. The primary restore later cascades into locked descendants; finding
    // a mixed-key legacy row only after opening the parent would be a preventable partial unlock.
    preflight_unlock_subtree(state.inner(), &folder_id)?;
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

    // Serialize against org mutation/revoke from HERE, not from the top of the command.
    //
    // This lock guards one thing in this function: `clear_org_folder_closure` at the end, which
    // lifts the org barrier for this folder. Everything before this point is a read plus the
    // biometric KEK release — and that release BLOCKS ON A HUMAN. Acquiring the lock at the top of
    // the command therefore held a process-wide org mutex across the Touch ID sheet, for as long as
    // the user took to answer (or not answer) it: org sync, share dispatch and revoke all stalled
    // behind a dialog. It is taken here instead, immediately before the first mutation, so the
    // barrier it exists for is unchanged while the unbounded human wait is outside it.
    //
    // The lock-state race this does NOT need to cover is covered elsewhere: the restore below takes
    // `lifecycle_guard` for its whole synchronous body, which is what serializes against a
    // concurrent `lock_folder` / `relock_all_inner`.
    let _org_mutation = state.org_share_mutation_lock.lock().await;

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
        let meeting_embedder = crate::embed::active_persistence_embedder_if_available();
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
        // External vault files are readable outside the lifecycle mutex. Publish them only after
        // the folder is admitted to the session unlock set, so every attachment byte read goes
        // through the ordinary owner gate and no pre-admission plaintext can escape to disk.
        reexport_notes_in_folder(&state, &folder_id);
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
    state.db.clear_org_folder_closure(&folder_id)?;

    // TWO-LEVEL UNLOCK — the other half of the cascading lock. Locking a project seals every
    // container beneath it, so unlocking one has to open them again, or the two actions are
    // asymmetric in the way users actually notice: one click sealed six folders, and six clicks
    // are needed to get them back.
    //
    // Each descendant is opened with its OWN content key, unwrapped from the master KEK the
    // restore above has now cached — so this costs no second Touch ID prompt, and the per-container
    // key separation that makes a single-folder unlock possible is untouched.
    //
    // Best-effort per descendant: a container that cannot be opened is logged and skipped rather
    // than failing the unlock that already succeeded. The failure direction matters — a descendant
    // left sealed stays unreadable, which is the safe side, and the user can retry it alone.
    let descendants: Vec<String> = container_subtree_deepest_first(state.inner(), &folder_id)?
        .into_iter()
        .filter(|id| *id != folder_id)
        .collect();
    for id in descendants {
        let Some(child) = state.db.folder_by_id(&id)? else {
            continue;
        };
        if !child.locked || folder_is_unlocked(state.inner(), &id)? {
            continue;
        }
        if let Err(error) = unlock_container_with_cached_kek(&app, state.inner(), &id).await {
            tracing::warn!(
                target: "lock",
                folder = %id,
                error = %error,
                "unlock_folder: a descendant container stayed sealed"
            );
        }
    }

    Ok(restored)
}

/// Session-unlock ONE already-sealed container using the master KEK cached by an earlier unlock.
///
/// The cascading unlock's per-descendant step. It deliberately does NOT contain the biometric
/// release, the candidate-recovery ladder, or the org-closure bookkeeping: those belong to the
/// container the user actually asked for, ran once there, and their result — the cached KEK — is
/// what makes this cheap. With no cached KEK there is nothing to do here, and saying so is better
/// than quietly prompting for Touch ID once per folder in a subtree.
async fn unlock_container_with_cached_kek(
    app: &AppHandle,
    state: &AppState,
    folder_id: &str,
) -> Result<(), AppError> {
    ensure_unambiguous_meeting_governance(state, folder_id)?;
    let kek: Zeroizing<[u8; 32]> = {
        let guard = state
            .master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        guard
            .clone()
            .ok_or_else(|| AppError::Auth("no cached master key for the cascading unlock".into()))?
    };
    let wrapped = state
        .db
        .folder_wrapped_key(folder_id)?
        .ok_or_else(|| AppError::Storage("locked folder has no wrapped key".into()))?;
    let ck_bytes = Zeroizing::new(crate::crypto::decrypt(
        &kek,
        &wrapped,
        &aad_wrapped_ck(folder_id),
    )?);
    let ck: Zeroizing<[u8; 32]> = Zeroizing::new(
        ck_bytes
            .as_slice()
            .try_into()
            .map_err(|_| AppError::Storage("unwrapped content key has wrong length".into()))?,
    );

    let heavy_inference = state.heavy_inference.clone();
    let app_for_restore = app.clone();
    let folder_id_owned = folder_id.to_string();
    crate::perf::run_heavy(&heavy_inference, move || -> Result<(), AppError> {
        let state = app_for_restore.state::<AppState>();
        let folder_id = folder_id_owned;
        // Same lifecycle guard as the primary restore, for the same reason: this mutates plaintext
        // columns, and a concurrent relock must not blank them mid-restore.
        let _lifecycle = lifecycle_guard(&state);

        for n in &state.db.notes_in_folder(&folder_id)? {
            let Some(blob) = &n.content_blob else {
                continue;
            };
            let aad = aad_content(&folder_id, &n.meeting_id, &n.provider_id, "note");
            let markdown = String::from_utf8(crate::crypto::decrypt(&ck, blob, &aad)?)
                .map_err(|_| AppError::Storage("decrypted note is not valid UTF-8".into()))?;
            state
                .db
                .restore_note_markdown(&n.meeting_id, &n.provider_id, &markdown)?;
        }

        let meeting_embedder = crate::embed::active_persistence_embedder_if_available();
        unseal_folder_extras(&state, &folder_id, &ck, meeting_embedder.as_deref())?;

        {
            let mut g = state
                .unlocked_folders
                .lock()
                .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
            g.insert(folder_id.clone());
        }
        reexport_notes_in_folder(&state, &folder_id);
        let unlocked = {
            state
                .unlocked_folders
                .lock()
                .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
                .clone()
        };
        rederive_links_for_folder(&state, &folder_id, &unlocked);
        tracing::info!(target: "lock", folder = %folder_id, "cascading unlock: container opened");
        Ok(())
    })
    .await?;
    Ok(())
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
    // Cancel response admission and shutdown active content sockets BEFORE waiting on lifecycle.
    // A slow MCP reader can therefore never delay the privacy transition.
    let mcp_revocation = crate::mcp::begin_visibility_revocation(
        &app,
        crate::mcp::VisibilityRevokingEntrypoint::RelockFolder,
    );
    relock_folder_inner_with_visibility_notice(state.inner(), &folder_id, || {
        // Epoch + session membership now authoritatively hide these folders. New MCP responses may
        // be admitted against that post-revocation snapshot while physical cleanup continues.
        crate::mcp::finish_visibility_revocation(mcp_revocation);
        // Emitted while the lifecycle guard is still held. The FE discards cached titles
        // synchronously; its canonical refresh cannot pass the same guard until physical reblank
        // has completed.
        emit_reminder_visibility_invalidated_fail_closed(&app);
    })?;
    // The relock purged ALL pending audit findings — ping the FE inbox (count-only).
    emit_audit_updated_after_purge(&app, state.inner());
    Ok(())
}

/// The relock cascade itself, with no `AppHandle` — so it is reachable from tests, exactly like its
/// sibling [`relock_all_inner_with_visibility_notice`].
///
/// `visibility_revoked` runs at the ONE point where the folders have been dropped from the session
/// unlock set and the seal epoch has advanced, but physical cleanup has not started. That ordering
/// is load-bearing for screen-share relock: gated readers must be shut out before any fallible
/// filesystem work, and a later failure must not re-open them.
pub(crate) fn relock_folder_inner_with_visibility_notice(
    state: &AppState,
    folder_id: &str,
    visibility_revoked: impl FnOnce(),
) -> Result<(), AppError> {
    // BLK-1: serialize with the rest of the lock state machine (it re-blanks the same columns
    // `remove_lock` is mid-restoring).
    let _lifecycle = lifecycle_guard(state);
    if !folder_is_unlocked(state, folder_id)? {
        return Err(AppError::Locked(
            "folder visibility changed before relock preparation".into(),
        ));
    }
    // CASCADE, because `unlock_folder` cascades. Unlocking a container opens its whole subtree
    // (`preflight_unlock_subtree` + the primary restore's descent into locked descendants), so a
    // relock that closed only the target left every descendant the unlock had opened still in the
    // session unlock set — "Lock again" on a project reported sealed while its folders stayed
    // readable through every gated reader. An asymmetric open/close pair is a leak by construction.
    //
    // Deepest-first, matching `lock_container_subtree`. Only SESSION-UNLOCKED containers are
    // targets: a descendant that was never opened has nothing to re-blank, and `relock` is a
    // session operation — it does not change any durable `locked` bit.
    let targets: Vec<String> = container_subtree_deepest_first(state, folder_id)?
        .into_iter()
        .filter(|id| folder_is_unlocked(state, id).unwrap_or(false))
        .collect();
    // Verify EVERY non-empty plaintext family, for EVERY target, before deleting an export or
    // blanking the first DB column. Retained blobs must match byte-identically; missing blobs are
    // encrypt+verified into an immutable repair plan. The lifecycle guard prevents either side
    // changing afterward. This whole pass is read-only, so a refusal anywhere leaves the entire
    // subtree exactly as it was — that is why it runs before the destructive export pass below.
    let mut verified_plans: Vec<(String, VerifiedRelockPlan)> = Vec::with_capacity(targets.len());
    for id in &targets {
        verified_plans.push((id.clone(), verify_relock_retained_blobs(state, id)?));
    }
    // Remove every managed plaintext export before revoking visibility. A user-modified export or
    // collision sibling fails here while the folder is still open, never after `locked` is visible.
    for id in &targets {
        prepare_folder_exports_before_relock(state, id)?;
    }
    // R7: advance the seal epoch at ENTRY — see `bump_seal_epoch`. Once for the whole cascade.
    bump_seal_epoch(state);
    // Revoke logical visibility FIRST, for every target at once. Screen-share relock must hide the
    // folders from every gated UI/MCP reader even when a later filesystem cleanup fails. Keep the
    // cached KEK until the physical reblank succeeds, though, so the same process retains authority
    // to retry safely.
    {
        let mut g = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        for id in &targets {
            g.remove(id);
        }
    }
    // Hand the caller its one revocation point — see this function's doc comment.
    visibility_revoked();
    if let Ok(mut cache) = state.verify_cache.lock() {
        cache.clear();
    }
    for id in &targets {
        if state.db.folder_has_pending_filing_sources(id)? {
            return Err(AppError::Unavailable(
                "filing recovery must be resolved before physical relock cleanup".into(),
            ));
        }
    }
    // Filing recovery is fallible filesystem work. It is scoped to this relock cascade and runs
    // only after the epoch/session authority transition above, so neither a related conflict nor an
    // unrelated stuck attempt can leave the selected subtree readable.
    reconcile_filing_projection_journal_for_folders(&state.db, &targets.iter().cloned().collect())?;
    // Any interrupted-fresh-seal rows now receive only ciphertexts that the preflight already
    // decrypt-verified against their exact plaintext. This is after export cleanup and logical
    // revocation, but before the first broad reblank.
    for (_, verified) in &verified_plans {
        verified.apply_repairs(&state.db)?;
    }
    let sealed: std::collections::HashSet<String> = targets.iter().cloned().collect();
    // Brain-v3 audit Fix 4: strip the just-re-sealed items' `[[Title]]` markers from VISIBLE sources
    // (in other still-open folders) + re-export their `.md`, BEFORE the relock purge drops the naming
    // edges. The whole cascade goes in one call so a marker pointing from one re-sealed folder into
    // another is never treated as "still visible".
    enqueue_marker_cleanup_for_folders(state, &sealed)?;
    // The re-blank tx also purges ALL memory rollups (may paraphrase the re-sealed facts) and
    // returns their exported vault paths — the files are removed here (command layer).
    // FACT LEDGER, RE-SEALED BEFORE THE PURGE. `blank_sealed_notes_in_folders` deletes facts, user
    // facts and supersessions, and on a RELOCK those rows are whatever the unlocked session ended
    // with — restored ones plus anything extracted while the folder was open. Sealing after that
    // call would encrypt nothing and leave the session's additions destroyed, so it happens here,
    // with the content key the preflight already verified. A folder whose key could not be verified
    // is skipped HERE — its ledger is not re-sealed — but the purge below still runs for it, so this
    // is not a guarantee that its facts survive; it is only a refusal to encrypt under a key nobody
    // proved. In practice that folder has no live plaintext left for the key to be missing over.
    for (id, verified) in &verified_plans {
        if let Some(ck) = verified.ck() {
            for mid in state.db.meeting_ids_in_folder(id)? {
                crate::commands::seal_fact_ledger_for_meeting(&state.db, id, &mid, ck)?;
            }
        }
    }
    remove_rollup_exports_before_seal_purge(&state.db)?;
    let rollup_exports = state.db.blank_sealed_notes_in_folders(&sealed)?;
    remove_rollup_export_files(&rollup_exports);
    // Phase 0.5 — re-blank the transcript + timeline plaintext and drop the decrypted session WAV
    // (the .enc + the *_blob columns stay; the folders are still locked=1 on disk). Per folder,
    // because each carries its OWN content key.
    for (id, verified) in &verified_plans {
        reblank_folder_extras_after_verification(state, id, verified.ck())?;
    }
    drain_lock_marker_export_cleanup(&state.db)?;
    // The folders have remained logically hidden throughout. A failure above retains the cached KEK
    // for a repair retry; success leaves it available for other still-unlocked folders.
    Ok(())
}

/// Relock ALL session-unlocked folders + zeroize the cached KEK (called on screen-share start in
/// Stage E, and exposed as a command). Re-blanks the plaintext markdown of every sealed note.
#[tauri::command]
pub fn relock_all(app: AppHandle, state: State<'_, AppState>) -> Result<(), AppError> {
    relock_all_with_visibility_gate(&app, state.inner())?;
    // The relock-all purged ALL pending audit findings — ping the FE inbox (count-only). The
    // off-thread `relock_all_inner` callers emit from their own handles (screen-share) or are
    // app teardown (window-close/exit — nothing left to notify).
    emit_audit_updated_after_purge(&app, state.inner());
    Ok(())
}

/// App-facing relock-all wrapper. Every production entrypoint (manual command, screen-share,
/// window close, app exit) uses this wrapper so response shutdown always precedes lifecycle
/// acquisition. Tests that exercise only the storage state machine may continue to call
/// [`relock_all_inner`] directly without constructing a Tauri runtime.
pub(crate) fn relock_all_with_visibility_gate(
    app: &AppHandle,
    state: &AppState,
) -> Result<(), AppError> {
    let mcp_revocation = crate::mcp::begin_visibility_revocation(
        app,
        crate::mcp::VisibilityRevokingEntrypoint::RelockAll,
    );
    let prior_epoch = state.seal_epoch.load(std::sync::atomic::Ordering::SeqCst);
    let result = relock_all_inner_with_visibility_notice(state, || {
        emit_reminder_visibility_invalidated_fail_closed(app);
    });
    // `relock_all_inner` deliberately keeps logical revocation even when a later vault/DB cleanup
    // fails. Reopen only if the epoch advanced and the unlock authority is observably empty.
    let logically_revoked = state.seal_epoch.load(std::sync::atomic::Ordering::SeqCst)
        != prior_epoch
        && state
            .unlocked_folders
            .lock()
            .map(|folders| folders.is_empty())
            .unwrap_or(false);
    if logically_revoked {
        crate::mcp::finish_visibility_revocation(mcp_revocation);
    }
    result
}

/// Inner relock-all usable without a command boundary (Stage E screen-share watcher, window-close,
/// app-exit). BLK-1: this is the OFF-THREAD blanker that races `remove_lock`; it acquires the
/// [`AppState::lifecycle`] guard FIRST so its re-blank can never land between `remove_lock`'s
/// restore-plaintext (Step 1) and clear-`content_blob` (Step 2). All three off-thread callers and
/// the `relock_all` command funnel through here, so the guard lives HERE (the `relock_all` command
/// must NOT take it separately — a std `Mutex` is non-reentrant and would self-deadlock).
#[cfg(test)]
pub(crate) fn relock_all_inner(state: &AppState) -> Result<(), AppError> {
    relock_all_inner_with_visibility_notice(state, || {})
}

pub(crate) fn relock_all_inner_with_visibility_notice(
    state: &AppState,
    visibility_revoked: impl FnOnce(),
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    // Snapshot the session set, then revoke every gated read BEFORE any fallible filesystem work.
    // This ordering is load-bearing for screen-share: an external edit or unlink failure may retain
    // recoverable vault bytes, but UI/MCP visibility must already be gone and the failure event must
    // never claim successful physical cleanup.
    let session_unlocked: Vec<String> = state
        .unlocked_folders
        .lock()
        .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
        .iter()
        .cloned()
        .collect();
    // R7: advance the seal epoch at ENTRY — see `bump_seal_epoch`.
    bump_seal_epoch(state);
    // Revoke every gated read before touching filesystem state. This is load-bearing for the
    // screen-share watcher: an unlink/reseal error must never leave plaintext visible in the UI or
    // MCP while sharing is active. The KEK is deliberately retained until physical cleanup below
    // succeeds, preserving repair authority without preserving visibility.
    {
        let mut g = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        g.clear();
    }
    // This callback is deliberately inside the lifecycle interval and immediately after the
    // in-memory visibility authority is cleared. A FE re-fetch therefore blocks until every
    // fallible reblank step has either completed or failed closed, while cached titles disappear
    // at the exact logical transition.
    visibility_revoked();
    if let Ok(mut cache) = state.verify_cache.lock() {
        cache.clear();
    }
    // Resolve every target and preflight ALL non-empty plaintext before the first export removal or
    // DB blank. Retained blobs are authenticated; missing blobs are encrypt+verified into immutable
    // repair plans. Visibility is already revoked fail-closed, while the cached KEK remains held for
    // an authenticated retry if any retained blob is corrupt or stale.
    let locked: std::collections::HashSet<String> =
        state.db.locked_folder_ids()?.into_iter().collect();
    for id in &locked {
        if state.db.folder_has_pending_filing_sources(id)? {
            return Err(AppError::Unavailable(
                "filing recovery must be resolved before global relock cleanup".into(),
            ));
        }
    }
    // Recovery is fallible filesystem work. Screen-share relock must first revoke every gated
    // reader and advance the epoch; a hostile journal path can then block physical cleanup without
    // ever restoring renderer/MCP visibility. Raw-locked source snapshots were refused above so
    // this global pass cannot restore and then orphan stale plaintext inside a sealed folder.
    reconcile_filing_projection_journal(&state.db)?;
    let mut verified_relocks = Vec::with_capacity(locked.len());
    for folder_id in &locked {
        verified_relocks.push((
            folder_id.clone(),
            verify_relock_retained_blobs(state, folder_id)?,
        ));
    }
    let mut export_cleanup: std::collections::HashSet<String> =
        session_unlocked.iter().cloned().collect();
    for (folder_id, verified) in &verified_relocks {
        if verified.ck().is_some() {
            export_cleanup.insert(folder_id.clone());
        }
    }
    for folder_id in &export_cleanup {
        prepare_folder_exports_before_relock(state, folder_id)?;
    }
    // Every folder plan was fully built before this first plaintext blank. Applying a repair stores
    // only ciphertext already decrypt-verified byte-identical; any later failure retains the cached
    // KEK and leaves completed rows recoverable for an idempotent retry.
    for (_, verified) in &verified_relocks {
        verified.apply_repairs(&state.db)?;
    }
    // Re-blank every sealed note across all locked folders. The tx also purges ALL memory rollups
    // (may paraphrase the re-sealed facts) — their exported vault `.md`s are removed here.
    // Brain-v3 audit Fix 4: BEFORE the relock purge drops the naming edges, strip the just-re-sealed
    // items' `[[Title]]` markers from every VISIBLE source note (a source in a DIFFERENT still-open
    // folder that was materialized while this folder was session-unlocked) + re-export those sources'
    // `.md`. Resolve the relocked folders' meeting + document ids first.
    enqueue_marker_cleanup_for_folders(state, &locked)?;
    // FACT LEDGER, RE-SEALED BEFORE THE PURGE. `blank_sealed_notes_in_folders` deletes facts, user
    // facts and supersessions, and on a RELOCK those rows are whatever the unlocked session ended
    // with — restored ones plus anything extracted while the folder was open. Sealing after that
    // call would encrypt nothing and leave the session's additions destroyed, so it happens here,
    // with the content key the preflight already verified. A folder whose key could not be verified
    // is skipped HERE — its ledger is not re-sealed — but the purge below still runs for it, so this
    // is not a guarantee that its facts survive; it is only a refusal to encrypt under a key nobody
    // proved. In practice that folder has no live plaintext left for the key to be missing over.
    for (folder_id, verified) in &verified_relocks {
        if let Some(ck) = verified.ck() {
            for mid in state.db.meeting_ids_in_folder(folder_id)? {
                crate::commands::seal_fact_ledger_for_meeting(&state.db, folder_id, &mid, ck)?;
            }
        }
    }
    remove_rollup_exports_before_seal_purge(&state.db)?;
    let rollup_exports = state.db.blank_sealed_notes_in_folders(&locked)?;
    remove_rollup_export_files(&rollup_exports);
    // Phase 0.5 — re-blank the transcript + timeline + drop the decrypted session WAVs for every
    // locked folder too (the .enc + *_blob columns stay).
    for (folder_id, verified) in &verified_relocks {
        reblank_folder_extras_after_verification(state, folder_id, verified.ck())?;
    }
    // Zeroize the cached KEK copy only after all seal/reblank work succeeded. If an earlier step
    // returned an error, visibility is still revoked but the KEK remains available for repair.
    // `Zeroize::zeroize`
    // is non-elidable; taking the `Zeroizing` out and dropping it also wipes it.
    {
        let mut g = state
            .master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        if let Some(mut k) = g.take() {
            k.zeroize();
        }
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
    // Last privacy leg: folder-owned plaintext is already reblanked and the cached KEK is gone.
    // Failure retains the SQLCipher outbox and propagates for a later idempotent retry.
    drain_lock_marker_export_cleanup(&state.db)
}

/// PERMANENTLY remove a folder's lock: KEK → unwrap CK → decrypt each note back to plaintext
/// markdown, clear `content_blob`, set `locked=0` + `wrapped_key=NULL`, and re-export each note's
/// `.md` to the vault. The folder returns to the default OPEN state.
#[tauri::command]
pub async fn remove_lock(state: State<'_, AppState>, folder_id: String) -> Result<(), AppError> {
    let _org_mutation = state.org_share_mutation_lock.lock().await;
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
    // Permanent unlock restores plaintext and later re-exports it. Refuse before epoch/key/cache/
    // DB mutation when any provider sibling belongs elsewhere, so opening one folder can never
    // materialize only part of a meeting still governed by another lock.
    ensure_unambiguous_meeting_governance(state, &folder_id)?;
    // Permanent unlock destroys this folder's wrapped-key generation. A salvage finalizer may
    // still need that exact CK to seal output produced after an immediate session relock.
    ensure_no_active_salvage_in_folder(state, &folder_id)?;
    // A source snapshot is plaintext governed only by SQLCipher. Permanent unlock must never clear
    // this folder's per-folder ciphertext generation while such a recovery authority remains.
    // Refuse rather than restore here: opening is user-visible and a recovery decision is separate.
    if state.db.folder_has_pending_filing_sources(&folder_id)? {
        return Err(AppError::Unavailable(
            "resolve pending filing recovery before removing this folder lock".into(),
        ));
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

    // Restore verified image bytes but retain every attachment blob until the same final atomic
    // folder-open commit that clears the other recovery ciphertexts.
    unseal_attachments_in_folder(state, &folder_id, &ck, false)?;

    // Phase 0.5 — permanently restore the TRANSCRIPT + TIMELINE plaintext (clear *_blob columns)
    // and the AUDIO WAV (decrypt .enc → file, drop .enc) under the SAME CK. Never lose audio. The
    // model-gated meeting embedder (Some only when the REAL e5 model is present → never stub vectors)
    // re-indexes the now-open folder's meetings so semantic / related-meetings work again.
    let meeting_embedder = crate::embed::active_persistence_embedder_if_available();
    let sealed_audio_to_retire =
        unseal_folder_extras_permanent(state, &folder_id, &ck, meeting_embedder.as_deref())?;

    // ONE SQL transaction flips the folder OPEN and clears every note/segment/timeline/manual/doc
    // blob. Before this commit, startup can reblank all restored plaintext from those blobs; after
    // it, the folder is intentionally open. No crash point can observe locked + blobless plaintext.
    state.db.commit_folder_permanent_unlock(&folder_id)?;
    {
        let mut g = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        g.remove(&folder_id);
    }

    // Only after the durable `locked=0` commit may readable vault files be recreated. Both meeting
    // and authored-note attachment renderers now pass through the ordinary open-folder gate.
    if let Some(vault) = vault_path(state) {
        let mut seen = std::collections::HashSet::new();
        for n in &notes {
            if !seen.insert(n.meeting_id.clone()) {
                continue;
            }
            let latest = match state.db.get_latest_note_for_meeting(&n.meeting_id)? {
                Some(latest) => latest,
                None => continue,
            };
            let meeting = state.db.get_meeting(&n.meeting_id)?;
            let (title, date) = match meeting {
                Some(meeting) => (
                    meeting.title.clone().unwrap_or_else(|| "Untitled".into()),
                    meeting.started_at.clone(),
                ),
                None => ("Untitled".to_string(), chrono::Utc::now().to_rfc3339()),
            };
            let sub = if folder.path.is_empty() {
                None
            } else {
                Some(folder.path.as_str())
            };
            let exported_markdown =
                match render_markdown_with_attachments_for_export_under_lifecycle_authorized(
                    state,
                    &crate::storage::AttachmentOwner::Meeting {
                        meeting_id: latest.meeting_id.clone(),
                        provider_id: latest.provider_id.clone(),
                    },
                    &latest.markdown,
                    std::path::Path::new(&vault),
                ) {
                    Ok(markdown) => markdown,
                    Err(error) => {
                        tracing::warn!(target: "export", error = %error, "meeting image re-export after permanent unlock failed");
                        continue;
                    }
                };
            if let Ok(path) = crate::export::write_note(
                std::path::Path::new(&vault),
                sub,
                &title,
                &date,
                &exported_markdown,
            ) {
                state.db.set_note_exported_path(
                    &n.meeting_id,
                    &latest.provider_id,
                    &path.to_string_lossy(),
                )?;
                state.db.set_note_exported_hash(
                    &n.meeting_id,
                    &latest.provider_id,
                    Some(&crate::export::note_content_hash(&exported_markdown)),
                )?;
            }
        }
    }
    reexport_notes_in_folder(state, &folder_id);

    // Only now is plaintext the canonical at-rest form. Retiring a redundant ciphertext before
    // `locked=0` would create a crash window where startup still treats the folder as sealed but
    // the DB points at a missing `.enc`. Failure here is space residue, never plaintext exposure or
    // content loss, so the completed permanent unlock remains successful and logs count-only.
    for sealed_path in sealed_audio_to_retire {
        if let Err(error) = crate::crypto::remove_file_verified_absent(
            std::path::Path::new(&sealed_path),
            "retire encrypted audio after permanent unlock",
        ) {
            tracing::warn!(target: "lock", error = %error, "permanent unlock left a redundant encrypted audio file for later cleanup");
        }
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
    state.db.clear_org_folder_closure(&folder_id)?;
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
    let _org_mutation = state.org_share_mutation_lock.lock().await;
    let node =
        discard_unrecoverable_folder_lock_with_enumeration(state.inner(), folder_id, None, || {
            emit_ask_history_invalidated_fail_closed(&app)
        })
        .await?;
    // The discard purged ALL pending audit findings — ping the FE inbox (count-only).
    emit_audit_updated_after_purge(&app, state.inner());
    Ok(node)
}

#[cfg(test)]
pub(crate) async fn discard_unrecoverable_folder_lock_inner(
    state: &AppState,
    folder_id: String,
) -> Result<FolderNode, AppError> {
    discard_unrecoverable_folder_lock_with_enumeration(state, folder_id, None, || {}).await
}

#[cfg(test)]
pub(crate) async fn discard_unrecoverable_folder_lock_with_candidates_for_test(
    state: &AppState,
    folder_id: String,
    candidates: Zeroizing<Vec<[u8; 32]>>,
) -> Result<FolderNode, AppError> {
    discard_unrecoverable_folder_lock_with_enumeration(
        state,
        folder_id,
        Some(Ok(candidates)),
        || {},
    )
    .await
}

/// Execute the destructive-discard state machine with either the production
/// strict Keychain enumeration or an explicit test candidate set. The test seam
/// is needed because `MURMUR_DEV_KEK` deliberately isolates the real Keychain
/// and therefore cannot authoritatively prove that an older Keychain KEK is
/// absent. Production never supplies `injected_enumeration`.
async fn discard_unrecoverable_folder_lock_with_enumeration(
    state: &AppState,
    folder_id: String,
    injected_enumeration: Option<Result<Zeroizing<Vec<[u8; 32]>>, AppError>>,
    visibility_will_change: impl FnOnce(),
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
    let enumeration = if let Some(enumeration) = injected_enumeration {
        enumeration
    } else {
        tokio::task::spawn_blocking(|| {
            crate::secrets::list_master_kek_candidates_strict(
                "Confirm this folder's key is unrecoverable",
            )
        })
        .await
        .map_err(|e| AppError::Auth(format!("kek-enumeration task join failed: {e}")))?
    };
    if !discard_proof_complete(enumeration, &wrapped, &folder_id)? {
        return Err(AppError::InvalidArg(recoverable.into()));
    }

    // ── PROVEN unrecoverable → discard THIS folder's sealed payload ──────────────────────────────
    // Serialize with the rest of the lock state machine (acquired AFTER the awaits above so the guard
    // never crosses a suspend point).
    let _lifecycle = lifecycle_guard(state);
    // Discard changes both the key/content generation and the protection domain. Invalidate any
    // post-await Ask writer before the first attachment/seal mutation.
    bump_seal_epoch(state);
    visibility_will_change();
    // `discard_folder_seal` returns ONLY the SEALED `.enc` audio paths (a never-sealed plaintext WAV
    // is readable content and is preserved, both on disk and in the DB). Best-effort unlink each.
    discard_attachments_in_folder(state, &folder_id)?;
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
    state.db.clear_org_folder_closure(&folder_id)?;
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
    let _org_mutation = state.org_share_mutation_lock.lock().await;
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
    let node =
        discard_unrecoverable_folder_lock_with_enumeration(state.inner(), folder_id, None, || {
            emit_ask_history_invalidated_fail_closed(&app)
        })
        .await?;
    // The discard purged ALL pending audit findings — ping the FE inbox (count-only).
    emit_audit_updated_after_purge(&app, state.inner());
    Ok(Some(node))
}
