//! Egress-consent commands — extracted verbatim from `commands` (God-file split, a PURE MOVE — no
//! behavior change). These five commands are each the SINGLE, dedicated writer that flips one
//! one-time egress-consent flag (cloud egress, web search, Jira, Slack) ON or OFF — persisting the
//! flag AND refreshing the in-memory config cache, so the next provider/connector build re-reads the
//! live value fail-closed. They are NON-content-gated: they read/write only a consent boolean via
//! `AppConfig`'s dedicated `grant_*`/`revoke_*` methods — NO meeting content, no seal/unlock surface,
//! no `AppConfigDto` round-trip. (The consent flag is preserve-only from the settings DTO — a raw
//! `save_config` can neither grant nor revoke it, which is exactly why these dedicated commands
//! exist; the ACTUAL egress gate that reads the flag lives in the provider/connector layer, not
//! here.) Every symbol keeps its EXACT prior body/signature and is re-exported at `crate::commands`
//! via `pub use settings_commands::*;` in `commands/mod.rs`, so
//! `generate_handler![commands::consent_to_cloud_egress]` in `lib.rs` and every `crate::commands::…`
//! caller resolve UNCHANGED. The file is bound under the name `settings_commands` (via `#[path]`) to
//! keep it clearly distinct from the crate-level `settings` module.
//!
//! NOTE: the tightly-coupled config-DTO core (`AppConfigDto` + its serde-default helpers,
//! `get_config` / `get_mcp_config` / `save_config` + the `*_inner` cores, `config_to_dto` /
//! `dto_to_config`, `static_connection_models`, `topic_threads`) deliberately STAYED in
//! `commands/mod.rs`: moving them would drag in non-listed helpers and create a circular re-export
//! of the shared `AppConfigDto` struct. Only the self-contained consent commands are extracted here.

use tauri::State;

use crate::error::AppError;
use crate::state::AppState;

/// E10 — grant the one-time cloud-egress consent. This is the ONLY supported way to flip
/// `cloud_egress_consented` true: it persists the flag AND updates the in-memory config cache, so
/// the next `make_provider(claude_code|anthropic)` is allowed to build. Idempotent.
///
/// The FE calls this from its first-cloud-send confirmation dialog. Until the user confirms, every
/// cloud summarize/chat returns `AppError::Unavailable("cloud egress not consented …")`, which the
/// FE detects and surfaces as the consent prompt.
#[tauri::command]
pub fn consent_to_cloud_egress(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cache.grant_cloud_egress_consent(&state.db)?;
    Ok(())
}

/// E10 — REVOKE the cloud-egress consent (the AI-settings privacy strip). Mirror of
/// [`consent_to_cloud_egress`] and the ONLY supported way to flip `cloud_egress_consented` false:
/// it persists the flag AND updates the in-memory config cache, so the NEXT
/// `make_provider(claude_code|anthropic|gateway)` / cloud-reasoner call is refused fail-closed
/// (`AppError::Unavailable`) — the gate re-reads the live config per call, no restart needed.
/// Idempotent; a settings save can still neither grant nor revoke (the DTO stays preserve-only).
#[tauri::command]
pub fn revoke_cloud_egress(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cache.revoke_cloud_egress(&state.db)?;
    Ok(())
}

/// brain2 connectors — grant the one-time WEB SEARCH egress consent. The web connector reaches an
/// EXTERNAL service (a NEW EGRESS CLASS): the redacted query leaves the device. This is the ONLY
/// supported way to flip `web_search_consented` true; it persists the flag AND updates the in-memory
/// config cache, so the next `ConnectorRegistry::build` exposes the web tool (provided web search is
/// also enabled and a key is stored). Idempotent. Until granted, the web connector is absent from the
/// brain's tool registry and the redacted query never leaves the device.
#[tauri::command]
pub fn consent_to_web_search(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cache.grant_web_search_consent(&state.db)?;
    Ok(())
}

/// One-time Jira egress consent — the ONLY way `jira_consented` flips true. Persists the flag AND
/// updates the in-memory config cache, so the next `ConnectorRegistry::build` exposes the jira tool
/// (provided Jira is also enabled + configured + a token is stored). Idempotent.
#[tauri::command]
pub fn consent_to_jira(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cache.grant_jira_consent(&state.db)?;
    Ok(())
}

/// One-time Slack egress consent — the ONLY way `slack_consented` flips true. Persists the flag AND
/// updates the in-memory config cache, so the next `ConnectorRegistry::build` exposes the slack tool
/// (provided Slack is also enabled + a token is stored). Idempotent.
#[tauri::command]
pub fn consent_to_slack(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cache.grant_slack_consent(&state.db)?;
    Ok(())
}
