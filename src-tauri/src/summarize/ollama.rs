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
/// Overall timeout for the `/api/generate` calls (`summarize` / `complete`). The other providers
/// are already bounded (anthropic 120 s via `build_client`, claude_code 180 s via
/// `CLAUDE_TIMEOUT`); Ollama was NOT, so a dead/wedged local server hung a summarize forever.
/// Local models on modest hardware can be genuinely slow on a long transcript, so this is
/// deliberately the most generous of the three — but still bounded.
const GENERATE_TIMEOUT: Duration = Duration::from_secs(600);

/// Talks to a local Ollama server's HTTP API (default `http://localhost:11434`).
pub struct OllamaProvider {
    client: reqwest::Client,
    base_url: String,
    model: String,
    /// Per-request cap on `/api/generate` ([`GENERATE_TIMEOUT`]; overridable in tests so the
    /// stalled-server regression doesn't wait 10 minutes).
    generate_timeout: Duration,
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
            generate_timeout: GENERATE_TIMEOUT,
        }
    }

    /// Test-only: shrink the `/api/generate` timeout so a stalled-server regression fails fast.
    #[cfg(test)]
    fn with_generate_timeout(mut self, timeout: Duration) -> Self {
        self.generate_timeout = timeout;
        self
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
                AppError::Unavailable(format!("Ollama not reachable at {} ({e})", self.base_url))
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
            .timeout(self.generate_timeout)
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
            .timeout(self.generate_timeout)
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
        assert!(
            names.is_empty(),
            "malformed body must degrade to an empty list"
        );
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

    /// Hardening item 2 (RED-before-GREEN, Ollama leg): a local server that ACCEPTS the
    /// `/api/generate` POST and then never answers must fail `summarize` within the request
    /// timeout. Pre-fix, the generate call carried NO timeout (unlike anthropic's 120 s /
    /// claude_code's 180 s), so a dead/wedged local server hung a summarize forever. The outer
    /// 10 s `tokio::time::timeout` is the RED detector: unpatched, it fires (test FAILs); with
    /// `.timeout(self.generate_timeout)` the request errors in ~0.4 s (PASS).
    #[tokio::test]
    async fn summarize_times_out_on_a_stalled_server_instead_of_hanging() {
        use crate::summarize::provider::{MeetingMeta, SummarizeRequest, SummarizerProvider};
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        // Accept ONE connection, read the request, then go silent (socket held open) for LONGER
        // than the outer 10 s RED detector — a shorter stall would end in a connection close,
        // which even the unpatched (timeout-less) code surfaces as an error, masking the hang.
        // The handle is dropped (not joined) — the thread dies with the test process.
        let _server = std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 8192];
                let _ = sock.read(&mut buf);
                std::thread::sleep(std::time::Duration::from_secs(30)); // never respond
            }
        });

        let provider = OllamaProvider::new(format!("http://{addr}"), "llama3.1".to_string())
            .with_generate_timeout(std::time::Duration::from_millis(400));
        let req = SummarizeRequest {
            transcript: "We shipped v2 and agreed Anna owns the rollout.".to_string(),
            meta: MeetingMeta {
                date_iso: "2026-07-16".to_string(),
                title_hint: None,
                duration_s: 60,
                language: Some("en".to_string()),
            },
            template: "TEMPLATE".to_string(),
            vault_titles: vec![],
            related_context: None,
            user_notes: None,
            live_bullets: None,
        };
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            provider.summarize(&req),
        )
        .await
        .expect("a stalled Ollama server must RESOLVE (as Err) within the timeout — not hang");
        assert!(res.is_err(), "a stalled generate must surface an error");
    }
}
