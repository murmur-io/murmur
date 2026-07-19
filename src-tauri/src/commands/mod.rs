use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use zeroize::{Zeroize, Zeroizing};

use crate::audio::Recorder;
use crate::error::AppError;
use crate::events::{StatusPayload, EVENT_STATUS};
use crate::settings::{AppConfig, BrainBackend};
use crate::state::AppState;
use crate::storage::models::{
    ActionItem, Analytics, AskVaultResult, BrainOverview, CalendarContext,
    CalendarEvent, CalendarEventFull, ChatTurn, Commitment, DigestResult, DocumentInfo,
    EntityDetail, EntityDossierResult, Folder, FolderNode, FullGraphData, FullGraphOpts, GraphData,
    Meeting,
    MeetingActionSummary, MeetingStatus, MeetingTimeline, NoteAssistRequest, NoteAssistResult,
    NoteCitation, NoteDoc, NoteFolder, NoteRecord, NoteSummary, OrganizeMove, OrganizePlan,
    PeopleList, PinResult, PropertyKind, PropertySchemaField, SearchHit,
    TopicThread, TypedNoteRow,
};
use crate::summarize::all_providers;
use crate::transcribe::types::Segment;
use crate::{pipeline, secrets};
use tauri::Emitter;

// ── Command submodules (God-file split) ─────────────────────────────────────────────────────────
// `commands` is being decomposed into per-domain files under `commands/`. Each submodule is
// glob-re-exported here so EVERY existing path — `generate_handler![commands::save_recipe]` in
// `lib.rs`, and any `crate::commands::…` caller — resolves UNCHANGED. The file is
// `commands/pipeline.rs` but bound under the name `pipeline_commands` to avoid colliding with the
// crate-level `pipeline` module imported above (`use crate::{pipeline, secrets};`).
#[path = "pipeline.rs"]
mod pipeline_commands;
pub use pipeline_commands::*;

// macOS Reminders (osascript) — no name collision with a crate module.
mod reminders;
pub use reminders::*;

// Model / capability / performance probes + NER download — no name collision with a crate module.
mod model_perf;
pub use model_perf::*;

// Keychain secret setters/probes. Bound as `secrets_commands` to avoid colliding with the
// crate-level `secrets` module (`use crate::{pipeline, secrets};` above).
#[path = "secrets.rs"]
mod secrets_commands;
pub use secrets_commands::*;

// MCP server-config commands. Bound as `mcp_commands` (via `#[path]`) to keep it clearly distinct
// from the crate-level `mcp` module.
#[path = "mcp.rs"]
mod mcp_commands;
pub use mcp_commands::*;

// Egress-consent commands. Bound as `settings_commands` (via `#[path]`) to keep it clearly distinct
// from the crate-level `settings` module.
#[path = "settings.rs"]
mod settings_commands;
pub use settings_commands::*;

// Graph / entity / people / dossier READ commands (a GATED domain — every read snapshots the live
// `unlocked` set and pushes it through the DB visibility predicate; the gate LOGIC is byte-identical,
// only relocated). Bound as `graph_commands` (via `#[path]`) to avoid colliding with the crate-level
// `crate::links`/graph types — and, more importantly, so it never shadows a `graph` name.
#[path = "graph.rs"]
mod graph_commands;
pub use graph_commands::*;

// User-chosen-path & Canvas EXPORT commands (a GATED domain — every command fails CLOSED on
// `meeting_is_unlocked` for a sealed-and-not-session-unlocked meeting; the gate LOGIC is
// byte-identical, only relocated). Bound as `export_commands` (via `#[path]`) to keep it clearly
// distinct from the crate-level `export` module. The note→VAULT export cluster + shared vault helpers
// deliberately STAY in `commands/mod.rs`.
#[path = "export.rs"]
mod export_commands;
pub use export_commands::*;

// Analytics / egress-ledger / Vault-Audit commands (a GATED domain — `get_analytics` is
// visible-content-only, the audit inbox re-gates every row against the live unlock set; the gate LOGIC
// is byte-identical, only relocated). Bound as `analytics_commands` (via `#[path]`) to keep it clearly
// distinct from the crate-level `audit` module and to avoid any future name shadow.
#[path = "analytics.rs"]
mod analytics_commands;
pub use analytics_commands::*;

// Facts / cross-meeting USER MEMORY commands + post-summary persistence (a GATED domain — user-memory
// reads/injection filter on the live unlock snapshot; the gate LOGIC is byte-identical, only
// relocated). Bound as `facts_commands` (via `#[path]`) to avoid colliding with the crate-level
// `crate::facts` module imported below (E0255).
#[path = "facts.rs"]
mod facts_commands;
pub use facts_commands::*;

// Audio / recording-lifecycle / voice-command / storage / voiceprint commands (a GATED domain —
// `list_voiceprints` snapshots the live `unlocked` set and `free_up_space` holds the seal lifecycle
// guard across the prune; the gate + at-rest-seal LOGIC is byte-identical, only relocated). Bound as
// `audio_commands` (via `#[path]`) to avoid colliding with the crate-level `crate::audio` module
// imported below (`use crate::audio::Recorder;`).
#[path = "audio.rs"]
mod audio_commands;
pub use audio_commands::*;

// Agentic Ask / in-meeting-chat helper commands (a GATED domain — `list_assistant_threads` /
// `gated_meeting_thread_turns` fail CLOSED on the live unlock snapshot and route through the
// `*_visible` readers; the gate LOGIC is byte-identical, only relocated). Bound as `ask_commands`
// (via `#[path]`) to keep it clearly distinct and avoid any future name shadow.
#[path = "ask.rs"]
mod ask_commands;
pub use ask_commands::*;

// Meetings read / detail / tags / speaker-reconcile commands (a GATED domain — `get_meeting_detail`
// masks to a sealed DTO, `get_timeline` returns empty, `list_meetings`/`search_meetings`/
// `brain_overview` route through the backend mask + `unlocked_snapshot`; the gate/mask LOGIC is
// byte-identical, only relocated). Bound as `meetings_commands` (via `#[path]`) to keep it clearly
// distinct and avoid any future name shadow.
#[path = "meetings.rs"]
mod meetings_commands;
pub use meetings_commands::*;

// Standalone-NOTES command surface (authored-note CRUD + Notes folder tree) — extracted verbatim
// (God-file split, PURE MOVE — gate/mask/seal/export-gate bodies byte-identical, only relocated).
// Bound as `notes_commands` (via `#[path]`) to keep it clearly distinct and avoid any future name
// shadow with a `crate::notes`-style module. The glob re-export makes every moved command resolve
// UNCHANGED at `crate::commands::…` for `generate_handler!` in `lib.rs` and every caller.
#[path = "notes.rs"]
mod notes_commands;
pub use notes_commands::*;

// Per-folder LOCK / UNLOCK / RELOCK / REMOVE-LOCK command surface — the verify-before-destroy
// CALLERS (a LOCK-CRITICAL domain, PURE MOVE — every seal/verify/blank/unseal ORDERING is
// byte-identical, only relocated). `lock_folder`/`seal_folder_extras` seal each blob and VERIFY it
// reads back decryptable BEFORE `blank_sealed_notes_in_folders` blanks the plaintext column /
// deletes the vault `.md`; audio seals via `encrypt_file` (verify-before-destroy inside) before the
// plaintext WAV is removed. `unlock_folder`/`unlock_meeting` do KEK → unwrap CK → decrypt back →
// materialize the session WAV → add the folder to the unlock set. `relock_*` re-blank plaintext +
// drop the decrypted session WAV (the `.enc` + `*_blob` columns stay). `remove_lock` decrypts every
// note/transcript/timeline/audio back to plaintext + re-exports the `.md`, never losing audio.
// The SHARED seal/unseal/audio/AAD helper web (`seal_folder_extras`/`unseal_folder_extras`/
// `unseal_folder_extras_permanent`/`reblank_folder_extras`/`seal_meeting_extras`/`aad_*`/`StreamRole`/
// `meeting_is_unlocked`/`unlocked_snapshot`/`lifecycle_guard`/`bump_seal_epoch`/`session_folder_ck`/
// `assert_in_vault`/`vault_path`) all STAY in `commands/mod.rs` (many are also called by the retained
// `move_note`/`delete_folder`/`delete_meeting` clusters); the moved commands reach them through
// `use super::*`, with the private ones promoted to `pub(crate)` (bodies byte-identical). Bound as
// `lock_commands` (via `#[path]`) to avoid colliding with the crate-level `lock()`/`lifecycle`
// mutex-guard machinery (E0255). The glob re-export makes every moved command resolve UNCHANGED at
// `crate::commands::…` for `generate_handler!` in `lib.rs` and every caller.
#[path = "lock.rs"]
mod lock_commands;
pub use lock_commands::*;

// ORG (Shared Brain / M6) + 1:1 LINK/USER SHARE (M3-client / M5) command surface — extracted verbatim
// (God-file split, PURE MOVE — every read-gate / egress-consent / redaction-firewall / crypto-envelope
// body is byte-identical, only relocated). The gated org/share READERS mask a sealed-not-unlocked
// source (`meeting_is_unlocked`/`folder_is_unlocked` + the `context_enabled` filter); every cloud
// egress stays fail-closed on the one-time consent, PII-scrubbed, and content-free-ledgered; the link
// key / account MK / OCK crypto moves verbatim (never logged). The SHARED session/crypto helpers
// (`share_base_url`/`valid_access_token`/`refresh_session`/`require_session_mk`/`SessionMk`/
// `session_server_user_id`/`tofu_check`/`TofuState`) + the ACCOUNT/AUTH commands + the gate helpers
// (`meeting_is_unlocked`/`folder_is_unlocked`/`session_folder_ck`/`sealed_document_blob`/`aad_content`)
// all STAY in `commands/mod.rs` (also called by the retained account + note/meeting-delete clusters);
// the moved commands reach them through `use super::*`, with the private ones promoted to `pub(crate)`
// (bodies byte-identical). Bound as `org_commands` (via `#[path]`) to avoid colliding with the
// crate-level `crate::e2ee::org` / `crate::storage::org_store` (E0255). The glob re-export makes every
// moved command resolve UNCHANGED at `crate::commands::…` for `generate_handler!` in `lib.rs` and
// every caller (incl. the sibling `commands/notes.rs` → `republish_org_shares_for_source`).
#[path = "org.rs"]
mod org_commands;
pub use org_commands::*;

/// Keychain account for the AI Gateway API key (matches `summarize::GATEWAY_KEY_ACCOUNT`).
/// Strictly separate from `ANTHROPIC_KEY_ACCOUNT` (defined in `commands/secrets.rs`) — never a
/// fallback to the Anthropic key (R3). Kept here because the gateway model-listing helpers below
/// reference it; `commands/secrets.rs` reaches it via `super::GATEWAY_KEY_ACCOUNT`.
pub(crate) const GATEWAY_KEY_ACCOUNT: &str = "gateway_api_key";

// ── IPC DTOs (camelCase mirrors of PHASE0-PLAN §6) ──

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartResult {
    pub meeting_id: String,
}


#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StopResult {
    pub meeting_id: String,
    pub markdown: String,
    /// Path of the exported Obsidian `.md`, or `None` when no vault is configured (the note
    /// is still saved to the DB — the vault is export-only).
    pub exported_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteDto {
    pub meeting_id: String,
    pub provider_id: String,
    pub markdown: String,
    pub exported_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    pub id: String,
    pub available: bool,
    pub reason: Option<String>,
}


/// A suggested label for a diarized cluster of the current meeting, from cosine re-identification
/// against the GATED set of labeled prior voiceprints. The FE offers it as a one-tap rename.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerSuggestion {
    /// The timeline label being suggested for (e.g. `others-1`).
    pub speaker: String,
    /// The suggested person name.
    pub suggested_label: String,
    /// The cosine score of the match (0..=1), for a "how confident" affordance.
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfigDto {
    pub provider_id: String,
    pub vault_path: Option<String>,
    pub vault_subfolder: Option<String>,
    pub whisper_model_path: Option<String>,
    pub language: Option<String>,
    pub anthropic_model: String,
    /// Brain/AI MODEL override for the active cloud provider. Settable from the DTO (the Settings
    /// UI owns the picker), like `anthropic_model` — a plain string, NOT preserve-only. Empty `""`
    /// = provider default. An omitted key deserializes to `""` (`#[serde(default)]`).
    #[serde(default)]
    pub provider_model: String,
    /// Brain/AI reasoning EFFORT (`""`/`low`/`medium`/`high`). Settable from the DTO. Honored ONLY
    /// by the direct `anthropic` provider (adaptive thinking); the `claude_code` CLI ignores it.
    /// An omitted key deserializes to `""` (`#[serde(default)]`).
    #[serde(default)]
    pub provider_effort: String,
    pub ollama_base_url: String,
    pub ollama_model: String,
    pub claude_binary: String,
    #[serde(default)]
    pub input_device: Option<String>,
    pub capture_system_audio: bool,
    #[serde(default = "default_true")]
    pub vad_enabled: bool,
    #[serde(default)]
    pub keep_hires_masters: bool,
    #[serde(default)]
    pub diarize_others: bool,
    #[serde(default)]
    pub voiceprint_enabled: bool,
    #[serde(default)]
    pub aec_enabled: bool,
    #[serde(default = "default_true")]
    pub post_aec_enabled: bool,
    /// Recording-storage cap in GB (`None` = no cap). Mirrors `AppConfig::audio_storage_limit_gb`.
    #[serde(default)]
    pub audio_storage_limit_gb: Option<u32>,
    /// Auto-delete oldest recordings' audio over the cap. Opt-in, default false.
    #[serde(default)]
    pub audio_auto_prune: bool,
    pub model_size: String,
    /// OPTIONAL live-caption ASR engine (`"whisper"` default / `"parakeet"`). Settable from the DTO
    /// (Settings ▸ Transcription owns the picker), like `model_size`. An omitted key deserializes to
    /// `"whisper"` (`#[serde(default = "default_live_asr_engine")]`) so an older FE payload never
    /// silently blanks it. Mirrors Rust `AppConfig::live_asr_engine` / FE `liveAsrEngine`.
    #[serde(default = "default_live_asr_engine")]
    pub live_asr_engine: String,
    /// Brain-sidecar IDLE-KILL window (s) — after this idle the host kills the `meetnotes-brain`
    /// child to reclaim its model RAM. Settable. An omitted key deserializes to 300
    /// (`#[serde(default = "…")]`) so an older FE payload never zeroes it. Mirrors Rust
    /// `AppConfig::brain_idle_timeout_secs` / FE `brainIdleTimeoutSecs`.
    #[serde(default = "default_brain_idle_timeout_secs")]
    pub brain_idle_timeout_secs: u64,
    /// Brain-sidecar READY-handshake timeout (s). Settable. Omitted ⇒ 90. Mirrors Rust
    /// `AppConfig::brain_ready_timeout_secs` / FE `brainReadyTimeoutSecs`.
    #[serde(default = "default_brain_ready_timeout_secs")]
    pub brain_ready_timeout_secs: u64,
    /// Brain-sidecar HARD per-generation cap (s) for calls with no explicit timeout. Settable.
    /// Omitted ⇒ 180. Mirrors Rust `AppConfig::brain_hard_cap_secs` / FE `brainHardCapSecs`.
    #[serde(default = "default_brain_hard_cap_secs")]
    pub brain_hard_cap_secs: u64,
    pub voice_trigger: bool,
    pub onboarded: bool,
    pub note_style: String,
    /// ENHANCE-MY-NOTES mode: "enhance" | "append" ("" from an older FE ⇒ "enhance").
    /// `#[serde(default)]` ⇒ an older FE payload that omits `notesMode` deserializes to `""`
    /// (the `dto_to_config` empty-guard then falls back to `"enhance"`).
    #[serde(default)]
    pub notes_mode: String,
    pub auto_organize: bool,
    /// NOTES assistant action toggles (`noteAssistRefine`/`noteAssistShorten`/`noteAssistEnhance`).
    /// Default true (`#[serde(default = "default_true")]`) so an older FE payload that omits them
    /// keeps all three ON — a missing toggle enables, never silently disables, the action.
    #[serde(default = "default_true")]
    pub note_assist_refine: bool,
    #[serde(default = "default_true")]
    pub note_assist_shorten: bool,
    #[serde(default = "default_true")]
    pub note_assist_enhance: bool,
    /// NOTES full-set opt-OUT list (`noteAssistActionsOff`): the ids of the non-legacy assistant
    /// actions the user turned OFF. `#[serde(default)]` ⇒ an older FE payload that omits it loads
    /// as an empty list (all actions enabled) — a missing field enables, never disables.
    #[serde(default)]
    pub note_assist_actions_off: Vec<String>,
    pub note_language: String,
    /// E3/security: default true (matches AppConfig::default) when the FE omits it on an older
    /// payload — an omitted flag must FAIL CLOSED (require a token), never silently disable MCP
    /// auth. Was `#[serde(default)]` (=false), which let a partial save flip the token requirement
    /// off; now defaults ON like its Stage-E siblings (BLK-3).
    #[serde(default = "default_true")]
    pub mcp_require_token: bool,
    /// Stage E: default true (matches AppConfig::default) when the FE omits it on an older payload.
    #[serde(default = "default_true")]
    pub lock_require_biometric: bool,
    /// Stage E: default true (matches AppConfig::default) when the FE omits it on an older payload.
    #[serde(default = "default_true")]
    pub relock_on_screenshare: bool,
    /// E10: one-time cloud-egress consent. DISPLAY-ONLY on this DTO: `get_config` carries the
    /// current value OUT so the FE can show consent status, but `dto_to_config` IGNORES whatever
    /// the FE sends back and PRESERVES the value already in `AppConfig` (BLK-4). The ONLY mutators
    /// are the dedicated `consent_to_cloud_egress` / `revoke_cloud_egress` commands, so a settings
    /// save can neither grant nor clear consent — even a partial/omitting payload
    /// (`#[serde(default)]` = false) is inert here.
    #[serde(default)]
    pub cloud_egress_consented: bool,
    /// M3-CLIENT: one-time SHARE-egress consent. DISPLAY-ONLY on this DTO (same discipline as
    /// `cloud_egress_consented`): `get_config` carries the stored value out so the FE can show
    /// consent status; `dto_to_config` PRESERVES it. Mutated ONLY by `consent_to_share_egress` /
    /// `revoke_share_egress`, so a settings save can neither grant nor clear it.
    #[serde(default)]
    pub share_egress_consented: bool,
    /// M6 Shared Brain: one-time ORG-egress consent. DISPLAY-ONLY on this DTO (same discipline as
    /// `share_egress_consented`): `get_config` carries the stored value out so the FE can show org
    /// consent status; `dto_to_config` PRESERVES it. Mutated ONLY by `consent_to_org_egress` /
    /// `revoke_org_egress`, so a settings save can neither grant nor clear it. `#[serde(default)]` =
    /// false (fail-closed).
    #[serde(default)]
    pub org_egress_consented: bool,
    /// Sharing-onboarding gate — whether the user has RESOLVED the first-run sharing decision
    /// (chose local-only OR went through the account door). DISPLAY-ONLY on this DTO, same
    /// preserve-only discipline as `share_egress_consented`: `get_config` carries the stored value
    /// OUT so the init gateway (`/welcome`) knows whether to show, but `dto_to_config` PRESERVES the
    /// stored value — a settings save can never set/clear it. Mutated ONLY by the dedicated
    /// `mark_sharing_choice_made` command. `#[serde(default)]` = false (a pre-existing install sees
    /// the gateway once).
    #[serde(default)]
    pub sharing_choice_made: bool,
    /// Phase H — which reasoner powers the on-device "brain" pre-analysis (Flow A): `cloud` |
    /// `local` | `off`. Unlike `cloud_egress_consented`, this IS settable from the DTO (the Settings
    /// UI owns the brain toggle). An omitted/unknown value deserializes to the default `Cloud`
    /// (`deserialize_with` tolerates an unknown token → `Cloud`, and `default` covers an omitted
    /// key), so a partial OR malformed save can never select an invalid backend.
    #[serde(default, deserialize_with = "deserialize_brain_backend_lenient")]
    pub brain_backend: BrainBackend,
    /// Phase H — the in-meeting VOICE ACTION DISPATCH master gate (Flow B). Settable from the DTO
    /// (the Settings UI owns the toggle). OPT-IN: an omitted value deserializes to `false`
    /// (`#[serde(default)]`), so a partial/older save can never silently enable the always-on
    /// in-meeting assistant.
    #[serde(default)]
    pub realtime_reactions: bool,
    /// Phase H — the SELECTED on-device brain model id (from `reason::BRAIN_MODELS`). Settable from
    /// the DTO, but `dto_to_config` VALIDATES it against the registry: an unknown/`None` id is
    /// IGNORED (the live selection is preserved, no error) so a settings save can never store a
    /// bogus model id. `select_brain_model` remains the other supported mutator.
    #[serde(default)]
    pub brain_model_id: Option<String>,
    /// Phase H — a CUSTOM on-device brain model file path (a `.gguf` NOT in the registry). Settable
    /// from the DTO (the "Custom GGUF model" input, sent as camelCase `brainModelPath`). Unlike
    /// `brain_model_id` this is stored VERBATIM (a local file path, not a registry id) — `dto_to_config`
    /// only normalizes empty→`None` (clears the custom path). `resolve_brain_model` (reason.rs) prefers
    /// this path when it points at an existing file and falls back safely to the registry id / cloud if
    /// the file is gone, so a stale path can never break load. Not egress — a local file, not a network.
    #[serde(default)]
    pub brain_model_path: Option<String>,
    /// brain2 RAG — the SEMANTIC SEARCH master flag. Settable from the DTO (the Settings UI owns the
    /// toggle), unlike `cloud_egress_consented` which is preserved-only. Plain bool; an omitted value
    /// deserializes to `false` (`#[serde(default)]`), so a partial/older save can never silently
    /// enable it. Flipping it on does NOT auto-index — the user runs `reindex_embeddings` to backfill.
    /// TIER 1 ASYMMETRY (intentional): the INTERNAL `AppConfig` now defaults this ON (the fresh-install
    /// default), but this DTO stays fail-safe-`false` so a partial/malformed FE save can never SILENTLY
    /// turn it ON (a field-omitting save resolves to `false` = the fail-safe OFF direction). `config_to_dto`
    /// carries the real stored value OUT and the FE echoes it back, so the true value still round-trips
    /// (see `dto_round_trips_semantic_search_enabled_both_ways`).
    #[serde(default)]
    pub semantic_search_enabled: bool,
    /// brain2 connector framework — the WEB SEARCH master toggle. Settable from the DTO (the Settings
    /// UI owns the toggle). An omitted value deserializes to `false` (`#[serde(default)]`), so a
    /// partial/older save can never silently enable it. Even ON, the web connector is exposed only
    /// once `web_search_consented` is granted AND a key is stored.
    #[serde(default)]
    pub web_search_enabled: bool,
    /// brain2 connector framework — one-time WEB SEARCH egress consent. PRESERVE-ONLY on this DTO,
    /// exactly like `cloud_egress_consented`: `get_config` carries the current value OUT (so the FE can
    /// show consent status), but `dto_to_config` IGNORES the incoming value and PRESERVES the stored
    /// one. The ONLY mutator is the dedicated `consent_to_web_search` command, so a settings save can
    /// neither grant nor clear web-search egress consent. `#[serde(default)]` = false (fail-closed).
    #[serde(default)]
    pub web_search_consented: bool,
    /// brain2 connectors (Phase 2) — the JIRA master toggle. Settable from the DTO (the Settings UI
    /// owns the toggle). An omitted value deserializes to `false` (`#[serde(default)]`), so a
    /// partial/older save can never silently enable it. Even ON, the connector is exposed only once
    /// `jira_consented` is granted AND a base URL + email + token are configured.
    #[serde(default)]
    pub jira_enabled: bool,
    /// brain2 connectors — one-time JIRA egress consent. PRESERVE-ONLY on this DTO, exactly like
    /// `web_search_consented`: `get_config` carries the current value OUT (so the FE can show consent
    /// status), but `dto_to_config` IGNORES the incoming value and PRESERVES the stored one. The ONLY
    /// mutator is the dedicated `consent_to_jira` command. `#[serde(default)]` = false (fail-closed).
    #[serde(default)]
    pub jira_consented: bool,
    /// The Jira Cloud site base URL (non-secret). Settable from the DTO. Default `""` (unset).
    #[serde(default)]
    pub jira_base_url: String,
    /// The Atlassian account email paired with the token for Basic auth (non-secret). Settable from
    /// the DTO. Default `""` (unset).
    #[serde(default)]
    pub jira_email: String,
    /// brain2 connectors (Phase 3) — the SLACK master toggle. Settable from the DTO (the Settings UI
    /// owns the toggle). An omitted value deserializes to `false` (`#[serde(default)]`), so a
    /// partial/older save can never silently enable it. Even ON, the connector is exposed only once
    /// `slack_consented` is granted AND a user token is configured.
    #[serde(default)]
    pub slack_enabled: bool,
    /// brain2 connectors — one-time SLACK egress consent. PRESERVE-ONLY on this DTO, exactly like
    /// `jira_consented`: `get_config` carries the current value OUT (so the FE can show consent
    /// status), but `dto_to_config` IGNORES the incoming value and PRESERVES the stored one. The ONLY
    /// mutator is the dedicated `consent_to_slack` command. `#[serde(default)]` = false (fail-closed).
    #[serde(default)]
    pub slack_consented: bool,
    /// Opt-in: inherit the shell environment into the `claude` CLI subprocess (restores the older
    /// behavior where an env `ANTHROPIC_API_KEY` reached the CLI). Settable from the DTO (the Settings
    /// UI owns the toggle). An omitted value deserializes to `false` (`#[serde(default)]`) = the
    /// hardened env-cleared run, so a partial/older save can never silently enable it. Even ON the DB
    /// encryption keys are never inherited (see `AppConfig::claude_code_inherit_env`).
    #[serde(default)]
    pub claude_code_inherit_env: bool,
    /// Base URL of the user's OpenAI-compatible AI gateway. Settable from the DTO (the Settings UI
    /// owns the field). An omitted value deserializes to `""` (`#[serde(default)]`). A non-empty
    /// value is validated at provider-construction time; `saveConfig` + `getConfig` persist it verbatim.
    #[serde(default)]
    pub gateway_base_url: String,
    /// Model id to send to the gateway (e.g. `"gpt-4o"`). Settable from the DTO. Default `""`.
    #[serde(default)]
    pub gateway_model: String,
    /// M3-CLIENT — base URL of the Murmur sharing server (self-host or hosted). Settable from the
    /// DTO (Settings → Account owns the field). An omitted value deserializes to `""` (unset →
    /// account/share commands fail closed `Unavailable`). Validated like `gateway_base_url` (https
    /// required, http loopback-only, no embedded creds) at `ShareClient::new`.
    #[serde(default)]
    pub share_base_url: String,
    /// Proactive brain P1 — the recall-card mute toggle (`crate::proactive`). Settable from the
    /// DTO (the Settings UI owns it). Defaults ON when omitted (`default_true`), matching
    /// `AppConfig::default` — an older FE payload must not silently flip the backend mute.
    #[serde(default = "default_true")]
    pub proactive_hints_enabled: bool,
    /// Cross-meeting USER MEMORY master gate (`crate::user_memory`). Settable from the DTO (the
    /// Settings UI owns the toggle). Defaults ON when omitted (`default_true`), matching
    /// `AppConfig::default` — an older FE payload must not silently turn memory off.
    #[serde(default = "default_true")]
    pub user_memory_enabled: bool,
    /// Model-role override — the connection serving the NOTES role (see
    /// `crate::summarize::roles`). Settable from the DTO (a future Settings UI owns the rows).
    /// `""` (and an omitted key, `#[serde(default)]`) = inherit the legacy mapping — so an older
    /// FE payload can never flip a role.
    #[serde(default)]
    pub role_notes_connection: String,
    /// Model-role override — the model for the Notes role (`""` = connection default).
    #[serde(default)]
    pub role_notes_model: String,
    /// Model-role override — the effort for the Notes role (`""` = provider default).
    #[serde(default)]
    pub role_notes_effort: String,
    /// Model-role override — the connection serving the ASK role. Same semantics as the Notes keys.
    #[serde(default)]
    pub role_ask_connection: String,
    /// Model-role override — the model for the Ask role.
    #[serde(default)]
    pub role_ask_model: String,
    /// Model-role override — the effort for the Ask role.
    #[serde(default)]
    pub role_ask_effort: String,
    /// Model-role override — the connection serving the LIVE role. Same semantics as the Notes keys.
    #[serde(default)]
    pub role_live_connection: String,
    /// Model-role override — the model for the Live role.
    #[serde(default)]
    pub role_live_model: String,
    /// Model-role override — the effort for the Live role.
    #[serde(default)]
    pub role_live_effort: String,
}

/// serde default for the Stage E security flags (which default ON in `AppConfig`).
fn default_true() -> bool {
    true
}

/// serde default for the DTO's `live_asr_engine` — the whisper live path (today's behavior), so an
/// older FE payload that omits `liveAsrEngine` never blanks the engine.
fn default_live_asr_engine() -> String {
    crate::transcribe::live_asr::ENGINE_WHISPER.to_string()
}

/// serde default for the DTO's `brain_idle_timeout_secs` — 300 s (matches `AppConfig`), so an older
/// FE payload that omits `brainIdleTimeoutSecs` never zeroes the idle-kill window.
fn default_brain_idle_timeout_secs() -> u64 {
    300
}

/// serde default for the DTO's `brain_ready_timeout_secs` — 90 s.
fn default_brain_ready_timeout_secs() -> u64 {
    90
}

/// serde default for the DTO's `brain_hard_cap_secs` — 180 s.
fn default_brain_hard_cap_secs() -> u64 {
    180
}

/// Lenient `brain_backend` deserialization for the settings DTO: an UNKNOWN/garbage token
/// degrades to the default `Cloud` instead of failing the whole `save_config` payload (the derived
/// enum would reject `"bogus"` with an error). Mirrors `BrainBackend::from_str_or_default`, so the
/// FE can never wedge a settings save with a stale/typo'd backend value. A non-string (e.g. null)
/// also falls back to `Cloud`.
fn deserialize_brain_backend_lenient<'de, D>(
    deserializer: D,
) -> std::result::Result<BrainBackend, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let token = Option::<String>::deserialize(deserializer)?;
    Ok(token
        .as_deref()
        .map(BrainBackend::from_str_or_default)
        .unwrap_or_default())
}

/// A meeting + its latest note + transcript segments (Library Detail view).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingDetailDto {
    pub meeting: Meeting,
    pub note: Option<NoteDto>,
    pub segments: Vec<Segment>,
    /// In-meeting voice-assistant interactions (the persisted Q&A: the user's spoken command + the
    /// assistant's answer + citations). EMPTY when the meeting is locked-and-not-session-unlocked
    /// (gated by `meeting_is_unlocked`, exactly like `note`/`segments`) — and also empty for a sealed
    /// meeting at rest because the rows were PURGED on seal (purge-on-seal, like the correction-log).
    pub assistant_interactions: Vec<crate::storage::models::AssistantInteraction>,
    /// Phase 0.5 — `true` when the meeting's folder is sealed AND not session-unlocked. The FE
    /// renders a locked state (Touch-ID-to-unlock) instead of content; `note`/`segments` are
    /// empty in that case (the content is encrypted at rest, decrypted only on session-unlock).
    pub locked: bool,
    /// Phase 5 provenance — the provider id used to generate the note (e.g. `"gateway"`/`"anthropic"`).
    /// `None` when locked (masked) or when the note has no recorded provider beyond `provider_id`.
    /// This mirrors `note.provider_id` but is surfaced here so the FE doesn't need to dig into `note`.
    pub ai_provider: Option<String>,
    /// Phase 5 provenance — the model id that was REQUESTED when generating the note (e.g.
    /// `"gpt-4o"`, `"claude-opus-4-8"`). `None` when locked, unknown, or the provider uses its own
    /// default (no explicit model was configured).
    pub ai_model: Option<String>,
    /// Phase 5 provenance — the model id ACTUALLY served by the gateway/API (from `CallMeta`).
    /// May differ from `ai_model` when the gateway aliases, load-balances, or falls back. `None`
    /// when locked, or when the provider did not return model metadata in the response.
    pub model_served: Option<String>,
}

// ── Commands (PHASE0-PLAN §7) ──



/// Begin mic capture. Inserts a Meeting(Draft→Recording), stores Recorder in state,
/// sets current_meeting. Returns the new meeting id. Errors if already recording.
#[tauri::command]
pub async fn start_recording(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<StartResult, AppError> {
    // Reject if a recording is already in progress.
    {
        let recorder = state
            .recorder
            .lock()
            .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
        if recorder.is_some() {
            return Err(AppError::Audio("already recording".into()));
        }
    }

    // Re-sweep orphaned capture helpers BEFORE spawning this recording's own (same decision logic
    // as the once-at-launch reaper): an orphan that appeared while THIS app kept running (another
    // instance crashed / was SIGKILL'd mid-recording) would otherwise capture alongside the new
    // recording until its 4h self-cap. Safe by construction — a helper owned by any LIVE Murmur
    // process (including the one this call is about to spawn for) is always spared. Best-effort,
    // never fails the recording. Hardening 2026-07-16: the sweep shells out to /bin/ps (×2) plus
    // a per-candidate `kill -0`, so it runs on a BLOCKING worker instead of stalling this async
    // runtime thread — but it is still AWAITED: the sweep MUST complete before this recording's
    // own helpers spawn (ordering guarantee unchanged).
    if let Err(e) =
        tauri::async_runtime::spawn_blocking(crate::audio::aec::reap_orphaned_capture_helpers)
            .await
    {
        tracing::warn!(target: "audio", error = %e, "orphan-helper sweep join failed; recording continues");
    }

    // Fresh recording ⇒ re-arm the 4h-cap rising-edge notice (see `recording_level`). If a previous
    // recording hit the cap and set this, the next recording must be able to fire the notice again.
    state
        .capped_notified
        .store(false, std::sync::atomic::Ordering::Relaxed);
    // Fresh recording ⇒ reset the Realtime-Reactions shadow counter (per-recording calibration) and
    // the per-recording whisper-card dedup set (each recording surfaces a contradiction at most once).
    state
        .reactions_shadow_count
        .store(0, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut e) = state.reactions_emitted.lock() {
        e.clear();
    }
    // Brain v2 L4: fresh recording ⇒ fresh running bullets (RAM buffer + delta tracker) — a stale
    // previous meeting's bullets must never seed the new meeting's substrate or prompt inject.
    crate::transcribe::bullets::clear_ram(&state.live_bullets, &state.live_bullets_tracker);

    let meeting_uuid = uuid::Uuid::new_v4();
    let meeting_id = meeting_uuid.to_string();
    let started_at = chrono::Utc::now().to_rfc3339();

    // STABLE PROVISIONAL TITLE (2026-07-16 companion note, Task 4): at record start `meetings.title`
    // was previously NULL — so `[[Meeting]]` had no meaningful target during the recording (the FE
    // showed a placeholder). Give the in-progress meeting a stable, human, date/time-based title up
    // front so a jotted companion note's `[[<name>]]` link is meaningful IMMEDIATELY. This is a
    // PROVISIONAL name: the existing auto-title-on-close (`pipeline::set_meeting_title` from the
    // generated note's front-matter) UPGRADES it, and the companion-note title-sync then refreshes
    // the note's managed title + link. Local time (matches how the user experiences "when").
    let provisional_title = provisional_meeting_title(&chrono::Local::now());

    // Persist the meeting in RECORDING state up-front so a crash mid-capture leaves a row behind
    // rather than losing the meeting silently. If this process dies before `stop_recording`, that
    // row is reconciled to the terminal ERROR state at the next launch
    // (`Db::reconcile_stuck_recordings`, called from `lib.rs` setup) so it never lingers as a
    // "still recording" ghost. Full audio salvage of the abandoned capture is tracked separately
    // (mic-spill task).
    state.db.insert_meeting(&Meeting {
        id: meeting_id.clone(),
        started_at,
        ended_at: None,
        title: Some(provisional_title),
        duration_s: 0,
        audio_path: None,
        status: MeetingStatus::Recording,
        folder_id: None,
    })?;

    // Free the mic from the voice listener (if any) before opening it for the recording.
    {
        let app2 = app.clone();
        let _ = tokio::task::spawn_blocking(move || stop_voice_listener(&app2)).await;
    }

    // Start mic capture on the configured input device (falls back to default if unset/gone).
    let input_device = state
        .config
        .lock()
        .ok()
        .and_then(|c| c.input_device.clone());
    let recorder = Recorder::start(input_device)?;
    // STAGE 2 crash-salvage: grab a read-only handle onto the live buffer + the device rate BEFORE the
    // recorder is moved into state — the spill writer mirrors this handle (never the RT callback).
    let sample_reader = recorder.sample_reader();
    let src_rate = recorder.source_sample_rate();
    {
        let mut slot = state
            .recorder
            .lock()
            .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
        *slot = Some(recorder);
    }
    {
        let mut current = state
            .current_meeting
            .lock()
            .map_err(|_| AppError::Audio("current_meeting mutex poisoned".into()))?;
        *current = Some(meeting_uuid);
    }

    // Optionally capture system audio (the other side of the call) alongside the mic.
    // Best-effort: if it can't start, we log and record mic-only — never fail recording.
    // `sys_scratch_for_spill` remembers the far-side scratch path so the crash-salvage sidecar can
    // pair the "others" track at next launch (only set when the system recorder actually started).
    let mut sys_scratch_for_spill: Option<std::path::PathBuf> = None;
    {
        let enabled = state
            .config
            .lock()
            .map(|c| c.capture_system_audio)
            .unwrap_or(false);
        if enabled && crate::audio::system::is_available(&app) {
            let sys_wav = std::env::temp_dir().join(format!("meetnotes-sys-{meeting_id}.wav"));
            match crate::audio::system::SystemAudioRecorder::start(&app, sys_wav.clone()) {
                Ok(rec) => {
                    sys_scratch_for_spill = Some(sys_wav);
                    if let Ok(mut slot) = state.system_recorder.lock() {
                        *slot = Some(rec);
                    }
                }
                Err(e) => tracing::warn!(
                    target: "audio", error = %e,
                    "system-audio capture unavailable; recording mic only"
                ),
            }
        }
    }

    // STAGE 2 crash-salvage: mirror the growing RAM mic buffer to an on-disk spill (+ a sidecar naming
    // the rate + the paired far-side scratch) so a crash / SIGKILL mid-record is recoverable at next
    // launch (see `audio::spill`). Its own NON-RT writer thread; best-effort — a spill start failure
    // NEVER fails the recording (the RAM buffer stays the sole primary source until Stop).
    match crate::audio::spill::SpillWriter::start(
        &meeting_id,
        sample_reader,
        src_rate,
        sys_scratch_for_spill,
    ) {
        Ok(w) => {
            if let Ok(mut slot) = state.spill_writer.lock() {
                *slot = Some(w);
            }
        }
        Err(e) => tracing::warn!(
            target: "audio", error = %e,
            "crash-salvage spill unavailable; recording on the RAM buffer only"
        ),
    }

    // NOTE: the live VPIO echo-cancel helper (aeccap) is intentionally NEVER spawned anymore.
    // It is superseded by the OFFLINE AEC pass (`post_aec_enabled`, default OFF — opt-in in
    // Settings → Audio) which cancels
    // echo after Stop using the captured system track as a perfect far-end reference — with zero
    // effect on the live call. On a real Mac VPIO (a) cancelled ~nothing (macOS gives an input-
    // only voice-processing unit no downlink reference) and (b) DUCKED all other apps' audio
    // system-wide, so starting a recording heavily quieted whatever was playing (e.g. YouTube).
    // The `aec_enabled` flag + the `aeccap` helper stay in the tree but dormant; the ASR feed +
    // archive get their echo removed offline instead. See
    // docs/research/2026-07-02-audio-echo-full-remediation.md.

    // Best-effort LIVE captions: a read-only background loop emitting partial transcripts
    // during recording (see transcribe::live). Never affects the recording or final note.
    if let Some(cfg) = state.config.lock().ok().map(|c| c.clone()) {
        let lang = cfg.language.as_deref().unwrap_or("");
        let configured = || {
            crate::transcribe::model::resolve_model_path(
                cfg.whisper_model_path.as_deref().map(std::path::Path::new),
                &cfg.model_size,
                lang,
            )
            .ok()
            .flatten()
        };
        // T1.3 — the UNCONDITIONAL live-model pin (`live_model_pin`, default "small"): the LIVE
        // caption tick decodes with the pinned SIZE whenever its file is downloaded, regardless
        // of `model_size` — a `large-v3` live tick saturates the shared Metal GPU for the whole
        // meeting (the heat complaint), while captions are throwaway (the post-Stop ACCURATE
        // pass on the configured model is the authoritative transcript and is unaffected).
        // `""` disables the pin → the configured model, with the LEGACY `brain_live` pin-to-small
        // (D1, spec §4.3: the live tick must not starve the light reasoner) still applying — the
        // full decision lives in `model::live_pin_size`.
        //
        // ABSENT-FILE fallback (T2 default-flip follow-up — a fresh ≥12 GB install downloads
        // ONLY `ggml-large-v3-turbo-q8_0.bin`, so the pinned `small` is absent on the flip's
        // target machines): prefer the largest downloaded live-SAFE model (small → base →
        // tiny); a live-safe CONFIGURED model still works as before; but a medium/large-class
        // configured model is NEVER handed to the live tick — captions are skipped for THIS
        // recording and the pinned size is downloaded in the background (single-flight,
        // best-effort) so the next recording — and the wake listener, which reconciles via
        // `restart_voice_listener` when this recording stops — has it.
        let live_model = match crate::transcribe::model::live_pin_size(
            &cfg.live_model_pin,
            cfg.brain_live,
        ) {
            Some(size) => match crate::transcribe::model::resolve_model_path(None, &size, lang) {
                Ok(Some(p)) => Some(p),
                _ => match crate::transcribe::model::live_fallback_model(lang) {
                    Some(p) => {
                        tracing::warn!(
                            target: "live",
                            pin = %size,
                            "pinned live model absent; live tick uses the largest downloaded live-safe whisper model"
                        );
                        Some(p)
                    }
                    None => match configured() {
                        Some(p) if !crate::transcribe::model::is_live_heavy_model_file(&p) => {
                            tracing::warn!(
                                target: "live",
                                pin = %size,
                                "pinned live model absent; live tick uses the configured whisper model (may contend with the light reasoner)"
                            );
                            Some(p)
                        }
                        Some(_) => {
                            // Only medium/large-class models downloaded (e.g. the fresh
                            // turbo-default install): NEVER run a large encoder on the 3 s
                            // live tick (T1.3 heat).
                            tracing::warn!(
                                target: "live",
                                pin = %size,
                                "pinned live model absent and only medium/large models downloaded; live captions off for this recording; downloading the pinned model in the background"
                            );
                            spawn_live_pin_download(size, lang.to_string());
                            None
                        }
                        None => None,
                    },
                },
            },
            None => configured(),
        };
        if let Some(model_path) = live_model {
            crate::transcribe::live::spawn(app.clone(), model_path, cfg.language.clone());
        }
    }

    let _ = app.emit(
        EVENT_STATUS,
        StatusPayload {
            stage: "recording".into(),
            message: "Recording…".into(),
            meeting_id: Some(meeting_id.clone()),
        },
    );

    Ok(StartResult { meeting_id })
}

/// Best-effort BACKGROUND download of the pinned LIVE model (T2 default-flip follow-up): a
/// fresh turbo-default install has no live-safe whisper model on disk, so the first record
/// start fetches the pinned size (~487 MB `small`) off the recording path. Single-flight (an
/// `AtomicBool` latch — consecutive record starts while a download is in flight must not race
/// two writers onto the same `.part` file); failure is logged and re-armed (next record start
/// retries). Deliberately does NOT spawn the live loop when the download lands mid-meeting:
/// a belated `live::spawn` could race a NEXT recording's own live loop and clear its
/// `live_transcript`. The model serves the next recording + the wake listener instead.
fn spawn_live_pin_download(size: String, language: String) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
    if IN_FLIGHT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    tauri::async_runtime::spawn(async move {
        match crate::transcribe::model::ensure_model(None, &size, &language, |_, _| {}).await {
            Ok(_) => {
                tracing::info!(target: "live", pin = %size, "pinned live model downloaded; live captions available from the next recording")
            }
            Err(e) => {
                tracing::warn!(target: "live", pin = %size, error = %e, "pinned live model download failed; will retry on a later record start")
            }
        }
        IN_FLIGHT.store(false, Ordering::SeqCst);
    });
}

/// Stop capture, then run the full pipeline (pipeline::run_after_stop). Returns the
/// exported note path + markdown. Emits status events throughout. Errors if not recording.
#[tauri::command]
pub async fn stop_recording(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<StopResult, AppError> {
    // Take the recorder out of state (errors if not recording).
    let recorder = {
        let mut slot = state
            .recorder
            .lock()
            .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
        slot.take()
            .ok_or_else(|| AppError::Audio("not recording".into()))?
    };

    // STAGE 2 crash-salvage: take the spill writer into a guard whose `Drop` stops the writer thread
    // and DELETES the plaintext spill + sidecar on EVERY exit path of the pipeline (success,
    // `?`-error, panic-unwind) — mirroring `pipeline::ScratchWav`. It is MOVED INTO the detached
    // pipeline task below (2026-07-16) and dropped there right after `run_after_stop` returns, so
    // its drop timing relative to the pipeline is unchanged (it survives until the archive WAV is
    // written) — but it is now immune to THIS command future being dropped mid-await (webview
    // teardown): the spill lives exactly as long as the pipeline that is consuming the audio.
    // ONLY a process crash (the task never finishes) leaves it behind for next-launch salvage.
    // A POISONED `spill_writer` mutex (`.lock().ok()` ⇒ None) merely DEFERS this clean-stop cleanup:
    // the spill lingers to next launch, where `claim_inflight` sees the row is no longer RECORDING and
    // DiscardOrphans it — benign (no leak, no content loss), so tolerating the poison here is correct.
    let spill_guard = state.spill_writer.lock().ok().and_then(|mut s| s.take());

    // The recording is definitively over — clear the accumulated live-caption buffer NOW so a
    // stale tail can never be injected into assistant prompts after Stop (nor keep egressing once
    // the just-recorded folder is sealed). The authoritative transcript is produced below.
    crate::transcribe::live::clear_live_transcript(&state.live_transcript);
    // Brain v2 L4: clear the running-bullets RAM the same way. The crash-recovery `live_bullets`
    // DB row deliberately SURVIVES this clear — the note pipeline below reads it as the
    // "Live notes (auto)" Stage-1 input and consumes (clears) it after the note persists.
    crate::transcribe::bullets::clear_ram(&state.live_bullets, &state.live_bullets_tracker);

    let meeting_uuid = {
        let mut current = state
            .current_meeting
            .lock()
            .map_err(|_| AppError::Audio("current_meeting mutex poisoned".into()))?;
        current
            .take()
            .ok_or_else(|| AppError::Audio("no current meeting".into()))?
    };
    let meeting_id = meeting_uuid.to_string();

    // DOCUMENT-FIRST cleanup (v2 companion note): the "Note" tab EAGERLY creates a companion note to
    // mount the editor on. If the user never wrote into it, remove it now so an unused recording
    // leaves no clutter. BEST-EFFORT — a delete failure NEVER fails Stop (and only ever deletes a
    // body-empty companion note, so no user content is at risk). Runs BEFORE the pipeline reads the
    // companion body for the summary, so an empty companion correctly falls back to `manual_notes`.
    if let Err(e) = delete_companion_note_if_empty_inner(state.inner(), &meeting_id).await {
        tracing::warn!(target: "notes", meeting_id = %meeting_id, error = %e, "empty-companion cleanup skipped (Stop unaffected)");
    }

    // Capture the mic stream's host start instant BEFORE consuming the recorder — it anchors the
    // mic ("me") segments onto the absolute timeline in the wall-clock merge (pipeline.rs).
    let mic_started_at = recorder.started_at();
    let (samples, src_rate) = recorder.stop()?;

    // Stop the system-audio sidecar (if any) and collect its WAV + host start instant. The
    // sidecar's start instant anchors the system ("others") segments; the two streams run on
    // INDEPENDENT clocks, so we merge by wall-clock, not sample count (see audio::merge).
    let (system_wav, system_started_at) = {
        let rec = state
            .system_recorder
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        match rec {
            Some(r) => {
                let started = r.started_at();
                (r.stop().unwrap_or(None), Some(started))
            }
            None => (None, None),
        }
    };

    // Stop the AEC mic helper (if any) and collect its WAV — used as the ASR mic feed; None falls
    // back to the raw cpal mic.
    let aec_mic_wav = {
        let rec = state
            .aec_recorder
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        match rec {
            Some(r) => r.stop().unwrap_or(None),
            None => None,
        }
    };

    // Duration from the persisted started_at, falling back to a sample-count estimate.
    let duration_s = compute_duration_s(&state, &meeting_id, samples.len(), src_rate);

    // DETACHED, panic-mapped pipeline execution (2026-07-16). The verified production wedge:
    // `run_after_stop` was awaited INLINE in this command future, and Tauri never settles the JS
    // invoke Promise for a command future that PANICS (tokio swallows the panic at the task
    // boundary) or is DROPPED mid-await — the FE stayed on "Transcribing…" forever with the
    // meeting row stuck at RECORDING and no terminal event. Running the pipeline in a REAL
    // spawned task fixes both halves:
    //   • a pipeline PANIC surfaces here as a `JoinError` → mapped to an `AppError` so the
    //     invoke Promise REJECTS (FE catch → "error"), and the in-task `TerminalStatusGuard`
    //     has already persisted `Error` + emitted the terminal `error` event during the unwind;
    //   • if THIS command future is dropped (webview teardown/reload), the detached task keeps
    //     running to completion and still performs its own status writes + event emits — the
    //     (re)loaded FE recovers via the event path even without the Promise.
    let task_app = app.clone();
    let task_meeting_id = meeting_id.clone();
    let pipeline_task = tauri::async_runtime::spawn(async move {
        // Owns the crash-salvage spill for exactly the pipeline's lifetime — see the comment at
        // the `take()` above. Dropped (spill deleted) when this scope exits: success, error, or
        // panic-unwind — never before the pipeline has finished with the audio.
        let _spill_guard = spill_guard;
        let state = task_app.state::<AppState>();
        // Armed NOW; disarmed on BOTH normal arms below. Fires ONLY on a panic-unwind (or an
        // unexpected early exit) — `run_after_stop`'s Err arm already persists `Error` + emits
        // the `error` stage itself, so there is no double-emit.
        let terminal_guard = pipeline::TerminalStatusGuard::arm(
            Some(task_app.clone()),
            state.db.clone(),
            &task_meeting_id,
        );
        let result = pipeline::run_after_stop(
            &task_app,
            &state,
            &task_meeting_id,
            samples,
            src_rate,
            duration_s,
            system_wav,
            aec_mic_wav,
            mic_started_at,
            system_started_at,
        )
        .await;
        terminal_guard.disarm();
        // Resume voice listening if it's still enabled (the mic is free again). Inside the task
        // (Ok arm only, preserving the pre-change `?` semantics) so it still runs when the outer
        // command future was dropped mid-pipeline.
        if result.is_ok() {
            restart_voice_listener(task_app.clone());
        }
        result
    });
    let result = await_pipeline_task(pipeline_task).await?;

    Ok(StopResult {
        meeting_id: result.meeting_id,
        markdown: result.note_markdown,
        exported_path: result
            .exported_path
            .map(|p| p.to_string_lossy().to_string()),
    })
}

/// Await the detached pipeline task, mapping a join failure (= the pipeline task PANICKED; it is
/// never aborted) into a real [`AppError`] so the invoke Promise REJECTS instead of never
/// settling (the FE's `stop()` catch then shows the error state). The panic's message +
/// location are logged by the global panic hook (`lib.rs`); the `JoinError` here carries no
/// note/transcript content, so the surfaced message is PII-safe.
async fn await_pipeline_task<T>(
    task: tauri::async_runtime::JoinHandle<Result<T, AppError>>,
) -> Result<T, AppError> {
    task.await.map_err(|e| {
        AppError::Other(anyhow::anyhow!(
            "note pipeline crashed unexpectedly ({e}) — the meeting was marked failed; \
             use Retry transcription on the recording to run it again"
        ))
    })?
}

/// Best-effort recording duration in whole seconds: prefer `now - started_at` from the
/// DB; fall back to `samples / sample_rate` if the timestamp can't be parsed.
fn compute_duration_s(
    state: &State<'_, AppState>,
    meeting_id: &str,
    sample_count: usize,
    src_rate: u32,
) -> i64 {
    if let Ok(Some(meeting)) = state.db.get_meeting(meeting_id) {
        if let Ok(started) = chrono::DateTime::parse_from_rfc3339(&meeting.started_at) {
            let secs = (chrono::Utc::now() - started.with_timezone(&chrono::Utc)).num_seconds();
            if secs >= 0 {
                return secs;
            }
        }
    }
    if src_rate > 0 {
        (sample_count as i64) / (src_rate as i64)
    } else {
        0
    }
}



















/// Ask the in-meeting assistant a CHAT message — the dedicated multi-turn conversation panel. Unlike
/// the fire-and-forget voice/card path, this RETURNS the reply (a `VoiceActionResult`) so the chat
/// panel can resolve the in-flight assistant bubble; the live tool-trace streams via `EVENT_CHAT_TOOL`.
/// The FE sends the FULL conversation each turn, so the brain has multi-turn memory. The heavy agentic
/// work runs on a blocking thread (it can take seconds) so the async runtime stays free. SAME gated +
/// consent-gated + redacting brain as voice (no new egress class).
/// `thread_id`/`anchor_text` are OPTIONAL thread identity (FE camelCase: `threadId`/`anchorText`):
/// the @brain thread this exchange belongs to, and the note text the thread was anchored to. A
/// missing `thread_id` is backend-generated (UUID v4) so every persisted exchange carries one. The
/// PERSISTED row's `command` is `latest` — the user's newest message, never the rendered history.
/// `meeting_id` (FE camelCase `meetingId`) is the OPTIONAL scope meeting the FE binds this thread to
/// (Phase 4): resolved as `meeting_id.or(state.current_meeting)` so an explicit FE id wins (a bound
/// past/anchored thread answers about ITS meeting even while a different meeting records) and a
/// `None` keeps the live-recording pointer. The resolved id is what `gated_live_context` /
/// the executor scope to AND what the persisted thread row is bound to — killing the wrong-meeting bug.
#[tauri::command]
pub async fn ask_assistant_chat(
    app: AppHandle,
    messages: Vec<ChatMsg>,
    thread_id: Option<String>,
    anchor_text: Option<String>,
    meeting_id: Option<String>,
    explicit_sources: Option<Vec<crate::storage::models::SourceRef>>,
) -> Result<crate::voice_action::VoiceActionResult, AppError> {
    let (latest, conversation) = format_chat(&messages)?;
    let thread_id = crate::transcribe::live::ensure_thread_id(thread_id);
    // note↔meeting-links PR-2 — SOURCE-SCOPED augmentation (PARTIAL, the SHOULD leg). The @brain
    // assistant loop (`run_assistant_query`) is a deep current-first cascade whose tool executor +
    // deterministic floor legs would need wide surgery to fully candidate-constrain; PR-2 threads
    // the pinned sources one level in so the cloud AGENTIC cascade reasons with the gated pinned
    // corpus injected into its conversation context. `None`/empty ⇒ byte-identical to before. The
    // remaining full candidate-constraint of the floor/tool legs is a documented follow-up.
    let explicit_sources = explicit_sources.filter(|s| !s.is_empty());
    tokio::task::spawn_blocking(move || {
        crate::transcribe::live::run_assistant_query(
            &app,
            &latest,
            &conversation,
            crate::events::EVENT_CHAT_TOOL,
            &thread_id,
            anchor_text.as_deref(),
            meeting_id.as_deref(),
            explicit_sources.as_deref(),
        )
    })
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("chat task join failed: {e}")))
}


#[cfg(test)]
#[path = "tests/chat_format_tests.rs"]
mod chat_format_tests;

/// The most recent note (markdown + export path) for the last-note preview pane.
#[tauri::command]
pub fn get_last_note(state: State<'_, AppState>) -> Result<Option<NoteDto>, AppError> {
    // BLK-2b: the latest VISIBLE note only — a sealed-and-not-unlocked latest note is skipped so the
    // recorder bar never shows its blanked content (and never depends on at-rest blanking).
    let unlocked = unlocked_snapshot(state.inner())?;
    let note = state.db.latest_note_visible(&unlocked)?;
    Ok(note.map(|n| NoteDto {
        meeting_id: n.meeting_id,
        provider_id: n.provider_id,
        markdown: n.markdown,
        exported_path: n.exported_path,
    }))
}

/// Replace a meeting note's markdown (in-app edit) and re-write the SAME vault file in
/// place (no duplicate). Returns the updated note.
///
/// COMMIT BOUNDARY: after the local write succeeds, BEST-EFFORT re-publish any org shares of this
/// meeting so a colleague sees the edit (never frozen at share time). Best-effort — a republish
/// failure NEVER fails the save (`let _ = …`); the launch sweep retries a `failed` row.
#[tauri::command]
pub async fn update_note(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
    markdown: String,
) -> Result<NoteDto, AppError> {
    // PERF (PR-1 finding 2): `update_note_inner` does the durable seal-on-write upsert + vault
    // re-write AND (Brain v3 gap #1) re-derives the meeting's chunks/vectors via Candle/Metal — a
    // multi-second stall on a long meeting / cold e5 if run INLINE on this async command's Tokio
    // worker. Route the whole synchronous body through the shared heavy-inference gate on the
    // blocking pool (the `unlock_folder` / `reindex_embeddings` precedent). Re-fetch `AppState`
    // inside the closure via `app.state()` — a bare `&AppState` cannot be captured by a `'static`
    // closure. Behavior is identical; it just no longer blocks the runtime thread.
    let heavy_inference = state.heavy_inference.clone();
    let app_for_edit = app.clone();
    let meeting_for_edit = meeting_id.clone();
    let markdown_for_edit = markdown.clone();
    let dto = crate::perf::run_heavy(&heavy_inference, move || -> Result<NoteDto, AppError> {
        let state = app_for_edit.state::<AppState>();
        update_note_inner(&state, &meeting_for_edit, &markdown_for_edit)
    })
    .await?;
    // If the edit re-published ≥1 org copy, ping open org views (Notes list + Settings) so the fresh
    // title/body shows without a manual "Sync now". Best-effort — a republish failure never fails the save.
    if republish_org_shares_for_source(state.inner(), Some(&meeting_id), None)
        .await
        .unwrap_or(0)
        > 0
    {
        crate::events::emit_org_feed_updated(&app, 1);
    }
    Ok(dto)
}

/// Export-collision guard — the ONE way a MEETING note's exported vault `.md` is FULLY
/// overwritten with DB-derived markdown. Before the overwrite, compares the CURRENT file bytes
/// against the stored `exported_hash` baseline (what Murmur last wrote): a mismatch means the user
/// (or their own vault-side agent, e.g. Claude Code over the vault) edited the file externally,
/// and the external version is preserved as a `<stem> (external edit …)` sibling instead of
/// silently destroyed. After the overwrite the baseline is re-stamped from the exact content
/// written. A NULL baseline (legacy row exported before the guard) is grandfathered — no sibling.
/// No PII in logs: meeting id + a boolean only, never the path (it embeds the note title).
pub(crate) fn overwrite_exported_note_guarded(
    state: &AppState,
    meeting_id: &str,
    provider_id: &str,
    path: &str,
    markdown: &str,
) -> Result<(), AppError> {
    let expected = state.db.get_note_exported_hash(meeting_id, provider_id)?;
    let path = std::path::Path::new(path);
    let sibling = crate::export::preserve_external_edit_if_any(path, expected.as_deref())?;
    crate::export::overwrite_note(path, markdown)?;
    state.db.set_note_exported_hash(
        meeting_id,
        provider_id,
        Some(&crate::export::note_content_hash(markdown)),
    )?;
    if sibling.is_some() {
        tracing::info!(
            target: "export",
            meeting_id = %meeting_id,
            sibling_created = true,
            "external vault edit preserved as a sibling before overwrite"
        );
    }
    Ok(())
}

/// Export-collision guard, APPEND side: after a read-modify-write of the CURRENT file (Re-Truth
/// stamps — which respect external edits by construction, so NO sibling is ever needed), re-stamp
/// the stored `exported_hash` baseline from the FINAL written content — but ONLY when the
/// PRE-APPEND bytes still matched the old baseline (or the row is legacy/NULL — grandfathered,
/// there is no signal to preserve). Re-stamping over a MISMATCH would LAUNDER an external edit
/// into the baseline: the next DB-derived full overwrite would see hash == baseline and destroy
/// the edit with no sibling (the adversarial MEDIUM). Keeping the stale baseline instead makes
/// that next overwrite preserve the whole file — external edit + appended callout — as a sibling.
/// Skipping the refresh in the CLEAN case would be the opposite bug (a false sibling out of
/// Murmur's own append), so the match check is what separates the two. Keyed on the LATEST
/// provider row — the same row `note_file_for` resolved the exported `.md` from.
pub(crate) fn refresh_meeting_note_exported_hash(
    state: &AppState,
    meeting_id: &str,
    pre_append: &str,
    written: &str,
) -> Result<(), AppError> {
    if let Some(latest) = state.db.get_latest_note_for_meeting(meeting_id)? {
        let baseline = state
            .db
            .get_note_exported_hash(meeting_id, &latest.provider_id)?;
        if let Some(b) = &baseline {
            if *b != crate::export::note_content_hash(pre_append) {
                // External edit present BEFORE the append — keep the stale baseline so the next
                // full overwrite preserves (edit + callout) as a sibling. Ids + boolean only.
                tracing::info!(
                    target: "export",
                    meeting_id = %meeting_id,
                    baseline_kept_stale = true,
                    "append over an externally-edited note — baseline deliberately not re-stamped"
                );
                return Ok(());
            }
        }
        state.db.set_note_exported_hash(
            meeting_id,
            &latest.provider_id,
            Some(&crate::export::note_content_hash(written)),
        )?;
    }
    Ok(())
}

// NOTE: the former `refresh_note_doc_exported_hash` (the authored-note twin of the meeting
// append-side refresh) was REMOVED with the note-accept canonical-store fix (2026-07-16): an
// authored note is never append-stamped on its exported file anymore — every note write goes
// through `update_note_doc_inner` → `write_note_to_vault`, which stamps `documents.exported_hash`
// fresh from the exact content it wrote (and `write_note` never clobbers different bytes, so an
// externally-edited exported file is preserved untouched rather than baseline-juggled).

/// Brain v3 (audit gap #1/#4) — BEST-EFFORT re-index of ONE meeting's note/transcript/topic chunks
/// after a content-affecting edit (note edit via [`update_note_inner`], rename via
/// [`rename_meeting`]). Without this, deleted note text / the old title kept surfacing in semantic
/// search + snippets (chunks are only re-derived by the pipeline, the manual Reindex, and unlock).
///
/// MODEL POLICY — the `embedder` is resolved by the CALLER (`embed_model_present().then(active_embedder)`)
/// and injected, exactly like [`reindex_meetings_after_unseal`]: `None` (model absent) writes NOTHING
/// (`index_meeting_chunks` has no chunk-only mode → any write would be a forbidden stub vector), and
/// injection keeps the model-present branch deterministically testable with the stub.
///
/// SEAL GATE (load-bearing): the re-index runs under the EMPTY unlock set, so
/// `index_meeting_if_enabled`'s visibility check admits ONLY a meeting whose folder is OPEN — a
/// SEALED folder's plaintext (even while session-unlocked) is never re-chunked by an edit. The
/// unlock/relock lifecycle owns those rows (`reindex_meetings_after_unseal` rebuilds on unlock;
/// `purge_chunks_tx` re-purges on relock), so the sealed-at-rest state never gains an index row here.
///
/// FLAG CONSISTENCY (Brain v3 audit gap, PR-1): the re-index respects `semantic_search_enabled`,
/// exactly like the pipeline's `should_auto_index` and the startup repair tick. When semantic is OFF
/// the `note_chunks`/`vec_chunks` are not retrieval-active (the semantic search paths short-circuit
/// on the flag), and raw FTS over the note/transcript stays fresh via its own triggers on the
/// plaintext write — so skipping the chunk re-index here is correct AND consistent (an edit/rename
/// must not be the ONE meeting-index path that ignores the flag).
///
/// BEST-EFFORT: a failure WARNs (ids only, no PII) and NEVER fails the save/rename — the plaintext
/// write already succeeded.
pub(crate) fn reindex_meeting_after_edit(
    state: &AppState,
    meeting_id: &str,
    embedder: Option<&dyn crate::embed::Embedder>,
) {
    let Some(embedder) = embedder else {
        return; // model absent → never a stub vector (mirrors the pipeline/reindex/unseal policy).
    };
    // Respect the master flag — an OFF flag means the vector chunks are not retrieval-active, so a
    // re-embed would be wasted work AND inconsistent with every other auto-index path.
    let enabled = state
        .config
        .lock()
        .map(|c| c.semantic_search_enabled)
        .unwrap_or(false);
    let empty = std::collections::HashSet::new();
    if let Err(e) =
        crate::pipeline::index_meeting_if_enabled(&state.db, meeting_id, enabled, &empty, embedder)
    {
        tracing::warn!(
            target: "rag",
            error = %e,
            "meeting re-index after edit failed (content saved unaffected)"
        );
    }
}

/// Inner of [`update_note`] taking `&AppState` — the FULL command body (gate + lifecycle guard +
/// seal-on-write upsert + vault re-write), so the seal-on-write regression binds the COMMAND
/// surface, not just the `upsert_note_reseal_if_locked` helper (residual W6). Resolves the
/// model-gated embedder and delegates to [`update_note_inner_with`] (embedder injected for
/// deterministic tests — the `reindex_meetings_after_unseal` precedent).
pub(crate) fn update_note_inner(
    state: &AppState,
    meeting_id: &str,
    markdown: &str,
) -> Result<NoteDto, AppError> {
    let embedder = crate::embed::embed_model_present().then(crate::embed::active_embedder);
    update_note_inner_with(state, meeting_id, markdown, embedder.as_deref())
}

/// Core of [`update_note_inner`] with the re-index embedder INJECTED (`None` = model absent →
/// no re-index; `Some` = re-derive the meeting's chunks/vectors after the save so deleted text
/// stops surfacing in semantic search — Brain v3 audit gap #1).
pub(crate) fn update_note_inner_with(
    state: &AppState,
    meeting_id: &str,
    markdown: &str,
    embedder: Option<&dyn crate::embed::Embedder>,
) -> Result<NoteDto, AppError> {
    // BLK-1 / TOCTOU (2026-07-10 audit F4): hold the lifecycle guard across gate+write so a
    // concurrent relock/seal cannot land between the unlock check and the upsert.
    let lifecycle = lifecycle_guard(state);
    // D4 READ/WRITE-GATE: refuse to mutate a sealed-and-not-session-unlocked meeting's note. Its
    // plaintext markdown is blanked while sealed, so an edit here would overwrite the (sealed)
    // content with the blanked value and corrupt it. Fail closed.
    if !meeting_is_unlocked(state, meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to edit the note".into(),
        ));
    }
    let existing = state
        .db
        .get_latest_note_for_meeting(meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;

    let created_at = chrono::Utc::now().to_rfc3339();
    // Seal-on-write (audit F1): an edit while the meeting's LOCKED folder is session-unlocked
    // re-seals the fresh markdown into `content_blob` — otherwise the relock would restore the
    // stale lock-time copy and destroy this edit. Open/rootless takes the plain upsert.
    upsert_note_reseal_if_locked(
        state,
        &NoteRecord {
            meeting_id: meeting_id.to_string(),
            provider_id: existing.provider_id.clone(),
            markdown: markdown.to_string(),
            created_at,
            exported_path: existing.exported_path.clone(),
            model_requested: existing.model_requested.clone(),
            model_served: existing.model_served.clone(),
            gateway_host: existing.gateway_host.clone(),
        },
    )?;

    if let Some(path) = existing.exported_path.as_deref() {
        overwrite_exported_note_guarded(state, meeting_id, &existing.provider_id, path, markdown)?;
    }

    // PERF (brain-v3 audit fix 2): release the GLOBAL lifecycle guard BEFORE the heavy leg — the
    // re-embed is multi-second Candle/Metal work on a long meeting, and holding the guard across
    // it delays screen-share auto-relock (`relock_all_inner`) and every lock/unlock op. Race-safe
    // without it: the indexers re-check the sealed-at-rest invariant INSIDE their write tx (a
    // mid-flight seal makes the write a refused no-op), and `rename_meeting_inner` already runs
    // the identical re-index with no guard. The fast leg above (gate + seal-on-write upsert +
    // guarded vault overwrite) stays under the guard.
    drop(lifecycle);

    // Brain v3 (audit gap #1): re-derive the meeting's chunks/vectors from the FRESH markdown so
    // deleted text stops surfacing in semantic search/snippets. Best-effort + empty-unlock-set
    // sealed-folder gate inside; never fails the save (the plaintext is already durable).
    reindex_meeting_after_edit(state, meeting_id, embedder);

    // Brain v3 PR-3 — LINK ENGINE: re-index this meeting note's `[[Title]]` wikilink edges (resolved
    // target ids) + refresh its semantic suggestions from the re-derived vectors (model-gated). Both
    // best-effort — a link failure never fails the note save. The wikilink pass runs regardless of
    // the embedder (deterministic); the semantic pass self-gates on `embed_model_present`.
    index_wikilinks_best_effort(state, crate::links::LinkKind::Meeting, meeting_id, markdown);
    auto_link_semantic_best_effort(state, crate::links::LinkKind::Meeting, meeting_id);

    Ok(NoteDto {
        meeting_id: meeting_id.to_string(),
        provider_id: existing.provider_id,
        markdown: markdown.to_string(),
        exported_path: existing.exported_path,
    })
}

/// VERIFY PASS (read-only): extract Jira issue keys from the meeting's note and check each against
/// LIVE Jira. GATED: sealed-not-unlocked meetings refuse (a verify against a blanked note would be
/// nonsense AND a read-gate bypass). Consent-gated: rides the Jira connector's enable+consent+key
/// gate (fail-closed `NeedsConsent` maps to `AppError::Unavailable`). NEVER called proactively —
/// FE-invoked only. Findings are computed against the note WITH OLD MARKERS STRIPPED so line
/// numbers line up with `apply_verify_markers`' post-strip numbering.
#[tauri::command]
pub async fn verify_note_sources(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<crate::verify::VerifyFinding>, AppError> {
    verify_note_sources_inner(state.inner(), meeting_id).await
}

pub(crate) async fn verify_note_sources_inner(
    state: &AppState,
    meeting_id: String,
) -> Result<Vec<crate::verify::VerifyFinding>, AppError> {
    if !meeting_is_unlocked(state, &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to verify the note".into(),
        ));
    }
    // Brain v2 L5 — SESSION verify cache, checked AFTER the read gate (the gate is never skipped
    // for a cache hit). A hit re-renders the panel without a second Jira egress; the cache is
    // RAM-only and cleared on relock_folder / relock_all, so it never outlives the session unlock.
    if let Some(cached) = state
        .verify_cache
        .lock()
        .map_err(|_| AppError::Other(anyhow::anyhow!("verify cache lock")))?
        .get(&meeting_id)
    {
        return Ok(cached.clone());
    }
    let note = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    // Strip our own old CALLOUT first (its body lines carry issue keys of their own), THEN the
    // inline markers, so extraction/judgment sees the canonical note lines and line numbers line
    // up with `apply_verify_markers`' post-strip numbering.
    let base = crate::verify::apply_verify_callout(&note.markdown, &[], "");
    let stripped = crate::verify::apply_verify_markers(&base, &[]);
    let keys = crate::verify::extract_issue_keys(&stripped);
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let config = state
        .config
        .lock()
        .map_err(|_| AppError::Other(anyhow::anyhow!("config lock")))?
        .clone();
    let registry = crate::connectors::ConnectorRegistry::build(&config);
    let lines: Vec<&str> = stripped.lines().collect();
    let mut findings = Vec::with_capacity(keys.len());
    for (line_no, key) in keys {
        let snap = registry.jira_lookup(&key).await.map_err(AppError::from)?;
        let line_text = lines.get(line_no - 1).copied().unwrap_or("");
        let (verdict, detail) = crate::verify::judge(line_text, &key, snap.as_ref());
        let url = snap.map(|s| s.url).unwrap_or_default();
        findings.push(crate::verify::VerifyFinding {
            line_no,
            key,
            verdict,
            detail,
            url,
        });
    }
    // Populate the session cache (RAM-only; cleared on relock). A poisoned lock only skips the
    // cache — the findings still return.
    if let Ok(mut cache) = state.verify_cache.lock() {
        cache.insert(meeting_id, findings.clone());
    }
    Ok(findings)
}

/// Apply verify markers to the note (WRITE — same gate + save/re-export tail as `update_note`).
/// Takes the findings the user just reviewed in the panel; validates every key's strict shape.
#[tauri::command]
pub fn apply_note_verify_markers(
    state: State<'_, AppState>,
    meeting_id: String,
    findings: Vec<crate::verify::VerifyFinding>,
) -> Result<NoteDto, AppError> {
    apply_note_verify_markers_inner(state.inner(), meeting_id, findings)
}

pub(crate) fn apply_note_verify_markers_inner(
    state: &AppState,
    meeting_id: String,
    findings: Vec<crate::verify::VerifyFinding>,
) -> Result<NoteDto, AppError> {
    // BLK-1 / TOCTOU (2026-07-10 audit F4): hold the lifecycle guard across gate+write so a
    // concurrent relock/seal cannot land between the unlock check and the upsert.
    let _lifecycle = lifecycle_guard(state);
    if !meeting_is_unlocked(state, &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to edit the note".into(),
        ));
    }
    for f in &findings {
        let ok = crate::verify::extract_issue_keys(&f.key)
            .first()
            .map(|(_, k)| k == &f.key)
            .unwrap_or(false);
        if !ok {
            return Err(AppError::InvalidArg("invalid issue key in findings".into()));
        }
    }
    let existing = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    // Brain v2 L5 — strip our own old CALLOUT first (so `apply_verify_markers`' internal
    // marker-strip numbering matches the findings, which were computed against the
    // callout-stripped note), apply the inline markers, then append the fresh consolidated
    // `> [!verify]-` callout dated now. All three regions are self-managed + idempotent.
    let base = crate::verify::apply_verify_callout(&existing.markdown, &[], "");
    let marked = crate::verify::apply_verify_markers(&base, &findings);
    let as_of = chrono::Utc::now().to_rfc3339();
    let marked = crate::verify::apply_verify_callout(&marked, &findings, &as_of);
    // Save + re-export — the exact `update_note` tail, with `marked`. Persisting via the
    // seal-on-write upsert keeps the callout in the CANONICAL DB note markdown so it SEALS with the
    // note under the folder lock (the enrich.rs persistence lesson — and, for a session-unlocked
    // LOCKED folder, the fresh markdown is re-sealed into `content_blob` in the same write); the
    // vault `.md` re-export follows when one exists.
    let created_at = chrono::Utc::now().to_rfc3339();
    upsert_note_reseal_if_locked(
        state,
        &NoteRecord {
            meeting_id: meeting_id.clone(),
            provider_id: existing.provider_id.clone(),
            markdown: marked.clone(),
            created_at,
            exported_path: existing.exported_path.clone(),
            model_requested: existing.model_requested.clone(),
            model_served: existing.model_served.clone(),
            gateway_host: existing.gateway_host.clone(),
        },
    )?;
    if let Some(path) = existing.exported_path.as_deref() {
        overwrite_exported_note_guarded(state, &meeting_id, &existing.provider_id, path, &marked)?;
    }
    Ok(NoteDto {
        meeting_id,
        provider_id: existing.provider_id,
        markdown: marked,
        exported_path: existing.exported_path,
    })
}

/// `enrich_note_context(meeting_id) -> Vec<ContextHit>` — CONNECTOR-AGNOSTIC preview of live context
/// to fold into the note. Read side: gathers hits from EVERY exposed (enabled + consented + keyed)
/// connector via the same registry the brain uses. Two modes (see the research brief):
/// - **Identifier lookup** (precise, minimal egress): Jira issue keys already in the note → live
///   `jira_lookup`. Only a validated `PROJ-123` leaves the Mac — never note content.
/// - **Free-text search** (fuzzy): every OTHER exposed connector (Slack/web) is searched for the
///   meeting's TITLE, through the framework's redaction + content-free egress ledger.
///
/// This is the EGRESS moment (an explicit user action, like `verify_note_sources`); the returned
/// hits are reviewed in the FE and only WRITTEN by `apply_note_enrichment`. Lock-gated: a
/// sealed-not-unlocked meeting refuses BEFORE any connector call. Empty vec = nothing to add.
#[tauri::command]
pub async fn enrich_note_context(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<crate::enrich::ContextHit>, AppError> {
    enrich_note_context_inner(state.inner(), meeting_id).await
}

/// How many free-text search hits to keep per connector (bounds egress-result noise; the caller
/// still reviews + can drop each before applying).
const ENRICH_SEARCH_HITS_PER_CONNECTOR: usize = 3;

pub(crate) async fn enrich_note_context_inner(
    state: &AppState,
    meeting_id: String,
) -> Result<Vec<crate::enrich::ContextHit>, AppError> {
    // READ-GATE FIRST — a sealed-not-unlocked meeting refuses before ANY connector egress.
    if !meeting_is_unlocked(state, &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to add live context".into(),
        ));
    }
    let note = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    // Clean base = the note with our own prior context block stripped, so key-extraction sees the
    // canonical prose (never our appended callout).
    let base = crate::enrich::apply_context_markers(&note.markdown, &[], "");
    let title = state
        .db
        .get_meeting(&meeting_id)?
        .and_then(|m| m.title)
        .unwrap_or_default();

    let config = state
        .config
        .lock()
        .map_err(|_| AppError::Other(anyhow::anyhow!("config lock")))?
        .clone();
    let registry = crate::connectors::ConnectorRegistry::build(&config);

    let mut hits: Vec<crate::enrich::ContextHit> = Vec::new();

    // ── Identifier-lookup mode (precise): Jira issue keys → live status. Egresses only the key. ──
    if registry.has("jira") {
        for (_line, key) in crate::verify::extract_issue_keys(&base) {
            if let Ok(Some(snap)) = registry.jira_lookup(&key).await {
                let mut detail = format!("{} · {}", snap.key, snap.status);
                if let Some(due) = snap.due.as_deref().filter(|d| !d.is_empty()) {
                    detail.push_str(&format!(" · due {due}"));
                }
                if !snap.summary.is_empty() {
                    detail.push_str(&format!(" — {}", snap.summary));
                }
                hits.push(crate::enrich::ContextHit {
                    source: "Jira".to_string(),
                    detail,
                    url: Some(snap.url).filter(|u| !u.is_empty()),
                });
            }
        }
    }

    // ── Free-text search mode (fuzzy): every OTHER exposed connector, queried on the meeting title.
    // The query is redacted + ledgered by the registry; skip when there is no title to search on. ──
    if !title.trim().is_empty() {
        for id in registry.ids() {
            if id == "jira" {
                continue; // handled precisely above — never double-pull Jira.
            }
            if let Ok(results) = registry.search(id, &title).await {
                for hit in results.into_iter().take(ENRICH_SEARCH_HITS_PER_CONNECTOR) {
                    let detail = if hit.snippet.trim().is_empty() {
                        hit.title
                    } else {
                        format!("{} — {}", hit.title, hit.snippet)
                    };
                    hits.push(crate::enrich::ContextHit {
                        // Loud attribution from the connector itself (e.g. "Slack", "web · Brave").
                        source: hit.source_label,
                        detail,
                        url: Some(hit.url).filter(|u| !u.is_empty()),
                    });
                }
            }
        }
    }

    Ok(hits)
}

/// `apply_note_enrichment(meeting_id, hits) -> NoteDto` — WRITE the reviewed context hits into the
/// note as one consolidated `> [!context]-` callout (dated now), via the EXACT `update_note` save +
/// re-export tail — so it persists in the CANONICAL DB note markdown and SEALS with the note under
/// the folder lock (NOT the vault-file-only path). No egress here (the hits were fetched by
/// `enrich_note_context`). Lock-gated. Passing an empty `hits` STRIPS the block (byte-exact undo).
#[tauri::command]
pub fn apply_note_enrichment(
    state: State<'_, AppState>,
    meeting_id: String,
    hits: Vec<crate::enrich::ContextHit>,
) -> Result<NoteDto, AppError> {
    apply_note_enrichment_inner(state.inner(), meeting_id, hits)
}

pub(crate) fn apply_note_enrichment_inner(
    state: &AppState,
    meeting_id: String,
    hits: Vec<crate::enrich::ContextHit>,
) -> Result<NoteDto, AppError> {
    // BLK-1 / TOCTOU: hold the lifecycle guard across the whole check-then-write so a concurrent seal
    // cannot slip between the gate and the `upsert_note`+`overwrite_note` (same leak class Lane A's
    // `link_related_notes_inner` guards). Lock order `lifecycle ⊃ db`.
    let _lifecycle = lifecycle_guard(state);
    if !meeting_is_unlocked(state, &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to edit the note".into(),
        ));
    }
    // SEAL-SAFETY GATE (mirrors Lane A): a SEALED note (any provider row carries a content_blob) has a
    // TRANSIENT `markdown` column — blanked on relock, restored from `content_blob` on unlock. Writing
    // enriched markdown into it would be silently dropped on the next relock (content_blob is
    // canonical), and — for the auto-file-into-locked case where the column is blank but the folder is
    // session-unlocked — could re-materialize plaintext into a sealed note. So refuse enrichment on a
    // sealed note even when the session has it unlocked.
    let sealed = state
        .db
        .sealable_notes_for_meeting(&meeting_id)?
        .iter()
        .any(|n| n.content_blob.is_some());
    if sealed {
        return Err(AppError::Locked(
            "this meeting's note is sealed — enrichment can't be persisted while locked".into(),
        ));
    }
    let existing = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    let as_of = chrono::Utc::now().to_rfc3339();
    let enriched = crate::enrich::apply_context_markers(&existing.markdown, &hits, &as_of);
    let created_at = chrono::Utc::now().to_rfc3339();
    // Seal-on-write seam (audit F1): the sealed case was refused above, but a LOCKED folder whose
    // note was never sealed (auto-filed while session-unlocked) still re-seals the fresh markdown.
    upsert_note_reseal_if_locked(
        state,
        &NoteRecord {
            meeting_id: meeting_id.clone(),
            provider_id: existing.provider_id.clone(),
            markdown: enriched.clone(),
            created_at,
            exported_path: existing.exported_path.clone(),
            model_requested: existing.model_requested.clone(),
            model_served: existing.model_served.clone(),
            gateway_host: existing.gateway_host.clone(),
        },
    )?;
    if let Some(path) = existing.exported_path.as_deref() {
        overwrite_exported_note_guarded(state, &meeting_id, &existing.provider_id, path, &enriched)?;
    }
    Ok(NoteDto {
        meeting_id,
        provider_id: existing.provider_id,
        markdown: enriched,
        exported_path: existing.exported_path,
    })
}

/// Max cross-meeting links Lane A appends to a note (small + high-precision — a note gains a handful
/// of related links, not a research dump).
const MAX_RELATED_LINKS: usize = 4;

/// Stage 2 / Lane A — the DETERMINISTIC, ZERO-EGRESS cross-meeting LINKING pass over a FINISHED note.
///
/// Mirrors [`apply_note_enrichment_inner`] (the shipped Lane B persist seam): gate → retrieve →
/// render → `upsert_note` (DB-canonical, so the links SEAL with the note) → re-export the vault
/// `.md`. Retrieval is [`related_context::related_note_links`], which is DOUBLE visibility-gated
/// (`search_visible` + `get_note_if_visible` on the live unlock set) and self-excluding — a
/// sealed-not-unlocked related note contributes NO link. Empty hits STRIP any stale links block
/// (byte-exact undo), so re-running self-heals the link graph. Idempotent + reversible via
/// `apply_link_markers`.
///
/// EGRESS: NONE. Lane A is fully local — a search over OWNED notes plus a local task-free gist. No
/// provider, no connector, no consent gate needed (nothing leaves the device).
///
/// SEAL-SAFETY (stronger than the read gate, load-bearing): after the `meeting_is_unlocked` read
/// gate, we ALSO require the note to be genuinely UNSEALED (`content_blob IS NULL` on every provider
/// row). A note in a locked folder is SEALED: its `markdown` column is transient (blanked on relock,
/// restored from `content_blob` on unlock) and its durable source of truth is `content_blob`.
/// Writing links into a sealed note's `markdown` column would either corrupt a just-sealed (blanked)
/// column (the auto-file-into-locked case, where the column is empty but the folder is session-
/// unlocked) or be silently dropped on the next relock (the column is discarded, `content_blob` is
/// canonical). So a sealed meeting is skipped even when the session happens to have it unlocked —
/// which is exactly what makes "persist DB-canonical so it SEALS with the note" true here.
pub(crate) fn link_related_notes_inner(state: &AppState, meeting_id: &str) -> Result<(), AppError> {
    // BLK-1 / TOCTOU: hold the lifecycle guard for the WHOLE check-then-write so a concurrent
    // `lock_folder`/`move_into_locked_folder`/`relock` cannot seal this meeting BETWEEN the seal-safety
    // gate below and the `upsert_note`+`overwrite_note` — which would re-materialize a plaintext `.md`
    // into a now-locked folder's vault dir (the Phase-2 lock-security TOCTOU leak). Every seal path
    // holds this same guard (`lock_folder_inner`/`move_into_locked_folder`/`relock_all_inner`/
    // `remove_lock_inner`), and the storage-prune was forced to take it by a prior TOCTOU finding.
    // Lock order is `lifecycle ⊃ db` (matching `lock_folder_inner`).
    let _lifecycle = lifecycle_guard(state);
    // READ GATE: a sealed-not-session-unlocked meeting is silently skipped — never link a locked note.
    if !meeting_is_unlocked(state, meeting_id)? {
        return Ok(());
    }
    // SEAL-SAFETY GATE: only write the markdown COLUMN of an UNSEALED note (durable canonical). A
    // sealed note (any provider row has a content_blob) is skipped even if session-unlocked. Read
    // UNDER the lifecycle guard, so a seal cannot slip in after this check and before the write.
    let sealed = state
        .db
        .sealable_notes_for_meeting(meeting_id)?
        .iter()
        .any(|n| n.content_blob.is_some());
    if sealed {
        return Ok(());
    }
    let Some(existing) = state.db.get_latest_note_for_meeting(meeting_id)? else {
        return Ok(()); // no note yet → nothing to link.
    };
    let title = state
        .db
        .get_meeting(meeting_id)?
        .and_then(|m| m.title)
        .unwrap_or_default();
    // Derive the salient query from the CANONICAL prose — strip our OWN links block first so a
    // re-link never keys the query off a previous run's `[[Title]]` links (stable / self-healing).
    let base = crate::enrich::apply_link_markers(&existing.markdown, &[]);
    let query = crate::summarize::related_context::salient_query(
        (!title.trim().is_empty()).then_some(title.as_str()),
        &base,
    );
    // Snapshot the live unlocked set (the SAME gate the retrieval keys on).
    let unlocked = {
        state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
            .clone()
    };
    let hits = crate::summarize::related_context::related_note_links(
        &state.db,
        meeting_id,
        &query,
        &unlocked,
        MAX_RELATED_LINKS,
    )?;
    // Empty hits ⇒ `apply_link_markers` strips any stale links block (byte-exact). Non-empty ⇒ replace.
    // No `as_of`: cross-meeting links are timeless (owned notes), so the block is a pure function of
    // (note, hits) → the `linked == existing.markdown` short-circuit below skips a rewrite when the
    // link set is unchanged, so the deferred auto-pass never churns the note / vault `.md`.
    let linked = crate::enrich::apply_link_markers(&existing.markdown, &hits);
    // No change (no hits AND no stale block) ⇒ nothing to persist; avoid a needless write + re-export.
    if linked == existing.markdown {
        return Ok(());
    }
    let created_at = chrono::Utc::now().to_rfc3339();
    // Seal-on-write seam (audit F1): the sealed case was refused above, but a LOCKED folder whose
    // note was never sealed (auto-filed while session-unlocked) still re-seals the fresh markdown.
    upsert_note_reseal_if_locked(
        state,
        &NoteRecord {
            meeting_id: meeting_id.to_string(),
            provider_id: existing.provider_id.clone(),
            markdown: linked.clone(),
            created_at,
            exported_path: existing.exported_path.clone(),
            model_requested: existing.model_requested.clone(),
            model_served: existing.model_served.clone(),
            gateway_host: existing.gateway_host.clone(),
        },
    )?;
    if let Some(path) = existing.exported_path.as_deref() {
        overwrite_exported_note_guarded(state, meeting_id, &existing.provider_id, path, &linked)?;
    }
    Ok(())
}

/// `link_related_notes(meeting_id)` — MANUAL re-link / backfill trigger for the Stage 2 / Lane A
/// pass. The AUTO pipeline runs the same [`link_related_notes_inner`] as a deferred post-`Exported`
/// pass; this command lets the user (or a backfill over old notes) re-run it on demand. Lock-gated +
/// seal-safe (a sealed meeting is a silent no-op) and ZERO egress.
#[tauri::command]
pub fn link_related_notes(state: State<'_, AppState>, meeting_id: String) -> Result<(), AppError> {
    link_related_notes_inner(state.inner(), &meeting_id)
}

/// brain2 realtime typed @brain notes — persist the user's free-text notes typed DURING a meeting
/// (the FE autosaves the whole buffer here). The buffer (a) feeds the in-meeting brain's system
/// prompt while recording and (b) is folded into the finalized note at summarize time.
///
/// READ/WRITE-GATE: refuse to write a sealed-and-not-session-unlocked meeting's buffer
/// (`AppError::Locked`). Its content is blanked while sealed, and an ungated write would resurrect
/// typed plaintext at rest behind the lock. Fail closed — mirrors `update_note`'s D4 gate.
#[tauri::command]
pub fn save_manual_notes(
    state: State<'_, AppState>,
    meeting_id: String,
    text: String,
) -> Result<(), AppError> {
    save_manual_notes_inner(state.inner(), &meeting_id, &text)
}

/// Inner of [`save_manual_notes`] taking `&AppState` (so the gate is unit-testable without a
/// `tauri::State`). GATED: a sealed-and-not-session-unlocked meeting is refused with
/// `AppError::Locked` (mirrors `update_note`'s D4 write-gate — never resurrect typed plaintext
/// behind a lock).
pub(crate) fn save_manual_notes_inner(
    state: &AppState,
    meeting_id: &str,
    text: &str,
) -> Result<(), AppError> {
    // BLK-1 / TOCTOU (2026-07-10 audit F4): hold the lifecycle guard across gate+write so a
    // concurrent relock/seal cannot land between the unlock check and the buffer write.
    let _lifecycle = lifecycle_guard(state);
    if !meeting_is_unlocked(state, meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to edit your notes".into(),
        ));
    }
    // Seal-on-write (audit F1): a session-unlocked LOCKED folder re-seals the fresh buffer into
    // `manual_notes_blob` in the same write; open/rootless takes the plain update. Fail-closed on a
    // missing session KEK.
    set_manual_notes_reseal_if_locked(state, meeting_id, text)?;
    // PII rule: log only the meeting id + buffer length, never the typed text.
    tracing::debug!(target: "notes", meeting_id = %meeting_id, len = text.len(), "manual notes saved");
    Ok(())
}

// ── Recording-time COMPANION NOTE (2026-07-16) ───────────────────────────────────────────────────
//
// During a recording, a jotted note must become a REAL, linked, standalone note — one living
// companion note per meeting (a `documents` row, kind='note', in the always-open Notes ROOT). The
// companion note is authored content only (what the user typed / accepted @brain drafts) — NEVER
// the sealed transcript — so it lives in the open root and is not itself a seal target. The link is
// TWO artifacts derived from ONE structured relation: the authoritative `documents.meeting_id`
// column, and a user-visible YAML front-matter `meeting: "[[<name>]]"` wikilink kept in sync.

/// The stable display NAME for a meeting used in the companion note's `[[<name>]]` wikilink + managed
/// title. `meetings.title` when non-empty (Task 4 gives every in-progress meeting a provisional
/// title at record start, upgraded by auto-title-on-close), else a safe fallback ("Untitled meeting")
/// so the link is never `[[]]`. PURE — the caller supplies the current title.
fn meeting_display_name(title: Option<&str>) -> String {
    match title.map(str::trim) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => "Untitled meeting".to_string(),
    }
}

/// A stable, human PROVISIONAL title for a meeting at record start ("Meeting 2026-07-16 14:05") from
/// the local start time. PURE (the caller supplies `now`) so it is unit-testable without racing the
/// clock. Meaningful immediately (so `[[<name>]]` links work during the recording) and stable (never
/// changes mid-recording); auto-title-on-close upgrades it to a content-derived headline.
fn provisional_meeting_title<Tz: chrono::TimeZone>(now: &chrono::DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    format!("Meeting {}", now.format("%Y-%m-%d %H:%M"))
}

/// Result of [`append_to_companion_note`] — the companion note's id (for opening it) and the
/// user-visible `[[<meeting display name>]]` wikilink the FE renders on the saved-note card.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanionAppendResult {
    pub note_id: String,
    pub meeting_wikilink: String,
}

/// IDEMPOTENTLY ensure the note markdown's YAML front-matter carries `meeting: "[[<name>]]"`, then
/// APPEND `block` to the body with a blank-line separator. PURE (no DB / no state) so the composition
/// is unit-testable in isolation. Never blanks existing content: the prior body + prior front-matter
/// keys are preserved; only the single managed `meeting:` key is (re)written to the current name, and
/// the new block is added after the existing body.
///
/// - No front-matter yet → a fresh `---\nmeeting: "[[name]]"\n---` block is prepended.
/// - Front-matter present, no `meeting:` key → the key is inserted (other keys untouched).
/// - Front-matter present WITH a `meeting:` key → that line is rewritten to the current name
///   (keeps the link correct across a rename); no duplicate key is ever added.
fn compose_companion_markdown(current: &str, meeting_name: &str, block: &str) -> String {
    let link_value = format!("\"[[{meeting_name}]]\"");
    let (yaml, body) = crate::storage::db::split_front_matter(current);

    // Rebuild the front-matter lines: rewrite an existing top-level `meeting:` key, else append it.
    let mut fm_lines: Vec<String> = Vec::new();
    let mut wrote_meeting = false;
    if !yaml.is_empty() {
        for raw in yaml.lines() {
            // A top-level (unindented) `meeting:` key is the managed link line — rewrite it.
            let is_meeting_key = !raw.starts_with(char::is_whitespace)
                && raw
                    .split_once(':')
                    .map(|(k, _)| k.trim().eq_ignore_ascii_case("meeting"))
                    .unwrap_or(false);
            if is_meeting_key {
                fm_lines.push(format!("meeting: {link_value}"));
                wrote_meeting = true;
            } else {
                fm_lines.push(raw.to_string());
            }
        }
    }
    if !wrote_meeting {
        fm_lines.push(format!("meeting: {link_value}"));
    }
    let front_matter = format!("---\n{}\n---", fm_lines.join("\n"));

    // Append the block to the body with a blank-line separator (never overwrite prior body).
    let body_trimmed = body.trim_end();
    let block_trimmed = block.trim();
    let new_body = if body_trimmed.is_empty() {
        block_trimmed.to_string()
    } else if block_trimmed.is_empty() {
        body_trimmed.to_string()
    } else {
        format!("{body_trimmed}\n\n{block_trimmed}")
    };

    if new_body.is_empty() {
        format!("{front_matter}\n")
    } else {
        format!("{front_matter}\n\n{new_body}\n")
    }
}

/// `get_or_create_companion_note(meeting_id)` — the DOCUMENT-FIRST entry point (v2 redesign): EAGERLY
/// get-or-create the ONE companion note for `meeting_id` and return `{ note_id, meeting_wikilink }`
/// WITHOUT appending any body, so the recording "Note" tab can mount the real create-note editor on a
/// stable note id. GATED: refuses a sealed-and-not-session-unlocked meeting (`AppError::Locked`).
/// Idempotent — a second call reuses the same note (one-note-per-meeting invariant). No `manual_notes`
/// write (that mirror belongs to the append path; the editor autosaves via the normal note path).
#[tauri::command]
pub fn get_or_create_companion_note(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<CompanionAppendResult, AppError> {
    get_or_create_companion_note_inner(state.inner(), &meeting_id)
}

/// Inner of [`get_or_create_companion_note`] taking `&AppState` (unit-testable gate + lazy-create).
/// Factored OUT of [`append_to_companion_note_inner`] so both the document-first editor mount and the
/// append path share ONE lazy get-or-create (gate → `companion_note_for_meeting` → else
/// `create_note_inner(None, meeting_name)` + `set_document_meeting_id` + front-matter `[[Meeting]]`
/// link). Returns the note id + the display wikilink; writes NO body.
pub(crate) fn get_or_create_companion_note_inner(
    state: &AppState,
    meeting_id: &str,
) -> Result<CompanionAppendResult, AppError> {
    if meeting_id.trim().is_empty() {
        return Err(AppError::Locked(
            "no meeting is being recorded — there is nothing to attach the note to".into(),
        ));
    }
    // GATE: refuse a sealed-and-not-session-unlocked meeting — never resurrect content behind a lock.
    if !meeting_is_unlocked(state, meeting_id)? {
        return Err(AppError::Locked(
            "this meeting is locked — unlock it to save a note".into(),
        ));
    }

    // The meeting's current display title drives the managed note title + the `[[<name>]]` link.
    let Some(meeting) = state.db.get_meeting(meeting_id)? else {
        return Err(AppError::InvalidArg(format!("no meeting {meeting_id}")));
    };
    let meeting_name = meeting_display_name(meeting.title.as_deref());
    let meeting_wikilink = format!("[[{meeting_name}]]");

    // GET-OR-CREATE the companion note in the always-open Notes ROOT (folder_id = None). A new
    // companion note is birthed with the managed title = the meeting name, structurally linked by
    // `meeting_id`, and its front-matter `[[Meeting]]` link stamped (empty-block compose — no body
    // is written). `create_note_inner`/`update_note_doc_inner` each hold their own lifecycle guard.
    let note_id = match state.db.companion_note_for_meeting(meeting_id)? {
        Some(id) => id,
        None => {
            let id = create_note_inner(state, None, &meeting_name)?;
            state.db.set_document_meeting_id(&id, meeting_id)?;
            // Brain v3 PR-3 — record the structured `companion` edge (note → meeting) alongside the
            // `documents.meeting_id` column, so the link graph carries it beyond the migrate backfill.
            if let Err(e) = state.db.set_companion_link(&id, meeting_id) {
                tracing::warn!(target: "links", error = %e, "companion link edge failed (note linked)");
            }
            // Stamp the front-matter `meeting: "[[…]]"` link on the fresh (empty) note so the
            // document-first editor mounts on a note that already carries the link (no body added).
            if let Some(row) = state.db.get_note_row(&id)? {
                let new_markdown = compose_companion_markdown(&row.text, &meeting_name, "");
                update_note_doc_inner(state, &id, &meeting_name, &new_markdown)?;
            }
            tracing::info!(target: "notes", note_id = %id, meeting_id = %meeting_id, "companion note created");
            id
        }
    };

    Ok(CompanionAppendResult {
        note_id,
        meeting_wikilink,
    })
}

/// `append_to_companion_note(meeting_id, markdown)` — turn an in-recording jot (or an accepted
/// `@brain` draft) into a REAL, linked, standalone companion note. GATED: refuses a
/// sealed-and-not-session-unlocked meeting (`AppError::Locked`) — mirrors the `save_note` tool's
/// gate. Lazily gets-or-creates the ONE companion note (Notes ROOT, `meeting_id` set, managed title =
/// the meeting's display name), APPENDS the block atomically under the lifecycle guard, refreshes the
/// front-matter `[[Meeting]]` link, re-indexes + re-exports through the guarded standalone-note path,
/// AND additively appends the same block to `manual_notes` (so the enhance/summary pipeline keeps
/// seeing in-meeting notes — mirrors `tools::save_note`). Returns the note id + the display wikilink
/// for the confirmation card. Never blanks prior content on any failure.
#[tauri::command]
pub fn append_to_companion_note(
    state: State<'_, AppState>,
    meeting_id: String,
    markdown: String,
) -> Result<CompanionAppendResult, AppError> {
    append_to_companion_note_inner(state.inner(), &meeting_id, &markdown)
}

/// Inner of [`append_to_companion_note`] taking `&AppState` (unit-testable gate + lazy-create).
pub(crate) fn append_to_companion_note_inner(
    state: &AppState,
    meeting_id: &str,
    markdown: &str,
) -> Result<CompanionAppendResult, AppError> {
    let block = markdown.trim();
    if block.is_empty() {
        return Err(AppError::InvalidArg("nothing to note".into()));
    }
    // BLK-1 / TOCTOU: `get_or_create_companion_note_inner` + `create_note_inner` /
    // `update_note_doc_inner` each take the lifecycle guard under their own scope (reentrant
    // `MutexGuard` is NOT allowed) — so we do NOT hold it here; instead the gate is re-checked
    // inside each helper's own guard (defense-in-depth). The shared get-or-create runs the
    // `meeting_is_unlocked` gate FIRST and lazily births the note (Notes ROOT, `meeting_id` set,
    // managed title = the meeting name).
    let CompanionAppendResult {
        note_id,
        meeting_wikilink,
    } = get_or_create_companion_note_inner(state, meeting_id)?;
    let meeting_name = meeting_wikilink
        .trim_start_matches("[[")
        .trim_end_matches("]]")
        .to_string();

    // APPEND the block: read the current body, compose (front-matter link refreshed idempotently +
    // block appended), and save through the standalone-note update path (re-index + guarded
    // re-export). Read the CURRENT text via the row (the note is in the open root, so no mask). The
    // managed title stays the meeting name — never blank it, never a user-facing title edit.
    let Some(row) = state.db.get_note_row(&note_id)? else {
        return Err(AppError::InvalidArg(format!("no note {note_id}")));
    };
    let new_markdown = compose_companion_markdown(&row.text, &meeting_name, block);
    // `update_note_doc_inner` re-checks the folder gate under its own lifecycle guard, re-indexes,
    // and re-exports through the guarded standalone-note path (external-edit preservation intact).
    update_note_doc_inner(state, &note_id, &meeting_name, &new_markdown)?;

    // ADDITIVELY append the SAME block to `manual_notes` so the enhance/summary pipeline keeps
    // seeing in-meeting notes — mirrors `tools::save_note` (append, never overwrite; reseal-aware).
    let existing = state.db.get_manual_notes(meeting_id).unwrap_or_default();
    let merged = if existing.trim().is_empty() {
        block.to_string()
    } else {
        format!("{existing}\n{block}")
    };
    set_manual_notes_reseal_if_locked(state, meeting_id, &merged)?;

    // PII rule: ids + block length only — never the note text or the meeting title.
    tracing::info!(target: "notes", note_id = %note_id, meeting_id = %meeting_id, len = block.len(), "appended to companion note");
    Ok(CompanionAppendResult {
        note_id,
        meeting_wikilink,
    })
}

/// TRUE iff the note markdown's body (YAML front-matter stripped) is whitespace-only — i.e. the
/// document-first "Note" tab mounted a companion note that the user never wrote into. Pure + Db-free.
/// The front-matter carries only the managed `meeting: "[[…]]"` link (never user content), so a
/// body-empty companion note is content-free and safe to remove.
fn companion_body_is_empty(markdown: &str) -> bool {
    let (_yaml, body) = crate::storage::db::split_front_matter(markdown);
    body.trim().is_empty()
}

/// `delete_companion_note_if_empty(meeting_id) -> bool` — remove an UNUSED auto-created companion note
/// so a recording the user never jotted into leaves NO clutter. Deletes IFF the companion note's body
/// (YAML front-matter stripped) is whitespace-only; a note with ANY user content is KEPT untouched.
/// GATED: refuses a sealed-and-not-session-unlocked meeting (`AppError::Locked`) — never touch content
/// behind a lock. Returns `true` when a body-empty companion note was deleted, `false` otherwise
/// (no companion note, or it had content). NO-LOSS: only ever deletes a body-empty note. `async`
/// because [`delete_note_inner`] runs the org-share revoke cascade.
#[tauri::command]
pub async fn delete_companion_note_if_empty(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<bool, AppError> {
    delete_companion_note_if_empty_inner(state.inner(), &meeting_id).await
}

/// Inner of [`delete_companion_note_if_empty`] taking `&AppState` (unit-testable gate).
pub(crate) async fn delete_companion_note_if_empty_inner(
    state: &AppState,
    meeting_id: &str,
) -> Result<bool, AppError> {
    if meeting_id.trim().is_empty() {
        return Ok(false); // nothing recording → nothing to clean up.
    }
    // GATE: refuse a sealed-and-not-session-unlocked meeting — never touch content behind a lock.
    if !meeting_is_unlocked(state, meeting_id)? {
        return Err(AppError::Locked(
            "this meeting is locked — unlock it before cleaning up its note".into(),
        ));
    }
    let Some(note_id) = state.db.companion_note_for_meeting(meeting_id)? else {
        return Ok(false); // no companion note was ever created.
    };
    let Some(row) = state.db.get_note_row(&note_id)? else {
        return Ok(false); // race: note vanished → nothing to do.
    };
    // NO-LOSS: only ever delete a note whose BODY is whitespace-only. Any user content ⇒ KEEP it.
    if !companion_body_is_empty(&row.text) {
        return Ok(false);
    }
    delete_note_inner(state, &note_id).await?;
    tracing::info!(target: "notes", note_id = %note_id, meeting_id = %meeting_id, "empty companion note deleted");
    Ok(true)
}

/// Best-effort: refresh the COMPANION note's managed title + its front-matter `meeting: "[[…]]"`
/// wikilink so the link/label stays correct when a meeting is (auto-)titled or renamed. A sync
/// failure NEVER fails the rename (the meeting title is already persisted). No-op when the meeting
/// has no companion note. Skips when the companion note's folder is sealed-not-unlocked (never write
/// plaintext behind a lock — the sync re-applies on the next unlock+append).
pub(crate) fn sync_companion_note_title_best_effort(state: &AppState, meeting_id: &str) {
    if let Err(e) = sync_companion_note_title(state, meeting_id) {
        // ids only — never the title text.
        tracing::warn!(target: "notes", meeting_id = %meeting_id, error = %e, "companion note title sync failed (meeting title unaffected)");
    }
}

fn sync_companion_note_title(state: &AppState, meeting_id: &str) -> Result<(), AppError> {
    let Some(note_id) = state.db.companion_note_for_meeting(meeting_id)? else {
        return Ok(()); // no companion note — nothing to sync.
    };
    let Some(meeting) = state.db.get_meeting(meeting_id)? else {
        return Ok(());
    };
    let meeting_name = meeting_display_name(meeting.title.as_deref());
    let Some(row) = state.db.get_note_row(&note_id)? else {
        return Ok(());
    };
    // Skip a sealed-not-unlocked companion note (its plaintext is blanked; unlocking + a later
    // append re-applies the correct title/link). `update_note_doc_inner` would refuse anyway.
    if !folder_is_unlocked(state, &row.folder_id)? {
        return Ok(());
    }
    // Nothing to do if the title AND the front-matter link already match (idempotent).
    let (yaml, _body) = crate::storage::db::split_front_matter(&row.text);
    let link_ok = yaml.lines().any(|l| {
        !l.starts_with(char::is_whitespace)
            && l.split_once(':')
                .map(|(k, v)| {
                    k.trim().eq_ignore_ascii_case("meeting")
                        && v.contains(&format!("[[{meeting_name}]]"))
                })
                .unwrap_or(false)
    });
    let title_ok = row
        .title
        .as_deref()
        .map(str::trim)
        .map(|t| t == meeting_name)
        .unwrap_or(false);
    if link_ok && title_ok {
        return Ok(());
    }
    // Re-write the managed title + refresh the `meeting:` front-matter line; body is UNCHANGED
    // (empty block append). Routes through the guarded update path (re-index + re-export).
    let new_markdown = compose_companion_markdown(&row.text, &meeting_name, "");
    update_note_doc_inner(state, &note_id, &meeting_name, &new_markdown)?;
    tracing::info!(target: "notes", note_id = %note_id, meeting_id = %meeting_id, "companion note title/link synced");
    Ok(())
}

/// brain2 realtime typed @brain notes — read the meeting's live typed-notes buffer (the FE rehydrates
/// the editor from this). GATED by `meeting_is_unlocked`: a sealed-and-not-session-unlocked meeting
/// returns "" (mask — never leak the buffer), exactly like the masked detail DTO drops note/segments.
#[tauri::command]
pub fn get_manual_notes(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<String, AppError> {
    get_manual_notes_inner(state.inner(), &meeting_id)
}

/// Inner of [`get_manual_notes`] taking `&AppState` (unit-testable gate). A sealed-and-not-session-
/// unlocked meeting returns "" (masked) — its buffer is never surfaced.
pub(crate) fn get_manual_notes_inner(
    state: &AppState,
    meeting_id: &str,
) -> Result<String, AppError> {
    if !meeting_is_unlocked(state, meeting_id)? {
        return Ok(String::new()); // sealed-not-unlocked ⇒ masked, never the stored buffer.
    }
    state.db.get_manual_notes(meeting_id)
}

/// Brain v3 PR-5 (Receipts): the audio receipt for every claim in a meeting's CURRENT note — the
/// transcript segment each note line most likely derives from, so the FE can seek the shipped audio
/// player/timeline to that second and prove the claim (speaker + ASR confidence in the tooltip).
///
/// Deterministic + on-device (extends `summarize::grounding`'s token-overlap pass — no LLM, no
/// egress). Recomputed on demand from the current note + segments, so there is NO new storage and
/// therefore NO new seal path (nothing at rest to encrypt/verify-before-destroy).
///
/// LOCK-MODEL (audio-adjacent read): the `meeting_is_unlocked` gate is FIRST. A sealed-and-NOT-
/// session-unlocked meeting returns an EMPTY list — never a segment time, speaker, or overlap that
/// would leak WHEN something was said or by WHOM, matching the masked DTO that already nulls
/// `audio_path`. Past the gate we read the note + segments through the SAME gated readers the detail
/// DTO uses (`get_latest_note_for_meeting` / `get_segments`), align, and return alignments only —
/// the DTO carries no note/transcript text (just a line index + the segment's audio coordinates +
/// non-content metadata). No PII in logs (id + count only).
#[tauri::command]
pub fn get_note_receipts(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<crate::summarize::grounding::ClaimAlignment>, AppError> {
    get_note_receipts_inner(state.inner(), &meeting_id)
}

/// Inner of [`get_note_receipts`] taking `&AppState` (unit-testable gate). Empty list for a
/// sealed-not-unlocked meeting; empty list when there is no note yet (nothing to receipt).
pub(crate) fn get_note_receipts_inner(
    state: &AppState,
    meeting_id: &str,
) -> Result<Vec<crate::summarize::grounding::ClaimAlignment>, AppError> {
    // GATE FIRST — a locked meeting leaks NOTHING (no segment times, no speakers, no overlaps).
    if !meeting_is_unlocked(state, meeting_id)? {
        return Ok(Vec::new());
    }
    let Some(note) = state.db.get_latest_note_for_meeting(meeting_id)? else {
        return Ok(Vec::new()); // no note yet ⇒ nothing to receipt.
    };
    let segments = state.db.get_segments(meeting_id)?;
    // `claim_index` refers to the RAW markdown lines (`markdown.split('\n')`) the FE renders, so a
    // chip maps straight back to its note line. `align_claims_to_segments` skips a leading YAML
    // front-matter block BY INDEX (metadata like `attendees:` never earns a receipt) plus headings/
    // blockquotes/fences/etc., so passing every raw line is safe and indices keep this numbering.
    let lines: Vec<&str> = note.markdown.split('\n').collect();
    let receipts = crate::summarize::grounding::align_claims_to_segments(&lines, &segments);
    tracing::debug!(
        target: "receipts",
        meeting = %meeting_id,
        note_lines = lines.len(),
        segments = segments.len(),
        receipts = receipts.len(),
        "computed note receipts"
    );
    Ok(receipts)
}

/// Brain v3 audit PR-8 (Knowledge Diff completion): the audio receipt for ONE fact's text against
/// its SOURCE meeting — lets the decision-ledger "Source" chip deep-seek the meeting's audio to the
/// second the fact derives from, instead of just opening the meeting. Reuses the SAME deterministic
/// token-overlap alignment as [`get_note_receipts`] (`align_claims_to_segments`, one line in ⇒ at
/// most one alignment out, `RECEIPT_MIN_OVERLAP` floor): a fact whose text the transcript doesn't
/// clearly support returns `None` and the FE falls back to plain open-the-meeting — a wrong receipt
/// is worse than none. Recomputed on demand, no storage, therefore no new seal path.
///
/// LOCK-MODEL (audio-adjacent read): the `meeting_is_unlocked` gate is FIRST, exactly like
/// `get_note_receipts` — a sealed-and-NOT-session-unlocked meeting returns `None`, never a segment
/// time, speaker, or overlap that would leak WHEN something was said or BY WHOM. No PII in logs
/// (id + hit flag only — never the fact text).
#[tauri::command]
pub fn get_fact_receipt(
    state: State<'_, AppState>,
    meeting_id: String,
    fact_text: String,
) -> Result<Option<crate::summarize::grounding::ClaimAlignment>, AppError> {
    get_fact_receipt_inner(state.inner(), &meeting_id, &fact_text)
}

/// Inner of [`get_fact_receipt`] taking `&AppState` (unit-testable gate). `None` for a
/// sealed-not-unlocked meeting; `None` when the fact text doesn't clear the alignment floor.
pub(crate) fn get_fact_receipt_inner(
    state: &AppState,
    meeting_id: &str,
    fact_text: &str,
) -> Result<Option<crate::summarize::grounding::ClaimAlignment>, AppError> {
    // GATE FIRST — a locked meeting leaks NOTHING (no segment times, no speakers, no overlaps).
    if !meeting_is_unlocked(state, meeting_id)? {
        return Ok(None);
    }
    let segments = state.db.get_segments(meeting_id)?;
    let lines = [fact_text];
    let receipt = crate::summarize::grounding::align_claims_to_segments(&lines, &segments)
        .into_iter()
        .next();
    tracing::debug!(
        target: "receipts",
        meeting = %meeting_id,
        segments = segments.len(),
        hit = receipt.is_some(),
        "computed fact receipt"
    );
    Ok(receipt)
}

/// The extensions document ingestion accepts (Brain v3 PR-2). Text (md/txt) plus the extracted
/// formats: PDF (macOS PDFKit, scanned-PDF pages fall back to on-device Vision OCR), DOCX/PPTX
/// (pure-Rust OOXML), XLSX (calamine), HTML, and — Brain v3 OCR — direct image import
/// (png/jpg/jpeg/heic/tiff/tif/bmp/gif) via on-device Apple Vision. Dispatch + extraction live in
/// `crate::extract`; anything else is rejected with `InvalidArg`.
const DOC_ALLOWED_EXTS: &[&str] = &[
    "md", "txt", "pdf", "docx", "pptx", "xlsx", "html", "htm", "png", "jpg", "jpeg", "heic", "tiff",
    "tif", "bmp", "gif",
];

/// Document ingestion — upload a local file INTO a folder so its EXTRACTED text is chunked + embedded
/// into the on-device vector layer and the brain/Ask can retrieve it. Returns the new document id.
/// ASYNC (Brain v3 PR-2): a large PDF's extract+chunk+embed runs off the UI thread behind the shared
/// heavy-inference permit + the RAM floor, emitting counts-only progress via [`EVENT_DOC_IMPORT`].
///
/// LOCK-MODEL:
/// - WRITE-GATE: refuse a sealed-and-NOT-session-unlocked folder (`AppError::Locked`) — an ungated
///   write would land plaintext at rest behind the lock (mirrors `save_manual_notes`'s gate).
/// - Extension allowlist — reject anything else with `AppError::InvalidArg`.
/// - We store the EXTRACTED TEXT only (`documents.text`), never the source binary — no new seal path.
/// - EMBED only when the REAL e5 model is present (`embed_model_present()`): otherwise the chunks are
///   stored WITHOUT vectors (no stub vectors polluting the index — mirrors `should_auto_index`).
/// - The text is SEALED-AND-RESTORED with the folder on lock/unlock; its chunks are PURGED on lock,
///   re-embeddable on unlock.
#[tauri::command]
pub async fn import_document(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    folder_id: String,
) -> Result<String, AppError> {
    // 0) WRITE-GATE up front, on the async task (touches the borrowed `&AppState`): a
    //    sealed-and-NOT-session-unlocked folder is refused BEFORE any file work so the caller fails
    //    fast (`AppError::Locked`), and an unknown folder is `InvalidArg`. The gate is RE-CHECKED
    //    inside the blocking closure right before the plaintext INSERT (below) using the cloned
    //    `unlocked_folders` handle, so a relock racing between here and the insert can't land
    //    plaintext at rest behind the lock.
    import_document_write_gate(state.inner(), &folder_id)?;

    // Clone ONLY the `Arc` handles that cross into the blocking closure — `AppState` itself is not
    // `Clone`. `db` for insert+index, `unlocked_folders` for the pre-insert gate re-check,
    // `heavy_inference` for the ONE heavy permit. Everything past this point is `'static`.
    let db = std::sync::Arc::clone(&state.inner().db);
    let unlocked = std::sync::Arc::clone(&state.inner().unlocked_folders);
    let sem = std::sync::Arc::clone(&state.inner().heavy_inference);

    // RAM floor: under memory pressure, do the (still off-thread) extract+insert but SKIP the embed —
    // the chunks + FTS are durable (keyword retrieval works) and the idempotent repair tick / a later
    // Reindex fills the vectors. Never fail the import over a busy machine. Decided here (on the async
    // task) and passed INTO the closure so the whole heavy pipeline stays inside ONE `run_heavy` scope.
    let ram_permits = crate::transcribe::model::topic_backfill_ram_permits_now();

    // The WHOLE pipeline — extract (whole-file read + zip/XML parse for OOXML/XLSX, or the per-page
    // PDFKit loop; multi-second on a large doc), insert the row + chunk (FTS durable), then embed —
    // runs OFF the Tokio worker behind the ONE heavy-inference permit (rust-tauri rule: long-running
    // work never blocks the runtime thread). Progress events fire from inside across the stages
    // (extracting → chunking → embedding → done); the `AppHandle` is `Clone` + `Send` so the emitter
    // reaches the FE from the blocking thread. Best-effort per stage: the row/chunks stay durable even
    // if the embed fails.
    let app2 = app.clone();
    let path2 = path.clone();
    let folder2 = folder_id.clone();
    crate::perf::run_heavy(&sem, move || {
        crate::events::emit_doc_import(&app2, "", "extracting", 0, 0);
        // EXTRACT (pure `path → text`, no DB/state). The progress closure translates the extractor's
        // per-page signal into `EVENT_DOC_IMPORT` "extracting done/total" events (Fix 3: real counts)
        // and records an OCR-cap truncation so the "done" event can flag a partial import (Fix 2).
        let ocr_truncated = std::cell::Cell::new(false);
        let (name, stored) = {
            let app_p = &app2;
            let extract_progress = |p: crate::extract::ExtractProgress| match p {
                crate::extract::ExtractProgress::Page { done, total } => {
                    crate::events::emit_doc_import(app_p, "", "extracting", done, total);
                }
                crate::extract::ExtractProgress::OcrTruncated { .. } => {
                    ocr_truncated.set(true);
                }
            };
            extract_document_text(&path2, &extract_progress)?
        };

        crate::events::emit_doc_import(&app2, "", "chunking", 0, 0);
        // GATE (re-checked from the cloned handle) + INSERT + CHUNK-ONLY index (FTS durable now).
        let id = insert_extracted_document(&db, &unlocked, &folder2, &name, &stored)?;

        // EMBED only if the RAM floor permitted (else defer to the repair tick). Best-effort: a
        // failure logs (no PII) and leaves the durable chunks/FTS + row in place. Per-sub-batch
        // progress (Fix 3) streams "embedding done/total" as each embed sub-batch completes.
        if ram_permits {
            let embedder = crate::embed::embed_model_present().then(crate::embed::active_embedder);
            crate::events::emit_doc_import(&app2, &id, "embedding", 0, 0);
            let id_for_progress = id.clone();
            let embed_progress = |done: usize, total: usize| {
                crate::events::emit_doc_import(&app2, &id_for_progress, "embedding", done, total);
            };
            if let Err(e) =
                index_document_row_kind_routed_progress(&db, &id, embedder.as_deref(), &embed_progress)
            {
                tracing::warn!(target: "rag", error = %e, document_id = %id, "import: embed failed (content stored)");
            }
        } else {
            tracing::info!(target: "documents", document_id = %id, "import: RAM floor — deferring embed to repair tick");
        }
        // DONE — carry the OCR-cap truncation flag so the FE can surface a partial-import notice.
        crate::events::emit_doc_import_done(&app2, &id, ocr_truncated.get());
        Ok(id)
    })
    .await
}

/// Inner of [`import_document`] taking `&AppState` (so the gate + allowlist + EXTRACTION are
/// unit-testable without a `tauri::State`). EXTRACTS the file to blocks, stores the serialized text,
/// inserts the row (write-gated), and DOES NOT embed (the async wrapper embeds behind the RAM floor /
/// permit; a unit test with the model absent still gets chunk-only indexing via `ingest_into_folder`).
///
/// This runs SYNCHRONOUSLY (the async command wrapper does the same extract→insert→embed pipeline
/// entirely inside `perf::run_heavy` / `spawn_blocking` — see [`import_document`]); this seam exists
/// only so the extract + gate + insert are testable without a `tauri::State` or a runtime. It has no
/// production caller (the async wrapper composes `extract_document_text` + `insert_extracted_document`
/// directly), so it is compiled only under `cfg(test)`.
#[cfg(test)]
pub(crate) fn import_document_inner(
    state: &AppState,
    path: &str,
    folder_id: &str,
) -> Result<String, AppError> {
    // EXTRACT (allowlist + `path → text`, pure, no DB/state).
    let (name, stored) = extract_document_text(path, &crate::extract::no_progress)?;
    // GATE + INSERT + chunk-only index (the shared seam the async wrapper also uses).
    insert_extracted_document(&state.db, &state.unlocked_folders, folder_id, &name, &stored)
}

/// EXTRACT a supported document to its storable text form — PURE `path → (display_name, text)`, with
/// NO `AppState`/DB/keychain touch, so the whole (potentially multi-second: whole-file read + zip
/// decompress + XML parse for OOXML/XLSX, or the per-page PDFKit loop) extraction can run OFF the
/// Tokio runtime inside `run_heavy`. Fails CLOSED with `AppError::InvalidArg` for an unsupported /
/// extension-less / unreadable / no-extractable-text file. Returns the file-name component as the
/// display name (never an on-disk path — no PII in the stored name/logs).
fn extract_document_text(
    path: &str,
    progress: &crate::extract::ProgressFn<'_>,
) -> Result<(String, String), AppError> {
    // Extension allowlist. Lowercased; an extension-less path is rejected.
    let p = std::path::Path::new(path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let ext = match ext {
        Some(e) if DOC_ALLOWED_EXTS.contains(&e.as_str()) => e,
        _ => {
            return Err(AppError::InvalidArg(
                "unsupported document type — import md, txt, pdf, docx, pptx, xlsx, html, or an image (png/jpg/heic/tiff/bmp/gif)".into(),
            ))
        }
    };

    // EXTRACT to blocks (page/heading preserved), then serialize to the storable text form. A
    // non-UTF-8 / unreadable / malformed / scanned-PDF / zip-bomb / over-size file fails closed inside
    // `extract_blocks` (the OOXML/XLSX decompression-ratio guard + the universal extracted-text
    // ceiling live there — see extract/mod.rs + extract/ooxml.rs). `progress` streams per-page counts.
    let blocks = crate::extract::extract_blocks(p, &ext, progress)?;
    if blocks.is_empty() || blocks.iter().all(|b| b.text.trim().is_empty()) {
        return Err(AppError::InvalidArg(
            "this document has no extractable text".into(),
        ));
    }
    let stored = crate::extract::blocks_to_stored_text(&blocks);

    // The display name = the file name (component only — never an on-disk path with personal content
    // in logs). Fallback to "document" if the path has no file-name component.
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "document".to_string());
    Ok((name, stored))
}

/// The WRITE-GATE for an uploaded document, evaluated from `Arc` handles (not a `&AppState`), so it
/// can be RE-CHECKED inside the blocking closure right before the plaintext INSERT — a relock racing
/// between the up-front async gate and the insert can't land plaintext at rest behind a lock. Mirrors
/// [`ingest_into_folder_opts`]'s gate exactly: unknown folder ⇒ `InvalidArg`; sealed-and-NOT-session-
/// unlocked ⇒ `AppError::Locked`; open OR session-unlocked ⇒ Ok.
fn insert_extracted_document(
    db: &std::sync::Arc<crate::storage::Db>,
    unlocked_folders: &std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    folder_id: &str,
    name: &str,
    stored: &str,
) -> Result<String, AppError> {
    // The folder must exist (so the FK holds + the gating has an anchor).
    let folder = db
        .folder_by_id(folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;

    // WRITE-GATE: a sealed-and-not-session-unlocked folder is refused (never resurrect plaintext at
    // rest behind a lock). Same predicate as `folder_is_unlocked`, evaluated from the cloned handle.
    if folder.locked {
        let session_unlocked = unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
            .contains(folder_id);
        if !session_unlocked {
            return Err(AppError::Locked(
                "this folder is locked — unlock it to add to the brain".into(),
            ));
        }
    }

    let id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().timestamp_millis();
    db.insert_document(&id, folder_id, name, stored, "document", created_at)?;

    // CHUNK-ONLY inline (FTS durable now); the caller embeds under the RAM floor + heavy permit. A
    // failure logs (no PII) and does NOT fail the ingest (the row + plaintext are durable; a later
    // unlock re-chunk / reindex recovers the index).
    if let Err(e) = index_document_row_kind_routed(db, &id, None) {
        tracing::warn!(target: "rag", error = %e, "ingest: chunk failed (content stored)");
    }

    // PII rule: log only ids, the kind, and byte counts — never the text/name.
    tracing::info!(
        target: "documents",
        document_id = %id,
        folder_id = %folder_id,
        kind = "document",
        bytes = stored.len(),
        "ingested document into brain"
    );
    Ok(id)
}

/// The WRITE-GATE for an uploaded document evaluated from a borrowed `&AppState` (the up-front,
/// fail-fast check on the async task before any file work). Same predicate as
/// [`insert_extracted_document`]'s re-check; delegates to the existing `folder_is_unlocked`.
fn import_document_write_gate(state: &AppState, folder_id: &str) -> Result<(), AppError> {
    let folder = state
        .db
        .folder_by_id(folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    if folder.locked && !folder_is_unlocked(state, folder_id)? {
        return Err(AppError::Locked(
            "this folder is locked — unlock it to add to the brain".into(),
        ));
    }
    Ok(())
}

/// Ingest TYPED text as a brain `note` (the Brain page "+ Add note"). Same gated ingest path + seal
/// + vector indexing as an uploaded document, just `kind="note"` and no file/extension step.
#[tauri::command]
pub fn import_text(
    state: State<'_, AppState>,
    name: String,
    text: String,
    folder_id: String,
) -> Result<String, AppError> {
    import_text_inner(state.inner(), &name, &text, &folder_id)
}

/// Inner of [`import_text`] taking `&AppState` (unit-testable gate). Empty text is refused.
pub(crate) fn import_text_inner(
    state: &AppState,
    name: &str,
    text: &str,
    folder_id: &str,
) -> Result<String, AppError> {
    if text.trim().is_empty() {
        return Err(AppError::InvalidArg("note text is empty".into()));
    }
    let name = if name.trim().is_empty() {
        "note"
    } else {
        name.trim()
    };
    ingest_into_folder(state, folder_id, name, text, "note")
}

/// The SINGLE gated ingest path for a typed note (`kind="note"`): look up the folder, WRITE-GATE it
/// (a sealed-not-unlocked folder is refused so content can never appear at rest behind a lock),
/// insert the `documents` row, and index its chunks into the vector layer ONLY when the REAL e5 model
/// is present (never stub vectors — mirrors `should_auto_index`). The row is sealed-and-restored +
/// purged-on-lock identically to an uploaded document. Returns the new id.
///
/// An uploaded DOCUMENT (`kind="document"`) takes the sibling [`insert_extracted_document`] seam
/// instead — same gate + insert + chunk-only index, but reachable from `Arc` handles so the whole
/// extract→insert→embed pipeline runs off the Tokio runtime inside `run_heavy` (Brain v3 PR-2).
fn ingest_into_folder(
    state: &AppState,
    folder_id: &str,
    name: &str,
    text: &str,
    kind: &str,
) -> Result<String, AppError> {
    // The folder must exist (so the FK holds + the gating has an anchor).
    let folder = state
        .db
        .folder_by_id(folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;

    // WRITE-GATE: a sealed-and-not-session-unlocked folder is refused (never resurrect plaintext at
    // rest behind a lock). One gate for every ingest path.
    if folder.locked && !folder_is_unlocked(state, folder_id)? {
        return Err(AppError::Locked(
            "this folder is locked — unlock it to add to the brain".into(),
        ));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().timestamp_millis();
    state
        .db
        .insert_document(&id, folder_id, name, text, kind, created_at)?;

    // ALWAYS chunk (doc_chunks + the fts_doc_chunks triggers) so keyword retrieval works on a
    // DEFAULT install — an ingested note must never be write-only memory. Vectors ONLY when the
    // REAL e5 model is present (never write stub vectors). Best-effort: a failure logs (no PII) and
    // does NOT fail the ingest (the row + plaintext are durable; a later unlock re-chunk / reindex
    // recovers the index).
    //
    // KIND-ROUTED (PR-1 finding 4): a pasted note (`kind='note'`) can carry YAML front-matter in its
    // raw `text`, which must NEVER be embedded/indexed (DESIGN §1a — tags/properties pollute the
    // vectors + snippets). Route through the ONE front-matter-stripping seam so the ingest matches
    // every other note-index path.
    let embedder = crate::embed::embed_model_present().then(crate::embed::active_embedder);
    if let Err(e) = index_document_row_kind_routed(&state.db, &id, embedder.as_deref()) {
        tracing::warn!(target: "rag", error = %e, "ingest: chunk/embed failed (content stored)");
    }

    // PII rule: log only ids, the kind, and byte/char counts — never the text/name.
    tracing::info!(
        target: "documents",
        document_id = %id,
        folder_id = %folder_id,
        kind = %kind,
        bytes = text.len(),
        "ingested into brain"
    );
    Ok(id)
}

/// List a folder's documents (metadata only — NO text). GATED: a sealed-and-NOT-session-unlocked
/// folder returns an EMPTY list (masked — never surface even a document name behind the lock),
/// exactly like the masked detail DTO drops note/segments.
#[tauri::command]
pub fn list_documents(
    state: State<'_, AppState>,
    folder_id: String,
) -> Result<Vec<DocumentInfo>, AppError> {
    list_documents_inner(state.inner(), &folder_id)
}

/// Inner of [`list_documents`] taking `&AppState` (unit-testable gate).
pub(crate) fn list_documents_inner(
    state: &AppState,
    folder_id: &str,
) -> Result<Vec<DocumentInfo>, AppError> {
    if !folder_is_unlocked(state, folder_id)? {
        return Ok(Vec::new()); // sealed-not-unlocked ⇒ masked (no ids, no names).
    }
    state.db.documents_in_folder(folder_id)
}

/// Read ONE document's full text. GATED: a sealed-and-NOT-session-unlocked folder returns "" (masked
/// — never leak the document text), exactly like `get_manual_notes`.
#[tauri::command]
pub fn get_document(state: State<'_, AppState>, id: String) -> Result<String, AppError> {
    get_document_inner(state.inner(), &id)
}

/// Inner of [`get_document`] taking `&AppState` (unit-testable gate).
pub(crate) fn get_document_inner(state: &AppState, id: &str) -> Result<String, AppError> {
    let Some((folder_id, _name, text)) = state.db.get_document(id)? else {
        return Ok(String::new()); // unknown id → nothing.
    };
    if !folder_is_unlocked(state, &folder_id)? {
        return Ok(String::new()); // sealed-not-unlocked ⇒ masked, never the stored text.
    }
    // Brain v3 PR-2: strip the block-structure markers a PR-2 upload stores in `text` — the FE gets
    // clean readable text (a note / md / txt / legacy row has no markers → unchanged).
    Ok(crate::extract::render_display_text(&text))
}



/// Permanently delete a document and cascade-delete its chunks + vectors. GATED: a
/// sealed-and-NOT-session-unlocked folder is refused (`AppError::Locked`) so the lock state can't be
/// mutated from behind the gate (consistent with `import_document`'s write-gate).
///
/// DELETE-CASCADE FIX (2026-07-15): `delete_document` is generic over BOTH `kind='document'`
/// (imported/ingested files) and `kind='note'` rows (`brain.component.ts`'s `removeItem` can reach
/// either), so it needs the SAME org-share revoke cascade as [`delete_note`] before dropping the local
/// row — a `kind='note'` document deleted through THIS surface must not resurrect via the org feed
/// either. `async` (was sync) because the revoke is a network round-trip.
#[tauri::command]
pub async fn delete_document(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError> {
    // Only a `kind='note'` row is ever tab-tracked (a plain ingested `kind='document'` has no tab —
    // see `TabKind` in `tab-keys.ts`), so only fire the delete-fan-out event for that case: emitting
    // it for an id nothing tracks is harmless, but this stays precise about what was actually deleted.
    let was_note = matches!(state.db.get_note_row(&id), Ok(Some(_)));
    delete_document_inner(state.inner(), &id).await?;
    if was_note {
        crate::events::emit_content_deleted(&app, "note", &id);
    }
    // The delete purged its audit findings (id-matched) — ping the FE inbox (count-only).
    emit_audit_updated_after_purge(&app, state.inner());
    Ok(())
}

/// Inner of [`delete_document`] taking `&AppState` (unit-testable gate). `async` for the org-share
/// revoke cascade (network round-trip); the gate + DB delete themselves stay synchronous internally.
pub(crate) async fn delete_document_inner(state: &AppState, id: &str) -> Result<(), AppError> {
    let Some(folder_id) = state.db.folder_for_document(id)? else {
        return Ok(()); // unknown id → idempotent no-op.
    };
    if !folder_is_unlocked(state, &folder_id)? {
        return Err(AppError::Locked(
            "this folder is locked — unlock it to delete a document".into(),
        ));
    }
    // REVOKE-BEFORE-DELETE (Bug A root cause): tear down every LIVE org share of this exact source
    // BEFORE the local row disappears, so the background org-sync tick can never re-pull a still-live
    // server item back into the local replica after the user asked to delete it. Fails LOUD: a revoke
    // failure (e.g. offline) aborts the delete rather than silently leaving a dangling live share.
    revoke_org_shares_for_source(state, None, Some(id)).await?;
    state.db.delete_document(id)?;
    tracing::info!(target: "documents", document_id = %id, "document deleted");
    Ok(())
}

// ── NOTES (authored `documents(kind='note')`) ────────────────────────────────────────────────────
//
// Standalone authored notes are `documents` rows with `kind='note'` — they REUSE the document
// substrate (folder-anchored, sealed with verify-before-destroy, gated, brain-indexed) and add only
// the authoring layer: create-empty, title/properties/tags, edit-and-reindex, a Notes folder tree,
// and vault `.md` export. Every read/list/export/write GATES on the note's FOLDER (via
// `folder_is_unlocked`) exactly like the document commands; a sealed-not-unlocked note is MASKED
// (title → "🔒 Locked", no markdown/snippet/tags) so its topic never leaks.

/// The DISPLAY-safe fallback for a note title: the stored `title` when present, else the `name`
/// slug. (Kept out of the DTO builders so the mask path can substitute "🔒 Locked" cleanly.)
fn note_display_title(row: &crate::storage::db::NoteRow) -> String {
    row.title
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| row.name.clone())
}

/// Build the FULL (editor) DTO from a raw note row — caller has ALREADY confirmed the folder is
/// unlocked. Parses front-matter into tags/properties; `markdown` is the full stored text.
fn note_doc_from_row(row: &crate::storage::db::NoteRow) -> NoteDoc {
    let (tags, properties) = crate::storage::db::parse_front_matter(&row.text);
    NoteDoc {
        id: row.id.clone(),
        title: note_display_title(row),
        folder_id: row.folder_id.clone(),
        markdown: row.text.clone(),
        tags,
        properties,
        updated_at: row.updated_at.unwrap_or(row.created_at),
        created_at: row.created_at,
        exported_path: row.exported_path.clone(),
        locked: false,
        shared: false, // WP6 wires this.
    }
}

/// The MASKED (sealed-not-unlocked) editor DTO: identity + timestamps only, NO body/title/tags —
/// the topic never leaks. Mirrors the masked meeting-detail DTO.
fn masked_note_doc(row: &crate::storage::db::NoteRow) -> NoteDoc {
    NoteDoc {
        id: row.id.clone(),
        title: "🔒 Locked".into(),
        folder_id: row.folder_id.clone(),
        markdown: String::new(),
        tags: Vec::new(),
        properties: std::collections::BTreeMap::new(),
        updated_at: row.updated_at.unwrap_or(row.created_at),
        created_at: row.created_at,
        exported_path: None,
        locked: true,
        shared: false,
    }
}

/// WP1 — write the note's markdown to `<vault>/<note-folder-path>/<title>.md` (note folders are
/// rooted under `Notes/…`, so this lands under `<vault>/Notes/…`), record the path in
/// `exported_path`. GATED (a sealed-not-unlocked note is never exported). Returns `Ok(None)` when no
/// vault is configured (a no-op, not an error, so the create/update save path never fails on it).
/// Idempotent + atomic + collision-suffixed via `export::write_note`.
fn export_note_to_vault(state: &AppState, id: &str) -> Result<Option<String>, AppError> {
    let Some(row) = state.db.get_note_row(id)? else {
        return Ok(None); // unknown id.
    };
    // GATE: never materialize a sealed-not-unlocked note's plaintext on disk. (The unseal path uses
    // `write_note_to_vault` directly — its authorization is the CK it just decrypted with.)
    if !folder_is_unlocked(state, &row.folder_id)? {
        return Ok(None);
    }
    write_note_to_vault(state, &row)
}

/// The actual vault-write for a note — NO folder gate. Callers MUST be authorized:
/// [`export_note_to_vault`] gates first; the unseal re-export path
/// ([`reexport_notes_in_folder`]) is authorized by the CK it decrypted the plaintext with (the
/// folder is mid-unlock, plaintext already restored) — exactly parallel to how `unseal_folder_extras`
/// writes restored plaintext into the DB columns without re-gating. Returns `Ok(None)` when no vault
/// is configured. Skips a sealed row (blank text) so a stray call never writes an empty note.
fn write_note_to_vault(
    state: &AppState,
    row: &crate::storage::db::NoteRow,
) -> Result<Option<String>, AppError> {
    if row.text.is_empty() {
        // A sealed (blanked) note or a fresh empty note: nothing meaningful to export; leave the
        // vault as-is (an empty file would be a leak of the note's existence, and content lives in
        // the DB regardless).
        return Ok(None);
    }
    let Some(vault) = vault_path(state) else {
        return Ok(None); // no vault → nothing to export (not an error).
    };

    // Subfolder = the note-folder's vault-relative path (rooted under "Notes/…"). Assert it stays
    // inside the vault (D5) before write_note creates the dir.
    let subfolder = state
        .db
        .note_folder_by_id(&row.folder_id)?
        .map(|f| f.path)
        .unwrap_or_else(|| "Notes".to_string());
    let vault_root = std::path::Path::new(&vault);
    assert_in_vault(vault_root, std::path::Path::new(&subfolder))?;

    let title = note_display_title(row);
    // The date prefix uses the note's created time (ISO) so the filename is stable across re-exports
    // (mirrors the meeting `YYYY-MM-DD HHmm - title.md` convention).
    let created_iso = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(row.created_at)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339();
    let path = crate::export::write_note(
        vault_root,
        Some(&subfolder),
        &title,
        &created_iso,
        &row.text,
    )?;
    let path_str = path.to_string_lossy().to_string();
    state.db.set_note_doc_exported_path(&row.id, Some(&path_str))?;
    // Export-collision guard: stamp the baseline from the text this export wrote. `write_note`
    // never overwrites different content (it collision-suffixes), so the file at `path` is
    // byte-equal to `row.text` in every branch — including the unlock/remove-lock re-export,
    // where any pre-lock baseline is stale and must be re-stamped fresh.
    state
        .db
        .set_note_doc_exported_hash(&row.id, Some(&crate::export::note_content_hash(&row.text)))?;
    Ok(Some(path_str))
}

/// WP3 — (re)index a note's BODY into `doc_chunks` (+ FTS via triggers) and `doc_vec_chunks` (only
/// when a real embedder is present). The FRONT-MATTER IS STRIPPED so tags/properties never pollute
/// the vectors (DESIGN §1a) — only the body is embedded, with the TITLE as the chunk header for
/// provenance. Old chunks for the note are purged first (clean replace) inside the same tx as the
/// re-insert. Mirrors `Db::index_document_chunks` but chunks the body (not the raw `text`, which for
/// a note carries YAML front-matter).
fn index_note_body_chunks(
    state: &AppState,
    id: &str,
    title: &str,
    markdown: &str,
    embedder: Option<&dyn crate::embed::Embedder>,
) -> Result<(), AppError> {
    let (_yaml, body) = crate::storage::db::split_front_matter(markdown);
    state
        .db
        .index_note_chunks(id, title, &body, embedder)
}

// ── NOTES — selection Brain-assistant (WP4) ──────────────────────────────────────────────────────
//
// The editor's selection popover calls `note_assistant_action`. Refine/Shorten rewrite the
// selection; Enhance retrieves related brain context (VISIBLE sources only, excluding the current
// note) and proposes an ADDITIVE passage with citations. Routing is `provider_for(Role::Notes)` —
// which gives local-Qwen-vs-cloud-Claude selection, the fail-closed consent gate, the
// `RedactingProvider` firewall, and the egress ledger FOR FREE — never a direct provider build.

/// RAII guard for ONE note-assist turn (residual W7 — the command-surface twin of
/// `transcribe::live::TurnGuard`): on drop — normal return, gate refusal, provider error, or a
/// panic unwinding through the async body — it decrements the per-note in-flight counter and,
/// when THIS turn raised it (`priority`, set only for a LOCAL decode), clears the user-turn
/// priority flag. The conditional clear means a concurrent live turn's flag is never stomped.
struct NoteAssistTurnGuard<'a> {
    state: &'a AppState,
    key: String,
    priority: bool,
}
impl Drop for NoteAssistTurnGuard<'_> {
    fn drop(&mut self) {
        if self.priority {
            self.state
                .user_turn_in_progress
                .store(false, std::sync::atomic::Ordering::Relaxed);
        }
        crate::transcribe::live::end_turn(&self.state.in_flight_turns, &self.key);
    }
}

/// The FULL known note-assistant action set (the FE catalog mirror). `custom` is always available
/// (the escape hatch) so it is intentionally OUTSIDE this list — it is handled explicitly. An action
/// not in this list and not `custom` is an unknown id → `InvalidArg`.
const NOTE_ASSIST_KNOWN_ACTIONS: &[&str] = &[
    // EDIT (replace)
    "refine",
    "grammar",
    "shorten",
    "expand",
    "simplify",
    "tone",
    "translate",
    // STRUCTURE
    "bullets",
    "table",
    "keypoints",
    // FROM YOUR BRAIN
    "enhance",
    "find_related",
    "link_entities",
    "fact_check",
    "ask",
    // EXTRACT
    "action_items",
    "decisions",
    // CREATE (artifact)
    "draft_followup",
    "spinoff_note",
];

/// The result shape the FE renders + applies off (see the seam contract table). MUST be one of
/// `"replace" | "insert" | "info" | "artifact"`. `custom` is a free-text replace.
fn note_assist_shape(action: &str) -> &'static str {
    match action {
        // EDIT + link_entities + custom rewrite the selection in place.
        "refine" | "grammar" | "shorten" | "expand" | "simplify" | "tone" | "translate"
        | "bullets" | "table" | "link_entities" | "custom" => "replace",
        // Keeps the text; appends after the selection.
        "keypoints" | "enhance" | "action_items" | "decisions" => "insert",
        // Read-only grounded answer + citations (no destructive edit).
        "find_related" | "fact_check" | "ask" => "info",
        // A drafted email/note (title + body).
        "draft_followup" | "spinoff_note" => "artifact",
        // Unknown ids never reach here (gated to InvalidArg upstream); default to the safest
        // non-destructive shape.
        _ => "info",
    }
}

/// Which citation-gathering strategy an action uses. Grounded brain actions reuse the enhance
/// readers (visibility-gated); `link_entities` uses the gated entity list; everything else needs
/// no retrieval.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NoteAssistRetrieval {
    /// No retrieval (pure edit on the selection).
    None,
    /// The enhance readers: `search_visible` + `search_doc_chunks_*_visible` (excluding this note).
    BrainCitations,
    /// The gated entity list (`list_entities_visible`) → which names to wikilink.
    Entities,
}

fn note_assist_retrieval(action: &str) -> NoteAssistRetrieval {
    match action {
        "enhance" | "find_related" | "fact_check" | "ask" => NoteAssistRetrieval::BrainCitations,
        "link_entities" => NoteAssistRetrieval::Entities,
        _ => NoteAssistRetrieval::None,
    }
}

/// The selection Brain-assistant action. GATED five ways (normative order): (1) the action must be
/// ENABLED in config (else `Unavailable`); (2) the note's folder must be unlocked (never send a
/// sealed note's text off-device / to any model, else `Locked`); (3) brain-grounded retrieval
/// contributes ONLY visible/unlocked sources; (4) the cloud path rides the redaction firewall via
/// `provider_for`. `find_related` is retrieval-ONLY (no provider, no egress). Returns the suggestion
/// + citations + display metadata (modelLabel/mode/redacted/shape/title).
#[tauri::command]
pub async fn note_assistant_action(
    state: State<'_, AppState>,
    req: NoteAssistRequest,
) -> Result<NoteAssistResult, AppError> {
    note_assistant_action_inner(state.inner(), req).await
}

/// Core of [`note_assistant_action`] over `&AppState` (unit-testable headless). The gate order is
/// normative: config-enabled → note-unlocked → build provider (consent/firewall) → retrieve → call.
pub(crate) async fn note_assistant_action_inner(
    state: &AppState,
    req: NoteAssistRequest,
) -> Result<NoteAssistResult, AppError> {
    // Production path: build the NOTES-role provider through the full egress gate chain.
    note_assistant_action_impl(state, req, None).await
}

/// The testable core of [`note_assistant_action_inner`]. `provider_override` lets a unit test inject
/// a scripted fake provider (so the provider-dependent actions run headless without shelling out to
/// a real LLM); in production it is `None` and the NOTES-role provider is built via `provider_for`
/// (consent gate + redaction firewall + egress ledger). The gate order is IDENTICAL either way — the
/// override only replaces the provider CONSTRUCTION at step (6), never a gate.
async fn note_assistant_action_impl(
    state: &AppState,
    req: NoteAssistRequest,
    provider_override: Option<std::sync::Arc<dyn crate::summarize::provider::SummarizerProvider>>,
) -> Result<NoteAssistResult, AppError> {
    let action = req.action.trim().to_lowercase();
    // (1) ACTION ENABLED? A disabled action is refused BEFORE any read/egress.
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    // The 3 legacy actions keep their own bools (backward compat — the FE still sends all three).
    // `custom` is the always-on escape hatch. Every OTHER KNOWN action is enabled UNLESS the user
    // opted it OUT (`note_assist_actions_off`). An id that is neither known nor `custom` → InvalidArg
    // BEFORE any read/egress.
    let enabled = match action.as_str() {
        "refine" => config.note_assist_refine,
        "shorten" => config.note_assist_shorten,
        "enhance" => config.note_assist_enhance,
        "custom" => true,
        other if NOTE_ASSIST_KNOWN_ACTIONS.contains(&other) => {
            !config.note_assist_actions_off.iter().any(|a| a == other)
        }
        other => {
            return Err(AppError::InvalidArg(format!(
                "unknown note-assistant action: {other}"
            )));
        }
    };
    if !enabled {
        return Err(AppError::Unavailable(format!(
            "the {action} note action is turned off in Settings"
        )));
    }
    if req.selection.trim().is_empty() {
        return Err(AppError::InvalidArg("no text selected".into()));
    }

    // TURN DISCIPLINE (residual W7 — Brain v2 P0.3 parity with `spawn_assistant_turn`): at most ONE
    // note-assist turn per note id at a time. A second call while one is in flight is refused (the
    // double-click pile-up guard), so duplicate decodes never stack generations on shared Metal.
    // The key is namespaced (`note-assist:<id>`) so it can never collide with the live loop's
    // meeting-id keys. Opaque id only — no PII.
    let turn_key = format!("note-assist:{}", req.note_id);
    if !crate::transcribe::live::try_begin_turn(&state.in_flight_turns, &turn_key) {
        return Err(AppError::Unavailable(
            "the note assistant is already working on this note — wait for it to finish".into(),
        ));
    }
    // RAII: released on EVERY exit path below (gate refusal, provider error, success) — a wedged
    // key can never permanently refuse this note. Mirrors `transcribe::live::TurnGuard`.
    let mut turn = NoteAssistTurnGuard {
        state,
        key: turn_key,
        priority: false,
    };

    // (2) READ-GATE: the note's folder must be unlocked (never egress a sealed note's text).
    let Some(row) = state.db.get_note_row(&req.note_id)? else {
        return Err(AppError::InvalidArg(format!("no note {}", req.note_id)));
    };
    if !folder_is_unlocked(state, &row.folder_id)? {
        return Err(AppError::Locked(
            "this note is locked — unlock its folder to use the assistant".into(),
        ));
    }

    let shape = note_assist_shape(&action).to_string();
    let retrieval = note_assist_retrieval(&action);

    // (3) FIND_RELATED: retrieval-ONLY — NO provider, NO egress. Gather visible citations through
    //     the SAME gated readers as enhance, build a one-line answer, and return `shape="info"`
    //     with `mode="local"`, `redacted=false`. This never raises the local user-turn priority
    //     flag (no decode contends for Metal) and never calls a model → a privacy win.
    if action == "find_related" {
        let citations = gather_note_enhance_citations(state, &config, &req)?;
        let suggestion = match citations.len() {
            0 => "No related sources found in your brain.".to_string(),
            1 => "1 related source in your brain.".to_string(),
            n => format!("{n} related sources in your brain."),
        };
        tracing::info!(
            target: "notes",
            action = %action,
            mode = "local",
            citations = citations.len(),
            redacted = false,
            "note assistant action completed (retrieval-only)"
        );
        return Ok(NoteAssistResult {
            action,
            suggestion,
            citations,
            model_label: "Your brain (local search)".to_string(),
            mode: "local".to_string(),
            redacted: false,
            shape,
            title: None,
        });
    }

    // (4) Resolve the display metadata (modelLabel/mode) from the RESOLVED target BEFORE the call.
    let target =
        crate::summarize::roles::provider_target(crate::summarize::roles::Role::Notes, &config);
    let mode = if crate::summarize::egress_is_cloud(&target.connection, &config) {
        "cloud"
    } else {
        "local"
    };
    // USER-TURN PRIORITY (residual W7): a LOCAL note-assist decode contends for the on-device
    // engine (shared Metal) — raise the priority flag for the turn's duration so the background
    // Realtime-Reactions scan defers, exactly like the live loop's assistant turns. The guard
    // clears it on every exit path; a cloud call never touches the flag.
    if mode == "local" {
        state
            .user_turn_in_progress
            .store(true, std::sync::atomic::Ordering::Relaxed);
        turn.priority = true;
    }
    let model_requested =
        crate::summarize::effective_model_requested(&target, &config);
    let conn_label = crate::summarize::roles::connection_display_name(&target.connection);
    let model_label = if model_requested.trim().is_empty() {
        conn_label.to_string()
    } else {
        format!("{conn_label} · {model_requested}")
    };

    // (5) Retrieve VISIBLE grounding for the brain-grounded actions (EXCLUDING this note). Both the
    //     citation readers and `list_entities_visible` push the session unlock set through
    //     `visibility_clause`, so a sealed source never grounds a result.
    let (citations, entity_names) = match retrieval {
        NoteAssistRetrieval::BrainCitations => {
            (gather_note_enhance_citations(state, &config, &req)?, Vec::new())
        }
        NoteAssistRetrieval::Entities => {
            let unlocked = unlocked_snapshot(state)?;
            let names: Vec<String> = state
                .db
                .list_entities_visible(&unlocked)?
                .into_iter()
                .map(|n| n.name)
                .collect();
            (Vec::new(), names)
        }
        NoteAssistRetrieval::None => (Vec::new(), Vec::new()),
    };

    // (6) Build the prompts, then call the NOTES-role provider (consent gate + redaction firewall +
    //     egress ledger ride inside `provider_for`/`complete_with_meta_opts`). The edit runs under a
    //     per-action token cap + low temperature (`GenOptions::edit_rewrite`) so a compression edit
    //     can't run away and LENGTHEN, and `generate_note_edit` enforces "shorten is actually shorter"
    //     with one stricter retry.
    let (system, user) =
        build_note_assist_prompt(&action, &req, &citations, &entity_names, &config.note_language);
    let provider = match provider_override {
        Some(p) => p,
        None => crate::summarize::provider_for(
            crate::summarize::roles::Role::Notes,
            &config,
            &state.heavy_inference,
        )?,
    };
    let opts = crate::reason::GenOptions::edit_rewrite(note_edit_max_tokens(
        &action,
        req.selection.chars().count(),
    ));
    let input_words = note_edit_word_count(&req.selection);
    let (mut suggestion, meta) =
        generate_note_edit(provider.as_ref(), &action, &system, &user, opts, input_words).await?;
    suggestion = suggestion.trim().to_string();

    // Artifacts carry a title (email subject / note title). Derive it from the note's own title for
    // a follow-up draft, and from the drafted body's first line for a spin-off note (a title only —
    // never logged). Non-artifacts have no title.
    let title = if shape == "artifact" {
        Some(derive_artifact_title(
            &action,
            row.title.as_deref().unwrap_or(""),
            &suggestion,
        ))
    } else {
        None
    };

    // `redacted` = the firewall scrubbed at least one PII token on THIS call (only a cloud
    // RedactingProvider populates `meta.redactions`; a local provider leaves it None → false).
    let redacted = meta
        .redactions
        .as_ref()
        .map(|r| r.email + r.card + r.phone + r.name > 0)
        .unwrap_or(false);

    tracing::info!(
        target: "notes",
        action = %action,
        mode = %mode,
        shape = %shape,
        citations = citations.len(),
        redacted,
        "note assistant action completed"
    );

    Ok(NoteAssistResult {
        action,
        suggestion,
        citations,
        model_label,
        mode: mode.to_string(),
        redacted,
        shape,
        title,
    })
}

/// Derive a non-PII-in-logs artifact title. `draft_followup` reuses the note's own title as an email
/// subject; `spinoff_note` uses the drafted body's first non-empty line (stripped of leading `#`),
/// falling back to the note title. The returned string is user-facing content (a title) — it is NEVER
/// logged.
fn derive_artifact_title(action: &str, note_title: &str, body: &str) -> String {
    let fallback = |t: &str| {
        let t = t.trim();
        if t.is_empty() {
            "Untitled".to_string()
        } else {
            t.to_string()
        }
    };
    match action {
        "draft_followup" => {
            let subj = note_title.trim();
            if subj.is_empty() {
                "Follow-up".to_string()
            } else {
                format!("Re: {subj}")
            }
        }
        // spinoff_note: first meaningful body line as the title.
        _ => body
            .lines()
            .map(|l| l.trim_start_matches('#').trim())
            .find(|l| !l.is_empty())
            .map(|l| l.chars().take(120).collect::<String>())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| fallback(note_title)),
    }
}

/// enhance-context retrieval: run the GATED brain readers (meeting `search_visible` + document/note
/// `search_doc_chunks_*_visible`), EXCLUDE the current note's own document id, cap at ≤6, and build
/// [`NoteCitation`]s. Only VISIBLE/unlocked sources contribute (both readers push the live session
/// unlock set through `visibility_clause`), so a sealed source never grounds an enhancement.
fn gather_note_enhance_citations(
    state: &AppState,
    config: &AppConfig,
    req: &NoteAssistRequest,
) -> Result<Vec<NoteCitation>, AppError> {
    const MAX_CITATIONS: usize = 6;
    let unlocked = unlocked_snapshot(state)?;
    // The query is the selection plus a little surrounding context (better recall than the raw
    // selection alone); the readers tokenize/defuse it safely.
    let mut query = req.selection.clone();
    if let Some(b) = &req.before {
        query.push(' ');
        query.push_str(b);
    }
    if let Some(a) = &req.after {
        query.push(' ');
        query.push_str(a);
    }

    let mut out: Vec<NoteCitation> = Vec::new();

    // Meeting notes/segments (FTS over visible meetings).
    for hit in state.db.search_visible(&query, MAX_CITATIONS as i64, &unlocked)? {
        out.push(NoteCitation {
            kind: "meeting".into(),
            id: hit.meeting.id.clone(),
            title: hit.meeting.title.clone().unwrap_or_else(|| "Meeting".into()),
            snippet: hit.snippet,
        });
        if out.len() >= MAX_CITATIONS {
            break;
        }
    }

    // Other notes/documents (semantic when the e5 model is present, else FTS). EXCLUDE the current
    // note's own document id (never cite the note being edited).
    if out.len() < MAX_CITATIONS {
        let doc_hits = if config.semantic_search_enabled && crate::embed::embed_model_present() {
            let embedder = crate::embed::active_embedder();
            let qvecs = embedder.embed_query(std::slice::from_ref(&query))?;
            match qvecs.into_iter().next() {
                Some(qvec) => state
                    .db
                    .search_doc_chunks_visible(&qvec, MAX_CITATIONS as i64, &unlocked)?,
                None => Vec::new(),
            }
        } else {
            state
                .db
                .search_doc_chunks_fts_visible(&query, MAX_CITATIONS as i64, &unlocked)?
        };
        for hit in doc_hits {
            if hit.document_id == req.note_id {
                continue; // never cite the note being edited.
            }
            out.push(NoteCitation {
                kind: "note".into(),
                id: hit.document_id,
                title: hit.name,
                snippet: hit.snippet,
            });
            if out.len() >= MAX_CITATIONS {
                break;
            }
        }
    }

    // Org shared brain (deliberately-disclosed colleague content — outside the folder-lock domain,
    // gated on membership only via `org_brain_available`, same seam as the `org_brain_search` agent
    // tool / MCP `org_search`). RETRIEVAL-ONLY: no provider call, no egress — this is a private,
    // user-navigated discovery surface, matching `find_related`'s zero-provider-call invariant.
    if out.len() < MAX_CITATIONS && crate::tools::org_brain_available(&state.db, config) {
        let org_hits = crate::tools::search_org_brain_hits(&state.db, config, &query)?;
        for hit in org_hits {
            out.push(NoteCitation {
                kind: "org".into(),
                id: hit.item_id,
                title: hit.title,
                snippet: hit.snippet,
            });
            if out.len() >= MAX_CITATIONS {
                break;
            }
        }
    }
    Ok(out)
}

/// Format the retrieved brain citations as a numbered grounding block for the grounded actions.
fn note_assist_grounding_block(citations: &[NoteCitation]) -> String {
    let mut grounding = String::new();
    for (i, c) in citations.iter().enumerate() {
        grounding.push_str(&format!(
            "[{n}] ({kind}) {title}: {snippet}\n",
            n = i + 1,
            kind = c.kind,
            title = c.title,
            snippet = c.snippet
        ));
    }
    if grounding.is_empty() {
        grounding.push_str("(no related material found)\n");
    }
    grounding
}

/// Build the (system, user) prompts for a note-assistant action. EDIT actions rewrite the selection
/// with its surrounding context; STRUCTURE reshapes it; the FROM-YOUR-BRAIN actions ground on the
/// retrieved citations (or entity names) ONLY; the INFO actions (`fact_check`/`ask`) produce an
/// ANSWER, not an edit; CREATE actions draft an artifact body. `note_language` steers the reply
/// language (matching the rest of the note stack). Every prompt passes the preceding text as
/// READ-ONLY context ("do NOT continue it") with the SELECTION LAST — the discipline that fixed
/// "shorten made it longer".
fn build_note_assist_prompt(
    action: &str,
    req: &NoteAssistRequest,
    citations: &[NoteCitation],
    entity_names: &[String],
    note_language: &str,
) -> (String, String) {
    let lang = if note_language.trim().is_empty() || note_language == "auto" {
        "the same language as the selected text".to_string()
    } else {
        format!("language code '{note_language}'")
    };
    // EDIT actions (refine/grammar/shorten/expand/simplify/tone) rewrite a passage the user
    // ALREADY wrote in some language — they must always match ITS language, never the global
    // `note_language` pin (that pin is for content GENERATED from scratch: the full note, the
    // STRUCTURE/FROM-YOUR-BRAIN/INFO/CREATE actions, and `translate`'s explicit target). Forcibly
    // translating during a surgical edit was the bug: Shorten on an English passage under a
    // Polish-pinned note_language rewrote it into Polish instead of shortening it in English.
    let edit_lang = "the same language as the selected text".to_string();
    let before = req.before.as_deref().unwrap_or("");
    let preceding = if before.trim().is_empty() {
        String::new()
    } else {
        format!("Preceding text (READ-ONLY context — do NOT reproduce or continue it):\n{before}\n\n")
    };
    let sel = req.selection.as_str();
    let variant = req.variant.as_deref().unwrap_or("").trim();
    let instruction = req.instruction.as_deref().unwrap_or("").trim();
    match action {
        "refine" => {
            let system = format!(
                "You refine a passage of the user's own note: improve clarity, grammar, and flow \
                 WITHOUT changing its meaning, adding facts, or padding its length. Reply in \
                 {edit_lang}. Output ONLY the rewritten passage — no preamble, no quotes, no \
                 explanation."
            );
            let user = format!("{preceding}PASSAGE TO REFINE (rewrite ONLY this):\n{sel}");
            (system, user)
        }
        "grammar" => {
            let system = format!(
                "You are a copy-editor. Correct ONLY spelling, grammar, and punctuation in the \
                 passage. Do NOT restructure, rephrase, shorten, lengthen, or change the meaning or \
                 word choice beyond what a correction requires. Reply in {edit_lang}. Output ONLY \
                 the corrected passage — no preamble, no quotes, no explanation."
            );
            let user = format!("{preceding}PASSAGE TO CORRECT (fix ONLY this):\n{sel}");
            (system, user)
        }
        "shorten" => {
            let system = format!(
                "You shorten a passage of the user's own note. Rewrite it in ABOUT HALF the \
                 sentences: keep every decision, number, name, date, and commitment; cut hedging, \
                 repetition, filler, and throat-clearing. The result MUST be shorter than the \
                 original. Reply in {edit_lang}. Output ONLY the shortened passage — no preamble, \
                 no quotes, no explanation.\n\nExample —\nOriginal: I think that, honestly, we should \
                 probably consider maybe moving the deadline to Friday, because the team is quite \
                 busy right now and there is a lot going on.\nShortened: Move the deadline to \
                 Friday — the team is overloaded."
            );
            let user = format!("{preceding}PASSAGE TO SHORTEN (rewrite ONLY this, shorter):\n{sel}");
            (system, user)
        }
        "expand" => {
            let system = format!(
                "You expand a terse passage of the user's own note into fuller, clearer prose. \
                 Elaborate ONLY on what is already stated — spell out shorthand, join fragments into \
                 sentences, add connective phrasing. Do NOT invent facts, opinions, numbers, names, \
                 or commitments that are not in the passage. Reply in {edit_lang}. Output ONLY the \
                 expanded passage — no preamble, no quotes, no explanation."
            );
            let user = format!("{preceding}PASSAGE TO EXPAND (rewrite ONLY this, fuller):\n{sel}");
            (system, user)
        }
        "simplify" => {
            let system = format!(
                "You rewrite a passage of the user's own note in plain, jargon-free language a \
                 non-expert can follow. Keep every fact, number, name, and decision; replace \
                 jargon and convoluted phrasing with simple words and short sentences. Do NOT add or \
                 remove information. Reply in {edit_lang}. Output ONLY the simplified passage — no \
                 preamble, no quotes, no explanation."
            );
            let user = format!("{preceding}PASSAGE TO SIMPLIFY (rewrite ONLY this):\n{sel}");
            (system, user)
        }
        "tone" => {
            let tone = if variant.is_empty() { "professional" } else { variant };
            let system = format!(
                "You rewrite a passage of the user's own note in a {tone} tone WITHOUT changing its \
                 meaning, facts, or length beyond what the tone requires. Reply in {edit_lang}. \
                 Output ONLY the rewritten passage — no preamble, no quotes, no explanation."
            );
            let user = format!("{preceding}PASSAGE TO REWRITE in a {tone} tone (rewrite ONLY this):\n{sel}");
            (system, user)
        }
        "translate" => {
            // For translate the TARGET language is the variant, overriding note_language.
            let target = if variant.is_empty() {
                "the language the user most likely wants".to_string()
            } else {
                variant.to_string()
            };
            let system = format!(
                "You translate a passage of the user's own note into {target}. Preserve meaning, \
                 tone, formatting, names, numbers, and any markdown. Do NOT add or omit content. \
                 Output ONLY the translated passage — no preamble, no quotes, no explanation."
            );
            let user = format!("{preceding}PASSAGE TO TRANSLATE into {target} (translate ONLY this):\n{sel}");
            (system, user)
        }
        "bullets" => {
            let system = format!(
                "You reformat a passage of the user's own note into a markdown bullet list. Turn \
                 each distinct point into its own `- ` line, preserving every fact, number, name, and \
                 decision; do NOT add, remove, or invent content. Reply in {lang}. Output ONLY the \
                 markdown list — no preamble, no heading, no explanation."
            );
            let user = format!("{preceding}PASSAGE TO CONVERT to a bullet list (convert ONLY this):\n{sel}");
            (system, user)
        }
        "table" => {
            let system = format!(
                "You reformat a passage of the user's own note into a GitHub-flavored markdown table \
                 with a header row and a `---` separator. Infer sensible columns from the content; \
                 preserve every fact, number, name, and decision; do NOT add or invent data. Reply \
                 in {lang}. Output ONLY the markdown table — no preamble, no explanation."
            );
            let user = format!("{preceding}PASSAGE TO CONVERT to a table (convert ONLY this):\n{sel}");
            (system, user)
        }
        "keypoints" => {
            let system = format!(
                "You write a SHORT TL;DR digest of a passage of the user's own note: 2–4 markdown \
                 bullets capturing only the key points, decisions, and numbers. This is an ADDITIVE \
                 summary to insert AFTER the selection — do NOT rewrite or reproduce the original. \
                 Reply in {lang}. Output ONLY the bullet digest — no preamble, no heading, no \
                 explanation."
            );
            let user = format!("{preceding}PASSAGE TO SUMMARIZE (write a short digest of this):\n{sel}");
            (system, user)
        }
        "enhance" => {
            let grounding = note_assist_grounding_block(citations);
            let system = format!(
                "You expand the user's note by proposing a SHORT ADDITIVE passage that builds on \
                 the selection using ONLY the RELATED MATERIAL provided — never invent facts. If the \
                 material adds nothing, reply with an empty line. Reply in {lang}. Output ONLY the \
                 additive passage to INSERT after the selection — no preamble, no headings, no \
                 explanation."
            );
            let user = format!(
                "RELATED MATERIAL (from the user's own brain):\n{grounding}\nSELECTION TO EXPAND:\n{sel}"
            );
            (system, user)
        }
        "link_entities" => {
            let names = if entity_names.is_empty() {
                "(none — return the selection unchanged)".to_string()
            } else {
                entity_names.join(", ")
            };
            let system = format!(
                "You rewrite a passage of the user's own note, wrapping ONLY the known entity names \
                 listed below in `[[wikilinks]]` where they appear. Do NOT invent links, do NOT link \
                 any name not in the list, do NOT change any other word, spacing, or punctuation, and \
                 do NOT double-wrap a name already inside `[[...]]`. If a name does not appear in the \
                 passage, leave the passage as-is for that name. Reply in {lang}. Output ONLY the \
                 rewritten passage — no preamble, no quotes, no explanation."
            );
            let user = format!(
                "KNOWN ENTITY NAMES (link ONLY these):\n{names}\n\n{preceding}PASSAGE TO LINK (rewrite ONLY this):\n{sel}"
            );
            (system, user)
        }
        "fact_check" => {
            let grounding = note_assist_grounding_block(citations);
            let system = format!(
                "You fact-check the SELECTION against the user's OWN brain (the RELATED MATERIAL \
                 below) ONLY — never external knowledge. Flag any claim in the selection that \
                 CONTRADICTS or is UNSUPPORTED by the material, quoting the conflicting source. If \
                 everything checks out, say so briefly. This is an ANSWER, not an edit — do NOT \
                 rewrite the selection. Reply in {lang}. Output ONLY your findings — no preamble."
            );
            let user = format!(
                "RELATED MATERIAL (from the user's own brain):\n{grounding}\nSELECTION TO FACT-CHECK:\n{sel}"
            );
            (system, user)
        }
        "ask" => {
            let grounding = note_assist_grounding_block(citations);
            let question = if instruction.is_empty() {
                "What is the most important thing to know about this selection?"
            } else {
                instruction
            };
            let system = format!(
                "You answer the user's QUESTION about the SELECTION, grounded in the SELECTION and \
                 the RELATED MATERIAL from their own brain ONLY — never invent facts. If the answer \
                 is not in the material, say so. This is an ANSWER, not an edit — do NOT rewrite the \
                 selection. Reply in {lang}. Output ONLY the answer — no preamble."
            );
            let user = format!(
                "QUESTION:\n{question}\n\nRELATED MATERIAL (from the user's own brain):\n{grounding}\nSELECTION:\n{sel}"
            );
            (system, user)
        }
        "action_items" => {
            let system = format!(
                "You extract action items / TODOs from a passage of the user's own note into a \
                 markdown checklist. Each task is its own `- [ ] ` line; capture the owner and any \
                 due date if stated; do NOT invent tasks. If there are no action items, reply with an \
                 empty line. This is an ADDITIVE list to insert AFTER the selection — do NOT reproduce \
                 the original. Reply in {lang}. Output ONLY the checklist — no preamble, no heading."
            );
            let user = format!("{preceding}PASSAGE TO SCAN for action items:\n{sel}");
            (system, user)
        }
        "decisions" => {
            let system = format!(
                "You extract the DECISIONS made in a passage of the user's own note into a short \
                 markdown bullet list. Capture only decisions actually made (not open questions or \
                 tasks); do NOT invent any. If there are no decisions, reply with an empty line. This \
                 is an ADDITIVE list to insert AFTER the selection — do NOT reproduce the original. \
                 Reply in {lang}. Output ONLY the list — no preamble, no heading."
            );
            let user = format!("{preceding}PASSAGE TO SCAN for decisions:\n{sel}");
            (system, user)
        }
        "draft_followup" => {
            let system = format!(
                "You draft a concise follow-up email or message based on the SELECTION from the \
                 user's own note. Cover the key points, decisions, and next steps; keep a {tone} \
                 tone. Do NOT invent facts beyond the selection. Do NOT include a subject line — just \
                 the message body. Reply in {lang}. Output ONLY the message body — no preamble, no \
                 explanation.",
                tone = if variant.is_empty() { "professional" } else { variant }
            );
            let user = format!("{preceding}SELECTION TO TURN INTO A FOLLOW-UP MESSAGE:\n{sel}");
            (system, user)
        }
        "custom" => {
            // The free-text "Ask Brain to edit…" instruction, applied to the SELECTION (shape=replace).
            // MUST have its own arm — without it `custom` fell through to the spinoff_note catch-all,
            // silently dropping the instruction and drafting an unrelated note that Accept then
            // destructively wrote over the selection. The instruction is woven into the directive; the
            // FE only sends `custom` with non-empty text, but an empty directive degrades to a refine.
            let directive = if instruction.is_empty() {
                "Improve the clarity, grammar, and flow of the passage without changing its meaning"
            } else {
                instruction
            };
            let system = format!(
                "You edit a passage of the user's own note by applying THIS instruction to it: \
                 \"{directive}\". Apply the instruction to the passage ONLY — do NOT invent facts \
                 beyond it, do NOT answer as chat, do NOT continue the surrounding text. Reply in \
                 {lang}. Output ONLY the edited passage — no preamble, no quotes, no explanation."
            );
            let user = format!("{preceding}PASSAGE TO EDIT (apply the instruction, rewrite ONLY this):\n{sel}");
            (system, user)
        }
        // spinoff_note
        _ => {
            let system = format!(
                "You draft a new standalone note from the SELECTION in the user's existing note. \
                 Start with a short `# ` heading that titles the new note, then write clean note body \
                 in markdown. Build ONLY on the selection — do NOT invent facts. Reply in {lang}. \
                 Output ONLY the new note (heading + body) — no preamble, no explanation."
            );
            let user = format!("{preceding}SELECTION TO TURN INTO A NEW NOTE:\n{sel}");
            (system, user)
        }
    }
}

/// Whitespace word count — the unit the note-edit length guard compares in (word/sentence targets
/// are followable by small models; character/token targets are not).
fn note_edit_word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// The RUNAWAY-GUARD token cap for a note edit, sized off the selection (rough chars/4 token
/// estimate). `shorten` is capped at ~the input length so it can't physically LENGTHEN; the
/// in-place edits (refine/grammar/simplify/tone/translate/bullets/table/link_entities/custom) get
/// modest headroom; the ADDITIVE / GENERATIVE actions (expand/enhance/keypoints/action_items/
/// decisions/fact_check/ask/drafts) get GENEROUS headroom because their output is new content, not a
/// rewrite of the selection. Floors keep a tiny selection from being truncated; ceilings keep a huge
/// selection inside the 4096-token on-device context budget. A safety net — the prompt's length
/// budget + [`generate_note_edit`]'s validation are the primary length controls.
fn note_edit_max_tokens(action: &str, input_chars: usize) -> usize {
    let input_tokens = (input_chars / 4).max(1);
    let (mult, floor, ceil) = match action {
        // Must not exceed ~input tokens (physically can't lengthen).
        "shorten" => (1.0_f64, 48usize, 1024usize),
        // Additive / generated output: the answer/draft/list is NEW content — give it room, and a
        // higher floor so a short selection can still yield a full draft or answer.
        "expand" | "enhance" | "keypoints" | "action_items" | "decisions" | "fact_check" | "ask"
        | "draft_followup" | "spinoff_note" => (3.0_f64, 256usize, 2048usize),
        // In-place edits can legitimately match or slightly exceed the input length.
        _ => (1.5_f64, 64usize, 1536usize),
    };
    ((input_tokens as f64 * mult).ceil() as usize).clamp(floor, ceil)
}

/// Generate one note edit through `provider`, and for `shorten` ENFORCE that the result is actually
/// shorter than the input — one stricter retry if the first attempt is not (the "shorten made it
/// longer" guard; the model otherwise ran unbounded on the fully-local path). Returns the shortest
/// candidate for `shorten`, the single result otherwise. Takes `&dyn SummarizerProvider` so it is
/// unit-testable with a scripted fake provider.
async fn generate_note_edit(
    provider: &dyn crate::summarize::provider::SummarizerProvider,
    action: &str,
    system: &str,
    user: &str,
    opts: crate::reason::GenOptions,
    input_words: usize,
) -> Result<(String, crate::summarize::meta::CallMeta), AppError> {
    // `opts` is `Copy` — each call gets its own copy (no `.clone()`, which would trip clippy).
    let (out, meta) = provider.complete_with_meta_opts(system, user, opts).await?;
    if action == "shorten" && note_edit_word_count(&out) >= input_words {
        // First attempt did not shorten — retry ONCE with a stricter instruction.
        let strict = format!(
            "{system}\n\nThe previous attempt was NOT shorter than the original. Return a STRICTLY \
             shorter version: fewer words than the original, keeping only the essential facts."
        );
        let (out2, meta2) = provider.complete_with_meta_opts(&strict, user, opts).await?;
        if note_edit_word_count(&out2) < note_edit_word_count(&out) {
            return Ok((out2, meta2));
        }
    }
    Ok((out, meta))
}

// ── NOTES — auto-organize (WP5) ──────────────────────────────────────────────────────────────────
//
// Two-step, non-destructive. `plan_organize_notes` PROPOSES a target note-folder per visible note
// (via `organize::classify_subfolder` on the Notes-role provider); `apply_organize_plan` creates
// the needed note-folders and MOVES notes (reusing the gated `move_note_doc_inner`). The user
// reviews the plan before applying.

/// Propose folder assignments for the VISIBLE notes (`folder_id = Some` scopes to one note-folder,
/// `None` = all visible notes). Non-destructive: returns an [`OrganizePlan`] the FE reviews. A note
/// already correctly filed (proposed folder == its current folder) is SKIPPED.
#[tauri::command]
pub async fn plan_organize_notes(
    state: State<'_, AppState>,
    folder_id: Option<String>,
) -> Result<OrganizePlan, AppError> {
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    let unlocked = unlocked_snapshot(state.inner())?;
    let notes = state
        .db
        .list_notes_visible(folder_id.as_deref(), &unlocked)?;
    if notes.is_empty() {
        return Ok(OrganizePlan { moves: Vec::new() });
    }

    // The existing note-folder names (for reuse-preference) + a name→id map for resolving toFolderId.
    let note_folders = state.db.list_note_folders()?;
    let existing_names: Vec<String> = note_folders.iter().map(|f| f.name.clone()).collect();
    let name_to_id: std::collections::HashMap<String, String> = note_folders
        .iter()
        .map(|f| (f.name.clone(), f.id.clone()))
        .collect();
    let id_to_name: std::collections::HashMap<String, String> = note_folders
        .iter()
        .map(|f| (f.id.clone(), f.name.clone()))
        .collect();

    let provider = crate::summarize::provider_for(crate::summarize::roles::Role::Notes, &config, &state.heavy_inference)?;

    let mut moves = Vec::new();
    for n in &notes {
        // The classifier reads the note's TITLE + a body excerpt (the summary passed to
        // classify_subfolder). The list DTO carries a leak-free snippet already (visible content).
        let target = crate::summarize::organize::classify_subfolder(
            provider.as_ref(),
            &n.title,
            &n.snippet,
            &existing_names,
        )
        .await;
        let Some(to_folder) = target else {
            continue; // model declined / unusable → leave this note where it is.
        };
        let from_folder = id_to_name
            .get(&n.folder_id)
            .cloned()
            .unwrap_or_else(|| "Notes".to_string());
        // Skip a note already filed under the proposed folder (no-op move).
        if to_folder == from_folder {
            continue;
        }
        let to_folder_id = name_to_id.get(&to_folder).cloned();
        moves.push(OrganizeMove {
            note_id: n.id.clone(),
            title: n.title.clone(),
            from_folder_id: n.folder_id.clone(),
            from_folder,
            to_folder,
            to_folder_id,
            reason: "content-based filing".to_string(),
        });
    }
    Ok(OrganizePlan { moves })
}

/// Apply an auto-organize plan: per move, ensure the target note-folder exists (create it under the
/// Notes root when `toFolderId` is null), then MOVE the note (reusing the gated `move_note_doc_inner`
/// — both-sides folder gate + re-export). Non-destructive + best-effort per move (a single failure
/// logs IDs/stage and continues; the rest still apply). Idempotent on an already-filed note.
#[tauri::command]
pub fn apply_organize_plan(state: State<'_, AppState>, plan: OrganizePlan) -> Result<(), AppError> {
    apply_organize_plan_inner(state.inner(), plan)
}

/// Inner of [`apply_organize_plan`] taking `&AppState`.
pub(crate) fn apply_organize_plan_inner(
    state: &AppState,
    plan: OrganizePlan,
) -> Result<(), AppError> {
    // Cache newly-created folder ids by NAME so several notes routed to the same NEW folder create it
    // exactly once.
    let mut created: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for mv in &plan.moves {
        // Resolve or create the target note-folder id.
        let target_id = if let Some(id) = mv.to_folder_id.as_deref() {
            // Must still be a note-folder (defensive — the plan could be stale).
            if state.db.note_folder_by_id(id)?.is_none() {
                tracing::warn!(target: "notes", "organize: stale target folder id, skipping a move");
                continue;
            }
            id.to_string()
        } else if let Some(id) = created.get(&mv.to_folder) {
            id.clone()
        } else {
            // Create a new note-folder under the Notes root.
            match create_note_folder_inner(state, &mv.to_folder, None) {
                Ok(f) => {
                    created.insert(mv.to_folder.clone(), f.id.clone());
                    f.id
                }
                Err(e) => {
                    tracing::warn!(target: "notes", error = %e, "organize: create target folder failed, skipping a move");
                    continue;
                }
            }
        };
        // Move the note (gated both sides + re-export). Best-effort per move.
        if let Err(e) = move_note_doc_inner(state, &mv.note_id, &target_id) {
            tracing::warn!(target: "notes", note_id = %mv.note_id, error = %e, "organize: move failed");
        }
    }
    tracing::info!(target: "notes", moves = plan.moves.len(), "organize plan applied");
    Ok(())
}

// ── CROSS-MEETING USER MEMORY (Phase 3) ────────────────────────────────────────
//
// The auditable "what the brain knows about you" surface: list the current user-scoped memory facts
// (with provenance), forget one, or clear all. Every read is VISIBILITY-GATED — a user fact whose
// SOURCE meeting is sealed-and-not-session-unlocked is INVISIBLE here (and injected into no prompt),
// because `list_user_facts_visible` filters by source-meeting `visibility_clause` on the live
// unlocked snapshot. Forget/clear are bitemporal INVALIDATE (close valid_to), never a silent delete.

/// Brain v2 L2.3 — IMPORT pasted memories from another AI assistant (a ChatGPT/Claude "what I
/// remember about you" export) into the user-memory store. Returns the number of NEW facts added.
///
/// Flow: extract candidates on the ON-DEVICE light reasoner (`user_memory::extract_imported_memories`
/// — stub ⇒ empty ⇒ 0) → deterministic `reconcile_facts` against ALL existing user facts (a
/// re-import of the same text reconciles to NoOps ⇒ 0 new) → ONLY when there is ≥1 Add, create a
/// SYNTHETIC anchor meeting (`import-<uuid>`, title "Memory Import", `Exported`, no audio, no
/// folder — so its facts are VISIBLE via the no-note arm of the visibility predicate) → stamp +
/// apply atomically. ORDER IS LOAD-BEARING: the meeting row is created only after reconcile found
/// something to add, so a stub/duplicate import leaves NO synthetic meeting behind. Deleting that
/// meeting undoes the whole import (`delete_meeting` purges its `user_facts` in-tx). ZERO egress:
/// extraction runs on [`import_extraction_reasoner`] — LOCAL-or-stub, NEVER cloud (the FE copy
/// promises on-device; a pasted third-party memory export must not ride the cloud Notes provider).
/// No local model ⇒ 0 imported (the FE hints the model may be missing). Runs on a blocking worker
/// (a local-model extraction can take seconds). Logs counts only.
#[tauri::command]
pub async fn import_memories(state: State<'_, AppState>, text: String) -> Result<usize, AppError> {
    let db = state.db.clone();
    let reasoner = import_extraction_reasoner(state.inner());
    let enabled = user_memory_enabled(state.inner());
    tauri::async_runtime::spawn_blocking(move || {
        import_memories_inner(&db, reasoner.as_ref(), enabled, &text)
    })
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("import task join failed: {e}")))?
}


/// Inner of [`import_memories`] (unit-testable: Db + reasoner + flag injected).
pub(crate) fn import_memories_inner(
    db: &crate::storage::Db,
    reasoner: &dyn crate::reason::LocalReasoner,
    memory_enabled: bool,
    text: &str,
) -> Result<usize, AppError> {
    if !memory_enabled {
        return Err(AppError::InvalidArg(
            "cross-meeting memory is turned off — enable it in Settings to import memories".into(),
        ));
    }
    if text.trim().is_empty() {
        return Err(AppError::InvalidArg("nothing to import — paste the memory text".into()));
    }
    // 1) Best-effort extraction (stub / decode failure ⇒ empty ⇒ 0 imported, nothing persisted).
    let candidates = crate::user_memory::extract_imported_memories(reasoner, text);
    if candidates.is_empty() {
        return Ok(0);
    }
    // 2) Deterministic reconcile against ALL existing user facts — the dedup: re-importing the same
    //    export yields NoOps only.
    let existing = db.user_facts_all()?;
    let at = chrono::Utc::now().to_rfc3339();
    let mut ops = crate::facts::reconcile_facts(&existing, &candidates, &at);
    let adds = ops
        .iter()
        .filter(|o| matches!(o, crate::facts::FactOp::Add(_)))
        .count();
    if adds == 0 {
        return Ok(0); // pure dedup/no-op import — no synthetic meeting is created.
    }
    // 3) The synthetic anchor meeting — created ONLY now that something will be added, so deleting
    //    it undoes the import and a no-op import leaves nothing behind.
    let meeting_id = format!("import-{}", uuid::Uuid::new_v4());
    db.insert_meeting(&crate::storage::models::Meeting {
        id: meeting_id.clone(),
        started_at: at.clone(),
        ended_at: None,
        title: Some("Memory Import".to_string()),
        duration_s: 0,
        audio_path: None,
        status: crate::storage::models::MeetingStatus::Exported,
        folder_id: None,
    })?;
    // 4) Stamp the anchor onto the Adds (gating + purge anchor) and apply atomically. MEM-1: use the
    //    import-aware apply so every pre-existing fact this import SUPERSEDES (an Invalidate on a fact
    //    anchored to another meeting) is linked to the synthetic import id — deleting the import then
    //    REOPENS those facts, making "delete to undo" a FULL reversal instead of a partial one that
    //    leaves prior memories permanently closed.
    crate::facts::set_meeting_id(&mut ops, &meeting_id);
    db.apply_user_fact_ops_recording_import_supersedes(&ops, &meeting_id)?;
    tracing::info!(
        target: "user_memory",
        meeting_id = %meeting_id,
        added = adds,
        "memories imported (anchored to a synthetic meeting)"
    );
    Ok(adds)
}


/// Permanently delete a meeting: its audio file, its exported vault note, and all DB rows
/// (segments, notes, timeline cascade via FK). Irreversible.
///
/// DELETE-CASCADE FIX (2026-07-15): revokes every live org share of this meeting FIRST (see
/// [`delete_meeting_inner`]), then fans out a content-free delete event so any other open surface
/// (the tab-strip) can prune itself. `async` (was sync) because the revoke is a network round-trip.
#[tauri::command]
pub async fn delete_meeting(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<(), AppError> {
    delete_meeting_inner(state.inner(), &meeting_id).await?;
    crate::events::emit_content_deleted(&app, "meeting", &meeting_id);
    // The delete purged its audit findings (id-matched) — ping the FE inbox (count-only).
    emit_audit_updated_after_purge(&app, state.inner());
    Ok(())
}

/// Inner of [`delete_meeting`] taking `&AppState` (unit-testable). `async` for the org-share revoke
/// cascade (network round-trip); the file/DB cascade itself stays synchronous internally.
pub(crate) async fn delete_meeting_inner(state: &AppState, meeting_id: &str) -> Result<(), AppError> {
    // REVOKE-BEFORE-DELETE (Bug A root cause): tear down every LIVE org share of this exact meeting
    // BEFORE the local rows disappear, so the background org-sync tick can never re-pull a still-live
    // server item back into the local replica after the user asked to delete it. Fails LOUD: a revoke
    // failure (e.g. offline) aborts the delete rather than silently leaving a dangling live share.
    revoke_org_shares_for_source(state, Some(meeting_id), None).await?;

    // Capture + remove on-disk files before the rows disappear (best-effort).
    // C4: the playback audio may exist as BOTH the plaintext WAV *and* its sealed `.enc` at once —
    // during a session-unlock, `session_unseal` decrypts the `.enc` to a plaintext WAV for playback
    // but KEEPS the `.enc`. So `audio_path` (plaintext form) alone would orphan the `.enc` on
    // record→lock→unlock→delete. Remove BOTH forms, exactly as the masters block below already does.
    if let Some(m) = state.db.get_meeting(meeting_id)? {
        remove_meeting_audio_files(m.audio_path.as_deref());
    }
    // Masters too — a master path may be the plaintext WAV or its `.enc`; clear both forms.
    if let Ok((mic, sys)) = state.db.get_meeting_master_paths(meeting_id) {
        for p in [mic, sys].into_iter().flatten() {
            remove_meeting_audio_files(Some(&p));
        }
    }
    if let Some(note) = state.db.get_latest_note_for_meeting(meeting_id)? {
        if let Some(path) = note.exported_path.as_deref() {
            let _ = std::fs::remove_file(path);
        }
    }
    // Brain v2 L2.1: the delete tx purges ALL memory rollups (they may paraphrase this meeting's
    // facts) and returns their exported vault paths — remove those files here, the same layer that
    // removed the note `.md`/audio above. Rollups regenerate from visible facts on the next pass.
    let rollup_exports = state.db.delete_meeting(meeting_id)?;
    remove_rollup_export_files(&rollup_exports);
    Ok(())
}

/// Brain v2 L2.1 LOCK-SAFETY (the filesystem half of `Db::purge_memory_rollups_tx`): best-effort
/// removal of purged memory rollups' exported vault `.md`s. Only ever the paths the DB recorded at
/// export time — never any other file; a missing file is fine (the DB rows are already gone, which
/// is what the leak gate needs). Same layering as the sealed-note vault `.md` deletion: rows in the
/// db-layer tx, files at the command layer.
pub(crate) fn remove_rollup_export_files(paths: &[String]) {
    for p in paths {
        let _ = std::fs::remove_file(p);
    }
}

/// C4 — remove ALL on-disk forms of one at-rest audio path (best-effort). A path recorded in the DB
/// may be the plaintext WAV OR its sealed `.enc`, and during a session-unlock BOTH coexist (the
/// unseal decrypts the `.enc` to a playable WAV but keeps the `.enc`). So deleting a meeting must
/// remove the path as-given, its `.enc` twin, and its plaintext twin — otherwise a
/// record→lock→Touch-ID-unlock→delete leaves the `.enc` orphaned on disk. This is disk-residue
/// cleanup, NOT a security gate (the plaintext WAV is removed regardless). Mirrors the masters block
/// in `delete_meeting`. `None` is a no-op.
fn remove_meeting_audio_files(audio_path: Option<&str>) {
    let Some(p) = audio_path else { return };
    let _ = std::fs::remove_file(p); // the path as recorded (plaintext WAV or the `.enc`).
    let _ = std::fs::remove_file(format!("{p}{ENC_SUFFIX}")); // its sealed `.enc` twin.
    let _ = std::fs::remove_file(p.trim_end_matches(ENC_SUFFIX)); // its plaintext twin.
}

/// Rename a meeting's title (in-app + Library list). Does not rename the vault file.
///
/// PERF (PR-1 finding 2): after Brain v3 gap #4 the rename re-derives every chunk header/vector
/// (the title is baked into `chunk_note`/`augment_chunk_text`) — Candle/Metal work that stalls a
/// SYNC command's IPC thread for seconds on a long meeting / cold e5. Now `async` + routed through
/// the shared heavy-inference gate on the blocking pool (the `update_note` / `unlock_folder`
/// precedent). `AppHandle` is injected by Tauri (the FE `invoke('rename_meeting', {...})` is
/// unchanged); it gives the `run_heavy` closure a `'static` `AppState` via `app.state()`.
#[tauri::command]
pub async fn rename_meeting(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
    title: String,
) -> Result<(), AppError> {
    let heavy_inference = state.heavy_inference.clone();
    crate::perf::run_heavy(&heavy_inference, move || -> Result<(), AppError> {
        let state = app.state::<AppState>();
        let embedder = crate::embed::embed_model_present().then(crate::embed::active_embedder);
        rename_meeting_inner(&state, &meeting_id, &title, embedder.as_deref())
    })
    .await
}

/// Inner of [`rename_meeting`] with the re-index embedder INJECTED (deterministic tests — the
/// `reindex_meetings_after_unseal` precedent). Brain v3 (audit gap #4): the meeting TITLE is baked
/// into every chunk's header + augmented text (`chunk_note`/`augment_chunk_text`), so a rename left
/// the OLD title in every vector/snippet until a manual full reindex — re-derive best-effort after
/// the title is persisted.
pub(crate) fn rename_meeting_inner(
    state: &AppState,
    meeting_id: &str,
    title: &str,
    embedder: Option<&dyn crate::embed::Embedder>,
) -> Result<(), AppError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(AppError::InvalidArg("title cannot be empty".into()));
    }
    state.db.set_meeting_title(meeting_id, title)?;
    // Keep the companion note's managed title + front-matter `[[Meeting]]` link in sync with the new
    // title. Best-effort — a sync failure NEVER fails the rename (the title is already persisted).
    sync_companion_note_title_best_effort(state, meeting_id);
    // Brain v3 (audit gap #4): refresh chunk headers/vectors so they carry the NEW title.
    // Best-effort + sealed-folder gate inside (empty unlock set); never fails the rename.
    reindex_meeting_after_edit(state, meeting_id, embedder);
    Ok(())
}

/// Grounded Q&A over a meeting's transcript ("chat with the meeting"). The configured
/// provider answers strictly from the transcript + the running conversation history.
#[tauri::command]
pub async fn chat_meeting(
    state: State<'_, AppState>,
    meeting_id: String,
    question: String,
    history: Vec<ChatTurn>,
    explicit_sources: Option<Vec<crate::storage::models::SourceRef>>,
) -> Result<String, AppError> {
    if question.trim().is_empty() {
        return Err(AppError::InvalidArg("question is empty".into()));
    }
    // D4 READ-GATE: a sealed-and-not-unlocked meeting's transcript is blanked; refuse to chat over
    // it (it would otherwise answer from an empty transcript or leak via the provider). Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to chat about this meeting".into(),
        ));
    }
    let segments = state.db.get_segments(&meeting_id)?;
    if segments.is_empty() {
        return Err(AppError::InvalidArg(
            "this meeting has no transcript to chat about yet".into(),
        ));
    }
    let mut transcript = segments
        .iter()
        .map(|s| format!("[{:.0}s] {}", s.start_s, s.text.trim()))
        .collect::<Vec<_>>()
        .join("\n");
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    // Inject the gated cross-meeting USER MEMORY brief (parity with the @brain agentic loop): derived
    // from VISIBLE user facts only under the LIVE unlock snapshot, empty when memory is disabled ⇒
    // byte-identical prompt. Rides this surface's existing redaction + consent egress (no new class).
    let unlocked = unlocked_snapshot(state.inner())?;
    // note↔meeting-links PR-2 — SOURCE-SCOPED augmentation: when the FE pins explicit sources (the
    // linked notes/meetings the user chose above the chat input), APPEND their gated PINNED corpus
    // (+ capped, gated link-expansion) to this meeting's transcript grounding — the meeting stays the
    // primary context, the pinned items add cross-item context. Every leg is `unlocked`-gated (a
    // sealed pinned source/neighbour contributes NOTHING — never a leak). `None`/empty ⇒ byte-
    // identical to the pre-change transcript-only grounding.
    if let Some(sources) = explicit_sources.filter(|s| !s.is_empty()) {
        let ask_conn =
            crate::summarize::roles::provider_target(crate::summarize::roles::Role::Ask, &config)
                .connection;
        let (pinned, _) = crate::summarize::vault_context::build_vault_context_pinned_visible(
            &state.db, &sources, &ask_conn, &unlocked,
        )?;
        if !pinned.trim().is_empty() {
            transcript.push_str(
                "\n\n=== LINKED NOTES & MEETINGS (the user pinned these as additional context) ===\n",
            );
            transcript.push_str(&pinned);
        }
    }
    // ASK role: meeting chat is a Q&A surface. With role keys absent this resolves to the same
    // default provider as before (the legacy chat path always ignored `brain_backend`).
    let provider = crate::summarize::provider_for(
        crate::summarize::roles::Role::Ask,
        &config,
        &state.heavy_inference,
    )?;
    let memory_brief = gated_memory_brief_for_injection(state.inner(), &unlocked, &question);
    let (system, user) =
        crate::summarize::chat::build(&transcript, &history, &question, &memory_brief);
    provider.complete(&system, &user).await
}







/// Per-meeting open/done action-item counts across the VISIBLE library, for the saved-views meetings
/// surface. GATED: routes the LIVE session unlock set through `Db::list_meeting_action_summaries`
/// (`list_meetings_visible` + `get_note_if_visible`), so a sealed-and-not-session-unlocked meeting
/// contributes NO row (aggregate posture) — same gate as `get_analytics` / `list_open_commitments`.
#[tauri::command]
pub fn list_meeting_action_summaries(
    state: State<'_, AppState>,
) -> Result<Vec<MeetingActionSummary>, AppError> {
    let unlocked = unlocked_snapshot(state.inner())?;
    state.db.list_meeting_action_summaries(&unlocked)
}

/// Run a recipe prompt over a meeting's transcript (grounded), returning the artifact text.
#[tauri::command]
pub async fn run_recipe(
    state: State<'_, AppState>,
    meeting_id: String,
    prompt: String,
) -> Result<String, AppError> {
    if prompt.trim().is_empty() {
        return Err(AppError::InvalidArg("recipe prompt is empty".into()));
    }
    // BLK-2b READ-GATE: a sealed-and-not-unlocked meeting's transcript is blanked; refuse to run a
    // recipe over it (would feed a cloud provider blank/garbage and depend on at-rest blanking).
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to run a recipe".into(),
        ));
    }
    let segments = state.db.get_segments(&meeting_id)?;
    if segments.is_empty() {
        return Err(AppError::InvalidArg(
            "this meeting has no transcript yet".into(),
        ));
    }
    let transcript = segments
        .iter()
        .map(|s| s.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    let provider = crate::summarize::provider_for(crate::summarize::roles::Role::Notes, &config, &state.heavy_inference)?;
    let (system, user) =
        crate::summarize::recipes::build_recipe_prompt(&transcript, &prompt, &config.note_language);
    provider.complete(&system, &user).await
}

/// Parse a meeting note's "## Action items" checklist into structured items.
#[tauri::command]
pub fn get_action_items(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<ActionItem>, AppError> {
    // D4 READ-GATE: a sealed-and-not-unlocked meeting's note markdown is blanked; refuse to parse
    // action items from it (would silently return none / leak a stale plaintext). Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to see action items".into(),
        ));
    }
    let note = state.db.get_latest_note_for_meeting(&meeting_id)?;
    Ok(match note {
        Some(n) => crate::summarize::action_items::parse_action_items(&n.markdown),
        None => Vec::new(),
    })
}

/// OPEN-COMMITMENTS rollup ("what did I promise / what's still open"): deterministically aggregate
/// every OPEN (`- [ ]`) action item across the VISIBLE library, with each item's meeting context.
/// No model — pure aggregation over the gated readers. `owner` (optional) filters case-insensitively.
/// GATED: routes through `Db::list_open_commitments`, which pushes the LIVE session unlock set
/// through `list_meetings_visible` + `get_note_if_visible` (the same predicate as `ask_vault` /
/// `generate_digest` / MCP) — a sealed-and-not-session-unlocked meeting contributes nothing.
#[tauri::command]
pub fn list_open_commitments(
    state: State<'_, AppState>,
    owner: Option<String>,
) -> Result<Vec<Commitment>, AppError> {
    let unlocked = unlocked_snapshot(state.inner())?;
    let owner = owner.as_deref().map(str::trim).filter(|o| !o.is_empty());
    state.db.list_open_commitments(&unlocked, owner)
}

/// Rewrite the note's action items into Obsidian Tasks format (📅 due dates) + re-write the
/// vault file in place. Returns the updated note.
#[tauri::command]
pub fn patch_note_tasks(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<NoteDto, AppError> {
    // BLK-1 / TOCTOU (2026-07-10 audit F4): hold the lifecycle guard across gate+write so a
    // concurrent relock/seal cannot land between the unlock check and the upsert.
    let _lifecycle = lifecycle_guard(state.inner());
    // D4 WRITE-GATE: refuse to rewrite a sealed-and-not-unlocked meeting's note (its plaintext is
    // blanked; patching would persist the blanked value over the sealed content). Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to rewrite the note's tasks".into(),
        ));
    }
    let existing = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    let patched = crate::summarize::action_items::patch_tasks_markdown(&existing.markdown);
    let created_at = chrono::Utc::now().to_rfc3339();
    // Seal-on-write (audit F1): a session-unlocked LOCKED folder re-seals the patched markdown.
    upsert_note_reseal_if_locked(
        state.inner(),
        &NoteRecord {
            meeting_id: meeting_id.clone(),
            provider_id: existing.provider_id.clone(),
            markdown: patched.clone(),
            created_at,
            exported_path: existing.exported_path.clone(),
            model_requested: existing.model_requested.clone(),
            model_served: existing.model_served.clone(),
            gateway_host: existing.gateway_host.clone(),
        },
    )?;
    if let Some(path) = existing.exported_path.as_deref() {
        overwrite_exported_note_guarded(
            state.inner(),
            &meeting_id,
            &existing.provider_id,
            path,
            &patched,
        )?;
    }
    Ok(NoteDto {
        meeting_id,
        provider_id: existing.provider_id,
        markdown: patched,
        exported_path: existing.exported_path,
    })
}

/// Pin a meeting moment: append a timestamped ^block-ref to the note (DB + vault file) and
/// return an obsidian:// deep link to the note.
#[tauri::command]
pub fn pin_moment(
    state: State<'_, AppState>,
    meeting_id: String,
    seconds: f64,
    label: String,
) -> Result<PinResult, AppError> {
    // BLK-1 / TOCTOU (2026-07-10 audit F4): hold the lifecycle guard across gate+write so a
    // concurrent relock/seal cannot land between the unlock check and the upsert.
    let _lifecycle = lifecycle_guard(state.inner());
    // BLK-2b WRITE-GATE: refuse to pin into a sealed-and-not-unlocked meeting's note — its plaintext
    // markdown is blanked, so appending a pin would persist the blanked value over the sealed
    // content AND re-export a plaintext `.md`. Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to pin a moment".into(),
        ));
    }
    let existing = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    let secs = seconds.max(0.0) as i64;
    let block_id = format!("m{secs}");
    let mmss = format!("{}:{:02}", secs / 60, secs % 60);
    let new_md = crate::export::append_pin(&existing.markdown, &mmss, &label, &block_id);
    let created_at = chrono::Utc::now().to_rfc3339();
    // Seal-on-write (audit F1): a session-unlocked LOCKED folder re-seals the pinned markdown.
    upsert_note_reseal_if_locked(
        state.inner(),
        &NoteRecord {
            meeting_id: meeting_id.clone(),
            provider_id: existing.provider_id.clone(),
            markdown: new_md.clone(),
            created_at,
            exported_path: existing.exported_path.clone(),
            model_requested: existing.model_requested.clone(),
            model_served: existing.model_served.clone(),
            gateway_host: existing.gateway_host.clone(),
        },
    )?;
    let url = match existing.exported_path.as_deref() {
        Some(path) => {
            overwrite_exported_note_guarded(
                state.inner(),
                &meeting_id,
                &existing.provider_id,
                path,
                &new_md,
            )?;
            let vault = {
                state
                    .config
                    .lock()
                    .map_err(|_| AppError::Config("config mutex poisoned".into()))?
                    .vault_path
                    .clone()
            };
            match vault.as_deref().filter(|p| !p.is_empty()) {
                Some(v) => crate::export::build_open_url(
                    std::path::Path::new(v),
                    std::path::Path::new(path),
                ),
                None => String::new(),
            }
        }
        None => String::new(),
    };
    Ok(PinResult {
        url,
        block_id,
        mmss,
    })
}

/// Extract the people + projects from a meeting note and persist them through the dual-sink:
///
/// - **Sink A (always):** upsert each entity into the encrypted DB (`upsert_entity`, case-
///   insensitive dedup) and record a mention (`add_mention`, idempotent). The DB is the
///   source of truth for the in-app graph and works with NO vault configured.
/// - **Sink B (gated):** mirror each entity as a `[[ ]]` vault stub via `ensure_entity_backlink`
///   ONLY when a vault is configured AND the meeting's folder is NOT locked (`folder_by_id`
///   disk-truth — NOT session unlock). A session-unlocked folder must NOT re-emit `.md` stubs
///   (they were removed on seal and stay out until a permanent remove-lock), so the write gate
///   uses `locked` while every READ uses `unlocked`. A meeting at the vault root (no folder)
///   has no lock and gets its stubs.
///
/// Returns the extracted `GraphPayload`. The caller decides whether extraction failures are fatal
/// (the `link_meeting_entities` command surfaces them; the pipeline hook swallows them).
pub async fn build_and_persist_entities(
    app: &AppHandle,
    state: &AppState,
    meeting_id: &str,
    title: &str,
    markdown: &str,
) -> Result<crate::summarize::graph::GraphPayload, AppError> {
    // COMPANION NOTE title sync (2026-07-16): every pipeline finish path calls this AFTER the
    // meeting's final title is persisted (auto-title-on-close via `set_meeting_title`), so this is
    // the one funnel to refresh a companion note's managed title + `[[Meeting]]` front-matter link
    // to the final title. Best-effort — never fails the graph/note (which already succeeded).
    sync_companion_note_title_best_effort(state, meeting_id);
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    let provider = crate::summarize::provider_for(crate::summarize::roles::Role::Notes, &config, &state.heavy_inference)?;
    let entities_started = std::time::Instant::now();
    let payload =
        crate::summarize::graph::extract_entities(provider.as_ref(), title, markdown).await?;
    tracing::info!(
        target: "perf",
        stage = "extract_entities",
        elapsed_ms = entities_started.elapsed().as_millis() as u64,
        "pipeline stage complete"
    );

    // Sink A — ALWAYS persist to the encrypted DB (the graph's source of truth). Collect the
    // resolved (entity_id, name) pairs so the bitemporal-facts pass below can extract + reconcile
    // facts ABOUT these very entities.
    let mut entity_refs: Vec<(String, String)> = Vec::new();
    for p in &payload.people {
        let id = state
            .db
            .upsert_entity(p, crate::storage::models::EntityKind::Person)?;
        state.db.add_mention(&id, meeting_id)?;
        entity_refs.push((id, p.clone()));
    }
    for pr in &payload.projects {
        let id = state
            .db
            .upsert_entity(pr, crate::storage::models::EntityKind::Project)?;
        state.db.add_mention(&id, meeting_id)?;
        entity_refs.push((id, pr.clone()));
    }

    // brain2 R2 — BITEMPORAL FACTS + Phase 3 CROSS-MEETING USER MEMORY. Both are BEST-EFFORT +
    // NEVER fail the note: extract entity·predicate·object / user-preference candidates via the
    // on-device reasoner (empty with the stub / no model), reconcile, apply in one tx each. A
    // reconcile/extract hiccup is logged (non-PII) and swallowed EITHER WAY — that part is
    // unchanged. What changed: the reasoner dispatch (the brain sidecar, up to the hard-cap
    // timeout — currently 180s) used to run INLINE on this async command's Tokio worker, TWICE
    // per meeting, on EVERY recording Stop (2026-07-13 perf audit, HIGH severity). Moved to the
    // blocking pool via `perf::run_heavy` (serializes against other heavy native calls too), using
    // the AppHandle re-fetch pattern (`State<'_, AppState>` can't be captured by a `'static`
    // closure). Both calls already catch + log their own errors internally, so the closure always
    // returns `Ok(())` — a join-panic from `run_heavy` itself is logged and swallowed the same way,
    // preserving the exact "never fail the note" contract.
    let app_for_facts = app.clone();
    let meeting_id_owned = meeting_id.to_string();
    let title_owned = title.to_string();
    let markdown_owned = markdown.to_string();
    let entity_refs_owned = entity_refs.clone();
    let facts_started = std::time::Instant::now();
    if let Err(e) = crate::perf::run_heavy(&state.heavy_inference, move || -> Result<(), AppError> {
        let state = app_for_facts.state::<AppState>();
        let t0 = std::time::Instant::now();
        if let Err(e) = persist_facts_for_meeting(
            &state,
            &meeting_id_owned,
            &title_owned,
            &markdown_owned,
            &entity_refs_owned,
        ) {
            tracing::warn!(target: "facts", error = %e, "fact reconcile failed (note unaffected)");
        }
        tracing::info!(target: "perf", stage = "persist_facts", elapsed_ms = t0.elapsed().as_millis() as u64, "pipeline stage complete");
        let t1 = std::time::Instant::now();
        if let Err(e) =
            persist_user_facts_for_meeting(&state, &meeting_id_owned, &title_owned, &markdown_owned)
        {
            tracing::warn!(target: "user_memory", error = %e, "user-fact reconcile failed (note unaffected)");
        }
        tracing::info!(target: "perf", stage = "persist_user_facts", elapsed_ms = t1.elapsed().as_millis() as u64, "pipeline stage complete");
        Ok(())
    })
    .await
    {
        tracing::warn!(target: "facts", error = %e, "facts/user-memory blocking task failed (note unaffected)");
    }
    tracing::info!(
        target: "perf",
        stage = "facts_and_user_memory_total",
        elapsed_ms = facts_started.elapsed().as_millis() as u64,
        "pipeline stage complete"
    );

    // Sink B — vault [[ ]] stubs, ONLY when a vault is configured AND the meeting's folder is
    // NOT sealed on disk. Disk-truth `locked` (not session `unlocked`): a session-unlock must
    // never re-write encrypted-content stubs back to plaintext on disk.
    let vault = config.vault_path.clone().filter(|p| !p.is_empty());
    if let Some(vault) = vault {
        let folder_locked = match state.db.get_meeting(meeting_id)?.and_then(|m| m.folder_id) {
            Some(folder_id) => state
                .db
                .folder_by_id(&folder_id)?
                .map(|f| f.locked)
                .unwrap_or(false),
            None => false, // vault root → never locked
        };
        if !folder_locked {
            let vault_path = std::path::Path::new(&vault);
            for p in &payload.people {
                crate::export::entity_stub::ensure_entity_backlink(vault_path, "People", p, title)?;
            }
            for pr in &payload.projects {
                crate::export::entity_stub::ensure_entity_backlink(
                    vault_path, "Projects", pr, title,
                )?;
            }
        }
    }

    // Brain v3 PR-3 — LINK ENGINE (post-pipeline): index the finalized meeting note's `[[Title]]`
    // wikilink edges (resolved target ids) and suggest content-similar neighbours from the meeting's
    // chunks/vectors (indexed earlier in the pipeline; model-gated inside). Both best-effort — a link
    // failure never fails the graph/note (both already succeeded).
    index_wikilinks_best_effort(state, crate::links::LinkKind::Meeting, meeting_id, markdown);
    auto_link_semantic_best_effort(state, crate::links::LinkKind::Meeting, meeting_id);

    Ok(payload)
}

// ── Re-Truth (the vault heals itself) — supersession review + one-tap stamp ──────────────────────

/// One supersession surfaced for review (camelCase for the FE). `sourceNotePath` is the absolute
/// on-disk `.md` the FE never shows (it shows `sourceNoteTitle`); `applied` reflects whether the
/// row has already been stamped (always `false` from `preview_supersessions`, which returns the
/// pending set).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupersessionDto {
    pub id: String,
    pub entity: String,
    pub predicate: String,
    pub old_value: String,
    pub new_value: String,
    pub source_note_title: String,
    pub source_note_path: String,
    pub source_meeting_id: String,
    pub superseding_meeting_id: String,
    /// The superseding note's title — `None` when that note is sealed (never leak a locked title).
    pub superseding_note_title: Option<String>,
    pub applied: bool,
}

/// Result of `apply_supersessions`: how many were stamped vs skipped because their source note sealed
/// (or lost its vault file) between preview and apply (the prune↔seal TOCTOU discipline).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResult {
    pub applied: usize,
    pub skipped_sealed: usize,
}

/// A source note is STAMPABLE iff it is BOTH session-unlocked (content-read gate) AND its folder is
/// NOT locked-on-disk (write-safety gate). A sealed source's `.md` was deleted on seal and its facts
/// must surface nothing, so the intersection excludes any locked-or-not-unlocked source — Re-Truth v1
/// only ever touches OPEN-folder notes.
fn source_is_stampable(state: &AppState, meeting_id: &str) -> Result<bool, AppError> {
    Ok(meeting_is_unlocked(state, meeting_id)? && !folder_locked_on_disk(state, meeting_id)?)
}

/// Whether a meeting's folder is sealed ON DISK (disk-truth `locked`, NOT session unlock). Mirrors the
/// `build_and_persist_entities` write gate: a meeting at the vault root (no folder) is never locked.
fn folder_locked_on_disk(state: &AppState, meeting_id: &str) -> Result<bool, AppError> {
    match state.db.folder_for_meeting(meeting_id)? {
        Some(fid) => Ok(state
            .db
            .folder_by_id(&fid)?
            .map(|f| f.locked)
            .unwrap_or(false)),
        None => Ok(false),
    }
}

/// Resolve a meeting's on-disk note file: `(absolute .md path, file-stem title)` from the latest note
/// row's `exported_path`. `None` when the meeting has no note or was never exported to the vault (so
/// there is nothing to stamp). RAW read — callers gate on the meeting BEFORE exposing the result.
fn note_file_for(state: &AppState, meeting_id: &str) -> Result<Option<(String, String)>, AppError> {
    let note = match state.db.get_latest_note_for_meeting(meeting_id)? {
        Some(n) => n,
        None => return Ok(None),
    };
    let path = match note.exported_path {
        Some(p) if !p.trim().is_empty() => p,
        _ => return Ok(None),
    };
    let stem = std::path::Path::new(&path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    Ok(Some((path, stem)))
}

/// Preview the PENDING supersessions whose superseding meeting is `meeting_id`. GATED: a row is
/// included ONLY when its SOURCE meeting is stampable (open-on-disk + unlocked) AND has a vault `.md`;
/// a sealed-or-unexported source contributes NOTHING. The superseding note's title is surfaced only
/// when that note is itself unlocked. Returns `[]` when there are none.
#[tauri::command]
pub fn preview_supersessions(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<SupersessionDto>, AppError> {
    preview_supersessions_inner(state.inner(), &meeting_id)
}

pub(crate) fn preview_supersessions_inner(
    state: &AppState,
    meeting_id: &str,
) -> Result<Vec<SupersessionDto>, AppError> {
    let rows = state.db.unapplied_supersessions_for(meeting_id)?;
    let mut out = Vec::new();
    for r in rows {
        // GATE the SOURCE note (content-read + write-safety). A sealed/not-unlocked source is dropped.
        if !source_is_stampable(state, &r.source_meeting_id)? {
            continue;
        }
        // GATE the SUPERSEDING side TOO — defense-in-depth. `new_value` is derived from the superseding
        // meeting's fact, so a sealed-and-not-session-unlocked superseding meeting must surface NOTHING,
        // even in the brief race window where `lock_folder` has flipped `locked=1` but not yet purged
        // this row. Purge-on-seal normally removes it; this second-side gate closes the race regardless.
        if !meeting_is_unlocked(state, &r.superseding_meeting_id)? {
            continue;
        }
        let Some((path, stem)) = note_file_for(state, &r.source_meeting_id)? else {
            continue; // no vault file → nothing to stamp/show.
        };
        // The superseding meeting is now known-unlocked, so its title is safe to surface (`None` only
        // when it was never exported to the vault).
        let superseding_note_title =
            note_file_for(state, &r.superseding_meeting_id)?.map(|(_, s)| s);
        out.push(SupersessionDto {
            id: r.id,
            entity: r.entity,
            predicate: r.predicate,
            old_value: r.old_value,
            new_value: r.new_value,
            source_note_title: stem,
            source_note_path: path,
            source_meeting_id: r.source_meeting_id,
            superseding_meeting_id: r.superseding_meeting_id,
            superseding_note_title,
            applied: r.applied_at.is_some(),
        });
    }
    Ok(out)
}

/// APPLY the given supersessions: append a `[!superseded]` callout to each SOURCE note (and a mirror
/// backlink to the superseding note, when it too is open). RE-GATES each row at apply time — a source
/// that sealed since preview is SKIPPED (never stamped), the prune↔seal TOCTOU discipline. Snapshots
/// each note's exact bytes into the row's pre-image BEFORE the (append-only) write, so `undo` restores
/// them byte-identical. Idempotent: an already-applied row is a no-op, and the callout carries a
/// stable marker so re-stamping never duplicates.
#[tauri::command]
pub fn apply_supersessions(
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> Result<ApplyResult, AppError> {
    apply_supersessions_inner(state.inner(), &ids)
}

pub(crate) fn apply_supersessions_inner(
    state: &AppState,
    ids: &[String],
) -> Result<ApplyResult, AppError> {
    // Seal-vs-write TOCTOU (Phase-0 lock-review follow-up): hold the lifecycle guard across the
    // per-row re-gate + `.md` writes, so a concurrent lock/relock cannot land between
    // `source_is_stampable` and the append (the same guard `update_note_inner` and every other
    // vault-writing command already holds).
    let _lifecycle = lifecycle_guard(state);
    let mut applied = 0usize;
    let mut skipped_sealed = 0usize;
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();

    // PER-BATCH PRISTINE CACHE, keyed by note FILE path. Multiple rows in ONE heal touch the SAME
    // note — every row shares the superseding meeting's note, and two facts from one old note share a
    // SOURCE note too. The undo pre-image for a file MUST be its PRE-BATCH ("pristine") content,
    // captured the FIRST time this call touches that path — never the mid-batch, already-stamped
    // bytes a later row would otherwise read. So all rows sharing a note carry IDENTICAL pristine
    // pre-images and undo (restore-each-file-once) is order-independent + byte-identical for N≥2.
    let mut pristine: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();

    // PRE-PASS: seed the cache from any pre-images ALREADY durably stored by an earlier (crashed)
    // attempt, keyed by note path — so a row that still has to capture its pre-image finds the
    // pristine bytes here instead of re-reading a sibling-stamped file, regardless of the id order a
    // retry arrives in (retry-safe AND order-independent).
    for id in ids {
        let Some(row) = state.db.get_supersession(id)? else {
            continue;
        };
        if let Some(pre) = &row.source_pre_image {
            if let Some((path, _)) = note_file_for(state, &row.source_meeting_id)? {
                pristine.entry(path).or_insert_with(|| pre.clone());
            }
        }
        if let Some(pre) = &row.superseding_pre_image {
            if let Some((path, _)) = note_file_for(state, &row.superseding_meeting_id)? {
                pristine.entry(path).or_insert_with(|| pre.clone());
            }
        }
    }

    for id in ids {
        let Some(row) = state.db.get_supersession(id)? else {
            continue; // unknown id — nothing to do.
        };
        if row.applied_at.is_some() {
            continue; // already stamped — idempotent no-op.
        }
        // TOCTOU re-gate: the source folder may have sealed since preview. A now-sealed/not-unlocked
        // (or unexported) source is SKIPPED, never stamped.
        if !source_is_stampable(state, &row.source_meeting_id)? {
            skipped_sealed += 1;
            continue;
        }
        let Some((source_path, source_stem)) = note_file_for(state, &row.source_meeting_id)? else {
            skipped_sealed += 1;
            continue;
        };

        // The superseding note gets a backlink ONLY when it is itself open-on-disk + unlocked (never
        // write into or reference a sealed note). Its stem feeds the source callout's `[[…]]` link.
        let superseding_open = meeting_is_unlocked(state, &row.superseding_meeting_id)?
            && !folder_locked_on_disk(state, &row.superseding_meeting_id)?;
        let superseding_file = if superseding_open {
            note_file_for(state, &row.superseding_meeting_id)?
        } else {
            None
        };

        // Resolve PRISTINE pre-images from the per-path cache (reads + UTF-8-validates each file on its
        // first batch-touch; every later row sharing the path reuses the SAME pristine bytes).
        let source_pre = pristine_note_bytes(&mut pristine, &source_path)?;
        let superseding_pre = match &superseding_file {
            Some((p, _)) => Some(pristine_note_bytes(&mut pristine, p)?),
            None => None,
        };

        // DURABLE-BEFORE-WRITE: persist the pristine pre-images BEFORE any `.md` write, so a crash
        // between write and mark-applied still leaves a recoverable un-stamped pre-image. `COALESCE`
        // never clobbers an already-stored pristine backup and, combined with the pristine cache, the
        // stored bytes are NEVER a re-snapshot of a stamped file.
        state.db.store_supersession_pre_images(
            id,
            Some(&source_pre),
            superseding_pre.as_deref(),
        )?;

        // Stamp the SOURCE note: append the callout to the CURRENT on-disk content (idempotent — a
        // retry over an already-stamped file is a no-op). The undo pre-image is the pristine cache
        // copy, never these current bytes.
        let current_source = std::fs::read_to_string(&source_path)
            .map_err(|e| AppError::Export(format!("read source note failed: {e}")))?;
        let new_source = crate::export::obsidian::append_supersession_callout(
            &current_source,
            &date,
            &row.predicate,
            &row.old_value,
            &row.new_value,
            superseding_file.as_ref().map(|(_, s)| s.as_str()),
        );
        crate::export::obsidian::overwrite_note(std::path::Path::new(&source_path), &new_source)?;
        // Export-collision guard: this is a read-modify-write APPEND of the CURRENT file (external
        // edits are preserved in place by construction — no sibling). The baseline is re-stamped
        // from the final written content ONLY when the pre-append bytes still matched it — see
        // the helper's laundering rationale.
        refresh_meeting_note_exported_hash(
            state,
            &row.source_meeting_id,
            &current_source,
            &new_source,
        )?;

        // Stamp the SUPERSEDING backlink (its pristine pre-image is now durably stored). Append to its
        // CURRENT content (idempotent).
        if let Some((sup_path, _)) = &superseding_file {
            let current_sup = std::fs::read_to_string(sup_path)
                .map_err(|e| AppError::Export(format!("read superseding note failed: {e}")))?;
            let new_sup = crate::export::obsidian::append_supersedes_callout(
                &current_sup,
                &date,
                &row.predicate,
                &row.old_value,
                &row.new_value,
                &source_stem,
            );
            crate::export::obsidian::overwrite_note(std::path::Path::new(sup_path), &new_sup)?;
            // Same conditional append-side baseline refresh as the source stamp above.
            refresh_meeting_note_exported_hash(
                state,
                &row.superseding_meeting_id,
                &current_sup,
                &new_sup,
            )?;
        }

        // APPLIED is the LAST write — flipped only after the note(s) are safely stamped.
        state
            .db
            .mark_supersession_applied(id, &chrono::Utc::now().to_rfc3339())?;
        applied += 1;
    }
    tracing::info!(target: "retruth", applied, skipped_sealed, "supersessions applied");
    Ok(ApplyResult {
        applied,
        skipped_sealed,
    })
}

/// Resolve the PRISTINE (pre-batch) bytes of a note file from the per-apply-call cache: on the first
/// touch of a path this batch, read + UTF-8-validate the file and cache it; every later touch reuses
/// the cached bytes. This is what makes all rows sharing a note carry identical pristine pre-images
/// (so a multi-row undo restores each file once, byte-identical). Refusing a non-UTF-8 file here —
/// before any write — keeps the stamp all-or-nothing.
fn pristine_note_bytes(
    cache: &mut std::collections::HashMap<String, Vec<u8>>,
    path: &str,
) -> Result<Vec<u8>, AppError> {
    if let Some(b) = cache.get(path) {
        return Ok(b.clone());
    }
    let bytes =
        std::fs::read(path).map_err(|e| AppError::Export(format!("read note failed: {e}")))?;
    String::from_utf8(bytes.clone())
        .map_err(|_| AppError::Export("note is not valid UTF-8".into()))?;
    cache.insert(path.to_string(), bytes.clone());
    Ok(bytes)
}

/// UNDO the given applied supersessions: restore each stamped note's byte-exact pre-image (atomic
/// overwrite) and clear the row's applied state + pre-images. A row that isn't applied is a no-op. A
/// note whose folder sealed since apply is SKIPPED (never re-materialize plaintext into a locked
/// folder) — the sealed content will return WITH the stamp on unlock.
#[tauri::command]
pub fn undo_supersessions(state: State<'_, AppState>, ids: Vec<String>) -> Result<(), AppError> {
    undo_supersessions_inner(state.inner(), &ids)
}

pub(crate) fn undo_supersessions_inner(state: &AppState, ids: &[String]) -> Result<(), AppError> {
    // Seal-vs-write TOCTOU (Phase-0 lock-review follow-up): same guard as
    // `apply_supersessions_inner` — the folder-open checks below and the restore writes must not
    // interleave with a concurrent lock/relock.
    let _lifecycle = lifecycle_guard(state);
    let undo_set: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();

    // Collect the DISTINCT affected note files touched by the UNDO SET (`path -> (meeting_id,
    // pristine pre-image)`). All rows in one heal sharing a note carry IDENTICAL pristine pre-images
    // (see apply's per-path cache), and `path ↔ meeting` is 1:1. Only files whose folder is still
    // OPEN are collected/rewritten (never re-materialize plaintext into a sealed folder — a sealed
    // note's stamp rides inside its sealed content already; and purge-on-seal has already dropped any
    // supersession referencing a sealed meeting).
    let mut affected: std::collections::HashMap<String, (String, Vec<u8>)> =
        std::collections::HashMap::new();
    let mut to_clear: Vec<&String> = Vec::new();
    for id in ids {
        let Some(row) = state.db.get_supersession(id)? else {
            continue;
        };
        if row.applied_at.is_none() {
            continue; // nothing applied to undo.
        }
        if let Some(pre) = &row.source_pre_image {
            if !folder_locked_on_disk(state, &row.source_meeting_id)? {
                if let Some((path, _)) = note_file_for(state, &row.source_meeting_id)? {
                    affected
                        .entry(path)
                        .or_insert_with(|| (row.source_meeting_id.clone(), pre.clone()));
                }
            }
        }
        if let Some(pre) = &row.superseding_pre_image {
            if !folder_locked_on_disk(state, &row.superseding_meeting_id)? {
                if let Some((path, _)) = note_file_for(state, &row.superseding_meeting_id)? {
                    affected
                        .entry(path)
                        .or_insert_with(|| (row.superseding_meeting_id.clone(), pre.clone()));
                }
            }
        }
        to_clear.push(id);
    }

    // For each affected file: rebuild it as pristine + the stamps of every SURVIVOR — a supersession
    // that touches THIS note's meeting, is NOT in the undo set, and remains applied. Because the
    // callout appends are idempotent + order-independent across distinct supersessions, replaying the
    // survivors reconstructs the exact on-disk state that matches the DB. A FULL undo (no survivors)
    // collapses to a plain pristine restore. This closes the partial-undo desync where restoring a
    // shared file to pristine silently stripped a still-applied sibling's on-disk stamp.
    for (path, (meeting_id, pristine)) in &affected {
        let mut text = String::from_utf8(pristine.clone())
            .map_err(|_| AppError::Export("stored pre-image is not valid UTF-8".into()))?;
        for s in state.db.supersessions_touching_meeting(meeting_id)? {
            if undo_set.contains(s.id.as_str()) || s.applied_at.is_none() {
                continue; // being undone, or not currently applied → no stamp to replay.
            }
            // Reproduce this survivor's stamp with its ORIGINAL date (the day it was applied) so the
            // replay byte-matches the original append.
            let date = s
                .applied_at
                .as_deref()
                .and_then(|a| a.split('T').next())
                .unwrap_or("")
                .to_string();
            if &s.source_meeting_id == meeting_id {
                // This file is the SURVIVOR's SOURCE note → re-append its `[!superseded]` callout,
                // reproducing the `· see [[…]]` link exactly as apply did (open superseding only).
                let sup_stem = superseding_link_stem(state, &s)?;
                text = crate::export::obsidian::append_supersession_callout(
                    &text,
                    &date,
                    &s.predicate,
                    &s.old_value,
                    &s.new_value,
                    sup_stem.as_deref(),
                );
            }
            if &s.superseding_meeting_id == meeting_id {
                // This file is the SURVIVOR's SUPERSEDING note → re-append its `[!supersedes]`
                // backlink referencing the survivor's SOURCE stem.
                if let Some((_, src_stem)) = note_file_for(state, &s.source_meeting_id)? {
                    text = crate::export::obsidian::append_supersedes_callout(
                        &text,
                        &date,
                        &s.predicate,
                        &s.old_value,
                        &s.new_value,
                        &src_stem,
                    );
                }
            }
        }
        // Export-collision guard: the undo rebuild is a FULL overwrite from the stored pre-image
        // (+ survivor replays), so an external edit made since apply is preserved as a sibling
        // first, and the baseline re-stamped from the rebuilt content. The no-note-row branch is
        // dead-defensive (`affected` is only ever keyed via `note_file_for`, which requires a
        // note row) — it still routes through the ONE guarded overwrite (Phase-0 follow-up): a
        // missing row reads a NULL baseline (grandfathered, no sibling) and its hash re-stamp
        // updates zero rows, so behavior is identical while the invariant "every full overwrite
        // of an exported note goes through the guard" holds structurally.
        let provider_id = state
            .db
            .get_latest_note_for_meeting(meeting_id)?
            .map(|n| n.provider_id)
            .unwrap_or_default();
        overwrite_exported_note_guarded(state, meeting_id, &provider_id, path, &text)?;
    }

    // Clear the applied state on the undone rows ONLY (pre-images dropped); survivors stay applied.
    for id in to_clear {
        state.db.clear_supersession_applied(id)?;
    }
    Ok(())
}

/// The `· see [[stem]]` link stem for a supersession's SOURCE-side callout: the superseding note's
/// file-stem when that note is open-on-disk + unlocked (exactly the apply-time condition), else
/// `None` (never leak a sealed meeting's title). Mirrors the apply path so a survivor replay
/// reproduces the original callout.
fn superseding_link_stem(
    state: &AppState,
    s: &crate::storage::models::SupersessionRow,
) -> Result<Option<String>, AppError> {
    let open = meeting_is_unlocked(state, &s.superseding_meeting_id)?
        && !folder_locked_on_disk(state, &s.superseding_meeting_id)?;
    if !open {
        return Ok(None);
    }
    Ok(note_file_for(state, &s.superseding_meeting_id)?.map(|(_, stem)| stem))
}

// ── Vault Audit v1 — deterministic vault-health inbox (see `crate::audit`) ──────────────────────

/// Run ONE deterministic audit pass over the visible corpus (EMPTY unlock set — the background-job
/// discipline; see the `crate::audit` module doc) on a blocking worker, then ping the FE inbox.
/// Zero egress, zero LLM.
#[tauri::command]
pub async fn run_vault_audit(
    app: AppHandle,
) -> Result<crate::audit::AuditRunSummary, AppError> {
    let handle = app.clone();
    let mut summary = tokio::task::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        let vault = vault_path(&state);
        crate::audit::run_audit_pass(
            &state.db,
            vault.as_deref().map(std::path::Path::new),
            chrono::Utc::now().timestamp_millis(),
            &state.seal_epoch,
        )
    })
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("audit task join failed: {e}")))??;
    // Phase 3 judge tier: score THIS run's contradiction/stale findings on the LOCAL light
    // engine (skip on stub, budget-bounded, degrade-to-keep — see `judge_run_findings`), then
    // report/emit the POST-judge pending count.
    {
        let state = app.state::<AppState>();
        let stats = crate::audit::judge_run_findings(state.inner(), &summary.run_id).await;
        summary.judged = stats.judged;
        summary.demoted = stats.demoted;
        if stats.demoted > 0 {
            summary.findings_total_pending = state.db.count_pending_audit_findings()?;
        }
    }
    crate::events::emit_audit_updated(&app, summary.findings_total_pending as u32);
    Ok(summary)
}

/// USER-INITIATED cloud explanation of one PENDING finding. Gates, in order:
/// 1. the finding exists and is still pending (`InvalidArg` otherwise);
/// 2. its source AND typed target are visible under the CURRENT session unlock set —
///    [`audit_row_visible`], untyped targets fail closed — else `AppError::Locked`;
/// 3. the provider builds through the [`crate::summarize::provider_for`] chain BEFORE any prompt
///    content is assembled — the fail-closed consent gate, the redaction firewall, and the
///    egress ledger all live inside that seam (the brief runner's posture).
/// The prompt carries the finding's own evidence + a bounded, GATED excerpt of the source note
/// (current session set). RETURN-ONLY: the explanation is never persisted (no new derived
/// plaintext at rest). A seal interleaving with the in-flight provider call affects only the
/// already-read excerpt — the same accepted posture as an in-flight brief/digest synthesis.
#[tauri::command]
pub async fn explain_audit_finding(
    state: State<'_, AppState>,
    id: String,
) -> Result<crate::audit::AuditExplanation, AppError> {
    explain_audit_finding_inner(state.inner(), &id).await
}

pub(crate) async fn explain_audit_finding_inner(
    state: &AppState,
    id: &str,
) -> Result<crate::audit::AuditExplanation, AppError> {
    let row = state
        .db
        .get_audit_finding(id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no audit finding {id}")))?;
    if row.status != "pending" {
        return Err(AppError::InvalidArg(
            "only a pending finding can be explained".into(),
        ));
    }
    // User-initiated read ⇒ the CURRENT session unlock set (unlike the background pass's
    // deliberately EMPTY set).
    let unlocked = unlocked_snapshot(state)?;
    if !audit_row_visible(state, &row, &unlocked)? {
        return Err(AppError::Locked(
            "this finding's source is locked — unlock it to explain".into(),
        ));
    }
    // Provider FIRST: an unconsented cloud target refuses HERE (`Unavailable`) and no content is
    // even assembled; a consented one is redaction-wrapped + ledgered inside the factory.
    let config = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?
        .clone();
    let provider = crate::summarize::provider_for(
        crate::summarize::roles::Role::Notes,
        &config,
        &state.heavy_inference,
    )?;
    // Bounded, GATED source excerpt. The visibility check above passed, so a `None` here means
    // the source sealed (or its note vanished) in between — fail CLOSED rather than explain
    // from evidence alone.
    let body = match row.source_kind.as_str() {
        "meeting" => state
            .db
            .get_note_if_visible(&row.source_id, &unlocked)?
            .map(|n| n.markdown),
        _ => state.db.note_markdown_if_visible(&row.source_id, &unlocked)?,
    }
    .ok_or_else(|| {
        AppError::Locked("this finding's source is locked — unlock it to explain".into())
    })?;
    let snippet: String = body
        .chars()
        .take(crate::audit::EXPLAIN_SNIPPET_CHARS)
        .collect();
    let (system, user) = crate::audit::build_explain_prompt(&row, &snippet);
    let explanation_md = provider.complete(&system, &user).await?;
    tracing::info!(
        target: "audit",
        finding_id = %id,
        provider = %provider.id(),
        chars = explanation_md.len(),
        "audit finding explained"
    );
    // RETURN-ONLY — nothing stored.
    Ok(crate::audit::AuditExplanation {
        finding_id: row.id,
        explanation_md,
        provider: provider.id().to_string(),
    })
}

/// Does a wikilink title resolve AT ALL right now — the gated `resolve_wikilink` (live session
/// set) OR an existing vault file stem: the SAME union the pass's broken-link resolver uses, so
/// accept and pass agree on "broken". Distinct from [`audit_link_target_ok`] (which adds the
/// open-on-disk bar for APPENDING a link — here we only ask whether the "broken" claim is still
/// true; a sealed-not-unlocked target stays "not found", consistent with the gate posture).
fn broken_link_target_resolves(state: &AppState, title: &str) -> Result<bool, AppError> {
    let unlocked = unlocked_snapshot(state)?;
    if state.db.resolve_wikilink(title, &unlocked)?.is_some() {
        return Ok(true);
    }
    let Some(vault) = vault_path(state) else {
        return Ok(false);
    };
    Ok(
        crate::export::obsidian::list_vault_titles(std::path::Path::new(&vault))?
            .iter()
            .any(|t| t == title),
    )
}

/// A path's file stem (the wikilink target Obsidian resolves) — empty on a pathological path.
fn file_stem_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}


/// Resolve the people + projects in a meeting note → persist them to the encrypted DB graph
/// (always) and mirror them as `[[Person]]` / `[[Project]]` vault stubs (only when a vault is
/// configured + the meeting's folder is unsealed). The graph self-assembles. The DB sink works
/// even with no vault set — hence no hard "set a vault folder" error anymore.
#[tauri::command]
pub async fn link_meeting_entities(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<crate::summarize::graph::GraphPayload, AppError> {
    // BLK-2b READ-GATE: a sealed-and-not-unlocked meeting's note is blanked; refuse to extract
    // entities from it (would feed a cloud provider blank text + re-write vault stubs). Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to link entities".into(),
        ));
    }
    let meeting = state
        .db
        .get_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no meeting with id {meeting_id}")))?;
    let note = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg("this meeting has no note yet".into()))?;
    let title = meeting
        .title
        .clone()
        .unwrap_or_else(|| "Meeting".to_string());
    build_and_persist_entities(&app, &state, &meeting_id, &title, &note.markdown).await
}

/// Parse an IPC link-endpoint kind string into [`crate::links::LinkKind`], or a clean `InvalidArg`.
fn parse_link_kind(s: &str) -> Result<crate::links::LinkKind, AppError> {
    crate::links::LinkKind::parse(s).ok_or_else(|| {
        AppError::InvalidArg(format!(
            "unknown link kind {s:?} (expected \"meeting\", \"note\", or \"document\")"
        ))
    })
}

/// Brain v3 PR-3 — every persisted link edge incident on `(kind, id)`, BOTH endpoints
/// visibility-gated in [`crate::storage::db::Db::links_for_visible`] (the queried item must be
/// visible or the list is empty — no existence leak — and each edge's neighbour is dropped unless it
/// too is visible). Snapshots the LIVE session unlock set. `kind` is `"meeting" | "note" |
/// "document"`. Dismissed edges are never returned; suggested (semantic) edges are, so the FE can
/// render Accept/Dismiss.
#[tauri::command]
pub fn list_links(
    state: State<'_, AppState>,
    kind: String,
    id: String,
) -> Result<Vec<crate::storage::models::LinkEdge>, AppError> {
    let link_kind = parse_link_kind(&kind)?;
    let unlocked = unlocked_snapshot(state.inner())?;
    state.db.links_for_visible(link_kind, &id, &unlocked)
}

/// Brain v3 PR-3 — ACCEPT a suggested (semantic) link: flip it `status='active'`,
/// `created_by='accepted'`, AND materialize the neighbour's `[[Title]]` into whichever endpoint is a
/// locally-owned, session-visible note (via the managed `apply_link_markers` block — the ONLY path
/// that writes a semantic link into a `.md`; the auto pass never does).
///
/// GATED (PR-5): accept is ONLY for an unconfirmed SUGGESTION (`edge_type='semantic'`,
/// `status='suggested'`) — anything else (a deterministic wikilink/companion, an already-active
/// manual/accepted, or a dismissed tombstone) is refused `InvalidArg`. BOTH endpoints must be
/// session-visible (`link_endpoint_is_unlocked` on each) — if EITHER is sealed-and-not-unlocked the
/// accept is refused `AppError::Locked` (never activate an edge behind a lock, never reveal a locked
/// neighbour by materializing a link to it). Idempotent (re-accepting an already-active edge is a
/// no-op; the DB DO-UPDATE guard never downgrades it). The materialized marker is preserved across a
/// later neighbour-seal and re-materialized on unlock (brain-v3 audit Fix 1/Fix 4).
#[tauri::command]
pub fn accept_link(state: State<'_, AppState>, id: i64) -> Result<(), AppError> {
    accept_link_inner(state.inner(), id)
}

pub(crate) fn accept_link_inner(state: &AppState, id: i64) -> Result<(), AppError> {
    // BLK-1 / TOCTOU + non-reentrant guard: SCOPE the lifecycle guard tightly around validate + gate
    // + row-flip so a concurrent lock/relock cannot land between the endpoint gate and the flip. It is
    // RELEASED before the note-body materialize below — `materialize_accepted_link` →
    // `update_note_inner`/`update_note_doc_inner` take the guard THEMSELVES, so composing them under one
    // held guard re-enters the non-reentrant `Mutex<()>` and self-DEADLOCKS on a valid accept (mirrors
    // `link_items_inner`/`unlink_items_inner`). Lock order `lifecycle ⊃ db`.
    let (src_kind, src_id, dst_kind, dst_id) = {
        let _lifecycle = lifecycle_guard(state);
        let Some((src_kind, src_id, dst_kind, dst_id, et, status)) = state.db.link_by_id(id)? else {
            return Err(AppError::InvalidArg(format!("no link {id}")));
        };
        // ── Fix 5 (brain-v3 audit): accept is ONLY for an unconfirmed SUGGESTION. Refuse anything else
        //    (a deterministic wikilink/companion, an already-active manual/accepted, or a dismissed
        //    tombstone) — "accepting" those is meaningless and would let a caller flip arbitrary rows. ──
        if !(et == "semantic" && status == "suggested") {
            return Err(AppError::InvalidArg(format!(
                "link {id} is not an acceptable suggestion (edge_type={et}, status={status})"
            )));
        }
        // ── GATE both endpoints BEFORE flipping the row (never accept behind a lock, never reveal a
        //    locked neighbour by activating an edge to it). Fail-closed on a sealed/unknown endpoint. ──
        let (Some(sk), Some(dk)) = (
            crate::links::LinkKind::parse(&src_kind),
            crate::links::LinkKind::parse(&dst_kind),
        ) else {
            return Err(AppError::InvalidArg(format!("link {id} has a corrupt endpoint kind")));
        };
        if !link_endpoint_is_unlocked(state, sk, &src_id)?
            || !link_endpoint_is_unlocked(state, dk, &dst_id)?
        {
            return Err(AppError::Locked(
                "one of these items is locked — unlock it to accept the link".into(),
            ));
        }
        // Flip the row first: the graph edge is active even if the .md materialize below is skipped
        // (e.g. neither endpoint is a locally-owned editable note). The panel reflects the accept.
        state.db.accept_link(id)?;
        (src_kind, src_id, dst_kind, dst_id)
    }; // ── lifecycle guard RELEASED here, before materialize (which re-takes it) ──
    let unlocked = unlocked_snapshot(state)?;
    // A semantic edge is undirected — materialize the neighbour's [[Title]] into whichever endpoint
    // is a LOCAL, session-VISIBLE note we own the markdown of. Try src's note first, then dst's.
    // Best-effort: a materialize skip/failure never rolls back the accept.
    let endpoints = [
        (src_kind.as_str(), src_id.as_str(), dst_kind.as_str(), dst_id.as_str()),
        (dst_kind.as_str(), dst_id.as_str(), src_kind.as_str(), src_id.as_str()),
    ];
    for (owner_k, owner_id, other_k, other_id) in endpoints {
        let (Some(owner_kind), Some(other_kind)) = (
            crate::links::LinkKind::parse(owner_k),
            crate::links::LinkKind::parse(other_k),
        ) else {
            continue;
        };
        // Resolve the neighbour's current gated title; skip if the neighbour is sealed (no leak).
        let Some(title) =
            state.db.link_endpoint_title_visible(other_kind, other_id, &unlocked)?
        else {
            continue;
        };
        if materialize_accepted_link(state, owner_kind, owner_id, &title)? {
            break; // wrote (or found already-present) in one owned, visible source — done.
        }
    }
    tracing::info!(target: "links", link_id = id, "accept_link");
    Ok(())
}

/// Materialize the accepted neighbour's `[[Title]]` into the OWNER `(kind, id)`'s markdown via the
/// managed `apply_link_markers` block — but ONLY when the owner is a LOCAL, session-VISIBLE note we
/// own the markdown of (a meeting AI note, or an authored note). Returns `true` when the owner IS
/// such a note (whether it wrote or the link was already present), `false` when the owner is not a
/// writable/visible target (the caller then tries the other endpoint). Merges with the related-notes
/// hits already rendered in the block so the auto block is preserved.
fn materialize_accepted_link(
    state: &AppState,
    kind: crate::links::LinkKind,
    id: &str,
    title: &str,
) -> Result<bool, AppError> {
    let hit = crate::enrich::ContextHit {
        source: match kind {
            crate::links::LinkKind::Meeting => "meeting",
            _ => "note",
        }
        .to_string(),
        detail: format!("[[{title}]]"),
        url: None,
    };
    match kind {
        crate::links::LinkKind::Meeting => {
            // GATE: the meeting's note must be session-visible to write plaintext.
            if !meeting_is_unlocked(state, id)? {
                return Ok(false);
            }
            let Some(existing) = state.db.get_latest_note_for_meeting(id)? else {
                return Ok(false);
            };
            // Skip a sealed (blob-present) note (the seal-safety gate `link_related_notes_inner` uses).
            let sealed = state
                .db
                .sealable_notes_for_meeting(id)?
                .iter()
                .any(|n| n.content_blob.is_some());
            if sealed {
                return Ok(false);
            }
            let merged = merge_related_hit(&existing.markdown, hit);
            if merged != existing.markdown {
                update_note_inner(state, id, &merged)?;
            }
            Ok(true)
        }
        crate::links::LinkKind::Note | crate::links::LinkKind::Document => {
            let Some(row) = state.db.get_note_row(id)? else {
                return Ok(false); // a raw document has no editable note markdown here.
            };
            if !folder_is_unlocked(state, &row.folder_id)? {
                return Ok(false);
            }
            let title_disp = row
                .title
                .clone()
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| row.name.clone());
            let merged = merge_related_hit(&row.text, hit);
            if merged != row.text {
                update_note_doc_inner(state, id, &title_disp, &merged)?;
            }
            Ok(true)
        }
    }
}

/// Append ONE `[[Title]]` [`ContextHit`] to a note's managed `murmur:links` block WITHOUT dropping
/// any related-notes hits already rendered there. `apply_link_markers` is a full-block REPLACE, so we
/// re-collect the existing rendered hits ([`crate::enrich::extract_link_hits`]), add the new one
/// (deduped by rendered detail), and re-apply — the block stays idempotent and the auto related-notes
/// entries survive an accept.
fn merge_related_hit(markdown: &str, new_hit: crate::enrich::ContextHit) -> String {
    let mut hits = crate::enrich::extract_link_hits(markdown);
    if !hits.iter().any(|h| h.detail == new_hit.detail) {
        hits.push(new_hit);
    }
    crate::enrich::apply_link_markers(markdown, &hits)
}

/// Brain v3 PR-3 — DISMISS a suggested link: TOMBSTONE it so no later auto pass re-suggests it. No
/// markdown is touched (dismiss is graph-only). Idempotent.
///
/// Fix 5 (brain-v3 audit): dismissal is for SUGGESTIONS only, and is GATED. Refuse a dismiss on a
/// DETERMINISTIC edge (`wikilink`/`companion`) — those are re-derived from the note body / companion
/// link on every save, so a tombstone would be resurrected next save (a confusing no-op) AND could
/// be abused to silently suppress a real structural link. A `manual` edge is removed via
/// `unlink_items`, not dismissed. Both endpoints are gated (fail-closed) so a caller can neither
/// dismiss behind a lock nor probe a locked neighbour's existence via the accept/refuse response.
#[tauri::command]
pub fn dismiss_link(state: State<'_, AppState>, id: i64) -> Result<(), AppError> {
    dismiss_link_inner(state.inner(), id)
}

pub(crate) fn dismiss_link_inner(state: &AppState, id: i64) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    let Some((src_kind, src_id, dst_kind, dst_id, et, _status)) = state.db.link_by_id(id)? else {
        return Err(AppError::InvalidArg(format!("no link {id}")));
    };
    // Only a suggestion (semantic) or an accepted-then-regretted semantic edge is dismissable. A
    // deterministic wikilink/companion edge is NOT (it would just come back); a manual edge is
    // removed by `unlink_items`. Refuse the rest with a clear InvalidArg.
    if et != "semantic" {
        return Err(AppError::InvalidArg(format!(
            "link {id} is a deterministic {et} edge — dismissal is for semantic suggestions (remove a manual link via unlink)"
        )));
    }
    // ── GATE both endpoints (fail-closed on a sealed/unknown endpoint). ──
    let (Some(sk), Some(dk)) = (
        crate::links::LinkKind::parse(&src_kind),
        crate::links::LinkKind::parse(&dst_kind),
    ) else {
        return Err(AppError::InvalidArg(format!("link {id} has a corrupt endpoint kind")));
    };
    if !link_endpoint_is_unlocked(state, sk, &src_id)?
        || !link_endpoint_is_unlocked(state, dk, &dst_id)?
    {
        return Err(AppError::Locked(
            "one of these items is locked — unlock it to dismiss the suggestion".into(),
        ));
    }
    state.db.dismiss_link(id)?;
    tracing::info!(target: "links", link_id = id, "dismiss_link");
    Ok(())
}

/// note↔meeting-links PR-1 — is a link ENDPOINT `(kind, id)` session-VISIBLE right now? Mirrors
/// [`materialize_accepted_link`]'s gate order: a `Meeting` endpoint gates on [`meeting_is_unlocked`];
/// a `Note`/`Document` endpoint resolves its owning folder via `get_note_row` and gates on
/// [`folder_is_unlocked`]. An UNKNOWN endpoint (no such note/document) reports `false` — fail-closed,
/// there is nothing legitimate to link. Used by `link_items`/`unlink_items` to refuse `AppError::Locked`
/// before any write, so a manual edge is never created behind a lock and never reveals a locked
/// neighbour.
fn link_endpoint_is_unlocked(
    state: &AppState,
    kind: crate::links::LinkKind,
    id: &str,
) -> Result<bool, AppError> {
    match kind {
        crate::links::LinkKind::Meeting => meeting_is_unlocked(state, id),
        crate::links::LinkKind::Note => match state.db.get_note_row(id)? {
            Some(row) => folder_is_unlocked(state, &row.folder_id),
            None => Ok(false), // unknown note → nothing to surface. Fail-closed.
        },
        crate::links::LinkKind::Document => {
            // An imported `document` (kind != 'note') is NOT a `get_note_row` row, so routing it
            // through `get_note_row` refused it fail-closed EVEN WHEN VISIBLE (a spurious `Locked`
            // on the +Link chooser). Gate it via the canonical visibility reader
            // (`get_document_if_visible` applies `visibility_clause` against the live unlock set):
            // `Some` ⇒ visible/unlocked, `None` ⇒ sealed-or-unknown ⇒ refuse. Documents stay
            // linkable (the chooser AND the Ask source-picker both offer them) and still fail-closed.
            let unlocked = unlocked_snapshot(state)?;
            Ok(state.db.get_document_if_visible(id, &unlocked)?.is_some())
        }
    }
}

/// note↔meeting-links PR-1 — USER-INITIATED link: persist ONE directed `manual` edge
/// `(src → dst)` AND, when the source is an OWNED note, materialize `[[dst Title]]` into its body.
///
/// GATE (BEFORE any write): BOTH endpoints must be session-VISIBLE — a `meeting` via
/// [`meeting_is_unlocked`], a `note`/`document` via its folder ([`folder_is_unlocked`]). If either is
/// sealed-and-not-session-unlocked → `AppError::Locked` (never link behind a lock, never reveal a
/// locked neighbour). Unknown kinds are `AppError::InvalidArg`.
///
/// The `manual` row (`created_by='user'`, `status='active'`, `score=1.0`) is idempotent on the
/// table's UNIQUE key. For a `note` SOURCE we own the markdown of, we ALSO materialize the neighbour's
/// gated `[[Title]]` into the managed `murmur:links` block via the SAME [`materialize_accepted_link`]
/// path the accept command uses — best-effort (a materialize skip never rolls back the row). A
/// `meeting`/`document` source (no owned editable body) creates the row ONLY. The materialized
/// wikilink is display-deduped against the manual edge in `links_for_visible`.
#[tauri::command]
pub fn link_items(
    state: State<'_, AppState>,
    src_kind: String,
    src_id: String,
    dst_kind: String,
    dst_id: String,
) -> Result<(), AppError> {
    link_items_inner(state.inner(), &src_kind, &src_id, &dst_kind, &dst_id)
}

pub(crate) fn link_items_inner(
    state: &AppState,
    src_kind: &str,
    src_id: &str,
    dst_kind: &str,
    dst_id: &str,
) -> Result<(), AppError> {
    let src = parse_link_kind(src_kind)?;
    let dst = parse_link_kind(dst_kind)?;
    // Refuse a self-link (a pair pointing at itself is meaningless and would pollute the graph).
    if src == dst && src_id == dst_id {
        return Err(AppError::InvalidArg("cannot link an item to itself".into()));
    }
    // BLK-1 / TOCTOU: SCOPE the lifecycle guard tightly around gate + row-write so a concurrent
    // lock/relock cannot land between the visibility check and the edge upsert. It is RELEASED before
    // the note-body materialize below — that callee (`update_note_doc_inner`) takes the guard ITSELF
    // and re-checks the folder gate, so composing them under one held guard would re-enter a
    // non-reentrant `Mutex<()>` (the lifecycle_guard doc: never hold it around a callee that takes
    // it). Lock order `lifecycle ⊃ db`.
    {
        let _lifecycle = lifecycle_guard(state);
        // ── GATE both endpoints BEFORE any write (fail-closed on a sealed/unknown endpoint). ──
        if !link_endpoint_is_unlocked(state, src, src_id)?
            || !link_endpoint_is_unlocked(state, dst, dst_id)?
        {
            return Err(AppError::Locked(
                "one of these items is locked — unlock it to link".into(),
            ));
        }
        // ── Persist the directed manual edge (idempotent on the UNIQUE key). ──
        state
            .db
            .upsert_manual_link(src.as_str(), src_id, dst.as_str(), dst_id)?;
    }
    // ── Fork #2: a NOTE source ALSO gets the neighbour's [[Title]] in its body (best-effort). ──
    // A meeting/document source has no owned editable markdown → row only. `materialize_accepted_link`
    // re-gates the source folder itself before writing plaintext (never behind a lock).
    if matches!(src, crate::links::LinkKind::Note) {
        let unlocked = unlocked_snapshot(state)?;
        // Resolve the dst's CURRENT gated title (skip if sealed — no leak; the gate above already
        // proved it visible, so this normally resolves).
        if let Some(title) = state.db.link_endpoint_title_visible(dst, dst_id, &unlocked)? {
            // Reuse the EXACT accept-materialize path (managed `murmur:links` block, merge-preserving).
            if let Err(e) = materialize_accepted_link(state, src, src_id, &title) {
                // Best-effort: the graph row is authoritative; a markdown-write failure never fails the
                // link (no PII — ids/error only).
                tracing::warn!(
                    target: "links",
                    error = %e,
                    "manual link materialize skipped (row persisted)"
                );
            }
        }
    }
    tracing::info!(
        target: "links",
        src_kind = src.as_str(),
        dst_kind = dst.as_str(),
        "link_items"
    );
    Ok(())
}

/// note↔meeting-links PR-1 — REMOVE a user-initiated link: delete ONLY the directed `manual` edge
/// `(src → dst)` and, when the source is an OWNED note, strip the matching `[[dst Title]]` from its
/// managed `murmur:links` block. NEVER touches a `wikilink`/`companion`/`semantic` row for the pair.
///
/// GATE: BOTH endpoints must be session-VISIBLE (same gate as `link_items`) — never mutate a note's
/// body behind a lock, never reveal a locked neighbour's title. The strip is BEST-EFFORT (a failure
/// logs and never fails the unlink — the graph row is the authoritative removal). Unknown kinds are
/// `AppError::InvalidArg`.
#[tauri::command]
pub fn unlink_items(
    state: State<'_, AppState>,
    src_kind: String,
    src_id: String,
    dst_kind: String,
    dst_id: String,
) -> Result<(), AppError> {
    unlink_items_inner(state.inner(), &src_kind, &src_id, &dst_kind, &dst_id)
}

pub(crate) fn unlink_items_inner(
    state: &AppState,
    src_kind: &str,
    src_id: &str,
    dst_kind: &str,
    dst_id: &str,
) -> Result<(), AppError> {
    let src = parse_link_kind(src_kind)?;
    let dst = parse_link_kind(dst_kind)?;
    // SCOPE the guard around gate + row-delete only; release before the note-body strip below (which
    // re-enters the guard via `update_note_doc_inner`). Same non-reentrancy discipline as `link_items`.
    {
        let _lifecycle = lifecycle_guard(state);
        // ── GATE both endpoints (never mutate a note body / reveal a neighbour behind a lock). ──
        if !link_endpoint_is_unlocked(state, src, src_id)?
            || !link_endpoint_is_unlocked(state, dst, dst_id)?
        {
            return Err(AppError::Locked(
                "one of these items is locked — unlock it to unlink".into(),
            ));
        }
        // ── Delete ONLY the manual edge (wikilink/companion/semantic rows for the pair untouched). ──
        state
            .db
            .delete_manual_link(src.as_str(), src_id, dst.as_str(), dst_id)?;
    }
    // ── A NOTE source: strip the matching [[Title]] from its managed block (best-effort). ──
    if matches!(src, crate::links::LinkKind::Note) {
        let unlocked = unlocked_snapshot(state)?;
        if let Some(title) = state.db.link_endpoint_title_visible(dst, dst_id, &unlocked)? {
            if let Err(e) = strip_manual_link_marker(state, src_id, &title) {
                tracing::warn!(
                    target: "links",
                    error = %e,
                    "manual link marker strip skipped (row removed)"
                );
            }
        }
    }
    tracing::info!(
        target: "links",
        src_kind = src.as_str(),
        dst_kind = dst.as_str(),
        "unlink_items"
    );
    Ok(())
}

/// note↔meeting-links PR-1 — remove ONE `[[title]]` [`ContextHit`] from an owned note's managed
/// `murmur:links` block, re-applying the block with that hit filtered out (the INVERSE of
/// [`merge_related_hit`]). Reuses [`crate::enrich::extract_link_hits`] +
/// [`crate::enrich::apply_link_markers`] so the block stays idempotent and any auto related-notes /
/// accepted-semantic hits that lived alongside it survive. WRITE-GATED via `update_note_doc_inner`
/// (refuses a sealed folder). A no-op (the note is unchanged) when the block never carried the hit.
fn strip_manual_link_marker(state: &AppState, note_id: &str, title: &str) -> Result<(), AppError> {
    let Some(row) = state.db.get_note_row(note_id)? else {
        return Ok(()); // no owned note markdown → nothing to strip.
    };
    let marker = format!("[[{title}]]");
    let mut hits = crate::enrich::extract_link_hits(&row.text);
    let before = hits.len();
    hits.retain(|h| h.detail != marker);
    if hits.len() == before {
        return Ok(()); // the marker was not in the managed block → note unchanged.
    }
    let merged = crate::enrich::apply_link_markers(&row.text, &hits);
    if merged != row.text {
        let title_disp = row
            .title
            .clone()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| row.name.clone());
        update_note_doc_inner(state, note_id, &title_disp, &merged)?;
    }
    Ok(())
}

/// Brain v3 PR-3 — WRITE-TIME wikilink indexing hook, called from every note-save funnel AFTER the
/// text is durable. Resolves `[[Title]]` → TARGET IDS and (delete-then-insert) stores this source's
/// `wikilink` edges. BEST-EFFORT: a failure logs (ids/counts only, no PII) and never fails the save
/// — the plaintext is already the canonical copy. `src_kind` is `Meeting` for an AI note funnel,
/// `Note` for an authored-note funnel.
fn index_wikilinks_best_effort(
    state: &AppState,
    src_kind: crate::links::LinkKind,
    src_id: &str,
    body: &str,
) {
    let unlocked = match unlocked_snapshot(state) {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!(target: "links", error = %e, "wikilink index skipped (unlock snapshot)");
            return;
        }
    };
    if let Err(e) = state
        .db
        .index_wikilinks_for_source(src_kind, src_id, body, &unlocked)
    {
        tracing::warn!(target: "links", error = %e, "wikilink index failed (text saved)");
    }
}

/// Brain v3 PR-3 — SEMANTIC AUTO-LINK hook, called AFTER a successful REAL-embedder index of an item
/// (never on the stub — model-gated at the CALL SITE, mirroring the chunk-index gate). Suggests up to
/// `SEMANTIC_LINK_CAP` content-similar neighbours (mutual-kNN / floor / cap; see `auto_link_semantic`).
/// BEST-EFFORT: a failure logs (counts only) and never fails the caller. O(k·log n) — no corpus scan.
fn auto_link_semantic_best_effort(state: &AppState, kind: crate::links::LinkKind, id: &str) {
    // GUARD: only run with the real embedder present — a stub vector carries no meaning, so a
    // stub-space "neighbour" would be noise. (The chunk index itself already refuses stub vectors.)
    if !crate::embed::embed_model_present() {
        return;
    }
    let unlocked = match unlocked_snapshot(state) {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!(target: "links", error = %e, "semantic auto-link skipped (unlock snapshot)");
            return;
        }
    };
    if let Err(e) = state.db.auto_link_semantic(kind, id, &unlocked) {
        tracing::warn!(target: "links", error = %e, "semantic auto-link failed (index intact)");
    }
}

/// Brain v3 PR-3 — RE-DERIVE the link engine for every meeting + note in a JUST-UNSEALED folder
/// (their `links` rows were purged on seal). Re-runs the WIKILINK pass (resolved target ids, from the
/// restored body) + the SEMANTIC pass (model-gated inside) per item, against the supplied unlock set.
/// Called from the unlock restore closure; each item's index is BEST-EFFORT so one bad row never
/// aborts the whole unlock. Logs ids/counts only.
pub(crate) fn rederive_links_for_folder(
    state: &AppState,
    folder_id: &str,
    unlocked: &std::collections::HashSet<String>,
) {
    // Meetings in the folder → re-index each latest note's wikilinks + semantic neighbours.
    match state.db.meeting_ids_in_folder(folder_id) {
        Ok(mids) => {
            for mid in mids {
                if let Ok(Some(note)) = state.db.get_latest_note_for_meeting(&mid) {
                    if let Err(e) = state.db.index_wikilinks_for_source(
                        crate::links::LinkKind::Meeting,
                        &mid,
                        &note.markdown,
                        unlocked,
                    ) {
                        tracing::warn!(target: "links", error = %e, "unlock wikilink re-derive (meeting) failed");
                    }
                }
                if crate::embed::embed_model_present() {
                    if let Err(e) =
                        state.db.auto_link_semantic(crate::links::LinkKind::Meeting, &mid, unlocked)
                    {
                        tracing::warn!(target: "links", error = %e, "unlock semantic re-derive (meeting) failed");
                    }
                }
            }
        }
        Err(e) => tracing::warn!(target: "links", error = %e, "unlock link re-derive: meeting-id list failed"),
    }
    // Documents/notes in the folder → re-index authored notes' wikilinks + semantic neighbours (a raw
    // document has no wikilinks but still gets a semantic pass over its chunks).
    match state.db.document_ids_in_folder(folder_id) {
        Ok(dids) => {
            for did in dids {
                if let Ok(Some(row)) = state.db.get_note_row(&did) {
                    if let Err(e) = state.db.index_wikilinks_for_source(
                        crate::links::LinkKind::Note,
                        &did,
                        &row.text,
                        unlocked,
                    ) {
                        tracing::warn!(target: "links", error = %e, "unlock wikilink re-derive (note) failed");
                    }
                }
                if crate::embed::embed_model_present() {
                    if let Err(e) =
                        state.db.auto_link_semantic(crate::links::LinkKind::Note, &did, unlocked)
                    {
                        tracing::warn!(target: "links", error = %e, "unlock semantic re-derive (doc) failed");
                    }
                }
            }
        }
        Err(e) => tracing::warn!(target: "links", error = %e, "unlock link re-derive: document-id list failed"),
    }
    // Fix 2 (brain-v3 audit) — COMPANION leg: the note↔meeting `companion` edge is NOT re-derived by
    // the wikilink/semantic passes above (it comes from `documents.meeting_id`, not the body), is NOT
    // a preserved decision row, and is set only at the recording-time write site — so one lock cycle
    // permanently deletes it without this. Re-assert both legs (companion notes IN this folder → their
    // meetings, AND companion notes ANYWHERE → this folder's meetings, since a companion note can be
    // filed in a different folder). Best-effort; the DB helper skips any sealed-at-rest endpoint.
    match state.db.meeting_ids_in_folder(folder_id) {
        Ok(mids) => {
            if let Err(e) = state.db.rederive_companion_links_for_folder(folder_id, &mids) {
                tracing::warn!(target: "links", error = %e, "unlock companion re-derive failed");
            }
            // Fix 3 (brain-v3 audit) — INBOUND leg: re-index every OUTSIDE source whose body links
            // `[[title]]` INTO a just-unsealed item, so a link from a note that may never be edited
            // again is restored (the seal purged the edge from the sealed side; rederive only touched
            // F's OWN sources). Best-effort; Fix 0 keeps it from naming any still-sealed target.
            if let Err(e) =
                state.db.rederive_inbound_wikilinks_for_folder(folder_id, &mids, unlocked)
            {
                tracing::warn!(target: "links", error = %e, "unlock inbound wikilink re-derive failed");
            }
            // Fix 4 (brain-v3 audit, INVERSE): re-materialize the `[[Title]]` markers the seal stripped,
            // from the PRESERVED accepted rows (Fix 1) incident on the just-unlocked items, into the
            // source notes' managed blocks, then re-export those sources' `.md`. A wikilink/manual
            // marker re-materializes via the source's own body re-index above; an accepted SEMANTIC
            // marker has no body wikilink to re-derive from, so this explicit re-add restores it.
            match state
                .db
                .rematerialize_accepted_markers_for_folder(folder_id, &mids, unlocked)
            {
                Ok(changed) => reexport_stripped_marker_sources(state, &changed),
                Err(e) => tracing::warn!(target: "links", error = %e, "unlock accepted-marker re-materialize failed"),
            }
        }
        Err(e) => tracing::warn!(target: "links", error = %e, "unlock companion re-derive: meeting-id list failed"),
    }
}

/// Resolve a clicked `[[Title]]` wikilink to a VISIBLE note/meeting/org-item to navigate to.
/// Returns `None` when nothing matches OR the only local match is a
/// sealed-and-not-session-unlocked target (gated in `Db::resolve_wikilink`) — so a wikilink click
/// never reveals or opens locked content. The org leg (2026-07-15) is gated by membership +
/// per-instance-enabled, the same seam `list_link_candidates`/`search_org_brain_hits` use. The FE
/// routes on `kind` (`"meeting"` | `"note"` | `"org"`), or offers to create a note when `None`.
#[tauri::command]
pub fn resolve_wikilink(
    state: State<'_, AppState>,
    title: String,
) -> Result<Option<crate::storage::models::WikiTarget>, AppError> {
    let unlocked = unlocked_snapshot(state.inner())?;
    state.db.resolve_wikilink(&title, &unlocked)
}

/// Live keystroke-prefix candidates for the inline `[[` / slash-menu link-insertion autocomplete
/// (note-editor Fix 2). Distinct from [`resolve_wikilink`] (exact-title resolve on Enter/click) and
/// from `note_assistant_action`'s `find_related` (SELECTION+semantic retrieval — the wrong shape
/// for filtering on a short, growing keystroke prefix): this is a lightweight, gated title-prefix
/// scan. GATED exactly like every other content read: notes/meetings go through
/// `Db::list_link_candidates_visible` (`visibility_clause` on both legs, same as `resolve_wikilink`);
/// org items go through `search_org_brain_hits`, the SAME retrieval-only, membership+enabled-gated,
/// zero-egress reader `find_related` already uses (never a provider/egress call). Reuses
/// [`crate::storage::models::NoteCitation`] — the popover renders it exactly like a `find_related`
/// citation row, and `kind == "org"` carries an org item id (never a local id), matching that
/// contract verbatim.
///
/// PAGINATED (2026-07-17 — the picker is an infinite scroll over the whole vault now, not a fixed
/// top-8): one call returns the `limit`-sized page at `offset` of the stable combined ordering
/// [visible notes] ++ [visible meetings] ++ [org hits] (org only for a non-empty prefix, folded in
/// after the local total the Db reader reports). The FE owns its page size; the clamp keeps one
/// IPC reply from ever dumping an unbounded slice of the vault.
#[tauri::command]
pub fn list_link_candidates(
    state: State<'_, AppState>,
    prefix: String,
    offset: Option<u32>,
    limit: Option<u32>,
) -> Result<Vec<crate::storage::models::NoteCitation>, AppError> {
    const DEFAULT_PAGE: i64 = 40;
    const MAX_PAGE: i64 = 100;
    let limit = limit.map_or(DEFAULT_PAGE, i64::from).clamp(1, MAX_PAGE);
    let offset = i64::from(offset.unwrap_or(0));
    let unlocked = unlocked_snapshot(state.inner())?;
    let (mut out, local_total) =
        state
            .db
            .list_link_candidates_visible(&prefix, limit, offset, &unlocked)?;
    if (out.len() as i64) < limit {
        let config = {
            state
                .config
                .lock()
                .map_err(|_| AppError::Config("config mutex poisoned".into()))?
                .clone()
        };
        let q = prefix.trim();
        if !q.is_empty() && crate::tools::org_brain_available(&state.db, &config) {
            // Earlier pages consumed `local_total` local rows, then `offset - local_total`
            // org rows once the offset ran past the local legs — skip exactly those. The
            // org reader is bounded (≤20 per leg pre-fusion), so skip/take over its Vec
            // is real pagination, not a hidden re-scan.
            let org_skip = (offset - local_total).max(0) as usize;
            let remaining = (limit - out.len() as i64).max(0) as usize;
            let org_hits = crate::tools::search_org_brain_hits(&state.db, &config, q)?;
            for hit in org_hits.into_iter().skip(org_skip).take(remaining) {
                out.push(crate::storage::models::NoteCitation {
                    kind: "org".into(),
                    id: hit.item_id,
                    title: hit.title,
                    snippet: hit.snippet,
                });
            }
        }
    }
    Ok(out)
}

/// Inner of [`get_person_dossier`] taking `&AppState` (unit-testable gate). `None` — an unknown id
/// OR an entity visible only through sealed-not-unlocked meetings — maps to `InvalidArg`, mirroring
/// [`get_entity_detail`] so unknown-vs-sealed-only stays indistinguishable (no existence leak).
pub(crate) fn get_person_dossier_inner(
    state: &AppState,
    entity_id: &str,
) -> Result<crate::summarize::dossier::DossierData, AppError> {
    let unlocked = unlocked_snapshot(state)?;
    crate::summarize::dossier::build_dossier_data(&state.db, entity_id, &unlocked)?
        .ok_or_else(|| AppError::InvalidArg(format!("no visible entity with id {entity_id}")))
}

/// Ask-My-Vault: answer a question across ALL past meetings' notes (grounded, with sources).
///
/// PR G (ask-unify): on the CLOUD brain backend this routes through the SAME model-driven agentic
/// loop as the in-meeting assistant — a VAULT-SCOPED gated executor (no meeting, read-only, NO
/// `propose_note`; web/calendar participate under their existing consent/availability gates) with
/// the live tool-trace on `EVENT_ASK_TOOL`. On the local/off backend, loop non-convergence, or ANY
/// loop error (incl. `Unavailable` = no cloud consent) it falls through to THE FLOOR
/// ([`ask_vault_floor`]) — exactly the pre-agentic corpus-pack + one-completion path with its
/// original error/consent semantics. `ask_thread_id` (FE camelCase `askThreadId`) is the OPTIONAL
/// thread identity stamped on every trace chip; absent ⇒ backend-generated (UUID v4).
#[tauri::command]
pub async fn ask_vault(
    app: AppHandle,
    state: State<'_, AppState>,
    question: String,
    history: Vec<ChatTurn>,
    ask_thread_id: Option<String>,
    explicit_sources: Option<Vec<crate::storage::models::SourceRef>>,
) -> Result<AskVaultResult, AppError> {
    if question.trim().is_empty() {
        return Err(AppError::InvalidArg("question is empty".into()));
    }
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    // The same 12-message discipline as the chat panel (CHAT_CONTEXT_TURNS): bounds prompt growth +
    // cloud egress on BOTH paths. The LATEST question still drives retrieval either way.
    let history: Vec<ChatTurn> = capped_ask_history(&history).to_vec();

    // note↔meeting-links PR-2 — SOURCE-SCOPED (pinned) Ask. When the FE source picker sends a
    // non-empty explicit source list, retrieval is PINNED to exactly those items (+ their capped,
    // gated link-expansion) and answered DETERMINISTICALLY via the SAME floor answer path — the
    // agentic vault-wide search is SKIPPED (the user controls the context, so a scoped Ask never
    // pulls unlisted vault items). `None`/empty ⇒ this whole block is a no-op and the path below is
    // BYTE-IDENTICAL to before.
    let pinned_sources: Option<Vec<crate::storage::models::SourceRef>> =
        explicit_sources.filter(|s| !s.is_empty());
    if let Some(sources) = pinned_sources {
        let unlocked = unlocked_snapshot(state.inner())?;
        let memory_brief = gated_memory_brief_for_injection(state.inner(), &unlocked, &question);
        let reranker = crate::rerank::active_reranker(
            state
                .reasoner
                .current_for(crate::summarize::roles::Role::Ask),
        );
        return ask_vault_floor(
            &state.db,
            &config,
            &unlocked,
            &question,
            &history,
            &memory_brief,
            Some(reranker),
            &state.heavy_inference,
            Some(sources),
        )
        .await;
    }

    // Agentic path — CLOUD-connection roles only (the same rule as the in-meeting brain:
    // local-GGUF multi-step tool-call reliability is unproven). The eligibility gate keys on the
    // ASK role's resolved target: with role keys absent, `!is_reasoner_only()` is EXACTLY the
    // legacy `brain_backend == Cloud` predicate (Cloud → provider connection, Local → "local",
    // Off → "off"). The loop is blocking (scoped-thread connectors), so it runs on a blocking
    // thread like `ask_assistant_chat`.
    if !crate::summarize::roles::resolve(crate::summarize::roles::Role::Ask, &config)
        .is_reasoner_only()
    {
        let thread_id = crate::transcribe::live::ensure_thread_id(ask_thread_id);
        let handle = app.clone();
        let q = question.clone();
        let h = history.clone();
        let attempt = tokio::task::spawn_blocking(move || {
            ask_vault_agentic_attempt(&handle, &q, &h, &thread_id)
        })
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("ask task join failed: {e}")))?;
        if let Some(result) = attempt {
            return Ok(result);
        }
    }

    // THE FLOOR — the pre-agentic behavior, unchanged (RED-first equivalence-tested).
    // Pass the LIVE session unlock set (E9): a folder the user has session-unlocked is included
    // again, while sealed-and-NOT-unlocked content stays excluded by the same visibility predicate.
    let unlocked = unlocked_snapshot(state.inner())?;
    // Gated cross-meeting USER MEMORY brief (parity with the @brain loop): VISIBLE facts only under
    // this same unlock snapshot, empty when memory is disabled ⇒ the floor prompt is byte-identical.
    // L2.2: relevance-filtered against the question (full-list fallback on zero hits).
    let memory_brief = gated_memory_brief_for_injection(state.inner(), &unlocked, &question);
    // Brain v2 L1.4 — the Ask-only reranker: resolve from the LIVE Ask-role reasoner.
    // `active_reranker` degrades stub/cloud reasoners to the identity StubReranker (rerank is
    // strictly on-device — a cloud reasoner would turn each pointwise judgment into egress).
    let reranker = crate::rerank::active_reranker(
        state
            .reasoner
            .current_for(crate::summarize::roles::Role::Ask),
    );
    ask_vault_floor(
        &state.db,
        &config,
        &unlocked,
        &question,
        &history,
        &memory_brief,
        Some(reranker),
        &state.heavy_inference,
        None, // whole-vault path: no explicit sources ⇒ the existing search corpus, unchanged.
    )
    .await
}






/// The floor's prompt assembly, split from the provider call so the floor-equivalence test can
/// prove it byte-identical to the pre-agentic implementation without a live provider.
pub(crate) enum AskFloorPrompt {
    /// Nothing to search — the canned early-return result (identical to the pre-change string).
    Empty(AskVaultResult),
    /// The assembled corpus prompt, ready for ONE provider completion.
    Ready {
        system: String,
        user: String,
        sources: Vec<crate::storage::models::VaultSource>,
    },
}


/// THE FLOOR — the pre-agentic Ask-My-Vault implementation: gated corpus pack + ONE provider
/// completion, with the original error/consent semantics (`make_provider`'s fail-closed consent
/// gate errors exactly as before). Runs on the local/off brain backend and whenever the agentic
/// attempt did not converge or errored.
#[allow(clippy::too_many_arguments)] // cohesive gated-Ask surface: corpus/consent state + the heavy-inference permit.
async fn ask_vault_floor(
    db: &std::sync::Arc<crate::storage::Db>,
    config: &AppConfig,
    unlocked: &std::collections::HashSet<String>,
    question: &str,
    history: &[ChatTurn],
    memory_brief: &str,
    reranker: Option<Box<dyn crate::rerank::Reranker>>,
    heavy: &std::sync::Arc<tokio::sync::Semaphore>,
    explicit_sources: Option<Vec<crate::storage::models::SourceRef>>,
) -> Result<AskVaultResult, AppError> {
    // `build_ask_vault_floor_prompt` does the LOCAL/on-device work — query embedding (Candle/
    // Metal) + hybrid FTS∪vector retrieval + reranker inference — synchronously. This is exactly
    // the path privacy-first users hit (a local/reasoner-only Ask role skips the agentic branch
    // above and lands here), and it used to run INLINE on this async command's Tokio worker
    // (2026-07-13 perf audit, HIGH severity — the local path for exactly the users this app
    // targets was the one NOT already spawn_blocking'd, unlike the agentic/cloud branch above).
    // Everything captured here is owned (`Arc<Db>` clone, owned `AppConfig`/`HashSet`/`String`s,
    // a `Send + Sync` boxed reranker) so the closure is `'static` with no AppHandle re-fetch
    // needed — this function doesn't otherwise need one.
    let db_for_prompt = db.clone();
    let config_for_prompt = config.clone();
    let unlocked_for_prompt = unlocked.clone();
    let question_owned = question.to_string();
    let history_owned = history.to_vec();
    let memory_brief_owned = memory_brief.to_string();
    let prompt = tokio::task::spawn_blocking(move || {
        build_ask_vault_floor_prompt(
            &db_for_prompt,
            &config_for_prompt,
            &unlocked_for_prompt,
            &question_owned,
            &history_owned,
            &memory_brief_owned,
            reranker.as_deref(),
            explicit_sources.as_deref(),
        )
    })
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("ask-vault-floor task panicked: {e}")))??;
    match prompt {
        AskFloorPrompt::Empty(result) => Ok(result),
        AskFloorPrompt::Ready {
            system,
            user,
            sources,
        } => {
            // ASK role. With role keys absent this builds the legacy default provider for EVERY
            // brain_backend (the pre-role floor ignored it) — original error/consent semantics.
            let provider =
                crate::summarize::provider_for(crate::summarize::roles::Role::Ask, config, heavy)?;
            let answer = provider.complete(&system, &user).await?;
            Ok(AskVaultResult {
                answer,
                sources,
                citations: Vec::new(),
            })
        }
    }
}

#[cfg(test)]
#[path = "tests/ask_vault_tests.rs"]
mod ask_vault_tests;

/// Entity DOSSIER (brain2 Phase 5b): synthesize the "state of [[entity]]" across all meetings —
/// Overview · 🕑 Timeline of mentions · ⏳ Open commitments · 🧭 Last said / next step, every claim
/// citing its [[Title]]. `entity` is an entity id (from `get_graph`) OR a name. The dossier data is
/// assembled through the SAME visibility gate as Ask-My-Vault (sealed-not-unlocked meetings
/// contribute nothing), then synthesized by the configured provider — so this is a CLOUD-egress
/// path that goes through the redaction firewall + consent gate (E6/E7/E10) exactly like `ask_vault`.
///
/// B2 (Shared Brain, READ-ONLY): when the caller has joined an org, ALSO searches the org partition
/// for this entity and folds `[org · author]`-labelled hits into the SYNTHESIS PROMPT ONLY as
/// additional citable context — an entity with ZERO local facts/mentions can still resolve here
/// purely from org content. This NEVER calls `build_and_persist_entities` and NEVER writes anything
/// derived from org content into `entities`/`entity_mentions`/`facts` — those tables are untouched
/// by this path. `has_org_context` on the response is the honest signal that org-sourced content
/// contributed, so the caller never silently blends a colleague's claims with the user's own
/// verified facts.
#[tauri::command]
pub async fn entity_dossier(
    state: State<'_, AppState>,
    entity: String,
) -> Result<EntityDossierResult, AppError> {
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    // Pass the LIVE session unlock set (E9): a folder the user has session-unlocked is included
    // again; sealed-and-NOT-unlocked content stays excluded by the same visibility predicate.
    let unlocked = unlocked_snapshot(state.inner())?;
    let notes_conn =
        crate::summarize::roles::provider_target(crate::summarize::roles::Role::Notes, &config)
            .connection;
    let (system, user, has_org_context) =
        build_entity_dossier_prompt(&state.db, &entity, &unlocked, &config, &notes_conn)?;
    // Build the provider (firewall + consent gate) BEFORE synthesizing — the factory refuses a
    // cloud provider until the user has consented to egress. NOTES role (the dossier is a
    // written synthesis); the corpus budget keys on the same resolved connection.
    let provider = crate::summarize::provider_for(crate::summarize::roles::Role::Notes, &config, &state.heavy_inference)?;
    let markdown = provider.complete(&system, &user).await?;
    Ok(EntityDossierResult {
        markdown,
        has_org_context,
    })
}

#[cfg(test)]
#[path = "tests/entity_dossier_tests.rs"]
mod entity_dossier_tests;

/// Generate a Weekly Vault Digest synthesizing meetings from the last `days` days; writes it
/// into the vault's Digests/ folder and returns the markdown + path.
#[tauri::command]
pub async fn generate_digest(
    state: State<'_, AppState>,
    days: i64,
) -> Result<DigestResult, AppError> {
    let days = days.clamp(1, 90);
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
    // Budget on the NOTES-role provider's RESOLVED connection — the corpus egresses to it
    // (identical to `provider_id` while role keys are absent).
    let notes_conn =
        crate::summarize::roles::provider_target(crate::summarize::roles::Role::Notes, &config)
            .connection;
    let budget = if notes_conn == "ollama" {
        4_000
    } else {
        80_000
    };
    // Finding 2 + BLK-2b: build the cloud corpus from VISIBLE meetings + VISIBLE notes only, so a
    // sealed-and-not-unlocked meeting's TITLE (the `### [[title]]` header) AND markdown never leave
    // the device. `list_meetings_visible` + `get_note_if_visible` push the session unlock set
    // through the same predicate as MCP — correctness no longer depends on at-rest blanking.
    let unlocked = unlocked_snapshot(state.inner())?;
    let mut corpus = String::new();
    let mut count = 0usize;
    for m in state.db.list_meetings_visible(300, &unlocked)? {
        if m.started_at.as_str() < cutoff.as_str() {
            continue;
        }
        if corpus.len() >= budget {
            break;
        }
        let Some(note) = state.db.get_note_if_visible(&m.id, &unlocked)? else {
            continue;
        };
        let title = m.title.clone().unwrap_or_else(|| "(untitled)".to_string());
        let date = m
            .started_at
            .split(['T', ' '])
            .next()
            .unwrap_or("")
            .to_string();
        let header = format!("\n\n### [[{title}]] · {date}\n");
        let remaining = budget.saturating_sub(corpus.len() + header.len());
        if remaining < 200 {
            break;
        }
        corpus.push_str(&header);
        corpus.push_str(&note.markdown.chars().take(remaining).collect::<String>());
        count += 1;
    }
    if count == 0 {
        return Err(AppError::InvalidArg(format!(
            "no summarized meetings in the last {days} days"
        )));
    }
    let range_label = format!("the last {days} days");
    let provider = crate::summarize::provider_for(crate::summarize::roles::Role::Notes, &config, &state.heavy_inference)?;
    let (system, user) =
        crate::summarize::digest::build_digest_prompt(&corpus, &range_label, &config.note_language);
    let markdown = provider.complete(&system, &user).await?;

    let exported_path = match config.vault_path.as_deref().filter(|p| !p.is_empty()) {
        Some(vault) => {
            let now = chrono::Utc::now().to_rfc3339();
            crate::export::write_note(
                std::path::Path::new(vault),
                Some("Digests"),
                "Weekly Digest",
                &now,
                &markdown,
            )
            .ok()
            .map(|p| p.to_string_lossy().to_string())
        }
        None => None,
    };
    Ok(DigestResult {
        markdown,
        exported_path,
    })
}

// ── Brain v2 L5 — scheduled briefs (schedule CRUD + propose-accept runs) ─────────────────────────

/// All brief schedules (config rows — labels, timing, hints; no meeting content).
#[tauri::command]
pub fn list_brief_schedules(
    state: State<'_, AppState>,
) -> Result<Vec<crate::storage::models::BriefSchedule>, AppError> {
    state.db.list_brief_schedules()
}

/// Validate a brief schedule's user-editable fields (shared by create + update).
fn validate_brief_schedule(s: &crate::storage::models::BriefSchedule) -> Result<(), AppError> {
    if s.label.trim().is_empty() {
        return Err(AppError::InvalidArg("brief label is empty".into()));
    }
    if let Some(d) = s.day_of_week {
        if !(0..=6).contains(&d) {
            return Err(AppError::InvalidArg(
                "day_of_week must be 0 (Monday) … 6 (Sunday)".into(),
            ));
        }
    }
    if !(0..=23).contains(&s.hour_local) {
        return Err(AppError::InvalidArg("hour must be 0…23".into()));
    }
    if !(0..=59).contains(&s.minute_local) {
        return Err(AppError::InvalidArg("minute must be 0…59".into()));
    }
    if !(1..=90).contains(&s.scope_days) {
        return Err(AppError::InvalidArg("scope_days must be 1…90".into()));
    }
    Ok(())
}

/// Create one brief schedule. `day_of_week`: 0 = Monday … 6 = Sunday, `None` = daily. The runner
/// (`crate::brief_runner`) fires it at most once per local day; the first fire is the first 60s
/// tick at/after `hour:minute` local.
#[tauri::command]
pub fn create_brief_schedule(
    state: State<'_, AppState>,
    label: String,
    day_of_week: Option<i64>,
    hour_local: i64,
    minute_local: i64,
    scope_days: Option<i64>,
    prompt_hint: Option<String>,
) -> Result<crate::storage::models::BriefSchedule, AppError> {
    let schedule = crate::storage::models::BriefSchedule {
        id: uuid::Uuid::new_v4().simple().to_string(),
        label: label.trim().to_string(),
        day_of_week,
        hour_local,
        minute_local,
        scope_days: scope_days.unwrap_or(7),
        prompt_hint: prompt_hint.map(|h| h.trim().to_string()).filter(|h| !h.is_empty()),
        enabled: true,
        last_run_at: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    validate_brief_schedule(&schedule)?;
    state.db.insert_brief_schedule(&schedule)?;
    Ok(schedule)
}

/// Update one brief schedule's editable fields (label / timing / window / hint / enabled).
#[tauri::command]
pub fn update_brief_schedule(
    state: State<'_, AppState>,
    schedule: crate::storage::models::BriefSchedule,
) -> Result<(), AppError> {
    validate_brief_schedule(&schedule)?;
    state.db.update_brief_schedule(&schedule)
}

/// Delete one brief schedule AND its staged runs.
#[tauri::command]
pub fn delete_brief_schedule(
    state: State<'_, AppState>,
    schedule_id: String,
) -> Result<(), AppError> {
    state.db.delete_brief_schedule(&schedule_id)
}

/// The PENDING (proposed, not yet accepted/dismissed) brief runs — the FE's proposal cards.
/// `note_md` was synthesized by the runner from VISIBLE-ONLY content (empty unlock set — the
/// consolidation-job discipline), so it cannot contain sealed content AT synthesis time; and a
/// meeting sealed AFTER the proposal purges its pending runs inside the seal tx
/// (`Db::purge_pending_brief_runs_tx` — the lock-security LEAK fix, 2026-07-10), so a row this
/// returns never paraphrases a currently-sealed meeting. That pair is what makes this read safe
/// without a per-meeting gate (documented posture, see `crate::brief_runner` + `migrate_briefs`).
#[tauri::command]
pub fn list_brief_runs(
    state: State<'_, AppState>,
) -> Result<Vec<crate::storage::models::BriefRun>, AppError> {
    state.db.list_pending_brief_runs()
}

/// ACCEPT a proposed brief: export its markdown to `<vault>/Briefs/` (atomic write) and CONSUME
/// the staged `note_md` (the vault `.md` becomes the only copy). Returns the exported path.
#[tauri::command]
pub fn accept_brief(state: State<'_, AppState>, run_id: String) -> Result<String, AppError> {
    let run = state
        .db
        .get_brief_run(&run_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no brief run {run_id}")))?;
    if run.status != "pending" {
        return Err(AppError::InvalidArg("this brief was already handled".into()));
    }
    if run.note_md.trim().is_empty() {
        return Err(AppError::InvalidArg("this brief has no content".into()));
    }
    let vault = vault_path(state.inner())
        .ok_or_else(|| AppError::InvalidArg("set an Obsidian vault first (Settings)".into()))?;
    let label = state
        .db
        .list_brief_schedules()?
        .into_iter()
        .find(|s| s.id == run.schedule_id)
        .map(|s| s.label)
        .unwrap_or_else(|| "Brief".to_string());
    let path = crate::export::write_note(
        std::path::Path::new(&vault),
        Some("Briefs"),
        &label,
        &run.proposed_at,
        &run.note_md,
    )?;
    state
        .db
        .accept_brief_run(&run_id, &chrono::Utc::now().to_rfc3339())?;
    Ok(path.to_string_lossy().to_string())
}

/// DISMISS a proposed brief: the staged row (markdown included) is deleted outright.
#[tauri::command]
pub fn dismiss_brief(state: State<'_, AppState>, run_id: String) -> Result<(), AppError> {
    state.db.delete_brief_run(&run_id)
}

// ── Brain v2 L5 — MCP server config (list/add/remove + per-server consent + test) ───────────────
//
// The list/add/remove + per-server consent commands were extracted verbatim to `commands/mcp.rs`
// (a NON-content-gated config domain); `test_mcp_server` (the async connectivity probe) stays here.

/// TEST a configured MCP server: JSON-RPC `initialize` + `tools/list`, returning the discovered
/// tool COUNT (never the tool metadata — server-supplied descriptions are untrusted input and stay
/// out of the FE/prompts). CONSENT-GATED like every other egress to the server: an unconsented or
/// disabled server refuses BEFORE any connection — for a stdio server the test LAUNCHES the
/// configured binary, so consent must come first.
#[tauri::command]
pub async fn test_mcp_server(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<usize, AppError> {
    let server = state
        .db
        .get_mcp_server(&server_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no MCP server {server_id}")))?;
    if !server.enabled || !server.consented {
        return Err(AppError::Unavailable(
            "this MCP server needs your one-time consent first".into(),
        ));
    }
    let client = crate::connectors::mcp_client::McpClient::for_server(&server)
        .ok_or_else(|| AppError::InvalidArg("invalid MCP server configuration".into()))?;
    // Content-free egress ledger row for the ATTEMPT, recorded BEFORE the handshake like every
    // connector egress — a failing test connection is still attempted egress (for a stdio server
    // the probe launches the configured binary), and the privacy receipt must show it
    // (lock-security WEAKNESS fix, 2026-07-10). No query text exists here; the row carries only
    // the per-server attribution (see `mcp_probe_entry`).
    crate::summarize::egress_log::active_sink()
        .record(crate::summarize::egress_log::mcp_probe_entry(&server.id));
    client.initialize().await.map_err(AppError::from)?;
    let tools = client.list_tools().await.map_err(AppError::from)?;
    Ok(tools.len())
}

/// Topic Threads: cluster the per-meeting topic spans (from cached timelines) across the whole
/// library into cross-meeting threads. Deterministic, no LLM. Only meetings whose timeline has
/// been generated (viewed at least once) contribute.
#[tauri::command]
pub fn topic_threads(state: State<'_, AppState>) -> Result<Vec<TopicThread>, AppError> {
    // BLK-2b: cluster only VISIBLE meetings' timelines — a sealed-and-not-unlocked meeting's
    // timeline `data` is blanked at rest, but gate on visibility so threads never depend on that
    // blanking (and a sealed meeting's topics never surface cross-meeting).
    let unlocked = unlocked_snapshot(state.inner())?;
    let mut input = Vec::new();
    for m in state.db.list_meetings_visible(500, &unlocked)? {
        let Some(json) = state.db.get_timeline_data(&m.id)? else {
            continue;
        };
        let Ok(tl) = serde_json::from_str::<MeetingTimeline>(&json) else {
            continue;
        };
        if tl.topics.is_empty() {
            continue;
        }
        input.push(crate::summarize::threads::MeetingTopics {
            meeting_id: m.id,
            title: m.title.unwrap_or_else(|| "(untitled)".to_string()),
            started_at: m.started_at,
            topics: tl
                .topics
                .iter()
                .map(|t| (t.label.clone(), t.start_s, t.end_s))
                .collect(),
        });
    }
    Ok(crate::summarize::threads::build_threads(&input))
}

/// Best-effort: the soonest macOS Calendar event in the next 60 minutes (title only). Returns
/// None if Calendar access is denied or there's nothing upcoming — never errors the UI.
#[tauri::command]
pub async fn next_calendar_event() -> Result<Option<CalendarEvent>, AppError> {
    let script = r#"set now to (current date)
set laterT to now + (60 * minutes)
set out to ""
try
  tell application "Calendar"
    repeat with c in calendars
      repeat with e in (every event of c whose start date is greater than or equal to now and start date is less than or equal to laterT)
        set out to out & (summary of e) & linefeed
      end repeat
    end repeat
  end tell
end try
return out"#;
    let res = tokio::task::spawn_blocking(move || {
        std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()
    })
    .await;
    let stdout = match res {
        Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return Ok(None),
    };
    let title = stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string);
    Ok(title.map(|title| CalendarEvent { title, start: None }))
}

/// CALENDAR source (local, zero-OAuth, on-device): list the user's events in a window around now
/// via the bundled `meetnotes-calendar` EventKit sidecar — title, attendees, agenda. GRACEFUL on
/// every failure: sidecar missing / Calendar permission denied / timeout / malformed output →
/// an empty list, never an error, never a block. No network egress: reading the local calendar
/// stays on device.
#[tauri::command]
pub async fn list_calendar_events(app: AppHandle) -> Result<Vec<CalendarEventFull>, AppError> {
    // Default window: now-1h .. now+12h (60 back, 720 forward minutes).
    Ok(crate::calendar::fetch_events(&app, 60, 720).await)
}

/// Build a compact [`CalendarContext`] (title + attendees + agenda) for one event so the existing
/// pre-meeting brief / note pre-analysis can consume it (the brain already takes context). Looks
/// the event up by id in the same window the sidecar surfaces. Returns `None` if the event isn't
/// found (expired from the window, or Calendar access denied) — never an error.
///
/// IMPORTANT: the returned text is on-device context. If it is later fed to a CLOUD provider it
/// MUST ride the existing `make_provider` redaction firewall + consent (the same path the
/// transcript takes) — this command opens NO new egress path.
#[tauri::command]
pub async fn calendar_context_for(
    app: AppHandle,
    event_id: String,
) -> Result<Option<CalendarContext>, AppError> {
    if event_id.trim().is_empty() {
        return Err(AppError::InvalidArg("event_id is empty".into()));
    }
    let events = crate::calendar::fetch_events(&app, 60, 720).await;
    Ok(events
        .iter()
        .find(|e| e.id == event_id)
        .map(CalendarContext::from_event))
}

/// Read current config (settings table), without secrets.
#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> Result<AppConfigDto, AppError> {
    let config = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    Ok(config_to_dto(&config))
}

/// Build the ready-to-paste Claude Code MCP config block for the localhost MCP server, WITH the
/// bearer token when `mcp_require_token` is on (the default). Fixes the handshake failure where
/// the copied snippet had no token so Claude Code got `-32001 unauthorized` on `initialize`.
///
/// Shape (pretty-printed, 2-space indent — `type: "http"` is required by Claude Code):
/// ```json
/// {
///   "mcpServers": {
///     "murmur": {
///       "type": "http",
///       "url": "http://127.0.0.1:8765",
///       "headers": { "Authorization": "Bearer <token>" }
///     }
///   }
/// }
/// ```
/// When `mcp_require_token` is OFF the server serves unauthenticated, so the `headers` block is
/// omitted entirely (no stale/placeholder Authorization header is ever emitted).
///
/// SECURITY: this surfaces the MCP bearer token to the LOCAL frontend for display in Settings so
/// the user can paste it into their own Claude Code config. It is NOT network egress — the token
/// never leaves the machine except by the user's manual copy. When the token flag is off no token
/// is read at all. Lock-security review requested.
#[tauri::command]
pub fn get_mcp_config(state: State<'_, AppState>) -> Result<String, AppError> {
    get_mcp_config_inner(state.inner())
}

/// Headless core of [`get_mcp_config`] — testable without a Tauri `State`.
pub(crate) fn get_mcp_config_inner(state: &AppState) -> Result<String, AppError> {
    // Read the flag the same way lib.rs does: fail CLOSED (require the token) on a poisoned lock,
    // so a broken config never emits an unauthenticated config for a server that DOES enforce.
    let require_token = state
        .config
        .lock()
        .map(|c| c.mcp_require_token)
        .unwrap_or(true);

    let url = format!("http://127.0.0.1:{}", crate::mcp::MCP_PORT);
    let mut server = serde_json::json!({
        "type": "http",
        "url": url,
    });
    if require_token {
        // Only read/mint the token when the server actually enforces it. On a keychain failure this
        // returns AppError (the FE shows an error) — never a config with an empty/placeholder token.
        let token = crate::secrets::get_or_create_mcp_token()?;
        server["headers"] = serde_json::json!({
            "Authorization": format!("Bearer {token}"),
        });
    }
    let config = serde_json::json!({
        "mcpServers": { "murmur": server },
    });
    serde_json::to_string_pretty(&config)
        .map_err(|e| AppError::Other(anyhow::anyhow!("serialize MCP config: {e}")))
}

/// Persist config to settings table + refresh in-memory cache. Does NOT touch Keychain.
#[tauri::command]
pub fn save_config(
    app: AppHandle,
    state: State<'_, AppState>,
    config: AppConfigDto,
) -> Result<(), AppError> {
    save_config_inner(state.inner(), config)?;
    // Enforce the cap immediately when a save leaves auto-prune ON with a cap set (e.g. the
    // user just lowered the limit). Best-effort; the config lock is already released. Runs
    // BEFORE `restart_voice_listener` (which consumes `app`) so `app.emit` stays valid.
    let (limit_gb, auto) = match state.config.lock() {
        Ok(c) => (c.audio_storage_limit_gb, c.audio_auto_prune),
        Err(_) => (None, false),
    };
    if let Ok(dir) = crate::pipeline::audio_dir() {
        // Hold the seal lifecycle guard across the prune (config lock already released above, so
        // the lock order is lifecycle ⊃ db, never config held while holding lifecycle) so the
        // prune can never interleave with a folder seal.
        let _lifecycle = lifecycle_guard(state.inner());
        if let Ok(s) = crate::storage::usage::maybe_prune(&state.db, &dir, limit_gb, auto, None) {
            if s.freed_bytes > 0 {
                let _ = app.emit(
                    crate::events::EVENT_STORAGE_PRUNED,
                    crate::events::StoragePrunedPayload {
                        freed_bytes: s.freed_bytes,
                        pruned_count: s.pruned_count,
                    },
                );
            }
        }
    }
    // Reconcile the voice-trigger listener with the new config (AppHandle-dependent; runs after
    // the config lock is released by `save_config_inner`).
    restart_voice_listener(app);
    Ok(())
}

/// Headless core of [`save_config`]: validate, merge, persist, and refresh the in-memory cache.
/// No `AppHandle`/event emission here, so it is unit-testable without Tauri.
pub(crate) fn save_config_inner(state: &AppState, config: AppConfigDto) -> Result<(), AppError> {
    // Validate the gateway URL eagerly, before persisting, so a malformed or credential-bearing
    // URL (`https://key:@host/v1`) is never stored in the plaintext settings row and never
    // round-trips to the FE. An empty URL is allowed (no gateway configured).
    if !config.gateway_base_url.trim().is_empty() {
        crate::summarize::gateway::validate_gateway_url(&config.gateway_base_url)?;
    }
    // M3-CLIENT: validate the sharing-server URL at the same seam (reject embedded creds / http on a
    // non-loopback host) so a bad value is refused BEFORE it is persisted. Empty is allowed (unset).
    if !config.share_base_url.trim().is_empty() {
        crate::summarize::gateway::validate_gateway_url(&config.share_base_url)?;
    }

    // Merge against the CURRENT config under the config lock so the security-sensitive flags that
    // save_config must NOT be able to flip from the DTO (BLK-4: cloud_egress_consented) are read
    // from the live value, not the incoming payload. Holding the guard across the merge+save+swap
    // makes it atomic w.r.t. a concurrent `consent_to_cloud_egress`.
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    let new_config = dto_to_config(config, &cache);
    new_config.save(&state.db)?;
    *cache = new_config;
    Ok(())
}

fn config_to_dto(c: &AppConfig) -> AppConfigDto {
    AppConfigDto {
        provider_id: c.provider_id.clone(),
        vault_path: c.vault_path.clone(),
        vault_subfolder: c.vault_subfolder.clone(),
        whisper_model_path: c.whisper_model_path.clone(),
        language: c.language.clone(),
        anthropic_model: c.anthropic_model.clone(),
        provider_model: c.provider_model.clone(),
        provider_effort: c.provider_effort.clone(),
        ollama_base_url: c.ollama_base_url.clone(),
        ollama_model: c.ollama_model.clone(),
        claude_binary: c.claude_binary.clone(),
        input_device: c.input_device.clone(),
        capture_system_audio: c.capture_system_audio,
        vad_enabled: c.vad_enabled,
        keep_hires_masters: c.keep_hires_masters,
        diarize_others: c.diarize_others,
        voiceprint_enabled: c.voiceprint_enabled,
        aec_enabled: c.aec_enabled,
        post_aec_enabled: c.post_aec_enabled,
        audio_storage_limit_gb: c.audio_storage_limit_gb,
        audio_auto_prune: c.audio_auto_prune,
        model_size: c.model_size.clone(),
        live_asr_engine: c.live_asr_engine.clone(),
        brain_idle_timeout_secs: c.brain_idle_timeout_secs,
        brain_ready_timeout_secs: c.brain_ready_timeout_secs,
        brain_hard_cap_secs: c.brain_hard_cap_secs,
        voice_trigger: c.voice_trigger,
        onboarded: c.onboarded,
        note_style: c.note_style.clone(),
        notes_mode: c.notes_mode.clone(),
        auto_organize: c.auto_organize,
        note_assist_refine: c.note_assist_refine,
        note_assist_shorten: c.note_assist_shorten,
        note_assist_enhance: c.note_assist_enhance,
        note_assist_actions_off: c.note_assist_actions_off.clone(),
        note_language: c.note_language.clone(),
        mcp_require_token: c.mcp_require_token,
        lock_require_biometric: c.lock_require_biometric,
        relock_on_screenshare: c.relock_on_screenshare,
        cloud_egress_consented: c.cloud_egress_consented,
        // DISPLAY-ONLY out (M3-CLIENT): FE shows share-egress consent status; cannot set it back.
        share_egress_consented: c.share_egress_consented,
        // DISPLAY-ONLY out (M6): FE shows org-egress consent status; cannot set it back.
        org_egress_consented: c.org_egress_consented,
        // DISPLAY-ONLY out: the init gateway reads this to know whether the first-run sharing
        // choice was already made; the FE cannot set it back (preserved in `dto_to_config`).
        sharing_choice_made: c.sharing_choice_made,
        brain_backend: c.brain_backend,
        realtime_reactions: c.realtime_reactions,
        brain_model_id: c.brain_model_id.clone(),
        brain_model_path: c.brain_model_path.clone(),
        semantic_search_enabled: c.semantic_search_enabled,
        web_search_enabled: c.web_search_enabled,
        // DISPLAY-ONLY out: lets the FE show "consented" status; the FE cannot set it back (preserved
        // in `dto_to_config`).
        web_search_consented: c.web_search_consented,
        jira_enabled: c.jira_enabled,
        // DISPLAY-ONLY out: lets the FE show "consented" status; the FE cannot set it back (preserved
        // in `dto_to_config`).
        jira_consented: c.jira_consented,
        jira_base_url: c.jira_base_url.clone(),
        jira_email: c.jira_email.clone(),
        slack_enabled: c.slack_enabled,
        // DISPLAY-ONLY out: lets the FE show "consented" status; the FE cannot set it back (preserved
        // in `dto_to_config`).
        slack_consented: c.slack_consented,
        claude_code_inherit_env: c.claude_code_inherit_env,
        gateway_base_url: c.gateway_base_url.clone(),
        gateway_model: c.gateway_model.clone(),
        share_base_url: c.share_base_url.clone(),
        proactive_hints_enabled: c.proactive_hints_enabled,
        user_memory_enabled: c.user_memory_enabled,
        role_notes_connection: c.role_notes_connection.clone(),
        role_notes_model: c.role_notes_model.clone(),
        role_notes_effort: c.role_notes_effort.clone(),
        role_ask_connection: c.role_ask_connection.clone(),
        role_ask_model: c.role_ask_model.clone(),
        role_ask_effort: c.role_ask_effort.clone(),
        role_live_connection: c.role_live_connection.clone(),
        role_live_model: c.role_live_model.clone(),
        role_live_effort: c.role_live_effort.clone(),
    }
}

/// Build the persisted `AppConfig` from an incoming settings DTO, merged against the `current`
/// config. Every plain field comes from the DTO (the Settings UI is authoritative for them), but
/// the security-sensitive `cloud_egress_consented` is PRESERVED from `current` and never taken from
/// the DTO (BLK-4) — so a settings save can neither grant nor clear cloud-egress consent. The
/// dedicated `consent_to_cloud_egress` command is the only path that flips it.
fn dto_to_config(d: AppConfigDto, current: &AppConfig) -> AppConfig {
    // Normalize empty strings on optional fields to None so they round-trip cleanly.
    let norm = |s: Option<String>| s.filter(|v| !v.trim().is_empty());
    AppConfig {
        provider_id: d.provider_id,
        vault_path: norm(d.vault_path),
        vault_subfolder: norm(d.vault_subfolder),
        whisper_model_path: norm(d.whisper_model_path),
        language: norm(d.language),
        anthropic_model: d.anthropic_model,
        // Brain/AI model + effort ARE settable from the DTO (the Settings UI owns the pickers),
        // exactly like `anthropic_model` — plain strings, NOT preserve-only. `""` = provider default.
        provider_model: d.provider_model,
        provider_effort: d.provider_effort,
        ollama_base_url: d.ollama_base_url,
        ollama_model: d.ollama_model,
        claude_binary: d.claude_binary,
        input_device: norm(d.input_device),
        capture_system_audio: d.capture_system_audio,
        vad_enabled: d.vad_enabled,
        keep_hires_masters: d.keep_hires_masters,
        diarize_others: d.diarize_others,
        voiceprint_enabled: d.voiceprint_enabled,
        aec_enabled: d.aec_enabled,
        post_aec_enabled: d.post_aec_enabled,
        // Recording-storage cap + auto-prune ARE settable from the DTO (the Storage UI owns them).
        // Plain settable fields — an omitted cap deserializes to `None`, auto-prune to `false`
        // (`#[serde(default)]`), so a partial/older save can never silently enable pruning.
        audio_storage_limit_gb: d.audio_storage_limit_gb,
        audio_auto_prune: d.audio_auto_prune,
        model_size: if d.model_size.trim().is_empty() {
            // Mirror AppConfig::default().model_size — an empty/blank choice from the FE
            // resolves the machine-conditional T2 default (`model::default_model_size_now`:
            // turbo-q8_0 when already downloaded / fresh ≥12 GB install, else `small`), the
            // same ONE decision every other blank-size path takes.
            AppConfig::default().model_size
        } else {
            d.model_size
        },
        // OPTIONAL parakeet live-ASR: the engine selector IS settable from the DTO (Settings ▸
        // Transcription owns the picker), like `model_size`. An empty/blank choice falls back to
        // the `"whisper"` default (never a blank engine); the seam's `should_use_parakeet` then
        // also falls back to whisper if parakeet's models aren't downloaded.
        live_asr_engine: if d.live_asr_engine.trim().is_empty() {
            crate::transcribe::live_asr::ENGINE_WHISPER.to_string()
        } else {
            d.live_asr_engine
        },
        // Brain-sidecar timeouts (settable). A 0 value would idle-kill/degrade immediately, so a 0
        // (or an omitted, serde-defaulted) value falls back to the built-in default — never a
        // pathological zero-window.
        brain_idle_timeout_secs: if d.brain_idle_timeout_secs == 0 {
            300
        } else {
            d.brain_idle_timeout_secs
        },
        brain_ready_timeout_secs: if d.brain_ready_timeout_secs == 0 {
            90
        } else {
            d.brain_ready_timeout_secs
        },
        brain_hard_cap_secs: if d.brain_hard_cap_secs == 0 {
            180
        } else {
            d.brain_hard_cap_secs
        },
        // T1.3/T1.4 (transcription heat): the live-model pin + live VAD gate are NOT carried on
        // the settings DTO (no FE toggles yet) — PRESERVE the live values (the L3/L4-flag
        // discipline), each round-tripping through its dedicated K_* load/save key.
        live_model_pin: current.live_model_pin.clone(),
        live_vad_gate: current.live_vad_gate,
        voice_trigger: d.voice_trigger,
        onboarded: d.onboarded,
        note_style: if d.note_style.trim().is_empty() {
            "standard".to_string()
        } else {
            d.note_style
        },
        // ENHANCE-MY-NOTES: an older FE that omits `notesMode` (or sends `""`) falls back to
        // `"enhance"` — the mode that makes the feature do something, and the safe default for
        // new installs. `#[serde(default)]` on the DTO field ensures an omitted key gives `""`.
        notes_mode: if d.notes_mode.trim().is_empty() {
            "enhance".to_string()
        } else {
            d.notes_mode
        },
        auto_organize: d.auto_organize,
        note_assist_refine: d.note_assist_refine,
        note_assist_shorten: d.note_assist_shorten,
        note_assist_enhance: d.note_assist_enhance,
        note_assist_actions_off: d.note_assist_actions_off,
        note_language: if d.note_language.trim().is_empty() {
            "auto".to_string()
        } else {
            d.note_language
        },
        mcp_require_token: d.mcp_require_token,
        lock_require_biometric: d.lock_require_biometric,
        relock_on_screenshare: d.relock_on_screenshare,
        // BLK-4: consent is NEVER set from the DTO. Preserve the live value; only the dedicated
        // `consent_to_cloud_egress` command may flip it. This makes an omitting/zeroed save inert.
        cloud_egress_consented: current.cloud_egress_consented,
        // BLK-4 (M3-CLIENT): share-egress consent is NEVER set from the DTO — preserve the live value.
        // Only `consent_to_share_egress` may flip it. An omitting/zeroed save is inert.
        share_egress_consented: current.share_egress_consented,
        // M6 Shared Brain: org-egress consent is NEVER set from the DTO — preserve the live value.
        // Only `consent_to_org_egress` may flip it. An omitting/zeroed save is inert.
        org_egress_consented: current.org_egress_consented,
        // Sharing-onboarding gate: the first-run choice latch is NEVER set from the DTO — preserve the
        // live value. Only `mark_sharing_choice_made` may flip it, so a settings save can never set or
        // clear it (a re-save from an older FE that omits the key can't accidentally reopen the gate).
        sharing_choice_made: current.sharing_choice_made,
        // brain2 RAG: the semantic-search master flag IS carried on the settings DTO (the Settings
        // UI owns the toggle). Plain bool; an omitted value already defaulted to OFF on the DTO
        // (`#[serde(default)]`), so a partial/older save can never silently enable it. Unlike
        // `cloud_egress_consented` (preserved-only), this one is settable.
        semantic_search_enabled: d.semantic_search_enabled,
        // Vault Audit Phase 3: the weekly-audit schedule is NOT carried on the settings DTO —
        // preserve the live value; only the dedicated `set_audit_schedule` command may flip it
        // (an omitting/older FE save can never silently disable the weekly pass).
        vault_audit_weekly_enabled: current.vault_audit_weekly_enabled,
        // brain2 RAG Phase 2: the SELECTED embedder id is NOT carried on the settings DTO — it is
        // owned exclusively by the dedicated `select_embed_model` command (which validates the id and
        // reports whether a re-index is needed). Preserve the live value, so a generic settings save
        // can never change (or clear) the embedder. Mirrors the `cloud_egress_consented` preserve-only
        // discipline.
        embed_model_id: current.embed_model_id.clone(),
        // Phase H (custom GGUF): a CUSTOM brain model file path IS carried on the settings DTO (the
        // "Custom GGUF model" input, camelCase `brainModelPath`). Unlike `brain_model_id` there is NO
        // registry validation — it is a local file path, stored VERBATIM. An empty/absent value clears
        // it (→ None, i.e. fall back to the registry id). `resolve_brain_model` (reason.rs) validates
        // the file's existence at load and falls back safely if the path is stale, so a bad/removed
        // path can never break startup.
        brain_model_path: match d.brain_model_path.as_deref() {
            Some(p) if !p.is_empty() => d.brain_model_path.clone(),
            _ => None,
        },
        // Phase H (registry): the selected brain model id IS carried on the settings DTO, but it is
        // VALIDATED against the registry first. A `Some(known-id)` is taken; an unknown id or `None`
        // is IGNORED — the live selection is preserved (no error, no bogus id stored). This mirrors
        // `select_brain_model`'s registry guard without crashing a settings save on a stale/typo'd id.
        brain_model_id: match d.brain_model_id.as_deref() {
            Some(id) if crate::reason::brain_model_by_id(id).is_some() => d.brain_model_id.clone(),
            _ => current.brain_model_id.clone(),
        },
        // Murmur Brain postures: `brain_live` + the light/heavy class model ids are set by the
        // dedicated posture / Brain-Live commands (like `cloud_egress_consented`), NEVER by the raw
        // settings save — so a partial/older DTO can neither enable Brain Live nor repoint an engine.
        brain_live: current.brain_live,
        brain_light_model_id: current.brain_light_model_id.clone(),
        brain_heavy_model_id: current.brain_heavy_model_id.clone(),
        brain_contradiction_cards: current.brain_contradiction_cards,
        // Phase H (brain backend): which reasoner powers the brain (cloud/local/off) IS taken from
        // the DTO (the Settings UI owns the toggle). `BrainBackend` deserializes an unknown/omitted
        // token to the default `Cloud`, so the value here is always a valid enum variant.
        brain_backend: d.brain_backend,
        // Phase H (Flow B): the in-meeting voice-action dispatch gate IS taken from the DTO (the
        // Settings UI owns the toggle). Plain bool; an omitted value already defaulted to OFF on the
        // DTO (`#[serde(default)]`), so the opt-in can never be silently enabled by a partial save.
        realtime_reactions: d.realtime_reactions,
        // brain2 connectors: the web-search master toggle IS settable from the DTO (Settings owns it).
        // An omitted value already defaulted to OFF on the DTO, so a partial save can't enable it.
        web_search_enabled: d.web_search_enabled,
        // brain2 connectors (NEW EGRESS CLASS): consent is NEVER set from the DTO — preserved from the
        // live value (BLK-4 mirror). Only `consent_to_web_search` may flip it, so a settings save can
        // neither grant nor clear web-search egress consent.
        web_search_consented: current.web_search_consented,
        // brain2 connectors (Phase 2): the Jira master toggle + non-secret base URL/email ARE settable
        // from the DTO (Settings owns them). An omitted toggle already defaulted to OFF on the DTO, so
        // a partial save can't enable it.
        jira_enabled: d.jira_enabled,
        // brain2 connectors (NEW EGRESS CLASS): consent is NEVER set from the DTO — preserved from the
        // live value (BLK-4 mirror). Only `consent_to_jira` may flip it, so a settings save can
        // neither grant nor clear Jira egress consent.
        jira_consented: current.jira_consented,
        jira_base_url: d.jira_base_url,
        jira_email: d.jira_email,
        // brain2 connectors (Phase 3): the Slack master toggle IS settable from the DTO (Settings owns
        // it). An omitted toggle already defaulted to OFF on the DTO, so a partial save can't enable it.
        slack_enabled: d.slack_enabled,
        // brain2 connectors (NEW EGRESS CLASS): consent is NEVER set from the DTO — preserved from the
        // live value (BLK-4 mirror). Only `consent_to_slack` may flip it, so a settings save can
        // neither grant nor clear Slack egress consent.
        slack_consented: current.slack_consented,
        // Opt-in env inheritance for the `claude` CLI IS settable from the DTO (the Settings UI owns
        // the toggle). Default OFF on the DTO (`#[serde(default)]`), so a partial/older save can never
        // silently enable it. Even ON, the DB keys are never inherited (claude_code.rs `harden_env`).
        claude_code_inherit_env: d.claude_code_inherit_env,
        // AI Gateway fields ARE settable from the DTO (the Settings UI owns them). An omitted value
        // deserializes to `""` (`#[serde(default)]`), which is a valid "unset" state.
        gateway_base_url: d.gateway_base_url,
        gateway_model: d.gateway_model,
        // M3-CLIENT: the sharing-server base URL IS settable from the DTO (Settings → Account owns
        // it). An omitted value defaults to `""` (unset) — no behavioral change for existing installs.
        share_base_url: d.share_base_url,
        // Proactive brain P1: the recall-card mute IS settable from the DTO (Settings owns the
        // toggle). An omitted value defaults ON (`default_true`), matching AppConfig::default —
        // the backend mute is an explicit user choice, never a partial-save side effect.
        proactive_hints_enabled: d.proactive_hints_enabled,
        // Cross-meeting USER MEMORY: the master gate IS settable from the DTO (Settings owns the
        // toggle). An omitted value defaults ON (`default_true`), matching AppConfig::default — an
        // older FE payload can never silently turn memory off.
        user_memory_enabled: d.user_memory_enabled,
        // Brain v2 L2.1: the consolidation-job flag is NOT carried on the settings DTO (no FE
        // toggle yet) — PRESERVE the live value so a settings save can neither enable nor clear it;
        // it round-trips through the dedicated K_MEMORY_CONSOLIDATION_ENABLED load/save keys.
        memory_consolidation_enabled: current.memory_consolidation_enabled,
        // Tier 3b (B) grounding: NOT yet carried on the settings DTO (the FE toggle is a follow-up),
        // so PRESERVE the live value here — a normal settings save can neither enable nor clear it,
        // and it round-trips through the dedicated K_GROUND_SUMMARY load/save keys. Mirrors the
        // preserve-only discipline used for consent + embedder id.
        ground_summary: current.ground_summary,
        // Brain v2 L3: the grammar-constraint gate, the JIT-Ask flag, and the loop-compaction
        // flag are NOT carried on the settings DTO (no FE toggles yet) — PRESERVE the live values
        // so a settings save can neither enable nor clear them; each round-trips through its
        // dedicated K_* load/save key (the memory_consolidation_enabled discipline).
        brain_heavy_grammar_enabled: current.brain_heavy_grammar_enabled,
        ask_jit_retrieval: current.ask_jit_retrieval,
        loop_transcript_compaction: current.loop_transcript_compaction,
        // Brain v2 L4: the live-bullets flag is NOT carried on the settings DTO (no FE toggle
        // yet) — PRESERVE the live value (same discipline as the L3 flags above); it round-trips
        // through the dedicated K_LIVE_BULLETS_ENABLED load/save keys.
        live_bullets_enabled: current.live_bullets_enabled,
        // Model-role keys ARE settable from the DTO (a future Settings UI owns the rows), like
        // `gateway_model` — plain strings, `""` = inherit legacy. An omitted key deserializes to
        // `""` (`#[serde(default)]`), so an older FE payload can never flip a role.
        role_notes_connection: d.role_notes_connection,
        role_notes_model: d.role_notes_model,
        role_notes_effort: d.role_notes_effort,
        role_ask_connection: d.role_ask_connection,
        role_ask_model: d.role_ask_model,
        role_ask_effort: d.role_ask_effort,
        role_live_connection: d.role_live_connection,
        role_live_model: d.role_live_model,
        role_live_effort: d.role_live_effort,
    }
}








/// DTO for a single model returned by `list_gateway_models`.
///
/// Shape: `{ "id": "gpt-4o" }` (camelCase). The FE populates the model picker from this.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayModelDto {
    pub id: String,
}

/// Fetch the model catalog from the configured AI Gateway (`GET {base}/v1/models`) and return
/// the raw list of model ids. Shared core of `list_gateway_models` and `list_models("gateway")`.
///
/// Inbound-only: this call sends NO meeting content — only an optional `Authorization: Bearer`
/// header carrying the stored gateway key. It therefore does NOT need the redaction firewall or
/// the cloud-egress consent gate.
///
/// Returns `AppError::InvalidArg` when no gateway base URL is configured.
async fn gateway_model_ids(state: &AppState) -> Result<Vec<String>, AppError> {
    let (base_url, model, api_key) = {
        let config = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        let base_url = config.gateway_base_url.clone();
        let model = config.gateway_model.clone();
        // R3: resolve the gateway key; never falls back to the Anthropic key.
        let api_key = secrets::get_secret(GATEWAY_KEY_ACCOUNT).ok().flatten();
        (base_url, model, api_key)
    };

    if base_url.trim().is_empty() {
        return Err(AppError::InvalidArg(
            "no gateway base URL configured — set it in Settings before fetching the model catalog"
                .into(),
        ));
    }

    let provider = crate::summarize::gateway::OpenAiCompatProvider::new(base_url, model, api_key)?;
    provider.list_models().await
}

/// Fetch the model catalog from the configured AI Gateway (`GET {base}/v1/models`) and return
/// the list of model ids the gateway exposes. Thin DTO wrapper over [`gateway_model_ids`] —
/// kept until the FE swaps to the unified `list_models("gateway")`.
#[tauri::command]
pub async fn list_gateway_models(
    state: State<'_, AppState>,
) -> Result<Vec<GatewayModelDto>, AppError> {
    let ids = gateway_model_ids(&state).await?;
    Ok(ids.into_iter().map(|id| GatewayModelDto { id }).collect())
}

/// Model ids for the connections whose catalog is static, compile-time data — pure so it is
/// unit-testable without state or network. `None`-network connections only:
///   - `"claude_code"` / `"anthropic"` → the curated [`crate::summarize::provider::CLAUDE_MODELS`],
///   - `"local"` → the on-device `reason::BRAIN_MODELS` registry ids,
///   - `"off"` → empty (a valid connection that runs no models),
///   - anything else → `AppError::InvalidArg`.
fn static_connection_models(connection: &str) -> Result<Vec<String>, AppError> {
    match connection {
        "claude_code" | "anthropic" => Ok(crate::summarize::provider::CLAUDE_MODELS
            .iter()
            .map(|id| id.to_string())
            .collect()),
        "local" => Ok(crate::reason::BRAIN_MODELS
            .iter()
            .map(|m| m.id.to_string())
            .collect()),
        "off" => Ok(vec![]),
        other => Err(AppError::InvalidArg(format!(
            "unknown connection '{other}' — expected claude_code, anthropic, ollama, gateway, local, or off"
        ))),
    }
}

/// Unified model catalog for a connection — the ONE source of truth behind the FE per-role
/// model dropdowns. Dispatch by `connection`:
///   - `"gateway"` → `GET {gateway_base_url}/v1/models` (shares [`gateway_model_ids`] with
///     `list_gateway_models`),
///   - `"ollama"` → `GET {ollama_base_url}/api/tags`, model names only,
///   - `"claude_code"` / `"anthropic"` / `"local"` / `"off"` → static lists
///     (see [`static_connection_models`]),
///   - anything else → `AppError::InvalidArg`.
///
/// Inbound-only on every arm: NO meeting content is sent — at most an `Authorization: Bearer`
/// header to the configured gateway — so no redaction firewall / consent gate is needed (the
/// `list_gateway_models` precedent). No content read → no lock-model surface.
#[tauri::command]
pub async fn list_models(
    state: State<'_, AppState>,
    connection: String,
) -> Result<Vec<String>, AppError> {
    match connection.as_str() {
        "gateway" => gateway_model_ids(&state).await,
        "ollama" => {
            let base_url = {
                let config = state
                    .config
                    .lock()
                    .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
                config.ollama_base_url.clone()
            };
            // OllamaProvider::new normalizes an empty base URL to the localhost default and
            // trims trailing slashes; the model arg is unused for a catalog fetch.
            crate::summarize::ollama::OllamaProvider::new(base_url, String::new())
                .list_models()
                .await
        }
        other => static_connection_models(other),
    }
}

/// DTO returned by `gateway_health`.
///
/// Shape: `{ "reachable": true, "modelCount": 6 }` (camelCase, matches the FE `GatewayHealth`
/// type). `reachable: false` with `model_count: 0` means the gateway is unreachable or not
/// configured — the FE renders a red dot.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayHealthDto {
    pub reachable: bool,
    pub model_count: u32,
}

/// Probe the configured AI Gateway for reachability and (optionally) catalog size.
///
/// Uses `OpenAiCompatProvider::probe()` which sends a GET to the models endpoint when one
/// exists, or to the chat endpoint for custom routes (a GET to a POST-only route returns
/// 4xx/405 but proves the server is reachable — no LLM call, no cost). Any HTTP response
/// (any status) → `reachable: true`; a transport failure → `reachable: false`.
///
/// This command NEVER returns an `Err` variant so the FE health dot always gets a clean value.
/// Inbound-only: sends NO meeting content, only an optional `Authorization: Bearer` header. Does
/// NOT need the redaction firewall or the consent gate (same rationale as `list_gateway_models`).
#[tauri::command]
pub async fn gateway_health(state: State<'_, AppState>) -> Result<GatewayHealthDto, AppError> {
    let (base_url, model, api_key) = {
        let config = match state.config.lock() {
            Ok(c) => c,
            Err(_) => {
                return Ok(GatewayHealthDto {
                    reachable: false,
                    model_count: 0,
                })
            }
        };
        let base_url = config.gateway_base_url.clone();
        let model = config.gateway_model.clone();
        // R3: resolve the gateway key; never falls back to the Anthropic key.
        let api_key = secrets::get_secret(GATEWAY_KEY_ACCOUNT).ok().flatten();
        (base_url, model, api_key)
    };

    if base_url.trim().is_empty() {
        // Not configured → degrade silently.
        return Ok(GatewayHealthDto {
            reachable: false,
            model_count: 0,
        });
    }

    let provider =
        match crate::summarize::gateway::OpenAiCompatProvider::new(base_url, model, api_key) {
            Ok(p) => p,
            Err(_) => {
                return Ok(GatewayHealthDto {
                    reachable: false,
                    model_count: 0,
                })
            }
        };

    // probe() never returns Err — degrades to (false, 0) on transport failure.
    let (reachable, model_count) = provider.probe().await;
    Ok(GatewayHealthDto {
        reachable,
        model_count,
    })
}

// ── Egress ledger DTOs (Phase 6) ────────────────────────────────────────────────────────────────
//
// All structs are `camelCase` on the wire (matches the FE `EgressLedger` / `EgressRow` types in
// `core/models.ts`). Carries ONLY counts, ids, labels, and token/byte numbers — no content (§8).

/// availability() fan-out across all three providers for the Settings UI.
#[tauri::command]
pub async fn provider_statuses(
    state: State<'_, AppState>,
) -> Result<Vec<ProviderStatus>, AppError> {
    use crate::summarize::provider::Availability;

    let config: AppConfig = {
        let guard = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        guard.clone()
    };

    let providers = all_providers(&config);
    let mut out = Vec::with_capacity(providers.len());
    for p in providers {
        let (available, reason) = match p.availability().await {
            Availability::Available => (true, None),
            Availability::Unavailable { reason } => (false, Some(reason)),
        };
        out.push(ProviderStatus {
            id: p.id().to_string(),
            available,
            reason,
        });
    }
    Ok(out)
}

/// Re-run summarize+export for an existing meeting with the configured provider, reusing
/// the meeting's stored transcript segments (Detail "re-summarize"/"re-export" seed —
/// wired in P0, UI optional).
#[tauri::command]
pub async fn resummarize(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<StopResult, AppError> {
    // BLK-2b READ/WRITE-GATE: re-summarizing reads the stored transcript (blanked while sealed) and
    // WRITES a fresh note + re-exports a plaintext `.md` to the vault. For a sealed-and-not-unlocked
    // meeting that would (a) feed a cloud provider blank text and (b) leave plaintext markdown +
    // a vault `.md` in a locked folder. Fail closed — the FE must unlock first.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to re-summarize".into(),
        ));
    }
    // DETACHED, panic-mapped execution (the same #346 pattern as `stop_recording`): awaited
    // INLINE, a panic inside `resummarize_existing` would leave the invoke Promise unsettled
    // forever (Tauri never settles a panicked command future) — the sibling wedge. In a real
    // spawned task the panic surfaces as a `JoinError` → `await_pipeline_task` maps it to an
    // `AppError` so the Promise REJECTS. NO status-guard analog on purpose: every status
    // resummarize can leave behind is already TERMINAL — it only writes `Summarized`/`Exported`
    // on success and persists `Error` in its own Err arm, so a panic leaves the row at its prior
    // terminal state and a Drop-guard would have nothing non-terminal to repair.
    let task_app = app.clone();
    let task_meeting_id = meeting_id.clone();
    let resummarize_task = tauri::async_runtime::spawn(async move {
        let state = task_app.state::<AppState>();
        pipeline::resummarize_existing(&task_app, &state, &task_meeting_id).await
    });
    let result = await_pipeline_task(resummarize_task).await?;
    // COMMIT BOUNDARY: a re-summarize rewrites the meeting's note → BEST-EFFORT re-publish any org
    // shares of it so members see the fresh summary. Best-effort — never fails the re-summarize. If
    // ≥1 org copy was re-published, ping open org views so the fresh summary shows without a manual sync.
    if republish_org_shares_for_source(state.inner(), Some(&meeting_id), None)
        .await
        .unwrap_or(0)
        > 0
    {
        crate::events::emit_org_feed_updated(&app, 1);
    }
    Ok(StopResult {
        meeting_id: result.meeting_id,
        markdown: result.note_markdown,
        exported_path: result
            .exported_path
            .map(|p| p.to_string_lossy().to_string()),
    })
}

/// FROM-DISK re-transcription of a meeting in the terminal `Error` state whose ARCHIVE audio
/// survived on disk (salvage-from-disk, 2026-07-16). The manual twin of the startup disk salvage:
/// re-runs the SAME post-Stop pipeline (`pipeline::run_salvage_from_disk` → `run_after_stop`)
/// off the archived WAV — heavy ASR rides the shared `perf::run_heavy` gate + the ASR watchdog
/// exactly like a live Stop. Detached + panic-mapped like `stop_recording` (#346): a panic
/// rejects the invoke Promise via `await_pipeline_task`, and the in-task `TerminalStatusGuard`
/// (status-aware) restores a terminal state.
///
/// GATES (fail-closed, in order): refuse while a recording is ACTIVE (same check as
/// `start_recording` — a salvage must not race a live capture); refuse a sealed-and-not-
/// session-unlocked meeting (`meeting_is_unlocked` — this command NEVER decrypts a `.enc`; for a
/// session-unlocked locked folder the playable WAV was already materialized by `unlock_folder`);
/// refuse a non-`Error` row; refuse when no archive audio exists on disk. The single-flight
/// CLAIM is the atomic `Error → Recording` status transition, so two concurrent retries can
/// never both run — and a crash mid-retry leaves a stuck `RECORDING` row with an on-disk WAV,
/// which the NEXT launch's disk salvage re-claims automatically.
#[tauri::command]
pub async fn retry_transcription(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<StopResult, AppError> {
    let wav_path = retry_transcription_prep(state.inner(), &meeting_id)?;

    let task_app = app.clone();
    let task_meeting_id = meeting_id.clone();
    let pipeline_task = tauri::async_runtime::spawn(async move {
        let state = task_app.state::<AppState>();
        // Armed NOW; disarmed after the run. The claim above flipped the row to the non-terminal
        // `Recording`, so a panic-unwind must restore a terminal state (the guard's Drop persists
        // `Error` — status-aware, so it never clobbers a row that already reached Summarized+).
        let terminal_guard = pipeline::TerminalStatusGuard::arm(
            Some(task_app.clone()),
            state.db.clone(),
            &task_meeting_id,
        );
        let result =
            pipeline::run_salvage_from_disk(&task_app, &state, &task_meeting_id, &wav_path).await;
        terminal_guard.disarm();
        result
    });
    let result = await_pipeline_task(pipeline_task).await?;

    Ok(StopResult {
        meeting_id: result.meeting_id,
        markdown: result.note_markdown,
        exported_path: result
            .exported_path
            .map(|p| p.to_string_lossy().to_string()),
    })
}

/// Synchronous prep/gate half of [`retry_transcription`], split out for headless tests: runs every
/// fail-closed check, resolves the on-disk archive WAV, and — as the LAST step — atomically claims
/// the row (`Error → Recording`). Nothing is mutated unless every gate passed.
pub(crate) fn retry_transcription_prep(
    state: &AppState,
    meeting_id: &str,
) -> Result<std::path::PathBuf, AppError> {
    // No retry while a recording is in progress (mirror of `start_recording`'s own check).
    {
        let recorder = state
            .recorder
            .lock()
            .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
        if recorder.is_some() {
            return Err(AppError::Audio(
                "a recording is in progress — stop it before retrying transcription".into(),
            ));
        }
    }
    // Lock gate: never re-pipeline (or decrypt) a sealed-and-not-session-unlocked meeting.
    if !meeting_is_unlocked(state, meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to retry transcription".into(),
        ));
    }
    let meeting = state
        .db
        .get_meeting(meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no meeting with id {meeting_id}")))?;
    if meeting.status != MeetingStatus::Error {
        return Err(AppError::InvalidArg(
            "retry transcription is only available for a failed recording".into(),
        ));
    }
    let path = meeting.audio_path.filter(|p| !p.trim().is_empty()).ok_or_else(|| {
        AppError::Storage(
            "this recording has no archived audio on disk — nothing to re-transcribe".into(),
        )
    })?;
    // Resolve a PLAINTEXT playable WAV. A `.enc`-only state past the gate (e.g. an unfiled row
    // pointing at a sealed file) still refuses — this path never decrypts.
    let plaintext = path.trim_end_matches(ENC_SUFFIX).to_string();
    if !std::path::Path::new(&plaintext).exists() {
        let enc = format!("{plaintext}{ENC_SUFFIX}");
        if std::path::Path::new(&enc).exists() {
            return Err(AppError::Locked(
                "this recording's audio is sealed — unlock its folder, then retry".into(),
            ));
        }
        return Err(AppError::Storage(
            "this recording's audio file is no longer on disk — nothing to re-transcribe".into(),
        ));
    }
    // Single-flight claim, LAST: only one retry may flip Error → Recording. (A crash after this
    // leaves a stuck RECORDING row + an on-disk WAV — exactly what startup disk salvage re-claims.)
    if !state.db.transition_meeting_status(
        meeting_id,
        MeetingStatus::Error,
        MeetingStatus::Recording,
    )? {
        return Err(AppError::InvalidArg(
            "a retry for this recording is already running".into(),
        ));
    }
    tracing::info!(target: "pipeline", meeting_id = %meeting_id, "retry transcription claimed — re-running the pipeline from the archived audio");
    Ok(std::path::PathBuf::from(plaintext))
}


/// Backend-mask sealed-not-session-unlocked meetings in a Library list, mirroring [`masked_detail`]:
/// a meeting whose folder is locked (`folders.locked = 1`) AND NOT in the current session unlock set
/// gets its real AI title replaced by the "🔒 Locked" placeholder and its `.enc` `audio_path` nulled
/// (so nothing can feed `convertFileSrc` / the `asset:` protocol for a locked recording). The row +
/// its `folder_id` are PRESERVED so the FE still renders the inline lock badge (it keys the badge off
/// `folder_id` + the folder's exposure). The lock decision routes through the session unlock set +
/// `locked_folder_ids` (the same source the `*_visible` reads use) — NOT the FE.
pub(crate) fn mask_locked_meetings(
    state: &AppState,
    meetings: Vec<Meeting>,
) -> Result<Vec<Meeting>, AppError> {
    let locked: std::collections::HashSet<String> =
        state.db.locked_folder_ids()?.into_iter().collect();
    if locked.is_empty() {
        return Ok(meetings); // no sealed folders at all → nothing to mask (fast path).
    }
    let unlocked = unlocked_snapshot(state)?;
    Ok(meetings
        .into_iter()
        .map(|m| {
            let sealed = match m.folder_id.as_deref() {
                Some(f) => locked.contains(f) && !unlocked.contains(f),
                None => false, // vault root / no folder → never sealed.
            };
            if sealed {
                Meeting {
                    title: Some("🔒 Locked".to_string()),
                    audio_path: None,
                    ..m
                }
            } else {
                m
            }
        })
        .collect())
}




// ── VOICEPRINTS (Phase 2): cosine re-identification + enroll + management ───────────────────────
//
// GATE DISCIPLINE (lock-model): every read here goes through `list_voiceprints_visible`, so a
// sealed-and-not-session-unlocked meeting's voiceprint is INVISIBLE — it is never listed, never a
// match candidate, and never a suggestion source. The suggester compares THIS meeting's clusters
// only against OTHER visible LABELED voiceprints; a sealed prior contributes nothing. The raw
// embedding never crosses the IPC boundary (the DTOs carry label + provenance + dim only).









/// EXPLICIT timeline generation — the HEAVY path split out of `get_timeline`. Runs the Notes-role
/// provider over the (decimated, for on-device) transcript to derive the speaker/topic map, caches
/// it, and returns it. For an on-device provider this loads a multi-GB model, so it is only ever
/// invoked deliberately: the FE auto-fires it for cheap cloud providers, and gates it behind a user
/// click for on-device ones. The on-device RAM guard in `reason::mistral` still applies as a backstop
/// (refuse-don't-OOM → deterministic floor).
#[tauri::command]
pub async fn generate_timeline(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<MeetingTimeline, AppError> {
    // Same READ-GATE as `get_timeline`: never derive a sealed-not-unlocked meeting's timeline.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Ok(MeetingTimeline::default());
    }
    let segments = state.db.get_segments(&meeting_id)?;
    if segments.is_empty() {
        return Ok(MeetingTimeline::default());
    }
    let duration_s = state
        .db
        .get_meeting(&meeting_id)?
        .map(|m| m.duration_s)
        .unwrap_or(0);
    let config = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        c.clone()
    };
    let provider = crate::summarize::provider_for(crate::summarize::roles::Role::Notes, &config, &state.heavy_inference)?;
    let timeline =
        crate::summarize::timeline::generate(provider.as_ref(), &segments, duration_s).await?;
    // SEAM-F1 (2026-07-11 audit, plaintext-at-rest): the provider `.await` above can span a relock,
    // so re-take the lifecycle guard and RE-GATE the meeting AFTER generation before persisting. Then
    // route the write through the seal-on-write seam: a session-unlocked LOCKED folder SEALS the fresh
    // timeline under the folder CK (never a bare plaintext `set_timeline_data`, which the pre-fix path
    // left in the sealed row after relock — plaintext at rest). If a relock landed mid-generation
    // (the meeting is now sealed-not-unlocked), do NOT persist plaintext at all — the generated
    // timeline is discarded (the FE re-generates after the next unlock).
    if let Ok(json) = serde_json::to_string(&timeline) {
        let _lifecycle = lifecycle_guard(state.inner());
        if !meeting_is_unlocked(state.inner(), &meeting_id)? {
            return Ok(timeline); // relocked mid-generation → persist nothing plaintext behind the lock.
        }
        // Fail-closed on a missing session KEK for a locked folder (never write unsealed plaintext).
        set_timeline_data_reseal_if_locked(state.inner(), &meeting_id, &json)?;
    }
    Ok(timeline)
}


/// Build the MASKED detail DTO for a sealed-and-not-session-unlocked meeting. Pure (no DB / state)
/// so the read-gate's masking contract is unit-testable. EVERY content channel is closed:
/// - `title` → "🔒 Locked" (the real title lives in `meetings.title`, plaintext-at-rest);
/// - `audio_path` → `None` so the FE has nothing to hand `convertFileSrc` (the `asset:` protocol
///   serve path that bypasses the `export_audio` command + `meeting_is_unlocked` gate);
/// - `note` / `segments` → empty;
/// - `locked` → true so the FE renders the unlock affordance, not an empty shell.
pub(crate) fn masked_detail(meeting: Meeting) -> MeetingDetailDto {
    MeetingDetailDto {
        meeting: Meeting {
            title: Some("🔒 Locked".to_string()),
            audio_path: None,
            ..meeting
        },
        note: None,
        segments: Vec::new(),
        // The Q&A log is masked too — a sealed-not-unlocked meeting surfaces NOTHING here (and at
        // rest the rows were purged on seal anyway).
        assistant_interactions: Vec::new(),
        locked: true,
        // Phase 5: provenance is gated alongside all other content — a locked meeting surfaces
        // nothing (a model name could leak which AI service processed the content).
        ai_provider: None,
        ai_model: None,
        model_served: None,
    }
}

/// Download the Whisper model matching the chosen size + language (multilingual unless
/// English is selected) from the whisper.cpp HuggingFace mirror into the app models dir if
/// missing; returns its path. No-op (returns the existing path) when already present. Emits
/// [`crate::events::EVENT_MODEL_DOWNLOAD`] progress (throttled) so the FE can show a progress bar.
#[tauri::command]
pub async fn download_model(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, AppError> {
    let (configured, size, language) = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        (
            c.whisper_model_path.clone(),
            c.model_size.clone(),
            c.language.clone().unwrap_or_default(),
        )
    };
    let p = configured.as_deref().map(std::path::Path::new);

    // Throttle progress events to roughly every 8 MB so a multi-GB download doesn't flood the FE.
    const EMIT_EVERY: u64 = 8 * 1024 * 1024;
    let mut last_emit: u64 = 0;
    let path = crate::transcribe::ensure_model(p, &size, &language, |downloaded, total| {
        if downloaded - last_emit >= EMIT_EVERY {
            last_emit = downloaded;
            let _ = app.emit(
                crate::events::EVENT_MODEL_DOWNLOAD,
                crate::events::ModelDownloadPayload {
                    downloaded,
                    total,
                    done: false,
                },
            );
        }
    })
    .await?;

    let _ = app.emit(
        crate::events::EVENT_MODEL_DOWNLOAD,
        crate::events::ModelDownloadPayload {
            downloaded: 0,
            total: None,
            done: true,
        },
    );
    Ok(path.to_string_lossy().to_string())
}


/// Download the OPTIONAL parakeet live-ASR engine's four int8 models (~600 MB) from the csukuangfj
/// sherpa-onnx HF mirror into `<models_dir>/parakeet-tdt-0.6b-v3-int8` if missing (atomic per-file
/// `.part` → rename; a file already present is skipped). Emits the SAME
/// [`crate::events::EVENT_MODEL_DOWNLOAD`] progress (throttled) as the whisper download so the FE
/// reuses one progress bar. No-op when all four are already present.
#[tauri::command]
pub async fn download_parakeet_models(app: AppHandle) -> Result<(), AppError> {
    // Throttle progress events to roughly every 8 MB (parity with `download_model`).
    const EMIT_EVERY: u64 = 8 * 1024 * 1024;
    let mut last_emit: u64 = 0;
    crate::transcribe::model::ensure_parakeet_models(|downloaded, total| {
        if downloaded - last_emit >= EMIT_EVERY {
            last_emit = downloaded;
            let _ = app.emit(
                crate::events::EVENT_MODEL_DOWNLOAD,
                crate::events::ModelDownloadPayload {
                    downloaded,
                    total,
                    done: false,
                },
            );
        }
    })
    .await?;

    let _ = app.emit(
        crate::events::EVENT_MODEL_DOWNLOAD,
        crate::events::ModelDownloadPayload {
            downloaded: 0,
            total: None,
            done: true,
        },
    );
    Ok(())
}

/// Download the on-device brain model identified by `model_id` (from the curated registry) into the
/// shared models dir if missing; returns its path. No-op (returns the existing path) when already
/// present. Unknown id ⇒ `AppError::InvalidArg`. INBOUND ONLY — fetches a model file and sends NO
/// meeting content (no egress). Emits [`crate::events::EVENT_BRAIN_DOWNLOAD`] progress events
/// (throttled). The downloaded file is NOT loaded here — wiring the brain is a later step.
/// (The registry-lookup core `brain_download_target` lives in `commands/model_perf.rs`.)
#[tauri::command]
pub async fn download_brain_model(app: AppHandle, model_id: String) -> Result<String, AppError> {
    let (url, dest) = brain_download_target(&model_id)?;

    if dest.is_file() {
        return Ok(dest.to_string_lossy().to_string());
    }

    // The pinned integrity hash for this model (None until the registry entry is pinned — the
    // downloader then verifies before promoting the file, or warns + skips when None).
    let expected_sha = crate::reason::brain_model_by_id(&model_id).and_then(|m| m.sha256);

    // Throttle progress events to roughly every 8 MB so a multi-GB download doesn't flood the FE.
    const EMIT_EVERY: u64 = 8 * 1024 * 1024;
    let mut last_emit: u64 = 0;
    crate::reason::download_brain_model(url, &dest, expected_sha, |downloaded, total| {
        if downloaded - last_emit >= EMIT_EVERY {
            last_emit = downloaded;
            let _ = app.emit(
                crate::events::EVENT_BRAIN_DOWNLOAD,
                crate::events::BrainDownloadPayload {
                    downloaded,
                    total,
                    done: false,
                },
            );
        }
    })
    .await?;

    let _ = app.emit(
        crate::events::EVENT_BRAIN_DOWNLOAD,
        crate::events::BrainDownloadPayload {
            downloaded: 0,
            total: None,
            done: true,
        },
    );
    Ok(dest.to_string_lossy().to_string())
}

/// Download the multilingual-e5-small model (3 HF files) into the shared models dir, INBOUND-ONLY,
/// emitting [`crate::events::EVENT_EMBED_DOWNLOAD`] progress (throttled per file). Sends NO meeting
/// content (no egress). The downloaded model is NOT loaded here — it is picked up lazily by
/// `embed::active_embedder` on the next embed (selected on model presence). Returns the model dir.
#[tauri::command]
pub async fn download_embed_model(app: AppHandle) -> Result<String, AppError> {
    let file_count = crate::embed::EMBED_MODEL_FILES.len();
    // Throttle progress to roughly every 2 MB so the (small) model download doesn't flood the FE.
    const EMIT_EVERY: u64 = 2 * 1024 * 1024;
    let mut last_emit: u64 = 0;
    let mut last_index: usize = usize::MAX;
    let dir = crate::embed::download_embed_model(|file_index, downloaded, total| {
        // Always emit on a file boundary; otherwise throttle by bytes.
        if file_index != last_index || downloaded - last_emit >= EMIT_EVERY {
            last_index = file_index;
            last_emit = downloaded;
            let _ = app.emit(
                crate::events::EVENT_EMBED_DOWNLOAD,
                crate::events::EmbedDownloadPayload {
                    file_index,
                    file_count,
                    downloaded,
                    total,
                    done: false,
                },
            );
        }
    })
    .await?;

    let _ = app.emit(
        crate::events::EVENT_EMBED_DOWNLOAD,
        crate::events::EmbedDownloadPayload {
            file_index: file_count,
            file_count,
            downloaded: 0,
            total: None,
            done: true,
        },
    );
    Ok(dir.to_string_lossy().to_string())
}

/// Result of [`reindex_embeddings`]. `status` is `"model_missing"` when the real e5 model is absent
/// (no indexing was attempted — re-indexing with the deterministic STUB embedder would poison the
/// index with garbage vectors, worse than nothing), else `"indexed"`. On `"indexed"`, `indexed` is
/// the count of VISIBLE meetings whose chunks were (re)built. NO PII — counts + a status string only.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReindexResult {
    pub status: String,
    pub indexed: usize,
    pub total: usize,
}

/// brain2 RAG — BACKFILL the semantic vector index for ALL VISIBLE meetings (the one-shot the user
/// runs after turning `semantic_search_enabled` on, or after installing the e5 model so the old
/// STUB-embedded chunks get replaced by real e5 vectors).
///
/// GATING (lock-model): the corpus is exactly `list_meetings_visible(unlocked)` — a sealed-and-not-
/// session-unlocked meeting is NEVER returned, so its plaintext is never chunked/embedded, and its
/// chunks STAY purged (the seal already purged them; we don't touch them). For each visible meeting
/// we re-fetch its note through `get_note_if_visible(unlocked)` (defense-in-depth: skip if the note
/// is not visible) and call `index_meeting_chunks`, which PURGES-then-reinserts — so any stale stub
/// vectors are replaced with e5 ones. No new read path: every read routes through `visibility_clause`.
///
/// MODEL GUARD: if the real e5 model is absent (`!embed_model_present()` ⇒ `active_embedder` is the
/// stub), MEETING indexing does nothing and the result is `{ status: "model_missing" }` — re-indexing
/// with garbage stub vectors is strictly worse than leaving the (old, possibly-stub) chunks alone;
/// the FE prompts the user to download e5 first via `download_embed_model`. DOCUMENT chunk/FTS
/// backfill still runs (chunk-only, zero vectors) so keyword retrieval over documents works
/// regardless of the model.
///
/// Emits [`crate::events::EVENT_REINDEX`] `{ done, total }` progress (counts only, NO PII).
/// EMBED_DIM stays 384 (e5 == stub width) ⇒ NO `vec_chunks` schema migration.
#[tauri::command]
pub async fn reindex_embeddings(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ReindexResult, AppError> {
    // Snapshot the LIVE session unlock set — visibility is evaluated against exactly this set, so a
    // sealed-not-unlocked folder's meetings are invisible and never indexed.
    let unlocked = state
        .unlocked_folders
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    // 2026-07-13 perf audit (MODERATE): this ran `reindex_embeddings_inner` — a vault-wide loop
    // (up to 100_000 meetings + every visible document) doing real Candle/Metal embed calls —
    // INLINE on the async command's Tokio worker, the same shape as the original startup-freeze
    // bug this whole investigation started from (now fixed there via a RAM floor + per-run cap;
    // the per-meeting embed BATCH SIZE is already safe here too, since index_meeting_chunks/
    // index_meeting_topic_chunks were sub-batched earlier today). This one is user-TRIGGERED (the
    // Settings "Reindex" button), not automatic, so the risk is lower, but the fix is the same
    // shape: move off the async runtime + gate on a RAM floor before starting.
    if !crate::transcribe::model::topic_backfill_ram_permits_now() {
        return Err(AppError::Unavailable(
            "not enough free memory to reindex right now — close some apps and try again".into(),
        ));
    }
    let db = state.db.clone();
    let heavy_inference = state.heavy_inference.clone();
    crate::perf::run_heavy(&heavy_inference, move || {
        reindex_embeddings_inner(
            &db,
            &unlocked,
            crate::embed::embed_model_present(),
            crate::embed::active_embedder().as_ref(),
            |done, total| {
                let _ = app.emit(
                    crate::events::EVENT_REINDEX,
                    crate::events::ReindexPayload { done, total },
                );
            },
        )
    })
    .await
}

/// "Powiązane wg znaczenia": the up-to-5 meetings most semantically similar to `meeting_id`.
///
/// GATING: routes through `Db::related_meetings_visible`, which re-embeds the meeting's OWN plaintext
/// chunks into a centroid and runs the gated `search_semantic_visible` (visibility_clause) — a
/// sealed-not-session-unlocked neighbour is never returned. When `semantic_search_enabled` is OFF
/// (the default) we short-circuit to an empty list so the FE simply hides the section. No raw
/// embedding ever crosses IPC (the centroid is computed and consumed in Rust; the DTO is `SearchHit`).
#[tauri::command]
pub async fn related_meetings(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<SearchHit>, AppError> {
    let enabled = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .semantic_search_enabled
    };
    if !enabled {
        return Ok(Vec::new()); // feature OFF → empty → FE hides the section.
    }
    // Defense-in-depth: purge-on-lock already empties a sealed source's chunks (→ []) and the
    // neighbour list is itself visibility-gated, but refuse a sealed-not-session-unlocked SOURCE
    // explicitly so the safety is stated, not merely emergent.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Ok(Vec::new());
    }
    let unlocked = unlocked_snapshot(state.inner())?;
    let emb = crate::embed::active_embedder();
    state
        .db
        .related_meetings_visible(&meeting_id, emb.as_ref(), 5, &unlocked)
}

/// Pure, AppHandle-free core of [`reindex_embeddings`] so the model-missing guard + the
/// visibility-gated loop are unit-testable headless. Takes the `Db`, the live `unlocked` session set,
/// whether the REAL e5 model is present (`model_present`), the active embedder, and a progress sink.
///
/// MODEL GUARD: `model_present == false` ⇒ return `{ status: "model_missing" }` and index NO
/// meeting and NO vector (re-indexing with the deterministic STUB embedder would poison the index
/// with garbage vectors — strictly worse than leaving the old chunks alone). Document CHUNK/FTS
/// backfill (zero vectors) still runs for visible documents that have no chunks yet, so keyword
/// retrieval covers the write-only legacy rows.
///
/// GATING (lock-model): the corpus is exactly `list_meetings_visible(unlocked)` — a sealed-and-not-
/// session-unlocked meeting is NEVER returned, so its plaintext is never chunked/embedded and its
/// chunks STAY purged. Each meeting's note is re-checked through `get_note_if_visible(unlocked)`
/// before `index_meeting_chunks` (PURGE-then-reinsert, so stale stub vectors are replaced).
pub(crate) fn reindex_embeddings_inner<F: FnMut(usize, usize)>(
    db: &crate::storage::Db,
    unlocked: &std::collections::HashSet<String>,
    model_present: bool,
    embedder: &dyn crate::embed::Embedder,
    mut on_progress: F,
) -> Result<ReindexResult, AppError> {
    // DOCUMENT backfill first — doc_chunks + the FTS index are model-INDEPENDENT (keyword retrieval
    // must work on a default install), so this runs even when the e5 model is absent. Visible
    // documents only (`visible_document_ids` applies `visibility_clause`; a sealed folder's docs
    // stay purged). Model present ⇒ full purge-then-reinsert re-embed (`force_reembed`). Model
    // ABSENT ⇒ chunk-only backfill of documents with NO chunks yet (the write-only legacy rows) —
    // never a purge-then-reinsert of an already-chunked document, which would DESTROY its existing
    // real vectors without replacing them. The shared leg is kind-ROUTED (Brain v3 audit gap #3):
    // authored notes re-chunk through the front-matter-stripping path, never raw `documents.text`.
    let docs_indexed = backfill_document_chunks(db, unlocked, model_present, embedder, true, None)?;
    if docs_indexed > 0 {
        tracing::info!(target: "rag", docs_indexed, "reindex: document chunks backfilled");
    }

    if !model_present {
        tracing::info!(target: "rag", "reindex_embeddings: e5 model missing; skipping (no stub indexing)");
        return Ok(ReindexResult {
            status: "model_missing".to_string(),
            indexed: 0,
            total: 0,
        });
    }

    // The corpus is the visibility-gated meeting list. `list_meetings_visible` already excludes
    // sealed-and-not-session-unlocked meetings (their notes are not visible under `visibility_clause`).
    let meetings = db.list_meetings_visible(100_000, unlocked)?;
    let total = meetings.len();

    let mut indexed = 0usize;
    for m in &meetings {
        // Defense-in-depth: only index a meeting whose latest note is currently visible.
        match db.get_note_if_visible(&m.id, unlocked) {
            Ok(Some(_note)) => {
                // The meeting is visible ⇒ its segments are restored plaintext; index BOTH the note-
                // summary and the transcript chunks. A read failure on segments is logged + skipped
                // (never aborts the whole backfill).
                match db.get_segments(&m.id) {
                    Ok(segments) => {
                        if let Err(e) = db.index_meeting_chunks(&m.id, &segments, embedder) {
                            // Never abort the whole backfill on one bad note — log (no PII) and continue.
                            tracing::warn!(target: "rag", error = %e, "reindex: indexing one meeting failed (skipped)");
                        } else if let Err(e) =
                            db.index_meeting_topic_chunks(&m.id, &segments, embedder, unlocked)
                        {
                            // Brain v2 L1.1 — topic chunks follow the note/transcript index (whose
                            // clean-replace purge covers all chunk classes). Same best-effort posture.
                            tracing::warn!(target: "rag", error = %e, "reindex: topic indexing failed (skipped)");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(target: "rag", error = %e, "reindex: reading segments failed (skipped)");
                    }
                }
            }
            Ok(None) => {
                // No visible note (sealed sibling, or no note yet) — skip; do NOT index.
            }
            Err(e) => {
                tracing::warn!(target: "rag", error = %e, "reindex: visibility check failed (skipped)");
            }
        }
        indexed += 1;
        on_progress(indexed, total);
    }

    tracing::info!(target: "rag", indexed, total, "reindex_embeddings complete");
    Ok(ReindexResult {
        status: "indexed".to_string(),
        indexed,
        total,
    })
}

/// Brain v3 (audit gap #3) — KIND-ROUTED (re)index of one `documents`-table row. An authored note
/// (`kind='note'`) carries YAML front-matter in its raw `text` column, which the note-save path
/// deliberately STRIPS before chunking (`index_note_body_chunks` → `split_front_matter` — DESIGN
/// §1a: tags/properties must never pollute the vectors). The reindex/unseal paths previously called
/// raw [`Db::index_document_chunks`] for EVERY row regardless of kind, re-embedding notes WITH their
/// front-matter — this helper is the ONE routing seam they all share now.
///
/// GATING: NOT a new read path — callers only pass ids already admitted by a gated reader
/// (`visible_document_ids` / the unseal path's own CK-decrypted rows), the exact contract
/// `index_document_chunks` already documents ("caller MUST only invoke this for visible content").
pub(crate) fn index_document_row_kind_routed(
    db: &crate::storage::Db,
    document_id: &str,
    embedder: Option<&dyn crate::embed::Embedder>,
) -> Result<(), AppError> {
    index_document_row_kind_routed_progress(
        db,
        document_id,
        embedder,
        &crate::storage::db::no_embed_progress,
    )
}

/// [`index_document_row_kind_routed`] with a per-sub-batch embed-progress callback (Brain v3 PR-4,
/// Fix 3): the document-import path streams "Embedding k/M" to the FE as each embed sub-batch lands.
/// KIND-ROUTED exactly like the no-op variant — a note routes through `index_note_chunks_progress`,
/// an uploaded document through `index_document_chunks_progress`.
pub(crate) fn index_document_row_kind_routed_progress(
    db: &crate::storage::Db,
    document_id: &str,
    embedder: Option<&dyn crate::embed::Embedder>,
    embed_progress: &crate::storage::db::EmbedProgressFn<'_>,
) -> Result<(), AppError> {
    match db.get_note_row(document_id)? {
        Some(row) => {
            // `kind='note'` — mirror `update_note_doc_inner`'s title fallback + body strip exactly.
            let title = row.title.as_deref().unwrap_or(&row.name);
            let title = title.trim();
            let title = if title.is_empty() { "Untitled" } else { title };
            let (_yaml, body) = crate::storage::db::split_front_matter(&row.text);
            db.index_note_chunks_progress(document_id, title, &body, embedder, embed_progress)
        }
        // Uploaded document (`kind='document'`) — the raw-text path is correct (no front-matter).
        None => db.index_document_chunks_progress(document_id, embedder, embed_progress),
    }
}

/// The DOCUMENT backfill leg shared by [`reindex_embeddings_inner`] (`force_reembed = true`: the
/// user-triggered full pass — model present ⇒ purge-then-reinsert re-embed of EVERY visible doc)
/// and the startup repair tick [`backfill_missing_brain_indexes`] (`force_reembed = false`:
/// NEEDS-ONLY — touch a row only when it has no chunks at all, or has chunks but ZERO vectors while
/// the real model is present, so an idempotent re-run does no work). Chunk-only backfill (no
/// vectors) runs even with the model absent, exactly like the original reindex doc leg. Every row
/// routes through [`index_document_row_kind_routed`] (front-matter never re-enters a note's chunks).
///
/// GATING (lock-model): the corpus is exactly `visible_document_ids(unlocked)` — a
/// sealed-and-not-in-`unlocked` folder's documents are never returned, so their (blank) plaintext
/// is never chunked and their index rows STAY purged.
///
/// `repair_budget` scopes the repair tick's per-run resource envelope to THAT caller (the
/// per-call-bounds discipline): `Some` ⇒ the tick's posture — spend one budget slot (and pace by
/// [`REPAIR_TICK_PACING_MS`]) per REAL index op, stop at zero, and never count/spend a ZERO-YIELD
/// row (its needs-probe is unchanged, so it would re-burn the cap every launch); `None` ⇒ the
/// user-triggered reindex, unbounded and counted exactly as before.
fn backfill_document_chunks(
    db: &crate::storage::Db,
    unlocked: &std::collections::HashSet<String>,
    model_present: bool,
    embedder: &dyn crate::embed::Embedder,
    force_reembed: bool,
    mut repair_budget: Option<&mut usize>,
) -> Result<usize, AppError> {
    let doc_embedder = model_present.then_some(embedder);
    let mut docs_indexed = 0usize;
    for did in db.visible_document_ids(unlocked)? {
        if let Some(budget) = repair_budget.as_deref_mut() {
            if *budget == 0 {
                tracing::info!(
                    target: "rag",
                    docs_indexed,
                    "doc backfill: per-run repair cap reached; remainder deferred to the next launch"
                );
                break;
            }
        }
        let should_index = if force_reembed {
            model_present || !db.document_has_chunks(&did)?
        } else {
            // Repair-tick probe (counts only, no content): missing chunks entirely (chunk-only
            // backfill works model-less), or chunked-but-vectorless while the REAL model is present
            // (the model arrived after import) — never a wholesale re-embed.
            //
            // TODO(reflow): re-index EXISTING fragmented docs so their RETRIEVAL (not just preview)
            // uses de-fragmented text. The preview is already fixed on read (`render_display_text` +
            // `get_document_if_visible` reflow), and any FUTURE (re)index reflows the chunk input
            // (`index_document_chunks`). But an ALREADY-chunked fragmented doc has chunks>0/vectors>0,
            // so this needs-only probe skips it — its stored chunks stay built from mangled glyphs.
            // A naive "re-index if `reflow::looks_fragmented(stored_text)` fires" is UNSAFE here: reflow
            // is read-only, so `documents.text` STAYS fragmented after a re-index → the probe fires
            // again every tick → an unbounded re-chunk loop. A safe version needs a one-shot guard
            // (a new additive `documents.reflow_reindexed_at` column + write-back), which is NEW at-rest
            // state on the seal-adjacent `documents` table — out of scope for this read-only change and
            // deferred to a follow-up. Users can force it today via Settings → Reindex.
            let (chunks, vectors) = db.doc_chunk_vector_counts(&did)?;
            chunks == 0 || (model_present && vectors == 0)
        };
        if !should_index {
            continue;
        }
        match index_document_row_kind_routed(db, &did, doc_embedder) {
            Ok(()) => match repair_budget.as_deref_mut() {
                Some(budget) => {
                    // Fix 3c — spend the budget ONLY on a row whose index pass actually produced
                    // rows: re-run the needs-probe and treat "still missing" as zero-yield.
                    let (chunks, vectors) = db.doc_chunk_vector_counts(&did)?;
                    let still_missing = chunks == 0 || (model_present && vectors == 0);
                    if !still_missing {
                        docs_indexed += 1;
                        *budget -= 1;
                        // Pacing between REAL index ops (the topic backfill's posture).
                        std::thread::sleep(std::time::Duration::from_millis(REPAIR_TICK_PACING_MS));
                    }
                }
                None => docs_indexed += 1,
            },
            Err(e) => {
                // Never abort the whole backfill on one bad document — log (no PII) and continue.
                tracing::warn!(target: "rag", error = %e, "doc backfill: indexing one document failed (skipped)");
            }
        }
    }
    Ok(docs_indexed)
}

/// Brain v3 (audit gap #2) — the IDEMPOTENT startup REPAIR TICK. Vector/chunk coverage previously
/// only recovered via the manual Settings → Reindex: an embed-model download, a flag flip, or a
/// model-absent unlock left meetings/documents chunk- or vector-less until the user noticed.
/// Generalizes the `backfill_topic_chunks_idempotent` startup pattern; wired into the SAME
/// `lib.rs` setup `spawn_blocking` (behind the same `topic_backfill_ram_permits_now()` RAM floor).
///
/// THE CONSOLIDATION INVARIANT (load-bearing): every read runs under the EMPTY unlock set — a
/// sealed folder's meetings/documents are invisible here EVEN MID-SESSION-UNLOCK, so sealed
/// plaintext is never touched by a background task (this fn deliberately takes no unlock set).
///
/// Gating matrix (mirrors the existing code exactly):
/// - DOC half: needs-only [`backfill_document_chunks`] — chunk/FTS backfill runs even with the
///   model absent (the reindex doc-leg policy); vectors only when `model_present`.
/// - MEETING half: `index_meeting_chunks` has no chunk-only mode, so it requires the REAL model
///   (never a stub vector) AND — being an AUTOMATIC index — the `semantic_search_enabled` flag
///   (the pipeline's `should_auto_index` posture, same as the topic backfill's flag gate).
///
/// Idempotent: a re-run finds full coverage and touches nothing (returns `(0, 0)`). Logs counts
/// only — no PII.
///
/// CAPPED at [`REPAIR_TICK_MAX_INDEX_PER_RUN`] REAL index operations per run with a
/// [`REPAIR_TICK_PACING_MS`] pacing sleep between them — the exact
/// [`Db::backfill_topic_chunks_idempotent`] launch-freeze posture (2026-07-13): a vault with
/// hundreds of missing rows must never re-embed everything in ONE unthrottled launch pass. The
/// needs-probe is the cursor — a capped run just defers the remainder to the next launch. A
/// ZERO-YIELD row (one whose index pass produces no chunks/vectors — e.g. an empty-bodied note)
/// is never counted as work and never spends the cap, otherwise permanently-empty rows would
/// re-burn the cap on every launch and starve the real tail.
pub(crate) fn backfill_missing_brain_indexes(
    db: &crate::storage::Db,
    semantic_enabled: bool,
    model_present: bool,
    embedder: &dyn crate::embed::Embedder,
) -> Result<(usize, usize), AppError> {
    backfill_missing_brain_indexes_capped(
        db,
        semantic_enabled,
        model_present,
        embedder,
        REPAIR_TICK_MAX_INDEX_PER_RUN,
    )
}

/// Max REAL index operations (rows that actually produced chunks/vectors) per repair-tick run —
/// mirrors `TOPIC_BACKFILL_MAX_REEMBED_PER_RUN` in [`Db::backfill_topic_chunks_idempotent`].
const REPAIR_TICK_MAX_INDEX_PER_RUN: usize = 50;

/// Pacing sleep between REAL repair-tick index operations — breathing room for the shared DB
/// connection + the Metal queue, mirroring the topic backfill's per-embed pause.
const REPAIR_TICK_PACING_MS: u64 = 50;

/// [`backfill_missing_brain_indexes`] with the per-run cap INJECTED (the test seam — production
/// always passes [`REPAIR_TICK_MAX_INDEX_PER_RUN`]).
pub(crate) fn backfill_missing_brain_indexes_capped(
    db: &crate::storage::Db,
    semantic_enabled: bool,
    model_present: bool,
    embedder: &dyn crate::embed::Embedder,
    max_real_index_ops: usize,
) -> Result<(usize, usize), AppError> {
    let empty = std::collections::HashSet::new();
    // ONE budget across both halves — a launch tick is one resource envelope, however the missing
    // rows are split between documents and meetings.
    let mut remaining = max_real_index_ops;

    // DOC half — needs-only (never the reindex command's wholesale re-embed).
    let docs = backfill_document_chunks(
        db,
        &empty,
        model_present,
        embedder,
        false,
        Some(&mut remaining),
    )?;

    // MEETING half — flag + model gated (see the gating matrix above).
    let mut meetings = 0usize;
    if semantic_enabled && model_present {
        for m in db.list_meetings_visible(100_000, &empty)? {
            if remaining == 0 {
                tracing::info!(
                    target: "rag",
                    meetings,
                    docs,
                    "repair tick: per-run cap reached; remainder deferred to the next launch"
                );
                break;
            }
            // Defense-in-depth (the reindex idiom): only a meeting whose latest note is visible
            // under the SAME empty set is considered — and "has a note" is the tick's precondition.
            match db.get_note_if_visible(&m.id, &empty) {
                Ok(Some(_note)) => {}
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(target: "rag", error = %e, "repair tick: visibility check failed (skipped)");
                    continue;
                }
            }
            // Needs probe (counts only): note-but-zero-chunks, or chunks-but-zero-vectors.
            let (chunks, vectors) = match db.meeting_chunk_vector_counts(&m.id) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(target: "rag", error = %e, "repair tick: chunk probe failed (skipped)");
                    continue;
                }
            };
            if chunks > 0 && vectors > 0 {
                continue; // covered — the idempotent no-op leg.
            }
            match db.get_segments(&m.id) {
                Ok(segments) => {
                    if let Err(e) = db.index_meeting_chunks(&m.id, &segments, embedder) {
                        tracing::warn!(target: "rag", error = %e, "repair tick: indexing one meeting failed (skipped)");
                    } else {
                        // Fix 3c — count + spend the cap ONLY when the pass actually produced
                        // rows. A zero-yield meeting (empty note body AND no segments → no
                        // chunks) stays "missing" forever; counting it would re-burn the cap on
                        // every launch. Zero-yield ⇒ the segments were empty, so the topic
                        // indexer (segment-derived) is skipped as the no-op it would be.
                        match db.meeting_chunk_vector_counts(&m.id) {
                            Ok((chunks, vectors)) if chunks > 0 && vectors > 0 => {
                                meetings += 1;
                                remaining -= 1;
                                // Topic chunks follow the note/transcript index (the reindex
                                // idiom) — under the SAME empty unlock set (sealed context
                                // never persists).
                                if let Err(e) = db.index_meeting_topic_chunks(
                                    &m.id, &segments, embedder, &empty,
                                ) {
                                    tracing::warn!(target: "rag", error = %e, "repair tick: topic indexing failed (skipped)");
                                }
                                // Pacing between REAL index ops (the topic backfill's posture) —
                                // idempotent no-ops stay unpaced.
                                std::thread::sleep(std::time::Duration::from_millis(
                                    REPAIR_TICK_PACING_MS,
                                ));
                            }
                            Ok(_) => {} // zero-yield: not work, no cap spend, no pacing.
                            Err(e) => {
                                tracing::warn!(target: "rag", error = %e, "repair tick: yield probe failed (skipped)");
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(target: "rag", error = %e, "repair tick: reading segments failed (skipped)");
                }
            }
        }
    }
    Ok((meetings, docs))
}

/// Show/hide the floating recorder bar window (also bound to the global ⌘⇧R shortcut).
#[tauri::command]
pub fn toggle_bar(app: AppHandle) {
    crate::toggle_bar(&app);
}

// ── folders + per-folder lock lifecycle (PHASE0-PLAN Stage C) ──
//
// Lock model: default OPEN (note exported to vault + visible in MCP). Lock is explicit per
// folder. Sealing encrypts each note's markdown into `content_blob` under a per-folder content
// key (CK), blanks the markdown column, removes the `.md` from the vault, and stores the
// KEK-wrapped CK in `folders.wrapped_key`. Session-unlock decrypts back into the markdown column
// for the session (no re-export). relock re-blanks. remove_lock is permanent (re-exports).

/// Build the folder tree (roots → children) from the flat folder list + per-folder note counts +
/// the current session unlock set.
#[tauri::command]
pub fn list_folders(state: State<'_, AppState>) -> Result<Vec<FolderNode>, AppError> {
    let folders = state.db.list_folders()?;
    let unlocked = {
        state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
            .clone()
    };
    // Gated by the session unlock set — a sealed-and-not-unlocked folder's notes must not
    // contribute to note_count (see count_notes_per_folder doc + .claude/rules/lock-model.md).
    let counts = state.db.count_notes_per_folder(&unlocked)?;
    let kinds = state.db.folder_kinds()?;
    Ok(build_folder_tree(&folders, &counts, &unlocked, &kinds))
}

/// Assemble `FolderNode` roots (parent_id == None) and recurse children. Sealed-but-session-
/// unlocked folders carry `unlocked = true`.
fn build_folder_tree(
    folders: &[Folder],
    counts: &std::collections::HashMap<String, usize>,
    unlocked: &std::collections::HashSet<String>,
    kinds: &std::collections::HashMap<String, String>,
) -> Vec<FolderNode> {
    fn node(
        f: &Folder,
        folders: &[Folder],
        counts: &std::collections::HashMap<String, usize>,
        unlocked: &std::collections::HashSet<String>,
        kinds: &std::collections::HashMap<String, String>,
    ) -> FolderNode {
        let children = folders
            .iter()
            .filter(|c| c.parent_id.as_deref() == Some(f.id.as_str()))
            .map(|c| node(c, folders, counts, unlocked, kinds))
            .collect();
        FolderNode {
            id: f.id.clone(),
            name: f.name.clone(),
            parent_id: f.parent_id.clone(),
            note_count: counts.get(&f.id).copied().unwrap_or(0),
            locked: f.locked,
            unlocked: f.locked && unlocked.contains(&f.id),
            kind: kinds
                .get(&f.id)
                .cloned()
                .unwrap_or_else(|| "meeting".to_string()),
            children,
        }
    }
    folders
        .iter()
        .filter(|f| f.parent_id.is_none())
        .map(|f| node(f, folders, counts, unlocked, kinds))
        .collect()
}

/// Create a folder under an optional parent. The vault-relative path is derived from the parent
/// path + the sanitized folder name; the matching vault subdirectory is created on disk.
#[tauri::command]
pub fn create_folder(
    state: State<'_, AppState>,
    name: String,
    parent_id: Option<String>,
) -> Result<Folder, AppError> {
    let clean = crate::summarize::organize::sanitize_folder(&name)
        .ok_or_else(|| AppError::InvalidArg("folder name is empty or invalid".into()))?;

    // Resolve the parent's vault-relative path (if any) and compose the child path.
    let parent_path = match parent_id.as_deref() {
        Some(pid) => {
            let parent = state
                .db
                .folder_by_id(pid)?
                .ok_or_else(|| AppError::InvalidArg(format!("no parent folder {pid}")))?;
            Some(parent.path)
        }
        None => None,
    };
    let rel_path = match &parent_path {
        Some(p) if !p.is_empty() => format!("{p}/{clean}"),
        _ => clean.clone(),
    };

    // Create the vault subdirectory (best-effort but surfaced): only when a vault is configured.
    // D5: canonicalize + assert the composed dir stays inside the vault root before any mkdir.
    if let Some(vault) = vault_path(&state) {
        let vault_root = std::path::Path::new(&vault);
        let dir = assert_in_vault(vault_root, std::path::Path::new(&rel_path))?;
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppError::Export(format!("create folder dir failed: {e}")))?;
    }

    let folder = Folder {
        id: uuid::Uuid::new_v4().to_string(),
        name: clean,
        path: rel_path,
        parent_id,
        locked: false,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    state.db.insert_folder(&folder)?;
    Ok(folder)
}

/// Reject filing a MEETING under a NOTE folder — the folder namespaces are disjoint (a meeting's
/// `.md` lives under a meeting folder; a note under a note folder). Called before any DB reassign
/// or FS move so a cross-namespace target can never take effect. `None` (the vault root) is always
/// valid. Returns `Ok(())` for a meeting folder or an unknown id (the caller's own resolve reports
/// a genuinely-missing folder).
fn ensure_meeting_folder_target(
    db: &crate::storage::db::Db,
    folder_id: Option<&str>,
) -> Result<(), AppError> {
    if let Some(fid) = folder_id {
        if db.folder_kind(fid)?.as_deref() == Some("note") {
            return Err(AppError::InvalidArg(
                "cannot move a meeting into a note folder".into(),
            ));
        }
    }
    Ok(())
}

/// Move a note into a folder (or to the root with `folder_id = None`).
///
/// Three cases by TARGET:
/// - **open / root:** if the note has an exported `.md` the file is moved on disk (copy-then-remove,
///   best-effort, never loses bytes).
/// - **locked + SESSION-UNLOCKED (CK available):** reassign, then SEAL the moved note to the
///   folder's at-rest sealed shape (encrypt markdown/transcript/timeline into blobs, blank the
///   plaintext, remove the vault `.md`, encrypt the WAV) so plaintext never lands in a locked
///   folder (BLK-2). Verify-before-destroy throughout.
/// - **locked + NOT session-unlocked:** REJECTED with [`AppError::Locked`] — there is no CK to seal
///   with, so we refuse rather than leave plaintext in a locked folder. The FE must unlock first.
#[tauri::command]
pub fn move_note(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
    folder_id: Option<String>,
) -> Result<(), AppError> {
    move_note_command_body(&app, state, meeting_id, folder_id)
}

/// Body of [`move_note`], split so the audit-inbox ping fires once after EVERY successful move
/// (a move INTO a locked folder seals + purges ALL pending findings via
/// `purge_chunks_for_meetings`; an open-target move purges nothing — the count-only ping is
/// correct either way).
fn move_note_command_body(
    app: &AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
    folder_id: Option<String>,
) -> Result<(), AppError> {
    move_note_inner_impl(&state, meeting_id, folder_id)?;
    emit_audit_updated_after_purge(app, state.inner());
    Ok(())
}

fn move_note_inner_impl(
    state: &State<'_, AppState>,
    meeting_id: String,
    folder_id: Option<String>,
) -> Result<(), AppError> {
    // Resolve current + target folder lock state.
    let note = state.db.get_latest_note_for_meeting(&meeting_id)?;

    // A MEETING may only be filed under a MEETING folder — never a note folder
    // (the folder namespaces are disjoint; filing a meeting into the Notes
    // namespace is the folder-leak's mirror). The FE meeting move-menu already
    // hides note folders, but gate the write too so a bypassed/typo'd id can't
    // cross the namespace boundary (2026-07-14).
    ensure_meeting_folder_target(&state.db, folder_id.as_deref())?;

    let target_locked = match folder_id.as_deref() {
        Some(fid) => {
            state
                .db
                .folder_by_id(fid)?
                .ok_or_else(|| AppError::InvalidArg(format!("no folder {fid}")))?
                .locked
        }
        None => false,
    };

    // ── Target is a LOCKED folder: seal-or-reject (BLK-2) ───────────────────────────────────────
    if target_locked {
        let fid = folder_id
            .as_deref()
            .expect("locked target implies Some(folder_id)");
        return move_into_locked_folder(state.inner(), &meeting_id, fid);
    }

    // ── Target is OPEN / root: existing reassign + best-effort FS move ──────────────────────────
    // The source folder's lock state: derive from the note's exported_path being present
    // (sealed notes have exported_path = NULL). If exported_path is None we treat the source as
    // "no movable file" and skip the FS move entirely.
    let exported = note.as_ref().and_then(|n| n.exported_path.clone());

    // Reassign in the DB first (the source-of-truth association). Targets EVERY provider row of
    // the meeting (WHERE meeting_id = ?1) so the meeting's folder is consistent across providers
    // and the seal/unlock lifecycle (which iterates provider rows) stays coherent.
    state
        .db
        .set_meeting_folder(&meeting_id, folder_id.as_deref())?;

    // Best-effort FS move only when a plaintext .md exists (target is open here).
    if let Some(src_path) = exported {
        if let Some(vault) = vault_path(state) {
            let target_rel = match folder_id.as_deref() {
                Some(fid) => state.db.folder_by_id(fid)?.map(|f| f.path),
                None => None,
            };
            move_note_file(
                state,
                &meeting_id,
                &src_path,
                &vault,
                target_rel.as_deref(),
            )?;
        }
    }
    Ok(())
}

/// BLK-2: move a meeting's note INTO a `locked` folder, sealing it to the folder's at-rest shape so
/// plaintext never lands in a locked folder. Requires the folder to be SESSION-UNLOCKED (its CK is
/// derivable from the cached KEK); otherwise REJECTS with [`AppError::Locked`]. Holds the lifecycle
/// guard for the whole reassign+seal so it can't interleave with a relock/remove-lock.
fn move_into_locked_folder(
    state: &AppState,
    meeting_id: &str,
    folder_id: &str,
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);

    // Must be session-unlocked — otherwise we have no CK to seal the moved note with.
    let session_unlocked = state
        .unlocked_folders
        .lock()
        .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
        .contains(folder_id);
    if !session_unlocked {
        return Err(AppError::Locked(
            "the destination folder is locked — unlock it first, then move the note".into(),
        ));
    }

    // The folder's existence + locked state were already validated by `move_note` (target_locked).
    let wrapped = state
        .db
        .folder_wrapped_key(folder_id)?
        .ok_or_else(|| AppError::Storage("locked folder has no wrapped key".into()))?;

    // The KEK is cached for a session-unlocked folder. If it is somehow absent (e.g. zeroized by a
    // concurrent relock between the unlock-set check and here), fail closed — never seal without a
    // verified CK.
    let kek: Zeroizing<[u8; 32]> = {
        let g = state
            .master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        g.clone().ok_or_else(|| {
            AppError::Locked(
                "the destination folder is locked — unlock it first, then move the note".into(),
            )
        })?
    };
    let ck_bytes = Zeroizing::new(crate::crypto::decrypt(
        &kek,
        &wrapped,
        &aad_wrapped_ck(folder_id),
    )?);
    let ck: Zeroizing<[u8; 32]> = Zeroizing::new(
        ck_bytes
            .as_slice()
            .try_into()
            .map_err(|_| AppError::Storage("unwrapped content key has wrong length".into()))?,
    );

    // Reassign EVERY provider row of the meeting into the locked folder (the source-of-truth
    // association), THEN seal that one meeting's note + extras under the folder CK.
    state.db.set_meeting_folder(meeting_id, Some(folder_id))?;
    seal_moved_note(state, folder_id, meeting_id, &ck)?;

    // The destination folder is SESSION-UNLOCKED (checked above), so the moved note MUST be READABLE
    // in-session exactly like its folder-mates — otherwise it shows EMPTY under the open padlock (the
    // seal just blanked its plaintext and nothing restored it: the "private note came back empty"
    // bug). Decrypt the just-sealed note markdown + transcript/timeline/audio back into the plaintext
    // columns FOR THE SESSION; every `content_blob`/`*_blob`/`.enc` STAYS at rest (folder is still
    // `locked=1` on disk), so a relock/app-kill re-seals cleanly with no plaintext left behind. This
    // is verify-clean by construction: `seal_moved_note` above already proved each blob decrypts back
    // byte-identical BEFORE it blanked anything, so this restore can only reproduce that same content.
    // Mirrors `unlock_folder`'s per-meeting restore, scoped to this one moved meeting. Under the
    // lifecycle guard held for the whole move, so no relock can interleave.
    for n in &state.db.sealable_notes_for_meeting(meeting_id)? {
        let Some(blob) = &n.content_blob else {
            continue;
        };
        let aad = aad_content(folder_id, meeting_id, &n.provider_id, "note");
        let pt = crate::crypto::decrypt(&ck, blob, &aad)?;
        let markdown = String::from_utf8(pt)
            .map_err(|_| AppError::Storage("decrypted moved note is not valid UTF-8".into()))?;
        state
            .db
            .restore_note_markdown(meeting_id, &n.provider_id, &markdown)?;
    }
    unseal_meeting_extras(state, folder_id, meeting_id, &ck)?;
    // Re-index this one meeting so semantic search / related-meetings recover in-session (its note
    // markdown was just restored above). Model-gated (never a stub vector); best-effort — a re-index
    // hiccup must not fail (or half-undo) the completed move.
    let meeting_embedder = crate::embed::embed_model_present().then(crate::embed::active_embedder);
    reindex_meetings_after_unseal(
        state,
        &[meeting_id.to_string()],
        meeting_embedder.as_deref(),
    );
    Ok(())
}

/// The auto-organize safety decision for a classifier-chosen vault subfolder (BLK-2 parity for the
/// summarize pipeline). A freshly auto-filed note's plaintext `.md` must NEVER land in a LOCKED
/// folder's on-disk directory with `folder_id = NULL`: a later `lock_folder` and the at-rest
/// reconcile both key off `folder_id` and would miss it, leaving plaintext in a sealed dir forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoFileTarget {
    /// The subfolder is open / root / unmanaged (no matching folder row, or not locked) — write the
    /// plaintext note there as usual.
    Open,
    /// The subfolder is a SESSION-UNLOCKED locked folder — write the note, then seal it INTO this
    /// folder id (encrypt markdown/extras, remove the plaintext `.md`), exactly like a manual move.
    SealInto(String),
    /// The subfolder is a LOCKED, NOT-session-unlocked folder — there is no CK to seal with, so the
    /// note must NOT be written here. The caller writes it at the vault root instead (reject).
    RejectToRoot,
}

/// Classify where a summarize-pipeline note may be auto-filed, given the classifier-chosen
/// vault-relative `subfolder`. Pure lookup — performs NO writes. See [`AutoFileTarget`]. A subfolder
/// matching no folder row, or a non-locked folder, is [`AutoFileTarget::Open`]. The pipeline calls
/// this BEFORE writing the note so plaintext is never written into a sealed dir in the first place.
pub fn classify_auto_file_target(
    state: &AppState,
    subfolder: Option<&str>,
) -> Result<AutoFileTarget, AppError> {
    let Some(sub) = subfolder.filter(|s| !s.is_empty()) else {
        return Ok(AutoFileTarget::Open);
    };
    let Some(folder) = state.db.folder_by_path(sub)? else {
        return Ok(AutoFileTarget::Open);
    };
    if !folder.locked {
        return Ok(AutoFileTarget::Open);
    }
    let session_unlocked = state
        .unlocked_folders
        .lock()
        .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
        .contains(&folder.id);
    if session_unlocked {
        Ok(AutoFileTarget::SealInto(folder.id))
    } else {
        Ok(AutoFileTarget::RejectToRoot)
    }
}

/// Seal a just-auto-filed note INTO a session-unlocked locked folder — the SAME BLK-2 path a manual
/// [`move_note`] into a locked folder takes (reassign every provider row + encrypt markdown/extras +
/// remove the plaintext `.md`). Called by the pipeline only after [`classify_auto_file_target`]
/// returned [`AutoFileTarget::SealInto`]. On the rare race where the folder was relocked in between,
/// [`move_into_locked_folder`] returns `Err(Locked)` BEFORE touching state; the caller then removes
/// the stray plaintext `.md` so it never survives in a sealed dir (the note's markdown is still in
/// the DB, recoverable).
pub fn seal_auto_filed_note(
    state: &AppState,
    meeting_id: &str,
    folder_id: &str,
) -> Result<(), AppError> {
    move_into_locked_folder(state, meeting_id, folder_id)
}

/// LOCK-STATE FINALIZER for a from-disk pipeline re-run (`pipeline::run_salvage_from_disk` —
/// `retry_transcription` + startup disk salvage). The re-run inserts fresh plaintext
/// segments/audio exactly like a live Stop; for a meeting whose folder is LOCKED that fresh
/// output must end up governed by the folder CK, matching what the normal pipeline's
/// `SealInto` auto-file path produces. Called AFTER the pipeline on BOTH arms (an Err run may
/// already have persisted segments before failing). Three branches, keyed on the folder's
/// CURRENT lock state:
///
/// - **No folder / open folder** (every startup-salvage row — a stuck `RECORDING` meeting has no
///   note rows, so no folder): nothing to do.
/// - **Locked + session-unlocked** (a `retry_transcription` of a meeting in an unlocked locked
///   folder): durably re-seal the fresh note/segments/timeline/audio under the folder CK via
///   [`seal_auto_filed_note`] — the SAME verify-before-blank path a manual move / auto-file
///   takes, which also restores the session plaintext afterward. Content is never lost.
/// - **Locked + NOT unlocked** (a relock/screen-share auto-relock landed MID-RUN — the entry gate
///   had passed): fail CLOSED. The relock's re-blank only covers rows with sealed blobs, so the
///   fresh unsealed rows would otherwise sit plaintext-at-rest behind the lock forever. Purge the
///   blob-less segments ([`Db::delete_unsealed_segments`] — derived data; the audio survives) and
///   remove the plaintext WAV ONLY when its sealed `.enc` twin exists (never destroy the only
///   copy). The row is already terminal `Error` (the note persist fail-closed with
///   `AppError::Locked` inside the pipeline) and recoverable: unlock → retry.
///
/// Best-effort by contract: every failure is logged (ids/counts only) and swallowed — the
/// finalizer must never mask the pipeline's own result.
pub(crate) fn finalize_salvage_lock_state(state: &AppState, meeting_id: &str) {
    let locked_folder = match state
        .db
        .folder_for_meeting(meeting_id)
        .and_then(|fid| match fid {
            Some(fid) => state.db.folder_by_id(&fid),
            None => Ok(None),
        }) {
        Ok(f) => f.filter(|f| f.locked),
        Err(e) => {
            tracing::warn!(target: "lock", meeting_id = %meeting_id, error = %e, "salvage lock finalizer: folder lookup failed");
            return;
        }
    };
    let Some(folder) = locked_folder else {
        return; // unfiled or open folder — the normal plaintext outcome is correct.
    };
    let session_unlocked = state
        .unlocked_folders
        .lock()
        .map(|u| u.contains(&folder.id))
        .unwrap_or(false); // poisoned set ⇒ treat as NOT unlocked (fail closed).
    if session_unlocked {
        // Same SealInto path as auto-file: seal fresh note+extras+audio (verify-before-blank),
        // then restore the session plaintext. Idempotent for already-sealed rows.
        if let Err(e) = seal_auto_filed_note(state, meeting_id, &folder.id) {
            tracing::warn!(target: "lock", meeting_id = %meeting_id, error = %e, "salvage lock finalizer: re-seal into the unlocked locked folder failed");
        } else {
            tracing::info!(target: "lock", meeting_id = %meeting_id, "salvage output re-sealed into its session-unlocked locked folder");
        }
        return;
    }
    // Mid-run relock: purge the unsealed plaintext leftovers, fail closed.
    match state.db.delete_unsealed_segments(meeting_id) {
        Ok(n) if n > 0 => {
            tracing::warn!(target: "lock", meeting_id = %meeting_id, purged = n, "salvage raced a relock — purged unsealed plaintext segments (audio stays sealed at rest; unlock + retry to re-transcribe)");
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(target: "lock", meeting_id = %meeting_id, error = %e, "salvage lock finalizer: unsealed-segment purge failed");
        }
    }
    // Remove the freshly-written plaintext WAV ONLY when the sealed `.enc` twin exists — the
    // audio content must never be destroyed without a surviving sealed copy.
    if let Ok(Some(m)) = state.db.get_meeting(meeting_id) {
        if let Some(path) = m.audio_path.as_deref().filter(|p| !p.ends_with(ENC_SUFFIX)) {
            let enc = format!("{path}{ENC_SUFFIX}");
            let sealed_copy_exists = std::path::Path::new(&enc).exists();
            if sealed_copy_exists
                && std::path::Path::new(path).exists()
                && std::fs::remove_file(path).is_ok()
            {
                tracing::warn!(target: "lock", meeting_id = %meeting_id, "salvage raced a relock — removed the plaintext session WAV (sealed .enc retained)");
            }
        }
    }
}

/// Seal ONE just-moved meeting's note (every provider row) + its transcript/timeline/audio under the
/// folder CK, removing each row's vault `.md`. Verify-before-blank per row (mirrors `lock_folder`):
/// the markdown is only blanked once its blob reads back identical, so a moved note is never lost.
fn seal_moved_note(
    state: &AppState,
    folder_id: &str,
    meeting_id: &str,
    ck: &[u8; 32],
) -> Result<(), AppError> {
    let notes = state.db.sealable_notes_for_meeting(meeting_id)?;
    // Encrypt + VERIFY every provider row BEFORE any blank, so a failure leaves intact plaintext.
    let mut sealed_rows: Vec<(String, Vec<u8>)> = Vec::new();
    let mut exported_paths: Vec<String> = Vec::new();
    for n in &notes {
        // Skip a row already sealed (blob present + markdown blanked) — idempotent.
        if n.content_blob.is_some() && n.markdown.is_empty() {
            continue;
        }
        let aad = aad_content(folder_id, meeting_id, &n.provider_id, "note");
        let blob = crate::crypto::encrypt(ck, n.markdown.as_bytes(), &aad)?;
        if crate::crypto::decrypt(ck, &blob, &aad)? != n.markdown.as_bytes() {
            return Err(AppError::Storage(
                "seal verification failed on moved note (decrypted blob mismatch)".into(),
            ));
        }
        sealed_rows.push((n.provider_id.clone(), blob));
        if let Some(p) = n.exported_path.clone() {
            exported_paths.push(p);
        }
    }
    for (provider_id, blob) in &sealed_rows {
        state.db.seal_note(meeting_id, provider_id, blob)?;
    }
    // Seal the moved meeting's transcript + timeline + audio under the SAME CK.
    seal_meeting_extras(state, folder_id, meeting_id, ck)?;
    // AFTER the column writes, remove the vault `.md` files (a leftover .md is reconcilable; lost
    // content is not — so this is last).
    // Export-collision guard: NO external-edit sibling on a seal delete — privacy wins over
    // preservation (a plaintext sibling of a to-be-sealed note would be a leak; see the same
    // decision in `lock_folder_inner`).
    for p in exported_paths {
        let _ = std::fs::remove_file(&p);
    }
    // The note's chunks/vectors are plaintext-derived and a dense embedding is invertible, so they
    // must NOT survive at rest for a meeting now sealed into a locked folder — same invariant the
    // lock_folder / relock / startup-reconcile paths enforce. Covers both the manual move-into-locked
    // and the auto-file callers. (Re-indexed on unlock once indexing ships.) The same tx purges ALL
    // memory rollups (cross-meeting synthesis that may paraphrase the just-sealed facts) — remove
    // their exported vault `.md`s here, like the note `.md`s above.
    let rollup_exports = state
        .db
        .purge_chunks_for_meetings(&[meeting_id.to_string()])?;
    remove_rollup_export_files(&rollup_exports);
    Ok(())
}

/// Move the exported `.md` to the target folder's vault subdir, preserving content. Re-points
/// the note's `exported_path`. Copy-then-remove so a failure never loses bytes.
fn move_note_file(
    state: &State<'_, AppState>,
    meeting_id: &str,
    src_path: &str,
    vault: &str,
    target_rel: Option<&str>,
) -> Result<(), AppError> {
    let src = std::path::Path::new(src_path);
    let bytes = match std::fs::read_to_string(src) {
        Ok(b) => b,
        // Source file already gone → nothing to move; leave DB association as set.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(AppError::Export(format!("read note for move failed: {e}"))),
    };
    let file_name = src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| AppError::Export("note path has no filename".into()))?;
    let vault_root = std::path::Path::new(vault);
    // D5: the destination (vault root + target folder rel-path + filename) must stay inside the
    // vault. Compose the vault-relative candidate and canonicalize+assert containment before any FS
    // write. `file_name` is derived from the real source path, but we still re-check it carries no
    // traversal segment.
    let rel_candidate = match target_rel.filter(|p| !p.is_empty()) {
        Some(rel) => std::path::Path::new(rel).join(file_name),
        None => std::path::PathBuf::from(file_name),
    };
    let dest = assert_in_vault(vault_root, &rel_candidate)?;
    let dest_dir = dest
        .parent()
        .ok_or_else(|| AppError::Export("destination has no parent dir".into()))?
        .to_path_buf();
    // Same-location no-op. `dest` is canonicalized (absolute, symlinks resolved) but `src` from the
    // DB is not — compare the CANONICALIZED source so a move to the same underlying file is detected
    // even when the path strings differ (e.g. /var vs /private/var on macOS). Skipping this would let
    // the copy-then-remove below delete the file it just wrote (data loss).
    let src_canon = src.canonicalize().unwrap_or_else(|_| src.to_path_buf());
    if dest == src_canon || dest == src {
        return Ok(());
    }
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| AppError::Export(format!("create move dir failed: {e}")))?;
    // Write the destination atomically, THEN remove the source (never lose bytes).
    // Export-collision guard: a move copies the CURRENT file bytes verbatim (external edits ride
    // along), so it is NOT a Murmur-authored write — the stored `exported_hash` baseline is
    // deliberately LEFT ALONE. It still describes what Murmur last authored, so an external edit
    // made before the move is still detected (and preserved) by the next DB-derived overwrite at
    // the new path. Re-stamping from the moved bytes would erase that signal.
    crate::export::overwrite_note(&dest, &bytes)?;
    let _ = std::fs::remove_file(src);
    // Re-point the exported path for every provider row of this meeting.
    if let Some(existing) = state.db.get_latest_note_for_meeting(meeting_id)? {
        state.db.set_note_exported_path(
            meeting_id,
            &existing.provider_id,
            &dest.to_string_lossy(),
        )?;
    }
    Ok(())
}

/// BLK-1: acquire the coarse [`AppState::lifecycle`] guard so a folder-lock state-machine op never
/// interleaves with another (notably the off-thread `relock_all_inner`). A `Mutex<()>` carries no
/// state, so a poisoned lock is recovered via `into_inner()` — never bricking all future lock ops.
/// `pub(crate)` for the pipeline's persist/export critical sections (residual W4) — NEVER hold it
/// across an `await` or around any callee that takes it (`lock_folder` / `relock_*` /
/// `remove_lock` / `move_into_locked_folder` → `seal_auto_filed_note`).
pub(crate) fn lifecycle_guard(state: &AppState) -> std::sync::MutexGuard<'_, ()> {
    state
        .lifecycle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// R7 (2026-07-10) — bump the SEAL EPOCH ([`AppState::seal_epoch`]): the monotonic counter every
/// lock-surface mutation advances at ENTRY (before any blank/purge), so the hourly memory
/// consolidation job can detect a seal/relock/remove-lock that interleaved with its pass and
/// abort BEFORE writing a rollup derived from a pre-seal fact read (the pass-vs-seal TOCTOU —
/// see [`crate::memory::run_consolidation_pass`]). Bumping at entry is load-bearing: the seal's
/// own rollup purge runs AFTER the bump, so a job that passes its epoch check pre-bump has its
/// write cleaned up by the purge, and a job that checks post-bump aborts — no ordering leaves a
/// stale rollup behind. Content-free (a counter), infallible, never blocks.
pub(crate) fn bump_seal_epoch(state: &AppState) {
    state
        .seal_epoch
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// Phase-0 lock-review follow-up: count `<stem> (external edit …).md` siblings sitting next to
/// the given exported `.md` paths and WARN (counts only — paths embed note titles). The seal /
/// relock paths delete ONLY the canonical exports; an external-edit sibling is USER-AUTHORED
/// content the collision guard preserved, so it is deliberately NEVER deleted — but it is
/// plaintext that survives the seal on disk, and that exposure must at least be visible.
pub(crate) fn warn_external_edit_siblings<'a>(stage: &str, paths: impl Iterator<Item = &'a String>) {
    let mut siblings = 0usize;
    for p in paths {
        let path = std::path::Path::new(p);
        let (Some(parent), Some(stem)) = (path.parent(), path.file_stem().and_then(|s| s.to_str()))
        else {
            continue;
        };
        let prefix = format!("{stem} (external edit ");
        let Ok(entries) = std::fs::read_dir(parent) else {
            continue;
        };
        for e in entries.flatten() {
            if let Some(name) = e.file_name().to_str() {
                if name.starts_with(&prefix) && name.ends_with(".md") {
                    siblings += 1;
                }
            }
        }
    }
    if siblings > 0 {
        tracing::warn!(
            target: "lock",
            stage,
            siblings,
            "external-edit sibling .md files remain next to sealed notes' exports — left in place (user-authored, never deleted by the seal), but they are plaintext outside the seal"
        );
    }
}

/// Lock-surface RAM hygiene: clear the live-transcript buffer ONLY when no recording is active —
/// never wipe an in-flight buffer (mid-recording egress correctness is owned by the visibility
/// gate in `transcribe::live`). Fail-safe: a poisoned recorder lock is treated as "recording".
pub(crate) fn clear_stale_live_transcript(state: &AppState) {
    let recording = state.recorder.lock().map(|g| g.is_some()).unwrap_or(true);
    crate::transcribe::live::clear_live_transcript_if_idle(&state.live_transcript, recording);
    // Brain v2 L4: the running-bullets RAM mirrors the live buffer — same idle-only hygiene
    // (never wipe an in-flight recording's bullets; mid-recording correctness is owned by the
    // `gated_live_bullets` visibility gate).
    if !recording {
        crate::transcribe::bullets::clear_ram(&state.live_bullets, &state.live_bullets_tracker);
    }
}

/// C1/C2 — best-effort in-process teardown of EVERY capture path on a true app exit
/// (`RunEvent::ExitRequested`, via the `lib::relock_and_zeroize_on_lifecycle` hook). Without this,
/// quitting mid-recording never runs any `stop()`, so the Swift capture helpers (system-audio /
/// AEC) reparent to launchd and keep writing their temp WAVs until their 4h self-cap — the
/// next-launch reaper (`aec::reap_orphaned_capture_helpers`) becomes the ONLY thing that reclaims
/// them. Calling this on a clean exit finalizes + reaps them in-process, so the reaper is the true
/// safety net rather than the primary path.
///
/// For each slot: `take()` it OUT of `AppState` under its own lock (deterministic — no `Drop`
/// ordering ambiguity), then let the value's teardown run:
///   - `system_recorder` / `aec_recorder`: `.stop()` SIGTERMs the helper so it flushes its WAV, then
///     reaps it (their `Drop` would do the same, but the explicit `stop()` matches the clean-Stop
///     path and consumes the value so the `Drop` no-ops).
///   - `recorder` / `spill_writer`: just `take()` — their existing `Drop` handles teardown (the
///     spill guard deletes the plaintext spill; the recorder stops its cpal stream) and runs
///     deterministically at end of scope.
///
/// Panic-free: a POISONED slot mutex is skipped (`.lock().ok()`), a `stop()` error is ignored — this
/// is a last-chance exit hook with no `Result` to surface. Never touches the DB / lock model. NOTE:
/// call this ONLY on the true exit path, never on a mere window-hide (the app keeps recording in the
/// tray then — see `lib::relock_and_zeroize_on_lifecycle`).
pub(crate) fn stop_all_capture(state: &AppState) {
    // System-audio sidecar: SIGTERM-flush + reap.
    if let Some(rec) = state
        .system_recorder
        .lock()
        .ok()
        .and_then(|mut slot| slot.take())
    {
        let _ = rec.stop();
    }
    // AEC (VPIO) helper: SIGTERM-flush + reap.
    if let Some(rec) = state
        .aec_recorder
        .lock()
        .ok()
        .and_then(|mut slot| slot.take())
    {
        let _ = rec.stop();
    }
    // The cpal mic recorder: `Drop` stops its stream — `take()` makes that deterministic here.
    let _mic = state.recorder.lock().ok().and_then(|mut slot| slot.take());
    // The crash-salvage spill writer: `Drop` stops the thread + deletes the plaintext spill.
    let _spill = state
        .spill_writer
        .lock()
        .ok()
        .and_then(|mut slot| slot.take());
}

/// Rename a folder: change its display `name` (and the matching vault subdirectory + every governed
/// `path`) without ever touching sealed content.
///
/// Steps, ordered so a crash never loses content:
///  1. Sanitize the new name (same component-safe rule as `create_folder`; reject `/`, `..`, NUL).
///  2. Recompose this folder's vault-relative path = parent path + sanitized name.
///  3. If a vault is configured, MOVE the on-disk subdir `old_path` → `new_path` (best-effort rename;
///     a missing source is fine). The dir holds only the OPEN folder's plaintext `.md`s — sealed
///     folders keep their `.md`s deleted, so a locked-folder rename just renames an empty/absent dir.
///  4. Update the `folders` row (name + path) and re-prefix the path of EVERY descendant folder, and
///     re-point EVERY affected note's `exported_path` from `old_path/...` → `new_path/...`. Sealed
///     notes have `exported_path = NULL` and are skipped — a LOCKED folder rename is metadata-only and
///     never reaches the sealed blob / wrapped key (no decrypt, no re-seal).
///
/// Idempotent-ish: renaming to the same (sanitized) name is a no-op move + a column rewrite to the
/// same values.
#[tauri::command]
pub fn rename_folder(
    state: State<'_, AppState>,
    folder_id: String,
    new_name: String,
) -> Result<Folder, AppError> {
    rename_folder_inner(state.inner(), folder_id, new_name)
}

/// Inner of [`rename_folder`] taking `&AppState` (so tests can drive it without a `tauri::State`).
/// Holds the [`AppState::lifecycle`] guard across the whole rename (path rewrites the seal/unseal
/// lifecycle keys FS ops off — see the command doc).
pub(crate) fn rename_folder_inner(
    state: &AppState,
    folder_id: String,
    new_name: String,
) -> Result<Folder, AppError> {
    // BLK-1: serialize with the rest of the lock state machine. A rename never decrypts, but it
    // rewrites `path` columns that the seal/unseal lifecycle keys vault FS ops off — hold the guard
    // so it can't interleave with a concurrent lock/unlock/remove that also rewrites paths.
    let _lifecycle = lifecycle_guard(state);

    let clean = crate::summarize::organize::sanitize_folder(&new_name)
        .ok_or_else(|| AppError::InvalidArg("folder name is empty or invalid".into()))?;

    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    let old_path = folder.path.clone();

    // Recompose this folder's path from its PARENT's path + the new sanitized name.
    let parent_path = match folder.parent_id.as_deref() {
        Some(pid) => state.db.folder_by_id(pid)?.map(|p| p.path),
        None => None,
    };
    let new_path = match parent_path.as_deref() {
        Some(p) if !p.is_empty() => format!("{p}/{clean}"),
        _ => clean.clone(),
    };

    // No-op fast path: same path AND same name → nothing to move/rewrite.
    if new_path == old_path && clean == folder.name {
        return Ok(Folder {
            name: clean,
            path: new_path,
            ..folder
        });
    }

    // Move the on-disk vault subdir, if a vault is configured. Both ends are containment-checked.
    // `std::fs::rename` moves the WHOLE subtree (including descendant `.md`s) in one atomic op.
    let mut vault_configured = false;
    if new_path != old_path {
        if let Some(vault) = vault_path(state) {
            vault_configured = true;
            let vault_root = std::path::Path::new(&vault);
            // Destination must stay inside the vault; the source is an existing in-vault dir.
            let dest = assert_in_vault(vault_root, std::path::Path::new(&new_path))?;
            let src = assert_in_vault(vault_root, std::path::Path::new(&old_path))?;
            if src.exists() {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        AppError::Export(format!("create rename parent dir failed: {e}"))
                    })?;
                }
                // A plain rename within the same vault is atomic on the same filesystem.
                std::fs::rename(&src, &dest)
                    .map_err(|e| AppError::Export(format!("rename folder dir failed: {e}")))?;
            } else {
                // Source absent (a locked folder's dir, or never materialized): ensure the
                // destination exists so future plaintext `.md`s land in the renamed dir.
                std::fs::create_dir_all(&dest).map_err(|e| {
                    AppError::Export(format!("create renamed folder dir failed: {e}"))
                })?;
            }
        }
    }

    // Rewrite the DB: this folder's name+path, then re-prefix every DESCENDANT folder's path. Order
    // doesn't risk content loss — no markdown/blob column is touched; only path strings move.
    state.db.rename_folder(&folder_id, &clean, &new_path)?;
    if new_path != old_path {
        reprefix_descendant_folder_paths(state, &folder_id, &new_path)?;
        // Re-derive every governed note's `exported_path` to point under its (possibly renamed)
        // folder's NEW on-disk dir. We rebuild from the file basename + the folder's new dir rather
        // than swapping path prefixes — robust to `/var` vs `/private/var` canonicalization drift in
        // the stored absolute path. The `fs::rename` already moved the bytes; this only re-points the
        // DB. Sealed notes (NULL exported_path) are skipped. Walks this folder + the whole subtree.
        if vault_configured {
            reexport_notes_under_subtree(state, &folder_id)?;
        }
    }

    Ok(Folder {
        name: clean,
        path: new_path,
        ..folder
    })
}

/// Recursively re-prefix the vault-relative `path` of every DESCENDANT folder of `folder_id` to sit
/// under `new_prefix` after the folder itself was renamed. Walks the tree one level at a time via
/// [`Db::child_folders`]; each child's recomposed path is `new_prefix` + the child's own name (so the
/// rewrite is structural, not a brittle string-replace). Does NOT touch the child's `name`, lock
/// state, or any note content — only the `path` column (the descendants' notes are re-pointed by the
/// single absolute-dir swap in the caller, since `fs::rename` moved the whole subtree at once).
fn reprefix_descendant_folder_paths(
    state: &AppState,
    folder_id: &str,
    new_prefix: &str,
) -> Result<(), AppError> {
    for child in state.db.child_folders(folder_id)? {
        let child_old = child.path.clone();
        let child_new = if new_prefix.is_empty() {
            child.name.clone()
        } else {
            format!("{new_prefix}/{}", child.name)
        };
        if child_new != child_old {
            state.db.rename_folder(&child.id, &child.name, &child_new)?;
        }
        // Recurse into this child's own subtree.
        reprefix_descendant_folder_paths(state, &child.id, &child_new)?;
    }
    Ok(())
}

/// After a folder rename moved the on-disk subtree, re-point the `exported_path` of every governed
/// note in `folder_id` AND its descendants to its folder's NEW vault dir. Each note's new path is
/// `<vault>/<folder.path>/<basename of the old exported_path>` (the `fs::rename` preserved the
/// filename). Rebuilding from the basename (not a string-prefix swap on the stored absolute path) is
/// robust to canonicalization drift (`/var` vs `/private/var`) and to where the original export wrote
/// the path. Sealed notes carry `exported_path = NULL` and are skipped. Requires a configured vault.
fn reexport_notes_under_subtree(state: &AppState, folder_id: &str) -> Result<(), AppError> {
    let Some(vault) = vault_path(state) else {
        return Ok(());
    };
    let vault_root = std::path::Path::new(&vault);

    let folder = match state.db.folder_by_id(folder_id)? {
        Some(f) => f,
        None => return Ok(()),
    };
    // The folder's NEW absolute dir (containment-checked).
    let new_dir = assert_in_vault(vault_root, std::path::Path::new(&folder.path))?;

    for n in state.db.notes_in_folder(folder_id)? {
        let Some(old) = n.exported_path else {
            continue; // sealed note (no .md) — nothing to re-point.
        };
        let Some(name) = std::path::Path::new(&old).file_name() else {
            continue;
        };
        let new_path = new_dir.join(name);
        state.db.set_note_exported_path(
            &n.meeting_id,
            &n.provider_id,
            &new_path.to_string_lossy(),
        )?;
    }

    // Recurse into descendant folders (their dirs moved with the same single `fs::rename`).
    for child in state.db.child_folders(folder_id)? {
        reexport_notes_under_subtree(state, &child.id)?;
    }
    Ok(())
}

/// Delete a folder, NEVER losing a note. SECURITY-CRITICAL — a folder may hold notes and may be
/// sealed (LOCKED). Rules, fail-closed:
///
///  - **Has child folders →** REJECT (`InvalidArg`). The FE deletes leaf-first; refusing here keeps
///    a subtree from being silently orphaned (a child's `parent_id` would dangle).
///  - **LOCKED + NOT session-unlocked →** REJECT (`AppError::Locked`). We have no CK to unseal the
///    folder's notes, so deleting the row would orphan encrypted-and-unrecoverable content (the
///    wrapped key lives on the row we'd delete). Tell the user to unlock first.
///  - **LOCKED + SESSION-UNLOCKED →** PERMANENTLY remove the lock first (`remove_lock_inner`:
///    KEK → unwrap CK → decrypt every note/transcript/timeline/audio back to plaintext, re-export the
///    `.md`, clear the blobs, flip the folder open). Only then does it become the OPEN case below, so
///    nothing is ever left encrypted-and-orphaned.
///  - **OPEN (now) →** move every note to the vault ROOT (`folder_id = NULL`), delete the folder row,
///    and remove the (now-empty) vault subdir. Notes survive at "All notes".
#[tauri::command]
pub fn delete_folder(state: State<'_, AppState>, folder_id: String) -> Result<(), AppError> {
    delete_folder_inner(state.inner(), folder_id)
}

/// Inner of [`delete_folder`] taking `&AppState` (so tests can drive it without a `tauri::State`).
/// See the command doc for the fail-closed rules.
pub(crate) fn delete_folder_inner(state: &AppState, folder_id: String) -> Result<(), AppError> {
    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;

    // Refuse a non-empty SUBTREE — never orphan child folders by dangling their parent_id.
    if !state.db.child_folders(&folder_id)?.is_empty() {
        return Err(AppError::InvalidArg(
            "this folder has subfolders — delete or move them first".into(),
        ));
    }

    // If sealed, it MUST be session-unlocked so we can unseal its notes back to plaintext before the
    // folder row (which carries the wrapped key) is destroyed. Otherwise refuse — never orphan
    // sealed content.
    if folder.locked {
        let session_unlocked = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
            .contains(&folder_id);
        if !session_unlocked {
            return Err(AppError::Locked(
                "unlock this folder first to delete it (its notes are sealed)".into(),
            ));
        }
        // Permanently unseal back to plaintext + re-export the `.md`s, then the folder is OPEN.
        // remove_lock_inner takes the lifecycle guard itself (so we do NOT hold it across this call —
        // the std Mutex is non-reentrant and would self-deadlock).
        remove_lock_inner(state, folder_id.clone())?;
    }

    // OPEN folder now (or was open all along): move its notes to the vault ROOT, then drop the row.
    // Serialize the reassign + row delete + FS cleanup under the lifecycle guard so it can't race a
    // concurrent lock/move on the same folder.
    let _lifecycle = lifecycle_guard(state);

    // Move every note in this folder to the vault root (folder_id = NULL). The notes' plaintext `.md`
    // files already live in this folder's vault subdir; we re-point each meeting's exported_path to
    // the root by moving the file (best-effort, copy-then-remove — never loses bytes).
    let notes = state.db.notes_in_folder(&folder_id)?;
    let mut moved_meetings = std::collections::HashSet::new();
    for n in &notes {
        if !moved_meetings.insert(n.meeting_id.clone()) {
            continue;
        }
        // Reassign every provider row of this meeting to the root.
        state.db.set_meeting_folder(&n.meeting_id, None)?;
        // Best-effort move of the plaintext `.md` to the vault root (only when one exists).
        if let Some(src_path) = n.exported_path.clone() {
            if let Some(vault) = vault_path(state) {
                move_note_file_to_root(state, &n.meeting_id, &src_path, &vault)?;
            }
        }
    }

    // NOTES-1 (2026-07-11 audit, CRITICAL data loss): AUTHORED notes (`documents(kind='note')`)
    // must be REPARENTED to the default note-folder, NOT destroyed — the FE promises "delete folder"
    // MOVES its notes to the default folder. The pre-fix path left them for `Db::delete_folder`'s
    // blanket `DELETE FROM documents`, permanently deleting authored notes. `Db::delete_folder` now
    // REFUSES if any authored note still references the folder, so this reparent MUST run first.
    reparent_authored_notes_to_default(state, &folder_id)?;

    // Delete the folder row, then remove the (now note-free) vault subdir. Row first: a leftover
    // empty dir is harmless/reconcilable; a dangling row is not.
    state.db.delete_folder(&folder_id)?;
    if let Some(vault) = vault_path(state) {
        let vault_root = std::path::Path::new(&vault);
        if let Ok(dir) = assert_in_vault(vault_root, std::path::Path::new(&folder.path)) {
            // remove_dir (not _all): only an EMPTY dir is removed, so a stray user file is never
            // clobbered. The notes' `.md`s were moved out above, so the dir should be empty.
            let _ = std::fs::remove_dir(&dir);
        }
    }
    Ok(())
}

/// Move a meeting's plaintext `.md` to the vault ROOT (copy-then-remove, never losing bytes) and
/// re-point its `exported_path`. A `&AppState`-only twin of [`move_note_file`] (whose `&State`
/// signature can't be reached from the `_inner` delete path). Used when deleting a folder demotes its
/// notes to "All notes".
fn move_note_file_to_root(
    state: &AppState,
    meeting_id: &str,
    src_path: &str,
    vault: &str,
) -> Result<(), AppError> {
    let src = std::path::Path::new(src_path);
    let bytes = match std::fs::read_to_string(src) {
        Ok(b) => b,
        // Source already gone → nothing to move; the DB association is already NULL.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(AppError::Export(format!("read note for move failed: {e}"))),
    };
    let file_name = src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| AppError::Export("note path has no filename".into()))?;
    let vault_root = std::path::Path::new(vault);
    let dest = assert_in_vault(vault_root, std::path::Path::new(file_name))?;
    let src_canon = src.canonicalize().unwrap_or_else(|_| src.to_path_buf());
    if dest == src_canon || dest == src {
        return Ok(()); // already at the root.
    }
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| AppError::Export(format!("create move dir failed: {e}")))?;
    }
    // Write the destination atomically, THEN remove the source (never lose bytes).
    // Export-collision guard: bytes move verbatim → the `exported_hash` baseline is deliberately
    // left alone (see `move_note_file` — re-stamping from moved bytes would erase the
    // external-edit signal).
    crate::export::overwrite_note(&dest, &bytes)?;
    let _ = std::fs::remove_file(src);
    if let Some(existing) = state.db.get_latest_note_for_meeting(meeting_id)? {
        state.db.set_note_exported_path(
            meeting_id,
            &existing.provider_id,
            &dest.to_string_lossy(),
        )?;
    }
    Ok(())
}

/// NOTES-1 (2026-07-11 audit, CRITICAL data loss) — reparent every AUTHORED note
/// (`documents(kind='note')`) in a to-be-deleted folder to the DEFAULT note-folder ("Notes"), moving
/// its plaintext `.md` into the default folder's vault subdir (copy-then-remove — never lose bytes)
/// and rewriting `documents.exported_path`. The FE's "delete folder" promises its notes MOVE to the
/// default folder; the pre-fix path left them for `Db::delete_folder`'s blanket `DELETE`, destroying
/// them. Runs AFTER any sealed folder was unsealed back to plaintext (`remove_lock_inner`), so the
/// notes here are plaintext. If the folder BEING deleted IS the default note-folder itself, its notes
/// can't reparent to themselves — REFUSE rather than risk destroying them (the FE never offers to
/// delete the root "Notes" folder). Best-effort FS move: a note's bytes live in the DB (canonical),
/// so a failed `.md` move never loses content — but a missing target keeps the reparent (the row moves
/// regardless). No PII in logs (ids only).
fn reparent_authored_notes_to_default(
    state: &AppState,
    folder_id: &str,
) -> Result<(), AppError> {
    let note_ids = state.db.note_ids_in_folder(folder_id)?;
    if note_ids.is_empty() {
        return Ok(());
    }
    let default_id = state.db.ensure_default_note_folder()?;
    if default_id == folder_id {
        // Deleting the default note-folder while it still holds authored notes: reparenting to
        // itself is a no-op and the row-delete would then destroy them. Fail closed.
        return Err(AppError::InvalidArg(
            "cannot delete the default \"Notes\" folder while it holds notes — move them first".into(),
        ));
    }
    let default_folder = state
        .db
        .note_folder_by_id(&default_id)?
        .ok_or_else(|| AppError::Storage("default note-folder missing after ensure".into()))?;
    for id in &note_ids {
        // Reassign the row to the default note-folder (the gate/seal anchor). The default folder is
        // OPEN (root "Notes" is never locked), so a plain reassign is correct — no reseal needed.
        state.db.set_note_doc_folder(id, &default_id)?;
        // Move the plaintext `.md` into the default folder's vault subdir + re-point exported_path.
        if let Some(row) = state.db.get_note_row(id)? {
            move_authored_note_md_to_folder(state, &row, &default_folder)?;
        }
    }
    tracing::info!(
        target: "notes",
        folder_id = %folder_id,
        moved = note_ids.len(),
        "reparented authored notes to the default folder before folder delete"
    );
    Ok(())
}

/// Move ONE authored note's plaintext `.md` from its old export path into `target` note-folder's
/// vault subdir (copy-then-remove — never lose bytes) and re-point `documents.exported_path`. A
/// `&AppState`-only helper for the `_inner` delete path (which can't reach the `&State`-signature
/// export helpers). No-op when there is no vault, no old export, or the source file is already gone
/// (the DB row is the canonical copy; a re-export recreates the `.md`).
fn move_authored_note_md_to_folder(
    state: &AppState,
    row: &crate::storage::db::NoteRow,
    target: &NoteFolder,
) -> Result<(), AppError> {
    let Some(vault) = vault_path(state) else {
        // No vault → nothing on disk to move; still clear the stale export path so a later lock
        // never chases it.
        state.db.set_note_doc_exported_path(&row.id, None)?;
        return Ok(());
    };
    let vault_root = std::path::Path::new(&vault);
    // Read the source bytes (if any). A missing source → nothing to move; clear the path.
    let bytes = match row.exported_path.as_deref() {
        Some(src_path) => match std::fs::read_to_string(src_path) {
            Ok(b) => Some((src_path.to_string(), b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(AppError::Export(format!("read note for move failed: {e}"))),
        },
        None => None,
    };
    let Some((src_path, content)) = bytes else {
        // No on-disk file → just re-export from the (canonical) DB text into the new folder if we
        // have text; otherwise clear the stale path.
        if row.text.is_empty() {
            state.db.set_note_doc_exported_path(&row.id, None)?;
        } else if let Some(p) = write_note_to_vault(state, row)? {
            let _ = p; // write_note_to_vault already re-points exported_path.
        }
        return Ok(());
    };
    // Compose the destination inside the target folder's vault subdir, D5-contained.
    let file_name = std::path::Path::new(&src_path)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| AppError::Export("note path has no filename".into()))?;
    let rel = std::path::Path::new(&target.path).join(file_name);
    let dest = assert_in_vault(vault_root, &rel)?;
    let src_canon = std::path::Path::new(&src_path)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&src_path));
    if dest == src_canon {
        // Already at the destination (nothing to move) — just record the path.
        state
            .db
            .set_note_doc_exported_path(&row.id, Some(&dest.to_string_lossy()))?;
        return Ok(());
    }
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| AppError::Export(format!("create move dir failed: {e}")))?;
    }
    // Write the destination atomically, THEN remove the source (never lose bytes).
    // Export-collision guard: bytes move verbatim → the `exported_hash` baseline is deliberately
    // left alone (see `move_note_file` — re-stamping from moved bytes would erase the
    // external-edit signal).
    crate::export::overwrite_note(&dest, &content)?;
    let _ = std::fs::remove_file(&src_path);
    state
        .db
        .set_note_doc_exported_path(&row.id, Some(&dest.to_string_lossy()))?;
    Ok(())
}

// ── Phase 0.5 full per-folder lock: transcript + timeline + audio seal helpers ──
//
// The note markdown was already sealed (encrypt→content_blob, blank plaintext). These helpers
// extend the SAME lifecycle to a folder's TRANSCRIPT (segments.text), TIMELINE (timelines.data),
// and the AUDIO WAV (a file at meetings.audio_path, NOT in the SQLCipher DB → plaintext on disk).
// All key off the folder content key (CK) the caller has already unwrapped.

/// Suffix marking an audio file as AES-GCM-encrypted-at-rest (sealed folder). The presence of
/// this suffix on `meetings.audio_path` is the on-disk "audio is sealed" signal.
const ENC_SUFFIX: &str = ".enc";

// ── B7/B8 AAD context binding ──────────────────────────────────────────────────────────────────
//
// Every AES-GCM blob is bound to its STORAGE CONTEXT via additional authenticated data so a
// ciphertext cannot be swapped between folders/meetings/providers/record-types. The AAD is NOT
// stored — it is RECONSTRUCTED deterministically from the row's identity at decrypt time. Format is
// a fixed, pipe-joined, versioned byte string; `crypto::decrypt` transparently falls back to empty
// AAD for legacy (pre-AAD) blobs and reports `AadUsed::Legacy` so we re-bind on the next write.
//
// SCHEMA VERSION is part of the content-blob AAD so a future format change is itself
// context-bound; bump it only alongside a migration. Audio + wrapped-CK AADs are intentionally
// minimal (the task spec: audio = meeting|folder, wrapped-CK = folder).

/// AAD schema version for content blobs (notes / transcript segments / timeline). Part of the bound
/// context, so a v1→v2 change cannot be silently down-mixed.
const AAD_SCHEMA_VERSION: &str = "1";

/// AAD for a folder's wrapped content-key: bound to the `folder_id` only (the wrapped CK lives on
/// the folder row; nothing else identifies it).
pub(crate) fn aad_wrapped_ck(folder_id: &str) -> Vec<u8> {
    format!("murmur:wrapck:v{AAD_SCHEMA_VERSION}|folder={folder_id}").into_bytes()
}

/// KEK-RECOVERY: try every master-KEK candidate the keychain stores hold against a folder's
/// wrapped content key, skipping the already-tried primary. Returns the unwrapped CK bytes, the
/// WINNING KEK (adopted for the session by the caller) and the candidate index (for the forensic
/// log — count/index only, never key bytes). Read-only and pure over its inputs, so it is unit-
/// testable without a keychain. Rationale: on machines where the no-UI keychain probe lies,
/// several KEK generations coexist under the same account and only ONE of them sealed this folder
/// (2026-07-05 field incident).
/// The discard SAFETY DECISION, factored out PURE so the "an enumeration failure must ABORT — never
/// be read as proof of absence" invariant is unit-testable WITHOUT a keychain. Returns:
/// - `Err(e)` — the candidate enumeration itself FAILED (`enumeration` was `Err`): the discard cannot
///   prove the folder unrecoverable, so it must abort (never wipe on an unproven absence).
/// - `Ok(false)` — a candidate DOES unwrap the folder's wrapped CK ⇒ RECOVERABLE ⇒ refuse to discard.
/// - `Ok(true)` — the enumeration completed and NO candidate unwraps ⇒ provably unrecoverable ⇒ safe.
pub(crate) fn discard_proof_complete(
    enumeration: Result<Zeroizing<Vec<[u8; 32]>>, AppError>,
    wrapped: &[u8],
    folder_id: &str,
) -> Result<bool, AppError> {
    let candidates = enumeration?;
    Ok(try_unwrap_ck_with_candidates(&candidates, wrapped, folder_id, None).is_none())
}

pub(crate) fn try_unwrap_ck_with_candidates(
    candidates: &[[u8; 32]],
    wrapped: &[u8],
    folder_id: &str,
    already_tried: Option<&[u8; 32]>,
) -> Option<(Vec<u8>, Zeroizing<[u8; 32]>, usize)> {
    for (i, cand) in candidates.iter().enumerate() {
        if already_tried == Some(cand) {
            continue;
        }
        if let Ok(bytes) = crate::crypto::decrypt(cand, wrapped, &aad_wrapped_ck(folder_id)) {
            return Some((bytes, Zeroizing::new(*cand), i));
        }
    }
    None
}

/// AAD for a content blob (note / transcript segment / timeline). Bound to
/// `folder_id | meeting_id | provider_id | record_type | schema_version`. `provider_id` is the note
/// provider for note rows, or a fixed sentinel for transcript/timeline rows (which have no provider).
pub(crate) fn aad_content(folder_id: &str, meeting_id: &str, provider_id: &str, record_type: &str) -> Vec<u8> {
    format!(
        "murmur:content:v{AAD_SCHEMA_VERSION}|folder={folder_id}|meeting={meeting_id}|provider={provider_id}|type={record_type}"
    )
    .into_bytes()
}

/// AAD for an uploaded DOCUMENT's sealed text blob. Bound to `folder_id | document_id | type=document
/// | schema_version` — documents anchor on a FOLDER (not a meeting), so the AAD uses the document id
/// in the role the meeting/provider play for note blobs. A document blob therefore cannot be lifted
/// onto a different folder/document row and decrypted there (B7).
fn aad_document(folder_id: &str, document_id: &str) -> Vec<u8> {
    format!(
        "murmur:document:v{AAD_SCHEMA_VERSION}|folder={folder_id}|document={document_id}|type=document"
    )
    .into_bytes()
}

/// The ROLE-LESS audio AAD (the historical B8 form): bound to `meeting_id | folder_id`. Retained as
/// the lower rung of the decrypt ladder so masters/playback sealed BEFORE stream-role binding (which
/// carry exactly this NON-empty AAD) still decrypt — see [`aad_audio_role`] and
/// [`crate::crypto::decrypt_file_multi`].
fn aad_audio(meeting_id: &str, folder_id: &str) -> Vec<u8> {
    format!("murmur:audio:v{AAD_SCHEMA_VERSION}|meeting={meeting_id}|folder={folder_id}")
        .into_bytes()
}

/// Which audio stream a `.enc` belongs to. Bound into the audio AAD ([`aad_audio_role`]) so the
/// three per-meeting files — playback WAV (`audio_path`), mic master, sys master — which previously
/// shared the SAME `aad_audio(meeting,folder)` and were therefore cross-decryptable within a meeting,
/// can no longer be swapped for one another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamRole {
    Playback,
    Mic,
    Sys,
}

impl StreamRole {
    fn as_str(self) -> &'static str {
        match self {
            StreamRole::Playback => "playback",
            StreamRole::Mic => "mic",
            StreamRole::Sys => "sys",
        }
    }
}

/// ROLE-BOUND audio AAD: [`aad_audio`] PLUS the stream role, so a mic master can't be swapped for the
/// sys master (or the playback WAV) within the same meeting. New seals bind THIS form.
///
/// ⚠ BACKWARD-COMPAT: existing masters/playback `.enc` were sealed with the role-LESS [`aad_audio`]
/// (a NON-empty AAD). A role-bound decrypt alone would NOT match them, and the empty-AAD legacy
/// fallback would NOT match them either (they are non-empty) → DATA LOSS. So decrypt ALWAYS goes
/// through [`audio_decrypt_ladder`] (role form → role-less form → empty), never a bare role decrypt.
/// A file re-binds to this role form on its next seal.
fn aad_audio_role(meeting_id: &str, folder_id: &str, role: StreamRole) -> Vec<u8> {
    format!(
        "murmur:audio:v{AAD_SCHEMA_VERSION}|meeting={meeting_id}|folder={folder_id}|stream={}",
        role.as_str()
    )
    .into_bytes()
}

/// The two AAD rungs to TRY when decrypting one audio stream, newest-binding first:
/// `[role-bound, role-less]`. (The empty-AAD pre-AAD fallback is built into `crypto::decrypt`, so it
/// is covered by the first rung — see [`crate::crypto::decrypt_file_multi`].) Returned owned so the
/// caller can borrow both as `&[&[u8]]`.
fn audio_decrypt_ladder(meeting_id: &str, folder_id: &str, role: StreamRole) -> (Vec<u8>, Vec<u8>) {
    (
        aad_audio_role(meeting_id, folder_id, role),
        aad_audio(meeting_id, folder_id),
    )
}

/// Sentinel provider id for content blobs that have no note-provider (transcript segments, timeline)
/// — keeps the AAD shape uniform across record types.
const AAD_NO_PROVIDER: &str = "-";

// ── per-file audio-at-rest stages (audio_path + the two masters all share these) ──────────────
// A meeting carries up to THREE at-rest audio files — the playback WAV (`audio_path`) and the two
// faithful masters (`mic_master_path` / `sys_master_path`). All follow the SAME seal lifecycle, so
// these run once PER FILE. Verify-before-destroy lives inside `crypto::encrypt_file`; running
// per-file means a crash mid-loop leaves already-sealed `.enc` + not-yet-sealed plaintext — never
// lost audio. Each returns the new path to persist, or `None` when there's nothing to do.
//
// The SEAL stage takes a single `aad: &[u8]` — the ROLE-bound `aad_audio_role(meeting, folder, role)`
// it binds the new ciphertext to. The two DECRYPT stages take an `aads: &[&[u8]]` LADDER (role form,
// then the historical role-less form) so a master/playback sealed before stream-role binding still
// decrypts and then re-binds on next seal — see `audio_decrypt_ladder` / `crypto::decrypt_file_multi`.
// Every audio blob at rest thus stays AAD-bound to its (meeting|folder|stream) context (B7/B8 + the
// stream-role hardening) — the same guarantee the content blobs get. `reblank_audio` performs no
// crypto (it only drops the decrypted session copy and re-points at the durable `.enc`), so no AAD.

/// SEAL: encrypt `<file>` → `<file>.enc` (verify inside), remove the plaintext only after the
/// verified `.enc` exists. `None` when already sealed, missing on disk, or absent. Idempotent.
fn seal_audio_at_rest(
    ck: &[u8; 32],
    path: Option<String>,
    aad: &[u8],
) -> Result<Option<String>, AppError> {
    let Some(path) = path else { return Ok(None) };
    if path.ends_with(ENC_SUFFIX) || !std::path::Path::new(&path).exists() {
        return Ok(None);
    }
    let enc_path = format!("{path}{ENC_SUFFIX}");
    crate::crypto::encrypt_file(
        ck,
        std::path::Path::new(&path),
        std::path::Path::new(&enc_path),
        aad,
    )?;
    let _ = std::fs::remove_file(&path);
    Ok(Some(enc_path))
}

/// SESSION-unseal: decrypt `<file>.enc` → `<file>` for the session, KEEPING the `.enc`. Returns
/// the plaintext path to persist (`None` if not sealed). `aads` is the role→role-less decrypt ladder
/// (see [`audio_decrypt_ladder`]) so a pre-role master still decrypts.
fn session_unseal_audio(
    ck: &[u8; 32],
    enc_path: Option<String>,
    aads: &[&[u8]],
) -> Result<Option<String>, AppError> {
    let Some(enc_path) = enc_path else {
        return Ok(None);
    };
    if !enc_path.ends_with(ENC_SUFFIX) {
        return Ok(None);
    }
    let plain = enc_path.trim_end_matches(ENC_SUFFIX).to_string();
    crate::crypto::decrypt_file_multi(
        ck,
        std::path::Path::new(&enc_path),
        std::path::Path::new(&plain),
        aads,
    )?;
    Ok(Some(plain))
}

/// RE-BLANK (relock): drop the decrypted session copy + re-point at the durable `.enc`. Returns
/// the `.enc` path to persist (`None` if already sealed or the `.enc` is missing). No crypto → no AAD.
fn reblank_audio(path: Option<String>) -> Result<Option<String>, AppError> {
    let Some(path) = path else { return Ok(None) };
    if path.ends_with(ENC_SUFFIX) {
        return Ok(None);
    }
    let enc_path = format!("{path}{ENC_SUFFIX}");
    if !std::path::Path::new(&enc_path).exists() {
        return Ok(None);
    }
    let _ = std::fs::remove_file(&path);
    Ok(Some(enc_path))
}

/// PERMANENT-unseal (remove-lock): decrypt `<file>.enc` → `<file>`, then remove the `.enc`.
/// Returns the plaintext path to persist (`None` if not sealed). Never loses audio. `aads` is the
/// role→role-less decrypt ladder (see [`audio_decrypt_ladder`]) so a pre-role master still decrypts.
fn permanent_unseal_audio(
    ck: &[u8; 32],
    enc_path: Option<String>,
    aads: &[&[u8]],
) -> Result<Option<String>, AppError> {
    let Some(enc_path) = enc_path else {
        return Ok(None);
    };
    if !enc_path.ends_with(ENC_SUFFIX) {
        return Ok(None);
    }
    let plain = enc_path.trim_end_matches(ENC_SUFFIX).to_string();
    crate::crypto::decrypt_file_multi(
        ck,
        std::path::Path::new(&enc_path),
        std::path::Path::new(&plain),
        aads,
    )?;
    let _ = std::fs::remove_file(&enc_path);
    Ok(Some(plain))
}

/// SEAL every governed meeting's transcript + timeline under `ck`, then the audio WAV. Mirrors
/// `lock_folder`'s note seal: each blob is verified-decryptable BEFORE the plaintext is blanked /
/// the plaintext WAV is removed — content (transcript / audio) is never lost.
pub(crate) fn seal_folder_extras(state: &AppState, folder_id: &str, ck: &[u8; 32]) -> Result<(), AppError> {
    let meeting_ids = state.db.meeting_ids_in_folder(folder_id)?;
    for mid in &meeting_ids {
        seal_meeting_extras(state, folder_id, mid, ck)?;
    }
    // Document ingestion: SEAL every uploaded document's text (USER-AUTHORED PRIMARY content,
    // SEALED-AND-RESTORED like the note markdown / typed notes — never lost). Encrypt the plaintext
    // under the folder CK, VERIFY it decrypts back byte-identical (verify-before-destroy), THEN blank
    // the plaintext. Done per FOLDER (documents anchor on the folder, not a meeting). An empty text ⇒
    // nothing to seal (blob stays NULL); an already-sealed document (blank text) is skipped.
    for d in state.db.raw_documents_in_folder(folder_id)? {
        if d.text.is_empty() {
            continue;
        }
        let aad = aad_document(folder_id, &d.id);
        let blob = crate::crypto::encrypt(ck, d.text.as_bytes(), &aad)?;
        if crate::crypto::decrypt(ck, &blob, &aad)? != d.text.as_bytes() {
            return Err(AppError::Storage(
                "document seal verification failed (blob mismatch)".into(),
            ));
        }
        state.db.seal_document(&d.id, &blob)?;
    }
    Ok(())
}

/// Seal ONE meeting's transcript + timeline + audio WAV under the folder CK (the per-meeting body of
/// [`seal_folder_extras`]). Reused by [`move_note`] to seal a note moved INTO a session-unlocked
/// locked folder (BLK-2) without touching the folder's other meetings. Verify-before-destroy
/// throughout (no transcript / audio loss); idempotent on already-sealed rows.
fn seal_meeting_extras(
    state: &AppState,
    folder_id: &str,
    mid: &str,
    ck: &[u8; 32],
) -> Result<(), AppError> {
    // Transcript: encrypt each segment's plaintext text, verify, then seal (blank text).
    let segs = state.db.raw_segments(mid)?;
    let mut sealed_segs: Vec<(i64, Vec<u8>)> = Vec::new();
    for s in &segs {
        // Skip rows already sealed (text_blob present, text blank) — idempotent.
        if s.text_blob.is_some() && s.text.is_empty() {
            continue;
        }
        let aad = aad_content(folder_id, mid, AAD_NO_PROVIDER, "segment");
        let blob = crate::crypto::encrypt(ck, s.text.as_bytes(), &aad)?;
        if crate::crypto::decrypt(ck, &blob, &aad)? != s.text.as_bytes() {
            return Err(AppError::Storage(
                "transcript seal verification failed (segment blob mismatch)".into(),
            ));
        }
        sealed_segs.push((s.idx, blob));
    }
    for (idx, blob) in &sealed_segs {
        state.db.seal_segment(mid, *idx, blob)?;
    }

    // Timeline: encrypt the cached JSON (if any), verify, then seal (blank data).
    if let Some(tl) = state.db.raw_timeline(mid)? {
        if !(tl.data_blob.is_some() && tl.data.is_empty()) {
            let aad = aad_content(folder_id, mid, AAD_NO_PROVIDER, "timeline");
            let blob = crate::crypto::encrypt(ck, tl.data.as_bytes(), &aad)?;
            if crate::crypto::decrypt(ck, &blob, &aad)? != tl.data.as_bytes() {
                return Err(AppError::Storage(
                    "timeline seal verification failed (blob mismatch)".into(),
                ));
            }
            state.db.seal_timeline(mid, &blob)?;
        }
    }

    // Audio at rest: the playback WAV + both masters, each encrypted → <file>.enc with
    // verify-before-destroy (inside encrypt_file), then the plaintext removed and the column
    // re-pointed at the .enc. Each blob is AAD-bound to (meeting|folder|STREAM-ROLE) so a sealed
    // audio file can't be swapped between contexts OR between the three streams of one meeting
    // (B7/B8 + stream-role hardening). The timeline was already sealed just above — do NOT re-seal it.
    if let Some(enc) = seal_audio_at_rest(
        ck,
        state.db.get_meeting(mid)?.and_then(|m| m.audio_path),
        &aad_audio_role(mid, folder_id, StreamRole::Playback),
    )? {
        state.db.set_meeting_audio_path(mid, Some(&enc))?;
    }
    let (mic, sys) = state.db.get_meeting_master_paths(mid)?;
    if let Some(enc) =
        seal_audio_at_rest(ck, mic, &aad_audio_role(mid, folder_id, StreamRole::Mic))?
    {
        state.db.set_meeting_mic_master_path(mid, Some(&enc))?;
    }
    if let Some(enc) =
        seal_audio_at_rest(ck, sys, &aad_audio_role(mid, folder_id, StreamRole::Sys))?
    {
        state.db.set_meeting_sys_master_path(mid, Some(&enc))?;
    }

    // brain2 realtime notes: SEAL the user's typed in-meeting notes (USER-AUTHORED PRIMARY content)
    // exactly like the timeline — encrypt the plaintext under the folder CK, VERIFY it decrypts back
    // byte-identical (verify-before-destroy), then blank the plaintext. NEVER blanked without the
    // sealed copy, and reversed by the matching unseal (session-unlock / remove-lock). An empty
    // buffer ⇒ nothing to seal (blob stays NULL); an already-sealed buffer (blank text) is skipped.
    if let Some(rn) = state.db.raw_manual_notes(mid)? {
        if !rn.text.is_empty() {
            let aad = aad_content(folder_id, mid, AAD_NO_PROVIDER, "manual_notes");
            let blob = crate::crypto::encrypt(ck, rn.text.as_bytes(), &aad)?;
            if crate::crypto::decrypt(ck, &blob, &aad)? != rn.text.as_bytes() {
                return Err(AppError::Storage(
                    "manual-notes seal verification failed (blob mismatch)".into(),
                ));
            }
            state.db.seal_manual_notes(mid, &blob)?;
        }
    }
    Ok(())
}

/// SESSION-unlock: decrypt every governed meeting's transcript + timeline back into the plaintext
/// columns and materialize a playable WAV (decrypt <file>.enc → <file>) re-pointing audio_path at
/// it. Keeps the `.enc` + the `*_blob` columns (folder is still locked on disk).
/// Re-index a just-unsealed folder's MEETINGS into `note_chunks` + `vec_chunks` so semantic search /
/// related-meetings recover after an unlock. Keyword search over meeting notes already recovers on
/// its own — the `fts_notes` UPDATE trigger fires when `restore_note_markdown` writes the plaintext
/// back; `note_chunks` have NO independent FTS index (unlike `doc_chunks`/`fts_doc_chunks`), they are
/// purely the plaintext substrate paired 1:1 with the (semantic) `vec_chunks`.
///
/// SYMMETRY / PRECEDENT: documents are ALREADY re-embedded on unseal (`index_document_chunks`, just
/// above the call sites) and re-purged on relock (`purge_doc_chunks_tx` inside
/// `blank_sealed_notes_in_folders` / `reblank_folder_extras`). MEETINGS were purged on lock
/// (`purge_chunks_for_meetings` / `blank_sealed_notes_in_folders` → `purge_chunks_tx`) but were
/// NEVER re-indexed on unlock — this closes that gap so meetings behave identically to documents. On
/// relock the SAME `blank_sealed_notes_in_folders` → `purge_chunks_tx` re-purges these rebuilt rows,
/// so a re-sealed folder still leaves NO meeting vector at rest.
///
/// MODEL POLICY — mirrors the MEETING half of `reindex_embeddings_inner` and the pipeline auto-index
/// (`should_auto_index`) EXACTLY: `Db::index_meeting_chunks` writes chunks AND vectors together (there
/// is no chunk-only mode, unlike `index_document_chunks(None)`), so it runs ONLY when the REAL e5
/// model is present — i.e. `embedder == Some`. Model ABSENT (`None`) ⇒ write NOTHING: never a stub
/// vector (the absolute no-stub-vector invariant), and a model-less install has nothing semantic to
/// restore anyway (and chunkless-of-vector `note_chunks` rows would have no FTS role). Callers pass
/// `embed_model_present().then(active_embedder)` — identical to the document re-index idiom.
///
/// ORDERING: the caller MUST have already restored each meeting's note plaintext markdown (via
/// `restore_note_markdown`) before this runs — `index_meeting_chunks` chunks the plaintext note. Both
/// production callers do (`unlock_folder` / `remove_lock_inner` restore markdown before the extras).
///
/// BEST-EFFORT: a per-meeting indexing failure WARNs (IDs/stage only, no PII) and continues — the
/// unlock/restore has already succeeded and MUST NOT be failed by a re-index hiccup.
fn reindex_meetings_after_unseal(
    state: &AppState,
    meeting_ids: &[String],
    embedder: Option<&dyn crate::embed::Embedder>,
) {
    let Some(embedder) = embedder else {
        // Model absent: meetings are not chunked (no chunk-only mode → would be stub vectors).
        // Exactly the pipeline / manual-reindex behavior; note_chunks stay purged until a real reindex.
        return;
    };
    for mid in meeting_ids {
        // The caller (`unseal_folder_extras`) has ALREADY restored each meeting's segment plaintext
        // (`restore_segment_text`) and note markdown BEFORE this runs, so the transcript chunks re-derive
        // from restored plaintext. A segments read failure is logged + skipped (never fails the unlock).
        let segments = match state.db.get_segments(mid) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(target: "rag", error = %e, "meeting re-index on unlock: reading segments failed (skipped)");
                continue;
            }
        };
        if let Err(e) = state.db.index_meeting_chunks(mid, &segments, embedder) {
            tracing::warn!(target: "rag", error = %e, "meeting re-index on unlock failed (note plaintext already restored)");
        } else {
            // Brain v2 L1.1 — rebuild the TOPIC chunks too (the seal purged them via the shared
            // choke point). The folder was just session-unlocked, so pass the LIVE unlock set —
            // the visibility gate inside must see this meeting as visible.
            let unlocked = state
                .unlocked_folders
                .lock()
                .map(|g| g.clone())
                .unwrap_or_default();
            if let Err(e) = state
                .db
                .index_meeting_topic_chunks(mid, &segments, embedder, &unlocked)
            {
                tracing::warn!(target: "rag", error = %e, "topic re-index on unlock failed (content unaffected)");
            }
        }
    }
}

/// SESSION-unseal the transcript + timeline + typed notes + audio of ONE meeting back into its
/// plaintext columns under the folder CK, KEEPING every `*_blob` / `.enc` at rest (the folder is
/// still `locked=1` on disk). Extracted verbatim from `unseal_folder_extras`'s per-meeting body so
/// the SAME verified restore serves both a full folder unlock AND a single note MOVED into a
/// session-unlocked locked folder — the moved note must read identically to its folder-mates, never
/// come back blank under the open padlock. Does NOT restore the note MARKDOWN (that is per-provider,
/// keyed on `content_blob`, and the callers restore it before/after this) nor re-index (also caller's
/// job — after markdown is back).
fn unseal_meeting_extras(
    state: &AppState,
    folder_id: &str,
    mid: &str,
    ck: &[u8; 32],
) -> Result<(), AppError> {
    for s in state.db.raw_segments(mid)? {
        let Some(blob) = &s.text_blob else { continue };
        let aad = aad_content(folder_id, mid, AAD_NO_PROVIDER, "segment");
        let pt = crate::crypto::decrypt(ck, blob, &aad)?;
        let text = String::from_utf8(pt)
            .map_err(|_| AppError::Storage("decrypted segment is not valid UTF-8".into()))?;
        state.db.restore_segment_text(mid, s.idx, &text)?;
    }
    if let Some(tl) = state.db.raw_timeline(mid)? {
        if let Some(blob) = &tl.data_blob {
            let aad = aad_content(folder_id, mid, AAD_NO_PROVIDER, "timeline");
            let pt = crate::crypto::decrypt(ck, blob, &aad)?;
            let data = String::from_utf8(pt)
                .map_err(|_| AppError::Storage("decrypted timeline is not valid UTF-8".into()))?;
            state.db.restore_timeline_data(mid, &data)?;
        }
    }
    // Typed notes: decrypt the sealed blob back into the plaintext column for the session (the
    // blob is kept — the folder is still locked on disk). Mirrors the timeline unseal.
    if let Some(rn) = state.db.raw_manual_notes(mid)? {
        if let Some(blob) = &rn.blob {
            let aad = aad_content(folder_id, mid, AAD_NO_PROVIDER, "manual_notes");
            let pt = crate::crypto::decrypt(ck, blob, &aad)?;
            let text = String::from_utf8(pt).map_err(|_| {
                AppError::Storage("decrypted manual notes is not valid UTF-8".into())
            })?;
            state.db.set_manual_notes(mid, &text)?;
        }
    }
    // Audio at rest: materialize a playable WAV for the session (playback + both masters), each
    // decrypted through the role→role-less AAD ladder (a pre-role master still decrypts); the
    // .enc is kept (folder still locked on disk).
    let (pb_role, pb_less) = audio_decrypt_ladder(mid, folder_id, StreamRole::Playback);
    if let Some(plain) = session_unseal_audio(
        ck,
        state.db.get_meeting(mid)?.and_then(|m| m.audio_path),
        &[&pb_role, &pb_less],
    )? {
        state.db.set_meeting_audio_path(mid, Some(&plain))?;
    }
    let (mic, sys) = state.db.get_meeting_master_paths(mid)?;
    let (mic_role, mic_less) = audio_decrypt_ladder(mid, folder_id, StreamRole::Mic);
    if let Some(plain) = session_unseal_audio(ck, mic, &[&mic_role, &mic_less])? {
        state.db.set_meeting_mic_master_path(mid, Some(&plain))?;
    }
    let (sys_role, sys_less) = audio_decrypt_ladder(mid, folder_id, StreamRole::Sys);
    if let Some(plain) = session_unseal_audio(ck, sys, &[&sys_role, &sys_less])? {
        state.db.set_meeting_sys_master_path(mid, Some(&plain))?;
    }
    Ok(())
}

/// `meeting_embedder` is the model-gated embedder for the MEETING re-index, resolved by the CALLER
/// (`embed_model_present().then(active_embedder)`) and passed in — `Some(real e5)` re-indexes the
/// folder's meetings, `None` (model absent) writes nothing (never a stub vector). It is injected
/// (rather than resolved internally like the document re-index below) so the model-PRESENT re-index
/// is deterministically testable without a real model on disk — meetings have no model-absent
/// chunk-only path, unlike documents.
pub(crate) fn unseal_folder_extras(
    state: &AppState,
    folder_id: &str,
    ck: &[u8; 32],
    meeting_embedder: Option<&dyn crate::embed::Embedder>,
) -> Result<(), AppError> {
    let meeting_ids = state.db.meeting_ids_in_folder(folder_id)?;
    for mid in &meeting_ids {
        unseal_meeting_extras(state, folder_id, mid, ck)?;
    }
    // Document ingestion: decrypt each sealed document's text back into the plaintext column for the
    // session (the blob is kept — folder still locked on disk), then RE-EMBED so semantic search /
    // Ask works again in-session. Mirrors the timeline/manual-notes unseal + the meeting re-index.
    let mut restored_doc_ids: Vec<String> = Vec::new();
    for d in state.db.raw_documents_in_folder(folder_id)? {
        if let Some(blob) = &d.blob {
            let aad = aad_document(folder_id, &d.id);
            let pt = crate::crypto::decrypt(ck, blob, &aad)?;
            let text = String::from_utf8(pt)
                .map_err(|_| AppError::Storage("decrypted document is not valid UTF-8".into()))?;
            state.db.set_document_text(&d.id, &text)?;
            restored_doc_ids.push(d.id.clone());
        }
    }
    // Re-index the restored documents: chunks + the FTS index come back UNCONDITIONALLY (keyword
    // retrieval must survive a lock/unlock cycle on a model-less install); vectors ONLY when the
    // REAL e5 model is present (never stub vectors; mirrors `import_document`). KIND-ROUTED (Brain
    // v3 audit gap #3): an authored note re-chunks through the front-matter-stripping path.
    // Best-effort: a failure logs (no PII) and does NOT fail the unlock — the text is restored.
    if !restored_doc_ids.is_empty() {
        let embedder = crate::embed::embed_model_present().then(crate::embed::active_embedder);
        for did in &restored_doc_ids {
            if let Err(e) = index_document_row_kind_routed(&state.db, did, embedder.as_deref()) {
                tracing::warn!(target: "rag", error = %e, "document re-index on unlock failed (text restored)");
            }
        }
    }
    // MEETINGS: re-index the folder's meetings into note_chunks + vec_chunks so semantic /
    // related-meetings recover in-session (their note markdown was restored by `unlock_folder`
    // BEFORE this call). The caller supplies the model-gated `meeting_embedder` — never a stub
    // vector; mirrors the document re-embed above and the meeting half of `reindex_embeddings_inner`.
    reindex_meetings_after_unseal(state, &meeting_ids, meeting_embedder);

    // NOTES: re-export each authored note's vault `.md` (deleted on lock). Best-effort — the note's
    // plaintext text was restored just above (document unseal leg), so `export_note_to_vault` writes
    // the fresh `.md` + re-records `exported_path`. A failure logs (no PII) and never fails the
    // unlock.
    reexport_notes_in_folder(state, folder_id);
    Ok(())
}

/// Re-export every authored NOTE's vault `.md` in a folder whose plaintext was JUST restored by the
/// unseal path (session-unlock or permanent remove-lock), re-recording each `exported_path`. Called
/// from INSIDE the unseal path — the folder is still `locked=1` and not yet in the session unlock set
/// at this point (see `unlock_folder`'s ordering), so it uses the UNGATED [`write_note_to_vault`]
/// (authorized by the CK the caller decrypted with), exactly as `unseal_folder_extras` writes the
/// restored plaintext into the DB without re-gating. Best-effort per note (a failure logs IDs/stage
/// only and continues). A blanked (still-sealed) row is skipped inside `write_note_to_vault`.
fn reexport_notes_in_folder(state: &AppState, folder_id: &str) {
    let note_ids = match state.db.note_ids_in_folder(folder_id) {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(target: "notes", error = %e, "note re-export: list failed");
            return;
        }
    };
    for nid in &note_ids {
        match state.db.get_note_row(nid) {
            Ok(Some(row)) => {
                if let Err(e) = write_note_to_vault(state, &row) {
                    tracing::warn!(target: "notes", note_id = %nid, error = %e, "note re-export on unlock failed");
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(target: "notes", note_id = %nid, error = %e, "note re-export: read failed");
            }
        }
    }
}

/// Brain-v3 audit Fix 4 — the FILESYSTEM half of the sealed-neighbour marker strip: given the
/// `(is_meeting, source_id)` sources whose managed block the seal tx just scrubbed in the DB,
/// re-export each VISIBLE source's vault `.md` so the on-disk file no longer names the sealed
/// neighbour (the DB-in-tx / filesystem-at-command layering that sealed-note `.md` deletion uses).
/// A note-doc source re-exports via the gated [`export_note_to_vault`] (reads the now-scrubbed
/// `documents.text`); a meeting-note source overwrites its recorded `.md` via the guarded overwrite
/// (reads the now-scrubbed `notes.markdown`). Best-effort per source — a re-export failure logs
/// (ids/stage only, no PII) and never fails the seal (the DB plaintext is already scrubbed, the
/// primary leak closed). Called AFTER the seal/purge legs so it writes the final scrubbed body.
pub(crate) fn reexport_stripped_marker_sources(state: &AppState, sources: &[(bool, String)]) {
    for (is_meeting, source_id) in sources {
        if *is_meeting {
            // Meeting-note source: overwrite its recorded `.md` from the scrubbed newest-provider row.
            match state.db.get_latest_note_for_meeting(source_id) {
                Ok(Some(note)) => {
                    if let Some(path) = note.exported_path.as_deref() {
                        if let Err(e) = overwrite_exported_note_guarded(
                            state,
                            source_id,
                            &note.provider_id,
                            path,
                            &note.markdown,
                        ) {
                            tracing::warn!(target: "links", meeting_id = %source_id, error = %e, "marker-strip .md re-export (meeting) failed");
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(target: "links", meeting_id = %source_id, error = %e, "marker-strip .md re-export: note read failed"),
            }
        } else if let Err(e) = export_note_to_vault(state, source_id) {
            // Note-doc source: the gate inside is satisfied (a stripped source is VISIBLE by
            // construction — the strip skips sealed-at-rest sources).
            tracing::warn!(target: "links", note_id = %source_id, error = %e, "marker-strip .md re-export (note) failed");
        }
    }
}

/// Brain-v3 audit Fix 4 — RELOCK helper: resolve the given folders' meeting + document ids, strip
/// their `[[Title]]` markers from VISIBLE source notes, and re-export those sources' `.md`. Best-effort
/// (a resolve/strip failure logs and never fails the relock). Called on relock_folder / relock_all
/// BEFORE the relock purge drops the naming edges. Distinct from the initial-lock inline call only in
/// that it spans a SET of folders (the relocked ones).
pub(crate) fn strip_and_reexport_markers_for_folders(
    state: &AppState,
    folder_ids: &std::collections::HashSet<String>,
) {
    let mut meeting_ids: Vec<String> = Vec::new();
    let mut document_ids: Vec<String> = Vec::new();
    for fid in folder_ids {
        match state.db.meeting_ids_in_folder(fid) {
            Ok(mut m) => meeting_ids.append(&mut m),
            Err(e) => tracing::warn!(target: "links", error = %e, "relock marker strip: meeting-id list failed"),
        }
        match state.db.document_ids_in_folder(fid) {
            Ok(mut d) => document_ids.append(&mut d),
            Err(e) => tracing::warn!(target: "links", error = %e, "relock marker strip: document-id list failed"),
        }
    }
    match state
        .db
        .strip_sealed_neighbour_markers(&meeting_ids, &document_ids)
    {
        Ok(changed) => reexport_stripped_marker_sources(state, &changed),
        Err(e) => tracing::warn!(target: "links", error = %e, "relock sealed-neighbour marker strip failed"),
    }
}

/// RE-BLANK (relock): re-blank the plaintext transcript + timeline of every governed meeting and
/// remove the decrypted session WAV, re-pointing audio_path back at the `.enc`. The `*_blob`
/// columns + the `.enc` stay (the folder is still `locked=1`). Idempotent.
pub(crate) fn reblank_folder_extras(state: &AppState, folder_id: &str) -> Result<(), AppError> {
    // SEAL-NET KEY (2026-07-16 lock review of #356): resolve the folder CK from the SESSION-CACHED
    // KEK only — never a keychain/biometric prompt (this runs on relock / app-close / screen-share).
    // `relock_all_inner` zeroizes the KEK BEFORE its sweep by design (screen-share posture), so the
    // net is live on the `relock_folder` path and absent on relock-all — there, non-rederivable
    // rows are left in place and warned about instead (a bounded residue beats destroying content).
    let seal_ck: Option<Zeroizing<[u8; 32]>> = (|| {
        let kek = state.master_kek.lock().ok()?.clone()?;
        let wrapped = state.db.folder_wrapped_key(folder_id).ok().flatten()?;
        let bytes = Zeroizing::new(
            crate::crypto::decrypt(&kek, &wrapped, &aad_wrapped_ck(folder_id)).ok()?,
        );
        let arr: [u8; 32] = bytes.as_slice().try_into().ok()?;
        Some(Zeroizing::new(arr))
    })();
    for mid in state.db.meeting_ids_in_folder(folder_id)? {
        for s in state.db.raw_segments(&mid)? {
            if s.text_blob.is_some() && !s.text.is_empty() {
                state.db.restore_segment_text(&mid, s.idx, "")?;
            }
        }
        if let Some(tl) = state.db.raw_timeline(&mid)? {
            if tl.data_blob.is_some() && !tl.data.is_empty() {
                state.db.restore_timeline_data(&mid, "")?;
            }
        }
        // Typed notes: re-blank the plaintext ONLY when the sealed blob exists (never destroy the
        // only copy). Mirrors the timeline reblank.
        if let Some(rn) = state.db.raw_manual_notes(&mid)? {
            if rn.blob.is_some() && !rn.text.is_empty() {
                state.db.set_manual_notes(&mid, "")?;
            }
        }
        if let Some(enc) = reblank_audio(state.db.get_meeting(&mid)?.and_then(|m| m.audio_path))? {
            state.db.set_meeting_audio_path(&mid, Some(&enc))?;
        }
        let (mic, sys) = state.db.get_meeting_master_paths(&mid)?;
        if let Some(enc) = reblank_audio(mic)? {
            state.db.set_meeting_mic_master_path(&mid, Some(&enc))?;
        }
        if let Some(enc) = reblank_audio(sys)? {
            state.db.set_meeting_sys_master_path(&mid, Some(&enc))?;
        }
        // FAIL-CLOSED sweep (2026-07-16 reviews: #352 adversarial MEDIUM, then the #356 lock-review
        // FAIL that scoped it): segment rows carrying PLAINTEXT with NO sealed blob — a
        // pipeline/move killed before its seal step — are invisible to the blob-guarded re-blanks
        // above. Decided per meeting, on PROOF, never on the comment's say-so:
        //   1. provably RE-DERIVABLE (status Error/Recording — exactly the states the retry gate
        //      accepts — AND this meeting's audio survives on disk, plaintext or `.enc`): DELETE.
        //      Unlock + retry re-transcribes; nothing unrecoverable is destroyed.
        //   2. otherwise, with the folder CK resolvable from the session KEK: SEAL them in place
        //      (verify-before-blank via `seal_meeting_extras`, which also covers the same crash's
        //      unsealed timeline/manual-notes/audio) — e.g. a kill mid-`move_note` leaves a
        //      COMPLETED meeting's transcript unsealed; deleting it would destroy the only copy.
        //   3. neither: LEAVE + WARN. A bounded plaintext residue behind the SQLCipher layer is
        //      strictly better than loss — content is NEVER deleted without a provable copy.
        let has_unsealed = state
            .db
            .raw_segments(&mid)?
            .iter()
            .any(|s| s.text_blob.is_none());
        if has_unsealed {
            let rederivable = state.db.get_meeting(&mid)?.is_some_and(|m| {
                matches!(
                    m.status,
                    MeetingStatus::Recording | MeetingStatus::Error
                ) && m.audio_path.as_deref().is_some_and(|p| {
                    let plain = p.strip_suffix(ENC_SUFFIX).unwrap_or(p);
                    std::path::Path::new(plain).exists()
                        || std::path::Path::new(&format!("{plain}{ENC_SUFFIX}")).exists()
                })
            });
            if rederivable {
                let purged = state.db.delete_unsealed_segments(&mid)?;
                if purged > 0 {
                    tracing::warn!(target: "lock", meeting_id = %mid, purged, "relock purged unsealed plaintext segments left by an interrupted pipeline (audio survives on disk; unlock + retry to re-transcribe)");
                }
            } else if let Some(ck) = seal_ck.as_ref() {
                // Best-effort by design: a seal failure must not abort the relock (that would
                // leave MORE plaintext) — the rows stay, warned about, recoverable via unlock.
                match seal_meeting_extras(state, folder_id, &mid, ck) {
                    Ok(()) => {
                        tracing::warn!(target: "lock", meeting_id = %mid, "relock sealed crash-window plaintext (interrupted move/seal) in place under the folder CK");
                    }
                    Err(e) => {
                        tracing::warn!(target: "lock", meeting_id = %mid, error = %e, "relock could not seal crash-window plaintext segments — left in place (never deleted without a provable copy)");
                    }
                }
            } else {
                tracing::warn!(target: "lock", meeting_id = %mid, "relock found unsealed plaintext segments that are not provably re-derivable and no session KEK is cached — left in place (unlock the folder to seal or retry)");
            }
        }
    }
    // Document ingestion: re-blank the plaintext text of every sealed document ONLY WHERE its
    // `text_blob` exists (never destroy the only copy), and PURGE the doc chunks the session unlock
    // re-embedded (a relocked folder must leave no doc vector at rest). Mirrors the manual-notes
    // reblank + the chunk purge.
    let mut reblanked_doc_ids: Vec<String> = Vec::new();
    for d in state.db.raw_documents_in_folder(folder_id)? {
        if d.blob.is_some() && !d.text.is_empty() {
            state.db.set_document_text(&d.id, "")?;
        }
        reblanked_doc_ids.push(d.id.clone());
    }
    state
        .db
        .purge_doc_chunks_for_documents(&reblanked_doc_ids)?;

    // NOTES: a session-unlock RE-EXPORTED each authored note's vault `.md` (and re-set
    // `exported_path`). On relock that plaintext `.md` must be deleted again + the path NULLed —
    // otherwise a re-sealed folder leaves a plaintext note on disk (a leak). PER ROW (residual W5):
    // the path is cleared ONLY after its `.md` was actually deleted (or is already absent) — a
    // FAILED delete keeps the path recorded so the next relock/startup pass retries (the pre-fix
    // bulk clear forgot the leaked `.md` forever). Count-only log — never paths (note titles).
    // Phase-0 follow-up: warn (counts only) when external-edit siblings sit next to these
    // exports — the relock deletes only the canonical `.md`s, never a preserved sibling.
    let relock_doc_rows = state.db.note_exported_path_rows_in_folder(folder_id)?;
    warn_external_edit_siblings("relock", relock_doc_rows.iter().map(|(_, p)| p));
    for (doc_id, p) in relock_doc_rows {
        let removed = match std::fs::remove_file(&p) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => {
                tracing::warn!(
                    target: "lock",
                    error = %e,
                    "relock: deleting a re-exported note .md failed — keeping exported_path for retry"
                );
                false
            }
        };
        if removed {
            state.db.set_note_doc_exported_path(&doc_id, None)?;
        }
    }
    Ok(())
}

/// PERMANENT remove-lock: decrypt every governed meeting's transcript + timeline back to plaintext,
/// clear the `*_blob` columns, and permanently restore the plaintext WAV (decrypt .enc → file,
/// remove the .enc). NEVER lose audio — the plaintext is written + the file decrypts before the
/// `.enc` is removed.
/// `meeting_embedder`: the caller-resolved, model-gated embedder for the MEETING re-index (see
/// [`unseal_folder_extras`] for why it is injected rather than resolved internally).
pub(crate) fn unseal_folder_extras_permanent(
    state: &AppState,
    folder_id: &str,
    ck: &[u8; 32],
    meeting_embedder: Option<&dyn crate::embed::Embedder>,
) -> Result<(), AppError> {
    for mid in state.db.meeting_ids_in_folder(folder_id)? {
        // Transcript: restore each segment from its blob (or keep the in-memory text if the folder
        // was session-unlocked and the blob is absent), then clear all blobs for the meeting.
        for s in state.db.raw_segments(&mid)? {
            if let Some(blob) = &s.text_blob {
                let aad = aad_content(folder_id, &mid, AAD_NO_PROVIDER, "segment");
                let pt = crate::crypto::decrypt(ck, blob, &aad)?;
                let text = String::from_utf8(pt).map_err(|_| {
                    AppError::Storage("decrypted segment is not valid UTF-8".into())
                })?;
                state.db.restore_segment_text(&mid, s.idx, &text)?;
            }
        }
        state.db.clear_segment_blobs(&mid)?;

        if let Some(tl) = state.db.raw_timeline(&mid)? {
            if let Some(blob) = &tl.data_blob {
                let aad = aad_content(folder_id, &mid, AAD_NO_PROVIDER, "timeline");
                let pt = crate::crypto::decrypt(ck, blob, &aad)?;
                let data = String::from_utf8(pt).map_err(|_| {
                    AppError::Storage("decrypted timeline is not valid UTF-8".into())
                })?;
                state.db.restore_timeline_data(&mid, &data)?;
            }
        }
        state.db.clear_timeline_blob(&mid)?;

        // Typed notes: permanently restore the plaintext from the blob (or keep the in-memory
        // plaintext if the folder was session-unlocked and the blob is absent), then clear the blob.
        // NEVER lose the typed notes — the plaintext is back before the blob is dropped.
        if let Some(rn) = state.db.raw_manual_notes(&mid)? {
            if let Some(blob) = &rn.blob {
                let aad = aad_content(folder_id, &mid, AAD_NO_PROVIDER, "manual_notes");
                let pt = crate::crypto::decrypt(ck, blob, &aad)?;
                let text = String::from_utf8(pt).map_err(|_| {
                    AppError::Storage("decrypted manual notes is not valid UTF-8".into())
                })?;
                state.db.set_manual_notes(&mid, &text)?;
            }
        }
        state.db.clear_manual_notes_blob(&mid)?;

        // Audio at rest: permanently restore the playback WAV + both masters from their .enc, each
        // decrypted through the role→role-less AAD ladder (a pre-role master still decrypts); the
        // .enc is dropped only after the plaintext is back.
        let (pb_role, pb_less) = audio_decrypt_ladder(&mid, folder_id, StreamRole::Playback);
        if let Some(plain) = permanent_unseal_audio(
            ck,
            state.db.get_meeting(&mid)?.and_then(|m| m.audio_path),
            &[&pb_role, &pb_less],
        )? {
            state.db.set_meeting_audio_path(&mid, Some(&plain))?;
        }
        let (mic, sys) = state.db.get_meeting_master_paths(&mid)?;
        let (mic_role, mic_less) = audio_decrypt_ladder(&mid, folder_id, StreamRole::Mic);
        if let Some(plain) = permanent_unseal_audio(ck, mic, &[&mic_role, &mic_less])? {
            state.db.set_meeting_mic_master_path(&mid, Some(&plain))?;
        }
        let (sys_role, sys_less) = audio_decrypt_ladder(&mid, folder_id, StreamRole::Sys);
        if let Some(plain) = permanent_unseal_audio(ck, sys, &[&sys_role, &sys_less])? {
            state.db.set_meeting_sys_master_path(&mid, Some(&plain))?;
        }
    }
    // Document ingestion: PERMANENTLY restore each document's plaintext from its blob (or keep the
    // in-memory plaintext if the folder was session-unlocked and the blob is absent), then clear the
    // blob. NEVER lose the document — the plaintext is back before the blob is dropped (mirrors the
    // note / manual-notes permanent restore). Then re-index (chunks + FTS always; vectors
    // model-present-gated) so the now-open folder's documents are searchable again.
    let mut restored_doc_ids: Vec<String> = Vec::new();
    for d in state.db.raw_documents_in_folder(folder_id)? {
        if let Some(blob) = &d.blob {
            let aad = aad_document(folder_id, &d.id);
            let pt = crate::crypto::decrypt(ck, blob, &aad)?;
            let text = String::from_utf8(pt)
                .map_err(|_| AppError::Storage("decrypted document is not valid UTF-8".into()))?;
            state.db.set_document_text(&d.id, &text)?;
        }
        state.db.clear_document_blob(&d.id)?;
        restored_doc_ids.push(d.id.clone());
    }
    if !restored_doc_ids.is_empty() {
        // Chunks + FTS come back unconditionally (keyword retrieval works model-less); vectors only
        // when the REAL e5 model is present (never stub vectors). KIND-ROUTED (Brain v3 audit gap
        // #3): an authored note re-chunks through the front-matter-stripping path.
        let embedder = crate::embed::embed_model_present().then(crate::embed::active_embedder);
        for did in &restored_doc_ids {
            if let Err(e) = index_document_row_kind_routed(&state.db, did, embedder.as_deref()) {
                tracing::warn!(target: "rag", error = %e, "document re-index on remove-lock failed (text restored)");
            }
        }
    }
    // MEETINGS: the folder is now permanently OPEN (plaintext + `.md` restored above), so there is no
    // privacy rationale to skip re-indexing its meetings. Re-embed them into note_chunks + vec_chunks
    // (caller-supplied model-gated embedder — never a stub vector) so semantic / related-meetings work
    // again. Mirrors the document re-embed above and the meeting half of `reindex_embeddings_inner`.
    let meeting_ids = state.db.meeting_ids_in_folder(folder_id)?;
    reindex_meetings_after_unseal(state, &meeting_ids, meeting_embedder);

    // NOTES: the folder is permanently open — re-export each authored note's vault `.md` (deleted on
    // lock) so the note lives on disk again. Best-effort (the plaintext text was restored above).
    reexport_notes_in_folder(state, folder_id);
    Ok(())
}

/// READ-GATE predicate (the user's actual complaint): a meeting is unlocked iff its folder is open
/// (NULL / not locked) OR its folder id is in the current session unlock set. Used by
/// `get_meeting_detail` / `get_segments` / `get_timeline` / `export_audio` to refuse a sealed-and-
/// not-session-unlocked meeting's content even though the SQLCipher DB is open.
/// Snapshot the live session unlock set (the same source `list_folders` / the graph reads use).
/// Passed to the `*_visible` DB reads (BLK-2b) so a sealed-and-not-unlocked meeting contributes
/// nothing to digests, search, last-note, topic threads, etc. — independent of at-rest blanking.
pub(crate) fn unlocked_snapshot(state: &AppState) -> Result<std::collections::HashSet<String>, AppError> {
    Ok(state
        .unlocked_folders
        .lock()
        .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
        .clone())
}

// `pub(crate)`: the disk-salvage worker (`audio::spill::salvage_disk_one`) re-checks this SAME
// gate fail-closed right before re-running a claimed meeting through the pipeline.
pub(crate) fn meeting_is_unlocked(state: &AppState, meeting_id: &str) -> Result<bool, AppError> {
    let folder_id = match state.db.folder_for_meeting(meeting_id)? {
        Some(f) => f,
        None => return Ok(true), // no folder / vault root → always open.
    };
    let folder = match state.db.folder_by_id(&folder_id)? {
        Some(f) => f,
        None => return Ok(true),
    };
    if !folder.locked {
        return Ok(true); // open folder.
    }
    let unlocked = state
        .unlocked_folders
        .lock()
        .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
    Ok(unlocked.contains(&folder_id))
}

/// FOLDER-level read gate (the document analogue of [`meeting_is_unlocked`]): a folder is unlocked
/// iff it is open (`locked=0`) OR its id is in the current session unlock set. Documents anchor on a
/// folder directly (not a meeting), so the document commands gate on this. A non-existent folder
/// reports `false` (fail-closed — there is nothing legitimate to read).
fn folder_is_unlocked(state: &AppState, folder_id: &str) -> Result<bool, AppError> {
    let folder = match state.db.folder_by_id(folder_id)? {
        Some(f) => f,
        None => return Ok(false), // unknown folder → nothing to surface.
    };
    if !folder.locked {
        return Ok(true); // open folder.
    }
    let unlocked = state
        .unlocked_folders
        .lock()
        .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
    Ok(unlocked.contains(folder_id))
}

// ── SEAL-ON-WRITE (2026-07-10 lock-audit F1) ─────────────────────────────────────────────────────
//
// Content WRITTEN while a locked folder is session-unlocked must be re-sealed under the folder CK
// AT WRITE TIME. The relock/reblank paths re-blank plaintext and keep the *_blob as the durable
// copy — so a plaintext-only write against a stale blob is CONTENT LOSS on the next relock, and a
// blob-less write (a note created while unlocked) is a PLAINTEXT LEAK at rest (the reblank guards on
// `blob IS NOT NULL` and rightly never blanks the only copy). These helpers give every write path
// one seam: open folder → the plain write; locked folder → encrypt the FRESH content, VERIFY it
// decrypts back byte-identical (verify-before-destroy), then persist plaintext + fresh blob in one
// atomic statement. FAIL-CLOSED: no session-cached KEK ⇒ `AppError::Locked` — never silently
// persist unsealable plaintext behind a lock.

/// Unwrap a LOCKED folder's content key from the SESSION-cached master KEK (set by `unlock_folder`
/// / `remove_lock`; zeroized by `relock_all_inner`). FAIL-CLOSED: with no cached KEK this returns
/// `AppError::Locked` — a background write path must never pop a Touch ID prompt, and must never
/// fall back to writing unsealed plaintext into a locked folder. The unwrap is AAD-bound to the
/// folder id exactly like `unlock_folder`'s (legacy empty-AAD fallback lives inside
/// `crypto::decrypt`).
fn session_folder_ck(state: &AppState, folder_id: &str) -> Result<Zeroizing<[u8; 32]>, AppError> {
    session_folder_ck_with(&state.db, &state.master_kek, folder_id)
}

/// Core of [`session_folder_ck`] over the raw handles (`Db` + the KEK mutex) instead of the whole
/// `AppState`, so seam consumers that deliberately do NOT hold an `AppState` — the agent's
/// [`crate::tools::GatedToolExecutor`] (residual W1) — can unwrap the folder CK through the SAME
/// fail-closed path. Same contract: no cached KEK ⇒ `AppError::Locked`, never a Touch ID prompt,
/// never unsealed plaintext behind a lock.
pub(crate) fn session_folder_ck_with(
    db: &crate::storage::Db,
    master_kek: &std::sync::Mutex<Option<Zeroizing<[u8; 32]>>>,
    folder_id: &str,
) -> Result<Zeroizing<[u8; 32]>, AppError> {
    let wrapped = db
        .folder_wrapped_key(folder_id)?
        .ok_or_else(|| AppError::Storage("locked folder has no wrapped key".into()))?;
    let kek: Zeroizing<[u8; 32]> = {
        let g = master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        g.clone().ok_or_else(|| {
            AppError::Locked("folder key unavailable — unlock the folder again".into())
        })?
    };
    let ck_bytes = Zeroizing::new(crate::crypto::decrypt(
        &kek,
        &wrapped,
        &aad_wrapped_ck(folder_id),
    )?);
    Ok(Zeroizing::new(ck_bytes.as_slice().try_into().map_err(
        |_| AppError::Storage("unwrapped content key has wrong length".into()),
    )?))
}

/// Encrypt an authored note's `text` under the folder's SESSION CK with the document AAD and VERIFY
/// the blob decrypts back byte-identical (verify-before-destroy) — the shared seal step of every
/// authored-note seal-on-write path (`reseal_document_if_locked`, the birth-seal in
/// `create_note_inner`, the move-seal in `move_note_doc_inner`). FAIL-CLOSED via
/// [`session_folder_ck`]: no cached KEK ⇒ `AppError::Locked` before any write.
fn sealed_document_blob(
    state: &AppState,
    folder_id: &str,
    doc_id: &str,
    text: &str,
) -> Result<Vec<u8>, AppError> {
    let ck = session_folder_ck(state, folder_id)?;
    let aad = aad_document(folder_id, doc_id);
    let blob = crate::crypto::encrypt(&ck, text.as_bytes(), &aad)?;
    if crate::crypto::decrypt(&ck, &blob, &aad)? != text.as_bytes() {
        return Err(AppError::Storage(
            "note seal-on-write verification failed (blob mismatch)".into(),
        ));
    }
    Ok(blob)
}

/// Persist an authored note's `title`+`text`+`updated_at`, RE-SEALING the fresh text into
/// `text_blob` when the owning folder is LOCKED (it is session-unlocked, or the caller's write gate
/// already refused). Open folder → the plain [`Db::update_note_row`]. Locked folder → encrypt under
/// the folder CK with the SAME AAD the folder seal uses (`aad_document`), VERIFY the blob decrypts
/// back byte-identical, then write plaintext + fresh blob atomically
/// ([`Db::update_note_row_sealed`]) — so relock re-blanks to THIS write, never a stale copy.
/// FAIL-CLOSED via [`session_folder_ck`]. Callers hold the lifecycle guard across gate+write.
fn reseal_document_if_locked(
    state: &AppState,
    folder_id: &str,
    doc_id: &str,
    title: &str,
    text: &str,
    updated_at: i64,
) -> Result<(), AppError> {
    let locked = state
        .db
        .folder_by_id(folder_id)?
        .map(|f| f.locked)
        .unwrap_or(false);
    if !locked {
        return state.db.update_note_row(doc_id, title, text, updated_at);
    }
    let blob = sealed_document_blob(state, folder_id, doc_id, text)?;
    state
        .db
        .update_note_row_sealed(doc_id, title, text, &blob, updated_at)?;
    tracing::debug!(target: "lock", note_id = %doc_id, "seal-on-write: authored note re-sealed under the folder CK");
    Ok(())
}

/// Persist a meeting note row, RE-SEALING the fresh markdown into `content_blob` when the meeting's
/// folder is LOCKED (session-unlocked, or the caller's gate already refused). Open/rootless meeting
/// → the plain [`Db::upsert_note`]. Locked → encrypt under the folder CK with the SAME AAD the
/// folder seal uses (`aad_content(..,"note")`), VERIFY byte-identical, then upsert plaintext + fresh
/// blob + the governing `folder_id` atomically ([`Db::upsert_note_sealed`]) — so relock re-blanks to
/// THIS markdown, and a NEW provider row created while unlocked is governed (and sealed) from birth.
/// FAIL-CLOSED via [`session_folder_ck`]. `pub(crate)` for the pipeline's (re)summarize persist.
pub(crate) fn upsert_note_reseal_if_locked(
    state: &AppState,
    note: &NoteRecord,
) -> Result<(), AppError> {
    let locked_folder = match state.db.folder_for_meeting(&note.meeting_id)? {
        Some(fid) => state.db.folder_by_id(&fid)?.filter(|f| f.locked),
        None => None,
    };
    let Some(folder) = locked_folder else {
        return state.db.upsert_note(note);
    };
    let ck = session_folder_ck(state, &folder.id)?;
    let aad = aad_content(&folder.id, &note.meeting_id, &note.provider_id, "note");
    let blob = crate::crypto::encrypt(&ck, note.markdown.as_bytes(), &aad)?;
    if crate::crypto::decrypt(&ck, &blob, &aad)? != note.markdown.as_bytes() {
        return Err(AppError::Storage(
            "note seal-on-write verification failed (blob mismatch)".into(),
        ));
    }
    state.db.upsert_note_sealed(note, &blob, &folder.id)?;
    tracing::debug!(target: "lock", meeting_id = %note.meeting_id, "seal-on-write: meeting note re-sealed under the folder CK");
    Ok(())
}

/// Persist the meeting's typed-notes buffer, RE-SEALING the fresh text into `manual_notes_blob`
/// when the meeting's folder is LOCKED (session-unlocked, or the caller's gate already refused).
/// Open/rootless → the plain [`Db::set_manual_notes`]. Locked → encrypt under the folder CK with
/// the SAME AAD the folder seal uses (`aad_content(.., "manual_notes")`), VERIFY byte-identical,
/// then write plaintext + fresh blob atomically ([`Db::set_manual_notes_sealed`]). FAIL-CLOSED via
/// [`session_folder_ck`].
fn set_manual_notes_reseal_if_locked(
    state: &AppState,
    meeting_id: &str,
    text: &str,
) -> Result<(), AppError> {
    set_manual_notes_reseal_with(&state.db, Some(&state.master_kek), meeting_id, text)
}

/// Core of [`set_manual_notes_reseal_if_locked`] over raw handles (residual W1): the agent's
/// [`crate::tools::GatedToolExecutor::run`] `save_note` write routes its `manual_notes` append
/// through THIS seam so an append made while the folder is session-unlocked is re-sealed at write
/// time (the pre-fix plain `set_manual_notes` write was destroyed at the next relock by the stale
/// blob restore). `master_kek: None` (an executor built without seal access) is FAIL-CLOSED: a
/// locked target refuses with `AppError::Locked` — never unsealed plaintext behind a lock; an
/// open/rootless meeting takes the plain write.
pub(crate) fn set_manual_notes_reseal_with(
    db: &crate::storage::Db,
    master_kek: Option<&std::sync::Mutex<Option<Zeroizing<[u8; 32]>>>>,
    meeting_id: &str,
    text: &str,
) -> Result<(), AppError> {
    let locked_folder = match db.folder_for_meeting(meeting_id)? {
        Some(fid) => db.folder_by_id(&fid)?.filter(|f| f.locked),
        None => None,
    };
    let Some(folder) = locked_folder else {
        return db.set_manual_notes(meeting_id, text);
    };
    let Some(master_kek) = master_kek else {
        return Err(AppError::Locked(
            "folder key unavailable — unlock the folder again".into(),
        ));
    };
    let ck = session_folder_ck_with(db, master_kek, &folder.id)?;
    let aad = aad_content(&folder.id, meeting_id, AAD_NO_PROVIDER, "manual_notes");
    let blob = crate::crypto::encrypt(&ck, text.as_bytes(), &aad)?;
    if crate::crypto::decrypt(&ck, &blob, &aad)? != text.as_bytes() {
        return Err(AppError::Storage(
            "manual-notes seal-on-write verification failed (blob mismatch)".into(),
        ));
    }
    db.set_manual_notes_sealed(meeting_id, text, &blob)?;
    tracing::debug!(target: "lock", meeting_id = %meeting_id, "seal-on-write: typed notes re-sealed under the folder CK");
    Ok(())
}

/// Persist a meeting's timeline JSON, RE-SEALING the fresh data into `data_blob` when the meeting's
/// folder is LOCKED (session-unlocked, or the caller's gate already refused). Open/rootless meeting →
/// the plain [`Db::set_timeline_data`]. Locked → encrypt under the folder CK with the SAME AAD the
/// folder seal uses (`aad_content(.., "timeline")`), VERIFY byte-identical, then upsert plaintext +
/// fresh blob atomically ([`Db::set_timeline_data_sealed`]) — so relock re-blanks to THIS timeline
/// (never a stale copy), and a timeline GENERATED while session-unlocked is sealed FROM BIRTH (never
/// a blob-less plaintext behind a lock). FAIL-CLOSED via [`session_folder_ck`]: no cached KEK ⇒
/// `AppError::Locked` before any write. 2026-07-11 audit SEAM-F1 (generate_timeline) + SEAM-F2
/// (rename_speaker).
fn set_timeline_data_reseal_if_locked(
    state: &AppState,
    meeting_id: &str,
    data: &str,
) -> Result<(), AppError> {
    let locked_folder = match state.db.folder_for_meeting(meeting_id)? {
        Some(fid) => state.db.folder_by_id(&fid)?.filter(|f| f.locked),
        None => None,
    };
    let Some(folder) = locked_folder else {
        return state.db.set_timeline_data(meeting_id, data);
    };
    let ck = session_folder_ck(state, &folder.id)?;
    let aad = aad_content(&folder.id, meeting_id, AAD_NO_PROVIDER, "timeline");
    let blob = crate::crypto::encrypt(&ck, data.as_bytes(), &aad)?;
    if crate::crypto::decrypt(&ck, &blob, &aad)? != data.as_bytes() {
        return Err(AppError::Storage(
            "timeline seal-on-write verification failed (blob mismatch)".into(),
        ));
    }
    state.db.set_timeline_data_sealed(meeting_id, data, &blob)?;
    tracing::debug!(target: "lock", meeting_id = %meeting_id, "seal-on-write: timeline re-sealed under the folder CK");
    Ok(())
}

/// The configured vault path (non-empty), or `None`. Takes `&AppState` (callers holding a
/// `tauri::State` pass `&state`, which Deref-coerces) so the `&AppState` inner cores can call it too.
pub(crate) fn vault_path(state: &AppState) -> Option<String> {
    state
        .config
        .lock()
        .ok()
        .and_then(|c| c.vault_path.clone())
        .filter(|p| !p.is_empty())
}

// ── D5 path containment: every vault FS op must stay inside the vault root ──────────────────────
//
// `create_folder` / `move_note_file` / `export_canvas` compose a vault-relative path from
// user-influenced input (a folder name, a note filename). A crafted `..` segment or an absolute
// component could otherwise escape the vault and write/overwrite an arbitrary file. These helpers
// CANONICALIZE the candidate and assert it is contained in the canonicalized vault root BEFORE any
// FS write, failing closed with `AppError::InvalidArg` on escape.

/// Canonicalize `vault` and assert `candidate` (which may not yet exist) resolves INSIDE it. Returns
/// the verified, vault-contained absolute path. Non-existent leaf components are allowed (the path is
/// about to be created) — the deepest EXISTING ancestor is canonicalized (so symlinks are resolved)
/// and the remaining components are appended after rejecting any `..` / root / prefix component that
/// could climb out. The vault root itself must exist (it is the user-configured directory).
pub(crate) fn assert_in_vault(
    vault: &std::path::Path,
    candidate: &std::path::Path,
) -> Result<std::path::PathBuf, AppError> {
    use std::path::Component;

    let root = vault
        .canonicalize()
        .map_err(|e| AppError::InvalidArg(format!("vault path is not accessible: {e}")))?;

    // Walk the candidate, splitting into the longest existing prefix (canonicalized) + a tail of
    // not-yet-existing components. Reject any `..`/RootDir/Prefix in the candidate outright — a
    // legitimate vault-relative target never needs to climb out of or re-anchor the path.
    let mut resolved = root.clone();
    // If the candidate is absolute, start from its root and let the containment check below decide;
    // but still forbid `..` traversal. We rebuild purely from Normal components joined onto either
    // the canonical existing ancestor or the vault root.
    let mut existing = root.clone();
    for comp in candidate.components() {
        match comp {
            Component::Normal(seg) => {
                let next = resolved.join(seg);
                resolved = next;
                // Track the deepest path that actually exists so we can canonicalize through
                // symlinks for the portion on disk.
                let probe = existing.join(seg);
                if probe.exists() {
                    existing = probe.canonicalize().map_err(|e| {
                        AppError::InvalidArg(format!("resolve path component: {e}"))
                    })?;
                }
            }
            Component::CurDir => {}
            // `..`, an absolute root, or a Windows prefix could escape the vault — reject.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(AppError::InvalidArg(
                    "path must stay inside the vault (no '..' or absolute segments)".into(),
                ));
            }
        }
    }

    // The canonicalized existing-prefix MUST be inside the vault root (defeats a symlink that points
    // out of the vault), and the fully-resolved target likewise.
    if !existing.starts_with(&root) || !resolved.starts_with(&root) {
        return Err(AppError::InvalidArg(
            "resolved path escapes the vault root".into(),
        ));
    }
    Ok(resolved)
}



// ═══════════════════════════════════════════════════════════════════════════════════════════════
// M3-CLIENT — account (OPAQUE) + zero-knowledge link sharing (mode A). Spec §3/§4.7/§7.
//
// LOCK-CRITICAL. The binding invariants (audited by lock-security-reviewer):
//   • `share_note_to_link` FIRST statement is `meeting_is_unlocked` → `AppError::Locked` (copies
//     `export_note`). A sealed-not-unlocked meeting leaks NOTHING.
//   • The note is cleaned (strip frontmatter + flatten wikilinks + strip obsidian://) BEFORE it
//     enters the envelope — the `vault-titles-egress-leak` class (pure fn `share::envelope`).
//   • The link key `L` NEVER leaves the device: it goes ONLY into the URL fragment, assembled
//     LOCALLY; it is never in a request body, never logged, never ledgered (convertFileSrc-trap).
//   • Every upload writes a CONTENT-FREE egress-ledger row (host + byte sizes only).
//   • First-ever share requires explicit `share_egress_consented` (fail-closed, mirrors
//     `consent_to_cloud_egress`). Share ops fail closed `Unavailable` when logged out.
//   • `list_my_shares` masks a sealed-not-unlocked meeting's title (route through `meeting_is_unlocked`).
//   • Session tokens + device id → Keychain only; MK → RAM only (`AccountSession`).
// ═══════════════════════════════════════════════════════════════════════════════════════════════

/// The FE-facing account status (camelCase). `loggedIn` = a session is present in RAM;
/// `unlockedForSharing` = the MK is available so a share can actually be sealed this session.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub logged_in: bool,
    /// Present when logged in: the account email (for display).
    pub email: Option<String>,
    /// True iff MK is in the session (a share can be created without re-auth).
    pub unlocked_for_sharing: bool,
    /// Whether the one-time share-egress consent has been granted (for the FE to show/skip the modal).
    pub share_consented: bool,
    /// Whether a sharing server is configured (a share is impossible without one).
    pub server_configured: bool,
    /// True iff logged in AND a biometric-gated MK cache exists — so a locked session can be restored
    /// with one Touch ID tap (`unlock_sharing_with_biometric`) instead of a password re-login. The FE
    /// shows the "Unlock with Touch ID" button only when this is set (and `unlockedForSharing` is not).
    /// NO Touch ID prompt is presented to compute this (existence probe only).
    pub biometric_unlock_available: bool,
}


/// Read the configured sharing-server base URL from the live config (empty ⇒ unset).
pub(crate) fn share_base_url(state: &AppState) -> Result<String, AppError> {
    Ok(state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?
        .share_base_url
        .clone())
}

/// Return a VALID (unexpired) access token for a bearer share op, PROACTIVELY redeeming the refresh
/// token via `/v1/auth/refresh` when the cached token is at/near its 30-min server-side expiry.
///
/// THIS is the fix for the "authentication error: … not authenticated" 401s: the in-RAM session (and
/// the biometric-restored session) keeps a bearer token that the server expires after 30 min, but
/// nothing ever refreshed it — so every share op past that window 401'd while the UI still showed the
/// user as logged in. Every bearer share op now obtains its token here instead of reading the cached
/// one directly. Fails closed `Unavailable` when logged out.
pub(crate) async fn valid_access_token(state: &AppState) -> Result<String, AppError> {
    // Snapshot the cached token + expiry; do NOT hold the std mutex across an await.
    let (token, expires_at) = {
        let g = state
            .account_session
            .lock()
            .map_err(|_| AppError::Storage("account-session mutex poisoned".into()))?;
        let s = crate::share::require_login(&g)?;
        (s.access_token.clone(), s.access_expires_at.clone())
    };
    if !crate::share::access_token_needs_refresh(expires_at.as_deref(), chrono::Utc::now()) {
        return Ok(token);
    }
    // RE-ENABLED (0.7.3): proactively redeem the refresh token when the bearer is at/near its 30-min
    // server-side expiry. Without this (the 0.7.2 defang) a restart-restored session — password OR
    // Touch ID — holds a DEAD bearer and every share op 401s until a full password re-login: the
    // access token TTL is 30 min and no other path ever renews it. The 0.7.1 field failures that
    // motivated the defang were server-side session-store resets during the 0.7.x deploy churn
    // (every refresh family revoked ⇒ refresh 401 ⇒ the Auth arm wiped the session), not a client
    // bug: the deployed `/v1/auth/refresh` answers correctly (401 only for an invalid token) and the
    // login response has carried `accessExpiresAt` since M1. Hardening on the re-enable: rotation is
    // RAM-first (see `refresh_session`) and the failure policy below never lets a TRANSIENT refresh
    // failure destroy a working session.
    match refresh_session(state, &token).await {
        Ok(fresh) => Ok(fresh),
        Err(e) => refresh_failure_fallback(e, token),
    }
}

/// Failure policy for a proactive token refresh (pure, unit-tested): a DEFINITIVE `Auth` refusal
/// propagates — the refresh token itself was refused, the session is unrecoverable and
/// `refresh_session` has already cleared it, so the FE must route to sign-in. ANY other failure
/// (network, 5xx, keychain) falls back to the cached bearer: it may still be valid inside the 120 s
/// refresh skew, and if it is genuinely dead the server fails closed with the same 401 the caller
/// already handles — a transient hiccup must never kill a working session (the 0.7.1 regression).
fn refresh_failure_fallback(err: AppError, cached_token: String) -> Result<String, AppError> {
    match err {
        AppError::Auth(_) => Err(err),
        other => {
            tracing::warn!(
                target: "share",
                error = %other,
                "token refresh failed transiently — continuing with the cached bearer"
            );
            Ok(cached_token)
        }
    }
}

/// Redeem the session's (single-use) refresh token for a fresh session pair, SINGLE-FLIGHTED so two
/// concurrent share ops can never double-spend the same refresh token (which would trip the server's
/// reuse detection and revoke the whole family, logging the user out mid-share). `stale_token` is the
/// access token the caller saw as expiring; if a racing op already refreshed while we waited for the
/// guard, we return THAT fresh token rather than spending our now-stale refresh token again.
///
/// Rotation is RAM-FIRST: the in-RAM session's tokens are updated the moment the server rotation
/// succeeds, and only THEN is the Keychain mirror written (best-effort, loud on failure). The reverse
/// order (0.7.1/#205) had a catastrophic failure mode: persist-then-RAM meant a Keychain write
/// failure left the just-SPENT refresh token as the stored "current" one, so the next refresh
/// re-presented it, tripped the server's reuse detection, and revoked the whole family — a forced
/// logout from a local disk/keychain hiccup. With RAM-first, a failed persist costs at most the
/// session not surviving the NEXT restart (password re-login), never the live session.
async fn refresh_session(state: &AppState, stale_token: &str) -> Result<String, AppError> {
    // Serialize refreshes; this async guard is deliberately held ACROSS the network call below (unlike
    // the std mutexes here, which are never held across an `await`).
    let _guard = state.share_refresh_lock.lock().await;

    // Double-check under the guard: a racing op may have refreshed already. If the session's token
    // changed AND is now fresh, use it — do not spend our (now-stale) refresh token a second time.
    // The session (NOT the Keychain) is the source of truth for the current refresh token — see the
    // RAM-first rationale above; the identity fields ride along for the best-effort persist below.
    let (refresh_token, device_id, email, account_id, server_user_id, generation) = {
        let g = state
            .account_session
            .lock()
            .map_err(|_| AppError::Storage("account-session mutex poisoned".into()))?;
        let s = crate::share::require_login(&g)?;
        if s.access_token != stale_token
            && !crate::share::access_token_needs_refresh(
                s.access_expires_at.as_deref(),
                chrono::Utc::now(),
            )
        {
            return Ok(s.access_token.clone());
        }
        (
            s.refresh_token.clone(),
            s.device_id.clone(),
            s.email.clone(),
            s.account_id.clone(),
            s.server_user_id.clone(),
            s.generation,
        )
    };
    let base = share_base_url(state)?;
    let client = crate::share::client::ShareClient::new(&base)?;

    let rotated = match client.refresh(&refresh_token).await {
        Ok(r) => r,
        Err(AppError::Auth(_)) => {
            // The refresh token itself is expired/revoked/reused ⇒ the session is unrecoverable. Clear
            // the in-RAM session FIRST (drops + zeroizes MK), then the Keychain tokens + biometric MK
            // cache, so `account_status` reports logged-out and the FE routes to a fresh sign-in.
            tracing::warn!(
                target: "share",
                "session refresh REFUSED by the server (401) — the refresh token is expired/revoked/reused; clearing the session"
            );
            if let Ok(mut sess) = state.account_session.lock() {
                *sess = None;
            }
            if let Err(e) = crate::share::clear_tokens() {
                tracing::warn!(
                    target: "share",
                    error = %e,
                    "session-expired cleanup: clearing the Keychain tokens failed"
                );
            }
            if let Err(e) = crate::secrets::keychain::clear_account_mk() {
                tracing::warn!(
                    target: "share",
                    error = %e,
                    "session-expired cleanup: clearing the biometric MK cache failed"
                );
            }
            return Err(AppError::Auth(
                "your sharing session expired — sign in again".into(),
            ));
        }
        // A network/5xx failure is NOT an auth problem — propagate as-is and keep the session so a
        // later op can still refresh (never clear tokens on a transient error).
        Err(e) => {
            tracing::warn!(
                target: "share",
                error = %e,
                "session refresh failed transiently (network/server) — session kept"
            );
            return Err(e);
        }
    };

    // RAM FIRST: the rotated pair becomes the live session immediately — the old refresh token is
    // SPENT server-side, so nothing may ever present it again (MK + generation untouched).
    let session_live = {
        match state.account_session.lock() {
            Ok(mut sess) => match sess.as_mut() {
                Some(s) => {
                    s.access_token = rotated.access_token.clone();
                    s.access_expires_at = Some(rotated.access_expires_at.clone());
                    s.refresh_token = rotated.refresh_token.clone();
                    true
                }
                None => false,
            },
            Err(_) => false,
        }
    };
    // A logout raced the refresh (the session vanished while the network call was in flight): do NOT
    // resurrect the rotated pair into the Keychain after an explicit sign-out — drop it on the floor
    // (nothing holds it → the server family dies by TTL) and fail the op closed. The egress still
    // happened, so the ledger row below is recorded first.
    if !session_live {
        crate::share::ledger_row(&state.db, &client.host(), "session_refresh", 0);
        tracing::warn!(
            target: "share",
            "session vanished during a token refresh (logout raced it) — rotated tokens discarded"
        );
        return Err(AppError::Unavailable(
            "not signed in to the sharing account".into(),
        ));
    }

    // Best-effort Keychain mirror so the rotated pair survives a restart. A failure here is LOUD but
    // NON-FATAL: the live session already holds the fresh pair; worst case the next restart falls
    // back to a password login. Failing the op instead would strand a spent token as "current" and
    // get the family revoked on the next refresh (the #205 failure mode).
    if let Err(e) = crate::share::store_tokens(&crate::share::PersistedTokens {
        access_token: rotated.access_token.clone(),
        refresh_token: rotated.refresh_token,
        device_id,
        email,
        account_id,
        server_user_id,
        generation,
        access_expires_at: Some(rotated.access_expires_at.clone()),
    }) {
        tracing::error!(
            target: "share",
            error = %e,
            "rotated session tokens could NOT be persisted to the Keychain — session stays live in RAM but will not survive a restart"
        );
    } else {
        tracing::info!(target: "share", "session refreshed (rotated pair persisted)");
    }

    // Content-free egress-ledger row (host + 0 bytes), consistent with `account_login`.
    crate::share::ledger_row(&state.db, &client.host(), "session_refresh", 0);
    Ok(rotated.access_token)
}

/// `account_status` — is there a logged-in sharing account this session, and can it share?
#[tauri::command]
pub fn account_status(state: State<'_, AppState>) -> Result<AccountStatus, AppError> {
    let session = state
        .account_session
        .lock()
        .map_err(|_| AppError::Storage("account-session mutex poisoned".into()))?;
    // Logged-in-but-locked survives a restart via the Keychain tokens even when MK isn't in RAM.
    let persisted_email = crate::share::load_tokens()?.map(|t| t.email);
    let (logged_in, email, unlocked) = match session.as_ref() {
        Some(s) => (true, Some(s.email.clone()), true),
        None => match persisted_email {
            Some(e) => (true, Some(e), false),
            None => (false, None, false),
        },
    };
    // Existence-only probe (NO Touch ID prompt): can a locked session be restored biometrically?
    // The no-prompt existence probe LIES for ACL'd data-protection items on current macOS
    // (2026-07-05 field incident: it reported not-found for items that a prompting read returns),
    // so a probe "false" must not HIDE the Touch ID button — on a macOS release build, offer it
    // whenever a logged-in-but-locked session exists; a tap with no real cached MK fails closed
    // ("no cached account key") and the FE falls back to the password CTA. The probe result still
    // short-circuits `true` when it does find the item.
    let probe_says_cached = crate::secrets::keychain::account_mk_cached().unwrap_or(false);
    let biometric_unlock_available =
        logged_in && (probe_says_cached || cfg!(all(target_os = "macos", not(debug_assertions))));
    let cfg = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    Ok(AccountStatus {
        logged_in,
        email,
        unlocked_for_sharing: unlocked,
        share_consented: cfg.share_egress_consented,
        server_configured: !cfg.share_base_url.trim().is_empty(),
        biometric_unlock_available,
    })
}



/// `mark_sharing_choice_made` — persist that the user has RESOLVED the first-run sharing decision
/// (either "use Murmur locally" OR they went through the account door), so the init gateway
/// (`/welcome`) never nags again. One-way latch: the ONLY mutator that sets it true; a normal
/// settings save PRESERVES it (`dto_to_config`). Idempotent — safe to call repeatedly. Carries no
/// egress and no PII.
#[tauri::command]
pub fn mark_sharing_choice_made(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cfg = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cfg.set_sharing_choice_made(&state.db)?;
    Ok(())
}

/// `account_signup(email, password, save_recovery)` — create a sharing account (spec §3.1a/§4.3).
///
/// Runs the OPAQUE CLIENT registration, generates the account MK + identity keypair + (skippable)
/// recovery phrase, wraps MK under `KEK_pw`, and uploads everything at `provision`/`provision/finish`.
/// The password never leaves the device. The email verification is a two-step flow: the server
/// emails a 6-digit code; the FE collects it and passes it here as `code`. Returns the recovery
/// phrase (24 words) ONLY when `save_recovery` is true (else `None` — skipped, §4.3).
#[tauri::command]
pub async fn account_signup(
    state: State<'_, AppState>,
    email: String,
    code: String,
    password: String,
    save_recovery: bool,
) -> Result<Option<String>, AppError> {
    let base = share_base_url(state.inner())?;
    let client = crate::share::client::ShareClient::new(&base)?;
    let password = Zeroizing::new(password);

    // 1. Exchange the emailed verification code for a single-use signup token. A rejected/expired/
    //    too-many-tries code comes back as a 4xx → InvalidArg; give the user a clear, actionable
    //    message. Connectivity errors (Unavailable) pass through so "can't reach" still means that.
    let signup_token = client
        .verify_email(email.trim(), code.trim())
        .await
        .map_err(|e| match e {
            AppError::InvalidArg(_) | AppError::Auth(_) => AppError::InvalidArg(
                "That verification code is incorrect or has expired — request a new one.".into(),
            ),
            other => other,
        })?;

    // 2. OPAQUE registration step 1 → server RegistrationResponse.
    let reg_start = crate::share::opaque_client::client_registration_start(password.as_bytes())?;
    let reg_response = client
        .provision(&signup_token, reg_start.request_bytes)
        .await?;

    // 3. OPAQUE registration step 2 → the upload + the stable export_key.
    let (upload_bytes, export_key) = crate::share::opaque_client::client_registration_finish(
        reg_start.state,
        password.as_bytes(),
        &reg_response,
    )?;
    let export_key = Zeroizing::new(export_key);

    // 4. Generate the account key hierarchy (§4.1). The server has already minted the user id at
    //    provision-time, but we don't learn it until provision/finish; the identity-slot + MK-wrap
    //    AAD bind to the account id. We use the VERIFIED email as the stable account identifier here
    //    (the server's credential_identifier is the email — see auth::provision), matching the login
    //    unwrap which re-derives the same acct binding. (A follow-up can switch to the server user_id
    //    once provision returns it before finish; deferred — keep the AAD stable across signup/login.)
    let acct_id = email.trim().to_string();
    let mk = crate::e2ee::keys::generate_master_key()?;
    let kek_pw = crate::e2ee::keys::derive_kek_pw(&export_key)?;
    let mk_wrap_pw = crate::e2ee::keys::wrap_mk_pw(&mk, &kek_pw, &acct_id)?;

    // M5 enablement: DERIVE the identity keypair deterministically from MK (not a stored random one),
    // so a fresh login on this or a second Mac re-derives the SAME sk_enc/sk_sig from MK and can
    // send/accept mode-B shares. The published bundle below is the derived public half.
    let identity = crate::e2ee::keys::derive_identity(&mk, &acct_id, 1)?;
    let created_at = chrono::Utc::now().to_rfc3339();
    let (bundle, bundle_sig) =
        crate::e2ee::keys::build_identity_bundle(&identity, &acct_id, 1, &created_at)?;

    // 5. (Skippable §4.3) recovery phrase: wrap MK under RK and RK under MK, upload both wraps.
    let (recovery_phrase, mk_wrap_rk, rk_wrap_mk) = if save_recovery {
        let (phrase, rk) = crate::e2ee::recovery::generate_recovery_phrase()?;
        let mk_wrap_rk = crate::e2ee::recovery::wrap_mk_rk(&mk, &rk, &acct_id)?;
        let rk_wrap_mk = crate::e2ee::recovery::wrap_rk_mk(&rk, &mk, &acct_id)?;
        (Some(phrase), Some(mk_wrap_rk), Some(rk_wrap_mk))
    } else {
        (None, None, None)
    };

    // 6. Atomic provision/finish — activate the account with all the key material.
    let finish_req = murmur_protocol::dto::ProvisionFinishRequest {
        signup_token,
        opaque_registration_upload: upload_bytes,
        mk_wrap_pw,
        mk_wrap_rk,
        rk_wrap_mk,
        bundle: murmur_protocol::dto::PublicKeyBundle {
            generation: bundle.generation,
            pk_enc: bundle.pk_enc.clone(),
            pk_sig: bundle.pk_sig.clone(),
            bundle_sig,
        },
    };
    let _user_id = client.provision_finish(finish_req).await?;

    // Content-free ledger row for the provisioning upload (host + a coarse size).
    crate::share::ledger_row(&state.db, &client.host(), "account_provision", 0);

    // Signup does NOT auto-login (the FE prompts a login next); no tokens stored yet.
    Ok(recovery_phrase)
}

/// `account_send_code(email)` — the PRE-SIGNUP send-code step (the leg the old inline form was
/// missing). Triggers the server to email a 6-digit verification code
/// (`POST /v1/auth/signup {email}` → 202). Needs NO account session (it runs before signup).
///
/// ANTI-ENUMERATION: the server ALWAYS returns 202 whether or not the address is already registered,
/// so this resolves `Ok(())` regardless — it never reveals account existence to the caller. The
/// email is trimmed; an empty email is rejected `InvalidArg` before any network call. Network/HTTP
/// failures map to `AppError::Unavailable` (inside `ShareClient`), never a raw reqwest string. The
/// email itself is never logged (the content-free ledger row carries only host + a coarse size).
#[tauri::command]
pub async fn account_send_code(state: State<'_, AppState>, email: String) -> Result<(), AppError> {
    let email = email.trim();
    if email.is_empty() {
        return Err(AppError::InvalidArg("email is required".into()));
    }
    let base = share_base_url(state.inner())?;
    let client = crate::share::client::ShareClient::new(&base)?;
    client.signup(email).await?;
    // Content-free ledger row for the send-code egress (host + a coarse size; no email, no code).
    crate::share::ledger_row(&state.db, &client.host(), "account_send_code", 0);
    Ok(())
}

/// `account_login(email, password) -> AccountStatus` — OPAQUE login; unwrap MK from the server's
/// `mkWrapPw` via the login `export_key`; keep MK for the session; store tokens in the Keychain.
#[tauri::command]
pub async fn account_login(
    state: State<'_, AppState>,
    email: String,
    password: String,
) -> Result<AccountStatus, AppError> {
    let base = share_base_url(state.inner())?;
    let client = crate::share::client::ShareClient::new(&base)?;
    let password = Zeroizing::new(password);
    let acct_id = email.trim().to_string();

    // OPAQUE login (3 messages).
    let login_start = crate::share::opaque_client::client_login_start(password.as_bytes())?;
    let start = client
        .login_start(acct_id.as_str(), login_start.ke1_bytes)
        .await?;
    let (ke3, export_key) = crate::share::opaque_client::client_login_finish(
        login_start.state,
        password.as_bytes(),
        &start.ke2,
    )?;
    let export_key = Zeroizing::new(export_key);
    let finish = client
        .login_finish(&start.login_id, ke3, crate::share::device_platform())
        .await?;

    // TOTP/MFA is not wired in the M3 client — if the server demands a second factor, fail cleanly
    // (the session fields are then absent). A follow-up milestone adds the `mfa_token` leg.
    if finish.mfa_required {
        return Err(AppError::Unavailable(
            "this account has two-factor auth enabled — not yet supported in the app".into(),
        ));
    }
    let access_expires_at = finish.access_expires_at.clone();
    // The account's stable server user id (UUID) — the client keys org grants on THIS, not the email.
    // A pre-fix server omits it (`None`) ⇒ org sharing prompts a re-login once the server ships it.
    let server_user_id = finish.user_id.clone();
    let (Some(access_token), Some(refresh_token), Some(device_id), Some(key_material)) = (
        finish.access_token,
        finish.refresh_token,
        finish.device_id,
        finish.key_material,
    ) else {
        return Err(AppError::Auth("login did not return a session".into()));
    };

    // Unwrap MK from the server-stored mk_wrap_pw using KEK_pw = HKDF(export_key). A wrong password
    // would have failed the OPAQUE finish above; a tampered mk_wrap fails closed here (AAD/AEAD).
    let kek_pw = crate::e2ee::keys::derive_kek_pw(&export_key)?;
    let mk = crate::e2ee::keys::unwrap_mk_pw(&key_material.mk_wrap_pw, &kek_pw, &acct_id)?;
    let generation = key_material.current_generation.unwrap_or(1);

    // Persist tokens + the non-secret generation to the Keychain (never SQLite, never logged).
    crate::share::store_tokens(&crate::share::PersistedTokens {
        access_token: access_token.clone(),
        refresh_token: refresh_token.clone(),
        device_id: device_id.clone(),
        email: acct_id.clone(),
        account_id: acct_id.clone(),
        server_user_id: server_user_id.clone(),
        generation,
        access_expires_at: access_expires_at.clone(),
    })?;

    // Cache the session (MK in RAM, zeroized on drop).
    {
        let mut session = state
            .account_session
            .lock()
            .map_err(|_| AppError::Storage("account-session mutex poisoned".into()))?;
        *session = Some(crate::share::AccountSession {
            account_id: acct_id.clone(),
            email: acct_id.clone(),
            server_user_id,
            device_id,
            mk: Zeroizing::new(*mk),
            generation,
            access_token,
            access_expires_at,
            refresh_token,
        });
    }

    // Cache the MK biometric-gated (WRITE — no Touch ID prompt) so a later restart restores the session
    // with one Touch ID tap (`unlock_sharing_with_biometric`) instead of a password re-login. Non-fatal:
    // a cache failure just means the FE won't offer the Touch ID button — login still succeeds. The MK
    // is never logged.
    if let Err(e) = crate::secrets::keychain::cache_account_mk_biometric(&mk) {
        tracing::warn!(
            target: "share",
            error = %e,
            "could not cache the account key for biometric unlock (non-fatal — password unlock still works)"
        );
    }

    crate::share::ledger_row(&state.db, &client.host(), "account_login", 0);

    // Membership discovery: reconcile the org set the server says we belong to (owned AND invited)
    // into local `org_state` now the session is cached — so an org we were invited to appears (and
    // becomes syncable) at login, not only after a create. Best-effort: a failure never blocks login.
    if let Err(e) = org_reconcile_memberships(state.inner()).await {
        tracing::warn!(target: "org", error = %brief_err(&e), "org membership reconcile at login failed (non-fatal)");
    }

    account_status(state)
}

/// `account_logout()` — best-effort server logout, clear the Keychain tokens + drop the session MK.
#[tauri::command]
pub async fn account_logout(state: State<'_, AppState>) -> Result<(), AppError> {
    // Best-effort server-side family revoke (ignore network errors — local logout still proceeds).
    if let (Ok(base), Ok(Some(access))) =
        (share_base_url(state.inner()), crate::share::access_token())
    {
        if !base.trim().is_empty() {
            if let Ok(client) = crate::share::client::ShareClient::new(&base) {
                let _ = client.logout(&access).await;
            }
        }
    }
    // Drop the in-RAM session FIRST so a fallible keychain clear below can't early-return (`?`) and
    // leave the live MK sitting in memory (lock-security review, 2026-07-05).
    if let Ok(mut session) = state.account_session.lock() {
        *session = None; // drops the AccountSession → zeroizes MK.
    }
    // Drop every unwrapped org content key too — the OCK cache is RAM-only and MUST be cleared
    // wholesale on logout (state.rs contract; lock-security review 2026-07-10). Best-effort so a
    // poisoned mutex can't block the rest of logout.
    if let Ok(mut ocks) = state.org_ock_cache.lock() {
        ocks.clear();
    }
    crate::share::clear_tokens()?;
    crate::secrets::keychain::clear_account_mk()?; // drop the biometric MK cache too (idempotent).
    Ok(())
}

/// `unlock_sharing_with_biometric() -> AccountStatus` — restore a logged-in-but-locked sharing session
/// (the MK is lost on restart, held only in RAM) by releasing the biometric-cached account master key
/// with a single Touch ID tap, instead of re-typing the account password.
///
/// Rebuilds the in-RAM [`crate::share::AccountSession`] from the persisted tokens + the biometric-
/// released MK + the persisted `generation`. Fails CLOSED — [`AppError::Unavailable`] when not signed
/// in or no MK is cached, [`AppError::BiometricFailed`] on a cancelled/failed tap — so the FE can fall
/// back to the password login. The MK never touches the log.
#[tauri::command]
pub async fn unlock_sharing_with_biometric(
    state: State<'_, AppState>,
) -> Result<AccountStatus, AppError> {
    // Must be logged in (tokens present) to have anything to restore.
    let tokens = crate::share::load_tokens()?
        .ok_or_else(|| AppError::Unavailable("not signed in to the sharing account".into()))?;

    // Release the cached MK behind a Touch ID prompt (signed build). Held zeroizing from here on;
    // fails closed if no cache exists or the user cancels the sheet.
    let mk = Zeroizing::new(
        crate::secrets::keychain::read_account_mk_biometric("Unlock Murmur sharing").inspect_err(
            |e| {
                tracing::warn!(
                    target: "share",
                    error = %e,
                    "unlock_sharing_with_biometric: account-MK release failed (keychain/biometric)"
                );
            },
        )?,
    );

    // Rebuild the session from the persisted tokens + released MK (MK moved in, zeroized on drop).
    {
        let mut session = state
            .account_session
            .lock()
            .map_err(|_| AppError::Storage("account-session mutex poisoned".into()))?;
        *session = Some(crate::share::AccountSession {
            account_id: tokens.account_id,
            email: tokens.email,
            server_user_id: tokens.server_user_id,
            device_id: tokens.device_id,
            mk,
            generation: tokens.generation,
            access_token: tokens.access_token,
            // Carry the persisted expiry so the first share op refreshes proactively when the restored
            // token is already past its 30-min TTL (the common case after any real restart).
            access_expires_at: tokens.access_expires_at,
            refresh_token: tokens.refresh_token,
        });
    }
    tracing::info!(
        target: "share",
        "unlock_sharing_with_biometric: session restored from the biometric MK cache"
    );

    account_status(state)
}








// ═══════════════════════════════════════════════════════════════════════════════════════════════
// M5-CLIENT — Murmur↔Murmur (mode B). Spec §4.8 / §6 / §7. THE HIGHEST LOCK BAR: `accept_share`
// WRITES into the user's Obsidian vault, so it is gated + verified before a single byte lands.
//
// Binding invariants (audited by lock-security-reviewer):
//   • `share_note_to_user` FIRST statement is `meeting_is_unlocked` → `AppError::Locked` (copies
//     `export_note`); then consent + login; then the note is CLEANED before enveloping; the request
//     carries ONLY ciphertext + the recipient EMAIL + wrapped keys — NEVER a title or note text.
//   • TOFU: on a `keys/lookup`, first contact PINS (on the stable account_id, not email) + shows the
//     safety-word fingerprint; an UNCHANGED pin proceeds; a CHANGED fingerprint BLOCKS (spec §4.8 —
//     key change is blocking, never click-through).
//   • `accept_share`: WRITE-GATE the target folder FIRST (default = an auto-created UNSEALED "Shared"
//     folder; a sealed-not-unlocked target is refused `AppError::Locked`); then the §4.8 signature +
//     binding verification via `open_from_sender` (HARD-FAILS unsigned/tampered/replayed/swapped/
//     gen-mismatch) BEFORE any write — on failure it writes NOTHING; IDEMPOTENT on `share_id`.
// ═══════════════════════════════════════════════════════════════════════════════════════════════





/// The TOFU state of a contact's current key vs the local pin.
pub(crate) enum TofuState {
    /// Never pinned — first contact (pin it, show safety words).
    FirstContact,
    /// The pin matches the current key — proceed.
    Match,
    /// The pin DIFFERS — a key change; BLOCK until re-verified (spec §4.8).
    Changed,
}


/// Compare a contact's current `fingerprint` to the local pin WITHOUT mutating anything.
pub(crate) fn tofu_check(
    db: &crate::storage::Db,
    account_id: &str,
    fingerprint: &str,
) -> Result<TofuState, AppError> {
    match db.get_pinned_contact(account_id)? {
        None => Ok(TofuState::FirstContact),
        Some((_, pinned)) if pinned == fingerprint => Ok(TofuState::Match),
        Some(_) => Ok(TofuState::Changed),
    }
}

/// The logged-in sharing session's `(account_id, generation, MK, access_token)` tuple.
pub(crate) type SessionMk = (String, u32, zeroize::Zeroizing<[u8; 32]>, String);

/// The logged-in sharing session's `(account_id, generation, MK, access_token)`, or a fail-closed
/// `Unavailable` when logged out (mode-B needs MK to DERIVE the identity keypair for sign/open). The
/// returned access token is proactively refreshed via [`valid_access_token`] so a long-idle mode-B
/// share never 401s on a lapsed bearer.
pub(crate) async fn require_session_mk(state: &AppState) -> Result<SessionMk, AppError> {
    // Refresh-if-needed FIRST (updates the session's cached token), then read the fresh token + MK.
    let access_token = valid_access_token(state).await?;
    let g = state
        .account_session
        .lock()
        .map_err(|_| AppError::Storage("account-session mutex poisoned".into()))?;
    let s = crate::share::require_login(&g)?;
    Ok((
        s.account_id.clone(),
        s.generation,
        zeroize::Zeroizing::new(*s.mk),
        access_token,
    ))
}

/// The account's stable SERVER USER ID (UUID) for org key-grants, from the live session. Errors with a
/// clear re-login prompt when the session predates the `server_user_id` field (a login before the
/// server started returning it on `LoginFinishResponse`). Org grants MUST key on this UUID — matching
/// `org_members.user_id` — never the email `account_id` (the `parse_org_id` 404 root cause, 2026-07-11).
pub(crate) fn session_server_user_id(state: &AppState) -> Result<String, AppError> {
    let g = state
        .account_session
        .lock()
        .map_err(|_| AppError::Storage("account-session mutex poisoned".into()))?;
    let s = crate::share::require_login(&g)?;
    s.server_user_id.clone().ok_or_else(|| {
        AppError::Unavailable(
            "sign out and sign back in to enable organization sharing (your saved session predates it)"
                .into(),
        )
    })
}


















#[cfg(test)]
#[path = "tests/lock_read_gate_tests.rs"]
mod lock_read_gate_tests;

// ── BLK-1 lifecycle-race + BLK-2 move-into-locked + BLK-3/BLK-4 config tests ──────────────────────
#[cfg(test)]
#[path = "tests/lifecycle_tests.rs"]
mod lifecycle_tests;


// ─── Task 1.4 — gateway key command argument validation ────────────────────────────────────────
#[cfg(test)]
#[path = "tests/gateway_key_tests.rs"]
mod gateway_key_tests;

#[cfg(test)]
#[path = "tests/storage_cmd_tests.rs"]
mod storage_cmd_tests;

#[cfg(test)]
#[path = "tests/pipeline_task_tests.rs"]
mod pipeline_task_tests;
