//! Localhost MCP server (HTTP, 127.0.0.1 only) exposing the user's meetings to MCP clients
//! (Claude Desktop / Code) with NO egress. Read-only tools over the SQLite DB. Implements the
//! MCP JSON-RPC essentials (initialize / tools/list / tools/call) over HTTP POST.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tiny_http::{Header, Method, Response, Server};

use crate::storage::Db;

/// Fixed localhost port for the MCP server.
pub const MCP_PORT: u16 = 8765;

/// Shared session unlock set: folder ids whose sealed notes are decrypted into the markdown
/// column for this session (so MCP can read them as plaintext + the visibility filter lets them
/// through). Sealed-and-not-unlocked notes stay invisible.
pub type UnlockedSet = Arc<Mutex<HashSet<String>>>;

/// Spawn the MCP server on a background thread. Best-effort: a bind failure is logged; the app
/// continues normally. `unlocked` is shared with the command surface so visibility tracks the
/// live session state. `require_token` gates `tools/call` behind a bearer token (default off).
pub fn spawn(db_path: PathBuf, unlocked: UnlockedSet, require_token: bool) {
    let _ = std::thread::Builder::new()
        .name("murmur-mcp".into())
        .spawn(move || run(db_path, unlocked, require_token));
}

fn run(db_path: PathBuf, unlocked: UnlockedSet, require_token: bool) {
    let addr = format!("127.0.0.1:{MCP_PORT}");
    let server = match Server::http(&addr) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(target: "mcp", error = %e, "MCP server failed to bind {addr}");
            return;
        }
    };
    // The expected token, if enforcement is on. Minted/persisted in the Keychain on first use.
    let expected_token = if require_token {
        match crate::secrets::get_or_create_mcp_token() {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::warn!(target: "mcp", error = %e, "could not mint MCP token; disabling enforcement");
                None
            }
        }
    } else {
        None
    };
    tracing::info!(target: "mcp", "MCP server listening on http://{addr}");
    for mut req in server.incoming_requests() {
        if *req.method() != Method::Post {
            let _ = req.respond(
                Response::from_string("Murmur MCP server — POST JSON-RPC here.")
                    .with_status_code(200),
            );
            continue;
        }
        // Extract the bearer token (if any) from the Authorization header before consuming body.
        let auth = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("Authorization"))
            .map(|h| h.value.as_str().to_string());
        let mut body = String::new();
        let _ = std::io::Read::read_to_string(req.as_reader(), &mut body);
        match handle_rpc(&db_path, &body, &unlocked, expected_token.as_deref(), auth.as_deref()) {
            Some(resp) => {
                let h = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
                let _ = req.respond(
                    Response::from_string(resp.to_string())
                        .with_status_code(200)
                        .with_header(h),
                );
            }
            // Notification (no id) → 202, no body.
            None => {
                let _ = req.respond(Response::from_string("").with_status_code(202));
            }
        }
    }
}

/// Returns Some(response) for JSON-RPC requests, None for notifications.
/// `expected_token` is `Some` only when enforcement is on; `auth` is the raw Authorization header.
fn handle_rpc(
    db_path: &Path,
    body: &str,
    unlocked: &UnlockedSet,
    expected_token: Option<&str>,
    auth: Option<&str>,
) -> Option<Value> {
    let req: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Some(rpc_err(Value::Null, -32700, "parse error")),
    };
    // Notifications have no "id" → no response.
    let id = req.get("id")?.clone();
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "murmur", "version": env!("CARGO_PKG_VERSION") }
        }),
        "tools/list" => json!({ "tools": tools_spec() }),
        "ping" => json!({}),
        "tools/call" => {
            // Bearer enforcement applies ONLY to tools/call; discovery stays open.
            if let Some(expected) = expected_token {
                if !bearer_ok(auth, expected) {
                    return Some(rpc_err(id, -32001, "unauthorized: bearer token required"));
                }
            }
            return Some(handle_tool_call(db_path, id, req.get("params"), unlocked));
        }
        _ => return Some(rpc_err(id, -32601, "method not found")),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// Constant-ish bearer-token check: the Authorization header must be `Bearer <expected>`.
fn bearer_ok(auth: Option<&str>, expected: &str) -> bool {
    match auth {
        Some(h) => {
            let token = h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer "));
            token.map(|t| t.trim() == expected).unwrap_or(false)
        }
        None => false,
    }
}

fn rpc_err(id: Value, code: i64, msg: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": msg } })
}

fn text_result(id: Value, text: String) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": text }] } })
}

fn tools_spec() -> Value {
    json!([
        {
            "name": "search_meetings",
            "description": "Full-text search across your meeting titles, transcripts and notes. Returns matching meetings with snippets and ids.",
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] }
        },
        {
            "name": "get_meeting",
            "description": "Get a meeting's AI note (summary) and full transcript by id.",
            "inputSchema": { "type": "object", "properties": { "meetingId": { "type": "string" } }, "required": ["meetingId"] }
        },
        {
            "name": "list_recent_meetings",
            "description": "List the most recent meetings (title, date, status, id).",
            "inputSchema": { "type": "object", "properties": { "limit": { "type": "number" } } }
        }
    ])
}

fn handle_tool_call(
    db_path: &Path,
    id: Value,
    params: Option<&Value>,
    unlocked: &UnlockedSet,
) -> Value {
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let args = params
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let db = match Db::open(db_path) {
        Ok(d) => d,
        Err(e) => return rpc_err(id, -32000, &format!("db open failed: {e}")),
    };
    // Snapshot the session unlock set so every query in this call sees a consistent view.
    let unlocked_set = unlocked.lock().map(|g| g.clone()).unwrap_or_default();
    let text = match name {
        "search_meetings" => {
            let q = args.get("query").and_then(Value::as_str).unwrap_or("");
            match db.search_visible(q, 20, &unlocked_set) {
                Ok(hits) if hits.is_empty() => format!("No meetings match \"{q}\"."),
                Ok(hits) => hits
                    .iter()
                    .map(|h| {
                        format!(
                            "- {} ({}) [id:{}] — {}",
                            h.meeting.title.clone().unwrap_or_else(|| "(untitled)".into()),
                            h.meeting.started_at,
                            h.meeting.id,
                            h.snippet
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(e) => return rpc_err(id, -32000, &format!("search failed: {e}")),
            }
        }
        "get_meeting" => {
            let mid = args.get("meetingId").and_then(Value::as_str).unwrap_or("");
            // A sealed-and-not-unlocked meeting is invisible — including its transcript.
            match db.meeting_is_visible(mid, &unlocked_set) {
                Ok(false) => format!("No data for meeting {mid}."),
                Err(e) => return rpc_err(id, -32000, &format!("visibility check failed: {e}")),
                Ok(true) => {
                    let note = db.get_note_if_visible(mid, &unlocked_set).ok().flatten();
                    let segs = db.get_segments(mid).unwrap_or_default();
                    let transcript = segs
                        .iter()
                        .map(|s| s.text.trim())
                        .filter(|t| !t.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                    match note {
                        Some(n) => format!("NOTE:\n{}\n\nTRANSCRIPT:\n{transcript}", n.markdown),
                        None if !transcript.is_empty() => format!("TRANSCRIPT:\n{transcript}"),
                        None => format!("No data for meeting {mid}."),
                    }
                }
            }
        }
        "list_recent_meetings" => {
            let limit = args
                .get("limit")
                .and_then(Value::as_i64)
                .unwrap_or(20)
                .clamp(1, 100);
            match db.list_meetings_visible(limit, &unlocked_set) {
                Ok(ms) => ms
                    .iter()
                    .map(|m| {
                        format!(
                            "- {} · {} · {:?} · id:{}",
                            m.title.clone().unwrap_or_else(|| "(untitled)".into()),
                            m.started_at,
                            m.status,
                            m.id
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(e) => return rpc_err(id, -32000, &format!("list failed: {e}")),
            }
        }
        other => return rpc_err(id, -32602, &format!("unknown tool: {other}")),
    };
    text_result(id, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_unlocked() -> UnlockedSet {
        Arc::new(Mutex::new(HashSet::new()))
    }

    fn rpc(body: &str) -> Option<Value> {
        handle_rpc(
            &PathBuf::from("/nonexistent.sqlite"),
            body,
            &empty_unlocked(),
            None,
            None,
        )
    }

    #[test]
    fn initialize_returns_server_info() {
        let r = rpc(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).unwrap();
        assert_eq!(r["result"]["serverInfo"]["name"], "murmur");
        assert_eq!(r["result"]["protocolVersion"], "2024-11-05");
    }

    #[test]
    fn tools_list_has_three_tools() {
        let r = rpc(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap();
        assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn notification_returns_none() {
        assert!(rpc(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
    }

    #[test]
    fn parse_error_and_unknown_method() {
        assert_eq!(rpc("not json").unwrap()["error"]["code"], -32700);
        assert_eq!(
            rpc(r#"{"jsonrpc":"2.0","id":3,"method":"bogus"}"#).unwrap()["error"]["code"],
            -32601
        );
    }

    fn rpc_auth(body: &str, expected: Option<&str>, auth: Option<&str>) -> Option<Value> {
        handle_rpc(
            &PathBuf::from("/nonexistent.sqlite"),
            body,
            &empty_unlocked(),
            expected,
            auth,
        )
    }

    #[test]
    fn token_disabled_keeps_discovery_open() {
        // Default (no enforcement): initialize/tools/list/ping all succeed without a token.
        assert!(rpc(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap()["result"].is_object());
        assert!(rpc(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).unwrap()["result"]
            .is_object());
    }

    #[test]
    fn token_required_gates_only_tools_call() {
        // NOTE: every assertion here must STOP before `handle_tool_call` reaches `Db::open`,
        // because `Db::open` fetches the SQLCipher DEK from the real Keychain (which can block on
        // a freshly-signed test binary). The unauthorized branches return early in `handle_rpc`,
        // so they never touch the DB — exactly the security-critical path we want to prove.
        let body = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"list_recent_meetings","arguments":{}}}"#;
        // No Authorization header → unauthorized error, returned BEFORE any DB access.
        let unauth = rpc_auth(body, Some("sekret"), None).unwrap();
        assert_eq!(unauth["error"]["code"], -32001);
        // Wrong token → unauthorized, also before DB access.
        let wrong = rpc_auth(body, Some("sekret"), Some("Bearer nope")).unwrap();
        assert_eq!(wrong["error"]["code"], -32001);
        // Discovery stays OPEN even with enforcement on (no token needed, no DB access).
        let disc = rpc_auth(
            r#"{"jsonrpc":"2.0","id":8,"method":"tools/list"}"#,
            Some("sekret"),
            None,
        )
        .unwrap();
        assert!(disc["result"]["tools"].is_array());
        // The "correct token reaches the DB" path is intentionally NOT asserted here: it would
        // call `Db::open` → real Keychain. `bearer_ok` (below) proves the matcher in isolation.
    }

    #[test]
    fn bearer_ok_matches_case_insensitive_scheme() {
        assert!(bearer_ok(Some("Bearer abc"), "abc"));
        assert!(bearer_ok(Some("bearer abc"), "abc"));
        assert!(!bearer_ok(Some("Basic abc"), "abc"));
        assert!(!bearer_ok(None, "abc"));
    }
}
