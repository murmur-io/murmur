//! The "What runs where" RESOLVED AI MAP — a pure, read-only projection of the config into the
//! per-job (engine, model, locality) rows the Settings AI page renders. Display-only: nothing
//! here steers dispatch ([`crate::summarize::roles::resolve`] stays the one resolver); this
//! module only MIRRORS it, so the table can never disagree with backend truth. No content, no
//! PII, no key material — config-derived metadata only.

use serde::Serialize;

use crate::reason::{brain_model_by_id, class_model_id, ModelClass};
use crate::settings::AppConfig;
use crate::summarize::roles::{self, Role, CONN_AFM, CONN_LOCAL, CONN_OFF};

/// One row of the resolved map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiMapRow {
    /// Stable job token the FE keys on: `notes` | `ask` | `live` | `reactions` |
    /// `transcription` | `embeddings` | `redaction`.
    pub job: String,
    /// Display title ("Notes & summaries").
    pub title: String,
    /// Display engine name ("Claude Code", the GGUF's registry name, "Whisper", …).
    pub engine: String,
    /// Resolved model id ("" = the engine's own default).
    pub model: String,
    /// True when this job cannot egress (on-device / loopback Ollama).
    pub on_device: bool,
    /// True when the job's text passes the redaction firewall before leaving (cloud engines).
    pub redacted: bool,
    /// False when the job is currently switched off (reactions without `brain_live`,
    /// embeddings with semantic search off).
    pub active: bool,
    /// True for the three routable roles (notes/ask/live) — the FE offers "Change".
    pub routable: bool,
}

/// Display name for an on-device brain model id — registry name, raw id when unknown,
/// generic label when empty.
fn brain_display(id: &str) -> String {
    brain_model_by_id(id).map(|m| m.name.to_string()).unwrap_or_else(|| {
        if id.is_empty() {
            "On-device brain".to_string()
        } else {
            id.to_string()
        }
    })
}

/// Build one routable role row by mirroring [`roles::resolve`].
fn role_row(job: &str, title: &str, role: Role, cfg: &AppConfig) -> AiMapRow {
    let t = roles::resolve(role, cfg);
    let conn = t.connection.as_str();
    let base = AiMapRow {
        job: job.to_string(),
        title: title.to_string(),
        engine: String::new(),
        model: String::new(),
        on_device: true,
        redacted: false,
        active: true,
        routable: true,
    };
    match conn {
        CONN_LOCAL => AiMapRow {
            engine: brain_display(&t.model),
            model: t.model.clone(),
            ..base
        },
        CONN_OFF => AiMapRow {
            engine: "Retrieval only (no model)".to_string(),
            ..base
        },
        CONN_AFM => AiMapRow {
            engine: "Apple Intelligence (on-device)".to_string(),
            ..base
        },
        _ => {
            let cloud = crate::summarize::egress_is_cloud(conn, cfg);
            // Show the model the connection will ACTUALLY use: an empty resolved model falls
            // back to the connection's own model key. Mirror backend truth by deferring to the
            // canonical crate helper rather than re-copying its per-connection fallback arms.
            let model = crate::summarize::effective_model_requested(&t, cfg);
            AiMapRow {
                engine: roles::connection_display_name(conn).to_string(),
                model,
                on_device: !cloud,
                redacted: cloud,
                ..base
            }
        }
    }
}

/// The full resolved map in display order. Pure (config in, rows out).
pub fn ai_map_rows(cfg: &AppConfig) -> Vec<AiMapRow> {
    let light_id = class_model_id(cfg, ModelClass::Light).unwrap_or_default();
    let embed = cfg
        .embed_model_id
        .as_deref()
        .and_then(crate::embed::embed_model_by_id)
        .unwrap_or_else(crate::embed::default_embed_model);
    vec![
        role_row("notes", "Notes & summaries", Role::Notes, cfg),
        role_row("ask", "Ask & chat", Role::Ask, cfg),
        role_row("live", "Live @brain", Role::Live, cfg),
        AiMapRow {
            job: "reactions".to_string(),
            title: "Realtime reactions".to_string(),
            engine: brain_display(&light_id),
            model: light_id,
            on_device: true,
            redacted: false,
            active: cfg.brain_live,
            routable: false,
        },
        AiMapRow {
            job: "transcription".to_string(),
            title: "Transcription".to_string(),
            engine: "Whisper".to_string(),
            model: cfg.model_size.clone(),
            on_device: true,
            redacted: false,
            active: true,
            routable: false,
        },
        AiMapRow {
            job: "embeddings".to_string(),
            title: "Search index".to_string(),
            engine: embed.name.to_string(),
            model: embed.id.to_string(),
            on_device: true,
            redacted: false,
            active: cfg.semantic_search_enabled,
            routable: false,
        },
        AiMapRow {
            job: "redaction".to_string(),
            title: "Name redaction".to_string(),
            engine: "On-device NER".to_string(),
            model: String::new(),
            on_device: true,
            redacted: false,
            active: true,
            routable: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::postures::{apply_posture, Posture};

    fn row<'a>(rows: &'a [AiMapRow], job: &str) -> &'a AiMapRow {
        rows.iter().find(|r| r.job == job).unwrap()
    }

    #[test]
    fn default_config_is_cloud_claude_with_inactive_reactions() {
        let rows = ai_map_rows(&AppConfig::default());
        assert_eq!(rows.len(), 7);
        let notes = row(&rows, "notes");
        assert_eq!(notes.engine, "Claude Code");
        assert!(!notes.on_device);
        assert!(notes.redacted);
        assert!(notes.routable);
        let reactions = row(&rows, "reactions");
        assert!(reactions.on_device);
        assert!(!reactions.active, "brain_live defaults off ⇒ reactions row inactive");
        assert!(!reactions.routable);
        let tr = row(&rows, "transcription");
        assert_eq!(tr.engine, "Whisper");
        assert_eq!(tr.model, "small");
        assert!(tr.on_device);
    }

    #[test]
    fn fully_local_preset_maps_every_role_on_device() {
        let mut cfg = AppConfig::default();
        apply_posture(&mut cfg, Posture::FullyLocal);
        let rows = ai_map_rows(&cfg);
        for job in ["notes", "ask", "live"] {
            let r = row(&rows, job);
            assert!(r.on_device, "{job} must be on-device under Fully local");
            assert!(!r.redacted);
        }
        let heavy = crate::reason::brain_model_by_id("qwen3-4b-instruct-2507").unwrap();
        assert_eq!(row(&rows, "notes").engine, heavy.name);
        assert!(row(&rows, "reactions").active, "Fully local turns brain_live on");
    }

    #[test]
    fn ollama_default_resolves_its_own_model_and_loopback_is_on_device() {
        let cfg = AppConfig {
            provider_id: "ollama".to_string(),
            ..AppConfig::default()
        };
        let rows = ai_map_rows(&cfg);
        let notes = row(&rows, "notes");
        assert_eq!(notes.engine, "Ollama");
        assert_eq!(notes.model, cfg.ollama_model, "empty role model must fall back to ollama_model");
        assert!(notes.on_device, "loopback ollama must not classify as cloud");
        assert!(!notes.redacted);
    }

    #[test]
    fn cloud_nondefault_role_redacts_and_falls_back_to_connection_model() {
        // An explicit Ask role on anthropic with an EMPTY role model must render the
        // connection's own key (anthropic_model), be off-device, and be redacted.
        let cfg = AppConfig {
            role_ask_connection: "anthropic".into(),
            anthropic_model: "claude-opus-4-8".into(),
            ..Default::default()
        };
        let rows = ai_map_rows(&cfg);
        let ask = row(&rows, "ask");
        assert_eq!(ask.engine, "Anthropic API");
        assert_eq!(ask.model, cfg.anthropic_model, "empty role model falls back to anthropic_model");
        assert!(!ask.on_device, "anthropic is cloud ⇒ not on-device");
        assert!(ask.redacted, "cloud egress passes the redaction firewall");
        assert!(ask.active);
        assert!(ask.routable);
    }

    #[test]
    fn reasoner_only_role_is_on_device_with_no_model() {
        // An explicit Ask role of `off` (CONN_OFF) resolves to retrieval-only: on-device, no model.
        let cfg = AppConfig {
            role_ask_connection: CONN_OFF.into(),
            ..Default::default()
        };
        let rows = ai_map_rows(&cfg);
        let ask = row(&rows, "ask");
        assert_eq!(ask.engine, "Retrieval only (no model)");
        assert_eq!(ask.model, "");
        assert!(ask.on_device);
        assert!(!ask.redacted);
    }

    #[test]
    fn semantic_off_renders_embeddings_inactive() {
        let cfg = AppConfig {
            semantic_search_enabled: false,
            ..AppConfig::default()
        };
        assert!(!row(&ai_map_rows(&cfg), "embeddings").active);
    }
}
