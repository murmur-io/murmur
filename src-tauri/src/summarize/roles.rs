//! Model ROLES — the one pure resolution layer between "what feature is asking" and "which
//! connection/model/effort serves it".
//!
//! Murmur steers every LLM call with exactly two legacy knobs today: `provider_id` (+
//! `provider_model`/`provider_effort`) for the SummarizerProvider factory, and `brain_backend`
//! (+ `brain_model_id`) for the reasoner dispatch. This module introduces three user-meaningful
//! roles — **Notes** (everything Murmur writes: summaries, digests, dossiers, briefs, recipes,
//! timelines, graph extraction), **Ask** (vault Q&A + meeting chat), and **Live** (the in-meeting
//! assistant / @brain threads / voice) — each resolvable to a [`RoleTarget`] by [`resolve`].
//!
//! ## The zero-behavior-change contract (load-bearing)
//! Nine additive config keys (`role_{notes,ask,live}_{connection,model,effort}`) override the
//! legacy knobs PER ROLE. A role whose CONNECTION key is empty (every install today) falls back to
//! the legacy mapping EXACTLY, so with the keys absent every call path behaves byte-identically to
//! the pre-role code. The legacy config is never rewritten — it stays the resolver's bottom layer
//! (downgrade-safe).
//!
//! ## Why THREE resolution views (not one)
//! The legacy code answers "which model serves feature X" with TWO DIFFERENT knobs depending on
//! the engine, and a single mapping cannot reproduce both:
//! - [`resolve`] — the role's user-facing target (the contract mapping): Notes → the default
//!   provider triple; Ask/Live → the `brain_backend` mapping (`cloud` inherits the default
//!   provider, `local`/`off` are reasoner-only targets).
//! - [`provider_target`] — "which **SummarizerProvider** serves this role". Legacy nuance: the
//!   pre-role Ask provider paths (`chat_meeting`, the `ask_vault` floor) IGNORE `brain_backend`
//!   and always use the default provider — so a reasoner-only target reached VIA LEGACY FALLBACK
//!   defers to the default provider triple (byte-identical floors), while an EXPLICIT
//!   `local`/`off` role key stays reasoner-only (the factory refuses it — see
//!   [`crate::summarize::provider_for`]).
//! - [`reasoner_target`] — "which **LocalReasoner** serves this role". Legacy nuance: EVERY
//!   pre-role reasoner dispatch (pre-analysis, facts, the agentic loops) keys on `brain_backend`
//!   — including the Notes-role sites — so under fallback ALL roles map `brain_backend` here.
//!   Resolving the Notes reasoner from `provider_id` instead would silently turn a
//!   `brain_backend = Off/Local` install's pre-analysis into a CLOUD call — an egress regression.
//!
//! With a role's connection key SET, all three views coincide (the role's explicit triple).
//! Everything here is pure (config in, target out): consent, redaction, and the egress ledger
//! stay inside the factory ([`crate::summarize::make_provider`]-family), keyed off the RESOLVED
//! connection — roles can never bypass them.

use crate::settings::{AppConfig, BrainBackend};

/// Reasoner-only connection id: the on-device GGUF brain ([`crate::reason::sidecar`]).
pub const CONN_LOCAL: &str = "local";
/// Reasoner-only connection id: no model — the deterministic [`crate::reason::StubReasoner`] floor.
pub const CONN_OFF: &str = "off";
/// Reasoner-only connection id (WS2, EXPERIMENTAL): the on-device Apple Foundation Models brain
/// ([`crate::reason::afm`]) via the `meetnotes-afm` sidecar. ON-DEVICE like [`CONN_LOCAL`] — it
/// builds no `SummarizerProvider` and is auto-excluded from the cloud agentic loop (see
/// [`RoleTarget::is_reasoner_only`]); falls back to the stub when the sidecar is absent.
pub const CONN_AFM: &str = "apple";

/// Human-facing display name for a connection id — for USER-VISIBLE status lines
/// (e.g. the pipeline's "Summarizing with …" line).
///
/// **DOCUMENTED MIRROR — change both halves in the SAME commit or they drift.** The frontend's
/// copy of these labels is `CONNECTION_LABELS` in `src/app/core/copy/labels.ts` (it used to live
/// in `settings.store.ts` AND again in `record.component.ts`; P3 collapsed all three into the one
/// copy module, which is the only place the FE may name a connection). The `connection_labels_
/// mirror_the_frontend_copy_module` test below pins the provider rows.
///
/// An unknown id falls back to itself, so a newly-added provider is never blank.
pub fn connection_display_name(connection: &str) -> &str {
    match connection {
        "claude_code" => "Claude Code",
        "codex_cli" => "Codex",
        "anthropic" => "Anthropic API",
        "ollama" => "Ollama",
        "gateway" => "Kong AI Gateway",
        CONN_LOCAL => "the on-device model",
        CONN_AFM => "Apple Intelligence (on-device)",
        other => other,
    }
}

/// A model role — the fixed, user-meaningful set of "what Murmur uses AI for".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Everything Murmur WRITES: note summaries, auto-organize, graph extraction, recipes,
    /// digests, dossiers, briefs, timelines — plus the note pipeline's pre-analysis/facts brain.
    Notes,
    /// Vault Q&A (`ask_vault` agentic loop + floor) and per-meeting chat.
    Ask,
    /// The in-meeting assistant: wake-word/voice turns, typed @brain threads, the live loop.
    Live,
}

impl Role {
    /// Stable lowercase token for logs (`notes` | `ask` | `live`). Never PII.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Notes => "notes",
            Role::Ask => "ask",
            Role::Live => "live",
        }
    }
}

/// A resolved (connection, model, effort) triple for one role. `connection` is a provider id
/// (`claude_code` / `codex_cli` / `anthropic` / `ollama` / `gateway`) or a reasoner-only target
/// ([`CONN_LOCAL`] / [`CONN_OFF`]). An empty `model`/`effort` means "inherit that connection's
/// own default" (the factory arms already resolve those — e.g. anthropic → `anthropic_model`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleTarget {
    pub connection: String,
    pub model: String,
    pub effort: String,
}

impl RoleTarget {
    /// `true` when this target is served by a [`crate::reason::LocalReasoner`] only (`local`/`off`/
    /// `apple`) — it can never build a `SummarizerProvider`, and the cloud agentic loops don't run
    /// on it. Including `apple` here is what auto-excludes the on-device Apple Foundation Models
    /// backend from the cloud agentic-eligibility gate AND makes `provider_for` refuse to build a
    /// cloud provider for an explicit `apple` role key — both correct, both free.
    pub fn is_reasoner_only(&self) -> bool {
        self.connection == CONN_LOCAL || self.connection == CONN_OFF || self.connection == CONN_AFM
    }

    /// `true` when this target builds NO `SummarizerProvider` — `off` (stub) / `apple` (AFM sidecar).
    /// The predicate split (spec §3.2, review code-truth #8): `local` is reasoner-only for the
    /// AGENTIC-eligibility gate ([`is_reasoner_only`]) BUT it now DOES build a provider (the on-device
    /// [`crate::summarize::local::LocalSummarizerProvider`]), so it is NOT included here. This is the
    /// predicate `provider_for` refuses on — decoupled from agentic eligibility, which stays cloud-only.
    pub fn builds_no_provider(&self) -> bool {
        self.connection == CONN_OFF || self.connection == CONN_AFM
    }
}

/// The raw role keys for `role` (connection, model, effort) — `""` everywhere = "inherit"
/// (the legacy fallback applies).
fn role_keys(role: Role, cfg: &AppConfig) -> (&str, &str, &str) {
    match role {
        Role::Notes => (
            &cfg.role_notes_connection,
            &cfg.role_notes_model,
            &cfg.role_notes_effort,
        ),
        Role::Ask => (
            &cfg.role_ask_connection,
            &cfg.role_ask_model,
            &cfg.role_ask_effort,
        ),
        Role::Live => (
            &cfg.role_live_connection,
            &cfg.role_live_model,
            &cfg.role_live_effort,
        ),
    }
}

/// Whether `role`'s connection key is explicitly set. The CONNECTION key is the override switch:
/// with it empty the role inherits the WHOLE legacy mapping (a lone model/effort key is ignored —
/// "" would be ambiguous between "CLI default" and "legacy provider_model" otherwise).
fn is_explicit(role: Role, cfg: &AppConfig) -> bool {
    !role_keys(role, cfg).0.trim().is_empty()
}

/// The role's EXPLICIT target from its own keys. Caller guarantees [`is_explicit`].
fn explicit_target(role: Role, cfg: &AppConfig) -> RoleTarget {
    let (connection, model, effort) = role_keys(role, cfg);
    RoleTarget {
        connection: connection.trim().to_string(),
        model: model.trim().to_string(),
        effort: effort.trim().to_string(),
    }
}

/// The LEGACY default-provider triple — what every pre-role `make_provider(&cfg.provider_id, …)`
/// call site effectively targeted.
///
/// `model` carries `provider_model` for the CLI/direct arms that read it (`claude_code`,
/// `codex_cli`, `anthropic`); the `ollama`/`gateway` arms have always resolved their model from
/// `ollama_model`/`gateway_model` and IGNORED `provider_model` (the Brain&AI picker is inert
/// there), so their fallback model is `""` = inherit-connection-default. Carrying
/// `provider_model` verbatim would make a stale Claude id from the picker suddenly reach an
/// ollama/gateway request — a real behavior change for existing configs.
fn legacy_default_target(cfg: &AppConfig) -> RoleTarget {
    let model = match cfg.provider_id.as_str() {
        crate::summarize::PROVIDER_CLAUDE_CODE
        | crate::summarize::PROVIDER_CODEX_CLI
        | crate::summarize::PROVIDER_ANTHROPIC => cfg.provider_model.clone(),
        _ => String::new(),
    };
    RoleTarget {
        connection: cfg.provider_id.clone(),
        model,
        effort: cfg.provider_effort.clone(),
    }
}

/// The LEGACY `brain_backend` mapping — what every pre-role `ReasonerCell::current()` dispatch
/// (and the agentic-eligibility gates) keyed on.
fn legacy_brain_target(cfg: &AppConfig) -> RoleTarget {
    match cfg.brain_backend {
        BrainBackend::Cloud => legacy_default_target(cfg),
        BrainBackend::Local => RoleTarget {
            connection: CONN_LOCAL.to_string(),
            model: cfg.brain_model_id.clone().unwrap_or_default(),
            effort: String::new(),
        },
        BrainBackend::Off => RoleTarget {
            connection: CONN_OFF.to_string(),
            model: String::new(),
            effort: String::new(),
        },
        // WS2 — the on-device Apple Foundation Models reasoner. Model/effort are empty (the sidecar
        // pins `SystemLanguageModel.default`), so a `role_*_connection = "apple"` key ALSO routes to
        // AFM automatically via the explicit_target path — per-role AFM steering falls out for free.
        BrainBackend::AppleFoundation => RoleTarget {
            connection: CONN_AFM.to_string(),
            model: String::new(),
            effort: String::new(),
        },
    }
}

/// Resolve `role` to its target — PURE, the role architecture's single source of truth.
///
/// Explicit role keys win; otherwise the EXACT legacy fallback:
/// - **Notes** → the default provider triple (`provider_id` + `provider_model`/`provider_effort`
///   where the arm reads them — see [`legacy_default_target`]).
/// - **Ask / Live** → the `brain_backend` mapping: `Cloud` inherits the default provider triple,
///   `Local` → ([`CONN_LOCAL`], `brain_model_id`, ""), `Off` → ([`CONN_OFF`], "", "").
pub fn resolve(role: Role, cfg: &AppConfig) -> RoleTarget {
    if is_explicit(role, cfg) {
        return explicit_target(role, cfg);
    }
    match role {
        Role::Notes => legacy_default_target(cfg),
        Role::Ask | Role::Live => legacy_brain_target(cfg),
    }
}

/// The target whose **SummarizerProvider** serves `role` — what
/// [`crate::summarize::provider_for`] builds, and what corpus BUDGETS must key on (the corpus
/// egresses to THIS connection).
///
/// Same as [`resolve`], with the one legacy nuance: a reasoner-only target reached VIA THE LEGACY
/// FALLBACK (i.e. `brain_backend = Local/Off` with no explicit role key) defers to the legacy
/// default provider triple — because the pre-role provider paths for Ask (`chat_meeting`, the
/// `ask_vault` floor) always used `provider_id` and IGNORED `brain_backend`. An EXPLICIT
/// `local`/`off` role key is returned as-is (reasoner-only); the factory refuses to build a
/// provider for it.
pub fn provider_target(role: Role, cfg: &AppConfig) -> RoleTarget {
    let target = resolve(role, cfg);
    if target.is_reasoner_only() && !is_explicit(role, cfg) {
        return legacy_default_target(cfg);
    }
    target
}

/// The target whose **LocalReasoner** serves `role` — what [`crate::reason::ReasonerCell::current_for`]
/// dispatches on.
///
/// Explicit role keys win (all three views coincide). Under the legacy fallback EVERY role maps
/// `brain_backend` — including Notes, whose reasoner sites (note-pipeline pre-analysis, facts)
/// have always dispatched on `brain_backend`. Mapping the Notes reasoner from `provider_id`
/// instead would turn a `brain_backend = Off/Local` install's pre-analysis into a cloud call —
/// an egress change this behavior-identical layer must not make.
pub fn reasoner_target(role: Role, cfg: &AppConfig) -> RoleTarget {
    if is_explicit(role, cfg) {
        return explicit_target(role, cfg);
    }
    legacy_brain_target(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_display_name_is_human_friendly() {
        // The real providers render as their user-facing labels (matches the FE).
        assert_eq!(connection_display_name("claude_code"), "Claude Code");
        assert_eq!(connection_display_name("codex_cli"), "Codex");
        assert_eq!(connection_display_name("anthropic"), "Anthropic API");
        assert_eq!(connection_display_name("ollama"), "Ollama");
        assert_eq!(connection_display_name("gateway"), "Kong AI Gateway");
        // Never the raw id / underscore token in a user-visible status line.
        assert_ne!(connection_display_name("claude_code"), "claude_code");
        // Unknown id falls back to itself (never blank) so a new provider still shows.
        assert_eq!(connection_display_name("brand_new"), "brand_new");
    }

    /// The DOCUMENTED MIRROR guard. `src/app/core/copy/labels.ts::CONNECTION_LABELS` carries the
    /// same strings; this test is the Rust half of "they change together or they drift".
    /// A rename that lands here without landing there (or vice versa) makes the pipeline's
    /// "Summarizing with …" line disagree with the Settings summary for the same connection.
    #[test]
    fn connection_labels_mirror_the_frontend_copy_module() {
        // Keep in lockstep with src/app/core/copy/labels.ts::CONNECTION_LABELS.
        const FRONTEND_LABELS: &[(&str, &str)] = &[
            ("claude_code", "Claude Code"),
            ("codex_cli", "Codex"),
            ("anthropic", "Anthropic API"),
            ("ollama", "Ollama"),
            ("gateway", "Kong AI Gateway"),
        ];
        for (id, label) in FRONTEND_LABELS {
            assert_eq!(
                connection_display_name(id),
                *label,
                "connection {id} drifted from the frontend copy module"
            );
        }
    }

    /// OOM DEFERRAL (perf-memory-audit): the exact decision `timeline_generation_on_device` makes —
    /// `is_on_device_provider(&resolve(Notes).connection)`. An explicit LOCAL Notes role ⇒ on-device
    /// (generation is a heavy multi-GB load → the FE hides it behind a click, so a passive Audio-tab
    /// open can't beachball the Mac); the default CLOUD provider ⇒ NOT on-device (cheap → auto-fired).
    #[test]
    fn timeline_generation_on_device_matches_resolved_notes_provider() {
        use crate::summarize::timeline::is_on_device_provider;
        // Explicit local Notes override → resolved connection is on-device (heavy → deferred).
        let local = AppConfig {
            role_notes_connection: CONN_LOCAL.to_string(),
            ..Default::default()
        };
        assert!(
            is_on_device_provider(&resolve(Role::Notes, &local).connection),
            "explicit local Notes role must classify as on-device (heavy generation)"
        );
        // Default (no role override) with a cloud default provider → NOT on-device (auto-generate).
        let cloud = legacy_cfg("claude_code", BrainBackend::Cloud);
        assert!(
            !is_on_device_provider(&resolve(Role::Notes, &cloud).connection),
            "cloud Notes provider must NOT classify as on-device (cheap → auto)"
        );
    }

    /// A discriminating legacy config: every legacy knob set to a distinct value so the identity
    /// matrix can tell exactly which knob each resolution read.
    fn legacy_cfg(provider_id: &str, backend: BrainBackend) -> AppConfig {
        AppConfig {
            provider_id: provider_id.to_string(),
            provider_model: "picker-model".to_string(),
            provider_effort: "high".to_string(),
            brain_backend: backend,
            brain_model_id: Some("bielik-11b-v3".to_string()),
            ..AppConfig::default()
        }
    }

    fn target(connection: &str, model: &str, effort: &str) -> RoleTarget {
        RoleTarget {
            connection: connection.to_string(),
            model: model.to_string(),
            effort: effort.to_string(),
        }
    }

    /// The RESOLVER IDENTITY MATRIX — with role keys absent, `resolve` reproduces the documented
    /// legacy mapping for every provider_id × brain_backend combination. This is the
    /// zero-behavior-change proof at the resolution layer.
    #[test]
    fn resolve_identity_matrix_with_keys_absent() {
        for provider_id in ["claude_code", "codex_cli", "anthropic", "ollama", "gateway"] {
            // The legacy arms read `provider_model` for claude_code/codex_cli/anthropic; ollama and
            // gateway resolve their model from ollama_model/gateway_model (provider_model is
            // inert there), so their fallback model is "" = inherit-connection-default.
            let legacy_model = match provider_id {
                "claude_code" | "codex_cli" | "anthropic" => "picker-model",
                _ => "",
            };
            let notes_want = target(provider_id, legacy_model, "high");
            for backend in [
                BrainBackend::Cloud,
                BrainBackend::Local,
                BrainBackend::Off,
                BrainBackend::AppleFoundation,
            ] {
                let cfg = legacy_cfg(provider_id, backend);
                // Notes ignores brain_backend entirely.
                assert_eq!(
                    resolve(Role::Notes, &cfg),
                    notes_want,
                    "{provider_id}/{backend:?}"
                );
                // Ask and Live map brain_backend identically.
                let brain_want = match backend {
                    BrainBackend::Cloud => notes_want.clone(),
                    BrainBackend::Local => target(CONN_LOCAL, "bielik-11b-v3", ""),
                    BrainBackend::Off => target(CONN_OFF, "", ""),
                    BrainBackend::AppleFoundation => target(CONN_AFM, "", ""),
                };
                for role in [Role::Ask, Role::Live] {
                    assert_eq!(
                        resolve(role, &cfg),
                        brain_want,
                        "{provider_id}/{backend:?}/{role:?}"
                    );
                }
            }
        }
    }

    /// Local fallback with NO selected brain model resolves model to "" (the reasoner then
    /// falls back to the stub, exactly like today's `local_reasoner`).
    #[test]
    fn resolve_local_fallback_without_model_id_is_empty_model() {
        let cfg = AppConfig {
            brain_backend: BrainBackend::Local,
            brain_model_id: None,
            ..AppConfig::default()
        };
        assert_eq!(resolve(Role::Ask, &cfg), target(CONN_LOCAL, "", ""));
    }

    /// Explicit role keys WIN over every legacy knob; a set connection with an empty model keeps
    /// the model "" (= that connection's own default — the factory arms resolve it).
    #[test]
    fn explicit_role_keys_win_and_empty_model_inherits_connection_default() {
        let cfg = AppConfig {
            role_ask_connection: "ollama".to_string(),
            role_ask_model: "mistral-small".to_string(),
            role_ask_effort: "low".to_string(),
            role_live_connection: "anthropic".to_string(),
            // live model/effort left "" — inherit anthropic's own defaults.
            ..legacy_cfg("claude_code", BrainBackend::Off)
        };
        assert_eq!(
            resolve(Role::Ask, &cfg),
            target("ollama", "mistral-small", "low")
        );
        assert_eq!(resolve(Role::Live, &cfg), target("anthropic", "", ""));
        // Notes has no explicit key → still the legacy default triple.
        assert_eq!(
            resolve(Role::Notes, &cfg),
            target("claude_code", "picker-model", "high")
        );
    }

    /// The CONNECTION key is the override switch: a lone model/effort key with the connection
    /// empty is ignored (full legacy inherit) — never a half-override.
    #[test]
    fn model_key_without_connection_key_is_ignored() {
        let cfg = AppConfig {
            role_notes_model: "sneaky-model".to_string(),
            role_notes_effort: "low".to_string(),
            ..legacy_cfg("claude_code", BrainBackend::Cloud)
        };
        assert_eq!(
            resolve(Role::Notes, &cfg),
            target("claude_code", "picker-model", "high")
        );
    }

    /// GATE EQUIVALENCE — the agentic-eligibility gates replace `brain_backend == Cloud` with
    /// `!resolve(role).is_reasoner_only()`. Under the legacy fallback the two predicates are
    /// identical for every backend (the RED/GREEN proof for the gate swap).
    #[test]
    fn reasoner_only_matches_legacy_cloud_gate_under_fallback() {
        // AppleFoundation is reasoner-only ⇒ the cloud agentic gate is CLOSED for it (like Local/Off).
        for backend in [
            BrainBackend::Cloud,
            BrainBackend::Local,
            BrainBackend::Off,
            BrainBackend::AppleFoundation,
        ] {
            let cfg = legacy_cfg("claude_code", backend);
            let legacy_gate_open = backend == BrainBackend::Cloud;
            for role in [Role::Ask, Role::Live] {
                assert_eq!(
                    !resolve(role, &cfg).is_reasoner_only(),
                    legacy_gate_open,
                    "{backend:?}/{role:?}"
                );
            }
        }
    }

    /// PROVIDER-TARGET LEGACY NUANCE — under fallback the SummarizerProvider question always
    /// answers with the default provider triple, for EVERY role and EVERY brain_backend: the
    /// pre-role Ask provider paths (chat_meeting, the ask_vault floor) ignored `brain_backend`.
    #[test]
    fn provider_target_is_default_triple_under_fallback_for_all_backends() {
        for provider_id in ["claude_code", "codex_cli", "anthropic", "ollama", "gateway"] {
            for backend in [
                BrainBackend::Cloud,
                BrainBackend::Local,
                BrainBackend::Off,
                BrainBackend::AppleFoundation,
            ] {
                let cfg = legacy_cfg(provider_id, backend);
                let want = legacy_default_target(&cfg);
                for role in [Role::Notes, Role::Ask, Role::Live] {
                    assert_eq!(
                        provider_target(role, &cfg),
                        want,
                        "{provider_id}/{backend:?}/{role:?}"
                    );
                }
            }
        }
    }

    /// An EXPLICIT local/off role key stays reasoner-only on the provider question (the factory
    /// then refuses it) — only the LEGACY fallback defers to the default provider.
    #[test]
    fn provider_target_keeps_explicit_reasoner_only_targets() {
        let cfg = AppConfig {
            role_ask_connection: CONN_LOCAL.to_string(),
            role_live_connection: CONN_OFF.to_string(),
            ..legacy_cfg("claude_code", BrainBackend::Cloud)
        };
        assert_eq!(provider_target(Role::Ask, &cfg).connection, CONN_LOCAL);
        assert_eq!(provider_target(Role::Live, &cfg).connection, CONN_OFF);
        // Notes stays on the default provider.
        assert_eq!(provider_target(Role::Notes, &cfg).connection, "claude_code");
    }

    /// REASONER-TARGET LEGACY NUANCE — under fallback EVERY role (Notes included) maps
    /// `brain_backend`, because every pre-role reasoner dispatch did. A Notes reasoner resolved
    /// from `provider_id` instead would cloud-dispatch an Off/Local install's pre-analysis.
    #[test]
    fn reasoner_target_maps_brain_backend_for_all_roles_under_fallback() {
        for backend in [
            BrainBackend::Cloud,
            BrainBackend::Local,
            BrainBackend::Off,
            BrainBackend::AppleFoundation,
        ] {
            let cfg = legacy_cfg("claude_code", backend);
            let want = legacy_brain_target(&cfg);
            for role in [Role::Notes, Role::Ask, Role::Live] {
                assert_eq!(reasoner_target(role, &cfg), want, "{backend:?}/{role:?}");
            }
        }
    }

    /// With explicit keys all three views coincide on the role's own triple.
    #[test]
    fn explicit_keys_unify_all_three_views() {
        let cfg = AppConfig {
            role_ask_connection: "gateway".to_string(),
            role_ask_model: "gpt-4o".to_string(),
            ..legacy_cfg("claude_code", BrainBackend::Off)
        };
        let want = target("gateway", "gpt-4o", "");
        assert_eq!(resolve(Role::Ask, &cfg), want);
        assert_eq!(provider_target(Role::Ask, &cfg), want);
        assert_eq!(reasoner_target(Role::Ask, &cfg), want);
    }

    /// Role/connection tokens are stable (they go into logs + the reasoner cache key).
    #[test]
    fn role_tokens_are_stable() {
        assert_eq!(Role::Notes.as_str(), "notes");
        assert_eq!(Role::Ask.as_str(), "ask");
        assert_eq!(Role::Live.as_str(), "live");
        assert!(target(CONN_LOCAL, "", "").is_reasoner_only());
        assert!(target(CONN_OFF, "", "").is_reasoner_only());
        assert!(target(CONN_AFM, "", "").is_reasoner_only());
        assert!(!target("claude_code", "", "").is_reasoner_only());
    }

    /// WS2 — the AppleFoundation backend maps to the on-device AFM reasoner-only target across all
    /// three resolution views, is auto-excluded from the cloud agentic gate, renders a human display
    /// name, and is reachable BOTH via `brain_backend = AppleFoundation` (legacy path) AND via an
    /// explicit `role_*_connection = "apple"` key (explicit path).
    #[test]
    fn apple_foundation_maps_to_afm_reasoner_only() {
        let want = target(CONN_AFM, "", "");

        // (a) brain_backend = AppleFoundation → CONN_AFM for the reasoner + resolve views (Ask/Live).
        let cfg = AppConfig {
            brain_backend: BrainBackend::AppleFoundation,
            ..AppConfig::default()
        };
        assert_eq!(legacy_brain_target(&cfg), want);
        for role in [Role::Ask, Role::Live] {
            assert_eq!(resolve(role, &cfg), want, "{role:?}");
            assert_eq!(reasoner_target(role, &cfg), want, "{role:?}");
            // The cloud agentic-eligibility gate (commands.rs) auto-excludes AFM like Local/Off.
            assert!(
                resolve(role, &cfg).is_reasoner_only(),
                "{role:?} must be reasoner-only"
            );
        }

        // (b) A human-facing display name (never the raw token) — mirrors the FE label.
        assert_eq!(
            connection_display_name(CONN_AFM),
            "Apple Intelligence (on-device)"
        );
        assert_ne!(connection_display_name(CONN_AFM), CONN_AFM);

        // (c) An EXPLICIT per-role `apple` key also routes to AFM and stays reasoner-only (the
        // provider factory then refuses to build a cloud provider for it — is_reasoner_only == true).
        let explicit = AppConfig {
            role_ask_connection: "apple".to_string(),
            ..AppConfig::default()
        };
        assert_eq!(resolve(Role::Ask, &explicit), want);
        assert_eq!(reasoner_target(Role::Ask, &explicit), want);
        assert!(provider_target(Role::Ask, &explicit).is_reasoner_only());
        assert_eq!(provider_target(Role::Ask, &explicit).connection, CONN_AFM);
    }
}
