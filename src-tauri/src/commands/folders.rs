//! FOLDERS command surface — create/list/rename/delete of meeting & note folders, extracted VERBATIM
//! from `commands` (God-file split, a PURE MOVE — every gate / verify-before-destroy / vault-assert
//! body is UNCHANGED, only relocated). This is the folder-CRUD domain: `list_folders` (the session-
//! unlock-aware folder tree), `create_folder`, `rename_folder`, `delete_folder`, plus the folder-CRUD-
//! only helpers (`build_folder_tree`, `reprefix_descendant_folder_paths`,
//! `reexport_notes_under_subtree`, `move_note_file_to_root`, `reparent_authored_notes_to_default`,
//! `move_authored_note_md_to_folder`).
//!
//! LOCK-MODEL (byte-identical to the pre-move form): `list_folders` folds the LIVE session unlock set
//! into every per-folder note count (a sealed-not-unlocked folder's notes never inflate the count).
//! `create_folder`/`rename_folder` route every on-disk path through `super::assert_in_vault` (D5
//! containment) and never touch a sealed blob / wrapped key (a locked-folder rename is metadata-only).
//! `delete_folder` is fail-closed: it REFUSES a non-empty subtree, REFUSES a sealed-NOT-unlocked
//! folder (`AppError::Locked` — no CK to unseal, never orphan encrypted content), and for a
//! session-unlocked locked folder PERMANENTLY unseals via `super::remove_lock_inner` (KEK → CK →
//! decrypt every note/transcript/timeline/audio back to plaintext + re-export) BEFORE demoting its
//! notes to the vault root — authored notes are REPARENTED to the default folder, never destroyed.
//! Every note move is copy-then-remove (never loses bytes). `rename_folder_inner`/`delete_folder_inner`
//! hold `super::lifecycle_guard` across the whole op so no concurrent lock/move can interleave.
//!
//! The MEETING move-into-folder command (`move_note`) + its auto-file / salvage seal machinery
//! (`move_into_locked_folder`, `seal_moved_note`, `classify_auto_file_target`, `seal_auto_filed_note`,
//! `finalize_salvage_lock_state`, `AutoFileTarget`, `ensure_meeting_folder_target`) deliberately STAY
//! in `commands/mod.rs` — they are pipeline-called seal machinery, not folder CRUD. Every gate/seal/
//! vault helper (`lifecycle_guard`, `remove_lock_inner`, `vault_path`, `assert_in_vault`,
//! `write_note_to_vault`, the `crate::export` facade) STAYS in `commands/mod.rs` (or its crate module)
//! — this module reads them through `use super::*` (a `commands` submodule sees its parent's private
//! items). The moved `rename_folder_inner`/`delete_folder_inner` cores + `build_folder_tree` stay
//! `pub(crate)` so the sibling `commands/notes.rs` and the STAYING test modules keep calling them via
//! the `pub use folders_commands::*;` re-export. Every symbol keeps its EXACT prior body + signature;
//! nothing changed except its file — no gate/seal body changed, only relocation.

use super::*;

/// Build the folder tree (roots → children) from the flat folder list + per-folder note counts +
/// the current session unlock set.
#[tauri::command]
pub fn list_folders(state: State<'_, AppState>) -> Result<Vec<FolderNode>, AppError> {
    let folders = state.db.list_folders()?;
    let unlocked = {
        state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
            .clone()
    };
    // Gated by the session unlock set — a sealed-and-not-unlocked folder's notes must not
    // contribute to note_count (see count_notes_per_folder doc + .claude/rules/lock-model.md).
    let counts = state.db.count_notes_per_folder(&unlocked)?;
    let kinds = state.db.folder_kinds()?;
    let levels = state.db.folder_levels()?;
    let folders = flatten_projects_for_legacy_tree(folders, &levels);
    Ok(build_folder_tree(&folders, &counts, &unlocked, &kinds))
}

/// Present the folder forest to LEGACY consumers exactly as it looked before the workspace
/// hierarchy existed: project rows are removed and their children are re-rooted.
///
/// The hierarchy migration gives every former root folder a project parent. Without this, the
/// shipped sidebar would render one root — the project — with every folder beneath it, and
/// `MeetingsSidebarTreeComponent` filters note-kind folders at the TOP LEVEL ONLY
/// (`folders.tree().filter(n => n.kind !== "note")`), so every note folder would leak straight into
/// the Meetings tree. That is verbatim the folder leak already fixed once on 2026-07-14.
///
/// Keeping this transform out of `build_folder_tree` (rather than teaching it about levels) means
/// the tree builder, its signature and its existing test stay byte-identical, and the shim is one
/// obvious function for the frontend cutover to delete.
pub(crate) fn flatten_projects_for_legacy_tree(
    folders: Vec<Folder>,
    levels: &std::collections::HashMap<String, String>,
) -> Vec<Folder> {
    let is_project = |id: &str| levels.get(id).map(String::as_str) == Some("project");
    folders
        .into_iter()
        .filter(|f| !is_project(&f.id))
        .map(|mut f| {
            if f.parent_id.as_deref().is_some_and(is_project) {
                f.parent_id = None;
            }
            f
        })
        .collect()
}

/// The workspace project a parentless creation belongs to, or an error.
///
/// `Db::workspace_project_id` returns an Option because a database can genuinely
/// lack one — before the hierarchy migration, or if it failed. Treating that as
/// "then create without a parent" reintroduces exactly what the parent is for: the
/// tree renders from the projects down, so a container with no project above it is
/// invisible, and so is everything the user puts in it. Refusing is recoverable;
/// an unreachable container full of notes is not.
pub(crate) fn require_workspace_project(state: &AppState) -> Result<String, AppError> {
    state.db.workspace_project_id()?.ok_or_else(|| {
        AppError::Storage(
            "this workspace has no default project yet — restart Murmur to finish setting it up"
                .into(),
        )
    })
}

/// Refuse any path-composing operation on a container whose path IS the vault root.
///
/// The hierarchy migration gives the default project `path = ''` — the first row in the app's
/// history whose path is the vault root. Without a refusal, a rename would compose
/// `fs::rename(<vault>, <vault>/<new name>)` and MOVE THE USER'S ENTIRE VAULT, and a delete would
/// target the vault directory itself.
///
/// `assert_in_vault` deliberately RESOLVES an empty relative path to the vault root — a documented
/// behaviour with its own test — so the refusal has to happen before a container reaches it. The
/// complete set of call sites is enumerated below rather than left to each future author to
/// rediscover; anything added later that composes filesystem work from a container it resolved by id
/// belongs on that list.
///
/// The complete set of places that compose filesystem work from a resolved container's OWN path,
/// and what each does with the vault-root row:
///   - `rename_folder_inner` (the subtree `fs::rename`, and `reexport_notes_under_subtree` beneath
///     it) — refused here, before anything is touched;
///   - `delete_folder_inner` (`fs::remove_dir`) — refused here;
///   - `move_note_folder_inner`'s `src`/`dst` — unreachable: it resolves through
///     `Db::note_folder_by_id`, which excludes any container whose path is the vault root. That
///     holds however such a row arrived — this migration only ever creates a meeting-kind project
///     there, but an import could produce a note-kind one, and the resolver refuses both.
///   - `write_note_to_vault` — takes a NOTE container's path, which is rooted under the note root
///     and falls back to `"Notes"`, so it is never empty;
///   - `remove_lock_inner`'s re-export and `move_note_file` — both already map an empty path to
///     `None`, which correctly means "the vault root" for a note that belongs nowhere else.
fn refuse_vault_root_container(folder: &Folder) -> Result<(), AppError> {
    if folder.path.is_empty() {
        return Err(AppError::InvalidArg(
            "the workspace root cannot be renamed or deleted".into(),
        ));
    }
    Ok(())
}

/// Assemble `FolderNode` roots (parent_id == None) and recurse children. Sealed-but-session-
/// unlocked folders carry `unlocked = true`.
pub(crate) fn build_folder_tree(
    folders: &[Folder],
    counts: &std::collections::HashMap<String, usize>,
    unlocked: &std::collections::HashSet<String>,
    kinds: &std::collections::HashMap<String, String>,
) -> Vec<FolderNode> {
    fn node(
        f: &Folder,
        folders: &[Folder],
        counts: &std::collections::HashMap<String, usize>,
        unlocked: &std::collections::HashSet<String>,
        kinds: &std::collections::HashMap<String, String>,
    ) -> FolderNode {
        let children = folders
            .iter()
            .filter(|c| c.parent_id.as_deref() == Some(f.id.as_str()))
            .map(|c| node(c, folders, counts, unlocked, kinds))
            .collect();
        FolderNode {
            id: f.id.clone(),
            name: f.name.clone(),
            parent_id: f.parent_id.clone(),
            note_count: counts.get(&f.id).copied().unwrap_or(0),
            locked: f.locked,
            unlocked: f.locked && unlocked.contains(&f.id),
            kind: kinds
                .get(&f.id)
                .cloned()
                .unwrap_or_else(|| "meeting".to_string()),
            children,
        }
    }
    folders
        .iter()
        .filter(|f| f.parent_id.is_none())
        .map(|f| node(f, folders, counts, unlocked, kinds))
        .collect()
}

/// Create a folder under an optional parent. The vault-relative path is derived from the parent
/// path + the sanitized folder name; the matching vault subdirectory is created on disk.
#[tauri::command]
pub fn create_folder(
    state: State<'_, AppState>,
    name: String,
    parent_id: Option<String>,
) -> Result<Folder, AppError> {
    create_folder_inner(state.inner(), name, parent_id)
}

/// Create a first-class peer Space. The row and its vault-relative directory either both become
/// visible or the freshly-created row is removed by the same bounded undo used for folders.
#[tauri::command]
pub fn create_space(state: State<'_, AppState>, name: String) -> Result<Folder, AppError> {
    create_space_inner(state.inner(), name)
}

pub(crate) fn create_space_inner(state: &AppState, name: String) -> Result<Folder, AppError> {
    let _lifecycle = lifecycle_guard(state);
    let clean = crate::summarize::organize::sanitize_folder(&name)
        .ok_or_else(|| AppError::InvalidArg("space name is empty or invalid".into()))?;
    if state
        .db
        .user_folder_path_exists_case_insensitive(&clean)?
    {
        return Err(AppError::InvalidArg(
            "a Space with this name already exists".into(),
        ));
    }
    let dir = match vault_path(state) {
        Some(vault) => Some(assert_in_vault(
            std::path::Path::new(&vault),
            std::path::Path::new(&clean),
        )?),
        None => None,
    };
    let space = Folder {
        id: uuid::Uuid::new_v4().to_string(),
        name: clean.clone(),
        path: clean,
        parent_id: None,
        locked: false,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    state.db.insert_space(&space)?;
    create_container_dir_or_undo(state, &space.id, dir.as_deref())?;
    Ok(space)
}

/// Inner of [`create_folder`] taking `&AppState`, so the real creation path can be exercised by a
/// test rather than approximated by calling its name sanitiser directly.
pub(crate) fn create_folder_inner(
    state: &AppState,
    name: String,
    parent_id: Option<String>,
) -> Result<Folder, AppError> {
    let clean = crate::summarize::organize::sanitize_folder(&name)
        .ok_or_else(|| AppError::InvalidArg("folder name is empty or invalid".into()))?;

    if parent_id.as_deref() == Some(crate::storage::tasks_store::TASK_FOLDER_ID) {
        return Err(AppError::InvalidArg("the task folder is internal".into()));
    }

    // A creation with no explicit parent belongs to the workspace project. The tree renders from the
    // projects down, so a container left parentless would exist and be unreachable. The project
    // occupies the vault root, so the composed path below is byte-identical to what a root container
    // receives today.
    let parent_id = match parent_id {
        Some(pid) => Some(pid),
        // Fail closed. A database with no workspace project is one the hierarchy
        // migration never finished on, and creating here anyway would produce the
        // unreachable container — invisible in a tree rendered from the projects
        // down, with everything the user puts in it.
        None => Some(require_workspace_project(state)?),
    };
    // What the parent's seal requires of this child — refuses a sealed parent whose key is not
    // available this session. Resolved BEFORE anything is written or created on disk.
    // The parent is resolved unconditionally above, so the gate is too. An `Option` here
    // would only be a leftover from when a creation could genuinely have no parent.
    let parent_seal = container_parent_seal(
        state,
        parent_id
            .as_deref()
            .expect("a creation always resolves a parent"),
    )?;

    // Resolve the parent's vault-relative path (if any) and compose the child path.
    let parent_path = match parent_id.as_deref() {
        Some(pid) => {
            let parent = state
                .db
                .folder_by_id(pid)?
                .ok_or_else(|| AppError::InvalidArg(format!("no parent folder {pid}")))?;
            Some(parent.path)
        }
        None => None,
    };
    let rel_path = match &parent_path {
        Some(p) if !p.is_empty() => format!("{p}/{clean}"),
        _ => clean.clone(),
    };

    // Create the vault subdirectory (best-effort but surfaced): only when a vault is configured.
    // D5: canonicalize + assert the composed dir stays inside the vault root before any mkdir.
    // The path is VALIDATED here and the directory is created at the very end, only
    // once the container is durably in the state it will be observed in.
    //
    // Creating it first is what made the undo hard: a failed seal then had to remove
    // a directory that the seal itself may have written into, `remove_dir` is
    // non-recursive so it fails on any residue, and removing recursively would risk
    // deleting something that was never ours. Ordering it last removes the whole
    // question — there is nothing to undo, because nothing was made. A row without a
    // directory is recoverable (the export path creates one on demand); plaintext
    // inside a sealed tree is not.
    let dir: Option<std::path::PathBuf> = match vault_path(state) {
        Some(vault) => Some(assert_in_vault(
            std::path::Path::new(&vault),
            std::path::Path::new(&rel_path),
        )?),
        None => None,
    };

    let folder = Folder {
        id: uuid::Uuid::new_v4().to_string(),
        name: clean,
        path: rel_path,
        parent_id,
        locked: false,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    if matches!(parent_seal, ParentSeal::SealChild) {
        // BORN sealed, in one statement. The parent is sealed and its key is available for
        // this session, which is the only reason the caller may proceed at all — so the
        // child's own key is minted and wrapped BEFORE the row exists, and the row is
        // written already locked.
        //
        // This is deliberately not "insert, then run the ordinary lock path". That is two
        // writes with a window between them, and the window IS the state this guard exists
        // to prevent: an open container inside a sealed one, with the caller told the
        // creation failed. No undo closes it — an undo that itself fails leaves the row
        // behind — so the window is not opened. What the ordinary path would add here is
        // nothing: its remaining work enumerates notes, documents, attachments and audio,
        // and a container that did not exist a moment ago has none.
        let wrapped = wrapped_key_for_new_sealed_container(state, &folder.id)?;
        state.db.insert_sealed_folder(&folder, &wrapped)?;
        create_container_dir_or_undo(state, &folder.id, dir.as_deref())?;
        // Report what the database now holds, not the value built before sealing.
        return Ok(Folder {
            locked: true,
            ..folder
        });
    }
    state.db.insert_folder(&folder)?;
    create_container_dir_or_undo(state, &folder.id, dir.as_deref())?;
    Ok(folder)
}

/// The components of `dir` that do NOT exist yet, shallowest first, bounded by `floor`.
///
/// Recorded BEFORE `create_dir_all` runs, because afterwards there is no way to tell what it
/// made from what was already there. An earlier version inferred it — remove every empty
/// ancestor — and that was wrong twice over: an empty directory on this path may well have
/// existed before the call (a user's own empty folder), and emptiness is not a stopping rule,
/// so on a vault holding nothing else the walk climbed to the vault itself.
fn components_to_create(dir: &std::path::Path, floor: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut missing = Vec::new();
    let mut current = dir;
    while current != floor && current.starts_with(floor) {
        if current.exists() {
            break;
        }
        missing.push(current.to_path_buf());
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    missing.reverse();
    missing
}

/// Remove exactly the directories this call created, deepest first.
///
/// A component that is ABSENT is not a failure — `create_dir_all` stops at the one that broke,
/// so the deepest recorded component is usually the one that was never made, and treating that
/// as a reason to stop would strand every component above it. Anything else that refuses to be
/// removed does stop the walk, and correctly: a non-recursive `remove_dir` fails on a directory
/// that now holds something, and a directory that holds something is no longer only ours.
///
/// Best-effort within that: what survives is an empty directory no row addresses, so no export
/// and no path composition can reach it.
fn remove_created_dirs(created: &[std::path::PathBuf]) {
    for path in created.iter().rev() {
        match std::fs::remove_dir(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return,
        }
    }
}

/// Materialise a container's vault directory, once the container itself is settled — and
/// remove the row if that fails.
///
/// The directory is created last so a failed SEAL has nothing on disk to undo. That trade
/// is only sound if the last step is itself undoable: `create_dir_all` fails on an
/// existing regular file at the path, a permissions change, or a full disk, and returning
/// then would leave a container the user was told was not created. Which half fails
/// changes nothing about what the caller was promised.
pub(crate) fn create_container_dir_or_undo(
    state: &AppState,
    folder_id: &str,
    dir: Option<&std::path::Path>,
) -> Result<(), AppError> {
    let Some(dir) = dir else {
        return Ok(());
    };
    let vault = vault_path(state);
    let floor = vault.as_deref().map(std::path::Path::new);
    // What this call is ABOUT to create, recorded while the answer is still knowable.
    let created = floor
        .map(|floor| components_to_create(dir, floor))
        .unwrap_or_default();
    let Err(create_error) = std::fs::create_dir_all(dir) else {
        return Ok(());
    };
    let create_error = AppError::Export(format!("create folder dir failed: {create_error}"));
    remove_created_dirs(&created);
    // If the container was born sealed, its wrapped content key is a COLUMN on the row being
    // deleted, so removing the row disposes of it — there is no separate key store to clean
    // up. Nothing was added to the session unlock set either: the born-sealed path writes the
    // row and nothing else, and never touches that set.
    //
    // A database and a filesystem cannot be made atomic with each other, so this removal is
    // best-effort and says so when it fails. What that leaves is not the hazard this step
    // addresses: a row that survives is either SEALED — in which case it is exactly as
    // protected as any other sealed container — or OPEN beneath an OPEN parent, which is an
    // ordinary empty folder. Neither is an open container inside a sealed one, which is the
    // state the ordering above makes unreachable.
    match state.db.delete_freshly_created_folder(folder_id) {
        Ok(()) => Err(create_error),
        Err(undo_error) => {
            tracing::error!(
                target: "storage",
                error = %undo_error,
                "could not remove a container whose directory could not be created"
            );
            Err(AppError::Storage(format!(
                "a container was created but its folder could not be made, and removing the \
                 container failed ({undo_error})"
            )))
        }
    }
}

/// Rename a folder: change its display `name` (and the matching vault subdirectory + every governed
/// `path`) without ever touching sealed content.
///
/// Steps, ordered so a crash never loses content:
///  1. Sanitize the new name (same component-safe rule as `create_folder`; reject `/`, `..`, NUL).
///  2. Recompose this folder's vault-relative path = parent path + sanitized name.
///  3. If a vault is configured, MOVE the on-disk subdir `old_path` → `new_path` (best-effort rename;
///     a missing source is fine). The dir holds only the OPEN folder's plaintext `.md`s — sealed
///     folders keep their `.md`s deleted, so a locked-folder rename just renames an empty/absent dir.
///  4. Update the `folders` row (name + path) and re-prefix the path of EVERY descendant folder, and
///     re-point EVERY affected note's `exported_path` from `old_path/...` → `new_path/...`. Sealed
///     notes have `exported_path = NULL` and are skipped — a LOCKED folder rename is metadata-only and
///     never reaches the sealed blob / wrapped key (no decrypt, no re-seal).
///
/// Idempotent-ish: renaming to the same (sanitized) name is a no-op move + a column rewrite to the
/// same values.
#[tauri::command]
pub fn rename_folder(
    state: State<'_, AppState>,
    folder_id: String,
    new_name: String,
) -> Result<Folder, AppError> {
    rename_folder_inner(state.inner(), folder_id, new_name)
}

/// Inner of [`rename_folder`] taking `&AppState` (so tests can drive it without a `tauri::State`).
/// Holds the [`AppState::lifecycle`] guard across the whole rename (path rewrites the seal/unseal
/// lifecycle keys FS ops off — see the command doc).
pub(crate) fn rename_folder_inner(
    state: &AppState,
    folder_id: String,
    new_name: String,
) -> Result<Folder, AppError> {
    if folder_id == crate::storage::tasks_store::TASK_FOLDER_ID {
        return Err(AppError::InvalidArg("the task folder is internal".into()));
    }
    // BLK-1: serialize with the rest of the lock state machine. A rename never decrypts, but it
    // rewrites `path` columns that the seal/unseal lifecycle keys vault FS ops off — hold the guard
    // so it can't interleave with a concurrent lock/unlock/remove that also rewrites paths.
    let _lifecycle = lifecycle_guard(state);

    let clean = crate::summarize::organize::sanitize_folder(&new_name)
        .ok_or_else(|| AppError::InvalidArg("folder name is empty or invalid".into()))?;

    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    refuse_vault_root_container(&folder)?;
    ensure_folder_subtree_unlocked(state, &folder_id)?;
    let old_path = folder.path.clone();

    // Recompose this folder's path from its PARENT's path + the new sanitized name.
    let parent_path = match folder.parent_id.as_deref() {
        Some(pid) => state.db.folder_by_id(pid)?.map(|p| p.path),
        None => None,
    };
    let new_path = match parent_path.as_deref() {
        Some(p) if !p.is_empty() => format!("{p}/{clean}"),
        _ => clean.clone(),
    };

    // No-op fast path: same path AND same name → nothing to move/rewrite.
    if new_path == old_path && clean == folder.name {
        return Ok(Folder {
            name: clean,
            path: new_path,
            ..folder
        });
    }

    // Move the on-disk vault subdir, if a vault is configured. Both ends are containment-checked.
    // `std::fs::rename` moves the WHOLE subtree (including descendant `.md`s) in one atomic op.
    let mut vault_configured = false;
    if new_path != old_path {
        if let Some(vault) = vault_path(state) {
            vault_configured = true;
            let vault_root = std::path::Path::new(&vault);
            // Destination must stay inside the vault; the source is an existing in-vault dir.
            let dest = assert_in_vault(vault_root, std::path::Path::new(&new_path))?;
            let src = assert_in_vault(vault_root, std::path::Path::new(&old_path))?;
            if src.exists() {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        AppError::Export(format!("create rename parent dir failed: {e}"))
                    })?;
                }
                // A plain rename within the same vault is atomic on the same filesystem.
                std::fs::rename(&src, &dest)
                    .map_err(|e| AppError::Export(format!("rename folder dir failed: {e}")))?;
            } else {
                // Source absent (a locked folder's dir, or never materialized): ensure the
                // destination exists so future plaintext `.md`s land in the renamed dir.
                std::fs::create_dir_all(&dest).map_err(|e| {
                    AppError::Export(format!("create renamed folder dir failed: {e}"))
                })?;
            }
        }
    }

    // Rewrite the DB: this folder's name+path, then re-prefix every DESCENDANT folder's path. Order
    // doesn't risk content loss — no markdown/blob column is touched; only path strings move.
    state.db.rename_folder(&folder_id, &clean, &new_path)?;
    if new_path != old_path {
        reprefix_descendant_folder_paths(state, &folder_id, &new_path)?;
        // Re-derive every governed note's `exported_path` to point under its (possibly renamed)
        // folder's NEW on-disk dir. We rebuild from the file basename + the folder's new dir rather
        // than swapping path prefixes — robust to `/var` vs `/private/var` canonicalization drift in
        // the stored absolute path. The `fs::rename` already moved the bytes; this only re-points the
        // DB. Sealed notes (NULL exported_path) are skipped. Walks this folder + the whole subtree.
        if vault_configured {
            reexport_notes_under_subtree(state, &folder_id)?;
        }
    }

    Ok(Folder {
        name: clean,
        path: new_path,
        ..folder
    })
}

/// A parent-directory rename moves the complete vault subtree and rewrites descendant export paths,
/// so every folder in that subtree must be session-visible before any content-bearing path row is
/// read. Metadata-only locked renames are refused; unlock first.
fn ensure_folder_subtree_unlocked(state: &AppState, folder_id: &str) -> Result<(), AppError> {
    if !folder_is_unlocked(state, folder_id)? {
        return Err(AppError::Locked(
            "unlock this folder before renaming it".into(),
        ));
    }
    for child in state.db.child_folders(folder_id)? {
        ensure_folder_subtree_unlocked(state, &child.id)?;
    }
    Ok(())
}

/// Recursively re-prefix the vault-relative `path` of every DESCENDANT folder of `folder_id` to sit
/// under `new_prefix` after the folder itself was renamed. Walks the tree one level at a time via
/// [`Db::child_folders`]; each child's recomposed path is `new_prefix` + the child's own name (so the
/// rewrite is structural, not a brittle string-replace). Does NOT touch the child's `name`, lock
/// state, or any note content — only the `path` column (the descendants' notes are re-pointed by the
/// single absolute-dir swap in the caller, since `fs::rename` moved the whole subtree at once).
fn reprefix_descendant_folder_paths(
    state: &AppState,
    folder_id: &str,
    new_prefix: &str,
) -> Result<(), AppError> {
    for child in state.db.child_folders(folder_id)? {
        let child_old = child.path.clone();
        let child_new = if new_prefix.is_empty() {
            child.name.clone()
        } else {
            format!("{new_prefix}/{}", child.name)
        };
        if child_new != child_old {
            state.db.rename_folder(&child.id, &child.name, &child_new)?;
        }
        // Recurse into this child's own subtree.
        reprefix_descendant_folder_paths(state, &child.id, &child_new)?;
    }
    Ok(())
}

/// After a folder rename moved the on-disk subtree, re-point the `exported_path` of every governed
/// note in `folder_id` AND its descendants to its folder's NEW vault dir. Each note's new path is
/// `<vault>/<folder.path>/<basename of the old exported_path>` (the `fs::rename` preserved the
/// filename). Rebuilding from the basename (not a string-prefix swap on the stored absolute path) is
/// robust to canonicalization drift (`/var` vs `/private/var`) and to where the original export wrote
/// the path. Sealed notes carry `exported_path = NULL` and are skipped. Requires a configured vault.
fn reexport_notes_under_subtree(state: &AppState, folder_id: &str) -> Result<(), AppError> {
    let Some(vault) = vault_path(state) else {
        return Ok(());
    };
    let vault_root = std::path::Path::new(&vault);

    let folder = match state.db.folder_by_id(folder_id)? {
        Some(f) => f,
        None => return Ok(()),
    };
    // The folder's NEW absolute dir (containment-checked).
    let new_dir = assert_in_vault(vault_root, std::path::Path::new(&folder.path))?;

    for n in state.db.notes_in_folder(folder_id)? {
        let Some(old) = n.exported_path else {
            continue; // sealed note (no .md) — nothing to re-point.
        };
        let Some(name) = std::path::Path::new(&old).file_name() else {
            continue;
        };
        let new_path = new_dir.join(name);
        state.db.set_note_exported_path(
            &n.meeting_id,
            &n.provider_id,
            &new_path.to_string_lossy(),
        )?;
    }

    // AUTHORED notes live in a different table with their own `exported_path`, and this walk
    // used to skip them entirely — so after a rename every one of them named a file that had
    // just been moved. The seal removes plaintext BY RECORDED PATH ONLY
    // (`note_exported_path_rows_in_folder` → `remove_note_export_if_unchanged`), with no
    // directory sweep behind it, so it removed nothing and the real `.md` stayed readable
    // inside a sealed folder. That is the 2026-07-11 NOTES-2 leak, on the path that never
    // called its fix.
    //
    // Rebuilt from basename + the folder's new directory, exactly like the meeting notes
    // above, which stays correct under `/var` vs `/private/var` canonicalisation drift in the
    // stored absolute path.
    for (document_id, old) in state.db.note_exported_path_rows_in_folder(folder_id)? {
        let Some(name) = std::path::Path::new(&old).file_name() else {
            continue;
        };
        let new_path = new_dir.join(name);
        state
            .db
            .set_note_doc_exported_path(&document_id, Some(&new_path.to_string_lossy()))?;
    }

    // Recurse into descendant folders (their dirs moved with the same single `fs::rename`).
    for child in state.db.child_folders(folder_id)? {
        reexport_notes_under_subtree(state, &child.id)?;
    }
    Ok(())
}

/// Delete a folder, NEVER losing a note. SECURITY-CRITICAL — a folder may hold notes and may be
/// sealed (LOCKED). Rules, fail-closed:
///
///  - **Has child folders →** REJECT (`InvalidArg`). The FE deletes leaf-first; refusing here keeps
///    a subtree from being silently orphaned (a child's `parent_id` would dangle).
///  - **LOCKED + NOT session-unlocked →** REJECT (`AppError::Locked`). We have no CK to unseal the
///    folder's notes, so deleting the row would orphan encrypted-and-unrecoverable content (the
///    wrapped key lives on the row we'd delete). Tell the user to unlock first.
///  - **LOCKED + SESSION-UNLOCKED →** PERMANENTLY remove the lock first (`remove_lock_inner`:
///    KEK → unwrap CK → decrypt every note/transcript/timeline/audio back to plaintext, re-export the
///    `.md`, clear the blobs, flip the folder open). Only then does it become the OPEN case below, so
///    nothing is ever left encrypted-and-orphaned.
///  - **OPEN (now) →** move every note to the vault ROOT (`folder_id = NULL`), delete the folder row,
///    and remove the (now-empty) vault subdir. Notes survive at "All notes".
#[tauri::command]
pub async fn delete_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    folder_id: String,
) -> Result<(), AppError> {
    let _share_mutation = state.lock_org_mutation().await;
    delete_folder_inner(state.inner(), folder_id)?;
    emit_ask_history_invalidated_fail_closed(&app);
    Ok(())
}

/// Inner of [`delete_folder`] taking `&AppState` (so tests can drive it without a `tauri::State`).
/// See the command doc for the fail-closed rules.
pub(crate) fn delete_folder_inner(state: &AppState, folder_id: String) -> Result<(), AppError> {
    if folder_id == crate::storage::tasks_store::TASK_FOLDER_ID {
        return Err(AppError::InvalidArg("the task folder is internal".into()));
    }
    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    refuse_vault_root_container(&folder)?;

    // Refuse a non-empty SUBTREE — never orphan child folders by dangling their parent_id.
    if !state.db.child_folders(&folder_id)?.is_empty() {
        return Err(AppError::InvalidArg(
            "this folder has subfolders — delete or move them first".into(),
        ));
    }

    // A canonical-NULL legacy meeting can still have provider rows split across this folder and a
    // sibling. Rehoming it through `set_meeting_folder(None)` would synchronize EVERY provider row,
    // detaching the sibling from its own (possibly sealed) content key. Refuse before lock removal,
    // DB placement changes or filesystem moves; the user must first file the meeting explicitly.
    if state
        .db
        .folder_has_ambiguous_meeting_governance(&folder_id)?
    {
        return Err(AppError::Locked(
            "this folder contains a legacy meeting assigned to multiple locations — move the meeting to one folder before deleting it"
                .into(),
        ));
    }

    // If sealed, it MUST be session-unlocked so we can unseal its notes back to plaintext before the
    // folder row (which carries the wrapped key) is destroyed. Otherwise refuse — never orphan
    // sealed content.
    if folder.locked {
        let session_unlocked = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
            .contains(&folder_id);
        if !session_unlocked {
            return Err(AppError::Locked(
                "unlock this folder first to delete it (its notes are sealed)".into(),
            ));
        }
    }

    // A prior failed relation replay leaves this exact folder as a retry shell. Check only after
    // the lock gate (the journal's existence is itself private lifecycle state), but before
    // `remove_lock_inner` or any other mutation.
    super::trash_commands::refuse_pending_recovery_journal(state, &folder_id)?;

    if folder.locked {
        // Permanently unseal back to plaintext + re-export the `.md`s, then the folder is OPEN.
        // remove_lock_inner takes the lifecycle guard itself (so we do NOT hold it across this call —
        // the std Mutex is non-reentrant and would self-deadlock).
        remove_lock_inner(state, folder_id.clone())?;
    }

    // OPEN folder now (or was open all along): move its notes to the vault ROOT, then drop the row.
    // Serialize the reassign + row delete + FS cleanup under the lifecycle guard so it can't race a
    // concurrent lock/move on the same folder.
    let _lifecycle = lifecycle_guard(state);
    ensure_no_active_salvage_in_folder(state, &folder_id)?;

    // Rehome EVERY meeting governed by this folder, including a newly filed recording that has no
    // provider note yet. Enumerating only `notes_in_folder` made that pre-note row keep a dangling
    // canonical `meetings.folder_id` after the folder disappeared, hiding it behind the fail-closed
    // read gate. The note rows below are retained only to locate an optional exported Markdown file.
    let notes = state.db.notes_in_folder(&folder_id)?;
    let meeting_ids = state.db.meeting_ids_in_folder(&folder_id)?;

    // TRASH CAPTURE — before ANY rehoming, because the member ids are what lets a restore put the
    // contents back, and they stop being discoverable the moment the first meeting is re-filed.
    //
    // Re-read the folder row: `remove_lock_inner` above may have cleared `locked`/`wrapped_key`, and
    // the snapshot must record the row as it now is. Note what this cannot recover: a sealed
    // container's LOCK is permanently removed by the delete (it must be, or its content would be
    // orphaned from its key), so a restored folder comes back OPEN — `FolderSnapshot::was_locked`
    // carries that fact to the FE instead of letting the user assume the lock survived.
    let snapshot_row = state.db.folder_by_id(&folder_id)?.unwrap_or(folder.clone());
    let kind = state
        .db
        .folder_kind(&folder_id)?
        .unwrap_or_else(|| "meeting".to_string());
    let authored_note_ids = state.db.note_ids_in_folder(&folder_id)?;
    super::trash_commands::capture_folder(
        state,
        &snapshot_row,
        &kind,
        &meeting_ids,
        &authored_note_ids,
    )?;

    for meeting_id in meeting_ids {
        // Reassign the canonical owner and every provider row atomically to the root.
        state.db.set_meeting_folder(&meeting_id, None)?;
        // Best-effort move of the plaintext `.md` to the vault root (only when one exists).
        if let Some(src_path) = notes
            .iter()
            .find(|note| note.meeting_id == meeting_id)
            .and_then(|note| note.exported_path.clone())
        {
            if let Some(vault) = vault_path(state) {
                move_note_file_to_root(state, &meeting_id, &src_path, &vault)?;
            }
        }
    }

    // NOTES-1 (2026-07-11 audit, CRITICAL data loss): AUTHORED notes (`documents(kind='note')`)
    // must be REPARENTED to the default note-folder, NOT destroyed — the FE promises "delete folder"
    // MOVES its notes to the default folder. The pre-fix path left them for `Db::delete_folder`'s
    // blanket `DELETE FROM documents`, permanently deleting authored notes. `Db::delete_folder` now
    // REFUSES if any authored note still references the folder, so this reparent MUST run first.
    reparent_authored_notes_to_default(state, &folder_id)?;

    // Delete the folder row, then remove the (now note-free) vault subdir. Row first: a leftover
    // empty dir is harmless/reconcilable; a dangling row is not.
    bump_seal_epoch(state);
    state.db.delete_folder(&folder_id)?;
    if let Some(vault) = vault_path(state) {
        let vault_root = std::path::Path::new(&vault);
        if let Ok(dir) = assert_in_vault(vault_root, std::path::Path::new(&folder.path)) {
            // remove_dir (not _all): only an EMPTY dir is removed, so a stray user file is never
            // clobbered. The notes' `.md`s were moved out above, so the dir should be empty.
            let _ = std::fs::remove_dir(&dir);
        }
    }
    Ok(())
}

/// Move a meeting's plaintext `.md` to the vault ROOT (copy-then-remove, never losing bytes) and
/// re-point its `exported_path`. A `&AppState`-only twin of [`move_note_file`] (whose `&State`
/// signature can't be reached from the `_inner` delete path). Used when deleting a folder demotes its
/// notes to "All notes".
fn move_note_file_to_root(
    state: &AppState,
    meeting_id: &str,
    src_path: &str,
    vault: &str,
) -> Result<(), AppError> {
    let src = std::path::Path::new(src_path);
    let bytes = match std::fs::read_to_string(src) {
        Ok(b) => b,
        // Source already gone → nothing to move; the DB association is already NULL.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(AppError::Export(format!("read note for move failed: {e}"))),
    };
    let file_name = src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| AppError::Export("note path has no filename".into()))?;
    let vault_root = std::path::Path::new(vault);
    let dest = assert_in_vault(vault_root, std::path::Path::new(file_name))?;
    let src_canon = src.canonicalize().unwrap_or_else(|_| src.to_path_buf());
    if dest == src_canon || dest == src {
        return Ok(()); // already at the root.
    }
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| AppError::Export(format!("create move dir failed: {e}")))?;
    }
    // Write the destination atomically, THEN remove the source (never lose bytes).
    // Export-collision guard: bytes move verbatim → the `exported_hash` baseline is deliberately
    // left alone (see `move_note_file` — re-stamping from moved bytes would erase the
    // external-edit signal).
    crate::export::overwrite_note(&dest, &bytes)?;
    let _ = std::fs::remove_file(src);
    if let Some(existing) = state.db.get_latest_note_for_meeting(meeting_id)? {
        state.db.set_note_exported_path(
            meeting_id,
            &existing.provider_id,
            &dest.to_string_lossy(),
        )?;
    }
    Ok(())
}

/// NOTES-1 (2026-07-11 audit, CRITICAL data loss) — reparent every AUTHORED note
/// (`documents(kind='note')`) in a to-be-deleted folder to the DEFAULT note-folder ("Notes"), moving
/// its plaintext `.md` into the default folder's vault subdir (copy-then-remove — never lose bytes)
/// and rewriting `documents.exported_path`. The FE's "delete folder" promises its notes MOVE to the
/// default folder; the pre-fix path left them for `Db::delete_folder`'s blanket `DELETE`, destroying
/// them. Runs AFTER any sealed folder was unsealed back to plaintext (`remove_lock_inner`), so the
/// notes here are plaintext. If the folder BEING deleted IS the default note-folder itself, its notes
/// can't reparent to themselves — REFUSE rather than risk destroying them (the FE never offers to
/// delete the root "Notes" folder). Best-effort FS move: a note's bytes live in the DB (canonical),
/// so a failed `.md` move never loses content — but a missing target keeps the reparent (the row moves
/// regardless). No PII in logs (ids only).
fn reparent_authored_notes_to_default(state: &AppState, folder_id: &str) -> Result<(), AppError> {
    let note_ids = state.db.note_ids_in_folder(folder_id)?;
    if note_ids.is_empty() {
        return Ok(());
    }
    // The RESERVED note root — the one `lock_folder` refuses to seal — not
    // `ensure_default_note_folder`, which returns whatever row holds the path "Notes",
    // creating it if absent and returning it EVEN WHEN IT IS SEALED. On a database where the
    // user has locked a container at that path, resolving through it filed these notes into a
    // sealed container and exported their plaintext into its directory: unsealed content
    // inside a sealed tree, which the at-rest re-blank sweep never visits because it keys off
    // the container being marked locked.
    //
    // Orphaned notes belong in the always-open home for unfiled notes, which is exactly what
    // the reserved root is for.
    let default_id = state.db.ensure_notes_root()?;
    if default_id == folder_id {
        // Deleting the default note-folder while it still holds authored notes: reparenting to
        // itself is a no-op and the row-delete would then destroy them. Fail closed.
        return Err(AppError::InvalidArg(
            "cannot delete the default \"Notes\" folder while it holds notes — move them first"
                .into(),
        ));
    }
    let default_folder = state
        .db
        .note_folder_by_id(&default_id)?
        .ok_or_else(|| AppError::Storage("default note-folder missing after ensure".into()))?;
    for id in &note_ids {
        // Reassign the row to the reserved note root (the gate/seal anchor). That root is
        // structurally open — `lock_folder` refuses to seal an `is_root` container — so a plain
        // reassign is correct and no reseal is needed. This used to say the same thing about
        // "the default folder", which was a claim about a resolver that did not honour it.
        state.db.set_note_doc_folder(id, &default_id)?;
        // Move the plaintext `.md` into the default folder's vault subdir + re-point exported_path.
        if let Some(row) = state.db.get_note_row(id)? {
            move_authored_note_md_to_folder(state, &row, &default_folder)?;
        }
    }
    tracing::info!(
        target: "notes",
        folder_id = %folder_id,
        moved = note_ids.len(),
        "reparented authored notes to the default folder before folder delete"
    );
    Ok(())
}

/// Move ONE authored note's plaintext `.md` from its old export path into `target` note-folder's
/// vault subdir (copy-then-remove — never lose bytes) and re-point `documents.exported_path`. A
/// `&AppState`-only helper for the `_inner` delete path (which can't reach the `&State`-signature
/// export helpers). No-op when there is no vault, no old export, or the source file is already gone
/// (the DB row is the canonical copy; a re-export recreates the `.md`).
fn move_authored_note_md_to_folder(
    state: &AppState,
    row: &crate::storage::db::NoteRow,
    target: &NoteFolder,
) -> Result<(), AppError> {
    let Some(vault) = vault_path(state) else {
        // No vault → nothing on disk to move; still clear the stale export path so a later lock
        // never chases it.
        state.db.set_note_doc_exported_path(&row.id, None)?;
        return Ok(());
    };
    let vault_root = std::path::Path::new(&vault);
    // Read the source bytes (if any). A missing source → nothing to move; clear the path.
    let bytes = match row.exported_path.as_deref() {
        Some(src_path) => match std::fs::read_to_string(src_path) {
            Ok(b) => Some((src_path.to_string(), b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(AppError::Export(format!("read note for move failed: {e}"))),
        },
        None => None,
    };
    let Some((src_path, content)) = bytes else {
        // No on-disk file → just re-export from the (canonical) DB text into the new folder if we
        // have text; otherwise clear the stale path.
        if row.text.is_empty() {
            state.db.set_note_doc_exported_path(&row.id, None)?;
        } else if let Some(p) = write_note_to_vault(state, row)? {
            let _ = p; // write_note_to_vault already re-points exported_path.
        }
        return Ok(());
    };
    // Compose the destination inside the target folder's vault subdir, D5-contained.
    let file_name = std::path::Path::new(&src_path)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| AppError::Export("note path has no filename".into()))?;
    let rel = std::path::Path::new(&target.path).join(file_name);
    let dest = assert_in_vault(vault_root, &rel)?;
    let src_canon = std::path::Path::new(&src_path)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&src_path));
    if dest == src_canon {
        // Already at the destination (nothing to move) — just record the path.
        state
            .db
            .set_note_doc_exported_path(&row.id, Some(&dest.to_string_lossy()))?;
        return Ok(());
    }
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| AppError::Export(format!("create move dir failed: {e}")))?;
    }
    // Write the destination atomically, THEN remove the source (never lose bytes).
    // Export-collision guard: bytes move verbatim → the `exported_hash` baseline is deliberately
    // left alone (see `move_note_file` — re-stamping from moved bytes would erase the
    // external-edit signal).
    crate::export::overwrite_note(&dest, &content)?;
    let _ = std::fs::remove_file(&src_path);
    state
        .db
        .set_note_doc_exported_path(&row.id, Some(&dest.to_string_lossy()))?;
    Ok(())
}
