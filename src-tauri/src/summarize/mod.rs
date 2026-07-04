use std::sync::Arc;

use crate::settings::AppConfig;
use crate::summarize::anthropic::AnthropicProvider;
use crate::summarize::claude_code::ClaudeCodeProvider;
use crate::summarize::ollama::OllamaProvider;
use crate::summarize::provider::SummarizerProvider;

pub mod action_items;
pub mod anthropic;
pub mod egress_log;
pub mod gateway;
pub mod meta;
pub mod brief;
pub mod chat;
pub mod claude_code;
pub mod digest;
pub mod dossier;
pub mod graph;
/// Tier 3b (B) anti-hallucination — DETERMINISTIC GROUNDING of the generated note against its own
/// transcript segments. Pure, on-device, zero-egress; annotates unsupported summary units with a
/// non-destructive `> unverified` marker.
pub mod grounding;
pub mod local;
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
pub mod roles;
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
/// OpenAI-compatible AI Gateway provider (LiteLLM / Kong / Portkey / vLLM / …).
pub const PROVIDER_GATEWAY: &str = "gateway";

/// Keychain account under which the Anthropic API key is stored
/// (matches `set_anthropic_key` / `has_anthropic_key` in `commands.rs`).
pub const ANTHROPIC_KEY_ACCOUNT: &str = "anthropic_api_key";
/// Keychain account under which the AI Gateway API key is stored.
/// Strictly separate from `ANTHROPIC_KEY_ACCOUNT` — never a fallback to the Anthropic key (R3).
pub const GATEWAY_KEY_ACCOUNT: &str = "gateway_api_key";

/// Egress classification for `make_provider`. claude_code/anthropic/gateway always send content
/// off-device. ollama is local ONLY when its base URL host is loopback — a remote `ollama_base_url`
/// is cloud egress and MUST be redacted + consent-gated. Unknown ids default to cloud (fail-safe).
///
/// NOTE: `gateway` is cloud even when its base URL is loopback — a localhost gateway can still
/// FORWARD to the cloud — so it is never consent-exempt and is always redaction-wrapped.
pub(crate) fn egress_is_cloud(id: &str, config: &AppConfig) -> bool {
    match id {
        PROVIDER_CLAUDE_CODE | PROVIDER_ANTHROPIC | PROVIDER_GATEWAY => true,
        PROVIDER_OLLAMA => match reqwest::Url::parse(&config.ollama_base_url) {
            Ok(u) => !gateway::host_is_loopback(&u),
            Err(_) => true, // unparseable → fail safe (treat as cloud)
        },
        // On-device connections egress NOTHING (spec §3.2; review code-truth #3 — load-bearing):
        // without this explicit arm the `_ => true` default would classify a `local` note as cloud,
        // demanding phantom consent, writing phantom ledger rows, and stamping a cloud host on the
        // Privacy Receipt. `off`/`apple` never reach the factory but are on-device by definition.
        roles::CONN_LOCAL | roles::CONN_OFF | roles::CONN_AFM => false,
        _ => true, // any future provider id defaults to cloud
    }
}

/// Build a provider by id, wiring config + secrets. Unknown id → `AppError::InvalidArg`.
///
/// Egress policy (E6/E7/E10): every cloud provider — `claude_code` AND `anthropic` — is wrapped
/// in [`RedactingProvider`] so high-confidence PII is scrubbed before any content leaves the
/// device, and is refused entirely until the user has granted one-time cloud-egress consent.
/// `ollama` is local-only and bypasses both.
///
/// Thin wrapper: delegates to [`make_provider_resolved`] with the LEGACY (model, effort) for `id`
/// — `provider_model`/`provider_effort` for the arms that historically read them, the
/// connection's own default otherwise — so every remaining direct caller is byte-identical to the
/// pre-role factory. Role-aware call sites use [`provider_for`] instead.
pub fn make_provider(
    id: &str,
    config: &AppConfig,
) -> crate::error::Result<Arc<dyn SummarizerProvider>> {
    // The legacy per-arm model semantics: only claude_code/anthropic ever read `provider_model`;
    // ollama/gateway resolve from ollama_model/gateway_model ("" = inherit below).
    let model = match id {
        PROVIDER_CLAUDE_CODE | PROVIDER_ANTHROPIC => config.provider_model.clone(),
        _ => String::new(),
    };
    let target = roles::RoleTarget {
        connection: id.to_string(),
        model,
        effort: config.provider_effort.clone(),
    };
    make_provider_resolved(&target, config)
}

/// Build the `SummarizerProvider` serving `role`, resolved through the role layer
/// ([`roles::provider_target`] — with role keys absent this is EXACTLY the legacy default
/// provider, so pre-role installs are byte-identical).
///
/// The consent gate, `egress_is_cloud` classification, `RedactingProvider` wrap, and the egress
/// ledger all live INSIDE [`make_provider_resolved`], keyed off the RESOLVED connection — a role
/// can never bypass them. An EXPLICIT reasoner-only target (`local`/`off` role keys) is refused
/// with `Unavailable`: those are `LocalReasoner` dispatch targets, never SummarizerProviders.
pub fn provider_for(
    role: roles::Role,
    config: &AppConfig,
) -> crate::error::Result<Arc<dyn SummarizerProvider>> {
    let target = roles::provider_target(role, config);
    // Refuse ONLY the targets that build no provider (off/apple). `local` now builds the on-device
    // LocalSummarizerProvider in `make_provider_resolved` (spec §3.2), so it is NOT refused here.
    if target.builds_no_provider() {
        return Err(crate::error::AppError::Unavailable(format!(
            "the {} role targets the on-device reasoner ({}); no summarizer provider is available \
             for it",
            role.as_str(),
            target.connection
        )));
    }
    make_provider_resolved(&target, config)
}

/// The EFFECTIVE model id a resolved target sends: the target's own model, or — when empty —
/// the connection's default (`anthropic_model` / `ollama_model` / `gateway_model`). For
/// `claude_code` an empty model stays `""` (the CLI's own default is unknowable here). This is
/// the single source of truth for provenance: the egress-ledger `model_requested` AND the note's
/// provenance row both derive from it, fixing the gap where anthropic-with-empty-`provider_model`
/// recorded an empty model even though the request carried `anthropic_model`.
pub(crate) fn effective_model_requested(target: &roles::RoleTarget, config: &AppConfig) -> String {
    let m = target.model.trim();
    if !m.is_empty() {
        return m.to_string();
    }
    match target.connection.as_str() {
        PROVIDER_ANTHROPIC => config.anthropic_model.clone(),
        PROVIDER_OLLAMA => config.ollama_model.clone(),
        PROVIDER_GATEWAY => config.gateway_model.clone(),
        _ => String::new(),
    }
}

/// The parameterized factory core: build a provider for a RESOLVED (connection, model, effort)
/// triple. ALL egress invariants live here, keyed off the resolved connection:
/// the fail-closed consent gate, the [`egress_is_cloud`] classification, the
/// [`RedactingProvider`] wrap, and the egress-ledger fields.
fn make_provider_resolved(
    target: &roles::RoleTarget,
    config: &AppConfig,
) -> crate::error::Result<Arc<dyn SummarizerProvider>> {
    let id = target.connection.as_str();
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
                // A resolved model is passed as `--model`; an empty value (the default) lets the
                // CLI use its own default. Effort is N/A for the CLI.
                .with_model(target.model.clone())
                // Opt-in: inherit the shell env (restores env ANTHROPIC_API_KEY); DB keys stay stripped.
                .with_inherit_env(config.claude_code_inherit_env),
        ),
        PROVIDER_ANTHROPIC => {
            // Resolve the key from the Keychain here so providers never touch secrets.
            let api_key = crate::secrets::get_secret(ANTHROPIC_KEY_ACCOUNT)?;
            // A resolved model takes precedence over the legacy `anthropic_model`; effort is
            // the adaptive-thinking tier (provider default when empty).
            let model = if target.model.trim().is_empty() {
                config.anthropic_model.clone()
            } else {
                target.model.clone()
            };
            Arc::new(AnthropicProvider::with_effort(
                api_key,
                model,
                target.effort.clone(),
            ))
        }
        PROVIDER_OLLAMA => {
            let model = if target.model.trim().is_empty() {
                config.ollama_model.clone()
            } else {
                target.model.clone()
            };
            let ollama = Arc::new(OllamaProvider::new(config.ollama_base_url.clone(), model));
            if !egress_is_cloud(id, config) {
                return Ok(ollama); // LOCAL ollama: unwrapped, unchanged behavior
            }
            ollama // REMOTE ollama: falls through to the RedactingProvider wrap below
        }
        PROVIDER_GATEWAY => {
            if config.gateway_base_url.trim().is_empty() {
                return Err(crate::error::AppError::InvalidArg(
                    "gateway base URL is not set".into(),
                ));
            }
            // R3 — resolve the GATEWAY key only; NEVER falls back to the Anthropic key.
            let api_key = crate::secrets::get_secret(GATEWAY_KEY_ACCOUNT).ok().flatten();
            let model = if target.model.trim().is_empty() {
                config.gateway_model.clone()
            } else {
                target.model.clone()
            };
            // R1/R4 enforced at construction via `validate_gateway_url` inside `new()`.
            Arc::new(crate::summarize::gateway::OpenAiCompatProvider::new(
                config.gateway_base_url.clone(),
                model,
                api_key,
            )?)
            // Falls through to the RedactingProvider wrap below (R2).
        }
        c if c == roles::CONN_LOCAL => {
            // FullyLocal Notes/Ask (spec §3.2): the on-device HEAVY engine as a provider. Resolve the
            // class model (target.model = the heavy id under the preset; else the persisted
            // brain_model_id). ABSENT ⇒ Unavailable — the note lands in Error + the recovery UX (P1),
            // NEVER a silent cloud fallback. Weights load once via the shared MODEL_CACHE. Returned
            // UNWRAPPED (no redaction, no ledger) like a loopback Ollama — `egress_is_cloud(local)` is
            // false, so the consent gate above was skipped and nothing egresses.
            let model_id = if target.model.trim().is_empty() {
                config.brain_model_id.clone()
            } else {
                Some(target.model.clone())
            };
            let configured = config.brain_model_path.as_deref().map(std::path::Path::new);
            match crate::reason::resolve_brain_model(configured, model_id.as_deref())? {
                Some(path) => {
                    let reasoner: Arc<dyn crate::reason::LocalReasoner> =
                        Arc::new(crate::reason::mistral::MistralReasoner::new(path)?);
                    return Ok(Arc::new(local::LocalSummarizerProvider::new(reasoner)));
                }
                None => {
                    return Err(crate::error::AppError::Unavailable(
                        "the on-device model for local notes is not downloaded".into(),
                    ));
                }
            }
        }
        other => {
            return Err(crate::error::AppError::InvalidArg(format!(
                "unknown provider id: {other}"
            )))
        }
    };

    // E6/E7 — redaction firewall on all cloud providers: scrub emails/cards/phones before they
    // reach the cloud (restored in the reply). `claude_code` shells out to the local `claude`
    // CLI, but that CLI uploads to Anthropic's cloud, so it needs the firewall exactly as the
    // direct HTTP `anthropic` provider does. A LOCAL ollama already returned above, unwrapped;
    // a REMOTE ollama falls through here and gets the same firewall treatment.
    //
    // Phase D — the name layer is now the ACTIVE on-device redactor: when the NER model is present,
    // `active_name_redactor()` returns the real DebertaNameRedactor (PERSON names → ⟪NAME_n⟫ before
    // egress, restored in the reply); otherwise it is the byte-identical NoopNameRedactor, so a
    // no-model build's egress is unchanged. The redactor only ever REMOVES content (a NER miss leaks
    // no more than the no-op).
    //
    // Phase 2b — wire the process-global egress sink so every cloud call records a content-free
    // audit row. Non-PII destination label + requested model are computed per provider arm here;
    // the full constructor is `with_name_redactor_and_sink`.
    let destination = match id {
        PROVIDER_CLAUDE_CODE => "claude_code (Anthropic CLI)".to_string(),
        PROVIDER_ANTHROPIC => "api.anthropic.com".to_string(),
        PROVIDER_GATEWAY => reqwest::Url::parse(&config.gateway_base_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| "gateway".to_string()),
        PROVIDER_OLLAMA => reqwest::Url::parse(&config.ollama_base_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| "ollama".to_string()),
        _ => id.to_string(),
    };
    // Provenance fix: record the EFFECTIVE model — for anthropic with an empty resolved model
    // that is `anthropic_model` (previously recorded as empty even though the request carried it).
    let model_requested = effective_model_requested(target, config);
    Ok(Arc::new(
        crate::summarize::redact::RedactingProvider::with_name_redactor_and_sink(
            inner,
            crate::summarize::redact::active_name_redactor(),
            crate::summarize::egress_log::active_sink(),
            id.to_string(),
            destination,
            model_requested,
        ),
    ))
}

/// Provider instances for the Settings UI "Provider availability" fan-out.
///
/// Availability-only: intentionally skips the consent gate and `RedactingProvider` wrap.
/// MUST NOT be used to summarize content — use [`make_provider`] for that.
///
/// Best-effort: a failure to read the Anthropic key from the Keychain degrades to a
/// keyless `AnthropicProvider` (which then reports `Unavailable`) rather than failing the
/// whole fan-out. The gateway entry is included ONLY when `gateway_base_url` is non-empty
/// AND the URL is valid; a bad URL degrades to omission (never panics).
pub fn all_providers(config: &AppConfig) -> Vec<Arc<dyn SummarizerProvider>> {
    let anthropic_key = crate::secrets::get_secret(ANTHROPIC_KEY_ACCOUNT)
        .ok()
        .flatten();

    let anthropic_model = if config.provider_model.trim().is_empty() {
        config.anthropic_model.clone()
    } else {
        config.provider_model.clone()
    };
    let mut providers: Vec<Arc<dyn SummarizerProvider>> = vec![
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
    ];
    // Gateway: include only when configured; a bad URL is omitted, never a panic.
    if !config.gateway_base_url.trim().is_empty() {
        let api_key = crate::secrets::get_secret(GATEWAY_KEY_ACCOUNT).ok().flatten();
        if let Ok(gw) = crate::summarize::gateway::OpenAiCompatProvider::new(
            config.gateway_base_url.clone(),
            config.gateway_model.clone(),
            api_key,
        ) {
            providers.push(Arc::new(gw));
        }
    }
    providers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egress_is_cloud_classification() {
        // claude_code and anthropic are always cloud regardless of config.
        let cfg = AppConfig::default();
        assert!(egress_is_cloud(PROVIDER_CLAUDE_CODE, &cfg));
        assert!(egress_is_cloud(PROVIDER_ANTHROPIC, &cfg));

        // ollama with default loopback URL is NOT cloud.
        let local_cfg = AppConfig {
            ollama_base_url: "http://localhost:11434".into(),
            ..AppConfig::default()
        };
        assert!(!egress_is_cloud(PROVIDER_OLLAMA, &local_cfg));

        // ollama with a remote URL IS cloud.
        let remote_cfg = AppConfig {
            ollama_base_url: "https://ollama.remote.example/api".into(),
            ..AppConfig::default()
        };
        assert!(egress_is_cloud(PROVIDER_OLLAMA, &remote_cfg));

        // ollama with an unparseable URL fails safe (treated as cloud).
        let bad_cfg = AppConfig {
            ollama_base_url: "not a url".into(),
            ..AppConfig::default()
        };
        assert!(egress_is_cloud(PROVIDER_OLLAMA, &bad_cfg));

        // Unknown provider ids default to cloud (fail-safe).
        assert!(egress_is_cloud("unknown-provider", &cfg));

        // On-device connections egress NOTHING (spec §3.2) — load-bearing for the Privacy Receipt:
        // a local note must never be classified cloud (else phantom consent/ledger/receipt-lie).
        assert!(!egress_is_cloud(roles::CONN_LOCAL, &cfg));
        assert!(!egress_is_cloud(roles::CONN_OFF, &cfg));
        assert!(!egress_is_cloud(roles::CONN_AFM, &cfg));
    }

    #[test]
    fn predicate_split_local_builds_a_provider_but_stays_agentic_ineligible() {
        use roles::{RoleTarget, CONN_AFM, CONN_LOCAL, CONN_OFF};
        let t = |c: &str| RoleTarget {
            connection: c.to_string(),
            model: String::new(),
            effort: String::new(),
        };
        // local: reasoner-only (NOT agentic-eligible) BUT now builds a provider (not refused).
        assert!(t(CONN_LOCAL).is_reasoner_only());
        assert!(!t(CONN_LOCAL).builds_no_provider());
        // off / apple: reasoner-only AND build no provider (still refused by provider_for).
        assert!(t(CONN_OFF).builds_no_provider());
        assert!(t(CONN_AFM).builds_no_provider());
        // cloud: neither.
        assert!(!t(PROVIDER_CLAUDE_CODE).is_reasoner_only());
        assert!(!t(PROVIDER_CLAUDE_CODE).builds_no_provider());
    }

    #[test]
    fn remote_ollama_requires_consent() {
        let cfg = AppConfig {
            ollama_base_url: "https://ollama.remote.example/api".into(),
            cloud_egress_consented: false,
            ..AppConfig::default()
        };
        let res = make_provider(PROVIDER_OLLAMA, &cfg);
        assert!(
            matches!(res, Err(crate::error::AppError::Unavailable(_))),
            "expected Unavailable for remote ollama without consent"
        );
    }

    #[test]
    fn local_ollama_stays_unwrapped_and_ungated() {
        let cfg = AppConfig {
            ollama_base_url: "http://localhost:11434".into(),
            cloud_egress_consented: false,
            ..AppConfig::default()
        };
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
        // ollama with a LOOPBACK url builds without consent (the default url is localhost).
        // A remote ollama_base_url is covered by remote_ollama_requires_consent.
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

    /// E10 revoke — `revoke_cloud_egress` puts every cloud-classified resolution back behind the
    /// fail-closed gate, exactly like a never-consented config: grant → provider builds, revoke →
    /// both the legacy factory AND the role resolver refuse with `Unavailable`.
    #[test]
    fn cloud_providers_refused_after_consent_revoked() {
        let p = crate::storage::db::unique_temp_path("meetnotes-revoke-gate-test", "sqlite");
        // Explicit key (NOT the Keychain) — Db::open would prompt/block in a test binary.
        let db = crate::storage::Db::open_with_key(
            &p,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        let mut cfg = AppConfig::load(&db).unwrap();
        cfg.grant_cloud_egress_consent(&db).unwrap();
        assert!(
            make_provider(PROVIDER_CLAUDE_CODE, &cfg).is_ok(),
            "granted consent must build the cloud provider"
        );
        cfg.revoke_cloud_egress(&db).unwrap();
        for id in [PROVIDER_CLAUDE_CODE, PROVIDER_ANTHROPIC] {
            let res = make_provider(id, &cfg);
            assert!(
                matches!(res, Err(crate::error::AppError::Unavailable(_))),
                "expected Unavailable for {id} after revoke (got Ok or wrong error)"
            );
        }
        for role in [roles::Role::Notes, roles::Role::Ask, roles::Role::Live] {
            assert!(
                matches!(provider_for(role, &cfg), Err(crate::error::AppError::Unavailable(_))),
                "provider_for {role:?} must refuse after revoke"
            );
        }
        // And the revocation is durable — a fresh load sees consent OFF.
        assert!(!AppConfig::load(&db).unwrap().cloud_egress_consented);
    }

    // ─── Task 1.3 — the four gateway security guardrails ───────────────────────────────────────

    /// R1 — gateway is refused when cloud-egress consent has not been granted.
    #[test]
    fn gateway_refused_without_consent() {
        let c = AppConfig {
            gateway_base_url: "https://gw.example.com/v1".into(),
            cloud_egress_consented: false,
            ..AppConfig::default()
        };
        let err = make_provider(PROVIDER_GATEWAY, &c)
            .map(|_| ())
            .expect_err("expected Err for gateway without consent");
        assert!(
            matches!(err, crate::error::AppError::Unavailable(_)),
            "expected Unavailable, got: {err}"
        );
    }

    /// R2 — a consented gateway with a loopback base URL builds successfully.
    /// (The RedactingProvider wrap is structural — it is transparent to `id()`, which reports the
    /// inner provider id. We assert construction succeeds; the wrapping is proven by the RedactingProvider
    /// tests in redact.rs and by the lock-security-reviewer audit.)
    #[test]
    fn gateway_localhost_is_still_redaction_wrapped() {
        let c = AppConfig {
            gateway_base_url: "http://127.0.0.1:4000/v1".into(),
            cloud_egress_consented: true,
            ..AppConfig::default()
        };
        // Must build without error — the make_provider consent gate + URL validation passed.
        assert!(
            make_provider(PROVIDER_GATEWAY, &c).is_ok(),
            "consented localhost gateway must build successfully"
        );
    }

    /// R4 — a remote http:// URL is rejected at provider-construction time (InvalidArg).
    #[test]
    fn gateway_remote_http_rejected() {
        let c = AppConfig {
            gateway_base_url: "http://gw.example.com/v1".into(),
            cloud_egress_consented: true,
            ..AppConfig::default()
        };
        let err = make_provider(PROVIDER_GATEWAY, &c)
            .map(|_| ())
            .expect_err("expected Err for remote http gateway");
        assert!(
            matches!(err, crate::error::AppError::InvalidArg(_)),
            "expected InvalidArg for remote http://, got: {err}"
        );
    }

    /// Empty base URL → InvalidArg (before even trying to validate the URL).
    #[test]
    fn gateway_empty_url_rejected() {
        let c = AppConfig {
            gateway_base_url: String::new(), // empty — not set
            cloud_egress_consented: true,
            ..AppConfig::default()
        };
        let err = make_provider(PROVIDER_GATEWAY, &c)
            .map(|_| ())
            .expect_err("expected Err for empty gateway URL");
        assert!(
            matches!(err, crate::error::AppError::InvalidArg(_)),
            "expected InvalidArg for empty URL, got: {err}"
        );
    }

    // ─── Model roles — provider_for keeps every factory invariant, keyed off the RESOLVED
    //     connection, and is byte-identical to make_provider under the legacy fallback ─────────

    /// FALLBACK IDENTITY at the factory level: with role keys absent, `provider_for` builds the
    /// SAME default provider as `make_provider(&provider_id, …)` for EVERY role and EVERY
    /// `brain_backend` — including Local/Off, where the legacy Ask provider paths (meeting chat,
    /// the ask_vault floor) always ignored `brain_backend`.
    #[test]
    fn provider_for_matches_legacy_factory_under_fallback() {
        use crate::settings::BrainBackend;
        for backend in [BrainBackend::Cloud, BrainBackend::Local, BrainBackend::Off] {
            let cfg = AppConfig {
                cloud_egress_consented: true,
                brain_backend: backend,
                ..AppConfig::default()
            };
            for role in [roles::Role::Notes, roles::Role::Ask, roles::Role::Live] {
                let p = provider_for(role, &cfg)
                    .unwrap_or_else(|e| panic!("{role:?}/{backend:?} must build: {e}"));
                assert_eq!(p.id(), PROVIDER_CLAUDE_CODE, "{role:?}/{backend:?}");
            }
        }
    }

    /// CONSENT INVARIANCE across roles: every cloud-classified resolution — explicit role keys
    /// included — is refused fail-closed without consent, exactly like `make_provider`. The gate
    /// keys off the RESOLVED connection inside the factory, so a role can never bypass it.
    #[test]
    fn provider_for_is_consent_gated_for_every_cloud_resolution() {
        // (a) fallback (default claude_code), all roles.
        let cfg = AppConfig::default(); // consent OFF
        for role in [roles::Role::Notes, roles::Role::Ask, roles::Role::Live] {
            assert!(
                matches!(provider_for(role, &cfg), Err(crate::error::AppError::Unavailable(_))),
                "fallback {role:?} must be consent-gated"
            );
        }
        // (b) explicit role keys onto each cloud connection (incl. REMOTE ollama).
        type ConfigTweak = fn(&mut AppConfig);
        let cases: [(&str, ConfigTweak); 4] = [
            ("claude_code", |_| {}),
            ("anthropic", |_| {}),
            ("gateway", |c| c.gateway_base_url = "http://127.0.0.1:4000/v1".into()),
            ("ollama", |c| c.ollama_base_url = "https://ollama.remote.example/api".into()),
        ];
        for (conn, extra) in cases {
            let mut cfg = AppConfig {
                role_ask_connection: conn.to_string(),
                cloud_egress_consented: false,
                ..AppConfig::default()
            };
            extra(&mut cfg);
            assert!(
                matches!(
                    provider_for(roles::Role::Ask, &cfg),
                    Err(crate::error::AppError::Unavailable(_))
                ),
                "explicit Ask→{conn} must be consent-gated"
            );
            // With consent granted the SAME resolution builds (and the RedactingProvider wrap is
            // transparent to id(), which reports the inner connection — the wrap itself is proven
            // by the redact.rs tests, exactly like the legacy factory tests above).
            cfg.cloud_egress_consented = true;
            let p = provider_for(roles::Role::Ask, &cfg)
                .unwrap_or_else(|e| panic!("consented Ask→{conn} must build: {e}"));
            assert_eq!(p.id(), conn);
        }
    }

    /// A LOOPBACK ollama role target stays consent-exempt and unwrapped — the classification is
    /// on the resolved connection + its URL, identical to the legacy factory.
    #[test]
    fn provider_for_local_ollama_role_is_ungated() {
        let cfg = AppConfig {
            role_ask_connection: "ollama".to_string(),
            ollama_base_url: "http://localhost:11434".into(),
            cloud_egress_consented: false,
            ..AppConfig::default()
        };
        let p = provider_for(roles::Role::Ask, &cfg).expect("loopback ollama needs no consent");
        assert_eq!(p.id(), PROVIDER_OLLAMA);
    }

    /// EXPLICIT reasoner-only role keys (`local`/`off`) can never build a SummarizerProvider —
    /// `provider_for` refuses with `Unavailable` (they are LocalReasoner dispatch targets). Only
    /// the LEGACY brain_backend fallback defers to the default provider (the floor nuance above).
    #[test]
    fn provider_for_refuses_explicit_reasoner_only_targets() {
        for conn in [roles::CONN_LOCAL, roles::CONN_OFF] {
            let cfg = AppConfig {
                role_ask_connection: conn.to_string(),
                cloud_egress_consented: true,
                ..AppConfig::default()
            };
            let err = provider_for(roles::Role::Ask, &cfg)
                .map(|_| ())
                .expect_err("explicit reasoner-only target must not build a provider");
            assert!(
                matches!(err, crate::error::AppError::Unavailable(_)),
                "expected Unavailable for explicit {conn}, got: {err}"
            );
        }
    }

    /// PROVENANCE — `effective_model_requested` names the model the request actually carries:
    /// the resolved model when set, else the CONNECTION's own default. This is the anthropic
    /// provenance fix: an empty resolved model records `anthropic_model` (previously empty).
    #[test]
    fn effective_model_requested_resolves_connection_defaults() {
        let cfg = AppConfig {
            anthropic_model: "claude-opus-4-8".to_string(),
            ollama_model: "llama3.1".to_string(),
            gateway_model: "gpt-4o".to_string(),
            ..AppConfig::default()
        };
        let t = |conn: &str, model: &str| roles::RoleTarget {
            connection: conn.to_string(),
            model: model.to_string(),
            effort: String::new(),
        };
        // An explicit resolved model always wins.
        assert_eq!(effective_model_requested(&t("anthropic", "claude-sonnet-4-6"), &cfg), "claude-sonnet-4-6");
        // Empty model → the connection's own default (THE anthropic fix).
        assert_eq!(effective_model_requested(&t("anthropic", ""), &cfg), "claude-opus-4-8");
        assert_eq!(effective_model_requested(&t("ollama", ""), &cfg), "llama3.1");
        assert_eq!(effective_model_requested(&t("gateway", ""), &cfg), "gpt-4o");
        // claude_code's default is the CLI's own — unknowable here, stays "".
        assert_eq!(effective_model_requested(&t("claude_code", ""), &cfg), "");
        assert_eq!(effective_model_requested(&t("claude_code", "claude-haiku-4-5"), &cfg), "claude-haiku-4-5");
    }

    /// Task 1.3 — `egress_is_cloud` explicitly classifies `PROVIDER_GATEWAY` as cloud.
    #[test]
    fn gateway_is_always_cloud() {
        let cfg = AppConfig::default();
        assert!(
            egress_is_cloud(PROVIDER_GATEWAY, &cfg),
            "gateway must always be cloud regardless of base URL"
        );
        let cfg_loopback = AppConfig {
            gateway_base_url: "http://127.0.0.1:4000/v1".into(),
            ..AppConfig::default()
        };
        assert!(
            egress_is_cloud(PROVIDER_GATEWAY, &cfg_loopback),
            "a loopback gateway is still cloud-classified"
        );
    }
}
