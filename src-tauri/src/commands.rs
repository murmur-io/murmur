use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use zeroize::{Zeroize, Zeroizing};

use crate::audio::Recorder;
use crate::error::AppError;
use crate::events::{StatusPayload, EVENT_STATUS};
use crate::settings::{AppConfig, BrainBackend};
use crate::state::AppState;
use crate::storage::models::{
    ActionItem, Analytics, AskVaultResult, BrainOverview, BuiltinRecipe,
    CalendarContext,
    CalendarEvent, CalendarEventFull, ChatTurn, Commitment, DigestResult, DocumentInfo,
    EntityDetail, Folder, FolderNode, GraphData, Meeting, MeetingStatus, MeetingTimeline,
    NoteRecord, PersonCard, PinResult, RecipeRecord, SearchHit, TopicThread,
};
use crate::summarize::all_providers;
use crate::transcribe::types::Segment;
use crate::{pipeline, secrets};
use tauri::Emitter;

/// Keychain account for the Anthropic API key (matches `summarize::ANTHROPIC_KEY_ACCOUNT`).
const ANTHROPIC_KEY_ACCOUNT: &str = "anthropic_api_key";

/// Keychain account for the AI Gateway API key (matches `summarize::GATEWAY_KEY_ACCOUNT`).
/// Strictly separate from `ANTHROPIC_KEY_ACCOUNT` — never a fallback to the Anthropic key (R3).
const GATEWAY_KEY_ACCOUNT: &str = "gateway_api_key";

// ── IPC DTOs (camelCase mirrors of PHASE0-PLAN §6) ──

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartResult {
    pub meeting_id: String,
}

/// Live recording state, so a freshly-loaded webview can resync to a capture that is STILL running
/// in the (long-lived) Rust process. In `tauri dev` a frontend hot-reload swaps the webview without
/// restarting the backend, so the FE store resets to `idle` while `AppState.recorder` is still
/// `Some(..)` — the desync that made the next Start fail with "already recording". This exposes only
/// the ACTIVELY-recording meeting (which cannot be sealed — it's a fresh in-progress draft), so it
/// leaks no locked content.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatus {
    /// True while the backend recorder is actively capturing.
    pub recording: bool,
    /// The in-progress meeting id, or `None` when idle.
    pub meeting_id: Option<String>,
    /// The in-progress meeting's `started_at` (RFC3339), so the FE anchors its elapsed timer to the
    /// real start instead of an epoch-sized value. `None` when idle or the row can't be read.
    pub started_at: Option<String>,
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

/// One stored voiceprint surfaced to a management view (opt-in voice biometrics). NEVER carries the
/// raw embedding — only the label + provenance + dimension the FE needs to list/forget. Read ONLY
/// through the gated `list_voiceprints` command (a sealed meeting's row never reaches here).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceprintInfo {
    pub id: String,
    pub meeting_id: String,
    /// The diarized cluster index within its source meeting (the `others-{n}` suffix).
    pub cluster_index: i64,
    /// The bound person name once the cluster is enrolled by rename (None until then).
    pub label: Option<String>,
    /// Embedding dimensionality (a harmless count; NOT the embedding itself).
    pub dim: i64,
    pub created_at: String,
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
    pub voice_trigger: bool,
    pub onboarded: bool,
    pub note_style: String,
    /// ENHANCE-MY-NOTES mode: "enhance" | "append" ("" from an older FE ⇒ "enhance").
    /// `#[serde(default)]` ⇒ an older FE payload that omits `notesMode` deserializes to `""`
    /// (the `dto_to_config` empty-guard then falls back to `"enhance"`).
    #[serde(default)]
    pub notes_mode: String,
    pub auto_organize: bool,
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

    let meeting_uuid = uuid::Uuid::new_v4();
    let meeting_id = meeting_uuid.to_string();
    let started_at = chrono::Utc::now().to_rfc3339();

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
        title: None,
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
        // D1 (spec §4.3): while Realtime Reactions (Brain Live) is on, pin the LIVE tick to `small`
        // when present — a large-v3 live tick alone can saturate the Metal GPU the light reasoner also
        // needs. If `small` is NOT on disk, fall back to the configured model + warn (pinning to an
        // absent file would kill the live loop). The post-call ACCURATE pass is unaffected.
        let live_model = if cfg.brain_live {
            match crate::transcribe::model::resolve_model_path(None, "small", lang) {
                Ok(Some(p)) => Some(p),
                _ => {
                    tracing::warn!(
                        target: "live",
                        "reactions on but ggml-small absent; live tick uses the configured whisper model (may contend with the light reasoner)"
                    );
                    configured()
                }
            }
        } else {
            configured()
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
    // and DELETES the plaintext spill + sidecar on EVERY exit path of this Stop (success, `?`-error,
    // panic) — mirroring `pipeline::ScratchWav`. It is held across `run_after_stop` below (dropped at
    // end of scope), so it survives until the archive WAV is written; after a normal Stop the spill is
    // gone. ONLY a crash (this function never runs) leaves it behind for next-launch salvage.
    // A POISONED `spill_writer` mutex (`.lock().ok()` ⇒ None) merely DEFERS this clean-stop cleanup:
    // the spill lingers to next launch, where `claim_inflight` sees the row is no longer RECORDING and
    // DiscardOrphans it — benign (no leak, no content loss), so tolerating the poison here is correct.
    let _spill_guard = state
        .spill_writer
        .lock()
        .ok()
        .and_then(|mut s| s.take());

    // The recording is definitively over — clear the accumulated live-caption buffer NOW so a
    // stale tail can never be injected into assistant prompts after Stop (nor keep egressing once
    // the just-recorded folder is sealed). The authoritative transcript is produced below.
    crate::transcribe::live::clear_live_transcript(&state.live_transcript);

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

    let result = pipeline::run_after_stop(
        &app,
        &state,
        &meeting_id,
        samples,
        src_rate,
        duration_s,
        system_wav,
        aec_mic_wav,
        mic_started_at,
        system_started_at,
    )
    .await?;

    // Resume voice listening if it's still enabled (the mic is free again).
    restart_voice_listener(app);

    Ok(StopResult {
        meeting_id: result.meeting_id,
        markdown: result.note_markdown,
        exported_path: result
            .exported_path
            .map(|p| p.to_string_lossy().to_string()),
    })
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

/// Current mic peak level 0.0..=1.0 for the meter (0.0 when idle). Cheap, polled by UI ~10x/s.
///
/// This is ALSO the detection site for the 4h `MAX_RECORDING_SECONDS` hard TIME cap. The live-caption
/// loop (`transcribe::live`) is only spawned when a whisper model resolves — a user with no model
/// downloaded gets no live loop, yet the recording (and the cap) still happen — so the live loop is
/// NOT a reliable place to detect the cap. The FE polls THIS command every 100 ms while recording
/// (`recorder.store.ts` `level`), unconditionally, for the whole recording — making it the site that
/// ALWAYS runs. On the RISING edge (cap reached, notice not yet emitted this recording) we emit
/// [`crate::events::EVENT_RECORDING_CAPPED`] exactly once so the FE can surface the notice and call
/// `stop_recording` to finalize the meeting (the capped buffer is intact — Stop still yields a note).
/// Best-effort: a failed emit only warns; the meter read is unaffected.
#[tauri::command]
pub fn recording_level(app: AppHandle, state: State<'_, AppState>) -> Result<f32, AppError> {
    let recorder = state
        .recorder
        .lock()
        .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
    let Some(r) = recorder.as_ref() else {
        return Ok(0.0);
    };
    let level = r.level();
    // 4h TIME cap: fire the "maximum recording length reached" notice exactly ONCE per recording,
    // on the false→true transition. `capped_notified` is the per-recording rising-edge latch,
    // re-armed at each `start_recording`.
    if r.cap_reached() {
        let already = state
            .capped_notified
            .load(std::sync::atomic::Ordering::Relaxed);
        if crate::audio::recorder::should_emit_cap_notice(true, already) {
            state
                .capped_notified
                .store(true, std::sync::atomic::Ordering::Relaxed);
            // PII rule (§8): log the flag only — never any content.
            tracing::warn!(
                target: "audio",
                "maximum recording length reached — surfacing cap notice to finalize the meeting"
            );
            crate::events::emit_recording_capped(&app);
        }
    }
    Ok(level)
}

/// Report whether the backend is CURRENTLY capturing, plus the in-progress meeting id and its start
/// time. A freshly-loaded webview calls this ONCE on init to resync: a `tauri dev` frontend hot-reload
/// (or any webview reload / Cmd-R / webview crash) swaps the FE without restarting the long-lived Rust
/// process, so `AppState.recorder` can still be `Some(..)` (genuinely recording to disk) while the FE
/// `RecorderStore` has reset to `idle`. Without this resync the next Start hits `start_recording`'s
/// `already recording` guard, and the Record screen disagrees with the still-`RECORDING` meeting row.
/// Read-only + leak-safe: the actively-recording meeting is a fresh in-progress draft that cannot be
/// sealed, so no `meeting_is_unlocked` gate is needed (it returns no note/transcript/audio content).
#[tauri::command]
pub fn recording_status(state: State<'_, AppState>) -> Result<RecordingStatus, AppError> {
    // The live recorder — NOT the lingering `current_meeting` — is the source of truth for "am I
    // recording". After a full process restart the recorder is `None` again, so idle is reported even
    // if a ghost row somehow survived reconcile.
    let recording = {
        let recorder = state
            .recorder
            .lock()
            .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
        recorder.is_some()
    };
    let meeting_id = if recording {
        let current = state
            .current_meeting
            .lock()
            .map_err(|_| AppError::Audio("current_meeting mutex poisoned".into()))?;
        current.map(|u| u.to_string())
    } else {
        None
    };
    Ok(recording_status_dto(&state.db, recording, meeting_id))
}

/// Assemble the [`RecordingStatus`] DTO from the recorder-presence flag + the in-progress meeting id,
/// resolving the start time from the persisted row. Split out of the command so both branches are
/// unit-testable WITHOUT a live [`Recorder`] (which needs mic hardware and can't be built headless).
/// The `started_at` lookup is best-effort: a missing/unreadable row just drops the anchor (the FE
/// falls back to "now") — it never fails the status read.
fn recording_status_dto(
    db: &crate::storage::Db,
    recording: bool,
    meeting_id: Option<String>,
) -> RecordingStatus {
    if !recording {
        return RecordingStatus {
            recording: false,
            meeting_id: None,
            started_at: None,
        };
    }
    let started_at = meeting_id
        .as_deref()
        .and_then(|id| db.get_meeting(id).ok().flatten())
        .map(|m| m.started_at);
    RecordingStatus {
        recording: true,
        meeting_id,
        started_at,
    }
}

/// Live-toggle the microphone mute mid-recording (no stream teardown). While muted, the cpal
/// capture callback writes SILENCE into the mic buffer for those frames — the stream stays
/// full-length so its wall-clock timeline (and thus "me"/"others" alignment) is preserved, and
/// no real mic audio is captured (privacy). No-op if not recording.
#[tauri::command]
pub fn set_mic_muted(state: State<'_, AppState>, muted: bool) -> Result<(), AppError> {
    let recorder = state
        .recorder
        .lock()
        .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
    if let Some(r) = recorder.as_ref() {
        r.set_muted(muted);
    }
    Ok(())
}

/// Whether the mic is currently muted on the live recorder (false when not recording).
#[tauri::command]
pub fn is_mic_muted(state: State<'_, AppState>) -> Result<bool, AppError> {
    let recorder = state
        .recorder
        .lock()
        .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
    Ok(recorder.as_ref().map(|r| r.is_muted()).unwrap_or(false))
}

/// Result of arming a MANUAL voice command (the button trigger).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceCommandArmResult {
    /// True when the live loop is now armed to capture the next utterance as a command.
    pub listening: bool,
    /// Short, non-PII reason when `listening` is false (e.g. "not recording").
    pub reason: Option<String>,
}

/// ARM the MANUAL voice-command capture: the user clicked "ask the assistant", so the next spoken
/// utterance is taken as a command — NO wake word, NO word-order requirement. This command does NOT
/// itself transcribe; it sets [`crate::state::CaptureState`] on `AppState` so the already-running
/// live-caption loop (`transcribe::live`) collects + dispatches the command over the SAME gated +
/// consent-gated `handle_voice_action` path as the wake trigger (no new egress class). Opt-in PER
/// CLICK — independent of the `realtime_reactions` toggle.
///
/// The live loop only runs DURING recording, so if no recording is in progress we arm nothing and
/// return `listening: false` with a clear reason (the FE should enable the button only while
/// recording). Emits [`crate::events::EVENT_VOICE_COMMAND_LISTENING`] so the FE can show the
/// "listening…" state; the answer arrives later via `EVENT_VOICE_ACTION_RESULT`.
#[tauri::command]
pub fn begin_voice_command(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<VoiceCommandArmResult, AppError> {
    let result = begin_voice_command_inner(state.inner())?;
    if result.listening {
        tracing::info!(target: "voice", "manual voice command armed");
        let _ = app.emit(
            crate::events::EVENT_VOICE_COMMAND_LISTENING,
            crate::events::VoiceCommandListeningPayload { active: true },
        );
    }
    Ok(result)
}

/// Headless core of [`begin_voice_command`]: arm the manual-capture state on `AppState`, returning
/// whether the live loop is now listening. The live loop only runs DURING recording, so when no
/// recording is in progress we arm nothing and report `listening: false` with a reason (arming a
/// capture nothing will ever consume would leave the FE stuck "listening"). No `AppHandle`/IPC here,
/// so it is unit-testable without Tauri.
pub(crate) fn begin_voice_command_inner(
    state: &AppState,
) -> Result<VoiceCommandArmResult, AppError> {
    // Latch the recorder's current total-sample offset AT CLICK TIME so the live loop transcribes
    // only the POST-CLICK utterance (the command the user is about to speak), cleanly isolated from
    // any prior speech in the rolling buffer. `None` (no recorder) ⇒ not recording ⇒ arm nothing.
    let start_sample = state
        .recorder
        .lock()
        .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?
        .as_ref()
        .map(|r| r.total_samples());
    let Some(offset) = start_sample else {
        return Ok(VoiceCommandArmResult {
            listening: false,
            reason: Some("not recording".into()),
        });
    };
    let mut guard = state
        .voice_command_capture
        .lock()
        .map_err(|_| AppError::Other(anyhow::anyhow!("voice-command capture mutex poisoned")))?;
    *guard = Some(crate::state::CaptureState::armed_from(offset));
    Ok(VoiceCommandArmResult {
        listening: true,
        reason: None,
    })
}

/// STOP the MANUAL voice-command capture (CLICK-TO-STOP): the user clicked "stop" / "done", so the
/// FULL accumulated post-click utterance is the command. This does NOT itself transcribe or dispatch;
/// it flips the armed [`crate::state::CaptureState`]'s `ended` flag so the already-running live loop
/// (`transcribe::live`) dispatches the FULL accumulated command over the SAME gated + consent-gated
/// `handle_voice_action` path on its next tick (no new read/egress class).
///
/// The dispatch + the "thinking…" PROCESSING event are emitted by the live loop, so the answer still
/// arrives via [`crate::events::EVENT_VOICE_ACTION_RESULT`]. On a NOT-armed state (no capture in
/// progress — the user double-clicked, or it already auto-stopped at the backstop) this is a graceful
/// no-op (`stopped: false`), never an error.
#[tauri::command]
pub fn end_voice_command(state: State<'_, AppState>) -> Result<VoiceCommandEndResult, AppError> {
    let result = end_voice_command_inner(state.inner())?;
    if result.stopped {
        tracing::info!(target: "voice", "manual voice command stopped by user — dispatching");
    }
    Ok(result)
}

/// Result of stopping a MANUAL voice command (the "stop" click).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceCommandEndResult {
    /// True when an armed capture was found and flagged to dispatch; false when nothing was armed
    /// (graceful no-op — the live loop already cleared it via dispatch or the backstop).
    pub stopped: bool,
}

/// Headless core of [`end_voice_command`]: flip the armed capture's `ended` flag so the live loop
/// dispatches the FULL accumulated utterance on its next tick. A NOT-armed state is a graceful no-op
/// (`stopped: false`). No `AppHandle`/IPC here, so it is unit-testable without Tauri.
pub(crate) fn end_voice_command_inner(state: &AppState) -> Result<VoiceCommandEndResult, AppError> {
    let mut guard = state
        .voice_command_capture
        .lock()
        .map_err(|_| AppError::Other(anyhow::anyhow!("voice-command capture mutex poisoned")))?;
    match guard.as_mut() {
        Some(capture) => {
            capture.ended = true;
            Ok(VoiceCommandEndResult { stopped: true })
        }
        // Nothing armed (already dispatched / backstop-stopped / never started) → graceful no-op.
        None => Ok(VoiceCommandEndResult { stopped: false }),
    }
}

/// Ask the in-meeting assistant a TYPED question (the text composer — the twin of the voice trigger).
/// Routes the typed command through the SAME gated agentic brain as voice ([`spawn_assistant_turn`] →
/// `run_assistant_turn`): the model decides which gated tools to call, falling through to the
/// deterministic floor on no-consent / non-convergence, and the answer arrives via
/// `EVENT_VOICE_ACTION_RESULT` with the live tool-trace on `EVENT_ASSISTANT_TOOL`. Runs OFF-thread
/// (the brain can take seconds). The text is the user's OWN words — the SAME egress class as a
/// dictated voice command (no new egress). Emits the "thinking…" processing affordance immediately.
/// `thread_id` is OPTIONAL: the FE passes an @brain thread's id to keep the exchange in that
/// thread; when absent (the voice/wake twin sends none) the backend GENERATES a UUID v4 inside the
/// turn, so every persisted exchange carries a thread identity going forward.
#[tauri::command]
pub fn ask_assistant_text(
    app: AppHandle,
    text: String,
    thread_id: Option<String>,
) -> Result<(), AppError> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(AppError::InvalidArg("empty question".into()));
    }
    let _ = app.emit(
        crate::events::EVENT_VOICE_COMMAND_PROCESSING,
        crate::events::VoiceCommandProcessingPayload { active: true },
    );
    crate::transcribe::live::spawn_assistant_turn(app, text, thread_id);
    Ok(())
}

/// One message in the in-meeting CHAT conversation (the dedicated chat panel). `role` is `"user"` or
/// `"assistant"`; the FE sends the FULL conversation (incl. the new user message as the last item) on
/// every turn, so the brain gets the prior turns as context (multi-turn memory). NO id/timestamp — the
/// FE owns the conversation state; the backend is stateless per call.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMsg {
    pub role: String,
    pub text: String,
}

/// Cap on conversation turns fed back as context — bounds tokens (and cloud egress) on a long chat.
const CHAT_CONTEXT_TURNS: usize = 12;

/// Format the chat `messages` into `(latest, conversation)`: `latest` is the user's newest message
/// (drives intent-routing + the deterministic floor), `conversation` is the recent history rendered
/// for the agentic loop's context. Errors when the last message is not a non-empty user message.
fn format_chat(messages: &[ChatMsg]) -> Result<(String, String), AppError> {
    let last = messages
        .last()
        .ok_or_else(|| AppError::InvalidArg("empty chat".into()))?;
    if !last.role.eq_ignore_ascii_case("user") || last.text.trim().is_empty() {
        return Err(AppError::InvalidArg(
            "the last chat message must be a non-empty user message".into(),
        ));
    }
    let latest = last.text.trim().to_string();
    let start = messages.len().saturating_sub(CHAT_CONTEXT_TURNS);
    let mut convo =
        String::from("This is an ongoing chat during a live meeting. Conversation so far:\n");
    for m in &messages[start..] {
        let who = if m.role.eq_ignore_ascii_case("assistant") {
            "Assistant"
        } else {
            "User"
        };
        convo.push_str(&format!("{who}: {}\n", m.text.trim()));
    }
    convo.push_str("\nAnswer the User's LATEST message, using the conversation above for context.");
    Ok((latest, convo))
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
#[tauri::command]
pub async fn ask_assistant_chat(
    app: AppHandle,
    messages: Vec<ChatMsg>,
    thread_id: Option<String>,
    anchor_text: Option<String>,
) -> Result<crate::voice_action::VoiceActionResult, AppError> {
    let (latest, conversation) = format_chat(&messages)?;
    let thread_id = crate::transcribe::live::ensure_thread_id(thread_id);
    tokio::task::spawn_blocking(move || {
        crate::transcribe::live::run_assistant_query(
            &app,
            &latest,
            &conversation,
            crate::events::EVENT_CHAT_TOOL,
            &thread_id,
            anchor_text.as_deref(),
        )
    })
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("chat task join failed: {e}")))
}

/// List the PERSISTED @brain thread exchanges for a meeting (only rows carrying a `thread_id`),
/// oldest first — the durable substrate the FE rebuilds its thread panels from across meeting
/// switches / restarts. GATED read: it routes through `list_assistant_threads_visible`
/// (`visibility_clause`-backed), so a sealed-and-not-session-unlocked meeting returns EMPTY —
/// never an error that leaks existence. On seal the rows are purged anyway
/// (`purge_assistant_interactions_tx`); the gate is defense-in-depth.
#[tauri::command]
pub fn list_assistant_threads(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<crate::storage::models::AssistantThreadRow>, AppError> {
    // Poisoned lock ⇒ empty unlock set ⇒ fail CLOSED (sealed meetings stay invisible) — the same
    // posture as the `get_meeting_detail` interactions read.
    let unlocked = state
        .unlocked_folders
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    state
        .db
        .list_assistant_threads_visible(&meeting_id, &unlocked)
}

#[cfg(test)]
mod chat_format_tests {
    use super::*;

    fn msg(role: &str, text: &str) -> ChatMsg {
        ChatMsg {
            role: role.into(),
            text: text.into(),
        }
    }

    #[test]
    fn format_chat_extracts_latest_and_renders_history() {
        let msgs = vec![
            msg("user", "what did we decide on pricing?"),
            msg("assistant", "You agreed on tiered pricing."),
            msg("user", "and the timeline?"),
        ];
        let (latest, convo) = format_chat(&msgs).unwrap();
        // `latest` (drives intent + the floor) is the newest USER message.
        assert_eq!(latest, "and the timeline?");
        // The conversation context carries the prior turns labelled by role → multi-turn memory.
        assert!(convo.contains("User: what did we decide on pricing?"));
        assert!(convo.contains("Assistant: You agreed on tiered pricing."));
        assert!(convo.contains("User: and the timeline?"));
        assert!(convo.contains("LATEST"));
    }

    #[test]
    fn format_chat_rejects_empty_or_non_user_last() {
        assert!(format_chat(&[]).is_err(), "empty chat is rejected");
        assert!(
            format_chat(&[msg("user", "   ")]).is_err(),
            "blank last message is rejected"
        );
        assert!(
            format_chat(&[msg("user", "hi"), msg("assistant", "hello")]).is_err(),
            "the last message must be from the user"
        );
    }

    #[test]
    fn format_chat_caps_history_to_recent_turns() {
        // A long chat: only the last CHAT_CONTEXT_TURNS are rendered (bounds tokens + cloud egress).
        let mut msgs: Vec<ChatMsg> = (0..39)
            .map(|i| msg("user", &format!("turn-{i}-text")))
            .collect();
        msgs.push(msg("user", "the final question"));
        let (latest, convo) = format_chat(&msgs).unwrap();
        assert_eq!(latest, "the final question");
        assert!(convo.contains("the final question"));
        assert!(
            !convo.contains("turn-0-text"),
            "turns beyond the cap are dropped"
        );
    }
}

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
#[tauri::command]
pub fn update_note(
    state: State<'_, AppState>,
    meeting_id: String,
    markdown: String,
) -> Result<NoteDto, AppError> {
    // D4 READ/WRITE-GATE: refuse to mutate a sealed-and-not-session-unlocked meeting's note. Its
    // plaintext markdown is blanked while sealed, so an edit here would overwrite the (sealed)
    // content with the blanked value and corrupt it. Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to edit the note".into(),
        ));
    }
    let existing = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;

    let created_at = chrono::Utc::now().to_rfc3339();
    state.db.upsert_note(&NoteRecord {
        meeting_id: meeting_id.clone(),
        provider_id: existing.provider_id.clone(),
        markdown: markdown.clone(),
        created_at,
        exported_path: existing.exported_path.clone(),
        model_requested: existing.model_requested.clone(),
        model_served: existing.model_served.clone(),
        gateway_host: existing.gateway_host.clone(),
    })?;

    if let Some(path) = existing.exported_path.as_deref() {
        crate::export::overwrite_note(std::path::Path::new(path), &markdown)?;
    }

    Ok(NoteDto {
        meeting_id,
        provider_id: existing.provider_id,
        markdown,
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
    let note = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    // Strip our own old markers so extraction/judgment sees the canonical note lines.
    let stripped = crate::verify::apply_verify_markers(&note.markdown, &[]);
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
    let marked = crate::verify::apply_verify_markers(&existing.markdown, &findings);
    // Save + re-export — the exact `update_note` tail, with `marked`.
    let created_at = chrono::Utc::now().to_rfc3339();
    state.db.upsert_note(&NoteRecord {
        meeting_id: meeting_id.clone(),
        provider_id: existing.provider_id.clone(),
        markdown: marked.clone(),
        created_at,
        exported_path: existing.exported_path.clone(),
        model_requested: existing.model_requested.clone(),
        model_served: existing.model_served.clone(),
        gateway_host: existing.gateway_host.clone(),
    })?;
    if let Some(path) = existing.exported_path.as_deref() {
        crate::export::overwrite_note(std::path::Path::new(path), &marked)?;
    }
    Ok(NoteDto {
        meeting_id,
        provider_id: existing.provider_id,
        markdown: marked,
        exported_path: existing.exported_path,
    })
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
    if !meeting_is_unlocked(state, meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to edit your notes".into(),
        ));
    }
    state.db.set_manual_notes(meeting_id, text)?;
    // PII rule: log only the meeting id + buffer length, never the typed text.
    tracing::debug!(target: "notes", meeting_id = %meeting_id, len = text.len(), "manual notes saved");
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

/// The md/txt extensions document ingestion accepts. md/txt only — NO new parsing crate; the file is
/// read as UTF-8 text via `std::fs::read_to_string`. Anything else is rejected with `InvalidArg`.
const DOC_ALLOWED_EXTS: &[&str] = &["md", "txt"];

/// Document ingestion — upload a local md/txt file INTO a folder so its text is chunked + embedded
/// into the on-device vector layer and the brain/Ask can retrieve it. Returns the new document id.
///
/// LOCK-MODEL:
/// - WRITE-GATE: refuse a sealed-and-NOT-session-unlocked folder (`AppError::Locked`) — an ungated
///   write would land plaintext at rest behind the lock (mirrors `save_manual_notes`'s gate).
/// - Extension allowlist (`md`/`txt`) — reject anything else with `AppError::InvalidArg`; NO new
///   crate (read as UTF-8 text).
/// - EMBED only when the REAL e5 model is present (`embed_model_present()`): otherwise the chunks are
///   stored WITHOUT vectors (no stub vectors polluting the index — mirrors `should_auto_index`).
/// - The text is SEALED-AND-RESTORED with the folder on lock/unlock; its chunks are PURGED on lock,
///   re-embeddable on unlock.
#[tauri::command]
pub fn import_document(
    state: State<'_, AppState>,
    path: String,
    folder_id: String,
) -> Result<String, AppError> {
    import_document_inner(state.inner(), &path, &folder_id)
}

/// Inner of [`import_document`] taking `&AppState` (so the gate + allowlist are unit-testable without
/// a `tauri::State`).
pub(crate) fn import_document_inner(
    state: &AppState,
    path: &str,
    folder_id: &str,
) -> Result<String, AppError> {
    // Extension allowlist (md/txt only). Lowercased; an extension-less path is rejected.
    let p = std::path::Path::new(path);
    let ext_ok = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .map(|e| DOC_ALLOWED_EXTS.contains(&e.as_str()))
        .unwrap_or(false);
    if !ext_ok {
        return Err(AppError::InvalidArg(
            "only .md and .txt documents can be imported".into(),
        ));
    }

    // Read the file as UTF-8 text (no parsing crate). A non-UTF-8 / unreadable file fails closed.
    let text = std::fs::read_to_string(p)
        .map_err(|e| AppError::InvalidArg(format!("could not read document: {e}")))?;

    // The display name = the file name (component only — never an on-disk path with personal content
    // in logs). Fallback to "document" if the path has no file-name component.
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "document".to_string());

    ingest_into_folder(state, folder_id, &name, &text, "document")
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

/// The SINGLE gated ingest path for both an uploaded document (`kind="document"`) and a typed note
/// (`kind="note"`): look up the folder, WRITE-GATE it (a sealed-not-unlocked folder is refused so
/// content can never appear at rest behind a lock), insert the `documents` row, and index its chunks
/// into the vector layer ONLY when the REAL e5 model is present (never stub vectors — mirrors
/// `should_auto_index`). The row is sealed-and-restored + purged-on-lock identically regardless of
/// kind. Returns the new id.
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
    // DEFAULT install — an ingested document must never be write-only memory. Vectors ONLY when the
    // REAL e5 model is present (never write stub vectors). Best-effort: a failure logs (no PII) and
    // does NOT fail the ingest (the row + plaintext are durable; a later unlock re-chunk / reindex
    // recovers the index).
    let embedder = crate::embed::embed_model_present().then(crate::embed::active_embedder);
    if let Err(e) = state.db.index_document_chunks(&id, embedder.as_deref()) {
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
    Ok(text)
}

/// Headline counts + semantic flags for the Brain page ("what's in my brain"). All counts are over
/// VISIBLE/unlocked content only (a sealed-not-unlocked folder's items are never counted); carries
/// NO text. The two flags drive the "vectorize your brain" nudge (semantic off / model absent).
#[tauri::command]
pub fn brain_overview(state: State<'_, AppState>) -> Result<BrainOverview, AppError> {
    brain_overview_inner(state.inner())
}

/// Inner of [`brain_overview`] taking `&AppState` (unit-testable gate).
pub(crate) fn brain_overview_inner(state: &AppState) -> Result<BrainOverview, AppError> {
    let unlocked = unlocked_snapshot(state)?;
    let (meeting_count, document_count, note_count, indexed_chunk_count) =
        state.db.brain_counts(&unlocked)?;
    let semantic_enabled = state
        .config
        .lock()
        .map(|c| c.semantic_search_enabled)
        .unwrap_or(false);
    Ok(BrainOverview {
        meeting_count,
        document_count,
        note_count,
        indexed_chunk_count,
        semantic_enabled,
        embed_model_present: crate::embed::embed_model_present(),
    })
}

/// Permanently delete a document and cascade-delete its chunks + vectors. GATED: a
/// sealed-and-NOT-session-unlocked folder is refused (`AppError::Locked`) so the lock state can't be
/// mutated from behind the gate (consistent with `import_document`'s write-gate).
#[tauri::command]
pub fn delete_document(state: State<'_, AppState>, id: String) -> Result<(), AppError> {
    delete_document_inner(state.inner(), &id)
}

/// Inner of [`delete_document`] taking `&AppState` (unit-testable gate).
pub(crate) fn delete_document_inner(state: &AppState, id: &str) -> Result<(), AppError> {
    let Some(folder_id) = state.db.folder_for_document(id)? else {
        return Ok(()); // unknown id → idempotent no-op.
    };
    if !folder_is_unlocked(state, &folder_id)? {
        return Err(AppError::Locked(
            "this folder is locked — unlock it to delete a document".into(),
        ));
    }
    state.db.delete_document(id)?;
    tracing::info!(target: "documents", document_id = %id, "document deleted");
    Ok(())
}

// ── CROSS-MEETING USER MEMORY (Phase 3) ────────────────────────────────────────
//
// The auditable "what the brain knows about you" surface: list the current user-scoped memory facts
// (with provenance), forget one, or clear all. Every read is VISIBILITY-GATED — a user fact whose
// SOURCE meeting is sealed-and-not-session-unlocked is INVISIBLE here (and injected into no prompt),
// because `list_user_facts_visible` filters by source-meeting `visibility_clause` on the live
// unlocked snapshot. Forget/clear are bitemporal INVALIDATE (close valid_to), never a silent delete.

/// List the current user-memory facts (open + visible) with provenance, plus the synthesized brief
/// that is injected into grounding. GATED: only facts whose SOURCE meeting is visible under the live
/// unlocked snapshot are returned — a sealed-not-unlocked meeting's user memory surfaces NOTHING.
#[tauri::command]
pub fn get_user_memory(
    state: State<'_, AppState>,
) -> Result<crate::user_memory::UserMemory, AppError> {
    get_user_memory_inner(state.inner())
}

/// Inner of [`get_user_memory`] taking `&AppState` (unit-testable gate).
pub(crate) fn get_user_memory_inner(
    state: &AppState,
) -> Result<crate::user_memory::UserMemory, AppError> {
    // FLAG: memory turned OFF entirely ⇒ the explicit disabled marker (empty facts + empty brief +
    // `disabled: true`), so the FE shows a "memory is off" affordance and NOTHING is surfaced. This
    // mirrors the injection paths, which are also flag-suppressed — the audit view can never show
    // facts the brain would not inject.
    if !user_memory_enabled(state) {
        return Ok(crate::user_memory::UserMemory::disabled());
    }
    let unlocked = unlocked_snapshot(state)?;
    let facts = state.db.list_user_facts_visible(&unlocked)?;
    // The audit view and the injected brief are derived from EXACTLY the same visible set, so the UI
    // faithfully mirrors what the brain actually injects.
    let brief = crate::user_memory::synthesize_brief(&facts);
    let dtos = facts
        .iter()
        .map(crate::user_memory::UserMemoryFact::from_fact)
        .collect();
    Ok(crate::user_memory::UserMemory {
        facts: dtos,
        brief,
        disabled: false,
    })
}

/// Forget ONE user-memory fact (bitemporal invalidate — the row is CLOSED, never silently deleted,
/// so history is preserved). After this the fact drops out of `get_user_memory` and the regenerated
/// brief. Idempotent. Content-free logging (the fact id only, never its text).
#[tauri::command]
pub fn forget_user_fact(state: State<'_, AppState>, id: String) -> Result<(), AppError> {
    forget_user_fact_inner(state.inner(), &id)
}

/// Inner of [`forget_user_fact`] taking `&AppState` (unit-testable).
pub(crate) fn forget_user_fact_inner(state: &AppState, id: &str) -> Result<(), AppError> {
    let at = chrono::Utc::now().to_rfc3339();
    let closed = state.db.forget_user_fact(id, &at)?;
    tracing::info!(target: "user_memory", fact_id = %id, closed, "user fact forgotten (invalidated)");
    Ok(())
}

/// Clear ALL user memory: bitemporal-close every currently-open user fact (invalidate, never delete —
/// closed history stays). After this `get_user_memory` and the brief are empty. Content-free logging
/// (a count only).
#[tauri::command]
pub fn clear_user_memory(state: State<'_, AppState>) -> Result<(), AppError> {
    clear_user_memory_inner(state.inner())
}

/// Inner of [`clear_user_memory`] taking `&AppState` (unit-testable).
pub(crate) fn clear_user_memory_inner(state: &AppState) -> Result<(), AppError> {
    let at = chrono::Utc::now().to_rfc3339();
    let n = state.db.clear_user_facts(&at)?;
    tracing::info!(target: "user_memory", count = n, "user memory cleared (all facts invalidated)");
    Ok(())
}

/// Full-text-ish search across meeting titles, transcripts, and notes (Library search).
#[tauri::command]
pub fn search_meetings(
    state: State<'_, AppState>,
    query: String,
) -> Result<Vec<SearchHit>, AppError> {
    // BLK-2b: search only VISIBLE meetings (open/unlocked folders) so a sealed-and-not-unlocked
    // meeting's title/transcript/note never surfaces in a hit — independent of at-rest blanking.
    let unlocked = unlocked_snapshot(state.inner())?;
    state.db.search_visible(&query, 100, &unlocked)
}

/// Permanently delete a meeting: its audio file, its exported vault note, and all DB rows
/// (segments, notes, timeline cascade via FK). Irreversible.
#[tauri::command]
pub fn delete_meeting(state: State<'_, AppState>, meeting_id: String) -> Result<(), AppError> {
    // Capture + remove on-disk files before the rows disappear (best-effort).
    if let Some(m) = state.db.get_meeting(&meeting_id)? {
        if let Some(audio) = m.audio_path.as_deref() {
            let _ = std::fs::remove_file(audio);
        }
    }
    // Masters too — a master path may be the plaintext WAV or its `.enc`; clear both forms.
    if let Ok((mic, sys)) = state.db.get_meeting_master_paths(&meeting_id) {
        for p in [mic, sys].into_iter().flatten() {
            let _ = std::fs::remove_file(&p);
            let _ = std::fs::remove_file(format!("{p}{ENC_SUFFIX}"));
            let _ = std::fs::remove_file(p.trim_end_matches(ENC_SUFFIX));
        }
    }
    if let Some(note) = state.db.get_latest_note_for_meeting(&meeting_id)? {
        if let Some(path) = note.exported_path.as_deref() {
            let _ = std::fs::remove_file(path);
        }
    }
    state.db.delete_meeting(&meeting_id)
}

/// Rename a meeting's title (in-app + Library list). Does not rename the vault file.
#[tauri::command]
pub fn rename_meeting(
    state: State<'_, AppState>,
    meeting_id: String,
    title: String,
) -> Result<(), AppError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(AppError::InvalidArg("title cannot be empty".into()));
    }
    state.db.set_meeting_title(&meeting_id, title)
}

/// Grounded Q&A over a meeting's transcript ("chat with the meeting"). The configured
/// provider answers strictly from the transcript + the running conversation history.
#[tauri::command]
pub async fn chat_meeting(
    state: State<'_, AppState>,
    meeting_id: String,
    question: String,
    history: Vec<ChatTurn>,
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
    let transcript = segments
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
    // ASK role: meeting chat is a Q&A surface. With role keys absent this resolves to the same
    // default provider as before (the legacy chat path always ignored `brain_backend`).
    let provider = crate::summarize::provider_for(crate::summarize::roles::Role::Ask, &config)?;
    // Inject the gated cross-meeting USER MEMORY brief (parity with the @brain agentic loop): derived
    // from VISIBLE user facts only under the LIVE unlock snapshot, empty when memory is disabled ⇒
    // byte-identical prompt. Rides this surface's existing redaction + consent egress (no new class).
    let unlocked = unlocked_snapshot(state.inner())?;
    let memory_brief = gated_memory_brief_for_injection(state.inner(), &unlocked);
    let (system, user) =
        crate::summarize::chat::build(&transcript, &history, &question, &memory_brief);
    provider.complete(&system, &user).await
}

/// Copy a meeting's recording (WAV) to a user-chosen path (FE picks it via a save dialog).
#[tauri::command]
pub fn export_audio(
    state: State<'_, AppState>,
    meeting_id: String,
    dest_path: String,
) -> Result<(), AppError> {
    // Phase 0.5 READ-GATE: refuse to export the audio of a sealed-and-not-unlocked meeting. Its
    // WAV is AES-GCM-encrypted at rest (audio_path → <file>.enc) and there is no plaintext on disk
    // to copy until the folder is session-unlocked; fail closed with a Locked error.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to export the audio".into(),
        ));
    }
    let meeting = state
        .db
        .get_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no meeting with id {meeting_id}")))?;
    let src = meeting
        .audio_path
        .ok_or_else(|| AppError::InvalidArg("this meeting has no audio file".into()))?;
    std::fs::copy(&src, &dest_path)
        .map_err(|e| AppError::Storage(format!("copy audio failed: {e}")))?;
    Ok(())
}

/// Which per-stream master to export.
enum MasterStream {
    Mic,
    Sys,
}

/// Shared READ-GATED export for a per-stream master archive (faithful float32 WAV). Refuses a
/// sealed-and-not-unlocked meeting (the master is `.enc` at rest, no plaintext to copy) and never
/// hands a path to the FE — the masters are reachable ONLY through these gated commands.
fn export_master(
    state: State<'_, AppState>,
    meeting_id: &str,
    dest_path: &str,
    which: MasterStream,
) -> Result<(), AppError> {
    if !meeting_is_unlocked(state.inner(), meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to export the master".into(),
        ));
    }
    let (mic, sys) = state.db.get_meeting_master_paths(meeting_id)?;
    let src = match which {
        MasterStream::Mic => mic,
        MasterStream::Sys => sys,
    }
    .ok_or_else(|| AppError::InvalidArg("this meeting has no master for that stream".into()))?;
    std::fs::copy(&src, dest_path)
        .map_err(|e| AppError::Storage(format!("copy master failed: {e}")))?;
    Ok(())
}

/// Export a meeting's MIC master archive (faithful native-rate float32 WAV) to a chosen path.
#[tauri::command]
pub fn export_mic_master(
    state: State<'_, AppState>,
    meeting_id: String,
    dest_path: String,
) -> Result<(), AppError> {
    export_master(state, &meeting_id, &dest_path, MasterStream::Mic)
}

/// Export a meeting's SYSTEM master archive (faithful 48 kHz float32 WAV) to a chosen path.
#[tauri::command]
pub fn export_sys_master(
    state: State<'_, AppState>,
    meeting_id: String,
    dest_path: String,
) -> Result<(), AppError> {
    export_master(state, &meeting_id, &dest_path, MasterStream::Sys)
}

/// Write a meeting's note markdown to a user-chosen path (FE picks it via a save dialog).
#[tauri::command]
pub fn export_note(
    state: State<'_, AppState>,
    meeting_id: String,
    dest_path: String,
) -> Result<(), AppError> {
    // D4 READ-GATE: refuse to export a sealed-and-not-unlocked meeting's note (its plaintext
    // markdown is blanked while sealed — exporting would write an empty/garbage file). Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to export the note".into(),
        ));
    }
    let note = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    std::fs::write(&dest_path, note.markdown.as_bytes())
        .map_err(|e| AppError::Storage(format!("write note failed: {e}")))?;
    Ok(())
}

/// Best-effort detection of a running meeting app (Zoom / Teams / Webex) to offer a
/// "start recording?" nudge. Browser-based Google Meet is NOT detectable this way.
#[tauri::command]
pub fn detect_meeting_app() -> Result<Option<String>, AppError> {
    let listing = match std::process::Command::new("ps")
        .arg("-axo")
        .arg("comm=")
        .output()
    {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(_) => return Ok(None),
    };
    for (needle, name) in [
        ("zoom.us", "Zoom"),
        ("Microsoft Teams", "Microsoft Teams"),
        ("Webex", "Webex"),
    ] {
        if listing.contains(needle) {
            return Ok(Some(name.to_string()));
        }
    }
    Ok(None)
}

/// Replace a meeting's tags (trimmed, de-duplicated by the DB).
#[tauri::command]
pub fn set_meeting_tags(
    state: State<'_, AppState>,
    meeting_id: String,
    tags: Vec<String>,
) -> Result<(), AppError> {
    state.db.set_meeting_tags(&meeting_id, &tags)
}

/// A meeting's tags (sorted).
#[tauri::command]
pub fn get_meeting_tags(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<String>, AppError> {
    state.db.get_meeting_tags(&meeting_id)
}

/// All distinct tags across meetings (for the Library filter).
#[tauri::command]
pub fn list_all_tags(state: State<'_, AppState>) -> Result<Vec<String>, AppError> {
    state.db.list_all_tags()
}

/// Meetings carrying a given tag, newest first.
#[tauri::command]
pub fn list_meetings_by_tag(
    state: State<'_, AppState>,
    tag: String,
) -> Result<Vec<Meeting>, AppError> {
    state.db.list_meetings_by_tag(&tag)
}

/// Built-in recipe templates (quick chips).
#[tauri::command]
pub fn list_builtin_recipes() -> Result<Vec<BuiltinRecipe>, AppError> {
    Ok(crate::summarize::recipes::BUILTIN_RECIPES
        .iter()
        .map(|(id, label, prompt)| BuiltinRecipe {
            id: id.to_string(),
            label: label.to_string(),
            prompt: prompt.to_string(),
        })
        .collect())
}

/// User-saved recipe templates.
#[tauri::command]
pub fn list_saved_recipes(state: State<'_, AppState>) -> Result<Vec<RecipeRecord>, AppError> {
    state.db.list_saved_recipes()
}

/// Save a recipe template (prompt + title).
#[tauri::command]
pub fn save_recipe(
    state: State<'_, AppState>,
    title: String,
    prompt: String,
) -> Result<RecipeRecord, AppError> {
    let title = title.trim();
    let prompt = prompt.trim();
    if title.is_empty() || prompt.is_empty() {
        return Err(AppError::InvalidArg(
            "recipe title and prompt are required".into(),
        ));
    }
    let rec = RecipeRecord {
        id: uuid::Uuid::new_v4().to_string(),
        title: title.to_string(),
        prompt: prompt.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    state.db.insert_recipe(&rec)?;
    Ok(rec)
}

/// Delete a saved recipe.
#[tauri::command]
pub fn delete_recipe(state: State<'_, AppState>, id: String) -> Result<(), AppError> {
    state.db.delete_recipe(&id)
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
    let provider = crate::summarize::provider_for(crate::summarize::roles::Role::Notes, &config)?;
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
    state.db.upsert_note(&NoteRecord {
        meeting_id: meeting_id.clone(),
        provider_id: existing.provider_id.clone(),
        markdown: patched.clone(),
        created_at,
        exported_path: existing.exported_path.clone(),
        model_requested: existing.model_requested.clone(),
        model_served: existing.model_served.clone(),
        gateway_host: existing.gateway_host.clone(),
    })?;
    if let Some(path) = existing.exported_path.as_deref() {
        crate::export::overwrite_note(std::path::Path::new(path), &patched)?;
    }
    Ok(NoteDto {
        meeting_id,
        provider_id: existing.provider_id,
        markdown: patched,
        exported_path: existing.exported_path,
    })
}

/// Escape a string for embedding inside an AppleScript `"…"` literal: backslash + double-quote are
/// escaped, and raw CR/LF are flattened to spaces (an AppleScript string literal cannot span lines).
/// This is what stops the item text from breaking out of the quoted literal or injecting extra
/// statements (`"`, `end tell`, …) into the osascript program.
fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\n', '\r'], " ")
}

/// Parse a strict ISO `YYYY-MM-DD` into `(year, month, day)`; `None` for anything else.
fn parse_iso_ymd(s: &str) -> Option<(i32, u32, u32)> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i32 = s.get(0..4)?.parse().ok()?;
    let m: u32 = s.get(5..7)?.parse().ok()?;
    let d: u32 = s.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((y, m, d))
}

/// Build the osascript program that creates a Reminder named `name`. When `due_date` is a valid
/// ISO `YYYY-MM-DD`, attach `remind me date`/`due date` (defaulted to 9am local) so the date
/// actually lands in Reminders — previously the date was dropped. The name is
/// `escape_applescript`-escaped so its text can never break out of the string literal. The date is
/// built by setting `day` to 1 FIRST (so a year/month change can't overflow the current day-of-month),
/// then year, then month, then the real day.
pub(crate) fn build_reminder_script(name: &str, due_date: Option<&str>) -> String {
    let esc = escape_applescript(name);
    match due_date.and_then(parse_iso_ymd) {
        Some((y, m, d)) => format!(
            "set theDate to current date\n\
             set day of theDate to 1\n\
             set year of theDate to {y}\n\
             set month of theDate to {m}\n\
             set day of theDate to {d}\n\
             set hours of theDate to 9\n\
             set minutes of theDate to 0\n\
             set seconds of theDate to 0\n\
             tell application \"Reminders\" to make new reminder with properties {{name:\"{esc}\", remind me date:theDate, due date:theDate}}"
        ),
        None => format!(
            "tell application \"Reminders\" to make new reminder with properties {{name:\"{esc}\"}}"
        ),
    }
}

/// Add a macOS Reminder (via osascript) for an action item. A denied Reminders permission
/// surfaces a clear, actionable error rather than crashing the UI. When the item carries an ISO
/// due date, it is set as the reminder's due/remind date (best-effort; verify on a real Mac).
#[tauri::command]
pub async fn add_reminder(text: String, due_date: Option<String>) -> Result<(), AppError> {
    let name = text.trim().to_string();
    if name.is_empty() {
        return Err(AppError::InvalidArg("empty reminder".into()));
    }
    let due = due_date.as_deref().filter(|d| !d.is_empty());
    let script = build_reminder_script(&name, due);
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
    })
    .await
    .map_err(|e| AppError::Unavailable(format!("reminder task failed: {e}")))?
    .map_err(|e| AppError::Unavailable(format!("osascript failed: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(AppError::Unavailable(format!(
            "Could not add to Reminders — grant access in System Settings ▸ Privacy & Security ▸ Reminders. ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// SYNCHRONOUS reminder creation for the off-thread voice-action dispatch (Flow B). Mirrors the
/// `add_reminder` command's osascript path, but blocking (it already runs on a detached task, so it
/// must not require an async runtime). Returns `Ok(())` on success, a typed `AppError` otherwise —
/// NEVER panics. NO PII logged by the caller; the reminder text is the user's own dictated note.
pub(crate) fn add_reminder_blocking(text: &str, due_date: Option<&str>) -> Result<(), AppError> {
    let name = text.trim();
    if name.is_empty() {
        return Err(AppError::InvalidArg("empty reminder".into()));
    }
    let due = due_date.filter(|d| !d.is_empty());
    let script = build_reminder_script(name, due);
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| AppError::Unavailable(format!("osascript failed: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(AppError::Unavailable(format!(
            "Could not add to Reminders — grant access in System Settings ▸ Privacy & Security ▸ Reminders. ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
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
    state.db.upsert_note(&NoteRecord {
        meeting_id: meeting_id.clone(),
        provider_id: existing.provider_id.clone(),
        markdown: new_md.clone(),
        created_at,
        exported_path: existing.exported_path.clone(),
        model_requested: existing.model_requested.clone(),
        model_served: existing.model_served.clone(),
        gateway_host: existing.gateway_host.clone(),
    })?;
    let url = match existing.exported_path.as_deref() {
        Some(path) => {
            crate::export::overwrite_note(std::path::Path::new(path), &new_md)?;
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
    state: &AppState,
    meeting_id: &str,
    title: &str,
    markdown: &str,
) -> Result<crate::summarize::graph::GraphPayload, AppError> {
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    let provider = crate::summarize::provider_for(crate::summarize::roles::Role::Notes, &config)?;
    let payload =
        crate::summarize::graph::extract_entities(provider.as_ref(), title, markdown).await?;

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

    // brain2 R2 — BITEMPORAL FACTS. BEST-EFFORT + NEVER fails the note: extract entity·predicate·
    // object candidates (on-device reasoner; empty with the stub / no model), load the existing facts
    // for these entities, run the PURE DETERMINISTIC reconcile, stamp the source meeting, and apply
    // the ops in ONE tx. `valid_from = recorded_at = the meeting's time` (started_at) — the deterministic
    // `at`. A reconcile/extract hiccup is logged (non-PII) and swallowed.
    if let Err(e) = persist_facts_for_meeting(state, meeting_id, title, markdown, &entity_refs) {
        tracing::warn!(target: "facts", error = %e, "fact reconcile failed (note unaffected)");
    }

    // Phase 3 CROSS-MEETING USER MEMORY — extract → reconcile → apply USER-SCOPED facts (preferences,
    // ongoing work, commitments about the USER) from the note + the user's own typed notes. Same
    // BEST-EFFORT + NEVER-fails-the-note contract as the entity facts above: empty with the stub / no
    // model, a hiccup is logged (non-PII) and swallowed. Runs even with zero entities (user memory is
    // not entity-scoped).
    if let Err(e) = persist_user_facts_for_meeting(state, meeting_id, title, markdown) {
        tracing::warn!(target: "user_memory", error = %e, "user-fact reconcile failed (note unaffected)");
    }

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

    Ok(payload)
}

/// brain2 R2 — extract → reconcile → apply bitemporal FACTS for one summarized meeting. Pulled out
/// of [`build_and_persist_entities`] so it can fail in isolation (its caller logs + swallows): a
/// facts hiccup must NEVER block the note pipeline. Steps:
///   1. BEST-EFFORT extract candidates from the note about `entity_refs` (empty with the stub/no
///      model — the deterministic core is what carries the value),
///   2. load the EXISTING facts for those entities (un-gated lifecycle read),
///   3. run the PURE deterministic [`crate::facts::reconcile_facts`] at the meeting's time,
///   4. stamp the source meeting onto the Add ops and apply them in ONE atomic tx.
///
/// `at` is the meeting's `started_at` (the fact's valid-time origin), falling back to now.
fn persist_facts_for_meeting(
    state: &AppState,
    meeting_id: &str,
    title: &str,
    markdown: &str,
    entity_refs: &[(String, String)],
) -> Result<(), AppError> {
    if entity_refs.is_empty() {
        return Ok(());
    }
    // 1) Best-effort extraction (panic-free, empty on stub/no model/decode failure). The reasoner
    //    is re-resolved from the LIVE config, so a consent/backend change applies without restart.
    let candidates = crate::facts::extract_fact_candidates(
        // Brain Live ON ⇒ the LOCAL light engine (facts stop egressing); OFF ⇒ today's Notes reasoner.
        &*state.reasoner.extraction_reasoner(),
        title,
        markdown,
        entity_refs,
        // Post-call extraction over the full note: no tight cap (the realtime path uses a capped preset).
        crate::reason::GenOptions::default(),
    );
    if candidates.is_empty() {
        return Ok(()); // nothing to reconcile — common in the default (no-model) build.
    }
    // 2) Existing facts for exactly these entities.
    let entity_ids: Vec<String> = entity_refs.iter().map(|(id, _)| id.clone()).collect();
    let existing = state.db.facts_for_entities(&entity_ids)?;
    // 3) Deterministic reconcile at the meeting's time (valid-time origin).
    let at = state
        .db
        .get_meeting(meeting_id)?
        .map(|m| m.started_at)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let mut ops = crate::facts::reconcile_facts(&existing, &candidates, &at);
    // 4) Stamp the source meeting (gating + purge anchor) and apply atomically.
    crate::facts::set_meeting_id(&mut ops, meeting_id);
    state.db.apply_fact_ops(&ops)?;
    Ok(())
}

/// Phase 3 CROSS-MEETING USER MEMORY — extract → reconcile → apply USER-SCOPED facts for one
/// summarized meeting. Mirrors [`persist_facts_for_meeting`] but for the user, not entities:
///   1. BEST-EFFORT extract user·predicate·object candidates from the note + the user's own typed
///      notes (empty with the stub / no model — the deterministic core is what carries the value),
///   2. load the EXISTING user facts (un-gated lifecycle read — reconcile runs before any seal),
///   3. run the PURE deterministic [`crate::facts::reconcile_facts`] at the meeting's time (the
///      user-scope sentinel in `entity_id` keys the reconcile),
///   4. stamp the source meeting onto the Add ops and apply them to `user_facts` in ONE atomic tx.
///
/// The (derived) memory brief is regenerated lazily on the next read/turn — no cache to invalidate.
fn persist_user_facts_for_meeting(
    state: &AppState,
    meeting_id: &str,
    title: &str,
    markdown: &str,
) -> Result<(), AppError> {
    // FLAG: when the user has turned cross-meeting memory OFF, skip extraction ENTIRELY — no
    // reasoner call, no candidates, nothing new persisted. (Existing facts stay; the user can
    // forget/clear them, and the gated reads/injection are separately flag-suppressed.)
    if !user_memory_enabled(state) {
        return Ok(());
    }
    // The user's OWN typed notes for this meeting are a high-signal memory source (an explicit
    // "remember that…"). Empty when none (best-effort read).
    let typed_notes = state.db.get_manual_notes(meeting_id).unwrap_or_default();
    // D5 — the meeting's own @brain THREAD TURNS are the HIGHEST-signal source (an explicit
    // "zapamiętaj, że…" in a thread). GATED like every content read: the just-finished meeting is
    // its own unlocked meeting, so `list_assistant_interactions_visible` under the live unlock
    // snapshot returns its turns (and NOTHING for a sealed-not-unlocked meeting — fail-closed). We
    // feed the USER COMMAND text (the high-signal part), never the assistant's answer.
    let thread_turns = gated_meeting_thread_turns(state, meeting_id);
    // 1) Best-effort extraction (panic-free, empty on stub/no model/decode failure). The reasoner is
    //    re-resolved from the LIVE config so a consent/backend change applies without restart.
    let candidates = crate::user_memory::extract_user_fact_candidates(
        // Brain Live ON ⇒ the LOCAL light engine (user facts stop egressing); OFF ⇒ today's reasoner.
        &*state.reasoner.extraction_reasoner(),
        title,
        markdown,
        &typed_notes,
        &thread_turns,
    );
    if candidates.is_empty() {
        return Ok(()); // nothing to reconcile — common in the default (no-model) build.
    }
    // 2) Existing user facts (all of them — the reconcile input).
    let existing = state.db.user_facts_all()?;
    // 3) Deterministic reconcile at the meeting's time (valid-time origin).
    let at = state
        .db
        .get_meeting(meeting_id)?
        .map(|m| m.started_at)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let mut ops = crate::facts::reconcile_facts(&existing, &candidates, &at);
    // 4) Stamp the source meeting (gating + purge anchor) and apply atomically.
    crate::facts::set_meeting_id(&mut ops, meeting_id);
    state.db.apply_user_fact_ops(&ops)?;
    Ok(())
}

/// Whether cross-meeting USER MEMORY is enabled (config `user_memory_enabled`, default TRUE). When
/// OFF: no extraction runs, no brief is injected into ANY surface, and `get_user_memory` reports the
/// disabled marker. Fail-safe: a poisoned config mutex reports ENABLED (the default) so a transient
/// lock error never silently disables the feature.
fn user_memory_enabled(state: &AppState) -> bool {
    state
        .config
        .lock()
        .map(|c| c.user_memory_enabled)
        .unwrap_or(true)
}

/// Read the meeting's OWN @brain THREAD TURNS for user-fact extraction (design spec D5), GATED by the
/// live session unlock snapshot: `list_assistant_interactions_visible` returns the meeting's turns
/// only when the meeting is VISIBLE (a sealed-not-unlocked meeting returns EMPTY — fail-closed). Only
/// the USER COMMAND text is included (the high-signal part — an explicit "zapamiętaj, że…"); the
/// assistant's answer is never fed back into extraction. Best-effort: any read error ⇒ empty string
/// (extraction degrades to note+notes). Content-free on error.
fn gated_meeting_thread_turns(state: &AppState, meeting_id: &str) -> String {
    let unlocked = match unlocked_snapshot(state) {
        Ok(u) => u,
        Err(_) => return String::new(),
    };
    let turns = match state
        .db
        .list_assistant_interactions_visible(meeting_id, &unlocked)
    {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    turns
        .iter()
        .map(|i| format!("User: {}", i.command.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The gated cross-meeting USER MEMORY brief for injection into the non-agentic Ask / meeting-chat
/// surfaces (design spec: parity with the @brain agentic loop, which already injects it). It is
/// DERIVED data — never sealed, always REGENERATED from the currently-VISIBLE user facts under the
/// passed `unlocked` snapshot — so a sealed-not-unlocked meeting's user facts inject NOTHING. When
/// memory is disabled (config `user_memory_enabled == false`) it returns EMPTY, so the prompt is
/// byte-identical to the pre-memory prompt. Rides the EXISTING redaction + consent egress of the
/// surface it is injected into — no new egress class.
fn gated_memory_brief_for_injection(
    state: &AppState,
    unlocked: &std::collections::HashSet<String>,
) -> String {
    if !user_memory_enabled(state) {
        return String::new();
    }
    let facts = state
        .db
        .list_user_facts_visible(unlocked)
        .unwrap_or_default();
    crate::user_memory::synthesize_brief(&facts)
}

/// Resolve the people + projects in a meeting note → persist them to the encrypted DB graph
/// (always) and mirror them as `[[Person]]` / `[[Project]]` vault stubs (only when a vault is
/// configured + the meeting's folder is unsealed). The graph self-assembles. The DB sink works
/// even with no vault set — hence no hard "set a vault folder" error anymore.
#[tauri::command]
pub async fn link_meeting_entities(
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
    build_and_persist_entities(&state, &meeting_id, &title, &note.markdown).await
}

/// Max co-occurring neighbors returned with an entity's detail (the neighborhood satellites).
const ENTITY_NEIGHBOR_LIMIT: i64 = 12;

/// The self-assembling graph: all VISIBLE entity nodes (with their visible mention counts) + all
/// VISIBLE co-occurrence edges. Snapshots the live session `unlocked` set (same as `list_folders`)
/// and pushes it through the visibility predicate, so sealed-and-not-unlocked meetings contribute
/// nothing — the graph can never disagree with Library/MCP about what's visible.
#[tauri::command]
pub fn get_graph(state: State<'_, AppState>) -> Result<GraphData, AppError> {
    let unlocked = unlocked_snapshot(state.inner())?;
    state.db.build_graph(&unlocked)
}

/// `/people` personal CRM: one card per VISIBLE Person entity, rolled up over the SAME gated
/// graph/facts/commitment readers as the graph + rollup views (`list_entities_visible` filtered to
/// people, `entity_mentions_visible`, `list_facts_visible`, `list_open_commitments`). Snapshots the
/// live session `unlocked` set like `get_graph`, so a person whose only mentions are in
/// sealed-and-not-session-unlocked meetings never appears and every count reflects visible sources
/// only. Read-only, no model, no new egress.
#[tauri::command]
pub fn list_people(state: State<'_, AppState>) -> Result<Vec<PersonCard>, AppError> {
    let unlocked = unlocked_snapshot(state.inner())?;
    state.db.list_people(&unlocked)
}

/// Detail for one entity: the entity, its VISIBLE backlinked meetings (as `VaultSource` chips),
/// and its top co-occurring neighbors. Snapshots the live `unlocked` set like `get_graph`.
/// Errors with `InvalidArg` if the entity id is unknown.
#[tauri::command]
pub fn get_entity_detail(
    state: State<'_, AppState>,
    entity_id: String,
) -> Result<EntityDetail, AppError> {
    let unlocked = unlocked_snapshot(state.inner())?;
    state
        .db
        .build_entity_detail(&entity_id, &unlocked, ENTITY_NEIGHBOR_LIMIT)?
        .ok_or_else(|| AppError::InvalidArg(format!("no entity with id {entity_id}")))
}

/// Structured, GATED, egress-free person dossier for the `/people` detail pane. Unlike
/// [`entity_dossier`] (which CLOUD-synthesizes a markdown String via the provider and discards the
/// struct), this returns the STRUCTURED [`DossierData`](crate::summarize::dossier::DossierData) with
/// NO provider/cloud call — deterministic DB assembly, strictly MORE local-first. Gated exactly like
/// [`get_entity_detail`]/[`list_people`]: it snapshots the LIVE session unlock set and reuses
/// `build_dossier_data` VERBATIM, so a sealed-and-not-session-unlocked meeting contributes NOTHING
/// (its title, note body, commitments, and facts all stay invisible until the folder is
/// session-unlocked). `corpus` is `#[serde(skip)]`, so meeting note bodies never reach the FE.
#[tauri::command]
pub fn get_person_dossier(
    state: State<'_, AppState>,
    entity_id: String,
) -> Result<crate::summarize::dossier::DossierData, AppError> {
    get_person_dossier_inner(state.inner(), &entity_id)
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
    let memory_brief = gated_memory_brief_for_injection(state.inner(), &unlocked);
    ask_vault_floor(
        &state.db,
        &config,
        &unlocked,
        &question,
        &history,
        &memory_brief,
    )
    .await
}

/// Max agentic rounds for the Ask surface. Not live-latency-bound like the in-meeting loop
/// (`CLOUD_MAX_STEPS` = 4), so it gets a little more room to search + read before answering —
/// still strictly bounded.
const ASK_MAX_STEPS: usize = 6;

/// Cap the incoming Ask history to the last [`CHAT_CONTEXT_TURNS`] turns — the same discipline as
/// the in-meeting chat panel, closing the unbounded-prompt-growth gap the pre-agentic `ask_vault`
/// had (it rendered the whole history uncapped).
fn capped_ask_history(history: &[ChatTurn]) -> &[ChatTurn] {
    let start = history.len().saturating_sub(CHAT_CONTEXT_TURNS);
    &history[start..]
}

/// Run the vault-scoped agentic attempt for [`ask_vault`]. Returns `Some(result)` ONLY when the
/// loop CONVERGED; `None` on non-convergence or ANY loop error — incl. `Unavailable` (no cloud
/// consent) — so the caller floors to the pre-agentic path with its original semantics.
fn ask_vault_agentic_attempt(
    app: &AppHandle,
    question: &str,
    history: &[ChatTurn],
    thread_id: &str,
) -> Option<AskVaultResult> {
    let state = app.state::<AppState>();
    let config = match state.config.lock() {
        Ok(c) => c.clone(),
        Err(_) => return None, // poisoned config ⇒ floor (which will surface its own error)
    };
    // Re-resolved per turn (never a startup snapshot): consent/provider/backend changes apply.
    // ASK role — under the legacy fallback this dispatches exactly like the pre-role `current()`.
    let reasoner = state
        .reasoner
        .current_for(crate::summarize::roles::Role::Ask);
    // VAULT-SCOPED executor: no live meeting, READ-ONLY, and NO note drafts (the Ask page has no
    // notes flow / Accept affordance, so `propose_note` is not advertised on this surface). The
    // AppHandle is present so web_search / calendar_lookup participate under their existing
    // consent/availability gates. Every read re-checks the LIVE unlocked set per call (C6).
    let executor = crate::tools::GatedToolExecutor {
        db: &state.db,
        unlocked: &state.unlocked_folders,
        config: &config,
        meeting_id: "",
        app: Some(app),
        allow_writes: false,
        note_drafts: false,
        proposed_note: std::sync::Mutex::new(None),
    };
    let sink = crate::transcribe::live::ToolEventSink {
        app: app.clone(),
        event: crate::events::EVENT_ASK_TOOL,
        thread_id: thread_id.to_string(),
    };
    // Gated cross-meeting USER MEMORY brief for the agentic persona (parity with the @brain loop):
    // VISIBLE facts only under the LIVE unlock snapshot, empty when memory is disabled ⇒ the persona
    // is byte-identical. Rides the loop's existing redaction + consent egress (no new class).
    let unlocked_now = state
        .unlocked_folders
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    let memory_brief = gated_memory_brief_for_injection(&state, &unlocked_now);
    match ask_vault_loop(
        &*reasoner,
        &executor,
        &state.db,
        &state.unlocked_folders,
        question,
        history,
        &memory_brief,
        Some(&sink as &dyn crate::agent::DeltaSink),
    ) {
        Ok(converged) => converged,
        Err(e) => {
            // PII rule: the error only — never the question/history text.
            tracing::debug!(
                target: "ask",
                error = %e,
                "ask agentic loop unavailable/failed; flooring to corpus completion"
            );
            None
        }
    }
}

/// The testable core of the agentic Ask path: drive [`crate::agent::run_agentic_loop`] with the
/// vault-QA persona over the rendered conversation, then map a converged outcome onto the Ask DTO.
/// `Ok(None)` = non-convergence (caller floors); `Err` propagates (caller floors) — the loop
/// contract of `run_informational`, applied to the Ask surface.
#[allow(clippy::too_many_arguments)]
fn ask_vault_loop(
    reasoner: &dyn crate::reason::LocalReasoner,
    executor: &dyn crate::agent::ToolExecutor,
    db: &crate::storage::Db,
    unlocked: &std::sync::Mutex<std::collections::HashSet<String>>,
    question: &str,
    history: &[ChatTurn],
    memory_brief: &str,
    sink: Option<&dyn crate::agent::DeltaSink>,
) -> Result<Option<AskVaultResult>, AppError> {
    let system = crate::summarize::vault_chat::agentic_system(memory_brief);
    let user = crate::summarize::vault_chat::render_conversation(history, question);
    let Some(outcome) =
        crate::agent::run_agentic_loop(reasoner, &system, &user, executor, ASK_MAX_STEPS, sink)?
    else {
        return Ok(None);
    };
    // Resolve sources against the LIVE unlocked set (fail-closed on a poisoned lock: no source
    // chips rather than an ungated resolution).
    let unlocked_now = unlocked.lock().map(|g| g.clone()).unwrap_or_default();
    Ok(Some(agent_outcome_to_ask_result(
        db,
        &unlocked_now,
        outcome,
    )))
}

/// Map a converged [`crate::agent::AgentOutcome`] onto the Ask DTO. `citations` carries the loop's
/// gated citation strings verbatim (`[[Title]]` / `(web) …`); `sources` additionally resolves each
/// `[[Title]]` to its VISIBLE meeting (id + date) so the existing source chips keep working. A
/// title that doesn't resolve to a visible meeting simply contributes no source — never an error,
/// never an ungated read (`meeting_by_title_visible` applies the same visibility predicate as
/// every gated reader).
fn agent_outcome_to_ask_result(
    db: &crate::storage::Db,
    unlocked: &std::collections::HashSet<String>,
    outcome: crate::agent::AgentOutcome,
) -> AskVaultResult {
    let mut sources: Vec<crate::storage::models::VaultSource> = Vec::new();
    for cite in &outcome.citations {
        let Some(title) = cite.strip_prefix("[[").and_then(|c| c.strip_suffix("]]")) else {
            continue; // "(web) …" / "(calendar) …" attributions have no meeting to resolve.
        };
        match db.meeting_by_title_visible(title, unlocked) {
            Ok(Some(m)) if !sources.iter().any(|s| s.meeting_id == m.id) => {
                sources.push(crate::storage::models::VaultSource {
                    meeting_id: m.id,
                    title: m.title.unwrap_or_else(|| title.to_string()),
                    started_at: m.started_at,
                });
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(target: "ask", error = %e, "citation source resolution failed")
            }
        }
    }
    AskVaultResult {
        answer: outcome.answer,
        sources,
        citations: outcome.citations,
    }
}

/// The floor's prompt assembly, split from the provider call so the floor-equivalence test can
/// prove it byte-identical to the pre-agentic implementation without a live provider.
enum AskFloorPrompt {
    /// Nothing to search — the canned early-return result (identical to the pre-change string).
    Empty(AskVaultResult),
    /// The assembled corpus prompt, ready for ONE provider completion.
    Ready {
        system: String,
        user: String,
        sources: Vec<crate::storage::models::VaultSource>,
    },
}

/// Everything the pre-agentic `ask_vault` did BEFORE its provider call, verbatim: gated corpus
/// assembly (hybrid when semantic search is ON, FTS otherwise — Phase 2b semantics unchanged), the
/// empty-corpus early return, and the corpus prompt build. The floor-equivalence test binds this
/// to the original statement sequence.
fn build_ask_vault_floor_prompt(
    db: &crate::storage::Db,
    config: &AppConfig,
    unlocked: &std::collections::HashSet<String>,
    question: &str,
    history: &[ChatTurn],
    memory_brief: &str,
) -> Result<AskFloorPrompt, AppError> {
    // Phase 2b (gated): when semantic search is ON, pick candidates by HYBRID retrieval (FTS ∪
    // vector KNN, RRF-fused) — embedding the query with the active embedder — then pack with the
    // SAME budget/citation logic and the SAME visibility gate. When OFF (the default) OR the index
    // is empty, this falls back to the existing FTS-only path UNCHANGED (the hybrid query
    // degenerates to FTS when no vectors exist; the flag-off branch is byte-for-byte the prior
    // behavior).
    // Budget on the ASK-role provider's RESOLVED connection — the corpus egresses to it. With
    // role keys absent this is the legacy `provider_id` for EVERY brain_backend (the pre-role
    // floor always ignored `brain_backend`), so the packed corpus is byte-identical.
    let ask_conn =
        crate::summarize::roles::provider_target(crate::summarize::roles::Role::Ask, config)
            .connection;
    let (corpus, sources) = if config.semantic_search_enabled {
        let embedder = crate::embed::active_embedder();
        // QUERY side: use the e5 `query:` prefix (asymmetric with the `passage:` index side).
        let query_vec = embedder
            .embed_query(std::slice::from_ref(&question.to_string()))?
            .into_iter()
            .next()
            .unwrap_or_default();
        crate::summarize::vault_context::build_vault_context_hybrid_visible(
            db, question, &ask_conn, &query_vec, unlocked,
        )?
    } else {
        crate::summarize::vault_context::build_vault_context_visible(
            db, question, &ask_conn, unlocked,
        )?
    };
    if corpus.trim().is_empty() {
        return Ok(AskFloorPrompt::Empty(AskVaultResult {
            answer: "No meeting notes to search yet — record and summarize a meeting first."
                .to_string(),
            sources: Vec::new(),
            citations: Vec::new(),
        }));
    }
    let (system, user) =
        crate::summarize::vault_chat::build(&corpus, history, question, memory_brief);
    Ok(AskFloorPrompt::Ready {
        system,
        user,
        sources,
    })
}

/// THE FLOOR — the pre-agentic Ask-My-Vault implementation: gated corpus pack + ONE provider
/// completion, with the original error/consent semantics (`make_provider`'s fail-closed consent
/// gate errors exactly as before). Runs on the local/off brain backend and whenever the agentic
/// attempt did not converge or errored.
async fn ask_vault_floor(
    db: &crate::storage::Db,
    config: &AppConfig,
    unlocked: &std::collections::HashSet<String>,
    question: &str,
    history: &[ChatTurn],
    memory_brief: &str,
) -> Result<AskVaultResult, AppError> {
    match build_ask_vault_floor_prompt(db, config, unlocked, question, history, memory_brief)? {
        AskFloorPrompt::Empty(result) => Ok(result),
        AskFloorPrompt::Ready {
            system,
            user,
            sources,
        } => {
            // ASK role. With role keys absent this builds the legacy default provider for EVERY
            // brain_backend (the pre-role floor ignored it) — original error/consent semantics.
            let provider =
                crate::summarize::provider_for(crate::summarize::roles::Role::Ask, config)?;
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
mod ask_vault_tests {
    use super::*;
    use crate::agent::ToolExecutor;
    use crate::reason::LocalReasoner;
    use crate::storage::models::{Folder, Meeting, MeetingStatus, NoteRecord};
    use crate::storage::Db;
    use serde_json::Value;
    use std::collections::{HashSet, VecDeque};
    use std::sync::Mutex;

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn tmp_db() -> Db {
        let p = crate::storage::db::unique_temp_path("murmur-askvault", "sqlite");
        Db::open_with_key(&p, TEST_DEK).unwrap()
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    fn seed_note(db: &Db, id: &str, title: &str, markdown: &str, folder: Option<&str>) {
        db.insert_meeting(&Meeting {
            id: id.into(),
            started_at: "2026-06-26T09:00:00Z".into(),
            ended_at: None,
            title: Some(title.into()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: id.into(),
            provider_id: "claude_code".into(),
            markdown: markdown.into(),
            created_at: "2026-06-26T09:05:00Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder(id, folder).unwrap();
    }

    /// A LOCKED folder + a blanked-note meeting inside it — the sealed-and-not-unlocked at-rest
    /// shape (title still indexed; the visibility gate is what must hide it).
    fn seed_sealed(db: &Db, meeting_id: &str, folder_id: &str, title: &str) {
        db.insert_folder(&Folder {
            id: folder_id.into(),
            name: "Secret".into(),
            path: "Secret".into(),
            parent_id: None,
            locked: true,
            created_at: "2026-06-26T00:00:00Z".into(),
        })
        .unwrap();
        seed_note(db, meeting_id, title, "", Some(folder_id));
    }

    /// A reasoner whose `structured()` returns canned JSON in sequence (a test double — the
    /// production loop drives the real ReasonerCell dispatch). Exhaustion yields an empty answer,
    /// which the loop treats as non-convergence.
    struct ScriptReasoner {
        script: Mutex<VecDeque<crate::error::Result<Value>>>,
    }
    impl ScriptReasoner {
        fn ok(steps: Vec<Value>) -> Self {
            Self {
                script: Mutex::new(steps.into_iter().map(Ok).collect()),
            }
        }
    }
    impl LocalReasoner for ScriptReasoner {
        fn id(&self) -> &str {
            "script"
        }
        fn reason(&self, _s: &str, _u: &str) -> crate::error::Result<String> {
            Ok("unused".into())
        }
        fn structured(&self, _s: &str, _u: &str, _schema: &Value) -> crate::error::Result<Value> {
            self.script
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(serde_json::json!({ "answer": "" })))
        }
    }

    /// The VAULT-SCOPED executor exactly as `ask_vault_agentic_attempt` builds it (no meeting,
    /// read-only, NO note drafts), minus the AppHandle (headless: connectors unavailable).
    fn ask_executor<'a>(
        db: &'a Db,
        unlocked: &'a Mutex<HashSet<String>>,
        cfg: &'a AppConfig,
    ) -> crate::tools::GatedToolExecutor<'a> {
        crate::tools::GatedToolExecutor {
            db,
            unlocked,
            config: cfg,
            meeting_id: "",
            app: None,
            allow_writes: false,
            note_drafts: false,
            proposed_note: Mutex::new(None),
        }
    }

    /// RED-first floor equivalence (the binding test of "the floor is today's behavior"): the
    /// extracted floor prompt must be BYTE-IDENTICAL to the pre-change statement sequence —
    /// `build_vault_context_visible` → `vault_chat::build` — for the same inputs, and the
    /// empty-corpus early return must keep the exact canned string. RED-proven: perturbing the
    /// floor (e.g. swapping the corpus builder for the fail-closed shim, or reordering sections)
    /// fails the byte equality here.
    #[test]
    fn ask_floor_prompt_matches_pre_change_implementation() {
        let db = tmp_db();
        seed_note(
            &db,
            "m1",
            "Atlas Kickoff",
            "We decided to ship atlas on Friday.",
            None,
        );
        seed_note(&db, "m2", "Weekly Sync", "Anna owns QA for atlas.", None);
        // Tier 1 flipped the semantic default ON; PIN it false so this test keeps EXPLICITLY exercising
        // the FTS-floor branch (its stated purpose) rather than the hybrid branch — which, on an empty
        // vec_chunks, happens to degenerate to the same bytes. Defensive/self-documenting, not a strict
        // regression guard (the hybrid path is byte-identical here even without the pin).
        let cfg = AppConfig {
            semantic_search_enabled: false,
            ..AppConfig::default()
        };
        let unlocked = HashSet::new();
        let history = vec![
            ChatTurn {
                role: "user".into(),
                content: "earlier question".into(),
            },
            ChatTurn {
                role: "assistant".into(),
                content: "earlier answer".into(),
            },
        ];
        let q = "what did we decide about atlas?";

        // The PRE-CHANGE implementation, replicated statement-for-statement.
        let (corpus, want_sources) = crate::summarize::vault_context::build_vault_context_visible(
            &db,
            q,
            &cfg.provider_id,
            &unlocked,
        )
        .unwrap();
        assert!(
            !corpus.trim().is_empty(),
            "fixture must produce a non-empty corpus"
        );
        // Empty memory brief ⇒ the floor prompt must stay BYTE-IDENTICAL to the pre-memory build.
        let (want_system, want_user) =
            crate::summarize::vault_chat::build(&corpus, &history, q, "");

        match build_ask_vault_floor_prompt(&db, &cfg, &unlocked, q, &history, "").unwrap() {
            AskFloorPrompt::Ready {
                system,
                user,
                sources,
            } => {
                assert_eq!(
                    system, want_system,
                    "floor system prompt diverged from pre-change"
                );
                assert_eq!(
                    user, want_user,
                    "floor user prompt diverged from pre-change"
                );
                assert_eq!(
                    sources
                        .iter()
                        .map(|s| s.meeting_id.as_str())
                        .collect::<Vec<_>>(),
                    want_sources
                        .iter()
                        .map(|s| s.meeting_id.as_str())
                        .collect::<Vec<_>>(),
                    "floor sources diverged from pre-change"
                );
            }
            AskFloorPrompt::Empty(_) => panic!("a non-empty corpus must yield Ready"),
        }

        // The empty-vault early return keeps the EXACT pre-change canned answer.
        let empty = tmp_db();
        match build_ask_vault_floor_prompt(&empty, &cfg, &unlocked, q, &[], "").unwrap() {
            AskFloorPrompt::Empty(r) => {
                assert_eq!(
                    r.answer,
                    "No meeting notes to search yet — record and summarize a meeting first."
                );
                assert!(r.sources.is_empty() && r.citations.is_empty());
            }
            AskFloorPrompt::Ready { .. } => panic!("an empty vault must yield the canned Empty"),
        }
    }

    /// The floor's ERROR/CONSENT semantics are untouched: an unconsented cloud provider is refused
    /// by `make_provider`'s fail-closed gate with `AppError::Unavailable` — exactly the pre-change
    /// behavior the FE consent flow keys on.
    #[test]
    fn ask_floor_preserves_no_consent_error_semantics() {
        let db = tmp_db();
        seed_note(
            &db,
            "m1",
            "Atlas Kickoff",
            "We decided to ship atlas on Friday.",
            None,
        );
        let cfg = AppConfig {
            provider_id: "anthropic".into(),
            ..AppConfig::default()
        };
        assert!(
            !cfg.cloud_egress_consented,
            "fresh config defaults to consent OFF"
        );
        let res = block_on(ask_vault_floor(
            &db,
            &cfg,
            &HashSet::new(),
            "atlas?",
            &[],
            "",
        ));
        assert!(
            matches!(res, Err(AppError::Unavailable(_))),
            "no-consent floor must keep the Unavailable refusal: {res:?}"
        );
    }

    /// The Cloud loop path: a scripted brain calls a GATED tool, answers, and the tool-derived
    /// `[[Title]]` citations flow into the DTO — verbatim in `citations` AND resolved (gated) into
    /// the structured `sources` chips.
    #[test]
    fn ask_loop_tool_citations_flow_into_dto() {
        let db = tmp_db();
        seed_note(
            &db,
            "m1",
            "Atlas Kickoff",
            "## Action items\n- [ ] Anna — ship the deck 2026-07-10\n",
            None,
        );
        let cfg = AppConfig::default();
        let unlocked = Mutex::new(HashSet::new());
        let exec = ask_executor(&db, &unlocked, &cfg);
        let brain = ScriptReasoner::ok(vec![
            serde_json::json!({ "tool": "get_open_commitments", "args": {} }),
            serde_json::json!({ "answer": "Anna ships the deck by 2026-07-10 [[Atlas Kickoff]]." }),
        ]);
        let out = ask_vault_loop(
            &brain,
            &exec,
            &db,
            &unlocked,
            "who owns the deck?",
            &[],
            "",
            None,
        )
        .unwrap()
        .expect("scripted brain converged");
        assert_eq!(
            out.answer,
            "Anna ships the deck by 2026-07-10 [[Atlas Kickoff]]."
        );
        assert!(
            out.citations.contains(&"[[Atlas Kickoff]]".to_string()),
            "tool-derived citation must reach the DTO verbatim: {:?}",
            out.citations
        );
        assert_eq!(
            out.sources.len(),
            1,
            "the citation resolves to ONE source chip"
        );
        assert_eq!(out.sources[0].meeting_id, "m1");
        assert_eq!(out.sources[0].title, "Atlas Kickoff");
        assert_eq!(out.sources[0].started_at, "2026-06-26T09:00:00Z");
    }

    /// The loop contract at this surface: non-convergence → `Ok(None)` (the command floors); a
    /// reasoner error — the no-consent `Unavailable` — PROPAGATES (the command's attempt wrapper
    /// converts it to a floor, whose semantics `ask_floor_preserves_no_consent_error_semantics`
    /// pins).
    #[test]
    fn ask_loop_non_convergence_floors_and_errors_propagate() {
        let db = tmp_db();
        let cfg = AppConfig::default();
        let unlocked = Mutex::new(HashSet::new());
        let exec = ask_executor(&db, &unlocked, &cfg);

        // Script exhaustion yields an empty answer → the loop bails without converging.
        let stuck = ScriptReasoner::ok(vec![
            serde_json::json!({ "tool": "search_meetings", "args": { "query": "a" } }),
        ]);
        let out = ask_vault_loop(&stuck, &exec, &db, &unlocked, "q", &[], "", None).unwrap();
        assert!(
            out.is_none(),
            "non-convergence must return Ok(None) for the command to floor"
        );

        struct Refuses;
        impl LocalReasoner for Refuses {
            fn id(&self) -> &str {
                "refuses"
            }
            fn reason(&self, _s: &str, _u: &str) -> crate::error::Result<String> {
                Ok("unused".into())
            }
            fn structured(
                &self,
                _s: &str,
                _u: &str,
                _schema: &Value,
            ) -> crate::error::Result<Value> {
                Err(AppError::Unavailable("no consent".into()))
            }
        }
        let res = ask_vault_loop(&Refuses, &exec, &db, &unlocked, "q", &[], "", None);
        assert!(
            matches!(res, Err(AppError::Unavailable(_))),
            "a loop error must propagate so the attempt wrapper can floor: {res:?}"
        );
    }

    /// The 12-message history discipline (CHAT_CONTEXT_TURNS) now applies to Ask: a 13th message
    /// is dropped from both the capped slice and the rendered conversation. RED vs the pre-change
    /// code, which rendered the history uncapped.
    #[test]
    fn ask_history_cap_enforced() {
        let msgs: Vec<ChatTurn> = (0..13)
            .map(|i| ChatTurn {
                role: "user".into(),
                content: format!("turn-{i}-text"),
            })
            .collect();
        let capped = capped_ask_history(&msgs);
        assert_eq!(capped.len(), CHAT_CONTEXT_TURNS);
        assert_eq!(
            capped[0].content, "turn-1-text",
            "the oldest message beyond the cap is dropped"
        );
        let rendered = crate::summarize::vault_chat::render_conversation(capped, "final question");
        assert!(
            !rendered.contains("turn-0-text"),
            "turn beyond the cap must not render"
        );
        assert!(
            rendered.contains("turn-12-text"),
            "the newest capped turn renders"
        );
        assert!(
            rendered.trim_end().ends_with("Assistant:"),
            "render keeps the completion cue"
        );

        // A short history passes through untouched.
        assert_eq!(capped_ask_history(&msgs[..3]).len(), 3);
    }

    /// SURFACE SPLIT: the vault executor must NOT advertise `propose_note` (the Ask page has no
    /// notes flow / Accept affordance) and must REFUSE to run it (the allowlist fails closed);
    /// the in-meeting executor (note_drafts: true) still advertises it. RED-able: drop the
    /// `"propose_note" => self.note_drafts` filter arm and the first assertion fails.
    #[test]
    fn propose_note_hidden_on_ask_surface_but_kept_in_meeting() {
        let db = tmp_db();
        let cfg = AppConfig::default();
        let unlocked = Mutex::new(HashSet::new());

        let vault = ask_executor(&db, &unlocked, &cfg);
        let names: Vec<&str> = vault.specs().iter().map(|s| s.name).collect();
        assert!(
            !names.contains(&"propose_note"),
            "the Ask surface must not advertise propose_note: {names:?}"
        );
        let res = vault.run("propose_note", &serde_json::json!({ "content": "draft" }));
        assert!(
            matches!(res, Err(AppError::InvalidArg(_))),
            "an un-advertised propose_note must be refused by the allowlist: {res:?}"
        );

        let in_meeting = crate::tools::GatedToolExecutor {
            db: &db,
            unlocked: &unlocked,
            config: &cfg,
            meeting_id: "live1",
            app: None,
            allow_writes: false,
            note_drafts: true,
            proposed_note: Mutex::new(None),
        };
        assert!(
            in_meeting.specs().iter().any(|s| s.name == "propose_note"),
            "the in-meeting surface keeps propose_note advertised"
        );
    }

    /// LOCK INVARIANT at the Ask surface: a scripted brain that tries to exfiltrate a
    /// sealed-not-unlocked meeting through the vault executor surfaces NOTHING sealed — not in the
    /// direct tool reads, not in the DTO's citations, not in the resolved `sources` (the
    /// citation→source resolver applies the same visibility predicate and only resolves once the
    /// folder is session-unlocked).
    #[test]
    fn ask_loop_never_surfaces_sealed_content() {
        let db = tmp_db();
        seed_note(
            &db,
            "open1",
            "Atlas Kickoff",
            "We decided to ship atlas on Friday.",
            None,
        );
        seed_sealed(&db, "sealed1", "fsec", "Atlas Secret Terms");

        // Seed self-check: the fixture must be sealed-not-unlocked BEFORE we prove the gate.
        let nothing = HashSet::new();
        assert!(db.meeting_is_visible("open1", &nothing).unwrap());
        assert!(
            !db.meeting_is_visible("sealed1", &nothing).unwrap(),
            "seed fixture: sealed1 must be gated"
        );

        let cfg = AppConfig::default();
        let unlocked = Mutex::new(HashSet::new());
        let exec = ask_executor(&db, &unlocked, &cfg);

        // Direct gate proof on THIS surface's executor shape.
        let got = exec
            .run(
                "get_meeting",
                &serde_json::json!({ "meetingId": "sealed1" }),
            )
            .unwrap();
        assert!(
            got.starts_with("No data"),
            "sealed fetch must be gated: {got}"
        );

        // The full loop, driven by an exfiltrating script.
        let brain = ScriptReasoner::ok(vec![
            serde_json::json!({ "tool": "get_meeting", "args": { "meetingId": "sealed1" } }),
            serde_json::json!({ "tool": "search_meetings", "args": { "query": "Atlas Secret Terms" } }),
            serde_json::json!({ "answer": "Here is what I found." }),
        ]);
        let out = ask_vault_loop(
            &brain,
            &exec,
            &db,
            &unlocked,
            "the secret terms?",
            &[],
            "",
            None,
        )
        .unwrap()
        .expect("converged");
        assert!(
            out.citations.iter().all(|c| !c.contains("Secret")),
            "sealed title must never be cited: {:?}",
            out.citations
        );
        assert!(
            out.sources.iter().all(|s| s.meeting_id != "sealed1"),
            "sealed meeting must never resolve into sources: {:?}",
            out.sources
        );

        // The resolver itself is gated: the sealed title resolves ONLY once session-unlocked.
        assert!(
            db.meeting_by_title_visible("Atlas Secret Terms", &nothing)
                .unwrap()
                .is_none(),
            "sealed-not-unlocked title must not resolve"
        );
        let mut open = HashSet::new();
        open.insert("fsec".to_string());
        assert_eq!(
            db.meeting_by_title_visible("Atlas Secret Terms", &open)
                .unwrap()
                .expect("session-unlocked title resolves")
                .id,
            "sealed1"
        );
    }

    /// The Ask trace stream is its OWN event — record-screen stores must never see Ask chips.
    #[test]
    fn ask_tool_event_is_distinct() {
        assert_ne!(
            crate::events::EVENT_ASK_TOOL,
            crate::events::EVENT_ASSISTANT_TOOL
        );
        assert_ne!(
            crate::events::EVENT_ASK_TOOL,
            crate::events::EVENT_CHAT_TOOL
        );
    }
}

/// Entity DOSSIER (brain2 Phase 5b): synthesize the "state of [[entity]]" across all meetings —
/// Overview · 🕑 Timeline of mentions · ⏳ Open commitments · 🧭 Last said / next step, every claim
/// citing its [[Title]]. `entity` is an entity id (from `get_graph`) OR a name. The dossier data is
/// assembled through the SAME visibility gate as Ask-My-Vault (sealed-not-unlocked meetings
/// contribute nothing), then synthesized by the configured provider — so this is a CLOUD-egress
/// path that goes through the redaction firewall + consent gate (E6/E7/E10) exactly like `ask_vault`.
#[tauri::command]
pub async fn entity_dossier(
    state: State<'_, AppState>,
    entity: String,
) -> Result<String, AppError> {
    if entity.trim().is_empty() {
        return Err(AppError::InvalidArg("entity is empty".into()));
    }
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
    let entity_id = crate::summarize::dossier::resolve_entity_id(&state.db, &entity, &unlocked)?
        .ok_or_else(|| AppError::InvalidArg(format!("no visible entity matching \"{entity}\"")))?;
    let data = crate::summarize::dossier::build_dossier_data(&state.db, &entity_id, &unlocked)?
        .ok_or_else(|| AppError::InvalidArg(format!("no visible entity matching \"{entity}\"")))?;
    // Build the provider (firewall + consent gate) BEFORE synthesizing — the factory refuses a
    // cloud provider until the user has consented to egress. NOTES role (the dossier is a
    // written synthesis); the corpus budget keys on the same resolved connection.
    let provider = crate::summarize::provider_for(crate::summarize::roles::Role::Notes, &config)?;
    let notes_conn =
        crate::summarize::roles::provider_target(crate::summarize::roles::Role::Notes, &config)
            .connection;
    let system = crate::summarize::dossier::dossier_system_prompt(&config.note_language);
    let user = crate::summarize::dossier::render_dossier_user(&data, &notes_conn);
    provider.complete(&system, &user).await
}

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
    let provider = crate::summarize::provider_for(crate::summarize::roles::Role::Notes, &config)?;
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

/// Export a meeting as an Obsidian Canvas (.canvas) — a spatial board of its topic spans.
/// Requires the timeline (open the meeting once). Returns the written path.
#[tauri::command]
pub fn export_canvas(state: State<'_, AppState>, meeting_id: String) -> Result<String, AppError> {
    // D4 READ-GATE: a sealed-and-not-unlocked meeting's timeline is blanked; refuse to build a
    // canvas from it. Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to export the canvas".into(),
        ));
    }
    let meeting = state
        .db
        .get_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no meeting with id {meeting_id}")))?;
    let json = state.db.get_timeline_data(&meeting_id)?.ok_or_else(|| {
        AppError::InvalidArg("open the meeting once to generate its timeline first".into())
    })?;
    let mut tl: MeetingTimeline = serde_json::from_str(&json)
        .map_err(|e| AppError::InvalidArg(format!("bad timeline data: {e}")))?;
    // Same coverage-repair the Detail view applies (heal a legacy cache that ends short of the
    // recording) so the exported canvas spans the meeting instead of the provider's early cluster.
    let segments = state.db.get_segments(&meeting_id)?;
    crate::summarize::timeline::repair_coverage(&mut tl, &segments);
    let title = meeting.title.unwrap_or_else(|| "Meeting".to_string());
    let topics: Vec<(String, f64, f64)> = tl
        .topics
        .iter()
        .map(|t| (t.label.clone(), t.start_s, t.end_s))
        .collect();
    let canvas = crate::export::canvas::build_canvas(&title, &topics);
    let vault = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .vault_path
            .clone()
    }
    .filter(|p| !p.is_empty())
    .ok_or_else(|| AppError::InvalidArg("set a vault folder in Settings first".into()))?;
    let vault_root = std::path::Path::new(&vault);
    // D5: the Canvas dir must resolve inside the vault root.
    let dir = assert_in_vault(vault_root, std::path::Path::new("Canvas"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::Export(format!("create Canvas dir failed: {e}")))?;
    let fname: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' | '^' | '[' | ']' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let fname = if fname.is_empty() {
        "Meeting".to_string()
    } else {
        fname
    };
    // D5: re-assert the final file path stays inside the vault (fname is sanitized, but bind the
    // guarantee at the write site).
    let path = assert_in_vault(
        vault_root,
        &std::path::Path::new("Canvas").join(format!("{fname}.canvas")),
    )?;
    std::fs::write(&path, canvas)
        .map_err(|e| AppError::Export(format!("write canvas failed: {e}")))?;
    Ok(path.to_string_lossy().to_string())
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

/// E10 — grant the one-time cloud-egress consent. This is the ONLY supported way to flip
/// `cloud_egress_consented` true: it persists the flag AND updates the in-memory config cache, so
/// the next `make_provider(claude_code|anthropic)` is allowed to build. Idempotent.
///
/// The FE calls this from its first-cloud-send confirmation dialog. Until the user confirms, every
/// cloud summarize/chat returns `AppError::Unavailable("cloud egress not consented …")`, which the
/// FE detects and surfaces as the consent prompt.
#[tauri::command]
pub fn consent_to_cloud_egress(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cache.grant_cloud_egress_consent(&state.db)?;
    Ok(())
}

/// E10 — REVOKE the cloud-egress consent (the AI-settings privacy strip). Mirror of
/// [`consent_to_cloud_egress`] and the ONLY supported way to flip `cloud_egress_consented` false:
/// it persists the flag AND updates the in-memory config cache, so the NEXT
/// `make_provider(claude_code|anthropic|gateway)` / cloud-reasoner call is refused fail-closed
/// (`AppError::Unavailable`) — the gate re-reads the live config per call, no restart needed.
/// Idempotent; a settings save can still neither grant nor revoke (the DTO stays preserve-only).
#[tauri::command]
pub fn revoke_cloud_egress(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cache.revoke_cloud_egress(&state.db)?;
    Ok(())
}

/// brain2 connectors — grant the one-time WEB SEARCH egress consent. The web connector reaches an
/// EXTERNAL service (a NEW EGRESS CLASS): the redacted query leaves the device. This is the ONLY
/// supported way to flip `web_search_consented` true; it persists the flag AND updates the in-memory
/// config cache, so the next `ConnectorRegistry::build` exposes the web tool (provided web search is
/// also enabled and a key is stored). Idempotent. Until granted, the web connector is absent from the
/// brain's tool registry and the redacted query never leaves the device.
#[tauri::command]
pub fn consent_to_web_search(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cache.grant_web_search_consent(&state.db)?;
    Ok(())
}

/// One-time Jira egress consent — the ONLY way `jira_consented` flips true. Persists the flag AND
/// updates the in-memory config cache, so the next `ConnectorRegistry::build` exposes the jira tool
/// (provided Jira is also enabled + configured + a token is stored). Idempotent.
#[tauri::command]
pub fn consent_to_jira(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cache.grant_jira_consent(&state.db)?;
    Ok(())
}

/// Store/replace the BYO Jira API token in the Keychain (account "jira_api_token"). An empty input
/// clears it. NEVER logged, NEVER returned to the FE — only `has_*` reports presence.
#[tauri::command]
pub fn set_jira_token(key: String) -> Result<(), AppError> {
    if key.trim().is_empty() {
        return secrets::delete_secret(crate::connectors::jira::JIRA_TOKEN_ACCOUNT);
    }
    secrets::set_secret(crate::connectors::jira::JIRA_TOKEN_ACCOUNT, key.trim())
}

/// Whether a Jira token is currently stored (UI shows "set"/"not set"; never the value).
#[tauri::command]
pub fn has_jira_token() -> Result<bool, AppError> {
    Ok(
        secrets::get_secret(crate::connectors::jira::JIRA_TOKEN_ACCOUNT)?
            .filter(|k| !k.trim().is_empty())
            .is_some(),
    )
}

/// One-time Slack egress consent — the ONLY way `slack_consented` flips true. Persists the flag AND
/// updates the in-memory config cache, so the next `ConnectorRegistry::build` exposes the slack tool
/// (provided Slack is also enabled + a token is stored). Idempotent.
#[tauri::command]
pub fn consent_to_slack(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cache.grant_slack_consent(&state.db)?;
    Ok(())
}

/// Store/replace the BYO Slack user token in the Keychain (account "slack_user_token"). An empty
/// input clears it. NEVER logged, NEVER returned to the FE — only `has_*` reports presence.
#[tauri::command]
pub fn set_slack_token(key: String) -> Result<(), AppError> {
    if key.trim().is_empty() {
        return secrets::delete_secret(crate::connectors::slack::SLACK_TOKEN_ACCOUNT);
    }
    secrets::set_secret(crate::connectors::slack::SLACK_TOKEN_ACCOUNT, key.trim())
}

/// Whether a Slack token is currently stored (UI shows "set"/"not set"; never the value).
#[tauri::command]
pub fn has_slack_token() -> Result<bool, AppError> {
    Ok(
        secrets::get_secret(crate::connectors::slack::SLACK_TOKEN_ACCOUNT)?
            .filter(|k| !k.trim().is_empty())
            .is_some(),
    )
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
        voice_trigger: c.voice_trigger,
        onboarded: c.onboarded,
        note_style: c.note_style.clone(),
        notes_mode: c.notes_mode.clone(),
        auto_organize: c.auto_organize,
        note_language: c.note_language.clone(),
        mcp_require_token: c.mcp_require_token,
        lock_require_biometric: c.lock_require_biometric,
        relock_on_screenshare: c.relock_on_screenshare,
        cloud_egress_consented: c.cloud_egress_consented,
        // DISPLAY-ONLY out (M3-CLIENT): FE shows share-egress consent status; cannot set it back.
        share_egress_consented: c.share_egress_consented,
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
            // Mirror AppConfig::default().model_size — an empty/blank choice from the FE must
            // fall back to the multilingual large-v3 default (best Polish quality), NOT a
            // smaller model that would silently downgrade transcription.
            AppConfig::default().model_size
        } else {
            d.model_size
        },
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
        // Sharing-onboarding gate: the first-run choice latch is NEVER set from the DTO — preserve the
        // live value. Only `mark_sharing_choice_made` may flip it, so a settings save can never set or
        // clear it (a re-save from an older FE that omits the key can't accidentally reopen the gate).
        sharing_choice_made: current.sharing_choice_made,
        // brain2 RAG: the semantic-search master flag IS carried on the settings DTO (the Settings
        // UI owns the toggle). Plain bool; an omitted value already defaulted to OFF on the DTO
        // (`#[serde(default)]`), so a partial/older save can never silently enable it. Unlike
        // `cloud_egress_consented` (preserved-only), this one is settable.
        semantic_search_enabled: d.semantic_search_enabled,
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
        // Tier 3b (B) grounding: NOT yet carried on the settings DTO (the FE toggle is a follow-up),
        // so PRESERVE the live value here — a normal settings save can neither enable nor clear it,
        // and it round-trips through the dedicated K_GROUND_SUMMARY load/save keys. Mirrors the
        // preserve-only discipline used for consent + embedder id.
        ground_summary: current.ground_summary,
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

/// List available microphone input devices for the FE picker (name + default flag).
#[tauri::command]
pub fn list_input_devices() -> Result<Vec<crate::audio::InputDeviceInfo>, AppError> {
    Ok(crate::audio::list_input_devices())
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageReportDto {
    pub audio_dir: String,
    pub used_bytes: u64,
    pub limit_bytes: Option<u64>,
    pub playback_bytes: u64,
    pub masters_bytes: u64,
    pub sealed_bytes: u64,
    pub recording_count: u64,
    pub auto_prune: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PruneSummaryDto {
    pub freed_bytes: u64,
    pub pruned_count: u64,
    pub masters_deleted: u64,
}

/// Recording-storage usage report: on-disk audio path, byte totals bucketed by category,
/// recording count, and the current cap + auto-prune flag. Sizes only — no content.
#[tauri::command]
pub fn get_storage_report(state: State<'_, AppState>) -> Result<StorageReportDto, AppError> {
    let (limit_bytes, auto_prune) = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        (
            c.audio_storage_limit_gb
                .map(|g| g as u64 * crate::storage::usage::BYTES_PER_GB),
            c.audio_auto_prune,
        )
    };
    let dir = crate::pipeline::audio_dir()?;
    let u = crate::storage::usage::scan_audio_usage(&dir)?;
    Ok(StorageReportDto {
        audio_dir: dir.to_string_lossy().into_owned(),
        used_bytes: u.used_bytes,
        limit_bytes,
        playback_bytes: u.playback_bytes,
        masters_bytes: u.masters_bytes,
        sealed_bytes: u.sealed_bytes,
        recording_count: u.recording_count,
        auto_prune,
    })
}

/// Manual "Free up space": prune oldest recordings to the cap NOW (works even when auto-prune
/// is off). Requires a cap — with none set it is an inert zero summary (the FE disables the
/// button). Never touches notes or locked audio.
#[tauri::command]
pub fn free_up_space(state: State<'_, AppState>) -> Result<PruneSummaryDto, AppError> {
    let limit_bytes = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        c.audio_storage_limit_gb
            // `Some(0)` is not a "delete everything" cap → no cap (mirrors `AppConfig::load`).
            .filter(|g| *g > 0)
            .map(|g| g as u64 * crate::storage::usage::BYTES_PER_GB)
    };
    let Some(limit) = limit_bytes else {
        return Ok(PruneSummaryDto {
            freed_bytes: 0,
            pruned_count: 0,
            masters_deleted: 0,
        });
    };
    let dir = crate::pipeline::audio_dir()?;
    // Hold the seal lifecycle guard across the prune so it can never interleave with a folder
    // seal (`lock_folder`) — the same guard every other multi-step audio-path mutator holds.
    // Acquired AFTER the config lock is released (single lock order: lifecycle ⊃ db, never
    // config held while holding lifecycle).
    let _lifecycle = lifecycle_guard(state.inner());
    let s = crate::storage::usage::prune_to_limit(&state.db, &dir, limit, None)?;
    Ok(PruneSummaryDto {
        freed_bytes: s.freed_bytes,
        pruned_count: s.pruned_count,
        masters_deleted: s.masters_deleted,
    })
}

/// Reveal the recordings folder in Finder (macOS `open`). No content read.
#[tauri::command]
pub fn reveal_audio_dir() -> Result<(), AppError> {
    let dir = crate::pipeline::audio_dir()?;
    std::process::Command::new("open")
        .arg(&dir)
        .spawn()
        .map_err(|e| AppError::Storage(format!("reveal audio dir: {e}")))?;
    Ok(())
}

/// Whether the CURRENT default audio output is the built-in speakers (echo risk while
/// capturing system audio). Best-effort introspection — `None` when undeterminable.
#[tauri::command]
pub fn output_is_builtin_speakers() -> Result<Option<bool>, AppError> {
    Ok(crate::audio::output::default_output_is_builtin_speakers())
}

/// Store/replace the Anthropic API key in Keychain (account "anthropic_api_key").
#[tauri::command]
pub fn set_anthropic_key(key: String) -> Result<(), AppError> {
    if key.trim().is_empty() {
        // Empty input clears the stored key.
        return secrets::delete_secret(ANTHROPIC_KEY_ACCOUNT);
    }
    secrets::set_secret(ANTHROPIC_KEY_ACCOUNT, &key)
}

/// Whether an Anthropic key is currently stored (UI shows "set"/"not set"; never the value).
#[tauri::command]
pub fn has_anthropic_key() -> Result<bool, AppError> {
    Ok(secrets::get_secret(ANTHROPIC_KEY_ACCOUNT)?.is_some())
}

/// Store/replace the AI Gateway API key in Keychain (account "gateway_api_key").
/// An empty/blank key is rejected — call `clear_gateway_key` to remove an existing key.
/// The key is NEVER logged and NEVER returned to the FE — only `has_gateway_key` reports presence.
/// Uses a SEPARATE keychain account from the Anthropic key (R3 — no cross-provider fallback).
#[tauri::command]
pub fn set_gateway_key(key: String) -> Result<(), AppError> {
    if key.trim().is_empty() {
        return Err(AppError::InvalidArg(
            "gateway API key must not be empty; use clear_gateway_key to remove an existing key"
                .into(),
        ));
    }
    secrets::set_secret(GATEWAY_KEY_ACCOUNT, key.trim())
}

/// Whether an AI Gateway key is currently stored (UI shows "set"/"not set"; never the value).
#[tauri::command]
pub fn has_gateway_key() -> Result<bool, AppError> {
    Ok(secrets::get_secret(GATEWAY_KEY_ACCOUNT)?
        .filter(|k| !k.trim().is_empty())
        .is_some())
}

/// Remove the stored AI Gateway API key from the Keychain.
/// Idempotent — no error if no key is stored. Mirrors `set_anthropic_key("")` semantics.
#[tauri::command]
pub fn clear_gateway_key() -> Result<(), AppError> {
    secrets::delete_secret(GATEWAY_KEY_ACCOUNT)
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

/// Per-model token-usage roll-up for `EgressLedger.byModel`.
///
/// Fields are `u64` so a large all-time cumulative sum (mirroring `EgressModelUsage`) cannot
/// silently wrap. JavaScript `number` (f64) handles these values without precision loss up to
/// 2^53 (~9 petaTokens), which is far beyond any realistic usage window.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsageDto {
    pub model: String,
    pub calls: u64,
    pub tokens: u64,
}

/// Per-day token-usage roll-up for `EgressLedger.byDay`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayUsageDto {
    /// ISO-8601 date string ("YYYY-MM-DD") in UTC.
    pub day: String,
    pub tokens: u64,
}

/// Redaction-count totals for the queried window.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedactionTotalsDto {
    pub email: u64,
    pub card: u64,
    pub phone: u64,
    pub name: u64,
}

/// One row from the `egress_log` table (content-free: counts + ids only).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EgressRowDto {
    /// Unix epoch (seconds) of the call.
    pub ts: i64,
    pub provider_id: String,
    pub destination: String,
    pub model_served: Option<String>,
    pub total_tokens: Option<u32>,
    pub redactions: RedactionTotalsDto,
}

/// Aggregated egress ledger for a rolling window (`days` days back from now).
///
/// Shape matches `EgressLedger` in `src/app/core/models.ts` (camelCase).
/// Every aggregate handles an empty `egress_log` gracefully — totals are zero, vecs empty.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EgressLedgerDto {
    pub total_calls: u64,
    pub total_tokens: u64,
    pub by_model: Vec<ModelUsageDto>,
    pub by_day: Vec<DayUsageDto>,
    pub total_redactions: RedactionTotalsDto,
    /// Last ≤20 rows from `egress_log`, newest first.
    pub recent: Vec<EgressRowDto>,
}

/// Aggregate the content-free `egress_log` table for the given rolling window and return the
/// ledger for the "Egress & Usage" Analytics panel.
///
/// `days` is the window width; pass `30` for the default 30-day view. The window is computed as
/// `ts >= (now_unix - days * 86400)`. An empty table (no cloud calls yet) returns all-zero totals
/// and empty vecs — never an error.
///
/// Read-only: queries `egress_log` only. No content columns are touched.
#[tauri::command]
pub fn get_egress_ledger(
    days: i64,
    state: State<'_, AppState>,
) -> Result<EgressLedgerDto, AppError> {
    let ledger = state.db.egress_summary(days)?;
    Ok(EgressLedgerDto {
        total_calls: ledger.total_calls,
        total_tokens: ledger.total_tokens,
        by_model: ledger
            .by_model
            .into_iter()
            .map(|m| ModelUsageDto {
                model: m.model,
                calls: m.calls,
                tokens: m.tokens,
            })
            .collect(),
        by_day: ledger
            .by_day
            .into_iter()
            .map(|d| DayUsageDto {
                day: d.day,
                tokens: d.tokens,
            })
            .collect(),
        total_redactions: RedactionTotalsDto {
            email: ledger.total_redactions.email,
            card: ledger.total_redactions.card,
            phone: ledger.total_redactions.phone,
            name: ledger.total_redactions.name,
        },
        recent: ledger
            .recent
            .into_iter()
            .map(|r| EgressRowDto {
                ts: r.ts,
                provider_id: r.provider_id,
                destination: r.destination,
                model_served: r.model_served,
                total_tokens: r.total_tokens,
                redactions: RedactionTotalsDto {
                    email: r.redactions.email,
                    card: r.redactions.card,
                    phone: r.redactions.phone,
                    name: r.redactions.name,
                },
            })
            .collect(),
    })
}

/// Store/replace the BYO web-search (Brave) API key in the Keychain (account "web_search_api_key").
/// An empty input clears it. The key is NEVER logged and NEVER returned to the FE — only `has_*`
/// reports presence. Mirrors `set_anthropic_key`.
#[tauri::command]
pub fn set_web_search_api_key(key: String) -> Result<(), AppError> {
    if key.trim().is_empty() {
        return secrets::delete_secret(crate::connectors::web::WEB_SEARCH_KEY_ACCOUNT);
    }
    secrets::set_secret(crate::connectors::web::WEB_SEARCH_KEY_ACCOUNT, key.trim())
}

/// Whether a web-search API key is currently stored (UI shows "set"/"not set"; never the value).
#[tauri::command]
pub fn has_web_search_key() -> Result<bool, AppError> {
    Ok(
        secrets::get_secret(crate::connectors::web::WEB_SEARCH_KEY_ACCOUNT)?
            .filter(|k| !k.trim().is_empty())
            .is_some(),
    )
}

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
    let result = pipeline::resummarize_existing(&app, &state, &meeting_id).await?;
    Ok(StopResult {
        meeting_id: result.meeting_id,
        markdown: result.note_markdown,
        exported_path: result
            .exported_path
            .map(|p| p.to_string_lossy().to_string()),
    })
}

/// Recent meetings for the Library list (newest first, capped). Sealed-and-not-session-unlocked
/// meetings are MASKED at the backend before the DTO crosses IPC (see [`mask_locked_meetings`]) —
/// the Library lock gate is enforced in code here, never trusted to the FE.
#[tauri::command]
pub fn list_meetings(state: State<'_, AppState>) -> Result<Vec<Meeting>, AppError> {
    let meetings = state.db.list_meetings(200)?;
    mask_locked_meetings(state.inner(), meetings)
}

/// Backend-mask sealed-not-session-unlocked meetings in a Library list, mirroring [`masked_detail`]:
/// a meeting whose folder is locked (`folders.locked = 1`) AND NOT in the current session unlock set
/// gets its real AI title replaced by the "🔒 Locked" placeholder and its `.enc` `audio_path` nulled
/// (so nothing can feed `convertFileSrc` / the `asset:` protocol for a locked recording). The row +
/// its `folder_id` are PRESERVED so the FE still renders the inline lock badge (it keys the badge off
/// `folder_id` + the folder's exposure). The lock decision routes through the session unlock set +
/// `locked_folder_ids` (the same source the `*_visible` reads use) — NOT the FE.
fn mask_locked_meetings(
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

/// Aggregate analytics for the dashboard + Analytics tab.
#[tauri::command]
pub fn get_analytics(state: State<'_, AppState>) -> Result<Analytics, AppError> {
    state.db.analytics()
}

/// Rename a speaker across a meeting's cached timeline (e.g. "User 1" → "Sarah"). Persists to
/// the timelines cache and returns the updated timeline.
#[tauri::command]
pub fn rename_speaker(
    state: State<'_, AppState>,
    meeting_id: String,
    old_label: String,
    new_label: String,
) -> Result<MeetingTimeline, AppError> {
    rename_speaker_inner(state.inner(), &meeting_id, &old_label, new_label.trim())
}

/// Inner of [`rename_speaker`] taking `&AppState` (unit-testable gate + enroll). `new_label` is
/// already trimmed by the command wrapper.
pub(crate) fn rename_speaker_inner(
    state: &AppState,
    meeting_id: &str,
    old_label: &str,
    new_label: &str,
) -> Result<MeetingTimeline, AppError> {
    if new_label.is_empty() {
        return Err(AppError::InvalidArg("new speaker name is empty".into()));
    }
    // BLK-2b WRITE-GATE: a sealed-and-not-unlocked meeting's timeline `data` is blanked; refuse to
    // rename a speaker (would persist a near-empty plaintext timeline over the sealed blob in a
    // locked folder). Fail closed.
    if !meeting_is_unlocked(state, meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to rename a speaker".into(),
        ));
    }
    let json = state
        .db
        .get_timeline_data(meeting_id)?
        .ok_or_else(|| AppError::InvalidArg("no timeline for this meeting yet".into()))?;
    let mut tl: crate::storage::models::MeetingTimeline = serde_json::from_str(&json)
        .map_err(|e| AppError::InvalidArg(format!("bad timeline data: {e}")))?;

    // Reconstruct the diarized CLUSTER for the OLD label BEFORE the rename rewrites it away. The FE
    // passes the DISPLAY label the lane shows ("Speaker 1"), not the raw `others-N` tag, so first try
    // the raw-tag parse (legacy / a raw-tag timeline), then fall back to segment↔turn overlap against
    // the still-original turns. A label with no overlapping diarized cluster (the "me" lane, or a
    // non-diarized meeting with no segments) → None → enroll nothing.
    let old_cluster = parse_others_cluster(old_label).or_else(|| {
        reconcile_meeting_speakers(state, meeting_id, Some(&tl.speakers)).cluster_for_label(old_label)
    });

    for turn in &mut tl.speakers {
        if turn.speaker == old_label {
            turn.speaker = new_label.to_string();
        }
    }
    let updated = serde_json::to_string(&tl)
        .map_err(|e| AppError::Storage(format!("serialize timeline: {e}")))?;
    state.db.set_timeline_data(meeting_id, &updated)?;

    // ENROLL-ON-RENAME (Phase 2, opt-in): if the OLD label resolves to a diarized cluster (either a
    // raw `others-{n}` tag or, via the reconciliation above, the display label the FE lane showed) and
    // the meeting produced a voiceprint for that cluster, bind the new person name to it so the next
    // meeting can re-identify this voice. Best-effort + no-op when: the opt-in is off, the label maps
    // to no cluster, or no voiceprint exists for that cluster (pre-opt-in recording). The rename itself
    // already succeeded regardless — a failed/absent enroll never fails the command. The WRITE is
    // anchored to THIS (already-unlocked) meeting; no other meeting's voiceprint is read/written.
    if let Some(cluster_index) = old_cluster {
        let enabled = state
            .config
            .lock()
            .map(|c| c.voiceprint_enabled)
            .unwrap_or(false);
        if enabled {
            match state
                .db
                .set_voiceprint_label_for_cluster(meeting_id, cluster_index, new_label)
            {
                Ok(n) => {
                    if n > 0 {
                        tracing::info!(
                            target: "transcribe", meeting_id = %meeting_id, cluster_index,
                            "enrolled a voiceprint on rename"
                        );
                    }
                }
                Err(e) => tracing::warn!(
                    target: "transcribe", error = %e,
                    "voiceprint enroll-on-rename failed (rename unaffected)"
                ),
            }
        }
    }
    Ok(tl)
}

/// Parse a diarized-cluster timeline label `others-{n}` → its cluster index, else None. The plain
/// `others` label (single remote speaker, no cluster suffix) and any human name return None.
fn parse_others_cluster(label: &str) -> Option<i64> {
    label
        .strip_prefix(crate::audio::merge::SPEAKER_OTHERS)?
        .strip_prefix('-')?
        .parse::<i64>()
        .ok()
}

// ── VOICEPRINTS (Phase 2): cosine re-identification + enroll + management ───────────────────────
//
// GATE DISCIPLINE (lock-model): every read here goes through `list_voiceprints_visible`, so a
// sealed-and-not-session-unlocked meeting's voiceprint is INVISIBLE — it is never listed, never a
// match candidate, and never a suggestion source. The suggester compares THIS meeting's clusters
// only against OTHER visible LABELED voiceprints; a sealed prior contributes nothing. The raw
// embedding never crosses the IPC boundary (the DTOs carry label + provenance + dim only).

/// Suggest a person label for each diarized `others-{n}` cluster of `meeting_id`, by cosine
/// re-identification against prior LABELED voiceprints. GATED: `meeting_is_unlocked` first (a locked
/// meeting yields no suggestions), then the candidate set is `list_voiceprints_visible` restricted to
/// labeled rows from OTHER meetings — a sealed prior is never in it. Only matches `>=`
/// `VOICEPRINT_MATCH_THRESHOLD` are returned. Empty when the opt-in is off, no voiceprint exists, or
/// nothing matches. NO PII is logged.
#[tauri::command]
pub fn suggest_speaker_labels(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<SpeakerSuggestion>, AppError> {
    suggest_speaker_labels_inner(state.inner(), &meeting_id)
}

/// Inner of [`suggest_speaker_labels`] taking `&AppState` (unit-testable gate).
pub(crate) fn suggest_speaker_labels_inner(
    state: &AppState,
    meeting_id: &str,
) -> Result<Vec<SpeakerSuggestion>, AppError> {
    use crate::transcribe::diarize::{
        suggest_voiceprint_labels, ClusterEmbeddingRef, LabeledEmbeddingRef,
        VOICEPRINT_MATCH_THRESHOLD,
    };
    // READ-GATE: a locked meeting surfaces nothing (its own clusters are invisible anyway, but fail
    // closed explicitly).
    if !meeting_is_unlocked(state, meeting_id)? {
        return Ok(Vec::new());
    }
    // The whole VISIBLE voiceprint corpus (sealed priors already excluded by the visibility clause).
    let unlocked = unlocked_snapshot(state)?;
    let all = state.db.list_voiceprints_visible(&unlocked)?;

    // THIS meeting's clusters (candidates to label) vs OTHER meetings' LABELED prints (the gallery).
    let mine: Vec<_> = all.iter().filter(|v| v.meeting_id == meeting_id).collect();
    if mine.is_empty() {
        return Ok(Vec::new());
    }
    let labeled_refs: Vec<LabeledEmbeddingRef<'_>> = all
        .iter()
        .filter(|v| v.meeting_id != meeting_id)
        .filter_map(|v| {
            v.label
                .as_deref()
                .filter(|l| !l.trim().is_empty())
                .map(|label| LabeledEmbeddingRef {
                    label,
                    embedding: &v.embedding,
                })
        })
        .collect();
    if labeled_refs.is_empty() {
        return Ok(Vec::new());
    }
    let cluster_refs: Vec<ClusterEmbeddingRef<'_>> = mine
        .iter()
        // Only suggest for clusters that are NOT already labeled in this meeting.
        .filter(|v| {
            v.label
                .as_deref()
                .map(|l| l.trim().is_empty())
                .unwrap_or(true)
        })
        .map(|v| ClusterEmbeddingRef {
            cluster_index: v.cluster_index as i32,
            embedding: &v.embedding,
        })
        .collect();

    let suggestions =
        suggest_voiceprint_labels(&cluster_refs, &labeled_refs, VOICEPRINT_MATCH_THRESHOLD);

    // RE-KEY by the DISPLAY label the FE lane actually shows: the timeline is LLM-generated, so lane
    // `speaker` = "Speaker 1"/a real name, NOT the raw `others-N` tag. Reconcile the cluster → that
    // display label via segment↔turn time-overlap so `suggestionByLabel().get(lane.speaker)` matches
    // for both multi-cluster and single-cluster 1:1. Best-effort: if the meeting has no timeline / no
    // segments (legacy, sealed-then-unlocked), reconciliation yields nothing → fall back to the raw
    // `others-N` tag (harmless — a legacy raw-tag timeline still matches; an LLM one just won't chip).
    let reconciliation = reconcile_meeting_speakers(state, meeting_id, None);
    Ok(suggestions
        .into_iter()
        .map(|s| {
            let cluster = s.cluster_index as i64;
            let speaker = reconciliation
                .label_for_cluster(cluster)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    format!("{}-{}", crate::audio::merge::SPEAKER_OTHERS, s.cluster_index)
                });
            SpeakerSuggestion { speaker, suggested_label: s.label, score: s.score }
        })
        .collect())
}

/// Build the cluster↔display-label reconciliation for THIS meeting from segment↔turn time-overlap.
/// Best-effort + gated by the caller (both call sites first pass `meeting_is_unlocked`): reads ONLY
/// this meeting's segments + its stored (or supplied) timeline turns — never another meeting's data,
/// so an enroll can never reach a sealed/other cluster. A missing timeline or missing segments (a
/// legacy or sealed-then-unlocked meeting) yields an empty reconciliation → no-suggestion / no-enroll,
/// never an error, never a fabricated cluster. Pass `turns` when the caller already parsed the
/// timeline (avoids a redundant DB read); pass `None` to load it from the DB. NO PII is logged.
fn reconcile_meeting_speakers(
    state: &AppState,
    meeting_id: &str,
    turns: Option<&[crate::storage::models::SpeakerTurn]>,
) -> crate::transcribe::diarize::SpeakerReconciliation {
    use crate::transcribe::diarize::{reconcile_speakers, TurnRef};
    let segments = state.db.get_segments(meeting_id).unwrap_or_default();
    // Own the turns when we have to load them, so the borrow outlives the ref view below.
    let loaded: Vec<crate::storage::models::SpeakerTurn> = match turns {
        Some(_) => Vec::new(),
        None => match state.db.get_timeline_data(meeting_id) {
            Ok(Some(json)) => serde_json::from_str::<MeetingTimeline>(&json)
                .map(|t| t.speakers)
                .unwrap_or_default(),
            _ => Vec::new(),
        },
    };
    let turns = turns.unwrap_or(&loaded);
    let turn_refs: Vec<TurnRef<'_>> = turns
        .iter()
        .map(|t| TurnRef { start_s: t.start_s, end_s: t.end_s, label: &t.speaker })
        .collect();
    reconcile_speakers(&segments, &turn_refs)
}

/// List stored voiceprints for a management view (label + source meeting + cluster + dim), GATED —
/// a sealed-not-unlocked meeting's voiceprint is EXCLUDED. The raw embedding is NEVER returned.
#[tauri::command]
pub fn list_voiceprints(state: State<'_, AppState>) -> Result<Vec<VoiceprintInfo>, AppError> {
    list_voiceprints_inner(state.inner())
}

/// Inner of [`list_voiceprints`] taking `&AppState` (unit-testable gate).
pub(crate) fn list_voiceprints_inner(state: &AppState) -> Result<Vec<VoiceprintInfo>, AppError> {
    let unlocked = unlocked_snapshot(state)?;
    let rows = state.db.list_voiceprints_visible(&unlocked)?;
    Ok(rows
        .into_iter()
        .map(|v| VoiceprintInfo {
            id: v.id,
            meeting_id: v.meeting_id,
            cluster_index: v.cluster_index,
            label: v.label,
            dim: v.dim,
            created_at: v.created_at,
        })
        .collect())
}

/// FORGET one stored voiceprint by id (hard delete — a voice biometric the user chose to erase).
/// Idempotent. Content-free logging (the id only). Not itself a content READ, so no gate is needed
/// (a delete widens no visibility); the management list it feeds IS gated.
#[tauri::command]
pub fn forget_voiceprint(state: State<'_, AppState>, id: String) -> Result<(), AppError> {
    let removed = state.db.delete_voiceprint(&id)?;
    tracing::info!(target: "transcribe", voiceprint_id = %id, removed, "voiceprint forgotten");
    Ok(())
}

/// CLEAR every stored voiceprint (the "forget all captured voices" affordance). Content-free
/// logging (a count only).
#[tauri::command]
pub fn clear_voiceprints(state: State<'_, AppState>) -> Result<(), AppError> {
    let n = state.db.clear_voiceprints()?;
    tracing::info!(target: "transcribe", count = n, "all voiceprints cleared");
    Ok(())
}

/// Speaker + topic timeline for a meeting (AI-derived, cached after first generation).
#[tauri::command]
pub async fn get_timeline(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<MeetingTimeline, AppError> {
    // Phase 0.5 READ-GATE: a sealed-and-not-unlocked meeting returns an EMPTY timeline (its
    // `timelines.data` is blanked at rest while sealed, but mask explicitly + skip regeneration so
    // we never re-derive a timeline from now-blank segments).
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Ok(MeetingTimeline::default());
    }
    // Fetch segments up-front: they anchor the coverage-repair for BOTH a cached timeline (so a
    // legacy cache generated before the repair existed — e.g. one ending at 0:14 for a 0:45
    // recording — heals on read) and a freshly-generated one.
    let segments = state.db.get_segments(&meeting_id)?;
    if let Some(json) = state.db.get_timeline_data(&meeting_id)? {
        if let Ok(mut t) = serde_json::from_str::<MeetingTimeline>(&json) {
            crate::summarize::timeline::repair_coverage(&mut t, &segments);
            return Ok(t);
        }
    }
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
    let provider = crate::summarize::provider_for(crate::summarize::roles::Role::Notes, &config)?;
    let timeline =
        crate::summarize::timeline::generate(provider.as_ref(), &segments, duration_s).await?;
    if let Ok(json) = serde_json::to_string(&timeline) {
        let _ = state.db.set_timeline_data(&meeting_id, &json);
    }
    Ok(timeline)
}

/// A meeting + its latest note + transcript segments for the Detail view.
/// Returns `None` if the meeting id is unknown.
#[tauri::command]
pub fn get_meeting_detail(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Option<MeetingDetailDto>, AppError> {
    let Some(meeting) = state.db.get_meeting(&meeting_id)? else {
        return Ok(None);
    };

    // Phase 0.5 READ-GATE: a meeting in a locked-and-NOT-session-unlocked folder returns a MASKED
    // DTO — `locked: true`, no note, no segments. The plaintext columns are blanked at rest while
    // sealed (and the audio is encrypted), but we mask explicitly so the FE never shows the empty
    // shell as if it were real content, and so the title can be masked too.
    //
    // `audio_path` is NULLED here too: the FE feeds it straight into `convertFileSrc` (the Tauri
    // `asset:` protocol, scoped to the audio dir) which serves the file to the webview WITHOUT
    // touching the `export_audio` command — i.e. the only audio read path that does NOT pass
    // through `meeting_is_unlocked`. While sealed the on-disk file is the AES-GCM `.enc` (so even a
    // leaked path serves ciphertext), but we must not depend on that single invariant: nulling the
    // path here means the gate covers the asset protocol regardless of the on-disk seal state, so a
    // plaintext WAV that briefly survives in the scoped dir (e.g. recorded into an already-sealed
    // folder, or a crash window) can never be served to a locked meeting's view.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Ok(Some(masked_detail(meeting)));
    }

    let note_row = state.db.get_latest_note_for_meeting(&meeting_id)?;
    // Phase 5: capture provenance from the note row BEFORE converting to NoteDto (NoteDto is a
    // subset and doesn't carry model fields). All three are None when the note is absent or when
    // the provider did not record provenance (pre-Phase-5 notes).
    let ai_provider = note_row.as_ref().map(|n| n.provider_id.clone());
    let ai_model = note_row.as_ref().and_then(|n| n.model_requested.clone());
    let model_served = note_row.as_ref().and_then(|n| n.model_served.clone());
    let note = note_row.map(|n| NoteDto {
        meeting_id: n.meeting_id,
        provider_id: n.provider_id,
        markdown: n.markdown,
        exported_path: n.exported_path,
    });
    let segments = state.db.get_segments(&meeting_id)?;
    // GATED read: only past the `meeting_is_unlocked` gate above do we surface the persisted
    // assistant Q&A. The DB read is ALSO `visibility_clause`-gated (it returns empty for a sealed-
    // not-unlocked meeting) — defense-in-depth, double-gated exactly like the rest of the DTO.
    let unlocked = state
        .unlocked_folders
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    let assistant_interactions = state
        .db
        .list_assistant_interactions_visible(&meeting_id, &unlocked)?;
    Ok(Some(MeetingDetailDto {
        meeting,
        note,
        segments,
        assistant_interactions,
        locked: false,
        ai_provider,
        ai_model,
        model_served,
    }))
}

/// Build the MASKED detail DTO for a sealed-and-not-session-unlocked meeting. Pure (no DB / state)
/// so the read-gate's masking contract is unit-testable. EVERY content channel is closed:
/// - `title` → "🔒 Locked" (the real title lives in `meetings.title`, plaintext-at-rest);
/// - `audio_path` → `None` so the FE has nothing to hand `convertFileSrc` (the `asset:` protocol
///   serve path that bypasses the `export_audio` command + `meeting_is_unlocked` gate);
/// - `note` / `segments` → empty;
/// - `locked` → true so the FE renders the unlock affordance, not an empty shell.
fn masked_detail(meeting: Meeting) -> MeetingDetailDto {
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

/// Whether a usable Whisper model is present for the chosen size + language (or the
/// explicit configured path). Lets the UI auto-detect + offer a download when missing.
#[tauri::command]
pub fn model_present(state: State<'_, AppState>) -> Result<bool, AppError> {
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
    Ok(crate::transcribe::resolve_model_path(p, &size, &language)?.is_some())
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

/// Whether a usable on-device brain (reasoning GGUF) is present at the resolved path — the
/// configured custom `brain_model_path`, else the selected `brain_model_id`'s file in the shared
/// models dir. Lets the UI offer a download. Purely a file-on-disk check (mistralrs is always
/// compiled; the real brain activates on model presence).
#[tauri::command]
pub fn brain_model_present(state: State<'_, AppState>) -> Result<bool, AppError> {
    let (configured, selected) = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        (c.brain_model_path.clone(), c.brain_model_id.clone())
    };
    let p = configured.as_deref().map(std::path::Path::new);
    Ok(crate::reason::resolve_brain_model(p, selected.as_deref())?.is_some())
}

/// macOS total physical RAM in whole GB via `sysctl -n hw.memsize` (no new FFI/crate). Returns
/// `None` on any error — the caller then treats every model as fitting rather than HIDING it behind
/// a failed probe.
fn total_ram_gb() -> Option<u64> {
    let out = std::process::Command::new("sysctl")
        .arg("-n")
        .arg("hw.memsize")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let bytes: u64 = String::from_utf8(out.stdout).ok()?.trim().parse().ok()?;
    Some(bytes / (1024 * 1024 * 1024))
}

/// The curated on-device brain model registry, each row carrying the picker flags `downloaded`
/// (file present in the shared models dir), `fits_ram` (min RAM within the machine's total RAM —
/// `true` when total RAM can't be read), and `selected` (the persisted `brain_model_id`). Feeds the
/// Phase-H model picker. No content read / no egress — static metadata + on-disk existence only.
#[tauri::command]
pub fn list_brain_models(
    state: State<'_, AppState>,
) -> Result<Vec<crate::reason::BrainModelDto>, AppError> {
    let (selected, light, heavy) = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        (
            c.brain_model_id.clone(),
            c.brain_light_model_id.clone(),
            c.brain_heavy_model_id.clone(),
        )
    };
    let dir = crate::transcribe::models_dir()?;
    Ok(crate::reason::brain_model_dtos(
        &dir,
        total_ram_gb(),
        selected.as_deref(),
        light.as_deref(),
        heavy.as_deref(),
    ))
}

/// The installed-base migration nudge: `Some` when the persisted `brain_model_id` points at a RETIRED
/// model (e.g. the non-commercial `qwen2.5-3b`), telling the FE to offer the Apache-licensed
/// replacement. `None` for an active/absent selection. Read-only capability probe (no content, no
/// egress) — like [`brain_model_present`]. The retired GGUF keeps working until the user switches;
/// nothing is changed silently.
#[tauri::command]
pub fn brain_model_retirement_nudge(
    state: State<'_, AppState>,
) -> Result<Option<crate::reason::RetiredModelNudge>, AppError> {
    let selected = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        c.brain_model_id.clone()
    };
    let dir = crate::transcribe::models_dir()?;
    Ok(crate::reason::retired_model_nudge(selected.as_deref(), &dir))
}

/// The DERIVED Murmur Brain posture (spec §2.1) for the Settings display — computed from the live
/// config (resolved role targets + `brain_live`), NEVER stored. `custom` when the dispatch keys match
/// no preset, so the label can never lie about egress. Read-only capability probe (no content).
#[tauri::command]
pub fn brain_posture(state: State<'_, AppState>) -> Result<crate::settings::Posture, AppError> {
    let c = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    Ok(crate::settings::postures::derive_posture(&c))
}

/// Apply a Murmur Brain posture PRESET (`cloud` / `hybrid` / `fully_local`) and persist it. `custom`
/// is a derived-only label and is rejected (`InvalidArg`). This is the SINGLE writer of the posture
/// presets — the raw settings save preserves `brain_live` + the posture role keys (see
/// `dto_to_config`), so a partial save can never change the posture. The Hybrid preset deliberately
/// leaves `role_live_*` untouched (the @brain assistant stays intact).
#[tauri::command]
pub fn set_brain_posture(state: State<'_, AppState>, posture: String) -> Result<(), AppError> {
    let p = crate::settings::Posture::from_settable(&posture)
        .ok_or_else(|| AppError::InvalidArg(format!("not a settable posture: {posture}")))?;
    let mut c = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    crate::settings::postures::apply_posture(&mut c, p);
    c.save(&state.db)?;
    Ok(())
}

/// The RESOLVED "what runs where" map for the Settings AI page — one row per AI job with its
/// resolved engine/model/locality (mirrors `roles::resolve`; display-only, steers nothing).
/// Read-only config projection: no content, no PII, no keys — NOT a gated content read.
#[tauri::command]
pub fn resolved_ai_map(
    state: State<'_, AppState>,
) -> Result<Vec<crate::settings::ai_map::AiMapRow>, AppError> {
    let c = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    Ok(crate::settings::ai_map::ai_map_rows(&c))
}

/// Whether the machine has enough RAM to run Realtime Reactions (the light engine) alongside a live
/// recording — the combined-residency guard (spec §3.3), including a KV estimate + call-overhead. Lets
/// the Brain Live card warn / gate before enabling. `true` when total RAM can't be read (never block
/// behind a failed probe). Read-only capability probe.
#[tauri::command]
pub fn brain_live_ram_ok() -> Result<bool, AppError> {
    let light = crate::reason::default_model_for_class(crate::reason::ModelClass::Light);
    let models: Vec<&crate::reason::BrainModel> = light.into_iter().collect();
    Ok(crate::reason::residency_fits(
        &models,
        total_ram_gb(),
        crate::reason::CALL_OVERHEAD_GB,
    ))
}

/// The Realtime-Reactions SHADOW counter (spec §4.2): how many contradiction cards WOULD have fired
/// this recording while the sub-toggle is OFF. Lets the FE offer "the brain would have flagged N —
/// enable?" (user-local calibration, no telemetry). Read-only; resets each `start_recording`.
#[tauri::command]
pub fn brain_reactions_shadow_count(state: State<'_, AppState>) -> Result<u64, AppError> {
    Ok(state
        .reactions_shadow_count
        .load(std::sync::atomic::Ordering::Relaxed))
}

/// Flip the Realtime-Reactions CONTRADICTION-card sub-toggle (spec §4.2). Default OFF (shadow mode);
/// the FE offers this only once the user's OWN shadow count clears a bar. Dedicated command (not the
/// raw settings save, which preserves it) so a partial/older save can never silently enable ⚠ cards.
#[tauri::command]
pub fn set_brain_contradiction_cards(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), AppError> {
    let mut c = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    c.brain_contradiction_cards = enabled;
    c.save(&state.db)?;
    Ok(())
}

/// Persist the user's SELECTED on-device brain model id. Validates `model_id` against the registry
/// (unknown id ⇒ `AppError::InvalidArg`) and saves it to config; the reasoner dispatch
/// (`ReasonerCell`) re-resolves per call, so the model takes effect on the next reasoning call
/// once its GGUF is present — no restart. Does NOT download — the FE calls
/// `download_brain_model(model_id)` for that.
#[tauri::command]
pub fn select_brain_model(state: State<'_, AppState>, model_id: String) -> Result<(), AppError> {
    select_brain_model_inner(&state, model_id)
}

/// Testable core of [`select_brain_model`]: validate the id against the registry, persist it.
fn select_brain_model_inner(state: &AppState, model_id: String) -> Result<(), AppError> {
    let model = crate::reason::brain_model_by_id(&model_id)
        .ok_or_else(|| AppError::InvalidArg(format!("unknown brain model id: {model_id}")))?;
    let mut c = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    // Keep the legacy single-model id (BrainBackend::Local + custom-path fallback) …
    c.brain_model_id = Some(model_id.clone());
    // … AND wire the CLASS handle so the Brain-Live `light()` / Fully-Local `heavy()` handles actually
    // use what the user just selected. Without this, selecting a light model leaves `light()` pointing
    // at the registry DEFAULT (a different, un-downloaded GGUF) → it silently resolves to the stub and
    // Realtime Reactions never fire despite Brain Live being "on".
    match model.class {
        crate::reason::ModelClass::Light => c.brain_light_model_id = Some(model_id),
        crate::reason::ModelClass::Heavy => c.brain_heavy_model_id = Some(model_id),
    }
    c.save(&state.db)?;
    Ok(())
}

/// Download the on-device brain model identified by `model_id` (from the curated registry) into the
/// shared models dir if missing; returns its path. No-op (returns the existing path) when already
/// present. Unknown id ⇒ `AppError::InvalidArg`. INBOUND ONLY — fetches a model file and sends NO
/// meeting content (no egress). Emits [`crate::events::EVENT_BRAIN_DOWNLOAD`] progress events
/// (throttled). The downloaded file is NOT loaded here — wiring the brain is a later step.
/// Resolve a registry `model_id` to its `(download url, on-disk dest)`. Unknown id ⇒
/// `AppError::InvalidArg` (the rejection [`download_brain_model`] enforces). Testable sync core.
fn brain_download_target(model_id: &str) -> Result<(&'static str, std::path::PathBuf), AppError> {
    let model = crate::reason::brain_model_by_id(model_id)
        .ok_or_else(|| AppError::InvalidArg(format!("unknown brain model id: {model_id}")))?;
    Ok((
        model.url,
        crate::transcribe::models_dir()?.join(model.filename),
    ))
}

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

/// WS2 (EXPERIMENTAL) — availability of the on-device Apple Foundation Models sidecar
/// (`meetnotes-afm`): is it bundled, and (if so) does its on-device model report available? Lets the
/// FE offer the "Apple Intelligence (on-device)" brain option ONLY on macOS-26 Apple-Silicon
/// hardware where the native sidecar is present — never advertise it elsewhere. On every current
/// (non-macOS-26) build the sidecar is ABSENT, so this returns
/// `{sidecar_present:false, model_available:None}`.
///
/// Opens NO content-read path (a device-capability probe only, like [`brain_model_present`]), so no
/// `meeting_is_unlocked` / `visibility_clause` gate applies. NEVER panics, NEVER egresses (the probe
/// is a local `--probe` spawn; a missing/wedged sidecar degrades gracefully).
#[tauri::command]
pub fn afm_available(app: AppHandle) -> Result<crate::reason::afm::AfmStatus, AppError> {
    Ok(crate::reason::afm::probe(Some(&app)))
}

/// `true` when all three multilingual-e5-small files are present in the shared models dir's embed
/// sub-dir — i.e. the REAL embedder would load (candle is always compiled; it activates on model
/// presence). Cheap existence probe; NEVER errors on a missing models dir (treats it as "not present").
#[tauri::command]
pub fn embed_model_present() -> Result<bool, AppError> {
    Ok(crate::embed::embed_model_present())
}

/// The bundled selectable embedders (multilingual-e5-small default + mmlw-retrieval-e5-small), each with
/// `downloaded` (files present in its own subdir) and `selected` (mirrors the persisted
/// `embed_model_id`). Feeds the embedder picker. No content read / no egress — static metadata +
/// on-disk existence only.
#[tauri::command]
pub fn list_embed_models(
    state: State<'_, AppState>,
) -> Result<Vec<crate::embed::EmbedModelDto>, AppError> {
    let selected = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        c.embed_model_id.clone()
    };
    Ok(crate::embed::embed_model_dtos(selected.as_deref()))
}

/// Result of [`select_embed_model`]. `reindex_needed` is `true` when the selection actually CHANGED
/// the resolved model (so its old vectors are stale — a different model's embeddings are not
/// comparable): the FE should prompt the user to run `reindex_embeddings` (and download the new
/// model first via `download_embed_model` if `!embed_model_present()`). Counts/flags only — no PII.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectEmbedModelResult {
    pub selected: String,
    pub reindex_needed: bool,
    pub model_present: bool,
}

/// Persist the user's SELECTED on-device embedding model id. Validates `model_id` against the
/// registry (unknown ⇒ `AppError::InvalidArg`) and saves it to config; `AppConfig::save` republishes
/// the process-global selection so `embed::active_embedder`/`embed_model_present`/`download_embed_model`
/// pick up the new model with NO restart. Switching the model INVALIDATES existing vectors (a
/// different model's embeddings are not comparable), so `reindex_needed` is `true` when the resolved
/// model actually changed — the FE then prompts the user to download (if missing) + re-index. All
/// bundled options are BERT/384 ⇒ NO `vec0` schema migration. Does NOT download and does NOT auto-
/// reindex (both are explicit user actions).
#[tauri::command]
pub fn select_embed_model(
    state: State<'_, AppState>,
    model_id: String,
) -> Result<SelectEmbedModelResult, AppError> {
    select_embed_model_inner(&state, model_id)
}

/// Testable core of [`select_embed_model`]: validate the id, compute whether the resolved model
/// changed, persist it. No `AppHandle`, so it runs headless.
fn select_embed_model_inner(
    state: &AppState,
    model_id: String,
) -> Result<SelectEmbedModelResult, AppError> {
    let model = crate::embed::embed_model_by_id(&model_id)
        .ok_or_else(|| AppError::InvalidArg(format!("unknown embed model id: {model_id}")))?;

    let mut c = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;

    // The PREVIOUS resolved model id (None/empty/unknown ⇒ the default) — the re-index trigger keys
    // on a real change of the resolved model, not merely a config-string write.
    let prev_resolved = c
        .embed_model_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(crate::embed::embed_model_by_id)
        .map(|m| m.id)
        .unwrap_or(crate::embed::DEFAULT_EMBED_MODEL_ID);
    let reindex_needed = prev_resolved != model.id;

    c.embed_model_id = Some(model.id.to_string());
    c.save(&state.db)?; // republishes the process-global selection.
    drop(c);

    Ok(SelectEmbedModelResult {
        selected: model.id.to_string(),
        reindex_needed,
        model_present: crate::embed::embed_model_present(),
    })
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

    reindex_embeddings_inner(
        &state.db,
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
    // stay purged). Model present ⇒ full purge-then-reinsert re-embed. Model ABSENT ⇒ chunk-only
    // backfill of documents with NO chunks yet (the write-only legacy rows) — never a
    // purge-then-reinsert of an already-chunked document, which would DESTROY its existing real
    // vectors without replacing them.
    let doc_embedder = model_present.then_some(embedder);
    let mut docs_indexed = 0usize;
    for did in db.visible_document_ids(unlocked)? {
        let should_index = model_present || !db.document_has_chunks(&did)?;
        if !should_index {
            continue;
        }
        match db.index_document_chunks(&did, doc_embedder) {
            Ok(()) => docs_indexed += 1,
            Err(e) => {
                // Never abort the whole backfill on one bad document — log (no PII) and continue.
                tracing::warn!(target: "rag", error = %e, "reindex: indexing one document failed (skipped)");
            }
        }
    }
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

/// True iff the on-device PERSON-name NER model (Phase D) is present on disk. Pure existence probe;
/// NEVER errors on a missing models dir (treats it as "not present"). When false, the redaction
/// firewall uses the byte-identical NoopNameRedactor (candle NER is always compiled; it activates on
/// model presence).
#[tauri::command]
pub fn ner_model_present() -> Result<bool, AppError> {
    Ok(crate::summarize::redact::ner_model_present())
}

/// Download the multilingual mDeBERTa-v3 NER model (3 HF files) into the shared models dir,
/// INBOUND-ONLY, emitting [`crate::events::EVENT_NER_DOWNLOAD`] progress (throttled per file). Sends
/// NO meeting content (no egress). The downloaded model is NOT loaded here — it is picked up lazily by
/// `summarize::redact::active_name_redactor` on the next cloud summarization (selected on model
/// presence). Returns the model dir.
#[tauri::command]
pub async fn download_ner_model(app: AppHandle) -> Result<String, AppError> {
    let file_count = crate::summarize::redact::NER_MODEL_FILES.len();
    // Throttle progress to roughly every 2 MB so the model download doesn't flood the FE.
    const EMIT_EVERY: u64 = 2 * 1024 * 1024;
    let mut last_emit: u64 = 0;
    let mut last_index: usize = usize::MAX;
    let dir = crate::summarize::redact::download_ner_model(|file_index, downloaded, total| {
        if file_index != last_index || downloaded - last_emit >= EMIT_EVERY {
            last_index = file_index;
            last_emit = downloaded;
            let _ = app.emit(
                crate::events::EVENT_NER_DOWNLOAD,
                crate::events::NerDownloadPayload {
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
        crate::events::EVENT_NER_DOWNLOAD,
        crate::events::NerDownloadPayload {
            file_index: file_count,
            file_count,
            downloaded: 0,
            total: None,
            done: true,
        },
    );
    Ok(dir.to_string_lossy().to_string())
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
    let counts = state.db.count_notes_per_folder()?;
    let unlocked = {
        state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
            .clone()
    };
    Ok(build_folder_tree(&folders, &counts, &unlocked))
}

/// Assemble `FolderNode` roots (parent_id == None) and recurse children. Sealed-but-session-
/// unlocked folders carry `unlocked = true`.
fn build_folder_tree(
    folders: &[Folder],
    counts: &std::collections::HashMap<String, usize>,
    unlocked: &std::collections::HashSet<String>,
) -> Vec<FolderNode> {
    fn node(
        f: &Folder,
        folders: &[Folder],
        counts: &std::collections::HashMap<String, usize>,
        unlocked: &std::collections::HashSet<String>,
    ) -> FolderNode {
        let children = folders
            .iter()
            .filter(|c| c.parent_id.as_deref() == Some(f.id.as_str()))
            .map(|c| node(c, folders, counts, unlocked))
            .collect();
        FolderNode {
            id: f.id.clone(),
            name: f.name.clone(),
            parent_id: f.parent_id.clone(),
            note_count: counts.get(&f.id).copied().unwrap_or(0),
            locked: f.locked,
            unlocked: f.locked && unlocked.contains(&f.id),
            children,
        }
    }
    folders
        .iter()
        .filter(|f| f.parent_id.is_none())
        .map(|f| node(f, folders, counts, unlocked))
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
    state: State<'_, AppState>,
    meeting_id: String,
    folder_id: Option<String>,
) -> Result<(), AppError> {
    // Resolve current + target folder lock state.
    let note = state.db.get_latest_note_for_meeting(&meeting_id)?;
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
        if let Some(vault) = vault_path(&state) {
            let target_rel = match folder_id.as_deref() {
                Some(fid) => state.db.folder_by_id(fid)?.map(|f| f.path),
                None => None,
            };
            move_note_file(
                &state,
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
    for p in exported_paths {
        let _ = std::fs::remove_file(&p);
    }
    // The note's chunks/vectors are plaintext-derived and a dense embedding is invertible, so they
    // must NOT survive at rest for a meeting now sealed into a locked folder — same invariant the
    // lock_folder / relock / startup-reconcile paths enforce. Covers both the manual move-into-locked
    // and the auto-file callers. (Re-indexed on unlock once indexing ships.)
    state
        .db
        .purge_chunks_for_meetings(&[meeting_id.to_string()])?;
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

/// SEAL a folder: generate a content key, KEK-wrap it, encrypt every governed note's markdown
/// into `content_blob`, then (after a DB commit) blank the markdown + delete the vault `.md`.
/// Atomicity: each note's blob is verified decryptable BEFORE we blank/delete; a crash after the
/// DB write but before the `.md` delete leaves a stale plaintext `.md` (reconcilable) — never
/// lost content.
#[tauri::command]
pub fn lock_folder(state: State<'_, AppState>, folder_id: String) -> Result<(), AppError> {
    lock_folder_inner(state.inner(), folder_id)
}

/// BLK-1: acquire the coarse [`AppState::lifecycle`] guard so a folder-lock state-machine op never
/// interleaves with another (notably the off-thread `relock_all_inner`). A `Mutex<()>` carries no
/// state, so a poisoned lock is recovered via `into_inner()` — never bricking all future lock ops.
fn lifecycle_guard(state: &AppState) -> std::sync::MutexGuard<'_, ()> {
    state
        .lifecycle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Inner of [`lock_folder`] taking `&AppState` (so the lifecycle stress test can drive it without a
/// `tauri::State`). Holds the [`AppState::lifecycle`] guard for the whole seal.
pub(crate) fn lock_folder_inner(state: &AppState, folder_id: String) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    if folder.locked {
        return Ok(()); // already sealed — idempotent.
    }

    // Prefer the SESSION-CACHED KEK (set by a successful unlock — possibly a RECOVERED key): it
    // keeps every folder sealed this session convergent on the key that demonstrably unwraps the
    // existing ones, and skips a redundant Touch ID prompt. Only fall through to the keychain when
    // nothing is cached. Minting a fresh KEK is then allowed ONLY when nothing is sealed yet: with
    // sealed folders present, a missing keychain item must be an ERROR (a fresh KEK would fork the
    // key the folders depend on — the 2026-07-05 field incident sealed folders under divergent
    // mints).
    let cached: Option<Zeroizing<[u8; 32]>> = {
        let g = state
            .master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        g.clone()
    };
    let kek = match cached {
        Some(k) => k,
        None => {
            let any_sealed = state.db.any_locked_folder()?;
            Zeroizing::new(crate::secrets::master_kek_with_policy(
                "Lock this folder",
                !any_sealed,
            )?)
        }
    };
    let ck = Zeroizing::new(crate::crypto::random_key()?);
    // Wrapped CK is AAD-bound to the folder id (B7): the wrapped key cannot be lifted onto a
    // different folder row and unwrapped there.
    let wrapped = crate::crypto::encrypt(&kek, &*ck, &aad_wrapped_ck(&folder_id))?;

    // Gather the notes to seal. A meeting may have MULTIPLE provider rows (e.g. re-summarized
    // with ollama then anthropic) each with DISTINCT markdown — seal EVERY (meeting, provider)
    // row into its OWN blob. Collapsing to one blob per meeting would destroy every provider's
    // content but the first (the PRIME-DIRECTIVE content-loss bug this guards against).
    let notes = state.db.notes_in_folder(&folder_id)?;
    let mut sealed_rows: Vec<(String, String, Vec<u8>)> = Vec::new();
    for n in &notes {
        // Encrypt this row's markdown bound to (folder|meeting|provider|note|v) and VERIFY it
        // reads back before we touch the plaintext.
        let aad = aad_content(&folder_id, &n.meeting_id, &n.provider_id, "note");
        let blob = crate::crypto::encrypt(&ck, n.markdown.as_bytes(), &aad)?;
        let check = crate::crypto::decrypt(&ck, &blob, &aad)?;
        if check != n.markdown.as_bytes() {
            return Err(AppError::Storage(
                "seal verification failed (decrypted blob mismatch)".into(),
            ));
        }
        sealed_rows.push((n.meeting_id.clone(), n.provider_id.clone(), blob));
    }

    // Capture every governed note's .md path BEFORE any seal_note nulls exported_path.
    let exported_paths: Vec<String> = notes
        .iter()
        .filter_map(|n| n.exported_path.clone())
        .collect();

    // Persist: mark the folder locked (+ wrapped key) and write every sealed blob per provider
    // row (markdown blanked, exported_path cleared). Each write is guarded by the verification
    // above, so a crash mid-loop leaves already-sealed rows recoverable and not-yet-sealed rows
    // with intact plaintext — never lost content.
    state
        .db
        .set_folder_locked(&folder_id, true, Some(&wrapped))?;
    for (meeting_id, provider_id, blob) in &sealed_rows {
        state.db.seal_note(meeting_id, provider_id, blob)?;
    }

    // Phase 0.5 — seal the TRANSCRIPT + TIMELINE (defense-in-depth in the OPEN db) and the AUDIO
    // WAV at rest, all under the SAME folder CK. Verify-before-destroy inside (no transcript /
    // audio loss). Done after the note seal so a partial-seal crash still leaves recoverable blobs.
    seal_folder_extras(state, &folder_id, &ck)?;
    drop(kek); // explicit: KEK zeroized when this Zeroizing drops here.
    drop(ck); // explicit: CK zeroized after sealing all extras.

    // Phase 2a LOCK-SAFETY: purge plaintext-derived semantic chunks + their (invertible) vectors
    // for every meeting now sealed in this folder — a vector is PII derived from the plaintext, so
    // it must not survive at rest in a locked folder. Done AFTER the seal so the index is dropped
    // only once the recoverable blobs exist. Re-index-on-unlock is a separate later step; until it
    // lands a locked-then-unlocked folder is simply not semantically searchable (degraded, not
    // leaky).
    let sealed_meeting_ids = state.db.meeting_ids_in_folder(&folder_id)?;
    state.db.purge_chunks_for_meetings(&sealed_meeting_ids)?;
    // Document ingestion LOCK-SAFETY: purge the (now-sealed) documents' plaintext-derived chunks +
    // their invertible vectors too — a doc vector is PII derived from the plaintext, so it must not
    // survive at rest in a locked folder. Re-embeddable on unlock (the text seal is restorable).
    let sealed_document_ids = state.db.document_ids_in_folder(&folder_id)?;
    state
        .db
        .purge_doc_chunks_for_documents(&sealed_document_ids)?;

    // AFTER the column writes, delete the vault `.md` files (a leftover .md is reconcilable;
    // lost content is not — so this is last).
    for p in exported_paths {
        let _ = std::fs::remove_file(&p);
    }

    // Belt-and-braces RAM hygiene: with no recording active, drop any stale live-caption buffer at
    // the moment a folder seals (post clear-on-Stop it is normally already empty; idempotent).
    clear_stale_live_transcript(state);
    Ok(())
}

/// Lock-surface RAM hygiene: clear the live-transcript buffer ONLY when no recording is active —
/// never wipe an in-flight buffer (mid-recording egress correctness is owned by the visibility
/// gate in `transcribe::live`). Fail-safe: a poisoned recorder lock is treated as "recording".
fn clear_stale_live_transcript(state: &AppState) {
    let recording = state.recorder.lock().map(|g| g.is_some()).unwrap_or(true);
    crate::transcribe::live::clear_live_transcript_if_idle(&state.live_transcript, recording);
}

/// SESSION-unlock a sealed folder: KEK → unwrap CK → decrypt each note's `content_blob` back into
/// the plaintext markdown column for the session, and add the folder id to the session unlock set.
/// Does NOT re-export to the vault. Returns the refreshed folder node.
#[tauri::command]
pub async fn unlock_folder(
    state: State<'_, AppState>,
    folder_id: String,
) -> Result<FolderNode, AppError> {
    // v0.3.2 — the master KEK is a BIOMETRIC-GATED keychain item. Reading it makes macOS present the
    // Touch ID / passcode sheet directly (with our reason string) and hand back the key — THAT single
    // sheet IS the unlock auth, so there is no separate app-side authentication step (which would
    // double-prompt: Touch ID, then a keychain-password dialog). Result: exactly ONE Touch ID prompt,
    // no "app wants to use keychain, enter password" dialog, no "Always Allow".
    //
    // The `lock_require_biometric` preference (K_LOCK_REQUIRE_BIOMETRIC, default true) is INFORMATIONAL
    // only: the biometric requirement is enforced by the keychain item's kSecAttrAccessControl (an
    // OS-level gate), not by any app-side `if`. An app boolean cannot waive the OS access control —
    // even with the flag false, reading the gated item still presents the system sheet. It is NOT read
    // here precisely because it cannot change this code path; it is surfaced in settings so the user
    // can see the guarantee, and is retained on the config DTO for forward-compat.

    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    if !folder.locked {
        return Err(AppError::InvalidArg("folder is not locked".into()));
    }
    let wrapped = state
        .db
        .folder_wrapped_key(&folder_id)?
        .ok_or_else(|| AppError::Storage("locked folder has no wrapped key".into()))?;

    // Reuse the KEK cached from an earlier unlock in this session so repeated unlocks do NOT
    // re-prompt for Touch ID (the cache is zeroized on relock-all). Only fall through to the
    // biometric-gated keychain read — the single Touch ID prompt — when nothing is cached.
    let kek: Zeroizing<[u8; 32]> = {
        let cached: Option<Zeroizing<[u8; 32]>> = {
            let g = state
                .master_kek
                .lock()
                .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
            g.clone()
        };
        match cached {
            Some(k) => k,
            None => {
                // The biometric-gated keychain read BLOCKS while the Touch ID sheet is up, so run it
                // on the blocking pool — never on an async-runtime worker thread. This is the single
                // Touch ID prompt. `allow_mint = false`: a locked folder EXISTS (we are unlocking
                // it), so a missing keychain item must NEVER be papered over with a fresh KEK — that
                // orphans every sealed folder (2026-07-05 field incident).
                let resolved = tokio::task::spawn_blocking(|| {
                    crate::secrets::master_kek_with_policy("Unlock this folder", false)
                })
                .await
                .map_err(|e| AppError::Auth(format!("master-kek task join failed: {e}")))?;
                match resolved {
                    Ok(bytes) => Zeroizing::new(bytes),
                    Err(resolve_err) => {
                        // LAST RESORT: even the primary release failed (e.g. an authoritatively
                        // missing item — or a read shape that lies on this macOS). Enumerate every
                        // candidate the stores hold and try each against THIS folder's wrapped CK;
                        // a winner proceeds exactly like a released KEK. Read-only.
                        tracing::warn!(
                            target: "lock",
                            folder = %folder_id,
                            error = %resolve_err,
                            "unlock_folder: master-KEK release failed — trying candidate recovery"
                        );
                        let candidates = tokio::task::spawn_blocking(|| {
                            crate::secrets::list_master_kek_candidates("Recover the folder key")
                        })
                        .await
                        .map_err(|e| {
                            AppError::Auth(format!("kek-recovery task join failed: {e}"))
                        })?
                        .unwrap_or_else(|_| Zeroizing::new(Vec::new()));
                        match try_unwrap_ck_with_candidates(&candidates, &wrapped, &folder_id, None)
                        {
                            Some((_bytes, winner, idx)) => {
                                tracing::warn!(
                                    target: "lock",
                                    folder = %folder_id,
                                    candidates = candidates.len(),
                                    winner_index = idx,
                                    "unlock_folder: RECOVERED the master KEK from the candidate set (primary release had failed)"
                                );
                                winner
                            }
                            None => return Err(resolve_err),
                        }
                    }
                }
            }
        }
    };
    // Wrapped CK is bound to the folder id (legacy folders fall back to empty AAD transparently).
    // A failure HERE with a successfully-released KEK means the CK was wrapped under a DIFFERENT
    // KEK than the one just read (store divergence / replaced item). RECOVERY: on machines where
    // the no-UI keychain probe lied, several KEK generations can coexist — enumerate every
    // candidate in the stores and try each against the wrapped CK before giving up. Read-only;
    // the winning KEK is adopted for the session (cached below) but nothing is rewritten.
    let (ck_bytes, kek) = match crate::crypto::decrypt(&kek, &wrapped, &aad_wrapped_ck(&folder_id))
    {
        Ok(b) => (Zeroizing::new(b), kek),
        Err(primary_err) => {
            tracing::warn!(
                target: "lock",
                folder = %folder_id,
                error = %primary_err,
                "unlock_folder: content-key unwrap failed with the primary master KEK — trying every keychain candidate"
            );
            let candidates = tokio::task::spawn_blocking(|| {
                crate::secrets::list_master_kek_candidates("Recover the folder key")
            })
            .await
            .map_err(|e| AppError::Auth(format!("kek-recovery task join failed: {e}")))?
            .unwrap_or_else(|_| Zeroizing::new(Vec::new()));
            match try_unwrap_ck_with_candidates(&candidates, &wrapped, &folder_id, Some(&*kek)) {
                Some((bytes, winner, idx)) => {
                    tracing::warn!(
                        target: "lock",
                        folder = %folder_id,
                        candidates = candidates.len(),
                        winner_index = idx,
                        "unlock_folder: RECOVERED the content key with a non-primary master-KEK candidate"
                    );
                    (Zeroizing::new(bytes), winner)
                }
                None => {
                    tracing::error!(
                        target: "lock",
                        folder = %folder_id,
                        candidates = candidates.len(),
                        "unlock_folder: NO keychain candidate unwraps this folder's content key"
                    );
                    return Err(primary_err);
                }
            }
        }
    };
    let ck: Zeroizing<[u8; 32]> = Zeroizing::new(
        ck_bytes
            .as_slice()
            .try_into()
            .map_err(|_| AppError::Storage("unwrapped content key has wrong length".into()))?,
    );

    // BLK-1: from here on we MUTATE plaintext columns (restore markdown / segments / timeline).
    // Acquire the lifecycle guard for the whole synchronous restore so a concurrent
    // `relock_all_inner` (screen-share / lifecycle) cannot blank these rows mid-restore. Acquired
    // AFTER the keychain `.await` above — holding a std `MutexGuard` across an await would make this
    // command's future `!Send`; everything below is synchronous, so the guard never crosses a
    // suspend point.
    let _lifecycle = lifecycle_guard(state.inner());

    // Decrypt EACH sealed provider row's own blob back into its own markdown column for the
    // session (no dedup by meeting — every provider's distinct content is restored independently).
    // Bound to (folder|meeting|provider|note); legacy blobs fall back to empty AAD.
    let notes = state.db.notes_in_folder(&folder_id)?;
    for n in &notes {
        let Some(blob) = &n.content_blob else {
            continue; // open note (shouldn't happen in a sealed folder) — skip.
        };
        let aad = aad_content(&folder_id, &n.meeting_id, &n.provider_id, "note");
        let pt = crate::crypto::decrypt(&ck, blob, &aad)?;
        let markdown = String::from_utf8(pt)
            .map_err(|_| AppError::Storage("decrypted note is not valid UTF-8".into()))?;
        state
            .db
            .restore_note_markdown(&n.meeting_id, &n.provider_id, &markdown)?;
    }

    // Phase 0.5 — decrypt the TRANSCRIPT + TIMELINE back into their plaintext columns and
    // materialize a playable WAV (decrypt .enc → file) for the session, under the SAME CK. The
    // model-gated meeting embedder (Some only when the REAL e5 model is present → never stub vectors)
    // re-indexes the folder's meetings so semantic / related-meetings recover in-session.
    let meeting_embedder = crate::embed::embed_model_present().then(crate::embed::active_embedder);
    unseal_folder_extras(state.inner(), &folder_id, &ck, meeting_embedder.as_deref())?;

    // Cache the KEK for the session (zeroized on relock-all + on drop) + add to the unlock set.
    {
        let mut g = state
            .master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        *g = Some(kek.clone());
    }
    {
        let mut g = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        g.insert(folder_id.clone());
    }
    tracing::info!(target: "lock", folder = %folder_id, "unlock_folder: session unlock complete");

    // Return the refreshed node.
    let counts = state.db.count_notes_per_folder()?;
    let unlocked = {
        state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
            .clone()
    };
    Ok(FolderNode {
        id: folder.id.clone(),
        name: folder.name.clone(),
        parent_id: folder.parent_id.clone(),
        note_count: counts.get(&folder.id).copied().unwrap_or(0),
        locked: true,
        unlocked: unlocked.contains(&folder.id),
        children: Vec::new(),
    })
}

/// Re-seal a session-unlocked folder for the rest of this session: re-blank the plaintext
/// markdown of its sealed notes and drop the folder from the unlock set. The `content_blob`
/// stays — the folder is still `locked=1` on disk.
#[tauri::command]
pub fn relock_folder(state: State<'_, AppState>, folder_id: String) -> Result<(), AppError> {
    // BLK-1: serialize with the rest of the lock state machine (it re-blanks the same columns
    // `remove_lock` is mid-restoring).
    let _lifecycle = lifecycle_guard(state.inner());
    {
        let mut g = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        g.remove(&folder_id);
    }
    let mut one = std::collections::HashSet::new();
    one.insert(folder_id.clone());
    state.db.blank_sealed_notes_in_folders(&one)?;
    // Phase 0.5 — re-blank the transcript + timeline plaintext and drop the decrypted session WAV
    // (the .enc + the *_blob columns stay; the folder is still locked=1 on disk).
    reblank_folder_extras(state.inner(), &folder_id)?;
    Ok(())
}

/// Relock ALL session-unlocked folders + zeroize the cached KEK (called on screen-share start in
/// Stage E, and exposed as a command). Re-blanks the plaintext markdown of every sealed note.
#[tauri::command]
pub fn relock_all(state: State<'_, AppState>) -> Result<(), AppError> {
    relock_all_inner(&state)
}

/// Inner relock-all usable without a command boundary (Stage E screen-share watcher, window-close,
/// app-exit). BLK-1: this is the OFF-THREAD blanker that races `remove_lock`; it acquires the
/// [`AppState::lifecycle`] guard FIRST so its re-blank can never land between `remove_lock`'s
/// restore-plaintext (Step 1) and clear-`content_blob` (Step 2). All three off-thread callers and
/// the `relock_all` command funnel through here, so the guard lives HERE (the `relock_all` command
/// must NOT take it separately — a std `Mutex` is non-reentrant and would self-deadlock).
pub(crate) fn relock_all_inner(state: &AppState) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    // Clear the session set.
    {
        let mut g = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        g.clear();
    }
    // Zeroize the cached KEK copy (C5: use zeroize::Zeroize, not a hand byte-loop the optimizer
    // could elide — `Zeroize::zeroize` is a guaranteed, non-elidable wipe). Taking the `Zeroizing`
    // out and dropping it ALSO wipes it; the explicit call makes the intent unmistakable.
    {
        let mut g = state
            .master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        if let Some(mut k) = g.take() {
            k.zeroize();
        }
    }
    // Re-blank every sealed note across all locked folders.
    let locked: std::collections::HashSet<String> =
        state.db.locked_folder_ids()?.into_iter().collect();
    state.db.blank_sealed_notes_in_folders(&locked)?;
    // Phase 0.5 — re-blank the transcript + timeline + drop the decrypted session WAVs for every
    // locked folder too (the .enc + *_blob columns stay).
    for fid in &locked {
        reblank_folder_extras(state, fid)?;
    }
    // B12: checkpoint + truncate the WAL so the just-re-blanked plaintext does not linger in the
    // sidecar. Best-effort — a busy checkpoint is logged, not fatal to the relock.
    if let Err(e) = state.db.checkpoint_truncate() {
        tracing::warn!(target: "lock", error = %e, "wal_checkpoint(TRUNCATE) on relock_all failed");
    }
    // Belt-and-braces RAM hygiene: with no recording active, drop any stale live-caption buffer on
    // relock-all (manual "Lock all", screen-share auto-relock, window-close, app-exit). Never
    // clears mid-recording — the in-flight buffer stays, gated by visibility at injection time.
    clear_stale_live_transcript(state);
    Ok(())
}

/// PERMANENTLY remove a folder's lock: KEK → unwrap CK → decrypt each note back to plaintext
/// markdown, clear `content_blob`, set `locked=0` + `wrapped_key=NULL`, and re-export each note's
/// `.md` to the vault. The folder returns to the default OPEN state.
#[tauri::command]
pub fn remove_lock(state: State<'_, AppState>, folder_id: String) -> Result<(), AppError> {
    remove_lock_inner(state.inner(), folder_id)
}

/// Inner of [`remove_lock`] taking `&AppState` (so the BLK-1 lifecycle stress test can drive it
/// without a `tauri::State`). BLK-1: holds the [`AppState::lifecycle`] guard across the ENTIRE
/// restore→clear sequence (Step 1 decrypt-plaintext-into-`markdown`, Step 2 clear `content_blob`),
/// so the off-thread `relock_all_inner` blanker can never blank `markdown` to `''` in the window
/// between the two steps — the exact `markdown='' + content_blob=NULL` permanent-loss race.
pub(crate) fn remove_lock_inner(state: &AppState, folder_id: String) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    if !folder.locked {
        return Ok(()); // already open — idempotent.
    }
    let wrapped = state
        .db
        .folder_wrapped_key(&folder_id)?
        .ok_or_else(|| AppError::Storage("locked folder has no wrapped key".into()))?;
    // Prefer the session-cached KEK (possibly a RECOVERED key), then the keychain with the strict
    // no-mint policy (this folder IS sealed — a fresh mint can never unwrap it). On an unwrap
    // failure, run the same candidate RECOVERY as `unlock_folder` before giving up.
    let cached: Option<Zeroizing<[u8; 32]>> = {
        let g = state
            .master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        g.clone()
    };
    let kek = match cached {
        Some(k) => k,
        None => Zeroizing::new(crate::secrets::master_kek_with_policy(
            "Remove this folder's lock",
            false,
        )?),
    };
    let ck_bytes = match crate::crypto::decrypt(&kek, &wrapped, &aad_wrapped_ck(&folder_id)) {
        Ok(b) => Zeroizing::new(b),
        Err(primary_err) => {
            tracing::warn!(
                target: "lock",
                folder = %folder_id,
                error = %primary_err,
                "remove_lock: content-key unwrap failed with the primary master KEK — trying every keychain candidate"
            );
            let candidates = crate::secrets::list_master_kek_candidates("Recover the folder key")
                .unwrap_or_else(|_| Zeroizing::new(Vec::new()));
            match try_unwrap_ck_with_candidates(&candidates, &wrapped, &folder_id, Some(&*kek)) {
                Some((bytes, winner, idx)) => {
                    tracing::warn!(
                        target: "lock",
                        folder = %folder_id,
                        candidates = candidates.len(),
                        winner_index = idx,
                        "remove_lock: RECOVERED the content key with a non-primary master-KEK candidate"
                    );
                    // Cache the winner so subsequent lock ops this session converge on the key
                    // that demonstrably unwraps existing folders (and skip re-enumeration).
                    if let Ok(mut g) = state.master_kek.lock() {
                        *g = Some(winner);
                    }
                    Zeroizing::new(bytes)
                }
                None => {
                    tracing::error!(
                        target: "lock",
                        folder = %folder_id,
                        candidates = candidates.len(),
                        "remove_lock: NO keychain candidate unwraps this folder's content key"
                    );
                    return Err(primary_err);
                }
            }
        }
    };
    let ck: Zeroizing<[u8; 32]> = Zeroizing::new(
        ck_bytes
            .as_slice()
            .try_into()
            .map_err(|_| AppError::Storage("unwrapped content key has wrong length".into()))?,
    );

    let vault = vault_path(state);
    let notes = state.db.notes_in_folder(&folder_id)?;

    // Step 1: restore EVERY provider row's plaintext from ITS OWN blob (or keep the in-memory
    // markdown if the folder is session-unlocked and the blob is absent). This must happen for
    // every row BEFORE any blob is cleared — otherwise a sibling provider's content is lost.
    for n in &notes {
        let markdown = if let Some(blob) = &n.content_blob {
            let aad = aad_content(&folder_id, &n.meeting_id, &n.provider_id, "note");
            let pt = crate::crypto::decrypt(&ck, blob, &aad)?;
            String::from_utf8(pt)
                .map_err(|_| AppError::Storage("decrypted note is not valid UTF-8".into()))?
        } else {
            n.markdown.clone()
        };
        state
            .db
            .restore_note_markdown(&n.meeting_id, &n.provider_id, &markdown)?;
    }

    // Step 2: per meeting, clear the blobs (all rows now hold plaintext) and re-export ONE `.md`
    // (the latest provider's note — matching how the rest of the app treats "the note" for a
    // meeting). All provider rows for that meeting share the re-exported path.
    let mut seen = std::collections::HashSet::new();
    for n in &notes {
        if !seen.insert(n.meeting_id.clone()) {
            continue;
        }
        state.db.clear_note_content_blob(&n.meeting_id)?;

        let Some(vault) = vault.as_deref() else {
            continue;
        };
        let latest = match state.db.get_latest_note_for_meeting(&n.meeting_id)? {
            Some(l) => l,
            None => continue,
        };
        let meeting = state.db.get_meeting(&n.meeting_id)?;
        let (title, date) = match meeting {
            Some(m) => (
                m.title.clone().unwrap_or_else(|| "Untitled".into()),
                m.started_at.clone(),
            ),
            None => ("Untitled".to_string(), chrono::Utc::now().to_rfc3339()),
        };
        let sub = if folder.path.is_empty() {
            None
        } else {
            Some(folder.path.as_str())
        };
        if let Ok(path) = crate::export::write_note(
            std::path::Path::new(vault),
            sub,
            &title,
            &date,
            &latest.markdown,
        ) {
            state.db.set_note_exported_path(
                &n.meeting_id,
                &latest.provider_id,
                &path.to_string_lossy(),
            )?;
        }
    }

    // Phase 0.5 — permanently restore the TRANSCRIPT + TIMELINE plaintext (clear *_blob columns)
    // and the AUDIO WAV (decrypt .enc → file, drop .enc) under the SAME CK. Never lose audio. The
    // model-gated meeting embedder (Some only when the REAL e5 model is present → never stub vectors)
    // re-indexes the now-open folder's meetings so semantic / related-meetings work again.
    let meeting_embedder = crate::embed::embed_model_present().then(crate::embed::active_embedder);
    unseal_folder_extras_permanent(state, &folder_id, &ck, meeting_embedder.as_deref())?;

    // Flip the folder back to OPEN + drop it from the session set.
    state.db.set_folder_locked(&folder_id, false, None)?;
    {
        let mut g = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        g.remove(&folder_id);
    }
    Ok(())
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

/// SESSION-unlock the folder OWNING a meeting (so the FE can unlock straight from the locked
/// Detail view). Resolves the meeting's folder, then delegates to the existing biometric
/// `unlock_folder` path (Touch ID → KEK → unwrap CK → decrypt note + transcript + timeline + audio
/// for the session). A meeting at the vault root or in an open folder is already unlocked → no-op
/// (returns `None`); a sealed folder returns the refreshed `FolderNode`.
#[tauri::command]
pub async fn unlock_meeting(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Option<FolderNode>, AppError> {
    let Some(folder_id) = state.db.folder_for_meeting(&meeting_id)? else {
        return Ok(None); // vault root — nothing to unlock.
    };
    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    if !folder.locked {
        return Ok(None); // open folder — already visible.
    }
    // Reuse the SAME biometric unlock path (do not fork the lifecycle).
    unlock_folder(state, folder_id).await.map(Some)
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
fn aad_wrapped_ck(folder_id: &str) -> Vec<u8> {
    format!("murmur:wrapck:v{AAD_SCHEMA_VERSION}|folder={folder_id}").into_bytes()
}

/// KEK-RECOVERY: try every master-KEK candidate the keychain stores hold against a folder's
/// wrapped content key, skipping the already-tried primary. Returns the unwrapped CK bytes, the
/// WINNING KEK (adopted for the session by the caller) and the candidate index (for the forensic
/// log — count/index only, never key bytes). Read-only and pure over its inputs, so it is unit-
/// testable without a keychain. Rationale: on machines where the no-UI keychain probe lies,
/// several KEK generations coexist under the same account and only ONE of them sealed this folder
/// (2026-07-05 field incident).
fn try_unwrap_ck_with_candidates(
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
fn aad_content(folder_id: &str, meeting_id: &str, provider_id: &str, record_type: &str) -> Vec<u8> {
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
fn seal_folder_extras(state: &AppState, folder_id: &str, ck: &[u8; 32]) -> Result<(), AppError> {
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
        }
    }
}

/// `meeting_embedder` is the model-gated embedder for the MEETING re-index, resolved by the CALLER
/// (`embed_model_present().then(active_embedder)`) and passed in — `Some(real e5)` re-indexes the
/// folder's meetings, `None` (model absent) writes nothing (never a stub vector). It is injected
/// (rather than resolved internally like the document re-index below) so the model-PRESENT re-index
/// is deterministically testable without a real model on disk — meetings have no model-absent
/// chunk-only path, unlike documents.
fn unseal_folder_extras(
    state: &AppState,
    folder_id: &str,
    ck: &[u8; 32],
    meeting_embedder: Option<&dyn crate::embed::Embedder>,
) -> Result<(), AppError> {
    let meeting_ids = state.db.meeting_ids_in_folder(folder_id)?;
    for mid in &meeting_ids {
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
                let data = String::from_utf8(pt).map_err(|_| {
                    AppError::Storage("decrypted timeline is not valid UTF-8".into())
                })?;
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
    // REAL e5 model is present (never stub vectors; mirrors `import_document`). Best-effort: a
    // failure logs (no PII) and does NOT fail the unlock — the plaintext text is already restored.
    if !restored_doc_ids.is_empty() {
        let embedder = crate::embed::embed_model_present().then(crate::embed::active_embedder);
        for did in &restored_doc_ids {
            if let Err(e) = state.db.index_document_chunks(did, embedder.as_deref()) {
                tracing::warn!(target: "rag", error = %e, "document re-index on unlock failed (text restored)");
            }
        }
    }
    // MEETINGS: re-index the folder's meetings into note_chunks + vec_chunks so semantic /
    // related-meetings recover in-session (their note markdown was restored by `unlock_folder`
    // BEFORE this call). The caller supplies the model-gated `meeting_embedder` — never a stub
    // vector; mirrors the document re-embed above and the meeting half of `reindex_embeddings_inner`.
    reindex_meetings_after_unseal(state, &meeting_ids, meeting_embedder);
    Ok(())
}

/// RE-BLANK (relock): re-blank the plaintext transcript + timeline of every governed meeting and
/// remove the decrypted session WAV, re-pointing audio_path back at the `.enc`. The `*_blob`
/// columns + the `.enc` stay (the folder is still `locked=1`). Idempotent.
fn reblank_folder_extras(state: &AppState, folder_id: &str) -> Result<(), AppError> {
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
    Ok(())
}

/// PERMANENT remove-lock: decrypt every governed meeting's transcript + timeline back to plaintext,
/// clear the `*_blob` columns, and permanently restore the plaintext WAV (decrypt .enc → file,
/// remove the .enc). NEVER lose audio — the plaintext is written + the file decrypts before the
/// `.enc` is removed.
/// `meeting_embedder`: the caller-resolved, model-gated embedder for the MEETING re-index (see
/// [`unseal_folder_extras`] for why it is injected rather than resolved internally).
fn unseal_folder_extras_permanent(
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
        // when the REAL e5 model is present (never stub vectors).
        let embedder = crate::embed::embed_model_present().then(crate::embed::active_embedder);
        for did in &restored_doc_ids {
            if let Err(e) = state.db.index_document_chunks(did, embedder.as_deref()) {
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
    Ok(())
}

/// READ-GATE predicate (the user's actual complaint): a meeting is unlocked iff its folder is open
/// (NULL / not locked) OR its folder id is in the current session unlock set. Used by
/// `get_meeting_detail` / `get_segments` / `get_timeline` / `export_audio` to refuse a sealed-and-
/// not-session-unlocked meeting's content even though the SQLCipher DB is open.
/// Snapshot the live session unlock set (the same source `list_folders` / the graph reads use).
/// Passed to the `*_visible` DB reads (BLK-2b) so a sealed-and-not-unlocked meeting contributes
/// nothing to digests, search, last-note, topic threads, etc. — independent of at-rest blanking.
fn unlocked_snapshot(state: &AppState) -> Result<std::collections::HashSet<String>, AppError> {
    Ok(state
        .unlocked_folders
        .lock()
        .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
        .clone())
}

fn meeting_is_unlocked(state: &AppState, meeting_id: &str) -> Result<bool, AppError> {
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

/// The configured vault path (non-empty), or `None`. Takes `&AppState` (callers holding a
/// `tauri::State` pass `&state`, which Deref-coerces) so the `&AppState` inner cores can call it too.
fn vault_path(state: &AppState) -> Option<String> {
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
fn assert_in_vault(
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

/// (Re)start the voice-trigger listener if enabled — model present and not recording —
/// replacing any existing one. Safe to call repeatedly to reconcile after a config change
/// or once a recording finishes.
pub fn restart_voice_listener(app: AppHandle) {
    let state = app.state::<AppState>();
    if let Some(mut l) = state.voice_listener.lock().ok().and_then(|mut g| g.take()) {
        l.stop();
    }
    let (enabled, configured, size, language) = match state.config.lock() {
        Ok(c) => (
            c.voice_trigger,
            c.whisper_model_path.clone(),
            c.model_size.clone(),
            c.language.clone(),
        ),
        Err(_) => return,
    };
    if !enabled {
        return;
    }
    // Don't grab the mic while a real recording is in progress.
    if state.recorder.lock().map(|g| g.is_some()).unwrap_or(false) {
        return;
    }
    let p = configured.as_deref().map(std::path::Path::new);
    match crate::transcribe::resolve_model_path(p, &size, language.as_deref().unwrap_or("")) {
        Ok(Some(model_path)) => {
            let listener =
                crate::audio::listener::VoiceListener::start(app.clone(), model_path, language);
            if let Ok(mut g) = state.voice_listener.lock() {
                *g = Some(listener);
            }
        }
        _ => tracing::warn!(target: "voice", "voice trigger enabled but no Whisper model present"),
    }
}

/// Stop + drop the voice-trigger listener, releasing the mic. No-op if not running.
pub fn stop_voice_listener(app: &AppHandle) {
    let state = app.state::<AppState>();
    if let Some(mut l) = state.voice_listener.lock().ok().and_then(|mut g| g.take()) {
        l.stop();
    }
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

/// One row of `list_my_shares` (camelCase). Content-free by construction — the server holds no
/// titles; the local title is added ONLY when the meeting is unlocked (else `null` + `locked:true`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MyShareEntry {
    pub share_id: String,
    /// The share's local meeting title — `None` (and `locked:true`) when the meeting is sealed and
    /// not session-unlocked, or when the share was created on another device (unknown locally).
    pub title: Option<String>,
    pub locked: bool,
    pub rev: u32,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub revoked: bool,
    pub download_count: u32,
    /// The LOCAL meeting this share belongs to (so the FE can filter to THIS note). `None` when the
    /// share was created on another device (no local `outbound_shares` row) — masked, same as `title`.
    pub meeting_id: Option<String>,
    /// The server-enforced open cap (`None` ⇒ uncapped); sourced from the server list row. Drives the
    /// `X / Y opens` label. The server enforces the cap atomically on `/fetch`; this is display-only.
    pub max_downloads: Option<u32>,
    /// `link` (mode-A zero-knowledge link) vs `user` (mode-B Murmur↔Murmur grant). Lets the FE split
    /// the "Active links" list from the person-share count. Serializes snake_case → "link"/"user".
    pub mode: murmur_protocol::dto::ShareMode,
}

/// Read the configured sharing-server base URL from the live config (empty ⇒ unset).
fn share_base_url(state: &AppState) -> Result<String, AppError> {
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
async fn valid_access_token(state: &AppState) -> Result<String, AppError> {
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
    let (refresh_token, device_id, email, account_id, generation) = {
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
    let biometric_unlock_available = logged_in
        && (probe_says_cached || cfg!(all(target_os = "macos", not(debug_assertions))));
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

/// `consent_to_share_egress` — grant the one-time SHARE-egress consent (§7 inv. 5). Fail-closed:
/// until this is set, `share_note_to_link` refuses. Mirror of `consent_to_cloud_egress`.
#[tauri::command]
pub fn consent_to_share_egress(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cfg = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cfg.grant_share_egress_consent(&state.db)?;
    Ok(())
}

/// `revoke_share_egress` — revoke the share-egress consent (the next share is refused fail-closed).
#[tauri::command]
pub fn revoke_share_egress(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cfg = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cfg.revoke_share_egress(&state.db)?;
    Ok(())
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

/// `share_note_to_link(meeting_id, expires_days?, password?, max_downloads?) -> String` — create a
/// zero-knowledge mode-A link share of a note and return the share URL. `max_downloads` sets an
/// optional server-enforced open cap. See the LOCK-CRITICAL invariants above.
#[tauri::command]
pub async fn share_note_to_link(
    state: State<'_, AppState>,
    meeting_id: String,
    expires_days: Option<u32>,
    password: Option<String>,
    max_downloads: Option<u32>,
) -> Result<String, AppError> {
    share_note_to_link_inner(state.inner(), meeting_id, expires_days, password, max_downloads).await
}

/// Core of [`share_note_to_link`] over `&AppState` so the lock gate + consent gate are unit-testable
/// headless (no Tauri `State`, no server). The gate order is normative — DO NOT reorder.
pub(crate) async fn share_note_to_link_inner(
    state: &AppState,
    meeting_id: String,
    expires_days: Option<u32>,
    password: Option<String>,
    max_downloads: Option<u32>,
) -> Result<String, AppError> {
    // (1) READ-GATE — FIRST statement (copies `export_note`). A sealed-not-unlocked meeting refuses.
    if !meeting_is_unlocked(state, &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to share the note".into(),
        ));
    }

    // (7) First-ever share = explicit consent (fail-closed, mirrors cloud egress).
    // (8) Logged out ⇒ fail closed Unavailable. A mode-A link share needs a live session (the bearer
    //     token) but NOT the account MK — `L`/`NK` are per-share random (MK binds only mode-B grants),
    //     so we require login + hold the access token; MK stays untouched in the session.
    let base = {
        let cfg = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        if !cfg.share_egress_consented {
            return Err(AppError::Unavailable(
                "sharing not consented — confirm the one-time upload notice first".into(),
            ));
        }
        cfg.share_base_url.clone()
    };
    // Proactively refresh the bearer if it is at/near its 30-min expiry — otherwise a long-lived or
    // biometric-restored session 401s here ("not authenticated") while still looking logged in. Fails
    // closed `Unavailable` when logged out (mirrors the old `require_login`).
    let access_token = valid_access_token(state).await?;

    let client = crate::share::client::ShareClient::new(&base)?;

    // (2) Fetch the note via the gated read, and its display title/timestamp.
    let note = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    let meeting = state.db.get_meeting(&meeting_id)?;
    let title = meeting
        .as_ref()
        .and_then(|m| m.title.clone())
        .unwrap_or_else(|| "Shared note".to_string());
    let created_at = meeting
        .as_ref()
        .map(|m| m.started_at.clone())
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    // (2 cont.) Clean the body: strip frontmatter + flatten wikilinks + strip obsidian:// (pure fn).
    let clean_body = crate::share::envelope::clean_note_body(&note.markdown);

    // (3) Build the inner envelope + seal a fresh link share (e2ee M2). rev starts at 1.
    let share_id = crate::share::new_share_id();
    let rev = 1u32;
    let env = murmur_protocol::envelope::ShareEnvelope::new(title, clean_body, created_at);
    let pw_ref = password.as_deref().filter(|s| !s.is_empty());
    let sealed = crate::e2ee::link::seal_link_share(&env, &share_id, rev, pw_ref)?;

    // (4) Upload: content cell + wrapped_nk + gate_salt + gate_secret + rev + passwordRequired.
    //     L is NOT in this request (CreateShareRequest has no `l` field) — it stays on-device.
    let expires_at = expires_days.map(|d| {
        let days = d.clamp(1, 365) as i64;
        (chrono::Utc::now() + chrono::Duration::days(days)).to_rfc3339()
    });
    let argon = if pw_ref.is_some() {
        Some(murmur_protocol::dto::ArgonParams {
            m: sealed.argon_params.m_cost_kib,
            t: sealed.argon_params.t_cost,
            p: sealed.argon_params.p_cost,
        })
    } else {
        None
    };
    let cell_bytes = sealed.ciphertext_cell.len();
    let create_req = murmur_protocol::dto::CreateShareRequest {
        share_id: share_id.clone(),
        mode: murmur_protocol::dto::ShareMode::Link,
        content_cell: sealed.ciphertext_cell,
        wrapped_nk: sealed.wrapped_nk,
        gate_salt: sealed.gate_salt.to_vec(),
        gate_secret: sealed.gate_secret.to_vec(),
        rev,
        password_required: pw_ref.is_some(),
        argon,
        expires_at,
        // Clamp a nonsensical 0 to 1 (mirrors the `expires_days` min(1) clamp); `None` ⇒ uncapped.
        // The server enforces the cap atomically on `/fetch` — nothing else changes here.
        max_downloads: max_downloads.map(|n| n.max(1)),
        // Mode A: no per-recipient wrapped keys (that is mode B / §4.8). Absent for link shares.
        recipients: None,
    };
    let created = client.create_share(&access_token, create_req).await?;

    // (6) CONTENT-FREE egress ledger row (host + byte size). NEVER the URL / L / title.
    crate::share::ledger_row(&state.db, &client.host(), "share_create", cell_bytes);
    // Local bookkeeping — share_id + meeting_id only (NO title column).
    state.db.insert_outbound_share(
        &share_id,
        &meeting_id,
        "link",
        rev,
        &chrono::Utc::now().to_rfc3339(),
    )?;

    // (5) Assemble the URL LOCALLY — L goes ONLY into the fragment; never logged/ledgered.
    let base_for_url = if created.share_base_url.trim().is_empty() {
        base
    } else {
        created.share_base_url
    };
    Ok(crate::share::assemble_share_url(
        &base_for_url,
        &share_id,
        &sealed.l,
    ))
}

/// `list_my_shares() -> Vec<MyShareEntry>` — the server's share list, with each entry's local title
/// added ONLY when the meeting is unlocked (a sealed-not-unlocked meeting is MASKED: `locked:true`,
/// no title). §7 inv. 6.
#[tauri::command]
pub async fn list_my_shares(state: State<'_, AppState>) -> Result<Vec<MyShareEntry>, AppError> {
    let base = share_base_url(state.inner())?;
    let access = valid_access_token(state.inner()).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    let resp = client.list_shares(&access).await?;

    let mut out = Vec::with_capacity(resp.shares.len());
    for s in resp.shares {
        // Resolve the local meeting for this share (None ⇒ created on another device ⇒ masked).
        let local_meeting = state.db.outbound_share_meeting(&s.share_id)?;
        let (title, locked) = match &local_meeting {
            Some(meeting_id) => {
                if meeting_is_unlocked(state.inner(), meeting_id)? {
                    let t = state.db.get_meeting(meeting_id)?.and_then(|m| m.title);
                    (t, false)
                } else {
                    // Sealed-and-not-unlocked ⇒ MASK the title.
                    (None, true)
                }
            }
            None => (None, true),
        };
        out.push(MyShareEntry {
            share_id: s.share_id,
            title,
            locked,
            rev: s.rev,
            created_at: s.created_at,
            expires_at: s.expires_at,
            revoked: s.revoked_at.is_some(),
            download_count: s.download_count,
            meeting_id: local_meeting,
            max_downloads: s.max_downloads,
            mode: s.mode,
        });
    }
    Ok(out)
}

/// `revoke_share(share_id)` — DELETE the server ciphertext + flip the local state. Idempotent.
#[tauri::command]
pub async fn revoke_share(state: State<'_, AppState>, share_id: String) -> Result<(), AppError> {
    let base = share_base_url(state.inner())?;
    let access = valid_access_token(state.inner()).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    client.revoke_share(&access, &share_id).await?;
    state.db.set_outbound_share_state(&share_id, "revoked")?;
    crate::share::ledger_row(&state.db, &client.host(), "share_revoke", 0);
    Ok(())
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

/// The read-only result of previewing a recipient (spec §4.8): is the address a Murmur account, its
/// safety-word fingerprint, and whether this is first contact (show + confirm) or a BLOCKING key
/// change (re-verify out of band). Mutates NO pin — the FE shows this before the user commits.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipientPreview {
    pub registered: bool,
    pub fingerprint: Option<String>,
    pub first_contact: bool,
    pub key_changed: bool,
}

/// The outcome of `share_note_to_user`: `"sent"` (recipient was a registered account, wrapped now) or
/// `"invited"` (unregistered → a pending invite; a re-wrap follows when they register). The
/// `fingerprint` is present for a registered recipient (the safety word the FE can echo).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareToUserResult {
    pub status: String,
    pub fingerprint: Option<String>,
}

/// One incoming (pending-accept) share in the inbox. CONTENT-FREE by construction — no title exists
/// server-side; the title only materializes locally on accept (inside the verified envelope).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareInboxItem {
    pub share_id: String,
    pub sender_fingerprint: String,
    pub rev: u32,
    pub size: u64,
    pub created_at: String,
    /// Already accepted locally (idempotency) — the FE can render it as done.
    pub already_accepted: bool,
}

/// The result of accepting a share: the new local meeting + its title (now known, from the verified
/// envelope).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptedShare {
    pub meeting_id: String,
    pub title: String,
}

/// The TOFU state of a contact's current key vs the local pin.
enum TofuState {
    /// Never pinned — first contact (pin it, show safety words).
    FirstContact,
    /// The pin matches the current key — proceed.
    Match,
    /// The pin DIFFERS — a key change; BLOCK until re-verified (spec §4.8).
    Changed,
}

/// Normalize an email for use as a stable pin key + server lookup (trim + lowercase).
fn norm_email(email: &str) -> String {
    email.trim().to_lowercase()
}

/// Compare a contact's current `fingerprint` to the local pin WITHOUT mutating anything.
fn tofu_check(
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
type SessionMk = (String, u32, zeroize::Zeroizing<[u8; 32]>, String);

/// The logged-in sharing session's `(account_id, generation, MK, access_token)`, or a fail-closed
/// `Unavailable` when logged out (mode-B needs MK to DERIVE the identity keypair for sign/open). The
/// returned access token is proactively refreshed via [`valid_access_token`] so a long-idle mode-B
/// share never 401s on a lapsed bearer.
async fn require_session_mk(state: &AppState) -> Result<SessionMk, AppError> {
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

/// The configured vault path (empty ⇒ `None`), read over `&AppState`.
fn config_vault(state: &AppState) -> Option<String> {
    state
        .config
        .lock()
        .ok()
        .and_then(|c| c.vault_path.clone())
        .filter(|p| !p.trim().is_empty())
}

/// `preview_share_recipient(email)` — is the address a Murmur account, and (if so) its fingerprint +
/// TOFU state. Read-only (pins nothing). Requires login + a configured server.
#[tauri::command]
pub async fn preview_share_recipient(
    state: State<'_, AppState>,
    email: String,
) -> Result<RecipientPreview, AppError> {
    let base = share_base_url(state.inner())?;
    let access = valid_access_token(state.inner()).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    let resp = client.lookup_key(&access, email.trim()).await?;
    let Some(key) = resp.key.filter(|_| resp.registered) else {
        return Ok(RecipientPreview {
            registered: false,
            fingerprint: None,
            first_contact: false,
            key_changed: false,
        });
    };
    // Recompute the fingerprint locally (never trust the server's string blindly).
    let fp = crate::e2ee::key_fingerprint(&key.pk_enc, &key.pk_sig);
    // Pin/check on the STABLE server account id (not the email) so send + accept share one namespace.
    let (first_contact, key_changed) = match tofu_check(&state.db, &key.user_id, &fp)? {
        TofuState::FirstContact => (true, false),
        TofuState::Match => (false, false),
        TofuState::Changed => (false, true),
    };
    Ok(RecipientPreview {
        registered: true,
        fingerprint: Some(fp),
        first_contact,
        key_changed,
    })
}

/// `share_note_to_user(meeting_id, recipient_email, expires_days?)` — mode-B share (spec §4.8/§7).
#[tauri::command]
pub async fn share_note_to_user(
    state: State<'_, AppState>,
    meeting_id: String,
    recipient_email: String,
    expires_days: Option<u32>,
) -> Result<ShareToUserResult, AppError> {
    share_note_to_user_inner(state.inner(), meeting_id, recipient_email, expires_days).await
}

/// Core of [`share_note_to_user`] over `&AppState`. Gate order is normative — DO NOT reorder.
pub(crate) async fn share_note_to_user_inner(
    state: &AppState,
    meeting_id: String,
    recipient_email: String,
    expires_days: Option<u32>,
) -> Result<ShareToUserResult, AppError> {
    // (1) READ-GATE — FIRST statement (copies `export_note`). A sealed-not-unlocked meeting refuses.
    if !meeting_is_unlocked(state, &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to share the note".into(),
        ));
    }

    // (2) consent (fail-closed, first-ever share) + login (needs MK to derive sk_sig for the grant).
    let base = {
        let cfg = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        if !cfg.share_egress_consented {
            return Err(AppError::Unavailable(
                "sharing not consented — confirm the one-time upload notice first".into(),
            ));
        }
        cfg.share_base_url.clone()
    };
    let (account_id, generation, mk, access_token) = require_session_mk(state).await?;
    let client = crate::share::client::ShareClient::new(&base)?;

    // (3) Fetch + CLEAN the note (gated read), build the inner envelope, seal a fresh NK.
    let note = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    let meeting = state.db.get_meeting(&meeting_id)?;
    let title = meeting
        .as_ref()
        .and_then(|m| m.title.clone())
        .unwrap_or_else(|| "Shared note".to_string());
    let created_at = meeting
        .as_ref()
        .map(|m| m.started_at.clone())
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let clean_body = crate::share::envelope::clean_note_body(&note.markdown);

    let share_id = crate::share::new_share_id();
    let rev = 1u32;
    let nk = crate::e2ee::random_key32()?;
    let env = murmur_protocol::envelope::ShareEnvelope::new(title, clean_body, created_at);
    let content_cell = crate::e2ee::seal_content(&nk, &env, &share_id, rev)?;
    let content_hash = {
        use sha2::{Digest, Sha256};
        Sha256::digest(&content_cell).to_vec()
    };
    // (3b) Wrap the RETAINED NK under the account MK (share-scoped AAD) BEFORE it is persisted — so a
    // re-locked session (no MK) can no longer decrypt an already-shared envelope from the retained
    // blob. Only `share_rewrap_pending`, which holds the MK session, unwraps it. NK stays Zeroizing.
    let nk_wrapped = crate::e2ee::wrap_key32(
        &mk,
        &nk,
        crate::e2ee::outbound_nk_at_rest_aad(&share_id).as_bytes(),
    )?;

    // (4) Look up the recipient → TOFU pin/verify; wrap-now (registered) or invite (unregistered).
    let recipient_email = recipient_email.trim().to_string();
    let recipient_acct = norm_email(&recipient_email);
    let lookup = client.lookup_key(&access_token, &recipient_email).await?;

    let expires_at = expires_days.map(|d| {
        let days = d.clamp(1, 365) as i64;
        (chrono::Utc::now() + chrono::Duration::days(days)).to_rfc3339()
    });

    let (recipients, status, fingerprint) =
        if let Some(key) = lookup.key.filter(|_| lookup.registered) {
            // Registered → verify the fingerprint + enforce TOFU (BLOCK on a changed key), then wrap now.
            let fp = crate::e2ee::key_fingerprint(&key.pk_enc, &key.pk_sig);
            match tofu_check(&state.db, &key.user_id, &fp)? {
                TofuState::Changed => {
                    return Err(AppError::Other(anyhow::anyhow!(
                    "this contact's key changed since you last shared — re-verify the safety words \
                     out of band, then share again"
                )));
                }
                _ => state.db.pin_contact(
                    &key.user_id,
                    Some(&recipient_email),
                    &fp,
                    &chrono::Utc::now().to_rfc3339(),
                )?,
            }

            // Derive OUR identity from MK and sign the grant (fingerprints are the party ids in the grant).
            let sender = crate::e2ee::keys::derive_identity(&mk, &account_id, generation)?;
            let sender_fp = crate::e2ee::key_fingerprint(&sender.pk_enc, &sender.pk_sig);
            let grant = crate::e2ee::wrap::seal_to_recipient(
                &nk,
                &content_cell,
                &key.pk_enc,
                &fp, // recipient_acct_id = recipient fingerprint
                &sender,
                &sender_fp, // sender_acct_id = our fingerprint
                generation,
                &share_id,
                rev,
            )?;
            let wrapped_key =
                crate::e2ee::wrap::pack_wrapped_key(&sender.pk_enc, &sender.pk_sig, &grant)?;
            let recipients = vec![murmur_protocol::dto::ShareRecipientInput {
                email: recipient_email.clone(),
                wrapped_key: Some(wrapped_key),
                key_generation: Some(generation),
                grant_sig: Some(grant.signature),
            }];
            // Retain the MK-wrapped NK + content_hash locally so an "Update share" / re-wrap can reuse
            // them; state 'sent'.
            state.db.insert_outbound_user_share(
                &share_id,
                &meeting_id,
                rev,
                &chrono::Utc::now().to_rfc3339(),
                "sent",
                &nk_wrapped,
                &recipient_acct,
                &recipient_email,
                &content_hash,
            )?;
            (recipients, "sent".to_string(), Some(fp))
        } else {
            // Unregistered → an invite; retain the MK-wrapped NK + content_hash for the on-launch
            // re-wrap ('awaiting_key').
            let recipients = vec![murmur_protocol::dto::ShareRecipientInput {
                email: recipient_email.clone(),
                wrapped_key: None,
                key_generation: None,
                grant_sig: None,
            }];
            state.db.insert_outbound_user_share(
                &share_id,
                &meeting_id,
                rev,
                &chrono::Utc::now().to_rfc3339(),
                "awaiting_key",
                &nk_wrapped,
                &recipient_acct,
                &recipient_email,
                &content_hash,
            )?;
            (recipients, "invited".to_string(), None)
        };

    // (5) Upload — mode='user'; the link fields are unused (empty). NO note content/title in the body.
    let create_req =
        assemble_user_share_request(&share_id, rev, content_cell.clone(), recipients, expires_at);
    let _ = client.create_user_share(&access_token, create_req).await?;

    // (6) CONTENT-FREE egress ledger (host + cell byte size). NEVER a title / note text / key.
    crate::share::ledger_row(
        &state.db,
        &client.host(),
        if status == "sent" {
            "share_user_send"
        } else {
            "share_user_invite"
        },
        content_cell.len(),
    );

    Ok(ShareToUserResult {
        status,
        fingerprint,
    })
}

/// Assemble the `POST /v1/shares` body for a mode-B share. PURE (so a test can assert the serialized
/// request carries NO note title / body — only ciphertext + wrapped keys + the recipient email).
fn assemble_user_share_request(
    share_id: &str,
    rev: u32,
    content_cell: Vec<u8>,
    recipients: Vec<murmur_protocol::dto::ShareRecipientInput>,
    expires_at: Option<String>,
) -> murmur_protocol::dto::CreateShareRequest {
    murmur_protocol::dto::CreateShareRequest {
        share_id: share_id.to_string(),
        mode: murmur_protocol::dto::ShareMode::User,
        content_cell,
        // Mode-B: the link fields are unused (the NK is wrapped per-recipient via HPKE instead).
        wrapped_nk: Vec::new(),
        gate_salt: Vec::new(),
        gate_secret: Vec::new(),
        rev,
        password_required: false,
        argon: None,
        expires_at,
        max_downloads: None,
        recipients: Some(recipients),
    }
}

/// `share_rewrap_pending()` — for each locally-retained mode-B invite whose recipient has since
/// registered, re-wrap the retained NK to their now-published key and attach it (`PUT /shares/{id}/
/// keys`). Reads ONLY key material + retained NK (never meeting content) → no read-gate. Returns the
/// number of shares advanced to `sent`.
#[tauri::command]
pub async fn share_rewrap_pending(state: State<'_, AppState>) -> Result<u32, AppError> {
    share_rewrap_pending_inner(state.inner()).await
}

pub(crate) async fn share_rewrap_pending_inner(state: &AppState) -> Result<u32, AppError> {
    let base = share_base_url(state)?;
    if base.trim().is_empty() {
        return Ok(0);
    }
    // Logged out ⇒ nothing to do (not an error — this is a best-effort launch sweep).
    let Ok((account_id, generation, mk, access_token)) = require_session_mk(state).await else {
        return Ok(0);
    };
    let client = crate::share::client::ShareClient::new(&base)?;
    let sender = crate::e2ee::keys::derive_identity(&mk, &account_id, generation)?;
    let sender_fp = crate::e2ee::key_fingerprint(&sender.pk_enc, &sender.pk_sig);

    let mut advanced = 0u32;
    for (share_id, rev, nk_bytes, nk_is_wrapped, recipient_email, content_hash) in
        state.db.list_awaiting_rewrap()?
    {
        // Re-look-up the recipient. Not registered yet / lookup error ⇒ leave it pending.
        let Ok(lookup) = client.lookup_key(&access_token, &recipient_email).await else {
            continue;
        };
        let Some(key) = lookup.key.filter(|_| lookup.registered) else {
            continue;
        };
        let fp = crate::e2ee::key_fingerprint(&key.pk_enc, &key.pk_sig);
        // A changed key on a not-yet-pinned invitee is first contact; a CHANGED existing pin is
        // blocking — skip it (don't silently re-wrap to a rotated key). Pin on the STABLE server
        // account id (not email) so send + accept share one namespace.
        match tofu_check(&state.db, &key.user_id, &fp)? {
            TofuState::Changed => continue,
            _ => state.db.pin_contact(
                &key.user_id,
                Some(&recipient_email),
                &fp,
                &chrono::Utc::now().to_rfc3339(),
            )?,
        }
        // Unwrap the retained NK: MK-wrapped (0.7+ rows, needs the live MK session) or legacy raw
        // (pre-0.7 rows, unwrap = identity). A wrong-MK / tampered / malformed blob ⇒ leave pending.
        let nk = if nk_is_wrapped {
            match crate::e2ee::unwrap_key32(
                &mk,
                &nk_bytes,
                crate::e2ee::outbound_nk_at_rest_aad(&share_id).as_bytes(),
            ) {
                Ok(k) => k,
                Err(_) => continue,
            }
        } else {
            let Ok(nk_arr) = crate::e2ee::to_arr32(&nk_bytes) else {
                continue;
            };
            zeroize::Zeroizing::new(nk_arr)
        };
        let grant = crate::e2ee::wrap::seal_to_recipient_with_hash(
            &nk,
            &content_hash,
            &key.pk_enc,
            &fp,
            &sender,
            &sender_fp,
            generation,
            &share_id,
            rev,
        )?;
        let wrapped_key =
            crate::e2ee::wrap::pack_wrapped_key(&sender.pk_enc, &sender.pk_sig, &grant)?;
        // `PUT /shares/{id}/keys` keys the recipient row by the SERVER user id, which `keys/lookup`
        // now returns (`key.user_id`) — so the attach resolves correctly (closes the earlier no-op
        // gap). The re-wrap crypto above is complete + verified.
        let attach = client
            .attach_key(
                &access_token,
                &share_id,
                murmur_protocol::dto::AttachKeyRequest {
                    recipient_acct_id: key.user_id.clone(),
                    wrapped_key,
                    key_generation: generation,
                    grant_sig: grant.signature,
                },
            )
            .await;
        if attach.is_ok() {
            state.db.set_outbound_share_state(&share_id, "sent")?;
            crate::share::ledger_row(&state.db, &client.host(), "share_user_rewrap", 0);
            advanced += 1;
        }
    }
    Ok(advanced)
}

/// `list_share_inbox()` — the caller's incoming pending-accept shares (content-free). No gate: no
/// local content is read; each item's title is unknown until accept decrypts the envelope.
#[tauri::command]
pub async fn list_share_inbox(state: State<'_, AppState>) -> Result<Vec<ShareInboxItem>, AppError> {
    let base = share_base_url(state.inner())?;
    let access = valid_access_token(state.inner()).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    let resp = client.list_inbox(&access).await?;
    let mut out = Vec::with_capacity(resp.items.len());
    for i in resp.items {
        let already_accepted = state.db.inbound_share_meeting(&i.share_id)?.is_some();
        out.push(ShareInboxItem {
            share_id: i.share_id,
            sender_fingerprint: i.sender_fingerprint,
            rev: i.rev,
            size: i.size,
            created_at: i.created_at,
            already_accepted,
        });
    }
    Ok(out)
}

/// `accept_share(share_id, folder_id?)` — THE HIGH-BAR vault WRITE. See the module invariants above.
#[tauri::command]
pub async fn accept_share(
    state: State<'_, AppState>,
    share_id: String,
    folder_id: Option<String>,
) -> Result<AcceptedShare, AppError> {
    accept_share_inner(state.inner(), share_id, folder_id).await
}

pub(crate) async fn accept_share_inner(
    state: &AppState,
    share_id: String,
    folder_id: Option<String>,
) -> Result<AcceptedShare, AppError> {
    // (1) IDEMPOTENT on share_id — a re-accept returns the existing meeting, never a duplicate note.
    if let Some(mid) = state.db.inbound_share_meeting(&share_id)? {
        let title = state
            .db
            .get_meeting(&mid)?
            .and_then(|m| m.title)
            .unwrap_or_else(|| "Shared note".to_string());
        return Ok(AcceptedShare {
            meeting_id: mid,
            title,
        });
    }

    // (1b) RESUME a stranded accept: a prior attempt flipped the server row to `accepted` but failed
    //      before the local ingest committed. The server no longer lists an accepted share in the
    //      inbox and a re-accept 404s, so without the durable resume record the share would be lost.
    //      Re-fetch (the blob stays fetchable while `accepted`) + re-verify + ingest from the record.
    if let Some(pending) = state.db.get_pending_share_accept(&share_id)? {
        return resume_pending_accept(state, pending).await;
    }

    // (2) WRITE-GATE the target folder FIRST (mirror `ingest_into_folder`). Default = an auto-created
    //     UNSEALED "Shared" folder; a sealed-not-session-unlocked target is REFUSED (write nothing).
    let target = resolve_accept_folder(state, folder_id.as_deref())?;

    // (3) Need a session (MK derives the recipient identity for HPKE-open) + server.
    let (account_id, generation, mk, access) = require_session_mk(state).await?;
    let base = share_base_url(state)?;
    let client = crate::share::client::ShareClient::new(&base)?;

    // (4) Find the pending inbox item for this share.
    let inbox = client.list_inbox(&access).await?;
    let item = inbox
        .items
        .into_iter()
        .find(|i| i.share_id == share_id)
        .ok_or_else(|| {
            AppError::InvalidArg(
                "no pending share to accept (already accepted/declined, expired, or not addressed to you)"
                    .into(),
            )
        })?;

    // (5) Unpack the sender's public identity + grant from the opaque blob; ATTEST the fingerprint
    //     against the server-relayed value, then TOFU (BLOCK on a changed key) — all before any write.
    let up = crate::e2ee::wrap::unpack_wrapped_key(&item.wrapped_key, &item.grant_sig)?;
    let sender_fp = crate::e2ee::key_fingerprint(&up.sender_pk_enc, &up.sender_pk_sig);
    if sender_fp != item.sender_fingerprint {
        return Err(AppError::InvalidArg(
            "share sender identity does not match the server-attested fingerprint — refusing"
                .into(),
        ));
    }
    // TOFU: BLOCK on a changed key before doing any work. But DEFER pinning a first-contact key until
    // AFTER a successful ingest (step 9 below) — otherwise a malicious server could pre-poison a pin
    // with a first-contact item whose grant later fails verification (adversarial finding).
    if matches!(
        tofu_check(&state.db, &item.sender_user_id, &sender_fp)?,
        TofuState::Changed
    ) {
        return Err(AppError::Other(anyhow::anyhow!(
            "this sender's key changed since you last accepted from them — re-verify the safety \
             words out of band before accepting"
        )));
    }

    // (6) Flip the server row to accepted (authorizes the blob fetch). This is the point of no return
    //     server-side: a re-accept 404s and the inbox drops the item.
    let accepted = client.accept_share_server(&access, &share_id).await?;

    // (6b) DURABLY record the resume state BEFORE the fetch/verify/ingest below — so ANY failure in
    //      (7)/(8) is recoverable from the CLIENT via the resume path (step 1b), never a stranded
    //      share. Carries only the opaque server-relayed key material the inbox already held.
    state
        .db
        .insert_pending_share_accept(&crate::storage::PendingShareAccept {
            share_id: share_id.clone(),
            blob_id: accepted.blob_id.clone(),
            target_folder_id: target.id.clone(),
            sender_user_id: item.sender_user_id.clone(),
            sender_fingerprint: sender_fp.clone(),
            wrapped_key: item.wrapped_key.clone(),
            grant_sig: item.grant_sig.clone(),
            rev: item.rev,
            key_generation: item.key_generation,
            created_at: chrono::Utc::now().to_rfc3339(),
        })?;

    // (7)+(8)+(9) fetch + VERIFY (§4.8) + decrypt + ingest + pin + drop the resume record.
    let recipient = crate::e2ee::keys::derive_identity(&mk, &account_id, generation)?;
    finalize_accepted_share(
        state,
        &client,
        &access,
        &target,
        &recipient,
        &sender_fp,
        &item.sender_user_id,
        &up,
        &accepted.blob_id,
        &share_id,
        item.rev,
        item.key_generation,
    )
    .await
}

/// The shared TAIL of an accept: fetch the (recipiency-authorized) content blob, VERIFY §4.8 +
/// decrypt + ingest into the write-gated folder, pin the sender AFTER a verified ingest, drop the
/// durable resume record, and ledger. Used by both the normal path (right after the server flip) and
/// the RESUME path (after a strand). Writes NOTHING on any verification/fetch failure — and leaves the
/// resume record in place on failure so a retry can finish.
#[allow(clippy::too_many_arguments)]
async fn finalize_accepted_share(
    state: &AppState,
    client: &crate::share::client::ShareClient,
    access: &str,
    target: &Folder,
    recipient: &crate::e2ee::keys::IdentityKeypair,
    sender_fp: &str,
    sender_user_id: &str,
    up: &crate::e2ee::wrap::UnpackedGrant,
    blob_id: &str,
    share_id: &str,
    rev: u32,
    key_generation: u32,
) -> Result<AcceptedShare, AppError> {
    let content_cell = client.get_blob(access, blob_id).await?;
    let result = accept_ingest_verified(
        state,
        target,
        recipient,
        sender_fp,
        sender_user_id,
        up,
        &content_cell,
        share_id,
        rev,
        key_generation,
    )?;
    // Pin ONLY NOW — after the grant verified (§4.8) and the note landed — so a forged/failed item
    // never leaves a poisoned pin. Then drop the resume record (the strand window is closed).
    state.db.pin_contact(
        sender_user_id,
        None,
        sender_fp,
        &chrono::Utc::now().to_rfc3339(),
    )?;
    state.db.delete_pending_share_accept(share_id)?;
    crate::share::ledger_row(&state.db, &client.host(), "share_accept", content_cell.len());
    Ok(result)
}

/// RESUME a stranded accept from its durable [`PendingShareAccept`] record: the server row was flipped
/// to `accepted` on a prior attempt but the local verify+ingest failed after the flip. Re-runs the
/// write-gate + fingerprint-attest + TOFU-block (never trust the saved state blindly), then finishes
/// via [`finalize_accepted_share`]. This makes the server flip effectively idempotent from the client.
async fn resume_pending_accept(
    state: &AppState,
    pending: crate::storage::PendingShareAccept,
) -> Result<AcceptedShare, AppError> {
    // (2) WRITE-GATE the saved target folder FIRST — refuse if it was sealed since the flip.
    let target = state
        .db
        .folder_by_id(&pending.target_folder_id)?
        .ok_or_else(|| AppError::InvalidArg("the share's target folder no longer exists".into()))?;
    if target.locked && !folder_is_unlocked(state, &target.id)? {
        return Err(AppError::Locked(
            "the target folder is locked — unlock it first to finish accepting the share".into(),
        ));
    }
    // (3) Session (MK derives our identity for HPKE-open) + server.
    let (account_id, generation, mk, access) = require_session_mk(state).await?;
    let base = share_base_url(state)?;
    let client = crate::share::client::ShareClient::new(&base)?;

    // (5) Re-unpack + ATTEST the saved sender identity, then TOFU-BLOCK on a changed key — before any
    //     write, exactly like the normal path.
    let up = crate::e2ee::wrap::unpack_wrapped_key(&pending.wrapped_key, &pending.grant_sig)?;
    let sender_fp = crate::e2ee::key_fingerprint(&up.sender_pk_enc, &up.sender_pk_sig);
    if sender_fp != pending.sender_fingerprint {
        return Err(AppError::InvalidArg(
            "share sender identity does not match the retained fingerprint — refusing".into(),
        ));
    }
    if matches!(
        tofu_check(&state.db, &pending.sender_user_id, &sender_fp)?,
        TofuState::Changed
    ) {
        return Err(AppError::Other(anyhow::anyhow!(
            "this sender's key changed since you last accepted from them — re-verify the safety \
             words out of band before accepting"
        )));
    }

    // (8)+(9) fetch (the blob stays fetchable while `accepted`) + verify + ingest + pin + drop record.
    let recipient = crate::e2ee::keys::derive_identity(&mk, &account_id, generation)?;
    finalize_accepted_share(
        state,
        &client,
        &access,
        &target,
        &recipient,
        &sender_fp,
        &pending.sender_user_id,
        &up,
        &pending.blob_id,
        &pending.share_id,
        pending.rev,
        pending.key_generation,
    )
    .await
}

/// Resolve + WRITE-GATE the folder an accepted share lands in. `Some(id)` uses that folder (refusing a
/// sealed-not-unlocked one with `AppError::Locked`); `None` gets-or-creates the UNSEALED "Shared"
/// folder.
fn resolve_accept_folder(state: &AppState, folder_id: Option<&str>) -> Result<Folder, AppError> {
    match folder_id {
        Some(fid) => {
            let f = state
                .db
                .folder_by_id(fid)?
                .ok_or_else(|| AppError::InvalidArg(format!("no folder {fid}")))?;
            if f.locked && !folder_is_unlocked(state, fid)? {
                return Err(AppError::Locked(
                    "the target folder is locked — unlock it first to accept the share into it"
                        .into(),
                ));
            }
            Ok(f)
        }
        None => get_or_create_shared_folder(state),
    }
}

/// Get-or-create the UNSEALED "Shared" folder at the vault root (the default accept target). If it
/// already exists and is sealed-not-unlocked, the write-gate refuses (`AppError::Locked`).
fn get_or_create_shared_folder(state: &AppState) -> Result<Folder, AppError> {
    const SHARED: &str = "Shared";
    if let Some(f) = state.db.folder_by_path(SHARED)? {
        if f.locked && !folder_is_unlocked(state, &f.id)? {
            return Err(AppError::Locked(
                "your \"Shared\" folder is locked — unlock it (or pick another folder) to accept"
                    .into(),
            ));
        }
        return Ok(f);
    }
    let folder = Folder {
        id: uuid::Uuid::new_v4().to_string(),
        name: SHARED.to_string(),
        path: SHARED.to_string(),
        parent_id: None,
        locked: false,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Some(vault) = config_vault(state) {
        let dir = std::path::Path::new(&vault).join(SHARED);
        let _ = std::fs::create_dir_all(&dir);
    }
    state.db.insert_folder(&folder)?;
    Ok(folder)
}

/// The load-bearing crypto+write step, factored out so it is unit-testable with a crafted grant + no
/// network. It (a) VERIFIES the §4.8 grant via `open_from_sender` (HARD-FAILS unsigned / tampered /
/// replayed / swapped / gen-mismatch), (b) decrypts the content cell, and ONLY THEN (c) ingests the
/// note into the (already write-gated) folder. On ANY verification/decrypt failure it returns
/// `AppError::InvalidArg` and writes NOTHING.
#[allow(clippy::too_many_arguments)]
fn accept_ingest_verified(
    state: &AppState,
    target: &Folder,
    recipient: &crate::e2ee::keys::IdentityKeypair,
    sender_fp: &str,
    sender_user_id: &str,
    up: &crate::e2ee::wrap::UnpackedGrant,
    content_cell: &[u8],
    share_id: &str,
    rev: u32,
    key_generation: u32,
) -> Result<AcceptedShare, AppError> {
    let recipient_fp = crate::e2ee::key_fingerprint(&recipient.pk_enc, &recipient.pk_sig);
    // (a) §4.8 VERIFY before any write. The pinned pk_sig is the one we unpacked + fingerprint-attested.
    let nk = crate::e2ee::wrap::open_from_sender(
        &up.grant,
        content_cell,
        recipient,
        &recipient_fp, // recipient_acct_id
        &recipient_fp, // self_acct_id
        sender_fp,     // sender_acct_id (as signed)
        key_generation,
        sender_fp,         // pinned_sender_acct_id
        &up.sender_pk_sig, // pinned_sender_pk_sig (attested to the server fingerprint upstream)
        share_id,
        rev,
    )
    .map_err(|_| {
        AppError::InvalidArg(
            "share grant failed verification (unsigned / tampered / replayed) — refusing to ingest"
                .into(),
        )
    })?;
    // (b) Decrypt the content cell → the inner envelope (title travels INSIDE).
    let env = crate::e2ee::open_content(&nk, content_cell, share_id, rev).map_err(|_| {
        AppError::InvalidArg("shared note failed to decrypt — refusing to ingest".into())
    })?;
    // (c) Ingest into the write-gated folder.
    ingest_shared_note(state, target, &env, sender_fp, sender_user_id, share_id)
}

/// Write a VERIFIED shared note into the vault + DB: a new `Exported` meeting (audio `None`) + a
/// `"shared"` note carrying `shared-by`/`shared-at`/`share-id` provenance frontmatter, atomically
/// exported to the folder's vault subdir, and an `inbound_shares` idempotency record. The new meeting
/// is a NORMAL row → it participates in every existing gate automatically.
fn ingest_shared_note(
    state: &AppState,
    target: &Folder,
    env: &murmur_protocol::envelope::ShareEnvelope,
    sender_fp: &str,
    sender_user_id: &str,
    share_id: &str,
) -> Result<AcceptedShare, AppError> {
    let meeting_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    // A well-formed created_at (RFC3339) is kept; otherwise fall back to now (never trust the payload).
    let started_at = if chrono::DateTime::parse_from_rfc3339(env.created_at.trim()).is_ok() {
        env.created_at.trim().to_string()
    } else {
        now.clone()
    };
    let title = {
        let t = env.title.trim();
        if t.is_empty() {
            "Shared note".to_string()
        } else {
            t.to_string()
        }
    };
    // Provenance frontmatter. `shared-by` is the ATTESTED sender fingerprint (safe base32) — NEVER the
    // attacker-controlled envelope, so a malicious sender can't forge/inject provenance.
    let full_md = format!(
        "---\nshared-by: {sender_fp}\nshared-at: {now}\nshare-id: {share_id}\n---\n\n{}",
        env.markdown
    );

    // Meeting row (Exported, no audio), associated with the target folder.
    state.db.insert_meeting(&Meeting {
        id: meeting_id.clone(),
        started_at: started_at.clone(),
        ended_at: None,
        title: Some(title.clone()),
        duration_s: 0,
        audio_path: None,
        status: MeetingStatus::Exported,
        folder_id: Some(target.id.clone()),
    })?;

    // Atomic vault export (best-effort — a missing/invalid vault just leaves exported_path None; the
    // note is still durable in the DB, the source of truth).
    let exported_path = config_vault(state).and_then(|vault| {
        crate::export::write_note(
            std::path::Path::new(&vault),
            Some(&target.path),
            &title,
            &started_at,
            &full_md,
        )
        .ok()
        .map(|p| p.to_string_lossy().to_string())
    });

    state.db.upsert_note(&NoteRecord {
        meeting_id: meeting_id.clone(),
        provider_id: "shared".to_string(),
        markdown: full_md,
        created_at: now.clone(),
        exported_path,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })?;
    // The meeting's folder is resolved via `notes.folder_id` (`folder_for_meeting`) — set it so every
    // gate (`meeting_is_unlocked`, `visibility_clause`) sees this note as living in the target folder.
    state.db.set_note_folder(&meeting_id, Some(&target.id))?;

    // Idempotency + provenance record (a re-accept of this share_id is INSERT-OR-IGNORE'd).
    state
        .db
        .insert_inbound_share(share_id, &meeting_id, sender_user_id, &now)?;

    tracing::info!(
        target: "share",
        share_id = %share_id,
        meeting_id = %meeting_id,
        folder_id = %target.id,
        "accepted a shared note into the vault"
    );
    Ok(AcceptedShare { meeting_id, title })
}

/// `decline_share(share_id)` — drop the wrapped key server-side + flip the local state. Idempotent.
#[tauri::command]
pub async fn decline_share(state: State<'_, AppState>, share_id: String) -> Result<(), AppError> {
    let base = share_base_url(state.inner())?;
    let access = valid_access_token(state.inner()).await?;
    let client = crate::share::client::ShareClient::new(&base)?;
    client.decline_share_server(&access, &share_id).await?;
    crate::share::ledger_row(&state.db, &client.host(), "share_decline", 0);
    Ok(())
}

#[cfg(test)]
mod lock_read_gate_tests {
    use super::*;

    fn meeting_with_audio(audio_path: Option<&str>) -> Meeting {
        Meeting {
            id: "m1".to_string(),
            started_at: "2026-06-27T09:00:00Z".to_string(),
            ended_at: None,
            title: Some("Quarterly board strategy".to_string()),
            duration_s: 1800,
            audio_path: audio_path.map(|s| s.to_string()),
            status: MeetingStatus::Summarized,
            folder_id: Some("secret-folder".to_string()),
        }
    }

    /// The master seal stages (`seal_audio_at_rest` → `permanent_unseal_audio`) round-trip a file
    /// byte-identical with verify-before-destroy: the plaintext is removed only after a verified
    /// `.enc` exists, and the `.enc` only after the plaintext is restored. These run per-file for
    /// `audio_path` AND both masters, so this covers the masters' at-rest crypto + crash-safety.
    #[test]
    fn master_seal_stage_round_trips_byte_identical() {
        let ck = [7u8; 32];
        // Seal binds the ROLE form (mic master); unseal goes through the role→role-less ladder. A
        // mismatch would fail the AES-GCM tag check, so this exercises the real bound round-trip
        // under the stream-role hardening.
        let mic_aad = aad_audio_role("m-master", "f-master", StreamRole::Mic);
        let (mic_role, mic_less) = audio_decrypt_ladder("m-master", "f-master", StreamRole::Mic);
        let plain =
            std::env::temp_dir().join(format!("murmur-seal-stage-{}.bin", std::process::id()));
        let original = b"RIFF\x00\x01\x02\xfffake-master-pcm....\x10\x20".to_vec();
        std::fs::write(&plain, &original).unwrap();
        let plain_s = plain.to_string_lossy().to_string();

        let enc = seal_audio_at_rest(&ck, Some(plain_s.clone()), &mic_aad)
            .unwrap()
            .expect("a fresh plaintext path seals");
        assert!(enc.ends_with(ENC_SUFFIX));
        assert!(
            !std::path::Path::new(&plain_s).exists(),
            "plaintext removed only after a verified .enc"
        );
        assert!(std::path::Path::new(&enc).exists(), ".enc written");
        // Idempotent: an already-sealed path is a no-op (never double-encrypts).
        assert!(seal_audio_at_rest(&ck, Some(enc.clone()), &mic_aad)
            .unwrap()
            .is_none());

        // A mic master must NOT decrypt under the SYS ladder (no cross-stream swap within a meeting).
        let (sys_role, sys_less) = audio_decrypt_ladder("m-master", "f-master", StreamRole::Sys);
        assert!(
            permanent_unseal_audio(&ck, Some(enc.clone()), &[&sys_role, &sys_less]).is_err(),
            "the mic master must not unseal under the sys role ladder"
        );

        let restored = permanent_unseal_audio(&ck, Some(enc.clone()), &[&mic_role, &mic_less])
            .unwrap()
            .expect("a .enc path unseals");
        assert_eq!(restored, plain_s);
        assert!(
            !std::path::Path::new(&enc).exists(),
            ".enc removed only after the plaintext is restored"
        );
        let back = std::fs::read(&restored).unwrap();
        let _ = std::fs::remove_file(&restored);
        assert_eq!(
            back, original,
            "master survives seal -> unseal byte-identical"
        );
    }

    /// REGRESSION (audio asset-protocol leak): `get_meeting_detail`'s masked DTO for a sealed-and-
    /// not-session-unlocked meeting MUST null `audio_path`. The FE feeds `audio_path` straight into
    /// `convertFileSrc` (the Tauri `asset:` protocol, scoped to the audio dir) which serves the
    /// file to the webview WITHOUT going through the `export_audio` command or `meeting_is_unlocked`
    /// — the one audio read path outside the command gate. Before the fix the masked DTO kept
    /// `audio_path` via `..meeting`; if a PLAINTEXT WAV lived in the scoped dir (e.g. a recording
    /// auto-filed / moved into an already-sealed folder, where the pipeline writes
    /// `<audio>/{id}.wav` with no seal-awareness, or a crash window before re-seal) the locked
    /// view would serve raw audio. Nulling the path closes the bypass regardless of on-disk state.
    #[test]
    fn masked_detail_nulls_audio_path_so_asset_protocol_cannot_serve_a_locked_recording() {
        // The dangerous case: a PLAINTEXT WAV still on disk in the scoped audio dir.
        let plaintext_wav = "/Users/x/Library/Application Support/MeetNotes/audio/m1.wav";
        let masked = masked_detail(meeting_with_audio(Some(plaintext_wav)));

        // The single load-bearing assertion: no path for `convertFileSrc` to serve.
        assert_eq!(
            masked.meeting.audio_path, None,
            "masked detail must NULL audio_path — the FE asset-protocol serve path bypasses the command gate"
        );
        // And the rest of the mask: title hidden, no note, no segments, locked flag set.
        assert_eq!(masked.meeting.title.as_deref(), Some("🔒 Locked"));
        assert!(masked.note.is_none(), "no note while locked");
        assert!(masked.segments.is_empty(), "no segments while locked");
        assert!(
            masked.locked,
            "locked flag set so the FE renders the unlock affordance"
        );
        // Non-content metadata is preserved so the FE can offer "unlock this folder".
        assert_eq!(masked.meeting.id, "m1");
        assert_eq!(masked.meeting.folder_id.as_deref(), Some("secret-folder"));
    }

    /// Even with NO audio (already `.enc`-renamed or never recorded), the masked DTO is `None` —
    /// the mask is unconditional, not dependent on the on-disk seal state.
    #[test]
    fn masked_detail_nulls_audio_path_even_when_already_absent() {
        let masked = masked_detail(meeting_with_audio(None));
        assert_eq!(masked.meeting.audio_path, None);
        assert!(masked.locked);
    }

    /// Phase 5 — provenance lock-gate: a LOCKED (sealed-not-unlocked) meeting's masked DTO MUST
    /// have ALL three provenance fields set to `None`. A model name / gateway host could reveal
    /// which AI service processed the note content — the same sensitivity as the note text itself.
    #[test]
    fn masked_detail_nulls_all_provenance_fields() {
        let masked = masked_detail(meeting_with_audio(None));
        assert!(
            masked.ai_provider.is_none(),
            "masked detail must NULL ai_provider (provenance leak)"
        );
        assert!(
            masked.ai_model.is_none(),
            "masked detail must NULL ai_model (provenance leak)"
        );
        assert!(
            masked.model_served.is_none(),
            "masked detail must NULL model_served (provenance leak)"
        );
        assert!(masked.locked, "locked flag set");
    }

    // ── D5 vault-containment (`assert_in_vault`) ────────────────────────────────────────────────

    fn tmp_vault(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "murmur-vault-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn assert_in_vault_accepts_legit_relative_and_nonexistent_leaf() {
        let vault = tmp_vault("ok");
        // A not-yet-existing nested target inside the vault is allowed (it's about to be created).
        let resolved =
            assert_in_vault(&vault, std::path::Path::new("Projects/Q3/note.md")).unwrap();
        assert!(
            resolved.starts_with(vault.canonicalize().unwrap()),
            "stays inside the vault root"
        );
        // The empty path resolves to the vault root itself.
        let root = assert_in_vault(&vault, std::path::Path::new("")).unwrap();
        assert_eq!(root, vault.canonicalize().unwrap());
        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn assert_in_vault_rejects_parent_dir_traversal_and_absolute() {
        let vault = tmp_vault("escape");
        // `..` traversal that would climb out of the vault.
        assert!(
            assert_in_vault(&vault, std::path::Path::new("../../etc/passwd")).is_err(),
            "must reject a '..' traversal"
        );
        // A `..` even mid-path is rejected outright.
        assert!(
            assert_in_vault(&vault, std::path::Path::new("Projects/../../secret")).is_err(),
            "must reject any embedded '..'"
        );
        // An absolute path is rejected (re-anchors outside the vault).
        assert!(
            assert_in_vault(&vault, std::path::Path::new("/etc/passwd")).is_err(),
            "must reject an absolute path"
        );
        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn assert_in_vault_rejects_symlink_escape() {
        let vault = tmp_vault("symlink");
        // A symlink INSIDE the vault that points OUTSIDE must not let a write escape.
        let outside = std::env::temp_dir().join(format!("murmur-outside-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        let link = vault.join("escape-link");
        #[cfg(unix)]
        {
            // Best-effort: if symlink creation fails (e.g. sandbox), skip the assertion.
            if std::os::unix::fs::symlink(&outside, &link).is_ok() {
                let res = assert_in_vault(&vault, std::path::Path::new("escape-link/evil.md"));
                assert!(
                    res.is_err(),
                    "a symlink that points outside the vault must be rejected"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&vault);
        let _ = std::fs::remove_dir_all(&outside);
    }

    // ── B7/B8 AAD context-binding regression at the helper level (defense-in-depth over crypto::) ──

    #[test]
    fn content_aad_distinguishes_every_context_axis() {
        // The five axes (folder, meeting, provider, record-type, schema-version) must each change
        // the AAD so a blob cannot be swapped across any of them.
        let base = aad_content("f", "m", "p", "note");
        assert_ne!(
            base,
            aad_content("F", "m", "p", "note"),
            "folder axis binds"
        );
        assert_ne!(
            base,
            aad_content("f", "M", "p", "note"),
            "meeting axis binds"
        );
        assert_ne!(
            base,
            aad_content("f", "m", "P", "note"),
            "provider axis binds"
        );
        assert_ne!(
            base,
            aad_content("f", "m", "p", "segment"),
            "record-type axis binds"
        );
        // wrapped-CK and audio AADs are distinct namespaces from content.
        assert_ne!(
            aad_wrapped_ck("f"),
            aad_content("f", "m", AAD_NO_PROVIDER, "note")
        );
        assert_ne!(
            aad_audio("m", "f"),
            aad_content("f", "m", AAD_NO_PROVIDER, "note")
        );
    }

    /// Stream-role hardening: each of the three per-meeting audio roles produces a DISTINCT AAD, and
    /// each differs from the historical role-LESS form — so within ONE meeting a mic master can't be
    /// swapped for the sys master or the playback WAV. The role-less form is retained verbatim as the
    /// backward-compat decrypt rung (it must equal the v1 string an existing master was sealed with).
    #[test]
    fn audio_role_aad_distinguishes_each_stream_and_keeps_legacy_form() {
        let pb = aad_audio_role("m", "f", StreamRole::Playback);
        let mic = aad_audio_role("m", "f", StreamRole::Mic);
        let sys = aad_audio_role("m", "f", StreamRole::Sys);
        assert_ne!(pb, mic, "playback vs mic binds");
        assert_ne!(pb, sys, "playback vs sys binds");
        assert_ne!(mic, sys, "mic vs sys binds");

        let role_less = aad_audio("m", "f");
        assert_ne!(
            role_less, mic,
            "the role form differs from the role-less form"
        );
        // Each role form is the role-less string PLUS a |stream=… suffix → a role-less blob can never
        // match a role AAD, which is exactly why the decrypt ladder must also try the role-less rung.
        assert!(
            mic.starts_with(&role_less),
            "role AAD extends the role-less form"
        );
        // The role-less form is the EXACT v1 string existing masters carry (no drift = no data loss).
        assert_eq!(role_less, b"murmur:audio:v1|meeting=m|folder=f".to_vec());
    }
}

// ── BLK-1 lifecycle-race + BLK-2 move-into-locked + BLK-3/BLK-4 config tests ──────────────────────
#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::storage::Db;
    use crate::transcribe::types::Segment;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, Once};

    // A fixed at-rest DB key (NOT the Keychain) — same shape the config tests use.
    const DB_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    // A fixed dev master KEK so lock/unlock/remove use a deterministic key WITHOUT the Keychain or a
    // Touch ID prompt (the `MURMUR_DEV_KEK` debug-only escape hatch in `secrets::keychain`).
    const DEV_KEK: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    static KEK_ENV: Once = Once::new();
    fn ensure_dev_kek() {
        // Set once, before any thread reads it, so the concurrent readers only ever READ env.
        KEK_ENV.call_once(|| std::env::set_var("MURMUR_DEV_KEK", DEV_KEK));
    }

    fn tmp_db_path(tag: &str) -> std::path::PathBuf {
        let p = crate::storage::db::unique_temp_path(&format!("murmur-lifecycle-{tag}"), "sqlite");
        let _ = std::fs::remove_file(&p);
        p
    }

    /// Construct an [`AppState`] backed by a real temp SQLCipher DB, no Keychain, no Tauri. The
    /// recorder/listeners are `None`; the config is the default (no vault → remove-lock skips
    /// re-export, keeping the test filesystem-quiet).
    fn build_state(tag: &str) -> AppState {
        ensure_dev_kek();
        let db = Arc::new(Db::open_with_key(&tmp_db_path(tag), DB_KEY).unwrap());
        AppState {
            recorder: Mutex::new(None),
            system_recorder: Mutex::new(None),
            aec_recorder: Mutex::new(None),
            spill_writer: Mutex::new(None),
            voice_listener: Mutex::new(None),
            voice_command_capture: Mutex::new(None),
            db,
            config: Arc::new(Mutex::new(AppConfig::default())),
            reasoner: crate::reason::ReasonerCell::fixed(Arc::new(crate::reason::StubReasoner)),
            current_meeting: Mutex::new(None),
            live_transcript: Mutex::new(String::new()),
            capped_notified: std::sync::atomic::AtomicBool::new(false),
            reactions_shadow_count: std::sync::atomic::AtomicU64::new(0),
            reactions_emitted: Mutex::new(HashSet::new()),
            unlocked_folders: Arc::new(Mutex::new(HashSet::new())),
            master_kek: Mutex::new(None),
            account_session: Mutex::new(None),
            lifecycle: Mutex::new(()),
            share_refresh_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// PHASE-4 lock gate (RED-before-GREEN): `verify_note_sources` + `apply_note_verify_markers` on a
    /// SEALED-and-not-session-unlocked meeting MUST refuse with `AppError::Locked` BEFORE any note
    /// read / connector egress — a locked note can neither be verified nor marker-written. Runs with
    /// NO Jira configured, so once session-unlocked the verify falls through to the fail-closed
    /// connector gate (`Unavailable`), proving `Locked` was the FIRST gate, not a downstream error.
    #[test]
    fn verify_commands_refuse_a_sealed_meeting() {
        let state = build_state("verify-lockgate");
        make_open_folder(&state.db, "f-vlock", "Secret");
        state
            .db
            .insert_meeting(&Meeting {
                id: "m-vlock".to_string(),
                started_at: "2026-07-05T09:00:00Z".to_string(),
                ended_at: None,
                title: Some("Sprint".to_string()),
                duration_s: 600,
                audio_path: None,
                status: MeetingStatus::Summarized,
                folder_id: Some("f-vlock".to_string()),
            })
            .unwrap();
        state
            .db
            .upsert_note(&NoteRecord {
                meeting_id: "m-vlock".to_string(),
                provider_id: "claude_code".to_string(),
                markdown: "# N\n- Ship PROJ-1 by 2026-07-08\n".to_string(),
                created_at: "2026-07-05T09:10:00Z".to_string(),
                exported_path: None,
                model_requested: None,
                model_served: None,
                gateway_host: None,
            })
            .unwrap();
        state.db.set_note_folder("m-vlock", Some("f-vlock")).unwrap();
        state
            .db
            .set_folder_locked("f-vlock", true, Some(&b"wrapped"[..]))
            .unwrap();
        // NOT session-unlocked: both commands fail closed with Locked.
        let e1 = block_on(verify_note_sources_inner(&state, "m-vlock".to_string())).unwrap_err();
        assert!(matches!(e1, AppError::Locked(_)), "verify must fail Locked, got: {e1:?}");
        let finding = crate::verify::VerifyFinding {
            line_no: 2,
            key: "PROJ-1".into(),
            verdict: crate::verify::Verdict::NotFound,
            detail: "PROJ-1 not found in Jira".into(),
            url: String::new(),
        };
        let e2 =
            apply_note_verify_markers_inner(&state, "m-vlock".to_string(), vec![finding]).unwrap_err();
        assert!(matches!(e2, AppError::Locked(_)), "apply must fail Locked, got: {e2:?}");

        // Session-unlock → the lock gate passes; verify now falls to the fail-closed connector gate
        // (no Jira configured ⇒ jira_lookup NeedsConsent ⇒ Unavailable), proving Locked was the gate.
        state
            .unlocked_folders
            .lock()
            .unwrap()
            .insert("f-vlock".to_string());
        let e3 = block_on(verify_note_sources_inner(&state, "m-vlock".to_string())).unwrap_err();
        assert!(
            matches!(e3, AppError::Unavailable(_)),
            "past the lock gate, an unconfigured Jira verify fails Unavailable, got: {e3:?}"
        );
    }

    /// M3-CLIENT lock gate (spec §7 inv. 1): `share_note_to_link` on a SEALED-and-not-session-unlocked
    /// meeting MUST refuse with `AppError::Locked` BEFORE any note read / network / consent check — a
    /// locked note can never be uploaded. This is the RED-before-GREEN regression for the leak class:
    /// it runs with NO server configured + NO login, so a non-`Locked` return (e.g. it fell through to
    /// the "not signed in"/"no server" errors) is a gate-order violation and fails the test.
    #[test]
    fn share_note_to_link_refuses_a_sealed_meeting() {
        let state = build_state("share-lockgate");
        // A locked folder holding a meeting + note.
        make_open_folder(&state.db, "f-lock", "Secret");
        state
            .db
            .insert_meeting(&Meeting {
                id: "m-locked".to_string(),
                started_at: "2026-07-04T09:00:00Z".to_string(),
                ended_at: None,
                title: Some("Board strategy".to_string()),
                duration_s: 600,
                audio_path: None,
                status: MeetingStatus::Summarized,
                folder_id: Some("f-lock".to_string()),
            })
            .unwrap();
        state
            .db
            .upsert_note(&NoteRecord {
                meeting_id: "m-locked".to_string(),
                provider_id: "claude_code".to_string(),
                markdown: "---\nattendees:\n  - Alice\n---\n# secret".to_string(),
                created_at: "2026-07-04T09:10:00Z".to_string(),
                exported_path: None,
                model_requested: None,
                model_served: None,
                gateway_host: None,
            })
            .unwrap();
        // Associate the note (hence the meeting) with the folder, then seal it. `meeting_is_unlocked`
        // resolves the folder via `notes.folder_id` (`folder_for_meeting`), so this MUST be set or the
        // meeting reads as vault-root/open and the gate is a no-op.
        state
            .db
            .set_note_folder("m-locked", Some("f-lock"))
            .unwrap();
        state
            .db
            .set_folder_locked("f-lock", true, Some(&b"wrapped"[..]))
            .unwrap();
        // NOT session-unlocked: `unlocked_folders` stays empty.

        let err = block_on(share_note_to_link_inner(
            &state,
            "m-locked".to_string(),
            None,
            None,
            None,
        ))
        .unwrap_err();
        assert!(
            matches!(err, AppError::Locked(_)),
            "a sealed meeting must fail closed with Locked, got: {err:?}"
        );

        // Session-unlock the folder → the gate passes, and the NEXT failure is the fail-closed
        // consent/login gate (proving the Locked was the gate, not a downstream error).
        state
            .unlocked_folders
            .lock()
            .unwrap()
            .insert("f-lock".to_string());
        let err2 = block_on(share_note_to_link_inner(
            &state,
            "m-locked".to_string(),
            None,
            None,
            None,
        ))
        .unwrap_err();
        assert!(
            matches!(err2, AppError::Unavailable(_)),
            "past the lock gate, an unconsented/logged-out share fails Unavailable, got: {err2:?}"
        );
    }

    // ── KEK candidate-recovery (0.7.4) tests ────────────────────────────────────────────────────

    /// The recovery loop finds the ONE candidate that sealed the folder (AAD-bound), returns its
    /// bytes + index, and yields None when no candidate (or the wrong folder id) matches.
    #[test]
    fn kek_recovery_tries_candidates_and_finds_the_sealing_key() {
        let kek_old: [u8; 32] = [3u8; 32];
        let kek_new: [u8; 32] = [9u8; 32];
        let ck = crate::crypto::random_key().unwrap();
        let wrapped = crate::crypto::encrypt(&kek_old, &ck, &aad_wrapped_ck("f-rec")).unwrap();

        // Primary (kek_new) failed upstream; candidates hold [wrong, old] → winner at index 1.
        let candidates = [[7u8; 32], kek_old];
        let (bytes, winner, idx) =
            try_unwrap_ck_with_candidates(&candidates, &wrapped, "f-rec", Some(&kek_new))
                .expect("recovery must find the sealing KEK among the candidates");
        assert_eq!(bytes.as_slice(), ck.as_slice(), "recovered CK must be the original");
        assert_eq!(*winner, kek_old, "the winner is the KEK that sealed the folder");
        assert_eq!(idx, 1);

        // No matching candidate → None (the caller surfaces the primary error).
        assert!(
            try_unwrap_ck_with_candidates(&[[7u8; 32]], &wrapped, "f-rec", Some(&kek_new))
                .is_none()
        );
        // AAD binding holds: the right KEK under the WRONG folder id must not unwrap.
        assert!(
            try_unwrap_ck_with_candidates(&candidates, &wrapped, "f-other", Some(&kek_new))
                .is_none()
        );
        // The already-tried primary is skipped even if listed as a candidate.
        assert!(
            try_unwrap_ck_with_candidates(&[kek_new], &wrapped, "f-rec", Some(&kek_new)).is_none()
        );
        // With NO already-tried key (the primary release itself failed), all candidates are tried.
        let (_, winner2, _) = try_unwrap_ck_with_candidates(&candidates, &wrapped, "f-rec", None)
            .expect("recovery with no primary must still find the sealing KEK");
        assert_eq!(*winner2, kek_old);
    }

    /// `any_locked_folder` (the mint-guard input) flips with seal state.
    #[test]
    fn any_locked_folder_reflects_seal_state() {
        let state = build_state("any-locked");
        make_open_folder(&state.db, "f-guard", "Guard");
        assert!(!state.db.any_locked_folder().unwrap());
        state
            .db
            .set_folder_locked("f-guard", true, Some(&b"wrapped"[..]))
            .unwrap();
        assert!(state.db.any_locked_folder().unwrap());
        state.db.set_folder_locked("f-guard", false, None).unwrap();
        assert!(!state.db.any_locked_folder().unwrap());
    }

    // ── session-refresh (0.7.3 re-enable) tests ─────────────────────────────────────────────────

    /// Snapshot-and-restore guard for the DEV secret file (`MeetNotes-dev/dev-secrets.json`): the
    /// refresh path's best-effort Keychain mirror writes REAL dev-store keys in a debug test run, and
    /// this test must not clobber the developer's live dev-app share session. Restores on drop (also
    /// on panic).
    struct DevSecretsSnapshot {
        path: std::path::PathBuf,
        original: Option<Vec<u8>>,
    }
    impl DevSecretsSnapshot {
        fn take() -> Self {
            let path = dirs::data_dir()
                .expect("data dir")
                .join(crate::state::app_dir_name())
                .join("dev-secrets.json");
            let original = std::fs::read(&path).ok();
            Self { path, original }
        }
    }
    impl Drop for DevSecretsSnapshot {
        fn drop(&mut self) {
            match &self.original {
                Some(bytes) => {
                    let _ = std::fs::write(&self.path, bytes);
                }
                None => {
                    let _ = std::fs::remove_file(&self.path);
                }
            }
        }
    }

    /// RED-before-GREEN for the 0.7.2 "sharing dies after a restart" regression: a session whose
    /// bearer is PAST its 30-min server-side expiry MUST be proactively redeemed via
    /// `/v1/auth/refresh` — the defanged `valid_access_token` returned the dead cached bearer, so
    /// every share op after a restart 401'd until a full password re-login. Drives the REAL refresh
    /// path against a local one-shot HTTP server (loopback `http` is allowed by the URL guardrails)
    /// and proves the RAM-FIRST rotation: the fresh pair lands in the in-RAM session.
    #[test]
    fn valid_access_token_refreshes_a_stale_bearer() {
        let _guard = DevSecretsSnapshot::take();

        // One-shot local `/v1/auth/refresh`: read the full request, answer with a rotated pair.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut sock, _) = listener.accept().unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            // Read until the JSON body's closing brace has arrived (tiny body, no keep-alive reuse).
            loop {
                let n = sock.read(&mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req);
                if n == 0 || (text.contains("\r\n\r\n") && text.trim_end().ends_with('}')) {
                    break;
                }
            }
            let body = r#"{"accessToken":"fresh-access","refreshToken":"fresh-refresh","deviceId":"dev-1","accessExpiresAt":"2099-01-01T00:00:00Z"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).unwrap();
            String::from_utf8_lossy(&req).to_string()
        });

        let state = build_state("refresh-stale-bearer");
        state.config.lock().unwrap().share_base_url = format!("http://{addr}");
        *state.account_session.lock().unwrap() = Some(crate::share::AccountSession {
            account_id: "acct-1".into(),
            email: "user@example.com".into(),
            device_id: "dev-1".into(),
            mk: Zeroizing::new([7u8; 32]),
            generation: 1,
            access_token: "stale-access".into(),
            // Long past expiry ⇒ `access_token_needs_refresh` fires ⇒ the refresh MUST happen.
            access_expires_at: Some("2020-01-01T00:00:00Z".into()),
            refresh_token: "r0".into(),
        });

        let token = block_on(valid_access_token(&state)).unwrap();
        assert_eq!(
            token, "fresh-access",
            "the bearer must be the ROTATED token, not the dead cached one (the 0.7.2 defang bug)"
        );

        let req = server.join().unwrap();
        assert!(
            req.contains("POST /v1/auth/refresh"),
            "must redeem the refresh token at /v1/auth/refresh, got: {req}"
        );
        assert!(req.contains("r0"), "must present the CURRENT refresh token");

        // RAM-first rotation: the fresh pair is live in the session immediately.
        let g = state.account_session.lock().unwrap();
        let s = g.as_ref().unwrap();
        assert_eq!(s.access_token, "fresh-access");
        assert_eq!(
            s.refresh_token, "fresh-refresh",
            "the rotated refresh token must land in the in-RAM session (RAM-first)"
        );
        assert_eq!(s.access_expires_at.as_deref(), Some("2099-01-01T00:00:00Z"));
    }

    /// A TRANSIENT refresh failure (server unreachable) must NOT kill the session: the cached bearer
    /// comes back (it may still be inside the 120 s skew; a genuinely dead one fails closed at the
    /// server) and the session — including the biometric restore path's state — stays intact. This is
    /// the OTHER half of the 0.7.1 (#205) regression, where any refresh hiccup wiped the session.
    #[test]
    fn transient_refresh_failure_keeps_the_cached_session() {
        // A bound-then-dropped port: connecting fails fast (connection refused), no server runs.
        let dead_addr = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };

        let state = build_state("refresh-transient");
        state.config.lock().unwrap().share_base_url = format!("http://{dead_addr}");
        *state.account_session.lock().unwrap() = Some(crate::share::AccountSession {
            account_id: "acct-1".into(),
            email: "user@example.com".into(),
            device_id: "dev-1".into(),
            mk: Zeroizing::new([7u8; 32]),
            generation: 1,
            access_token: "cached-access".into(),
            access_expires_at: Some("2020-01-01T00:00:00Z".into()),
            refresh_token: "r0".into(),
        });

        let token = block_on(valid_access_token(&state)).unwrap();
        assert_eq!(
            token, "cached-access",
            "a transient refresh failure must fall back to the cached bearer, not error"
        );

        // The session survives untouched — nothing cleared, refresh token still spendable.
        let g = state.account_session.lock().unwrap();
        let s = g.as_ref().expect("session must NOT be cleared on a transient failure");
        assert_eq!(s.refresh_token, "r0");
    }

    /// The pure failure policy: a DEFINITIVE `Auth` refusal propagates (the session is dead and
    /// `refresh_session` already cleared it — the FE must route to sign-in); every other failure
    /// domain falls back to the cached bearer.
    #[test]
    fn refresh_failure_fallback_policy() {
        let auth = refresh_failure_fallback(AppError::Auth("refused".into()), "cached".into());
        assert!(matches!(auth, Err(AppError::Auth(_))));
        let net =
            refresh_failure_fallback(AppError::Unavailable("offline".into()), "cached".into());
        assert_eq!(net.unwrap(), "cached");
        let storage = refresh_failure_fallback(AppError::Storage("disk".into()), "cached".into());
        assert_eq!(storage.unwrap(), "cached");
    }

    // ── M5-CLIENT (mode B) tests ──────────────────────────────────────────────────────────────────

    use crate::e2ee::keys::{derive_identity, generate_master_key, IdentityKeypair};
    use crate::e2ee::wrap::{pack_wrapped_key, seal_to_recipient, UnpackedGrant};
    use crate::e2ee::{key_fingerprint, random_key32, seal_content};
    use murmur_protocol::envelope::ShareEnvelope;

    /// Build a `(sender, recipient)` identity pair from two fixed MKs (deterministic, no network).
    fn mode_b_pair() -> (IdentityKeypair, [u8; 32], IdentityKeypair, [u8; 32]) {
        let sender_mk = *generate_master_key().unwrap();
        let recip_mk = *generate_master_key().unwrap();
        let sender = derive_identity(&sender_mk, "sender@acct", 1).unwrap();
        let recipient = derive_identity(&recip_mk, "recipient@acct", 1).unwrap();
        (sender, sender_mk, recipient, recip_mk)
    }

    /// Craft the accept-side inputs a real inbox item + blob would carry: a valid grant sealed by
    /// `sender` to `recipient`, packed into the opaque `wrapped_key`, plus the content cell `C`.
    fn craft_valid_grant(
        sender: &IdentityKeypair,
        recipient: &IdentityKeypair,
        env: &ShareEnvelope,
        share_id: &str,
        rev: u32,
    ) -> (UnpackedGrant, Vec<u8>, String) {
        let nk = random_key32().unwrap();
        let content = seal_content(&nk, env, share_id, rev).unwrap();
        let sender_fp = key_fingerprint(&sender.pk_enc, &sender.pk_sig);
        let recipient_fp = key_fingerprint(&recipient.pk_enc, &recipient.pk_sig);
        let grant = seal_to_recipient(
            &nk,
            &content,
            &recipient.pk_enc,
            &recipient_fp,
            sender,
            &sender_fp,
            1,
            share_id,
            rev,
        )
        .unwrap();
        let blob = pack_wrapped_key(&sender.pk_enc, &sender.pk_sig, &grant).unwrap();
        let up = crate::e2ee::wrap::unpack_wrapped_key(&blob, &grant.signature).unwrap();
        (up, content, sender_fp)
    }

    /// HIGH-BAR: `accept_share` refuses a SEALED (not-session-unlocked) target folder with `Locked`,
    /// BEFORE any inbox/network/decrypt — RED-before-GREEN for the vault write-gate. Runs with NO
    /// server + NO login, so a non-`Locked` return would mean the gate wasn't first.
    #[test]
    fn accept_share_refuses_a_sealed_target_folder() {
        let state = build_state("accept-sealed");
        make_open_folder(&state.db, "f-lock", "Secret");
        state
            .db
            .set_folder_locked("f-lock", true, Some(&b"wrapped"[..]))
            .unwrap();
        // NOT session-unlocked.
        let before = state.db.list_meetings(1000).unwrap().len();
        let err = block_on(accept_share_inner(
            &state,
            "share-xyz".to_string(),
            Some("f-lock".to_string()),
        ))
        .unwrap_err();
        assert!(
            matches!(err, AppError::Locked(_)),
            "sealed target must fail Locked, got {err:?}"
        );
        assert_eq!(
            state.db.list_meetings(1000).unwrap().len(),
            before,
            "no meeting written"
        );
        assert!(
            state
                .db
                .inbound_share_meeting("share-xyz")
                .unwrap()
                .is_none(),
            "no inbound record"
        );
    }

    /// The happy path: a VALID grant → verify + decrypt + ingest writes exactly one `Exported` meeting
    /// with a `"shared"` note carrying provenance frontmatter (mode-B seal→open round-trip through the
    /// command's ingest layer).
    #[test]
    fn accept_ingest_writes_a_verified_shared_note() {
        let state = build_state("accept-happy");
        let target = get_or_create_shared_folder(&state).unwrap();
        let (sender, _smk, recipient, _rmk) = mode_b_pair();
        let env = ShareEnvelope::new(
            "Q3 Strategy",
            "- ship the thing\n- talk to Alice",
            "2026-07-04T10:00:00Z",
        );
        let (up, content, sender_fp) = craft_valid_grant(&sender, &recipient, &env, "share-1", 1);

        let res = accept_ingest_verified(
            &state,
            &target,
            &recipient,
            &sender_fp,
            "sender-uuid",
            &up,
            &content,
            "share-1",
            1,
            1,
        )
        .unwrap();
        assert_eq!(res.title, "Q3 Strategy");
        let note = state
            .db
            .get_note(&res.meeting_id, "shared")
            .unwrap()
            .unwrap();
        assert!(
            note.markdown.contains("ship the thing"),
            "body ingested: {}",
            note.markdown
        );
        assert!(
            note.markdown.contains("shared-by: "),
            "provenance frontmatter present"
        );
        assert!(
            note.markdown.contains("share-id: share-1"),
            "share-id provenance present"
        );
        // Idempotency record written; the meeting is a normal Exported row in the target folder.
        assert_eq!(
            state
                .db
                .inbound_share_meeting("share-1")
                .unwrap()
                .as_deref(),
            Some(res.meeting_id.as_str())
        );
        let m = state.db.get_meeting(&res.meeting_id).unwrap().unwrap();
        assert!(matches!(m.status, MeetingStatus::Exported));
        assert!(m.audio_path.is_none(), "shared notes carry no audio");
    }

    /// The full `accept_share_inner` is idempotent on `share_id`: a second call after a successful
    /// ingest returns the SAME meeting and writes no duplicate (short-circuits before any network).
    #[test]
    fn accept_share_is_idempotent_on_share_id() {
        let state = build_state("accept-idem");
        let target = get_or_create_shared_folder(&state).unwrap();
        let (sender, _s, recipient, _r) = mode_b_pair();
        let env = ShareEnvelope::new("Dup", "body", "2026-07-04T10:00:00Z");
        let (up, content, sender_fp) = craft_valid_grant(&sender, &recipient, &env, "share-dup", 1);
        let first = accept_ingest_verified(
            &state,
            &target,
            &recipient,
            &sender_fp,
            "s",
            &up,
            &content,
            "share-dup",
            1,
            1,
        )
        .unwrap();
        let count = state.db.list_meetings(1000).unwrap().len();
        // The command-level idempotency short-circuit returns the existing meeting with no network.
        let again = block_on(accept_share_inner(&state, "share-dup".to_string(), None)).unwrap();
        assert_eq!(again.meeting_id, first.meeting_id, "same meeting returned");
        assert_eq!(
            state.db.list_meetings(1000).unwrap().len(),
            count,
            "no duplicate meeting"
        );
    }

    /// §4.8 BINDING through the command layer: an UNSIGNED (zeroed-sig) grant, a TAMPERED content
    /// cell, and a REPLAY to the wrong recipient are ALL rejected `InvalidArg` and write NOTHING.
    #[test]
    fn accept_ingest_rejects_unsigned_tampered_and_replayed_grants() {
        let state = build_state("accept-reject");
        let target = get_or_create_shared_folder(&state).unwrap();
        let (sender, _s, recipient, _r) = mode_b_pair();
        let env = ShareEnvelope::new("t", "body", "2026-07-04T10:00:00Z");

        // (a) Unsigned: zero the grant signature.
        let (mut up, content, sender_fp) = craft_valid_grant(&sender, &recipient, &env, "s-a", 1);
        up.grant.signature = vec![0u8; 64];
        let e = accept_ingest_verified(
            &state, &target, &recipient, &sender_fp, "s", &up, &content, "s-a", 1, 1,
        )
        .unwrap_err();
        assert!(
            matches!(e, AppError::InvalidArg(_)),
            "unsigned must be rejected"
        );

        // (b) Tampered content: a different cell than the one the grant's hash binds.
        let (up, _c, sender_fp) = craft_valid_grant(&sender, &recipient, &env, "s-b", 1);
        let evil = seal_content(
            &random_key32().unwrap(),
            &ShareEnvelope::new("t", "EVIL", "2026-07-04T10:00:00Z"),
            "s-b",
            1,
        )
        .unwrap();
        let e = accept_ingest_verified(
            &state, &target, &recipient, &sender_fp, "s", &up, &evil, "s-b", 1, 1,
        )
        .unwrap_err();
        assert!(
            matches!(e, AppError::InvalidArg(_)),
            "tampered cell must be rejected"
        );

        // (c) Replay to the WRONG recipient (grant addressed to `recipient`, opened by `attacker`).
        let attacker =
            derive_identity(&generate_master_key().unwrap(), "attacker@acct", 1).unwrap();
        let (up, content, sender_fp) = craft_valid_grant(&sender, &recipient, &env, "s-c", 1);
        let e = accept_ingest_verified(
            &state, &target, &attacker, &sender_fp, "s", &up, &content, "s-c", 1, 1,
        )
        .unwrap_err();
        assert!(
            matches!(e, AppError::InvalidArg(_)),
            "replay to a different recipient must be rejected"
        );

        // NONE of the three wrote a meeting or an inbound record.
        assert_eq!(
            state.db.list_meetings(1000).unwrap().len(),
            0,
            "no meeting written for any rejected grant"
        );
        for sid in ["s-a", "s-b", "s-c"] {
            assert!(
                state.db.inbound_share_meeting(sid).unwrap().is_none(),
                "no inbound record for {sid}"
            );
        }
    }

    /// TOFU: first contact PINS (on the account_id, not email); the SAME fingerprint proceeds; a
    /// CHANGED fingerprint for the same contact is a BLOCK (never a silent overwrite).
    #[test]
    fn tofu_first_contact_pins_and_a_changed_fingerprint_blocks() {
        let state = build_state("tofu");
        let acct = "alice@example.com";
        // First contact: no pin yet.
        assert!(matches!(
            tofu_check(&state.db, acct, "FP-ORIGINAL").unwrap(),
            TofuState::FirstContact
        ));
        state
            .db
            .pin_contact(
                acct,
                Some("Alice@Example.com"),
                "FP-ORIGINAL",
                "2026-07-04T00:00:00Z",
            )
            .unwrap();
        // Same key → Match.
        assert!(matches!(
            tofu_check(&state.db, acct, "FP-ORIGINAL").unwrap(),
            TofuState::Match
        ));
        // A different fingerprint for the SAME account → Changed (blocking).
        assert!(matches!(
            tofu_check(&state.db, acct, "FP-DIFFERENT").unwrap(),
            TofuState::Changed
        ));
        // The pin persisted the ORIGINAL fingerprint (a Changed check does not overwrite it).
        assert_eq!(
            state.db.get_pinned_contact(acct).unwrap().unwrap().1,
            "FP-ORIGINAL"
        );
    }

    /// The mode-B share request carries NO note title or body — only ciphertext + wrapped keys + the
    /// recipient email. Seal a note with distinctive plaintext, build the exact `POST /v1/shares`
    /// body, serialize it, and assert the plaintext never appears.
    #[test]
    fn share_to_user_request_leaks_no_note_content() {
        let (sender, _s, recipient, _r) = mode_b_pair();
        let nk = random_key32().unwrap();
        let title = "TOPSECRET_TITLE_ZZZ";
        let body = "TOPSECRET_BODY_QQQ meeting minutes";
        let env = ShareEnvelope::new(title, body, "2026-07-04T10:00:00Z");
        let content = seal_content(&nk, &env, "s-leak", 1).unwrap();
        let sender_fp = key_fingerprint(&sender.pk_enc, &sender.pk_sig);
        let recipient_fp = key_fingerprint(&recipient.pk_enc, &recipient.pk_sig);
        let grant = seal_to_recipient(
            &nk,
            &content,
            &recipient.pk_enc,
            &recipient_fp,
            &sender,
            &sender_fp,
            1,
            "s-leak",
            1,
        )
        .unwrap();
        let wrapped = pack_wrapped_key(&sender.pk_enc, &sender.pk_sig, &grant).unwrap();
        let recipients = vec![murmur_protocol::dto::ShareRecipientInput {
            email: "bob@example.com".to_string(),
            wrapped_key: Some(wrapped),
            key_generation: Some(1),
            grant_sig: Some(grant.signature),
        }];
        let req = assemble_user_share_request("s-leak", 1, content, recipients, None);
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains(title),
            "title must never appear in the request body"
        );
        assert!(
            !json.contains("TOPSECRET_BODY_QQQ"),
            "note body must never appear in the request body"
        );
        assert!(
            json.contains("bob@example.com"),
            "the recipient email is present (allowed metadata)"
        );
    }

    /// MANUAL voice command: with NO recording in progress, arming reports `listening:false`
    /// ("not recording") and leaves the capture state empty — the live loop (which only runs while
    /// recording) would never consume it, so we must not pretend to listen.
    #[test]
    fn begin_voice_command_not_recording_does_not_arm() {
        let state = build_state("voicecmd-notrec");
        assert!(
            state.recorder.lock().unwrap().is_none(),
            "precondition: not recording"
        );

        let res = begin_voice_command_inner(&state).unwrap();
        assert!(!res.listening, "must not listen when not recording");
        assert_eq!(res.reason.as_deref(), Some("not recording"));
        assert!(
            state.voice_command_capture.lock().unwrap().is_none(),
            "no capture must be armed when not recording"
        );
    }

    /// MANUAL voice command: arming sets a fresh full-budget [`crate::state::CaptureState`] on the
    /// state (the live loop reads it). We exercise the arming half directly (a live `Recorder` needs
    /// a real audio device); the decision/dispatch half is unit-tested in `transcribe::live`.
    #[test]
    fn begin_voice_command_arms_capture_state() {
        use crate::state::CaptureState;
        let state = build_state("voicecmd-arm");
        // Simulate the arm-while-recording write the inner does once the recorder gate passes.
        {
            let mut g = state.voice_command_capture.lock().unwrap();
            *g = Some(CaptureState::armed());
        }
        let armed = *state.voice_command_capture.lock().unwrap();
        assert_eq!(
            armed,
            Some(CaptureState {
                budget: CaptureState::DEFAULT_BUDGET,
                start_sample: None,
                ended: false
            }),
            "arming must store a fresh full-budget capture the live loop can consume"
        );
    }

    /// CLICK-TO-STOP: `end_voice_command` on an ARMED capture flips `ended` so the live loop
    /// dispatches the FULL accumulated utterance on its next tick — it does NOT clear the capture
    /// itself (the live loop owns the terminal clear-on-dispatch).
    #[test]
    fn end_voice_command_flags_armed_capture_to_dispatch() {
        use crate::state::CaptureState;
        let state = build_state("voicecmd-end-armed");
        {
            let mut g = state.voice_command_capture.lock().unwrap();
            *g = Some(CaptureState::armed_from(1000));
        }

        let res = end_voice_command_inner(&state).unwrap();
        assert!(res.stopped, "an armed capture must report stopped:true");

        let after = state
            .voice_command_capture
            .lock()
            .unwrap()
            .expect("capture still present");
        assert!(
            after.ended,
            "the user's stop must flip `ended` so the live loop dispatches"
        );
        assert_eq!(
            after.start_sample,
            Some(1000),
            "the latched offset is preserved"
        );
    }

    /// CLICK-TO-STOP: `end_voice_command` on a NOT-armed state (double-click, already auto-stopped at
    /// the backstop, or never started) is a graceful no-op (`stopped: false`), never an error.
    #[test]
    fn end_voice_command_not_armed_is_graceful_noop() {
        let state = build_state("voicecmd-end-noop");
        assert!(
            state.voice_command_capture.lock().unwrap().is_none(),
            "precondition: not armed"
        );

        let res = end_voice_command_inner(&state).unwrap();
        assert!(
            !res.stopped,
            "a not-armed end must be a graceful no-op (stopped:false)"
        );
        assert!(
            state.voice_command_capture.lock().unwrap().is_none(),
            "a no-op end must not fabricate a capture"
        );
    }

    fn seed_meeting(db: &Db, mid: &str, markdown: &str, folder_id: Option<&str>) {
        db.insert_meeting(&Meeting {
            id: mid.to_string(),
            started_at: "2026-06-27T09:00:00Z".to_string(),
            ended_at: None,
            title: Some("Quarterly strategy".to_string()),
            duration_s: 600,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None, // association lives on notes; set via set_meeting_folder below
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: mid.to_string(),
            provider_id: "claude_code".to_string(),
            markdown: markdown.to_string(),
            created_at: "2026-06-27T09:05:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.insert_segments(
            mid,
            &[
                Segment {
                    idx: 0,
                    start_s: 0.0,
                    end_s: 2.0,
                    text: "alpha bravo".to_string(),
                    speaker: None,
                    confidence: None,
                },
                Segment {
                    idx: 1,
                    start_s: 2.0,
                    end_s: 4.0,
                    text: "charlie delta".to_string(),
                    speaker: None,
                    confidence: None,
                },
            ],
        )
        .unwrap();
        db.set_timeline_data(mid, "{\"topics\":[],\"speakers\":[]}")
            .unwrap();
        db.set_meeting_folder(mid, folder_id).unwrap();
    }

    fn make_open_folder(db: &Db, id: &str, path: &str) {
        db.insert_folder(&Folder {
            id: id.to_string(),
            name: path.to_string(),
            path: path.to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-27T08:00:00Z".to_string(),
        })
        .unwrap();
    }

    /// Seed a meeting with an explicit title + audio path into a folder (association lives on the
    /// note's `folder_id`, resolved back by `list_meetings`).
    fn seed_titled_meeting(db: &Db, mid: &str, title: &str, audio: &str, folder_id: &str) {
        db.insert_meeting(&Meeting {
            id: mid.to_string(),
            started_at: "2026-07-04T09:00:00Z".to_string(),
            ended_at: None,
            title: Some(title.to_string()),
            duration_s: 600,
            audio_path: Some(audio.to_string()),
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: mid.to_string(),
            provider_id: "claude_code".to_string(),
            markdown: "# note".to_string(),
            created_at: "2026-07-04T09:05:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_meeting_folder(mid, Some(folder_id)).unwrap();
    }

    /// #2 (0.7 security fast-follow): `list_meetings` MUST mask a sealed-and-NOT-session-unlocked
    /// meeting at the BACKEND — the real AI title + `.enc` `audio_path` may not cross IPC (the Library
    /// lock gate is enforced in code, not trusted to the FE). This is the RED-before-GREEN regression:
    /// on the unpatched command a sealed row still carried "Board strategy" + its `.enc` path. The
    /// `folder_id` stays (the FE keys the lock badge off it), an OPEN-folder meeting is untouched, and
    /// a session-unlock reveals the real fields again (masking is reversible, never lossy).
    #[test]
    fn list_meetings_masks_a_sealed_meeting_at_the_backend() {
        let state = build_state("list-mask");
        let db = &state.db;
        make_open_folder(db, "f-sealed", "Secret");
        make_open_folder(db, "f-open", "Standups");
        seed_titled_meeting(db, "m-sealed", "Board strategy", "/data/m-sealed.wav.enc", "f-sealed");
        seed_titled_meeting(db, "m-open", "Open standup", "/data/m-open.wav", "f-open");
        db.set_folder_locked("f-sealed", true, Some(&b"wrapped"[..]))
            .unwrap();
        // NOT session-unlocked: `unlocked_folders` stays empty.

        let masked =
            mask_locked_meetings(&state, db.list_meetings(200).unwrap()).unwrap();
        let sealed = masked.iter().find(|m| m.id == "m-sealed").unwrap();
        assert_eq!(
            sealed.title.as_deref(),
            Some("🔒 Locked"),
            "the real AI title must be masked at the backend for a sealed meeting"
        );
        assert_eq!(
            sealed.audio_path, None,
            "the .enc audio_path must be nulled so nothing can feed convertFileSrc"
        );
        assert_eq!(
            sealed.folder_id.as_deref(),
            Some("f-sealed"),
            "folder_id is preserved so the FE still renders the lock badge"
        );

        // The open-folder meeting is untouched.
        let open = masked.iter().find(|m| m.id == "m-open").unwrap();
        assert_eq!(open.title.as_deref(), Some("Open standup"));
        assert_eq!(open.audio_path.as_deref(), Some("/data/m-open.wav"));

        // Session-unlock the sealed folder → the real title + audio path come back (reversible).
        state
            .unlocked_folders
            .lock()
            .unwrap()
            .insert("f-sealed".to_string());
        let unmasked =
            mask_locked_meetings(&state, db.list_meetings(200).unwrap()).unwrap();
        let now = unmasked.iter().find(|m| m.id == "m-sealed").unwrap();
        assert_eq!(now.title.as_deref(), Some("Board strategy"));
        assert_eq!(now.audio_path.as_deref(), Some("/data/m-sealed.wav.enc"));
    }

    /// WS6 (Tier 4b) — the NEW structured, egress-free `get_person_dossier` command inherits the
    /// dossier visibility gate through `get_person_dossier_inner`: with a folder sealed and NOT
    /// session-unlocked, its meeting + commitment contribute NOTHING to the dossier, and both reappear
    /// only once the folder id is inserted into the live `unlocked_folders` set. An unknown entity id
    /// → `AppError::InvalidArg` (unknown vs sealed-only indistinguishable — no existence leak). This
    /// is the command-seam mirror of `dossier::build_dossier_gates_sealed_and_filters_commitments`.
    #[test]
    fn get_person_dossier_gates_sealed() {
        let state = build_state("person-dossier");
        let db = &state.db;
        make_open_folder(db, "f-lock", "Locked");
        seed_meeting(
            db,
            "open1",
            "## Action items\n- [ ] Anna — draft Atlas spec 2026-07-01\n",
            None,
        );
        seed_meeting(
            db,
            "sealedX",
            "LOCKED Atlas price\n## Action items\n- [ ] Carol — sign 2026-07-09\n",
            Some("f-lock"),
        );
        let atlas = db
            .upsert_entity("Atlas", crate::storage::models::EntityKind::Project)
            .unwrap();
        db.add_mention(&atlas, "open1").unwrap();
        db.add_mention(&atlas, "sealedX").unwrap();
        db.set_folder_locked("f-lock", true, Some(&b"wrapped"[..]))
            .unwrap();

        // Sealed + not session-unlocked: the sealed meeting + its commitment MUST be invisible.
        let data = get_person_dossier_inner(&state, &atlas).unwrap();
        assert!(
            data.meetings.iter().any(|m| m.meeting_id == "open1"),
            "the visible mentioning meeting must be present"
        );
        assert!(
            data.meetings.iter().all(|m| m.meeting_id != "sealedX"),
            "sealed-not-unlocked meeting leaked into the dossier (gate violation)"
        );
        assert!(
            data.commitments
                .iter()
                .any(|c| c.text.contains("draft Atlas spec")),
            "the visible commitment must be present"
        );
        assert!(
            data.commitments.iter().all(|c| !c.text.contains("sign")),
            "sealed-not-unlocked commitment leaked (gate violation)"
        );

        // Session-unlock the folder → the sealed meeting + its commitment reappear (reversible gate).
        state
            .unlocked_folders
            .lock()
            .unwrap()
            .insert("f-lock".to_string());
        let data2 = get_person_dossier_inner(&state, &atlas).unwrap();
        assert!(
            data2.meetings.iter().any(|m| m.meeting_id == "sealedX"),
            "session-unlock must reveal the sealed meeting"
        );
        assert!(
            data2.commitments.iter().any(|c| c.text.contains("sign")),
            "session-unlock must reveal the sealed commitment"
        );

        // Unknown id → InvalidArg (unknown vs sealed-only indistinguishable; no existence leak).
        match get_person_dossier_inner(&state, "no-such-entity") {
            Err(AppError::InvalidArg(_)) => {}
            other => panic!("unknown entity must be InvalidArg, got {other:?}"),
        }
    }

    /// WS6 (Tier 4b) DTO contract lock: the serialized `DossierData` the FE receives exposes
    /// `entity`/`meetings`/`commitments`/`neighbors`/`facts` in camelCase and OMITS `corpus`
    /// (`#[serde(skip)]`) — so meeting note bodies never cross IPC. Guards both the FE field contract
    /// and the leak invariant.
    #[test]
    fn get_person_dossier_dto_omits_corpus_and_is_camelcase() {
        let state = build_state("person-dossier-dto");
        let db = &state.db;
        seed_meeting(
            db,
            "m-open",
            "## Action items\n- [ ] Anna — draft Atlas spec 2026-07-01\n",
            None,
        );
        let atlas = db
            .upsert_entity("Atlas", crate::storage::models::EntityKind::Project)
            .unwrap();
        db.add_mention(&atlas, "m-open").unwrap();

        let data = get_person_dossier_inner(&state, &atlas).unwrap();
        let v = serde_json::to_value(&data).unwrap();
        let obj = v.as_object().expect("dossier serializes to a JSON object");
        for key in ["entity", "meetings", "commitments", "neighbors", "facts"] {
            assert!(obj.contains_key(key), "DTO must expose `{key}`");
        }
        assert!(
            !obj.contains_key("corpus"),
            "corpus must be serde-skipped — note bodies must never cross IPC to the FE"
        );
        // Nested camelCase the FE consumes (a commitment carries meetingId/meetingTitle/dueDate).
        let commit = v["commitments"]
            .get(0)
            .expect("the visible commitment must serialize");
        assert!(
            commit.get("meetingId").is_some(),
            "commitment.meetingId must be camelCase"
        );
        assert!(
            commit.get("meeting_id").is_none(),
            "snake_case commitment.meeting_id must not appear"
        );
    }

    /// Add a user-memory fact sourced from `meeting_id` (the gating + purge anchor) for the injection
    /// tests below.
    fn add_user_fact(db: &Db, meeting_id: &str, object: &str) {
        db.apply_user_fact_ops(&[crate::facts::FactOp::Add(crate::facts::NewFact {
            entity_id: crate::user_memory::USER_SCOPE.to_string(),
            subject: "You".into(),
            predicate: "prefer".into(),
            object: object.into(),
            valid_from: "2026-06-27T09:00:00Z".into(),
            recorded_at: "2026-06-27T09:00:00Z".into(),
            confidence: 1.0,
            meeting_id: Some(meeting_id.to_string()),
        })])
        .unwrap();
    }

    /// LOCK INVARIANT for the Ask surface's memory injection (RED-before-GREEN, task Phase-1 Ask):
    /// a user-memory fact whose SOURCE meeting is sealed-and-not-session-unlocked does NOT appear in
    /// the `ask_vault` system prompt, and an EMPTY memory brief yields a BYTE-IDENTICAL prompt. Before
    /// the visibility gate on `gated_memory_brief_for_injection` (deleting the
    /// `list_user_facts_visible` predicate) this FAILS — the sealed fact leaks into the Ask prompt.
    #[test]
    fn ask_vault_memory_brief_is_gated_and_empty_is_byte_identical() {
        let state = build_state("ask-mem-gate");
        make_open_folder(&state.db, "f1", "Secret");
        seed_meeting(&state.db, "m1", "We shipped the beta.", None);
        state.db.set_meeting_folder("m1", Some("f1")).unwrap();
        add_user_fact(&state.db, "m1", "Polish replies");

        // A grounding corpus + the (byte-identical) EMPTY-brief prompt to compare against.
        let corpus = "### [[Sync]] · 2026-07-01 · id:m1\nWe shipped the beta.";
        let (want_empty_system, _) =
            crate::summarize::vault_chat::build(corpus, &[], "what did we ship?", "");

        // OPEN folder: the fact is VISIBLE → the gated brief carries it → it reaches the Ask prompt.
        let open = unlocked_snapshot(&state).unwrap();
        let brief_open = gated_memory_brief_for_injection(&state, &open);
        assert!(
            brief_open.contains("Polish replies"),
            "an open-folder user fact must be in the brief"
        );
        let (sys_open, _) =
            crate::summarize::vault_chat::build(corpus, &[], "what did we ship?", &brief_open);
        assert!(
            sys_open.contains("Polish replies"),
            "and reach the Ask system prompt when open"
        );
        assert_ne!(
            sys_open, want_empty_system,
            "a present brief changes the prompt"
        );

        // SEAL the folder (session NOT unlocked). Flip the flag only (the real seal purges the fact);
        // here we prove the READ GATE hides a fact whose row still exists at rest.
        state
            .db
            .set_folder_locked("f1", true, Some(&b"wrapped"[..]))
            .unwrap();
        let sealed = unlocked_snapshot(&state).unwrap();
        let brief_sealed = gated_memory_brief_for_injection(&state, &sealed);
        assert!(
            brief_sealed.is_empty(),
            "a sealed-source user fact must NOT be in the injected brief"
        );
        let (sys_sealed, _) =
            crate::summarize::vault_chat::build(corpus, &[], "what did we ship?", &brief_sealed);
        assert!(
            !sys_sealed.contains("Polish replies"),
            "the sealed fact must not reach the Ask prompt"
        );
        // Empty brief ⇒ the prompt is BYTE-IDENTICAL to the pre-memory prompt.
        assert_eq!(
            sys_sealed, want_empty_system,
            "empty memory ⇒ byte-identical Ask prompt"
        );

        // A SESSION UNLOCK re-admits it (reversible gate).
        state
            .unlocked_folders
            .lock()
            .unwrap()
            .insert("f1".to_string());
        let unlocked = unlocked_snapshot(&state).unwrap();
        assert!(
            gated_memory_brief_for_injection(&state, &unlocked).contains("Polish replies"),
            "a session unlock must re-admit the user fact to the Ask brief"
        );

        // FLAG OFF: even the visible fact must NOT be injected when memory is disabled.
        state.config.lock().unwrap().user_memory_enabled = false;
        assert!(
            gated_memory_brief_for_injection(&state, &unlocked).is_empty(),
            "memory disabled must suppress the Ask brief even for a visible fact"
        );
        // And get_user_memory reports the explicit disabled marker (empty + disabled:true).
        let mem = get_user_memory_inner(&state).unwrap();
        assert!(
            mem.disabled,
            "disabled flag ⇒ the audit payload reports disabled"
        );
        assert!(
            mem.facts.is_empty() && mem.brief.is_empty(),
            "disabled ⇒ nothing surfaced"
        );
    }

    // ── VOICEPRINTS (Phase 2): matcher gating + enroll + management ───────────────────────────────

    /// CRITICAL LOCK INVARIANT (RED-before-GREEN): `suggest_speaker_labels` NEVER sources a
    /// suggestion from a SEALED voiceprint. A labeled prior in a locked-not-unlocked folder must NOT
    /// match the current meeting's cluster; a session unlock re-admits it. Before the
    /// `list_voiceprints_visible` gate (an ungated SELECT of labeled priors) this FAILS — the sealed
    /// person's name leaks as a suggestion.
    #[test]
    fn suggest_speaker_labels_never_uses_a_sealed_voiceprint() {
        let state = build_state("vp-suggest-gate");
        state.config.lock().unwrap().voiceprint_enabled = true;

        // A LABELED prior voiceprint ("Sarah") living in a folder we will SEAL.
        make_open_folder(&state.db, "f-secret", "Secret");
        seed_meeting(&state.db, "prior", "prior note", None);
        state
            .db
            .set_meeting_folder("prior", Some("f-secret"))
            .unwrap();
        let sarah = vec![1.0f32, 0.0, 0.0];
        state
            .db
            .insert_voiceprint(
                "vp-prior",
                "prior",
                0,
                Some("Sarah"),
                &sarah,
                "2026-07-01T00:00:00Z",
            )
            .unwrap();

        // The CURRENT (open) meeting has a cluster near-identical to Sarah's voiceprint.
        seed_meeting(&state.db, "cur", "current note", None);
        let cur_cluster = vec![0.99f32, 0.14, 0.0];
        state
            .db
            .insert_voiceprint(
                "vp-cur",
                "cur",
                1,
                None,
                &cur_cluster,
                "2026-07-02T00:00:00Z",
            )
            .unwrap();

        // OPEN prior → the suggestion surfaces (Sarah for others-1).
        let sugg_open = suggest_speaker_labels_inner(&state, "cur").unwrap();
        assert_eq!(
            sugg_open.len(),
            1,
            "an open labeled prior yields a suggestion"
        );
        assert_eq!(sugg_open[0].speaker, "others-1");
        assert_eq!(sugg_open[0].suggested_label, "Sarah");
        assert!(sugg_open[0].score >= 0.9);

        // SEAL the prior's folder (session NOT unlocked): the sealed labeled prior must vanish from
        // the candidate gallery → NO suggestion. RED here without the gate.
        state
            .db
            .set_folder_locked("f-secret", true, Some(&b"wrapped"[..]))
            .unwrap();
        let sugg_sealed = suggest_speaker_labels_inner(&state, "cur").unwrap();
        assert!(
            sugg_sealed.is_empty(),
            "a sealed voiceprint must never source a suggestion (leak)"
        );

        // A session unlock re-admits it (reversible gate).
        state
            .unlocked_folders
            .lock()
            .unwrap()
            .insert("f-secret".to_string());
        let sugg_unlocked = suggest_speaker_labels_inner(&state, "cur").unwrap();
        assert_eq!(
            sugg_unlocked.len(),
            1,
            "a session unlock re-admits the labeled prior"
        );
        assert_eq!(sugg_unlocked[0].suggested_label, "Sarah");
    }

    /// A LOCKED current meeting yields no suggestions (fail-closed READ-GATE), even with a strong
    /// visible labeled prior.
    #[test]
    fn suggest_speaker_labels_locked_current_meeting_is_empty() {
        let state = build_state("vp-suggest-locked-cur");
        state.config.lock().unwrap().voiceprint_enabled = true;

        seed_meeting(&state.db, "prior", "prior", None);
        let v = vec![1.0f32, 0.0, 0.0];
        state
            .db
            .insert_voiceprint(
                "vp-prior",
                "prior",
                0,
                Some("Sarah"),
                &v,
                "2026-07-01T00:00:00Z",
            )
            .unwrap();

        make_open_folder(&state.db, "f-cur", "Cur");
        seed_meeting(&state.db, "cur", "cur", None);
        state.db.set_meeting_folder("cur", Some("f-cur")).unwrap();
        state
            .db
            .insert_voiceprint("vp-cur", "cur", 1, None, &v, "2026-07-02T00:00:00Z")
            .unwrap();
        state
            .db
            .set_folder_locked("f-cur", true, Some(&b"wrapped"[..]))
            .unwrap();

        assert!(
            suggest_speaker_labels_inner(&state, "cur")
                .unwrap()
                .is_empty(),
            "a locked current meeting must surface no suggestions"
        );
    }

    /// ENROLL-ON-RENAME: renaming a diarized cluster (`others-1` → "Sarah") binds the label to that
    /// cluster's voiceprint row (opt-in on). A NON-cluster rename or a plain-`others` label enrolls
    /// nothing; and with the opt-in OFF nothing is enrolled.
    #[test]
    fn rename_speaker_enrolls_the_cluster_voiceprint() {
        let state = build_state("vp-enroll");
        state.config.lock().unwrap().voiceprint_enabled = true;
        seed_meeting(&state.db, "m1", "note", None);
        let emb = vec![0.6f32, 0.8];
        state
            .db
            .insert_voiceprint("vp1", "m1", 1, None, &emb, "2026-07-01T00:00:00Z")
            .unwrap();

        // Rename the diarized cluster others-1 → "Sarah": the voiceprint gets labeled.
        rename_speaker_inner(&state, "m1", "others-1", "Sarah").unwrap();
        let listed = list_voiceprints_inner(&state).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].label.as_deref(),
            Some("Sarah"),
            "enroll bound the label"
        );
        assert_eq!(listed[0].cluster_index, 1);

        // A rename of a PLAIN non-cluster label enrolls nothing (still one row, label unchanged).
        rename_speaker_inner(&state, "m1", "me", "Bob").unwrap();
        assert_eq!(
            list_voiceprints_inner(&state).unwrap()[0].label.as_deref(),
            Some("Sarah"),
            "renaming a non-cluster label does not touch a voiceprint"
        );
    }

    /// ENROLL is suppressed when the opt-in is OFF (no silent biometric binding).
    #[test]
    fn rename_speaker_does_not_enroll_when_opt_in_off() {
        let state = build_state("vp-enroll-off");
        state.config.lock().unwrap().voiceprint_enabled = false;
        seed_meeting(&state.db, "m1", "note", None);
        state
            .db
            .insert_voiceprint("vp1", "m1", 0, None, &[1.0f32, 0.0], "2026-07-01T00:00:00Z")
            .unwrap();

        rename_speaker_inner(&state, "m1", "others-0", "Sarah").unwrap();
        assert!(
            list_voiceprints_inner(&state).unwrap()[0].label.is_none(),
            "opt-in OFF ⇒ no enroll"
        );
    }

    /// Insert diarization-tagged segments + an LLM-DISPLAY-labeled timeline for a meeting (overwrites
    /// `seed_meeting`'s placeholder segments/timeline). Each `(start, end, tag)` segment carries the
    /// RAW diarization tag; each `(start, end, label)` turn carries the LLM DISPLAY label the FE lane
    /// renders — the two key spaces the reconciler must bridge.
    fn seed_diarized(
        db: &Db,
        mid: &str,
        segs: &[(f64, f64, &str)],
        turns: &[(f64, f64, &str)],
    ) {
        let segments: Vec<Segment> = segs
            .iter()
            .enumerate()
            .map(|(idx, (s, e, tag))| Segment {
                idx: idx as i64,
                start_s: *s,
                end_s: *e,
                text: "x".to_string(),
                speaker: Some((*tag).to_string()),
                confidence: None,
            })
            .collect();
        db.insert_segments(mid, &segments).unwrap();
        let speakers: String = turns
            .iter()
            .map(|(s, e, l)| format!("{{\"speaker\":\"{l}\",\"startS\":{s},\"endS\":{e}}}"))
            .collect::<Vec<_>>()
            .join(",");
        db.set_timeline_data(mid, &format!("{{\"topics\":[],\"speakers\":[{speakers}]}}"))
            .unwrap();
    }

    /// CRUX (RED-before-GREEN): the timeline is LLM-generated, so the FE lane's `speaker` is the
    /// DISPLAY label ("Speaker 2"), NOT the raw `others-N` tag. The suggestion MUST be keyed by that
    /// display label so `suggestionByLabel().get(lane.speaker)` matches and the "Looks like Anna?"
    /// chip renders. The OLD tag-keyed code emits `speaker == "others-1"` → the FE lookup misses →
    /// the chip never renders. This asserts the reconciled display-label key.
    #[test]
    fn suggest_speaker_labels_keys_by_llm_display_label() {
        let state = build_state("vp-suggest-display");
        state.config.lock().unwrap().voiceprint_enabled = true;

        // A prior LABELED voiceprint ("Anna").
        seed_meeting(&state.db, "prior", "prior", None);
        let anna = vec![1.0f32, 0.0, 0.0];
        state
            .db
            .insert_voiceprint("vp-prior", "prior", 0, Some("Anna"), &anna, "2026-07-01T00:00:00Z")
            .unwrap();

        // Current meeting: diarized cluster 1 (segments tagged `others-1`), whose timeline lane the
        // LLM labeled "Speaker 2"; its voiceprint is near-identical to Anna's.
        seed_meeting(&state.db, "cur", "cur", None);
        seed_diarized(
            &state.db,
            "cur",
            &[(0.0, 5.0, "others-1")],
            &[(0.0, 5.0, "Speaker 2")],
        );
        let cur = vec![0.99f32, 0.14, 0.0];
        state
            .db
            .insert_voiceprint("vp-cur", "cur", 1, None, &cur, "2026-07-02T00:00:00Z")
            .unwrap();

        let sugg = suggest_speaker_labels_inner(&state, "cur").unwrap();
        assert_eq!(sugg.len(), 1, "the prior match surfaces one suggestion");
        assert_eq!(
            sugg[0].speaker, "Speaker 2",
            "suggestion MUST be keyed by the DISPLAY label the FE lane shows (not the raw others-1 tag)"
        );
        assert_eq!(sugg[0].suggested_label, "Anna");
    }

    /// ENROLL via the DISPLAY label: the FE passes the lane label "Speaker 2" (not `others-1`), so
    /// enroll must reconstruct cluster 1 from segment↔turn overlap. The OLD
    /// `parse_others_cluster("Speaker 2") = None` code enrolls nothing (RED).
    #[test]
    fn rename_speaker_enrolls_via_display_label() {
        let state = build_state("vp-enroll-display");
        state.config.lock().unwrap().voiceprint_enabled = true;
        seed_meeting(&state.db, "m1", "note", None);
        seed_diarized(
            &state.db,
            "m1",
            &[(0.0, 5.0, "others-1")],
            &[(0.0, 5.0, "Speaker 2")],
        );
        state
            .db
            .insert_voiceprint("vp1", "m1", 1, None, &[0.6f32, 0.8], "2026-07-01T00:00:00Z")
            .unwrap();

        // Rename the DISPLAY-labeled lane "Speaker 2" → "Anna": cluster 1 gets enrolled.
        let tl = rename_speaker_inner(&state, "m1", "Speaker 2", "Anna").unwrap();
        assert!(
            tl.speakers.iter().any(|t| t.speaker == "Anna"),
            "the display rename still persists in the timeline"
        );
        let listed = list_voiceprints_inner(&state).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].label.as_deref(),
            Some("Anna"),
            "enroll reconstructed cluster 1 from the display label via overlap"
        );
        assert_eq!(listed[0].cluster_index, 1);
    }

    /// Single-cluster 1:1 end-to-end: plain `others` segments ↔ cluster 0, LLM lane "Speaker 1".
    /// Both suggest (keyed by "Speaker 1") and enroll (reconstruct cluster 0) work — and the 1:1 case
    /// is inferred from the SEGMENT tag shape, not the {0}-only voiceprint set.
    #[test]
    fn suggest_and_enroll_single_cluster_1to1() {
        let state = build_state("vp-1to1");
        state.config.lock().unwrap().voiceprint_enabled = true;

        // Prior labeled "Sam".
        seed_meeting(&state.db, "prior", "prior", None);
        let sam = vec![0.0f32, 1.0, 0.0];
        state
            .db
            .insert_voiceprint("vp-prior", "prior", 0, Some("Sam"), &sam, "2026-07-01T00:00:00Z")
            .unwrap();

        // Current single-cluster meeting: plain `others` segments, LLM lane "Speaker 1", cluster 0.
        seed_meeting(&state.db, "cur", "cur", None);
        seed_diarized(
            &state.db,
            "cur",
            &[(0.0, 6.0, "others"), (7.0, 9.0, "others")],
            &[(0.0, 9.0, "Speaker 1")],
        );
        let cur = vec![0.02f32, 0.99, 0.0];
        state
            .db
            .insert_voiceprint("vp-cur", "cur", 0, None, &cur, "2026-07-02T00:00:00Z")
            .unwrap();

        // Suggest is keyed by the display label.
        let sugg = suggest_speaker_labels_inner(&state, "cur").unwrap();
        assert_eq!(sugg.len(), 1);
        assert_eq!(sugg[0].speaker, "Speaker 1", "1:1 suggestion keyed by the display label");
        assert_eq!(sugg[0].suggested_label, "Sam");

        // Enroll via the display label reconstructs cluster 0.
        rename_speaker_inner(&state, "cur", "Speaker 1", "Sam").unwrap();
        let cur_row = list_voiceprints_inner(&state)
            .unwrap()
            .into_iter()
            .find(|v| v.meeting_id == "cur")
            .unwrap();
        assert_eq!(cur_row.label.as_deref(), Some("Sam"), "1:1 enroll bound cluster 0");
        assert_eq!(cur_row.cluster_index, 0);
    }

    /// Renaming a lane that maps to NO diarized cluster (the "me" lane / a non-diarized display label)
    /// enrolls NOTHING — no fabricated cluster — while a real diarized lane in the same meeting still
    /// enrolls. Guards against the reconciler over-reaching.
    #[test]
    fn rename_speaker_non_diarized_lane_enrolls_nothing() {
        let state = build_state("vp-me-lane");
        state.config.lock().unwrap().voiceprint_enabled = true;
        seed_meeting(&state.db, "m1", "note", None);
        // A diarized cluster 0 lane ("Speaker 1") AND a disjoint "me" lane the LLM labeled "Jakub".
        seed_diarized(
            &state.db,
            "m1",
            &[(0.0, 4.0, "others-0"), (4.0, 8.0, "me")],
            &[(0.0, 4.0, "Speaker 1"), (4.0, 8.0, "Jakub")],
        );
        state
            .db
            .insert_voiceprint("vp0", "m1", 0, None, &[1.0f32, 0.0], "2026-07-01T00:00:00Z")
            .unwrap();

        // Renaming the "me"-mapped lane "Jakub" enrolls nothing (it overlaps only "me"/None segments).
        rename_speaker_inner(&state, "m1", "Jakub", "Jakub K").unwrap();
        assert!(
            list_voiceprints_inner(&state).unwrap()[0].label.is_none(),
            "a non-diarized (me) lane must never fabricate/enroll a cluster"
        );

        // The real diarized lane still enrolls (positive control).
        rename_speaker_inner(&state, "m1", "Speaker 1", "Anna").unwrap();
        assert_eq!(
            list_voiceprints_inner(&state).unwrap()[0].label.as_deref(),
            Some("Anna"),
            "the diarized lane still enrolls its cluster"
        );
    }

    /// list/forget/clear management commands are gated (list) and idempotent (forget/clear).
    #[test]
    fn voiceprint_management_list_forget_clear() {
        let state = build_state("vp-manage");
        seed_meeting(&state.db, "m1", "note", None);
        seed_meeting(&state.db, "m2", "note2", None);
        state
            .db
            .insert_voiceprint(
                "vp1",
                "m1",
                0,
                Some("Sarah"),
                &[1.0f32, 0.0],
                "2026-07-01T00:00:00Z",
            )
            .unwrap();
        state
            .db
            .insert_voiceprint("vp2", "m2", 0, None, &[0.0f32, 1.0], "2026-07-02T00:00:00Z")
            .unwrap();

        let listed = list_voiceprints_inner(&state).unwrap();
        assert_eq!(listed.len(), 2, "both visible voiceprints listed");
        // The DTO carries no raw embedding — only label + provenance + dim.
        let sarah = listed.iter().find(|v| v.id == "vp1").unwrap();
        assert_eq!(sarah.label.as_deref(), Some("Sarah"));
        assert_eq!(sarah.dim, 2);

        // Forget one.
        state.db.delete_voiceprint("vp1").unwrap();
        assert_eq!(list_voiceprints_inner(&state).unwrap().len(), 1);
        // Clear all.
        state.db.clear_voiceprints().unwrap();
        assert!(list_voiceprints_inner(&state).unwrap().is_empty());
    }

    /// PR-A #3 belt-and-braces: sealing a folder while NO recording is active clears any stale
    /// live-transcript buffer (post clear-on-Stop it is normally already empty — idempotent
    /// hygiene so a stale tail can never outlive the folder it belongs to).
    #[test]
    fn lock_folder_clears_stale_live_transcript_when_not_recording() {
        let state = build_state("lock-clears-live");
        make_open_folder(&state.db, "f-lock", "Secret");
        seed_meeting(&state.db, "m1", "# note", Some("f-lock"));
        *state.live_transcript.lock().unwrap() =
            "stale tail of the just-recorded meeting".to_string();
        assert!(
            state.recorder.lock().unwrap().is_none(),
            "precondition: not recording"
        );

        lock_folder_inner(&state, "f-lock".to_string()).unwrap();

        assert!(
            state.live_transcript.lock().unwrap().is_empty(),
            "lock_folder with no active recording must clear the stale live-transcript buffer"
        );
    }

    /// PR-A #3 belt-and-braces: `relock_all` (manual "Lock all" + the screen-share auto-relock via
    /// `relock_all_inner`) clears the stale buffer too when no recording is active.
    #[test]
    fn relock_all_clears_stale_live_transcript_when_not_recording() {
        let state = build_state("relock-clears-live");
        *state.live_transcript.lock().unwrap() = "stale tail".to_string();
        assert!(
            state.recorder.lock().unwrap().is_none(),
            "precondition: not recording"
        );

        relock_all_inner(&state).unwrap();

        assert!(
            state.live_transcript.lock().unwrap().is_empty(),
            "relock_all with no active recording must clear the stale live-transcript buffer"
        );
    }

    /// brain2 realtime notes: with the meeting in an OPEN folder (or no folder) the gate is open, so
    /// `save`/`get` round-trip the buffer.
    #[test]
    fn manual_notes_save_get_round_trip_when_unlocked() {
        let state = build_state("manual-notes-open");
        seed_meeting(&state.db, "m1", "note", None); // no folder ⇒ always open

        get_manual_notes_inner(&state, "m1")
            .map(|s| assert_eq!(s, ""))
            .unwrap();
        save_manual_notes_inner(&state, "m1", "ship Friday; Anna owns QA").unwrap();
        assert_eq!(
            get_manual_notes_inner(&state, "m1").unwrap(),
            "ship Friday; Anna owns QA",
            "open-folder buffer round-trips through the gated commands"
        );
    }

    /// LOCK-GATE: on a sealed-and-NOT-session-unlocked meeting, `save_manual_notes` is refused with
    /// `AppError::Locked` (never resurrect typed plaintext behind a lock) and `get_manual_notes`
    /// returns "" (masked) EVEN THOUGH the column still holds content — never leaking the buffer.
    /// Unlocking the folder restores both read and write.
    #[test]
    fn manual_notes_save_refused_and_get_masked_when_sealed() {
        let state = build_state("manual-notes-sealed");
        make_open_folder(&state.db, "f-lock", "Secret");
        seed_meeting(&state.db, "m1", "note", Some("f-lock"));
        // The buffer holds content at rest (e.g. typed pre-seal); the gate must mask/refuse it.
        state
            .db
            .set_manual_notes("m1", "secret typed plaintext")
            .unwrap();
        // Seal the folder (locked, NOT in the session unlock set).
        state
            .db
            .set_folder_locked("f-lock", true, Some(&b"wrapped"[..]))
            .unwrap();

        // WRITE is refused with Locked.
        let err = save_manual_notes_inner(&state, "m1", "new secret").unwrap_err();
        assert!(
            matches!(err, AppError::Locked(_)),
            "sealed write must be AppError::Locked, got {err:?}"
        );
        // The refused write left the stored buffer untouched.
        assert_eq!(
            state.db.get_manual_notes("m1").unwrap(),
            "secret typed plaintext",
            "refused write must not mutate"
        );
        // READ is masked to "" despite the column holding content.
        assert_eq!(
            get_manual_notes_inner(&state, "m1").unwrap(),
            "",
            "sealed-not-unlocked read must mask the buffer, never leak it"
        );

        // Session-unlock the folder ⇒ both read and write work again.
        state
            .unlocked_folders
            .lock()
            .unwrap()
            .insert("f-lock".to_string());
        assert_eq!(
            get_manual_notes_inner(&state, "m1").unwrap(),
            "secret typed plaintext"
        );
        save_manual_notes_inner(&state, "m1", "edited after unlock").unwrap();
        assert_eq!(
            get_manual_notes_inner(&state, "m1").unwrap(),
            "edited after unlock"
        );
    }

    /// COUNTEREXAMPLE A (verify-before-destroy): the user's typed notes SURVIVE a real lock→unlock
    /// cycle — SEALED under the folder CK (plaintext blanked, blob present) on lock, RESTORED
    /// byte-identical on unlock. Drives the PRODUCTION seal (`lock_folder_inner` →
    /// `seal_meeting_extras`) and unseal (`unseal_folder_extras`), so the typed notes are never
    /// blanked-and-lost. RED on the prior blank-on-seal code (no blob → nothing to restore).
    #[test]
    fn manual_notes_survive_lock_unlock_cycle_sealed_and_restored() {
        let state = build_state("manual-notes-seal-cycle");
        make_open_folder(&state.db, "f-lock", "Secret");
        seed_meeting(&state.db, "m1", "# note", Some("f-lock"));
        let typed = "zażółć 🔒 DECISION: ship Friday; Anna owns QA sign-off";
        state.db.set_manual_notes("m1", typed).unwrap();

        // LOCK → production seal: plaintext blanked at rest, manual_notes_blob present.
        lock_folder_inner(&state, "f-lock".to_string()).unwrap();
        let sealed = state.db.raw_manual_notes("m1").unwrap().unwrap();
        assert_eq!(
            sealed.text, "",
            "typed notes plaintext blanked while sealed"
        );
        assert!(
            sealed.blob.is_some(),
            "typed notes sealed under the folder CK (blob present)"
        );

        // UNLOCK (mirror unlock_folder's internals): KEK → unwrap CK → unseal extras.
        let kek = secrets::get_or_create_master_kek().unwrap();
        let wrapped = state.db.folder_wrapped_key("f-lock").unwrap().unwrap();
        let ck_vec = crate::crypto::decrypt(&kek, &wrapped, &aad_wrapped_ck("f-lock")).unwrap();
        let ck: [u8; 32] = ck_vec.try_into().expect("CK is 32 bytes");
        unseal_folder_extras(&state, "f-lock", &ck, None).unwrap();

        assert_eq!(
            state.db.get_manual_notes("m1").unwrap(),
            typed,
            "typed notes restored byte-identical on unlock — sealed-and-restored, never lost"
        );
    }

    /// REMOVE-LOCK: permanently removing a folder's lock decrypts the typed notes back to plaintext
    /// and clears the blob — the typed notes are NEVER lost.
    #[test]
    fn manual_notes_survive_remove_lock_permanently() {
        let state = build_state("manual-notes-remove-lock");
        make_open_folder(&state.db, "f-lock", "Secret");
        seed_meeting(&state.db, "m1", "# note", Some("f-lock"));
        let typed = "permanent: revisit auth next sprint";
        state.db.set_manual_notes("m1", typed).unwrap();

        lock_folder_inner(&state, "f-lock".to_string()).unwrap();
        assert_eq!(
            state.db.raw_manual_notes("m1").unwrap().unwrap().text,
            "",
            "blanked while sealed"
        );

        // Cache the KEK for remove_lock (it reads the gated master KEK), then permanently remove.
        let kek = secrets::get_or_create_master_kek().unwrap();
        *state.master_kek.lock().unwrap() = Some(Zeroizing::new(kek));
        remove_lock_inner(&state, "f-lock".to_string()).unwrap();

        let rn = state.db.raw_manual_notes("m1").unwrap().unwrap();
        assert_eq!(
            rn.text, typed,
            "typed notes permanently restored to plaintext on remove-lock"
        );
        assert!(
            rn.blob.is_none(),
            "manual_notes_blob cleared on remove-lock"
        );
    }

    // ── document ingestion ──────────────────────────────────────────────────

    /// Write a temp md/txt file with `text`, return its absolute path. Cleaned up by the OS temp dir.
    fn write_temp_doc(tag: &str, ext: &str, text: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-doc-{tag}-{}-{}.{ext}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&p, text).unwrap();
        p
    }

    /// IMPORT round-trip: an md document uploaded into an OPEN folder is stored (text readable) and
    /// listed (metadata, no text). A .txt is accepted too. The doc-chunk rows exist (chunked) — and,
    /// on a no-model CI machine, carry NO vectors (stub vectors are never written).
    #[test]
    fn import_document_round_trip_md_and_txt_open_folder() {
        let state = build_state("doc-import");
        make_open_folder(&state.db, "f-open", "Project");

        let md = write_temp_doc(
            "md",
            "md",
            "# Spec\n\nThe budget is 100k.\n\nAnna owns delivery.",
        );
        let id = import_document_inner(&state, md.to_str().unwrap(), "f-open").unwrap();
        // Stored + readable through the gated get.
        assert_eq!(
            get_document_inner(&state, &id).unwrap(),
            "# Spec\n\nThe budget is 100k.\n\nAnna owns delivery.",
            "document text readable in an open folder"
        );
        // Listed with metadata only (the DTO has no text field).
        let listed = list_documents_inner(&state, "f-open").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert!(listed[0].name.ends_with(".md"));
        // CHUNKING is unconditional (keyword/FTS retrieval must work on a default install);
        // only the VECTORS stay model-presence-gated (never write stub vectors — mirrors
        // `should_auto_index`'s no-stub contract). So:
        //   - always          → ≥1 doc_chunks row;
        //   - model present   → matching real e5 vectors;
        //   - model ABSENT    → exactly 0 doc_vec_chunks rows (no stub poisoning).
        assert!(
            state.db.doc_chunk_count(&id).unwrap() >= 1,
            "the document must be chunked regardless of model presence (always-chunk)"
        );
        if crate::embed::embed_model_present() {
            assert!(
                state.db.doc_vec_count(&id).unwrap() >= 1,
                "with the e5 model present, the chunks must carry real vectors"
            );
        } else {
            assert_eq!(
                state.db.doc_vec_count(&id).unwrap(),
                0,
                "with no e5 model, NO stub vectors are written (no-stub contract)"
            );
        }

        // A .txt import is also accepted.
        let txt = write_temp_doc("txt", "txt", "plain text notes about hiring");
        let id2 = import_document_inner(&state, txt.to_str().unwrap(), "f-open").unwrap();
        assert_eq!(list_documents_inner(&state, "f-open").unwrap().len(), 2);
        assert!(!id2.is_empty());
    }

    /// ALLOWLIST: a non-md/txt extension (and an extension-less path) is rejected with InvalidArg,
    /// and NO row is inserted (the folder stays empty).
    #[test]
    fn import_document_rejects_non_md_txt() {
        let state = build_state("doc-allowlist");
        make_open_folder(&state.db, "f-open", "Project");

        let pdf = write_temp_doc("pdf", "pdf", "%PDF-1.4 ...");
        let err = import_document_inner(&state, pdf.to_str().unwrap(), "f-open").unwrap_err();
        assert!(
            matches!(err, AppError::InvalidArg(_)),
            "pdf must be rejected, got {err:?}"
        );

        // Extension-less path.
        let noext = {
            let mut p = std::env::temp_dir();
            p.push(format!("murmur-doc-noext-{}", std::process::id()));
            std::fs::write(&p, "x").unwrap();
            p
        };
        let err2 = import_document_inner(&state, noext.to_str().unwrap(), "f-open").unwrap_err();
        assert!(
            matches!(err2, AppError::InvalidArg(_)),
            "extension-less must be rejected"
        );

        assert!(
            list_documents_inner(&state, "f-open").unwrap().is_empty(),
            "no row inserted on reject"
        );
    }

    /// WRITE-GATE + LIST/GET MASK: importing into a sealed-and-NOT-session-unlocked folder is refused
    /// (`AppError::Locked`); a sealed folder's existing documents are masked from `list_documents`
    /// (empty) and `get_document` ("") even though the row still exists. Unlocking restores both.
    #[test]
    fn document_import_refused_and_list_get_masked_when_sealed() {
        let state = build_state("doc-sealed");
        make_open_folder(&state.db, "f-lock", "Secret");
        // Seed a document while open, then seal the folder (locked, NOT in the session set).
        let md = write_temp_doc("seal", "md", "secret: launch date is the 14th");
        let existing = import_document_inner(&state, md.to_str().unwrap(), "f-lock").unwrap();
        state
            .db
            .set_folder_locked("f-lock", true, Some(&b"wrapped"[..]))
            .unwrap();

        // IMPORT is refused with Locked.
        let md2 = write_temp_doc("seal2", "md", "another secret");
        let err = import_document_inner(&state, md2.to_str().unwrap(), "f-lock").unwrap_err();
        assert!(
            matches!(err, AppError::Locked(_)),
            "sealed import must be Locked, got {err:?}"
        );
        // DELETE is refused with Locked too.
        let derr = delete_document_inner(&state, &existing).unwrap_err();
        assert!(
            matches!(derr, AppError::Locked(_)),
            "sealed delete must be Locked, got {derr:?}"
        );

        // LIST is masked to empty; GET is masked to "" — even though the row + text column persist.
        assert!(
            list_documents_inner(&state, "f-lock").unwrap().is_empty(),
            "sealed list masked"
        );
        assert_eq!(
            get_document_inner(&state, &existing).unwrap(),
            "",
            "sealed get masked"
        );

        // Session-unlock ⇒ list + get + import work again.
        state
            .unlocked_folders
            .lock()
            .unwrap()
            .insert("f-lock".to_string());
        assert_eq!(
            list_documents_inner(&state, "f-lock").unwrap().len(),
            1,
            "unlocked list visible"
        );
        assert_eq!(
            get_document_inner(&state, &existing).unwrap(),
            "secret: launch date is the 14th",
            "unlocked get returns the text"
        );
        let md3 = write_temp_doc("seal3", "md", "after unlock");
        import_document_inner(&state, md3.to_str().unwrap(), "f-lock").unwrap();
    }

    /// IMPORT_TEXT round-trip: typed text is ingested as a kind='note' document — stored + readable,
    /// listed with kind="note", and chunked WHEN the e5 model is present (0 chunks when absent, the
    /// no-stub contract). Empty text is refused. Reuses the same gated ingest as `import_document`.
    #[test]
    fn import_text_round_trip_as_note() {
        let state = build_state("text-import");
        make_open_folder(&state.db, "f-open", "Project");

        let id = import_text_inner(&state, "Decisions", "We ship the beta on Friday.", "f-open")
            .unwrap();
        assert_eq!(
            get_document_inner(&state, &id).unwrap(),
            "We ship the beta on Friday.",
            "typed note text readable in an open folder"
        );
        let listed = list_documents_inner(&state, "f-open").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].kind, "note", "import_text stores kind='note'");
        assert_eq!(listed[0].name, "Decisions");
        // Model-presence-gated chunk count (deterministic on a no-model CI machine).
        if crate::embed::embed_model_present() {
            assert!(
                state.db.doc_chunk_count(&id).unwrap() >= 1,
                "note chunked+embedded when model present"
            );
        } else {
            assert_eq!(
                state.db.doc_chunk_count(&id).unwrap(),
                0,
                "no stub vectors when model absent"
            );
        }
        // Empty / whitespace-only text is refused.
        assert!(matches!(
            import_text_inner(&state, "n", "   ", "f-open").unwrap_err(),
            AppError::InvalidArg(_)
        ));
    }

    /// import_text is WRITE-GATED like import_document: a sealed-not-unlocked folder refuses it with
    /// Locked, and a note in a sealed folder is masked from list/get.
    #[test]
    fn import_text_refused_when_folder_sealed() {
        let state = build_state("text-sealed");
        make_open_folder(&state.db, "f-lock", "Secret");
        let note = import_text_inner(&state, "n", "the code is 4291", "f-lock").unwrap();
        state
            .db
            .set_folder_locked("f-lock", true, Some(&b"wrapped"[..]))
            .unwrap();

        assert!(matches!(
            import_text_inner(&state, "n2", "another", "f-lock").unwrap_err(),
            AppError::Locked(_)
        ));
        assert!(
            list_documents_inner(&state, "f-lock").unwrap().is_empty(),
            "sealed note list masked"
        );
        assert_eq!(
            get_document_inner(&state, &note).unwrap(),
            "",
            "sealed note get masked"
        );
    }

    /// brain_overview counts ONLY visible content: a note in a SEALED-not-unlocked folder is NOT
    /// counted; session-unlocking the folder makes it count. Flags reflect config + model presence.
    #[test]
    fn brain_overview_counts_only_visible() {
        let state = build_state("brain-overview");
        make_open_folder(&state.db, "f-open", "Open");
        make_open_folder(&state.db, "f-lock", "Locked");
        import_text_inner(&state, "a", "open note one", "f-open").unwrap();
        import_document_inner(
            &state,
            write_temp_doc("ov", "md", "an open document")
                .to_str()
                .unwrap(),
            "f-open",
        )
        .unwrap();
        let secret = import_text_inner(&state, "s", "secret note", "f-lock").unwrap();
        assert!(!secret.is_empty());
        state
            .db
            .set_folder_locked("f-lock", true, Some(&b"wrapped"[..]))
            .unwrap();

        // Sealed folder's note is excluded: 1 note (open) + 1 document (open), NOT 2 notes.
        let ov = brain_overview_inner(&state).unwrap();
        assert_eq!(ov.note_count, 1, "sealed folder's note not counted");
        assert_eq!(ov.document_count, 1, "open document counted");
        assert_eq!(ov.embed_model_present, crate::embed::embed_model_present());

        // Session-unlock ⇒ the sealed note now counts.
        state
            .unlocked_folders
            .lock()
            .unwrap()
            .insert("f-lock".to_string());
        assert_eq!(
            brain_overview_inner(&state).unwrap().note_count,
            2,
            "unlocked note counted"
        );
    }

    /// COUNTEREXAMPLE (verify-before-destroy): an uploaded document's TEXT survives a real lock→unlock
    /// cycle — SEALED under the folder CK (plaintext blanked, blob present) on lock, RESTORED
    /// byte-identical on unlock. Drives the PRODUCTION seal (`lock_folder_inner` →
    /// `seal_folder_extras`) and unseal (`unseal_folder_extras`). The doc chunks are purged on lock.
    #[test]
    fn document_text_survives_lock_unlock_cycle_sealed_and_restored() {
        let state = build_state("doc-seal-cycle");
        make_open_folder(&state.db, "f-lock", "Secret");
        let md = write_temp_doc(
            "cycle",
            "md",
            "zażółć 🔒 DECISION: ship Friday; Anna owns QA",
        );
        let id = import_document_inner(&state, md.to_str().unwrap(), "f-lock").unwrap();
        let original = "zażółć 🔒 DECISION: ship Friday; Anna owns QA";

        // LOCK → production seal: text blanked at rest, text_blob present, doc chunks purged.
        lock_folder_inner(&state, "f-lock".to_string()).unwrap();
        let sealed = state
            .db
            .raw_documents_in_folder("f-lock")
            .unwrap()
            .into_iter()
            .find(|d| d.id == id)
            .unwrap();
        assert_eq!(
            sealed.text, "",
            "document text plaintext blanked while sealed"
        );
        assert!(
            sealed.blob.is_some(),
            "document sealed under the folder CK (blob present)"
        );
        assert_eq!(
            state.db.doc_chunk_count(&id).unwrap(),
            0,
            "doc chunks purged on lock (re-embeddable on unlock)"
        );

        // UNLOCK (mirror unlock_folder's internals): KEK → unwrap CK → unseal extras.
        let kek = secrets::get_or_create_master_kek().unwrap();
        let wrapped = state.db.folder_wrapped_key("f-lock").unwrap().unwrap();
        let ck_vec = crate::crypto::decrypt(&kek, &wrapped, &aad_wrapped_ck("f-lock")).unwrap();
        let ck: [u8; 32] = ck_vec.try_into().expect("CK is 32 bytes");
        unseal_folder_extras(&state, "f-lock", &ck, None).unwrap();

        let restored = state
            .db
            .raw_documents_in_folder("f-lock")
            .unwrap()
            .into_iter()
            .find(|d| d.id == id)
            .unwrap();
        assert_eq!(
            restored.text, original,
            "document text restored byte-identical on unlock — sealed-and-restored, never lost"
        );
    }

    /// REMOVE-LOCK: permanently removing a folder's lock decrypts the document text back to plaintext
    /// and clears the blob — the document is NEVER lost.
    #[test]
    fn document_text_survives_remove_lock_permanently() {
        let state = build_state("doc-remove-lock");
        make_open_folder(&state.db, "f-lock", "Secret");
        let md = write_temp_doc("perm", "md", "permanent: revisit auth next sprint");
        let id = import_document_inner(&state, md.to_str().unwrap(), "f-lock").unwrap();

        lock_folder_inner(&state, "f-lock".to_string()).unwrap();
        let sealed = state
            .db
            .raw_documents_in_folder("f-lock")
            .unwrap()
            .into_iter()
            .find(|d| d.id == id)
            .unwrap();
        assert_eq!(sealed.text, "", "blanked while sealed");

        let kek = secrets::get_or_create_master_kek().unwrap();
        *state.master_kek.lock().unwrap() = Some(Zeroizing::new(kek));
        remove_lock_inner(&state, "f-lock".to_string()).unwrap();

        let d = state
            .db
            .raw_documents_in_folder("f-lock")
            .unwrap()
            .into_iter()
            .find(|d| d.id == id)
            .unwrap();
        assert_eq!(
            d.text, "permanent: revisit auth next sprint",
            "permanently restored on remove-lock"
        );
        assert!(
            d.blob.is_none(),
            "document text_blob cleared on remove-lock"
        );
    }

    /// DELETE cascades the document's chunks/vectors away.
    #[test]
    fn delete_document_cascades_chunks() {
        let state = build_state("doc-delete");
        make_open_folder(&state.db, "f-open", "Project");
        let md = write_temp_doc("del", "md", "alpha bravo charlie");
        let id = import_document_inner(&state, md.to_str().unwrap(), "f-open").unwrap();
        assert_eq!(list_documents_inner(&state, "f-open").unwrap().len(), 1);

        delete_document_inner(&state, &id).unwrap();
        assert!(
            list_documents_inner(&state, "f-open").unwrap().is_empty(),
            "document deleted"
        );
        assert_eq!(
            state.db.doc_chunk_count(&id).unwrap(),
            0,
            "doc chunks cascade-deleted with the document"
        );
    }

    /// PR B HEADLINE (write-only-memory bug): on a DEFAULT install (no e5 model, semantic flag OFF)
    /// an ingested brain note/document MUST still be REACHABLE — chunked unconditionally, surfaced
    /// by the FLAG-OFF Ask corpus builder, and surfaced through the advertised tool seam (which MCP
    /// and the agentic loop share). RED on the old code: ingest skipped chunking without the model,
    /// the flag-off corpus packed meetings only, and both search tools ignored documents.
    #[test]
    fn model_less_ingest_is_reachable_by_flag_off_ask_and_tools() {
        let state = build_state("docs-default-reach");
        make_open_folder(&state.db, "f-open", "Project");
        let id = import_text_inner(
            &state,
            "Preferencje",
            "Ulubiony kolor użytkownika to fioletowoszary.",
            "f-open",
        )
        .unwrap();

        // 1. ALWAYS-CHUNK: doc_chunks rows exist regardless of model presence (keyword retrieval
        //    must work on a default install; vectors stay model-gated).
        assert!(
            state.db.doc_chunk_count(&id).unwrap() >= 1,
            "ingest must store doc_chunks rows even without the e5 model"
        );

        // 2. The FLAG-OFF Ask corpus (the default `ask_vault` path) surfaces the token.
        let nothing = HashSet::new();
        let (corpus, _) = crate::summarize::vault_context::build_vault_context_visible(
            &state.db,
            "fioletowoszary",
            "anthropic",
            &nothing,
        )
        .unwrap();
        assert!(
            // Assert on a CONTENT word absent from the query — sentinels echo the query itself
            // ("No meetings match \"fioletowoszary\"."), so a query-token check can self-satisfy.
            corpus.contains("Ulubiony") && corpus.contains("fioletowoszary"),
            "flag-off Ask corpus must surface ingested document/note content; got: {corpus:?}"
        );

        // 3. The tool seam (PIN semantic OFF — this test asserts the flag-OFF keyword-fallback path)
        //    surfaces the token through BOTH advertised search tools — the same seam MCP and the
        //    agentic loop dispatch through.
        let cfg = AppConfig {
            semantic_search_enabled: false,
            ..AppConfig::default()
        };
        let out = crate::tools::execute_tool(
            &crate::tools::ToolCall::SearchMeetings {
                query: "fioletowoszary".into(),
            },
            &state.db,
            &nothing,
            &cfg,
        )
        .unwrap();
        assert!(
            out.contains("Ulubiony"),
            "search_meetings must surface document CONTENT (not just echo the query in an \
             empty-result sentinel); got: {out:?}"
        );
        let out2 = crate::tools::execute_tool(
            &crate::tools::ToolCall::SearchSemantic {
                query: "fioletowoszary".into(),
            },
            &state.db,
            &nothing,
            &cfg,
        )
        .unwrap();
        assert!(
            out2.contains("Ulubiony"),
            "search_semantic (flag OFF) must fall back to gated keyword doc search and surface \
             document CONTENT; got: {out2:?}"
        );
    }

    /// TIER 1 default-on graceful degradation: with `semantic_search_enabled = true` but NO e5 model
    /// (CI / the common fresh-install state), `search_semantic` must DEGRADE to gated keyword doc
    /// search and still surface document CONTENT — not panic, not go empty. Proves the NEW default is
    /// safe when the model is absent (which it is until the user downloads e5).
    #[test]
    fn semantic_on_without_model_surfaces_keyword_results() {
        let state = build_state("docs-semantic-on-nomodel");
        make_open_folder(&state.db, "f-open", "Project");
        import_text_inner(
            &state,
            "Preferencje",
            "Ulubiony kolor użytkownika to fioletowoszary.",
            "f-open",
        )
        .unwrap();
        let nothing = HashSet::new();
        // The NEW default: semantic ON. With no e5 model on disk the hybrid leg degenerates to FTS.
        let cfg = AppConfig {
            semantic_search_enabled: true,
            ..AppConfig::default()
        };
        let out = crate::tools::execute_tool(
            &crate::tools::ToolCall::SearchSemantic {
                query: "fioletowoszary".into(),
            },
            &state.db,
            &nothing,
            &cfg,
        )
        .unwrap();
        assert!(
            out.contains("Ulubiony"),
            "semantic ON + no model must degrade to gated keyword doc search and surface CONTENT; \
             got: {out:?}"
        );
    }

    /// SEAL/UNSEAL PARITY for the new keyword legs: locking a folder makes its document's unique
    /// token INVISIBLE through EVERY read leg (the gated FTS helper, the flag-off Ask corpus, both
    /// search tools), and a session unlock brings it back through the SAME legs — including on a
    /// model-less install (the unlock re-chunk no longer requires the e5 model).
    #[test]
    fn sealed_folder_docs_invisible_through_every_leg_until_unlock() {
        let state = build_state("docs-seal-legs");
        make_open_folder(&state.db, "f-lock", "Secret");
        let id = import_text_inner(
            &state,
            "plan",
            "TAJNYTOKEN kwartalny raport przejęcia",
            "f-lock",
        )
        .unwrap();
        assert!(
            state.db.doc_chunk_count(&id).unwrap() >= 1,
            "precondition: chunked on ingest"
        );

        lock_folder_inner(&state, "f-lock".to_string()).unwrap();

        let nothing = HashSet::new();
        let cfg = AppConfig::default();
        // Leak assertions check "przejęcia" — a CONTENT word absent from the query — because the
        // empty-result sentinels honestly ECHO the query text ("No meetings or documents match
        // \"TAJNYTOKEN\"."), which is not a content leak.
        let assert_leg_visibility = |unlocked: &HashSet<String>, expected: bool, phase: &str| {
            let fts_hit = state
                .db
                .search_doc_chunks_fts_visible("TAJNYTOKEN", 10, unlocked)
                .unwrap()
                .iter()
                .any(|h| h.document_id == id);
            assert_eq!(fts_hit, expected, "FTS helper leg, {phase}");
            let (corpus, _) = crate::summarize::vault_context::build_vault_context_visible(
                &state.db,
                "TAJNYTOKEN",
                "anthropic",
                unlocked,
            )
            .unwrap();
            assert_eq!(
                corpus.contains("przejęcia"),
                expected,
                "Ask corpus leg, {phase}"
            );
            for call in [
                crate::tools::ToolCall::SearchMeetings {
                    query: "TAJNYTOKEN".into(),
                },
                crate::tools::ToolCall::SearchSemantic {
                    query: "TAJNYTOKEN".into(),
                },
            ] {
                let out = crate::tools::execute_tool(&call, &state.db, unlocked, &cfg).unwrap();
                assert_eq!(
                    out.contains("przejęcia"),
                    expected,
                    "tool leg {call:?}, {phase}"
                );
            }
        };

        // Sealed-and-not-unlocked: the token leaks through NO leg.
        assert_leg_visibility(&nothing, false, "sealed");

        // Session-unlock (mirror unlock_folder's internals: KEK → unwrap CK → unseal extras, which
        // re-chunks model-lessly), then evaluate every leg against the live unlock set.
        let kek = secrets::get_or_create_master_kek().unwrap();
        let wrapped = state.db.folder_wrapped_key("f-lock").unwrap().unwrap();
        let ck_vec = crate::crypto::decrypt(&kek, &wrapped, &aad_wrapped_ck("f-lock")).unwrap();
        let ck: [u8; 32] = ck_vec.try_into().expect("CK is 32 bytes");
        unseal_folder_extras(&state, "f-lock", &ck, None).unwrap();
        assert!(
            state.db.doc_chunk_count(&id).unwrap() >= 1,
            "unlock must re-chunk the document even without the e5 model"
        );
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        assert_leg_visibility(&unlocked, true, "session-unlocked");
    }

    /// Mirror `unlock_folder`'s own note-markdown restore (KEK → unwrap CK → decrypt each sealed
    /// provider row's `content_blob` back into its plaintext markdown column) — the step
    /// `unlock_folder` runs BEFORE `unseal_folder_extras`, so the meeting re-index has plaintext to
    /// chunk. Returns the unwrapped CK for the follow-on `unseal_folder_extras` call.
    fn restore_notes_and_ck(state: &AppState, folder_id: &str) -> [u8; 32] {
        let kek = secrets::get_or_create_master_kek().unwrap();
        let wrapped = state.db.folder_wrapped_key(folder_id).unwrap().unwrap();
        let ck_vec = crate::crypto::decrypt(&kek, &wrapped, &aad_wrapped_ck(folder_id)).unwrap();
        let ck: [u8; 32] = ck_vec.try_into().expect("CK is 32 bytes");
        for n in state.db.notes_in_folder(folder_id).unwrap() {
            if let Some(blob) = &n.content_blob {
                let aad = aad_content(folder_id, &n.meeting_id, &n.provider_id, "note");
                let md = String::from_utf8(crate::crypto::decrypt(&ck, blob, &aad).unwrap()).unwrap();
                state
                    .db
                    .restore_note_markdown(&n.meeting_id, &n.provider_id, &md)
                    .unwrap();
            }
        }
        ck
    }

    /// REGRESSION (the reported bug): locking a folder PURGES its meetings' `note_chunks`/`vec_chunks`
    /// (`purge_chunks_for_meetings`), but session-unlock (`unseal_folder_extras`) previously re-indexed
    /// ONLY documents and NEVER the meetings — so semantic / related-meetings stayed DEAD for those
    /// meetings until a manual full re-index. This asserts the fix on the PRODUCTION unlock path.
    ///
    /// RED-before-GREEN (deterministic, machine-independent): the meeting embedder is now injected, so
    /// the model-PRESENT branch is driven by passing `Some(&StubEmbedder)` (the same stand-in every
    /// meeting-chunk test uses) into the PRODUCTION `unseal_folder_extras`. On the pre-fix code
    /// `unseal_folder_extras` had no meeting re-index at all (`index_meeting_chunks`'s only production
    /// callers were the pipeline + the manual reindex command, grep-confirmed), so `note_chunk_count`
    /// would stay 0 even with `Some` — RED. The `None` call proves the ABSOLUTE no-stub-vector /
    /// mirror-the-pipeline invariant: model-absent unlock writes ZERO meeting chunks AND vectors.
    #[test]
    fn unlock_reindexes_folder_meetings_and_never_stub_vectors() {
        let state = build_state("unlock-reindex-meetings");
        make_open_folder(&state.db, "f-lock", "Secret");
        seed_meeting(
            &state.db,
            "m1",
            "# Q3 plan\n\nbudget planning and hiring across the org",
            Some("f-lock"),
        );
        // Index while the folder is OPEN (the stub stands in for a present model, as every other
        // meeting-chunk test does) → chunks + vectors present.
        state.db.index_meeting_chunks("m1", &[], &crate::embed::StubEmbedder).unwrap();
        assert!(state.db.note_chunk_count("m1").unwrap() > 0, "precondition: chunked while open");
        assert!(state.db.note_vec_count("m1").unwrap() > 0, "precondition: vectored while open");

        // LOCK (production seal) → meeting chunks + vectors purged.
        lock_folder_inner(&state, "f-lock".to_string()).unwrap();
        assert_eq!(state.db.note_chunk_count("m1").unwrap(), 0, "note_chunks purged on lock");
        assert_eq!(state.db.note_vec_count("m1").unwrap(), 0, "vec_chunks purged on lock");

        // Restore note markdown exactly as `unlock_folder` does BEFORE `unseal_folder_extras`.
        let ck = restore_notes_and_ck(&state, "f-lock");

        // MODEL-ABSENT (embedder = None): the PRODUCTION unlock must write NOTHING for meetings — no
        // chunk-only mode → any write would be a forbidden stub vector. Mirrors the pipeline exactly.
        unseal_folder_extras(&state, "f-lock", &ck, None).unwrap();
        assert_eq!(
            state.db.note_chunk_count("m1").unwrap(),
            0,
            "model-absent unlock writes NO meeting note_chunks (mirrors the pipeline/reindex policy)"
        );
        assert_eq!(
            state.db.note_vec_count("m1").unwrap(),
            0,
            "ABSOLUTE no-stub-vector: model-absent unlock never writes a meeting vector"
        );

        // MODEL-PRESENT (embedder = Some): the PRODUCTION unlock re-indexes the folder's meetings.
        // RED on the pre-fix code (unseal_folder_extras had no meeting re-index → stayed 0).
        unseal_folder_extras(&state, "f-lock", &ck, Some(&crate::embed::StubEmbedder)).unwrap();
        assert!(
            state.db.note_chunk_count("m1").unwrap() > 0,
            "production unlock re-indexes meeting note_chunks when the e5 model is present (was DEAD before the fix)"
        );
        assert!(
            state.db.note_vec_count("m1").unwrap() > 0,
            "production unlock re-indexes meeting vec_chunks when the e5 model is present"
        );
    }

    /// REMOVE-LOCK analogue: permanently opening a folder (`remove_lock_inner` →
    /// `unseal_folder_extras_permanent`) restores plaintext + `.md` for every meeting; the folder is
    /// then permanently OPEN, so there is no privacy rationale to skip re-indexing — meetings must
    /// re-index exactly like documents. Same deterministic RED-before-GREEN shape as the session-unlock
    /// test, driving the PRODUCTION `unseal_folder_extras_permanent` with `None` then `Some`.
    #[test]
    fn remove_lock_reindexes_folder_meetings_and_never_stub_vectors() {
        let state = build_state("remove-lock-reindex-meetings");
        make_open_folder(&state.db, "f-lock", "Secret");
        seed_meeting(
            &state.db,
            "m1",
            "# Roadmap\n\nrevisit auth and the billing migration next sprint",
            Some("f-lock"),
        );
        state.db.index_meeting_chunks("m1", &[], &crate::embed::StubEmbedder).unwrap();
        assert!(state.db.note_chunk_count("m1").unwrap() > 0, "precondition: chunked while open");

        lock_folder_inner(&state, "f-lock".to_string()).unwrap();
        assert_eq!(state.db.note_chunk_count("m1").unwrap(), 0, "purged on lock");

        // `remove_lock_inner` restores the note markdown (Step 1) BEFORE `unseal_folder_extras_permanent`.
        let ck = restore_notes_and_ck(&state, "f-lock");

        // MODEL-ABSENT (None): permanent unseal writes NO meeting chunks/vectors (no stub vectors).
        unseal_folder_extras_permanent(&state, "f-lock", &ck, None).unwrap();
        assert_eq!(
            state.db.note_chunk_count("m1").unwrap(),
            0,
            "model-absent remove-lock writes NO meeting note_chunks (mirrors the pipeline policy)"
        );
        assert_eq!(
            state.db.note_vec_count("m1").unwrap(),
            0,
            "ABSOLUTE no-stub-vector: model-absent remove-lock never writes a meeting vector"
        );

        // MODEL-PRESENT (Some): permanent unseal re-indexes the now-open folder's meetings. RED on the
        // pre-fix code (unseal_folder_extras_permanent had no meeting re-index → stayed 0).
        unseal_folder_extras_permanent(&state, "f-lock", &ck, Some(&crate::embed::StubEmbedder)).unwrap();
        assert!(
            state.db.note_chunk_count("m1").unwrap() > 0,
            "remove-lock re-indexes meeting note_chunks when the e5 model is present (was DEAD before the fix)"
        );
        assert!(state.db.note_vec_count("m1").unwrap() > 0, "remove-lock re-indexes meeting vec_chunks");
    }

    /// RELOCK re-purges the freshly-rebuilt meeting chunks: a lock → unlock (re-index) → relock cycle
    /// must leave ZERO meeting chunks/vectors at rest — `relock_folder` → `blank_sealed_notes_in_folders`
    /// → `purge_chunks_tx` covers the rows the unlock re-index rebuilt, so a re-sealed folder never
    /// leaves a meeting vector at rest.
    #[test]
    fn lock_unlock_relock_leaves_zero_meeting_chunks() {
        let state = build_state("relock-repurge-meetings");
        make_open_folder(&state.db, "f-lock", "Secret");
        seed_meeting(
            &state.db,
            "m1",
            "# Notes\n\nlaunch on the 14th; hire two engineers",
            Some("f-lock"),
        );
        state.db.index_meeting_chunks("m1", &[], &crate::embed::StubEmbedder).unwrap();

        // Lock → purge.
        lock_folder_inner(&state, "f-lock".to_string()).unwrap();
        assert_eq!(state.db.note_chunk_count("m1").unwrap(), 0, "purged on lock");

        // Session-unlock: restore markdown + the PRODUCTION unseal with a present (stub) model, which
        // rebuilds the meeting chunks — so the relock below has something to re-purge.
        let ck = restore_notes_and_ck(&state, "f-lock");
        unseal_folder_extras(&state, "f-lock", &ck, Some(&crate::embed::StubEmbedder)).unwrap();
        state
            .unlocked_folders
            .lock()
            .unwrap()
            .insert("f-lock".to_string());
        assert!(
            state.db.note_chunk_count("m1").unwrap() > 0,
            "precondition: meeting re-indexed while session-unlocked"
        );
        assert!(state.db.note_vec_count("m1").unwrap() > 0, "precondition: vectors rebuilt");

        // RELOCK → re-purge. Zero meeting chunks AND zero vectors at rest.
        relock_all_inner(&state).unwrap();
        assert_eq!(
            state.db.note_chunk_count("m1").unwrap(),
            0,
            "relock must re-purge the meeting note_chunks the unlock re-index rebuilt"
        );
        assert_eq!(
            state.db.note_vec_count("m1").unwrap(),
            0,
            "relock must re-purge the rebuilt meeting vec_chunks (no invertible vector at rest)"
        );
    }

    /// REINDEX BACKFILL (PR B): `reindex_embeddings_inner` covers documents. Model ABSENT →
    /// write-only (chunkless) documents gain chunks + FTS reachability with ZERO vectors, while an
    /// already-chunked document's existing vectors are NOT purged (no downgrade). Model PRESENT →
    /// the chunkless document is re-embedded with vectors too.
    #[test]
    fn reindex_backfills_documents_regardless_of_model() {
        let state = build_state("reindex-docs");
        make_open_folder(&state.db, "f-open", "Project");
        // A write-only legacy row: document stored, never chunked (the pre-PR-B model-less ingest).
        state
            .db
            .insert_document(
                "d-legacy",
                "f-open",
                "legacy.md",
                "szmaragdowy raport roczny",
                "document",
                100,
            )
            .unwrap();
        // An already-indexed document WITH vectors (stub-deterministic).
        state
            .db
            .insert_document(
                "d-vec",
                "f-open",
                "vec.md",
                "vectored content here",
                "document",
                200,
            )
            .unwrap();
        state
            .db
            .index_document_chunks("d-vec", Some(&crate::embed::StubEmbedder))
            .unwrap();
        let vec_before = state.db.doc_vec_count("d-vec").unwrap();
        assert!(vec_before >= 1, "precondition: d-vec has vectors");
        assert_eq!(
            state.db.doc_chunk_count("d-legacy").unwrap(),
            0,
            "precondition: d-legacy chunkless"
        );

        // Model ABSENT: chunk/FTS backfill only.
        let nothing = HashSet::new();
        let stub = crate::embed::StubEmbedder;
        let res = reindex_embeddings_inner(&state.db, &nothing, false, &stub, |_, _| {}).unwrap();
        assert_eq!(res.status, "model_missing", "meeting semantics unchanged");
        assert!(
            state.db.doc_chunk_count("d-legacy").unwrap() >= 1,
            "legacy doc backfilled"
        );
        assert_eq!(
            state.db.doc_vec_count("d-legacy").unwrap(),
            0,
            "no stub vectors on backfill"
        );
        assert_eq!(
            state.db.doc_vec_count("d-vec").unwrap(),
            vec_before,
            "model-absent reindex must NOT purge an indexed document's vectors"
        );
        assert!(
            state
                .db
                .search_doc_chunks_fts_visible("szmaragdowy", 10, &nothing)
                .unwrap()
                .iter()
                .any(|h| h.document_id == "d-legacy"),
            "backfilled document must be keyword-findable"
        );

        // Model PRESENT: the full purge-then-reinsert re-embed covers documents.
        let res2 = reindex_embeddings_inner(&state.db, &nothing, true, &stub, |_, _| {}).unwrap();
        assert_eq!(res2.status, "indexed");
        assert!(
            state.db.doc_vec_count("d-legacy").unwrap() >= 1,
            "model-present reindex must (re)embed document chunks"
        );
    }

    /// BLK-1: hammer the off-thread `relock_all_inner` (the blanker) WHILE `remove_lock_inner` runs
    /// its restore→clear sequence, across many seal/remove cycles, and assert the IRREVERSIBLE-LOSS
    /// state — a note with `markdown=''` AND `content_blob=NULL` — NEVER occurs. The coarse
    /// `AppState::lifecycle` mutex serializes the two so the blank can never land between
    /// `remove_lock`'s Step 1 (restore plaintext) and Step 2 (clear blob).
    #[test]
    fn relock_all_never_destroys_a_note_being_remove_locked() {
        const MID: &str = "m-blk1";
        const FOLDER: &str = "f-blk1";
        const ORIGINAL_MD: &str = "# Board notes\n\n- launch on the 14th\n- hire two engineers";

        let state = Arc::new(build_state("blk1"));
        make_open_folder(&state.db, FOLDER, "Confidential");
        seed_meeting(&state.db, MID, ORIGINAL_MD, Some(FOLDER));

        // A background thread that spams the off-thread blanker continuously.
        let stop = Arc::new(AtomicBool::new(false));
        let spammer = {
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    // Ignore errors — a busy WAL checkpoint etc. is non-fatal to the invariant.
                    let _ = relock_all_inner(&state);
                }
            })
        };

        const ITERS: usize = 60;
        for i in 0..ITERS {
            // Seal the folder (note markdown → '' + content_blob set), then permanently remove the
            // lock (restore plaintext, clear blob). The blanker is racing the whole time.
            lock_folder_inner(&state, FOLDER.to_string()).unwrap();
            remove_lock_inner(&state, FOLDER.to_string()).unwrap();

            // The load-bearing invariant: no provider row is ever blanked-AND-blob-cleared, and the
            // restore put the ORIGINAL content back with the blob gone (folder fully open again).
            for n in state.db.sealable_notes_for_meeting(MID).unwrap() {
                assert!(
                    !(n.markdown.is_empty() && n.content_blob.is_none()),
                    "IRREVERSIBLE DATA LOSS at iter {i}: markdown='' AND content_blob=NULL"
                );
                assert_eq!(
                    n.markdown, ORIGINAL_MD,
                    "note restored to original at iter {i}"
                );
                assert!(
                    n.content_blob.is_none(),
                    "content_blob cleared after remove_lock at iter {i}"
                );
            }
        }

        stop.store(true, Ordering::Relaxed);
        spammer.join().unwrap();

        // Final state: open folder, original content, no residual blob anywhere.
        assert!(!state.db.folder_by_id(FOLDER).unwrap().unwrap().locked);
        let note = state.db.get_latest_note_for_meeting(MID).unwrap().unwrap();
        assert_eq!(note.markdown, ORIGINAL_MD);
    }

    /// BLK-2 (reject half): moving a note INTO a locked folder that is NOT session-unlocked must be
    /// REJECTED (`AppError::Locked`) and leave the note untouched — never reassigned, never blanked.
    #[test]
    fn move_into_locked_not_unlocked_folder_rejects_and_leaves_note_intact() {
        const MID: &str = "m-blk2r";
        const TARGET: &str = "f-blk2r";
        const MD: &str = "# secret meeting\n\nplaintext that must not move into a locked folder";

        let state = build_state("blk2r");
        seed_meeting(&state.db, MID, MD, None); // at the vault root (open)
        make_open_folder(&state.db, TARGET, "Locked-Target-R");
        lock_folder_inner(&state, TARGET.to_string()).unwrap(); // seal it; NOT session-unlocked

        let res = move_into_locked_folder(&state, MID, TARGET);
        assert!(
            matches!(res, Err(AppError::Locked(_))),
            "must reject with Locked, got {res:?}"
        );

        // Untouched: still at the root, still plaintext, no blob.
        assert_eq!(
            state.db.folder_for_meeting(MID).unwrap(),
            None,
            "note was NOT reassigned"
        );
        let n = &state.db.sealable_notes_for_meeting(MID).unwrap()[0];
        assert_eq!(n.markdown, MD, "plaintext preserved");
        assert!(n.content_blob.is_none(), "never sealed");
    }

    /// BLK-2 (seal half): moving a note INTO a locked + SESSION-UNLOCKED folder seals it to the
    /// folder's at-rest shape — `content_blob` set, `markdown` blanked, transcript blanked — so no
    /// plaintext ever lands in a locked folder, and the note is reassigned to the target.
    #[test]
    fn move_into_locked_unlocked_folder_seals_the_moved_note() {
        const MID: &str = "m-blk2s";
        const TARGET: &str = "f-blk2s";
        const MD: &str = "# moving in\n\nthis becomes ciphertext at rest";

        let state = build_state("blk2s");
        seed_meeting(&state.db, MID, MD, None); // at the vault root (open, plaintext)
        make_open_folder(&state.db, TARGET, "Locked-Target-S");
        lock_folder_inner(&state, TARGET.to_string()).unwrap(); // seal the (empty) target

        // Make the target SESSION-UNLOCKED: in the unlock set + KEK cached (as a real unlock would).
        state
            .unlocked_folders
            .lock()
            .unwrap()
            .insert(TARGET.to_string());
        let kek = secrets::get_or_create_master_kek().unwrap();
        *state.master_kek.lock().unwrap() = Some(Zeroizing::new(kek));

        move_into_locked_folder(&state, MID, TARGET).unwrap();

        // Reassigned into the target AND sealed at rest (blob set, plaintext blanked).
        assert_eq!(
            state.db.folder_for_meeting(MID).unwrap().as_deref(),
            Some(TARGET)
        );
        let n = &state.db.sealable_notes_for_meeting(MID).unwrap()[0];
        assert!(
            n.content_blob.is_some(),
            "moved note must be sealed (content_blob set)"
        );
        assert!(
            n.markdown.is_empty(),
            "moved note plaintext markdown blanked at rest"
        );
        assert!(
            n.exported_path.is_none(),
            "no vault .md for a note in a locked folder"
        );
        // Transcript sealed too (text blanked, text_blob present).
        for s in state.db.raw_segments(MID).unwrap() {
            assert!(s.text.is_empty(), "segment text blanked");
            assert!(s.text_blob.is_some(), "segment text_blob present");
        }

        // And it round-trips: a permanent remove-lock restores the original plaintext (no loss).
        remove_lock_inner(&state, TARGET.to_string()).unwrap();
        let restored = state.db.get_latest_note_for_meeting(MID).unwrap().unwrap();
        assert_eq!(
            restored.markdown, MD,
            "remove-lock restores the moved note's original content"
        );
    }

    /// Auto-organize seam: [`classify_auto_file_target`] maps a classifier-chosen subfolder to the
    /// right BLK-2 outcome — Open for an unmanaged / open subfolder, RejectToRoot for a locked +
    /// not-session-unlocked folder (so plaintext never lands in a sealed dir), and SealInto for a
    /// locked + session-unlocked folder (write then seal, like a manual move).
    #[test]
    fn classify_auto_file_target_covers_open_locked_and_unlocked() {
        let state = build_state("autofile");

        // No subfolder / unmanaged subfolder → Open.
        assert_eq!(
            classify_auto_file_target(&state, None).unwrap(),
            AutoFileTarget::Open
        );
        assert_eq!(
            classify_auto_file_target(&state, Some("Nonexistent")).unwrap(),
            AutoFileTarget::Open,
            "a subfolder with no matching folder row writes as usual"
        );

        // An OPEN folder row → Open.
        make_open_folder(&state.db, "f-open", "Standups");
        assert_eq!(
            classify_auto_file_target(&state, Some("Standups")).unwrap(),
            AutoFileTarget::Open
        );

        // A LOCKED, not-session-unlocked folder → RejectToRoot (no CK to seal with).
        make_open_folder(&state.db, "f-locked", "Confidential");
        lock_folder_inner(&state, "f-locked".to_string()).unwrap();
        assert_eq!(
            classify_auto_file_target(&state, Some("Confidential")).unwrap(),
            AutoFileTarget::RejectToRoot,
            "plaintext must not be written into a locked, not-unlocked folder"
        );

        // Make it SESSION-UNLOCKED (in the set + KEK cached) → SealInto(folder_id).
        state
            .unlocked_folders
            .lock()
            .unwrap()
            .insert("f-locked".to_string());
        let kek = secrets::get_or_create_master_kek().unwrap();
        *state.master_kek.lock().unwrap() = Some(Zeroizing::new(kek));
        assert_eq!(
            classify_auto_file_target(&state, Some("Confidential")).unwrap(),
            AutoFileTarget::SealInto("f-locked".to_string()),
            "a session-unlocked locked folder seals the auto-filed note in"
        );
    }

    /// Auto-organize seam (seal half): a note auto-filed into a session-unlocked locked folder via
    /// [`seal_auto_filed_note`] is sealed to the folder's at-rest shape (blob set, markdown blanked)
    /// and reassigned — exactly like a manual move. No plaintext survives in the sealed dir.
    #[test]
    fn seal_auto_filed_note_seals_into_unlocked_locked_folder() {
        const MID: &str = "m-autofile";
        const TARGET: &str = "f-autofile";
        const MD: &str = "# auto-filed\n\nthis becomes ciphertext at rest";

        let state = build_state("autofile-seal");
        seed_meeting(&state.db, MID, MD, None);
        make_open_folder(&state.db, TARGET, "AutoLocked");
        lock_folder_inner(&state, TARGET.to_string()).unwrap();
        state
            .unlocked_folders
            .lock()
            .unwrap()
            .insert(TARGET.to_string());
        let kek = secrets::get_or_create_master_kek().unwrap();
        *state.master_kek.lock().unwrap() = Some(Zeroizing::new(kek));

        seal_auto_filed_note(&state, MID, TARGET).unwrap();

        assert_eq!(
            state.db.folder_for_meeting(MID).unwrap().as_deref(),
            Some(TARGET)
        );
        let n = &state.db.sealable_notes_for_meeting(MID).unwrap()[0];
        assert!(
            n.content_blob.is_some(),
            "auto-filed note sealed (content_blob set)"
        );
        assert!(
            n.markdown.is_empty(),
            "auto-filed note plaintext blanked at rest"
        );
    }

    /// BLK-3: an `AppConfigDto` payload that OMITS `mcpRequireToken` deserializes to `true`
    /// (fail-closed), matching its Stage-E siblings — never silently `false`.
    #[test]
    fn dto_omitting_mcp_require_token_defaults_true() {
        let json = r#"{
            "providerId":"claude_code",
            "anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434",
            "ollamaModel":"llama3.1",
            "claudeBinary":"claude",
            "captureSystemAudio":false,
            "modelSize":"large-v3",
            "voiceTrigger":false,
            "onboarded":true,
            "noteStyle":"standard",
            "autoOrganize":false,
            "noteLanguage":"auto"
        }"#;
        let dto: AppConfigDto = serde_json::from_str(json).unwrap();
        assert!(
            dto.mcp_require_token,
            "omitted mcpRequireToken must fail closed to true (BLK-3)"
        );
        assert!(dto.lock_require_biometric, "Stage-E flags default ON");
        assert!(dto.relock_on_screenshare, "Stage-E flags default ON");
        assert!(
            !dto.cloud_egress_consented,
            "consent defaults OFF (fail-closed)"
        );
        assert!(
            dto.proactive_hints_enabled,
            "omitted proactiveHintsEnabled defaults ON (matches AppConfig::default)"
        );
        assert!(
            dto.user_memory_enabled,
            "omitted userMemoryEnabled defaults ON (matches AppConfig::default)"
        );
    }

    /// Proactive brain P1 — the mute toggle round-trips through the settings DTO: `config_to_dto`
    /// carries it OUT (the FE reads `proactiveHintsEnabled`) and `dto_to_config` takes it IN (the
    /// FE sets it), so Settings can actually mute the backend scanner.
    #[test]
    fn dto_round_trips_proactive_hints_toggle() {
        // OUT: the DTO reflects the live value.
        let cfg = AppConfig {
            proactive_hints_enabled: false,
            ..Default::default()
        };
        assert!(!config_to_dto(&cfg).proactive_hints_enabled);

        // IN: the DTO value lands in the merged config (proven from a differing `current`).
        let mut dto = config_to_dto(&AppConfig::default());
        dto.proactive_hints_enabled = false;
        let current = AppConfig::default(); // ON
        assert!(
            !dto_to_config(dto, &current).proactive_hints_enabled,
            "a settings save must be able to mute the backend scanner"
        );
    }

    /// Cross-meeting USER MEMORY — the master gate round-trips through the settings DTO:
    /// `config_to_dto` carries it OUT (the FE reads `userMemoryEnabled`) and `dto_to_config` takes it
    /// IN (the FE sets it), so Settings can actually turn memory off in the backend.
    #[test]
    fn dto_round_trips_user_memory_toggle() {
        // OUT: the DTO reflects the live value.
        let cfg = AppConfig {
            user_memory_enabled: false,
            ..Default::default()
        };
        assert!(!config_to_dto(&cfg).user_memory_enabled);

        // IN: the DTO value lands in the merged config (proven from a differing `current`).
        let mut dto = config_to_dto(&AppConfig::default());
        dto.user_memory_enabled = false;
        let current = AppConfig::default(); // ON
        assert!(
            !dto_to_config(dto, &current).user_memory_enabled,
            "a settings save must be able to turn cross-meeting memory off"
        );
    }

    /// BLK-4: `save_config`'s merge (`dto_to_config`) NEVER lets the DTO set cloud-egress consent —
    /// an omitting/zeroed save preserves an existing `true`, and a save carrying `true` cannot GRANT
    /// it. Only `consent_to_cloud_egress` may flip it.
    #[test]
    fn save_config_merge_never_clobbers_or_grants_consent() {
        // (a) preserve an existing grant even when the DTO carries false.
        let mut dto = config_to_dto(&AppConfig::default());
        dto.cloud_egress_consented = false;
        let current = AppConfig {
            cloud_egress_consented: true,
            ..AppConfig::default()
        };
        assert!(
            dto_to_config(dto, &current).cloud_egress_consented,
            "an omitting/false save must NOT clobber an existing consent (BLK-4)"
        );

        // (b) a save carrying true cannot GRANT consent (default-off stays off).
        let mut dto2 = config_to_dto(&AppConfig::default());
        dto2.cloud_egress_consented = true;
        let current2 = AppConfig::default(); // consent off
        assert!(
            !dto_to_config(dto2, &current2).cloud_egress_consented,
            "a settings save must NEVER grant consent — only the dedicated command may (BLK-4)"
        );
    }

    /// Sharing-onboarding gate: `sharing_choice_made` is PRESERVE-ONLY on the settings DTO exactly
    /// like the consent flags. `config_to_dto` carries the stored value OUT (so the init gateway can
    /// read it), but `dto_to_config` PRESERVES the live value — a save carrying `false` can't reopen
    /// an already-made choice, and a save carrying `true` can't set it (only the dedicated
    /// `mark_sharing_choice_made` command latches it).
    #[test]
    fn save_config_merge_never_clobbers_or_sets_sharing_choice_made() {
        // OUT: config_to_dto reflects the live value.
        let cfg_made = AppConfig {
            sharing_choice_made: true,
            ..AppConfig::default()
        };
        assert!(config_to_dto(&cfg_made).sharing_choice_made);

        // (a) an omitting/false save must NOT reopen an already-made choice.
        let mut dto = config_to_dto(&AppConfig::default());
        dto.sharing_choice_made = false;
        let current = AppConfig {
            sharing_choice_made: true,
            ..AppConfig::default()
        };
        assert!(
            dto_to_config(dto, &current).sharing_choice_made,
            "an omitting/false save must NOT clear the first-run sharing latch"
        );

        // (b) a save carrying true cannot SET the latch (default-off stays off — only the command latches).
        let mut dto2 = config_to_dto(&AppConfig::default());
        dto2.sharing_choice_made = true;
        let current2 = AppConfig::default(); // latch off
        assert!(
            !dto_to_config(dto2, &current2).sharing_choice_made,
            "a settings save must NEVER set the sharing latch — only mark_sharing_choice_made may"
        );
    }

    /// brain2 connectors (NEW EGRESS CLASS): `web_search_consented` is PRESERVE-ONLY on the settings
    /// DTO exactly like `cloud_egress_consented` — an omitting/false save can't clear an existing
    /// grant, and a save carrying `true` can't grant it. Only `consent_to_web_search` may flip it.
    #[test]
    fn save_config_merge_never_clobbers_or_grants_web_search_consent() {
        // (a) preserve an existing grant even when the DTO carries false.
        let mut dto = config_to_dto(&AppConfig::default());
        dto.web_search_consented = false;
        let current = AppConfig {
            web_search_consented: true,
            ..AppConfig::default()
        };
        assert!(
            dto_to_config(dto, &current).web_search_consented,
            "an omitting/false save must NOT clobber an existing web-search consent"
        );

        // (b) a save carrying true cannot GRANT consent (default-off stays off).
        let mut dto2 = config_to_dto(&AppConfig::default());
        dto2.web_search_consented = true;
        let current2 = AppConfig::default(); // consent off
        assert!(
            !dto_to_config(dto2, &current2).web_search_consented,
            "a settings save must NEVER grant web-search consent — only the dedicated command may"
        );
    }

    /// `web_search_enabled` IS settable from the DTO (unlike the consent flag): config_to_dto carries
    /// it out, dto_to_config takes it in (proven by starting from a different `current`).
    #[test]
    fn dto_takes_web_search_enabled_from_payload() {
        let dto_on = {
            let mut d = config_to_dto(&AppConfig::default());
            d.web_search_enabled = true;
            d
        };
        let current_off = AppConfig::default(); // enabled off
        assert!(
            dto_to_config(dto_on, &current_off).web_search_enabled,
            "web_search_enabled is settable from the DTO"
        );
        // And OUT: config_to_dto reflects the live value.
        let cfg = AppConfig {
            web_search_enabled: true,
            ..AppConfig::default()
        };
        assert!(config_to_dto(&cfg).web_search_enabled);
    }

    /// Phase H: the three brain toggles round-trip through the settings DTO. `config_to_dto` carries
    /// them OUT (so the FE can read them) and `dto_to_config` takes them IN (so the FE can set them),
    /// for every `BrainBackend` variant + the bool + a known model id.
    #[test]
    fn dto_round_trips_brain_backend_realtime_reactions_and_model_id() {
        for backend in [BrainBackend::Cloud, BrainBackend::Local, BrainBackend::Off] {
            let cfg = AppConfig {
                brain_backend: backend,
                realtime_reactions: true,
                brain_model_id: Some("bielik-11b-v3".to_string()),
                ..AppConfig::default()
            };
            // OUT: get_config carries the live values to the FE.
            let dto = config_to_dto(&cfg);
            assert_eq!(dto.brain_backend, backend);
            assert!(dto.realtime_reactions);
            assert_eq!(dto.brain_model_id.as_deref(), Some("bielik-11b-v3"));

            // IN: a settings save sets them from the DTO (start from a DIFFERENT current to prove the
            // value comes from the DTO, not preservation).
            let current = AppConfig {
                brain_backend: BrainBackend::Cloud,
                realtime_reactions: false,
                brain_model_id: Some("qwen3-4b-instruct-2507".to_string()),
                ..AppConfig::default()
            };
            let merged = dto_to_config(dto, &current);
            assert_eq!(merged.brain_backend, backend);
            assert!(merged.realtime_reactions);
            assert_eq!(merged.brain_model_id.as_deref(), Some("bielik-11b-v3"));
        }
    }

    /// brain2 RAG: `semantic_search_enabled` round-trips BOTH ways through the settings DTO. OUT:
    /// `config_to_dto` carries the live value so the FE toggle reflects it. IN: `dto_to_config` TAKES
    /// it from the DTO (settable — unlike `cloud_egress_consented`), proven by starting from a
    /// different `current` so the merged value can only have come from the DTO.
    #[test]
    fn dto_round_trips_semantic_search_enabled_both_ways() {
        // OUT: true and false both surface on the DTO.
        let cfg_on = AppConfig {
            semantic_search_enabled: true,
            ..AppConfig::default()
        };
        assert!(config_to_dto(&cfg_on).semantic_search_enabled);
        let cfg_off = AppConfig {
            semantic_search_enabled: false,
            ..AppConfig::default()
        };
        assert!(!config_to_dto(&cfg_off).semantic_search_enabled);

        // IN (set true): DTO=true over current=false ⇒ merged true (the DTO is authoritative).
        let mut dto_on = config_to_dto(&AppConfig::default());
        dto_on.semantic_search_enabled = true;
        let current_off = AppConfig {
            semantic_search_enabled: false,
            ..AppConfig::default()
        };
        assert!(
            dto_to_config(dto_on, &current_off).semantic_search_enabled,
            "semantic_search_enabled MUST be settable from the DTO (turn on)"
        );

        // IN (clear): DTO=false over current=true ⇒ merged false (settable both directions — NOT
        // preserve-only like cloud_egress_consented).
        let mut dto_off = config_to_dto(&AppConfig::default());
        dto_off.semantic_search_enabled = false;
        let current_on = AppConfig {
            semantic_search_enabled: true,
            ..AppConfig::default()
        };
        assert!(
            !dto_to_config(dto_off, &current_on).semantic_search_enabled,
            "semantic_search_enabled MUST be settable from the DTO (turn off)"
        );
    }

    /// Brain/AI picker: `provider_model` + `provider_effort` round-trip through the settings DTO
    /// BOTH ways. OUT: `config_to_dto` carries the live values so the FE pickers reflect them. IN:
    /// `dto_to_config` TAKES them from the DTO (settable — like `anthropic_model`, NOT preserve-only),
    /// proven by starting from a different `current` so the merged value can only have come from the DTO.
    #[test]
    fn dto_round_trips_provider_model_and_effort_both_ways() {
        // OUT.
        let cfg = AppConfig {
            provider_model: "claude-sonnet-4-6".to_string(),
            provider_effort: "high".to_string(),
            ..AppConfig::default()
        };
        let dto = config_to_dto(&cfg);
        assert_eq!(dto.provider_model, "claude-sonnet-4-6");
        assert_eq!(dto.provider_effort, "high");

        // IN (set): DTO values over a DIFFERENT current ⇒ merged values come from the DTO.
        let mut dto_in = config_to_dto(&AppConfig::default());
        dto_in.provider_model = "claude-haiku-4-5".to_string();
        dto_in.provider_effort = "low".to_string();
        let current = AppConfig {
            provider_model: "claude-opus-4-8".to_string(),
            provider_effort: "medium".to_string(),
            ..AppConfig::default()
        };
        let merged = dto_to_config(dto_in, &current);
        assert_eq!(merged.provider_model, "claude-haiku-4-5");
        assert_eq!(merged.provider_effort, "low");

        // IN (clear to provider default): DTO="" over current set ⇒ merged "" (settable both ways).
        let mut dto_clear = config_to_dto(&AppConfig::default());
        dto_clear.provider_model = String::new();
        dto_clear.provider_effort = String::new();
        let current2 = AppConfig {
            provider_model: "claude-opus-4-8".to_string(),
            provider_effort: "high".to_string(),
            ..AppConfig::default()
        };
        let merged2 = dto_to_config(dto_clear, &current2);
        assert_eq!(merged2.provider_model, "");
        assert_eq!(merged2.provider_effort, "");
    }

    /// Model roles: the 9 role keys round-trip through the settings DTO BOTH ways (settable, like
    /// `gateway_model` — NOT preserve-only), and an OLDER FE payload that omits them entirely
    /// deserializes to `""` (inherit-legacy) — a partial save can never flip a role.
    #[test]
    fn dto_round_trips_role_keys_and_omitted_keys_default_empty() {
        // OUT: config_to_dto carries the live values.
        let cfg = AppConfig {
            role_notes_connection: "anthropic".to_string(),
            role_notes_model: "claude-opus-4-8".to_string(),
            role_notes_effort: "high".to_string(),
            role_ask_connection: "ollama".to_string(),
            role_ask_model: "mistral-small".to_string(),
            role_live_connection: "off".to_string(),
            ..AppConfig::default()
        };
        let dto = config_to_dto(&cfg);
        assert_eq!(dto.role_notes_connection, "anthropic");
        assert_eq!(dto.role_notes_model, "claude-opus-4-8");
        assert_eq!(dto.role_notes_effort, "high");
        assert_eq!(dto.role_ask_connection, "ollama");
        assert_eq!(dto.role_ask_model, "mistral-small");
        assert_eq!(dto.role_ask_effort, "");
        assert_eq!(dto.role_live_connection, "off");

        // IN: dto_to_config takes them from the DTO (start from a DIFFERENT current to prove the
        // value comes from the DTO, not preservation) — including clearing back to "".
        let current = AppConfig {
            role_notes_connection: "gateway".to_string(),
            role_ask_connection: "claude_code".to_string(),
            role_live_connection: "local".to_string(),
            role_live_model: "bielik-11b-v3".to_string(),
            ..AppConfig::default()
        };
        let merged = dto_to_config(dto, &current);
        assert_eq!(merged.role_notes_connection, "anthropic");
        assert_eq!(merged.role_ask_connection, "ollama");
        assert_eq!(merged.role_live_connection, "off");
        assert_eq!(
            merged.role_live_model, "",
            "a \"\" role key from the DTO clears the override"
        );

        // An older FE payload OMITTING the role keys deserializes them to "" (#[serde(default)]).
        let json = serde_json::to_string(&config_to_dto(&AppConfig::default())).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let obj = v.as_object_mut().unwrap();
        for k in [
            "roleNotesConnection",
            "roleNotesModel",
            "roleNotesEffort",
            "roleAskConnection",
            "roleAskModel",
            "roleAskEffort",
            "roleLiveConnection",
            "roleLiveModel",
            "roleLiveEffort",
        ] {
            assert!(obj.remove(k).is_some(), "DTO must serialize {k}");
        }
        let dto_old: AppConfigDto = serde_json::from_value(v).unwrap();
        assert_eq!(dto_old.role_notes_connection, "");
        assert_eq!(dto_old.role_ask_connection, "");
        assert_eq!(dto_old.role_live_connection, "");
    }

    /// Phase H graceful degradation: a DTO carrying an UNKNOWN `brain_model_id` must NOT be stored —
    /// the live selection is preserved (no error, no bogus id) — while an unknown/omitted
    /// `brain_backend` deserializes to the default `Cloud` rather than crashing the save.
    #[test]
    fn dto_unknown_brain_model_id_preserved_and_unknown_backend_defaults_cloud() {
        // (a) unknown model id ⇒ ignored, current selection preserved. The `current` here is the now
        // RETIRED `qwen2.5-3b`, so this doubles as the installed-base guarantee: a settings save must
        // not wipe a persisted retired selection (its on-disk GGUF keeps resolving; see reason.rs).
        let mut dto = config_to_dto(&AppConfig::default());
        dto.brain_model_id = Some("totally-made-up-model".to_string());
        let current = AppConfig {
            brain_model_id: Some("qwen2.5-3b".to_string()),
            ..AppConfig::default()
        };
        assert_eq!(
            dto_to_config(dto, &current).brain_model_id.as_deref(),
            Some("qwen2.5-3b"),
            "an unknown brain_model_id must be ignored, preserving the live selection"
        );

        // (b) a None model id likewise preserves the current selection (a settings save without a
        // brain pick must not clear an existing one).
        let mut dto_none = config_to_dto(&AppConfig::default());
        dto_none.brain_model_id = None;
        let current_some = AppConfig {
            brain_model_id: Some("qwen3-14b".to_string()),
            ..AppConfig::default()
        };
        assert_eq!(
            dto_to_config(dto_none, &current_some)
                .brain_model_id
                .as_deref(),
            Some("qwen3-14b")
        );

        // (d) FIX D: a CUSTOM brain_model_path IS settable verbatim (no registry validation — it's a
        // local file path). It round-trips OUT (config_to_dto) and IN (dto_to_config) unchanged, even
        // while a bogus brain_model_id is (correctly) ignored on the same save.
        let cfg_with_path = AppConfig {
            brain_model_path: Some("/models/custom.gguf".to_string()),
            ..AppConfig::default()
        };
        let dto_path = config_to_dto(&cfg_with_path);
        assert_eq!(
            dto_path.brain_model_path.as_deref(),
            Some("/models/custom.gguf"),
            "config_to_dto carries the custom path OUT to the FE"
        );
        let merged_path = dto_to_config(dto_path, &AppConfig::default());
        assert_eq!(
            merged_path.brain_model_path.as_deref(),
            Some("/models/custom.gguf"),
            "dto_to_config stores the custom path verbatim (settable, not preserve-only)"
        );

        // (e) an EMPTY brainModelPath clears the custom path (→ None, fall back to the registry id).
        let mut dto_empty = config_to_dto(&AppConfig::default());
        dto_empty.brain_model_path = Some(String::new());
        let current_path = AppConfig {
            brain_model_path: Some("/models/old.gguf".to_string()),
            ..AppConfig::default()
        };
        assert_eq!(
            dto_to_config(dto_empty, &current_path).brain_model_path,
            None,
            "an empty brainModelPath clears the custom path"
        );

        // (f) the FE sends camelCase `brainModelPath`; it must deserialize into the field (and an
        // omitted value defaults to None). Same minimal-but-valid JSON shape as case (c) below.
        let json_path = r#"{
            "providerId":"claude_code","anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "captureSystemAudio":false,"modelSize":"large-v3","voiceTrigger":false,"onboarded":true,
            "noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto",
            "brainModelPath":"/m/x.gguf"
        }"#;
        let parsed: AppConfigDto = serde_json::from_str(json_path).unwrap();
        assert_eq!(parsed.brain_model_path.as_deref(), Some("/m/x.gguf"));

        // (c) an unknown/omitted brainBackend token deserializes to the default Cloud (no crash),
        // then flows through dto_to_config as Cloud.
        let json = r#"{
            "providerId":"claude_code","anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "captureSystemAudio":false,"modelSize":"large-v3","voiceTrigger":false,"onboarded":true,
            "noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto","brainBackend":"bogus"
        }"#;
        let dto_bad: AppConfigDto = serde_json::from_str(json).unwrap();
        assert_eq!(
            dto_bad.brain_backend,
            BrainBackend::Cloud,
            "unknown token → Cloud"
        );
        assert!(
            !dto_bad.realtime_reactions,
            "omitted realtimeReactions defaults OFF"
        );
        assert_eq!(
            dto_to_config(dto_bad, &AppConfig::default()).brain_backend,
            BrainBackend::Cloud
        );
    }

    // ── brain model registry: select + download-target resolution ───────────────────────────────

    /// `select_brain_model` validates the id against the registry and PERSISTS it; reloading config
    /// from the DB returns the chosen id. An unknown id is rejected with `InvalidArg` and leaves the
    /// stored selection untouched.
    #[test]
    fn select_brain_model_persists_valid_and_rejects_unknown() {
        let state = build_state("brain-select");

        select_brain_model_inner(&state, "bielik-11b-v3".to_string()).unwrap();
        assert_eq!(
            state.config.lock().unwrap().brain_model_id.as_deref(),
            Some("bielik-11b-v3")
        );
        // Selecting a HEAVY model also wires the class handle so heavy()/Fully-Local uses it.
        assert_eq!(
            state.config.lock().unwrap().brain_heavy_model_id.as_deref(),
            Some("bielik-11b-v3"),
            "selecting a heavy model must set brain_heavy_model_id (else heavy() ignores the choice)"
        );
        // Survives a reload from the settings table.
        assert_eq!(
            AppConfig::load(&state.db)
                .unwrap()
                .brain_model_id
                .as_deref(),
            Some("bielik-11b-v3")
        );

        // Selecting a LIGHT model wires the light class handle (the Brain-Live path) — the bug this
        // guards: a light selection that only set brain_model_id left light() on the registry default.
        select_brain_model_inner(&state, "qwen3-1.7b".to_string()).unwrap();
        assert_eq!(
            state.config.lock().unwrap().brain_light_model_id.as_deref(),
            Some("qwen3-1.7b"),
            "selecting a light model must set brain_light_model_id (else Realtime Reactions stay stub)"
        );

        // Unknown id ⇒ InvalidArg, selection unchanged (still the last valid pick, qwen3-1.7b).
        let err = select_brain_model_inner(&state, "not-a-real-model".to_string()).unwrap_err();
        assert!(matches!(err, AppError::InvalidArg(_)));
        assert_eq!(
            state.config.lock().unwrap().brain_model_id.as_deref(),
            Some("qwen3-1.7b")
        );
    }

    /// `select_embed_model` validates the id, PERSISTS it, and reports `reindex_needed` only when the
    /// resolved model actually CHANGED (a different model's vectors are stale). Unknown id ⇒
    /// `InvalidArg` with the selection untouched. Serialized on the embedder-selection global lock;
    /// restores the default (`None`) at the end so it cannot leak state into parallel tests.
    #[test]
    fn select_embed_model_persists_and_flags_reindex() {
        let _g = crate::embed::EMBED_SELECTION_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let state = build_state("embed-select");

        // Selecting the SAME (default) model as the effective one ⇒ no re-index needed.
        let res = select_embed_model_inner(&state, "multilingual-e5-small".to_string()).unwrap();
        assert_eq!(res.selected, "multilingual-e5-small");
        assert!(
            !res.reindex_needed,
            "re-selecting the default must not force a re-index"
        );

        // Switching to mmlw CHANGES the resolved model ⇒ re-index needed; persists + reloads.
        let res = select_embed_model_inner(&state, "mmlw-retrieval-e5-small".to_string()).unwrap();
        assert_eq!(res.selected, "mmlw-retrieval-e5-small");
        assert!(
            res.reindex_needed,
            "changing the embed model must flag a re-index"
        );
        assert_eq!(
            state.config.lock().unwrap().embed_model_id.as_deref(),
            Some("mmlw-retrieval-e5-small")
        );
        assert_eq!(
            AppConfig::load(&state.db)
                .unwrap()
                .embed_model_id
                .as_deref(),
            Some("mmlw-retrieval-e5-small")
        );

        // Unknown id ⇒ InvalidArg, selection unchanged.
        let err = select_embed_model_inner(&state, "not-a-real-embedder".to_string()).unwrap_err();
        assert!(matches!(err, AppError::InvalidArg(_)));
        assert_eq!(
            state.config.lock().unwrap().embed_model_id.as_deref(),
            Some("mmlw-retrieval-e5-small")
        );

        // Restore the default so the shared process-global doesn't leak mmlw into other tests.
        crate::embed::set_selected_embed_model_id(None);
    }

    /// The download-target resolver rejects an unknown id (the exact guard `download_brain_model`
    /// enforces before any network I/O) and resolves a known id to its registry URL + a path inside
    /// the shared models dir.
    #[test]
    fn brain_download_target_rejects_unknown_and_resolves_known() {
        assert!(matches!(
            brain_download_target("bogus-id"),
            Err(AppError::InvalidArg(_))
        ));
        // A RETIRED id is also rejected by the download target (un-selectable / un-downloadable fresh;
        // the installed-base file, if present, is only RESOLVED, never re-fetched).
        assert!(matches!(
            brain_download_target("qwen2.5-3b"),
            Err(AppError::InvalidArg(_))
        ));
        let (url, dest) = brain_download_target("qwen3-1.7b").unwrap();
        assert_eq!(
            url,
            "https://huggingface.co/bartowski/Qwen_Qwen3-1.7B-GGUF/resolve/main/Qwen_Qwen3-1.7B-Q4_K_M.gguf"
        );
        assert!(dest.ends_with("Qwen_Qwen3-1.7B-Q4_K_M.gguf"));
    }

    // ── rename_folder / delete_folder (folder lifecycle) ────────────────────────────────────────

    /// A fresh, unique temp vault dir for the FS-side rename/delete tests.
    fn tmp_vault(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "murmur-folderlc-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// An [`AppState`] with a REAL temp vault dir configured, so the FS-side of rename/delete (dir
    /// move/remove + note `.md` move) actually runs (the keyless `build_state` skips it).
    fn build_state_with_vault(tag: &str, vault: &std::path::Path) -> AppState {
        let s = build_state(tag);
        {
            let mut c = s.config.lock().unwrap();
            c.vault_path = Some(vault.to_string_lossy().to_string());
        }
        s
    }

    fn make_child_folder(db: &Db, id: &str, name: &str, path: &str, parent_id: &str) {
        db.insert_folder(&Folder {
            id: id.to_string(),
            name: name.to_string(),
            path: path.to_string(),
            parent_id: Some(parent_id.to_string()),
            locked: false,
            created_at: "2026-06-27T08:30:00Z".to_string(),
        })
        .unwrap();
    }

    /// Renaming an OPEN folder updates `name` + `path`, MOVES the on-disk vault subdir, and re-points
    /// each note's `exported_path` — content (the `.md` bytes) survives byte-identical.
    #[test]
    fn rename_open_folder_moves_dir_and_reprefixes_paths() {
        let vault = tmp_vault("rename-open");
        let state = build_state_with_vault("rename-open", &vault);

        // An open folder "Work" with one note whose `.md` lives in <vault>/Work/.
        make_open_folder(&state.db, "f1", "Work");
        let work_dir = vault.join("Work");
        std::fs::create_dir_all(&work_dir).unwrap();
        let md_path = work_dir.join("note.md");
        std::fs::write(&md_path, "# real content").unwrap();
        seed_meeting(&state.db, "m1", "# real content", Some("f1"));
        state
            .db
            .set_note_exported_path("m1", "claude_code", &md_path.to_string_lossy())
            .unwrap();

        let renamed = rename_folder_inner(&state, "f1".into(), "Projects".into()).unwrap();
        assert_eq!(renamed.name, "Projects");
        assert_eq!(renamed.path, "Projects");

        // DB row updated.
        let f = state.db.folder_by_id("f1").unwrap().unwrap();
        assert_eq!(f.name, "Projects");
        assert_eq!(f.path, "Projects");

        // On-disk subdir moved (old gone, new present with the SAME bytes).
        assert!(!work_dir.exists(), "old dir gone after rename");
        let new_md = vault.join("Projects").join("note.md");
        assert!(new_md.exists(), "note .md moved into the renamed dir");
        assert_eq!(std::fs::read_to_string(&new_md).unwrap(), "# real content");

        // exported_path re-pointed under the new dir (compare canonicalized — the stored path is the
        // canonicalized absolute form, which on macOS is /private/var… vs the test's /var…).
        let n = state.db.get_latest_note_for_meeting("m1").unwrap().unwrap();
        let stored = n.exported_path.expect("note still has an exported path");
        assert_eq!(
            std::fs::canonicalize(&stored).unwrap(),
            std::fs::canonicalize(&new_md).unwrap(),
            "exported_path points at the moved .md"
        );

        let _ = std::fs::remove_dir_all(&vault);
    }

    /// Renaming a LOCKED folder is METADATA-ONLY: the row name+path change, but no sealed content is
    /// touched — `locked` stays true, the `wrapped_key` is unchanged, and the note's `content_blob`
    /// (the ciphertext) is byte-identical before/after. The blanked plaintext stays blanked.
    #[test]
    fn rename_locked_folder_is_metadata_only_and_never_touches_sealed_content() {
        let vault = tmp_vault("rename-locked");
        let state = build_state_with_vault("rename-locked", &vault);
        std::fs::create_dir_all(vault.join("Secret")).unwrap();

        make_open_folder(&state.db, "lf", "Secret");
        seed_meeting(&state.db, "ms", "# top secret strategy", Some("lf"));
        lock_folder_inner(&state, "lf".to_string()).unwrap(); // seal it (NOT session-unlocked)

        let wrapped_before = state.db.folder_wrapped_key("lf").unwrap();
        let blob_before = state.db.sealable_notes_for_meeting("ms").unwrap()[0]
            .content_blob
            .clone();
        assert!(blob_before.is_some(), "sealed note has a content_blob");

        let renamed = rename_folder_inner(&state, "lf".into(), "Vault".into()).unwrap();
        assert_eq!(renamed.name, "Vault");

        let f = state.db.folder_by_id("lf").unwrap().unwrap();
        assert!(f.locked, "still sealed after a metadata rename");
        assert_eq!(f.name, "Vault");
        assert_eq!(f.path, "Vault");
        assert_eq!(
            state.db.folder_wrapped_key("lf").unwrap(),
            wrapped_before,
            "the wrapped CK is untouched by a rename"
        );
        let after = &state.db.sealable_notes_for_meeting("ms").unwrap()[0];
        assert_eq!(
            after.content_blob, blob_before,
            "ciphertext byte-identical after rename"
        );
        assert!(after.markdown.is_empty(), "blanked plaintext stays blanked");

        let _ = std::fs::remove_dir_all(&vault);
    }

    /// A rename re-prefixes DESCENDANT folder paths too (a child of the renamed folder moves with it).
    #[test]
    fn rename_reprefixes_descendant_folder_paths() {
        let state = build_state("rename-desc"); // no vault → pure DB path rewrite
        make_open_folder(&state.db, "parent", "Work");
        make_child_folder(&state.db, "child", "Q3", "Work/Q3", "parent");

        rename_folder_inner(&state, "parent".into(), "Projects".into()).unwrap();

        assert_eq!(
            state.db.folder_by_id("parent").unwrap().unwrap().path,
            "Projects"
        );
        assert_eq!(
            state.db.folder_by_id("child").unwrap().unwrap().path,
            "Projects/Q3",
            "the child's path moves under the renamed parent"
        );
    }

    /// Deleting an OPEN folder moves its notes to the vault ROOT (folder_id = NULL), survives the
    /// note bytes (the `.md` moves to the root), deletes the folder row, and removes the empty subdir.
    #[test]
    fn delete_open_folder_demotes_notes_to_root_and_removes_dir() {
        let vault = tmp_vault("del-open");
        let state = build_state_with_vault("del-open", &vault);

        make_open_folder(&state.db, "f", "Trash-Me");
        let dir = vault.join("Trash-Me");
        std::fs::create_dir_all(&dir).unwrap();
        let md = dir.join("keep.md");
        std::fs::write(&md, "# must survive").unwrap();
        seed_meeting(&state.db, "m", "# must survive", Some("f"));
        state
            .db
            .set_note_exported_path("m", "claude_code", &md.to_string_lossy())
            .unwrap();

        delete_folder_inner(&state, "f".into()).unwrap();

        // Folder row gone.
        assert!(
            state.db.folder_by_id("f").unwrap().is_none(),
            "folder row deleted"
        );
        // Note survived, now at the root (folder_id NULL).
        assert_eq!(
            state.db.folder_for_meeting("m").unwrap(),
            None,
            "note demoted to All notes"
        );
        let n = state.db.get_latest_note_for_meeting("m").unwrap().unwrap();
        assert_eq!(n.markdown, "# must survive", "note content never lost");
        let root_md = vault.join("keep.md");
        assert!(root_md.exists(), ".md moved to the vault root");
        assert_eq!(std::fs::read_to_string(&root_md).unwrap(), "# must survive");
        assert!(!dir.exists(), "emptied folder dir removed");

        let _ = std::fs::remove_dir_all(&vault);
    }

    /// SECURITY: deleting a LOCKED folder that is NOT session-unlocked is REFUSED (`AppError::Locked`)
    /// — the row (with the wrapped key) and the sealed `content_blob` are untouched, so nothing is
    /// orphaned encrypted-and-unrecoverable.
    #[test]
    fn delete_locked_not_unlocked_folder_refuses_and_keeps_sealed_content() {
        let state = build_state("del-locked");
        make_open_folder(&state.db, "lf", "Sealed");
        seed_meeting(&state.db, "m", "# confidential", Some("lf"));
        lock_folder_inner(&state, "lf".to_string()).unwrap(); // sealed, NOT session-unlocked

        let res = delete_folder_inner(&state, "lf".into());
        assert!(
            matches!(res, Err(AppError::Locked(_))),
            "must refuse with Locked, got {res:?}"
        );

        // Folder + sealed content intact.
        assert!(
            state.db.folder_by_id("lf").unwrap().is_some(),
            "folder NOT deleted"
        );
        assert!(
            state.db.folder_wrapped_key("lf").unwrap().is_some(),
            "wrapped key kept"
        );
        let n = &state.db.sealable_notes_for_meeting("m").unwrap()[0];
        assert!(n.content_blob.is_some(), "ciphertext kept (never orphaned)");
        assert_eq!(
            state.db.folder_for_meeting("m").unwrap().as_deref(),
            Some("lf")
        );
    }

    /// SECURITY: deleting a LOCKED + SESSION-UNLOCKED folder UNSEALS its notes back to plaintext
    /// (remove-lock) BEFORE the row is destroyed, then demotes them to the root — so nothing is left
    /// encrypted-and-orphaned and no note is lost.
    #[test]
    fn delete_locked_session_unlocked_folder_unseals_then_demotes_notes() {
        let vault = tmp_vault("del-unlocked");
        let state = build_state_with_vault("del-unlocked", &vault);
        std::fs::create_dir_all(vault.join("Secret")).unwrap();

        make_open_folder(&state.db, "lf", "Secret");
        seed_meeting(&state.db, "m", "# decrypt me back", Some("lf"));
        lock_folder_inner(&state, "lf".to_string()).unwrap();

        // Make it SESSION-UNLOCKED: in the unlock set + KEK cached (as a real unlock would leave it).
        state
            .unlocked_folders
            .lock()
            .unwrap()
            .insert("lf".to_string());
        let kek = secrets::get_or_create_master_kek().unwrap();
        *state.master_kek.lock().unwrap() = Some(Zeroizing::new(kek));

        delete_folder_inner(&state, "lf".into()).unwrap();

        // Folder gone; note unsealed (plaintext restored, blob cleared) and demoted to the root.
        assert!(
            state.db.folder_by_id("lf").unwrap().is_none(),
            "folder row deleted"
        );
        assert_eq!(
            state.db.folder_for_meeting("m").unwrap(),
            None,
            "note demoted to All notes"
        );
        let n = &state.db.sealable_notes_for_meeting("m").unwrap()[0];
        assert_eq!(
            n.markdown, "# decrypt me back",
            "plaintext restored before delete"
        );
        assert!(
            n.content_blob.is_none(),
            "no orphaned ciphertext left behind"
        );
        // The session set no longer references the deleted folder.
        assert!(!state.unlocked_folders.lock().unwrap().contains("lf"));

        let _ = std::fs::remove_dir_all(&vault);
    }

    /// Deleting a folder that still has CHILD folders is refused (`InvalidArg`) — never orphan a
    /// subtree by dangling a child's parent_id.
    #[test]
    fn delete_folder_with_children_refuses() {
        let state = build_state("del-children");
        make_open_folder(&state.db, "parent", "Work");
        make_child_folder(&state.db, "child", "Q3", "Work/Q3", "parent");

        let res = delete_folder_inner(&state, "parent".into());
        assert!(
            matches!(res, Err(AppError::InvalidArg(_))),
            "must refuse, got {res:?}"
        );
        assert!(
            state.db.folder_by_id("parent").unwrap().is_some(),
            "parent NOT deleted"
        );
        assert!(
            state.db.folder_by_id("child").unwrap().is_some(),
            "child NOT orphaned"
        );
    }

    // ── reindex_embeddings (semantic backfill) ──────────────────────────────────────────────────

    /// True iff `mid` surfaces in the GATED semantic read for `text` under `unlocked` — i.e. it has
    /// vec0 chunks AND is visible. Uses the stub embedder (this test asserts WHICH meetings are
    /// indexed, not retrieval QUALITY, so the deterministic stub is sufficient plumbing).
    fn reindex_semantic_finds(db: &Db, mid: &str, text: &str, unlocked: &HashSet<String>) -> bool {
        use crate::embed::Embedder;
        let emb = crate::embed::StubEmbedder;
        let qv = emb.embed(std::slice::from_ref(&text.to_string())).unwrap();
        let qvec = qv.into_iter().next().unwrap_or_default();
        db.search_semantic_visible(&qvec, 50, unlocked)
            .unwrap()
            .iter()
            .any(|h| h.meeting.id == mid)
    }

    /// GATING: `reindex_embeddings_inner` over a corpus of two OPEN (visible) meetings + one SEALED
    /// (locked, not-session-unlocked) meeting indexes ONLY the two visible ones. The sealed meeting
    /// is never returned by `list_meetings_visible`, its plaintext is never chunked/embedded, and its
    /// chunks STAY purged (the seal already removed them) — RED if the gate were dropped.
    #[test]
    fn reindex_indexes_only_visible_meetings_skips_sealed() {
        let state = build_state("reindex-gate");
        make_open_folder(&state.db, "f-lock", "Confidential");

        // Two open meetings (no folder ⇒ always visible) + one in a folder we will SEAL.
        seed_meeting(
            &state.db,
            "m-open-1",
            "Quarterly budget planning and hiring runway.",
            None,
        );
        seed_meeting(
            &state.db,
            "m-open-2",
            "Roadmap review for the next sprint.",
            None,
        );
        seed_meeting(
            &state.db,
            "m-sealed",
            "Secret acquisition numbers and the term sheet.",
            Some("f-lock"),
        );

        // Seal the folder (verify-before-destroy seal + chunk purge) — m-sealed is now invisible.
        lock_folder_inner(&state, "f-lock".to_string()).unwrap();

        let nothing = HashSet::new();
        let stub = crate::embed::StubEmbedder;
        // model_present = true so the guard passes (we deliberately use the stub here for plumbing).
        let res = reindex_embeddings_inner(&state.db, &nothing, true, &stub, |_, _| {}).unwrap();
        assert_eq!(res.status, "indexed");
        // Only the two VISIBLE meetings were processed (the sealed one is absent from the corpus).
        assert_eq!(
            res.total, 2,
            "sealed meeting must NOT be in the reindex corpus"
        );
        assert_eq!(res.indexed, 2);

        // The two open meetings are now semantically findable; the sealed one is NOT — even under the
        // same empty unlock set (it has no chunks, and is gated out).
        assert!(reindex_semantic_finds(
            &state.db,
            "m-open-1",
            "budget planning hiring",
            &nothing
        ));
        assert!(reindex_semantic_finds(
            &state.db,
            "m-open-2",
            "roadmap sprint review",
            &nothing
        ));
        assert!(
            !reindex_semantic_finds(
                &state.db,
                "m-sealed",
                "secret acquisition term sheet",
                &nothing
            ),
            "a sealed-not-unlocked meeting must never be indexed by reindex (gate violation)"
        );
    }

    /// MODEL GUARD: with the real e5 model ABSENT (`model_present = false`), `reindex_embeddings_inner`
    /// returns `{ status: "model_missing" }` and indexes NOTHING — it must NOT poison the index with
    /// the deterministic STUB embedder. RED if the guard were dropped (the open meeting would gain
    /// stub chunks).
    #[test]
    fn reindex_model_missing_indexes_nothing() {
        let state = build_state("reindex-nomodel");
        seed_meeting(&state.db, "m-open", "Quarterly budget planning.", None);

        let nothing = HashSet::new();
        let stub = crate::embed::StubEmbedder;
        // model_present = false → the guard short-circuits BEFORE any indexing.
        let res = reindex_embeddings_inner(&state.db, &nothing, false, &stub, |_, _| {}).unwrap();
        assert_eq!(res.status, "model_missing");
        assert_eq!(res.indexed, 0);
        assert_eq!(res.total, 0);
        // No chunks were written — the meeting is NOT semantically findable.
        assert!(
            !reindex_semantic_finds(&state.db, "m-open", "budget planning", &nothing),
            "model_missing guard must index NOTHING (no stub poisoning)"
        );
    }

    // ── FIX 6 integration: save_config_inner → validate_gateway_url boundary ─────────────────

    /// Drives the REAL `save_config_inner` (the headless core of `save_config`) to guard the
    /// integration boundary: a future edit that removes the URL-validation guard from
    /// `save_config_inner` would break this test, not just the unit-level `validate_gateway_url`
    /// call-site tests.
    ///
    /// Three sub-cases:
    ///   (a) credential-bearing URL → `InvalidArg`; persisted config IS NOT changed.
    ///   (b) empty URL → `Ok`; empty round-trips.
    ///   (c) valid https URL → `Ok`; value persists and is visible in the in-memory cache.
    #[test]
    fn save_config_inner_validates_gateway_url_at_the_integration_seam() {
        let state = build_state("cfg-gw-url-integration");

        // ── (a) credential-bearing URL → rejected before writing to DB ───────────────────────
        let mut dto_bad = config_to_dto(&AppConfig::default());
        dto_bad.gateway_base_url = "https://key:@gw.example.com/v1".to_string();
        let err = save_config_inner(&state, dto_bad)
            .expect_err("credential URL must be rejected by save_config_inner");
        assert!(
            matches!(err, AppError::InvalidArg(_)),
            "expected InvalidArg for credential URL, got: {err:?}"
        );
        // The bad URL must NOT have been written into the in-memory config cache.
        let cached_url = state.config.lock().unwrap().gateway_base_url.clone();
        assert_ne!(
            cached_url, "https://key:@gw.example.com/v1",
            "credential URL must not reach the config cache (save_config_inner must reject first)"
        );

        // ── (b) empty URL → Ok (no gateway configured) ───────────────────────────────────────
        let mut dto_empty = config_to_dto(&AppConfig::default());
        dto_empty.gateway_base_url = String::new();
        save_config_inner(&state, dto_empty).expect("empty gateway URL must be accepted");
        assert_eq!(
            state.config.lock().unwrap().gateway_base_url,
            "",
            "empty gateway URL must persist and round-trip"
        );

        // ── (c) valid https URL → Ok + persisted in the cache ────────────────────────────────
        let mut dto_ok = config_to_dto(&AppConfig::default());
        dto_ok.gateway_base_url = "https://gw.example.com/v1".to_string();
        save_config_inner(&state, dto_ok).expect("valid https gateway URL must be accepted");
        assert_eq!(
            state.config.lock().unwrap().gateway_base_url,
            "https://gw.example.com/v1",
            "valid URL must be persisted and visible in the in-memory config cache"
        );
    }

    // ─── list_models — static connection catalogs (pure, no network) ─────────────────────────

    /// `claude_code` and `anthropic` both serve the curated CLAUDE_MODELS constant — the single
    /// source of truth that replaced the FE-hardcoded dropdown list.
    #[test]
    fn list_models_claude_connections_return_curated_constant() {
        for conn in ["claude_code", "anthropic"] {
            let ids = static_connection_models(conn)
                .unwrap_or_else(|e| panic!("{conn} must resolve statically: {e:?}"));
            let want: Vec<String> = crate::summarize::provider::CLAUDE_MODELS
                .iter()
                .map(|id| id.to_string())
                .collect();
            assert_eq!(ids, want, "{conn} must serve CLAUDE_MODELS verbatim");
            assert!(
                ids.contains(&"claude-opus-4-8".to_string()),
                "curated list must include the default Opus id"
            );
        }
    }

    /// `local` serves exactly the BRAIN_MODELS registry ids, in registry order.
    #[test]
    fn list_models_local_matches_brain_registry_ids() {
        let ids = static_connection_models("local").expect("local must resolve statically");
        let want: Vec<String> = crate::reason::BRAIN_MODELS
            .iter()
            .map(|m| m.id.to_string())
            .collect();
        assert_eq!(ids, want, "local must mirror the BRAIN_MODELS registry ids");
        assert!(!ids.is_empty(), "the brain registry is never empty");
    }

    /// `off` is a valid connection with no models — empty list, not an error.
    #[test]
    fn list_models_off_returns_empty() {
        let ids = static_connection_models("off").expect("off must resolve statically");
        assert!(ids.is_empty(), "off runs no models");
    }

    /// An unknown connection id refuses with `InvalidArg` — never a panic, never an empty Ok.
    #[test]
    fn list_models_unknown_connection_refuses() {
        for conn in ["", "openai", "GATEWAY", "claude"] {
            let err =
                static_connection_models(conn).expect_err("unknown connection must be refused");
            assert!(
                matches!(err, AppError::InvalidArg(_)),
                "expected InvalidArg for '{conn}', got: {err:?}"
            );
        }
    }

    // ── recording_status (webview-reload resync) ─────────────────────────────────────────────────
    // A `tauri dev` FE hot-reload swaps the webview WITHOUT restarting the Rust process, so the store
    // resets to `idle` while the backend recorder is still `Some(..)`. `recording_status` lets the
    // freshly-loaded FE resync instead of pressing Start and hitting the "already recording" guard.
    // `Recorder` needs mic hardware (can't be built headless), so we test the pure DTO builder that
    // the command delegates to.

    #[test]
    fn recording_status_reports_idle_when_not_recording() {
        let state = build_state("recstatus-idle");
        // Even a lingering meeting row + a set `current_meeting` must NOT read as recording: the live
        // recorder is the source of truth, and here it is `None`.
        state
            .db
            .insert_meeting(&Meeting {
                id: "ghost".to_string(),
                started_at: "2026-07-05T10:00:00Z".to_string(),
                ended_at: None,
                title: None,
                duration_s: 0,
                audio_path: None,
                status: MeetingStatus::Recording,
                folder_id: None,
            })
            .unwrap();
        let st = recording_status_dto(&state.db, false, Some("ghost".to_string()));
        assert!(!st.recording);
        assert_eq!(st.meeting_id, None);
        assert_eq!(st.started_at, None);
    }

    #[test]
    fn recording_status_anchors_started_at_from_the_persisted_row() {
        let state = build_state("recstatus-live");
        state
            .db
            .insert_meeting(&Meeting {
                id: "live-1".to_string(),
                started_at: "2026-07-05T11:22:33Z".to_string(),
                ended_at: None,
                title: None,
                duration_s: 0,
                audio_path: None,
                status: MeetingStatus::Recording,
                folder_id: None,
            })
            .unwrap();
        let st = recording_status_dto(&state.db, true, Some("live-1".to_string()));
        assert!(st.recording);
        assert_eq!(st.meeting_id.as_deref(), Some("live-1"));
        // The FE anchors its elapsed timer to THIS, not an epoch-sized value.
        assert_eq!(st.started_at.as_deref(), Some("2026-07-05T11:22:33Z"));
    }

    #[test]
    fn recording_status_degrades_when_the_row_is_missing() {
        // Recording is true but the row can't be read → still report recording, just drop the anchor
        // (the FE falls back to "now"); the status read must never fail on a best-effort lookup.
        let state = build_state("recstatus-norow");
        let st = recording_status_dto(&state.db, true, Some("no-such-meeting".to_string()));
        assert!(st.recording);
        assert_eq!(st.meeting_id.as_deref(), Some("no-such-meeting"));
        assert_eq!(st.started_at, None);
    }
}

#[cfg(test)]
mod reminder_script_tests {
    use super::{build_reminder_script, escape_applescript, parse_iso_ymd};

    #[test]
    fn parses_strict_iso_only() {
        assert_eq!(parse_iso_ymd("2026-07-01"), Some((2026, 7, 1)));
        assert_eq!(parse_iso_ymd(" 2026-12-31 "), Some((2026, 12, 31)));
        assert_eq!(parse_iso_ymd("2026-13-01"), None); // month out of range
        assert_eq!(parse_iso_ymd("2026-07-32"), None); // day out of range
        assert_eq!(parse_iso_ymd("2026/07/01"), None); // wrong separators
        assert_eq!(parse_iso_ymd("26-07-01"), None); // not 4-digit year
        assert_eq!(parse_iso_ymd(""), None);
    }

    #[test]
    fn due_date_sets_the_date_properties() {
        let s = build_reminder_script("Ship the deck", Some("2026-07-01"));
        // The date is actually attached now (the bug was: only `name` was set).
        assert!(s.contains("set year of theDate to 2026"));
        assert!(s.contains("set month of theDate to 7"));
        assert!(s.contains("set day of theDate to 1"));
        assert!(s.contains("remind me date:theDate"));
        assert!(s.contains("due date:theDate"));
        assert!(s.contains("name:\"Ship the deck\""));
        // `day` is reset to 1 BEFORE year/month so a month change can't overflow the day.
        let reset = s.find("set day of theDate to 1").unwrap();
        let yr = s.find("set year of theDate").unwrap();
        assert!(
            reset < yr,
            "day must be reset to 1 before changing year/month"
        );
    }

    #[test]
    fn no_due_date_is_name_only() {
        let s = build_reminder_script("Call Bob", None);
        assert!(s.contains("name:\"Call Bob\""));
        assert!(!s.contains("due date"));
        assert!(!s.contains("theDate"));
    }

    #[test]
    fn invalid_due_date_falls_back_to_name_only() {
        let s = build_reminder_script("Task", Some("not-a-date"));
        assert!(
            !s.contains("due date"),
            "an unparseable date must not produce date props"
        );
        assert!(s.contains("name:\"Task\""));
    }

    #[test]
    fn item_text_cannot_break_out_of_the_applescript_literal() {
        // A name carrying a quote + a forged statement must stay INSIDE the string literal: the
        // `"` is escaped to `\"`, so `end tell` / the injected `make` never become real statements.
        let evil =
            "pwn\", remind me date:theDate}\nend tell\ntell application \"Finder\" to delete";
        let esc = escape_applescript(evil);
        assert!(
            !esc.contains('\n'),
            "raw newlines flattened (literals can't span lines)"
        );
        // Every `"` in the payload is preceded by a backslash — no bare quote survives to close
        // the literal early. (Checked by scanning: each `"` byte has a `\` immediately before it.)
        let bytes = esc.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'"' {
                assert!(
                    i > 0 && bytes[i - 1] == b'\\',
                    "unescaped quote survived at {i}"
                );
            }
        }
        let s = build_reminder_script(evil, Some("2026-07-01"));
        // The ONE real `tell` statement (unescaped quotes around Reminders) is intact...
        assert!(
            s.contains("tell application \"Reminders\""),
            "the real Reminders statement must survive"
        );
        // ...and the injected Finder `tell` never becomes real code: its quotes are escaped, so it
        // stays as inert data inside the name literal (no `tell application "Finder"` with REAL quotes).
        assert!(
            !s.contains("tell application \"Finder\""),
            "injected statement must remain escaped data, not executable code"
        );
        // The whole program is a single line (newlines in the payload were flattened), so a forged
        // `end tell` can never start its own statement line.
        assert!(
            !s.lines().any(|l| l.trim() == "end tell"),
            "no standalone injected `end tell` statement line"
        );
        // Every embedded double-quote from the payload is backslash-escaped in the program.
        assert!(s.contains("\\\""), "payload quotes are escaped");
    }
}

// ─── Task 1.4 — gateway key command argument validation ────────────────────────────────────────
#[cfg(test)]
mod gateway_key_tests {
    use super::*;

    /// `set_gateway_key("")` must return `InvalidArg`, not silently succeed.
    #[test]
    fn set_gateway_key_empty_is_invalid_arg() {
        let err = set_gateway_key(String::new()).unwrap_err();
        assert!(
            matches!(err, AppError::InvalidArg(_)),
            "empty gateway key must be InvalidArg, got: {err:?}"
        );
    }

    /// `set_gateway_key("   ")` (whitespace-only) is also invalid.
    #[test]
    fn set_gateway_key_whitespace_is_invalid_arg() {
        let err = set_gateway_key("   ".to_string()).unwrap_err();
        assert!(
            matches!(err, AppError::InvalidArg(_)),
            "whitespace-only gateway key must be InvalidArg"
        );
    }

    // ── FIX 6: gateway URL validated at save time ──────────────────────────────────────────────

    /// `save_config` rejects a gateway URL that embeds credentials — the validation used by the
    /// save path (`validate_gateway_url`) refuses `https://key:@host/v1` before it reaches the DB.
    /// Empty URL (no gateway configured) and a valid https URL are both accepted.
    #[test]
    fn save_config_gateway_url_with_credentials_is_rejected() {
        // Credential-bearing URL → InvalidArg (never stored).
        let err =
            crate::summarize::gateway::validate_gateway_url("https://key:@host/v1").unwrap_err();
        assert!(
            matches!(err, AppError::InvalidArg(_)),
            "URL with credentials must be InvalidArg, got: {err:?}"
        );
    }

    #[test]
    fn save_config_valid_gateway_url_is_accepted() {
        // Valid https URL → Ok.
        assert!(
            crate::summarize::gateway::validate_gateway_url("https://gw.example.com/v1").is_ok(),
            "valid https gateway URL must be accepted"
        );
        // Localhost http → Ok.
        assert!(
            crate::summarize::gateway::validate_gateway_url("http://127.0.0.1:4000/v1").is_ok(),
            "loopback http gateway URL must be accepted"
        );
    }
}

#[cfg(test)]
mod storage_cmd_tests {
    use super::*;

    #[test]
    fn free_up_space_is_noop_without_a_cap() {
        let p = crate::storage::db::unique_temp_path("murmur-cmd-storage", "sqlite");
        let _ = std::fs::remove_file(&p);
        let state = AppState::init_at(
            &p,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        // No limit set (default None) → free_up_space must be an inert zero summary.
        let s = crate::storage::usage::prune_to_limit(
            &state.db,
            &crate::pipeline::audio_dir().unwrap(),
            u64::MAX,
            None,
        )
        .unwrap();
        assert_eq!(s.freed_bytes, 0);
        let _ = std::fs::remove_file(&p);
    }
}
