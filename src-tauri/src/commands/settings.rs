//! Egress-consent commands — extracted verbatim from `commands` (God-file split, a PURE MOVE — no
//! behavior change). These commands are each the SINGLE, dedicated writer that flips one
//! one-time egress-consent flag (cloud egress, web search, Jira, Slack, Notion, ClickUp) ON or OFF — persisting the
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
use crate::storage::models::{NoteTemplate, NoteTemplateSection};

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

/// One-time Notion egress consent — the ONLY way `notion_consented` flips true. Persists the flag
/// AND updates the in-memory config cache, so the next `ConnectorRegistry::build` exposes the notion
/// tool (provided Notion is also enabled + a token is stored). Idempotent.
#[tauri::command]
pub fn consent_to_notion(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cache.grant_notion_consent(&state.db)?;
    Ok(())
}

/// One-time ClickUp egress consent — the ONLY way `clickup_consented` flips true. Persists the flag
/// AND updates the in-memory config cache, so the next `ConnectorRegistry::build` exposes the clickup
/// tool (provided ClickUp is also enabled + a workspace id and token are configured). Idempotent.
#[tauri::command]
pub fn consent_to_clickup(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cache.grant_clickup_consent(&state.db)?;
    Ok(())
}

// ── Note templates (user-authored named sections) ────────────────────────────────────────────────
//
// CONTENT-FREE, single-user metadata — exactly like `save_recipe` / `upsert_saved_view`: these
// persist only a note SHAPE (a name, a tone line, ordered `{heading, instruction}` sections, and
// extra front-matter keys), NEVER meeting content, so they are NOT visibility-gated (there is
// nothing sealed to leak). A saved template is rendered into the summarizer SYSTEM PROMPT by
// `summarize::template::build_template` (the same `SummarizeRequest.template` seam the built-in
// styles use) and still passes the `RedactingProvider` firewall on egress, unchanged.
//
// SECURITY: `save_note_template` REJECTS any template whose text carries a scripting token
// (`<%` / `tp.` / `require(` / `process.`) with `AppError::InvalidArg` — a template is DECLARATIVE
// data, never code. After every mutation we refresh the process-global saved-template registry
// (`template::set_saved_templates`) so the next note generation resolves the live data (the pipeline
// renders through `build_template` with only the style string, so the registry — not `AppState` —
// is how a saved-template id reaches the renderer).

/// List the user's saved note templates (newest first). CONTENT-FREE; not gated.
#[tauri::command]
pub fn list_note_templates(state: State<'_, AppState>) -> Result<Vec<NoteTemplate>, AppError> {
    state.db.list_note_templates()
}

/// Save (create or replace) a note template. Validates the name + at least one section, REJECTS any
/// scripting token, persists, refreshes the registry, and returns the stored row. An empty `id`
/// mints a new uuid; a non-empty existing id replaces in place.
#[tauri::command]
pub fn save_note_template(
    state: State<'_, AppState>,
    id: Option<String>,
    name: String,
    tone: String,
    sections: Vec<NoteTemplateSection>,
    extra_frontmatter_keys: Vec<String>,
) -> Result<NoteTemplate, AppError> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::InvalidArg("template name is required".into()));
    }
    // SECURITY GATE — no scripting, ever. Validate the RAW inputs (before any normalization drops
    // blank-heading rows) so a forbidden token can never slip through in a discarded field. A note
    // template is DECLARATIVE data rendered into the system prompt, never code.
    {
        let raw = NoteTemplate {
            id: String::new(),
            name: name.clone(),
            tone: tone.clone(),
            sections: sections.clone(),
            extra_frontmatter_keys: extra_frontmatter_keys.clone(),
            created_at: String::new(),
        };
        crate::summarize::template::validate_note_template(&raw.name, &raw.tone, &raw)?;
    }
    // Normalize: drop wholly-blank sections/keys so a stray empty row can't produce a bare `## `.
    let sections: Vec<NoteTemplateSection> = sections
        .into_iter()
        .filter(|s| !s.heading.trim().is_empty())
        .map(|s| NoteTemplateSection {
            heading: s.heading.trim().to_string(),
            instruction: s.instruction.trim().to_string(),
        })
        .collect();
    if sections.is_empty() {
        return Err(AppError::InvalidArg(
            "a note template needs at least one section".into(),
        ));
    }
    let extra_frontmatter_keys: Vec<String> = extra_frontmatter_keys
        .into_iter()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect();

    let id = id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    // Preserve the original created_at when replacing an existing template.
    let created_at = state
        .db
        .list_note_templates()?
        .into_iter()
        .find(|t| t.id == id)
        .map(|t| t.created_at)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    let rec = NoteTemplate {
        id,
        name: name.clone(),
        tone: tone.trim().to_string(),
        sections,
        extra_frontmatter_keys,
        created_at,
    };
    // SECURITY GATE (belt-and-suspenders) — re-validate the exact normalized row that will be
    // persisted + egressed. Normalization only trims, so this can't newly pass what the raw check
    // above rejected; it guarantees the STORED bytes carry no scripting token.
    crate::summarize::template::validate_note_template(&rec.name, &rec.tone, &rec)?;

    state.db.insert_note_template(&rec)?;
    // Refresh the renderer's registry so the next note generation sees the live template.
    crate::summarize::template::set_saved_templates(state.db.list_note_templates()?);
    Ok(rec)
}

/// Delete a saved note template by id, then refresh the renderer's registry.
#[tauri::command]
pub fn delete_note_template(state: State<'_, AppState>, id: String) -> Result<(), AppError> {
    state.db.delete_note_template(&id)?;
    crate::summarize::template::set_saved_templates(state.db.list_note_templates()?);
    Ok(())
}
