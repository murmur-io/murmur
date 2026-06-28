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
    /// Preferred microphone input device by NAME (cpal exposes no stable id). None = system
    /// default. A saved device that is no longer present falls back to default at capture time.
    pub input_device: Option<String>,
    /// Capture system audio (the other side of the call) via ScreenCaptureKit. Default off.
    pub capture_system_audio: bool,
    /// Voice-activity-detection pre-segmentation + ASR-feed loudness normalisation for the
    /// Accurate batch transcription. Default ON; off = transcribe the whole buffer (legacy).
    pub vad_enabled: bool,
    /// Keep faithful per-stream float32 MASTER archives (mic native + system 48k) alongside the
    /// 16 kHz playback mix. Default OFF — on, it roughly doubles audio disk use per recording.
    pub keep_hires_masters: bool,
    /// Run N-way speaker diarization on the system ("others") stream to label remote speakers
    /// (others-0/1/2). Default OFF; requires system-audio capture + downloads ~40 MB of models.
    pub diarize_others: bool,
    /// Echo cancellation (VPIO): capture an AEC'd mic in parallel with cpal and use it as the ASR
    /// feed (the raw cpal mic stays the archive). Default OFF; EXPERIMENTAL — best with speakers.
    pub aec_enabled: bool,
    /// Whisper model size: "tiny" | "base" | "small" | "medium" | "large-v3-turbo" |
    /// "large-v3". Default "large-v3" (~3 GB, multilingual) — best transcription quality,
    /// notably for Polish; downloaded on demand via `download_model`.
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
    /// Require an `Authorization: Bearer <token>` on EVERY MCP method (E3) — including
    /// initialize / tools/list / ping, not just tools/call. Default ON (fail-closed): an
    /// unauthenticated localhost process must not be able to even enumerate the meeting tools.
    /// Bind is always 127.0.0.1.
    pub mcp_require_token: bool,
    /// Require a passing biometric (Touch ID, falling back to device passcode) before unlocking a
    /// sealed folder. Default ON. Degrades to allow when no biometric/passcode policy is available
    /// (no Touch ID hardware / CI), so it never locks the user out.
    pub lock_require_biometric: bool,
    /// Auto-relock all session-unlocked folders (and zeroize the cached KEK) when screen
    /// capture/sharing STARTS. Default ON.
    pub relock_on_screenshare: bool,
    /// E10 — one-time cloud-egress consent. The FIRST time the user would send meeting content
    /// to a cloud LLM (claude_code / anthropic), the egress path refuses until this is flipped
    /// `true` via the dedicated `consent_to_cloud_egress` command. Default OFF (fail-closed): no
    /// content leaves the device until the user has explicitly acknowledged it once.
    ///
    /// SECURITY: this flag DOES round-trip through `AppConfigDto` (so `get_config` can carry the
    /// current value out for the FE to DISPLAY consent status), but `dto_to_config`/`save_config`
    /// IGNORE the incoming DTO value and PRESERVE whatever is already stored — the FE cannot set,
    /// clear, or clobber it as a side effect of a normal settings save (BLK-4). It is mutated ONLY
    /// by the purpose-built `consent_to_cloud_egress` command, so flipping it is an explicit,
    /// auditable user act.
    pub cloud_egress_consented: bool,
    /// brain2 RAG Phase 2b — master gate for the on-device semantic (vector) retrieval layer.
    /// Default OFF (`#[serde(default)]` ⇒ a config persisted before this field existed loads as
    /// `false`). When OFF, NOTHING in the vector path runs: no chunk indexing on note creation, the
    /// Ask-My-Vault corpus stays pure FTS, and the MCP `search_semantic` tool reports "disabled" —
    /// so shipping this changes NOTHING vs today. It is wired ON only once a real embedding model
    /// (Phase 2c) replaces the stub; until then the only embedder is the deterministic `StubEmbedder`.
    #[serde(default)]
    pub semantic_search_enabled: bool,
    /// Phase B — optional explicit path to a local reasoning GGUF for the on-device brain
    /// (`MistralReasoner`). `None` (the default) means the resolver falls back to the default model
    /// filename inside the shared models dir. Consulted ONLY when the `local-brain` feature is
    /// compiled in; the default build never loads it. `#[serde(default)]` ⇒ a config persisted before
    /// this field existed loads as `None`.
    #[serde(default)]
    pub brain_model_path: Option<String>,
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
            input_device: None,
            capture_system_audio: false,
            vad_enabled: true,
            keep_hires_masters: false,
            diarize_others: false,
            aec_enabled: false,
            model_size: "large-v3".to_string(),
            voice_trigger: false,
            onboarded: false,
            note_style: "standard".to_string(),
            auto_organize: false,
            note_language: "auto".to_string(),
            mcp_require_token: true,
            lock_require_biometric: true,
            relock_on_screenshare: true,
            cloud_egress_consented: false,
            semantic_search_enabled: false,
            brain_model_path: None,
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
const K_INPUT_DEVICE: &str = "input_device";
const K_CAPTURE_SYSTEM_AUDIO: &str = "capture_system_audio";
const K_VAD_ENABLED: &str = "vad_enabled";
const K_KEEP_HIRES_MASTERS: &str = "keep_hires_masters";
const K_DIARIZE_OTHERS: &str = "diarize_others";
const K_AEC_ENABLED: &str = "aec_enabled";
const K_MODEL_SIZE: &str = "model_size";
const K_VOICE_TRIGGER: &str = "voice_trigger";
const K_ONBOARDED: &str = "onboarded";
const K_NOTE_STYLE: &str = "note_style";
const K_AUTO_ORGANIZE: &str = "auto_organize";
const K_NOTE_LANGUAGE: &str = "note_language";
const K_MCP_REQUIRE_TOKEN: &str = "mcp_require_token";
const K_LOCK_REQUIRE_BIOMETRIC: &str = "lock_require_biometric";
const K_RELOCK_ON_SCREENSHARE: &str = "relock_on_screenshare";
const K_CLOUD_EGRESS_CONSENTED: &str = "cloud_egress_consented";
const K_SEMANTIC_SEARCH_ENABLED: &str = "semantic_search_enabled";
const K_BRAIN_MODEL_PATH: &str = "brain_model_path";

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
        cfg.input_device = opt(db.get_setting(K_INPUT_DEVICE)?);
        if let Some(v) = db.get_setting(K_CAPTURE_SYSTEM_AUDIO)? {
            cfg.capture_system_audio = v == "true";
        }
        if let Some(v) = db.get_setting(K_VAD_ENABLED)? {
            cfg.vad_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_KEEP_HIRES_MASTERS)? {
            cfg.keep_hires_masters = v == "true";
        }
        if let Some(v) = db.get_setting(K_DIARIZE_OTHERS)? {
            cfg.diarize_others = v == "true";
        }
        if let Some(v) = db.get_setting(K_AEC_ENABLED)? {
            cfg.aec_enabled = v == "true";
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
        if let Some(v) = db.get_setting(K_LOCK_REQUIRE_BIOMETRIC)? {
            cfg.lock_require_biometric = v == "true";
        }
        if let Some(v) = db.get_setting(K_RELOCK_ON_SCREENSHARE)? {
            cfg.relock_on_screenshare = v == "true";
        }
        if let Some(v) = db.get_setting(K_CLOUD_EGRESS_CONSENTED)? {
            cfg.cloud_egress_consented = v == "true";
        }
        if let Some(v) = db.get_setting(K_SEMANTIC_SEARCH_ENABLED)? {
            cfg.semantic_search_enabled = v == "true";
        }
        cfg.brain_model_path = opt(db.get_setting(K_BRAIN_MODEL_PATH)?);

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
        db.set_setting(K_INPUT_DEVICE, self.input_device.as_deref().unwrap_or(""))?;
        db.set_setting(
            K_CAPTURE_SYSTEM_AUDIO,
            if self.capture_system_audio { "true" } else { "false" },
        )?;
        db.set_setting(
            K_VAD_ENABLED,
            if self.vad_enabled { "true" } else { "false" },
        )?;
        db.set_setting(
            K_KEEP_HIRES_MASTERS,
            if self.keep_hires_masters { "true" } else { "false" },
        )?;
        db.set_setting(
            K_DIARIZE_OTHERS,
            if self.diarize_others { "true" } else { "false" },
        )?;
        db.set_setting(
            K_AEC_ENABLED,
            if self.aec_enabled { "true" } else { "false" },
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
        db.set_setting(
            K_LOCK_REQUIRE_BIOMETRIC,
            if self.lock_require_biometric { "true" } else { "false" },
        )?;
        db.set_setting(
            K_RELOCK_ON_SCREENSHARE,
            if self.relock_on_screenshare { "true" } else { "false" },
        )?;
        db.set_setting(
            K_CLOUD_EGRESS_CONSENTED,
            if self.cloud_egress_consented { "true" } else { "false" },
        )?;
        db.set_setting(
            K_SEMANTIC_SEARCH_ENABLED,
            if self.semantic_search_enabled { "true" } else { "false" },
        )?;
        db.set_setting(
            K_BRAIN_MODEL_PATH,
            self.brain_model_path.as_deref().unwrap_or(""),
        )?;
        Ok(())
    }

    /// E10 — record the user's one-time consent to send meeting content to a cloud LLM provider.
    /// Flips the in-memory flag AND persists it. This is the ONLY supported way to grant consent;
    /// it is deliberately separate from `save_config` so consent can never be granted as an
    /// incidental side effect of a settings write.
    ///
    /// The FE wires a first-cloud-send confirmation prompt that calls the `consent_to_cloud_egress`
    /// Tauri command before the first claude_code/anthropic run. Until the user confirms, the egress
    /// path returns `AppError::Unavailable("cloud egress not consented …")`, which the FE detects to
    /// surface the consent dialog.
    pub fn grant_cloud_egress_consent(&mut self, db: &Db) -> Result<()> {
        self.cloud_egress_consented = true;
        db.set_setting(K_CLOUD_EGRESS_CONSENTED, "true")
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
        // Stage E security flags default ON.
        assert!(cfg.lock_require_biometric);
        assert!(cfg.relock_on_screenshare);
        // E10 cloud-egress consent is fail-closed (OFF) until explicitly granted.
        assert!(!cfg.cloud_egress_consented);
        // Phase 2b semantic search is OFF by default — shipping it changes nothing.
        assert!(!cfg.semantic_search_enabled);
    }

    /// A config persisted before `semantic_search_enabled` existed (key absent from both the JSON
    /// DTO and the settings table) must load with the flag defaulting to `false` — `#[serde(default)]`
    /// for the JSON path, the missing-key fallthrough for the settings-table path. Proves the
    /// prod-safe default for existing installs.
    #[test]
    fn missing_semantic_flag_defaults_off() {
        // JSON path: deserialize a payload that omits the field entirely.
        let json = r#"{
            "providerId":"claude_code","vaultPath":null,"vaultSubfolder":null,
            "whisperModelPath":null,"language":null,"anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "inputDevice":null,"captureSystemAudio":false,"vadEnabled":true,"keepHiresMasters":false,
            "diarizeOthers":false,"aecEnabled":false,"modelSize":"large-v3","voiceTrigger":false,
            "onboarded":false,"noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto",
            "mcpRequireToken":true,"lockRequireBiometric":true,"relockOnScreenshare":true,
            "cloudEgressConsented":false
        }"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert!(!cfg.semantic_search_enabled, "serde default must be false");

        // Settings-table path: an empty DB (no key written) loads false.
        let db = temp_db();
        assert!(!AppConfig::load(&db).unwrap().semantic_search_enabled);
    }

    #[test]
    fn semantic_search_flag_round_trips() {
        let db = temp_db();
        let cfg = AppConfig {
            semantic_search_enabled: true,
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert!(AppConfig::load(&db).unwrap().semantic_search_enabled);
    }

    #[test]
    fn cloud_egress_consent_grant_persists() {
        let db = temp_db();
        let mut cfg = AppConfig::load(&db).unwrap();
        assert!(!cfg.cloud_egress_consented);
        cfg.grant_cloud_egress_consent(&db).unwrap();
        assert!(cfg.cloud_egress_consented);
        // Survives a reload from the settings table.
        let reloaded = AppConfig::load(&db).unwrap();
        assert!(reloaded.cloud_egress_consented);
    }

    #[test]
    fn security_flags_round_trip() {
        let db = temp_db();
        let cfg = AppConfig {
            lock_require_biometric: false,
            relock_on_screenshare: false,
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert!(!loaded.lock_require_biometric);
        assert!(!loaded.relock_on_screenshare);
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
