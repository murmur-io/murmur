use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::error::AppError;
use crate::summarize::provider::*;
use crate::summarize::template;

const DEFAULT_BASE_URL: &str = "http://localhost:11434";
const DEFAULT_MODEL: &str = "llama3.1";
/// Short timeout for the readiness probe so the Settings UI fan-out stays snappy.
/// Ollama runs locally, so 500 ms is plenty when it's up and keeps the UI from
/// stalling when it's down (review SF-2).
const AVAILABILITY_TIMEOUT: Duration = Duration::from_millis(500);
/// Timeout for the `/api/tags` catalog fetch feeding the Settings model dropdown — longer than
/// the readiness probe (a cold Ollama can be slow to answer) but still bounded so the UI never
/// hangs on a dead server.
const LIST_MODELS_TIMEOUT: Duration = Duration::from_secs(5);

/// Talks to a local Ollama server's HTTP API (default `http://localhost:11434`).
pub struct OllamaProvider {
    client: reqwest::Client,
    base_url: String,
    model: String,
}

impl OllamaProvider {
    /// `model` e.g. "llama3.1". Empty args fall back to the defaults.
    pub fn new(base_url: String, model: String) -> Self {
        let base_url = if base_url.trim().is_empty() {
            DEFAULT_BASE_URL.to_string()
        } else {
            // Normalize a trailing slash so `{base}/api/...` joins cleanly.
            base_url.trim_end_matches('/').to_string()
        };
        let model = if model.trim().is_empty() {
            DEFAULT_MODEL.to_string()
        } else {
            model
        };
        Self {
            client: reqwest::Client::new(),
            base_url,
            model,
        }
    }

    /// `GET {base}/api/tags` → list of locally installed model names for the Settings dropdown.
    ///
    /// Inbound-only: sends NO meeting content — the request is a bare GET to the local (or
    /// user-configured) Ollama server and the response carries only model names. It therefore
    /// does NOT need the redaction firewall or the cloud-egress consent gate (same rationale as
    /// the gateway `list_models` / `list_gateway_models` precedent).
    ///
    /// A transport failure or non-2xx status maps to `AppError::Unavailable`; a malformed body
    /// degrades to an empty list (see [`parse_tags_response`]) — never a panic.
    pub async fn list_models(&self) -> crate::error::Result<Vec<String>> {
        let url = format!("{}/api/tags", self.base_url);
        let resp = self
            .client
            .get(&url)
            .timeout(LIST_MODELS_TIMEOUT)
            .send()
            .await
            .map_err(|e| {
                AppError::Unavailable(format!(
                    "Ollama not reachable at {} ({e})",
                    self.base_url
                ))
            })?;

        let status = resp.status();
        if !status.is_success() {
            return Err(AppError::Unavailable(format!(
                "Ollama at {} returned {status}",
                self.base_url
            )));
        }

        let body = resp.text().await.map_err(|e| {
            AppError::Unavailable(format!("failed to read Ollama /api/tags body: {e}"))
        })?;

        parse_tags_response(&body)
    }
}

/// Mirror of the `/api/generate` non-streaming response (only the fields we use).
#[derive(Debug, Deserialize)]
struct GenerateResponse {
    #[serde(default)]
    response: String,
}

/// Minimal mirror of the Ollama `/api/tags` response — only `models[].name` is extracted.
/// A missing or unrecognised body degrades to an empty list, never a panic.
#[derive(Debug, Deserialize, Default)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagEntry>,
}

#[derive(Debug, Deserialize)]
struct TagEntry {
    name: String,
}

/// Parse a raw Ollama `/api/tags` response body into a `Vec<String>` of model names.
///
/// A malformed or empty body returns `Ok(vec![])` — the caller (the FE model dropdown) handles
/// an empty list gracefully. Inbound-only metadata: NO meeting content is sent to produce this.
pub(crate) fn parse_tags_response(body: &str) -> crate::error::Result<Vec<String>> {
    let parsed: TagsResponse = serde_json::from_str(body).unwrap_or_default();
    Ok(parsed.models.into_iter().map(|m| m.name).collect())
}

#[async_trait::async_trait]
impl SummarizerProvider for OllamaProvider {
    fn id(&self) -> &str {
        "ollama"
    }

    /// Probe `GET {base}/api/tags`; reachable + 2xx → Available.
    async fn availability(&self) -> Availability {
        let url = format!("{}/api/tags", self.base_url);
        match self
            .client
            .get(&url)
            .timeout(AVAILABILITY_TIMEOUT)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => Availability::Available,
            Ok(resp) => Availability::Unavailable {
                reason: format!("Ollama at {} returned {}", self.base_url, resp.status()),
            },
            Err(e) => Availability::Unavailable {
                reason: format!("Ollama not reachable at {} ({e})", self.base_url),
            },
        }
    }

    /// POST `{base}/api/generate` with `{ model, prompt, stream:false }` where `prompt` is
    /// the full rendered template + meeting content; return the `response` text.
    async fn summarize(&self, req: &SummarizeRequest) -> crate::error::Result<String> {
        // Ollama's /api/generate has no separate system/user channel — send the combined
        // prompt (instructions + metadata + vault titles + transcript).
        let prompt = template::render_prompt(req);

        let url = format!("{}/api/generate", self.base_url);
        let body = json!({
            "model": self.model,
            "prompt": prompt,
            "stream": false,
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::Summarize(format!("Ollama request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            return Err(AppError::Summarize(format!(
                "Ollama API returned {status}: {}",
                err_body.trim()
            )));
        }

        let parsed: GenerateResponse = resp
            .json()
            .await
            .map_err(|e| AppError::Summarize(format!("failed to parse Ollama response: {e}")))?;

        let note = parsed.response.trim_start_matches('\u{feff}').trim();
        if note.is_empty() {
            return Err(AppError::Summarize(
                "Ollama response contained no text".to_string(),
            ));
        }

        Ok(note.to_string())
    }

    async fn complete(&self, system: &str, user: &str) -> crate::error::Result<String> {
        let url = format!("{}/api/generate", self.base_url);
        let body = json!({
            "model": self.model,
            "system": system,
            "prompt": user,
            "stream": false,
        });
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::Summarize(format!("Ollama request failed: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            return Err(AppError::Summarize(format!(
                "Ollama API returned {status}: {}",
                err_body.trim()
            )));
        }
        let parsed: GenerateResponse = resp
            .json()
            .await
            .map_err(|e| AppError::Summarize(format!("failed to parse Ollama response: {e}")))?;
        Ok(parsed.response.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real-shaped `/api/tags` body: names come back in order, extra fields are ignored.
    #[test]
    fn parse_tags_response_extracts_names() {
        let body = r#"{"models":[
            {"name":"llama3.1:latest","modified_at":"2026-01-01T00:00:00Z","size":4661224676},
            {"name":"qwen2.5:7b","modified_at":"2026-02-01T00:00:00Z","size":4431231234}
        ]}"#;
        let names = parse_tags_response(body).unwrap();
        assert_eq!(
            names,
            vec!["llama3.1:latest", "qwen2.5:7b"],
            "names must match fixture order"
        );
    }

    /// A malformed (non-JSON) body degrades to an empty list — never a panic.
    #[test]
    fn parse_tags_response_malformed_body_returns_empty() {
        let names = parse_tags_response("not json at all").unwrap();
        assert!(names.is_empty(), "malformed body must degrade to an empty list");
    }

    /// An empty models array returns an empty list cleanly.
    #[test]
    fn parse_tags_response_empty_models_returns_empty() {
        let names = parse_tags_response(r#"{"models":[]}"#).unwrap();
        assert!(names.is_empty(), "empty models must return an empty list");
    }

    /// A non-Ollama schema (e.g. the OpenAI `{"data":[…]}` shape) degrades to empty.
    #[test]
    fn parse_tags_response_non_ollama_schema_returns_empty() {
        let names = parse_tags_response(r#"{"data":[{"id":"gpt-4o"}]}"#).unwrap();
        assert!(
            names.is_empty(),
            "an OpenAI-shaped body must degrade to an empty list, not misparse"
        );
    }
}
