//! MCP server-config commands — extracted verbatim from `commands` (God-file split, a PURE MOVE —
//! no behavior change). These five commands manage the user's MCP server ROSTER (list/add/remove) +
//! the per-server ONE-TIME egress consent flag (`consented`). NON-content-gated: they read/write
//! only MCP-server connection config rows + a consent bool — NO meeting content, no seal/unlock
//! surface, so no `meeting_is_unlocked` / `visibility_clause` gate applies. (The consent bool is
//! fail-closed: a server stays absent from the brain's tools until `consent_to_mcp_server` flips it;
//! the actual read-only MCP SERVER surface — the visibility-gated one — lives in `crate::mcp`, not
//! here.) `test_mcp_server` (the async connectivity probe) deliberately stayed in `commands/mod.rs`.
//! Every symbol keeps its EXACT prior body/signature and is re-exported at `crate::commands` via
//! `pub use mcp_commands::*;` in `commands/mod.rs`, so `generate_handler![commands::list_mcp_servers]`
//! in `lib.rs` and every `crate::commands::…` caller resolve UNCHANGED. The file is bound under the
//! name `mcp_commands` (not `mcp`) via `#[path]` to avoid any collision with the crate-level `mcp`
//! module.

use tauri::State;

use crate::error::AppError;
use crate::state::AppState;

/// All configured MCP servers (connection config only).
#[tauri::command]
pub fn list_mcp_servers(
    state: State<'_, AppState>,
) -> Result<Vec<crate::storage::models::McpServer>, AppError> {
    state.db.list_mcp_servers()
}

/// Add one MCP server. `transport` is `"http"` (a JSON-RPC endpoint URL) or `"stdio"` (a LOCAL
/// PROCESS — `endpoint` must be an ABSOLUTE path).
///
/// ⚠️ stdio WARNING (surface this in the FE add-server flow): a stdio MCP server is ARBITRARY
/// CODE EXECUTION — Murmur launches that binary with your user's permissions every time the brain
/// queries it. Only add binaries you trust and control; the absolute-path requirement prevents
/// $PATH hijacking but does NOT make an untrusted binary safe. The server stays fail-closed
/// (absent from the brain's tools) until `consent_to_mcp_server` is granted.
#[tauri::command]
pub fn add_mcp_server(
    state: State<'_, AppState>,
    label: String,
    transport: String,
    endpoint: String,
    args: Option<Vec<String>>,
) -> Result<crate::storage::models::McpServer, AppError> {
    let label = label.trim().to_string();
    if label.is_empty() {
        return Err(AppError::InvalidArg("server label is empty".into()));
    }
    let endpoint = endpoint.trim().to_string();
    match transport.as_str() {
        "http" => {
            if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
                return Err(AppError::InvalidArg(
                    "an http MCP server needs an http(s):// endpoint URL".into(),
                ));
            }
        }
        "stdio" => {
            // ABSOLUTE-PATH-ONLY (defense-in-depth against $PATH hijacking; re-checked at spawn).
            if !std::path::Path::new(&endpoint).is_absolute() {
                return Err(AppError::InvalidArg(
                    "a stdio MCP server command must be an ABSOLUTE path".into(),
                ));
            }
        }
        _ => {
            return Err(AppError::InvalidArg(
                "transport must be \"http\" or \"stdio\"".into(),
            ))
        }
    }
    let server = crate::storage::models::McpServer {
        // Hyphen-free id so it embeds cleanly in the `mcp_<id>_query` tool name.
        id: uuid::Uuid::new_v4().simple().to_string(),
        label,
        transport,
        endpoint,
        args: args.unwrap_or_default(),
        enabled: true,
        consented: false, // fail-closed until the dedicated consent command flips it.
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    state.db.insert_mcp_server(&server)?;
    Ok(server)
}

/// Remove one MCP server (its tool disappears from the brain on the next spec build).
#[tauri::command]
pub fn remove_mcp_server(state: State<'_, AppState>, server_id: String) -> Result<(), AppError> {
    state.db.delete_mcp_server(&server_id)
}

/// Grant the ONE-TIME per-server egress consent for an MCP server. Mirrors
/// `consent_to_web_search`: the dedicated command is the ONLY writer that can flip consent ON.
#[tauri::command]
pub fn consent_to_mcp_server(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<(), AppError> {
    if state.db.get_mcp_server(&server_id)?.is_none() {
        return Err(AppError::InvalidArg(format!("no MCP server {server_id}")));
    }
    state.db.set_mcp_server_consented(&server_id, true)
}

/// Revoke an MCP server's egress consent — it drops out of the connector registry and the brain's
/// tool list on the next build (fail-closed).
#[tauri::command]
pub fn revoke_mcp_consent(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<(), AppError> {
    state.db.set_mcp_server_consented(&server_id, false)
}
