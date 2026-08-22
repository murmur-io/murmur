use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use super::*;
use crate::share::org_dto::OrgItemAccess;
use crate::share::task_envelope::{
    TaskEnvelope, TaskImageRef, TaskOrgRef, TaskStatus, TaskSubtask, TASK_ENVELOPE_VERSION,
};
use crate::storage::tasks_store::{OrgTaskRow, TaskLocalRefRow};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskDraftDto {
    pub org_id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub status: TaskStatus,
    pub due_at: Option<String>,
    pub assignee_user_id: Option<String>,
    #[serde(default)]
    pub subtasks: Vec<TaskSubtask>,
    #[serde(default)]
    pub org_refs: Vec<TaskOrgRef>,
    #[serde(default)]
    pub images: Vec<TaskImageRef>,
    pub access: OrgItemAccess,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskLocalRefDto {
    pub kind: String,
    pub ref_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDto {
    pub id: String,
    pub doc_id: String,
    pub item_id: String,
    pub source_document_id: Option<String>,
    #[serde(flatten)]
    pub envelope: TaskEnvelope,
    pub access: String,
    pub can_edit: bool,
    pub can_manage: bool,
    pub local_refs: Vec<TaskLocalRefDto>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskAssigneeDto {
    pub user_id: String,
    pub label: String,
}

fn envelope_from_draft(draft: TaskDraftDto, created_at: String) -> TaskEnvelope {
    TaskEnvelope {
        version: TASK_ENVELOPE_VERSION,
        org_id: draft.org_id,
        title: draft.title.trim().to_string(),
        description: draft.description,
        status: draft.status,
        due_at: draft.due_at,
        assignee_user_id: draft.assignee_user_id,
        created_at,
        subtasks: draft.subtasks,
        org_refs: draft.org_refs,
        images: draft.images,
    }
}

pub(super) fn validate_task_org_refs(
    state: &AppState,
    envelope: &TaskEnvelope,
) -> Result<(), AppError> {
    envelope.validate(&envelope.org_id)?;
    for reference in &envelope.org_refs {
        if !state
            .db
            .visible_org_task_ref(&envelope.org_id, &reference.doc_id)?
        {
            return Err(AppError::InvalidArg(
                "task references must point to live tasks in the same organization".into(),
            ));
        }
    }
    Ok(())
}

/// Task plaintext is org-disclosed content, but it must not survive a local account/membership
/// invalidation merely because SQLCipher still retains the offline projection. `org_state` is the
/// repository's durable local membership witness; the live account session binds that witness to
/// the currently authenticated person. Context disablement remains a reversible empty view.
pub(crate) fn require_task_read_context(state: &AppState, org_id: &str) -> Result<(), AppError> {
    session_server_user_id(state)?;
    let org = resolve_org(state, org_id)?;
    if !org.context_enabled {
        return Err(AppError::Auth(
            "organization context is disabled on this device".into(),
        ));
    }
    Ok(())
}

pub(crate) fn task_dto(state: &AppState, row: OrgTaskRow) -> Result<TaskDto, AppError> {
    require_task_read_context(state, &row.org_id)?;
    let mut envelope = TaskEnvelope::from_json(&row.envelope_json, &row.org_id)?;
    envelope.org_refs = state.db.visible_task_org_refs(&row.id)?;
    let (can_edit, can_manage) = org_item_permissions(state, &row.item_id)?;
    let local_refs = state
        .db
        .task_local_refs(&row.id)?
        .into_iter()
        .map(|row| TaskLocalRefDto {
            kind: row.kind,
            ref_id: row.ref_id,
        })
        .collect();
    Ok(TaskDto {
        id: row.id,
        doc_id: row.doc_id,
        item_id: row.item_id,
        source_document_id: row.source_document_id,
        envelope,
        access: row.access,
        can_edit,
        can_manage,
        local_refs,
        updated_at: row.updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_dto_serializes_org_id_once() {
        let org_id = "11111111-1111-4111-8111-111111111111";
        let dto = TaskDto {
            id: format!("{org_id}:22222222-2222-4222-8222-222222222222"),
            doc_id: "22222222-2222-4222-8222-222222222222".into(),
            item_id: "33333333-3333-4333-8333-333333333333".into(),
            source_document_id: None,
            envelope: TaskEnvelope {
                version: TASK_ENVELOPE_VERSION,
                org_id: org_id.into(),
                title: "Task".into(),
                description: String::new(),
                status: TaskStatus::Todo,
                due_at: None,
                assignee_user_id: None,
                created_at: "2026-08-22T09:00:00Z".into(),
                subtasks: Vec::new(),
                org_refs: Vec::new(),
                images: Vec::new(),
            },
            access: "edit".into(),
            can_edit: true,
            can_manage: true,
            local_refs: Vec::new(),
            updated_at: "2026-08-22T09:00:00Z".into(),
        };

        let json = serde_json::to_string(&dto).unwrap();
        assert_eq!(
            json.matches("\"orgId\":").count(),
            1,
            "TaskDto must expose the envelope organization without duplicate JSON keys"
        );
    }
}

fn load_task_row_for_read(state: &AppState, id: &str) -> Result<Option<OrgTaskRow>, AppError> {
    let Some(org_id) = state.db.org_task_org_for_id(id)? else {
        return Ok(None);
    };
    require_task_read_context(state, &org_id)?;
    state.db.get_org_task(id)
}

pub(crate) fn require_task_edit_permission(
    state: &AppState,
    row: &OrgTaskRow,
) -> Result<(), AppError> {
    require_task_read_context(state, &row.org_id)?;
    let (can_edit, _) = org_item_permissions(state, &row.item_id)?;
    if can_edit {
        Ok(())
    } else {
        Err(AppError::Auth("this shared task is view only".into()))
    }
}

pub(crate) fn require_task_manage_permission(
    state: &AppState,
    row: &OrgTaskRow,
) -> Result<(), AppError> {
    require_task_read_context(state, &row.org_id)?;
    let (_, can_manage) = org_item_permissions(state, &row.item_id)?;
    if can_manage {
        Ok(())
    } else {
        Err(AppError::Auth(
            "only the task owner or organization owner can delete it".into(),
        ))
    }
}

#[tauri::command]
pub fn list_tasks(
    state: State<'_, AppState>,
    org_id: Option<String>,
) -> Result<Vec<TaskDto>, AppError> {
    session_server_user_id(state.inner())?;
    if let Some(org_id) = org_id.as_deref() {
        require_task_read_context(state.inner(), org_id)?;
    }
    let rows = state.db.list_org_tasks(org_id.as_deref())?;
    rows.into_iter()
        .map(|row| task_dto(state.inner(), row))
        .collect()
}

#[tauri::command]
pub fn get_task(state: State<'_, AppState>, id: String) -> Result<Option<TaskDto>, AppError> {
    load_task_row_for_read(state.inner(), &id)?
        .map(|row| task_dto(state.inner(), row))
        .transpose()
}

#[tauri::command]
pub async fn create_task(
    app: AppHandle,
    state: State<'_, AppState>,
    draft: TaskDraftDto,
) -> Result<TaskDto, AppError> {
    let org = resolve_org(state.inner(), &draft.org_id)?;
    require_task_read_context(state.inner(), &org.org_id)?;
    let source_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now();
    let access = draft.access;
    let envelope = envelope_from_draft(draft, created_at.to_rfc3339());
    validate_task_org_refs(state.inner(), &envelope)?;
    validate_task_assignee(
        state.inner(),
        &org.org_id,
        envelope.assignee_user_id.as_deref(),
    )
    .await?;
    let payload = envelope.to_canonical_json(&org.org_id)?;
    state.db.create_task_source(
        &source_id,
        &envelope.title,
        &payload,
        created_at.timestamp_millis(),
    )?;
    let (task_id, _item_id) = match share_task_source_to_org_notifying(
        state.inner(),
        &org.org_id,
        &source_id,
        access,
        &app,
    )
    .await
    {
        Ok(published) => published,
        Err(error) => {
            // Errors after the durable org-share journal exists are crash-recoverable and must keep
            // their source payload. A pre-journal refusal (for example consent/session failure) has
            // no recovery owner, so remove the otherwise-invisible task source before returning.
            if state
                .db
                .org_shares_for_source_revoke(None, Some(&source_id))?
                .is_empty()
            {
                state.db.delete_task_source(&source_id)?;
            }
            return Err(error);
        }
    };
    let row = state
        .db
        .get_org_task(&task_id)?
        .ok_or_else(|| AppError::Storage("published task projection is unavailable".into()))?;
    task_dto(state.inner(), row)
}

#[tauri::command]
pub async fn update_task(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    draft: TaskDraftDto,
) -> Result<TaskDto, AppError> {
    let row = load_task_row_for_read(state.inner(), &id)?
        .ok_or_else(|| AppError::InvalidArg("no such task".into()))?;
    if draft.org_id != row.org_id {
        return Err(AppError::InvalidArg(
            "a task cannot move between organizations".into(),
        ));
    }
    require_task_edit_permission(state.inner(), &row)?;
    let old = TaskEnvelope::from_json(&row.envelope_json, &row.org_id)?;
    let access = draft.access;
    if access.as_str() != row.access {
        return Err(AppError::InvalidArg(
            "change task access through the manage control".into(),
        ));
    }
    let envelope = envelope_from_draft(draft, old.created_at);
    validate_task_org_refs(state.inner(), &envelope)?;
    validate_task_assignee(
        state.inner(),
        &row.org_id,
        envelope.assignee_user_id.as_deref(),
    )
    .await?;
    let payload = envelope.to_canonical_json(&row.org_id)?;
    if let Some(source_id) = row.source_document_id.as_deref() {
        if !state.db.update_task_source(
            source_id,
            &envelope.title,
            &payload,
            chrono::Utc::now().timestamp_millis(),
        )? {
            return Err(AppError::Unavailable("task source changed — retry".into()));
        }
        republish_org_shares_for_source_notifying(state.inner(), None, Some(source_id), &app)
            .await?;
    } else {
        org_update_own_item_notifying(
            state.inner(),
            &row.item_id,
            &envelope.title,
            &payload,
            Some(&app),
        )
        .await?;
    }
    crate::events::emit_org_feed_updated(&app, 1);
    let row = state
        .db
        .get_org_task(&id)?
        .ok_or_else(|| AppError::Storage("updated task projection is unavailable".into()))?;
    task_dto(state.inner(), row)
}

#[tauri::command]
pub async fn delete_task(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError> {
    let row = load_task_row_for_read(state.inner(), &id)?
        .ok_or_else(|| AppError::InvalidArg("no such task".into()))?;
    require_task_manage_permission(state.inner(), &row)?;
    delete_org_item_as_author_notifying(state.inner(), &row.item_id, Some(&app)).await?;
    if let Some(source_id) = row.source_document_id.as_deref() {
        state.db.delete_task_source(source_id)?;
    }
    crate::events::emit_org_feed_updated(&app, 1);
    Ok(())
}

#[tauri::command]
pub fn set_task_local_refs(
    state: State<'_, AppState>,
    id: String,
    refs: Vec<TaskLocalRefDto>,
) -> Result<Vec<TaskLocalRefDto>, AppError> {
    set_task_local_refs_inner(state.inner(), &id, &refs)
}

pub(crate) fn set_task_local_refs_inner(
    state: &AppState,
    id: &str,
    refs: &[TaskLocalRefDto],
) -> Result<Vec<TaskLocalRefDto>, AppError> {
    let _lifecycle = lifecycle_guard(state);
    let row = load_task_row_for_read(state, id)?
        .ok_or_else(|| AppError::InvalidArg("no such task".into()))?;
    require_task_read_context(state, &row.org_id)?;
    for reference in refs {
        match reference.kind.as_str() {
            "note" => {
                let (folder_id, _, _) = state
                    .db
                    .note_gate_anchor(&reference.ref_id)?
                    .ok_or_else(|| AppError::InvalidArg("no such local note reference".into()))?;
                if !folder_is_unlocked(state, &folder_id)? {
                    return Err(AppError::Locked(
                        "unlock the note folder before linking it to a task".into(),
                    ));
                }
            }
            "meeting" => {
                state
                    .db
                    .get_meeting_gate_anchor(&reference.ref_id)?
                    .ok_or_else(|| {
                        AppError::InvalidArg("no such local meeting reference".into())
                    })?;
                if !meeting_is_unlocked(state, &reference.ref_id)? {
                    return Err(AppError::Locked(
                        "unlock the meeting folder before linking it to a task".into(),
                    ));
                }
            }
            "dashboard" => {
                if state.db.get_dashboard(&reference.ref_id)?.is_none() {
                    return Err(AppError::InvalidArg(
                        "no such local dashboard reference".into(),
                    ));
                }
            }
            _ => return Err(AppError::InvalidArg("invalid local task reference".into())),
        }
    }
    let rows: Vec<TaskLocalRefRow> = refs
        .iter()
        .enumerate()
        .map(|(position, row)| TaskLocalRefRow {
            kind: row.kind.clone(),
            ref_id: row.ref_id.clone(),
            position: position as u32,
        })
        .collect();
    state.db.replace_task_local_refs(id, &rows)?;
    Ok(refs.to_vec())
}

#[tauri::command]
pub async fn task_list_assignees(
    state: State<'_, AppState>,
    org_id: String,
) -> Result<Vec<TaskAssigneeDto>, AppError> {
    require_task_read_context(state.inner(), &org_id)?;
    let mut rows: Vec<TaskAssigneeDto> = org_task_list_members_inner(state.inner(), &org_id)
        .await?
        .into_iter()
        .map(|member| TaskAssigneeDto {
            user_id: member.user_id.clone(),
            label: member
                .email
                .as_deref()
                .and_then(|email| email.split('@').next())
                .filter(|label| !label.trim().is_empty())
                .unwrap_or("Member")
                .to_string(),
        })
        .collect();
    rows.sort_by(|a, b| {
        a.label
            .cmp(&b.label)
            .then_with(|| a.user_id.cmp(&b.user_id))
    });
    Ok(rows)
}

pub(crate) async fn validate_task_assignee(
    state: &AppState,
    org_id: &str,
    assignee_user_id: Option<&str>,
) -> Result<(), AppError> {
    let Some(assignee_user_id) = assignee_user_id else {
        return Ok(());
    };
    let members = org_task_list_members_inner(state, org_id).await?;
    if members
        .iter()
        .any(|member| member.user_id == assignee_user_id && !member.removed)
    {
        Ok(())
    } else {
        Err(AppError::InvalidArg(
            "task assignee is not an active member of this organization".into(),
        ))
    }
}
