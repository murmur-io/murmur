//! Pipeline / recipes / saved-views commands — extracted verbatim from `commands` (God-file split
//! PR-1, a PURE MOVE — no behavior change). These are the recipe-template and saved-view surfaces:
//! a small, self-contained, NON-content-gated domain (they persist recipe prompts + FE-owned view
//! definitions, never meeting content — see the LOCK-SECURITY NOTE below). Every `#[tauri::command]`
//! keeps its EXACT prior body/signature and is re-exported at `crate::commands` via the
//! `pub use pipeline::*;` glob in `commands/mod.rs`, so `generate_handler![commands::save_recipe]`
//! in `lib.rs` and every `crate::commands::…` caller resolve UNCHANGED. Tests stay in
//! `commands/mod.rs` (the shared `lifecycle_tests` harness); `is_valid_saved_view_scope` is promoted
//! to `pub(crate)` so that harness's `use super::*` still reaches it through the re-export.

use tauri::State;

use crate::error::AppError;
use crate::state::AppState;
use crate::storage::models::{BuiltinRecipe, RecipeRecord, SavedView};

/// Built-in recipe templates (quick chips).
#[tauri::command]
pub fn list_builtin_recipes() -> Result<Vec<BuiltinRecipe>, AppError> {
    Ok(crate::summarize::recipes::BUILTIN_RECIPES
        .iter()
        .map(|(id, label, prompt)| BuiltinRecipe {
            id: id.to_string(),
            label: label.to_string(),
            prompt: prompt.to_string(),
        })
        .collect())
}

/// User-saved recipe templates.
#[tauri::command]
pub fn list_saved_recipes(state: State<'_, AppState>) -> Result<Vec<RecipeRecord>, AppError> {
    state.db.list_saved_recipes()
}

/// Save a recipe template (prompt + title).
#[tauri::command]
pub fn save_recipe(
    state: State<'_, AppState>,
    title: String,
    prompt: String,
) -> Result<RecipeRecord, AppError> {
    let title = title.trim();
    let prompt = prompt.trim();
    if title.is_empty() || prompt.is_empty() {
        return Err(AppError::InvalidArg(
            "recipe title and prompt are required".into(),
        ));
    }
    let rec = RecipeRecord {
        id: uuid::Uuid::new_v4().to_string(),
        title: title.to_string(),
        prompt: prompt.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    state.db.insert_recipe(&rec)?;
    Ok(rec)
}

/// Delete a saved recipe.
#[tauri::command]
pub fn delete_recipe(state: State<'_, AppState>, id: String) -> Result<(), AppError> {
    state.db.delete_recipe(&id)
}

// ── Saved views (Feature B — "Saved views over the meetings list") ─────────────────────────────
//
// LOCK-SECURITY NOTE: the four saved-view commands below (`list_saved_views` / `upsert_saved_view`
// / `delete_saved_view` / `reorder_saved_views`) are the ONE legitimate NEW-command case that is
// NOT content-gated. They persist only a user's VIEW DEFINITION (an opaque FE-owned `config` JSON
// blob + presentation fields) — single-user, non-shared metadata, exactly like the `saved_recipes`
// commands. They store/return NO meeting content, so there is nothing sealed to leak. The ACTUAL
// content aggregation the meetings surface consumes is `list_meeting_action_summaries`, which IS
// gated (routes through `unlocked_snapshot` → the gated `Db::list_meeting_action_summaries`).

/// The valid saved-view scopes — one per list surface. A scope not in this set is rejected as an
/// argument error so a typo can't quietly create an un-listable orphan row. `"notes"` was added
/// 2026-07-14 (Saved Views ported to the Notes surface, mirroring Meetings).
const SAVED_VIEW_SCOPE_MEETINGS: &str = "meetings";
const SAVED_VIEW_SCOPE_NOTES: &str = "notes";

/// Whether `scope` names a list surface that supports saved views.
pub(crate) fn is_valid_saved_view_scope(scope: &str) -> bool {
    scope == SAVED_VIEW_SCOPE_MEETINGS || scope == SAVED_VIEW_SCOPE_NOTES
}

/// List the user's saved views for a list surface (`scope` = "meetings"). NOT content-gated: view
/// metadata only (see the LOCK-SECURITY NOTE above).
#[tauri::command]
pub fn list_saved_views(
    state: State<'_, AppState>,
    scope: String,
) -> Result<Vec<SavedView>, AppError> {
    if !is_valid_saved_view_scope(&scope) {
        return Err(AppError::InvalidArg(format!(
            "unsupported saved-view scope: {scope}"
        )));
    }
    state.db.list_saved_views(&scope)
}

/// Create or update a saved view. On FIRST save (empty `id`) the server generates the id +
/// `created_at`; `updated_at` is always stamped server-side. Trims + rejects empty name/scope/layout
/// as an argument error. NOT content-gated: view metadata only (see the LOCK-SECURITY NOTE above).
#[tauri::command]
pub fn upsert_saved_view(
    state: State<'_, AppState>,
    view: SavedView,
) -> Result<SavedView, AppError> {
    let scope = view.scope.trim();
    let name = view.name.trim();
    let layout = view.layout.trim();
    if !is_valid_saved_view_scope(scope) {
        return Err(AppError::InvalidArg(format!(
            "unsupported saved-view scope: {scope}"
        )));
    }
    if name.is_empty() || layout.is_empty() {
        return Err(AppError::InvalidArg(
            "saved-view name and layout are required".into(),
        ));
    }
    let now = chrono::Utc::now().to_rfc3339();
    let id = view.id.trim();
    let (id, created_at) = if id.is_empty() {
        // First save: server-generate the id + creation timestamp.
        (uuid::Uuid::new_v4().to_string(), now.clone())
    } else {
        // Edit: keep the caller's id; preserve the original created_at if we already have the row.
        let created_at = state
            .db
            .list_saved_views(scope)?
            .into_iter()
            .find(|v| v.id == id)
            .map(|v| v.created_at)
            .unwrap_or_else(|| now.clone());
        (id.to_string(), created_at)
    };
    let rec = SavedView {
        id,
        scope: scope.to_string(),
        name: name.to_string(),
        layout: layout.to_string(),
        config: view.config,
        sort_order: view.sort_order,
        created_at,
        updated_at: now,
    };
    state.db.upsert_saved_view(&rec)?;
    Ok(rec)
}

/// Delete a saved view by id (idempotent). NOT content-gated: view metadata only.
#[tauri::command]
pub fn delete_saved_view(state: State<'_, AppState>, id: String) -> Result<(), AppError> {
    state.db.delete_saved_view(&id)
}

/// Persist a user reordering of the saved views in a scope. NOT content-gated: view metadata only.
#[tauri::command]
pub fn reorder_saved_views(
    state: State<'_, AppState>,
    scope: String,
    ordered_ids: Vec<String>,
) -> Result<(), AppError> {
    if !is_valid_saved_view_scope(&scope) {
        return Err(AppError::InvalidArg(format!(
            "unsupported saved-view scope: {scope}"
        )));
    }
    state.db.reorder_saved_views(&scope, &ordered_ids)
}
