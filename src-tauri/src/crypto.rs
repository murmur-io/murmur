//! Application-layer AES-256-GCM for the per-folder lock (Layer 2, distinct from the whole-DB
//! SQLCipher layer). A locked folder's note markdown is encrypted under a per-folder content key
//! (CK); each CK is wrapped under a master KEK that is released only by biometric. Cell format:
//! `nonce(12) || ciphertext || tag(16)`, stored as a BLOB.
//!
//! ## Associated data (AAD) context-binding — B7/B8
//!
//! Every blob is now AEAD-bound to its STORAGE CONTEXT via AES-GCM additional authenticated data
//! (a wrapped CK to its `folder_id`, a content blob to `folder_id|meeting_id|provider_id|…`, audio
//! to `meeting_id|folder_id`). AAD is authenticated-but-not-encrypted: it never appears on disk and
//! does NOT change the cell format (`nonce || ct+tag` is byte-identical in size) — it only changes
//! the GCM tag. Binding the context defeats a "swap a ciphertext from folder A into folder B" or
//! "replay a different meeting's audio" attack: decryption under the WRONG context fails the tag
//! check and returns [`AppError::Locked`].
//!
//! ## BACKWARD-COMPATIBILITY — MANDATORY (never brick existing folders)
//!
//! Existing locked folders' wrapped-keys / content blobs / audio `.enc` were written BEFORE AAD
//! existed (empty AAD). Because an AAD-bound blob and a legacy no-AAD blob are byte-indistinguishable
//! on disk, [`decrypt`] tries the supplied AAD FIRST and, only if that fails AND the AAD is non-empty,
//! falls back to empty AAD (the legacy form). A successful legacy decrypt is reported to the caller
//! (`AadUsed::Legacy`) so it can RE-BIND the blob (re-encrypt with the real AAD) on the next write.
//! This makes the migration lazy and lossless: an old folder still unlocks, and re-binds itself the
//! first time it is re-sealed. A blob whose context was tampered/swapped fails BOTH the bound and the
//! legacy attempt (the legacy attempt only succeeds for genuinely-pre-AAD blobs, never for a blob
//! bound to a DIFFERENT context) → fails closed.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::{AppError, Result};

const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

// Darwin's O_NOFOLLOW. Murmur is macOS-first; other Unix test targets retain the pre/post-open
// identity checks and omit the platform-specific flag rather than guessing another ABI value.
#[cfg(target_os = "macos")]
const NOFOLLOW_FLAG: i32 = 0x0000_0100;
#[cfg(not(target_os = "macos"))]
const NOFOLLOW_FLAG: i32 = 0;

const ATOMIC_STAGE_MARKER: &str = ".murmur-crypto-";

fn storage_error(operation: &str, error: impl std::fmt::Display) -> AppError {
    AppError::Storage(format!("{operation}: {error}"))
}

/// Open and read one app-owned regular file without following a final-component symlink. The
/// identity/length checks make a concurrent pathname substitution fail closed. Audio artifacts
/// must have one name: otherwise sealing one pathname could leave another plaintext hard link.
fn read_owned_file(path: &Path, operation: &str) -> Result<Vec<u8>> {
    let path_before = std::fs::symlink_metadata(path).map_err(|e| storage_error(operation, e))?;
    if path_before.file_type().is_symlink() || !path_before.is_file() || path_before.nlink() != 1 {
        return Err(AppError::Storage(format!(
            "{operation}: audio artifact is not an owned single-link regular file"
        )));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(NOFOLLOW_FLAG)
        .open(path)
        .map_err(|e| storage_error(operation, e))?;
    let opened = file.metadata().map_err(|e| storage_error(operation, e))?;
    if opened.dev() != path_before.dev()
        || opened.ino() != path_before.ino()
        || opened.len() != path_before.len()
        || opened.nlink() != 1
    {
        return Err(AppError::Storage(format!(
            "{operation}: audio artifact identity changed while opening"
        )));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(opened.len()).map_err(|_| {
        AppError::Storage(format!(
            "{operation}: audio artifact is too large to address"
        ))
    })?);
    file.read_to_end(&mut bytes)
        .map_err(|e| storage_error(operation, e))?;
    let after = file.metadata().map_err(|e| storage_error(operation, e))?;
    let path_after = std::fs::symlink_metadata(path).map_err(|e| storage_error(operation, e))?;
    if after.dev() != opened.dev()
        || after.ino() != opened.ino()
        || after.len() != opened.len()
        || after.nlink() != 1
        || path_after.file_type().is_symlink()
        || path_after.dev() != opened.dev()
        || path_after.ino() != opened.ino()
        || path_after.len() != opened.len()
        || bytes.len() as u64 != opened.len()
    {
        return Err(AppError::Storage(format!(
            "{operation}: audio artifact changed while reading"
        )));
    }
    Ok(bytes)
}

fn sync_parent(path: &Path, operation: &str) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        AppError::Storage(format!("{operation}: destination has no parent directory"))
    })?;
    File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| storage_error(operation, e))
}

struct AtomicStage {
    path: PathBuf,
    file: File,
    device: u64,
    inode: u64,
    published: bool,
}

impl Drop for AtomicStage {
    fn drop(&mut self) {
        if !self.published {
            // A failed decrypt must not strand plaintext in a `.part` until the next launch. Keep
            // a stable handle, truncate+sync it first (works even if parent permissions prevent
            // unlink), then remove only when the path still names this exact inode.
            let _ = self.file.set_len(0);
            let _ = self.file.sync_all();
            if let Ok(named) = std::fs::symlink_metadata(&self.path) {
                if !named.file_type().is_symlink()
                    && named.is_file()
                    && named.dev() == self.device
                    && named.ino() == self.inode
                    && named.nlink() == 1
                {
                    let _ = std::fs::remove_file(&self.path);
                    let _ = sync_parent(&self.path, "sync failed audio crypto staging cleanup");
                }
            }
        }
    }
}

impl AtomicStage {
    /// Erase and unlink the exact inode retained by this guard after it has been renamed to the
    /// published path. Every durability/identity failure is collected so the caller can return a
    /// composite error instead of silently relying on [`Drop`].
    fn cleanup_failed_publish(&mut self, operation: &str) -> Result<()> {
        let mut failures = Vec::new();

        if let Err(error) = self.file.set_len(0) {
            failures.push(format!("truncate exact published inode failed: {error}"));
        }
        if let Err(error) = self.file.sync_all() {
            failures.push(format!("sync truncated published inode failed: {error}"));
        }

        match self.file.metadata() {
            Ok(metadata)
                if metadata.dev() == self.device
                    && metadata.ino() == self.inode
                    && metadata.len() == 0 => {}
            Ok(_) => failures.push(
                "exact published inode did not verify as the retained zero-length inode".into(),
            ),
            Err(error) => {
                failures.push(format!("verify truncated published inode failed: {error}"))
            }
        }

        match std::fs::symlink_metadata(&self.path) {
            Ok(named)
                if !named.file_type().is_symlink()
                    && named.is_file()
                    && named.dev() == self.device
                    && named.ino() == self.inode
                    && named.nlink() == 1 => {
                if let Err(error) = std::fs::remove_file(&self.path) {
                    failures.push(format!("unlink exact published inode failed: {error}"));
                }
            }
            Ok(_) => failures.push(
                "published pathname no longer names the retained single-link inode; refusing to unlink"
                    .into(),
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => failures.push(format!(
                "inspect published pathname before unlink failed: {error}"
            )),
        }

        match self.file.metadata() {
            Ok(metadata)
                if metadata.dev() == self.device
                    && metadata.ino() == self.inode
                    && metadata.len() == 0
                    && metadata.nlink() == 0 => {}
            Ok(_) => failures
                .push("exact published inode was not proven zero-length and fully unlinked".into()),
            Err(error) => failures.push(format!(
                "verify exact published inode after unlink failed: {error}"
            )),
        }

        match std::fs::symlink_metadata(&self.path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                failures.push(format!("verify published pathname absence failed: {error}"))
            }
            Ok(_) => failures.push("published pathname remains readable after cleanup".into()),
        }
        if let Err(error) = sync_parent(
            &self.path,
            "sync failed audio crypto published-file cleanup",
        ) {
            failures.push(error.to_string());
        }
        match std::fs::symlink_metadata(&self.path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => failures.push(format!(
                "re-verify published pathname absence after parent sync failed: {error}"
            )),
            Ok(_) => failures
                .push("published pathname became readable again during durable cleanup".into()),
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(AppError::Storage(format!(
                "{operation}: failed published-file cleanup could not be proven: {}",
                failures.join("; ")
            )))
        }
    }
}

/// Same-directory, private, crash-durable publish. The final path is reopened without following a
/// symlink and compared byte-for-byte before success. A crash before rename leaves only an
/// unmistakable `.part` file; startup removes those untracked stages before exposing the library.
fn durable_atomic_write(dest: &Path, bytes: &[u8], operation: &str) -> Result<()> {
    let parent = dest.parent().ok_or_else(|| {
        AppError::Storage(format!("{operation}: destination has no parent directory"))
    })?;
    let target_name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AppError::Storage(format!("{operation}: invalid destination name")))?;

    let mut stage = None;
    for _ in 0..8 {
        let candidate = parent.join(format!(
            ".{target_name}{ATOMIC_STAGE_MARKER}{}.part",
            uuid::Uuid::new_v4()
        ));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(NOFOLLOW_FLAG)
            .open(&candidate)
        {
            Ok(file) => {
                stage = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(storage_error(operation, error)),
        }
    }
    let (stage_path, mut stage_file) = stage.ok_or_else(|| {
        AppError::Storage(format!(
            "{operation}: could not allocate a unique staging file"
        ))
    })?;
    let created_meta = stage_file
        .metadata()
        .map_err(|e| storage_error(operation, e))?;
    let mut cleanup = AtomicStage {
        path: stage_path.clone(),
        file: stage_file
            .try_clone()
            .map_err(|e| storage_error(operation, e))?,
        device: created_meta.dev(),
        inode: created_meta.ino(),
        published: false,
    };

    stage_file
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|e| storage_error(operation, e))?;
    stage_file
        .write_all(bytes)
        .and_then(|()| stage_file.sync_all())
        .map_err(|e| storage_error(operation, e))?;
    let staged_meta = stage_file
        .metadata()
        .map_err(|e| storage_error(operation, e))?;
    if staged_meta.nlink() != 1
        || staged_meta.len() != bytes.len() as u64
        || staged_meta.permissions().mode() & 0o777 != 0o600
    {
        return Err(AppError::Storage(format!(
            "{operation}: staged file failed private identity verification"
        )));
    }
    drop(stage_file);
    if read_owned_file(&stage_path, operation)? != bytes {
        return Err(AppError::Storage(format!(
            "{operation}: staged file failed byte-for-byte verification"
        )));
    }

    std::fs::rename(&stage_path, dest).map_err(|e| storage_error(operation, e))?;
    // The cleanup capability follows the exact inode through rename. Keep it armed until the
    // published pathname has passed final readback AND the directory entry is durable; a failure
    // in either step must truncate/remove a decrypted plaintext destination while its `.enc`
    // source still exists.
    cleanup.path = dest.to_path_buf();

    let publish_result = (|| -> Result<()> {
        let final_meta =
            std::fs::symlink_metadata(dest).map_err(|e| storage_error(operation, e))?;
        if final_meta.file_type().is_symlink()
            || final_meta.dev() != staged_meta.dev()
            || final_meta.ino() != staged_meta.ino()
            || final_meta.nlink() != 1
            || final_meta.permissions().mode() & 0o777 != 0o600
            || read_owned_file(dest, operation)? != bytes
        {
            return Err(AppError::Storage(format!(
                "{operation}: published file failed durable identity verification"
            )));
        }
        sync_parent(dest, operation)
    })();

    match publish_result {
        Ok(()) => {
            cleanup.published = true;
            Ok(())
        }
        Err(publish_error) => {
            let cleanup_result = cleanup.cleanup_failed_publish(operation);
            // Cleanup above was explicit and its result is preserved below. Do not let Drop perform
            // a second best-effort attempt whose suppressed failures could obscure that result.
            cleanup.published = true;
            match cleanup_result {
                Ok(()) => Err(publish_error),
                Err(cleanup_error) => Err(AppError::Storage(format!(
                    "{operation}: post-rename failure ({publish_error}); cleanup failure ({cleanup_error})"
                ))),
            }
        }
    }
}

/// Remove a sensitive file and prove the pathname is absent before the caller changes any DB
/// pointer or reports a seal as complete. NotFound is idempotent success; every other outcome is a
/// hard error. A successful unlink is parent-directory-synced before returning.
pub(crate) fn remove_file_verified_absent(path: &Path, operation: &str) -> Result<()> {
    let before = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(storage_error(operation, error)),
    };
    if before.file_type().is_symlink() || !before.is_file() || before.nlink() != 1 {
        return Err(AppError::Storage(format!(
            "{operation}: sensitive artifact is not an owned single-link regular file"
        )));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(NOFOLLOW_FLAG)
        .open(path)
        .map_err(|e| storage_error(operation, e))?;
    let opened = file.metadata().map_err(|e| storage_error(operation, e))?;
    if opened.dev() != before.dev() || opened.ino() != before.ino() || opened.nlink() != 1 {
        return Err(AppError::Storage(format!(
            "{operation}: sensitive artifact identity changed while opening"
        )));
    }
    let named = std::fs::symlink_metadata(path).map_err(|e| storage_error(operation, e))?;
    if named.file_type().is_symlink()
        || named.dev() != opened.dev()
        || named.ino() != opened.ino()
        || named.nlink() != 1
    {
        return Err(AppError::Storage(format!(
            "{operation}: sensitive pathname no longer names the opened inode"
        )));
    }
    std::fs::remove_file(path).map_err(|e| storage_error(operation, e))?;
    let after = file.metadata().map_err(|e| storage_error(operation, e))?;
    if after.dev() != opened.dev() || after.ino() != opened.ino() || after.nlink() != 0 {
        return Err(AppError::Storage(format!(
            "{operation}: exact sensitive inode did not lose its final name"
        )));
    }
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => sync_parent(path, operation),
        Err(error) => Err(storage_error(operation, error)),
        Ok(_) => Err(AppError::Storage(format!(
            "{operation}: sensitive pathname was recreated during unlink"
        ))),
    }
}

/// Validate a managed plaintext export against Murmur's canonical digest without changing the
/// filesystem. Missing files are idempotent success; a symlink, hard link, identity race, length
/// mismatch, or byte mismatch fails closed. Destructive multi-file operations use this as a full
/// preflight before they remove the first member of a governed export set.
pub(crate) fn verify_file_content(
    path: &Path,
    expected_len: Option<u64>,
    expected_sha256: &[u8; 32],
    operation: &str,
) -> Result<()> {
    use sha2::{Digest, Sha256};

    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(storage_error(operation, error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.nlink() != 1 {
        return Err(AppError::Storage(format!(
            "{operation}: managed export is not an owned single-link regular file"
        )));
    }
    if expected_len.is_some_and(|expected| expected != metadata.len()) {
        return Err(AppError::Storage(format!(
            "{operation}: managed export was changed outside Murmur; preserving it"
        )));
    }
    let bytes = read_owned_file(path, operation)?;
    if Sha256::digest(&bytes).as_slice() != expected_sha256 {
        return Err(AppError::Storage(format!(
            "{operation}: managed export was changed outside Murmur; preserving it"
        )));
    }
    Ok(())
}

/// Remove one managed plaintext export only when the exact named inode still has the canonical
/// length + SHA-256 recorded by Murmur. The pathname is first atomically moved to a private sibling,
/// then the displaced inode is opened without following symlinks and verified. A mismatch is moved
/// back (or left at the private sibling when another writer already recreated the original name) and
/// returns an error, so callers can abort a seal before publishing the locked state without losing an
/// external edit. A matching quarantine is removed through [`remove_file_verified_absent`].
pub(crate) fn remove_file_verified_content(
    path: &Path,
    expected_len: Option<u64>,
    expected_sha256: &[u8; 32],
    operation: &str,
) -> Result<()> {
    use sha2::{Digest, Sha256};

    let before = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(storage_error(operation, error)),
    };
    if before.file_type().is_symlink() || !before.is_file() || before.nlink() != 1 {
        return Err(AppError::Storage(format!(
            "{operation}: managed export is not an owned single-link regular file"
        )));
    }
    if expected_len.is_some_and(|expected| expected != before.len()) {
        return Err(AppError::Storage(format!(
            "{operation}: managed export was changed outside Murmur; preserving it"
        )));
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AppError::Storage(format!("{operation}: export name is not valid UTF-8")))?;
    let quarantine = path.with_file_name(format!(
        ".{file_name}.murmur-remove-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::rename(path, &quarantine).map_err(|error| storage_error(operation, error))?;

    let restore = |reason: &str| -> Result<()> {
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::rename(&quarantine, path)
                    .map_err(|error| storage_error(operation, error))?;
                sync_parent(path, operation)?;
            }
            Err(error) => return Err(storage_error(operation, error)),
            Ok(_) => {
                // Another writer recreated the canonical name. Keep the displaced bytes at the
                // private sibling rather than overwrite either version; the caller remains open.
                sync_parent(&quarantine, operation)?;
            }
        }
        Err(AppError::Storage(format!("{operation}: {reason}")))
    };

    let opened = match OpenOptions::new()
        .read(true)
        .custom_flags(NOFOLLOW_FLAG)
        .open(&quarantine)
    {
        Ok(file) => file,
        Err(error) => return restore(&format!("quarantined export could not be opened: {error}")),
    };
    let metadata = match opened.metadata() {
        Ok(metadata) => metadata,
        Err(error) => return restore(&format!("quarantined export could not be inspected: {error}")),
    };
    if metadata.dev() != before.dev()
        || metadata.ino() != before.ino()
        || metadata.nlink() != 1
        || expected_len.is_some_and(|expected| expected != metadata.len())
    {
        return restore("managed export identity changed during quarantine");
    }
    let mut bytes = Vec::with_capacity(metadata.len().min(64 * 1024 * 1024) as usize);
    let mut reader = opened;
    if let Err(error) = reader.read_to_end(&mut bytes) {
        return restore(&format!("quarantined export could not be read: {error}"));
    }
    if Sha256::digest(&bytes).as_slice() != expected_sha256 {
        return restore("managed export was changed outside Murmur; preserving it");
    }

    remove_file_verified_absent(&quarantine, operation)?;
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(storage_error(operation, error)),
        Ok(_) => Err(AppError::Storage(format!(
            "{operation}: managed export was recreated during removal; preserving the new file"
        ))),
    }
}

/// Prove a named artifact is a stable, app-owned regular file without reading its potentially huge
/// contents. Used when a relock only needs to establish that its already-verified `.enc` twin still
/// exists before deleting the session plaintext.
pub(crate) fn owned_regular_file_exists(path: &Path, operation: &str) -> Result<bool> {
    let named = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(storage_error(operation, error)),
    };
    if named.file_type().is_symlink() || !named.is_file() || named.nlink() != 1 {
        return Err(AppError::Storage(format!(
            "{operation}: artifact is not an owned single-link regular file"
        )));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(NOFOLLOW_FLAG)
        .open(path)
        .map_err(|e| storage_error(operation, e))?;
    let opened = file.metadata().map_err(|e| storage_error(operation, e))?;
    if opened.dev() != named.dev()
        || opened.ino() != named.ino()
        || opened.len() != named.len()
        || opened.nlink() != 1
    {
        return Err(AppError::Storage(format!(
            "{operation}: artifact identity changed while opening"
        )));
    }
    Ok(true)
}

/// Remove untracked atomic stages left by a crash. The name is deliberately narrow and every
/// removal is verified; callers invoke this only after the process-wide instance lock is held.
pub(crate) fn sweep_atomic_stages(
    dir: &Path,
    expected_targets: &std::collections::HashSet<String>,
) -> Result<usize> {
    let mut removed = 0usize;
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(storage_error(
                "inspect audio crypto staging directory",
                error,
            ))
        }
    };
    for entry in entries {
        let entry = entry.map_err(|e| storage_error("inspect audio crypto staging entry", e))?;
        let name = match entry.file_name().to_str() {
            Some(name) => name.to_owned(),
            None => continue,
        };
        let Some(body) = name.strip_prefix('.') else {
            continue;
        };
        let Some((target, stage_suffix)) = body.rsplit_once(ATOMIC_STAGE_MARKER) else {
            continue;
        };
        let Some(stage_id) = stage_suffix.strip_suffix(".part") else {
            continue;
        };
        if target.is_empty() || uuid::Uuid::parse_str(stage_id).is_err() {
            continue;
        }
        if !expected_targets.contains(target) {
            return Err(AppError::Storage(
                "audio crypto staging artifact has no matching locked-audio target".into(),
            ));
        }
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|e| storage_error("inspect audio crypto staging artifact", e))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.nlink() != 1 {
            return Err(AppError::Storage(
                "audio crypto staging artifact has an ambiguous identity".into(),
            ));
        }
        remove_file_verified_absent(&path, "remove abandoned audio crypto staging artifact")?;
        removed += 1;
    }
    Ok(removed)
}

/// Which AAD form successfully decrypted a blob — so callers can lazily RE-BIND legacy (pre-AAD)
/// blobs to their real context on the next write (see module docs, B7/B8 backward-compat).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AadUsed {
    /// Decrypted with the caller-supplied context AAD (already bound — nothing to migrate).
    Bound,
    /// Decrypted with empty AAD — a legacy pre-AAD blob. Caller SHOULD re-bind on next write.
    Legacy,
}

/// Encrypt `plaintext` under a 32-byte key, binding `aad` as AES-GCM additional authenticated data
/// → `nonce(12) || ciphertext+tag`. `aad` is authenticated but NOT stored (it is reconstructed from
/// the storage context at decrypt time). Pass `&[]` for no binding (legacy form).
pub fn encrypt(key: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce_bytes)
        .map_err(|e| AppError::Storage(format!("nonce RNG: {e}")))?;
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|e| AppError::Storage(format!("AES-GCM encrypt: {e}")))?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Decrypt a `nonce(12) || ciphertext+tag` blob under a 32-byte key, expecting it to be AEAD-bound
/// to `aad`. Tries the supplied `aad` first; on failure with a NON-EMPTY `aad`, retries with empty
/// AAD to transparently read LEGACY pre-AAD blobs (see module docs). Returns the plaintext plus an
/// [`AadUsed`] flag the caller can use to re-bind a legacy blob. A wrong key, tampered ciphertext, or
/// a blob bound to a DIFFERENT context fails closed (`AppError::Locked`).
pub fn decrypt_with_aad(key: &[u8; 32], blob: &[u8], aad: &[u8]) -> Result<(Vec<u8>, AadUsed)> {
    if blob.len() < NONCE_LEN + TAG_LEN {
        return Err(AppError::Storage("ciphertext too short".into()));
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let (nonce_bytes, ct) = blob.split_at(NONCE_LEN);
    let nonce = Nonce::from_slice(nonce_bytes);

    // Primary: the caller's real context AAD.
    if let Ok(pt) = cipher.decrypt(nonce, Payload { msg: ct, aad }) {
        return Ok((pt, AadUsed::Bound));
    }
    // Backward-compat: a genuinely pre-AAD blob was written with empty AAD. Only attempt the legacy
    // form when the caller asked for a non-empty AAD (otherwise this is identical to the primary
    // attempt above and we would just be repeating a failure). A blob bound to a DIFFERENT non-empty
    // context never matches empty AAD, so this does NOT weaken context-swap detection.
    if !aad.is_empty() {
        if let Ok(pt) = cipher.decrypt(nonce, Payload { msg: ct, aad: &[] }) {
            return Ok((pt, AadUsed::Legacy));
        }
    }
    Err(AppError::Locked(
        "decryption failed (wrong key, tampered data, or wrong storage context)".into(),
    ))
}

/// Decrypt a blob expecting `aad` (with the same legacy fallback as [`decrypt_with_aad`]), discarding
/// the [`AadUsed`] flag. Use this at call-sites that do not re-bind. A wrong key / tampered
/// ciphertext / wrong context fails closed (`AppError::Locked`).
pub fn decrypt(key: &[u8; 32], blob: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    decrypt_with_aad(key, blob, aad).map(|(pt, _)| pt)
}

/// A random 32-byte key (folder content key or master KEK).
pub fn random_key() -> Result<[u8; 32]> {
    let mut k = [0u8; 32];
    getrandom::getrandom(&mut k).map_err(|e| AppError::Storage(format!("key RNG: {e}")))?;
    Ok(k)
}

/// Encrypt the file at `src` under `key` (binding `aad` as context) into `dest` (the
/// `nonce(12) || ciphertext+tag` blob), then VERIFY the ciphertext decrypts back byte-identical to
/// the source BEFORE returning. This mirrors `seal_note`'s verify-before-destroy: the caller removes
/// the plaintext only after a successful return, so a corrupt write can never lose audio. The
/// plaintext WAV (separate file at `meetings.audio_path`, NOT in the SQLCipher DB) is encrypted at
/// rest for locked folders. `aad` should be the audio context (`meeting_id|folder_id`); `&[]` for
/// the legacy unbound form.
pub fn encrypt_file(key: &[u8; 32], src: &Path, dest: &Path, aad: &[u8]) -> Result<()> {
    let plaintext = read_owned_file(src, "read audio for encrypt")?;
    let blob = encrypt(key, &plaintext, aad)?;
    // Verify once before I/O, durably publish through a private same-directory staging file, then
    // REOPEN the final pathname and decrypt what is actually on disk before callers may destroy the
    // plaintext. An in-memory-only check is not verify-before-destroy.
    let check = decrypt(key, &blob, aad)?;
    if check != plaintext {
        return Err(AppError::Storage(
            "audio seal verification failed (decrypted blob mismatch)".into(),
        ));
    }
    durable_atomic_write(dest, &blob, "write encrypted audio")?;
    let stored = read_owned_file(dest, "verify stored encrypted audio")?;
    if decrypt(key, &stored, aad)? != plaintext {
        return Err(AppError::Storage(
            "stored audio seal verification failed (decrypted file mismatch)".into(),
        ));
    }
    Ok(())
}

/// Decrypt the encrypted-WAV file at `src` (a `nonce(12) || ciphertext+tag` blob) under `key`,
/// expecting context `aad` (legacy empty-AAD fallback applies, see [`decrypt_with_aad`]), into the
/// plaintext WAV at `dest`. Used to materialize a playable WAV for the session on unlock, and to
/// permanently restore the plaintext on remove-lock.
pub fn decrypt_file(key: &[u8; 32], src: &Path, dest: &Path, aad: &[u8]) -> Result<()> {
    let blob = read_owned_file(src, "read encrypted audio")?;
    let plaintext = decrypt(key, &blob, aad)?;
    durable_atomic_write(dest, &plaintext, "write decrypted audio")
}

/// Decrypt an encrypted-WAV file at `src` trying a LADDER of candidate AADs in priority order,
/// writing the plaintext to `dest` on the first candidate that succeeds. Each candidate carries the
/// same empty-AAD legacy fallback as [`decrypt_with_aad`].
///
/// This is the audio backward-compatibility ladder for the stream-role AAD hardening. The three
/// per-meeting audio files (playback WAV + mic/sys masters) are now sealed with a ROLE-bound AAD so
/// they can't be swapped for one another. But a master/playback `.enc` sealed BEFORE the role existed
/// carries the role-LESS `aad_audio(meeting,folder)` — a NON-EMPTY AAD that a role-bound decrypt
/// alone would miss AND the empty-AAD fallback would also miss → DATA LOSS. Passing the role-less AAD
/// as a lower rung makes the migration lossless; the file re-binds to the role form on its next seal.
/// A swapped file (mic ciphertext presented as sys) matches NEITHER rung and fails closed. Fails
/// closed (`AppError::Locked`) only if NO candidate (and no empty fallback) matches.
pub fn decrypt_file_multi(key: &[u8; 32], src: &Path, dest: &Path, aads: &[&[u8]]) -> Result<()> {
    let blob = read_owned_file(src, "read encrypted audio")?;
    for aad in aads {
        if let Ok(pt) = decrypt(key, &blob, aad) {
            durable_atomic_write(dest, &pt, "write decrypted audio")?;
            return Ok(());
        }
    }
    Err(AppError::Locked(
        "audio decryption failed (wrong key, tampered data, or wrong storage context)".into(),
    ))
}

/// Authenticate an encrypted audio file against the ordered AAD ladder without materializing a
/// plaintext pathname. Used by startup repair before a dangling DB pointer may be repointed at the
/// ciphertext-only survivor. A wrong key, truncated/tampered file, or swapped stream fails closed.
pub(crate) fn verify_encrypted_file_multi(
    key: &[u8; 32],
    src: &Path,
    aads: &[&[u8]],
) -> Result<()> {
    let blob = read_owned_file(src, "verify encrypted audio")?;
    for aad in aads {
        if let Ok(plaintext) = decrypt(key, &blob, aad) {
            let _plaintext = zeroize::Zeroizing::new(plaintext);
            return Ok(());
        }
    }
    Err(AppError::Locked(
        "audio verification failed (wrong key, tampered data, or wrong storage context)".into(),
    ))
}

/// Authenticate a retained ciphertext and prove that an already-materialized plaintext sibling is
/// byte-identical before permanent unlock is allowed to retire the ciphertext. This is the
/// session-unlock shape: the DB points at plaintext while the `.enc` remains the rollback copy.
pub(crate) fn verify_encrypted_file_matches_plaintext_multi(
    key: &[u8; 32],
    encrypted: &Path,
    plaintext: &Path,
    aads: &[&[u8]],
) -> Result<()> {
    let blob = read_owned_file(encrypted, "verify retained encrypted audio")?;
    let expected = zeroize::Zeroizing::new(read_owned_file(
        plaintext,
        "verify permanent-unlock plaintext audio",
    )?);
    for aad in aads {
        if let Ok(candidate) = decrypt(key, &blob, aad) {
            let candidate = zeroize::Zeroizing::new(candidate);
            if candidate.as_slice() == expected.as_slice() {
                return Ok(());
            }
            return Err(AppError::Locked(
                "permanent-unlock plaintext does not match its retained ciphertext".into(),
            ));
        }
    }
    Err(AppError::Locked(
        "audio verification failed (wrong key, tampered data, or wrong storage context)".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_hides_plaintext() {
        let k = random_key().unwrap();
        let pt = "locked note markdown 🔒".as_bytes();
        let blob = encrypt(&k, pt, b"folder-42").unwrap();
        assert_ne!(
            &blob[NONCE_LEN..],
            pt,
            "ciphertext must differ from plaintext"
        );
        assert_eq!(decrypt(&k, &blob, b"folder-42").unwrap(), pt);
    }

    #[test]
    fn wrong_key_fails_closed() {
        let blob = encrypt(&random_key().unwrap(), b"secret", b"ctx").unwrap();
        assert!(decrypt(&random_key().unwrap(), &blob, b"ctx").is_err());
    }

    #[test]
    fn kek_wraps_and_unwraps_a_content_key() {
        let kek = random_key().unwrap();
        let ck = random_key().unwrap();
        let wrapped = encrypt(&kek, &ck, b"folder-id").unwrap();
        assert_eq!(decrypt(&kek, &wrapped, b"folder-id").unwrap(), ck);
    }

    // ── B7/B8 AAD context-binding + backward-compat regression ─────────────────────────────────

    /// A pre-AAD blob (written with EMPTY aad — the legacy form) must STILL decrypt when the caller
    /// now supplies a real context AAD — and report `AadUsed::Legacy` so the caller re-binds. This
    /// is the "never brick existing folders" guarantee.
    #[test]
    fn legacy_pre_aad_blob_still_decrypts_under_new_context() {
        let k = random_key().unwrap();
        let pt = b"existing locked folder note written before AAD existed";

        // LEGACY blob: encrypted with empty AAD, exactly as the shipped v0.3.2 code wrote it.
        let legacy_blob = encrypt(&k, pt, b"").unwrap();

        // New code reads it WITH a context AAD → must succeed via the empty-AAD fallback…
        let (out, used) = decrypt_with_aad(&k, &legacy_blob, b"folder-7|meeting-1").unwrap();
        assert_eq!(out, pt, "a pre-AAD blob must still decrypt (no bricking)");
        assert_eq!(
            used,
            AadUsed::Legacy,
            "must report it read a legacy blob so caller re-binds"
        );

        // …and the plain `decrypt` wrapper agrees.
        assert_eq!(
            decrypt(&k, &legacy_blob, b"folder-7|meeting-1").unwrap(),
            pt
        );
    }

    /// A blob bound to context A must FAIL to decrypt when presented as context B (a swapped/replayed
    /// ciphertext). The legacy empty-AAD fallback must NOT rescue it (it is bound, not legacy).
    #[test]
    fn swapped_context_blob_fails_closed() {
        let k = random_key().unwrap();
        let pt = b"secret bound to folder A";
        let blob_a = encrypt(&k, pt, b"folder-A").unwrap();

        // Correct context → ok.
        assert_eq!(decrypt(&k, &blob_a, b"folder-A").unwrap(), pt);
        // Wrong context (attacker moved the ciphertext into folder B) → fails closed.
        let res = decrypt(&k, &blob_a, b"folder-B");
        assert!(
            res.is_err(),
            "a blob bound to folder A must not decrypt as folder B"
        );
        assert!(
            matches!(res, Err(AppError::Locked(_))),
            "context mismatch must fail closed with Locked, got {res:?}"
        );
        // And the AadUsed-returning form also rejects it (legacy fallback must not rescue a BOUND
        // blob under the wrong context).
        assert!(decrypt_with_aad(&k, &blob_a, b"folder-B").is_err());
    }

    /// Re-binding: a legacy blob, once re-encrypted with its real context, decrypts as `Bound` and
    /// then refuses the wrong context — proving the lazy migration actually upgrades protection.
    #[test]
    fn rebinding_a_legacy_blob_upgrades_to_bound() {
        let k = random_key().unwrap();
        let pt = b"note to be migrated";
        let legacy = encrypt(&k, pt, b"").unwrap();

        // Read legacy → decide to re-bind to the real context.
        let (recovered, used) = decrypt_with_aad(&k, &legacy, b"ctx-real").unwrap();
        assert_eq!(used, AadUsed::Legacy);
        let rebound = encrypt(&k, &recovered, b"ctx-real").unwrap();

        // Now it is bound: correct context reads as Bound, wrong context fails.
        let (out2, used2) = decrypt_with_aad(&k, &rebound, b"ctx-real").unwrap();
        assert_eq!(out2, pt);
        assert_eq!(
            used2,
            AadUsed::Bound,
            "after re-bind the blob is context-bound"
        );
        assert!(
            decrypt(&k, &rebound, b"ctx-wrong").is_err(),
            "re-bound blob rejects wrong context"
        );
    }

    fn temp_path(label: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "meetnotes-crypto-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
    }

    #[test]
    fn verified_content_removal_deletes_only_the_recorded_bytes() {
        use sha2::{Digest, Sha256};

        let path = temp_path("verified-remove.md");
        let bytes = b"# Murmur-authored export\n";
        std::fs::write(&path, bytes).unwrap();
        let hash: [u8; 32] = Sha256::digest(bytes).into();

        remove_file_verified_content(
            &path,
            Some(bytes.len() as u64),
            &hash,
            "test verified export removal",
        )
        .unwrap();

        assert!(!path.exists(), "an unchanged managed export is removed");
    }

    #[test]
    fn verified_content_removal_restores_an_external_edit() {
        use sha2::{Digest, Sha256};

        let path = temp_path("verified-preserve.md");
        let authored = b"# Murmur-authored export\n";
        let external = b"# User edit in Obsidian\n";
        std::fs::write(&path, external).unwrap();
        let hash: [u8; 32] = Sha256::digest(authored).into();

        let error = remove_file_verified_content(
            &path,
            None,
            &hash,
            "test preserve external edit",
        )
        .unwrap_err();

        assert!(matches!(error, AppError::Storage(_)));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            external,
            "a mismatching file is restored byte-identical at the canonical path"
        );
    }

    #[test]
    fn audio_encrypt_decrypt_round_trips_byte_identical() {
        // Synthesize a small "WAV" payload (bytes are opaque to the crypto layer — a real WAV
        // header would be identical content). Encrypt → .enc, remove plaintext, decrypt → assert
        // byte-identical, and assert the ciphertext does NOT contain the plaintext.
        let key = random_key().unwrap();
        let wav = temp_path("audio.wav");
        let enc = temp_path("audio.wav.enc");
        let restored = temp_path("audio-restored.wav");
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&wav, &payload).unwrap();

        // ENCRYPT (verify-before-destroy happens inside) → then simulate the lock removing the
        // plaintext WAV. Bind the audio context AAD (meeting|folder).
        let aad = b"meeting-x|folder-y";
        encrypt_file(&key, &wav, &enc, aad).unwrap();
        let blob = std::fs::read(&enc).unwrap();
        assert!(
            !contains(&blob, &payload),
            "ciphertext must not leak the plaintext audio"
        );
        std::fs::remove_file(&wav).unwrap();
        assert!(!wav.exists(), "plaintext WAV removed while sealed");

        // DECRYPT for the session → byte-identical (same AAD).
        decrypt_file(&key, &enc, &restored, aad).unwrap();
        assert_eq!(
            std::fs::read(&restored).unwrap(),
            payload,
            "audio round-trips byte-identical"
        );

        // Wrong key fails closed.
        assert!(decrypt_file(&random_key().unwrap(), &enc, &restored, aad).is_err());
        // Wrong AAD (audio replayed into a different meeting/folder) fails closed.
        assert!(decrypt_file(&key, &enc, &restored, b"meeting-OTHER|folder-y").is_err());

        let _ = std::fs::remove_file(&enc);
        let _ = std::fs::remove_file(&restored);
    }

    /// Stream-role AAD backward-compat ladder ([`decrypt_file_multi`]): a `.enc` sealed under ANY of
    /// the three historical/new forms must still decrypt, while a swapped/wrong-context file fails
    /// closed. The ladder is `[role-bound, role-less]` (each rung also tries empty-AAD internally):
    ///   - a NEW role-bound master (`…|stream=mic`) decrypts on rung 1;
    ///   - a LEGACY role-LESS master (`…folder=…`, NON-empty) decrypts on rung 2 (the migration that
    ///     would otherwise be DATA LOSS);
    ///   - a PRE-AAD master (empty AAD) decrypts via the empty fallback built into rung 1;
    ///   - a mic file presented under the SYS ladder fails closed (no rung matches).
    #[test]
    fn audio_role_aad_ladder_reads_all_legacy_forms_and_rejects_swaps() {
        let key = random_key().unwrap();
        let payload: Vec<u8> = (0..2048u32).map(|i| (i % 251) as u8).collect();

        let role_mic: &[u8] = b"murmur:audio:v1|meeting=m|folder=f|stream=mic";
        let role_sys: &[u8] = b"murmur:audio:v1|meeting=m|folder=f|stream=sys";
        let role_less: &[u8] = b"murmur:audio:v1|meeting=m|folder=f";
        // The mic ladder a caller would use: role-bound first, then the role-less legacy form.
        let mic_ladder: &[&[u8]] = &[role_mic, role_less];
        let sys_ladder: &[&[u8]] = &[role_sys, role_less];

        let src = temp_path("ladder-src.wav");
        let enc = temp_path("ladder.wav.enc");
        let out = temp_path("ladder-out.wav");
        std::fs::write(&src, &payload).unwrap();

        // (1) NEW role-bound mic master → decrypts on rung 1 of the mic ladder.
        encrypt_file(&key, &src, &enc, role_mic).unwrap();
        decrypt_file_multi(&key, &enc, &out, mic_ladder).unwrap();
        assert_eq!(
            std::fs::read(&out).unwrap(),
            payload,
            "role-bound master round-trips"
        );
        // …and a SYS ladder must NOT read a MIC file (swap rejected, fails closed).
        assert!(
            decrypt_file_multi(&key, &enc, &out, sys_ladder).is_err(),
            "a mic master must not decrypt under the sys ladder (no swaps within a meeting)"
        );

        // (2) LEGACY role-LESS master (sealed before the stream role existed) → rung 2 reads it.
        encrypt_file(&key, &src, &enc, role_less).unwrap();
        decrypt_file_multi(&key, &enc, &out, mic_ladder).unwrap();
        assert_eq!(
            std::fs::read(&out).unwrap(),
            payload,
            "role-less legacy master still decrypts"
        );

        // (3) PRE-AAD master (empty AAD) → the empty fallback inside rung 1 reads it.
        encrypt_file(&key, &src, &enc, b"").unwrap();
        decrypt_file_multi(&key, &enc, &out, mic_ladder).unwrap();
        assert_eq!(
            std::fs::read(&out).unwrap(),
            payload,
            "pre-AAD master still decrypts"
        );

        // (4) Wrong KEY fails closed regardless of ladder.
        assert!(decrypt_file_multi(&random_key().unwrap(), &enc, &out, mic_ladder).is_err());

        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&enc);
        let _ = std::fs::remove_file(&out);
    }

    /// Naive subslice search for the leak assertion.
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() || needle.len() > haystack.len() {
            return false;
        }
        haystack.windows(needle.len()).any(|w| w == needle)
    }
}
