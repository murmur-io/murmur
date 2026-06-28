use std::sync::Arc;

use crate::settings::AppConfig;
use crate::summarize::anthropic::AnthropicProvider;
use crate::summarize::claude_code::ClaudeCodeProvider;
use crate::summarize::ollama::OllamaProvider;
use crate::summarize::provider::SummarizerProvider;

pub mod action_items;
pub mod anthropic;
pub mod brief;
pub mod chat;
pub mod claude_code;
pub mod digest;
pub mod dossier;
pub mod graph;
pub mod ollama;
pub mod organize;
pub mod provider;
pub mod recipes;
pub mod redact;
pub mod template;
pub mod threads;
pub mod timeline;
pub mod vault_chat;
pub mod vault_context;

pub use provider::{Availability, MeetingMeta, SummarizeRequest, SummarizerProvider as _};

/// Default provider id when settings unset.
pub const DEFAULT_PROVIDER_ID: &str = "claude_code";

/// Stable provider ids (mirrors each provider's `id()`).
pub const PROVIDER_CLAUDE_CODE: &str = "claude_code";
pub const PROVIDER_ANTHROPIC: &str = "anthropic";
pub const PROVIDER_OLLAMA: &str = "ollama";

/// Keychain account under which the Anthropic API key is stored
/// (matches `set_anthropic_key` / `has_anthropic_key` in `commands.rs`).
pub const ANTHROPIC_KEY_ACCOUNT: &str = "anthropic_api_key";

/// True iff `id` names a provider that sends meeting content OFF-DEVICE to a cloud LLM.
///
/// `claude_code` shells out to the local `claude` CLI, but that CLI is a *thin client* for
/// Anthropic's hosted models — the transcript is uploaded to the cloud just like the direct
/// `anthropic` HTTP provider. Both are therefore "cloud" and MUST go through the redaction
/// firewall + consent gate. `ollama` runs the model locally (no egress) and is exempt.
fn is_cloud(id: &str) -> bool {
    matches!(id, PROVIDER_CLAUDE_CODE | PROVIDER_ANTHROPIC)
}

/// Build a provider by id, wiring config + secrets. Unknown id → `AppError::InvalidArg`.
///
/// Egress policy (E6/E7/E10): every cloud provider — `claude_code` AND `anthropic` — is wrapped
/// in [`RedactingProvider`] so high-confidence PII is scrubbed before any content leaves the
/// device, and is refused entirely until the user has granted one-time cloud-egress consent.
/// `ollama` is local-only and bypasses both.
pub fn make_provider(
    id: &str,
    config: &AppConfig,
) -> crate::error::Result<Arc<dyn SummarizerProvider>> {
    // E10 — fail-closed consent gate: no cloud provider is built (so no content can be sent)
    // until the user has explicitly consented once. ollama is local, so it is never gated.
    if is_cloud(id) && !config.cloud_egress_consented {
        return Err(crate::error::AppError::Unavailable(
            "cloud egress not consented: this provider sends meeting content to a cloud LLM; \
             grant one-time consent before using it"
                .to_string(),
        ));
    }

    let inner: Arc<dyn SummarizerProvider> = match id {
        PROVIDER_CLAUDE_CODE => Arc::new(ClaudeCodeProvider::with_binary(
            config.claude_binary.clone(),
        )),
        PROVIDER_ANTHROPIC => {
            // Resolve the key from the Keychain here so providers never touch secrets.
            let api_key = crate::secrets::get_secret(ANTHROPIC_KEY_ACCOUNT)?;
            Arc::new(AnthropicProvider::new(api_key, config.anthropic_model.clone()))
        }
        PROVIDER_OLLAMA => {
            return Ok(Arc::new(OllamaProvider::new(
                config.ollama_base_url.clone(),
                config.ollama_model.clone(),
            )))
        }
        other => {
            return Err(crate::error::AppError::InvalidArg(format!(
                "unknown provider id: {other}"
            )))
        }
    };

    // E6/E7 — redaction firewall on BOTH cloud providers: scrub emails/cards/phones before they
    // reach the cloud (restored in the reply). `claude_code` shells out to the local `claude`
    // CLI, but that CLI uploads to Anthropic's cloud, so it needs the firewall exactly as the
    // direct HTTP `anthropic` provider does. ollama (local) already returned above, unwrapped.
    Ok(Arc::new(crate::summarize::redact::RedactingProvider::new(
        inner,
    )))
}

/// All three provider instances (for availability fan-out in the Settings UI).
///
/// Best-effort: a failure to read the Anthropic key from the Keychain degrades to a
/// keyless `AnthropicProvider` (which then reports `Unavailable`) rather than failing the
/// whole fan-out.
pub fn all_providers(config: &AppConfig) -> Vec<Arc<dyn SummarizerProvider>> {
    let anthropic_key = crate::secrets::get_secret(ANTHROPIC_KEY_ACCOUNT)
        .ok()
        .flatten();

    vec![
        Arc::new(ClaudeCodeProvider::with_binary(config.claude_binary.clone())),
        Arc::new(AnthropicProvider::new(
            anthropic_key,
            config.anthropic_model.clone(),
        )),
        Arc::new(OllamaProvider::new(
            config.ollama_base_url.clone(),
            config.ollama_model.clone(),
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_cloud_classification() {
        assert!(is_cloud(PROVIDER_CLAUDE_CODE));
        assert!(is_cloud(PROVIDER_ANTHROPIC));
        assert!(!is_cloud(PROVIDER_OLLAMA));
        assert!(!is_cloud("something-else"));
    }

    fn consented_config() -> AppConfig {
        AppConfig {
            cloud_egress_consented: true,
            ..Default::default()
        }
    }

    #[test]
    fn cloud_providers_are_redaction_wrapped() {
        // Both cloud providers must be wrapped so PII is scrubbed before egress. The wrapper is
        // transparent to `id()`, so we assert construction succeeds (with consent granted) and
        // the wrapped provider reports the inner id.
        let cfg = consented_config();
        let cc = make_provider(PROVIDER_CLAUDE_CODE, &cfg).unwrap();
        assert_eq!(cc.id(), PROVIDER_CLAUDE_CODE);
        let an = make_provider(PROVIDER_ANTHROPIC, &cfg).unwrap();
        assert_eq!(an.id(), PROVIDER_ANTHROPIC);
    }

    #[test]
    fn ollama_is_not_consent_gated() {
        // ollama is local-only: it builds even with consent OFF (the default).
        let cfg = AppConfig::default();
        assert!(!cfg.cloud_egress_consented);
        let ol = make_provider(PROVIDER_OLLAMA, &cfg).unwrap();
        assert_eq!(ol.id(), PROVIDER_OLLAMA);
    }

    #[test]
    fn cloud_providers_refused_without_consent() {
        // Fail-closed: neither cloud provider can be built until consent is granted, so no
        // content can ever be sent before the user has acknowledged egress.
        let cfg = AppConfig::default(); // consent OFF
        for id in [PROVIDER_CLAUDE_CODE, PROVIDER_ANTHROPIC] {
            // `dyn SummarizerProvider` isn't Debug, so inspect the Result without `{:?}`.
            let res = make_provider(id, &cfg);
            assert!(
                matches!(res, Err(crate::error::AppError::Unavailable(_))),
                "expected Unavailable for {id} without consent (got Ok or wrong error)"
            );
        }
    }
}
