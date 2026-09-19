//! Journaled local container moves. The command owns org-mutation then lifecycle admission.
//! The SQLCipher journal is durable before a no-clobber rename. Recovery runs before readers.
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::storage::db::{map_err, Db};

#[derive(Clone)]
struct Container {
    id: String,
    path: String,
    parent: Option<String>,
    kind: String,
    locked: bool,
    reserved: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct MovePlan {
    id: String,
    old_parent: Option<String>,
    parent: Option<String>,
    old_path: String,
    new_path: String,
    level: String,
    vault: Option<PathBuf>,
    export_vault: Option<PathBuf>,
    device: Option<u64>,
    inode: Option<u64>,
    folders: Vec<(String, String, String)>,
}

fn invalid(message: &str) -> AppError {
    AppError::InvalidArg(message.into())
}
fn recovery_error() -> AppError {
    AppError::Storage(
        "container move recovery requires attention; original bytes were preserved".into(),
    )
}
fn unavailable() -> AppError {
    AppError::Locked("container is unavailable; remove its folder lock before moving".into())
}
fn io_error(error: std::io::Error) -> AppError {
    // OS error descriptions do not include user paths or names.
    AppError::Storage(format!(
        "container move filesystem operation failed: {}",
        error.kind()
    ))
}
fn under(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}
fn user_path(path: &str, allow_root: bool) -> Result<()> {
    if path.is_empty() {
        return if allow_root {
            Ok(())
        } else {
            Err(invalid("the workspace root cannot be moved"))
        };
    }
    if path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || path == ".murmur"
        || path.starts_with(".murmur/")
    {
        return Err(invalid("container path must remain inside the user vault"));
    }
    Ok(())
}

/// Reject symlinks in every existing component, including the final component. The directory
/// rename below is exclusive; an unrelated destination is never merged or overwritten.
fn checked_path(vault: &Path, relative: &str) -> Result<PathBuf> {
    user_path(relative, true)?;
    let root = vault.canonicalize().map_err(io_error)?;
    let mut path = root;
    for component in Path::new(relative).components() {
        path.push(component.as_os_str());
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(invalid(
                    "container path contains a non-directory or symbolic link",
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(error)),
        }
    }
    Ok(path)
}

#[cfg(unix)]
fn identity(path: &Path) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).map_err(io_error)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(recovery_error());
    }
    Ok((meta.dev(), meta.ino()))
}
#[cfg(not(unix))]
fn identity(_path: &Path) -> Result<(u64, u64)> {
    Err(AppError::Unavailable(
        "container moves require a supported filesystem".into(),
    ))
}
fn sync_directory(path: &Path) -> Result<()> {
    std::fs::File::open(path)
        .map_err(io_error)?
        .sync_all()
        .map_err(io_error)
}
fn sync_parents(old: &Path, new: &Path) -> Result<()> {
    sync_directory(old.parent().ok_or_else(recovery_error)?)?;
    sync_directory(new.parent().ok_or_else(recovery_error)?)
}

/// macOS renameatx_np(RENAME_EXCL) atomically refuses an occupied destination. Other systems fail
/// closed rather than falling back to std::fs::rename, which overwrites an existing empty folder.
#[cfg(target_os = "macos")]
fn rename_exclusive(old: &Path, new: &Path) -> Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    extern "C" {
        fn renameatx_np(
            from_fd: i32,
            from: *const std::ffi::c_char,
            to_fd: i32,
            to: *const std::ffi::c_char,
            flags: u32,
        ) -> i32;
    }
    let from_parent =
        std::fs::File::open(old.parent().ok_or_else(recovery_error)?).map_err(io_error)?;
    let to_parent =
        std::fs::File::open(new.parent().ok_or_else(recovery_error)?).map_err(io_error)?;
    let from = std::ffi::CString::new(old.file_name().ok_or_else(recovery_error)?.as_bytes())
        .map_err(|_| invalid("invalid container name"))?;
    let to = std::ffi::CString::new(new.file_name().ok_or_else(recovery_error)?.as_bytes())
        .map_err(|_| invalid("invalid container name"))?;
    // SAFETY: directory fds are live and names are NUL-terminated single components.
    let result = unsafe {
        renameatx_np(
            from_parent.as_raw_fd(),
            from.as_ptr(),
            to_parent.as_raw_fd(),
            to.as_ptr(),
            4,
        )
    };
    if result != 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    from_parent.sync_all().map_err(io_error)?;
    to_parent.sync_all().map_err(io_error)
}
#[cfg(not(target_os = "macos"))]
fn rename_exclusive(_old: &Path, _new: &Path) -> Result<()> {
    Err(AppError::Unavailable(
        "container directory moves currently require macOS".into(),
    ))
}

impl Db {
    /// A failed inverse rename can leave canonical paths and filesystem location disagreeing.
    /// Until startup recovery resolves the durable intent, no seal or path mutation may claim
    /// those paths are authoritative. Recovery itself deliberately bypasses this admission.
    pub(crate) fn ensure_container_move_ready(&self) -> Result<()> {
        let pending: bool = self
            .lock()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM container_move_journal)",
                [],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        if pending {
            return Err(AppError::Locked(
                "a container move needs recovery; restart Murmur before changing files or locks"
                    .into(),
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn move_container_fail_inverse_for_test(
        &self,
        id: &str,
        parent: &str,
        vault: &Path,
    ) -> Result<()> {
        self.move_container_local_at_boundary(id, Some(parent), Some(vault), Some("inverse_failed"))
    }
    pub(crate) fn migrate_container_move_journal(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS container_move_journal (
            container_id TEXT PRIMARY KEY,
            old_parent TEXT,
            new_parent TEXT,
            old_path TEXT NOT NULL,
            new_path TEXT NOT NULL,
            intended_level TEXT NOT NULL,
            phase TEXT NOT NULL CHECK(phase IN ('prepared','committed')),
            payload TEXT NOT NULL
        );",
        )
        .map_err(map_err)
    }

    /// Called only with the org mutation and lifecycle guards held. Uses one connection mutex
    /// across validation, journal, rename and DB commit so ordinary writers cannot see half a move.
    pub(crate) fn move_container_local(
        &self,
        id: &str,
        parent: Option<&str>,
        vault: Option<&Path>,
    ) -> Result<()> {
        self.move_container_local_at_boundary(id, parent, vault, None)
    }

    fn move_container_local_at_boundary(
        &self,
        id: &str,
        parent: Option<&str>,
        vault: Option<&Path>,
        stop: Option<&str>,
    ) -> Result<()> {
        self.ensure_container_move_ready()?;
        let source_kind = self.folder_kind(id)?.ok_or_else(unavailable)?;
        let resolved_root;
        let parent = if parent.is_none() && source_kind == "note" {
            resolved_root = self.ensure_notes_root()?;
            Some(resolved_root.as_str())
        } else {
            parent
        };
        let canonical_note_root = self.note_root_id()?;
        let mut conn = self.lock();
        if conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM container_move_journal)",
                [],
                |row| row.get::<_, bool>(0),
            )
            .map_err(map_err)?
        {
            return Err(recovery_error());
        }
        let mut folders = Vec::new();
        {
            let mut statement = conn.prepare("SELECT id,path,parent_id,COALESCE(kind,'meeting'),locked,COALESCE(is_root,0) FROM folders").map_err(map_err)?;
            let rows = statement
                .query_map([], |row| {
                    Ok(Container {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        parent: row.get(2)?,
                        kind: row.get(3)?,
                        locked: row.get(4)?,
                        reserved: row.get(5)?,
                    })
                })
                .map_err(map_err)?;
            for row in rows {
                folders.push(row.map_err(map_err)?);
            }
        }
        let source = folders
            .iter()
            .find(|folder| folder.id == id)
            .ok_or_else(unavailable)?;
        if source.locked {
            return Err(unavailable());
        }
        if source.reserved || !matches!(source.kind.as_str(), "meeting" | "note") {
            return Err(invalid(
                "system and reserved root containers cannot be moved",
            ));
        }
        user_path(&source.path, false)?;
        // Union physical and parent-linked descendants, including malformed legacy parent links.
        // A cycle is refused rather than attempting to repair unrelated data during a move.
        let mut subtree: HashSet<String> = folders
            .iter()
            .filter(|f| under(&f.path, &source.path))
            .map(|f| f.id.clone())
            .collect();
        loop {
            let old_len = subtree.len();
            for f in &folders {
                if f.parent.as_ref().is_some_and(|p| subtree.contains(p)) {
                    subtree.insert(f.id.clone());
                }
            }
            if old_len == subtree.len() {
                break;
            }
        }
        for folder in folders.iter().filter(|f| subtree.contains(&f.id)) {
            if folder.locked {
                return Err(unavailable());
            }
            if folder.reserved
                || !matches!(folder.kind.as_str(), "meeting" | "note")
                || !under(&folder.path, &source.path)
            {
                return Err(invalid("container hierarchy is inconsistent; move refused"));
            }
            assert_ancestry(&folders, &folder.id)?;
        }
        let target = parent
            .map(|pid| folders.iter().find(|f| f.id == pid).ok_or_else(unavailable))
            .transpose()?;
        if let Some(target) = target {
            if target.locked {
                return Err(unavailable());
            }
            if target.reserved && canonical_note_root.as_deref() != Some(target.id.as_str()) {
                return Err(invalid("reserved system container is not a destination"));
            }
            if !matches!(target.kind.as_str(), "meeting" | "note") {
                return Err(invalid("system container is not a destination"));
            }
            user_path(&target.path, true)?;
            if subtree.contains(&target.id) {
                return Err(invalid(
                    "a container cannot move into itself or its descendants",
                ));
            }
            assert_ancestry(&folders, &target.id)?;
        }
        // Ancestors can be locked even when a malformed legacy child row is open.
        for folder in &folders {
            let relevant = subtree.contains(&folder.id)
                || target.is_some_and(|t| t.id == folder.id)
                || (!folder.path.is_empty()
                    && (under(&source.path, &folder.path)
                        || target.is_some_and(|t| under(&t.path, &folder.path))));
            if relevant && folder.locked {
                return Err(unavailable());
            }
            if relevant && conn.query_row("SELECT EXISTS(SELECT 1 FROM org_share_closures WHERE scope_kind='folder' AND scope_id=?1)", [&folder.id], |r| r.get::<_, bool>(0)).map_err(map_err)? {
                return Err(unavailable());
            }
        }
        let leaf = Path::new(&source.path)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| invalid("invalid container path"))?;
        let parent_path = target.map_or("", |f| f.path.as_str());
        let new_path = if parent_path.is_empty() {
            leaf.to_string()
        } else {
            format!("{parent_path}/{leaf}")
        };
        let level = if parent.is_none() && source.kind == "meeting" {
            "project"
        } else {
            "folder"
        };
        if source.parent.as_deref() == parent && source.path == new_path {
            return Err(invalid("the container is already here"));
        }
        let changes: Vec<_> = folders
            .iter()
            .filter(|f| subtree.contains(&f.id))
            .map(|f| {
                (
                    f.id.clone(),
                    f.path.clone(),
                    format!("{}{}", new_path, &f.path[source.path.len()..]),
                )
            })
            .collect();
        for (_, old, new) in &changes {
            user_path(old, false)?;
            user_path(new, false)?;
            if folders
                .iter()
                .any(|f| !subtree.contains(&f.id) && f.path == *new)
            {
                return Err(invalid("a container already occupies that destination"));
            }
        }
        let mut plan = MovePlan {
            id: id.into(),
            old_parent: source.parent.clone(),
            parent: parent.map(str::to_owned),
            old_path: source.path.clone(),
            new_path,
            level: level.into(),
            vault: None,
            export_vault: vault.map(Path::to_path_buf),
            device: None,
            inode: None,
            folders: changes,
        };
        if let Some(vault) = vault {
            let vault = vault.canonicalize().map_err(io_error)?;
            let old = checked_path(&vault, &plan.old_path)?;
            let new = checked_path(&vault, &plan.new_path)?;
            if old != new && new.try_exists().map_err(io_error)? {
                return Err(invalid("a directory already occupies that destination"));
            }
            std::fs::create_dir_all(&old).map_err(io_error)?;
            std::fs::create_dir_all(new.parent().ok_or_else(recovery_error)?).map_err(io_error)?;
            let (device, inode) = identity(&old)?;
            plan.device = Some(device);
            plan.inode = Some(inode);
            plan.vault = Some(vault);
            sync_directory(&old)?;
            sync_parents(&old, &new)?;
        }
        validate_exports(&conn, &plan)?;
        // FULL synchronous means the journal WAL commit reaches stable storage before rename.
        conn.execute_batch("PRAGMA synchronous=FULL")
            .map_err(map_err)?;
        let payload = serde_json::to_string(&plan).map_err(|_| recovery_error())?;
        conn.execute("INSERT INTO container_move_journal(container_id,old_parent,new_parent,old_path,new_path,intended_level,phase,payload) VALUES(?1,?2,?3,?4,?5,?6,'prepared',?7)", params![plan.id, plan.old_parent, plan.parent, plan.old_path, plan.new_path, plan.level, payload]).map_err(map_err)?;
        boundary(stop, "prepared")?;
        if let Some(vault) = &plan.vault {
            let old = checked_path(vault, &plan.old_path)?;
            let new = checked_path(vault, &plan.new_path)?;
            require_identity(&plan, &old)?;
            if old != new {
                if let Err(error) = rename_exclusive(&old, &new) {
                    // An fsync failure may occur AFTER rename. Retain authority unless rollback
                    // state is proven, so neither cleanup nor a later move guesses where bytes are.
                    if old.try_exists().map_err(io_error)? && !new.try_exists().map_err(io_error)? {
                        conn.execute(
                            "DELETE FROM container_move_journal WHERE container_id=?1",
                            [&plan.id],
                        )
                        .map_err(map_err)?;
                    }
                    return Err(error);
                }
            }
        }
        boundary(stop, "moved")?;
        if let Err(error) = commit_plan(&mut conn, &plan) {
            if stop != Some("inverse_failed") && rollback_files(&plan).is_ok() {
                conn.execute(
                    "DELETE FROM container_move_journal WHERE container_id=?1",
                    [&plan.id],
                )
                .map_err(map_err)?;
            }
            return Err(error);
        }
        boundary(stop, "committed")?;
        boundary(stop, "acknowledgement")?;
        conn.execute(
            "DELETE FROM container_move_journal WHERE container_id=?1",
            [&plan.id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Before lock-at-rest reconciliation, AppState, UI and MCP readers. Ambiguous occupancy or
    /// changed inode fails graceful startup; the journal and every byte remain available to repair.
    pub(crate) fn recover_container_moves(&self) -> Result<()> {
        let mut conn = self.lock();
        let rows: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare("SELECT phase,payload FROM container_move_journal ORDER BY container_id")
                .map_err(map_err)?;
            let values = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(map_err)?;
            values
                .collect::<std::result::Result<_, _>>()
                .map_err(map_err)?
        };
        for (phase, payload) in rows {
            let plan: MovePlan = serde_json::from_str(&payload).map_err(|_| recovery_error())?;
            let current: (String, Option<String>) = conn
                .query_row(
                    "SELECT path,parent_id FROM folders WHERE id=?1",
                    [&plan.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(map_err)?;
            let db_old = current.0 == plan.old_path && current.1 == plan.old_parent;
            let db_new = current.0 == plan.new_path && current.1 == plan.parent;
            if (phase == "prepared" && !db_old) || (phase == "committed" && !db_new) {
                return Err(recovery_error());
            }
            if let Some(vault) = &plan.vault {
                let old = checked_path(vault, &plan.old_path)?;
                let new = checked_path(vault, &plan.new_path)?;
                let old_exists = old.try_exists().map_err(io_error)?;
                let new_exists = new.try_exists().map_err(io_error)?;
                if old == new {
                    require_identity(&plan, &old)?;
                } else {
                    match (old_exists, new_exists) {
                        (true, false) if phase == "prepared" => {
                            require_identity(&plan, &old)?;
                            // No filesystem commit: cancel the prepared intent.
                            conn.execute(
                                "DELETE FROM container_move_journal WHERE container_id=?1",
                                [&plan.id],
                            )
                            .map_err(map_err)?;
                            continue;
                        }
                        (true, false) if phase == "committed" => {
                            // SQLite is already canonical at the new location. Complete the
                            // identity-bound rename; never roll DB paths back after its commit.
                            require_identity(&plan, &old)?;
                            rename_exclusive(&old, &new)?;
                        }
                        (false, true) => {
                            require_identity(&plan, &new)?;
                            sync_parents(&old, &new)?;
                        }
                        _ => return Err(recovery_error()),
                    }
                }
            }
            if phase == "prepared" {
                commit_plan(&mut conn, &plan)?;
            }
            conn.execute(
                "DELETE FROM container_move_journal WHERE container_id=?1",
                [&plan.id],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }
}

fn assert_ancestry(folders: &[Container], id: &str) -> Result<()> {
    let mut next = Some(id);
    let mut seen = HashSet::new();
    while let Some(id) = next {
        if !seen.insert(id) {
            return Err(invalid("container hierarchy contains a cycle"));
        }
        let folder = folders
            .iter()
            .find(|f| f.id == id)
            .ok_or_else(|| invalid("container hierarchy has a missing parent"))?;
        if folder.locked {
            return Err(unavailable());
        }
        next = folder.parent.as_deref();
    }
    Ok(())
}
fn require_identity(plan: &MovePlan, path: &Path) -> Result<()> {
    let (device, inode) = identity(path)?;
    if plan.device != Some(device) || plan.inode != Some(inode) {
        return Err(recovery_error());
    }
    Ok(())
}
fn rollback_files(plan: &MovePlan) -> Result<()> {
    if let Some(vault) = &plan.vault {
        let old = checked_path(vault, &plan.old_path)?;
        let new = checked_path(vault, &plan.new_path)?;
        if old != new {
            require_identity(plan, &new)?;
            rename_exclusive(&new, &old)?;
        }
    }
    Ok(())
}
fn boundary(stop: Option<&str>, phase: &str) -> Result<()> {
    if stop == Some(phase) {
        return Err(AppError::Storage(
            "injected container move interruption".into(),
        ));
    }
    Ok(())
}

fn export_prefixes(plan: &MovePlan) -> Vec<(String, String)> {
    let mut prefixes = vec![(format!("{}/", plan.old_path), format!("{}/", plan.new_path))];
    for vault in [&plan.export_vault, &plan.vault].into_iter().flatten() {
        let pair = (
            format!("{}/", vault.join(&plan.old_path).display()),
            format!("{}/", vault.join(&plan.new_path).display()),
        );
        if !prefixes.contains(&pair) {
            prefixes.push(pair);
        }
    }
    prefixes
}

fn validate_exports(conn: &Connection, plan: &MovePlan) -> Result<()> {
    let prefixes = export_prefixes(plan);
    for (id, _, _) in &plan.folders {
        let mut stmt = conn.prepare("SELECT exported_path FROM documents WHERE folder_id=?1 AND exported_path IS NOT NULL UNION ALL SELECT exported_path FROM notes WHERE (folder_id=?1 OR meeting_id IN(SELECT id FROM meetings WHERE folder_id=?1)) AND exported_path IS NOT NULL").map_err(map_err)?;
        let rows = stmt
            .query_map([id], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        for row in rows {
            let path = row.map_err(map_err)?;
            if !prefixes.iter().any(|(old, _)| path.starts_with(old))
                || Path::new(&path)
                    .components()
                    .any(|c| matches!(c, Component::ParentDir))
            {
                return Err(invalid(
                    "a container export is outside its directory; move refused",
                ));
            }
        }
    }
    Ok(())
}

fn commit_plan(conn: &mut Connection, plan: &MovePlan) -> Result<()> {
    let tx = conn.transaction().map_err(map_err)?;
    for (id, old, new) in &plan.folders {
        if tx
            .execute(
                "UPDATE folders SET path=?3 WHERE id=?1 AND path=?2",
                params![id, old, new],
            )
            .map_err(map_err)?
            != 1
        {
            return Err(recovery_error());
        }
    }
    if tx
        .execute(
            "UPDATE folders SET parent_id=?2,level=?3 WHERE id=?1",
            params![plan.id, plan.parent, plan.level],
        )
        .map_err(map_err)?
        != 1
    {
        return Err(recovery_error());
    }
    // Rewrite only anchored prefixes, never replace() inside filenames. Both legacy relative and
    // current absolute exports are supported; the Unicode offset is SQLite character-based.
    for (old, new) in export_prefixes(plan) {
        let chars = old.chars().count() as i64;
        // Physical directory ownership, not a possibly stale provider folder_id, determines
        // which export moved. Rewrite every canonical reference to those exact managed paths.
        tx.execute("UPDATE documents SET exported_path=?1 || substr(exported_path,?2+1) WHERE substr(exported_path,1,?2)=?3", params![new, chars, old]).map_err(map_err)?;
        tx.execute("UPDATE notes SET exported_path=?1 || substr(exported_path,?2+1) WHERE substr(exported_path,1,?2)=?3", params![new, chars, old]).map_err(map_err)?;
    }
    tx.execute(
        "UPDATE container_move_journal SET phase='committed' WHERE container_id=?1",
        [&plan.id],
    )
    .map_err(map_err)?;
    tx.commit().map_err(map_err)
}

#[cfg(test)]
mod tests;
