use serde::Deserialize;
use serde_json::json;

use crate::error::AppError;
use crate::summarize::meta::CallMeta;
use crate::summarize::provider::*;
use crate::summarize::template;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MODEL: &str = "claude-opus-4-8";
/// Generous but non-streaming-safe output cap (keeps the request under the SDK/HTTP
/// timeout window while leaving room for a full note).
const MAX_TOKENS: u32 = 16_000;

/// Talks to the first-party Anthropic Messages API over `reqwest`.
///
/// The API key is resolved from the macOS Keychain by the factory and passed in at
/// construction; this provider never reads the Keychain itself.
pub struct AnthropicProvider {
    client: reqwest::Client,
    model: String,
    api_key: Option<String>,
    /// Reasoning EFFORT: `""`/`"default"` (provider default — no thinking config sent), or
    /// `"low"`/`"medium"`/`"high"`. When set, ADAPTIVE thinking + an output effort tier is added to
    /// the request body. Opus 4.8 requires `thinking.type = "adaptive"`; sending the older
    /// `thinking.enabled`/`budget_tokens` shape would 400, so we deliberately never do that.
    effort: String,
}

impl AnthropicProvider {
    /// `api_key` already resolved from Keychain by the factory. `model` defaults to
    /// `claude-opus-4-8` when empty. Effort defaults to provider-default (`""`).
    pub fn new(api_key: Option<String>, model: String) -> Self {
        Self::with_effort(api_key, model, String::new())
    }

    /// Like [`new`], plus an explicit reasoning-effort tier (`""`/`"default"` = provider default).
    pub fn with_effort(api_key: Option<String>, model: String, effort: String) -> Self {
        let model = if model.trim().is_empty() {
            DEFAULT_MODEL.to_string()
        } else {
            model
        };
        Self {
            client: build_client(),
            model,
            api_key,
            effort,
        }
    }
}

/// Inject ADAPTIVE thinking + an output-effort tier into a `/v1/messages` request `body` when
/// `effort` is a real tier. Empty `""` or `"default"` ⇒ the body is left UNTOUCHED (provider
/// default — no thinking config sent at all). An UNKNOWN tier is also left untouched (fail-safe:
/// never send a malformed effort that would 400). `body` must be a JSON object.
///
/// Shape (Opus 4.8): `"thinking": {"type": "adaptive"}` + `"output_config": {"effort": <tier>}`.
/// NEVER `thinking.enabled`/`budget_tokens` — that legacy shape 400s on the 4.x adaptive models.
fn apply_effort(body: &mut serde_json::Value, effort: &str) {
    let tier = effort.trim();
    if tier.is_empty() || tier.eq_ignore_ascii_case("default") {
        return;
    }
    if !matches!(tier, "low" | "medium" | "high") {
        return;
    }
    if let Some(obj) = body.as_object_mut() {
        obj.insert("thinking".to_string(), json!({ "type": "adaptive" }));
        obj.insert("output_config".to_string(), json!({ "effort": tier }));
    }
}

/// Build the reqwest client for cloud egress (E12): pin a TLS floor of 1.2 (refuse to negotiate
/// the broken TLS 1.0/1.1) and set an overall request timeout so a stalled connection cannot wedge
/// the pipeline. Falls back to `Client::new()` only if the builder somehow fails (it shouldn't with
/// the rustls backend) so the provider is never un-constructible.
///
/// `pub(crate)` so `gateway.rs` can reuse the same hardened builder without duplicating it.
///
/// FOLLOW-UP: adopt `rustls-platform-verifier` for OS trust-store / policy-aware cert verification
/// once it is an approved dependency (not added here — needs user sign-off).
pub(crate) fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .min_tls_version(reqwest::tls::Version::TLS_1_2)
        .timeout(std::time::Duration::from_secs(120))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Minimal mirror of the `/v1/messages` success response.
#[derive(Debug, Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    /// Token-usage breakdown — `None` if the API response did not include it.
    #[serde(default)]
    usage: Option<AnthropicUsage>,
    /// Model id as echoed back by the API — may differ from requested if the endpoint re-routes.
    #[serde(default)]
    model: Option<String>,
}

/// Token-usage block returned by the Anthropic Messages API.
#[derive(Debug, Deserialize, Default)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: Option<u32>,
    #[serde(default)]
    output_tokens: Option<u32>,
    /// Tokens served from the prompt cache (prompt-caching feature).
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    text: Option<String>,
}

/// Parse a raw Anthropic Messages API JSON response body into `(text, CallMeta)`.
///
/// The text is the concatenation of all `"text"` content blocks, BOM-stripped and trimmed.
/// `CallMeta` is populated from the `usage` and `model` fields; any absent field is `None`.
/// A response that is missing `usage` / `model` still parses successfully — those fields
/// degrade to `None` rather than erroring.
pub(crate) fn parse_messages_response(body: &str) -> crate::error::Result<(String, CallMeta)> {
    let parsed: MessagesResponse = serde_json::from_str(body)
        .map_err(|e| AppError::Summarize(format!("failed to parse Anthropic response: {e}")))?;

    if parsed.stop_reason.as_deref() == Some("refusal") {
        return Err(AppError::Summarize(
            "Anthropic declined the request (stop_reason=refusal)".to_string(),
        ));
    }

    let text: String = parsed
        .content
        .iter()
        .filter(|b| b.block_type == "text")
        .filter_map(|b| b.text.as_deref())
        .collect::<Vec<_>>()
        .join("");

    let note = text.trim_start_matches('\u{feff}').trim();
    if note.is_empty() {
        return Err(AppError::Summarize(
            "Anthropic response contained no text content".to_string(),
        ));
    }

    // Build CallMeta from the optional `usage` + `model` fields.
    let meta = {
        let prompt = parsed.usage.as_ref().and_then(|u| u.input_tokens);
        let completion = parsed.usage.as_ref().and_then(|u| u.output_tokens);
        let total = match (prompt, completion) {
            (Some(p), Some(c)) => Some(p + c),
            _ => None,
        };
        CallMeta {
            model_served: parsed.model.filter(|m| !m.is_empty()),
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
            cached_tokens: parsed
                .usage
                .as_ref()
                .and_then(|u| u.cache_read_input_tokens),
            // The raw provider never redacts (the RedactingProvider wrapper does + overwrites this
            // with the real scrub count for cloud egress).
            redactions: None,
        }
    };

    Ok((note.to_string(), meta))
}

#[async_trait::async_trait]
impl SummarizerProvider for AnthropicProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    /// Report `Available` iff an API key is present. No network call.
    async fn availability(&self) -> Availability {
        match &self.api_key {
            Some(key) if !key.trim().is_empty() => Availability::Available,
            _ => Availability::Unavailable {
                reason: "no Anthropic API key set in Keychain".to_string(),
            },
        }
    }

    /// POST to `/v1/messages` with the note-format template as `system` and the meeting
    /// content as the `user` message; return the concatenated text of the response.
    async fn summarize(&self, req: &SummarizeRequest) -> crate::error::Result<String> {
        let system_prompt = if req.template.trim().is_empty() {
            template::default_template()
        } else {
            req.template.clone()
        };
        let user_content = template::render_user_content(req);
        let (text, _meta) = self.messages_call(&system_prompt, &user_content).await?;
        Ok(text)
    }

    async fn complete(&self, system: &str, user: &str) -> crate::error::Result<String> {
        let (text, _meta) = self.messages_call(system, user).await?;
        Ok(text)
    }

    async fn summarize_with_meta(
        &self,
        req: &SummarizeRequest,
    ) -> crate::error::Result<(String, CallMeta)> {
        let system_prompt = if req.template.trim().is_empty() {
            template::default_template()
        } else {
            req.template.clone()
        };
        let user_content = template::render_user_content(req);
        self.messages_call(&system_prompt, &user_content).await
    }

    async fn complete_with_meta(
        &self,
        system: &str,
        user: &str,
    ) -> crate::error::Result<(String, CallMeta)> {
        self.messages_call(system, user).await
    }
}

impl AnthropicProvider {
    /// Shared internal POST+parse path for both `summarize*` and `complete*`.
    ///
    /// Builds the `/v1/messages` request body, POSTs it, checks the status, and delegates
    /// parsing (text extraction + `CallMeta` population) to [`parse_messages_response`].
    /// All four public trait methods funnel through here so the HTTP code is never duplicated.
    async fn messages_call(
        &self,
        system: &str,
        user: &str,
    ) -> crate::error::Result<(String, CallMeta)> {
        let api_key = self
            .api_key
            .as_deref()
            .filter(|k| !k.trim().is_empty())
            .ok_or_else(|| AppError::Unavailable("Anthropic API key is not set".to_string()))?;

        let mut body = json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "system": system,
            "messages": [ { "role": "user", "content": user } ],
        });
        apply_effort(&mut body, &self.effort);

        let resp = self
            .client
            .post(API_URL)
            .header("x-api-key", api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::Summarize(format!("Anthropic request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            // Surface the API error message but keep it PII-free (it's about the request,
            // not personal data).
            let err_body = resp.text().await.unwrap_or_default();
            let detail = extract_api_error(&err_body).unwrap_or_else(|| err_body.clone());
            return Err(AppError::Summarize(format!(
                "Anthropic API returned {status}: {}",
                detail.trim()
            )));
        }

        let body_text = resp.text().await.map_err(|e| {
            AppError::Summarize(format!("failed to read Anthropic response body: {e}"))
        })?;

        parse_messages_response(&body_text)
    }
}

/// Best-effort extraction of the `error.message` field from an Anthropic error body.
fn extract_api_error(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("error")?.get("message")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Task 2.3 — parse_messages_response fixture tests (RED → GREEN) ─────────────────────────

    /// Anthropic JSON fixture with `usage` + `model` — confirms we stop discarding them.
    const ANTHROPIC_FIXTURE: &str = r#"{
        "id": "msg_01234",
        "type": "message",
        "role": "assistant",
        "model": "claude-opus-4-8",
        "content": [{"type": "text", "text": "Hello world"}],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 11,
            "output_tokens": 22,
            "cache_read_input_tokens": 5
        }
    }"#;

    #[test]
    fn parse_messages_response_extracts_usage_and_model() {
        let (text, meta) = parse_messages_response(ANTHROPIC_FIXTURE).unwrap();
        assert_eq!(text, "Hello world");
        assert_eq!(meta.prompt_tokens, Some(11), "input_tokens → prompt_tokens");
        assert_eq!(
            meta.completion_tokens,
            Some(22),
            "output_tokens → completion_tokens"
        );
        assert_eq!(meta.total_tokens, Some(33), "total = 11 + 22");
        assert_eq!(
            meta.cached_tokens,
            Some(5),
            "cache_read_input_tokens → cached_tokens"
        );
        assert_eq!(
            meta.model_served.as_deref(),
            Some("claude-opus-4-8"),
            "model field → model_served"
        );
    }

    /// A response missing `usage` and `model` still parses — degrades gracefully to `None`.
    #[test]
    fn parse_messages_response_degrades_gracefully_without_usage() {
        let body = r#"{
            "content": [{"type": "text", "text": "Minimal response"}],
            "stop_reason": "end_turn"
        }"#;
        let (text, meta) = parse_messages_response(body).unwrap();
        assert_eq!(text, "Minimal response");
        assert_eq!(meta, CallMeta::default(), "no usage/model → all None");
    }

    /// A `stop_reason=refusal` response propagates as an error (unchanged behavior).
    #[test]
    fn parse_messages_response_errors_on_refusal() {
        let body = r#"{
            "content": [],
            "stop_reason": "refusal"
        }"#;
        assert!(
            matches!(parse_messages_response(body), Err(AppError::Summarize(_))),
            "refusal must map to AppError::Summarize"
        );
    }

    fn base_body() -> serde_json::Value {
        json!({
            "model": "claude-opus-4-8",
            "max_tokens": MAX_TOKENS,
            "system": "sys",
            "messages": [ { "role": "user", "content": "u" } ],
        })
    }

    #[test]
    fn effort_set_injects_adaptive_thinking_and_output_effort() {
        for tier in ["low", "medium", "high"] {
            let mut body = base_body();
            apply_effort(&mut body, tier);
            // Adaptive thinking (the ONLY shape Opus 4.8 accepts) — never enabled/budget_tokens.
            assert_eq!(body["thinking"]["type"], "adaptive");
            assert!(body["thinking"].get("enabled").is_none());
            assert!(body["thinking"].get("budget_tokens").is_none());
            // Output effort tier carried through verbatim.
            assert_eq!(body["output_config"]["effort"], tier);
        }
    }

    #[test]
    fn effort_empty_or_default_omits_thinking_and_effort() {
        for tier in ["", "   ", "default", "Default"] {
            let mut body = base_body();
            apply_effort(&mut body, tier);
            assert!(
                body.get("thinking").is_none(),
                "effort {tier:?} must NOT add thinking (provider default)"
            );
            assert!(
                body.get("output_config").is_none(),
                "effort {tier:?} must NOT add output_config"
            );
        }
    }

    #[test]
    fn effort_unknown_tier_is_inert() {
        // A garbage tier must never produce a malformed body that would 400 — leave it untouched.
        let mut body = base_body();
        apply_effort(&mut body, "ultra");
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn with_effort_threads_model_and_effort() {
        let p = AnthropicProvider::with_effort(None, String::new(), "high".to_string());
        // Empty model falls back to the default; effort is retained.
        assert_eq!(p.model, DEFAULT_MODEL);
        assert_eq!(p.effort, "high");
        // The plain constructor defaults effort to provider-default (empty).
        let p = AnthropicProvider::new(None, "claude-haiku-4-5".to_string());
        assert_eq!(p.model, "claude-haiku-4-5");
        assert_eq!(p.effort, "");
    }
}
