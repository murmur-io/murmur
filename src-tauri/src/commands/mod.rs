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

// DOCUMENT-INGEST command surface (upload/import/list/read/delete of brain `documents` — a GATED
// domain: every write WRITE-GATES the folder + re-checks the gate before the plaintext INSERT, every
// read masks a sealed-not-unlocked folder to empty/"", `delete_document` revokes org shares
// before dropping the row; the gate/mask LOGIC is byte-identical, only relocated). Bound as
// `documents_commands` (via `#[path]`) to keep it clearly distinct and avoid any future name shadow.
// The glob re-export makes every moved command resolve UNCHANGED at `crate::commands::…` for
// `generate_handler!` in `lib.rs` and every caller (incl. the STAYING test modules).
#[path = "documents.rs"]
mod documents_commands;
pub use documents_commands::*;

// FOLDERS command surface (create/list/rename/delete of meeting/note folders — a GATED domain:
// `list_folders` folds the session unlock set into per-folder note counts, `delete_folder` refuses a
// sealed-not-unlocked folder and PERMANENTLY unseals a session-unlocked one via `remove_lock_inner`
// before demoting its notes to root, never orphaning encrypted content; the gate/seal LOGIC is
// byte-identical, only relocated). The MEETING move-into-folder command (`move_note`) + its auto-file
// / salvage seal helper web (`move_into_locked_folder`/`seal_moved_note`/`classify_auto_file_target`/
// `seal_auto_filed_note`/`finalize_salvage_lock_state`/`AutoFileTarget`/`ensure_meeting_folder_target`)
// deliberately STAY in `commands/mod.rs` — they are pipeline-called seal machinery, not folder CRUD.
// Bound as `folders_commands` (via `#[path]`) to avoid colliding with the crate-level
// `crate::storage::folders_store` and any future `folders` name (E0255). The glob re-export makes every
// moved command resolve UNCHANGED at `crate::commands::…` for `generate_handler!` and every caller
// (incl. the sibling `commands/notes.rs` → `rename_folder_inner`/`delete_folder_inner`).
#[path = "folders.rs"]
mod folders_commands;
pub use folders_commands::*;

// LINKS command surface (the note↔meeting↔document link engine surface — a GATED domain: `list_links`
// gates BOTH endpoints, `accept_link`/`dismiss_link` gate + refuse behind a lock, `link_items`/
// `unlink_items` gate both endpoints before any write, `list_link_candidates`/`resolve_wikilink` route
// through the `*_visible` readers; the gate LOGIC is byte-identical, only relocated). `accept_link_inner`
// keeps its EXACT scoped-lifecycle-guard body (the guard is scoped in a `{ }` block RELEASED before the
// materialize loop so the composed `update_note_inner` can re-take the non-reentrant `Mutex<()>` without
// self-DEADLOCK) — moved verbatim. The write-time index hooks (`index_wikilinks_best_effort`/
// `auto_link_semantic_best_effort`), the unlock re-derive (`rederive_links_for_folder`), the entity
// persist (`build_and_persist_entities`), and the `link_related_notes_inner` core (pipeline-called) STAY
// in `commands/mod.rs` — the moved commands reach them through `use super::*` (a `commands` submodule sees
// its parent's private items). Bound as `links_commands` (via `#[path]`) to avoid colliding with the
// crate-level `crate::links` / `crate::storage::links` (E0255). The glob re-export makes every moved
// command resolve UNCHANGED at `crate::commands::…` for `generate_handler!` and every caller.
#[path = "links.rs"]
mod links_commands;
pub use links_commands::*;

// CALENDAR connector command surface (local macOS Calendar — NOT sealed content) — extracted
// verbatim (God-file split, PURE MOVE — every body byte-identical, only relocated). These reads hit
// the user's LOCAL calendar (osascript / the `meetnotes-calendar` EventKit sidecar), never sealed
// meeting/note content, so there is NO content gate here (nothing to gate) and NO network egress
// (any downstream cloud use of a `CalendarContext` still rides the `make_provider` firewall +
// consent). Bound as `calendar_commands` (via `#[path]`) to avoid colliding with the crate-level
// `crate::calendar` module these commands call (E0255). The glob re-export makes every moved command
// resolve UNCHANGED at `crate::commands::…` for `generate_handler!` in `lib.rs` and every caller.
#[path = "calendar.rs"]
mod calendar_commands;
pub use calendar_commands::*;

// ML-MODEL + AI-GATEWAY management command surface (NOT content-gated) — extracted verbatim
// (God-file split, PURE MOVE — every body byte-identical, only relocated). Two clusters, both
// OUTSIDE the lock model: the AI-gateway/provider catalog + health probes (INBOUND-ONLY — at most an
// `Authorization: Bearer` header leaves, never meeting content) and the model DOWNLOADS + vector
// REINDEX trigger. `reindex_embeddings`'s visibility-gated corpus runs entirely through the SHARED
// `reindex_embeddings_inner` (+ `backfill_document_chunks` / `index_document_row_kind_routed` and the
// `ReindexResult` DTO), which all STAY in `commands/mod.rs` (also called by the startup repair tick +
// the lifecycle tests); the moved command reaches them through `use super::*`. Bound as
// `models_commands` (via `#[path]`) to avoid any name shadow with the crate-level model modules
// (`crate::transcribe::model` / `crate::reason` / `crate::embed`). The glob re-export makes every
// moved command resolve UNCHANGED at `crate::commands::…` for `generate_handler!` and every caller.
#[path = "models.rs"]
mod models_commands;
pub use models_commands::*;

// WEEKLY DIGEST + scheduled-BRIEFS command surface (a GATED domain where it reads content) —
// extracted verbatim (God-file split, PURE MOVE — every body byte-identical, only relocated).
// `generate_digest` builds its cloud corpus from VISIBLE meetings + VISIBLE notes only
// (`list_meetings_visible` + `get_note_if_visible` — the same `visibility_clause` predicate MCP
// uses), so a sealed-not-session-unlocked meeting's title/markdown never egress. `list_brief_runs`
// returns pending proposals whose `note_md` was synthesized from VISIBLE-ONLY content AND is purged
// on seal (`Db::purge_pending_brief_runs_tx`, inside the seal tx) — the documented no-per-meeting-gate
// posture; the gate LOGIC is byte-identical, only relocated. `accept_brief` exports via
// `crate::export::write_note` under the SHARED `vault_path` (which STAYS in `commands/mod.rs`,
// reached via `super::`). Bound as `brief_commands` (via `#[path]`) to avoid any name shadow with the
// crate-level `crate::brief_runner`. The glob re-export makes every moved command resolve UNCHANGED at
// `crate::commands::…` for `generate_handler!` and every caller.
#[path = "brief.rs"]
mod brief_commands;
pub use brief_commands::*;

// NOTE-ENRICHMENT / VERIFY / SELECTION-ASSISTANT / SUPERSESSION / MEMORY-IMPORT command surface (a
// GATED domain — every reader of meeting/note content keeps its gate, every note WRITE keeps its
// write-gate + seal-on-write) — extracted verbatim (God-file split, PURE MOVE — every gate/seal/guard
// body byte-identical, only relocated + visibility WIDENED to `pub(crate)` where a RETAINED lifecycle
// test reaches a note-assist helper via `super::…`, the ONLY change to any moved item). The moved
// readers keep `meeting_is_unlocked`/`folder_is_unlocked` (→ `AppError::Locked`) verbatim; the moved
// note WRITES keep their seal-on-write seam (`upsert_note_reseal_if_locked` — a session-unlocked
// LOCKED folder re-seals the fresh markdown; a SEALED note is refused). The supersession commands
// RE-GATE each row at apply time (the prune↔seal TOCTOU discipline) under the shared lifecycle guard.
// The SHARED note-write/seal/gate web (`upsert_note_reseal_if_locked`/`overwrite_exported_note_guarded`/
// `meeting_is_unlocked`/`folder_is_unlocked`/`lifecycle_guard`/`set_timeline_data_reseal_if_locked`/
// `index_wikilinks_best_effort`/`auto_link_semantic_best_effort`), the SUPERSESSION gate helpers
// (`source_is_stampable`/`folder_locked_on_disk`/`note_file_for` — also called by the sibling
// `commands/analytics.rs`), and the memory helpers (`import_extraction_reasoner`/`user_memory_enabled`,
// owned by the siblings `ask.rs`/`facts.rs`) all STAY in `commands/mod.rs`; the moved commands reach
// them through `use super::*`. Bound as `enrich_commands` (via `#[path]`) to avoid colliding with the
// crate-level `crate::enrich` module these commands call (E0255). The glob re-export makes every moved
// command resolve UNCHANGED at `crate::commands::…` for `generate_handler!` and every caller (incl.
// the STAYING test modules).
#[path = "enrich.rs"]
mod enrich_commands;
pub use enrich_commands::*;

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

/// Stage 2 / Lane A — the cross-meeting LINKING pass over a FINISHED note. NOW A NO-OP.
///
/// This pass used to APPEND a machine-managed `> [!related]- Related notes` (`murmur:links`) block
/// into the note body, mirroring cross-meeting `[[Title]]` links so they surfaced inside Obsidian.
/// That block was RETIRED: it went stale (its wikilinks are excluded from edge-indexing, so it
/// diverged from the live RELATED panel), rendered as raw junk in the plain-text editor, and created
/// a latent clobber risk (an open editor holding a stale body could overwrite it). The RELATED panel
/// reads the live `links` table (driven by the deterministic `index_wikilinks_best_effort` +
/// `auto_link_semantic_best_effort` hooks on every note save) and is unaffected — it is the real,
/// always-fresh surface. So this pass writes NOTHING now.
///
/// The command (`link_related_notes`) + the deferred pipeline call site are kept (a stable no-op) so
/// no caller/registration churns; any pre-existing `murmur:links` block in an existing note is
/// stripped on the next `get_note` read (natural save-time cleanup — see
/// `commands/notes.rs::get_note_inner`). NO note body is read or written here, so there is no seal /
/// TOCTOU / lifecycle-guard surface left to reason about.
pub(crate) fn link_related_notes_inner(_state: &AppState, _meeting_id: &str) -> Result<(), AppError> {
    Ok(())
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

/// Pack meeting-chat's additional explicit sources under one fair, provider-specific budget.
/// The anchor meeting is already represented by the primary transcript, so remove it before
/// packing; otherwise its large generated note can consume the source budget before a note the
/// user added. The remaining deduped set enters the visibility-gated budgeted builder ONCE, keeping
/// one global neighbour dedupe and one global link-expansion cap across the whole scope.
pub(crate) fn pack_chat_pinned_sources(
    db: &crate::storage::Db,
    meeting_id: &str,
    explicit_sources: &[crate::storage::models::SourceRef],
    provider_id: &str,
    unlocked: &std::collections::HashSet<String>,
) -> Result<String, AppError> {
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut filtered = Vec::new();
    for source in explicit_sources {
        if source.kind == crate::links::LinkKind::Meeting && source.id == meeting_id {
            continue;
        }
        let key = (source.kind.as_str().to_string(), source.id.clone());
        if !seen.insert(key) {
            continue;
        }
        filtered.push(source.clone());
    }
    let budget = crate::summarize::vault_context::budget_for(provider_id)
        .min(crate::summarize::chat::MAX_PINNED_SOURCE_CHARS);
    let (corpus, _) =
        crate::summarize::vault_context::build_vault_context_pinned_visible_with_budget(
            db, &filtered, budget, unlocked,
        )?;
    Ok(corpus)
}

/// Grounded Q&A over a meeting's transcript ("chat with the meeting"). The configured
/// provider answers strictly from the transcript, explicitly pinned sources, and running history.
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
    let mut pinned_sources = String::new();
    if let Some(sources) = explicit_sources.filter(|s| !s.is_empty()) {
        let ask_conn =
            crate::summarize::roles::provider_target(crate::summarize::roles::Role::Ask, &config)
                .connection;
        pinned_sources =
            pack_chat_pinned_sources(&state.db, &meeting_id, &sources, &ask_conn, &unlocked)?;
    }
    // ASK role: meeting chat is a Q&A surface. With role keys absent this resolves to the same
    // default provider as before (the legacy chat path always ignored `brain_backend`).
    let provider = crate::summarize::provider_for(
        crate::summarize::roles::Role::Ask,
        &config,
        &state.heavy_inference,
    )?;
    let memory_brief = gated_memory_brief_for_injection(state.inner(), &unlocked, &question);
    let (system, user) = crate::summarize::chat::build_with_sources(
        &transcript,
        &pinned_sources,
        &history,
        &question,
        &memory_brief,
    );
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
//
// The supersession COMMANDS (`preview_supersessions`/`apply_supersessions`/`undo_supersessions`) +
// their DTOs (`SupersessionDto`/`ApplyResult`) + the enrich-only helpers (`pristine_note_bytes`/
// `superseding_link_stem`) were extracted verbatim to `commands/enrich.rs` (God-file split, PURE
// MOVE — every gate/re-gate/undo body byte-identical, only relocated). The three gate helpers below
// (`source_is_stampable`/`folder_locked_on_disk`/`note_file_for`) STAY here: the sibling
// `commands/analytics.rs` (the Re-Truth audit-finding path) also reaches them via `super::…`, and
// the moved commands reach them the same way.

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
    // The org-item viewer pins the read-only SHARED note being viewed (an org item is not a valid
    // local `SourceRef`, so it can't ride `explicit_sources`). Present ⇒ the deterministic floor
    // path is used and the item is packed FIRST, gated by `get_org_item` (tombstoned/disabled-org
    // ⇒ nothing). FE camelCase `pinnedOrgItemId`. `None`/empty ⇒ byte-identical to before.
    pinned_org_item_id: Option<String>,
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
    let pinned_sources = explicit_sources.filter(|s| !s.is_empty());
    let pinned_org = pinned_org_item_id.filter(|s| !s.trim().is_empty());
    if pinned_sources.is_some() || pinned_org.is_some() {
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
            pinned_sources,
            pinned_org,
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
        None, // …and no pinned org item — this is the vault-wide fallthrough.
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
    pinned_org_item_id: Option<String>,
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
            pinned_org_item_id.as_deref(),
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
