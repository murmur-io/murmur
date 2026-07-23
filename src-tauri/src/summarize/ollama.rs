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
/// Recording postprocess must not pin the coordinator for ten minutes behind a wedged local
/// daemon. `keep_alive: 0` below requests immediate unload after the bounded response.
const RECORDING_GENERATE_TIMEOUT: Duration = Duration::from_secs(180);
const UNCERTAIN_UNLOAD_TIMEOUT: Duration = Duration::from_secs(2);

fn effective_generate_timeout(configured: Duration, recording_owned: bool) -> Duration {
    if recording_owned {
        configured.min(RECORDING_GENERATE_TIMEOUT)
    } else {
        configured
    }
}

/// Talks to a local Ollama server's HTTP API (default `http://localhost:11434`).
pub struct OllamaProvider {
    client: reqwest::Client,
    base_url: String,
    model: String,
    /// Per-request cap on `/api/generate` ([`GENERATE_TIMEOUT`]; overridable in tests so the
    /// stalled-server regression doesn't wait 10 minutes).
    generate_timeout: Duration,
    local_model_admission: bool,
    recording_token: Option<crate::perf::RecordingSessionToken>,
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
            local_model_admission: false,
            recording_token: None,
        }
    }

    pub(crate) fn with_model_admission(
        base_url: String,
        model: String,
        local_model_admission: bool,
        recording_token: Option<crate::perf::RecordingSessionToken>,
    ) -> Self {
        let mut provider = Self::new(base_url, model);
        provider.local_model_admission = local_model_admission;
        provider.recording_token = recording_token;
        provider.generate_timeout = effective_generate_timeout(
            provider.generate_timeout,
            provider.local_model_admission && provider.recording_token.is_some(),
        );
        provider
    }

    /// Loopback/local Ollama needs a detached owner for daemon cleanup and model-lease quarantine.
    /// Remote Ollama is cloud egress: it must remain structurally inside the redaction wrapper's
    /// affine egress lease, so cancellation drops the HTTP future instead of leaving content-bearing
    /// network work detached after that lease is gone.
    async fn generate_owned(
        &self,
        body: serde_json::Value,
    ) -> crate::error::Result<RawGenerateResponse> {
        let task = OwnedGenerate {
            client: self.client.clone(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            timeout: self.generate_timeout,
            local_model_admission: self.local_model_admission,
            recording_token: self.recording_token.clone(),
            body,
            cleanup_timeout: UNCERTAIN_UNLOAD_TIMEOUT,
            #[cfg(test)]
            completion: None,
        };
        if self.local_model_admission {
            tokio::spawn(task.run()).await.map_err(|e| {
                AppError::Other(anyhow::anyhow!("Ollama generation owner panicked: {e}"))
            })?
        } else {
            task.run().await
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

struct RawGenerateResponse {
    status: reqwest::StatusCode,
    body: Vec<u8>,
}

#[derive(Debug, Deserialize, Default)]
struct UnloadAck {
    #[serde(default)]
    done: bool,
    #[serde(default)]
    done_reason: Option<String>,
}

fn verified_unload_body(body: &[u8]) -> bool {
    serde_json::from_slice::<UnloadAck>(body).is_ok_and(|ack| {
        ack.done
            && ack
                .done_reason
                .as_deref()
                .map_or(true, |reason| reason == "unload")
    })
}

struct OwnedGenerate {
    client: reqwest::Client,
    base_url: String,
    model: String,
    timeout: Duration,
    local_model_admission: bool,
    recording_token: Option<crate::perf::RecordingSessionToken>,
    body: serde_json::Value,
    cleanup_timeout: Duration,
    #[cfg(test)]
    completion: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

#[cfg(test)]
struct CompletionGuard(Option<std::sync::Arc<std::sync::atomic::AtomicBool>>);

#[cfg(test)]
impl Drop for CompletionGuard {
    fn drop(&mut self) {
        if let Some(done) = self.0.as_ref() {
            done.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

impl OwnedGenerate {
    fn recovery_key(&self) -> String {
        ollama_recovery_key(&self.base_url, &self.model)
    }

    async fn verified_unload(&self, url: &str) -> bool {
        let unload = async {
            let response = self
                .client
                .post(url)
                .json(&json!({ "model": self.model, "keep_alive": 0 }))
                .timeout(self.cleanup_timeout)
                .send()
                .await;
            let Ok(response) = response else {
                return false;
            };
            if !response.status().is_success() {
                return false;
            }
            // Fully consume the response: status headers alone do not prove the daemon completed
            // the unload operation rather than disconnecting mid-command.
            response
                .bytes()
                .await
                .is_ok_and(|body| verified_unload_body(&body))
        };
        tokio::time::timeout(self.cleanup_timeout, unload)
            .await
            .unwrap_or(false)
    }

    fn quarantine_uncertain_runtime(&self, recovery_key: String) -> crate::error::Result<()> {
        crate::perf::quarantine_resident_model(crate::perf::ResidentModelKind::Ollama, recovery_key)
    }

    async fn run(self) -> crate::error::Result<RawGenerateResponse> {
        #[cfg(test)]
        let _completion = CompletionGuard(self.completion.clone());
        let url = format!("{}/api/generate", self.base_url);
        let recovery_key = self.recovery_key();
        if self.local_model_admission {
            if let Some(quarantined_key) =
                crate::perf::resident_model_quarantine_key(crate::perf::ResidentModelKind::Ollama)
            {
                if quarantined_key != recovery_key {
                    return Err(AppError::Unavailable(
                        "local-model residency is quarantined by a different uncertain Ollama runtime"
                            .into(),
                    ));
                }
                if !self.verified_unload(&url).await {
                    return Err(AppError::Unavailable(
                        "uncertain Ollama generation is still quarantined; verified unload failed"
                            .into(),
                    ));
                }
                crate::perf::clear_resident_model_quarantine(
                    crate::perf::ResidentModelKind::Ollama,
                    &recovery_key,
                )?;
            }
        }

        // Cache eviction may poll/reap the GGUF child, so acquire the kind-tagged lease on the
        // blocking pool. The returned lease is then owned by THIS detached async task.
        let generation = if self.local_model_admission {
            let token = self.recording_token.clone();
            Some(
                tokio::task::spawn_blocking(move || match token.as_ref() {
                    Some(token) => crate::perf::acquire_recording_model_generation(
                        token,
                        crate::perf::ResidentModelKind::Ollama,
                    ),
                    None => crate::perf::acquire_unscoped_model_generation(
                        crate::perf::ResidentModelKind::Ollama,
                    ),
                })
                .await
                .map_err(|e| {
                    AppError::Other(anyhow::anyhow!(
                        "Ollama model-admission worker panicked: {e}"
                    ))
                })??,
            )
        } else {
            None
        };

        let sent = self
            .client
            .post(&url)
            .json(&self.body)
            .timeout(self.timeout)
            .send()
            .await;
        let response = match sent {
            Ok(response) => response,
            Err(error) => {
                // A timed-out/disconnected request may still be running daemon-side. Keep the
                // lease while asking Ollama, content-free and bounded, to evict this exact model.
                // If that proof also fails, poison the lane BEFORE releasing the lease.
                if self.local_model_admission && !self.verified_unload(&url).await {
                    self.quarantine_uncertain_runtime(recovery_key.clone())?;
                }
                drop(generation);
                return Err(AppError::Summarize(format!(
                    "Ollama request failed: {error}"
                )));
            }
        };
        let status = response.status();
        let body = match response.bytes().await {
            Ok(body) => body.to_vec(),
            Err(error) => {
                // Broken/truncated response bodies are cancellation-shaped too. Ask the daemon to
                // unload before releasing ownership, just like the request-timeout path above.
                if self.local_model_admission && !self.verified_unload(&url).await {
                    self.quarantine_uncertain_runtime(recovery_key)?;
                }
                drop(generation);
                return Err(AppError::Summarize(format!(
                    "failed to read Ollama response: {error}"
                )));
            }
        };
        drop(generation);
        Ok(RawGenerateResponse { status, body })
    }
}

fn ollama_recovery_key(base_url: &str, model: &str) -> String {
    format!("{}:{}{}", base_url.len(), base_url, model)
}

/// Parse only after the owned generation task has dropped its model lease.
fn parse_generate_response(raw: RawGenerateResponse) -> crate::error::Result<String> {
    if !raw.status.is_success() {
        return Err(AppError::Summarize(format!(
            "Ollama API returned {}: {}",
            raw.status,
            String::from_utf8_lossy(&raw.body).trim()
        )));
    }
    let parsed: GenerateResponse = serde_json::from_slice(&raw.body)
        .map_err(|e| AppError::Summarize(format!("failed to parse Ollama response: {e}")))?;
    Ok(parsed.response)
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

fn generate_body(model: &str, prompt: &str, system: Option<&str>) -> serde_json::Value {
    match system {
        Some(system) => json!({
            "model": model,
            "system": system,
            "prompt": prompt,
            "stream": false,
            "keep_alive": 0,
        }),
        None => json!({
            "model": model,
            "prompt": prompt,
            "stream": false,
            "keep_alive": 0,
        }),
    }
}

fn loopback_ollama_base_url(base_url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(base_url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !crate::summarize::gateway::host_is_loopback(&parsed)
    {
        return None;
    }
    Some(base_url.trim_end_matches('/').to_string())
}

fn configured_ollama_models(config: &crate::settings::AppConfig) -> Vec<String> {
    let mut models = std::collections::BTreeSet::new();
    for role in [
        crate::summarize::roles::Role::Notes,
        crate::summarize::roles::Role::Ask,
        crate::summarize::roles::Role::Live,
    ] {
        let target = crate::summarize::roles::provider_target(role, config);
        if target.connection == crate::summarize::PROVIDER_OLLAMA {
            let model = crate::summarize::effective_model_requested(&target, config);
            if !model.trim().is_empty() {
                models.insert(model);
            }
        }
    }
    models.into_iter().collect()
}

/// Best-effort Starting-phase eviction of models a previous loopback Ollama call may have left
/// resident under its default keep-alive. The endpoint classification is fail-closed: malformed or
/// non-loopback URLs cause zero network calls. All configured role models share one ~2s total bound.
pub(crate) async fn unload_local_models_for_recording(
    config: &crate::settings::AppConfig,
) -> usize {
    let Some(base_url) = loopback_ollama_base_url(&config.ollama_base_url) else {
        return 0;
    };
    let models = configured_ollama_models(config);
    if models.is_empty() {
        return 0;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let client = reqwest::Client::new();
    let mut unloaded = 0usize;
    for model in models {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let request = client
            .post(format!("{base_url}/api/generate"))
            .json(&json!({ "model": model, "keep_alive": 0 }))
            .timeout(remaining)
            .send();
        match tokio::time::timeout(remaining, request).await {
            Ok(Ok(response)) if response.status().is_success() => {
                let body_budget = deadline.saturating_duration_since(std::time::Instant::now());
                let fully_read = !body_budget.is_zero()
                    && tokio::time::timeout(body_budget, response.bytes())
                        .await
                        .is_ok_and(|body| body.is_ok_and(|bytes| verified_unload_body(&bytes)));
                if fully_read {
                    unloaded += 1;
                    let recovery_key = ollama_recovery_key(&base_url, &model);
                    if crate::perf::resident_model_quarantine_key(
                        crate::perf::ResidentModelKind::Ollama,
                    )
                    .as_deref()
                        == Some(recovery_key.as_str())
                    {
                        let _ = crate::perf::clear_resident_model_quarantine(
                            crate::perf::ResidentModelKind::Ollama,
                            &recovery_key,
                        );
                    }
                }
            }
            Ok(Ok(response)) => {
                tracing::debug!(target: "ollama", status = %response.status(), "recording preflight Ollama unload refused");
            }
            Ok(Err(e)) => {
                tracing::debug!(target: "ollama", error = %e, "recording preflight Ollama unload unavailable");
            }
            Err(_) => {
                tracing::debug!(target: "ollama", "recording preflight Ollama unload timed out");
                break;
            }
        }
    }
    unloaded
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
        // prompt (instructions + metadata + vault titles + transcript). Build all content BEFORE
        // admission so the residency lane covers only daemon inference/response/unload.
        let prompt = template::render_prompt(req);
        let body = generate_body(&self.model, &prompt, None);
        let response = parse_generate_response(self.generate_owned(body).await?)?;
        let note = response.trim_start_matches('\u{feff}').trim();
        if note.is_empty() {
            return Err(AppError::Summarize(
                "Ollama response contained no text".to_string(),
            ));
        }

        Ok(note.to_string())
    }

    async fn complete(&self, system: &str, user: &str) -> crate::error::Result<String> {
        // Own prompt strings/body before admission; no formatting or parsing pins residency.
        let body = generate_body(&self.model, user, Some(system));
        Ok(parse_generate_response(self.generate_owned(body).await?)?
            .trim()
            .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_generate_body_requests_immediate_model_unload() {
        for body in [
            generate_body("m", "prompt", None),
            generate_body("m", "prompt", Some("system")),
        ] {
            assert_eq!(body.get("keep_alive"), Some(&serde_json::json!(0)));
            assert_eq!(body.get("stream"), Some(&serde_json::json!(false)));
        }
    }

    #[test]
    fn quarantine_clears_only_on_an_explicit_unload_ack() {
        assert!(verified_unload_body(
            br#"{"done":true,"done_reason":"unload"}"#
        ));
        assert!(verified_unload_body(br#"{"done":true}"#));
        assert!(!verified_unload_body(br#"{}"#));
        assert!(!verified_unload_body(
            br#"{"done":true,"done_reason":"stop"}"#
        ));
    }

    #[test]
    fn recording_generation_uses_the_stricter_bounded_timeout() {
        assert_eq!(
            effective_generate_timeout(Duration::from_secs(600), true),
            RECORDING_GENERATE_TIMEOUT
        );
        assert_eq!(
            effective_generate_timeout(Duration::from_secs(30), true),
            Duration::from_secs(30)
        );
        assert_eq!(
            effective_generate_timeout(Duration::from_secs(600), false),
            Duration::from_secs(600)
        );
    }

    #[tokio::test]
    async fn owned_generation_survives_waiter_cancellation_until_response() {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let _ = socket.read(&mut buf);
            let _ = accepted_tx.send(());
            release_rx.recv().unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 17\r\nConnection: close\r\n\r\n{\"response\":\"ok\"}",
                )
                .unwrap();
        });
        let completed = Arc::new(AtomicBool::new(false));
        let owner = OwnedGenerate {
            client: reqwest::Client::new(),
            base_url: format!("http://{addr}"),
            model: "m".into(),
            timeout: Duration::from_secs(2),
            local_model_admission: false,
            recording_token: None,
            body: generate_body("m", "prompt", None),
            cleanup_timeout: UNCERTAIN_UNLOAD_TIMEOUT,
            completion: Some(Arc::clone(&completed)),
        };
        let waiter = tokio::spawn(async move { tokio::spawn(owner.run()).await });
        accepted_rx.await.unwrap();
        waiter.abort();
        release_tx.send(()).unwrap();
        server.join().unwrap();
        for _ in 0..100 {
            if completed.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            completed.load(Ordering::SeqCst),
            "dropping the waiter must not cancel the owned generation lifecycle"
        );
    }

    /// RED-before-GREEN: if both the content-bearing request and the content-free unload proof
    /// stall, the generation lease may end but the process-wide residency lane remains poisoned.
    /// An incompatible model must not overlap a daemon generation we cannot disprove.
    #[test]
    fn failed_timeout_cleanup_quarantines_incompatible_model_admission() {
        use std::io::Read;

        struct ResetLifecycle;
        impl Drop for ResetLifecycle {
            fn drop(&mut self) {
                crate::perf::reset_model_lifecycle_for_test();
            }
        }
        let _serial = crate::perf::model_lifecycle_test_guard();
        crate::perf::reset_model_lifecycle_for_test();
        let _reset = ResetLifecycle;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let _server = std::thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let mut request = [0u8; 8192];
                let _ = socket.read(&mut request);
                // Hold the original request open. The cleanup connection can enter the listen
                // backlog, but receives no response within its own small bound either.
                std::thread::sleep(Duration::from_millis(500));
            }
        });

        let base_url = format!("http://{addr}");
        let recovery_key = ollama_recovery_key(&base_url, "m");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(
            OwnedGenerate {
                client: reqwest::Client::new(),
                base_url,
                model: "m".into(),
                timeout: Duration::from_millis(40),
                local_model_admission: true,
                recording_token: None,
                body: generate_body("m", "prompt", None),
                cleanup_timeout: Duration::from_millis(40),
                completion: None,
            }
            .run(),
        );
        assert!(result.is_err());
        assert_eq!(
            crate::perf::resident_model_quarantine_key(crate::perf::ResidentModelKind::Ollama)
                .as_deref(),
            Some(recovery_key.as_str())
        );
        assert!(crate::perf::acquire_unscoped_model_generation(
            crate::perf::ResidentModelKind::Whisper
        )
        .is_err());

        crate::perf::clear_resident_model_quarantine(
            crate::perf::ResidentModelKind::Ollama,
            &recovery_key,
        )
        .unwrap();
    }

    #[test]
    fn recording_unload_classifies_only_loopback_ollama_urls() {
        assert_eq!(
            loopback_ollama_base_url("http://localhost:11434/"),
            Some("http://localhost:11434".to_string())
        );
        assert!(loopback_ollama_base_url("http://127.0.0.1:11434").is_some());
        assert!(loopback_ollama_base_url("http://[::1]:11434").is_some());
        assert!(loopback_ollama_base_url("https://ollama.example.com").is_none());
        assert!(loopback_ollama_base_url("not a url").is_none());
    }

    #[tokio::test]
    async fn recording_preflight_sends_content_free_keep_alive_zero_to_loopback() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let read = socket.read(&mut buf).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..read]);
                let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_len = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_len {
                    break;
                }
            }
            let response_body = r#"{"done":true,"done_reason":"unload"}"#;
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            )
            .unwrap();
            String::from_utf8(request).unwrap()
        });

        let config = crate::settings::AppConfig {
            provider_id: crate::summarize::PROVIDER_OLLAMA.to_string(),
            ollama_base_url: format!("http://{addr}"),
            ollama_model: "murmur-test-model".to_string(),
            ..crate::settings::AppConfig::default()
        };
        assert_eq!(unload_local_models_for_recording(&config).await, 1);
        let request = server.join().unwrap();
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(
            json.get("model"),
            Some(&serde_json::json!("murmur-test-model"))
        );
        assert_eq!(json.get("keep_alive"), Some(&serde_json::json!(0)));
        assert!(
            json.get("prompt").is_none(),
            "unload must carry no user content"
        );
    }

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
        let res =
            tokio::time::timeout(std::time::Duration::from_secs(10), provider.summarize(&req))
                .await
                .expect(
                    "a stalled Ollama server must RESOLVE (as Err) within the timeout — not hang",
                );
        assert!(res.is_err(), "a stalled generate must surface an error");
    }
}
