//! Murmur Brain POSTURES (spec §2.1) — a DERIVED display state over the dispatch config, plus the
//! preset writers. The posture is **never stored**: [`derive_posture`] computes it from the resolved
//! role targets + `brain_live` on read, so a hand-tuned combination that matches no preset renders
//! [`Posture::Custom`] and can never lie (a "Fully Local" banner over an egressing Ask key is
//! structurally impossible — adversarial review, product #4).
//!
//! The presets ([`apply_posture`]) are the ONLY writers. Load-bearing rule (review code-truth #1):
//! the **Hybrid preset MUST NOT touch `role_live_*`** — `Role::Live` is the shipped in-meeting @brain
//! assistant, and pinning it to `local` would silently drop its agentic loop to the deterministic
//! floor. Realtime Reactions ride the `brain_live` flag + the `light()` engine handle, NOT `Role::Live`.

use serde::{Deserialize, Serialize};

use crate::reason::{class_model_id, ModelClass};
use crate::settings::{AppConfig, BrainBackend};
use crate::summarize::roles::{self, Role, CONN_AFM, CONN_LOCAL, CONN_OFF};

/// The three product postures + the derived-only `Custom` (any non-preset combination). Serialized
/// snake_case for the FE (`cloud` / `hybrid` / `fully_local` / `custom`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Posture {
    Cloud,
    Hybrid,
    FullyLocal,
    Custom,
}

impl Posture {
    /// Parse a settable posture from the FE. `Custom` is NOT settable (it is a derived label only),
    /// so it maps to `None` — the command rejects it with `InvalidArg`.
    pub fn from_settable(s: &str) -> Option<Self> {
        match s.trim() {
            "cloud" => Some(Posture::Cloud),
            "hybrid" => Some(Posture::Hybrid),
            "fully_local" => Some(Posture::FullyLocal),
            _ => None,
        }
    }
}

/// A connection routed to the on-device brain (local GGUF / off-stub / Apple sidecar) — i.e. NOT a
/// cloud provider. Egress happens only through the cloud-provider connections.
fn is_on_device(conn: &str) -> bool {
    conn == CONN_LOCAL || conn == CONN_OFF || conn == CONN_AFM
}

/// Derive the posture from the CURRENT config — pure, the single source of truth for the label. Keyed
/// on the RESOLVED role targets (so it accounts for the legacy `brain_backend` fallback), never on a
/// stored string.
pub fn derive_posture(cfg: &AppConfig) -> Posture {
    let notes = roles::resolve(Role::Notes, cfg).connection;
    let ask = roles::resolve(Role::Ask, cfg).connection;
    let live = roles::resolve(Role::Live, cfg).connection;
    let notes_od = is_on_device(&notes);
    let ask_od = is_on_device(&ask);
    let live_od = is_on_device(&live);

    // Fully Local: notes + ask + the @brain (Live) assistant are ALL on-device (zero cloud egress).
    // The Live axis is load-bearing (deep-review): Notes+Ask local but Live=cloud would mislabel an
    // egressing @brain as "Fully Local" — the exact privacy-label lie this module forbids. A config
    // with only some roles on-device is a hand-tuned combination and renders Custom.
    if notes_od && ask_od && live_od {
        return Posture::FullyLocal;
    }
    // Cloud / Hybrid: notes + ask + live are ALL cloud (nothing on-device routed). The only difference
    // is the brain_live flag. Anything else — e.g. a lone `brain_backend = Off` making Ask on-device
    // while Notes stays cloud — is a hand-tuned combination and renders Custom (never mislabeled Cloud).
    if !notes_od && !ask_od && !live_od {
        return if cfg.brain_live {
            Posture::Hybrid
        } else {
            Posture::Cloud
        };
    }
    Posture::Custom
}

/// Clear the connection/model/effort keys for a role so it inherits the legacy default mapping.
fn clear_role(cfg: &mut AppConfig, role: Role) {
    let (c, m, e) = match role {
        Role::Notes => (
            &mut cfg.role_notes_connection,
            &mut cfg.role_notes_model,
            &mut cfg.role_notes_effort,
        ),
        Role::Ask => (
            &mut cfg.role_ask_connection,
            &mut cfg.role_ask_model,
            &mut cfg.role_ask_effort,
        ),
        Role::Live => (
            &mut cfg.role_live_connection,
            &mut cfg.role_live_model,
            &mut cfg.role_live_effort,
        ),
    };
    c.clear();
    m.clear();
    e.clear();
}

/// Set a role's explicit connection + model (effort cleared).
fn set_role(cfg: &mut AppConfig, role: Role, connection: &str, model: &str) {
    let (c, m, e) = match role {
        Role::Notes => (
            &mut cfg.role_notes_connection,
            &mut cfg.role_notes_model,
            &mut cfg.role_notes_effort,
        ),
        Role::Ask => (
            &mut cfg.role_ask_connection,
            &mut cfg.role_ask_model,
            &mut cfg.role_ask_effort,
        ),
        Role::Live => (
            &mut cfg.role_live_connection,
            &mut cfg.role_live_model,
            &mut cfg.role_live_effort,
        ),
    };
    *c = connection.to_string();
    *m = model.to_string();
    e.clear();
}

/// Apply a posture PRESET, mutating the dispatch keys. Presets are the only writers; `Custom` is a
/// no-op (never written). `derive_posture(apply_posture(cfg, p)) == p` for the three real postures.
pub fn apply_posture(cfg: &mut AppConfig, posture: Posture) {
    match posture {
        Posture::Cloud => {
            cfg.brain_live = false;
            cfg.brain_backend = BrainBackend::Cloud;
            clear_role(cfg, Role::Notes);
            clear_role(cfg, Role::Ask);
            clear_role(cfg, Role::Live);
        }
        Posture::Hybrid => {
            // Notes/Ask/@brain stay on the default cloud provider; realtime + local facts come from
            // brain_live + light(). CRITICAL: do NOT touch role_live_* (would lobotomize @brain).
            cfg.brain_live = true;
            cfg.brain_backend = BrainBackend::Cloud;
            clear_role(cfg, Role::Notes);
            clear_role(cfg, Role::Ask);
            clear_role(cfg, Role::Live);
        }
        Posture::FullyLocal => {
            // Zero egress: notes + ask on the heavy engine; @brain on the light engine (its agentic
            // loop degrades to the deterministic floor by the existing local-only gate — the posture
            // UI states this). brain_live powers realtime + local fact extraction.
            cfg.brain_live = true;
            let heavy = class_model_id(cfg, ModelClass::Heavy).unwrap_or_default();
            let light = class_model_id(cfg, ModelClass::Light).unwrap_or_default();
            set_role(cfg, Role::Notes, CONN_LOCAL, &heavy);
            set_role(cfg, Role::Ask, CONN_LOCAL, &heavy);
            set_role(cfg, Role::Live, CONN_LOCAL, &light);
        }
        Posture::Custom => {
            // Derived-only label — never written.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_cloud() {
        assert_eq!(derive_posture(&AppConfig::default()), Posture::Cloud);
    }

    #[test]
    fn presets_round_trip_through_derive() {
        for p in [Posture::Cloud, Posture::Hybrid, Posture::FullyLocal] {
            let mut cfg = AppConfig::default();
            apply_posture(&mut cfg, p);
            assert_eq!(derive_posture(&cfg), p, "apply then derive must round-trip {p:?}");
        }
    }

    #[test]
    fn hybrid_never_touches_role_live() {
        // CRITICAL (review code-truth #1): the Hybrid preset must leave the @brain (Live) role on its
        // cloud default — never pin it local — so the agentic loop survives.
        let mut cfg = AppConfig::default();
        apply_posture(&mut cfg, Posture::Hybrid);
        assert!(
            cfg.role_live_connection.trim().is_empty(),
            "Hybrid must not set role_live_connection (got {:?})",
            cfg.role_live_connection
        );
        assert!(cfg.brain_live, "Hybrid enables brain_live");
    }

    #[test]
    fn hand_edited_combo_renders_custom_not_a_lie() {
        // Fully Local, then a hand-edit that repoints Ask to the cloud → the label must become Custom,
        // never keep saying "Fully Local" over an egressing Ask (product #4).
        let mut cfg = AppConfig::default();
        apply_posture(&mut cfg, Posture::FullyLocal);
        assert_eq!(derive_posture(&cfg), Posture::FullyLocal);
        cfg.role_ask_connection = crate::summarize::PROVIDER_CLAUDE_CODE.to_string();
        cfg.role_ask_model.clear();
        assert_eq!(
            derive_posture(&cfg),
            Posture::Custom,
            "a cloud Ask under a local Notes must be Custom, not a mislabeled Fully Local"
        );
    }

    #[test]
    fn fully_local_requires_the_live_axis_too() {
        // Notes + Ask local but @brain (Live) still on the cloud provider must NOT read "Fully Local"
        // (the @brain assistant would egress live meeting context). Deep-review: the Live-axis lie.
        let mut cfg = AppConfig::default();
        apply_posture(&mut cfg, Posture::FullyLocal);
        assert_eq!(derive_posture(&cfg), Posture::FullyLocal);
        cfg.role_live_connection = crate::summarize::PROVIDER_CLAUDE_CODE.to_string();
        cfg.role_live_model.clear();
        assert_eq!(
            derive_posture(&cfg),
            Posture::Custom,
            "a cloud @brain (Live) under local Notes+Ask must be Custom, never mislabeled Fully Local"
        );
    }

    #[test]
    fn off_backend_is_custom_never_cloud() {
        // A legacy brain_backend=Off makes Ask/@brain on-device (stub) while Notes stays cloud — a
        // mixed state that must render Custom, not Cloud (review code-truth #12).
        let cfg = AppConfig {
            brain_backend: BrainBackend::Off,
            ..AppConfig::default()
        };
        assert_eq!(derive_posture(&cfg), Posture::Custom);
    }

    #[test]
    fn fully_local_pins_notes_ask_local_and_heavy() {
        let mut cfg = AppConfig::default();
        apply_posture(&mut cfg, Posture::FullyLocal);
        assert_eq!(cfg.role_notes_connection, CONN_LOCAL);
        assert_eq!(cfg.role_ask_connection, CONN_LOCAL);
        assert_eq!(cfg.role_live_connection, CONN_LOCAL);
        // Notes/Ask use the heavy default; @brain uses the light default.
        assert_eq!(cfg.role_notes_model, "qwen3-4b-instruct-2507");
        assert_eq!(cfg.role_live_model, "qwen3-1.7b");
    }

    #[test]
    fn custom_is_not_settable() {
        assert_eq!(Posture::from_settable("cloud"), Some(Posture::Cloud));
        assert_eq!(Posture::from_settable("hybrid"), Some(Posture::Hybrid));
        assert_eq!(Posture::from_settable("fully_local"), Some(Posture::FullyLocal));
        assert_eq!(Posture::from_settable("custom"), None);
        assert_eq!(Posture::from_settable("bogus"), None);
    }
}
