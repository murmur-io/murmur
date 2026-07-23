//! Gated note-image attachment commands and the shared E2EE-bundle seam.

use std::collections::HashSet;
use std::io::Write;

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::*;
use crate::storage::{
    AttachmentOwner, AttachmentRecord, IncomingAttachment, NewAttachment, MAX_ATTACHMENT_BYTES,
    MAX_ATTACHMENT_DIMENSION, MAX_ATTACHMENT_PIXELS,
};

const ATTACHMENT_AAD_VERSION: &str = "1";
const MAX_BASE64_INPUT: usize = MAX_ATTACHMENT_BYTES.div_ceil(3) * 4 + 128;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentDto {
    pub id: String,
    pub owner_kind: String,
    pub owner_id: String,
    pub mime_type: String,
    pub extension: String,
    pub byte_len: u64,
    pub width: u32,
    pub height: u32,
    pub sha256: String,
    pub data_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentBundleItem {
    pub id: String,
    pub mime_type: String,
    pub extension: String,
    pub width: u32,
    pub height: u32,
    pub sha256: [u8; 32],
    pub data: Vec<u8>,
}

pub(crate) fn attachment_aad(
    folder_id: &str,
    owner: &AttachmentOwner,
    attachment_id: &str,
) -> Vec<u8> {
    let owner_context = match owner {
        AttachmentOwner::Document { document_id } => format!("document|{document_id}"),
        AttachmentOwner::Meeting {
            meeting_id,
            provider_id,
        } => format!("meeting|{meeting_id}|{provider_id}"),
        AttachmentOwner::OrgItem { item_id } => format!("org|{item_id}"),
    };
    format!(
        "murmur-lock/v{ATTACHMENT_AAD_VERSION}|{folder_id}|{owner_context}|{attachment_id}|attachment"
    )
    .into_bytes()
}

fn gate_attachment_owner(state: &AppState, owner: &AttachmentOwner) -> Result<(), AppError> {
    match owner {
        AttachmentOwner::Document { document_id } => {
            // Resolve only the non-content folder anchor before the read gate. `NoteRow` contains
            // title, Markdown and the exported path; loading it here would surface plaintext from a
            // logically locked row whose interrupted seal cleanup had not blanked those columns yet.
            let folder_id = state
                .db
                .folder_for_document(document_id)?
                .ok_or_else(|| AppError::InvalidArg(format!("no note {document_id}")))?;
            if !folder_is_unlocked(state, &folder_id)? {
                return Err(AppError::Locked(
                    "this note is locked — unlock its folder to access images".into(),
                ));
            }
        }
        AttachmentOwner::Meeting { meeting_id, .. } => {
            if !meeting_is_unlocked(state, meeting_id)? {
                return Err(AppError::Locked(
                    "this meeting is locked — unlock its folder to access images".into(),
                ));
            }
        }
        AttachmentOwner::OrgItem { item_id } => {
            // The org store applies both live-item and per-instance context gates in SQL.
            if state.db.get_org_item(item_id)?.is_none() {
                return Err(AppError::InvalidArg(
                    "no visible live org item for this image".into(),
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn plaintext_attachment_data(
    state: &AppState,
    row: &AttachmentRecord,
) -> Result<Vec<u8>, AppError> {
    if !row.data.is_empty() {
        verify_stored_attachment(row, &row.data)?;
        return Ok(row.data.clone());
    }
    let blob = row.data_blob.as_deref().ok_or_else(|| {
        AppError::Storage("attachment has neither plaintext nor a recoverable seal".into())
    })?;
    let folder_id = state
        .db
        .folder_for_attachment_owner(&row.owner)?
        .ok_or_else(|| AppError::Storage("sealed attachment has no owning folder".into()))?;
    let ck = session_folder_ck(state, &folder_id)?;
    let data = crate::crypto::decrypt(&ck, blob, &attachment_aad(&folder_id, &row.owner, &row.id))?;
    verify_stored_attachment(row, &data)?;
    Ok(data)
}

fn verify_stored_attachment(row: &AttachmentRecord, data: &[u8]) -> Result<(), AppError> {
    if data.len() as u64 != row.byte_len || Sha256::digest(data).as_slice() != row.sha256 {
        return Err(AppError::Storage(
            "attachment bytes do not match their authenticated metadata".into(),
        ));
    }
    let image = validate_image(&row.mime_type, data)?;
    if image.extension != row.extension || image.width != row.width || image.height != row.height {
        return Err(AppError::Storage(
            "attachment image metadata does not match its bytes".into(),
        ));
    }
    Ok(())
}

fn dto_from_row(state: &AppState, row: &AttachmentRecord) -> Result<AttachmentDto, AppError> {
    let data = plaintext_attachment_data(state, row)?;
    Ok(AttachmentDto {
        id: row.id.clone(),
        owner_kind: row.owner.kind().to_string(),
        owner_id: row.owner.owner_id().to_string(),
        mime_type: row.mime_type.clone(),
        extension: row.extension.clone(),
        byte_len: row.byte_len,
        width: row.width,
        height: row.height,
        sha256: hex_lower(&row.sha256),
        data_url: format!(
            "data:{};base64,{}",
            row.mime_type,
            encode_base64(&data, false, true)
        ),
    })
}

#[tauri::command]
pub fn add_note_attachment(
    state: State<'_, AppState>,
    owner_kind: String,
    owner_id: String,
    file_name: String,
    mime_type: String,
    data_base64: String,
) -> Result<AttachmentDto, AppError> {
    add_note_attachment_inner(
        state.inner(),
        &owner_kind,
        &owner_id,
        &file_name,
        &mime_type,
        &data_base64,
    )
}

pub(crate) fn add_note_attachment_inner(
    state: &AppState,
    owner_kind: &str,
    owner_id: &str,
    _file_name: &str,
    mime_type: &str,
    data_base64: &str,
) -> Result<AttachmentDto, AppError> {
    let _lifecycle = lifecycle_guard(state);
    if data_base64.len() > MAX_BASE64_INPUT {
        return Err(AppError::InvalidArg(
            "encoded image exceeds the size limit".into(),
        ));
    }
    if owner_kind == "org" {
        return Err(AppError::InvalidArg(
            "org images are materialized only by verified share ingest".into(),
        ));
    }
    let owner = state.db.resolve_attachment_owner(owner_kind, owner_id)?;
    gate_attachment_owner(state, &owner)?;
    let data = decode_base64(data_base64)?;
    let image = validate_image(mime_type, &data)?;
    if image.mime_type != "image/webp" {
        return Err(AppError::InvalidArg(
            "new images must be locally normalized to metadata-free WebP".into(),
        ));
    }
    reject_webp_metadata(&data)?;
    let id = uuid::Uuid::new_v4().to_string();
    let hash: [u8; 32] = Sha256::digest(&data).into();
    let created_at = chrono::Utc::now().timestamp_millis();
    let folder_id = state.db.folder_for_attachment_owner(&owner)?;
    let locked = match folder_id.as_deref() {
        Some(fid) => state.db.folder_by_id(fid)?.is_some_and(|f| f.locked),
        None => false,
    };
    let mut sealed = None;
    if locked {
        let fid = folder_id
            .as_deref()
            .ok_or_else(|| AppError::Storage("locked attachment owner has no folder".into()))?;
        let ck = session_folder_ck(state, fid)?;
        let aad = attachment_aad(fid, &owner, &id);
        let blob = crate::crypto::encrypt(&ck, &data, &aad)?;
        if crate::crypto::decrypt(&ck, &blob, &aad)? != data {
            return Err(AppError::Storage(
                "attachment birth-seal verification failed".into(),
            ));
        }
        sealed = Some(blob);
    }
    // Locked owners are blank from birth; reads decrypt the verified seal through the session CK.
    let stored_data: &[u8] = if locked { &[] } else { &data };
    state.db.insert_attachment(&NewAttachment {
        id: &id,
        owner: &owner,
        mime_type: image.mime_type,
        extension: image.extension,
        width: image.width,
        height: image.height,
        sha256: &hash,
        byte_len: data.len(),
        data: stored_data,
        data_blob: sealed.as_deref(),
        created_at,
    })?;
    let row = state
        .db
        .list_attachments(&owner)?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| AppError::Storage("inserted attachment disappeared".into()))?;
    dto_from_row(state, &row)
}

#[tauri::command]
pub fn list_note_attachments(
    state: State<'_, AppState>,
    owner_kind: String,
    owner_id: String,
) -> Result<Vec<AttachmentDto>, AppError> {
    list_note_attachments_inner(state.inner(), &owner_kind, &owner_id)
}

pub(crate) fn list_note_attachments_inner(
    state: &AppState,
    owner_kind: &str,
    owner_id: &str,
) -> Result<Vec<AttachmentDto>, AppError> {
    let _lifecycle = lifecycle_guard(state);
    let owner = state.db.resolve_attachment_owner(owner_kind, owner_id)?;
    gate_attachment_owner(state, &owner)?;
    state
        .db
        .list_attachments(&owner)?
        .iter()
        .map(|row| dto_from_row(state, row))
        .collect()
}

#[tauri::command]
pub fn delete_note_attachment(
    state: State<'_, AppState>,
    owner_kind: String,
    owner_id: String,
    attachment_id: String,
) -> Result<(), AppError> {
    delete_note_attachment_inner(state.inner(), &owner_kind, &owner_id, &attachment_id)
}

pub(crate) fn delete_note_attachment_inner(
    state: &AppState,
    owner_kind: &str,
    owner_id: &str,
    attachment_id: &str,
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    if owner_kind == "org" {
        return Err(AppError::InvalidArg(
            "org images are removed with their org item".into(),
        ));
    }
    let owner = state.db.resolve_attachment_owner(owner_kind, owner_id)?;
    gate_attachment_owner(state, &owner)?;
    let row = state
        .db
        .list_attachments(&owner)?
        .into_iter()
        .find(|r| r.id == attachment_id);
    let Some(row) = row else {
        return Ok(());
    };
    remove_attachment_exports(
        std::slice::from_ref(&row),
        "could not remove exported image before deleting it",
    )?;
    state.db.delete_attachment(&owner, attachment_id)?;
    Ok(())
}

/// Gated, exact-owner bundle read used by link/user/org share code. Unknown/cross-owner ids fail.
pub fn attachment_bundle_for_owner(
    state: &AppState,
    owner: &AttachmentOwner,
    referenced_ids: &HashSet<String>,
) -> Result<Vec<AttachmentBundleItem>, AppError> {
    let _lifecycle = lifecycle_guard(state);
    gate_attachment_owner(state, owner)?;
    state
        .db
        .list_referenced_attachments(owner, referenced_ids)?
        .iter()
        .map(|row| {
            let data = plaintext_attachment_data(state, row)?;
            Ok(AttachmentBundleItem {
                id: row.id.clone(),
                mime_type: row.mime_type.clone(),
                extension: row.extension.clone(),
                width: row.width,
                height: row.height,
                sha256: row.sha256,
                data,
            })
        })
        .collect()
}

/// Materialize already-authenticated bundle records under an exact local owner. Caller controls ids
/// so canonical markdown references survive share ingest. Folder owners are sealed from birth when
/// locked; org items remain SQLCipher-only by design.
pub fn validate_incoming_attachment_bundle(
    incoming: &[IncomingAttachment],
) -> Result<(), AppError> {
    if incoming.len() > crate::storage::MAX_ATTACHMENTS_PER_OWNER {
        return Err(AppError::InvalidArg(
            "attachment bundle exceeds local limits".into(),
        ));
    }
    let mut seen = HashSet::new();
    let mut total = 0usize;
    for item in incoming {
        if item.mime_type != "image/webp" || item.extension != "webp" {
            return Err(AppError::InvalidArg(
                "attachments must be normalized WebP images".into(),
            ));
        }
        let parsed = uuid::Uuid::parse_str(&item.id)
            .map_err(|_| AppError::InvalidArg("attachment id is not a UUID".into()))?;
        if parsed.get_version_num() != 4
            || parsed.to_string() != item.id
            || !seen.insert(item.id.clone())
        {
            return Err(AppError::InvalidArg(
                "attachment ids must be unique canonical UUIDv4 values".into(),
            ));
        }
        let image = validate_image(&item.mime_type, &item.data)?;
        reject_webp_metadata(&item.data)?;
        if image.extension != item.extension
            || image.width != item.width
            || image.height != item.height
            || Sha256::digest(&item.data).as_slice() != item.sha256
        {
            return Err(AppError::InvalidArg(
                "attachment manifest does not match its bytes".into(),
            ));
        }
        total = total
            .checked_add(item.data.len())
            .ok_or_else(|| AppError::InvalidArg("attachment bundle is too large".into()))?;
        if total > crate::storage::MAX_ATTACHMENT_BYTES_PER_OWNER {
            return Err(AppError::InvalidArg(
                "attachment bundle exceeds local limits".into(),
            ));
        }
    }
    Ok(())
}

pub fn materialize_attachment_bundle(
    state: &AppState,
    owner: &AttachmentOwner,
    incoming: &[IncomingAttachment],
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    materialize_attachment_bundle_under_lifecycle(state, owner, incoming)
}

/// Variant for composed commands that already hold the non-reentrant lifecycle mutex.
pub fn materialize_attachment_bundle_under_lifecycle(
    state: &AppState,
    owner: &AttachmentOwner,
    incoming: &[IncomingAttachment],
) -> Result<(), AppError> {
    gate_attachment_owner(state, owner)?;
    validate_incoming_attachment_bundle(incoming)?;
    let folder_id = state.db.folder_for_attachment_owner(owner)?;
    let locked = folder_id
        .as_deref()
        .map(|fid| state.db.folder_by_id(fid))
        .transpose()?
        .flatten()
        .is_some_and(|f| f.locked);
    let ck = if locked {
        let fid = folder_id
            .as_deref()
            .ok_or_else(|| AppError::Storage("locked attachment owner has no folder".into()))?;
        Some(session_folder_ck(state, fid)?)
    } else {
        None
    };
    let mut blobs: Vec<Option<Vec<u8>>> = Vec::with_capacity(incoming.len());
    for item in incoming {
        let blob = if let (Some(ck), Some(fid)) = (ck.as_deref(), folder_id.as_deref()) {
            let aad = attachment_aad(fid, owner, &item.id);
            let blob = crate::crypto::encrypt(ck, &item.data, &aad)?;
            if crate::crypto::decrypt(ck, &blob, &aad)? != item.data {
                return Err(AppError::Storage(
                    "attachment ingest seal verification failed".into(),
                ));
            }
            Some(blob)
        } else {
            None
        };
        blobs.push(blob);
    }
    let now = chrono::Utc::now().timestamp_millis();
    let records: Vec<NewAttachment<'_>> = incoming
        .iter()
        .zip(blobs.iter())
        .map(|(item, blob)| NewAttachment {
            id: &item.id,
            owner,
            mime_type: &item.mime_type,
            extension: &item.extension,
            width: item.width,
            height: item.height,
            sha256: &item.sha256,
            byte_len: item.data.len(),
            data: if locked { &[] } else { &item.data },
            data_blob: blob.as_deref(),
            created_at: now,
        })
        .collect();
    let refs: Vec<&NewAttachment<'_>> = records.iter().collect();
    state.db.insert_attachments(&refs)
}

/// Initial folder seal: create and verify every per-folder attachment blob while retaining
/// plaintext. The caller marks the folder locked only after this succeeds; blanking happens last.
pub(crate) fn seal_attachments_in_folder(
    state: &AppState,
    folder_id: &str,
    ck: &[u8; 32],
) -> Result<(), AppError> {
    let rows = state.db.attachments_in_folder(folder_id)?;
    let mut sealed = Vec::with_capacity(rows.len());
    for row in rows {
        if row.data.is_empty() && row.byte_len != 0 {
            return Err(AppError::Storage(
                "attachment plaintext missing before folder seal".into(),
            ));
        }
        verify_stored_attachment(&row, &row.data)?;
        let aad = attachment_aad(folder_id, &row.owner, &row.id);
        if row
            .data_blob
            .as_deref()
            .and_then(|blob| crate::crypto::decrypt(ck, blob, &aad).ok())
            .is_some_and(|plain| plain == row.data)
        {
            continue;
        }
        let blob = crate::crypto::encrypt(ck, &row.data, &aad)?;
        if crate::crypto::decrypt(ck, &blob, &aad)? != row.data {
            return Err(AppError::Storage(
                "attachment seal verification failed".into(),
            ));
        }
        sealed.push((row.id, blob));
    }
    state.db.store_attachment_seals(&sealed)
}

/// Restore session plaintext (keep blob) or permanently open it (clear blob only after restore).
pub(crate) fn unseal_attachments_in_folder(
    state: &AppState,
    folder_id: &str,
    ck: &[u8; 32],
    permanent: bool,
) -> Result<(), AppError> {
    for row in state.db.attachments_in_folder(folder_id)? {
        let data = if let Some(blob) = row.data_blob.as_deref() {
            let data =
                crate::crypto::decrypt(ck, blob, &attachment_aad(folder_id, &row.owner, &row.id))?;
            verify_stored_attachment(&row, &data)?;
            data
        } else {
            verify_stored_attachment(&row, &row.data)?;
            row.data.clone()
        };
        // Restore verified bytes first; the same statement clears the blob only for permanent open.
        state
            .db
            .restore_attachment_data(&row.id, &data, permanent)?;
    }
    Ok(())
}

pub(crate) fn unseal_attachments_for_meeting(
    state: &AppState,
    folder_id: &str,
    meeting_id: &str,
    ck: &[u8; 32],
) -> Result<(), AppError> {
    for row in state.db.attachments_for_meeting(meeting_id)? {
        let blob = row.data_blob.as_deref().ok_or_else(|| {
            AppError::Storage("moved locked attachment has no recoverable seal".into())
        })?;
        let data =
            crate::crypto::decrypt(ck, blob, &attachment_aad(folder_id, &row.owner, &row.id))?;
        verify_stored_attachment(&row, &data)?;
        state.db.restore_attachment_data(&row.id, &data, false)?;
    }
    Ok(())
}

pub(crate) fn remove_attachment_exports_before_move(
    attachments: &[AttachmentRecord],
) -> Result<(), AppError> {
    remove_attachment_exports(
        attachments,
        "could not remove an exported image before moving it into a locked folder",
    )
}

/// Delete tracked plaintext files while their DB rows still retain retry metadata. Callers perform
/// the ownership move/delete only after this succeeds, so a filesystem refusal can never turn a
/// known plaintext path into an untracked orphan.
pub(crate) fn remove_attachment_exports(
    attachments: &[AttachmentRecord],
    failure: &str,
) -> Result<(), AppError> {
    // Preflight the complete set first. This makes a known external edit fail before any sibling
    // export is removed; canonical SQLCipher bytes remain available if a later unlink still races.
    verify_attachment_exports(attachments, failure)?;
    for attachment in attachments {
        let Some(path) = attachment.exported_path.as_deref() else {
            continue;
        };
        remove_tracked_export_pair(
            std::path::Path::new(path),
            &attachment.id,
            attachment.byte_len,
            &attachment.sha256,
            failure,
        )?;
    }
    Ok(())
}

pub(crate) fn verify_attachment_exports(
    attachments: &[AttachmentRecord],
    failure: &str,
) -> Result<(), AppError> {
    for attachment in attachments {
        let Some(path) = attachment.exported_path.as_deref() else {
            continue;
        };
        verify_tracked_export_pair(
            std::path::Path::new(path),
            &attachment.id,
            attachment.byte_len,
            &attachment.sha256,
            failure,
        )?;
    }
    Ok(())
}

fn attachment_export_temp_path(path: &std::path::Path, attachment_id: &str) -> std::path::PathBuf {
    path.with_file_name(format!(".{attachment_id}.murmur.tmp"))
}

/// A tracked export is app-owned only while its bytes still equal the SQLCipher canonical record.
/// Symlinks, directories and edited bytes are preserved and fail closed: callers must never turn a
/// user-modified Obsidian asset into an implicit destructive target.
fn verify_tracked_export_file(
    path: &std::path::Path,
    byte_len: u64,
    sha256: &[u8; 32],
    failure: &str,
) -> Result<(), AppError> {
    crate::crypto::verify_file_content(path, Some(byte_len), sha256, failure)
}

fn verify_tracked_export_pair(
    path: &std::path::Path,
    attachment_id: &str,
    byte_len: u64,
    sha256: &[u8; 32],
    failure: &str,
) -> Result<(), AppError> {
    verify_tracked_export_file(path, byte_len, sha256, failure)?;
    verify_tracked_export_file(
        &attachment_export_temp_path(path, attachment_id),
        byte_len,
        sha256,
        failure,
    )
}

fn remove_file_if_present(path: &std::path::Path, failure: &str) -> Result<(), AppError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(AppError::Export(format!("{failure}: {e}"))),
    }
}

fn remove_tracked_export_pair(
    path: &std::path::Path,
    attachment_id: &str,
    byte_len: u64,
    sha256: &[u8; 32],
    failure: &str,
) -> Result<(), AppError> {
    crate::crypto::remove_file_verified_content(path, Some(byte_len), sha256, failure)?;
    crate::crypto::remove_file_verified_content(
        &attachment_export_temp_path(path, attachment_id),
        Some(byte_len),
        sha256,
        failure,
    )
}

/// Detach every no-longer-referenced image after the new canonical Markdown has been persisted.
/// The SQLCipher row deliberately survives: textarea undo may restore the marker after an autosave,
/// and destroying pixels at that point would make ordinary Cmd-Z lossy. Only an explicit attachment
/// cleanup or owner deletion destroys canonical bytes. The plaintext vault replica is still removed.
pub(crate) fn prune_unreferenced_attachments(
    state: &AppState,
    owner: &AttachmentOwner,
    markdown: &str,
) -> Result<(), AppError> {
    let referenced = referenced_attachment_ids(markdown)?;
    let stale: Vec<AttachmentRecord> = state
        .db
        .list_attachments(owner)?
        .into_iter()
        .filter(|row| !referenced.contains(&row.id))
        .collect();
    remove_attachment_exports(
        &stale,
        "could not remove an exported image after its note marker was deleted",
    )?;
    for row in stale {
        state.db.set_attachment_exported_path(&row.id, None)?;
    }
    Ok(())
}

/// Relock/startup filesystem half. Data is blanked only where a recoverable blob exists. Exported
/// paths remain recorded after a failed unlink so the next reconciliation retries.
pub(crate) fn reblank_attachments_in_folder(
    state: &AppState,
    folder_id: &str,
) -> Result<(), AppError> {
    let exports = state.db.blank_attachments_in_folder(folder_id)?;
    delete_attachment_exports_with_retry(&state.db, exports)
}

pub(crate) fn delete_attachment_exports_with_retry(
    db: &crate::storage::Db,
    exports: Vec<(String, String)>,
) -> Result<(), AppError> {
    let mut failed = false;
    for (id, path) in exports {
        let integrity = match db.attachment_integrity(&id) {
            Ok(Some(integrity)) => integrity,
            Ok(None) => {
                tracing::warn!(target: "lock", "attachment cleanup lost its canonical integrity row — preserving export path");
                failed = true;
                continue;
            }
            Err(e) => {
                tracing::warn!(target: "lock", error = %e, "attachment cleanup could not read canonical integrity — preserving export path");
                failed = true;
                continue;
            }
        };
        let removed = verify_tracked_export_pair(
            std::path::Path::new(&path),
            &id,
            integrity.0,
            &integrity.1,
            "could not verify an exported image during lock cleanup",
        )
        .and_then(|()| {
            remove_tracked_export_pair(
                std::path::Path::new(&path),
                &id,
                integrity.0,
                &integrity.1,
                "could not remove an exported image during lock cleanup",
            )
        })
        .map(|()| true)
        .unwrap_or_else(|e| {
            tracing::warn!(target: "lock", error = %e, "deleting an exported image failed — keeping retry metadata");
            failed = true;
            false
        });
        if removed {
            db.set_attachment_exported_path(&id, None)?;
        }
    }
    if failed {
        Err(AppError::Export(
            "one or more exported images could not be removed; retry metadata was preserved".into(),
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn discard_attachments_in_folder(
    state: &AppState,
    folder_id: &str,
) -> Result<(), AppError> {
    // Privacy beats availability on the unrecoverable-key escape hatch: keep the DB rows until every
    // tracked plaintext export is gone, so a failed unlink remains retryable instead of orphaned.
    let sealed: Vec<_> = state
        .db
        .attachments_in_folder(folder_id)?
        .into_iter()
        .filter(|row| row.data_blob.is_some())
        .collect();
    remove_attachment_exports(
        &sealed,
        "could not remove exported image before discarding its unrecoverable seal",
    )?;
    state.db.delete_sealed_attachments_in_folder(folder_id)
}

const ATTACHMENT_URI_PREFIX: &str = "murmur-attachment://";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentMarker {
    pub start: usize,
    pub end: usize,
    pub id: String,
}

/// Parse only the canonical image form `![alt](murmur-attachment://UUID)`. Bare URI text and
/// examples inside fenced/inline code are deliberately inert, so export and share use one exact
/// interpretation of which private bytes the Markdown actually references.
pub fn parse_attachment_markers(markdown: &str) -> Result<Vec<AttachmentMarker>, AppError> {
    let mut markers = Vec::new();
    let mut offset = 0usize;
    let mut fence: Option<(u8, usize)> = None;
    let mut inline_code: Option<usize> = None;

    for line in markdown.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let bytes = content.as_bytes();
        let mut first = 0usize;
        let mut indent_columns = 0usize;
        while let Some(byte) = bytes.get(first) {
            match byte {
                b' ' => indent_columns += 1,
                b'\t' => indent_columns = (indent_columns / 4 + 1) * 4,
                _ => break,
            }
            first += 1;
        }
        let fence_run = (indent_columns <= 3)
            .then_some(())
            .and_then(|()| bytes.get(first))
            .and_then(|&ch| {
                if ch != b'`' && ch != b'~' {
                    return None;
                }
                let run = bytes[first..].iter().take_while(|&&b| b == ch).count();
                (run >= 3).then_some((ch, run))
            });
        if let Some((ch, run)) = fence_run {
            match fence {
                None => fence = Some((ch, run)),
                Some((open_ch, open_run))
                    if open_ch == ch
                        && run >= open_run
                        && bytes[first + run..]
                            .iter()
                            .all(|b| matches!(b, b' ' | b'\t' | b'\r')) =>
                {
                    fence = None;
                }
                Some(_) => {}
            }
            inline_code = None;
            offset += line.len();
            continue;
        }
        if fence.is_some() {
            offset += line.len();
            continue;
        }
        // Four-space/tab-indented Markdown is code, not an active image. Treating ambiguous nested
        // list indentation as inert is the privacy-safe side of the parser/renderer seam.
        if indent_columns >= 4 {
            inline_code = None;
            offset += line.len();
            continue;
        }

        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] == b'`' && !ascii_token_is_escaped(bytes, i) {
                let run = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                match inline_code {
                    None => inline_code = Some(run),
                    Some(open) if open == run => inline_code = None,
                    Some(_) => {}
                }
                i += run;
                continue;
            }
            if inline_code.is_some()
                || bytes[i] != b'!'
                || bytes.get(i + 1) != Some(&b'[')
                || ascii_token_is_escaped(bytes, i)
            {
                i += 1;
                continue;
            }

            let Some(label_end_rel) = content[i + 2..].find("](") else {
                i += 2;
                continue;
            };
            let uri_start = i + 2 + label_end_rel + 2;
            if !content[uri_start..].starts_with(ATTACHMENT_URI_PREFIX) {
                i += 2;
                continue;
            }
            let id_start = uri_start + ATTACHMENT_URI_PREFIX.len();
            let close = content[id_start..]
                .find(')')
                .map(|relative| id_start + relative)
                .ok_or_else(|| AppError::InvalidArg("unterminated attachment marker".into()))?;
            let id = &content[id_start..close];
            let parsed = uuid::Uuid::parse_str(id)
                .map_err(|_| AppError::InvalidArg("attachment marker id is not a UUID".into()))?;
            if parsed.get_version_num() != 4 || parsed.to_string() != id {
                return Err(AppError::InvalidArg(
                    "attachment marker id must be a canonical UUIDv4".into(),
                ));
            }
            markers.push(AttachmentMarker {
                start: offset + i,
                end: offset + close + 1,
                id: id.to_string(),
            });
            i = close + 1;
        }
        offset += line.len();
    }
    Ok(markers)
}

fn ascii_token_is_escaped(bytes: &[u8], at: usize) -> bool {
    let slashes = bytes[..at]
        .iter()
        .rev()
        .take_while(|&&b| b == b'\\')
        .count();
    slashes % 2 == 1
}

pub fn referenced_attachment_ids(markdown: &str) -> Result<HashSet<String>, AppError> {
    Ok(parse_attachment_markers(markdown)?
        .into_iter()
        .map(|marker| marker.id)
        .collect())
}

/// Validate every active canonical marker against this exact owner before any Markdown write.
/// This keeps the SQLCipher document and its image manifest one atomic logical unit: a foreign,
/// deleted or fabricated UUID never becomes canonical text merely because no vault export ran.
pub(crate) fn validate_attachment_references_before_save(
    state: &AppState,
    owner: &AttachmentOwner,
    markdown: &str,
) -> Result<(), AppError> {
    // `murmur-pending://` is an editor-only placeholder. Reject it at the canonical backend seam,
    // including direct IPC callers, so a failed/aborted upload can never persist an unresolved
    // private marker. Deliberately fail closed even inside code spans: this internal URI scheme is
    // not user-authored Markdown and has no durable meaning.
    if markdown.contains("murmur-pending://") {
        return Err(AppError::InvalidArg(
            "markdown contains an unresolved pending image".into(),
        ));
    }
    let referenced = referenced_attachment_ids(markdown)?;
    let rows = state.db.list_attachments(owner)?;
    if rows
        .iter()
        .filter(|row| referenced.contains(&row.id))
        .count()
        != referenced.len()
    {
        return Err(AppError::InvalidArg(
            "markdown references an unknown image for this note".into(),
        ));
    }
    let stale: Vec<_> = rows
        .into_iter()
        .filter(|row| !referenced.contains(&row.id))
        .collect();
    // A marker removal may detach a plaintext Obsidian replica after the Markdown write. Verify the
    // replica before that canonical mutation so a known external edit refuses atomically.
    for row in &stale {
        let Some(path) = row.exported_path.as_deref() else {
            continue;
        };
        verify_tracked_export_pair(
            std::path::Path::new(path),
            &row.id,
            row.byte_len,
            &row.sha256,
            "could not detach an externally changed exported image",
        )?;
    }
    Ok(())
}

/// Write every referenced attachment first, then return Obsidian-native markdown. The caller writes
/// that markdown only after this succeeds and hashes the returned text, not the canonical DB text.
#[cfg(test)]
pub(crate) fn render_markdown_with_attachments_for_export(
    state: &AppState,
    owner: &AttachmentOwner,
    markdown: &str,
    vault_root: &std::path::Path,
) -> Result<String, AppError> {
    let _lifecycle = lifecycle_guard(state);
    let markers = parse_attachment_markers(markdown)?;
    if markers.is_empty() {
        return Ok(markdown.to_string());
    }
    gate_attachment_owner(state, owner)?;
    render_markdown_with_attachment_markers(state, owner, markdown, vault_root, markers, true)
}

/// Explicit "Save Markdown…" export. Its image copies are user-owned, just like the selected
/// Markdown path: folder lock/delete never removes them and they never replace the lifecycle-tracked
/// Obsidian-vault replica. This also lets a normal vault export and a one-off export use different
/// roots without corrupting the canonical `exported_path` bookkeeping.
#[cfg(test)]
pub(crate) fn render_markdown_with_attachments_for_user_export(
    state: &AppState,
    owner: &AttachmentOwner,
    markdown: &str,
    export_root: &std::path::Path,
) -> Result<String, AppError> {
    let _lifecycle = lifecycle_guard(state);
    render_markdown_with_attachments_for_user_export_under_lifecycle_authorized(
        state,
        owner,
        markdown,
        export_root,
    )
}

/// Explicit-export variant for a caller that already holds the non-reentrant lifecycle mutex and
/// has gated the source. Keeping publication inside that interval prevents a relock from completing
/// cleanup and then being followed by a stale plaintext attachment write.
pub(crate) fn render_markdown_with_attachments_for_user_export_under_lifecycle_authorized(
    state: &AppState,
    owner: &AttachmentOwner,
    markdown: &str,
    export_root: &std::path::Path,
) -> Result<String, AppError> {
    let markers = parse_attachment_markers(markdown)?;
    if markers.is_empty() {
        return Ok(markdown.to_string());
    }
    gate_attachment_owner(state, owner)?;
    render_markdown_with_attachment_markers(state, owner, markdown, export_root, markers, false)
}

/// Lock-transition variant. Its caller has already authenticated and decrypted the governing CK;
/// it exists so the unseal path does not pretend a not-yet-published session gate is authorization.
pub(crate) fn render_markdown_with_attachments_for_export_under_lifecycle_authorized(
    state: &AppState,
    owner: &AttachmentOwner,
    markdown: &str,
    vault_root: &std::path::Path,
) -> Result<String, AppError> {
    let markers = parse_attachment_markers(markdown)?;
    if markers.is_empty() {
        return Ok(markdown.to_string());
    }
    render_markdown_with_attachment_markers(state, owner, markdown, vault_root, markers, true)
}

fn render_markdown_with_attachment_markers(
    state: &AppState,
    owner: &AttachmentOwner,
    markdown: &str,
    vault_root: &std::path::Path,
    markers: Vec<AttachmentMarker>,
    tracked: bool,
) -> Result<String, AppError> {
    let ids: HashSet<String> = markers.iter().map(|marker| marker.id.clone()).collect();
    let rows = state.db.list_referenced_attachments(owner, &ids)?;
    let mut names = std::collections::HashMap::with_capacity(rows.len());
    for row in rows {
        let data = plaintext_attachment_data(state, &row)?;
        let name = if tracked {
            ensure_attachment_exported(state, &row, &data, vault_root)?
        } else {
            ensure_user_owned_attachment_export(&row, &data, vault_root)?
        };
        names.insert(row.id, name);
    }

    let mut out = String::with_capacity(markdown.len());
    let mut cursor = 0usize;
    for marker in markers {
        out.push_str(&markdown[cursor..marker.start]);
        let name = names.get(&marker.id).ok_or_else(|| {
            AppError::InvalidArg("attachment marker belongs to a different note".into())
        })?;
        out.push_str("![[Murmur Attachments/");
        out.push_str(name);
        out.push_str("]]");
        cursor = marker.end;
    }
    out.push_str(&markdown[cursor..]);
    Ok(out)
}

fn ensure_attachment_exported(
    state: &AppState,
    row: &AttachmentRecord,
    data: &[u8],
    vault_root: &std::path::Path,
) -> Result<String, AppError> {
    let dir = assert_in_vault(vault_root, std::path::Path::new("Murmur Attachments"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::Export(format!("create attachment export dir failed: {e}")))?;
    let desired = if let Some(existing) = row.exported_path.as_deref() {
        let path = std::path::PathBuf::from(existing);
        if let Ok(relative) = path.strip_prefix(vault_root) {
            let verified = assert_in_vault(vault_root, relative)?;
            if verified.parent() != Some(dir.as_path()) {
                return Err(AppError::Export(
                    "recorded attachment export is outside Murmur Attachments".into(),
                ));
            }
            // Once a path is tracked, modified bytes must stay tracked and block lifecycle
            // operations. Publishing a suffix and forgetting this path would leave private
            // plaintext outside lock.
            verify_tracked_export_pair(
                &verified,
                &row.id,
                row.byte_len,
                &row.sha256,
                "could not reuse the tracked exported image",
            )?;
            verified
        } else {
            // The configured vault changed. Retire the old app-owned replica while it is still
            // durably tracked, then start a new lifecycle in the current vault. A user edit blocks
            // this transition and stays preserved/tracked rather than becoming an orphan.
            remove_attachment_exports(
                std::slice::from_ref(row),
                "could not retire the attachment export from the previous vault",
            )?;
            state.db.set_attachment_exported_path(&row.id, None)?;
            choose_attachment_export_path(&dir, row, data)?
        }
    } else {
        choose_attachment_export_path(&dir, row, data)?
    };
    let desired_text = desired
        .to_str()
        .ok_or_else(|| AppError::Export("attachment export path is not valid UTF-8".into()))?;
    // LOAD-BEARING ordering: durable tracking precedes the first plaintext temp/write/link. Any
    // crash after this line is recoverable from `exported_path` plus the deterministic temp name.
    state
        .db
        .set_attachment_exported_path(&row.id, Some(desired_text))?;
    publish_tracked_attachment(&desired, row, data)?;
    desired
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .ok_or_else(|| AppError::Export("attachment export has no safe filename".into()))
}

fn publish_tracked_attachment(
    desired: &std::path::Path,
    row: &AttachmentRecord,
    data: &[u8],
) -> Result<(), AppError> {
    verify_tracked_export_pair(
        desired,
        &row.id,
        row.byte_len,
        &row.sha256,
        "could not publish the tracked exported image",
    )?;
    let tmp = attachment_export_temp_path(desired, &row.id);
    if std::fs::symlink_metadata(desired).is_ok() {
        remove_file_if_present(&tmp, "remove recovered attachment temp failed")?;
        return Ok(());
    }

    if std::fs::symlink_metadata(&tmp).is_err() {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| AppError::Export(format!("create attachment temp failed: {e}")))?;
        file.write_all(data)
            .and_then(|()| file.sync_all())
            .map_err(|e| AppError::Export(format!("write attachment export failed: {e}")))?;
    }
    // A crash may leave the deterministic temp behind. It was verified above, so linking it is a
    // safe resume rather than an overwrite.
    match std::fs::hard_link(&tmp, desired) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            verify_tracked_export_file(
                desired,
                row.byte_len,
                &row.sha256,
                "attachment export path raced with another file",
            )?;
        }
        Err(e) => {
            return Err(AppError::Export(format!(
                "publish attachment export failed: {e}"
            )))
        }
    }
    sync_export_directory(desired)?;
    remove_file_if_present(&tmp, "remove attachment temp failed")?;
    sync_export_directory(desired)?;
    verify_tracked_export_file(
        desired,
        row.byte_len,
        &row.sha256,
        "published attachment failed its integrity check",
    )
}

fn ensure_user_owned_attachment_export(
    row: &AttachmentRecord,
    data: &[u8],
    export_root: &std::path::Path,
) -> Result<String, AppError> {
    let dir = assert_in_vault(export_root, std::path::Path::new("Murmur Attachments"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::Export(format!("create attachment export dir failed: {e}")))?;
    let desired = choose_attachment_export_path(&dir, row, data)?;
    match std::fs::read(&desired) {
        Ok(existing) if existing == data => {}
        Ok(_) => {
            return Err(AppError::Export(
                "user export path changed while the image was being written".into(),
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let tmp = desired.with_file_name(format!(".{}.user-export.murmur.tmp", row.id));
            match std::fs::read(&tmp) {
                Ok(existing) if existing == data => {}
                Ok(_) => {
                    return Err(AppError::Export(
                        "a user-export temp file was changed; it was preserved".into(),
                    ))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&tmp)
                        .map_err(|e| {
                            AppError::Export(format!("create user attachment temp failed: {e}"))
                        })?;
                    file.write_all(data)
                        .and_then(|()| file.sync_all())
                        .map_err(|e| {
                            AppError::Export(format!("write user attachment export failed: {e}"))
                        })?;
                }
                Err(e) => {
                    return Err(AppError::Export(format!(
                        "read user attachment temp failed: {e}"
                    )))
                }
            }
            std::fs::hard_link(&tmp, &desired).map_err(|e| {
                AppError::Export(format!("publish user attachment export failed: {e}"))
            })?;
            sync_export_directory(&desired)?;
            remove_file_if_present(&tmp, "remove user attachment temp failed")?;
            sync_export_directory(&desired)?;
        }
        Err(e) => {
            return Err(AppError::Export(format!(
                "read user attachment export failed: {e}"
            )))
        }
    }
    desired
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .ok_or_else(|| AppError::Export("attachment export has no safe filename".into()))
}

fn sync_export_directory(path: &std::path::Path) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Export("attachment export has no parent directory".into()))?;
    std::fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| AppError::Export(format!("sync attachment export directory failed: {e}")))
}

fn choose_attachment_export_path(
    dir: &std::path::Path,
    row: &AttachmentRecord,
    data: &[u8],
) -> Result<std::path::PathBuf, AppError> {
    let mut n = 0usize;
    loop {
        let name = if n == 0 {
            format!("{}.{}", row.id, row.extension)
        } else {
            format!("{} ({n}).{}", row.id, row.extension)
        };
        let candidate = dir.join(name);
        match std::fs::read(&candidate) {
            Ok(existing) if existing == data => return Ok(candidate),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Err(e) => {
                return Err(AppError::Export(format!(
                    "read existing attachment export failed: {e}"
                )))
            }
        }
        n = n.checked_add(1).ok_or_else(|| {
            AppError::Export("attachment export collision suffix exhausted".into())
        })?;
    }
}

struct ValidatedImage {
    mime_type: &'static str,
    extension: &'static str,
    width: u32,
    height: u32,
}

fn validate_image(mime_type: &str, data: &[u8]) -> Result<ValidatedImage, AppError> {
    if data.is_empty() || data.len() > MAX_ATTACHMENT_BYTES {
        return Err(AppError::InvalidArg(format!(
            "image must be between 1 byte and {} MiB",
            MAX_ATTACHMENT_BYTES / 1024 / 1024
        )));
    }
    let image = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        if data.len() < 24 || &data[12..16] != b"IHDR" {
            return Err(AppError::InvalidArg("malformed PNG".into()));
        }
        ValidatedImage {
            mime_type: "image/png",
            extension: "png",
            width: u32::from_be_bytes([data[16], data[17], data[18], data[19]]),
            height: u32::from_be_bytes([data[20], data[21], data[22], data[23]]),
        }
    } else if data.starts_with(&[0xff, 0xd8]) {
        let (width, height) = jpeg_dimensions(data)?;
        ValidatedImage {
            mime_type: "image/jpeg",
            extension: "jpg",
            width,
            height,
        }
    } else if data.len() >= 30 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        let (width, height) = webp_dimensions(data)?;
        ValidatedImage {
            mime_type: "image/webp",
            extension: "webp",
            width,
            height,
        }
    } else {
        return Err(AppError::InvalidArg(
            "unsupported image bytes (PNG, JPEG, or WebP required)".into(),
        ));
    };
    if mime_type.trim().to_ascii_lowercase() != image.mime_type {
        return Err(AppError::InvalidArg(
            "declared image MIME does not match its bytes".into(),
        ));
    }
    if image.width == 0
        || image.height == 0
        || image.width > MAX_ATTACHMENT_DIMENSION
        || image.height > MAX_ATTACHMENT_DIMENSION
        || u64::from(image.width) * u64::from(image.height) > MAX_ATTACHMENT_PIXELS
    {
        return Err(AppError::InvalidArg(
            "image dimensions exceed the safe limit".into(),
        ));
    }
    Ok(image)
}

fn jpeg_dimensions(data: &[u8]) -> Result<(u32, u32), AppError> {
    let mut i = 2usize;
    while i + 3 < data.len() {
        while i < data.len() && data[i] == 0xff {
            i += 1;
        }
        if i >= data.len() {
            break;
        }
        let marker = data[i];
        i += 1;
        if marker == 0xd8 || marker == 0xd9 || (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            continue;
        }
        if i + 2 > data.len() {
            break;
        }
        let len = usize::from(u16::from_be_bytes([data[i], data[i + 1]]));
        if len < 2 || i + len > data.len() {
            return Err(AppError::InvalidArg("malformed JPEG".into()));
        }
        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) {
            if len < 7 {
                return Err(AppError::InvalidArg("malformed JPEG dimensions".into()));
            }
            let height = u32::from(u16::from_be_bytes([data[i + 3], data[i + 4]]));
            let width = u32::from(u16::from_be_bytes([data[i + 5], data[i + 6]]));
            return Ok((width, height));
        }
        i += len;
    }
    Err(AppError::InvalidArg(
        "JPEG has no supported frame dimensions".into(),
    ))
}

fn webp_dimensions(data: &[u8]) -> Result<(u32, u32), AppError> {
    match &data[12..16] {
        b"VP8X" if data.len() >= 30 => {
            let width =
                1 + u32::from(data[24]) + (u32::from(data[25]) << 8) + (u32::from(data[26]) << 16);
            let height =
                1 + u32::from(data[27]) + (u32::from(data[28]) << 8) + (u32::from(data[29]) << 16);
            Ok((width, height))
        }
        b"VP8L" if data.len() >= 25 && data[20] == 0x2f => {
            let width = 1 + u32::from(data[21]) + ((u32::from(data[22]) & 0x3f) << 8);
            let height = 1
                + (u32::from(data[22]) >> 6)
                + (u32::from(data[23]) << 2)
                + ((u32::from(data[24]) & 0x0f) << 10);
            Ok((width, height))
        }
        b"VP8 " if data.len() >= 30 && data[23..26] == [0x9d, 0x01, 0x2a] => {
            let width = u32::from(u16::from_le_bytes([data[26], data[27]]) & 0x3fff);
            let height = u32::from(u16::from_le_bytes([data[28], data[29]]) & 0x3fff);
            Ok((width, height))
        }
        _ => Err(AppError::InvalidArg("malformed WebP dimensions".into())),
    }
}

fn reject_webp_metadata(data: &[u8]) -> Result<(), AppError> {
    let mut offset = 12usize;
    while offset + 8 <= data.len() {
        let tag = &data[offset..offset + 4];
        let size = u32::from_le_bytes([
            data[offset + 4],
            data[offset + 5],
            data[offset + 6],
            data[offset + 7],
        ]) as usize;
        if matches!(tag, b"EXIF" | b"XMP " | b"ICCP") {
            return Err(AppError::InvalidArg(
                "WebP metadata chunks are not allowed".into(),
            ));
        }
        let padded = size
            .checked_add(size & 1)
            .and_then(|n| n.checked_add(8))
            .ok_or_else(|| AppError::InvalidArg("malformed WebP chunks".into()))?;
        offset = offset
            .checked_add(padded)
            .ok_or_else(|| AppError::InvalidArg("malformed WebP chunks".into()))?;
        if offset > data.len() {
            return Err(AppError::InvalidArg("malformed WebP chunks".into()));
        }
    }
    if offset != data.len() {
        return Err(AppError::InvalidArg("malformed WebP chunks".into()));
    }
    Ok(())
}

fn decode_base64(input: &str) -> Result<Vec<u8>, AppError> {
    if input.len() > MAX_BASE64_INPUT {
        return Err(AppError::InvalidArg(
            "encoded image exceeds the size limit".into(),
        ));
    }
    let encoded = input
        .split_once(",")
        .filter(|(prefix, _)| prefix.starts_with("data:image/") && prefix.ends_with(";base64"))
        .map(|(_, data)| data)
        .unwrap_or(input);
    if encoded.bytes().any(|b| b.is_ascii_whitespace()) {
        return Err(AppError::InvalidArg(
            "base64 image data must not contain whitespace".into(),
        ));
    }
    let padding = encoded.bytes().rev().take_while(|&b| b == b'=').count();
    if padding > 2 {
        return Err(AppError::InvalidArg("invalid base64 padding".into()));
    }
    let payload_len = encoded.len().saturating_sub(padding);
    let payload = &encoded[..payload_len];
    if payload.as_bytes().contains(&b'=')
        || (padding > 0 && encoded.len() % 4 != 0)
        || (padding == 1 && payload_len % 4 != 3)
        || (padding == 2 && payload_len % 4 != 2)
        || (padding == 0 && payload_len % 4 == 1)
    {
        return Err(AppError::InvalidArg("invalid base64 padding".into()));
    }
    let mut out = Vec::with_capacity(payload.len().saturating_mul(3) / 4);
    let mut acc = 0u32;
    let mut bits = 0u8;
    for b in payload.bytes() {
        let value = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return Err(AppError::InvalidArg("invalid base64 image data".into())),
        };
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    if bits >= 6 || (bits > 0 && (acc & ((1u32 << bits) - 1)) != 0) {
        return Err(AppError::InvalidArg("invalid base64 image data".into()));
    }
    if out.len() > MAX_ATTACHMENT_BYTES {
        return Err(AppError::InvalidArg(
            "decoded image exceeds the size limit".into(),
        ));
    }
    Ok(out)
}

fn encode_base64(data: &[u8], url_safe: bool, padding: bool) -> String {
    let alphabet = if url_safe {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
    } else {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
    };
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(alphabet[((n >> 18) & 63) as usize] as char);
        out.push(alphabet[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(alphabet[((n >> 6) & 63) as usize] as char);
        } else if padding {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(alphabet[(n & 63) as usize] as char);
        } else if padding {
            out.push('=');
        }
    }
    out
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        b.extend_from_slice(&width.to_be_bytes());
        b.extend_from_slice(&height.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        b
    }

    #[test]
    fn base64url_round_trip_without_padding() {
        let bytes = png(320, 200);
        let encoded = encode_base64(&bytes, true, false);
        assert_eq!(decode_base64(&encoded).unwrap(), bytes);
    }

    #[test]
    fn png_magic_dimensions_and_mime_are_enforced() {
        let bytes = png(320, 200);
        let image = validate_image("image/png", &bytes).unwrap();
        assert_eq!((image.width, image.height), (320, 200));
        assert!(validate_image("image/jpeg", &bytes).is_err());
        assert!(validate_image("image/png", &png(12_001, 2)).is_err());
    }

    #[test]
    fn data_url_is_standard_padded_base64() {
        assert_eq!(encode_base64(b"a", false, true), "YQ==");
    }
}
