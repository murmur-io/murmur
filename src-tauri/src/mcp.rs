//! Localhost MCP server (HTTP, 127.0.0.1 only) exposing the user's meetings to MCP clients
//! (Claude Desktop / Code) with NO egress. Read-only tools over the SQLite DB. Implements the
//! MCP JSON-RPC essentials (initialize / tools/list / tools/call) over HTTP POST.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tiny_http::{Header, Method, Response, Server};

use crate::storage::Db;

/// Fixed localhost port for the MCP server.
pub const MCP_PORT: u16 = 8765;

/// Spawn the MCP server on a background thread. Best-effort: a bind failure is logged; the app
/// continues normally.
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
    tracing::info!(target: "mcp", "MCP server listening on http://{addr}");
    for mut req in server.incoming_requests() {
        if *req.method() != Method::Post {
            let _ = req.respond(
                Response::from_string("Murmur MCP server — POST JSON-RPC here.")
                    .with_status_code(200),
            );
            continue;
        }
        let mut body = String::new();
        let _ = std::io::Read::read_to_string(req.as_reader(), &mut body);
        match handle_rpc(&db_path, &body) {
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
fn handle_rpc(db_path: &Path, body: &str) -> Option<Value> {
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
        "tools/call" => return Some(handle_tool_call(db_path, id, req.get("params"))),
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

fn handle_tool_call(db_path: &Path, id: Value, params: Option<&Value>) -> Value {
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

    fn rpc(body: &str) -> Option<Value> {
        handle_rpc(&PathBuf::from("/nonexistent.sqlite"), body)
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
}
