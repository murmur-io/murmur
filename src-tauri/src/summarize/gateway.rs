//! OpenAI-compatible "AI Gateway" provider + URL guardrails.
//!
//! Security invariants:
//!   R1 — the provider is cloud-classified and consent-gated even on loopback (a local gateway
//!         can forward to the cloud). Consent gate is enforced by `make_provider`, not here.
//!   R2 — the provider is always wrapped in `RedactingProvider` (enforced by `make_provider`).
//!   R3 — the API key is the GATEWAY key only; it NEVER falls back to the Anthropic key.
//!   R4 — http:// is rejected for non-loopback URLs; validated at construction in `new()`.
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use crate::error::{AppError, Result};
use crate::summarize::meta::CallMeta;
use crate::summarize::provider::*;
use crate::summarize::template;

/// True iff the URL host is loopback — the ONLY case where plain `http://` is allowed and (for
/// `ollama`) the only case treated as non-cloud. A loopback gateway can still FORWARD to the cloud,
/// so loopback is NOT a redaction exemption for the future `gateway` provider — only for `ollama`.
pub fn host_is_loopback(url: &reqwest::Url) -> bool {
    match url.host_str() {
        None => false,
        Some(h) => {
            if h.eq_ignore_ascii_case("localhost") {
                return true;
            }
            let h = h.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(h);
            h.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
        }
    }
}

/// Validate a user-supplied gateway base URL (guardrails R1/R4): https required, except http on
/// loopback; reject every other scheme (no file:/ftp:/gopher: SSRF surface). Also rejects
/// embedded credentials (`https://key:@host/v1`) — they would leak through reqwest error Display;
/// the API key belongs in the dedicated keychain field instead.
pub fn validate_gateway_url(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw.trim())
        .map_err(|_| AppError::InvalidArg("gateway URL is not a valid URL".into()))?;
    // Reject embedded credentials before the scheme check — they would leak through reqwest error
    // Display strings and are never the right way to supply an API key.
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AppError::InvalidArg(
            "gateway URL must not embed credentials — set the API key in the key field".into(),
        ));
    }
    match url.scheme() {
        "https" => Ok(url),
        "http" if host_is_loopback(&url) => Ok(url),
        "http" => Err(AppError::InvalidArg(
            "gateway URL must use https:// (http:// is only allowed for localhost)".into(),
        )),
        other => Err(AppError::InvalidArg(format!("unsupported gateway URL scheme: {other}"))),
    }
}

/// Build the canonical `application/json` body for an OpenAI `/v1/chat/completions` request.
/// `stream: false` is always set (streaming is Phase 9).
pub(crate) fn chat_body(model: &str, system: &str, user: &str) -> Value {
    serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user",   "content": user},
        ],
        "stream": false,
    })
}

/// Build a `/v1/chat/completions` body with `response_format: json_schema` so the gateway
/// applies native constrained decoding (Task 8.2). The `strict: true` flag asks the model
/// to guarantee the output matches `schema`.
pub(crate) fn chat_body_json(model: &str, system: &str, user: &str, schema: &Value) -> Value {
    serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user",   "content": user},
        ],
        "stream": false,
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "result",
                "schema": schema,
                "strict": true
            }
        }
    })
}

/// Minimal mirror of the OpenAI `/v1/chat/completions` success response.
#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    #[serde(default)]
    choices: Vec<ChatChoice>,
    /// Token-usage breakdown — `None` if the gateway did not include it.
    #[serde(default)]
    usage: Option<OpenAiUsage>,
    /// Model id as reported in the response — may differ from the requested model.
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    #[serde(default)]
    message: Option<ChatMessage>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: Option<String>,
}

/// Token-usage block returned by OpenAI-compatible APIs.
#[derive(Debug, Deserialize, Default)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: Option<u32>,
    #[serde(default)]
    completion_tokens: Option<u32>,
    #[serde(default)]
    total_tokens: Option<u32>,
    /// Nested block containing the prompt-cache hit count.
    #[serde(default)]
    prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAiPromptTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u32>,
}

/// Parse a raw OpenAI-compatible `/v1/chat/completions` response body into `(text, CallMeta)`.
///
/// `CallMeta` is populated from `usage.{prompt_tokens,completion_tokens,total_tokens}`,
/// `usage.prompt_tokens_details.cached_tokens`, and the top-level `model`. Any absent field
/// degrades to `None` — a response without `usage`/`model` still parses successfully.
pub(crate) fn parse_chat_response(body: &str) -> Result<(String, CallMeta)> {
    let parsed: ChatCompletionResponse = serde_json::from_str(body)
        .map_err(|e| AppError::Summarize(format!("failed to parse gateway response: {e}")))?;

    let text = parsed
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message)
        .and_then(|m| m.content)
        .ok_or_else(|| {
            AppError::Summarize(
                "gateway response missing choices[0].message.content".to_string(),
            )
        })?;

    let note = text.trim_start_matches('\u{feff}').trim();
    if note.is_empty() {
        return Err(AppError::Summarize(
            "gateway response contained no text content".to_string(),
        ));
    }

    let meta = CallMeta {
        model_served: parsed.model.filter(|m| !m.is_empty()),
        prompt_tokens: parsed.usage.as_ref().and_then(|u| u.prompt_tokens),
        completion_tokens: parsed.usage.as_ref().and_then(|u| u.completion_tokens),
        total_tokens: parsed.usage.as_ref().and_then(|u| u.total_tokens),
        cached_tokens: parsed
            .usage
            .as_ref()
            .and_then(|u| u.prompt_tokens_details.as_ref())
            .and_then(|d| d.cached_tokens),
    };

    Ok((note.to_string(), meta))
}

/// Extract a useful error message from an OpenAI-format error envelope, or return `None` if the
/// body is not parseable or does not have `error.message`.
fn extract_gateway_error(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    v.get("error")?.get("message")?.as_str().map(str::to_string)
}

/// Map an HTTP error status + raw response body to a structured, actionable `AppError`.
///
/// Parses the OpenAI error envelope `{"error":{"message":"…","type":"…","code":"…"}}` (best-effort
/// — body may not be JSON). Branches on status:
///   401/403 → `Unavailable` (bad key, actionable message)
///   404     → `Summarize` with the envelope message (e.g. "model 'x' not found")
///   429     → `Unavailable` (rate-limited / budget exceeded)
///   5xx     → `Summarize` with "gateway error {status}: {detail}"
///   else    → `Summarize` with the envelope message or "HTTP {status}"
///
/// NEVER includes the API key in the message (R3).
pub(crate) fn map_gateway_error(status: u16, body: &str) -> AppError {
    let detail = extract_gateway_error(body).unwrap_or_else(|| format!("HTTP {status}"));
    match status {
        401 | 403 => AppError::Unavailable(
            "gateway rejected the API key (check the key for this endpoint)".into(),
        ),
        429 => AppError::Unavailable("gateway rate-limited or out of budget".into()),
        500..=599 => AppError::Summarize(format!("gateway error {status}: {}", detail.trim())),
        _ => AppError::Summarize(format!("gateway: {}", detail.trim())),
    }
}

/// Minimal mirror of the OpenAI `/v1/models` response — only `data[].id` is extracted.
/// A missing or unrecognised body (non-OpenAI gateway) degrades to an empty list, never a panic.
#[derive(Debug, Deserialize, Default)]
struct ModelsListResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
}

/// Parse a raw OpenAI-compatible `/v1/models` response body into a `Vec<String>` of model ids.
///
/// A malformed or empty body returns `Ok(vec![])` — the caller (the FE picker) handles an empty
/// list gracefully. This is inbound-only metadata: NO meeting content is sent to produce this.
pub(crate) fn parse_models_response(body: &str) -> Result<Vec<String>> {
    let parsed: ModelsListResponse = serde_json::from_str(body).unwrap_or_default();
    Ok(parsed.data.into_iter().map(|m| m.id).collect())
}

/// An OpenAI-compatible AI Gateway provider (LiteLLM / Kong / Portkey / vLLM / local LiteLLM /
/// LM Studio / …). Talks to `{base}/chat/completions` with an optional `Authorization: Bearer`
/// header.
///
/// Always cloud-classified — even a localhost gateway can forward to the cloud — so it is:
///   • consent-gated by `make_provider` before construction,
///   • redaction-wrapped by `make_provider` after construction (R2).
pub struct OpenAiCompatProvider {
    /// Validated base URL with a trailing `/` ENSURED so that `join("chat/completions")` always
    /// appends rather than replacing the last path segment (RFC 3986: a join on a base WITHOUT a
    /// trailing slash replaces the final segment — `"http://h/v1".join("chat/completions")` →
    /// `"http://h/chat/completions"`, dropping `/v1`). The trailing slash is added in `new()`.
    base: reqwest::Url,
    /// Model id sent in the request body. An empty string sends whatever the gateway defaults to.
    model: String,
    /// Optional API key — only sent when `Some`. NEVER falls back to the Anthropic key (R3).
    api_key: Option<String>,
    /// Hardened reqwest client (TLS 1.2 floor + timeouts) reused from `anthropic::build_client`.
    client: reqwest::Client,
}

impl OpenAiCompatProvider {
    /// Validate the base URL (R4) and construct the provider. `api_key` is optional (R3);
    /// if `None` no `Authorization` header is sent (useful for unauthenticated local gateways).
    ///
    /// The base path is normalized to end with `/` so that `join("chat/completions")` always
    /// APPENDS — never replaces the last segment (RFC 3986 trap, e.g. `/v1` → `/v1/`).
    pub fn new(base_url: String, model: String, api_key: Option<String>) -> Result<Self> {
        let mut base = validate_gateway_url(&base_url)?; // enforces R1/R4 + no-creds at construction
        // Ensure trailing slash so join("chat/completions") appends rather than replacing the last
        // path segment. `http://host/v1` → `http://host/v1/`; `http://host/v1/` is a no-op.
        if !base.path().ends_with('/') {
            let p = format!("{}/", base.path());
            base.set_path(&p);
        }
        Ok(Self {
            base,
            model,
            api_key,
            client: crate::summarize::anthropic::build_client(), // reuse the hardened builder
        })
    }

    /// Compose the `/chat/completions` endpoint URL from the normalized base. Exposed as
    /// `pub(crate)` so unit tests can assert the composed URL without making a network call.
    pub(crate) fn chat_endpoint(&self) -> Result<reqwest::Url> {
        self.base
            .join("chat/completions")
            .map_err(|e| AppError::Summarize(format!("gateway URL join failed: {e}")))
    }

    /// Compose the `/models` endpoint URL from the normalized base.
    pub(crate) fn models_endpoint(&self) -> Result<reqwest::Url> {
        self.base
            .join("models")
            .map_err(|e| AppError::Summarize(format!("gateway URL join failed: {e}")))
    }

    /// `GET {base}/models` → list of model ids from the gateway catalog.
    ///
    /// Inbound-only: sends NO meeting content — only an optional `Authorization: Bearer` header.
    /// Therefore this path does NOT need the redaction firewall or the consent gate.
    /// A non-2xx response maps to `AppError::Unavailable` (the gateway is reachable but refused).
    pub async fn list_models(&self) -> Result<Vec<String>> {
        let url = self.models_endpoint()?;

        let mut req = self.client.get(url);

        // R3 — only attach the gateway key; never falls back to another provider's key.
        if let Some(ref key) = self.api_key {
            if !key.trim().is_empty() {
                req = req.header("authorization", format!("Bearer {key}"));
            }
        }

        let resp = req
            .send()
            .await
            .map_err(|e| AppError::Unavailable(format!("gateway /models request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            // Route through map_gateway_error for message parity with the chat path (401/429
            // produce the same actionable messages here as on a bad summarization key/rate-limit).
            return Err(map_gateway_error(status.as_u16(), &err_body));
        }

        let body = resp.text().await.map_err(|e| {
            AppError::Unavailable(format!("failed to read gateway /models body: {e}"))
        })?;

        parse_models_response(&body)
    }

    /// POST `body` to `{base}/chat/completions`, set the `Authorization` header only when a key is
    /// present (R3), and return the raw response body text. Maps non-2xx responses to
    /// `AppError::Summarize`/`Unavailable` with the gateway's `error.message` when available.
    ///
    /// This is the single HTTP path shared by `post_chat` (free-text) and `post_chat_json`
    /// (json_schema mode). Do NOT duplicate this code for new request variants — build the body
    /// with the appropriate `chat_body*` helper and call this.
    async fn post_chat_raw(&self, body: Value) -> Result<String> {
        let url = self.chat_endpoint()?;

        let mut req = self
            .client
            .post(url)
            .header("content-type", "application/json")
            .json(&body);

        // R3 — only attach the gateway key; never falls back to another provider's key.
        if let Some(ref key) = self.api_key {
            if !key.trim().is_empty() {
                req = req.header("authorization", format!("Bearer {key}"));
            }
        }

        let resp = req
            .send()
            .await
            .map_err(|e| AppError::Summarize(format!("gateway request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            return Err(map_gateway_error(status.as_u16(), &err_body));
        }

        resp.text()
            .await
            .map_err(|e| AppError::Summarize(format!("failed to read gateway response body: {e}")))
    }

    /// POST to `{base}/chat/completions` with a free-text body (no `response_format`).
    async fn post_chat(&self, system: &str, user: &str) -> Result<String> {
        self.post_chat_raw(chat_body(&self.model, system, user)).await
    }

    /// Shared call path returning both the text and the `CallMeta`.
    async fn call_with_meta(&self, system: &str, user: &str) -> Result<(String, CallMeta)> {
        let body_text = self.post_chat(system, user).await?;
        parse_chat_response(&body_text)
    }
}

#[async_trait]
impl SummarizerProvider for OpenAiCompatProvider {
    fn id(&self) -> &str {
        "gateway"
    }

    /// Report `Available` iff a base URL is set. No network call — availability is checked
    /// at construction time; the health command (`gateway_health`, Phase 4) does the probe.
    async fn availability(&self) -> Availability {
        // A successfully-constructed provider already has a validated URL — it is "available"
        // from a configuration standpoint. Phase 4 adds a live health probe command.
        Availability::Available
    }

    async fn summarize(&self, req: &SummarizeRequest) -> Result<String> {
        let (text, _meta) = self.summarize_with_meta(req).await?;
        Ok(text)
    }

    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        let (text, _meta) = self.call_with_meta(system, user).await?;
        Ok(text)
    }

    async fn summarize_with_meta(
        &self,
        req: &SummarizeRequest,
    ) -> Result<(String, CallMeta)> {
        let system_prompt = if req.template.trim().is_empty() {
            template::default_template()
        } else {
            req.template.clone()
        };
        let user_content = template::render_user_content(req);
        self.call_with_meta(&system_prompt, &user_content).await
    }

    async fn complete_with_meta(
        &self,
        system: &str,
        user: &str,
    ) -> Result<(String, CallMeta)> {
        self.call_with_meta(system, user).await
    }

    /// Gateway override: use `response_format: json_schema` so the gateway enforces valid JSON
    /// output natively (constrained decoding). Returns both the parsed value AND the real
    /// `CallMeta` (token usage + served model) from the response — so the egress ledger records
    /// actual token counts for timeline/graph side-tasks instead of the default empty meta.
    async fn complete_json_with_meta(
        &self,
        system: &str,
        user: &str,
        schema: &Value,
    ) -> Result<(Value, crate::summarize::meta::CallMeta)> {
        let body = chat_body_json(&self.model, system, user, schema);
        let raw = self.post_chat_raw(body).await?;
        let (text, meta) = parse_chat_response(&raw)?;
        let v = serde_json::from_str::<Value>(&text).map_err(|e| {
            AppError::Summarize(format!(
                "complete_json: gateway returned invalid JSON in content field: {e}"
            ))
        })?;
        Ok((v, meta))
    }
    // `complete_json` inherits the delegating default: calls `complete_json_with_meta` and
    // drops the meta — callers that only need the value are unchanged.
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Task 2.4 — parse_chat_response fixture tests (RED → GREEN) ─────────────────────────────

    /// Full OpenAI-compat fixture with usage + model + cached_tokens.
    const OPENAI_FIXTURE: &str = r#"{
        "id": "chatcmpl-abc123",
        "object": "chat.completion",
        "created": 1677858242,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hello world" },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 13,
            "completion_tokens": 7,
            "total_tokens": 20,
            "prompt_tokens_details": {
                "cached_tokens": 4
            }
        }
    }"#;

    #[test]
    fn parse_chat_response_extracts_usage_and_model() {
        let (text, meta) = parse_chat_response(OPENAI_FIXTURE).unwrap();
        assert_eq!(text, "Hello world");
        assert_eq!(meta.prompt_tokens, Some(13), "prompt_tokens");
        assert_eq!(meta.completion_tokens, Some(7), "completion_tokens");
        assert_eq!(meta.total_tokens, Some(20), "total_tokens (API-reported)");
        assert_eq!(meta.cached_tokens, Some(4), "prompt_tokens_details.cached_tokens");
        assert_eq!(meta.model_served.as_deref(), Some("gpt-4o"), "model → model_served");
    }

    /// A response missing `usage` and `model` degrades gracefully.
    #[test]
    fn parse_chat_response_degrades_gracefully_without_usage() {
        let body = r#"{
            "choices": [{"message": {"content": "Minimal"}, "finish_reason": "stop"}]
        }"#;
        let (text, meta) = parse_chat_response(body).unwrap();
        assert_eq!(text, "Minimal");
        assert_eq!(meta, CallMeta::default(), "no usage/model → all None");
    }

    /// A response with no choices errors cleanly.
    #[test]
    fn parse_chat_response_errors_on_empty_choices() {
        let body = r#"{"choices": []}"#;
        assert!(
            matches!(parse_chat_response(body), Err(AppError::Summarize(_))),
            "empty choices must map to AppError::Summarize"
        );
    }

    #[test]
    fn loopback_detection() {
        assert!(host_is_loopback(&reqwest::Url::parse("http://localhost:11434").unwrap()));
        assert!(host_is_loopback(&reqwest::Url::parse("http://127.0.0.1:4000/v1").unwrap()));
        assert!(host_is_loopback(&reqwest::Url::parse("http://[::1]:8000").unwrap()));
        assert!(!host_is_loopback(&reqwest::Url::parse("https://api.example.com/v1").unwrap()));
    }

    #[test]
    fn url_validation_rejects_plain_http_remote_and_bad_scheme() {
        assert!(validate_gateway_url("https://gw.example.com/v1").is_ok());
        assert!(validate_gateway_url("http://localhost:4000/v1").is_ok());
        assert!(validate_gateway_url("http://evil.example.com/v1").is_err()); // remote http rejected
        assert!(validate_gateway_url("file:///etc/passwd").is_err()); // scheme rejected
        assert!(validate_gateway_url("not a url").is_err());
    }

    // ─── Task 8.2 — chat_body_json builder test (RED → GREEN) ───────────────────────────────────

    /// The JSON-schema body must carry `response_format.type == "json_schema"`, the caller-supplied
    /// schema embedded under `response_format.json_schema.schema`, and `strict: true`.
    #[test]
    fn chat_body_json_carries_response_format_json_schema() {
        let schema = serde_json::json!({"type":"object","properties":{"people":{"type":"array"}}});
        let body = chat_body_json("gpt-4o", "SYS", "USER", &schema);
        assert_eq!(body["model"], "gpt-4o", "model must be forwarded");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["stream"], false, "streaming must be disabled");
        let rf = &body["response_format"];
        assert_eq!(rf["type"], "json_schema", "response_format.type must be 'json_schema'");
        let js = &rf["json_schema"];
        assert_eq!(js["name"], "result", "json_schema.name must be 'result'");
        assert_eq!(js["strict"], true, "strict must be true");
        // The supplied schema appears verbatim under json_schema.schema.
        assert_eq!(
            js["schema"],
            schema,
            "caller-supplied schema must appear verbatim under json_schema.schema"
        );
    }

    /// `chat_body_json` with an empty schema still produces a valid JSON-schema body.
    #[test]
    fn chat_body_json_with_empty_schema() {
        let schema = serde_json::json!({});
        let body = chat_body_json("llama-3", "S", "U", &schema);
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["schema"], schema);
    }

    /// Task 1.2 — the chat body must be OpenAI-shaped with system/user roles and stream:false.
    #[test]
    fn chat_body_is_openai_shaped() {
        let body = chat_body("gpt-4o", "SYS", "USER");
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "SYS");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "USER");
        assert_eq!(body["stream"], false);
    }

    /// Task 1.2 — construction succeeds for valid URLs, fails for invalid ones.
    #[test]
    fn new_validates_url_at_construction() {
        // Valid HTTPS remote → ok.
        assert!(OpenAiCompatProvider::new(
            "https://gw.example.com/v1".to_string(),
            "gpt-4o".to_string(),
            None,
        )
        .is_ok());
        // Valid HTTP localhost → ok.
        assert!(OpenAiCompatProvider::new(
            "http://127.0.0.1:4000/v1".to_string(),
            String::new(),
            None,
        )
        .is_ok());
        // Remote HTTP → rejected (R4).
        let err = OpenAiCompatProvider::new(
            "http://gw.example.com/v1".to_string(),
            String::new(),
            None,
        )
        .err()
        .expect("expected InvalidArg for remote http URL");
        assert!(
            matches!(err, AppError::InvalidArg(_)),
            "expected InvalidArg, got: {err}"
        );
        // Bad scheme → rejected.
        let err2 = OpenAiCompatProvider::new(
            "file:///etc/passwd".to_string(),
            String::new(),
            None,
        )
        .err()
        .expect("expected InvalidArg for file:// URL");
        assert!(
            matches!(err2, AppError::InvalidArg(_)),
            "expected InvalidArg for bad scheme, got: {err2}"
        );
    }

    /// Fix — the composed chat-completions URL must APPEND to the base path, not replace it.
    /// `http://h:4000/v1` → `http://h:4000/v1/chat/completions` (the `/v1` must survive).
    /// `http://h:4000` (no path) → `http://h:4000/chat/completions`.
    #[test]
    fn chat_endpoint_preserves_base_path() {
        let p = OpenAiCompatProvider::new(
            "http://localhost:4000/v1".to_string(),
            "gpt-4o".to_string(),
            None,
        )
        .unwrap();
        assert_eq!(
            p.chat_endpoint().unwrap().as_str(),
            "http://localhost:4000/v1/chat/completions",
            "base path /v1 must be preserved (not replaced) in the join"
        );

        // Base without any path: join → /chat/completions.
        let p2 = OpenAiCompatProvider::new(
            "http://localhost:4000".to_string(),
            String::new(),
            None,
        )
        .unwrap();
        assert_eq!(
            p2.chat_endpoint().unwrap().as_str(),
            "http://localhost:4000/chat/completions"
        );

        // HTTPS remote with /v1 path also preserved.
        let p3 = OpenAiCompatProvider::new(
            "https://gw.example.com/v1".to_string(),
            String::new(),
            None,
        )
        .unwrap();
        assert_eq!(
            p3.chat_endpoint().unwrap().as_str(),
            "https://gw.example.com/v1/chat/completions"
        );

        // Trailing slash in the user-supplied URL is idempotent.
        let p4 = OpenAiCompatProvider::new(
            "http://localhost:4000/v1/".to_string(),
            String::new(),
            None,
        )
        .unwrap();
        assert_eq!(
            p4.chat_endpoint().unwrap().as_str(),
            "http://localhost:4000/v1/chat/completions"
        );
    }

    /// Minor fix — embedded credentials in the URL must be rejected to prevent leaking the secret
    /// through reqwest error Display strings.
    #[test]
    fn validate_gateway_url_rejects_embedded_credentials() {
        // username + empty password (common key-in-URL pattern).
        let err = validate_gateway_url("https://mykey:@gw.example.com/v1")
            .err()
            .expect("embedded creds must be rejected");
        assert!(
            matches!(err, AppError::InvalidArg(_)),
            "expected InvalidArg for embedded creds, got: {err}"
        );
        // username + password.
        let err2 = validate_gateway_url("https://user:pass@gw.example.com/v1")
            .err()
            .expect("embedded user:pass must be rejected");
        assert!(matches!(err2, AppError::InvalidArg(_)));
        // No credentials → still valid.
        assert!(validate_gateway_url("https://gw.example.com/v1").is_ok());
    }

    // ─── Task 4.1 — map_gateway_error fixture tests (RED → GREEN) ──────────────────────────────

    /// 404 with an OpenAI error envelope → `Summarize` containing the envelope message.
    #[test]
    fn map_gateway_error_404_with_message_is_summarize() {
        let err = map_gateway_error(
            404,
            r#"{"error":{"message":"model 'gpt-5' not found","type":"invalid_request_error"}}"#,
        );
        match err {
            AppError::Summarize(msg) => {
                assert!(
                    msg.contains("model 'gpt-5' not found"),
                    "404 Summarize must include the envelope message, got: {msg}"
                );
            }
            other => panic!("expected Summarize for 404, got: {other:?}"),
        }
    }

    /// 401 → `Unavailable` with an actionable key-check message.
    #[test]
    fn map_gateway_error_401_is_unavailable() {
        let err = map_gateway_error(401, "");
        assert!(
            matches!(err, AppError::Unavailable(_)),
            "401 must map to Unavailable, got: {err:?}"
        );
        if let AppError::Unavailable(msg) = err {
            assert!(
                msg.contains("API key"),
                "401 Unavailable must mention 'API key', got: {msg}"
            );
        }
    }

    /// 429 → `Unavailable` (rate-limited / budget exceeded).
    #[test]
    fn map_gateway_error_429_is_unavailable() {
        let err = map_gateway_error(429, "");
        assert!(
            matches!(err, AppError::Unavailable(_)),
            "429 must map to Unavailable, got: {err:?}"
        );
    }

    /// 403 → `Unavailable` (key rejected).
    #[test]
    fn map_gateway_error_403_is_unavailable() {
        let err = map_gateway_error(403, "");
        assert!(
            matches!(err, AppError::Unavailable(_)),
            "403 must map to Unavailable, got: {err:?}"
        );
    }

    /// 500 → `Summarize` with "gateway error 500: …".
    #[test]
    fn map_gateway_error_500_is_summarize_with_status() {
        let err = map_gateway_error(
            500,
            r#"{"error":{"message":"internal server error"}}"#,
        );
        match err {
            AppError::Summarize(msg) => {
                assert!(
                    msg.contains("gateway error 500"),
                    "5xx Summarize must include 'gateway error <status>', got: {msg}"
                );
            }
            other => panic!("expected Summarize for 500, got: {other:?}"),
        }
    }

    /// Non-JSON body degrades gracefully (no panic, falls back to "HTTP {status}").
    #[test]
    fn map_gateway_error_non_json_body_degrades_gracefully() {
        let err = map_gateway_error(503, "Service Unavailable");
        match err {
            AppError::Summarize(msg) => {
                // falls back to "HTTP 503" when JSON parse fails
                assert!(
                    msg.contains("503") || msg.contains("HTTP"),
                    "non-JSON 5xx must still include status info, got: {msg}"
                );
            }
            other => panic!("expected Summarize for 503, got: {other:?}"),
        }
    }

    /// Task 1.2 — gateway error extraction from an OpenAI-format error envelope.
    #[test]
    fn gateway_error_extraction() {
        let body = r#"{"error":{"message":"model 'x' not found","type":"invalid_request_error"}}"#;
        assert_eq!(extract_gateway_error(body).as_deref(), Some("model 'x' not found"));
        // No error field → None (callers fall back to a generic message).
        assert!(extract_gateway_error("{}").is_none());
        assert!(extract_gateway_error("not json").is_none());
    }

    // ─── Task 3.1 — parse_models_response fixture tests (RED → GREEN) ────────────────────────────

    /// Standard OpenAI-compat `/v1/models` response: two ids are returned in order.
    #[test]
    fn parse_models_response_extracts_ids() {
        let body = r#"{"object":"list","data":[{"id":"gpt-4o"},{"id":"llama-3"}]}"#;
        let ids = parse_models_response(body).unwrap();
        assert_eq!(ids, vec!["gpt-4o", "llama-3"], "ids must match fixture order");
    }

    /// A malformed (non-JSON) body degrades to an empty list — never a panic.
    #[test]
    fn parse_models_response_malformed_body_returns_empty() {
        let ids = parse_models_response("not json at all").unwrap();
        assert!(ids.is_empty(), "malformed body must degrade to an empty list");
    }

    /// An empty data array returns an empty list cleanly.
    #[test]
    fn parse_models_response_empty_data_returns_empty() {
        let body = r#"{"object":"list","data":[]}"#;
        let ids = parse_models_response(body).unwrap();
        assert!(ids.is_empty(), "empty data must return an empty list");
    }

    /// A non-OpenAI body (e.g. a provider that returns only `{"models":[…]}`) degrades to empty.
    #[test]
    fn parse_models_response_non_openai_schema_returns_empty() {
        let body = r#"{"models":["gpt-4o","llama-3"]}"#;
        let ids = parse_models_response(body).unwrap();
        assert!(
            ids.is_empty(),
            "a non-OpenAI schema with no 'data' key must degrade to an empty list"
        );
    }

    /// Task 3.1 — models_endpoint appends to the base path (same RFC 3986 invariant as
    /// chat_endpoint — the `/v1` must survive; join must APPEND, not replace).
    #[test]
    fn models_endpoint_preserves_base_path() {
        let p = OpenAiCompatProvider::new(
            "http://localhost:4000/v1".to_string(),
            "gpt-4o".to_string(),
            None,
        )
        .unwrap();
        assert_eq!(
            p.models_endpoint().unwrap().as_str(),
            "http://localhost:4000/v1/models",
            "base path /v1 must be preserved (not replaced) in the join"
        );
    }
}
