use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::storage::Db;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    /// default "claude_code"
    pub provider_id: String,
    pub vault_path: Option<String>,
    pub vault_subfolder: Option<String>,
    pub whisper_model_path: Option<String>,
    /// "en" | None=auto
    pub language: Option<String>,
    /// default "claude-opus-4-8"
    pub anthropic_model: String,
    /// default "http://localhost:11434"
    pub ollama_base_url: String,
    /// default "llama3.1"
    pub ollama_model: String,
    /// default "claude"
    pub claude_binary: String,
    /// Capture system audio (the other side of the call) via ScreenCaptureKit. Default off.
    pub capture_system_audio: bool,
    /// Whisper model size: "tiny" | "base" | "small" | "medium" | "large-v3". Default "small".
    pub model_size: String,
    /// Voice trigger: start recording when a wake phrase is heard. Default off.
    pub voice_trigger: bool,
    /// Whether the first-run onboarding has been completed.
    pub onboarded: bool,
    /// Summary style preset: "standard" | "brief" | "detailed" | "action".
    pub note_style: String,
    /// When true, Claude files each note into a thematic subfolder of the vault.
    pub auto_organize: bool,
    /// Summary note language: "auto" (match the meeting) | "en" | "pl" | "de" | ... .
    pub note_language: String,
    /// Require an `Authorization: Bearer <token>` on MCP `tools/call`. Default OFF so the
    /// existing local Claude connection keeps working; discovery (initialize/tools/list/ping)
    /// stays open regardless. Bind is always 127.0.0.1.
    pub mcp_require_token: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            provider_id: "claude_code".to_string(),
            vault_path: None,
            vault_subfolder: None,
            whisper_model_path: None,
            language: None,
            anthropic_model: "claude-opus-4-8".to_string(),
            ollama_base_url: "http://localhost:11434".to_string(),
            ollama_model: "llama3.1".to_string(),
            claude_binary: "claude".to_string(),
            capture_system_audio: false,
            model_size: "small".to_string(),
            voice_trigger: false,
            onboarded: false,
            note_style: "standard".to_string(),
            auto_organize: false,
            note_language: "auto".to_string(),
            mcp_require_token: false,
        }
    }
}

// Settings-table keys. Kept as constants so `load`/`save` stay in sync.
const K_PROVIDER_ID: &str = "provider_id";
const K_VAULT_PATH: &str = "vault_path";
const K_VAULT_SUBFOLDER: &str = "vault_subfolder";
const K_WHISPER_MODEL_PATH: &str = "whisper_model_path";
const K_LANGUAGE: &str = "language";
const K_ANTHROPIC_MODEL: &str = "anthropic_model";
const K_OLLAMA_BASE_URL: &str = "ollama_base_url";
const K_OLLAMA_MODEL: &str = "ollama_model";
const K_CLAUDE_BINARY: &str = "claude_binary";
const K_CAPTURE_SYSTEM_AUDIO: &str = "capture_system_audio";
const K_MODEL_SIZE: &str = "model_size";
const K_VOICE_TRIGGER: &str = "voice_trigger";
const K_ONBOARDED: &str = "onboarded";
const K_NOTE_STYLE: &str = "note_style";
const K_AUTO_ORGANIZE: &str = "auto_organize";
const K_NOTE_LANGUAGE: &str = "note_language";
const K_MCP_REQUIRE_TOKEN: &str = "mcp_require_token";

impl AppConfig {
    /// Read all known keys from the settings table, falling back to `Default` for any
    /// key that has never been written. Empty stored strings for `Option` fields are
    /// treated as `None`.
    pub fn load(db: &Db) -> Result<Self> {
        let mut cfg = AppConfig::default();

        if let Some(v) = db.get_setting(K_PROVIDER_ID)? {
            if !v.is_empty() {
                cfg.provider_id = v;
            }
        }
        cfg.vault_path = opt(db.get_setting(K_VAULT_PATH)?);
        cfg.vault_subfolder = opt(db.get_setting(K_VAULT_SUBFOLDER)?);
        cfg.whisper_model_path = opt(db.get_setting(K_WHISPER_MODEL_PATH)?);
        cfg.language = opt(db.get_setting(K_LANGUAGE)?);
        if let Some(v) = db.get_setting(K_ANTHROPIC_MODEL)? {
            if !v.is_empty() {
                cfg.anthropic_model = v;
            }
        }
        if let Some(v) = db.get_setting(K_OLLAMA_BASE_URL)? {
            if !v.is_empty() {
                cfg.ollama_base_url = v;
            }
        }
        if let Some(v) = db.get_setting(K_OLLAMA_MODEL)? {
            if !v.is_empty() {
                cfg.ollama_model = v;
            }
        }
        if let Some(v) = db.get_setting(K_CLAUDE_BINARY)? {
            if !v.is_empty() {
                cfg.claude_binary = v;
            }
        }
        if let Some(v) = db.get_setting(K_CAPTURE_SYSTEM_AUDIO)? {
            cfg.capture_system_audio = v == "true";
        }
        if let Some(v) = db.get_setting(K_MODEL_SIZE)? {
            if !v.is_empty() {
                cfg.model_size = v;
            }
        }
        if let Some(v) = db.get_setting(K_VOICE_TRIGGER)? {
            cfg.voice_trigger = v == "true";
        }
        if let Some(v) = db.get_setting(K_ONBOARDED)? {
            cfg.onboarded = v == "true";
        }
        if let Some(v) = db.get_setting(K_NOTE_STYLE)? {
            if !v.is_empty() {
                cfg.note_style = v;
            }
        }
        if let Some(v) = db.get_setting(K_AUTO_ORGANIZE)? {
            cfg.auto_organize = v == "true";
        }
        if let Some(v) = db.get_setting(K_NOTE_LANGUAGE)? {
            if !v.is_empty() {
                cfg.note_language = v;
            }
        }
        if let Some(v) = db.get_setting(K_MCP_REQUIRE_TOKEN)? {
            cfg.mcp_require_token = v == "true";
        }

        Ok(cfg)
    }

    /// Persist every field into the settings table (NOT the api key — that's Keychain).
    /// `Option` fields persist as an empty string when `None`.
    pub fn save(&self, db: &Db) -> Result<()> {
        db.set_setting(K_PROVIDER_ID, &self.provider_id)?;
        db.set_setting(K_VAULT_PATH, self.vault_path.as_deref().unwrap_or(""))?;
        db.set_setting(
            K_VAULT_SUBFOLDER,
            self.vault_subfolder.as_deref().unwrap_or(""),
        )?;
        db.set_setting(
            K_WHISPER_MODEL_PATH,
            self.whisper_model_path.as_deref().unwrap_or(""),
        )?;
        db.set_setting(K_LANGUAGE, self.language.as_deref().unwrap_or(""))?;
        db.set_setting(K_ANTHROPIC_MODEL, &self.anthropic_model)?;
        db.set_setting(K_OLLAMA_BASE_URL, &self.ollama_base_url)?;
        db.set_setting(K_OLLAMA_MODEL, &self.ollama_model)?;
        db.set_setting(K_CLAUDE_BINARY, &self.claude_binary)?;
        db.set_setting(
            K_CAPTURE_SYSTEM_AUDIO,
            if self.capture_system_audio { "true" } else { "false" },
        )?;
        db.set_setting(K_MODEL_SIZE, &self.model_size)?;
        db.set_setting(
            K_VOICE_TRIGGER,
            if self.voice_trigger { "true" } else { "false" },
        )?;
        db.set_setting(K_ONBOARDED, if self.onboarded { "true" } else { "false" })?;
        db.set_setting(K_NOTE_STYLE, &self.note_style)?;
        db.set_setting(
            K_AUTO_ORGANIZE,
            if self.auto_organize { "true" } else { "false" },
        )?;
        db.set_setting(K_NOTE_LANGUAGE, &self.note_language)?;
        db.set_setting(
            K_MCP_REQUIRE_TOKEN,
            if self.mcp_require_token { "true" } else { "false" },
        )?;
        Ok(())
    }
}

/// Map a stored setting to an `Option`: a missing key or an empty string → `None`.
fn opt(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_db() -> Db {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "meetnotes-config-test-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // Tests use an explicit key (NOT the Keychain) — Db::open would hit macOS Keychain and
        // prompt/block depending on the test-binary signature.
        Db::open_with_key(
            &p,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap()
    }

    #[test]
    fn load_returns_defaults_on_empty_db() {
        let db = temp_db();
        let cfg = AppConfig::load(&db).unwrap();
        assert_eq!(cfg.provider_id, "claude_code");
        assert_eq!(cfg.anthropic_model, "claude-opus-4-8");
        assert!(cfg.vault_path.is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let db = temp_db();
        let cfg = AppConfig {
            provider_id: "ollama".to_string(),
            vault_path: Some("/vault".to_string()),
            language: Some("en".to_string()),
            ..Default::default()
        };
        cfg.save(&db).unwrap();

        let loaded = AppConfig::load(&db).unwrap();
        assert_eq!(loaded.provider_id, "ollama");
        assert_eq!(loaded.vault_path.as_deref(), Some("/vault"));
        assert_eq!(loaded.language.as_deref(), Some("en"));
    }

    #[test]
    fn empty_option_persists_as_none() {
        let db = temp_db();
        let cfg = AppConfig::default(); // vault_path = None
        cfg.save(&db).unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert!(loaded.vault_path.is_none());
        let _ = PathBuf::new();
    }
}
