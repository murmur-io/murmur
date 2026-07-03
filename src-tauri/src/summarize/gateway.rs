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
        // The raw provider never redacts (the RedactingProvider wrapper does + overwrites this
        // with the real scrub count for cloud egress).
        redactions: None,
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

/// Resolve the chat-completions endpoint URL from a validated base URL — pure, testable, no I/O.
///
/// Heuristic (base path after stripping any trailing `/`):
///   - empty / root (`""`)  → `{scheme}://{host}/chat/completions`
///   - `/v1`                → `{scheme}://{host}/v1/chat/completions`
///   - anything else (e.g. `/test`, a Kong route, or already `/…/chat/completions`)
///     → use the base URL path AS-IS (it IS the full chat endpoint)
///
/// Preserves scheme, host, port, and query. Never fails.
pub(crate) fn resolve_chat_endpoint(base: &reqwest::Url) -> reqwest::Url {
    let trimmed = base.path().trim_end_matches('/');
    let new_path = match trimmed {
        "" => "/chat/completions",
        "/v1" => "/v1/chat/completions",
        p => p, // custom route or already-full endpoint — use as-is (trailing slash stripped)
    };
    let mut url = base.clone();
    url.set_path(new_path);
    url
}

/// Resolve the models-catalog endpoint URL from a validated base URL, if one exists — pure,
/// testable, no I/O.
///
/// Heuristic (base path after stripping any trailing `/`):
///   - empty / root (`""`)          → `Some({scheme}://{host}/models)`
///   - `/v1`                         → `Some({scheme}://{host}/v1/models)`
///   - ends with `/chat/completions` → `Some(sibling /models)` (replace last segment)
///   - any other custom path         → `None` (custom routes have no catalog, e.g. Kong `/test`)
///
/// A `None` return means the caller skips the catalog fetch rather than returning an error.
pub(crate) fn resolve_models_endpoint(base: &reqwest::Url) -> Option<reqwest::Url> {
    let trimmed = base.path().trim_end_matches('/');
    let new_path: String = match trimmed {
        "" => "/models".to_string(),
        "/v1" => "/v1/models".to_string(),
        p if p.ends_with("/chat/completions") => {
            // sibling endpoint: strip "/chat/completions", append "/models"
            let prefix = &p[..p.len() - "/chat/completions".len()];
            format!("{prefix}/models")
        }
        _ => return None, // custom route — no catalog
    };
    let mut url = base.clone();
    url.set_path(&new_path);
    Some(url)
}

/// Classify a transport outcome as reachable or not.
/// Any HTTP response (regardless of status code) → `true`; a transport-layer failure
/// (DNS, connect refused, timeout) → `false`. Factored out of `probe()` so the decision logic
/// is unit-testable without a live server. The live GET in `probe()` is NOT unit-testable.
pub(crate) fn classify_reachable(got_response: bool) -> bool {
    got_response
}

/// An OpenAI-compatible AI Gateway provider (LiteLLM / Kong / Portkey / vLLM / local LiteLLM /
/// LM Studio / …). Talks to the resolved chat endpoint with an optional `Authorization: Bearer`
/// header.
///
/// Always cloud-classified — even a localhost gateway can forward to the cloud — so it is:
///   • consent-gated by `make_provider` before construction,
///   • redaction-wrapped by `make_provider` after construction (R2).
pub struct OpenAiCompatProvider {
    /// Validated base URL stored as-is from construction. Endpoint resolution is handled by the
    /// pure helpers `resolve_chat_endpoint` / `resolve_models_endpoint` at call time, so no
    /// trailing-slash normalization is needed here.
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
    /// The base URL is stored as validated; `chat_endpoint()` / `models_endpoint()` apply the
    /// path-resolution heuristic at call time so no trailing-slash forcing is needed here.
    pub fn new(base_url: String, model: String, api_key: Option<String>) -> Result<Self> {
        let base = validate_gateway_url(&base_url)?; // enforces R1/R4 + no-creds at construction
        Ok(Self {
            base,
            model,
            api_key,
            client: crate::summarize::anthropic::build_client(), // reuse the hardened builder
        })
    }

    /// Resolve the chat-completions endpoint URL. Delegates to the pure `resolve_chat_endpoint`
    /// helper; `pub(crate)` so unit tests can assert the URL without making a network call.
    pub(crate) fn chat_endpoint(&self) -> reqwest::Url {
        resolve_chat_endpoint(&self.base)
    }

    /// Resolve the models-catalog endpoint URL, if this base has one.
    /// Returns `None` for custom route bases (e.g. `/test`) that have no catalog.
    /// Delegates to the pure `resolve_models_endpoint` helper; `pub(crate)` for unit tests.
    pub(crate) fn models_endpoint(&self) -> Option<reqwest::Url> {
        resolve_models_endpoint(&self.base)
    }

    /// `GET {base}/models` → list of model ids from the gateway catalog.
    ///
    /// Returns `Ok(vec![])` immediately when the base URL has no models endpoint (i.e. a custom
    /// route such as Kong `/test` — `resolve_models_endpoint` returns `None`). The FE model picker
    /// handles an empty list gracefully by showing a manual entry field.
    ///
    /// Inbound-only: sends NO meeting content — only an optional `Authorization: Bearer` header.
    /// Therefore this path does NOT need the redaction firewall or the consent gate.
    /// A non-2xx response maps to `AppError::Unavailable` (the gateway is reachable but refused).
    pub async fn list_models(&self) -> Result<Vec<String>> {
        let url = match self.models_endpoint() {
            None => return Ok(vec![]), // custom route — no catalog, not an error
            Some(u) => u,
        };

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

    /// Probe the gateway for reachability without sending any meeting content.
    ///
    /// Targets the models endpoint when one exists (`Some`), otherwise the chat endpoint
    /// (a GET to a POST-only route returns 4xx but PROVES the server is reachable — no LLM call,
    /// no cost). Returns `(reachable, model_count)` where:
    ///   - `reachable`: `true` if ANY HTTP response arrived (any status code); `false` on a
    ///     transport-layer failure (DNS, connection refused, timeout).
    ///   - `model_count`: number of model ids returned ONLY when the models endpoint exists,
    ///     responded with 200, and the body parses via `parse_models_response`; else `0`.
    ///
    /// Never returns an error — degrades to `(false, 0)` on any transport failure.
    pub async fn probe(&self) -> (bool, u32) {
        let (url, is_models) = match resolve_models_endpoint(&self.base) {
            Some(u) => (u, true),
            None => (resolve_chat_endpoint(&self.base), false),
        };

        let mut req = self.client.get(url);
        // R3 — attach the gateway key when present; never fall back to another provider's key.
        if let Some(ref key) = self.api_key {
            if !key.trim().is_empty() {
                req = req.header("authorization", format!("Bearer {key}"));
            }
        }

        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                let model_count = if is_models && status.is_success() {
                    resp.text()
                        .await
                        .ok()
                        .and_then(|b| parse_models_response(&b).ok())
                        .map(|ids| ids.len() as u32)
                        .unwrap_or(0)
                } else {
                    0
                };
                (classify_reachable(true), model_count)
            }
            Err(_) => (classify_reachable(false), 0),
        }
    }

    /// POST `body` to `{base}/chat/completions`, set the `Authorization` header only when a key is
    /// present (R3), and return the raw response body text. Maps non-2xx responses to
    /// `AppError::Summarize`/`Unavailable` with the gateway's `error.message` when available.
    ///
    /// This is the single HTTP path shared by `post_chat` (free-text) and `post_chat_json`
    /// (json_schema mode). Do NOT duplicate this code for new request variants — build the body
    /// with the appropriate `chat_body*` helper and call this.
    async fn post_chat_raw(&self, body: Value) -> Result<String> {
        let url = self.chat_endpoint(); // pure resolution — never fails

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
        .map(|_| ()).expect_err("expected InvalidArg for remote http URL");
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
        .map(|_| ()).expect_err("expected InvalidArg for file:// URL");
        assert!(
            matches!(err2, AppError::InvalidArg(_)),
            "expected InvalidArg for bad scheme, got: {err2}"
        );
    }

    /// `chat_endpoint` applies the path-resolution heuristic: root/`/v1` → append
    /// `chat/completions`; any other path (custom Kong route, already-full endpoint) → as-is.
    #[test]
    fn chat_endpoint_preserves_base_path() {
        // /v1 base — must append /chat/completions.
        let p = OpenAiCompatProvider::new(
            "http://localhost:4000/v1".to_string(),
            "gpt-4o".to_string(),
            None,
        )
        .unwrap();
        assert_eq!(
            p.chat_endpoint().as_str(),
            "http://localhost:4000/v1/chat/completions",
            "base path /v1 must be preserved and chat/completions appended"
        );

        // No path (root) → /chat/completions.
        let p2 = OpenAiCompatProvider::new(
            "http://localhost:4000".to_string(),
            String::new(),
            None,
        )
        .unwrap();
        assert_eq!(p2.chat_endpoint().as_str(), "http://localhost:4000/chat/completions");

        // HTTPS remote with /v1 — preserved.
        let p3 = OpenAiCompatProvider::new(
            "https://gw.example.com/v1".to_string(),
            String::new(),
            None,
        )
        .unwrap();
        assert_eq!(
            p3.chat_endpoint().as_str(),
            "https://gw.example.com/v1/chat/completions"
        );

        // Trailing slash on /v1 → idempotent.
        let p4 = OpenAiCompatProvider::new(
            "http://localhost:4000/v1/".to_string(),
            String::new(),
            None,
        )
        .unwrap();
        assert_eq!(
            p4.chat_endpoint().as_str(),
            "http://localhost:4000/v1/chat/completions"
        );

        // Custom Kong route /test → used AS-IS (it IS the full chat endpoint).
        let p5 = OpenAiCompatProvider::new(
            "https://gw.example.com/test".to_string(),
            String::new(),
            None,
        )
        .unwrap();
        assert_eq!(
            p5.chat_endpoint().as_str(),
            "https://gw.example.com/test",
            "custom route /test must be used as-is (no appending)"
        );

        // Trailing slash on custom route → stripped.
        let p6 = OpenAiCompatProvider::new(
            "https://gw.example.com/test/".to_string(),
            String::new(),
            None,
        )
        .unwrap();
        assert_eq!(
            p6.chat_endpoint().as_str(),
            "https://gw.example.com/test",
            "custom route /test/ trailing slash must be stripped"
        );

        // Already-full endpoint pasted by user → returned as-is.
        let p7 = OpenAiCompatProvider::new(
            "https://gw.example.com/v1/chat/completions".to_string(),
            String::new(),
            None,
        )
        .unwrap();
        assert_eq!(
            p7.chat_endpoint().as_str(),
            "https://gw.example.com/v1/chat/completions",
            "already-full endpoint must be returned as-is"
        );
    }

    /// Minor fix — embedded credentials in the URL must be rejected to prevent leaking the secret
    /// through reqwest error Display strings.
    #[test]
    fn validate_gateway_url_rejects_embedded_credentials() {
        // username + empty password (common key-in-URL pattern).
        let err = validate_gateway_url("https://mykey:@gw.example.com/v1")
            .expect_err("embedded creds must be rejected");
        assert!(
            matches!(err, AppError::InvalidArg(_)),
            "expected InvalidArg for embedded creds, got: {err}"
        );
        // username + password.
        let err2 = validate_gateway_url("https://user:pass@gw.example.com/v1")
            .expect_err("embedded user:pass must be rejected");
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

    // ─── resolve_chat_endpoint pure-function tests (RED → GREEN) ─────────────────────────────────

    fn u(s: &str) -> reqwest::Url {
        reqwest::Url::parse(s).unwrap()
    }

    #[test]
    fn resolve_chat_endpoint_root() {
        assert_eq!(
            resolve_chat_endpoint(&u("https://h/")).as_str(),
            "https://h/chat/completions"
        );
    }

    #[test]
    fn resolve_chat_endpoint_v1() {
        assert_eq!(
            resolve_chat_endpoint(&u("https://h/v1")).as_str(),
            "https://h/v1/chat/completions"
        );
    }

    #[test]
    fn resolve_chat_endpoint_v1_trailing_slash() {
        assert_eq!(
            resolve_chat_endpoint(&u("https://h/v1/")).as_str(),
            "https://h/v1/chat/completions"
        );
    }

    #[test]
    fn resolve_chat_endpoint_custom_path_as_is() {
        assert_eq!(
            resolve_chat_endpoint(&u("https://h/test")).as_str(),
            "https://h/test",
            "/test custom Kong route must be used as-is"
        );
    }

    #[test]
    fn resolve_chat_endpoint_custom_path_trailing_slash_stripped() {
        assert_eq!(
            resolve_chat_endpoint(&u("https://h/test/")).as_str(),
            "https://h/test",
            "/test/ trailing slash must be stripped"
        );
    }

    #[test]
    fn resolve_chat_endpoint_already_full() {
        assert_eq!(
            resolve_chat_endpoint(&u("https://h/v1/chat/completions")).as_str(),
            "https://h/v1/chat/completions",
            "already-full endpoint must be returned as-is"
        );
    }

    // ─── resolve_models_endpoint pure-function tests (RED → GREEN) ───────────────────────────────

    #[test]
    fn resolve_models_endpoint_root_has_catalog() {
        let r = resolve_models_endpoint(&u("https://h/"));
        assert_eq!(r.unwrap().as_str(), "https://h/models");
    }

    #[test]
    fn resolve_models_endpoint_v1_has_catalog() {
        let r = resolve_models_endpoint(&u("https://h/v1"));
        assert_eq!(r.unwrap().as_str(), "https://h/v1/models");
    }

    #[test]
    fn resolve_models_endpoint_custom_path_is_none() {
        let r = resolve_models_endpoint(&u("https://h/test"));
        assert!(r.is_none(), "custom /test route must yield None");
    }

    #[test]
    fn resolve_models_endpoint_full_chat_path_yields_sibling_models() {
        let r = resolve_models_endpoint(&u("https://h/v1/chat/completions"));
        assert_eq!(r.unwrap().as_str(), "https://h/v1/models");
    }

    // ─── classify_reachable pure helper tests (RED → GREEN) ──────────────────────────────────────

    #[test]
    fn classify_reachable_got_response_is_true() {
        assert!(classify_reachable(true), "any HTTP response → reachable");
    }

    #[test]
    fn classify_reachable_transport_failure_is_false() {
        assert!(!classify_reachable(false), "transport failure → not reachable");
    }

    // ─── list_models with no catalog (None models endpoint) — async, no network ─────────────────

    #[tokio::test]
    async fn list_models_with_custom_route_returns_empty_without_network() {
        // A custom-route base has no models endpoint; list_models must return Ok(vec![])
        // immediately without attempting a network call.
        let provider = OpenAiCompatProvider::new(
            "https://gw.example.com/test".to_string(),
            "llama-3".to_string(),
            None,
        )
        .unwrap();
        // models_endpoint() is None for /test — list_models must short-circuit.
        assert!(provider.models_endpoint().is_none());
        let ids = provider.list_models().await.unwrap();
        assert!(ids.is_empty(), "custom route must yield empty model list without a network call");
    }

    /// `models_endpoint` applies the path-resolution heuristic: root/`/v1` → `Some(…/models)`;
    /// a sibling of `/chat/completions` → `Some(…/models)`; custom routes → `None`.
    #[test]
    fn models_endpoint_preserves_base_path() {
        // /v1 base → Some(/v1/models).
        let p = OpenAiCompatProvider::new(
            "http://localhost:4000/v1".to_string(),
            "gpt-4o".to_string(),
            None,
        )
        .unwrap();
        assert_eq!(
            p.models_endpoint().unwrap().as_str(),
            "http://localhost:4000/v1/models",
            "base path /v1 must be preserved in the models endpoint"
        );

        // Root base → Some(/models).
        let p2 = OpenAiCompatProvider::new(
            "http://localhost:4000".to_string(),
            String::new(),
            None,
        )
        .unwrap();
        assert_eq!(p2.models_endpoint().unwrap().as_str(), "http://localhost:4000/models");

        // Custom Kong route /test → None (no catalog).
        let p3 = OpenAiCompatProvider::new(
            "https://gw.example.com/test".to_string(),
            String::new(),
            None,
        )
        .unwrap();
        assert!(
            p3.models_endpoint().is_none(),
            "custom route /test must yield None models endpoint"
        );

        // Already-full /v1/chat/completions → Some(sibling /v1/models).
        let p4 = OpenAiCompatProvider::new(
            "https://gw.example.com/v1/chat/completions".to_string(),
            String::new(),
            None,
        )
        .unwrap();
        assert_eq!(
            p4.models_endpoint().unwrap().as_str(),
            "https://gw.example.com/v1/models",
            "full /v1/chat/completions base must produce sibling /v1/models"
        );
    }
}
