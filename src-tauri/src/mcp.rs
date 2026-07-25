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
            "description": "Full-text search across your meeting titles, transcripts, notes, and imported documents/brain notes. Returns matching meetings and documents with snippets and ids. If nothing relevant turns up and you have joined an org, also try org_search — a colleague may have already shared the answer.",
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] }
        },
        {
            "name": "get_meeting",
            "description": "Get a meeting's AI note (summary) and transcript by id (from a search hit labelled 'meeting:...'). The transcript is STRUCTURED by default — one line per segment, '[<start_s>–<end_s>] <Speaker>: <text>' with Me/Others/Unknown speakers and raw-second timestamps; pass transcriptFormat 'plain' for the old flat text. The transcript is returned as a WINDOW (default first 6000 chars) prefixed with 'TOTAL_CHARS: <N> (showing <start>..<end>)'; page a long transcript by passing offset + maxChars.",
            "inputSchema": { "type": "object", "properties": { "meetingId": { "type": "string" }, "transcriptFormat": { "type": "string", "enum": ["structured", "plain"], "description": "Transcript rendering (default 'structured')." }, "offset": { "type": "number", "description": "Chars to skip into the transcript (default 0)." }, "maxChars": { "type": "number", "description": "Max transcript chars to return from offset (default: a bounded 6000-char window with the total disclosed)." } }, "required": ["meetingId"] }
        },
        {
            "name": "get_document",
            "description": "Get the body of one standalone note or imported/uploaded document by id (from a search hit labelled 'document:...'). Use this — not get_meeting — for ids from the DOCUMENTS section of a search result. The body is returned as a WINDOW (default first 6000 chars) prefixed with 'TOTAL_CHARS: <N> (showing <start>..<end>)'; page a big document by passing offset + maxChars.",
            "inputSchema": { "type": "object", "properties": { "documentId": { "type": "string" }, "offset": { "type": "number", "description": "Chars to skip into the body (default 0)." }, "maxChars": { "type": "number", "description": "Max body chars to return from offset (default: a bounded 6000-char window with the total disclosed)." } }, "required": ["documentId"] }
        },
        {
            "name": "get_document_outline",
            "description": "Get the STRUCTURAL OUTLINE (heading/section map + page numbers) of one standalone note or imported/uploaded document by id (from a 'document:...' search hit). Use this on a BIG document BEFORE get_document: read the map, then fetch the section you need with get_document's offset + maxChars instead of paging blindly. Returns the section headings in document order; a flat/heading-less document has no outline. Sealed-and-locked documents return no outline.",
            "inputSchema": { "type": "object", "properties": { "documentId": { "type": "string" } }, "required": ["documentId"] }
        },
        {
            "name": "list_recent_meetings",
            "description": "List the most recent meetings (title, date, status, id).",
            "inputSchema": { "type": "object", "properties": { "limit": { "type": "number" } } }
        },
        {
            "name": "search_semantic",
            "description": "Semantic (meaning-based) search across your meeting notes and imported documents/brain notes, fused with full-text search. Finds relevant content even without the exact words. When semantic search is disabled in Murmur settings it falls back to keyword-only matching (the result says so). If nothing relevant turns up and you have joined an org, also try org_search — a colleague may have already shared the answer.",
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
        },
        {
            "name": "knowledge_diff",
            "description": "The DECISION LEDGER for one person or project: what you knew about it changed over time (bitemporal facts). Pass an entity name (e.g. 'Anna' or 'Project Atlas') or id, plus two ISO-8601 instants 'from' and 'to' (e.g. '2026-06-01T00:00:00Z'). Returns what CHANGED between those two moments (added / removed / changed facts, e.g. status in-progress → shipped) PLUS the full chronological supersession ledger — each decision carries the old value, the new value, when it took effect, and the source meeting id. Answers 'what changed since', 'when did X flip', 'the history of Y's status'. Sealed-and-locked meetings' facts are excluded.",
            "inputSchema": { "type": "object", "properties": { "entity": { "type": "string" }, "from": { "type": "string", "description": "ISO-8601 instant to snapshot the earlier state at." }, "to": { "type": "string", "description": "ISO-8601 instant to snapshot the later state at." } }, "required": ["entity", "from", "to"] }
        },
        {
            "name": "org_search",
            "description": "Fallback for when search_meetings / search_semantic find nothing relevant in your OWN vault and you have joined an org: search the ORGANIZATION brain — notes your colleagues explicitly shared to the shared org brain (synced + decrypted locally; no data leaves this device). Results are attributed '[org · <author>]' and MUST be cited as coming from that colleague. Only meaningful when you have joined an org and consented to org sharing (otherwise returns no results). Use for 'what does the team / someone else know or decide about X' questions.",
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] }
        },
        {
            "name": "query_database",
            "description": "Query the TYPED PROPERTIES of a note-folder's notes as a small database (the folder's Table/Board columns: status, owner, due date, priority, etc.). Give the folder NAME (or id) and a filter: 'key op value' clauses joined by AND / OR, op ∈ = != > < >= <= or 'contains' (e.g. 'status=Done', 'openItems>3', 'owner contains ann', 'status=Open AND priority=High'). Empty filter = every row. Sealed-and-locked note folders are excluded. Use for 'which notes are still open', 'what does Anna own', 'high-priority items' questions over a note-folder's columns.",
            "inputSchema": { "type": "object", "properties": { "folder": { "type": "string" }, "filter": { "type": "string" } }, "required": ["folder"] }
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
/// Brain v3 PR-2 — read an optional non-negative integer MCP arg (agent paging). Absent / non-numeric
/// → 0 (the DEFAULT, mapped to today's byte-identical behavior by the tool).
fn mcp_usize_arg(args: &Value, key: &str) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(usize::MAX as u64) as usize
}

/// Brain v3 audit Fix 2 — the MCP `get_meeting`/`get_document` DEFAULT window. UNLIKE the in-app
/// agentic loop (which caps every tool result at `RESULT_BUDGET` before re-feeding it to the model),
/// a raw MCP `tools/call` returns the ENTIRE payload to the connected client — so a client that
/// omits paging on a multi-MB document/transcript would be flooded. When the MCP client passes NO
/// paging (both `offset` and `maxChars` absent/0) we substitute THIS bounded default `maxChars`, and
/// the tool returns a DISCLOSED window (`TOTAL_CHARS: <N> …`) so the client can see the full length
/// and page the rest with explicit `offset`. A client that DOES pass paging is honored verbatim.
/// DELIBERATE default change (documented): the pre-fix MCP default `(0,0)` returned the whole body.
const MCP_DEFAULT_WINDOW_CHARS: usize = 6000;

/// Resolve the MCP paging window for a body tool: honor an explicit `maxChars`, but whenever the
/// client gives no (or a zero) `maxChars`, bound it to [`MCP_DEFAULT_WINDOW_CHARS`] so a huge payload
/// is windowed + disclosed instead of flooding the client (a raw MCP tools/call has no RESULT_BUDGET).
/// `offset` is honored verbatim, so a client can still page a large body a window at a time. Returns
/// `(offset, max_chars)`.
fn mcp_body_window(args: &Value) -> (usize, usize) {
    let offset = mcp_usize_arg(args, "offset");
    let max_chars = mcp_usize_arg(args, "maxChars");
    // maxChars == 0 means "absent" (mcp_usize_arg's default) or an explicit unbounded request — both
    // are the flood case, so bound them to the default window. An explicit positive maxChars wins.
    let max_chars = if max_chars == 0 {
        MCP_DEFAULT_WINDOW_CHARS
    } else {
        max_chars
    };
    (offset, max_chars)
}

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
            // Feature D: default to the STRUCTURED transcript. Only the exact literal "plain" selects
            // the legacy flat join; an absent/other value routes to "structured".
            transcript_format: args
                .get("transcriptFormat")
                .and_then(Value::as_str)
                .filter(|f| *f == "plain")
                .unwrap_or("structured")
                .to_string(),
            // Brain v3 audit Fix 2 — bound + DISCLOSE the default MCP window (no paging args → a
            // 6000-char disclosed window, not the whole transcript) so a huge transcript can't flood
            // the client; explicit offset/maxChars are honored verbatim.
            offset: mcp_body_window(args).0,
            max_chars: mcp_body_window(args).1,
        },
        "get_document" => ToolCall::GetDocument {
            document_id: args
                .get("documentId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            // Audit Fix 2 — same bounded + disclosed default window as get_meeting.
            offset: mcp_body_window(args).0,
            max_chars: mcp_body_window(args).1,
        },
        // Brain v3 audit Fix 3(b) — the document OUTLINE (heading map). Gated by
        // `get_document_outline_if_visible` inside `execute_tool` (a sealed-not-unlocked doc → empty).
        "get_document_outline" => ToolCall::GetDocumentOutline {
            document_id: args
                .get("documentId")
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
        // Brain v3 PR-6 — the KNOWLEDGE DIFF / decision ledger for one entity. Routes through the
        // SAME gated reader (`resolve_entity_id` + `build_knowledge_diff` → `list_facts_visible`) as
        // the dossier, so a sealed-and-not-unlocked meeting's fact is invisible here too. `entity`,
        // `from`, `to` are all required.
        "knowledge_diff" => {
            let entity = args.get("entity").and_then(Value::as_str).unwrap_or("");
            if entity.trim().is_empty() {
                return Err((-32602, "missing required argument: entity".to_string()));
            }
            let from = args.get("from").and_then(Value::as_str).unwrap_or("");
            let to = args.get("to").and_then(Value::as_str).unwrap_or("");
            if from.trim().is_empty() {
                return Err((-32602, "missing required argument: from".to_string()));
            }
            if to.trim().is_empty() {
                return Err((-32602, "missing required argument: to".to_string()));
            }
            // B2 — this is UNTRUSTED MCP client input, so validate at the dispatch boundary that
            // BOTH `from` and `to` parse as RFC3339. `facts.rs::normalize_instant` returns an
            // unparseable string UNCHANGED and `cmp_instant` then compares it lexically, so a
            // garbage `from` sorts AFTER a real `to`, SWAPS the range, and yields a confident but
            // wrong "0 changes" with NO error. Reject the bad timestamp here (naming the offending
            // arg) rather than silently returning an empty window. `build_knowledge_diff`'s lenient
            // pass-through is left intact for the other in-app callers that rely on it.
            for (arg, value) in [("from", from), ("to", to)] {
                if chrono::DateTime::parse_from_rfc3339(value).is_err() {
                    return Err((
                        -32602,
                        format!("invalid ISO-8601 timestamp for '{arg}': {value}"),
                    ));
                }
            }
            ToolCall::KnowledgeDiff {
                entity: entity.to_string(),
                from: from.to_string(),
                to: to.to_string(),
            }
        }
        // Shared Brain — LOCAL, egress-free search of the org partition (synced colleagues' shares).
        // Untrusted multi-writer content: `execute_tool` provenance-labels + fence-neutralizes it. Not
        // folder-lock gated (org items live outside the lock domain), so `unlocked_set` is irrelevant
        // to it; when no org is joined/consented the partition is empty ⇒ "no results" (never a leak).
        "org_search" => ToolCall::OrgBrainSearch {
            query: args
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        },
        // Feature C — TYPED note-folder database query. `folder` is required (name or id); `filter`
        // is optional (empty = all rows). Gated by `list_notes_visible_typed` against `unlocked_set`,
        // so a sealed-not-unlocked note folder yields no rows here.
        "query_database" => {
            let folder = args.get("folder").and_then(Value::as_str).unwrap_or("");
            if folder.trim().is_empty() {
                return Err((-32602, "missing required argument: folder".to_string()));
            }
            ToolCall::QueryDatabase {
                folder: folder.to_string(),
                filter: args
                    .get("filter")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
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
    fn tools_list_has_eleven_tools() {
        let r = rpc(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 11);
        // The Phase 2b semantic tool is advertised.
        assert!(tools.iter().any(|t| t["name"] == "search_semantic"));
        // The Phase 5a open-commitments rollup tool is advertised.
        assert!(tools.iter().any(|t| t["name"] == "get_open_commitments"));
        // The Phase 5b entity-dossier tool is advertised.
        assert!(tools.iter().any(|t| t["name"] == "get_entity_dossier"));
        // Shared Brain — the org partition search tool is advertised.
        assert!(tools.iter().any(|t| t["name"] == "org_search"));
        // Feature D — the full-note/document reader tool is advertised (the 8th tool).
        assert!(
            tools.iter().any(|t| t["name"] == "get_document"),
            "get_document must be advertised in the MCP tool catalog"
        );
        // Feature C — the typed note-folder database query tool is advertised (the 9th tool).
        assert!(
            tools.iter().any(|t| t["name"] == "query_database"),
            "query_database must be advertised in the MCP tool catalog"
        );
        // Brain v3 PR-6 — the knowledge-diff / decision-ledger tool is advertised (the 10th tool).
        assert!(
            tools.iter().any(|t| t["name"] == "knowledge_diff"),
            "knowledge_diff must be advertised in the MCP tool catalog"
        );
        // Brain v3 audit Fix 3(b) — the document-outline tool is advertised (the 11th tool), and its
        // args must be MIRRORED between the MCP tools list and the agentic tool surface (documentId).
        let outline = tools
            .iter()
            .find(|t| t["name"] == "get_document_outline")
            .expect("get_document_outline must be advertised in the MCP tool catalog");
        assert_eq!(
            outline["inputSchema"]["required"][0], "documentId",
            "the MCP outline tool must advertise the documentId arg (parity with the tool surface)"
        );
    }

    /// A4 (RED-before-GREEN): the MCP catalog must steer callers toward `org_search` as a FALLBACK
    /// when `search_meetings`/`search_semantic` find nothing — and `org_search`'s own description
    /// must lead with that fallback framing, not present itself as an unrelated alternative.
    #[test]
    fn tool_catalog_nudges_org_search_as_a_fallback() {
        let r = rpc(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        let desc = |name: &str| -> String {
            tools
                .iter()
                .find(|t| t["name"] == name)
                .and_then(|t| t["description"].as_str())
                .unwrap_or_default()
                .to_string()
        };
        let search_meetings = desc("search_meetings");
        let search_semantic = desc("search_semantic");
        let org_search = desc("org_search");
        assert!(
            search_meetings.contains("org_search"),
            "search_meetings must mention org_search as a fallback: {search_meetings}"
        );
        assert!(
            search_semantic.contains("org_search"),
            "search_semantic must mention org_search as a fallback: {search_semantic}"
        );
        assert!(
            org_search.to_lowercase().starts_with("fallback"),
            "org_search's own description must LEAD with the fallback framing: {org_search}"
        );
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

    /// Flag ON: both the real-model hybrid route and its model-unavailable keyword fallback apply
    /// the SAME visibility gate as `search_meetings`. Unit tests deliberately take the fallback so
    /// a developer's installed Metal model is never loaded; the DB-level test exercises the vector
    /// gate directly. A sealed-and-not-unlocked meeting is excluded and reappears after unlock.
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

        // Seed deterministic vector rows directly. Holding an admitted active-model handle across
        // `dispatch_tool` would also hold the model-selection read barrier while AppConfig::load
        // republishes that selection, creating a same-thread read→write deadlock. MCP behavior is
        // what this test owns; real-model loading is covered by the embedder tests / Mac bake-off.
        let emb = crate::embed::StubEmbedder;
        db.index_meeting_chunks("open", &[], &emb).unwrap();
        db.index_meeting_chunks("sealed", &[], &emb)
            .unwrap();
        // Seal the folder AFTER indexing (a stray vec row now exists for a sealed meeting).
        db.set_folder_locked("f-lock", true, None).unwrap();

        let args = json!({ "query": "budget planning hiring quarter" });

        // Not unlocked → sealed meeting must NOT appear.
        let out = dispatch_tool(&db, "search_semantic", &args, &HashSet::new()).unwrap();
        assert!(
            out.contains("semantic model is not installed"),
            "unit tests must stay on the bounded model-free fallback: {out}"
        );
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

    /// Feature C: `query_database` is visibility-gated exactly like the other read tools (mirror of
    /// `get_open_commitments_is_visibility_gated`). A typed note in a sealed-and-not-unlocked
    /// note-folder is INVISIBLE to the query (its title + typed values never surface), and reappears
    /// once the folder is session-unlocked. RED if the tool bypasses `list_notes_visible_typed`.
    #[test]
    fn query_database_is_visibility_gated() {
        use crate::storage::models::{NoteFolder, PropertyKind, PropertySchemaField};
        let (db, p) = temp_db();
        // A LOCKED note-folder with a typed `status` schema and one typed note.
        db.insert_note_folder(
            &NoteFolder {
                id: "nf-lock".into(),
                name: "Secret Tasks".into(),
                path: "Notes/Secret Tasks".into(),
                parent_id: None,
                locked: false,
                unlocked: false,
                is_root: false,
                kind: "note".into(),
            },
            "2026-07-14T00:00:00Z",
        )
        .unwrap();
        db.set_note_folder_schema(
            "nf-lock",
            &[PropertySchemaField {
                key: "status".into(),
                kind: PropertyKind::Select,
                options: vec!["Open".into(), "Done".into()],
            }],
        )
        .unwrap();
        // Note title deliberately DISJOINT from the folder name so a substring test can't collide.
        db.insert_note(
            "n-secret",
            "nf-lock",
            "launch-plan",
            "Launch Plan",
            "---\nstatus: Open\n---\nbody",
            1_000,
        )
        .unwrap();
        db.set_folder_locked("nf-lock", true, Some(b"wrapped"))
            .unwrap();

        // Not unlocked → the sealed folder's typed row is invisible; the row's title never leaks.
        let args = json!({ "folder": "Secret Tasks", "filter": "status=Open" });
        let out = dispatch_tool(&db, "query_database", &args, &HashSet::new()).unwrap();
        assert!(
            !out.contains("Launch Plan"),
            "sealed-not-unlocked note-folder's typed row leaked (gate violation): {out}"
        );

        // Session-unlock → the typed row reappears in the query result.
        let mut unlocked = HashSet::new();
        unlocked.insert("nf-lock".to_string());
        let out2 = dispatch_tool(&db, "query_database", &args, &unlocked).unwrap();
        assert!(
            out2.contains("[[Launch Plan]]"),
            "unlocked typed row must reappear in query_database: {out2}"
        );

        // Missing required `folder` arg is an InvalidArg (JSON-RPC -32602), never a silent all-rows.
        let bad = dispatch_tool(
            &db,
            "query_database",
            &json!({ "filter": "x=y" }),
            &unlocked,
        );
        assert!(bad.is_err(), "query_database requires a folder argument");
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

    /// Brain v3 PR-6 (RED-before-GREEN gate): the MCP `knowledge_diff` dispatch is visibility-gated.
    /// A fact whose SOURCE meeting is in a sealed-and-not-session-unlocked folder must be ABSENT from
    /// the diff AND the decision ledger — its object never renders — and reappear once the folder is
    /// session-unlocked. Routes through the SAME gated reader (`list_facts_visible`) as the dossier.
    /// EGRESS-FREE: `dispatch_tool` builds gated structured text only, never a provider/cloud call.
    #[test]
    fn mcp_knowledge_diff_is_visibility_gated() {
        use crate::facts::{FactOp, NewFact};
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
        // OPEN meetings carry a supersession (Atlas.status in-progress → shipped) the ledger surfaces.
        seed(&db, "m_open1", "Kickoff", "Atlas kickoff\n", None);
        seed(&db, "m_open2", "Ship Review", "Atlas shipped\n", None);
        // SEALED meeting carries a fact whose OBJECT must never leak while the folder is sealed.
        seed(
            &db,
            "m_sealed",
            "Secret Atlas Review",
            "Atlas secret budget\n",
            Some("f-lock"),
        );
        let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
        db.add_mention(&atlas, "m_open1").unwrap();
        db.add_mention(&atlas, "m_open2").unwrap();
        db.add_mention(&atlas, "m_sealed").unwrap();

        let add = |predicate: &str, object: &str, vf: &str, meeting: &str| {
            FactOp::Add(NewFact {
                entity_id: atlas.clone(),
                subject: "Atlas".to_string(),
                predicate: predicate.to_string(),
                object: object.to_string(),
                valid_from: vf.to_string(),
                recorded_at: vf.to_string(),
                confidence: 1.0,
                meeting_id: Some(meeting.to_string()),
            })
        };
        // Seed Atlas.status = in-progress (open) on m_open1.
        db.apply_fact_ops(&[add(
            "status",
            "in-progress",
            "2026-06-01T00:00:00Z",
            "m_open1",
        )])
        .unwrap();
        // The reconcile-style change: close in-progress @2026-06-20 (Invalidate the minted row) and
        // open shipped on m_open2 — a real supersession, built exactly like `apply_fact_ops` does.
        let ip_id = db
            .facts_for_entities(std::slice::from_ref(&atlas))
            .unwrap()
            .into_iter()
            .find(|f| f.object == "in-progress")
            .expect("in-progress row exists")
            .id;
        db.apply_fact_ops(&[
            FactOp::Invalidate {
                id: ip_id,
                valid_to: "2026-06-20T00:00:00Z".to_string(),
            },
            add("status", "shipped", "2026-06-20T00:00:00Z", "m_open2"),
        ])
        .unwrap();
        // The SEALED-source fact whose object must never leak while the folder is sealed.
        db.apply_fact_ops(&[add(
            "budget",
            "SECRET-42M",
            "2026-06-15T00:00:00Z",
            "m_sealed",
        )])
        .unwrap();

        db.set_folder_locked("f-lock", true, None).unwrap();

        let args = json!({
            "entity": "Atlas",
            "from": "2026-06-10T00:00:00Z",
            "to": "2026-06-25T00:00:00Z"
        });

        // NOT unlocked: the open supersession (in-progress → shipped) renders; the SEALED fact's
        // object (SECRET-42M) and predicate (budget) never appear anywhere in the payload.
        let out = dispatch_tool(&db, "knowledge_diff", &args, &HashSet::new()).unwrap();
        assert!(
            out.contains("in-progress") && out.contains("shipped"),
            "the open-source supersession must render: {out}"
        );
        assert!(
            !out.contains("SECRET-42M") && !out.contains("budget"),
            "a sealed-not-unlocked meeting's fact leaked into the knowledge diff (gate violation): {out}"
        );

        // Session-unlock the folder → the sealed fact (budget = SECRET-42M) reappears.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let out2 = dispatch_tool(&db, "knowledge_diff", &args, &unlocked).unwrap();
        assert!(
            out2.contains("SECRET-42M"),
            "unlocked sealed fact must reappear in the diff: {out2}"
        );

        // Missing required args are InvalidArg (-32602), never a silent all-facts read.
        assert!(dispatch_tool(
            &db,
            "knowledge_diff",
            &json!({ "from": "x", "to": "y" }),
            &unlocked
        )
        .is_err());
        assert!(dispatch_tool(
            &db,
            "knowledge_diff",
            &json!({ "entity": "Atlas", "to": "y" }),
            &unlocked
        )
        .is_err());
        assert!(dispatch_tool(
            &db,
            "knowledge_diff",
            &json!({ "entity": "Atlas", "from": "x" }),
            &unlocked
        )
        .is_err());

        // Unknown entity → friendly non-leaking message (never an error). Uses VALID RFC3339
        // bounds so it reaches the entity-resolution path (B2 rejects malformed timestamps at the
        // dispatch boundary before the entity is ever resolved).
        let none = dispatch_tool(
            &db,
            "knowledge_diff",
            &json!({ "entity": "Nonexistent", "from": "2026-06-10T00:00:00Z", "to": "2026-06-25T00:00:00Z" }),
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            none.contains("No visible entity"),
            "unknown entity → friendly message"
        );

        let _ = std::fs::remove_file(&p);
    }

    /// B2 (RED-before-GREEN) — the MCP `knowledge_diff` dispatch validates that BOTH `from` and `to`
    /// parse as RFC3339. An unparseable `from` used to pass through (`normalize_instant` returns it
    /// UNCHANGED, `cmp_instant` compares lexically), SWAP the range, and yield a confident but wrong
    /// "0 changes" with NO error. Now it is a -32602 that names the offending argument; a well-formed
    /// pair proceeds past validation. RED on the pre-fix code (which returned Ok with an empty window).
    #[test]
    fn mcp_knowledge_diff_rejects_unparseable_timestamp() {
        let (db, p) = temp_db();

        // A garbage `from` (valid `to`) → -32602 naming `from`, never a silent "0 changes".
        let err = dispatch_tool(
            &db,
            "knowledge_diff",
            &json!({ "entity": "Atlas", "from": "not-a-date", "to": "2026-06-25T00:00:00Z" }),
            &HashSet::new(),
        )
        .unwrap_err();
        assert_eq!(err.0, -32602, "malformed timestamp must be InvalidArg: {err:?}");
        assert!(
            err.1.contains("from"),
            "the error must name the offending argument (from): {}",
            err.1
        );

        // Well-formed RFC3339 bounds do NOT error at the validation step — dispatch proceeds to
        // `execute_tool` (an unknown entity there is a friendly Ok message, not a validation error).
        let ok = dispatch_tool(
            &db,
            "knowledge_diff",
            &json!({ "entity": "Atlas", "from": "2026-06-10T00:00:00Z", "to": "2026-06-25T00:00:00Z" }),
            &HashSet::new(),
        );
        assert!(
            ok.is_ok(),
            "valid RFC3339 bounds must pass the dispatch validation: {ok:?}"
        );

        let _ = std::fs::remove_file(&p);
    }

    /// Feature D: the MCP `get_document` dispatch is visibility-gated — a document in a
    /// sealed-and-not-session-unlocked folder returns the masked "No data" sentinel, and reappears
    /// once the folder is session-unlocked. Mirrors the other MCP gate tests but through the real
    /// `dispatch_tool` (JSON args → `ToolCall::GetDocument` → gated `execute_tool`).
    #[test]
    fn mcp_get_document_is_visibility_gated() {
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
        db.insert_note(
            "note-1",
            "f-lock",
            "note-name",
            "Secret Note",
            "the classified body text",
            1_700_000_000,
        )
        .unwrap();
        db.set_folder_locked("f-lock", true, None).unwrap();

        let args = json!({ "documentId": "note-1" });
        // Locked, not unlocked → masked sentinel, no body/title.
        let out = dispatch_tool(&db, "get_document", &args, &HashSet::new()).unwrap();
        assert_eq!(out, "No data for document note-1.");
        assert!(
            !out.contains("classified"),
            "sealed document body leaked via MCP get_document"
        );

        // Session-unlock → body reappears.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let out2 = dispatch_tool(&db, "get_document", &args, &unlocked).unwrap();
        assert!(
            out2.contains("the classified body text"),
            "unlocked body must reappear: {out2}"
        );
        assert!(
            out2.contains("TITLE: [[Secret Note]]"),
            "title must render: {out2}"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// Brain v3 audit Fix 2 — the MCP `get_document` DEFAULT (no paging args) no longer floods the
    /// client with the whole body: it returns a BOUNDED window (`MCP_DEFAULT_WINDOW_CHARS`) plus a
    /// `TOTAL_CHARS: …` disclosure so the client can see the true length and page the rest. An
    /// explicit larger `maxChars` is honored verbatim. RED on the pre-fix `(0,0)`-returns-everything.
    #[test]
    fn mcp_get_document_default_window_is_bounded_and_disclosed() {
        use crate::storage::models::Folder;
        let (db, p) = temp_db();
        db.insert_folder(&Folder {
            id: "f-open".to_string(),
            name: "Docs".to_string(),
            path: "Docs".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
        // A body LARGER than the default window so the flood is measurable.
        let big = "A".repeat(MCP_DEFAULT_WINDOW_CHARS + 5000);
        db.insert_document(
            "bigdoc",
            "f-open",
            "big.md",
            &big,
            "document",
            1_700_000_000,
        )
        .unwrap();

        // Default (no paging) → bounded to MCP_DEFAULT_WINDOW_CHARS + a disclosure header showing
        // the TRUE total; NOT the whole 11000-char body.
        let out = dispatch_tool(
            &db,
            "get_document",
            &json!({ "documentId": "bigdoc" }),
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            out.contains(&format!(
                "BODY (TOTAL_CHARS: {} (showing 0..{}))",
                big.chars().count(),
                MCP_DEFAULT_WINDOW_CHARS
            )),
            "the MCP default must disclose the true total + the bounded window: {}",
            &out[..out.len().min(120)]
        );
        // The returned body is bounded (window + headers/title), NOT the full 11000-char body.
        assert!(
            out.len() < big.len(),
            "the default MCP window must NOT return the whole body (flood): got {} vs body {}",
            out.len(),
            big.len()
        );
        assert!(
            !out.contains("[end of content]"),
            "the bounded default window does NOT reach the end of a body larger than the window: {}",
            &out[out.len().saturating_sub(60)..]
        );

        // Explicit larger maxChars is honored (the client CAN ask for more).
        let full = dispatch_tool(
            &db,
            "get_document",
            &json!({ "documentId": "bigdoc", "offset": 0, "maxChars": big.chars().count() + 10 }),
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            full.contains("[end of content]"),
            "an explicit full window reaches the end: last 60 = {}",
            &full[full.len().saturating_sub(60)..]
        );
        assert!(
            full.len() > out.len(),
            "explicit large maxChars returns more than the default window"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// Brain v3 audit Fix 3(b) — the MCP `get_document_outline` dispatch is visibility-gated: a
    /// document in a sealed-and-not-session-unlocked folder returns the "no outline" sentinel (no
    /// heading leak), and the heading map reappears once the folder is session-unlocked. Proves the
    /// NEW gated read routes through `execute_tool` → `get_document_outline_if_visible`.
    #[test]
    fn mcp_get_document_outline_is_visibility_gated() {
        use crate::storage::models::Folder;
        let (db, p) = temp_db();
        db.insert_folder(&Folder {
            id: "f-lock".to_string(),
            name: "Specs".to_string(),
            path: "Specs".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
        // A two-section doc → two L1 headings in the outline.
        let blocks = vec![
            crate::extract::ExtractedBlock {
                text: "Confidential design of the vault store.".to_string(),
                page: Some(1),
                heading_path: Some("SecretDesign".to_string()),
            },
            crate::extract::ExtractedBlock {
                text: "The keys are wrapped by the master KEK.".to_string(),
                page: Some(2),
                heading_path: Some("SecretDesign › Keys".to_string()),
            },
        ];
        let stored = crate::extract::blocks_to_stored_text(&blocks);
        db.insert_document(
            "od1",
            "f-lock",
            "spec.pdf",
            &stored,
            "document",
            1_700_000_000,
        )
        .unwrap();
        db.index_document_chunks("od1", None).unwrap();
        db.set_folder_locked("f-lock", true, None).unwrap();

        let args = json!({ "documentId": "od1" });
        // Locked, not unlocked → the "no outline" sentinel; the heading trail must NOT leak.
        let out = dispatch_tool(&db, "get_document_outline", &args, &HashSet::new()).unwrap();
        assert!(
            out.contains("No outline for document od1"),
            "sealed → sentinel: {out}"
        );
        assert!(
            !out.contains("SecretDesign"),
            "sealed document headings leaked via MCP outline: {out}"
        );

        // Session-unlock → the heading map reappears in document order.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let out2 = dispatch_tool(&db, "get_document_outline", &args, &unlocked).unwrap();
        assert!(
            out2.contains("SecretDesign (p.1)"),
            "unlocked outline lists section + page: {out2}"
        );
        assert!(
            out2.contains("SecretDesign › Keys (p.2)"),
            "document order preserved: {out2}"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// Feature D: the MCP `get_meeting` dispatch defaults to the STRUCTURED transcript, and honors an
    /// explicit `transcriptFormat: "plain"` for the legacy flat text.
    #[test]
    fn mcp_get_meeting_transcript_format_switches() {
        use crate::storage::models::Meeting;
        use crate::transcribe::types::Segment;
        let (db, p) = temp_db();
        // Titleless meeting → no TITLE prefix; a note so get_meeting returns content.
        db.insert_meeting(&Meeting {
            id: "mm".to_string(),
            started_at: "2026-06-27T09:00:00Z".to_string(),
            ended_at: None,
            title: None,
            duration_s: 60,
            audio_path: None,
            status: crate::storage::models::MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&crate::storage::models::NoteRecord {
            meeting_id: "mm".to_string(),
            provider_id: "claude_code".to_string(),
            markdown: "n".to_string(),
            created_at: "2026-06-27T09:05:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.insert_segments(
            "mm",
            &[Segment {
                idx: 0,
                start_s: 5.0,
                end_s: 8.0,
                text: "opening remarks".into(),
                speaker: Some("me".into()),
                confidence: None,
            }],
        )
        .unwrap();

        // Default (no transcriptFormat) → STRUCTURED (speaker label + timestamp token). Audit
        // Fix 2: the MCP default now bounds+discloses the transcript window, so the section header
        // carries a `TOTAL_CHARS: …` disclosure (the whole short transcript fits the 6000 window).
        let def = dispatch_tool(
            &db,
            "get_meeting",
            &json!({ "meetingId": "mm" }),
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            def.contains("Me: opening remarks"),
            "default must be structured: {def}"
        );
        assert!(
            def.contains("[5–8]"),
            "default must carry a timestamp token: {def}"
        );
        assert!(
            def.contains("TRANSCRIPT (TOTAL_CHARS:"),
            "the MCP default now discloses the transcript window total: {def}"
        );

        // Explicit plain → the legacy flat text (no speaker label, no timestamp). Audit Fix 2: with
        // NO paging args the MCP default now applies the bounded+disclosed window, so the short
        // transcript is fully returned WITH its `TOTAL_CHARS` header + the end-of-content marker.
        let plain = dispatch_tool(
            &db,
            "get_meeting",
            &json!({ "meetingId": "mm", "transcriptFormat": "plain" }),
            &HashSet::new(),
        )
        .unwrap();
        assert_eq!(
            plain,
            "NOTE:\nn\n\nTRANSCRIPT (TOTAL_CHARS: 15 (showing 0..15)):\nopening remarks\n[end of content]"
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
