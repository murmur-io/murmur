//! OpenAI-compatible "AI Gateway" provider + URL guardrails.
//!
//! Security invariants:
//!   R1 — the provider is cloud-classified and consent-gated even on loopback (a local gateway
//!         can forward to the cloud). Consent gate is enforced by `make_provider`, not here.
//!   R2 — the provider is always wrapped in `RedactingProvider` (enforced by `make_provider`).
//!   R3 — the API key is the GATEWAY key only; it NEVER falls back to the Anthropic key.
//!   R4 — http:// is rejected for non-loopback URLs; validated at construction in `new()`.
use async_trait::async_trait;
use serde_json::Value;

use crate::error::{AppError, Result};
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

/// Extract a useful error message from an OpenAI-format error envelope, or return `None` if the
/// body is not parseable or does not have `error.message`.
fn extract_gateway_error(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    v.get("error")?.get("message")?.as_str().map(str::to_string)
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

    /// POST to `{base}/chat/completions`, set the `Authorization` header only when a key is
    /// present (R3), and extract the first choice's `message.content`. Maps non-2xx responses to
    /// `AppError::Summarize` with the gateway's `error.message` when available.
    async fn call(&self, system: &str, user: &str) -> Result<String> {
        let url = self.chat_endpoint()?;

        let body = chat_body(&self.model, system, user);

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
            let detail =
                extract_gateway_error(&err_body).unwrap_or_else(|| format!("HTTP {status}"));
            return Err(AppError::Summarize(format!("gateway: {}", detail.trim())));
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| AppError::Summarize(format!("failed to parse gateway response: {e}")))?;

        let text = json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|t| t.as_str())
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
        Ok(note.to_string())
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
        let system_prompt = if req.template.trim().is_empty() {
            template::default_template()
        } else {
            req.template.clone()
        };
        let user_content = template::render_user_content(req);
        self.call(&system_prompt, &user_content).await
    }

    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        self.call(system, user).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Task 1.2 — gateway error extraction from an OpenAI-format error envelope.
    #[test]
    fn gateway_error_extraction() {
        let body = r#"{"error":{"message":"model 'x' not found","type":"invalid_request_error"}}"#;
        assert_eq!(extract_gateway_error(body).as_deref(), Some("model 'x' not found"));
        // No error field → None (callers fall back to a generic message).
        assert!(extract_gateway_error("{}").is_none());
        assert!(extract_gateway_error("not json").is_none());
    }
}
