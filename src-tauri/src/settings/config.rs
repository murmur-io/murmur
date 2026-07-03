use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::storage::Db;

/// Which reasoning backend powers the on-device "brain" pre-analysis step (Flow A,
/// `orchestrate.rs`). The brain is a SEAM — see [`crate::reason::active_reasoner`]:
///
/// - **`Cloud`** (the DEFAULT, the user's choice): the SMARTEST option — the cloud LLM
///   ([`crate::reason::CloudReasoner`]) reasons via the SAME `make_provider` factory the note
///   summary uses, so the egress posture is IDENTICAL (RedactingProvider + fail-closed
///   `cloud_egress_consented` gate). No local model required.
/// - **`Local`**: the on-device GGUF (`MistralReasoner`, e.g. Bielik) when the selected model file
///   is present on disk (mistralrs is always compiled); otherwise the `StubReasoner`.
/// - **`Off`**: the dependency-free `StubReasoner` (the deterministic floor — zero brain).
///
/// `#[serde(default)]` on the field ⇒ a config persisted before this field existed loads as
/// `Cloud` (the chosen default). NEVER changes the egress envelope: a Cloud brain that lacks
/// consent simply falls back to the deterministic floor at call time (best-effort).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BrainBackend {
    /// Cloud LLM via the summarizer-provider seam — the default brain.
    #[default]
    Cloud,
    /// On-device GGUF when present, else the stub.
    Local,
    /// No brain — the deterministic floor.
    Off,
}

impl BrainBackend {
    /// Stable lowercase token persisted in the settings table (`cloud` | `local` | `off`).
    pub fn as_str(self) -> &'static str {
        match self {
            BrainBackend::Cloud => "cloud",
            BrainBackend::Local => "local",
            BrainBackend::Off => "off",
        }
    }

    /// Parse the persisted token; an unknown/empty value falls back to the default (`Cloud`).
    pub fn from_str_or_default(s: &str) -> Self {
        match s {
            "local" => BrainBackend::Local,
            "off" => BrainBackend::Off,
            _ => BrainBackend::Cloud,
        }
    }
}

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
    /// Brain/AI model OVERRIDE for the active cloud provider. Empty `""` (the default) means
    /// "use the provider's own default". For `claude_code` it is passed as `--model <id>`; for
    /// `anthropic` it takes precedence over `anthropic_model`. `#[serde(default)]` ⇒ a config
    /// persisted before this field existed loads as `""` (provider default).
    #[serde(default)]
    pub provider_model: String,
    /// Brain/AI reasoning EFFORT for the active cloud provider: `""` (provider default), `"low"`,
    /// `"medium"`, or `"high"`. ONLY the direct `anthropic` HTTP provider honors it (adaptive
    /// thinking + output effort); the `claude_code` CLI has NO effort flag, so it is a no-op there.
    /// `#[serde(default)]` ⇒ a config persisted before this field existed loads as `""`.
    #[serde(default)]
    pub provider_effort: String,
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
    /// Post-hoc on-device echo cancellation (WebRTC AEC3): after Stop, cancel the captured
    /// system-audio out of the mic track (ASR feed AND playback mix) when a speaker leak is
    /// detected. Pure offline DSP, no live-call impact; falls back to the raw mic on any anomaly.
    /// Fixes the doubled voice when recording on speakers — but rides an UNPROVEN v0.1 AEC3 crate
    /// on every system-audio recording, so it is DEFAULT-OFF until real-Mac-verified. Users can
    /// opt in from Settings.
    /// `#[serde(default)]` ⇒ a config persisted before this field existed loads as `false`
    /// (the safe default) instead of resetting the whole config.
    #[serde(default)]
    pub post_aec_enabled: bool,
    /// Whisper model size: "tiny" | "base" | "small" | "medium" | "large-v3-turbo" |
    /// "large-v3". Default "small" (~466 MB) — a RAM-safe default: `large-v3` is ~3 GB and swaps
    /// on 8 GB Macs, and onboarding already preselects `small`. All sizes (incl. `large-v3`, best
    /// for Polish) stay selectable; the chosen model is downloaded on demand via `download_model`.
    pub model_size: String,
    /// Voice trigger: start recording when a wake phrase is heard. Default off.
    pub voice_trigger: bool,
    /// Whether the first-run onboarding has been completed.
    pub onboarded: bool,
    /// Summary style preset: "standard" | "brief" | "detailed" | "action".
    pub note_style: String,
    /// ENHANCE-MY-NOTES: how the user's typed in-meeting notes shape the summary.
    /// "enhance" (default) — the notes become the SKELETON of the generated note (they ride
    /// INSIDE the redacted provider prompt — a deliberate, loud, consent-riding egress);
    /// "append" — legacy: transcript-only summary + verbatim `## My notes` section.
    /// Empty/unknown values fall back to "enhance".
    /// `#[serde(default)]` ⇒ a config persisted before this field existed loads as `"enhance"`.
    #[serde(default)]
    pub notes_mode: String,
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
    /// by the purpose-built `consent_to_cloud_egress` / `revoke_cloud_egress` commands, so flipping
    /// it either way is an explicit, auditable user act.
    pub cloud_egress_consented: bool,
    /// brain2 RAG Phase 2b — master gate for the on-device semantic (vector) retrieval layer.
    /// Default OFF (`#[serde(default)]` ⇒ a config persisted before this field existed loads as
    /// `false`). When OFF, NOTHING in the vector path runs: no chunk indexing on note creation, the
    /// Ask-My-Vault corpus stays pure FTS, and the MCP `search_semantic` tool reports "disabled" —
    /// so shipping this changes NOTHING vs today. It is wired ON only once a real embedding model
    /// (Phase 2c) replaces the stub; until then the only embedder is the deterministic `StubEmbedder`.
    #[serde(default)]
    pub semantic_search_enabled: bool,
    /// brain2 RAG Phase 2 — the SELECTED on-device embedding model id (from
    /// [`crate::embed::EMBED_MODELS`], e.g. `"multilingual-e5-small"` (default) / `"mmlw-e5-small"`).
    /// `None`/empty (the default) resolves to `multilingual-e5-small` ⇒ BYTE-IDENTICAL to the
    /// historical hardcoded behavior. Set via the `select_embed_model` command, which also triggers a
    /// re-index (a different model's vectors are not comparable). `#[serde(default)]` ⇒ a config
    /// persisted before this field existed loads as `None`. All bundled options are BERT/384 so
    /// switching costs NO `vec0` schema migration (only a re-embed).
    #[serde(default)]
    pub embed_model_id: Option<String>,
    /// Phase B — optional explicit path to a local reasoning GGUF for the on-device brain
    /// (`MistralReasoner`). `None` (the default) means the resolver falls back to the default model
    /// filename inside the shared models dir. Consulted at runtime when `brain_backend == Local`;
    /// with no model on disk the resolver falls back to the stub. `#[serde(default)]` ⇒ a config
    /// persisted before this field existed loads as `None`.
    #[serde(default)]
    pub brain_model_path: Option<String>,
    /// Phase B (model registry) — the SELECTED on-device brain model id (from `reason::BRAIN_MODELS`,
    /// e.g. `bielik-11b-v3` / `qwen3-14b` / `qwen2.5-3b`). `None` (the default) means no model is
    /// chosen ⇒ the resolver falls back to the `StubReasoner`. Set via the `select_brain_model`
    /// command. Consulted at runtime when `brain_backend == Local`. `#[serde(default)]` ⇒ a config
    /// persisted before this field existed loads as `None`.
    #[serde(default)]
    pub brain_model_id: Option<String>,
    /// Which reasoning backend powers the brain pre-analysis (Flow A). Default `Cloud` — the
    /// cloud LLM is the smartest reasoner and routes through the SAME `make_provider` egress
    /// envelope as the note summary. `#[serde(default)]` ⇒ a config persisted before this field
    /// existed loads as `Cloud`. See [`BrainBackend`].
    #[serde(default)]
    pub brain_backend: BrainBackend,
    /// Phase E (Flow B) — the in-meeting VOICE ACTION DISPATCH master gate. When ON, a wake-word
    /// hit in a live caption ("Claudku, zrób research o X") DISPATCHES the parsed action against the
    /// gated vault (research/recall/reminder/note) and emits a live result. OPT-IN, default OFF
    /// (`#[serde(default)]` ⇒ a config persisted before this field existed loads as `false`):
    /// the always-on in-meeting assistant is a privacy + surprise-egress decision — a wake
    /// false-fire must never silently trigger a cloud call or a mid-meeting action. With it OFF the
    /// live loop behaves EXACTLY as before (wake detected + surfaced, NO dispatch).
    #[serde(default)]
    pub realtime_reactions: bool,
    /// brain2 connector framework (Phase F) — master toggle for the WEB SEARCH connector. Default
    /// OFF (`#[serde(default)]` ⇒ a config persisted before this field existed loads as `false`).
    /// When OFF the web connector is ABSENT from the brain's tool registry (no web tool offered), so
    /// shipping this changes NOTHING vs today. Settable from the settings DTO (the Settings UI owns
    /// the toggle). Even when ON, the connector is exposed only once `web_search_consented` is granted
    /// AND a Brave API key is stored — see `connectors::web::WebConnector::from_config_if_available`.
    #[serde(default)]
    pub web_search_enabled: bool,
    /// brain2 connector framework — one-time WEB SEARCH egress consent. The web connector reaches an
    /// EXTERNAL service (a NEW EGRESS CLASS): the outgoing (redacted) query leaves the device. Default
    /// OFF (fail-closed): no query egresses until the user explicitly consents once via the dedicated
    /// `consent_to_web_search` command. Like `cloud_egress_consented`, this is PRESERVE-ONLY on the
    /// settings DTO — `dto_to_config` ignores the incoming value and keeps the stored one, so a normal
    /// settings save can neither grant nor clear it. `#[serde(default)]` ⇒ pre-existing configs load
    /// as `false`.
    #[serde(default)]
    pub web_search_consented: bool,
    /// Opt-in: restore the OLDER-VERSION behavior of INHERITING the shell environment into the
    /// `claude` CLI subprocess, so env vars set in the user's shell — `ANTHROPIC_API_KEY`,
    /// `ANTHROPIC_BASE_URL`, proxy vars (`HTTPS_PROXY`) — reach the CLI again. The F2 audit hardening
    /// started CLEARING the child env, which broke users who authenticated the CLI via an env API key
    /// ("worked in older versions, exit 1 after update"). Default OFF (`#[serde(default)]` ⇒
    /// pre-existing configs load as `false`) = the hardened, env-cleared behavior. When ON the child
    /// inherits the environment EXCEPT `MURMUR_DEV_DEK` / `MURMUR_DEV_KEK` (the DB encryption keys),
    /// which are ALWAYS stripped — they decrypt the whole library and must NEVER reach a subprocess.
    /// Affects ONLY the `claude_code` provider (the `anthropic` provider uses the Keychain key).
    #[serde(default)]
    pub claude_code_inherit_env: bool,
    /// Base URL of the user's OpenAI-compatible AI gateway (LiteLLM / Kong / Portkey / vLLM / …).
    /// Default `""` (unset). Required when `provider_id == "gateway"`. Must be https:// (or http://
    /// on loopback). Validated + stored via `getConfig`/`saveConfig`; validated again at provider-
    /// construction time by `validate_gateway_url`. `#[serde(default)]` ⇒ pre-existing configs load
    /// as `""` (unset) — no behavioral change for existing installs.
    #[serde(default)]
    pub gateway_base_url: String,
    /// Model id to send to the gateway (e.g. `"gpt-4o"`, `"mistral/mistral-7b"`). Default `""`.
    /// An empty value sends whatever the gateway's default is. `#[serde(default)]` ⇒ pre-existing
    /// configs load as `""`.
    #[serde(default)]
    pub gateway_model: String,
    /// Proactive brain P1 — while recording, the deterministic ZERO-EGRESS matcher
    /// (`crate::proactive`) surfaces dismissible recall cards ("you discussed this on … →
    /// [[meeting]]", "open commitment: …") from the live-caption tail. Default ON (spec D2: the
    /// conservative threshold + the hard ≥120 s cooldown keep the default quiet); flipping it OFF
    /// mutes the scanner IN THE BACKEND — the matcher never runs, not just UI hiding. No egress
    /// either way: this path never touches a provider or a consent gate.
    /// `#[serde(default = "default_true")]` ⇒ a config persisted before this field existed loads
    /// as `true`.
    #[serde(default = "default_true")]
    pub proactive_hints_enabled: bool,
    /// Cross-meeting USER MEMORY master gate (Phase 3, `crate::user_memory`). Default ON (spec: memory
    /// is a headline capability). When OFF, memory is turned off ENTIRELY in the BACKEND: no user-fact
    /// extraction runs after a meeting (`persist_user_facts_for_meeting` early-returns), NO memory
    /// brief is injected into ANY surface (the @brain agentic loop, Ask, per-meeting chat), and
    /// `get_user_memory` reports the disabled marker — not just UI hiding. Existing facts are NOT
    /// deleted by flipping it (the user can forget/clear them); flipping it back ON resumes injection
    /// from the still-present facts. `#[serde(default = "default_true")]` ⇒ a config persisted before
    /// this field existed loads as `true` (memory stays on for existing installs).
    #[serde(default = "default_true")]
    pub user_memory_enabled: bool,
    /// Model-role override — the CONNECTION serving the **Notes** role (everything Murmur writes).
    /// `""` (the default, and every pre-role install) = inherit the legacy mapping EXACTLY — see
    /// [`crate::summarize::roles::resolve`]. Values: `claude_code`/`anthropic`/`ollama`/`gateway`
    /// (or `local`/`off` for the reasoner-only roles). ADDITIVE: the legacy keys are never
    /// rewritten; `#[serde(default)]` ⇒ pre-existing configs load as `""` (zero behavior change).
    #[serde(default)]
    pub role_notes_connection: String,
    /// Model-role override — the MODEL for the Notes role. `""` = the connection's own default.
    /// Consulted only when `role_notes_connection` is set (the connection key is the override
    /// switch). `#[serde(default)]` ⇒ `""`.
    #[serde(default)]
    pub role_notes_model: String,
    /// Model-role override — the reasoning EFFORT for the Notes role (`""`/`low`/`medium`/`high`;
    /// honored by the `anthropic` provider only, like `provider_effort`). Consulted only when
    /// `role_notes_connection` is set. `#[serde(default)]` ⇒ `""`.
    #[serde(default)]
    pub role_notes_effort: String,
    /// Model-role override — the CONNECTION serving the **Ask** role (vault Q&A + meeting chat).
    /// Same semantics as `role_notes_connection`.
    #[serde(default)]
    pub role_ask_connection: String,
    /// Model-role override — the MODEL for the Ask role. Same semantics as `role_notes_model`.
    #[serde(default)]
    pub role_ask_model: String,
    /// Model-role override — the EFFORT for the Ask role. Same semantics as `role_notes_effort`.
    #[serde(default)]
    pub role_ask_effort: String,
    /// Model-role override — the CONNECTION serving the **Live** role (the in-meeting assistant,
    /// @brain threads, voice). Same semantics as `role_notes_connection`.
    #[serde(default)]
    pub role_live_connection: String,
    /// Model-role override — the MODEL for the Live role. Same semantics as `role_notes_model`.
    #[serde(default)]
    pub role_live_model: String,
    /// Model-role override — the EFFORT for the Live role. Same semantics as `role_notes_effort`.
    #[serde(default)]
    pub role_live_effort: String,
}

/// serde default for flags that default ON (mirrors `commands::default_true` for the DTO side).
fn default_true() -> bool {
    true
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
            provider_model: String::new(),
            provider_effort: String::new(),
            ollama_base_url: "http://localhost:11434".to_string(),
            ollama_model: "llama3.1".to_string(),
            claude_binary: "claude".to_string(),
            input_device: None,
            capture_system_audio: false,
            vad_enabled: true,
            keep_hires_masters: false,
            diarize_others: false,
            aec_enabled: false,
            post_aec_enabled: false,
            model_size: "small".to_string(),
            voice_trigger: false,
            onboarded: false,
            note_style: "standard".to_string(),
            notes_mode: "enhance".to_string(),
            auto_organize: false,
            note_language: "auto".to_string(),
            mcp_require_token: true,
            lock_require_biometric: true,
            relock_on_screenshare: true,
            cloud_egress_consented: false,
            semantic_search_enabled: false,
            embed_model_id: None,
            brain_model_path: None,
            brain_model_id: None,
            brain_backend: BrainBackend::default(),
            realtime_reactions: false,
            web_search_enabled: false,
            web_search_consented: false,
            claude_code_inherit_env: false,
            gateway_base_url: String::new(),
            gateway_model: String::new(),
            proactive_hints_enabled: true,
            user_memory_enabled: true,
            role_notes_connection: String::new(),
            role_notes_model: String::new(),
            role_notes_effort: String::new(),
            role_ask_connection: String::new(),
            role_ask_model: String::new(),
            role_ask_effort: String::new(),
            role_live_connection: String::new(),
            role_live_model: String::new(),
            role_live_effort: String::new(),
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
const K_PROVIDER_MODEL: &str = "provider_model";
const K_PROVIDER_EFFORT: &str = "provider_effort";
const K_OLLAMA_BASE_URL: &str = "ollama_base_url";
const K_OLLAMA_MODEL: &str = "ollama_model";
const K_CLAUDE_BINARY: &str = "claude_binary";
const K_INPUT_DEVICE: &str = "input_device";
const K_CAPTURE_SYSTEM_AUDIO: &str = "capture_system_audio";
const K_VAD_ENABLED: &str = "vad_enabled";
const K_KEEP_HIRES_MASTERS: &str = "keep_hires_masters";
const K_DIARIZE_OTHERS: &str = "diarize_others";
const K_AEC_ENABLED: &str = "aec_enabled";
const K_POST_AEC_ENABLED: &str = "post_aec_enabled";
const K_MODEL_SIZE: &str = "model_size";
const K_VOICE_TRIGGER: &str = "voice_trigger";
const K_ONBOARDED: &str = "onboarded";
const K_NOTE_STYLE: &str = "note_style";
const K_NOTES_MODE: &str = "notes_mode";
const K_AUTO_ORGANIZE: &str = "auto_organize";
const K_NOTE_LANGUAGE: &str = "note_language";
const K_MCP_REQUIRE_TOKEN: &str = "mcp_require_token";
const K_LOCK_REQUIRE_BIOMETRIC: &str = "lock_require_biometric";
const K_RELOCK_ON_SCREENSHARE: &str = "relock_on_screenshare";
const K_CLOUD_EGRESS_CONSENTED: &str = "cloud_egress_consented";
const K_SEMANTIC_SEARCH_ENABLED: &str = "semantic_search_enabled";
const K_EMBED_MODEL_ID: &str = "embed_model_id";
const K_BRAIN_MODEL_PATH: &str = "brain_model_path";
const K_BRAIN_MODEL_ID: &str = "brain_model_id";
const K_BRAIN_BACKEND: &str = "brain_backend";
const K_REALTIME_REACTIONS: &str = "realtime_reactions";
const K_WEB_SEARCH_ENABLED: &str = "web_search_enabled";
const K_WEB_SEARCH_CONSENTED: &str = "web_search_consented";
const K_CLAUDE_CODE_INHERIT_ENV: &str = "claude_code_inherit_env";
const K_GATEWAY_BASE_URL: &str = "gateway_base_url";
const K_GATEWAY_MODEL: &str = "gateway_model";
const K_PROACTIVE_HINTS_ENABLED: &str = "proactive_hints_enabled";
const K_USER_MEMORY_ENABLED: &str = "user_memory_enabled";
const K_ROLE_NOTES_CONNECTION: &str = "role_notes_connection";
const K_ROLE_NOTES_MODEL: &str = "role_notes_model";
const K_ROLE_NOTES_EFFORT: &str = "role_notes_effort";
const K_ROLE_ASK_CONNECTION: &str = "role_ask_connection";
const K_ROLE_ASK_MODEL: &str = "role_ask_model";
const K_ROLE_ASK_EFFORT: &str = "role_ask_effort";
const K_ROLE_LIVE_CONNECTION: &str = "role_live_connection";
const K_ROLE_LIVE_MODEL: &str = "role_live_model";
const K_ROLE_LIVE_EFFORT: &str = "role_live_effort";

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
        // `""` is a VALID value (= provider default) and also the default, so the stored value
        // is taken verbatim — no non-empty guard (unlike anthropic_model, whose default is a real id).
        if let Some(v) = db.get_setting(K_PROVIDER_MODEL)? {
            cfg.provider_model = v;
        }
        if let Some(v) = db.get_setting(K_PROVIDER_EFFORT)? {
            cfg.provider_effort = v;
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
        if let Some(v) = db.get_setting(K_POST_AEC_ENABLED)? {
            cfg.post_aec_enabled = v == "true";
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
        if let Some(v) = db.get_setting(K_NOTES_MODE)? {
            if !v.is_empty() {
                cfg.notes_mode = v;
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
        cfg.embed_model_id = opt(db.get_setting(K_EMBED_MODEL_ID)?);
        // Publish the selection to the process-global seam the zero-arg embedder resolvers read
        // (`embed::active_embedder`/`embed_model_present`/`embed_model_dir`/`download_embed_model`).
        // `None`/empty ⇒ the default model ⇒ byte-identical to the historical behavior.
        crate::embed::set_selected_embed_model_id(cfg.embed_model_id.clone());
        cfg.brain_model_path = opt(db.get_setting(K_BRAIN_MODEL_PATH)?);
        cfg.brain_model_id = opt(db.get_setting(K_BRAIN_MODEL_ID)?);
        if let Some(v) = db.get_setting(K_BRAIN_BACKEND)? {
            if !v.is_empty() {
                cfg.brain_backend = BrainBackend::from_str_or_default(&v);
            }
        }
        if let Some(v) = db.get_setting(K_REALTIME_REACTIONS)? {
            cfg.realtime_reactions = v == "true";
        }
        if let Some(v) = db.get_setting(K_WEB_SEARCH_ENABLED)? {
            cfg.web_search_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_WEB_SEARCH_CONSENTED)? {
            cfg.web_search_consented = v == "true";
        }
        if let Some(v) = db.get_setting(K_CLAUDE_CODE_INHERIT_ENV)? {
            cfg.claude_code_inherit_env = v == "true";
        }
        // `""` is valid (= unset) for the gateway fields, so we take the stored value verbatim
        // (no non-empty guard — mirrors `provider_model` rather than `anthropic_model`).
        if let Some(v) = db.get_setting(K_GATEWAY_BASE_URL)? {
            cfg.gateway_base_url = v;
        }
        if let Some(v) = db.get_setting(K_GATEWAY_MODEL)? {
            cfg.gateway_model = v;
        }
        if let Some(v) = db.get_setting(K_PROACTIVE_HINTS_ENABLED)? {
            cfg.proactive_hints_enabled = v == "true";
        }
        if let Some(v) = db.get_setting(K_USER_MEMORY_ENABLED)? {
            cfg.user_memory_enabled = v == "true";
        }
        // Model-role keys: `""` is a VALID value (= inherit legacy) and also the default, so the
        // stored value is taken verbatim (mirrors `provider_model`, not `anthropic_model`).
        if let Some(v) = db.get_setting(K_ROLE_NOTES_CONNECTION)? {
            cfg.role_notes_connection = v;
        }
        if let Some(v) = db.get_setting(K_ROLE_NOTES_MODEL)? {
            cfg.role_notes_model = v;
        }
        if let Some(v) = db.get_setting(K_ROLE_NOTES_EFFORT)? {
            cfg.role_notes_effort = v;
        }
        if let Some(v) = db.get_setting(K_ROLE_ASK_CONNECTION)? {
            cfg.role_ask_connection = v;
        }
        if let Some(v) = db.get_setting(K_ROLE_ASK_MODEL)? {
            cfg.role_ask_model = v;
        }
        if let Some(v) = db.get_setting(K_ROLE_ASK_EFFORT)? {
            cfg.role_ask_effort = v;
        }
        if let Some(v) = db.get_setting(K_ROLE_LIVE_CONNECTION)? {
            cfg.role_live_connection = v;
        }
        if let Some(v) = db.get_setting(K_ROLE_LIVE_MODEL)? {
            cfg.role_live_model = v;
        }
        if let Some(v) = db.get_setting(K_ROLE_LIVE_EFFORT)? {
            cfg.role_live_effort = v;
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
        db.set_setting(K_PROVIDER_MODEL, &self.provider_model)?;
        db.set_setting(K_PROVIDER_EFFORT, &self.provider_effort)?;
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
        db.set_setting(
            K_POST_AEC_ENABLED,
            if self.post_aec_enabled { "true" } else { "false" },
        )?;
        db.set_setting(K_MODEL_SIZE, &self.model_size)?;
        db.set_setting(
            K_VOICE_TRIGGER,
            if self.voice_trigger { "true" } else { "false" },
        )?;
        db.set_setting(K_ONBOARDED, if self.onboarded { "true" } else { "false" })?;
        db.set_setting(K_NOTE_STYLE, &self.note_style)?;
        db.set_setting(K_NOTES_MODE, &self.notes_mode)?;
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
            K_EMBED_MODEL_ID,
            self.embed_model_id.as_deref().unwrap_or(""),
        )?;
        // Keep the process-global embedder selection in sync with the persisted value on every save.
        crate::embed::set_selected_embed_model_id(self.embed_model_id.clone());
        db.set_setting(
            K_BRAIN_MODEL_PATH,
            self.brain_model_path.as_deref().unwrap_or(""),
        )?;
        db.set_setting(
            K_BRAIN_MODEL_ID,
            self.brain_model_id.as_deref().unwrap_or(""),
        )?;
        db.set_setting(K_BRAIN_BACKEND, self.brain_backend.as_str())?;
        db.set_setting(
            K_REALTIME_REACTIONS,
            if self.realtime_reactions { "true" } else { "false" },
        )?;
        db.set_setting(
            K_WEB_SEARCH_ENABLED,
            if self.web_search_enabled { "true" } else { "false" },
        )?;
        db.set_setting(
            K_WEB_SEARCH_CONSENTED,
            if self.web_search_consented { "true" } else { "false" },
        )?;
        db.set_setting(
            K_CLAUDE_CODE_INHERIT_ENV,
            if self.claude_code_inherit_env { "true" } else { "false" },
        )?;
        db.set_setting(K_GATEWAY_BASE_URL, &self.gateway_base_url)?;
        db.set_setting(K_GATEWAY_MODEL, &self.gateway_model)?;
        db.set_setting(
            K_PROACTIVE_HINTS_ENABLED,
            if self.proactive_hints_enabled { "true" } else { "false" },
        )?;
        db.set_setting(
            K_USER_MEMORY_ENABLED,
            if self.user_memory_enabled { "true" } else { "false" },
        )?;
        db.set_setting(K_ROLE_NOTES_CONNECTION, &self.role_notes_connection)?;
        db.set_setting(K_ROLE_NOTES_MODEL, &self.role_notes_model)?;
        db.set_setting(K_ROLE_NOTES_EFFORT, &self.role_notes_effort)?;
        db.set_setting(K_ROLE_ASK_CONNECTION, &self.role_ask_connection)?;
        db.set_setting(K_ROLE_ASK_MODEL, &self.role_ask_model)?;
        db.set_setting(K_ROLE_ASK_EFFORT, &self.role_ask_effort)?;
        db.set_setting(K_ROLE_LIVE_CONNECTION, &self.role_live_connection)?;
        db.set_setting(K_ROLE_LIVE_MODEL, &self.role_live_model)?;
        db.set_setting(K_ROLE_LIVE_EFFORT, &self.role_live_effort)?;
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
    ///
    /// FAIL-CLOSED ORDERING — persist FIRST, flip the in-memory flag ONLY on a durable success. If
    /// the write fails we return the error with the session still UNCONSENTED, so we never egress on
    /// a consent that isn't durably recorded. (The inverse — `revoke_cloud_egress` — deliberately
    /// flips first; see its doc.)
    pub fn grant_cloud_egress_consent(&mut self, db: &Db) -> Result<()> {
        db.set_setting(K_CLOUD_EGRESS_CONSENTED, "true")?;
        self.cloud_egress_consented = true;
        Ok(())
    }

    /// E10 — REVOKE the cloud-egress consent. Mirror of [`grant_cloud_egress_consent`]: flips the
    /// in-memory flag AND persists the single key, and is the ONLY supported way to clear consent
    /// (the DTO stays preserve-only in both directions, so a settings save can neither grant nor
    /// revoke). After revoke, every cloud-classified provider build is refused fail-closed
    /// (`AppError::Unavailable`) again — the gate reads the live config per call, so the very next
    /// summarize/ask/reasoner call refuses without a restart.
    ///
    /// FAIL-CLOSED ORDERING (opposite of the grants) — flip the in-memory flag FIRST, THEN persist.
    /// Revoke's safe-failure direction is "stop egressing immediately", so the session must go
    /// unconsented even if the durable write then fails; persist-first would keep the session
    /// egressing on a write error. Both grant and revoke thus fail toward NOT egressing.
    pub fn revoke_cloud_egress(&mut self, db: &Db) -> Result<()> {
        self.cloud_egress_consented = false;
        db.set_setting(K_CLOUD_EGRESS_CONSENTED, "false")
    }

    /// brain2 connector framework — record the user's one-time consent to send the (redacted) web
    /// search query to an EXTERNAL search service. Mirrors [`grant_cloud_egress_consent`]: flips the
    /// in-memory flag AND persists it. This is the ONLY supported mutator (deliberately separate from
    /// `save_config`), so web-search egress consent can never be granted as an incidental side effect
    /// of a settings write. Until granted, the web connector is absent from the brain's tool registry.
    ///
    /// FAIL-CLOSED ORDERING — persist FIRST, flip the in-memory flag ONLY on a durable success, so a
    /// failed write leaves the session unconsented (no web egress on a consent that wasn't recorded).
    pub fn grant_web_search_consent(&mut self, db: &Db) -> Result<()> {
        db.set_setting(K_WEB_SEARCH_CONSENTED, "true")?;
        self.web_search_consented = true;
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
        let p = crate::storage::db::unique_temp_path("meetnotes-config-test", "sqlite");
        // Tests use an explicit key (NOT the Keychain) — Db::open would hit macOS Keychain and
        // prompt/block depending on the test-binary signature.
        Db::open_with_key(
            &p,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap()
    }

    /// ENHANCE-MY-NOTES: the mode defaults to "enhance" for fresh installs AND for existing
    /// users (AppConfig::load falls back to Default for a never-written key).
    #[test]
    fn notes_mode_defaults_to_enhance() {
        assert_eq!(AppConfig::default().notes_mode, "enhance");
    }

    /// A1 — offline AEC3 rides an UNPROVEN v0.1 crate on every system-audio recording, so it is
    /// DEFAULT-OFF until real-Mac-verified. Assert both the in-memory struct default AND the
    /// settings-table load path (empty DB / key absent) resolve to `false`. `#[serde(default)]`
    /// (bool default = false) covers a config persisted before the field flip.
    #[test]
    fn post_aec_defaults_off() {
        // In-memory struct default.
        assert!(!AppConfig::default().post_aec_enabled);
        // Settings-table path: empty DB (no key written) loads false.
        let db = temp_db();
        assert!(!AppConfig::load(&db).unwrap().post_aec_enabled);
        // But an explicit opt-in still persists + reloads ON (not a one-way latch).
        let cfg = AppConfig {
            post_aec_enabled: true,
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert!(AppConfig::load(&db).unwrap().post_aec_enabled);
    }

    /// A2 — the app default whisper model is `small` (RAM-safe; `large-v3` is ~3 GB and swaps on
    /// 8 GB Macs). Must match `transcribe::model_filename("", _)`'s empty-size fallback so a config
    /// that bypasses onboarding no longer lands on `large-v3`. All sizes stay selectable.
    #[test]
    fn model_size_defaults_to_small() {
        assert_eq!(AppConfig::default().model_size, "small");
        // Empty settings table loads the same default.
        let db = temp_db();
        assert_eq!(AppConfig::load(&db).unwrap().model_size, "small");
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
            "diarizeOthers":false,"aecEnabled":false,"postAecEnabled":true,"modelSize":"large-v3","voiceTrigger":false,
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
    fn brain_model_id_round_trips_and_defaults_none() {
        let db = temp_db();
        // Absent key ⇒ None (prod-safe default for existing installs).
        assert!(AppConfig::load(&db).unwrap().brain_model_id.is_none());
        let cfg = AppConfig {
            brain_model_id: Some("bielik-11b-v3".to_string()),
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert_eq!(
            AppConfig::load(&db).unwrap().brain_model_id.as_deref(),
            Some("bielik-11b-v3")
        );
    }

    #[test]
    fn brain_backend_defaults_cloud_and_round_trips() {
        let db = temp_db();
        // Absent key ⇒ Cloud (the chosen default for fresh + pre-existing installs).
        assert_eq!(AppConfig::load(&db).unwrap().brain_backend, BrainBackend::Cloud);

        for backend in [BrainBackend::Cloud, BrainBackend::Local, BrainBackend::Off] {
            let cfg = AppConfig {
                brain_backend: backend,
                ..Default::default()
            };
            cfg.save(&db).unwrap();
            assert_eq!(AppConfig::load(&db).unwrap().brain_backend, backend);
        }
    }

    #[test]
    fn brain_backend_serde_round_trips_and_defaults() {
        // Token form persisted to the settings table.
        assert_eq!(BrainBackend::Cloud.as_str(), "cloud");
        assert_eq!(BrainBackend::Local.as_str(), "local");
        assert_eq!(BrainBackend::Off.as_str(), "off");
        assert_eq!(BrainBackend::from_str_or_default("local"), BrainBackend::Local);
        assert_eq!(BrainBackend::from_str_or_default("off"), BrainBackend::Off);
        // Unknown / empty falls back to the default brain.
        assert_eq!(BrainBackend::from_str_or_default("bogus"), BrainBackend::Cloud);
        assert_eq!(BrainBackend::from_str_or_default(""), BrainBackend::Cloud);

        // JSON DTO: a payload omitting brainBackend loads as Cloud (#[serde(default)]).
        let json = r#"{
            "providerId":"claude_code","vaultPath":null,"vaultSubfolder":null,
            "whisperModelPath":null,"language":null,"anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "inputDevice":null,"captureSystemAudio":false,"vadEnabled":true,"keepHiresMasters":false,
            "diarizeOthers":false,"aecEnabled":false,"postAecEnabled":true,"modelSize":"large-v3","voiceTrigger":false,
            "onboarded":false,"noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto",
            "mcpRequireToken":true,"lockRequireBiometric":true,"relockOnScreenshare":true,
            "cloudEgressConsented":false
        }"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.brain_backend, BrainBackend::Cloud);
        // And an explicit value round-trips through serde with the lowercase rename.
        let dto: AppConfig = serde_json::from_str(
            &serde_json::to_string(&AppConfig {
                brain_backend: BrainBackend::Off,
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(dto.brain_backend, BrainBackend::Off);
    }

    #[test]
    fn realtime_reactions_defaults_off_and_round_trips() {
        let db = temp_db();
        // Absent key ⇒ OFF (opt-in; the always-on in-meeting assistant must never default on).
        assert!(!AppConfig::load(&db).unwrap().realtime_reactions);
        let cfg = AppConfig {
            realtime_reactions: true,
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert!(AppConfig::load(&db).unwrap().realtime_reactions);
    }

    #[test]
    fn web_search_flags_default_off_and_round_trip() {
        let db = temp_db();
        // Fail-closed defaults: both OFF until explicitly set/granted.
        let cfg = AppConfig::load(&db).unwrap();
        assert!(!cfg.web_search_enabled, "web search master toggle defaults OFF");
        assert!(!cfg.web_search_consented, "web search egress consent fail-closed OFF");

        // `web_search_enabled` is a settable flag; consent is granted via its dedicated method.
        let cfg = AppConfig {
            web_search_enabled: true,
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert!(loaded.web_search_enabled);
        // Consent was NOT granted by a plain save.
        assert!(!loaded.web_search_consented);
    }

    #[test]
    fn web_search_consent_grant_persists() {
        let db = temp_db();
        let mut cfg = AppConfig::load(&db).unwrap();
        assert!(!cfg.web_search_consented);
        cfg.grant_web_search_consent(&db).unwrap();
        assert!(cfg.web_search_consented);
        // Survives a reload from the settings table.
        assert!(AppConfig::load(&db).unwrap().web_search_consented);
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
    fn embed_model_id_defaults_none_and_round_trips() {
        // Serialize with other tests that touch the process-global embedder selection.
        let _g = crate::embed::EMBED_SELECTION_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let db = temp_db();
        // Absent key ⇒ None (⇒ the default embedder ⇒ byte-identical to historical behavior).
        assert!(AppConfig::load(&db).unwrap().embed_model_id.is_none());

        let cfg = AppConfig {
            embed_model_id: Some("mmlw-e5-small".to_string()),
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        assert_eq!(
            AppConfig::load(&db).unwrap().embed_model_id.as_deref(),
            Some("mmlw-e5-small")
        );

        // Clearing back to None (default model) also round-trips — not a one-way latch.
        let cleared = AppConfig {
            embed_model_id: None,
            ..Default::default()
        };
        cleared.save(&db).unwrap();
        assert!(AppConfig::load(&db).unwrap().embed_model_id.is_none());
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

    /// E10 revoke — `revoke_cloud_egress` mirrors the grant: it flips the in-memory flag AND
    /// persists the key, so the revocation is fail-closed across a reload/restart too.
    #[test]
    fn cloud_egress_consent_revoke_persists() {
        let db = temp_db();
        let mut cfg = AppConfig::load(&db).unwrap();
        cfg.grant_cloud_egress_consent(&db).unwrap();
        assert!(AppConfig::load(&db).unwrap().cloud_egress_consented);
        cfg.revoke_cloud_egress(&db).unwrap();
        assert!(!cfg.cloud_egress_consented, "in-memory flag must flip immediately");
        // Survives a reload from the settings table.
        assert!(!AppConfig::load(&db).unwrap().cloud_egress_consented);
    }

    /// FAIL-CLOSED ORDERING (grants persist-then-flip). On the normal path the durable record and
    /// the in-memory flag must AGREE after a grant — the durable write landed and only then did the
    /// session flip. (The failure branch — a `set_setting` error leaving the flag false — is a
    /// STRUCTURAL guarantee: the grant returns `?` before touching `self`, so a write error can never
    /// flip the flag. We can't force a `set_setting` error without a Db mock the crate doesn't have,
    /// so we pin the observable agreement of the two sides here.)
    #[test]
    fn cloud_egress_grant_durable_record_and_flag_agree() {
        let db = temp_db();
        let mut cfg = AppConfig::load(&db).unwrap();
        // Pre-grant: neither side consents.
        assert!(!cfg.cloud_egress_consented);
        assert_ne!(db.get_setting(K_CLOUD_EGRESS_CONSENTED).unwrap().as_deref(), Some("true"));
        // Grant: the durable record is written FIRST, then the flag flips — both true afterwards.
        cfg.grant_cloud_egress_consent(&db).unwrap();
        assert!(cfg.cloud_egress_consented, "session flag flipped");
        assert_eq!(
            db.get_setting(K_CLOUD_EGRESS_CONSENTED).unwrap().as_deref(),
            Some("true"),
            "durable consent record persisted (persist-first)"
        );
    }

    /// Same fail-closed agreement for the web-search grant.
    #[test]
    fn web_search_grant_durable_record_and_flag_agree() {
        let db = temp_db();
        let mut cfg = AppConfig::load(&db).unwrap();
        assert!(!cfg.web_search_consented);
        cfg.grant_web_search_consent(&db).unwrap();
        assert!(cfg.web_search_consented);
        assert_eq!(
            db.get_setting(K_WEB_SEARCH_CONSENTED).unwrap().as_deref(),
            Some("true"),
            "durable web-search consent record persisted (persist-first)"
        );
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
    fn provider_model_and_effort_round_trip_and_default_empty() {
        let db = temp_db();
        // Absent keys ⇒ `""` (provider default) for fresh + pre-existing installs.
        let fresh = AppConfig::load(&db).unwrap();
        assert_eq!(fresh.provider_model, "");
        assert_eq!(fresh.provider_effort, "");

        let cfg = AppConfig {
            provider_model: "claude-sonnet-4-6".to_string(),
            provider_effort: "high".to_string(),
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert_eq!(loaded.provider_model, "claude-sonnet-4-6");
        assert_eq!(loaded.provider_effort, "high");

        // Clearing back to `""` (provider default) also round-trips — not a one-way latch.
        let cleared = AppConfig {
            provider_model: String::new(),
            provider_effort: String::new(),
            ..Default::default()
        };
        cleared.save(&db).unwrap();
        let reloaded = AppConfig::load(&db).unwrap();
        assert_eq!(reloaded.provider_model, "");
        assert_eq!(reloaded.provider_effort, "");
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

    /// Task 1.1 — gateway fields default to `""` and are never set by pre-existing configs.
    #[test]
    fn gateway_fields_default_empty() {
        // In-memory struct default.
        let cfg = AppConfig::default();
        assert_eq!(cfg.gateway_base_url, "", "gateway_base_url must default to empty");
        assert_eq!(cfg.gateway_model, "", "gateway_model must default to empty");

        // Settings-table path: an empty DB (no key written) loads the defaults.
        let db = temp_db();
        let loaded = AppConfig::load(&db).unwrap();
        assert_eq!(loaded.gateway_base_url, "");
        assert_eq!(loaded.gateway_model, "");
    }

    /// Task 1.1 — gateway fields round-trip through save/load unchanged.
    #[test]
    fn gateway_fields_round_trip() {
        let db = temp_db();
        let cfg = AppConfig {
            gateway_base_url: "https://my-gateway.example.com/v1".to_string(),
            gateway_model: "gpt-4o".to_string(),
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert_eq!(
            loaded.gateway_base_url,
            "https://my-gateway.example.com/v1"
        );
        assert_eq!(loaded.gateway_model, "gpt-4o");

        // Clearing back to `""` (unset) also round-trips — not a one-way latch.
        let cleared = AppConfig { ..Default::default() };
        cleared.save(&db).unwrap();
        let reloaded = AppConfig::load(&db).unwrap();
        assert_eq!(reloaded.gateway_base_url, "");
        assert_eq!(reloaded.gateway_model, "");
    }

    /// Proactive brain P1 — the mute flag defaults ON (spec D2: default-on with conservative
    /// thresholds), for fresh installs AND for configs persisted before the field existed (both
    /// the settings-table path and the serde path), and an explicit OFF round-trips (the backend
    /// mute is a real, persistable choice — not a one-way latch).
    #[test]
    fn proactive_hints_defaults_on_and_round_trips() {
        // In-memory default + empty settings table (key never written) ⇒ ON.
        assert!(AppConfig::default().proactive_hints_enabled);
        let db = temp_db();
        assert!(AppConfig::load(&db).unwrap().proactive_hints_enabled);

        // serde path: a JSON payload omitting the field entirely loads ON (default_true).
        let json = r#"{
            "providerId":"claude_code","vaultPath":null,"vaultSubfolder":null,
            "whisperModelPath":null,"language":null,"anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "inputDevice":null,"captureSystemAudio":false,"vadEnabled":true,"keepHiresMasters":false,
            "diarizeOthers":false,"aecEnabled":false,"postAecEnabled":true,"modelSize":"large-v3","voiceTrigger":false,
            "onboarded":false,"noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto",
            "mcpRequireToken":true,"lockRequireBiometric":true,"relockOnScreenshare":true,
            "cloudEgressConsented":false
        }"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.proactive_hints_enabled, "an omitted key must default ON");

        // Explicit OFF persists + reloads OFF (the backend-side mute).
        let cfg = AppConfig { proactive_hints_enabled: false, ..Default::default() };
        cfg.save(&db).unwrap();
        assert!(!AppConfig::load(&db).unwrap().proactive_hints_enabled);
    }

    /// Cross-meeting USER MEMORY — the master gate defaults ON for fresh installs AND for configs
    /// persisted before the field existed (both the settings-table path and the serde path), and an
    /// explicit OFF round-trips (turning memory off is a real, persistable choice — not a one-way
    /// latch). ON-on-absent is the "memory stays on for existing installs" guarantee.
    #[test]
    fn user_memory_enabled_defaults_on_and_round_trips() {
        // In-memory default + empty settings table (key never written) ⇒ ON.
        assert!(AppConfig::default().user_memory_enabled);
        let db = temp_db();
        assert!(AppConfig::load(&db).unwrap().user_memory_enabled);

        // serde path: a JSON payload omitting the field entirely loads ON (default_true).
        let json = r#"{
            "providerId":"claude_code","vaultPath":null,"vaultSubfolder":null,
            "whisperModelPath":null,"language":null,"anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "inputDevice":null,"captureSystemAudio":false,"vadEnabled":true,"keepHiresMasters":false,
            "diarizeOthers":false,"aecEnabled":false,"postAecEnabled":true,"modelSize":"large-v3","voiceTrigger":false,
            "onboarded":false,"noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto",
            "mcpRequireToken":true,"lockRequireBiometric":true,"relockOnScreenshare":true,
            "cloudEgressConsented":false
        }"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.user_memory_enabled, "an omitted key must default ON");

        // Explicit OFF persists + reloads OFF (the backend-side memory kill switch).
        let cfg = AppConfig { user_memory_enabled: false, ..Default::default() };
        cfg.save(&db).unwrap();
        assert!(!AppConfig::load(&db).unwrap().user_memory_enabled);
    }

    /// Model roles — the 9 role keys default `""` (inherit-legacy) for fresh installs AND for
    /// configs persisted before the keys existed (both the settings-table path and the serde
    /// path), and set values round-trip — including clearing back to `""` (not a one-way latch).
    /// `""`-on-absent is the ZERO-BEHAVIOR-CHANGE guarantee for existing installs.
    #[test]
    fn role_keys_default_empty_and_round_trip() {
        // In-memory struct default + empty settings table (keys never written) ⇒ "" everywhere.
        let d = AppConfig::default();
        let db = temp_db();
        let fresh = AppConfig::load(&db).unwrap();
        for cfg in [&d, &fresh] {
            assert_eq!(cfg.role_notes_connection, "");
            assert_eq!(cfg.role_notes_model, "");
            assert_eq!(cfg.role_notes_effort, "");
            assert_eq!(cfg.role_ask_connection, "");
            assert_eq!(cfg.role_ask_model, "");
            assert_eq!(cfg.role_ask_effort, "");
            assert_eq!(cfg.role_live_connection, "");
            assert_eq!(cfg.role_live_model, "");
            assert_eq!(cfg.role_live_effort, "");
        }

        // serde path: a pre-role JSON payload (fields omitted) loads "" (#[serde(default)]).
        let json = r#"{
            "providerId":"claude_code","vaultPath":null,"vaultSubfolder":null,
            "whisperModelPath":null,"language":null,"anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "inputDevice":null,"captureSystemAudio":false,"vadEnabled":true,"keepHiresMasters":false,
            "diarizeOthers":false,"aecEnabled":false,"postAecEnabled":true,"modelSize":"large-v3","voiceTrigger":false,
            "onboarded":false,"noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto",
            "mcpRequireToken":true,"lockRequireBiometric":true,"relockOnScreenshare":true,
            "cloudEgressConsented":false
        }"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.role_notes_connection, "");
        assert_eq!(cfg.role_ask_connection, "");
        assert_eq!(cfg.role_live_connection, "");

        // Set values persist + reload verbatim.
        let cfg = AppConfig {
            role_notes_connection: "anthropic".to_string(),
            role_notes_model: "claude-opus-4-8".to_string(),
            role_notes_effort: "high".to_string(),
            role_ask_connection: "ollama".to_string(),
            role_ask_model: "mistral-small".to_string(),
            role_live_connection: "off".to_string(),
            ..Default::default()
        };
        cfg.save(&db).unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert_eq!(loaded.role_notes_connection, "anthropic");
        assert_eq!(loaded.role_notes_model, "claude-opus-4-8");
        assert_eq!(loaded.role_notes_effort, "high");
        assert_eq!(loaded.role_ask_connection, "ollama");
        assert_eq!(loaded.role_ask_model, "mistral-small");
        assert_eq!(loaded.role_ask_effort, "");
        assert_eq!(loaded.role_live_connection, "off");

        // Clearing back to "" (inherit legacy) also round-trips.
        AppConfig::default().save(&db).unwrap();
        let cleared = AppConfig::load(&db).unwrap();
        assert_eq!(cleared.role_notes_connection, "");
        assert_eq!(cleared.role_ask_connection, "");
        assert_eq!(cleared.role_live_connection, "");
    }

    /// Task 1.1 — serde: a payload omitting the gateway fields loads with empty defaults.
    #[test]
    fn gateway_fields_serde_default() {
        let json = r#"{
            "providerId":"claude_code","vaultPath":null,"vaultSubfolder":null,
            "whisperModelPath":null,"language":null,"anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "inputDevice":null,"captureSystemAudio":false,"vadEnabled":true,"keepHiresMasters":false,
            "diarizeOthers":false,"aecEnabled":false,"postAecEnabled":true,"modelSize":"large-v3","voiceTrigger":false,
            "onboarded":false,"noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto",
            "mcpRequireToken":true,"lockRequireBiometric":true,"relockOnScreenshare":true,
            "cloudEgressConsented":false
        }"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.gateway_base_url, "", "serde default must be empty string");
        assert_eq!(cfg.gateway_model, "", "serde default must be empty string");
    }
}
