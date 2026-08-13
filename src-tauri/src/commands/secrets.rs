//! Keychain secret (BYO API token/key) setters + presence probes — extracted verbatim from
//! `commands` (God-file split, a PURE MOVE — no behavior change). These commands store/replace/clear
//! BYO credentials in the macOS Keychain (Jira token, Slack token, Notion token, ClickUp token,
//! Anthropic key, AI-Gateway key,
//! web-search key) and report ONLY presence (`has_*`). NON-content-gated: they read/write NO meeting
//! content and touch no seal/unlock surface — the keys are never logged, never returned to the FE.
//! Every symbol keeps its EXACT prior body/signature and is re-exported at `crate::commands` via
//! `pub use secrets_commands::*;` in `commands/mod.rs`, so `generate_handler![commands::set_anthropic_key]`
//! in `lib.rs` and every `crate::commands::…` caller resolve UNCHANGED. The file is bound under the
//! name `secrets_commands` (not `secrets`) via `#[path]` to avoid colliding with the crate-level
//! `secrets` module (`use crate::{pipeline, secrets};` in `commands/mod.rs`).
//!
//! `ANTHROPIC_KEY_ACCOUNT` moves here (all its users are the commands below). `GATEWAY_KEY_ACCOUNT`
//! stays defined in `commands/mod.rs` (the gateway model-listing helpers there still reference it)
//! and is reached from here via `super::GATEWAY_KEY_ACCOUNT`.

use crate::error::AppError;
use crate::secrets;
use crate::state::AppState;
use tauri::State;

// The AI-Gateway keychain account const stays in `commands/mod.rs` (shared with the gateway
// model-listing helpers that were NOT moved); reach it via `super`.
use super::GATEWAY_KEY_ACCOUNT;

/// Keychain account for the Anthropic API key (matches `summarize::ANTHROPIC_KEY_ACCOUNT`).
const ANTHROPIC_KEY_ACCOUNT: &str = "anthropic_api_key";

fn set_ask_secret(
    state: &AppState,
    account: &str,
    value: Option<&str>,
) -> Result<(), AppError> {
    let _lifecycle = super::lifecycle_guard(state);
    let current = secrets::get_secret(account)?;
    let next = value.map(str::trim).filter(|value| !value.is_empty());
    if current.as_deref().map(str::trim) == next {
        return Ok(());
    }
    // Rotate before the fallible Keychain mutation: failure may over-invalidate, never authorize a
    // provider/connector holding credentials that no longer match the captured dispatch.
    state.db.advance_ask_dispatch_generation()?;
    match next {
        Some(value) => secrets::set_secret(account, value),
        None => secrets::delete_secret(account),
    }
}

/// Store/replace the BYO Jira API token in the Keychain (account "jira_api_token"). An empty input
/// clears it. NEVER logged, NEVER returned to the FE — only `has_*` reports presence.
#[tauri::command]
pub fn set_jira_token(state: State<'_, AppState>, key: String) -> Result<(), AppError> {
    set_ask_secret(&state, crate::connectors::jira::JIRA_TOKEN_ACCOUNT, Some(&key))
}

/// Whether a Jira token is currently stored (UI shows "set"/"not set"; never the value).
#[tauri::command]
pub fn has_jira_token() -> Result<bool, AppError> {
    Ok(
        secrets::get_secret(crate::connectors::jira::JIRA_TOKEN_ACCOUNT)?
            .filter(|k| !k.trim().is_empty())
            .is_some(),
    )
}

/// Store/replace the BYO Slack user token in the Keychain (account "slack_user_token"). An empty
/// input clears it. NEVER logged, NEVER returned to the FE — only `has_*` reports presence.
#[tauri::command]
pub fn set_slack_token(state: State<'_, AppState>, key: String) -> Result<(), AppError> {
    set_ask_secret(&state, crate::connectors::slack::SLACK_TOKEN_ACCOUNT, Some(&key))
}

/// Whether a Slack token is currently stored (UI shows "set"/"not set"; never the value).
#[tauri::command]
pub fn has_slack_token() -> Result<bool, AppError> {
    Ok(
        secrets::get_secret(crate::connectors::slack::SLACK_TOKEN_ACCOUNT)?
            .filter(|k| !k.trim().is_empty())
            .is_some(),
    )
}

/// Store/replace the BYO Notion integration token in the Keychain (account "notion_api_token"). An
/// empty input clears it. NEVER logged, NEVER returned to the FE — only `has_*` reports presence.
#[tauri::command]
pub fn set_notion_token(state: State<'_, AppState>, key: String) -> Result<(), AppError> {
    set_ask_secret(&state, crate::connectors::notion::NOTION_TOKEN_ACCOUNT, Some(&key))
}

/// Whether a Notion token is currently stored (UI shows "set"/"not set"; never the value).
#[tauri::command]
pub fn has_notion_token() -> Result<bool, AppError> {
    Ok(
        secrets::get_secret(crate::connectors::notion::NOTION_TOKEN_ACCOUNT)?
            .filter(|k| !k.trim().is_empty())
            .is_some(),
    )
}

/// Store/replace the BYO ClickUp personal API token in the Keychain (account "clickup_api_token").
/// An empty input clears it. NEVER logged, NEVER returned to the FE — only `has_*` reports presence.
#[tauri::command]
pub fn set_clickup_token(state: State<'_, AppState>, key: String) -> Result<(), AppError> {
    set_ask_secret(
        &state,
        crate::connectors::clickup::CLICKUP_TOKEN_ACCOUNT,
        Some(&key),
    )
}

/// Whether a ClickUp token is currently stored (UI shows "set"/"not set"; never the value).
#[tauri::command]
pub fn has_clickup_token() -> Result<bool, AppError> {
    Ok(
        secrets::get_secret(crate::connectors::clickup::CLICKUP_TOKEN_ACCOUNT)?
            .filter(|k| !k.trim().is_empty())
            .is_some(),
    )
}

/// Store/replace the Anthropic API key in Keychain (account "anthropic_api_key").
#[tauri::command]
pub fn set_anthropic_key(state: State<'_, AppState>, key: String) -> Result<(), AppError> {
    set_ask_secret(&state, ANTHROPIC_KEY_ACCOUNT, Some(&key))
}

/// Whether an Anthropic key is currently stored (UI shows "set"/"not set"; never the value).
#[tauri::command]
pub fn has_anthropic_key() -> Result<bool, AppError> {
    Ok(secrets::get_secret(ANTHROPIC_KEY_ACCOUNT)?.is_some())
}

/// Store/replace the AI Gateway API key in Keychain (account "gateway_api_key").
/// An empty/blank key is rejected — call `clear_gateway_key` to remove an existing key.
/// The key is NEVER logged and NEVER returned to the FE — only `has_gateway_key` reports presence.
/// Uses a SEPARATE keychain account from the Anthropic key (R3 — no cross-provider fallback).
#[tauri::command]
pub fn set_gateway_key(state: State<'_, AppState>, key: String) -> Result<(), AppError> {
    validate_gateway_key(&key)?;
    set_ask_secret(&state, GATEWAY_KEY_ACCOUNT, Some(&key))
}

pub(crate) fn validate_gateway_key(key: &str) -> Result<(), AppError> {
    if key.trim().is_empty() {
        return Err(AppError::InvalidArg(
            "gateway API key must not be empty; use clear_gateway_key to remove an existing key"
                .into(),
        ));
    }
    Ok(())
}

/// Whether an AI Gateway key is currently stored (UI shows "set"/"not set"; never the value).
#[tauri::command]
pub fn has_gateway_key() -> Result<bool, AppError> {
    Ok(secrets::get_secret(GATEWAY_KEY_ACCOUNT)?
        .filter(|k| !k.trim().is_empty())
        .is_some())
}

/// Remove the stored AI Gateway API key from the Keychain.
/// Idempotent — no error if no key is stored. Mirrors `set_anthropic_key("")` semantics.
#[tauri::command]
pub fn clear_gateway_key(state: State<'_, AppState>) -> Result<(), AppError> {
    set_ask_secret(&state, GATEWAY_KEY_ACCOUNT, None)
}

/// Store/replace the BYO web-search (Brave) API key in the Keychain (account "web_search_api_key").
/// An empty input clears it. The key is NEVER logged and NEVER returned to the FE — only `has_*`
/// reports presence. Mirrors `set_anthropic_key`.
#[tauri::command]
pub fn set_web_search_api_key(state: State<'_, AppState>, key: String) -> Result<(), AppError> {
    set_ask_secret(&state, crate::connectors::web::WEB_SEARCH_KEY_ACCOUNT, Some(&key))
}

/// Whether a web-search API key is currently stored (UI shows "set"/"not set"; never the value).
#[tauri::command]
pub fn has_web_search_key() -> Result<bool, AppError> {
    Ok(
        secrets::get_secret(crate::connectors::web::WEB_SEARCH_KEY_ACCOUNT)?
            .filter(|k| !k.trim().is_empty())
            .is_some(),
    )
}
