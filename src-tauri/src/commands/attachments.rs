//! Gated note-image attachment commands and the shared E2EE-bundle seam.

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom, Write};

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
            if state.db.is_task_source(document_id)? {
                let org_id = state
                    .db
                    .org_task_org_for_source(document_id)?
                    .ok_or_else(|| AppError::Auth("no visible live task for this image".into()))?;
                super::tasks::require_task_read_context(state, &org_id)?;
                if state
                    .db
                    .visible_org_task_item_for_source(document_id)?
                    .is_none()
                {
                    return Err(AppError::Auth("no visible live task for this image".into()));
                }
                return Ok(());
            }
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
            if let Some(org_id) = state.db.org_task_org_for_item(item_id)? {
                super::tasks::require_task_read_context(state, &org_id)?;
                if !state.db.visible_org_task_for_item(item_id)? {
                    return Err(AppError::Auth("no visible live task for this image".into()));
                }
                return Ok(());
            }
            // The org store applies both live-item and per-instance context gates in SQL.
            if state.db.get_org_item(item_id)?.is_none()
                && !state.db.visible_org_task_for_item(item_id)?
            {
                return Err(AppError::InvalidArg(
                    "no visible live org item for this image".into(),
                ));
            }
        }
    }
    Ok(())
}

fn require_editable_task_attachment_owner(
    state: &AppState,
    owner_kind: &str,
    owner_id: &str,
) -> Result<(), AppError> {
    let item_id = match owner_kind {
        "org" => {
            let ctx = state.db.org_item_edit_ctx(owner_id)?;
            if ctx.as_ref().and_then(|ctx| ctx.source_kind.as_deref()) != Some("task") {
                return Err(AppError::Auth(
                    "only editable shared tasks accept local image changes".into(),
                ));
            }
            Some(owner_id.to_string())
        }
        "task" => state.db.visible_org_task_item_for_source(owner_id)?,
        _ => return Ok(()),
    };
    let Some(item_id) = item_id else {
        return Err(AppError::Auth(
            "only editable shared tasks accept local image changes".into(),
        ));
    };
    let org_id = state
        .db
        .org_task_org_for_item(&item_id)?
        .ok_or_else(|| AppError::Auth("no visible live task for this image".into()))?;
    super::tasks::require_task_read_context(state, &org_id)?;
    let (can_edit, _) = org_item_permissions(state, &item_id)?;
    if !can_edit {
        return Err(AppError::Auth(
            "only editable shared tasks accept local image changes".into(),
        ));
    }
    Ok(())
}

pub(crate) fn plaintext_attachment_data(
    state: &AppState,
    row: &AttachmentRecord,
) -> Result<Vec<u8>, AppError> {
    // Central byte gate: callers may resolve/list an attachment through different surfaces, but no
    // one may obtain plaintext after Task membership/session/context invalidation.
    gate_attachment_owner(state, &row.owner)?;
    plaintext_attachment_data_after_owner_gate(state, row)
}

fn plaintext_attachment_data_after_owner_gate(
    state: &AppState,
    row: &AttachmentRecord,
) -> Result<Vec<u8>, AppError> {
    // Folder ownership is part of the BYTE authorization, not merely the sealed-blob decrypt path.
    // `meeting_is_unlocked` allows a legacy meeting when every governing folder is open, but such a
    // multi-folder split has no unique key/export domain. Resolve it before returning even already-
    // plaintext bytes so list/render/export all fail closed on ambiguity. `None` remains valid for
    // a genuinely unfiled meeting and for org-owned rows.
    let folder_id = state.db.folder_for_attachment_owner(&row.owner)?;
    if !row.data.is_empty() {
        verify_stored_attachment(row, &row.data)?;
        return Ok(row.data.clone());
    }
    let blob = row.data_blob.as_deref().ok_or_else(|| {
        AppError::Storage("attachment has neither plaintext nor a recoverable seal".into())
    })?;
    let folder_id = folder_id
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
    require_editable_task_attachment_owner(state, owner_kind, owner_id)?;
    let owner = state.db.resolve_attachment_owner(owner_kind, owner_id)?;
    gate_attachment_owner(state, &owner)?;
    let data = decode_base64(data_base64)?;
    let image = validate_image(mime_type, &data)?;
    // WebKit's <canvas> cannot ENCODE WebP (`toBlob("image/webp")` silently yields PNG), so the FE
    // falls back to a metadata-free PNG. Accept both, running the metadata rejector that matches the
    // container. JPEG and any other source container are never produced by the local normalizer.
    match image.mime_type {
        "image/webp" => reject_webp_metadata(&data)?,
        "image/png" => reject_png_metadata(&data)?,
        _ => {
            return Err(AppError::InvalidArg(
                "new images must be locally normalized to metadata-free WebP or PNG".into(),
            ))
        }
    }
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
    require_editable_task_attachment_owner(state, owner_kind, owner_id)?;
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

/// Initial/republish Task egress has already authenticated the actor and resolved the exact target
/// org, but the first publish has no `org_tasks` projection yet. This narrowly-authorized seam binds
/// that org witness to a real hidden Task source before reading any image bytes; user-facing reads
/// must continue through [`attachment_bundle_for_owner`] and its live projection gate.
pub(crate) fn attachment_bundle_for_task_source_authorized(
    state: &AppState,
    owner: &AttachmentOwner,
    org_id: &str,
    referenced_ids: &HashSet<String>,
) -> Result<Vec<AttachmentBundleItem>, AppError> {
    let _lifecycle = lifecycle_guard(state);
    super::tasks::require_task_read_context(state, org_id)?;
    let AttachmentOwner::Document { document_id } = owner else {
        return Err(AppError::Auth(
            "task publish attachment owner is not a task source".into(),
        ));
    };
    if !state.db.is_task_source(document_id)? {
        return Err(AppError::Auth(
            "task publish attachment owner is not a task source".into(),
        ));
    }
    if let Some(projected_org_id) = state.db.org_task_org_for_source(document_id)? {
        if projected_org_id != org_id {
            return Err(AppError::Auth(
                "task publish attachment org witness mismatch".into(),
            ));
        }
    }
    state
        .db
        .list_referenced_attachments(owner, referenced_ids)?
        .iter()
        .map(|row| {
            let data = plaintext_attachment_data_after_owner_gate(state, row)?;
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
        // A shared note may carry either a normalized WebP or the metadata-free PNG fallback that
        // WebKit clients now produce; both must materialize on the recipient.
        if !matches!(
            (item.mime_type.as_str(), item.extension.as_str()),
            ("image/webp", "webp") | ("image/png", "png")
        ) {
            return Err(AppError::InvalidArg(
                "attachments must be normalized WebP or PNG images".into(),
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
        match image.mime_type {
            "image/webp" => reject_webp_metadata(&item.data)?,
            "image/png" => reject_png_metadata(&item.data)?,
            _ => {
                return Err(AppError::InvalidArg(
                    "attachments must be normalized WebP or PNG images".into(),
                ))
            }
        }
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

struct AttachmentExportFileSnapshot {
    path: std::path::PathBuf,
    bytes: zeroize::Zeroizing<Vec<u8>>,
    permissions: std::fs::Permissions,
    exact: crate::export::ExactFileLink,
}

struct CreatedAttachmentExport {
    link: crate::export::ExactFileLink,
}

struct AttachmentExportRollbackEntry {
    id: String,
    owner: AttachmentOwner,
    byte_len: u64,
    sha256: [u8; 32],
    before_exported_path: Option<String>,
    before_files: Vec<AttachmentExportFileSnapshot>,
    staged_path: Option<std::path::PathBuf>,
    staged_before_files: Vec<AttachmentExportFileSnapshot>,
    created_files: Vec<CreatedAttachmentExport>,
}

/// Exact pre-publication receipts for the attachment side of finalized-recording filing. Entries
/// contain plaintext only in zeroizing memory and never cross IPC or the egress/logging seams.
pub(crate) struct AttachmentExportRollbackJournal {
    attempt_id: String,
    entries: Vec<AttachmentExportRollbackEntry>,
    vault: Option<crate::export::ExactVault>,
}

impl Default for AttachmentExportRollbackJournal {
    fn default() -> Self {
        Self::with_attempt_id(uuid::Uuid::new_v4().to_string())
    }
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum TestAttachmentWriteFault {
    PartialWriteUnknownHardlinkAndReplacement,
}

#[cfg(test)]
thread_local! {
    static TEST_ATTACHMENT_WRITE_FAULT: std::cell::Cell<Option<TestAttachmentWriteFault>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_test_attachment_partial_write_fault() {
    TEST_ATTACHMENT_WRITE_FAULT.with(|slot| {
        slot.set(Some(
            TestAttachmentWriteFault::PartialWriteUnknownHardlinkAndReplacement,
        ));
    });
}

#[cfg(test)]
fn take_test_attachment_write_fault() -> Option<TestAttachmentWriteFault> {
    TEST_ATTACHMENT_WRITE_FAULT.with(|slot| slot.take())
}

impl AttachmentExportRollbackJournal {
    pub(crate) fn with_attempt_id(attempt_id: String) -> Self {
        Self {
            attempt_id,
            entries: Vec::new(),
            vault: None,
        }
    }

    pub(crate) fn configure_vault(
        &mut self,
        vault_root: &std::path::Path,
    ) -> Result<crate::export::ExactVault, AppError> {
        if let Some(vault) = self.vault.as_ref() {
            if vault.configured_path() != vault_root {
                return Err(AppError::Export(
                    "one attachment filing journal cannot span two vault roots".into(),
                ));
            }
            return Ok(vault.clone());
        }
        let vault = crate::export::ExactVault::open(vault_root)?;
        self.vault = Some(vault.clone());
        Ok(vault)
    }

    fn snapshot_file_if_present(
        path: &std::path::Path,
        expected_len: u64,
        expected_sha256: &[u8; 32],
    ) -> Result<Option<AttachmentExportFileSnapshot>, AppError> {
        use std::os::unix::fs::MetadataExt;

        let Some(exact) = crate::export::open_exact_absolute_existing_file(path)? else {
            return Ok(None);
        };
        let (bytes, before) = exact.read_stable_bytes(expected_len)?;
        if before.nlink() != 1 || before.len() != expected_len {
            return Err(AppError::Export(
                "attachment export rollback target is not a regular file".into(),
            ));
        }
        if bytes.len() as u64 != expected_len
            || Sha256::digest(&bytes).as_slice() != expected_sha256
        {
            return Err(AppError::Export(
                "attachment rollback snapshot does not match canonical SQLCipher integrity".into(),
            ));
        }
        Ok(Some(AttachmentExportFileSnapshot {
            path: path.to_path_buf(),
            bytes: zeroize::Zeroizing::new(bytes),
            permissions: before.permissions(),
            exact,
        }))
    }

    fn snapshot_projection(
        path: &std::path::Path,
        attachment_id: &str,
        expected_len: u64,
        expected_sha256: &[u8; 32],
    ) -> Result<Vec<AttachmentExportFileSnapshot>, AppError> {
        let mut files = Vec::new();
        if let Some(snapshot) = Self::snapshot_file_if_present(path, expected_len, expected_sha256)?
        {
            files.push(snapshot);
        }
        let twin = attachment_export_temp_path(path, attachment_id);
        if let Some(snapshot) =
            Self::snapshot_file_if_present(&twin, expected_len, expected_sha256)?
        {
            files.push(snapshot);
        }
        Ok(files)
    }

    fn capture_before(&mut self, row: &AttachmentRecord) -> Result<(), AppError> {
        if self.entries.iter().any(|entry| entry.id == row.id) {
            return Ok(());
        }
        let before_files = row
            .exported_path
            .as_deref()
            .map(|path| {
                Self::snapshot_projection(
                    std::path::Path::new(path),
                    &row.id,
                    row.byte_len,
                    &row.sha256,
                )
            })
            .transpose()?
            .unwrap_or_default();
        self.entries.push(AttachmentExportRollbackEntry {
            id: row.id.clone(),
            owner: row.owner.clone(),
            byte_len: row.byte_len,
            sha256: row.sha256,
            before_exported_path: row.exported_path.clone(),
            before_files,
            staged_path: None,
            staged_before_files: Vec::new(),
            created_files: Vec::new(),
        });
        Ok(())
    }

    fn prepare_staged(
        &mut self,
        row: &AttachmentRecord,
        path: &std::path::Path,
    ) -> Result<(), AppError> {
        let snapshots = Self::snapshot_projection(path, &row.id, row.byte_len, &row.sha256)?;
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.id == row.id)
            .ok_or_else(|| {
                AppError::Storage("attachment filing journal missed its pre-stage receipt".into())
            })?;
        if let Some(existing) = entry.staged_path.as_deref() {
            if existing != path {
                return Err(AppError::Storage(
                    "one attachment filing attempt selected two projection paths".into(),
                ));
            }
            return Ok(());
        }
        entry.staged_path = Some(path.to_path_buf());
        entry.staged_before_files = snapshots;
        Ok(())
    }

    fn remove_snapshotted_staged_file(
        &self,
        row: &AttachmentRecord,
        path: &std::path::Path,
    ) -> Result<bool, AppError> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.id == row.id)
            .ok_or_else(|| {
                AppError::Storage("attachment filing journal missed its staged snapshot".into())
            })?;
        let Some(snapshot) = entry
            .staged_before_files
            .iter()
            .find(|snapshot| snapshot.path == path)
        else {
            return Ok(false);
        };
        crate::export::remove_exact_created_links(
            std::slice::from_ref(&snapshot.exact),
            snapshot.bytes.len() as u64,
            &entry.sha256,
        )?;
        Ok(true)
    }

    fn remove_captured_before_projection(&self, row: &AttachmentRecord) -> Result<(), AppError> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.id == row.id)
            .ok_or_else(|| {
                AppError::Storage("attachment filing journal missed its old-vault snapshot".into())
            })?;
        let mut groups =
            std::collections::HashMap::<(u64, u64), Vec<&crate::export::ExactFileLink>>::new();
        for snapshot in &entry.before_files {
            groups
                .entry(snapshot.exact.identity())
                .or_default()
                .push(&snapshot.exact);
        }
        for group in groups.values() {
            crate::export::remove_exact_created_link_refs(group, entry.byte_len, &entry.sha256)?;
        }
        Ok(())
    }

    fn mark_created_link(
        &mut self,
        row: &AttachmentRecord,
        link: &mut Option<crate::export::ExactFileLink>,
    ) -> Result<(), AppError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.id == row.id)
            .ok_or_else(|| {
                AppError::Storage("attachment filing journal missed its creation receipt".into())
            })?;
        if entry.created_files.iter().any(|created| {
            link.as_ref()
                .is_some_and(|link| created.link.path() == link.path())
        }) {
            return Err(AppError::Storage(
                "attachment filing journal received duplicate creation authority".into(),
            ));
        }
        let link = link.take().ok_or_else(|| {
            AppError::Storage("attachment creation authority was already consumed".into())
        })?;
        entry.created_files.push(CreatedAttachmentExport { link });
        Ok(())
    }

    fn created_link(
        &self,
        row: &AttachmentRecord,
        path: &std::path::Path,
    ) -> Result<&crate::export::ExactFileLink, AppError> {
        self.entries
            .iter()
            .find(|entry| entry.id == row.id)
            .and_then(|entry| {
                entry
                    .created_files
                    .iter()
                    .find(|created| created.link.path() == path)
            })
            .map(|created| &created.link)
            .ok_or_else(|| {
                AppError::Storage("attachment filing journal lost exact creation authority".into())
            })
    }

    fn created_link_mut(
        &mut self,
        row: &AttachmentRecord,
        path: &std::path::Path,
    ) -> Result<&mut crate::export::ExactFileLink, AppError> {
        self.entries
            .iter_mut()
            .find(|entry| entry.id == row.id)
            .and_then(|entry| {
                entry
                    .created_files
                    .iter_mut()
                    .find(|created| created.link.path() == path)
            })
            .map(|created| &mut created.link)
            .ok_or_else(|| {
                AppError::Storage("attachment filing journal lost exact creation authority".into())
            })
    }

    fn staged_snapshot_link(
        &self,
        row: &AttachmentRecord,
        path: &std::path::Path,
    ) -> Result<&crate::export::ExactFileLink, AppError> {
        self.entries
            .iter()
            .find(|entry| entry.id == row.id)
            .and_then(|entry| {
                entry
                    .staged_before_files
                    .iter()
                    .find(|snapshot| snapshot.path == path)
            })
            .map(|snapshot| &snapshot.exact)
            .ok_or_else(|| {
                AppError::Storage("attachment filing journal lost its staged snapshot".into())
            })
    }

    fn staged_snapshot_link_optional(
        &self,
        row: &AttachmentRecord,
        path: &std::path::Path,
    ) -> Option<&crate::export::ExactFileLink> {
        self.entries
            .iter()
            .find(|entry| entry.id == row.id)
            .and_then(|entry| {
                entry
                    .staged_before_files
                    .iter()
                    .find(|snapshot| snapshot.path == path)
            })
            .map(|snapshot| &snapshot.exact)
    }

    fn snapshot_matches_current(snapshot: &AttachmentExportFileSnapshot) -> Result<bool, AppError> {
        use std::os::unix::fs::PermissionsExt;

        if !snapshot.exact.is_present()? {
            return Ok(false);
        }
        let digest: [u8; 32] = Sha256::digest(snapshot.bytes.as_slice()).into();
        match snapshot
            .exact
            .read_stable_bytes(snapshot.bytes.len() as u64)
        {
            Ok((bytes, metadata)) => {
                if metadata.permissions().mode() != snapshot.permissions.mode()
                    || bytes.as_slice() != snapshot.bytes.as_slice()
                    || Sha256::digest(&bytes).as_slice() != digest
                {
                    return Err(AppError::Export(
                        "refusing to overwrite a changed attachment rollback target".into(),
                    ));
                }
            }
            Err(error) => return Err(error),
        }
        Ok(true)
    }

    fn restore_snapshot(
        state: &AppState,
        attempt_id: &str,
        entry: &AttachmentExportRollbackEntry,
        snapshot: &AttachmentExportFileSnapshot,
    ) -> Result<(), AppError> {
        use std::os::unix::fs::MetadataExt;

        if Self::snapshot_matches_current(snapshot)? {
            return Ok(());
        }
        let projection_id = uuid::Uuid::new_v4().to_string();
        let path = snapshot
            .path
            .to_str()
            .ok_or_else(|| AppError::Export("attachment restore path is not valid UTF-8".into()))?;
        let folder_id = state
            .db
            .folder_for_attachment_owner(&entry.owner)?
            .unwrap_or_default();
        state
            .db
            .reserve_filing_projection(&crate::storage::FilingProjectionReservation {
                attempt_id,
                projection_id: &projection_id,
                operation_kind: "recording_filing_attachment_restore",
                owner_kind: "attachment",
                owner_id: &entry.id,
                provider_id: "",
                source_folder_id: &folder_id,
                target_folder_id: &folder_id,
                source_path: entry.before_exported_path.as_deref(),
                temp_path: path,
                final_path: Some(path),
                expected_len: snapshot.bytes.len() as u64,
                expected_sha256: &entry.sha256,
            })?;
        let mut ownership = match snapshot.exact.create_replacement(0o600) {
            Ok(ownership) => ownership,
            Err(error) => {
                state
                    .db
                    .clear_filing_projection(attempt_id, &projection_id)?;
                return Err(error);
            }
        };
        if let Err(error) = state.db.bind_filing_projection_identity(
            attempt_id,
            &projection_id,
            ownership.identity().0,
            ownership.identity().1,
        ) {
            let cleanup = crate::export::remove_exact_created_link(&ownership, 1);
            if cleanup.is_ok() {
                state
                    .db
                    .clear_filing_projection(attempt_id, &projection_id)?;
            }
            return Err(match cleanup {
                Ok(()) => error,
                Err(cleanup) => AppError::Storage(format!(
                    "{error}; unbound attachment restore cleanup failed: {cleanup}"
                )),
            });
        }
        let restore = (|| -> Result<(), AppError> {
            ownership
                .file_mut()
                .write_all(&snapshot.bytes)
                .and_then(|()| {
                    ownership
                        .file_mut()
                        .set_permissions(snapshot.permissions.clone())
                })
                .and_then(|()| ownership.file_mut().sync_all())
                .map_err(|error| {
                    AppError::Export(format!(
                        "could not restore an attachment rollback target: {error}"
                    ))
                })?;
            ownership
                .file_mut()
                .seek(SeekFrom::Start(0))
                .map_err(|error| {
                    AppError::Export(format!("seek restored attachment failed: {error}"))
                })?;
            let mut readback = Vec::with_capacity(snapshot.bytes.len());
            ownership
                .file_mut()
                .read_to_end(&mut readback)
                .map_err(|error| {
                    AppError::Export(format!("read back restored attachment failed: {error}"))
                })?;
            let metadata = ownership.file_mut().metadata().map_err(|error| {
                AppError::Export(format!("stat restored attachment failed: {error}"))
            })?;
            if metadata.dev() != ownership.identity().0
                || metadata.ino() != ownership.identity().1
                || metadata.nlink() != 1
                || readback.as_slice() != snapshot.bytes.as_slice()
            {
                return Err(AppError::Export(
                    "restored attachment failed exact-inode readback verification".into(),
                ));
            }
            ownership.sync_parent()
        })();
        if let Err(original) = restore {
            return match crate::export::remove_exact_created_link(&ownership, 1) {
                Ok(()) => {
                    state
                        .db
                        .clear_filing_projection(attempt_id, &projection_id)?;
                    Err(original)
                }
                Err(cleanup) => match ownership.scrub_attempt_owned_plaintext() {
                    Ok(()) => {
                        state
                            .db
                            .clear_filing_projection(attempt_id, &projection_id)?;
                        Err(AppError::Storage(format!(
                            "{original}; partial attachment restore unlink refused ({cleanup}); retained inode scrubbed"
                        )))
                    }
                    Err(scrub) => Err(AppError::Storage(format!(
                        "{original}; partial attachment restore cleanup failed: {cleanup}; retained-inode scrub failed: {scrub}"
                    ))),
                },
            };
        }
        state
            .db
            .mark_filing_projection_published(attempt_id, &projection_id)?;
        Ok(())
    }

    fn rollback_entry(
        &self,
        state: &AppState,
        entry: &AttachmentExportRollbackEntry,
    ) -> Result<(), AppError> {
        let current_path = state.db.attachment_exported_path(&entry.id)?;
        let staged_text = entry
            .staged_path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string());
        if current_path != entry.before_exported_path
            && current_path != staged_text
            && current_path.is_some()
        {
            return Err(AppError::Export(
                "attachment projection changed concurrently during filing rollback".into(),
            ));
        }

        // A deterministic temp can appear in both sets. `before_files` is the authoritative
        // pre-attempt inode; a staged snapshot may intentionally be displaced by publication and
        // must never replace that rollback authority merely because sorting changed tie order.
        let mut snapshots_by_path = std::collections::BTreeMap::new();
        for snapshot in &entry.before_files {
            snapshots_by_path.insert(snapshot.path.clone(), snapshot);
        }
        for snapshot in &entry.staged_before_files {
            snapshots_by_path
                .entry(snapshot.path.clone())
                .or_insert(snapshot);
        }
        let mut seen_snapshot_inodes = std::collections::HashSet::new();
        let snapshots = snapshots_by_path
            .into_values()
            .filter(|snapshot| seen_snapshot_inodes.insert(snapshot.exact.identity()))
            .collect::<Vec<_>>();
        let mut preflight_failures = Vec::new();
        for snapshot in &snapshots {
            if let Err(error) = Self::snapshot_matches_current(snapshot) {
                preflight_failures.push(format!("{}: {error}", snapshot.path.display()));
            }
        }

        let mut created_groups =
            std::collections::HashMap::<(u64, u64), Vec<&crate::export::ExactFileLink>>::new();
        for created in &entry.created_files {
            created_groups
                .entry(created.link.identity())
                .or_default()
                .push(&created.link);
        }
        if !preflight_failures.is_empty() {
            return Err(AppError::Export(format!(
                "attachment rollback preflight failed: {}",
                preflight_failures.join("; ")
            )));
        }

        let mut removal_failures = Vec::new();
        for links in created_groups.values() {
            if let Err(error) =
                crate::export::remove_exact_created_link_refs(links, entry.byte_len, &entry.sha256)
            {
                let scrub = links
                    .first()
                    .ok_or_else(|| {
                        AppError::Storage("attachment rollback group lost its exact link".into())
                    })?
                    .scrub_attempt_owned_plaintext();
                if let Err(scrub) = scrub {
                    removal_failures.push(format!(
                        "{error}; exact attachment plaintext scrub also failed: {scrub}"
                    ));
                }
            }
        }
        if !removal_failures.is_empty() {
            return Err(AppError::Export(format!(
                "attachment rollback removal failed: {}",
                removal_failures.join("; ")
            )));
        }

        // Fail closed while old projections are recreated. The canonical row must not point at a
        // just-created empty/partial inode; only verified readback below restores the prior path.
        state.db.set_attachment_exported_path(&entry.id, None)?;
        let mut restore_failures = Vec::new();
        for snapshot in snapshots {
            if let Err(error) = Self::restore_snapshot(state, &self.attempt_id, entry, snapshot) {
                restore_failures.push(format!("{}: {error}", snapshot.path.display()));
            }
        }
        if !restore_failures.is_empty() {
            return Err(AppError::Export(format!(
                "attachment rollback restore failed: {}",
                restore_failures.join("; ")
            )));
        }
        state.db.promote_attachment_restore_and_clear(
            &self.attempt_id,
            &entry.id,
            entry.before_exported_path.as_deref(),
        )?;
        if state.db.attachment_exported_path(&entry.id)? != entry.before_exported_path {
            return Err(AppError::Storage(
                "attachment rollback did not restore its canonical path".into(),
            ));
        }
        Ok(())
    }

    /// Attempt every attachment receipt in reverse publication order and surface all failures.
    pub(crate) fn rollback(&self, state: &AppState) -> Result<(), AppError> {
        let mut failures = Vec::new();
        for entry in self.entries.iter().rev() {
            if let Err(error) = self.rollback_entry(state, entry) {
                failures.push(format!("{}: {error}", entry.id));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(AppError::Export(format!(
                "one or more attachment projections could not be rolled back: {}",
                failures.join("; ")
            )))
        }
    }
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
    render_markdown_with_attachment_markers(state, owner, markdown, vault_root, markers, true, None)
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
    render_markdown_with_attachment_markers(
        state,
        owner,
        markdown,
        export_root,
        markers,
        false,
        None,
    )
}

/// Ordinary under-lifecycle variant. Holding the lifecycle mutex is not read authorization, so the
/// renderer still goes through the central attachment-owner gate.
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
    render_markdown_with_attachment_markers(state, owner, markdown, vault_root, markers, true, None)
}

/// Filing-only renderer. Its journal is updated at the exact metadata/write/link seams so a later
/// bundle-stage or SQLite failure can restore the attachment projection without guessing which
/// files this attempt created.
pub(crate) fn render_markdown_with_attachments_for_export_with_rollback_journal(
    state: &AppState,
    owner: &AttachmentOwner,
    markdown: &str,
    vault_root: &std::path::Path,
    journal: &mut AttachmentExportRollbackJournal,
) -> Result<String, AppError> {
    let markers = parse_attachment_markers(markdown)?;
    if markers.is_empty() {
        return Ok(markdown.to_string());
    }
    render_markdown_with_attachment_markers(
        state,
        owner,
        markdown,
        vault_root,
        markers,
        true,
        Some(journal),
    )
}

fn render_markdown_with_attachment_markers(
    state: &AppState,
    owner: &AttachmentOwner,
    markdown: &str,
    vault_root: &std::path::Path,
    markers: Vec<AttachmentMarker>,
    tracked: bool,
    mut rollback_journal: Option<&mut AttachmentExportRollbackJournal>,
) -> Result<String, AppError> {
    let ids: HashSet<String> = markers.iter().map(|marker| marker.id.clone()).collect();
    let rows = state.db.list_referenced_attachments(owner, &ids)?;
    let mut names = std::collections::HashMap::with_capacity(rows.len());
    for row in rows {
        let data = plaintext_attachment_data(state, &row)?;
        let name = if tracked {
            ensure_attachment_exported(
                state,
                &row,
                &data,
                vault_root,
                rollback_journal.as_deref_mut(),
            )?
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
    mut rollback_journal: Option<&mut AttachmentExportRollbackJournal>,
) -> Result<String, AppError> {
    let exact_vault = if let Some(journal) = rollback_journal.as_deref_mut() {
        journal.configure_vault(vault_root)?
    } else {
        crate::export::ExactVault::open(vault_root)?
    };
    if let Some(journal) = rollback_journal.as_deref_mut() {
        journal.capture_before(row)?;
    }
    let dir = assert_in_vault(vault_root, std::path::Path::new("Murmur Attachments"))?;
    exact_vault.ensure_directory(&dir)?;
    let attachment_dir_identity = exact_vault.directory_identity(&dir)?;
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
            match exact_vault.existing_bytes_match(&verified, data)? {
                Some(true) => {}
                Some(false) | None => {
                    return Err(AppError::Export(
                        "could not reuse the tracked exported image: exact bytes are missing or changed"
                            .into(),
                    ))
                }
            }
            verified
        } else {
            // The configured vault changed. Retire the old app-owned replica while it is still
            // durably tracked, then start a new lifecycle in the current vault. A user edit blocks
            // this transition and stays preserved/tracked rather than becoming an orphan.
            if let Some(journal) = rollback_journal.as_deref_mut() {
                journal.remove_captured_before_projection(row)?;
            } else {
                remove_tracked_attachment_projection_exact(row)?;
            }
            state.db.set_attachment_exported_path(&row.id, None)?;
            choose_attachment_export_path_exact(&exact_vault, &dir, row, data)?
        }
    } else {
        choose_attachment_export_path_exact(&exact_vault, &dir, row, data)?
    };
    let desired_text = desired
        .to_str()
        .ok_or_else(|| AppError::Export("attachment export path is not valid UTF-8".into()))?;
    if let Some(journal) = rollback_journal.as_deref_mut() {
        journal.prepare_staged(row, &desired)?;
    }
    // LOAD-BEARING ordering: durable tracking precedes the first plaintext temp/write/link. Any
    // crash after this line is recoverable from `exported_path` plus the deterministic temp name.
    state
        .db
        .set_attachment_exported_path(&row.id, Some(desired_text))?;
    exact_vault.verify_directory_identity(&dir, attachment_dir_identity)?;
    publish_tracked_attachment(
        state,
        &exact_vault,
        attachment_dir_identity,
        &desired,
        row,
        data,
        rollback_journal,
    )?;
    desired
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .ok_or_else(|| AppError::Export("attachment export has no safe filename".into()))
}

fn publish_tracked_attachment(
    state: &AppState,
    vault: &crate::export::ExactVault,
    directory_identity: (u64, u64),
    desired: &std::path::Path,
    row: &AttachmentRecord,
    data: &[u8],
    rollback_journal: Option<&mut AttachmentExportRollbackJournal>,
) -> Result<(), AppError> {
    if vault.existing_bytes_match(desired, data)? == Some(false) {
        return Err(AppError::Export(
            "could not publish the tracked exported image: destination bytes changed".into(),
        ));
    }
    let tmp = attachment_export_temp_path(desired, &row.id);
    if let Some(journal) = rollback_journal {
        return publish_tracked_attachment_journaled(
            state,
            vault,
            directory_identity,
            desired,
            &tmp,
            row,
            data,
            journal,
        );
    }
    if vault.existing_bytes_match(desired, data)? == Some(true) {
        if let Some(link) = vault.open_existing_file(&tmp)? {
            crate::export::remove_exact_verified_link(&link, 1, row.byte_len, &row.sha256)?;
        }
        return Ok(());
    }

    let mut temp_link = if let Some(link) = vault.open_existing_file(&tmp)? {
        let (bytes, _) = link.read_stable_bytes(row.byte_len)?;
        if bytes.as_slice() != data {
            return Err(AppError::Export(
                "recovered tracked attachment temp has changed bytes".into(),
            ));
        }
        link
    } else {
        vault.verify_directory_identity(
            desired.parent().ok_or_else(|| {
                AppError::Export("attachment export has no parent directory".into())
            })?,
            directory_identity,
        )?;
        let mut link = vault.create_file(&tmp, 0o600)?;
        let write_result = link
            .file_mut()
            .write_all(data)
            .and_then(|()| link.file_mut().sync_all());
        if let Err(error) = write_result {
            return Err(remove_or_scrub_attempt_owned(
                &link,
                AppError::Export(format!("write attachment export failed: {error}")),
                "partial attachment temp",
            ));
        }
        link
    };
    match temp_link.publish_exclusive(desired) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if vault.existing_bytes_match(desired, data)? != Some(true) {
                return Err(AppError::Export(
                    "attachment export path raced with different bytes".into(),
                ));
            }
            crate::export::remove_exact_verified_link(&temp_link, 1, row.byte_len, &row.sha256)?;
            return Ok(());
        }
        Err(error) => {
            return Err(AppError::Export(format!(
                "publish exact attachment export failed: {error}"
            )))
        }
    }
    temp_link.sync_parent()?;
    vault.verify_directory_identity(
        desired
            .parent()
            .ok_or_else(|| AppError::Export("attachment export has no parent directory".into()))?,
        directory_identity,
    )?;
    let (bytes, _) = temp_link.read_stable_bytes(row.byte_len)?;
    if bytes.as_slice() != data {
        return Err(AppError::Export(
            "published attachment failed exact readback".into(),
        ));
    }
    Ok(())
}

fn remove_or_scrub_attempt_owned(
    link: &crate::export::ExactFileLink,
    original: AppError,
    context: &str,
) -> AppError {
    match crate::export::remove_exact_created_link(link, 1) {
        Ok(()) => original,
        Err(cleanup) => match link.scrub_attempt_owned_plaintext() {
            Ok(()) => AppError::Storage(format!(
                "{original}; {context} unlink refused ({cleanup}); retained inode scrubbed"
            )),
            Err(scrub) => AppError::Storage(format!(
                "{original}; {context} cleanup failed: {cleanup}; retained-inode scrub failed: {scrub}"
            )),
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_tracked_attachment_journaled(
    state: &AppState,
    vault: &crate::export::ExactVault,
    directory_identity: (u64, u64),
    desired: &std::path::Path,
    tmp: &std::path::Path,
    row: &AttachmentRecord,
    data: &[u8],
    journal: &mut AttachmentExportRollbackJournal,
) -> Result<(), AppError> {
    use std::os::unix::fs::MetadataExt;

    if vault.existing_bytes_match(desired, data)? == Some(true) {
        if !journal.remove_snapshotted_staged_file(row, tmp)?
            && vault.open_existing_file(tmp)?.is_some()
        {
            return Err(AppError::Export(
                "recovered attachment temp exists without exact snapshot authority".into(),
            ));
        }
        return Ok(());
    }

    let temp_was_created = journal.staged_snapshot_link_optional(row, tmp).is_none();
    let mut durable_projection_id = None;
    if temp_was_created {
        vault.verify_directory_identity(
            desired.parent().ok_or_else(|| {
                AppError::Export("attachment export has no parent directory".into())
            })?,
            directory_identity,
        )?;
        let projection_id = uuid::Uuid::new_v4().to_string();
        let temp_path = tmp.to_str().ok_or_else(|| {
            AppError::Export("attachment filing temp path is not valid UTF-8".into())
        })?;
        let final_path = desired.to_str().ok_or_else(|| {
            AppError::Export("attachment filing target path is not valid UTF-8".into())
        })?;
        let folder_id = state
            .db
            .folder_for_attachment_owner(&row.owner)?
            .unwrap_or_default();
        state
            .db
            .reserve_filing_projection(&crate::storage::FilingProjectionReservation {
                attempt_id: &journal.attempt_id,
                projection_id: &projection_id,
                operation_kind: "recording_filing",
                owner_kind: "attachment",
                owner_id: &row.id,
                provider_id: "",
                source_folder_id: &folder_id,
                target_folder_id: &folder_id,
                source_path: row.exported_path.as_deref(),
                temp_path,
                final_path: Some(final_path),
                expected_len: row.byte_len,
                expected_sha256: &row.sha256,
            })?;
        let link = match vault.create_file(tmp, 0o600) {
            Ok(link) => link,
            Err(error) => {
                state
                    .db
                    .clear_filing_projection(&journal.attempt_id, &projection_id)?;
                return Err(error);
            }
        };
        if let Err(error) = state.db.bind_filing_projection_identity(
            &journal.attempt_id,
            &projection_id,
            link.identity().0,
            link.identity().1,
        ) {
            let cleanup = crate::export::remove_exact_created_link(&link, 1);
            if cleanup.is_ok() {
                state
                    .db
                    .clear_filing_projection(&journal.attempt_id, &projection_id)?;
            }
            return Err(match cleanup {
                Ok(()) => error,
                Err(cleanup) => AppError::Storage(format!(
                    "{error}; unbound attachment filing temp cleanup failed: {cleanup}"
                )),
            });
        }
        durable_projection_id = Some(projection_id);
        let mut authority = Some(link);
        if let Err(original) = journal.mark_created_link(row, &mut authority) {
            let cleanup = authority
                .as_ref()
                .map(|link| crate::export::remove_exact_created_link(link, 1))
                .transpose();
            return Err(match cleanup {
                Ok(_) => original,
                Err(cleanup) => AppError::Storage(format!(
                    "{original}; unjournaled exact attachment temp cleanup failed: {cleanup}"
                )),
            });
        }
        #[cfg(test)]
        let write_fault = take_test_attachment_write_fault();
        #[cfg(test)]
        let write_result = if matches!(
            write_fault,
            Some(TestAttachmentWriteFault::PartialWriteUnknownHardlinkAndReplacement)
        ) {
            let prefix_len = data.len().max(1).div_ceil(2).min(data.len());
            {
                let link = journal.created_link_mut(row, tmp)?;
                link.file_mut()
                    .write_all(&data[..prefix_len])
                    .and_then(|()| link.file_mut().sync_all())
                    .map_err(|error| {
                        AppError::Export(format!("inject partial attachment write failed: {error}"))
                    })?;
            }
            let hostile = tmp.with_file_name(format!(
                ".murmur-test-partial-attachment-hardlink-{}",
                row.id
            ));
            let hostile_authority = journal
                .created_link(row, tmp)?
                .hard_link_sibling(&hostile)
                .map_err(|error| {
                    AppError::Export(format!("inject attachment hardlink failed: {error}"))
                })?;
            drop(hostile_authority);
            std::fs::remove_file(tmp).map_err(|error| {
                AppError::Export(format!("inject attachment name removal failed: {error}"))
            })?;
            std::fs::write(tmp, b"concurrent replacement").map_err(|error| {
                AppError::Export(format!(
                    "inject attachment name replacement failed: {error}"
                ))
            })?;
            Err(std::io::Error::other(
                "injected partial attachment write failure",
            ))
        } else {
            let link = journal.created_link_mut(row, tmp)?;
            link.file_mut()
                .write_all(data)
                .and_then(|()| link.file_mut().sync_all())
        };
        #[cfg(not(test))]
        let write_result = {
            let link = journal.created_link_mut(row, tmp)?;
            link.file_mut()
                .write_all(data)
                .and_then(|()| link.file_mut().sync_all())
        };
        if let Err(error) = write_result {
            let original = AppError::Export(format!("write exact attachment temp failed: {error}"));
            return Err(remove_or_scrub_attempt_owned(
                journal.created_link(row, tmp)?,
                original,
                "partial attachment temp",
            ));
        }
        let metadata = match journal.created_link_mut(row, tmp)?.file_mut().metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                let original =
                    AppError::Export(format!("stat exact attachment temp failed: {error}"));
                return Err(remove_or_scrub_attempt_owned(
                    journal.created_link(row, tmp)?,
                    original,
                    "unverified attachment temp",
                ));
            }
        };
        let identity = journal.created_link(row, tmp)?.identity();
        if metadata.dev() != identity.0
            || metadata.ino() != identity.1
            || metadata.nlink() != 1
            || metadata.len() != row.byte_len
        {
            let original =
                AppError::Export("exact attachment temp failed identity verification".into());
            return Err(remove_or_scrub_attempt_owned(
                journal.created_link(row, tmp)?,
                original,
                "invalid attachment temp",
            ));
        }
    }

    let final_link = {
        let source = if temp_was_created {
            journal.created_link(row, tmp)?
        } else {
            journal.staged_snapshot_link(row, tmp)?
        };
        match source.hard_link_sibling(desired) {
            Ok(link) => Some(link),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if vault.existing_bytes_match(desired, data)? != Some(true) {
                    return Err(AppError::Export(
                        "attachment export path raced with another file".into(),
                    ));
                }
                None
            }
            Err(error) => {
                return Err(AppError::Export(format!(
                    "publish exact attachment export failed: {error}"
                )))
            }
        }
    };
    let published_new = final_link.is_some();
    if let Some(link) = final_link {
        let mut authority = Some(link);
        if let Err(original) = journal.mark_created_link(row, &mut authority) {
            let cleanup = authority
                .as_ref()
                .map(|link| {
                    crate::export::remove_exact_verified_link(link, 2, row.byte_len, &row.sha256)
                })
                .transpose();
            return Err(match cleanup {
                Ok(_) => original,
                Err(cleanup) => match authority
                    .as_ref()
                    .ok_or_else(|| {
                        AppError::Storage(
                            "unjournaled attachment link lost its affine authority".into(),
                        )
                    })?
                    .scrub_attempt_owned_plaintext()
                {
                    Ok(()) => AppError::Storage(format!(
                        "{original}; unjournaled attachment link unlink refused ({cleanup}); retained inode scrubbed"
                    )),
                    Err(scrub) => AppError::Storage(format!(
                        "{original}; unjournaled attachment link cleanup failed: {cleanup}; retained-inode scrub failed: {scrub}"
                    )),
                },
            });
        }
    }
    vault.verify_directory_identity(
        desired
            .parent()
            .ok_or_else(|| AppError::Export("attachment export has no parent directory".into()))?,
        directory_identity,
    )?;
    if published_new {
        journal.created_link(row, desired)?.sync_parent()?;
    }

    let source = if temp_was_created {
        journal.created_link(row, tmp)?
    } else {
        journal.staged_snapshot_link(row, tmp)?
    };
    crate::export::remove_exact_verified_link(
        source,
        if published_new { 2 } else { 1 },
        row.byte_len,
        &row.sha256,
    )?;
    if vault.existing_bytes_match(desired, data)? != Some(true) {
        return Err(AppError::Export(
            "published attachment failed its exact integrity check".into(),
        ));
    }
    if let Some(projection_id) = durable_projection_id {
        state
            .db
            .mark_filing_projection_published(&journal.attempt_id, &projection_id)?;
    }
    Ok(())
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

fn choose_attachment_export_path_exact(
    vault: &crate::export::ExactVault,
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
        match vault.existing_bytes_match(&candidate, data)? {
            None | Some(true) => return Ok(candidate),
            Some(false) => {}
        }
        n = n.checked_add(1).ok_or_else(|| {
            AppError::Export("attachment export collision suffix exhausted".into())
        })?;
    }
}

fn remove_tracked_attachment_projection_exact(row: &AttachmentRecord) -> Result<(), AppError> {
    let Some(path) = row.exported_path.as_deref() else {
        return Ok(());
    };
    let path = std::path::Path::new(path);
    let temp = attachment_export_temp_path(path, &row.id);
    let mut links = Vec::new();
    if let Some(link) = crate::export::open_exact_absolute_existing_file(path)? {
        links.push(link);
    }
    if let Some(link) = crate::export::open_exact_absolute_existing_file(&temp)? {
        links.push(link);
    }
    let mut groups =
        std::collections::HashMap::<(u64, u64), Vec<&crate::export::ExactFileLink>>::new();
    for link in &links {
        groups.entry(link.identity()).or_default().push(link);
    }
    for group in groups.values() {
        crate::export::remove_exact_created_link_refs(group, row.byte_len, &row.sha256)?;
    }
    Ok(())
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

/// Enforce that a PNG the FE claims is metadata-free actually is. Walk the chunk stream with checked
/// arithmetic, require the canonical `IHDR`-first / `IEND`-last framing, reject any trailing bytes,
/// and fail closed if any privacy-bearing ancillary chunk is present. `sRGB` (WebKit's own encoder
/// emits it) and other rendering-intent chunks are benign and allowed. No PII is placed in errors.
fn reject_png_metadata(data: &[u8]) -> Result<(), AppError> {
    const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if data.len() < SIGNATURE.len() || &data[..SIGNATURE.len()] != SIGNATURE {
        return Err(AppError::InvalidArg("malformed PNG".into()));
    }
    let mut offset = SIGNATURE.len();
    let mut first = true;
    let mut saw_iend = false;
    while offset < data.len() {
        if offset + 8 > data.len() {
            return Err(AppError::InvalidArg("malformed PNG chunks".into()));
        }
        let len = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        let tag = &data[offset + 4..offset + 8];
        if first {
            if tag != b"IHDR" {
                return Err(AppError::InvalidArg("PNG must begin with IHDR".into()));
            }
            first = false;
        }
        // eXIf/tEXt/zTXt/iTXt carry arbitrary embedded text, iCCP an ICC profile, tIME a timestamp —
        // all can re-introduce the private metadata the local normalizer exists to strip.
        if matches!(
            tag,
            b"eXIf" | b"tEXt" | b"zTXt" | b"iTXt" | b"iCCP" | b"tIME"
        ) {
            return Err(AppError::InvalidArg(
                "PNG metadata chunks are not allowed".into(),
            ));
        }
        let is_iend = tag == b"IEND";
        // Advance past length(4) + type(4) + data(len) + crc(4).
        let advance = len
            .checked_add(12)
            .ok_or_else(|| AppError::InvalidArg("malformed PNG chunks".into()))?;
        offset = offset
            .checked_add(advance)
            .ok_or_else(|| AppError::InvalidArg("malformed PNG chunks".into()))?;
        if offset > data.len() {
            return Err(AppError::InvalidArg("malformed PNG chunks".into()));
        }
        if is_iend {
            saw_iend = true;
            break;
        }
    }
    if !saw_iend {
        return Err(AppError::InvalidArg("PNG is missing its IEND chunk".into()));
    }
    if offset != data.len() {
        return Err(AppError::InvalidArg("trailing bytes after PNG IEND".into()));
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

    fn png_crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        crc ^ 0xFFFF_FFFF
    }

    fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + data.len());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc_input = Vec::with_capacity(4 + data.len());
        crc_input.extend_from_slice(kind);
        crc_input.extend_from_slice(data);
        out.extend_from_slice(&png_crc32(&crc_input).to_be_bytes());
        out
    }

    /// A canvas-style PNG (`IHDR sRGB IDAT IEND`) with an optional extra ancillary chunk inserted
    /// after `sRGB`, mirroring what WebKit's own encoder emits.
    fn png_document(extra: Option<(&[u8; 4], &[u8])>) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&8u32.to_be_bytes());
        ihdr.extend_from_slice(&8u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        out.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
        out.extend_from_slice(&png_chunk(b"sRGB", &[0]));
        if let Some((kind, data)) = extra {
            out.extend_from_slice(&png_chunk(kind, data));
        }
        out.extend_from_slice(&png_chunk(b"IDAT", &[0x78, 0x9c, 0x63, 0x00, 0x01]));
        out.extend_from_slice(&png_chunk(b"IEND", &[]));
        out
    }

    #[test]
    fn reject_png_metadata_allows_clean_srgb_png() {
        assert!(reject_png_metadata(&png_document(None)).is_ok());
        // A bare `IHDR IDAT IEND` PNG (no rendering-intent chunk) is equally clean.
        let mut bare = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        bare.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
        bare.extend_from_slice(&png_chunk(b"IDAT", &[0x78, 0x9c, 0x63, 0x00, 0x01]));
        bare.extend_from_slice(&png_chunk(b"IEND", &[]));
        assert!(reject_png_metadata(&bare).is_ok());
    }

    #[test]
    fn reject_png_metadata_rejects_privacy_bearing_chunks() {
        for kind in [b"eXIf", b"tEXt", b"zTXt", b"iTXt", b"iCCP", b"tIME"] {
            let bytes = png_document(Some((kind, b"payload")));
            assert!(
                matches!(reject_png_metadata(&bytes), Err(AppError::InvalidArg(_))),
                "chunk {:?} must be rejected",
                std::str::from_utf8(kind).unwrap()
            );
        }
    }

    #[test]
    fn reject_png_metadata_rejects_bad_framing_and_trailing_bytes() {
        assert!(reject_png_metadata(b"not a png at all").is_err());
        // Trailing bytes after IEND are refused.
        let mut trailing = png_document(None);
        trailing.extend_from_slice(b"junk");
        assert!(reject_png_metadata(&trailing).is_err());
        // A declared length that overruns the buffer is refused via checked arithmetic.
        let mut overrun = b"\x89PNG\r\n\x1a\n".to_vec();
        overrun.extend_from_slice(&0xffff_ffffu32.to_be_bytes());
        overrun.extend_from_slice(b"IHDR");
        assert!(reject_png_metadata(&overrun).is_err());
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
