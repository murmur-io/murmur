use std::sync::Arc;

use crate::settings::AppConfig;
use crate::summarize::anthropic::AnthropicProvider;
use crate::summarize::claude_code::ClaudeCodeProvider;
use crate::summarize::ollama::OllamaProvider;
use crate::summarize::provider::SummarizerProvider;

pub mod action_items;
pub mod anthropic;
pub mod gateway;
pub mod brief;
pub mod chat;
pub mod claude_code;
pub mod digest;
pub mod dossier;
pub mod graph;
/// The REAL on-device PERSON-name NER redactor (Phase D). ALWAYS compiled; the real impl is selected
/// at runtime by `redact::active_name_redactor` when the NER model dir is present, else the
/// byte-identical `NoopNameRedactor` (so a no-model build's name egress is unchanged).
pub mod ner_deberta;
pub mod ollama;
pub mod organize;
pub mod provider;
pub mod recipes;
pub mod redact;
pub mod related_context;
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

/// Egress classification for `make_provider`. claude_code/anthropic always send content
/// off-device. ollama is local ONLY when its base URL host is loopback — a remote `ollama_base_url`
/// is cloud egress and MUST be redacted + consent-gated. Unknown ids default to cloud (fail-safe).
pub(crate) fn egress_is_cloud(id: &str, config: &AppConfig) -> bool {
    match id {
        PROVIDER_CLAUDE_CODE | PROVIDER_ANTHROPIC => true,
        PROVIDER_OLLAMA => match reqwest::Url::parse(&config.ollama_base_url) {
            Ok(u) => !gateway::host_is_loopback(&u),
            Err(_) => true, // unparseable → fail safe (treat as cloud)
        },
        _ => true, // any future provider id defaults to cloud
    }
}

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
    // E10 — fail-closed consent gate, now classification-aware: no cloud provider is built (so no
    // content can be sent) until the user has explicitly consented once. ollama is gated ONLY when
    // its base URL is non-loopback (remote) — closing the gap where a remote ollama_base_url would
    // bypass the redaction firewall and consent check.
    if egress_is_cloud(id, config) && !config.cloud_egress_consented {
        return Err(crate::error::AppError::Unavailable(
            "cloud egress not consented: this provider sends meeting content off-device; \
             grant one-time consent before using it"
                .to_string(),
        ));
    }

    let inner: Arc<dyn SummarizerProvider> = match id {
        PROVIDER_CLAUDE_CODE => Arc::new(
            ClaudeCodeProvider::with_binary(config.claude_binary.clone())
                // Brain/AI model picker: a chosen model is passed as `--model`; an empty value
                // (the default) lets the CLI use its own default. Effort is N/A for the CLI.
                .with_model(config.provider_model.clone())
                // Opt-in: inherit the shell env (restores env ANTHROPIC_API_KEY); DB keys stay stripped.
                .with_inherit_env(config.claude_code_inherit_env),
        ),
        PROVIDER_ANTHROPIC => {
            // Resolve the key from the Keychain here so providers never touch secrets.
            let api_key = crate::secrets::get_secret(ANTHROPIC_KEY_ACCOUNT)?;
            // Brain/AI model picker takes precedence over the legacy `anthropic_model`; effort is
            // the adaptive-thinking tier (provider default when empty).
            let model = if config.provider_model.trim().is_empty() {
                config.anthropic_model.clone()
            } else {
                config.provider_model.clone()
            };
            Arc::new(AnthropicProvider::with_effort(
                api_key,
                model,
                config.provider_effort.clone(),
            ))
        }
        PROVIDER_OLLAMA => {
            let ollama = Arc::new(OllamaProvider::new(
                config.ollama_base_url.clone(),
                config.ollama_model.clone(),
            ));
            if !egress_is_cloud(id, config) {
                return Ok(ollama); // LOCAL ollama: unwrapped, unchanged behavior
            }
            ollama // REMOTE ollama: falls through to the RedactingProvider wrap below
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
    //
    // Phase D — the name layer is now the ACTIVE on-device redactor: when the NER model is present,
    // `active_name_redactor()` returns the real DebertaNameRedactor (PERSON names → ⟪NAME_n⟫ before
    // egress, restored in the reply); otherwise it is the byte-identical NoopNameRedactor, so a
    // no-model build's egress is unchanged. The redactor only ever REMOVES content (a NER miss leaks
    // no more than the no-op).
    Ok(Arc::new(
        crate::summarize::redact::RedactingProvider::with_name_redactor(
            inner,
            crate::summarize::redact::active_name_redactor(),
        ),
    ))
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

    let anthropic_model = if config.provider_model.trim().is_empty() {
        config.anthropic_model.clone()
    } else {
        config.provider_model.clone()
    };
    vec![
        Arc::new(
            ClaudeCodeProvider::with_binary(config.claude_binary.clone())
                .with_model(config.provider_model.clone())
                .with_inherit_env(config.claude_code_inherit_env),
        ),
        Arc::new(AnthropicProvider::with_effort(
            anthropic_key,
            anthropic_model,
            config.provider_effort.clone(),
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

    #[test]
    fn egress_is_cloud_classification() {
        // claude_code and anthropic are always cloud.
        let cfg = AppConfig::default();
        assert!(egress_is_cloud(PROVIDER_CLAUDE_CODE, &cfg));
        assert!(egress_is_cloud(PROVIDER_ANTHROPIC, &cfg));

        // ollama with default localhost URL is NOT cloud.
        assert!(!egress_is_cloud(PROVIDER_OLLAMA, &cfg));

        // ollama with a remote URL IS cloud.
        let mut remote_cfg = AppConfig::default();
        remote_cfg.ollama_base_url = "https://ollama.remote.example/api".into();
        assert!(egress_is_cloud(PROVIDER_OLLAMA, &remote_cfg));

        // ollama with an unparseable URL fails safe (treated as cloud).
        let mut bad_cfg = AppConfig::default();
        bad_cfg.ollama_base_url = "not a url".into();
        assert!(egress_is_cloud(PROVIDER_OLLAMA, &bad_cfg));

        // Unknown provider ids default to cloud (fail-safe).
        assert!(egress_is_cloud("unknown-provider", &cfg));
    }

    #[test]
    fn remote_ollama_requires_consent() {
        let mut cfg = AppConfig::default();
        cfg.ollama_base_url = "https://ollama.remote.example/api".into();
        cfg.cloud_egress_consented = false;
        let res = make_provider(PROVIDER_OLLAMA, &cfg);
        assert!(
            matches!(res, Err(crate::error::AppError::Unavailable(_))),
            "expected Unavailable for remote ollama without consent"
        );
    }

    #[test]
    fn local_ollama_stays_unwrapped_and_ungated() {
        let mut cfg = AppConfig::default();
        cfg.ollama_base_url = "http://localhost:11434".into();
        cfg.cloud_egress_consented = false;
        // local ollama must build without consent
        assert!(make_provider(PROVIDER_OLLAMA, &cfg).is_ok());
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
