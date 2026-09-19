//! The one generic local-container mover. No content is returned across IPC.
use super::*;

#[tauri::command]
pub async fn move_container(
    state: State<'_, AppState>,
    id: String,
    parent_id: Option<String>,
) -> Result<(), AppError> {
    let _share_mutation = state.lock_org_mutation().await;
    move_container_inner(state.inner(), &id, parent_id.as_deref())
}

pub(crate) fn move_container_inner(
    state: &AppState,
    id: &str,
    parent_id: Option<&str>,
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    let vault = vault_path(state);
    state
        .db
        .move_container_local(id, parent_id, vault.as_deref().map(std::path::Path::new))
}
