//! Localhost MCP server (HTTP, 127.0.0.1 only) exposing the user's meetings to MCP clients
//! (Claude Desktop / Code) with NO egress. Read-only tools over the SQLite DB.
//!
//! Hardening (see `docs/SECURITY-AUDIT.md`, finding H1): the server is **opt-in** (Settings →
//! Local MCP server, default off), and every request must (a) carry no browser `Origin`,
//! (b) target a loopback `Host`, and (c) present `Authorization: Bearer <token>` matching the
//! per-install token. (a)+(b) defeat DNS-rebinding from a web page; (c) stops other local
//! processes that can reach the port but cannot read the app-data DB. The `enabled` flag is
//! re-checked per request, so toggling the feature off takes effect live (no restart needed).

use std::path::PathBuf;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::error::Result;
use crate::storage::Db;

/// Fixed localhost port for the MCP server.
pub const MCP_PORT: u16 = 8765;

/// Settings-table keys backing the MCP gate.
const K_MCP_ENABLED: &str = "mcp_enabled";
const K_MCP_TOKEN: &str = "mcp_token";

/// Spawn the MCP server on a background thread. Best-effort: a bind/DB failure is logged and
/// the app continues normally. Callers gate this on the `mcp_enabled` setting (see
/// `commands::maybe_start_mcp`); the per-request check below also enforces it live.
pub fn spawn(db_path: PathBuf) {
    let _ = std::thread::Builder::new()
        .name("murmur-mcp".into())
        .spawn(move || run(db_path));
}

fn run(db_path: PathBuf) {
    let addr = format!("127.0.0.1:{MCP_PORT}");
    let server = match Server::http(&addr) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(target: "mcp", error = %e, "MCP server failed to bind {addr}");
            return;
        }
    };
    // One connection for the server's lifetime: read-only queries, and WAL lets it observe the
    // main app's committed writes. Also avoids re-running migrations on every request.
    let db = match Db::open(&db_path) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(target: "mcp", error = %e, "MCP server failed to open DB");
            return;
        }
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

        // Read the gate-relevant headers as owned values first, so the borrow on `req` is
        // released before we read its body / consume it to respond.
        let host = header_value(&req, "Host");
        let origin = header_value(&req, "Origin");
        let auth = header_value(&req, "Authorization");

        let enabled = is_enabled(&db);
        let token = current_token(&db);
        if let Err((code, msg)) = authorize(
            host.as_deref(),
            origin.as_deref(),
            auth.as_deref(),
            enabled,
            token.as_deref(),
        ) {
            let _ = req.respond(Response::from_string(msg).with_status_code(code));
            continue;
        }

        let mut body = String::new();
        let _ = req.as_reader().read_to_string(&mut body);
        match handle_rpc(&db, &body) {
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

/// First value of header `name` (case-insensitive), as an owned String.
fn header_value(req: &Request, name: &'static str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str().to_string())
}

// ── Security gate ────────────────────────────────────────────────────────────

/// Decide whether a request may proceed. Pure (header values + DB-derived state in, verdict
/// out) so it is unit-tested below. `Err((http_status, message))` rejects the request.
fn authorize(
    host: Option<&str>,
    origin: Option<&str>,
    auth: Option<&str>,
    enabled: bool,
    token: Option<&str>,
) -> std::result::Result<(), (u16, &'static str)> {
    if !enabled {
        return Err((403, "MCP server is disabled in Murmur settings"));
    }
    // A browser attaches Origin on a cross-origin POST (including a DNS-rebound page); a
    // legitimate MCP stdio/HTTP client never does. So any Origin means "not an MCP client".
    if origin.is_some() {
        return Err((403, "Origin header not allowed"));
    }
    // Anti-DNS-rebinding: the Host must be loopback, not an attacker's rebound domain.
    if !host.map(is_loopback_host).unwrap_or(false) {
        return Err((403, "invalid Host header"));
    }
    let expected = match token {
        Some(t) if !t.is_empty() => t,
        _ => return Err((403, "MCP token not provisioned")),
    };
    match auth.and_then(parse_bearer) {
        Some(p) if ct_eq(p.as_bytes(), expected.as_bytes()) => Ok(()),
        _ => Err((401, "missing or invalid bearer token")),
    }
}

/// Accept only loopback Hosts (with or without the server port).
fn is_loopback_host(h: &str) -> bool {
    let with_port = |name: &str| h == format!("{name}:{MCP_PORT}");
    h == "127.0.0.1" || h == "localhost" || with_port("127.0.0.1") || with_port("localhost")
}

/// Extract the token from an `Authorization: Bearer <token>` header (scheme case-insensitive).
fn parse_bearer(auth: &str) -> Option<&str> {
    let rest = auth
        .strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("bearer "))?;
    let t = rest.trim();
    (!t.is_empty()).then_some(t)
}

/// Constant-time byte compare (lengths may differ-leak; the token is fixed-length).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ── Settings-backed state ────────────────────────────────────────────────────

fn is_enabled(db: &Db) -> bool {
    db.get_setting(K_MCP_ENABLED).ok().flatten().as_deref() == Some("true")
}

fn current_token(db: &Db) -> Option<String> {
    db.get_setting(K_MCP_TOKEN)
        .ok()
        .flatten()
        .filter(|t| !t.is_empty())
}

/// Return the per-install MCP token, generating + persisting one on first use.
pub fn ensure_token(db: &Db) -> Result<String> {
    if let Some(t) = current_token(db) {
        return Ok(t);
    }
    let t = uuid::Uuid::new_v4().simple().to_string();
    db.set_setting(K_MCP_TOKEN, &t)?;
    Ok(t)
}

/// Build the ready-to-paste Claude Desktop client config. Includes the bearer header whenever
/// a token is present.
pub fn client_config_json(url: &str, token: &str) -> String {
    let server = if token.is_empty() {
        json!({ "url": url })
    } else {
        json!({ "url": url, "headers": { "Authorization": format!("Bearer {token}") } })
    };
    let v = json!({ "mcpServers": { "murmur": server } });
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
}

// ── JSON-RPC ─────────────────────────────────────────────────────────────────

/// Returns Some(response) for JSON-RPC requests, None for notifications.
fn handle_rpc(db: &Db, body: &str) -> Option<Value> {
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
        "tools/call" => return Some(handle_tool_call(db, id, req.get("params"))),
        _ => return Some(rpc_err(id, -32601, "method not found")),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
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

fn handle_tool_call(db: &Db, id: Value, params: Option<&Value>) -> Value {
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let args = params
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let text = match name {
        "search_meetings" => {
            let q = args.get("query").and_then(Value::as_str).unwrap_or("");
            match db.search(q, 20) {
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
            let note = db.get_latest_note_for_meeting(mid).ok().flatten();
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
        "list_recent_meetings" => {
            let limit = args
                .get("limit")
                .and_then(Value::as_i64)
                .unwrap_or(20)
                .clamp(1, 100);
            match db.list_meetings(limit) {
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

    fn test_db() -> Db {
        let p = std::env::temp_dir().join(format!(
            "murmur-mcp-test-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        Db::open(&p).unwrap()
    }

    fn rpc(body: &str) -> Option<Value> {
        handle_rpc(&test_db(), body)
    }

    // ── protocol layer ──

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

    // ── security gate ──

    #[test]
    fn authorize_blocks_when_disabled() {
        let r = authorize(Some("127.0.0.1:8765"), None, Some("Bearer x"), false, Some("x"));
        assert_eq!(r.unwrap_err().0, 403);
    }

    #[test]
    fn authorize_blocks_browser_origin() {
        let r = authorize(
            Some("127.0.0.1:8765"),
            Some("https://evil.example"),
            Some("Bearer tok"),
            true,
            Some("tok"),
        );
        assert_eq!(r.unwrap_err().0, 403);
    }

    #[test]
    fn authorize_blocks_non_loopback_host() {
        let r = authorize(Some("evil.example:8765"), None, Some("Bearer tok"), true, Some("tok"));
        assert_eq!(r.unwrap_err().0, 403);
    }

    #[test]
    fn authorize_requires_matching_token() {
        // wrong token
        assert_eq!(
            authorize(Some("localhost:8765"), None, Some("Bearer nope"), true, Some("tok"))
                .unwrap_err()
                .0,
            401
        );
        // missing header
        assert_eq!(
            authorize(Some("localhost:8765"), None, None, true, Some("tok"))
                .unwrap_err()
                .0,
            401
        );
        // correct token, loopback host, no origin, enabled → allowed
        assert!(authorize(Some("localhost:8765"), None, Some("Bearer tok"), true, Some("tok")).is_ok());
    }

    #[test]
    fn authorize_blocks_when_token_unprovisioned() {
        let r = authorize(Some("localhost:8765"), None, Some("Bearer x"), true, None);
        assert_eq!(r.unwrap_err().0, 403);
    }

    #[test]
    fn loopback_host_matrix() {
        assert!(is_loopback_host("127.0.0.1:8765"));
        assert!(is_loopback_host("localhost:8765"));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("localhost"));
        assert!(!is_loopback_host("evil.example"));
        assert!(!is_loopback_host("127.0.0.1.evil.example:8765"));
        assert!(!is_loopback_host("127.0.0.1:9999"));
    }

    #[test]
    fn bearer_parsing() {
        assert_eq!(parse_bearer("Bearer abc"), Some("abc"));
        assert_eq!(parse_bearer("bearer abc"), Some("abc"));
        assert_eq!(parse_bearer("Basic abc"), None);
        assert_eq!(parse_bearer("Bearer    "), None);
    }

    #[test]
    fn ct_eq_basic() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }

    #[test]
    fn token_round_trips_and_is_stable() {
        let db = test_db();
        let a = ensure_token(&db).unwrap();
        let b = ensure_token(&db).unwrap();
        assert_eq!(a, b, "token must be stable once provisioned");
        assert!(!a.is_empty());
    }

    #[test]
    fn client_config_includes_bearer_only_with_token() {
        assert!(client_config_json("http://127.0.0.1:8765", "tok123").contains("Bearer tok123"));
        assert!(!client_config_json("http://127.0.0.1:8765", "").contains("Authorization"));
    }
}
