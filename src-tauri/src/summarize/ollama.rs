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
}

/// Mirror of the `/api/generate` non-streaming response (only the fields we use).
#[derive(Debug, Deserialize)]
struct GenerateResponse {
    #[serde(default)]
    response: String,
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
