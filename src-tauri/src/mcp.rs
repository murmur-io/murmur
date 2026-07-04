//! Localhost MCP server (HTTP, 127.0.0.1 only) exposing the user's meetings to MCP clients
//! (Claude Desktop / Code) with NO egress. Read-only tools over the SQLite DB. Implements the
//! MCP JSON-RPC essentials (initialize / tools/list / tools/call) over HTTP POST.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tiny_http::{Header, Method, Response, Server};

use crate::storage::Db;

/// Fixed localhost port for the MCP server.
pub const MCP_PORT: u16 = 8765;

/// Max request body we will read (E5). The MCP JSON-RPC requests are tiny; cap hard so a
/// malicious local client can't OOM us with an unbounded body.
const MAX_BODY_BYTES: u64 = 1 << 20; // 1 MiB

/// The only `Host` header values we accept (E2). A request whose Host is anything else — a DNS
/// name resolving to 127.0.0.1, a `0.0.0.0` rebinding, an external host — is rejected, which
/// blocks DNS-rebinding attacks against the localhost server.
const ALLOWED_HOSTS: &[&str] = &["127.0.0.1:8765", "localhost:8765"];

/// The only `Origin` header values we accept when one is present (E5). A browser page on another
/// origin (or a `null` opaque origin) must not be able to script this server. Requests with NO
/// Origin (native MCP clients like Claude Desktop/Code) are allowed through — Origin is a
/// browser-set header. We never reflect the Origin back.
fn origin_allowed(origin: &str) -> bool {
    matches!(
        origin,
        "http://127.0.0.1:8765" | "http://localhost:8765" | "http://127.0.0.1" | "http://localhost"
    )
}

/// Case-insensitive fetch of a single request header value. Compares the header field name
/// ASCII-case-insensitively against `name` (avoids `tiny_http::HeaderField::equiv`, which requires
/// a `'static` argument).
fn header_value(req: &tiny_http::Request, name: &str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

/// Shared session unlock set: folder ids whose sealed notes are decrypted into the markdown
/// column for this session (so MCP can read them as plaintext + the visibility filter lets them
/// through). Sealed-and-not-unlocked notes stay invisible.
pub type UnlockedSet = Arc<Mutex<HashSet<String>>>;

/// Spawn the MCP server on a background thread. Best-effort: a bind failure is logged; the app
/// continues normally. `unlocked` is shared with the command surface so visibility tracks the
/// live session state. `require_token` gates `tools/call` behind a bearer token (default ON —
/// `AppConfig::mcp_require_token` defaults `true`, and `lib.rs` fails CLOSED to `true` on a
/// poisoned/unreadable config).
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
                // FAIL CLOSED (E3): enforcement is required but the token could not be minted/read.
                // Do NOT fall back to an unauthenticated server — that would serve the whole tool
                // surface ungated. Refuse to start the MCP listener so the gate can never be
                // bypassed by a transient Keychain failure.
                tracing::error!(target: "mcp", error = %e, "MCP token required but unavailable — refusing to start the MCP server (fail closed)");
                return;
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

        // E2: reject any request whose Host header is not exactly one of our loopback hosts.
        // A missing Host on HTTP/1.1 is non-conformant — reject it too (fail-closed).
        match header_value(&req, "Host") {
            Some(h) if ALLOWED_HOSTS.contains(&h.trim()) => {}
            _ => {
                let _ = req.respond(Response::from_string("forbidden host").with_status_code(403));
                continue;
            }
        }

        // E5: if an Origin header is present it must be on the loopback allow-list. We never
        // reflect it; a cross-origin / null Origin is refused outright.
        if let Some(origin) = header_value(&req, "Origin") {
            if !origin_allowed(origin.trim()) {
                let _ =
                    req.respond(Response::from_string("forbidden origin").with_status_code(403));
                continue;
            }
        }

        // Extract the bearer token (if any) from the Authorization header before consuming body.
        let auth = header_value(&req, "Authorization");
        // E5: cap the body so a local client cannot stream an unbounded payload. `Read::take`
        // needs a `Sized` reader; `req.as_reader()` is `&mut dyn Read`, so take a `&mut` of it
        // (which is `Sized` + `Read`) before `.take(..)`.
        let mut body = String::new();
        {
            use std::io::Read as _;
            let reader = req.as_reader();
            let _ = reader.take(MAX_BODY_BYTES).read_to_string(&mut body);
        }
        match handle_rpc(
            &db_path,
            &body,
            &unlocked,
            expected_token.as_deref(),
            auth.as_deref(),
        ) {
            Some(resp) => {
                // The header pair is a compile-time ASCII constant so this never errors, but we do
                // NOT unwrap on the per-request server loop (a panic here would kill the MCP thread
                // for the whole session). Fall back to a header-less 200 in the impossible Err case.
                let resp_body = resp.to_string();
                match Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]) {
                    Ok(h) => {
                        let _ = req.respond(
                            Response::from_string(resp_body)
                                .with_status_code(200)
                                .with_header(h),
                        );
                    }
                    Err(_) => {
                        let _ = req.respond(Response::from_string(resp_body).with_status_code(200));
                    }
                }
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

    // E3: when enforcement is on, require a valid bearer token before ANY method — including
    // initialize / tools/list / ping. Discovery is no longer open: an unauthenticated local
    // process cannot even enumerate the tools. The check runs first, before any dispatch.
    if let Some(expected) = expected_token {
        if !bearer_ok(auth, expected) {
            return Some(rpc_err(id, -32001, "unauthorized: bearer token required"));
        }
    }

    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "murmur", "version": env!("CARGO_PKG_VERSION") }
        }),
        "tools/list" => json!({ "tools": tools_spec() }),
        "ping" => json!({}),
        "tools/call" => {
            return Some(handle_tool_call(db_path, id, req.get("params"), unlocked));
        }
        _ => return Some(rpc_err(id, -32601, "method not found")),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// Bearer-token check in CONSTANT TIME (E5): the Authorization header must be `Bearer <expected>`
/// and the token must equal `expected` byte-for-byte. The comparison uses `subtle::ConstantTimeEq`
/// over fixed-length byte slices so a timing side-channel cannot be used to recover the token a
/// prefix at a time. A length mismatch short-circuits to `false` WITHOUT a data-dependent compare
/// (lengths are not secret; the bytes are), and a non-matching length never feeds `ct_eq` mismatched
/// slices.
fn bearer_ok(auth: Option<&str>, expected: &str) -> bool {
    let Some(h) = auth else { return false };
    let Some(token) = h
        .strip_prefix("Bearer ")
        .or_else(|| h.strip_prefix("bearer "))
    else {
        return false;
    };
    let token = token.trim().as_bytes();
    let expected = expected.as_bytes();
    if token.len() != expected.len() {
        return false;
    }
    token.ct_eq(expected).into()
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
            "description": "Full-text search across your meeting titles, transcripts, notes, and imported documents/brain notes. Returns matching meetings and documents with snippets and ids.",
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
        },
        {
            "name": "search_semantic",
            "description": "Semantic (meaning-based) search across your meeting notes and imported documents/brain notes, fused with full-text search. Finds relevant content even without the exact words. When semantic search is disabled in Murmur settings it falls back to keyword-only matching (the result says so).",
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] }
        },
        {
            "name": "get_open_commitments",
            "description": "Roll up every OPEN action item ('- [ ]', still open / not done) across your meetings, with each item's owner, due date and source meeting. Answers 'what did I promise / what is still open'. Optionally filter by owner (case-insensitive). Sealed-and-locked meetings are excluded.",
            "inputSchema": { "type": "object", "properties": { "owner": { "type": "string" } } }
        },
        {
            "name": "get_entity_dossier",
            "description": "Assemble a DOSSIER on one person or project across all your meetings: a timeline of mentions, the entity's open commitments, and co-occurring people/projects — each citing its source meeting [[Title]]. Pass an entity name (e.g. 'Anna' or 'Project Atlas') or id. Returns the gated source material for YOU to synthesize the 'state of [[entity]]' (Overview, Timeline, Open commitments, Last said / next step). Sealed-and-locked meetings are excluded.",
            "inputSchema": { "type": "object", "properties": { "entity": { "type": "string" } }, "required": ["entity"] }
        }
    ])
}

/// A tool-dispatch error mapped to a JSON-RPC `(code, message)`. Kept separate from the `Value`
/// builders so the dispatch logic is testable against an injected `Db` without the HTTP/JSON-RPC
/// envelope (and without `handle_tool_call`'s `Db::open` → Keychain).
type ToolError = (i64, String);

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
    match dispatch_tool(&db, name, &args, &unlocked_set) {
        Ok(text) => text_result(id, text),
        Err((code, msg)) => rpc_err(id, code, &msg),
    }
}

/// Dispatch a `tools/call` against an OPEN `Db`. THIN MAPPER: parse the JSON-RPC tool name + args
/// into a transport-agnostic [`crate::tools::ToolCall`], then run it through the single gated
/// [`crate::tools::execute_tool`] seam (shared with the future local brain). Every read there is
/// visibility-gated against `unlocked_set` (`search_visible` / `meeting_is_visible` /
/// `get_note_if_visible` / `list_meetings_visible` / `search_hybrid_visible` / `build_dossier_data`),
/// so a sealed-and-not-unlocked meeting is invisible to all of them. JSON-RPC error codes for the
/// transport concerns (unknown tool, missing required arg) are produced HERE; runtime tool failures
/// map to `-32000` exactly as before. Returns the tool's text payload or a `(code, message)` error.
fn dispatch_tool(
    db: &Db,
    name: &str,
    args: &Value,
    unlocked_set: &HashSet<String>,
) -> std::result::Result<String, ToolError> {
    use crate::tools::ToolCall;
    let call = match name {
        "search_meetings" => ToolCall::SearchMeetings {
            query: args
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        },
        "search_semantic" => ToolCall::SearchSemantic {
            query: args
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        },
        "get_meeting" => ToolCall::GetMeeting {
            meeting_id: args
                .get("meetingId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        },
        "list_recent_meetings" => ToolCall::ListRecentMeetings {
            limit: args
                .get("limit")
                .and_then(Value::as_i64)
                .unwrap_or(20)
                .clamp(1, 100),
        },
        "get_open_commitments" => ToolCall::GetOpenCommitments {
            owner: args
                .get("owner")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|o| !o.is_empty())
                .map(str::to_string),
        },
        "get_entity_dossier" => {
            let entity = args.get("entity").and_then(Value::as_str).unwrap_or("");
            if entity.trim().is_empty() {
                return Err((-32602, "missing required argument: entity".to_string()));
            }
            ToolCall::GetEntityDossier {
                entity: entity.to_string(),
            }
        }
        other => return Err((-32602, format!("unknown tool: {other}"))),
    };
    // The `semantic_search_enabled` flag lives in the whole-DB-encrypted settings table; load it from
    // the SAME DB the MCP reader opened. On a load failure this degrades to `AppConfig::default()`,
    // whose Tier 1 default is now flag ON — harmless: with no e5 model the hybrid `search_semantic`
    // leg degenerates to the SAME gated FTS (no leak, no crash), and every leg stays visibility-gated.
    let config = crate::settings::AppConfig::load(db).unwrap_or_default();
    crate::tools::execute_tool(&call, db, unlocked_set, &config)
        .map_err(|e| (-32000, e.to_string()))
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
    fn tools_list_has_six_tools() {
        let r = rpc(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 6);
        // The Phase 2b semantic tool is advertised.
        assert!(tools.iter().any(|t| t["name"] == "search_semantic"));
        // The Phase 5a open-commitments rollup tool is advertised.
        assert!(tools.iter().any(|t| t["name"] == "get_open_commitments"));
        // The Phase 5b entity-dossier tool is advertised.
        assert!(tools.iter().any(|t| t["name"] == "get_entity_dossier"));
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
        // With enforcement OFF (expected_token = None), discovery still works without a token —
        // this preserves the no-token local connection when the user hasn't enabled the gate.
        assert!(rpc(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap()["result"].is_object());
        assert!(
            rpc(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).unwrap()["result"].is_object()
        );
    }

    #[test]
    fn token_required_gates_every_method() {
        // E3: with enforcement ON, EVERY method (initialize / tools/list / ping / tools/call)
        // requires a valid bearer token. NOTE: every assertion here STOPS before
        // `handle_tool_call` reaches `Db::open` (real Keychain), because the unauthorized branch
        // returns early in `handle_rpc` — exactly the security-critical path we want to prove.
        for method in ["initialize", "tools/list", "ping", "tools/call"] {
            let body = format!(
                r#"{{"jsonrpc":"2.0","id":7,"method":"{method}","params":{{"name":"list_recent_meetings","arguments":{{}}}}}}"#
            );
            // No Authorization header → unauthorized, BEFORE any dispatch/DB access.
            let unauth = rpc_auth(&body, Some("sekret"), None).unwrap();
            assert_eq!(
                unauth["error"]["code"], -32001,
                "method {method} must be gated"
            );
            // Wrong token → unauthorized, also before dispatch.
            let wrong = rpc_auth(&body, Some("sekret"), Some("Bearer nope")).unwrap();
            assert_eq!(
                wrong["error"]["code"], -32001,
                "method {method} wrong-token must be gated"
            );
        }
        // A CORRECT token lets discovery through (no DB access on initialize/tools/list/ping).
        let ok = rpc_auth(
            r#"{"jsonrpc":"2.0","id":8,"method":"tools/list"}"#,
            Some("sekret"),
            Some("Bearer sekret"),
        )
        .unwrap();
        assert!(ok["result"]["tools"].is_array());
        // The "correct token reaches the DB" path is intentionally NOT asserted here: it would
        // call `Db::open` → real Keychain. `bearer_ok` (below) proves the matcher in isolation.
    }

    #[test]
    fn bearer_ok_constant_time_matches_scheme_and_value() {
        assert!(bearer_ok(Some("Bearer abc"), "abc"));
        assert!(bearer_ok(Some("bearer abc"), "abc"));
        assert!(!bearer_ok(Some("Basic abc"), "abc"));
        assert!(!bearer_ok(Some("Bearer abc"), "abcd")); // length mismatch
        assert!(!bearer_ok(Some("Bearer abd"), "abc")); // same length, different bytes
        assert!(!bearer_ok(None, "abc"));
    }

    #[test]
    fn host_allow_list_is_loopback_only() {
        // E2: only the two exact loopback authorities are accepted.
        assert!(ALLOWED_HOSTS.contains(&"127.0.0.1:8765"));
        assert!(ALLOWED_HOSTS.contains(&"localhost:8765"));
        // Anything else (rebinding host, external name, bare host w/o port) is NOT in the list.
        for bad in [
            "evil.example.com:8765",
            "0.0.0.0:8765",
            "127.0.0.1",
            "localhost",
            "127.0.0.1:9999",
        ] {
            assert!(!ALLOWED_HOSTS.contains(&bad), "{bad} must not be allowed");
        }
    }

    // ── Phase 2b: search_semantic MCP tool (gated) ─────────────────────────────────────────────

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn temp_db() -> (Db, PathBuf) {
        let p = crate::storage::db::unique_temp_path("murmur-mcp-test", "sqlite");
        let db = Db::open_with_key(&p, TEST_DEK).unwrap();
        (db, p)
    }

    fn seed(db: &Db, mid: &str, title: &str, md: &str, folder: Option<&str>) {
        use crate::storage::models::{Meeting, MeetingStatus, NoteRecord};
        db.insert_meeting(&Meeting {
            id: mid.to_string(),
            started_at: "2026-06-27T09:00:00Z".to_string(),
            ended_at: None,
            title: Some(title.to_string()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: mid.to_string(),
            provider_id: "claude_code".to_string(),
            markdown: md.to_string(),
            created_at: "2026-06-27T09:05:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder(mid, folder).unwrap();
    }

    /// Flag OFF (the default): `search_semantic` DEGRADES to gated keyword (FTS/BM25) matching —
    /// no vector read ever runs — and the output is HONESTLY labelled as keyword matching, so the
    /// MCP client is never told a semantic search happened. Content stays reachable on the default
    /// install (the PR B write-only-memory fix).
    #[test]
    fn search_semantic_flag_off_degrades_to_labelled_keyword_match() {
        let (db, p) = temp_db();
        seed(&db, "m1", "Budget", "budget planning hiring quarter", None);
        // Tier 1 flipped the semantic default ON; this test covers the flag-OFF keyword-degradation
        // labelling, so pin the flag it asserts about explicitly (it used to rely on the old default).
        db.set_setting("semantic_search_enabled", "false").unwrap();
        let out = dispatch_tool(
            &db,
            "search_semantic",
            &json!({ "query": "budget" }),
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            out.contains("semantic search is off"),
            "flag-off semantic tool must label its keyword degradation, got: {out}"
        );
        assert!(
            out.contains("Budget"),
            "flag-off fallback must still surface the gated keyword hit, got: {out}"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// Flag ON: `search_semantic` routes through `search_hybrid_visible`, which applies the SAME
    /// visibility gate as `search_meetings`. A sealed-and-not-unlocked meeting is EXCLUDED from the
    /// results and reappears once its folder is session-unlocked.
    #[test]
    fn search_semantic_is_visibility_gated_when_enabled() {
        use crate::storage::models::Folder;
        let (db, p) = temp_db();
        // Enable the flag in the settings table.
        let cfg = crate::settings::AppConfig {
            semantic_search_enabled: true,
            ..Default::default()
        };
        cfg.save(&db).unwrap();

        db.insert_folder(&Folder {
            id: "f-lock".to_string(),
            name: "Secret".to_string(),
            path: "Secret".to_string(),
            parent_id: None,
            locked: false, // index while visible, then lock.
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
        seed(
            &db,
            "open",
            "Open",
            "budget planning hiring quarter apollo",
            None,
        );
        seed(
            &db,
            "sealed",
            "Sealed",
            "budget planning hiring quarter secret",
            Some("f-lock"),
        );

        let emb = crate::embed::active_embedder();
        db.index_meeting_chunks("open", &[], emb.as_ref()).unwrap();
        db.index_meeting_chunks("sealed", &[], emb.as_ref()).unwrap();
        // Seal the folder AFTER indexing (a stray vec row now exists for a sealed meeting).
        db.set_folder_locked("f-lock", true, None).unwrap();

        let args = json!({ "query": "budget planning hiring quarter" });

        // Not unlocked → sealed meeting must NOT appear.
        let out = dispatch_tool(&db, "search_semantic", &args, &HashSet::new()).unwrap();
        assert!(out.contains("id:open"), "open meeting must surface");
        assert!(
            !out.contains("id:sealed"),
            "sealed-not-unlocked meeting leaked through search_semantic (gate violation)"
        );

        // Session-unlock → sealed meeting reappears.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let out2 = dispatch_tool(&db, "search_semantic", &args, &unlocked).unwrap();
        assert!(
            out2.contains("id:sealed"),
            "unlocked meeting must reappear in semantic results"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// Phase 5a: `get_open_commitments` is visibility-gated exactly like the other tools. A sealed-
    /// and-not-unlocked meeting's open action items NEVER appear; they reappear once the folder is
    /// session-unlocked. The payload renders owner · due · "text" · [[Title]].
    #[test]
    fn get_open_commitments_is_visibility_gated() {
        use crate::storage::models::Folder;
        let (db, p) = temp_db();
        db.insert_folder(&Folder {
            id: "f-lock".to_string(),
            name: "Secret".to_string(),
            path: "Secret".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
        seed(
            &db,
            "open",
            "Open Sync",
            "## Action items\n- [ ] Anna — ship the deck 2026-07-01\n- [x] Bob — already done\n",
            None,
        );
        seed(
            &db,
            "sealed",
            "Secret Sync",
            "## Action items\n- [ ] Carol — sign the contract 2026-07-05\n",
            Some("f-lock"),
        );
        db.set_folder_locked("f-lock", true, None).unwrap();

        // Not unlocked → only the open meeting's open item; sealed item invisible; done item dropped.
        let out = dispatch_tool(&db, "get_open_commitments", &json!({}), &HashSet::new()).unwrap();
        assert!(
            out.contains("ship the deck"),
            "open commitment must surface"
        );
        assert!(out.contains("[[Open Sync]]"), "source title must render");
        assert!(out.contains("due 2026-07-01"), "due date must render");
        assert!(out.contains("Anna"), "owner must render");
        assert!(
            !out.contains("already done"),
            "checked-off item must not be a commitment"
        );
        assert!(
            !out.contains("sign the contract") && !out.contains("Secret Sync"),
            "sealed-not-unlocked meeting's commitments leaked (gate violation)"
        );

        // Session-unlock → the sealed meeting's commitment reappears.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let out2 = dispatch_tool(&db, "get_open_commitments", &json!({}), &unlocked).unwrap();
        assert!(
            out2.contains("sign the contract"),
            "unlocked commitment must reappear"
        );

        // Owner filter (case-insensitive).
        let out3 = dispatch_tool(
            &db,
            "get_open_commitments",
            &json!({ "owner": "anna" }),
            &unlocked,
        )
        .unwrap();
        assert!(
            out3.contains("ship the deck"),
            "owner filter must keep Anna's item"
        );
        assert!(
            !out3.contains("sign the contract"),
            "owner filter must drop Carol's item"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// Phase 5b: `get_entity_dossier` is visibility-gated AND egress-free. A sealed-and-not-unlocked
    /// mentioning meeting contributes nothing to the dossier payload (no title, no note body), and
    /// reappears once the folder is session-unlocked. The dispatch builds GATED STRUCTURED DATA only
    /// — it never constructs a provider or makes a cloud call (the whole `dispatch_tool` path has no
    /// `make_provider`/`complete`), so the MCP server stays read-only + egress-free.
    #[test]
    fn get_entity_dossier_is_visibility_gated_and_egress_free() {
        use crate::storage::models::{EntityKind, Folder};
        let (db, p) = temp_db();
        db.insert_folder(&Folder {
            id: "f-lock".to_string(),
            name: "Secret".to_string(),
            path: "Secret".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
        seed(
            &db,
            "open",
            "Kickoff",
            "## Action items\n- [ ] Anna — draft Atlas spec 2026-07-01\n",
            None,
        );
        seed(
            &db,
            "sealed",
            "Secret Atlas Review",
            "LOCKED Atlas acquisition price\n## Action items\n- [ ] Carol — sign 2026-07-09\n",
            Some("f-lock"),
        );
        let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
        db.add_mention(&atlas, "open").unwrap();
        db.add_mention(&atlas, "sealed").unwrap();
        db.set_folder_locked("f-lock", true, None).unwrap();

        // Not unlocked → the dossier resolves Atlas by NAME, includes the open meeting [[Title]]
        // and its commitment, and EXCLUDES the sealed meeting's title, note body, and commitment.
        let args = json!({ "entity": "Atlas" });
        let out = dispatch_tool(&db, "get_entity_dossier", &args, &HashSet::new()).unwrap();
        assert!(
            out.contains("DOSSIER for [[Atlas]]"),
            "overview header must render"
        );
        assert!(out.contains("[[Kickoff]]"), "visible meeting must be cited");
        assert!(
            out.contains("draft Atlas spec"),
            "visible open commitment must surface"
        );
        assert!(
            !out.contains("Secret Atlas Review") && !out.contains("LOCKED Atlas acquisition"),
            "sealed-not-unlocked meeting leaked into the dossier (gate violation)"
        );
        assert!(
            !out.contains("sign"),
            "sealed commitment leaked into the dossier"
        );

        // Session-unlock → the sealed meeting + its content reappear.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let out2 = dispatch_tool(&db, "get_entity_dossier", &args, &unlocked).unwrap();
        assert!(
            out2.contains("[[Secret Atlas Review]]"),
            "unlocked meeting must reappear"
        );
        assert!(
            out2.contains("LOCKED Atlas acquisition"),
            "unlocked content must reappear"
        );

        // Unknown entity → a friendly, non-leaking message (never an error).
        let none = dispatch_tool(
            &db,
            "get_entity_dossier",
            &json!({ "entity": "Nonexistent" }),
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            none.contains("No visible entity"),
            "unknown entity → friendly message"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn origin_allow_list_rejects_cross_origin_and_null() {
        // E5: loopback origins allowed; cross-origin and the opaque "null" origin rejected.
        assert!(origin_allowed("http://127.0.0.1:8765"));
        assert!(origin_allowed("http://localhost:8765"));
        assert!(origin_allowed("http://127.0.0.1"));
        assert!(origin_allowed("http://localhost"));
        for bad in [
            "null",
            "https://evil.example.com",
            "http://evil.example.com",
            "https://127.0.0.1:8765", // wrong scheme
            "http://127.0.0.1:9999",  // wrong port
        ] {
            assert!(!origin_allowed(bad), "{bad} must be rejected");
        }
    }
}
