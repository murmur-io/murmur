use serde::Deserialize;
use serde_json::json;

use crate::error::AppError;
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
}

impl AnthropicProvider {
    /// `api_key` already resolved from Keychain by the factory. `model` defaults to
    /// `claude-opus-4-8` when empty.
    pub fn new(api_key: Option<String>, model: String) -> Self {
        let model = if model.trim().is_empty() {
            DEFAULT_MODEL.to_string()
        } else {
            model
        };
        Self {
            client: reqwest::Client::new(),
            model,
            api_key,
        }
    }
}

/// Minimal mirror of the `/v1/messages` success response — we only need the text blocks.
#[derive(Debug, Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    text: Option<String>,
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
        let api_key = self.api_key.as_deref().filter(|k| !k.trim().is_empty()).ok_or_else(|| {
            AppError::Unavailable("Anthropic API key is not set".to_string())
        })?;

        let system_prompt = if req.template.trim().is_empty() {
            template::default_template()
        } else {
            req.template.clone()
        };
        let user_content = template::render_user_content(req);

        let body = json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "system": system_prompt,
            "messages": [
                { "role": "user", "content": user_content }
            ],
        });

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

        let parsed: MessagesResponse = resp
            .json()
            .await
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

        Ok(note.to_string())
    }

    async fn complete(&self, system: &str, user: &str) -> crate::error::Result<String> {
        let api_key = self
            .api_key
            .as_deref()
            .filter(|k| !k.trim().is_empty())
            .ok_or_else(|| AppError::Unavailable("Anthropic API key is not set".to_string()))?;

        let body = json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "system": system,
            "messages": [ { "role": "user", "content": user } ],
        });

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
            let err_body = resp.text().await.unwrap_or_default();
            let detail = extract_api_error(&err_body).unwrap_or_else(|| err_body.clone());
            return Err(AppError::Summarize(format!(
                "Anthropic API returned {status}: {}",
                detail.trim()
            )));
        }

        let parsed: MessagesResponse = resp
            .json()
            .await
            .map_err(|e| AppError::Summarize(format!("failed to parse Anthropic response: {e}")))?;
        let text: String = parsed
            .content
            .iter()
            .filter(|b| b.block_type == "text")
            .filter_map(|b| b.text.as_deref())
            .collect::<Vec<_>>()
            .join("");
        Ok(text.trim().to_string())
    }
}

/// Best-effort extraction of the `error.message` field from an Anthropic error body.
fn extract_api_error(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("error")?.get("message")?.as_str().map(str::to_string)
}
