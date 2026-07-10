//! Brain v2 L5 — a HAND-ROLLED MCP client: JSON-RPC 2.0 over HTTP and stdio. NO new crates (the
//! architect decision): HTTP rides the existing `reqwest` client (20s timeout via
//! [`super::http_client`]); stdio rides `tokio::process` with its own wall-clock timeout.
//!
//! Scope (v1, per the spec): `initialize` / `tools/list` / `tools/call`. SSE / streamable-HTTP
//! session headers are DEFERRED — a plain JSON body is expected back; a server that answers with
//! an event stream fails gracefully as `ConnectorError::Failed`.
//!
//! ## Security posture
//! - A stdio server is ARBITRARY CODE EXECUTION: the command path must be ABSOLUTE (validated at
//!   `add_mcp_server` AND re-checked here at spawn — defense-in-depth), the child is
//!   `kill_on_drop`, and the whole session is wall-clock bounded ([`STDIO_TIMEOUT`]).
//! - Server-supplied strings (tool names, descriptions, results) are UNTRUSTED INPUT. This module
//!   only parses + returns them; callers ([`super::mcp::McpConnector`], `tools.rs`) must never
//!   interpolate server-supplied metadata into system prompts (results are treated as DATA by the
//!   agentic loop's existing contract, and truncated by `RESULT_BUDGET`).
//! - NO PII in logs: endpoint transport + status codes + counts only — never the query text, the
//!   tool result, or server metadata.

use serde_json::{json, Value};

use super::ConnectorError;

/// Wall-clock bound on one stdio MCP session (spawn → handshake → request → response). Matches
/// the HTTP client's 20s request timeout so neither transport can wedge the tool worker.
const STDIO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Cap on stdout bytes read from a stdio server in one session (a runaway/hostile server cannot
/// balloon memory).
const STDIO_MAX_OUTPUT: usize = 1 << 20; // 1 MiB

/// Hard cap on an HTTP MCP response BODY, in bytes, enforced while STREAMING the body — before any
/// parse (L5 follow-up, 2026-07-10): a hostile/runaway HTTP server cannot balloon memory with an
/// unbounded reply. An over-cap body degrades to a synthetic truncated-with-note tool RESULT (see
/// [`parse_http_body`]), never an unbounded buffer. Mirrors [`STDIO_MAX_OUTPUT`].
const HTTP_MAX_BODY: usize = 1 << 20; // 1 MiB

/// The protocol version this client advertises in `initialize`.
const MCP_PROTOCOL_VERSION: &str = "2025-03-26";

/// One discovered tool (name + description). Both fields are SERVER-SUPPLIED and untrusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
}

/// The transport one [`McpClient`] speaks.
#[derive(Debug, Clone)]
pub enum McpTransport {
    /// JSON-RPC 2.0 POSTed to an HTTP endpoint (plain JSON response body).
    Http { endpoint: String },
    /// JSON-RPC 2.0 over a spawned local process's stdin/stdout (newline-delimited messages).
    Stdio { command: String, args: Vec<String> },
}

/// A minimal MCP client for ONE configured server.
pub struct McpClient {
    transport: McpTransport,
}

impl McpClient {
    pub fn new(transport: McpTransport) -> Self {
        Self { transport }
    }

    /// Build a client from a configured server row, validating the transport shape (fail-closed
    /// `None` on an unknown transport / a non-absolute stdio path / a non-http(s) endpoint).
    pub fn for_server(server: &crate::storage::models::McpServer) -> Option<Self> {
        match server.transport.as_str() {
            "http"
                if server.endpoint.starts_with("http://")
                    || server.endpoint.starts_with("https://") =>
            {
                Some(Self::new(McpTransport::Http {
                    endpoint: server.endpoint.clone(),
                }))
            }
            "stdio" if std::path::Path::new(&server.endpoint).is_absolute() => {
                Some(Self::new(McpTransport::Stdio {
                    command: server.endpoint.clone(),
                    args: server.args.clone(),
                }))
            }
            _ => None,
        }
    }

    /// JSON-RPC `initialize` — proves the endpoint speaks MCP. (For stdio, every session performs
    /// its own handshake internally; this runs one explicitly so `test_mcp_server` can report a
    /// clean pass/fail.)
    pub async fn initialize(&self) -> Result<Value, ConnectorError> {
        self.rpc("initialize", initialize_params()).await
    }

    /// JSON-RPC `tools/list` — the discovered tool catalog (names + descriptions; UNTRUSTED).
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, ConnectorError> {
        let result = self.rpc("tools/list", json!({})).await?;
        Ok(parse_tools(&result))
    }

    /// JSON-RPC `tools/call` — run one tool, returning its concatenated text content.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, ConnectorError> {
        let result = self
            .rpc("tools/call", json!({ "name": name, "arguments": arguments }))
            .await?;
        Ok(parse_tool_result_text(&result))
    }

    /// Dispatch ONE JSON-RPC request over the configured transport, returning the `result` value.
    async fn rpc(&self, method: &str, params: Value) -> Result<Value, ConnectorError> {
        match &self.transport {
            McpTransport::Http { endpoint } => http_rpc(endpoint, method, params).await,
            McpTransport::Stdio { command, args } => {
                stdio_rpc(command, args, method, params).await
            }
        }
    }
}

/// The `initialize` params this client sends (static, content-free).
fn initialize_params() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": "murmur", "version": env!("CARGO_PKG_VERSION") }
    })
}

/// Wrap `(method, params)` into a JSON-RPC 2.0 request with `id`.
fn jsonrpc_request(id: u64, method: &str, params: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// Extract the `result` from a JSON-RPC response value; a JSON-RPC `error` (or a missing result)
/// becomes `ConnectorError::Failed` carrying the error CODE only (server messages are untrusted
/// and stay out of our error strings/logs).
fn jsonrpc_result(response: &Value) -> Result<Value, ConnectorError> {
    if let Some(err) = response.get("error") {
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
        return Err(ConnectorError::Failed(format!("mcp JSON-RPC error {code}")));
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| ConnectorError::Failed("mcp response carries no result".into()))
}

/// Parse a `tools/list` result into the tool catalog (missing/malformed entries are skipped).
pub(crate) fn parse_tools(result: &Value) -> Vec<McpToolInfo> {
    result
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| {
                    let name = t.get("name")?.as_str()?.trim().to_string();
                    if name.is_empty() {
                        return None;
                    }
                    let description = t
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    Some(McpToolInfo { name, description })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a `tools/call` result into its concatenated `content[].text` payload; a result with no
/// text content degrades to the compact JSON of the result (so a structured-only reply is still
/// usable DATA for the loop).
pub(crate) fn parse_tool_result_text(result: &Value) -> String {
    let texts: Vec<&str> = result
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|i| i.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|i| i.get("text").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    if texts.is_empty() {
        return result.to_string();
    }
    texts.join("\n")
}

/// HTTP transport: POST the JSON-RPC request, expect a plain JSON response body. Uses the shared
/// bounded [`super::http_client`] (20s). SSE responses are NOT parsed (deferred) — they fail as a
/// body-parse error.
async fn http_rpc(endpoint: &str, method: &str, params: Value) -> Result<Value, ConnectorError> {
    let client = super::http_client();
    let resp = client
        .post(endpoint)
        .header("Accept", "application/json")
        .json(&jsonrpc_request(1, method, &params))
        .send()
        .await
        .map_err(|e| ConnectorError::Failed(format!("mcp http request: {}", e.without_url())))?;
    let status = resp.status();
    if !status.is_success() {
        tracing::warn!(target: "connector", provider = "mcp", status = status.as_u16(), "mcp http error");
        return Err(ConnectorError::Failed(format!("mcp HTTP {status}")));
    }
    // Stream the body under the hard [`HTTP_MAX_BODY`] cap: stop reading the moment the next chunk
    // would exceed it (memory is bounded at cap + one network chunk, and the connection is dropped
    // with the response). The old `resp.json()` buffered an UNBOUNDED body before parsing.
    let mut resp = resp;
    let mut body: Vec<u8> = Vec::new();
    let mut over_cap = false;
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if body.len().saturating_add(chunk.len()) > HTTP_MAX_BODY {
                    over_cap = true;
                    break;
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => {
                return Err(ConnectorError::Failed(format!(
                    "mcp body read: {}",
                    e.without_url()
                )))
            }
        }
    }
    parse_http_body(&body, over_cap)
}

/// PURE cap-then-parse of a (bounded) HTTP JSON-RPC body. `over_cap` — or a body that somehow
/// still exceeds [`HTTP_MAX_BODY`] — degrades to a synthetic truncated-with-note tool RESULT
/// (`content[].text`), so the agent loop sees a harmless, loud data note instead of either an
/// unbounded buffer or a hard failure; a bounded body parses as the normal JSON-RPC envelope.
/// Logs the cap only — NEVER body content.
pub(crate) fn parse_http_body(body: &[u8], over_cap: bool) -> Result<Value, ConnectorError> {
    if over_cap || body.len() > HTTP_MAX_BODY {
        tracing::warn!(target: "connector", provider = "mcp", cap_bytes = HTTP_MAX_BODY, "mcp http body over cap; degrading to a truncated note");
        return Ok(json!({
            "content": [{
                "type": "text",
                "text": "[MCP response truncated: the server reply exceeded the 1 MiB body cap]"
            }]
        }));
    }
    let envelope: Value = serde_json::from_slice(body)
        .map_err(|e| ConnectorError::Failed(format!("mcp body parse: {e}")))?;
    jsonrpc_result(&envelope)
}

/// stdio transport: spawn the (ABSOLUTE-path, re-validated) command, run the MCP handshake
/// (`initialize` → `notifications/initialized`), send the request, and read newline-delimited
/// JSON messages off stdout until the matching response id arrives — the whole session bounded by
/// [`STDIO_TIMEOUT`] and [`STDIO_MAX_OUTPUT`]. The child is killed on drop (no orphan servers).
async fn stdio_rpc(
    command: &str,
    args: &[String],
    method: &str,
    params: Value,
) -> Result<Value, ConnectorError> {
    // Defense-in-depth: NEVER spawn a relative path (a $PATH lookup), even if a row slipped past
    // the command-layer validation.
    if !std::path::Path::new(command).is_absolute() {
        return Err(ConnectorError::Unconfigured(
            "stdio MCP command must be an absolute path".into(),
        ));
    }
    let fut = stdio_session(command, args, method, params);
    match tokio::time::timeout(STDIO_TIMEOUT, fut).await {
        Ok(res) => res,
        Err(_) => Err(ConnectorError::Failed("mcp stdio timeout".into())),
    }
}

/// One bounded stdio session (see [`stdio_rpc`] — the caller owns the timeout).
async fn stdio_session(
    command: &str,
    args: &[String],
    method: &str,
    params: Value,
) -> Result<Value, ConnectorError> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut child = tokio::process::Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| ConnectorError::Failed(format!("mcp stdio spawn: {e}")))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| ConnectorError::Failed("mcp stdio: no stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ConnectorError::Failed("mcp stdio: no stdout".into()))?;
    let mut lines = BufReader::new(stdout).lines();

    // MCP handshake: initialize (id 1) → wait for its response → notifications/initialized.
    let init = jsonrpc_request(1, "initialize", &initialize_params());
    write_line(&mut stdin, &init).await?;
    read_response(&mut lines, 1).await?;
    let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    write_line(&mut stdin, &initialized).await?;

    // The actual request (id 2).
    let req = jsonrpc_request(2, method, &params);
    write_line(&mut stdin, &req).await?;
    let response = read_response(&mut lines, 2).await?;
    // Session over — the child dies via kill_on_drop.
    jsonrpc_result(&response)
}

/// Write one newline-delimited JSON message.
async fn write_line(
    stdin: &mut tokio::process::ChildStdin,
    msg: &Value,
) -> Result<(), ConnectorError> {
    use tokio::io::AsyncWriteExt;
    let mut line = msg.to_string();
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| ConnectorError::Failed(format!("mcp stdio write: {e}")))
}

/// Read newline-delimited JSON messages until the response with `id` arrives (server-initiated
/// notifications/requests are skipped), bounded by [`STDIO_MAX_OUTPUT`] total bytes.
async fn read_response(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    id: u64,
) -> Result<Value, ConnectorError> {
    let mut read_bytes = 0usize;
    loop {
        let line = lines
            .next_line()
            .await
            .map_err(|e| ConnectorError::Failed(format!("mcp stdio read: {e}")))?
            .ok_or_else(|| ConnectorError::Failed("mcp stdio closed before responding".into()))?;
        read_bytes = read_bytes.saturating_add(line.len());
        if read_bytes > STDIO_MAX_OUTPUT {
            return Err(ConnectorError::Failed("mcp stdio output over budget".into()));
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue; // non-JSON noise on stdout — skip.
        };
        if msg.get("id").and_then(Value::as_u64) == Some(id) {
            return Ok(msg);
        }
        // A different id / a notification — keep reading.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tools_maps_catalog_and_skips_malformed() {
        let result = json!({
            "tools": [
                { "name": "search_docs", "description": "Search the docs" },
                { "name": "  ", "description": "blank name skipped" },
                { "description": "no name skipped" },
                { "name": "fetch" }
            ]
        });
        let tools = parse_tools(&result);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "search_docs");
        assert_eq!(tools[0].description, "Search the docs");
        assert_eq!(tools[1].name, "fetch");
        assert_eq!(tools[1].description, "");
        // Missing/malformed tools array → empty.
        assert!(parse_tools(&json!({})).is_empty());
        assert!(parse_tools(&json!({ "tools": "nope" })).is_empty());
    }

    #[test]
    fn parse_tool_result_concatenates_text_content() {
        let result = json!({
            "content": [
                { "type": "text", "text": "line one" },
                { "type": "image", "data": "…ignored…" },
                { "type": "text", "text": "line two" }
            ]
        });
        assert_eq!(parse_tool_result_text(&result), "line one\nline two");
        // No text content → the compact JSON of the result (structured replies stay usable).
        let structured = json!({ "structuredContent": { "answer": 42 } });
        assert_eq!(parse_tool_result_text(&structured), structured.to_string());
    }

    #[test]
    fn jsonrpc_result_maps_errors_without_leaking_messages() {
        let err = json!({ "jsonrpc": "2.0", "id": 1,
            "error": { "code": -32601, "message": "SECRET-INTERNAL-DETAIL" } });
        match jsonrpc_result(&err) {
            Err(ConnectorError::Failed(m)) => {
                assert!(m.contains("-32601"), "carries the code: {m}");
                assert!(
                    !m.contains("SECRET-INTERNAL-DETAIL"),
                    "server-supplied message must NOT ride our error strings: {m}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        let ok = json!({ "jsonrpc": "2.0", "id": 1, "result": { "tools": [] } });
        assert_eq!(jsonrpc_result(&ok).unwrap(), json!({ "tools": [] }));
        assert!(jsonrpc_result(&json!({ "jsonrpc": "2.0", "id": 1 })).is_err());
    }

    #[test]
    fn for_server_fail_closed_on_bad_transport_shapes() {
        let base = crate::storage::models::McpServer {
            id: "abc123".into(),
            label: "Docs".into(),
            transport: "http".into(),
            endpoint: "https://127.0.0.1:9999/mcp".into(),
            args: vec![],
            enabled: true,
            consented: true,
            created_at: "2026-07-10T00:00:00Z".into(),
        };
        assert!(McpClient::for_server(&base).is_some());
        // http transport with a non-http endpoint → None.
        let bad = crate::storage::models::McpServer {
            endpoint: "ftp://x".into(),
            ..base.clone()
        };
        assert!(McpClient::for_server(&bad).is_none());
        // stdio with a RELATIVE path → None (code execution needs an absolute path).
        let rel = crate::storage::models::McpServer {
            transport: "stdio".into(),
            endpoint: "some-binary".into(),
            ..base.clone()
        };
        assert!(McpClient::for_server(&rel).is_none());
        // stdio with an absolute path → Some.
        let abs = crate::storage::models::McpServer {
            transport: "stdio".into(),
            endpoint: "/usr/local/bin/mcp-server".into(),
            ..base.clone()
        };
        assert!(McpClient::for_server(&abs).is_some());
        // Unknown transport → None.
        let unk = crate::storage::models::McpServer {
            transport: "sse".into(),
            ..base
        };
        assert!(McpClient::for_server(&unk).is_none());
    }

    /// L5 follow-up (R4) — the HTTP body cap, tested on the PURE cap-then-parse core (no network):
    /// an over-cap body (by flag or by length) degrades to the truncated-with-note tool result and
    /// NEVER attempts to parse/hold the oversized bytes; a bounded body parses normally.
    #[test]
    fn http_body_over_cap_degrades_to_truncated_note() {
        // Synthetic oversized body: one byte past the cap.
        let oversized = vec![b'x'; HTTP_MAX_BODY + 1];
        let v = parse_http_body(&oversized, false).unwrap();
        let text = parse_tool_result_text(&v);
        assert!(
            text.contains("truncated") && text.contains("1 MiB"),
            "over-cap body becomes a loud truncated note: {text}"
        );
        assert!(
            !text.contains("xxx"),
            "no oversized content survives into the result"
        );

        // The streaming reader signals over_cap with a PARTIAL buffer — same degradation.
        let v = parse_http_body(b"{\"partial\":", true).unwrap();
        assert!(parse_tool_result_text(&v).contains("truncated"));

        // A bounded, valid envelope parses as the normal JSON-RPC result.
        let ok = br#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"hi"}]}}"#;
        let v = parse_http_body(ok, false).unwrap();
        assert_eq!(parse_tool_result_text(&v), "hi");

        // A bounded but malformed body is still a parse failure (unchanged posture).
        assert!(matches!(
            parse_http_body(b"not json", false),
            Err(ConnectorError::Failed(_))
        ));
    }

    /// A relative stdio command is refused at the rpc layer too (defense-in-depth), WITHOUT any
    /// spawn attempt.
    #[test]
    fn stdio_rpc_refuses_relative_paths() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let res = rt.block_on(stdio_rpc("relative-bin", &[], "tools/list", json!({})));
        assert!(matches!(res, Err(ConnectorError::Unconfigured(_))));
    }
}
