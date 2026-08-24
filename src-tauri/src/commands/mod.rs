use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use zeroize::{Zeroize, Zeroizing};

use crate::audio::source::{
    discard_untracked_empty_mic, ActiveRecording, CaptureSpool, RawF32LeSink, RECORDING_LEASE_MS,
};
use crate::audio::Recorder;
use crate::error::AppError;
use crate::events::{StatusPayload, EVENT_STATUS};
use crate::settings::{AppConfig, BrainBackend};
use crate::state::AppState;
use crate::storage::models::{
    ActionItem, Analytics, AskVaultResult, BrainOverview, CalendarContext, CalendarEvent,
    CalendarEventFull, ChatTurn, Commitment, DigestResult, DocumentInfo, EntityDetail,
    EntityDossierResult, Folder, FolderNode, FullGraphData, FullGraphOpts, GraphData, Meeting,
    MeetingActionSummary, MeetingStatus, MeetingTimeline, NoteAssistRequest, NoteAssistResult,
    NoteCitation, NoteDoc, NoteFolder, NoteRecord, NoteSummary, OrganizeMove, OrganizePlan,
    PeopleList, PinResult, PropertyKind, PropertySchemaField, SearchHit, TopicThread, TypedNoteRow,
};
use crate::storage::Db;
use crate::transcribe::types::Segment;
use crate::{pipeline, secrets};
use tauri::Emitter;

/// A successful visibility reduction must synchronously revoke every Ask renderer cache. If the
/// content-free event bus itself fails, destroy the renderer and terminate instead of leaving
/// plaintext messages/source labels available from a stale slideout cache.
pub(crate) fn emit_ask_history_invalidated_fail_closed(app: &AppHandle) {
    if crate::events::emit_ask_history_invalidated(app) {
        return;
    }
    if let Some(window) = app.get_webview_window("main") {
        if let Err(error) = window.hide() {
            tracing::error!(
                target: "ask_history",
                error = %error,
                "failed to hide Murmur after Ask history invalidation failure"
            );
        }
        if let Err(error) = window.destroy() {
            tracing::error!(
                target: "ask_history",
                error = %error,
                "failed to destroy renderer after Ask history invalidation failure"
            );
        }
    }
    app.exit(1);
}

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

// Dashboards — user-composed boards of tiles over EXISTING sources. Every tile payload resolves
// through gated readers at read time; composite Ask uses the shared authorized provider/egress
// seam and revalidates its backend-owned witness after every await.
mod dashboards;
pub use dashboards::*;

// Org-owned Tasks — one encrypted stable document per task, projected from the shared org feed.
// Device-private note/meeting/dashboard refs never enter the Task envelope.
mod tasks;
pub use tasks::*;

// Model / capability / performance probes + NER download — no name collision with a crate module.
mod model_perf;
pub use model_perf::*;

// LIVE-CAPTION model readiness: the ONE resolution shared by `start_recording` (which needs the
// model PATH) and `get_config` (which needs the STATE the recorder renders), plus the live-safe
// companion-download decision `download_model` consults. Holds no `#[tauri::command]`, so it is NOT
// glob-re-exported — callers reach it as `live_captions::…` / `super::live_captions::…`.
mod live_captions;

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

#[path = "ask_history.rs"]
mod ask_history_commands;
pub use ask_history_commands::*;

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

// Canonical note-image attachments: gated binary CRUD + validation and the shared bundle seam.
#[path = "attachments.rs"]
mod attachment_commands;
pub use attachment_commands::*;

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

// WORKSPACE HIERARCHY command surface (Projects › Folders › items — the container forest the new
// sidebar renders, plus its paged item reader). READ-ONLY: no seal, no key, no write. Bound as
// `workspace_commands` (via `#[path]`) to mirror the sibling modules and avoid colliding with
// `crate::storage::workspace_store` or any future `workspace` name (E0255).
#[path = "workspace.rs"]
mod workspace_commands;
pub use workspace_commands::*;

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

/// Final IPC content gate after a long pipeline await. A result may own a plaintext markdown copy
/// even though screen-share/manual relock has already blanked the canonical rows and revoked every
/// reader. Recheck under the short lock lifecycle guard immediately before building the DTO; never
/// let the cached copy cross Tauri IPC after revocation.
pub(crate) fn ensure_post_await_result_visible(
    state: &AppState,
    meeting_id: &str,
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    if !meeting_is_unlocked(state, meeting_id)? {
        return Err(AppError::Locked(
            "this meeting was relocked before its result could be shown — unlock it to view the note"
                .into(),
        ));
    }
    Ok(())
}

/// Optimistic authorization token for work that derives a result from one or more visible content
/// rows and then releases the lifecycle mutex across a long `.await` / blocking inference call.
/// Every lock, relock and remove-lock transition bumps this epoch at entry. A mismatch therefore
/// means the caller's cached plaintext was observed under an obsolete authorization generation and
/// must neither be returned nor persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContentVisibilitySnapshot {
    seal_epoch: u64,
}

/// Capture the current authorization epoch while the caller already owns `lifecycle_guard`.
/// Keeping this twin explicit prevents nested locking of the non-reentrant lifecycle mutex.
pub(crate) fn capture_content_visibility_snapshot_under_lifecycle(
    state: &AppState,
) -> ContentVisibilitySnapshot {
    ContentVisibilitySnapshot {
        seal_epoch: state.seal_epoch.load(std::sync::atomic::Ordering::SeqCst),
    }
}

/// Capture the current content-authorization generation under the same lifecycle mutex as every
/// lock transition. Call this BEFORE assembling a visible multi-source corpus.
pub(crate) fn capture_content_visibility_snapshot(state: &AppState) -> ContentVisibilitySnapshot {
    let _lifecycle = lifecycle_guard(state);
    capture_content_visibility_snapshot_under_lifecycle(state)
}

pub(crate) fn require_current_content_visibility_snapshot_under_lifecycle(
    state: &AppState,
    snapshot: ContentVisibilitySnapshot,
) -> Result<(), AppError> {
    if state.seal_epoch.load(std::sync::atomic::Ordering::SeqCst) != snapshot.seal_epoch {
        return Err(AppError::Locked(
            "content was locked while this operation was running — unlock it and retry".into(),
        ));
    }
    Ok(())
}

/// Revalidate a multi-source result immediately before it crosses IPC. For a derived write, acquire
/// `lifecycle_guard` and use the `_under_lifecycle` twin so the check and write are one interval.
pub(crate) fn require_current_content_visibility_snapshot(
    state: &AppState,
    snapshot: ContentVisibilitySnapshot,
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    require_current_content_visibility_snapshot_under_lifecycle(state, snapshot)
}

/// A single-meeting snapshot additionally binds the folder association. Ordinary moves between
/// open folders do not bump the global epoch, while moving into a different protection domain must
/// still invalidate a cached transcript/note/timeline result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MeetingContentSnapshot {
    folder_id: Option<String>,
    visibility: ContentVisibilitySnapshot,
    /// Content-free witness for ACTIVE, currently-visible Related endpoints. Conversion sends
    /// their plaintext as secondary context, so removal/deactivation/deletion during an await must
    /// invalidate admission even when no folder seal epoch changed.
    active_related: Vec<(String, String)>,
}

fn active_related_witness(
    state: &AppState,
    meeting_id: &str,
) -> Result<Vec<(String, String)>, AppError> {
    let unlocked = unlocked_snapshot(state)?;
    let mut related = active_conversion_related_edges(state, meeting_id, &unlocked)?
        .into_iter()
        .map(|edge| (edge.other_kind, edge.other_id))
        .collect::<Vec<_>>();
    related.sort();
    related.dedup();
    Ok(related)
}

/// Resolve exactly the active, visible Related inputs that conversion sends to the provider.
/// The canonical companion note is this operation's structural OUTPUT, not user-selected secondary
/// context. Exclude it by `documents.meeting_id` rather than by edge type: link collapse can expose
/// the same endpoint through its generated `wikilink` representative instead of `companion`.
/// Shared Brain Org relations stay private graph metadata and never become conversion input.
fn active_conversion_related_edges(
    state: &AppState,
    meeting_id: &str,
    unlocked: &std::collections::HashSet<String>,
) -> Result<Vec<crate::storage::models::LinkEdge>, AppError> {
    let companion_note_id = state.db.companion_note_for_meeting(meeting_id)?;
    Ok(state
        .db
        .links_for_visible(crate::links::LinkKind::Meeting, meeting_id, unlocked)?
        .into_iter()
        .filter(|edge| {
            edge.status == "active"
                && edge.other_kind != crate::links::LinkKind::Org.as_str()
                && !(edge.other_kind == "note"
                    && companion_note_id.as_deref() == Some(edge.other_id.as_str()))
        })
        .collect())
}

pub(crate) fn capture_meeting_content_snapshot(
    state: &AppState,
    meeting_id: &str,
) -> Result<MeetingContentSnapshot, AppError> {
    let _lifecycle = lifecycle_guard(state);
    if !meeting_is_unlocked(state, meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it and retry".into(),
        ));
    }
    Ok(MeetingContentSnapshot {
        folder_id: state.db.folder_for_meeting(meeting_id)?,
        visibility: ContentVisibilitySnapshot {
            seal_epoch: state.seal_epoch.load(std::sync::atomic::Ordering::SeqCst),
        },
        active_related: active_related_witness(state, meeting_id)?,
    })
}

pub(crate) fn require_current_meeting_content_snapshot_under_lifecycle(
    state: &AppState,
    meeting_id: &str,
    snapshot: &MeetingContentSnapshot,
) -> Result<(), AppError> {
    require_current_content_visibility_snapshot_under_lifecycle(state, snapshot.visibility)?;
    if !meeting_is_unlocked(state, meeting_id)?
        || state.db.folder_for_meeting(meeting_id)? != snapshot.folder_id
        || active_related_witness(state, meeting_id)? != snapshot.active_related
    {
        return Err(AppError::Locked(
            "this meeting moved, was locked, or its Related context changed while the operation was running — retry with the current context".into(),
        ));
    }
    Ok(())
}

pub(crate) fn require_current_meeting_content_snapshot(
    state: &AppState,
    meeting_id: &str,
    snapshot: &MeetingContentSnapshot,
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    require_current_meeting_content_snapshot_under_lifecycle(state, meeting_id, snapshot)
}

/// Run a bounded meeting-content read only after revalidating its snapshot while holding the seal
/// lifecycle guard. A relock that wins before this interval prevents `read` from running; a relock
/// that starts afterwards waits until every plaintext source has been copied into the request.
/// Callers MUST return from this helper before awaiting provider/network work.
fn read_current_meeting_content_under_snapshot<T>(
    state: &AppState,
    meeting_id: &str,
    snapshot: &MeetingContentSnapshot,
    read: impl FnOnce() -> Result<T, AppError>,
) -> Result<T, AppError> {
    let _lifecycle = lifecycle_guard(state);
    require_current_meeting_content_snapshot_under_lifecycle(state, meeting_id, snapshot)?;
    read()
}

pub(crate) fn meeting_dispatch_admission(
    app: &AppHandle,
    meeting_id: String,
    snapshot: MeetingContentSnapshot,
) -> crate::state::ContentDispatchAdmission {
    crate::state::ContentDispatchAdmission::new(app, move |state| {
        require_current_meeting_content_snapshot_under_lifecycle(state, &meeting_id, &snapshot)
    })
}

/// Authored-note counterpart of [`MeetingContentSnapshot`]. The content-free gate anchor is read
/// before any title/body/export path and is rebound after every long await.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DocumentContentSnapshot {
    folder_id: String,
    visibility: ContentVisibilitySnapshot,
}

pub(crate) fn capture_document_content_snapshot(
    state: &AppState,
    note_id: &str,
) -> Result<DocumentContentSnapshot, AppError> {
    let _lifecycle = lifecycle_guard(state);
    let Some((folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(note_id)? else {
        return Err(AppError::InvalidArg(format!("no note {note_id}")));
    };
    if !folder_is_unlocked(state, &folder_id)? {
        return Err(AppError::Locked(
            "this note's folder is locked — unlock it and retry".into(),
        ));
    }
    Ok(DocumentContentSnapshot {
        folder_id,
        visibility: ContentVisibilitySnapshot {
            seal_epoch: state.seal_epoch.load(std::sync::atomic::Ordering::SeqCst),
        },
    })
}

pub(crate) fn require_current_document_content_snapshot(
    state: &AppState,
    note_id: &str,
    snapshot: &DocumentContentSnapshot,
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    require_current_document_content_snapshot_under_lifecycle(state, note_id, snapshot)
}

pub(crate) fn require_current_document_content_snapshot_under_lifecycle(
    state: &AppState,
    note_id: &str,
    snapshot: &DocumentContentSnapshot,
) -> Result<(), AppError> {
    require_current_content_visibility_snapshot_under_lifecycle(state, snapshot.visibility)?;
    let Some((folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(note_id)? else {
        return Err(AppError::InvalidArg(format!("no note {note_id}")));
    };
    if folder_id != snapshot.folder_id || !folder_is_unlocked(state, &folder_id)? {
        return Err(AppError::Locked(
            "this note moved or was locked while the operation was running — unlock it and retry"
                .into(),
        ));
    }
    Ok(())
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
    /// Omission means PRESERVE the stored value so an older/partial client cannot reset an
    /// explicit opt-out. `get_config` always returns `Some`, while either explicit boolean remains
    /// a real user choice.
    #[serde(default)]
    pub diarize_others: Option<bool>,
    /// Voice biometrics are an independent opt-in. Omission must preserve an existing enrollment
    /// choice rather than silently revoking it during an older/partial settings save.
    #[serde(default)]
    pub voiceprint_enabled: Option<bool>,
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
    /// Brain-sidecar IDLE-KILL window (s) — after this idle the host kills the `murmur-brain`
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
    /// Display name for the `me` capture lane in generated notes. Empty = unset.
    #[serde(default)]
    pub user_display_name: String,
    pub note_language: String,
    /// On-device post-generation support marker. Settable from Settings. The thresholds are not
    /// calibrated, so the marker is a review cue rather than proof. Omission means PRESERVE the
    /// stored value; an explicit `false` remains a durable opt-out.
    #[serde(default)]
    pub ground_summary: Option<bool>,
    /// Workspace glossary update. Omission means PRESERVE the stored value so an older or partial
    /// client cannot erase it; `Some("")` is an explicit clear.
    #[serde(default)]
    pub glossary: Option<String>,
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
    /// brain2 connectors — the NOTION master toggle. Settable from the DTO (the Settings UI owns the
    /// toggle). Even ON, the connector is exposed only once `notion_consented` is granted AND an
    /// integration token is configured.
    ///
    /// OMISSION-SAFE (`Option`, unlike the older `jira_enabled`/`slack_enabled` plain bools): an
    /// ABSENT key means "don't touch" and `dto_to_config` PRESERVES the stored value, while an
    /// explicit `false` still disables. Needed because a caller that predates this field (the
    /// onboarding wizard round-trips the whole DTO) would otherwise silently CLEAR a toggle the user
    /// had enabled. This is strictly at least as fail-closed as the plain-bool shape: preserving can
    /// never ENABLE a connector the user did not enable.
    #[serde(default)]
    pub notion_enabled: Option<bool>,
    /// brain2 connectors — one-time NOTION egress consent. PRESERVE-ONLY on this DTO, exactly like
    /// `slack_consented`: `get_config` carries the current value OUT (so the FE can show consent
    /// status), but `dto_to_config` IGNORES the incoming value and PRESERVES the stored one. The ONLY
    /// mutator is the dedicated `consent_to_notion` command. `#[serde(default)]` = false (fail-closed).
    #[serde(default)]
    pub notion_consented: bool,
    /// brain2 connectors — the CLICKUP master toggle. Settable from the DTO, OMISSION-SAFE exactly
    /// like [`AppConfigDto::notion_enabled`] (absent ⇒ preserve; explicit `false` ⇒ disable). Even
    /// ON, the connector is exposed only once `clickup_consented` is granted AND a workspace (team)
    /// id + API token are configured.
    #[serde(default)]
    pub clickup_enabled: Option<bool>,
    /// brain2 connectors — one-time CLICKUP egress consent. PRESERVE-ONLY on this DTO, exactly like
    /// `notion_consented`; the ONLY mutator is the dedicated `consent_to_clickup` command.
    /// `#[serde(default)]` = false (fail-closed).
    #[serde(default)]
    pub clickup_consented: bool,
    /// The ClickUp workspace ("team") id the task search reads (non-secret). Settable from the DTO,
    /// OMISSION-SAFE (absent ⇒ preserve the stored id; an explicit `""` ⇒ clear it), so a caller that
    /// predates this field cannot wipe a configured workspace.
    #[serde(default)]
    pub clickup_team_id: Option<String>,
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
    /// DISPLAY-ONLY out — LIVE-CAPTION readiness for THIS machine, so the recorder can tell the user
    /// whether live captions are on instead of the truth living in a backend `warn!`:
    /// `"ready"` | `"modelMissing"` (nothing live-safe downloaded — the live-safe companion fetch
    /// never landed; re-running the model download fixes it) | `"pinnedHeavy"` (the live pin names a
    /// medium/large-class size that isn't downloaded — a configuration choice, since a heavy model is
    /// never run on the 3 s tick) | `"noModel"` (no whisper model at all — the model-download banner
    /// owns that state). It is a DEVICE/DISK fact, not a persisted setting, so it is computed in
    /// `get_config` (NOT in the pure `config_to_dto`, which leaves it `""` = not probed) and
    /// `dto_to_config` ignores it entirely — a settings save can neither set nor clear it.
    /// `#[serde(default)]` keeps an older FE payload that omits the key deserializing cleanly.
    #[serde(default)]
    pub live_captions: String,
    /// DISPLAY-ONLY out — would a `download_model` run right now ALSO fetch the live-safe caption
    /// companion (`live_captions::companion_size_for`)? The onboarding wizard discloses the extra
    /// transfer from THIS flag rather than re-deriving the rule in TypeScript, so the wizard's
    /// promise and the download's behavior cannot drift. Same DISPLAY-ONLY discipline as
    /// [`Self::live_captions`]: filled in by `get_config`, `false` in the pure `config_to_dto`, and
    /// ignored by `dto_to_config`.
    #[serde(default)]
    pub live_companion_pending: bool,
    /// P1 (C9) — WHO chose `model_size`: `Some("auto")` (the user took Murmur's recommendation) or
    /// `Some("user")` (a deliberate pick). ABSENT (`None`) means PRESERVE — the stored value is left
    /// exactly as it is, which is why a plain settings save that knows nothing about this field can
    /// never clobber it. Deliberately an `Option`, NOT a plain string: the field must be REVERSIBLE
    /// (auto → user → auto), not write-once. It lives in its own settings row rather than on
    /// `AppConfig` (see `settings::config::model_size_source`), so `dto_to_config`'s exhaustive
    /// struct literal is untouched; `save_config_inner` applies it. `config_to_dto` leaves it `None`
    /// so a read-modify-write round-trip is a preserve, and the READ surface is
    /// `whisper_recommendation().modelSizeSource`.
    #[serde(default)]
    pub model_size_source: Option<String>,
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

struct RecordingStartGuard<'a> {
    db: &'a crate::storage::Db,
    meeting_id: String,
    armed: bool,
}

struct RecordingStartingFlag<'a> {
    flag: &'a std::sync::atomic::AtomicBool,
    armed: bool,
}

struct PendingManualStopGuard {
    app: AppHandle,
    meeting_id: String,
    armed: bool,
}

impl PendingManualStopGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingManualStopGuard {
    fn drop(&mut self) {
        if self.armed {
            crate::transcribe::live::fail_pending_manual_for_meeting(&self.app, &self.meeting_id);
        }
    }
}

impl RecordingStartingFlag<'_> {
    fn disarm(&mut self) {
        self.flag.store(false, std::sync::atomic::Ordering::Release);
        self.armed = false;
    }
}

impl Drop for RecordingStartingFlag<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.flag.store(false, std::sync::atomic::Ordering::Release);
        }
    }
}

impl RecordingStartGuard<'_> {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RecordingStartGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self
                .db
                .update_meeting_status(&self.meeting_id, MeetingStatus::Error);
        }
    }
}

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
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if recorder.is_some() {
            return Err(AppError::Audio("already recording".into()));
        }
    }

    if state
        .recording_starting
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .is_err()
    {
        return Err(AppError::Audio(
            "recording start already in progress".into(),
        ));
    }
    let mut recording_starting = RecordingStartingFlag {
        flag: &state.recording_starting,
        armed: true,
    };

    // Install recording priority before listener/helper/DB/capture side effects. The owner stays
    // local through every fallible preparation step, so any early return drops it to Aborted.
    let mut model_session = crate::perf::begin_recording_session()?;
    {
        let app2 = app.clone();
        tokio::task::spawn_blocking(move || stop_voice_listener(&app2))
            .await
            .map_err(|e| AppError::Audio(format!("voice-listener stop worker panicked: {e}")))??;
    }
    // Starting atomically closes new unscoped model/egress admission, but a generation admitted
    // immediately before it may not have spawned its helper PID yet. One kill followed by one long
    // wait has a check-to-spawn hole: the helper can appear after that kill and burn CPU/RAM for the
    // whole 30 s. Repeatedly signal the out-of-process Brain, then wait only a short exact-session
    // slice, under one shared deadline. Quiescence is authoritative; once true, Starting prevents a
    // late unscoped spawn. Cloud egress simply drains through these slices.
    let quiescence_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let remaining = quiescence_deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(AppError::Unavailable(
                "local AI did not quiesce in time to start recording safely".into(),
            ));
        }
        if !crate::reason::sidecar::kill_for_recording_async(
            remaining.min(std::time::Duration::from_secs(2)),
        )
        .await?
        {
            return Err(AppError::Unavailable(
                "on-device Brain could not be proven stopped; recording was not started".into(),
            ));
        }
        let remaining = quiescence_deadline.saturating_duration_since(std::time::Instant::now());
        if !crate::summarize::claude_code::kill_for_recording(
            remaining.min(std::time::Duration::from_secs(2)),
        )
        .await?
        {
            return Err(AppError::Unavailable(
                "Cloud AI CLI processes could not be proven stopped; recording was not started"
                    .into(),
            ));
        }
        let remaining = quiescence_deadline.saturating_duration_since(std::time::Instant::now());
        if model_session
            .wait_for_quiescence_async(remaining.min(std::time::Duration::from_millis(50)))
            .await?
        {
            break;
        }
    }
    let afm_reaped = tokio::task::spawn_blocking(|| {
        crate::reason::afm::reap_unreaped_for_recording(std::time::Duration::from_secs(2))
    })
    .await
    .map_err(|error| {
        AppError::Other(anyhow::anyhow!(
            "Apple Foundation Models reap worker panicked: {error}"
        ))
    })??;
    if !afm_reaped {
        return Err(AppError::Unavailable(
            "Apple Foundation Models worker could not be proven stopped; recording was not started"
                .into(),
        ));
    }
    state.reasoner.release_local_cache();
    crate::embed::release_real_embedder_cache();
    crate::summarize::ner_deberta::release_all_caches();
    if !crate::reason::sidecar::kill_for_recording_async(std::time::Duration::from_secs(2)).await? {
        return Err(AppError::Unavailable(
            "on-device Brain did not exit in time; recording was not started".into(),
        ));
    }
    let ollama_config = state
        .config
        .lock()
        .map_err(|_| AppError::Unavailable("configuration mutex poisoned".into()))?
        .clone();
    let unloaded =
        crate::summarize::ollama::unload_local_models_for_recording(&ollama_config).await;
    if unloaded > 0 {
        tracing::info!(target: "ollama", models = unloaded, "released loopback Ollama models before recording");
    }
    if crate::perf::resident_model_quarantine_key(crate::perf::ResidentModelKind::Ollama).is_some()
    {
        return Err(AppError::Unavailable(
            "a previous Ollama generation could not be proven stopped; recording is blocked until a verified unload or app restart"
                .into(),
        ));
    }
    // A non-cancellable generation may have populated an idle cache while Start waited.
    state.reasoner.release_local_cache();
    crate::embed::release_real_embedder_cache();
    crate::summarize::ner_deberta::release_all_caches();
    // Re-scan helpers BEFORE spawning this recording's own: a survivor from a crashed/SIGKILL'd
    // instance must never capture alongside a new session. Cross-process `kill(pid)` is not safe
    // against PID reuse on Darwin, so this boundary is deliberately detection-only: a live Murmur
    // child is never touched but still defers this Start, while any orphan/ambiguous target FAILS
    // CLOSED before the meeting row or capture artifacts are created. Current Swift helpers watch
    // an exact parent-owned stdin pipe and retain a 4 h wall cap; Brain watches its own protocol
    // pipe and exits even during a stuck generation. The scan stays on a blocking worker so
    // process-table probes do not stall Tokio.
    tauri::async_runtime::spawn_blocking(|| {
        crate::audio::aec::detect_surviving_capture_helpers(None)
    })
    .await
    .map_err(|error| {
        AppError::Audio(format!(
            "surviving-helper detection worker panicked: {error}"
        ))
    })??;

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

    let _recording_lifecycle = state
        .lifecycle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // The optimistic check above intentionally happens before the listener-stop await, but it is
    // same lifecycle exclusion used by install/delete, before creating the meeting row or any
    // artifact. This check and the final slot install are one synchronous critical section.
    {
        let recorder = state
            .recorder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if recorder.is_some() {
            return Err(AppError::Audio("already recording".into()));
        }
    }
    *state
        .recording_stop
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    // Only the authoritative Start winner may reset per-recording observer state. A concurrent
    // loser must not clear the active meeting's fault latch, cards or live bullets.
    state
        .capped_notified
        .store(false, std::sync::atomic::Ordering::Relaxed);
    state
        .capture_fault_notified
        .store(false, std::sync::atomic::Ordering::Relaxed);
    state
        .reactions_shadow_count
        .store(0, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut emitted) = state.reactions_emitted.lock() {
        emitted.clear();
    }
    crate::transcribe::bullets::clear_ram(&state.live_bullets, &state.live_bullets_tracker);

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
    let mut start_guard = RecordingStartGuard {
        db: &state.db,
        meeting_id: meeting_id.clone(),
        armed: true,
    };

    // PREPARE the mic stream while still paused. No frame may become capturable until a create-new
    // stable-handle artifact, SQLCipher generation row and sole checkpoint writer all exist.
    let input_device = state
        .config
        .lock()
        .ok()
        .and_then(|c| c.input_device.clone());
    let mut prepared = Recorder::prepare(input_device)?;
    let src_rate = prepared.source_sample_rate();
    let generation = crate::storage::models::RecordingGenerationKey::fresh(&meeting_id)?;
    // Resolve every fallible directory component before activation. Once `activate` succeeds the
    // remaining path contains no `?` until the sole ActiveRecording owner is installed.
    let recording_inflight_dir = pipeline::recording_inflight_dir()?;
    let raw_path = recording_inflight_dir.join(format!("{}.mic.f32", generation.generation_id()));
    let sink = RawF32LeSink::create(raw_path)?;
    let mic = crate::storage::models::RecordingMicAssertion::for_generation(
        &generation,
        src_rate,
        sink.device(),
        sink.inode(),
    )?;
    let lease = match state
        .db
        .prepare_recording_generation(&generation, &mic, RECORDING_LEASE_MS)
    {
        Ok(lease) => lease,
        Err(error) => {
            // A transaction/commit failure can be ambiguous. Unlink only after proving that no
            // generation row exists; otherwise preserve the verified inode for reconciliation.
            match state.db.get_recording_generation_snapshot(&generation) {
                Ok(None) => discard_untracked_empty_mic(sink)?,
                Ok(Some(_)) | Err(_) => tracing::warn!(
                    target: "audio",
                    "recording PREPARE failed ambiguously; preserving the empty artifact for recovery"
                ),
            }
            return Err(error);
        }
    };
    let generation_heartbeat = lease.heartbeat();
    let sample_reader = prepared.sample_reader();
    let checkpoint_writer = prepared.take_checkpoint_writer()?;
    let spool = CaptureSpool::start(
        state.db.clone(),
        generation.clone(),
        lease,
        mic.clone(),
        sample_reader,
        checkpoint_writer,
        sink,
    )?;
    let recorder = prepared.activate()?;

    // Optionally capture system audio (the other side of the call) alongside the mic.
    // Best-effort: if it can't start, we log and record mic-only — never fail recording.
    // `sys_scratch_for_spill` remembers the far-side scratch path so the crash-salvage sidecar can
    // pair the "others" track at next launch (only set when the system recorder actually started).
    let mut system = {
        let enabled = state
            .config
            .lock()
            .map(|c| c.capture_system_audio)
            .unwrap_or(false);
        if enabled && crate::audio::system::is_available(&app) {
            let sys_wav =
                recording_inflight_dir.join(format!("{}.system.wav", generation.generation_id()));
            match crate::audio::system::SystemAudioRecorder::start(&app, sys_wav) {
                Ok(rec) => Some(rec),
                Err(e) => {
                    tracing::warn!(
                        target: "audio", error = %e,
                        "system-audio capture unavailable; recording mic only"
                    );
                    None
                }
            }
        } else {
            None
        }
    };
    if let Some(system_recorder) = system.as_ref() {
        let offset_micros =
            signed_instant_offset_micros(recorder.started_at(), system_recorder.started_at());
        if let Err(error) = state.db.set_recording_system_start_offset(
            &generation,
            &generation_heartbeat,
            offset_micros,
        ) {
            let outcome = system
                .take()
                .ok_or_else(|| AppError::Audio("system recorder ownership was lost".into()))?
                .stop();
            crate::audio::source::discard_unaligned_system_stop(&outcome)?;
            tracing::warn!(target: "audio", error = %error, "system capture alignment was not durable; continuing mic-only");
        }
    }

    // Capture, spool, ledger and system alignment are now prepared. Open Live admission only at
    // this point, then move the affine owner into the same ActiveRecording value as capture.
    model_session.transition_to_live()?;
    let recording_model_token = model_session.token();
    let active_recording = ActiveRecording::new(
        meeting_id.clone(),
        recorder,
        spool,
        generation,
        mic,
        system,
        model_session,
    );
    // The live thread keeps a generation-bound file handle even after Stop atomically removes the
    // sole ActiveRecording owner from AppState. That lets an already-ended manual command wait for
    // the spool's certified prefix instead of turning a Stop race into a fake "nothing heard".
    let manual_clip_source = active_recording.manual_clip_source();

    {
        let mut slot = state
            .recorder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut current = state
            .current_meeting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = Some(active_recording);
        *current = Some(meeting_uuid);
    }
    recording_starting.disarm();
    start_guard.disarm();

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
        // recording. The live-safe companion model is fetched by `download_model` (see
        // `commands/live_captions.rs`) so the DEFAULT install has one; recording lifecycle code
        // never launches a ~487 MB background transfer beside capture.
        //
        // The whole chain lives in `live_captions::resolve` — ONE decision, also read by
        // `get_config` so the recorder UI can render the "live captions are off" state instead of
        // this log line being the only trace of it.
        let resolved = live_captions::resolve(&cfg);
        // The pin the resolution actually used (an empty config pin still pins `small` under the
        // legacy `brain_live` guarantee), so the log field never reads as an empty pin.
        let pin = crate::transcribe::model::live_pin_size(&cfg.live_model_pin, cfg.brain_live)
            .unwrap_or_default();
        match &resolved {
            live_captions::LiveCaptions::Fallback(_) => tracing::warn!(
                target: "live",
                pin = %pin,
                "pinned live model absent; live tick uses the largest downloaded live-safe whisper model"
            ),
            live_captions::LiveCaptions::Configured(_) => tracing::warn!(
                target: "live",
                pin = %pin,
                "pinned live model absent; live tick uses the configured whisper model (may contend with the light reasoner)"
            ),
            // Only medium/large-class models downloaded (e.g. a fresh turbo-default install whose
            // live-safe companion download never landed): NEVER run a large encoder on the 3 s live
            // tick (T1.3 heat). The FE surfaces this state from `get_config`.
            live_captions::LiveCaptions::ModelMissing => tracing::warn!(
                target: "live",
                pin = %pin,
                "pinned live model absent and only medium/large models downloaded; live captions off for this recording; download a live-safe model from Settings while idle"
            ),
            live_captions::LiveCaptions::PinnedHeavy => tracing::warn!(
                target: "live",
                pin = %pin,
                "the pinned live model is a medium/large-class size that is not downloaded; live captions off for this recording (a heavy model is never run on the live tick)"
            ),
            live_captions::LiveCaptions::Pinned(_)
            | live_captions::LiveCaptions::Unpinned(_)
            | live_captions::LiveCaptions::NoModel => {}
        }
        if let Some(model_path) = resolved.model_path() {
            crate::transcribe::live::spawn(
                app.clone(),
                meeting_id.clone(),
                model_path,
                cfg.language.clone(),
                recording_model_token,
                manual_clip_source,
            );
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
    spawn_recording_terminal_watchdog(app.clone(), meeting_id.clone());

    Ok(StartResult { meeting_id })
}

fn signed_instant_offset_micros(
    mic_started_at: std::time::Instant,
    system_started_at: std::time::Instant,
) -> i64 {
    if system_started_at >= mic_started_at {
        i64::try_from(system_started_at.duration_since(mic_started_at).as_micros())
            .unwrap_or(i64::MAX)
    } else {
        -i64::try_from(mic_started_at.duration_since(system_started_at).as_micros())
            .unwrap_or(i64::MAX)
    }
}

/// Stop capture, then run the full pipeline (pipeline::run_after_stop). Returns the exported note
/// path + markdown. `companion_flush_completed` is an optional FE durability witness: only explicit
/// `Some(true)` permits deletion of an empty companion stub; missing/false preserves it. Emits
/// status events throughout. Errors if not recording.
#[tauri::command]
pub async fn stop_recording(
    app: AppHandle,
    state: State<'_, AppState>,
    companion_flush_completed: Option<bool>,
) -> Result<StopResult, AppError> {
    let flight =
        launch_recording_stop_flight(&app, state.inner(), companion_flush_completed, None)?;
    let result = flight.wait().await.map_err(AppError::Audio)?;
    ensure_post_await_result_visible(state.inner(), &result.meeting_id)?;
    Ok(StopResult {
        meeting_id: result.meeting_id,
        markdown: result.markdown,
        exported_path: result.exported_path,
    })
}

/// Start (or join) the one detached Stop owner. Backend terminal-capture detection uses the same
/// seam as the command, so finalization never depends on a live webview or its 100 ms level poll.
fn launch_recording_stop_flight(
    app: &AppHandle,
    state: &AppState,
    companion_flush_completed: Option<bool>,
    expected_meeting_id: Option<&str>,
) -> Result<std::sync::Arc<crate::state::RecordingStopFlight>, AppError> {
    let (flight, launch) = {
        // Keep the recorder identity stable through single-flight lookup/creation. An old backend
        // watcher must never observe meeting A, lose the lock, then create a Stop owner for a
        // newly-installed meeting B.
        let recorder = state
            .recorder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut slot = state
            .recording_stop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let (Some(expected), Some(active)) = (expected_meeting_id, recorder.as_ref()) {
            if active.meeting_id != expected {
                return Err(AppError::Audio(
                    "recording Stop request belongs to a stale meeting".into(),
                ));
            }
        }
        match slot.as_ref() {
            Some(existing) => (existing.clone(), None),
            None => {
                let active_meeting_id = recorder
                    .as_ref()
                    .map(|active| active.meeting_id.clone())
                    .ok_or_else(|| AppError::Audio("not recording".into()))?;
                if expected_meeting_id.is_some_and(|expected| expected != active_meeting_id) {
                    return Err(AppError::Audio(
                        "recording Stop request belongs to a stale meeting".into(),
                    ));
                }
                let created = std::sync::Arc::new(crate::state::RecordingStopFlight::new());
                *slot = Some(created.clone());
                (created, Some(active_meeting_id))
            }
        }
    };
    if let Some(expected_owner_meeting_id) = launch {
        let owner_app = app.clone();
        let monitor_app = app.clone();
        let completion = flight.clone();
        // Only the invocation that creates the single-flight owner may authorize cleanup. Missing
        // (older webview/direct/concurrent/automatic caller) and explicit false both preserve the
        // stub, so a late companion save can never race a backend delete.
        let delete_empty_companion = companion_cleanup_allowed(companion_flush_completed);
        // The monitor awaits a nested task so an owner panic is converted into a completed shared
        // error instead of leaving every concurrent Stop waiter asleep forever.
        tauri::async_runtime::spawn(async move {
            let owner = tauri::async_runtime::spawn(stop_recording_owner(
                owner_app,
                delete_empty_companion,
                expected_owner_meeting_id,
            ));
            let outcome = match owner.await {
                Ok(Ok(result)) => Ok(crate::state::RecordingStopResult {
                    meeting_id: result.meeting_id,
                    markdown: result.markdown,
                    exported_path: result.exported_path,
                }),
                Ok(Err(error)) => Err(error.to_string()),
                Err(error) => Err(format!("recording Stop task crashed: {error}")),
            };
            let failed = outcome.is_err();
            completion.complete(outcome);
            if failed {
                let state = monitor_app.state::<AppState>();
                let mut slot = state
                    .recording_stop
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if slot
                    .as_ref()
                    .is_some_and(|current| std::sync::Arc::ptr_eq(current, &completion))
                {
                    *slot = None;
                }
            }
        });
    }
    Ok(flight)
}

enum RecordingTerminalTrigger {
    CaptureFault {
        fault: crate::audio::recorder::CaptureFault,
        retained_frames: u64,
        sample_rate: u32,
    },
    Capped,
}

/// Observe the backend recorder itself, not the renderer. A webview reload/crash, a missed event,
/// or a hidden window must not leave a self-stopped recorder and its affine spool/model owner stuck
/// in `AppState`. The meeting id fences this watcher from a later recording.
fn spawn_recording_terminal_watchdog(app: AppHandle, meeting_id: String) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let trigger = {
                let state = app.state::<AppState>();
                let mut slot = state
                    .recorder
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(active) = slot.as_mut() else {
                    return;
                };
                if active.meeting_id != meeting_id {
                    return;
                }
                let mic_is_muted = active.is_muted();
                if crate::audio::system::mic_must_be_restored(mic_is_muted, active.system.as_mut())
                {
                    // The helper was healthy when mute was accepted but can fail later. Restore
                    // the mic within this backend-owned 100 ms heartbeat; renderer health is not
                    // required, so a hidden/reloaded webview cannot leave the recording silent.
                    active.set_muted(false);
                    tracing::warn!(
                        target: "audio",
                        "system audio became unavailable while mic-muted; microphone restored"
                    );
                    crate::events::emit_mic_auto_unmuted(&app);
                }
                if let Some(fault) = active.fault() {
                    Some(RecordingTerminalTrigger::CaptureFault {
                        fault,
                        retained_frames: active.total_samples() as u64,
                        sample_rate: active.source_sample_rate(),
                    })
                } else if active.cap_reached() {
                    Some(RecordingTerminalTrigger::Capped)
                } else {
                    None
                }
            };
            let Some(trigger) = trigger else {
                continue;
            };

            let state = app.state::<AppState>();
            match trigger {
                RecordingTerminalTrigger::CaptureFault {
                    fault,
                    retained_frames,
                    sample_rate,
                } => {
                    if !state
                        .capture_fault_notified
                        .swap(true, std::sync::atomic::Ordering::AcqRel)
                    {
                        crate::events::emit_recording_capture_fault(
                            &app,
                            fault,
                            retained_frames,
                            sample_rate,
                        );
                    }
                }
                RecordingTerminalTrigger::Capped => {
                    if !state
                        .capped_notified
                        .swap(true, std::sync::atomic::Ordering::AcqRel)
                    {
                        crate::events::emit_recording_capped(&app);
                    }
                }
            }
            let _ = launch_recording_stop_flight(&app, state.inner(), None, Some(&meeting_id));
            return;
        }
    });
}

fn companion_cleanup_allowed(companion_flush_completed: Option<bool>) -> bool {
    companion_flush_completed == Some(true)
}

async fn delete_empty_companion_after_confirmed_flush(
    state: &AppState,
    meeting_id: &str,
    companion_flush_completed: bool,
    app: Option<&AppHandle>,
) -> Result<bool, AppError> {
    if !companion_flush_completed {
        return Ok(false);
    }
    delete_companion_note_if_empty_inner_notifying(state, meeting_id, app).await
}

async fn stop_recording_owner(
    app: AppHandle,
    delete_empty_companion: bool,
    expected_meeting_id: String,
) -> Result<StopResult, AppError> {
    // DETACHED, panic-mapped Stop + pipeline execution. Ownership moves into a real task BEFORE
    // this command awaits anything, so webview cancellation cannot drop the sole capture/spool
    // handles. The blocking cpal stop, spool drain/fsync and thread joins run on the blocking pool,
    // never on a Tokio command worker.
    let task_app = app.clone();
    let stop_task = tauri::async_runtime::spawn(async move {
        let guarded_meeting_id = {
            let state = task_app.state::<AppState>();
            let slot = state
                .recorder
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let active_meeting_id = slot
                .as_ref()
                .map(|active| active.meeting_id.clone())
                .ok_or_else(|| AppError::Audio("not recording".into()))?;
            if active_meeting_id != expected_meeting_id {
                return Err(AppError::Audio(
                    "recording Stop owner belongs to a stale meeting".into(),
                ));
            }
            active_meeting_id
        };
        // Armed before the affine ActiveRecording owner is taken. Any panic/error in finalization,
        // companion cleanup, or the later pipeline now clears current_meeting and reaches Error.
        let state = task_app.state::<AppState>();
        let terminal_guard = pipeline::TerminalStatusGuard::arm(
            Some(task_app.clone()),
            state.db.clone(),
            &guarded_meeting_id,
        );
        let mut pending_manual_guard = PendingManualStopGuard {
            app: task_app.clone(),
            meeting_id: guarded_meeting_id.clone(),
            armed: true,
        };
        let finish_app = task_app.clone();
        let mut finish_task = tauri::async_runtime::spawn_blocking(move || {
            let state = finish_app.state::<AppState>();
            let _recording_lifecycle = state
                .lifecycle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut active = {
                let mut slot = state
                    .recorder
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                // Same lock order as begin/end_voice_command: recorder -> capture. Latch every
                // still-armed command at this recording's exact terminal frame before removing the
                // ActiveRecording. The stable ManualClipSource held by the live thread can then
                // finish from the durable spool even though the recorder slot is already empty.
                let observed_stop = slot.as_ref().map(|active| active.total_samples());
                let mut capture = state
                    .voice_command_capture
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(capture) = capture.as_mut() {
                    capture.max_end_sample = latch_manual_end_sample(
                        capture.start_sample,
                        capture.max_end_sample,
                        observed_stop,
                    );
                    capture.ended = true;
                }
                drop(capture);
                slot.as_mut()
                    .ok_or_else(|| AppError::Audio("not recording".into()))?
                    .transition_model_to_draining()?;
                slot.take()
                    .ok_or_else(|| AppError::Audio("not recording".into()))?
            };
            let meeting_id = active.meeting_id.clone();
            let mut finish_failures = 0u8;
            let (finalized, model_session) = loop {
                match active.try_finish(&state.db) {
                    Ok(Some(finalized)) => break finalized,
                    Ok(None) => {
                        std::thread::sleep(std::time::Duration::from_millis(25));
                    }
                    Err(error) => {
                        finish_failures = finish_failures.saturating_add(1);
                        if finish_failures < 3 {
                            std::thread::sleep(std::time::Duration::from_millis(50));
                            continue;
                        }
                        if let Err(release_error) = active.release_for_recovery(&state.db) {
                            tracing::warn!(target: "audio", error = %release_error, "failed Stop could not expire its ledger lease; startup recovery will wait for natural expiry");
                        }
                        let _ = state
                            .db
                            .update_meeting_status(&meeting_id, MeetingStatus::Error);
                        if let Ok(mut current) = state.current_meeting.lock() {
                            *current = None;
                        }
                        crate::transcribe::live::clear_live_transcript(&state.live_transcript);
                        crate::transcribe::bullets::clear_ram(
                            &state.live_bullets,
                            &state.live_bullets_tracker,
                        );
                        return Err(error);
                    }
                }
            };
            if let Ok(mut current) = state.current_meeting.lock() {
                *current = None;
            }
            let duration_s = compute_duration_s(
                &state,
                &meeting_id,
                finalized.frames as usize,
                finalized.sample_rate,
            );
            Ok((meeting_id, finalized, duration_s, model_session))
        });

        let finish = match tokio::time::timeout(
            std::time::Duration::from_secs(20),
            &mut finish_task,
        )
        .await
        {
            Ok(joined) => joined.map_err(|error| {
                AppError::Other(anyhow::anyhow!(
                    "recording finalization task failed: {error}"
                ))
            })??,
            Err(_) => {
                // `spawn_blocking` cannot be safely aborted. Move its still-owned JoinHandle into
                // a continuation so ActiveRecording, native threads, spool lease and Draining model
                // owner remain alive until proven settled. Only then release the generation and
                // drop model admission; a permanently wedged producer therefore fails closed.
                let recovery_app = task_app.clone();
                let recovery_meeting_id = guarded_meeting_id.clone();
                tauri::async_runtime::spawn(async move {
                    // The outer guard must be disarmed while the blocking owner is still alive, so
                    // transfer terminal responsibility to this continuation. A late JoinError or
                    // panic in its cleanup then still expires recovery ownership, clears the live
                    // pointers and persists Error instead of leaving a forever-Recording ghost.
                    let late_terminal_guard = pipeline::TerminalStatusGuard::arm(
                        Some(recovery_app.clone()),
                        recovery_app.state::<AppState>().db.clone(),
                        &recovery_meeting_id,
                    );
                    let terminal_cleanup_completed = match finish_task.await {
                        Ok(Ok((meeting_id, finalized, _duration_s, model_session))) => {
                            let cleanup_app = recovery_app.clone();
                            let cleanup = tauri::async_runtime::spawn_blocking(
                                move || -> Result<(), AppError> {
                                    let state = cleanup_app.state::<AppState>();
                                    let _lifecycle = state
                                        .lifecycle
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                                    // These two writes are the durable hand-off from the timed-out Stop
                                    // owner to startup recovery. Do not disarm TerminalStatusGuard unless
                                    // BOTH succeeded: a merely non-panicking cleanup is not proof that
                                    // the generation was released or the meeting left Recording.
                                    finalized.release_for_recovery(&state.db)?;
                                    state
                                        .db
                                        .update_meeting_status(&meeting_id, MeetingStatus::Error)?;
                                    if let Ok(mut current) = state.current_meeting.lock() {
                                        if current.as_ref().map(uuid::Uuid::to_string).as_deref()
                                            == Some(meeting_id.as_str())
                                        {
                                            *current = None;
                                        }
                                    }
                                    crate::transcribe::live::clear_live_transcript(
                                        &state.live_transcript,
                                    );
                                    crate::transcribe::bullets::clear_ram(
                                        &state.live_bullets,
                                        &state.live_bullets_tracker,
                                    );
                                    drop(model_session);
                                    Ok(())
                                },
                            )
                            .await;
                            match cleanup {
                                Ok(Ok(())) => true,
                                Ok(Err(error)) => {
                                    tracing::error!(target: "audio", error = %error, "late Stop recovery could not persist its terminal hand-off");
                                    false
                                }
                                Err(error) => {
                                    tracing::error!(target: "audio", error = %error, "late Stop recovery worker panicked");
                                    false
                                }
                            }
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(target: "audio", error = %error, "timed-out Stop finished with preserved recovery state");
                            false
                        }
                        Err(error) => {
                            tracing::error!(target: "audio", error = %error, "timed-out Stop worker panicked");
                            false
                        }
                    };
                    crate::transcribe::live::fail_pending_manual_for_meeting(
                        &recovery_app,
                        &recovery_meeting_id,
                    );
                    if terminal_cleanup_completed {
                        late_terminal_guard.disarm();
                    }
                });
                // The guard must not mutate/expire a generation still owned by the continuation.
                terminal_guard.disarm();
                pending_manual_guard.disarm();
                return Err(AppError::Audio(
                    "recording Stop exceeded 20 seconds; capture recovery is continuing safely in the background"
                        .into(),
                ));
            }
        };

        let (meeting_id, finalized, duration_s, mut model_session) = finish;
        let state = task_app.state::<AppState>();

        // The recorder slot is gone, so the live loop is terminating and Draining admits no new
        // work. Wait for its exact-token model/egress leases, then reclaim idle caches/sidecar
        // before opening Postprocess for batch Whisper and note generation.
        let live_quiescent = model_session
            .wait_for_quiescence_async(std::time::Duration::from_secs(30))
            .await?;
        if !live_quiescent {
            if let Err(error) = finalized.release_for_recovery(&state.db) {
                tracing::warn!(target: "audio", error = %error, "live-drain failure could not release finalized generation; recovery will wait for lease expiry");
            }
            return Err(AppError::Unavailable(
                "live local AI did not drain in time; postprocess was not started".into(),
            ));
        }
        state.reasoner.release_local_cache();
        crate::embed::release_real_embedder_cache();
        crate::summarize::ner_deberta::release_all_caches();
        if !crate::reason::sidecar::kill_for_recording_async(std::time::Duration::from_secs(2))
            .await?
        {
            if let Err(error) = finalized.release_for_recovery(&state.db) {
                tracing::warn!(target: "audio", error = %error, "sidecar-drain failure could not release finalized generation; recovery will wait for lease expiry");
            }
            return Err(AppError::Unavailable(
                "on-device Brain did not exit in time; postprocess was not started".into(),
            ));
        }
        model_session.transition_to_postprocess()?;
        let recording_model_token = model_session.token().validated_for_postprocess()?;

        // A manual command whose exact audio range became durable while Stop was draining remains
        // in a single owned AppState slot. Dispatch it now under this recording's exact
        // Postprocess token and await completion before the batch pipeline can claim the one model
        // lane. PROCESSING is emitted by the handoff only after its worker was actually accepted.
        crate::transcribe::live::dispatch_pending_manual_after_stop(
            &task_app,
            &meeting_id,
            recording_model_token.clone(),
        )
        .await;
        pending_manual_guard.disarm();
        // Keep the visibility-gated live context until the cross-Stop manual command has consumed
        // it. The final batch transcript is not in SQLite yet, so clearing these buffers before the
        // owned handoff would make a valid "what did they say?" command answer from an empty note.
        crate::transcribe::live::clear_live_transcript(&state.live_transcript);
        crate::transcribe::bullets::clear_ram(&state.live_bullets, &state.live_bullets_tracker);

        // Delete an empty stub only with the FE's explicit durable-flush witness. A missing/false
        // witness means a save may still be in flight after the bounded FE deadline, so preserve
        // the row; the late write can then land safely (at worst an unused empty stub remains).
        if !delete_empty_companion {
            tracing::info!(target: "notes", meeting_id = %meeting_id, "empty-companion cleanup skipped because durable flush was not confirmed");
        }
        match delete_empty_companion_after_confirmed_flush(
            &state,
            &meeting_id,
            delete_empty_companion,
            Some(&task_app),
        )
        .await
        {
            Ok(true) => emit_ask_history_invalidated_fail_closed(&task_app),
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(target: "notes", meeting_id = %meeting_id, error = %error, "empty-companion cleanup skipped (Stop unaffected)");
            }
        }

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
        let task_meeting_id = meeting_id.clone();
        let result = pipeline::run_file_backed(
            &task_app,
            &state,
            &task_meeting_id,
            finalized,
            duration_s,
            Some(recording_model_token),
        )
        .await;
        let lifecycle_result = match model_session
            .wait_for_quiescence_async(std::time::Duration::from_secs(30))
            .await
        {
            Ok(true) => {
                state.reasoner.release_local_cache();
                crate::embed::release_real_embedder_cache();
                crate::summarize::ner_deberta::release_all_caches();
                match crate::reason::sidecar::kill_for_recording_async(
                    std::time::Duration::from_secs(2),
                )
                .await
                {
                    Ok(true) => model_session.finish(),
                    Ok(false) => Err(AppError::Unavailable(
                        "postprocess Brain did not exit in time".into(),
                    )),
                    Err(error) => Err(error),
                }
            }
            Ok(false) => Err(AppError::Unavailable(
                "postprocess local AI did not drain in time".into(),
            )),
            Err(error) => Err(error),
        };
        let result = match (result, lifecycle_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Ok(_), Err(error)) => Err(error),
            (Err(error), _) => Err(error),
        };
        terminal_guard.disarm();
        // Resume voice listening if it's still enabled (the mic is free again). Inside the task
        // (Ok arm only, preserving the pre-change `?` semantics) so it still runs when the outer
        // command future was dropped mid-pipeline.
        if result.is_ok() {
            restart_voice_listener(task_app.clone());
        }
        let result = result?;
        Ok(StopResult {
            meeting_id: result.meeting_id,
            markdown: result.note_markdown,
            exported_path: result
                .exported_path
                .map(|path| path.to_string_lossy().to_string()),
        })
    });
    await_pipeline_task(stop_task).await
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
    let visibility = {
        let state = app.state::<AppState>();
        capture_content_visibility_snapshot(&state)
    };
    // note↔meeting-links PR-2 — SOURCE-SCOPED augmentation (PARTIAL, the SHOULD leg). The @brain
    // assistant loop (`run_assistant_query`) is a deep current-first cascade whose tool executor +
    // deterministic floor legs would need wide surgery to fully candidate-constrain; PR-2 threads
    // the pinned sources one level in so the cloud AGENTIC cascade reasons with the gated pinned
    // corpus injected into its conversation context. `None`/empty ⇒ byte-identical to before. The
    // remaining full candidate-constraint of the floor/tool legs is a documented follow-up.
    let explicit_sources = explicit_sources.filter(|s| !s.is_empty());
    let app_for_task = app.clone();
    let result = tokio::task::spawn_blocking(move || {
        crate::transcribe::live::run_assistant_query(
            &app_for_task,
            &latest,
            &conversation,
            crate::events::EVENT_CHAT_TOOL,
            &thread_id,
            anchor_text.as_deref(),
            meeting_id.as_deref(),
            explicit_sources.as_deref(),
            None, // run_assistant_query re-fetches the exact active-meeting token from AppState.
        )
    })
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("chat task join failed: {e}")))?;
    let state = app.state::<AppState>();
    require_current_content_visibility_snapshot(&state, visibility)?;
    Ok(result)
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
    let visibility = capture_meeting_content_snapshot(state.inner(), &meeting_id)?;
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
    if republish_org_shares_for_source_notifying(state.inner(), Some(&meeting_id), None, &app)
        .await
        .unwrap_or(0)
        > 0
    {
        crate::events::emit_org_feed_updated(&app, 1);
    }
    require_current_meeting_content_snapshot(state.inner(), &meeting_id, &visibility)?;
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
    let exported_markdown = if markdown.contains("murmur-attachment://") {
        let vault = vault_path(state)
            .ok_or_else(|| AppError::Export("no vault configured for image export".into()))?;
        render_markdown_with_attachments_for_export_under_lifecycle_authorized(
            state,
            &crate::storage::AttachmentOwner::Meeting {
                meeting_id: meeting_id.to_string(),
                provider_id: provider_id.to_string(),
            },
            markdown,
            std::path::Path::new(&vault),
        )?
    } else {
        markdown.to_string()
    };
    let expected = state.db.get_note_exported_hash(meeting_id, provider_id)?;
    let path = std::path::Path::new(path);
    let sibling = crate::export::preserve_external_edit_if_any(path, expected.as_deref())?;
    crate::export::overwrite_note(path, &exported_markdown)?;
    state.db.set_note_exported_hash(
        meeting_id,
        provider_id,
        Some(&crate::export::note_content_hash(&exported_markdown)),
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
    let embedder = crate::embed::active_persistence_embedder_if_available();
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
pub(crate) fn link_related_notes_inner(
    _state: &AppState,
    _meeting_id: &str,
) -> Result<(), AppError> {
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

const CONVERTED_NOTE_START: &str = "<!-- murmur:converted-meeting-note -->";
const CONVERTED_NOTE_END: &str = "<!-- /murmur:converted-meeting-note -->";

/// Resolve one conversion-only template choice without mutating the user's global note style.
/// `None`/`default` means the current Settings default; built-ins are accepted directly; every
/// other value must be an existing saved-template id (unknown ids fail closed rather than silently
/// producing a different shape than the picker showed).
#[derive(Debug, Clone)]
struct ConversionTemplateSelection {
    style: String,
    saved: Option<crate::storage::models::NoteTemplate>,
}

fn resolve_conversion_template(
    requested: Option<&str>,
    configured_default: &str,
    saved_templates: Vec<crate::storage::models::NoteTemplate>,
) -> Result<ConversionTemplateSelection, AppError> {
    let requested = requested.map(str::trim).filter(|id| !id.is_empty());
    let selected = match requested {
        None | Some("default") => configured_default.trim(),
        Some(id) => id,
    };
    if matches!(selected, "" | "standard" | "brief" | "detailed" | "action") {
        return Ok(ConversionTemplateSelection {
            style: selected.to_string(),
            saved: None,
        });
    }
    if let Some(saved) = saved_templates
        .into_iter()
        .find(|template| template.id == selected)
    {
        return Ok(ConversionTemplateSelection {
            style: selected.to_string(),
            saved: Some(saved),
        });
    }
    Err(AppError::InvalidArg(
        "the selected note template no longer exists".into(),
    ))
}

/// Compose a generated conversion into the companion note without ever replacing user-authored
/// prose. On first conversion the model's front-matter becomes the base (plus Murmur's managed
/// `meeting:` wikilink). Later conversions replace only the fenced generated block; text before or
/// after it survives byte-for-byte. The generated front-matter is not allowed to overwrite an
/// existing note's user-managed front-matter.
fn compose_converted_companion_markdown(
    current: &str,
    meeting_name: &str,
    generated: &str,
) -> Result<String, AppError> {
    let (current_yaml, current_body) = crate::storage::db::split_front_matter(current);
    let (generated_yaml, generated_body) = crate::storage::db::split_front_matter(generated);
    let generated_body = generated_body.trim();
    if generated_body.is_empty() {
        return Err(AppError::Unavailable(
            "the note provider returned an empty note".into(),
        ));
    }
    if generated_body.contains(CONVERTED_NOTE_START) || generated_body.contains(CONVERTED_NOTE_END)
    {
        return Err(AppError::InvalidArg(
            "the note provider returned Murmur's reserved conversion markers".into(),
        ));
    }

    let managed = format!("{CONVERTED_NOTE_START}\n{generated_body}\n{CONVERTED_NOTE_END}");
    // Keep every byte outside the managed fence untouched. Emptiness is semantic only; never
    // `trim_end` user prose because trailing blank lines may be deliberate Markdown formatting.
    let current_body_is_empty = current_body.trim().is_empty();
    let starts = current_body
        .match_indices(CONVERTED_NOTE_START)
        .collect::<Vec<_>>();
    let ends = current_body
        .match_indices(CONVERTED_NOTE_END)
        .collect::<Vec<_>>();
    let body = match (starts.as_slice(), ends.as_slice()) {
        ([(start, _)], [(end, _)]) if start < end => {
            let suffix_start = *end + CONVERTED_NOTE_END.len();
            format!(
                "{}{}{}",
                &current_body[..*start],
                managed,
                &current_body[suffix_start..]
            )
        }
        ([], []) if current_body_is_empty => managed,
        ([], []) => {
            let separator = if current_body.ends_with("\n\n") {
                ""
            } else if current_body.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            format!("{current_body}{separator}{managed}")
        }
        _ => {
            return Err(AppError::InvalidArg(
                "the companion note contains a malformed converted-note block; repair its Murmur markers before converting again".into(),
            ));
        }
    };

    // A newly-created companion has only Murmur's `meeting:` front-matter and no body. In that
    // state preserve the provider's declarative metadata. Once user content exists, their current
    // front-matter is authoritative and generated metadata cannot clobber it.
    let base_yaml = if current_body_is_empty && !generated_yaml.trim().is_empty() {
        generated_yaml
    } else {
        current_yaml
    };
    let base = if base_yaml.trim().is_empty() {
        body
    } else {
        format!("---\n{}\n---\n\n{body}", base_yaml.trim())
    };
    Ok(stamp_companion_meeting_link_preserving_body(
        &base,
        meeting_name,
    ))
}

fn conversion_transcript(segments: &[Segment], linked_context: &str) -> String {
    let mut transcript = segments
        .iter()
        .map(|segment| {
            let speaker = segment.speaker.as_deref().unwrap_or("unknown");
            format!(
                "[{:.0}-{:.0}s] ({speaker}) {}",
                segment.start_s,
                segment.end_s,
                segment.text.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if !linked_context.trim().is_empty() {
        transcript.push_str(
            "\n\nLINKED CONTEXT (secondary context selected in Related; the primary meeting \
             transcript above remains the source of truth for this meeting's decisions and tasks)\n",
        );
        transcript.push_str(linked_context.trim());
    }
    transcript
}

/// Persist a completed conversion while holding the lock lifecycle across the source-snapshot
/// recheck and the companion write. This closes the post-provider relock race: a stale generated
/// result can neither create nor update a note. The companion remains the canonical authored-note
/// row (`documents.meeting_id` + structured companion edge), and existing body text is preserved.
fn persist_converted_companion_under_snapshot(
    state: &AppState,
    meeting_id: &str,
    snapshot: &MeetingContentSnapshot,
    meeting_name: &str,
    generated: &str,
) -> Result<CompanionAppendResult, AppError> {
    let embedder = crate::embed::active_persistence_embedder_if_available();
    persist_converted_companion_under_snapshot_with(
        state,
        meeting_id,
        snapshot,
        meeting_name,
        generated,
        embedder.as_deref(),
    )
}

fn persist_converted_companion_under_snapshot_with(
    state: &AppState,
    meeting_id: &str,
    snapshot: &MeetingContentSnapshot,
    meeting_name: &str,
    generated: &str,
    embedder: Option<&dyn crate::embed::Embedder>,
) -> Result<CompanionAppendResult, AppError> {
    let lifecycle = lifecycle_guard(state);
    require_current_meeting_content_snapshot_under_lifecycle(state, meeting_id, snapshot)?;
    let meeting_wikilink = format!("[[{meeting_name}]]");

    let (note_id, markdown, created) = if let Some(id) =
        state.db.companion_note_for_meeting(meeting_id)?
    {
        let Some((folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(&id)? else {
            return Err(AppError::Storage(
                "the companion note disappeared during conversion".into(),
            ));
        };
        if !folder_is_unlocked(state, &folder_id)? {
            return Err(AppError::Locked(
                "unlock the companion note's folder before converting this meeting".into(),
            ));
        }
        let row = state
            .db
            .get_note_row(&id)?
            .ok_or_else(|| AppError::Storage("the companion note is unavailable".into()))?;
        let markdown = compose_converted_companion_markdown(&row.text, meeting_name, generated)?;
        (id, markdown, false)
    } else {
        // Compose/validate BEFORE birth. An empty or malformed provider response leaves no row.
        let markdown = compose_converted_companion_markdown("", meeting_name, generated)?;
        let id = create_generated_root_companion_under_lifecycle_authorized(
            state,
            meeting_id,
            meeting_name,
            &markdown,
        )?;
        (id, markdown, true)
    };
    if created {
        // DB canonical state is already complete atomically. Vault export remains a derived,
        // best-effort projection just like the ordinary note-update path.
        if let Err(error) = export_note_to_vault_under_lifecycle_authorized(state, &note_id) {
            tracing::warn!(
                target: "notes",
                error = %error,
                "converted note vault export failed (canonical DB note retained)"
            );
        }
    } else {
        update_note_doc_under_lifecycle_authorized(state, &note_id, meeting_name, &markdown)?;
    }
    drop(lifecycle);

    refresh_note_doc_derived_best_effort(state, &note_id, meeting_name, &markdown, embedder);

    Ok(CompanionAppendResult {
        note_id,
        meeting_wikilink,
    })
}

/// Birth the conversion companion in the reserved always-open Notes root. Unlike ordinary note
/// creation this path starts non-empty, but it cannot target a locked folder; consequently it does
/// not broaden sealed-note birth semantics. Document content, `meeting_id`, and the required
/// companion edge commit in one DB transaction.
fn create_generated_root_companion_under_lifecycle_authorized(
    state: &AppState,
    meeting_id: &str,
    title: &str,
    markdown: &str,
) -> Result<String, AppError> {
    let folder_id = state.db.ensure_notes_root()?;
    if !folder_is_unlocked(state, &folder_id)? {
        return Err(AppError::Locked(
            "the Notes root is unexpectedly locked; the converted note was not created".into(),
        ));
    }
    let title = title.trim();
    let title = if title.is_empty() {
        crate::storage::db::UNTITLED_TITLE
    } else {
        title
    };
    let id = uuid::Uuid::new_v4().to_string();
    validate_attachment_references_before_save(
        state,
        &crate::storage::AttachmentOwner::Document {
            document_id: id.clone(),
        },
        markdown,
    )?;
    state.db.insert_companion_note_atomic(
        &id,
        &folder_id,
        &crate::export::sanitize_title(title),
        title,
        markdown,
        meeting_id,
        chrono::Utc::now().timestamp_millis(),
    )?;
    tracing::info!(
        target: "notes",
        note_id = %id,
        meeting_id = %meeting_id,
        "converted companion note created atomically"
    );
    Ok(id)
}

/// Convert an unlocked meeting and its ACTIVE, visible Related items into the canonical companion
/// note. This is one bounded Notes-role summarize call through the existing provider factory; cloud
/// providers therefore retain the consent gate, RedactingProvider and content-free egress ledger.
#[tauri::command]
pub async fn convert_meeting_to_note(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
    template_id: Option<String>,
) -> Result<CompanionAppendResult, AppError> {
    convert_meeting_to_note_inner_with(Some(&app), state.inner(), meeting_id, template_id, None)
        .await
}

async fn convert_meeting_to_note_inner_with(
    app: Option<&AppHandle>,
    state: &AppState,
    meeting_id: String,
    template_id: Option<String>,
    provider_override: Option<std::sync::Arc<dyn crate::summarize::provider::SummarizerProvider>>,
) -> Result<CompanionAppendResult, AppError> {
    let config = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?
        .clone();
    let target =
        crate::summarize::roles::provider_target(crate::summarize::roles::Role::Notes, &config);
    let snapshot = capture_meeting_content_snapshot(state, &meeting_id)?;
    let (meeting, segments, manual_notes, linked_context, vault_titles) =
        read_current_meeting_content_under_snapshot(state, &meeting_id, &snapshot, || {
            let meeting = state
                .db
                .get_meeting(&meeting_id)?
                .ok_or_else(|| AppError::InvalidArg(format!("no meeting {meeting_id}")))?;
            let segments = state.db.get_segments(&meeting_id)?;
            let manual_notes = state.db.get_manual_notes(&meeting_id)?;
            let unlocked = unlocked_snapshot(state)?;
            let edges = active_conversion_related_edges(state, &meeting_id, &unlocked)?;
            let mut sources = Vec::new();
            let mut vault_titles = Vec::new();
            for edge in edges {
                let Some(kind) = crate::links::LinkKind::parse(&edge.other_kind) else {
                    continue;
                };
                sources.push(crate::storage::models::SourceRef {
                    kind,
                    id: edge.other_id,
                });
                if !edge.other_title.trim().is_empty() {
                    vault_titles.push(edge.other_title);
                }
            }
            let linked_context =
                crate::summarize::vault_context::build_vault_context_exact_visible_with_budget(
                    &state.db,
                    &sources,
                    crate::summarize::vault_context::budget_for(&target.connection),
                    &unlocked,
                )?;
            Ok((
                meeting,
                segments,
                manual_notes,
                linked_context,
                vault_titles,
            ))
        })?;
    if segments.is_empty() {
        return Err(AppError::InvalidArg(
            "this meeting has no transcript to convert".into(),
        ));
    }
    let selection = resolve_conversion_template(
        template_id.as_deref(),
        &config.note_style,
        state.db.list_note_templates()?,
    )?;
    let provider = match provider_override {
        Some(provider) => provider,
        None => crate::summarize::provider_for(
            crate::summarize::roles::Role::Notes,
            &config,
            &state.heavy_inference,
        )?,
    };
    let labeled = segments.iter().any(|segment| segment.speaker.is_some());
    let diarized_others = segments.iter().any(|segment| {
        segment
            .speaker
            .as_deref()
            .map(|speaker| speaker.starts_with("others-"))
            .unwrap_or(false)
    });
    let mut template = match selection.saved.as_ref() {
        Some(saved) => crate::summarize::template::build_template_from_saved(
            saved,
            &config.note_language,
            labeled,
            diarized_others,
            &config.user_display_name,
        ),
        None => crate::summarize::template::build_template(
            &selection.style,
            &config.note_language,
            labeled,
            diarized_others,
            &config.user_display_name,
        ),
    };
    template.push_str(
        "\n\nCONVERSION CONTEXT: Treat the primary meeting transcript as authoritative. Linked \
         context is secondary background selected by the user: use it to explain established \
         context and add valid [[wikilinks]], but never copy another item's decisions or action \
         items into this meeting unless the primary transcript explicitly supports them.",
    );
    // Typed in-meeting notes are part of the user's full meeting context. The source read is gated
    // by the same meeting snapshot/admission as the transcript and rides `SummarizeRequest` so the
    // existing RedactingProvider scrubs it before any cloud egress.
    let request = crate::summarize::SummarizeRequest {
        transcript: conversion_transcript(&segments, &linked_context),
        meta: crate::summarize::MeetingMeta {
            date_iso: meeting.started_at.chars().take(10).collect(),
            title_hint: meeting.title.clone(),
            duration_s: meeting.duration_s,
            language: config.language.clone(),
        },
        template,
        vault_titles,
        related_context: None,
        user_notes: (!manual_notes.trim().is_empty()).then_some(manual_notes),
        live_bullets: None,
        glossary: crate::summarize::template::render_glossary_for_prompt(&config.glossary),
    };
    let generated = match app {
        Some(app) => {
            let admission = meeting_dispatch_admission(app, meeting_id.clone(), snapshot.clone());
            admission.run(|| provider.summarize(&request)).await?
        }
        None => provider.summarize(&request).await?,
    };
    require_current_meeting_content_snapshot(state, &meeting_id, &snapshot)?;
    let meeting_name = meeting_display_name(meeting.title.as_deref());
    persist_converted_companion_under_snapshot(
        state,
        &meeting_id,
        &snapshot,
        &meeting_name,
        &generated,
    )
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
    let (yaml, body) = crate::storage::db::split_front_matter(current);
    let body_trimmed = body.trim_end();
    let block_trimmed = block.trim();
    let new_body = if body_trimmed.is_empty() {
        block_trimmed.to_string()
    } else if block_trimmed.is_empty() {
        body_trimmed.to_string()
    } else {
        format!("{body_trimmed}\n\n{block_trimmed}")
    };
    let body_with_legacy_newline = if new_body.is_empty() {
        String::new()
    } else {
        format!("{new_body}\n")
    };
    let with_existing_front_matter = if yaml.is_empty() {
        body_with_legacy_newline
    } else if body_with_legacy_newline.is_empty() {
        format!("---\n{yaml}\n---\n")
    } else {
        format!("---\n{yaml}\n---\n\n{body_with_legacy_newline}")
    };
    stamp_companion_meeting_link_preserving_body(&with_existing_front_matter, meeting_name)
}

/// Stamp/refresh only Murmur's top-level `meeting:` YAML key while preserving every BODY byte.
/// Conversion uses this after an exact managed-fence splice so user-authored trailing whitespace
/// and Markdown line-break markers cannot be normalized as a side-effect of re-conversion.
fn stamp_companion_meeting_link_preserving_body(current: &str, meeting_name: &str) -> String {
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

    if body.is_empty() {
        format!("{front_matter}\n")
    } else {
        format!("{front_matter}\n\n{body}")
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
            if let Some(row) = visible_authored_note_row_snapshot(state, &id)? {
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

/// Snapshot a visible authored-note row for cross-domain companion helpers. The lifecycle mutex and
/// content-free anchor ensure title/body/export-path columns are never selected before the folder
/// gate. Callers that later write re-enter the canonical note update path, which revalidates again.
fn visible_authored_note_row_snapshot(
    state: &AppState,
    note_id: &str,
) -> Result<Option<crate::storage::db::NoteRow>, AppError> {
    let _lifecycle = lifecycle_guard(state);
    let Some((folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(note_id)? else {
        return Ok(None);
    };
    if !folder_is_unlocked(state, &folder_id)? {
        return Ok(None);
    }
    state.db.get_note_row(note_id)
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
    let Some(row) = visible_authored_note_row_snapshot(state, &note_id)? else {
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
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<bool, AppError> {
    let deleted =
        delete_companion_note_if_empty_inner_notifying(state.inner(), &meeting_id, Some(&app))
            .await?;
    if deleted {
        emit_ask_history_invalidated_fail_closed(&app);
    }
    Ok(deleted)
}

/// Inner of [`delete_companion_note_if_empty`] taking `&AppState` (unit-testable gate).
#[cfg(test)]
pub(crate) async fn delete_companion_note_if_empty_inner(
    state: &AppState,
    meeting_id: &str,
) -> Result<bool, AppError> {
    delete_companion_note_if_empty_inner_notifying(state, meeting_id, None).await
}

async fn delete_companion_note_if_empty_inner_notifying(
    state: &AppState,
    meeting_id: &str,
    app: Option<&AppHandle>,
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
    let Some(row) = visible_authored_note_row_snapshot(state, &note_id)? else {
        return Ok(false); // race: note vanished → nothing to do.
    };
    // NO-LOSS: only ever delete a note whose BODY is whitespace-only. Any user content ⇒ KEEP it.
    if !companion_body_is_empty(&row.text) {
        return Ok(false);
    }
    let _org_mutation = state.org_share_mutation_lock.lock().await;
    // The first snapshot was taken before waiting for the async mutation authority. Re-establish
    // emptiness under lifecycle immediately before installing the durable close barrier: an editor
    // that won the wait must keep both its content and its shares.
    {
        let _lifecycle = lifecycle_guard(state);
        if !meeting_is_unlocked(state, meeting_id)?
            || state.db.companion_note_for_meeting(meeting_id)?.as_deref() != Some(&note_id)
        {
            return Ok(false);
        }
        let Some((folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(&note_id)?
        else {
            return Ok(false);
        };
        if !folder_is_unlocked(state, &folder_id)? {
            return Ok(false);
        }
        let Some(current) = state.db.get_note_row(&note_id)? else {
            return Ok(false);
        };
        if !companion_body_is_empty(&current.text) {
            return Ok(false);
        }
        // Source-content migration guards reject subsequent edits while the network revoke is in
        // flight. Do this in the same lifecycle critical section as the authoritative recheck.
        state.db.begin_org_source_closure("document", &note_id)?;
    }
    revoke_org_shares_for_source_notifying(state, None, Some(&note_id), app).await?;
    let deleted = delete_companion_after_revoke_if_still_empty(state, meeting_id, &note_id)?;
    if !deleted {
        // Once remote revoke began, a surprising source drift is not authority to reopen admission:
        // doing so would retain new local content after destroying its remote share. Keep the durable
        // barrier and surface a retryable failure for explicit recovery.
        return Err(AppError::Unavailable(
            "companion changed after share revocation began; cleanup remains safely closed".into(),
        ));
    }
    Ok(deleted)
}

fn delete_companion_after_revoke_if_still_empty(
    state: &AppState,
    meeting_id: &str,
    expected_note_id: &str,
) -> Result<bool, AppError> {
    let _lifecycle = lifecycle_guard(state);
    if !meeting_is_unlocked(state, meeting_id)? {
        return Err(AppError::Locked(
            "this meeting is locked — unlock it before cleaning up its note".into(),
        ));
    }
    if state.db.companion_note_for_meeting(meeting_id)?.as_deref() != Some(expected_note_id) {
        return Ok(false);
    }
    let Some((folder_id, _created_at, _updated_at)) =
        state.db.note_gate_anchor(expected_note_id)?
    else {
        return Ok(false);
    };
    if !folder_is_unlocked(state, &folder_id)? {
        return Ok(false);
    }
    let Some(row) = state.db.get_note_row(expected_note_id)? else {
        return Ok(false);
    };
    if !companion_body_is_empty(&row.text) {
        return Ok(false);
    }
    let owner = crate::storage::AttachmentOwner::Document {
        document_id: expected_note_id.to_string(),
    };
    let attachments = state.db.list_attachments(&owner)?;
    remove_attachment_exports(
        &attachments,
        "could not remove an exported image before deleting the empty companion note",
    )?;
    if let Some(path) = &row.exported_path {
        let _ = std::fs::remove_file(path);
    }
    bump_seal_epoch(state);
    state.db.delete_document(expected_note_id)?;
    tracing::info!(target: "notes", note_id = %expected_note_id, meeting_id = %meeting_id, "empty companion note deleted");
    Ok(true)
}

/// Best-effort: refresh the COMPANION note's managed title + its front-matter `meeting: "[[…]]"`
/// wikilink so the link/label stays correct when a meeting is (auto-)titled or renamed. A sync
/// failure NEVER fails the rename (the meeting title is already persisted). No-op when the meeting
/// has no companion note. Skips when the companion note's folder is sealed-not-unlocked (never write
/// plaintext behind a lock — the sync re-applies on the next unlock+append).
#[cfg(test)]
pub(crate) fn sync_companion_note_title_best_effort(state: &AppState, meeting_id: &str) {
    let embedder = crate::embed::active_persistence_embedder_if_available();
    sync_companion_note_title_best_effort_with(state, meeting_id, embedder.as_deref());
}

fn sync_companion_note_title_best_effort_with(
    state: &AppState,
    meeting_id: &str,
    embedder: Option<&dyn crate::embed::Embedder>,
) {
    if let Err(e) = sync_companion_note_title(state, meeting_id, embedder) {
        // ids only — never the title text.
        tracing::warn!(target: "notes", meeting_id = %meeting_id, error = %e, "companion note title sync failed (meeting title unaffected)");
    }
}

fn sync_companion_note_title(
    state: &AppState,
    meeting_id: &str,
    embedder: Option<&dyn crate::embed::Embedder>,
) -> Result<(), AppError> {
    let Some(note_id) = state.db.companion_note_for_meeting(meeting_id)? else {
        return Ok(()); // no companion note — nothing to sync.
    };
    let Some(meeting) = state.db.get_meeting(meeting_id)? else {
        return Ok(());
    };
    let meeting_name = meeting_display_name(meeting.title.as_deref());
    let Some(row) = visible_authored_note_row_snapshot(state, &note_id)? else {
        return Ok(());
    };
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
    update_note_doc_inner_with(state, &note_id, &meeting_name, &new_markdown, embedder)?;
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
fn masked_note_doc(id: &str, folder_id: &str, created_at: i64, updated_at: Option<i64>) -> NoteDoc {
    NoteDoc {
        id: id.to_string(),
        title: "🔒 Locked".into(),
        folder_id: folder_id.to_string(),
        markdown: String::new(),
        tags: Vec::new(),
        properties: std::collections::BTreeMap::new(),
        updated_at: updated_at.unwrap_or(created_at),
        created_at,
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
    let _lifecycle = lifecycle_guard(state);
    export_note_to_vault_under_lifecycle_authorized(state, id)
}

/// Vault export for a caller that already owns the non-reentrant lifecycle mutex. The content-free
/// anchor and gate precede the full row read, and the caller keeps relock serialized through image
/// plus Markdown publication.
fn export_note_to_vault_under_lifecycle_authorized(
    state: &AppState,
    id: &str,
) -> Result<Option<String>, AppError> {
    let Some((folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(id)? else {
        return Ok(None); // unknown/non-note id.
    };
    // GATE before any title/body/export-path read. The unseal path uses `write_note_to_vault`
    // directly because its authorization is the CK it just decrypted with.
    if !folder_is_unlocked(state, &folder_id)? {
        return Ok(None);
    }
    let Some(row) = state.db.get_note_row(id)? else {
        return Ok(None); // unknown id.
    };
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
    let owner = crate::storage::AttachmentOwner::Document {
        document_id: row.id.clone(),
    };
    let exported_markdown = render_markdown_with_attachments_for_export_under_lifecycle_authorized(
        state, &owner, &row.text, vault_root,
    )?;
    let path = crate::export::write_note(
        vault_root,
        Some(&subfolder),
        &title,
        &created_iso,
        &exported_markdown,
    )?;
    let path_str = path.to_string_lossy().to_string();
    state
        .db
        .set_note_doc_exported_path(&row.id, Some(&path_str))?;
    // Export-collision guard: stamp the baseline from the text this export wrote. `write_note`
    // never overwrites different content (it collision-suffixes), so the file at `path` is
    // byte-equal to `row.text` in every branch — including the unlock/remove-lock re-export,
    // where any pre-lock baseline is stale and must be re-stamped fresh.
    state.db.set_note_doc_exported_hash(
        &row.id,
        Some(&crate::export::note_content_hash(&exported_markdown)),
    )?;
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
    state.db.index_note_chunks(id, title, &body, embedder)
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
    let visibility = capture_content_visibility_snapshot(state.inner());
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

    let provider = crate::summarize::provider_for(
        crate::summarize::roles::Role::Notes,
        &config,
        &state.heavy_inference,
    )?;

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
        require_current_content_visibility_snapshot(state.inner(), visibility)?;
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
    require_current_content_visibility_snapshot(state.inner(), visibility)?;
    Ok(OrganizePlan { moves })
}

/// Apply an auto-organize plan: per move, ensure the target note-folder exists (create it under the
/// Notes root when `toFolderId` is null), then MOVE the note (reusing the gated `move_note_doc_inner`
/// — both-sides folder gate + re-export). Non-destructive + best-effort per move (a single failure
/// logs IDs/stage and continues; the rest still apply). Idempotent on an already-filed note.
#[tauri::command]
pub async fn apply_organize_plan(
    app: AppHandle,
    state: State<'_, AppState>,
    plan: OrganizePlan,
) -> Result<(), AppError> {
    let _share_mutation = state.org_share_mutation_lock.lock().await;
    let may_reduce_visibility = plan.moves.iter().any(|mv| {
        mv.to_folder_id
            .as_deref()
            .and_then(|id| state.db.folder_by_id(id).ok().flatten())
            .is_some_and(|folder| folder.locked)
    });
    if may_reduce_visibility {
        emit_ask_history_invalidated_fail_closed(&app);
    }
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
    delete_meeting_inner_notifying(state.inner(), &meeting_id, Some(&app)).await?;
    emit_ask_history_invalidated_fail_closed(&app);
    crate::events::emit_content_deleted(&app, "meeting", &meeting_id);
    // The delete purged its audit findings (id-matched) — ping the FE inbox (count-only).
    emit_audit_updated_after_purge(&app, state.inner());
    Ok(())
}

/// Inner of [`delete_meeting`] taking `&AppState` (unit-testable). `async` for the org-share revoke
/// cascade (network round-trip); the file/DB cascade itself stays synchronous internally.
#[cfg(test)]
pub(crate) async fn delete_meeting_inner(
    state: &AppState,
    meeting_id: &str,
) -> Result<(), AppError> {
    delete_meeting_inner_notifying(state, meeting_id, None).await
}

async fn delete_meeting_inner_notifying(
    state: &AppState,
    meeting_id: &str,
    app: Option<&AppHandle>,
) -> Result<(), AppError> {
    // Gate before the network revoke without selecting title/audio/note content. The await below can
    // span a relock, so this is only the early refusal; destructive authority is reacquired below.
    {
        let _lifecycle = lifecycle_guard(state);
        ensure_no_active_salvage_for_meeting(state, meeting_id)?;
        if state.db.get_meeting_gate_anchor(meeting_id)?.is_none() {
            return Ok(());
        }
        if !meeting_is_unlocked(state, meeting_id)? {
            return Err(AppError::Locked(
                "this meeting is locked — unlock it before deleting it".into(),
            ));
        }
    }
    let _org_mutation = state.org_share_mutation_lock.lock().await;
    state.db.begin_org_source_closure("meeting", meeting_id)?;
    // REVOKE-BEFORE-DELETE (Bug A root cause): tear down every LIVE org share of this exact meeting
    // BEFORE the local rows disappear, so the background org-sync tick can never re-pull a still-live
    // server item back into the local replica after the user asked to delete it. Fails LOUD: a revoke
    // failure (e.g. offline) aborts the delete rather than silently leaving a dangling live share.
    revoke_org_shares_for_source_notifying(state, Some(meeting_id), None, app).await?;

    // Serialize against Start/Stop/seal. The content-free generation preflight MUST happen before
    // the first unlink; Db::delete_meeting repeats it transactionally before row deletion.
    let _lifecycle = lifecycle_guard(state);
    ensure_no_active_salvage_for_meeting(state, meeting_id)?;
    if state.db.get_meeting_gate_anchor(meeting_id)?.is_none() {
        return Ok(());
    }
    if !meeting_is_unlocked(state, meeting_id)? {
        return Err(AppError::Locked(
            "this meeting was locked while revoking its shares — unlock it and retry deletion"
                .into(),
        ));
    }
    reconcile_released_generation_cleanup(state, meeting_id)?;
    if state
        .db
        .meeting_has_recording_recovery_ownership(meeting_id)?
    {
        return Err(AppError::Audio(
            "cannot delete a meeting while recording recovery owns its artifacts".into(),
        ));
    }

    let attachment_rows = state.db.attachments_for_meeting(meeting_id)?;
    remove_attachment_exports(
        &attachment_rows,
        "could not remove an exported image before deleting the meeting",
    )?;

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
    bump_seal_epoch(state);
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

/// Lock-critical variant: remove every recorded rollup export BEFORE the transaction purges the
/// rows that carry those paths. This ordering makes an unlink failure retryable and closes the
/// crash window where DB-first purge permanently forgot a plaintext vault file.
pub(crate) fn remove_rollup_exports_before_seal_purge(db: &Db) -> Result<(), AppError> {
    for rollup in db.list_memory_rollups()? {
        if let Some(path) = rollup.exported_path.as_deref() {
            crate::crypto::remove_file_verified_absent(
                std::path::Path::new(path),
                "remove memory-rollup export before sealed-content purge",
            )?;
        }
    }
    Ok(())
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
        let embedder = crate::embed::active_persistence_embedder_if_available();
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
    // The title is part of dashboard material/composite digests. Serialize this short canonical
    // write with history preflight+hydration so a durable dashboard conversation can never validate
    // the old title and then hydrate its old turns after the new title has landed.
    {
        let _lifecycle = lifecycle_guard(state);
        state.db.set_meeting_title(meeting_id, title)?;
    }
    // Keep the companion note's managed title + front-matter `[[Meeting]]` link in sync with the new
    // title. Best-effort — a sync failure NEVER fails the rename (the title is already persisted).
    sync_companion_note_title_best_effort_with(state, meeting_id, embedder);
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

pub(crate) fn resolved_ask_corpus_budget(config: &AppConfig) -> usize {
    let connection =
        crate::summarize::roles::provider_target(crate::summarize::roles::Role::Ask, config)
            .connection;
    crate::summarize::vault_context::budget_for(&connection)
}

/// Durable authorization captured for one Ask dispatch. The monotonic generation defeats
/// A->B->A ABA changes; the exact projection additionally fails closed if a writer mutates the
/// in-memory config without rotating the generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AskDispatchSnapshot {
    pub(crate) generation: i64,
    projection: String,
}

pub(crate) fn ask_dispatch_projection(config: &AppConfig) -> String {
    let provider =
        crate::summarize::roles::provider_target(crate::summarize::roles::Role::Ask, config);
    let reasoner =
        crate::summarize::roles::reasoner_target(crate::summarize::roles::Role::Ask, config);
    serde_json::json!({
        "provider": [provider.connection, provider.model, provider.effort],
        "reasoner": [reasoner.connection, reasoner.model, reasoner.effort],
        "effectiveModel": crate::summarize::effective_model_requested(&provider, config),
        "cloudConsent": config.cloud_egress_consented,
        "anthropicModel": config.anthropic_model,
        "ollama": [config.ollama_base_url, config.ollama_model],
        "gateway": [config.gateway_base_url, config.gateway_model],
        "claude": [config.claude_binary, config.claude_code_inherit_env.to_string()],
        "local": [
            config.brain_model_path.clone().unwrap_or_default(),
            config.brain_model_id.clone().unwrap_or_default(),
            config.brain_light_model_id.clone().unwrap_or_default(),
            config.brain_heavy_model_id.clone().unwrap_or_default(),
            config.brain_idle_timeout_secs.to_string(),
            config.brain_ready_timeout_secs.to_string(),
            config.brain_hard_cap_secs.to_string(),
        ],
        "retrieval": [
            config.embed_model_id.clone().unwrap_or_default(),
            config.semantic_search_enabled,
            config.user_memory_enabled,
            config.ask_jit_retrieval,
            config.brain_heavy_grammar_enabled,
            config.loop_transcript_compaction,
        ],
        "connectors": {
            "web": [config.web_search_enabled, config.web_search_consented],
            "jira": [config.jira_enabled, config.jira_consented],
            "jiraEndpoint": [config.jira_base_url, config.jira_email],
            "slack": [config.slack_enabled, config.slack_consented],
            "notion": [config.notion_enabled, config.notion_consented],
            "clickup": [config.clickup_enabled, config.clickup_consented],
            "clickupTeam": config.clickup_team_id,
        },
    })
    .to_string()
}

pub(crate) fn capture_ask_dispatch_snapshot_under_lifecycle(
    state: &AppState,
    config: &AppConfig,
) -> Result<AskDispatchSnapshot, AppError> {
    Ok(AskDispatchSnapshot {
        generation: state.db.ask_dispatch_generation()?,
        projection: ask_dispatch_projection(config),
    })
}

pub(crate) fn require_current_ask_dispatch_under_lifecycle(
    state: &AppState,
    expected: &AskDispatchSnapshot,
) -> Result<(), AppError> {
    let config = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    if state.db.ask_dispatch_generation()? != expected.generation
        || ask_dispatch_projection(&config) != expected.projection
    {
        return Err(AppError::Locked(
            "Ask provider changed while generating the answer".into(),
        ));
    }
    Ok(())
}

/// Grounded Q&A over a meeting's transcript ("chat with the meeting"). The configured
/// provider answers strictly from the transcript, explicitly pinned sources, and running history.
#[tauri::command]
pub async fn chat_meeting(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
    question: String,
    history: Vec<ChatTurn>,
    explicit_sources: Option<Vec<crate::storage::models::SourceRef>>,
    dashboard_id: Option<String>,
) -> Result<String, AppError> {
    if dashboard_id
        .as_deref()
        .is_some_and(|id| !id.trim().is_empty())
    {
        return Err(AppError::InvalidArg(
            "dashboard chat requires a persisted conversation".into(),
        ));
    }
    chat_meeting_inner(
        &app,
        state.inner(),
        meeting_id,
        question,
        history,
        explicit_sources,
        dashboard_id,
        None,
        None,
        None,
    )
    .await
}

// This is the single gated meeting-chat boundary. Its arguments deliberately keep the caller's
// content snapshot and dashboard witness adjacent to the user inputs so neither authorization
// token can be accidentally reconstructed after provider dispatch.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn chat_meeting_inner(
    app: &AppHandle,
    state: &AppState,
    meeting_id: String,
    question: String,
    history: Vec<ChatTurn>,
    explicit_sources: Option<Vec<crate::storage::models::SourceRef>>,
    dashboard_id: Option<String>,
    authoritative_snapshot: Option<MeetingContentSnapshot>,
    authoritative_dashboard: Option<dashboards::DashboardContextWitness>,
    authoritative_ask_dispatch: Option<AskDispatchSnapshot>,
) -> Result<String, AppError> {
    if question.trim().is_empty() {
        return Err(AppError::InvalidArg("question is empty".into()));
    }
    require_backend_owned_dashboard_history(
        authoritative_snapshot.is_some(),
        dashboard_id.as_deref(),
        &history,
    )?;
    let (inputs, config) = meeting_provider_inputs(
        state,
        &meeting_id,
        authoritative_snapshot,
        dashboard_id.as_deref(),
        explicit_sources.as_deref().unwrap_or_default(),
        &history,
        &question,
    )?;
    let dashboard_witness = inputs.dashboard.clone();
    if authoritative_ask_dispatch
        .as_ref()
        .is_some_and(|expected| expected != &inputs.ask_dispatch)
    {
        return Err(AppError::Locked(
            "Ask provider changed while preparing the answer".into(),
        ));
    }
    if let Some(expected) = authoritative_dashboard.as_ref() {
        if dashboard_witness.as_ref() != Some(expected) {
            return Err(AppError::Locked(
                "dashboard changed while preparing the answer".into(),
            ));
        }
    }
    // ASK role: meeting chat is a Q&A surface. With role keys absent this resolves to the same
    // default provider as before (the legacy chat path always ignored `brain_backend`).
    let config_for_dispatch = config.clone();
    let heavy_for_dispatch = state.heavy_inference.clone();
    let meeting_for_admission = meeting_id.clone();
    let inputs_for_admission = inputs.clone();
    let dashboard_id_for_admission = dashboard_id.clone();
    let sources_for_admission = explicit_sources.clone().unwrap_or_default();
    let history_for_admission = history.clone();
    let question_for_admission = question.clone();
    let dispatch_admission = crate::state::ContentDispatchAdmission::new(app, move |state| {
        require_meeting_dashboard_scope_under_lifecycle(
            state,
            &meeting_for_admission,
            &inputs_for_admission,
            dashboard_id_for_admission.as_deref(),
            &sources_for_admission,
            &history_for_admission,
            &question_for_admission,
        )
    });
    let answer = dispatch_admission
        .run(|| async {
            let provider = crate::summarize::provider_for(
                crate::summarize::roles::Role::Ask,
                &config_for_dispatch,
                &heavy_for_dispatch,
            )?;
            provider.complete(&inputs.system, &inputs.user).await
        })
        .await?;
    require_meeting_dashboard_scope_for_return(
        state,
        &meeting_id,
        &inputs,
        dashboard_id.as_deref(),
        explicit_sources.as_deref().unwrap_or_default(),
        &history,
        &question,
    )?;
    Ok(answer)
}

#[derive(Clone)]
struct MeetingProviderInputs {
    visibility: MeetingContentSnapshot,
    dashboard: Option<dashboards::DashboardContextWitness>,
    ask_dispatch: AskDispatchSnapshot,
    system: String,
    user: String,
    input_digest: String,
}

fn composed_provider_input_digest(system: &str, user: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(system.as_bytes());
    hasher.update([0]);
    hasher.update(user.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[allow(clippy::too_many_arguments)]
fn meeting_provider_inputs(
    state: &AppState,
    meeting_id: &str,
    authoritative_snapshot: Option<MeetingContentSnapshot>,
    dashboard_id: Option<&str>,
    additional_sources: &[crate::storage::models::SourceRef],
    history: &[ChatTurn],
    question: &str,
) -> Result<(MeetingProviderInputs, AppConfig), AppError> {
    let _lifecycle = lifecycle_guard(state);
    let config = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?
        .clone();
    let inputs = meeting_provider_inputs_under_lifecycle(
        state,
        meeting_id,
        authoritative_snapshot,
        &config,
        dashboard_id,
        additional_sources,
        history,
        question,
    )?;
    Ok((inputs, config))
}

#[allow(clippy::too_many_arguments)]
fn meeting_provider_inputs_under_lifecycle(
    state: &AppState,
    meeting_id: &str,
    authoritative_snapshot: Option<MeetingContentSnapshot>,
    config: &AppConfig,
    dashboard_id: Option<&str>,
    additional_sources: &[crate::storage::models::SourceRef],
    history: &[ChatTurn],
    question: &str,
) -> Result<MeetingProviderInputs, AppError> {
    let snapshot = match authoritative_snapshot {
        Some(snapshot) => {
            require_current_meeting_content_snapshot_under_lifecycle(state, meeting_id, &snapshot)?;
            snapshot
        }
        None => {
            if !meeting_is_unlocked(state, meeting_id)? {
                return Err(AppError::Locked(
                    "this meeting's folder is locked — unlock it and retry".into(),
                ));
            }
            MeetingContentSnapshot {
                folder_id: state.db.folder_for_meeting(meeting_id)?,
                visibility: ContentVisibilitySnapshot {
                    seal_epoch: state.seal_epoch.load(std::sync::atomic::Ordering::SeqCst),
                },
                active_related: active_related_witness(state, meeting_id)?,
            }
        }
    };
    let segments = state.db.get_segments(meeting_id)?;
    if segments.is_empty() {
        return Err(AppError::InvalidArg(
            "this meeting has no transcript to chat about yet".into(),
        ));
    }
    let transcript = segments
        .iter()
        .map(|segment| format!("[{:.0}s] {}", segment.start_s, segment.text.trim()))
        .collect::<Vec<_>>()
        .join("\n");
    let unlocked = unlocked_snapshot(state)?;
    let composite = match dashboard_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => {
            let ask_conn = crate::summarize::roles::provider_target(
                crate::summarize::roles::Role::Ask,
                config,
            )
            .connection;
            let budget = crate::summarize::vault_context::budget_for(&ask_conn)
                .min(crate::summarize::chat::MAX_PINNED_SOURCE_CHARS);
            Some(dashboards::dashboard_composite_context(
                &state.db,
                id,
                &unlocked,
                budget,
                additional_sources,
                Some(meeting_id),
            )?)
        }
        None => None,
    };
    let pinned_sources = if let Some(context) = composite.as_ref() {
        context.packed_corpus.clone()
    } else if additional_sources.is_empty() {
        String::new()
    } else {
        let ask_conn =
            crate::summarize::roles::provider_target(crate::summarize::roles::Role::Ask, config)
                .connection;
        pack_chat_pinned_sources(
            &state.db,
            meeting_id,
            additional_sources,
            &ask_conn,
            &unlocked,
        )?
    };
    let memory_brief = if composite.is_some() {
        String::new()
    } else {
        gated_memory_brief_for_injection(state, &unlocked, question)
    };
    let (system, user) = crate::summarize::chat::build_with_composite_sources(
        &transcript,
        &pinned_sources,
        history,
        question,
        &memory_brief,
        composite.is_some(),
    );
    let input_digest = composed_provider_input_digest(&system, &user);
    Ok(MeetingProviderInputs {
        visibility: snapshot,
        dashboard: composite.map(|context| context.witness),
        ask_dispatch: capture_ask_dispatch_snapshot_under_lifecycle(state, config)?,
        system,
        user,
        input_digest,
    })
}

#[allow(clippy::too_many_arguments)]
fn require_meeting_dashboard_scope_under_lifecycle(
    state: &AppState,
    meeting_id: &str,
    expected: &MeetingProviderInputs,
    dashboard_id: Option<&str>,
    additional_sources: &[crate::storage::models::SourceRef],
    history: &[ChatTurn],
    question: &str,
) -> Result<(), AppError> {
    let config = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?
        .clone();
    let current = meeting_provider_inputs_under_lifecycle(
        state,
        meeting_id,
        Some(expected.visibility.clone()),
        &config,
        dashboard_id,
        additional_sources,
        history,
        question,
    )?;
    if current.dashboard != expected.dashboard
        || current.ask_dispatch != expected.ask_dispatch
        || current.input_digest != expected.input_digest
    {
        return Err(AppError::Locked(
            "meeting or dashboard context changed while generating the answer".into(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn require_meeting_dashboard_scope_for_return(
    state: &AppState,
    meeting_id: &str,
    expected: &MeetingProviderInputs,
    dashboard_id: Option<&str>,
    additional_sources: &[crate::storage::models::SourceRef],
    history: &[ChatTurn],
    question: &str,
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    require_meeting_dashboard_scope_under_lifecycle(
        state,
        meeting_id,
        expected,
        dashboard_id,
        additional_sources,
        history,
        question,
    )
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
    let visibility = capture_meeting_content_snapshot(state.inner(), &meeting_id)?;
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
    let provider = crate::summarize::provider_for(
        crate::summarize::roles::Role::Notes,
        &config,
        &state.heavy_inference,
    )?;
    let (system, user) =
        crate::summarize::recipes::build_recipe_prompt(&transcript, &prompt, &config.note_language);
    let artifact = provider.complete(&system, &user).await?;
    require_current_meeting_content_snapshot(state.inner(), &meeting_id, &visibility)?;
    Ok(artifact)
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
pub(crate) async fn build_and_persist_entities(
    app: &AppHandle,
    state: &AppState,
    meeting_id: &str,
    title: &str,
    markdown: &str,
    recording_model_token: Option<crate::perf::RecordingSessionToken>,
) -> Result<crate::summarize::graph::GraphPayload, AppError> {
    let visibility = capture_meeting_content_snapshot(state, meeting_id)?;
    // COMPANION NOTE title sync (2026-07-16): every pipeline finish path calls this AFTER the
    // meeting's final title is persisted (auto-title-on-close via `set_meeting_title`), so this is
    // the one funnel to refresh a companion note's managed title + `[[Meeting]]` front-matter link
    // to the final title. Best-effort — never fails the graph/note (which already succeeded).
    // Keep this async orchestration model-free. In recording Postprocess an unscoped embedder is
    // correctly refused by the session token; outside recording it would run synchronous Metal on
    // the async worker and could select a second model after the pipeline's meeting index. The
    // `None` path still clean-replaces companion chunks + FTS and PURGES stale vectors; the bounded
    // repair tick fills real vectors later.
    sync_companion_note_title_best_effort_with(state, meeting_id, None);
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    let provider = match recording_model_token.clone() {
        Some(token) => crate::summarize::provider_for_recording(
            crate::summarize::roles::Role::Notes,
            &config,
            &state.heavy_inference,
            token,
        )?,
        None => crate::summarize::provider_for(
            crate::summarize::roles::Role::Notes,
            &config,
            &state.heavy_inference,
        )?,
    };
    let entities_started = std::time::Instant::now();
    let payload =
        crate::summarize::graph::extract_entities(provider.as_ref(), title, markdown).await?;
    // The glossary never enters this existing graph-provider call. Canonicalize its response
    // locally, before the first DB entity/mention, fact reference, or vault-stub write.
    let payload = crate::summarize::graph::canonicalize_with_glossary(payload, &config.glossary);
    tracing::info!(
        target: "perf",
        stage = "extract_entities",
        elapsed_ms = entities_started.elapsed().as_millis() as u64,
        "pipeline stage complete"
    );

    // Sink A — ALWAYS persist to the encrypted DB (the graph's source of truth). Collect the
    // resolved (entity_id, name) pairs so the bitemporal-facts pass below can extract + reconcile
    // facts ABOUT these very entities.
    let lifecycle = lifecycle_guard(state);
    require_current_meeting_content_snapshot_under_lifecycle(state, meeting_id, &visibility)?;
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
    drop(lifecycle);

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
    let facts_model_token = recording_model_token;
    let facts_visibility = visibility.clone();
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
            facts_model_token.as_ref(),
            &facts_visibility,
        ) {
            tracing::warn!(target: "facts", error = %e, "fact reconcile failed (note unaffected)");
        }
        tracing::info!(target: "perf", stage = "persist_facts", elapsed_ms = t0.elapsed().as_millis() as u64, "pipeline stage complete");
        let t1 = std::time::Instant::now();
        if let Err(e) =
            persist_user_facts_for_meeting(
                &state,
                &meeting_id_owned,
                &title_owned,
                &markdown_owned,
                facts_model_token.as_ref(),
                &facts_visibility,
            )
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

    let _lifecycle = lifecycle_guard(state);
    require_current_meeting_content_snapshot_under_lifecycle(state, meeting_id, &visibility)?;
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
    index_wikilinks_best_effort_under_lifecycle(
        state,
        &_lifecycle,
        crate::links::LinkKind::Meeting,
        meeting_id,
        markdown,
    );
    auto_link_semantic_best_effort_under_lifecycle(
        state,
        &_lifecycle,
        crate::links::LinkKind::Meeting,
        meeting_id,
    );

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
pub async fn run_vault_audit(app: AppHandle) -> Result<crate::audit::AuditRunSummary, AppError> {
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
    let visibility = capture_content_visibility_snapshot(state);
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
        _ => state
            .db
            .note_markdown_if_visible(&row.source_id, &unlocked)?,
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
    let _lifecycle = lifecycle_guard(state);
    require_current_content_visibility_snapshot_under_lifecycle(state, visibility)?;
    let unlocked = unlocked_snapshot(state)?;
    if !audit_row_visible(state, &row, &unlocked)? {
        return Err(AppError::Locked(
            "this finding's source was locked while the explanation was generated".into(),
        ));
    }
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
    let lifecycle = lifecycle_guard(state);
    index_wikilinks_best_effort_under_lifecycle(state, &lifecycle, src_kind, src_id, body);
}

/// Same hook for callers that already own the lifecycle barrier. Keeping the guard proof in the
/// signature prevents the non-reentrant mutex from being acquired twice while still ensuring link
/// membership cannot change across an exact dashboard-history admission.
fn index_wikilinks_best_effort_under_lifecycle(
    state: &AppState,
    _lifecycle: &std::sync::MutexGuard<'_, ()>,
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
    // Link membership contributes to the exact dashboard corpus. Resolution + replacement are
    // short synchronous DB work and must share the lifecycle barrier with history admission.
    let result = state
        .db
        .index_wikilinks_for_source(src_kind, src_id, body, &unlocked);
    if let Err(e) = result {
        tracing::warn!(target: "links", error = %e, "wikilink index failed (text saved)");
    }
}

/// Brain v3 PR-3 — SEMANTIC AUTO-LINK hook, called AFTER a successful REAL-embedder index of an item
/// (never on the stub — model-gated at the CALL SITE, mirroring the chunk-index gate). Suggests up to
/// `SEMANTIC_LINK_CAP` content-similar neighbours (mutual-kNN / floor / cap; see `auto_link_semantic`).
/// BEST-EFFORT: a failure logs (counts only) and never fails the caller. O(k·log n) — no corpus scan.
fn auto_link_semantic_best_effort(state: &AppState, kind: crate::links::LinkKind, id: &str) {
    let lifecycle = lifecycle_guard(state);
    auto_link_semantic_best_effort_under_lifecycle(state, &lifecycle, kind, id);
}

/// Guard-owning twin of [`auto_link_semantic_best_effort`] for pipeline stages that already hold
/// the lifecycle barrier.
fn auto_link_semantic_best_effort_under_lifecycle(
    state: &AppState,
    _lifecycle: &std::sync::MutexGuard<'_, ()>,
    kind: crate::links::LinkKind,
    id: &str,
) {
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
    // The embedder/index build happened before this hook. Serialize the bounded kNN-derived link
    // replacement so an exact dashboard witness cannot be validated across a membership change.
    let result = state.db.auto_link_semantic(kind, id, &unlocked);
    if let Err(e) = result {
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
                        state
                            .db
                            .auto_link_semantic(crate::links::LinkKind::Meeting, &mid, unlocked)
                    {
                        tracing::warn!(target: "links", error = %e, "unlock semantic re-derive (meeting) failed");
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(target: "links", error = %e, "unlock link re-derive: meeting-id list failed")
        }
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
                        state
                            .db
                            .auto_link_semantic(crate::links::LinkKind::Note, &did, unlocked)
                    {
                        tracing::warn!(target: "links", error = %e, "unlock semantic re-derive (doc) failed");
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(target: "links", error = %e, "unlock link re-derive: document-id list failed")
        }
    }
    // Fix 2 (brain-v3 audit) — COMPANION leg: the note↔meeting `companion` edge is NOT re-derived by
    // the wikilink/semantic passes above (it comes from `documents.meeting_id`, not the body), is NOT
    // a preserved decision row, and is set only at the recording-time write site — so one lock cycle
    // permanently deletes it without this. Re-assert both legs (companion notes IN this folder → their
    // meetings, AND companion notes ANYWHERE → this folder's meetings, since a companion note can be
    // filed in a different folder). Best-effort; the DB helper skips any sealed-at-rest endpoint.
    match state.db.meeting_ids_in_folder(folder_id) {
        Ok(mids) => {
            if let Err(e) = state
                .db
                .rederive_companion_links_for_folder(folder_id, &mids)
            {
                tracing::warn!(target: "links", error = %e, "unlock companion re-derive failed");
            }
            // Fix 3 (brain-v3 audit) — INBOUND leg: re-index every OUTSIDE source whose body links
            // `[[title]]` INTO a just-unsealed item, so a link from a note that may never be edited
            // again is restored (the seal purged the edge from the sealed side; rederive only touched
            // F's OWN sources). Best-effort; Fix 0 keeps it from naming any still-sealed target.
            if let Err(e) = state
                .db
                .rederive_inbound_wikilinks_for_folder(folder_id, &mids, unlocked)
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
                Err(e) => {
                    tracing::warn!(target: "links", error = %e, "unlock accepted-marker re-materialize failed")
                }
            }
        }
        Err(e) => {
            tracing::warn!(target: "links", error = %e, "unlock companion re-derive: meeting-id list failed")
        }
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
// Same cohesive-surface exemption the two functions it delegates to already carry
// (`ask_vault_floor`, `build_ask_vault_floor_prompt`): these are one gated-Ask
// parameter set — question, history, thread identity, and the three mutually
// exclusive SCOPE pins (explicit sources / org item / board). Bundling them into a
// struct would hide which pins are exclusive without removing a single one.
#[allow(clippy::too_many_arguments)]
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
    // The BOARD this question was asked from. Present ⇒ its DERIVED tiles (promises, drift,
    // pulse, reminders, person, living answer) are rendered as a labelled brief and prepended to
    // the pinned corpus — the tiles the user is looking at, which `get_dashboard_sources`
    // deliberately never turns into `SourceRef`s because a drift lane is not a retrievable
    // document. An ID, never the finished text: the FE handing a string straight into a prompt
    // would be a new injection surface and would generate content outside the gate.
    // FE camelCase `dashboardId`. `None`/empty ⇒ byte-identical to before.
    dashboard_id: Option<String>,
) -> Result<AskVaultResult, AppError> {
    if dashboard_id
        .as_deref()
        .is_some_and(|id| !id.trim().is_empty())
    {
        return Err(AppError::InvalidArg(
            "dashboard Ask requires a persisted conversation".into(),
        ));
    }
    ask_vault_inner(
        &app,
        state.inner(),
        question,
        history,
        ask_thread_id,
        explicit_sources,
        pinned_org_item_id,
        dashboard_id,
        None,
        None,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn ask_vault_inner(
    app: &AppHandle,
    state: &AppState,
    question: String,
    history: Vec<ChatTurn>,
    ask_thread_id: Option<String>,
    explicit_sources: Option<Vec<crate::storage::models::SourceRef>>,
    pinned_org_item_id: Option<String>,
    dashboard_id: Option<String>,
    authoritative_snapshot: Option<DurableScopeSnapshot>,
    authoritative_dashboard: Option<dashboards::DashboardContextWitness>,
    authoritative_ask_dispatch: Option<AskDispatchSnapshot>,
) -> Result<AskVaultResult, AppError> {
    let durable_history = authoritative_snapshot.is_some();
    if question.trim().is_empty() {
        return Err(AppError::InvalidArg("question is empty".into()));
    }
    let visibility = authoritative_snapshot
        .unwrap_or_else(|| DurableScopeSnapshot::Vault(capture_content_visibility_snapshot(state)));
    let (config, ask_dispatch) = {
        let _lifecycle = lifecycle_guard(state);
        let config = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone();
        let dispatch = capture_ask_dispatch_snapshot_under_lifecycle(state, &config)?;
        (config, dispatch)
    };
    if authoritative_ask_dispatch
        .as_ref()
        .is_some_and(|expected| expected != &ask_dispatch)
    {
        return Err(AppError::Locked(
            "Ask provider changed while preparing the answer".into(),
        ));
    }
    // The same 12-message discipline as the chat panel (CHAT_CONTEXT_TURNS): bounds prompt growth +
    // cloud egress on BOTH paths. The LATEST question still drives retrieval either way.
    let mut history: Vec<ChatTurn> = capped_ask_history(&history).to_vec();

    // note↔meeting-links PR-2 — SOURCE-SCOPED (pinned) Ask. When the FE source picker sends a
    // non-empty explicit source list, retrieval is PINNED to exactly those items (+ their capped,
    // gated link-expansion) and answered DETERMINISTICALLY via the SAME floor answer path — the
    // agentic vault-wide search is SKIPPED (the user controls the context, so a scoped Ask never
    // pulls unlisted vault items). `None`/empty ⇒ this whole block is a no-op and the path below is
    // BYTE-IDENTICAL to before.
    let pinned_sources = explicit_sources.filter(|s| !s.is_empty());
    let pinned_org = pinned_org_item_id.filter(|s| !s.trim().is_empty());
    // A BOARD is a scope in its own right. Gating this on `pinned_sources` alone meant a
    // board whose tiles are ALL derived — a Promise ledger and nothing else, which is
    // precisely the case this exists for — produced an empty source list, missed the
    // floor entirely, and fell through to a vault-wide search with its board id silently
    // dropped. Review caught it; the motivating scenario was the one still broken.
    let board_id = dashboard_id.filter(|s| !s.trim().is_empty());
    require_backend_owned_dashboard_history(durable_history, board_id.as_deref(), &history)?;
    if board_id.is_some() {
        remove_duplicate_dashboard_question(&mut history, &question);
    }
    if pinned_sources.is_some() || pinned_org.is_some() || board_id.is_some() {
        // Snapshot AND resolve under ONE guard, exactly as `get_dashboard` does. Sharing
        // the caller's snapshot is not enough on its own: it prevents a second, later
        // snapshot but does not serialize against a relock landing between the snapshot
        // and `resolve_tile`. The later `require_current_content_visibility_snapshot`
        // cannot cover that window either — it runs AFTER the provider call, so it
        // suppresses the RESULT, not the disclosure. The guard is dropped before the
        // await below; it is held only for the reads it has to bracket.
        let (unlocked, composite, scoped_config) = dashboard_composite_floor_inputs_current(
            state,
            board_id.as_deref(),
            pinned_sources.as_deref().unwrap_or_default(),
        )?;
        let dashboard_witness = composite.as_ref().map(|context| context.witness.clone());
        if let Some(expected) = authoritative_dashboard.as_ref() {
            if dashboard_witness.as_ref() != Some(expected) {
                return Err(AppError::Locked(
                    "dashboard changed while preparing the answer".into(),
                ));
            }
        }
        // A dashboard is a closed composite scope. Cross-vault user-memory would silently widen it.
        let memory_brief = if composite.is_some() {
            String::new()
        } else {
            gated_memory_brief_for_injection(state, &unlocked, &question)
        };
        let dispatch_admission = durable_dispatch_admission(
            app,
            visibility.clone(),
            ask_dispatch.clone(),
            dashboard_witness.clone(),
        );
        let result = if let Some(context) = composite.as_ref() {
            if pinned_org.is_some() {
                return Err(AppError::InvalidArg(
                    "dashboard and organization scopes cannot be combined".into(),
                ));
            }
            ask_vault_prepacked_dashboard_authorized(
                context,
                &scoped_config,
                &question,
                &history,
                &state.heavy_inference,
                dispatch_admission.clone(),
            )
            .await?
        } else {
            let reranker = crate::rerank::active_reranker(
                state
                    .reasoner
                    .current_for(crate::summarize::roles::Role::Ask),
            );
            ask_vault_floor_authorized(
                &state.db,
                &scoped_config,
                &unlocked,
                &question,
                &history,
                &memory_brief,
                Some(reranker),
                &state.heavy_inference,
                pinned_sources,
                pinned_org,
                dispatch_admission.clone(),
            )
            .await?
        };
        require_durable_scope_for_dispatch_with_ask(
            state,
            &visibility,
            &ask_dispatch,
            dashboard_witness.as_ref(),
        )?;
        return Ok(result);
    }

    let dispatch_admission = durable_dispatch_admission(
        app,
        visibility.clone(),
        ask_dispatch.clone(),
        authoritative_dashboard.clone(),
    );

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
        let attempt_admission = dispatch_admission.clone();
        let attempt_config = config.clone();
        let attempt = tokio::task::spawn_blocking(move || {
            ask_vault_agentic_attempt(
                &handle,
                &q,
                &h,
                &thread_id,
                attempt_config,
                attempt_admission,
                durable_history,
            )
        })
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("ask task join failed: {e}")))??;
        if let Some(result) = attempt {
            require_durable_scope_for_dispatch_with_ask(
                state,
                &visibility,
                &ask_dispatch,
                authoritative_dashboard.as_ref(),
            )?;
            return Ok(result);
        }
    }

    // THE FLOOR — the pre-agentic behavior, unchanged (RED-first equivalence-tested).
    // Pass the LIVE session unlock set (E9): a folder the user has session-unlocked is included
    // again, while sealed-and-NOT-unlocked content stays excluded by the same visibility predicate.
    let unlocked = unlocked_snapshot(state)?;
    // Gated cross-meeting USER MEMORY brief (parity with the @brain loop): VISIBLE facts only under
    // this same unlock snapshot, empty when memory is disabled ⇒ the floor prompt is byte-identical.
    // L2.2: relevance-filtered against the question (full-list fallback on zero hits).
    let memory_brief = gated_memory_brief_for_injection(state, &unlocked, &question);
    // Brain v2 L1.4 — the Ask-only reranker: resolve from the LIVE Ask-role reasoner.
    // `active_reranker` degrades stub/cloud reasoners to the identity StubReranker (rerank is
    // strictly on-device — a cloud reasoner would turn each pointwise judgment into egress).
    let reranker = crate::rerank::active_reranker(
        state
            .reasoner
            .current_for(crate::summarize::roles::Role::Ask),
    );
    let result = ask_vault_floor_authorized(
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
        dispatch_admission,
    )
    .await?;
    require_durable_scope_for_dispatch_with_ask(
        state,
        &visibility,
        &ask_dispatch,
        authoritative_dashboard.as_ref(),
    )?;
    Ok(result)
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

/// Reject caller-supplied history for dashboard chat unless it came from the durable backend seam.
fn require_backend_owned_dashboard_history(
    durable_history: bool,
    dashboard_id: Option<&str>,
    history: &[ChatTurn],
) -> Result<(), AppError> {
    if !durable_history
        && dashboard_id.is_some_and(|id| !id.trim().is_empty())
        && !history.is_empty()
    {
        return Err(AppError::Locked(
            "dashboard conversations require backend-owned history".into(),
        ));
    }
    Ok(())
}

/// Resolve the complete board corpus and its unlock snapshot under one lifecycle interval. The
/// guard is released with the returned values before any provider `.await`.
#[cfg(test)]
pub(crate) fn dashboard_composite_floor_inputs(
    state: &AppState,
    config: &AppConfig,
    dashboard_id: Option<&str>,
    additional_sources: &[crate::storage::models::SourceRef],
) -> Result<
    (
        std::collections::HashSet<String>,
        Option<dashboards::DashboardCompositeContext>,
    ),
    AppError,
> {
    with_board_scoped_floor_inputs(state, |unlocked| {
        let Some(id) = dashboard_id.map(str::trim).filter(|id| !id.is_empty()) else {
            return Ok(None);
        };
        let ask_conn =
            crate::summarize::roles::provider_target(crate::summarize::roles::Role::Ask, config)
                .connection;
        let budget = crate::summarize::vault_context::budget_for(&ask_conn);
        dashboards::dashboard_composite_context(
            &state.db,
            id,
            unlocked,
            budget,
            additional_sources,
            None,
        )
        .map(Some)
    })
}

/// Production resolver for provider inputs: capture the config which determines the Ask budget
/// inside the same lifecycle interval as the dashboard witness. The returned config is the exact
/// provider selection used for dispatch after the guard is released.
fn dashboard_composite_floor_inputs_current(
    state: &AppState,
    dashboard_id: Option<&str>,
    additional_sources: &[crate::storage::models::SourceRef],
) -> Result<
    (
        std::collections::HashSet<String>,
        Option<dashboards::DashboardCompositeContext>,
        AppConfig,
    ),
    AppError,
> {
    let _lifecycle = lifecycle_guard(state);
    let config = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?
        .clone();
    let unlocked = unlocked_snapshot(state)?;
    let composite = match dashboard_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => {
            let ask_conn = crate::summarize::roles::provider_target(
                crate::summarize::roles::Role::Ask,
                &config,
            )
            .connection;
            Some(dashboards::dashboard_composite_context(
                &state.db,
                id,
                &unlocked,
                crate::summarize::vault_context::budget_for(&ask_conn),
                additional_sources,
                None,
            )?)
        }
        None => None,
    };
    Ok((unlocked, composite, config))
}

/// Execute a board-input resolver inside the same lifecycle interval as its unlock snapshot.
/// Kept separate so the serialization property has a deterministic two-thread oracle.
#[cfg(test)]
pub(crate) fn with_board_scoped_floor_inputs<T>(
    state: &AppState,
    resolve: impl FnOnce(&std::collections::HashSet<String>) -> Result<T, AppError>,
) -> Result<(std::collections::HashSet<String>, T), AppError> {
    let _lifecycle = lifecycle_guard(state);
    let unlocked = unlocked_snapshot(state)?;
    let resolved = resolve(&unlocked)?;
    Ok((unlocked, resolved))
}

// Same cohesive-surface exemption `ask_vault` and `build_ask_vault_floor_prompt` carry:
// one gated-Ask parameter set, whose three SCOPE pins are mutually exclusive. Bundling
// them into a struct would hide that exclusivity without removing an argument.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
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
    ask_vault_floor_core(
        db,
        config,
        unlocked,
        question,
        history,
        memory_brief,
        reranker,
        heavy,
        explicit_sources,
        pinned_org_item_id,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn ask_vault_floor_authorized(
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
    dispatch_admission: crate::state::ContentDispatchAdmission,
) -> Result<AskVaultResult, AppError> {
    ask_vault_floor_core(
        db,
        config,
        unlocked,
        question,
        history,
        memory_brief,
        reranker,
        heavy,
        explicit_sources,
        pinned_org_item_id,
        Some(dispatch_admission),
    )
    .await
}

pub(crate) async fn ask_vault_prepacked_dashboard_authorized(
    context: &dashboards::DashboardCompositeContext,
    config: &AppConfig,
    question: &str,
    history: &[ChatTurn],
    heavy: &std::sync::Arc<tokio::sync::Semaphore>,
    dispatch_admission: crate::state::ContentDispatchAdmission,
) -> Result<AskVaultResult, AppError> {
    if context.packed_corpus.trim().is_empty() {
        return Ok(AskVaultResult {
            answer: "Nothing on this board is readable right now — unlock its folders, or add \
                     tiles with content you can see."
                .to_string(),
            sources: Vec::new(),
            citations: Vec::new(),
        });
    }
    let config = config.clone();
    let heavy = heavy.clone();
    ask_vault_prepacked_dashboard_dispatch(
        context,
        question,
        history,
        dispatch_admission,
        move || crate::summarize::provider_for(crate::summarize::roles::Role::Ask, &config, &heavy),
    )
    .await
}

async fn ask_vault_prepacked_dashboard_dispatch<F>(
    context: &dashboards::DashboardCompositeContext,
    question: &str,
    history: &[ChatTurn],
    dispatch_admission: crate::state::ContentDispatchAdmission,
    provider_factory: F,
) -> Result<AskVaultResult, AppError>
where
    F: FnOnce() -> Result<
        std::sync::Arc<dyn crate::summarize::provider::SummarizerProvider>,
        AppError,
    >,
{
    let (system, user) = crate::summarize::vault_chat::build_for_dashboard(
        &context.packed_corpus,
        history,
        question,
    );
    let answer = dispatch_admission
        .run(|| async {
            let provider = provider_factory()?;
            provider.complete(&system, &user).await
        })
        .await?;
    Ok(AskVaultResult {
        answer,
        sources: context.packed_sources.clone(),
        citations: Vec::new(),
    })
}

#[allow(clippy::too_many_arguments)]
async fn ask_vault_floor_core(
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
    dispatch_admission: Option<crate::state::ContentDispatchAdmission>,
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
            let answer = match dispatch_admission {
                Some(admission) => {
                    let config = config.clone();
                    let heavy = heavy.clone();
                    admission
                        .run(|| async {
                            let provider = crate::summarize::provider_for(
                                crate::summarize::roles::Role::Ask,
                                &config,
                                &heavy,
                            )?;
                            provider.complete(&system, &user).await
                        })
                        .await?
                }
                None => {
                    let provider = crate::summarize::provider_for(
                        crate::summarize::roles::Role::Ask,
                        config,
                        heavy,
                    )?;
                    provider.complete(&system, &user).await?
                }
            };
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
    let visibility = capture_content_visibility_snapshot(state.inner());
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
    let provider = crate::summarize::provider_for(
        crate::summarize::roles::Role::Notes,
        &config,
        &state.heavy_inference,
    )?;
    let markdown = provider.complete(&system, &user).await?;
    require_current_content_visibility_snapshot(state.inner(), visibility)?;
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
///
/// Also carries the two DISPLAY-ONLY live-caption facts (`live_captions::dto_probe`, ONE
/// models-dir lookup): the `live_captions` readiness state — the same resolution `start_recording`
/// runs, so the recorder can render a calm "live captions are off" notice instead of that fact only
/// existing in a backend `warn!` — and `live_companion_pending`, the same decision `download_model`
/// makes, so the onboarding wizard discloses the extra transfer without duplicating the rule. Both
/// are device/disk probes (a few `is_file` checks), deliberately NOT part of the pure
/// `config_to_dto`.
#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> Result<AppConfigDto, AppError> {
    // Snapshot the config, then probe — the disk checks below must not hold the config mutex.
    let config: AppConfig = {
        let guard = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        guard.clone()
    };
    let mut dto = config_to_dto(&config);
    let (live_state, companion_pending) = live_captions::dto_probe(&config);
    dto.live_captions = live_state;
    dto.live_companion_pending = companion_pending;
    Ok(dto)
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

    // P1 (C9): the model-size provenance lives in its own settings row, so it is applied HERE
    // rather than through `dto_to_config`'s exhaustive `AppConfig` literal. `None` = PRESERVE (the
    // overwhelmingly common case — an ordinary settings save says nothing about provenance);
    // `Some(token)` sets it, and an unrecognised token is REFUSED before anything is persisted.
    //
    // VALIDATION happens here (before any write); the WRITE itself is deferred until after the
    // config it describes has been persisted. Writing it first meant a later failure — a poisoned
    // mutex, or `AppConfig::save` erroring — left the provenance row describing a `model_size` that
    // was never stored, i.e. Murmur claiming it picked a size the user had picked.
    let model_size_source = config.model_size_source.clone();
    if let Some(source) = model_size_source.as_deref() {
        crate::settings::validate_model_size_source(source)?;
    }

    // Provider-role selection determines the exact Ask packing budget. Serialize the durable
    // config write and cache replacement with dashboard/history preflight, dispatch polling, and
    // post-await CAS. Lock order is lifecycle -> config -> DB throughout this interval.
    let _lifecycle = lifecycle_guard(state);

    // Generic Settings cannot change `embed_model_id`: `dto_to_config` preserves it from this
    // mutex-protected cache, while the dedicated selector takes the model-selection write barrier
    // before taking the same mutex. Do not make an unrelated Settings save wait behind a minutes-
    // long reindex persistence handle.
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    let new_config = dto_to_config(config, &cache);
    if ask_dispatch_projection(&cache) != ask_dispatch_projection(&new_config) {
        // Rotate FIRST. AppConfig::save is row-wise; a later failure may over-invalidate, but no
        // partial provider/config mutation can retain the old dispatch authorization.
        state.db.advance_ask_dispatch_generation()?;
    }
    new_config.save(&state.db)?;
    *cache = new_config;

    // Only now that the config it describes is durable — and NON-FATALLY, matching the startup
    // backfill. The token was already validated above, so the only failures left are DB-level. If
    // one happens, the `model_size` the user asked for IS saved and the cache already reflects it;
    // returning `Err` here would report "save failed" for a save that succeeded, which is worse than
    // a provenance row that is one launch stale (the backfill re-runs at every launch and the row is
    // advisory — nothing branches on it today).
    if let Some(source) = model_size_source.as_deref() {
        if let Err(e) = crate::settings::set_model_size_source(&state.db, source) {
            tracing::warn!(target: "settings", error = %e, "model_size_source write failed after a successful config save");
        }
    }
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
        diarize_others: Some(c.diarize_others),
        voiceprint_enabled: Some(c.voiceprint_enabled),
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
        user_display_name: c.user_display_name.clone(),
        note_language: c.note_language.clone(),
        ground_summary: Some(c.ground_summary),
        glossary: Some(c.glossary.clone()),
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
        // Always carried OUT as an explicit value, so the FE round-trips a real boolean (only a
        // caller that predates the field ever omits it — see the `Option` rationale on the field).
        notion_enabled: Some(c.notion_enabled),
        // DISPLAY-ONLY out: lets the FE show "consented" status; the FE cannot set it back (preserved
        // in `dto_to_config`).
        notion_consented: c.notion_consented,
        clickup_enabled: Some(c.clickup_enabled),
        // DISPLAY-ONLY out: lets the FE show "consented" status; the FE cannot set it back (preserved
        // in `dto_to_config`).
        clickup_consented: c.clickup_consented,
        clickup_team_id: Some(c.clickup_team_id.clone()),
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
        // NOT settings: live-caption readiness + the pending-companion disclosure are disk probes,
        // filled in by `get_config` (the ONE FE-facing read) so this stays pure and every config
        // round-trip test is unaffected.
        live_captions: String::new(),
        live_companion_pending: false,
        // P1: `None` = PRESERVE. A read-modify-write round-trip through the settings screen must
        // never rewrite (or clear) the model-size provenance as a side effect.
        model_size_source: None,
    }
}

/// Build the persisted `AppConfig` from an incoming settings DTO, merged against the `current`
/// config. Every plain field comes from the DTO (the Settings UI is authoritative for them), but
/// the security-sensitive `cloud_egress_consented` is PRESERVED from `current` and never taken from
/// the DTO (BLK-4) — so a settings save can neither grant nor clear cloud-egress consent. The
/// dedicated `consent_to_cloud_egress` command is the only path that flips it.
/// Keep a proposed model id only if it passes `accept`; otherwise fall back to the stored value,
/// and to empty ("let the provider pick") when that is unusable too.
///
/// The fallback is validated as well: a config written before this boundary existed can already
/// hold a hostile id, and returning it unchecked would preserve exactly what the guard exists to
/// remove.
/// Whether this connection turns its model id into a CLI ARGUMENT, and therefore takes the strict
/// short-slug ceiling rather than the looser JSON-body one.
///
/// FAILS CLOSED. The listed connections are the ones PROVEN to send the id in a JSON request body
/// (`anthropic`, `ollama`, `gateway`) plus the two that run no external model at all (`local`,
/// `off`); everything else — including `""` and any connection added later — is treated as
/// argv-building and gets the strict predicate.
///
/// Written as a JSON-body allowlist for exactly that reason. The obvious spelling,
/// `matches!(connection, "claude_code" | "codex_cli")`, fails OPEN: a new CLI-backed provider, or a
/// typo, would silently inherit the 200-character ceiling and could store an id `model_args`
/// refuses at the wire while `effective_model_requested` still reports it to the egress ledger.
/// Being wrong in this direction costs a rejected long id; being wrong in the other costs a lying
/// ledger.
pub(crate) fn connection_builds_argv(connection: &str) -> bool {
    !matches!(
        connection,
        "anthropic" | "ollama" | "gateway" | "local" | "off"
    )
}

/// THE rule, in one place: a model id is validated against the connection that will SEND it.
///
/// Every model field routes through here — `provider_model` by `provider_id`, each role model by
/// its own role connection, and the per-connection fields by the connection they belong to. Stating
/// the rule and then hard-coding one field to a fixed predicate is how the last two rounds of
/// defects happened; there are no exceptions left to get wrong.
/// Resolve what a stored connection string MEANS before judging a model id against it.
///
/// An empty role connection is not an unknown connection — in this app it means "inherit the
/// default", and `roles::resolve` follows that to `provider_id`. Treating `""` as unknown made the
/// fail-closed default fire on inherited roles, so a legitimate 65–200 character Ollama id was
/// cleared even though the arm that would send it is Ollama. Fail-closed is right for a connection
/// nobody recognises; it is wrong for one whose meaning is defined.
pub(crate) fn effective_connection(connection: &str, config_provider_id: &str) -> String {
    let connection = connection.trim();
    if connection.is_empty() {
        return config_provider_id.trim().to_string();
    }
    connection.to_string()
}

pub(crate) fn model_predicate_for(connection: &str) -> fn(&str) -> bool {
    if connection_builds_argv(connection) {
        crate::summarize::provider::valid_model_id
    } else {
        crate::summarize::provider::valid_catalog_model_id
    }
}

/// Keep a proposed model id only if the target connection can actually send it.
///
/// When BOTH the proposal and the stored value fail, the answer is empty — "let the provider pick".
/// That is a deliberate choice, not an oversight: switching from a JSON-body arm to a CLI arm can
/// strand a 65–200 character Hugging Face id that `claude_code` will never put on the wire, and the
/// only alternatives are worse. Keeping it means `effective_model_requested` reports a model to the
/// egress ledger that `model_args` silently drops — the ledger lie A6 exists to prevent. Rejecting
/// the whole save means an engine switch fails because of a field the user did not touch. Empty is
/// the one outcome where the stored value and the wire agree, and the provider's own default is a
/// working configuration rather than a broken one.
///
/// The loss is not silent at the layer that can speak: `AppConfig::load` logs it, and the Settings
/// UI keeps and explains an unlisted id rather than dropping it mid-edit
/// (`unlistedAfterEngineSwitch`).
fn sanitized_model(proposed: String, previous: &str, accept: fn(&str) -> bool) -> String {
    if proposed.trim().is_empty() || accept(&proposed) {
        proposed.trim().to_string()
    } else if accept(previous) {
        previous.trim().to_string()
    } else {
        String::new()
    }
}

fn dto_to_config(d: AppConfigDto, current: &AppConfig) -> AppConfig {
    // Normalize empty strings on optional fields to None so they round-trip cleanly.
    let norm = |s: Option<String>| s.filter(|v| !v.trim().is_empty());
    // The picker's catalog is a HINT, so `providerModel` and the per-role models are now free
    // text. Refuse a value that is not a plain model slug HERE, at the persistence boundary,
    // rather than dropping it later at the CLI.
    //
    // Dropping it later would make the EGRESS LEDGER LIE: `summarize::effective_model_requested`
    // reads the stored value independently of what `claude_code::model_args` actually puts on the
    // wire, so a rejected id would be RECORDED as requested while the provider silently ran its
    // own default. Refusing to persist it means the ledger and the wire cannot disagree — and the
    // CLI-side `valid_model_id` checks become defence in depth over an already-clean value.
    // The fallback is validated too. A config persisted BEFORE this boundary existed can already
    // hold a hostile id, and falling back to it unchecked would leave the very value this guard
    // exists to stop. When neither the proposal nor the stored value is a valid slug the answer is
    // empty — "let the provider pick", which is an explicit, supported choice.
    // ONE rule for every model field: validate against the connection that will SEND the id.
    // `claude_code` / `codex_cli` build `--model <id>` argv, where the short-slug ceiling is part of
    // the defence; `anthropic`, `ollama` and `gateway` put the id in a JSON body, where a long
    // Hugging Face path is legitimate. Every SHAPE refusal applies to both.
    let provider_conn = d.provider_id.clone();
    let notes_conn = d.role_notes_connection.clone();
    let ask_conn = d.role_ask_connection.clone();
    let live_conn = d.role_live_connection.clone();

    // Judge against the EFFECTIVE connection — the one that will actually send the id — because
    // that is what `roles::resolve` hands to the factory and what `effective_model_requested`
    // reports to the egress ledger. An inherited (empty) role connection resolves to `provider_id`.
    let for_connection = |proposed: String, previous: &str, connection: &str| -> String {
        let effective = effective_connection(connection, &provider_conn);
        sanitized_model(proposed, previous, model_predicate_for(&effective))
    };
    // A ROLE model, whose connection may be `""` = INHERIT.
    //
    // An inheriting role is judged by TRANSPORT SAFETY ONLY, never by the default engine's rule.
    // This used to resolve `""` to `provider_id` and apply that arm's predicate, on the belief that
    // an inherited role sends its own model. It does not: `roles::is_explicit` keys on the
    // connection key alone, so `resolve` goes to `legacy_default_target` and never reads
    // `role_*_model` at all. Judging an inert value by the CLI rule DESTROYED it — a legitimate
    // long Ollama id, kept deliberately by the UI on the way to Inherit, was blanked by the very
    // next autosave because the default engine happened to be `claude_code`. The UI promised
    // retention and the boundary broke the promise.
    //
    // The moment the role points at a real connection again, `for_connection` judges it there and
    // clears it if that arm cannot send it — with the row saying so.
    let for_role = |proposed: String, previous: &str, connection: &str| -> String {
        if connection.trim().is_empty() {
            return sanitized_model(
                proposed,
                previous,
                crate::summarize::provider::valid_catalog_model_id,
            );
        }
        sanitized_model(proposed, previous, model_predicate_for(connection.trim()))
    };
    // A role can point at ANY connection, so the predicate must follow the CONNECTION, not the
    // field. Applying the loose one uniformly was wrong: a role targeting `claude_code` could
    // store a 65–200 character id, which `model_args` then refuses at the wire — so the id would
    // sit in config and be reported by `effective_model_requested` to the egress ledger while
    // `--model` was silently omitted. That is the ledger-lies failure A6 exists to prevent,
    // reintroduced through a different door.
    // Snapshot the role connections BEFORE the struct literal: fields are evaluated in written
    // order, and each `role_*_connection` is moved out of the DTO above the matching `_model`.

    AppConfig {
        provider_id: d.provider_id,
        vault_path: norm(d.vault_path),
        vault_subfolder: norm(d.vault_subfolder),
        whisper_model_path: norm(d.whisper_model_path),
        language: norm(d.language),
        anthropic_model: for_connection(d.anthropic_model, &current.anthropic_model, "anthropic"),
        // Brain/AI model + effort ARE settable from the DTO (the Settings UI owns the pickers),
        // exactly like `anthropic_model` — plain strings, NOT preserve-only. `""` = provider default.
        //
        // Judged by `provider_id`, like every other model field, because that is the only arm that
        // ever reads it. This was UNCONDITIONALLY STRICT for several rounds, justified by "a role
        // with an explicit CLI connection but a blank model inherits `provider_model`". That claim
        // is FALSE, and `the_resolved_target_model_is_always_one_the_wire_will_send` now asserts so:
        // `roles::is_explicit` keys on the connection alone and `explicit_target` reads only that
        // role's own model key, so an explicit role never inherits this value. The one reader is
        // `legacy_default_target`, which returns it for `claude_code`/`codex_cli`/`anthropic` and
        // `""` for `ollama`/`gateway`.
        //
        // The over-broad rule was not merely redundant, it destroyed data: under `anthropic` — a
        // JSON-body arm — a legitimate 65–200 character vendor id was refused and the field reset,
        // which is exactly the A1/A2 violation this whole change exists to end.
        //
        // Switching engines stays safe because this runs on EVERY save with the DTO's NEW
        // `provider_id`: moving to an argv arm re-judges the stored id under the strict rule and
        // clears it there, so the ledger can never report a model the wire drops.
        provider_model: for_connection(d.provider_model, &current.provider_model, &provider_conn),
        provider_effort: d.provider_effort,
        ollama_base_url: d.ollama_base_url,
        ollama_model: for_connection(d.ollama_model, &current.ollama_model, "ollama"),
        claude_binary: d.claude_binary,
        input_device: norm(d.input_device),
        capture_system_audio: d.capture_system_audio,
        vad_enabled: d.vad_enabled,
        keep_hires_masters: d.keep_hires_masters,
        diarize_others: d.diarize_others.unwrap_or(current.diarize_others),
        voiceprint_enabled: d.voiceprint_enabled.unwrap_or(current.voiceprint_enabled),
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
        user_display_name: d.user_display_name.trim().to_string(),
        note_language: if d.note_language.trim().is_empty() {
            "auto".to_string()
        } else {
            d.note_language
        },
        ground_summary: d.ground_summary.unwrap_or(current.ground_summary),
        // Omission-safe update semantics: clients predating the glossary preserve the live value,
        // while an explicit empty string intentionally clears it.
        glossary: d.glossary.unwrap_or_else(|| current.glossary.clone()),
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
        // brain2 connectors: the Notion master toggle IS settable from the DTO (Settings owns it).
        // OMISSION-SAFE: an ABSENT key preserves the stored toggle (a caller that predates the field
        // cannot clear it); an explicit `false` still disables. Preserving can never ENABLE a
        // connector the user did not enable, so this stays fail-closed.
        notion_enabled: d.notion_enabled.unwrap_or(current.notion_enabled),
        // brain2 connectors (NEW EGRESS CLASS): consent is NEVER set from the DTO — preserved from the
        // live value (BLK-4 mirror). Only `consent_to_notion` may flip it, so a settings save can
        // neither grant nor clear Notion egress consent.
        notion_consented: current.notion_consented,
        // brain2 connectors: the ClickUp master toggle + non-secret workspace id ARE settable from
        // the DTO (Settings owns them), with the same omission-safe preserve as Notion.
        clickup_enabled: d.clickup_enabled.unwrap_or(current.clickup_enabled),
        // brain2 connectors (NEW EGRESS CLASS): consent is NEVER set from the DTO — preserved from the
        // live value. Only `consent_to_clickup` may flip it.
        clickup_consented: current.clickup_consented,
        clickup_team_id: d
            .clickup_team_id
            .unwrap_or_else(|| current.clickup_team_id.clone()),
        // Opt-in env inheritance for the `claude` CLI IS settable from the DTO (the Settings UI owns
        // the toggle). Default OFF on the DTO (`#[serde(default)]`), so a partial/older save can never
        // silently enable it. Even ON, the DB keys are never inherited (claude_code.rs `harden_env`).
        claude_code_inherit_env: d.claude_code_inherit_env,
        // AI Gateway fields ARE settable from the DTO (the Settings UI owns them). An omitted value
        // deserializes to `""` (`#[serde(default)]`), which is a valid "unset" state.
        gateway_base_url: d.gateway_base_url,
        gateway_model: for_connection(d.gateway_model, &current.gateway_model, "gateway"),
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
        role_notes_model: for_role(d.role_notes_model, &current.role_notes_model, &notes_conn),
        role_notes_effort: d.role_notes_effort,
        role_ask_connection: d.role_ask_connection,
        role_ask_model: for_role(d.role_ask_model, &current.role_ask_model, &ask_conn),
        role_ask_effort: d.role_ask_effort,
        role_live_connection: d.role_live_connection,
        role_live_model: for_role(d.role_live_model, &current.role_live_model, &live_conn),
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
    if republish_org_shares_for_source_notifying(state.inner(), Some(&meeting_id), None, &app)
        .await
        .unwrap_or(0)
        > 0
    {
        crate::events::emit_org_feed_updated(&app, 1);
    }
    ensure_post_await_result_visible(state.inner(), &result.meeting_id)?;
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

    ensure_post_await_result_visible(state.inner(), &result.meeting_id)?;

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
    // No retry while any recording lifecycle owns priority. Checking only the recorder slot misses
    // Starting (priority installed before capture) and Draining/Postprocess (slot already taken),
    // which would let recovery ASR race capture preparation or the live Stop pipeline.
    if crate::perf::recording_has_priority() {
        return Err(AppError::Audio(
            "a recording is in progress — stop it before retrying transcription".into(),
        ));
    }
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
    // Serialize the released-generation claim/cleanup/final reread with Stop, Delete and Lock.
    // The guard is intentionally acquired only after dropping the recorder mutex above.
    let _lifecycle = lifecycle_guard(state);
    // Lock gate: never re-pipeline (or decrypt) a sealed-and-not-session-unlocked meeting.
    if !meeting_is_unlocked(state, meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to retry transcription".into(),
        ));
    }
    reconcile_released_generation_cleanup(state, meeting_id)?;
    if state
        .db
        .meeting_has_recording_recovery_ownership(meeting_id)?
    {
        return Err(AppError::Storage(
            "this recording already has an active recovery owner — wait for recovery to finish"
                .into(),
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
    let path = meeting
        .audio_path
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| {
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

/// Retry one expired ARCHIVED cleanup in the current process, then let the caller reread the
/// nonterminal predicate. A filesystem/DB failure is returned honestly, but the helper releases
/// its claim so the next command attempt can retry without a restart.
pub(crate) fn reconcile_released_generation_cleanup(
    state: &AppState,
    meeting_id: &str,
) -> Result<(), AppError> {
    if !state
        .db
        .meeting_has_nonterminal_recording_generation(meeting_id)?
    {
        return Ok(());
    }
    crate::audio::source::resume_released_generation_cleanup_for_meeting(
        &state.db,
        &pipeline::recording_inflight_dir()?,
        &pipeline::audio_dir()?,
        meeting_id,
    )
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
    let visibility = capture_meeting_content_snapshot(state.inner(), &meeting_id)?;
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
    let provider = crate::summarize::provider_for(
        crate::summarize::roles::Role::Notes,
        &config,
        &state.heavy_inference,
    )?;
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
        if require_current_meeting_content_snapshot_under_lifecycle(
            state.inner(),
            &meeting_id,
            &visibility,
        )
        .is_err()
        {
            return Ok(MeetingTimeline::default());
        }
        // Fail-closed on a missing session KEK for a locked folder (never write unsealed plaintext).
        set_timeline_data_reseal_if_locked(state.inner(), &meeting_id, &json)?;
    }
    require_current_meeting_content_snapshot(state.inner(), &meeting_id, &visibility)?;
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
    let visibility = capture_meeting_content_snapshot(state.inner(), &meeting_id)?;
    let unlocked = unlocked_snapshot(state.inner())?;
    let emb = crate::embed::active_admitted_embedder();
    let hits = state
        .db
        .related_meetings_visible(&meeting_id, emb.as_ref(), 5, &unlocked)?;
    finish_related_meetings_result(state.inner(), &meeting_id, &visibility, hits)
}

/// Bind a derived related-meetings result to the exact source-meeting authorization generation.
/// Embedding and KNN search are synchronous but can still run long enough for a manual or
/// screen-share relock to revoke the source while its chunk plaintext remains cached in RAM.
pub(crate) fn finish_related_meetings_result(
    state: &AppState,
    meeting_id: &str,
    visibility: &MeetingContentSnapshot,
    hits: Vec<SearchHit>,
) -> Result<Vec<SearchHit>, AppError> {
    require_current_meeting_content_snapshot(state, meeting_id, visibility)?;
    Ok(hits)
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
    if model_present {
        // Establish one clean generation before the first REAL vector is written. Chunks/FTS and
        // canonical content remain intact; interruption can therefore leave B-or-missing rows but
        // never an A/B mixture queried in one vector space. The caller's persistence handle pins B
        // and owns the selection read guard through this whole function.
        db.invalidate_all_vector_embeddings()?;
    }

    // DOCUMENT backfill first — doc_chunks + the FTS index are model-INDEPENDENT (keyword retrieval
    // must work on a default install), so this runs even when the e5 model is absent. Visible
    // documents only (`visible_document_ids` applies `visibility_clause`; a sealed folder's docs
    // stay purged). Model present ⇒ full purge-then-reinsert re-embed (`force_reembed`). Model
    // ABSENT ⇒ chunk-only backfill of documents with NO chunks yet (the write-only legacy rows) —
    // never a purge-then-reinsert of an already-chunked document, which would DESTROY its existing
    // real vectors without replacing them. The shared leg is kind-ROUTED (Brain v3 audit gap #3):
    // authored notes re-chunk through the front-matter-stripping path, never raw `documents.text`.
    let docs_indexed =
        backfill_document_chunks(db, unlocked, model_present, embedder, true, None, None)?;
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

    // Org replicas live outside the folder-lock domain by design (org-disclosed, SQLCipher at
    // rest). Rebuild that fourth vector partition under the SAME pinned REAL handle; its helper
    // keyset-reads one item at a time and never changes feed cursors or canonical chunks/FTS.
    let org_indexed = org_commands::reindex_org_embeddings(db, embedder)?;

    tracing::info!(target: "rag", indexed, total, org_indexed, "reindex_embeddings complete");
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

fn index_document_row_kind_routed_background(
    db: &crate::storage::Db,
    document_id: &str,
    embedder: Option<&dyn crate::embed::Embedder>,
    epoch: u64,
) -> Result<bool, AppError> {
    match db.get_note_row(document_id)? {
        Some(row) => {
            let title = row.title.as_deref().unwrap_or(&row.name);
            let title = title.trim();
            let title = if title.is_empty() { "Untitled" } else { title };
            let (_yaml, body) = crate::storage::db::split_front_matter(&row.text);
            db.index_note_chunks_background(document_id, title, &body, embedder, epoch)
        }
        None => db.index_document_chunks_background(document_id, embedder, epoch),
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
    background_epoch: Option<u64>,
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
        let indexed = match background_epoch {
            Some(epoch) => index_document_row_kind_routed_background(db, &did, doc_embedder, epoch),
            None => index_document_row_kind_routed(db, &did, doc_embedder).map(|_| true),
        };
        match indexed {
            Ok(false) => return Ok(docs_indexed),
            Ok(true) => match repair_budget.as_deref_mut() {
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
#[cfg(test)]
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
        None,
    )
}

pub(crate) fn backfill_missing_brain_indexes_background(
    db: &crate::storage::Db,
    semantic_enabled: bool,
    model_present: bool,
    embedder: &dyn crate::embed::Embedder,
    epoch: u64,
) -> Result<(usize, usize), AppError> {
    backfill_missing_brain_indexes_capped(
        db,
        semantic_enabled,
        model_present,
        embedder,
        REPAIR_TICK_MAX_INDEX_PER_RUN,
        Some(epoch),
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
    background_epoch: Option<u64>,
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
        background_epoch,
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
                    let indexed = match background_epoch {
                        Some(epoch) => {
                            db.index_meeting_chunks_background(&m.id, &segments, embedder, epoch)
                        }
                        None => db
                            .index_meeting_chunks(&m.id, &segments, embedder)
                            .map(|_| true),
                    };
                    match indexed {
                        Err(e) => {
                            tracing::warn!(target: "rag", error = %e, "repair tick: indexing one meeting failed (skipped)");
                        }
                        Ok(false) => return Ok((meetings, docs)),
                        Ok(true) => {
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
                                    let topic_indexed = match background_epoch {
                                        Some(epoch) => db
                                            .index_meeting_topic_chunks_background(
                                                &m.id, &segments, embedder, &empty, epoch,
                                            )
                                            .map(|_| ()),
                                        None => db.index_meeting_topic_chunks(
                                            &m.id, &segments, embedder, &empty,
                                        ),
                                    };
                                    if let Err(e) = topic_indexed {
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
pub async fn move_note(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
    folder_id: Option<String>,
) -> Result<(), AppError> {
    let _share_mutation = state.org_share_mutation_lock.lock().await;
    move_note_command_body(&app, state.inner(), meeting_id, folder_id)
}

/// Body of [`move_note`], split so the audit-inbox ping fires once after EVERY successful move
/// (a move INTO a locked folder seals + purges ALL pending findings via
/// `purge_chunks_for_meetings`; an open-target move purges nothing — the count-only ping is
/// correct either way).
fn move_note_command_body(
    app: &AppHandle,
    state: &AppState,
    meeting_id: String,
    folder_id: Option<String>,
) -> Result<(), AppError> {
    let target_locked = match folder_id.as_deref() {
        Some(folder_id) => state
            .db
            .folder_by_id(folder_id)?
            .is_some_and(|folder| folder.locked),
        None => false,
    };
    if target_locked {
        emit_ask_history_invalidated_fail_closed(app);
    }
    move_note_inner_impl(state, meeting_id, folder_id)?;
    emit_audit_updated_after_purge(app, state);
    Ok(())
}

fn move_note_inner_impl(
    state: &AppState,
    meeting_id: String,
    folder_id: Option<String>,
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    // The salvage finalizer is folder/CK-bound. Reassignment while it is awaiting ASR/provider
    // work would either apply that key to the wrong folder or leave fresh plaintext ungoverned.
    ensure_no_active_salvage_for_meeting(state, &meeting_id)?;
    // Resolve current + target folder lock state.
    if !meeting_is_unlocked(state, &meeting_id)? {
        return Err(AppError::Locked(
            "the source folder is locked — unlock it before moving the note".into(),
        ));
    }
    let note = state.db.get_latest_note_for_meeting(&meeting_id)?;

    // The only production caller of this path is the user-initiated `move_note` command (the
    // recording pipeline files a meeting by writing its note row directly, never through here),
    // so the refusal surfaces to the person who asked for it and reaches nothing automated.
    //
    // A meeting's container lives on its NOTE rows — `meetings` has no folder column — so a meeting
    // that has produced no note yet (still recording, or a failed summarize) has nothing to carry the
    // assignment. The write would match zero rows and report success. Refuse instead, and say why:
    // such a meeting stays where it is until it has a note, which is the state it is already in.
    if folder_id.is_some() && note.is_none() {
        return Err(AppError::InvalidArg(
            "this meeting has no note yet — it can be filed once processing finishes".into(),
        ));
    }

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
    if let Some(folder_id) = folder_id.as_deref() {
        if state.db.org_folder_closure_exists(folder_id)? {
            return Err(AppError::Unavailable(
                "the destination folder is closing or locked for sharing; retry after reopening it"
                    .into(),
            ));
        }
    }

    // ── Target is a LOCKED folder: seal-or-reject (BLK-2) ───────────────────────────────────────
    if target_locked {
        let fid = folder_id.as_deref().ok_or_else(|| {
            AppError::Storage("locked meeting-folder target lost its folder id".into())
        })?;
        if state
            .db
            .source_has_active_remote_share(Some(&meeting_id), None)?
        {
            return Err(AppError::Unavailable(
                "revoke this meeting's shares before moving it into a locked folder".into(),
            ));
        }
        return move_into_locked_folder_under_lifecycle(state, &meeting_id, fid);
    }

    // ── Target is OPEN / root: existing reassign + best-effort FS move ──────────────────────────
    // The source folder's lock state: derive from the note's exported_path being present
    // (sealed notes have exported_path = NULL). If exported_path is None we treat the source as
    // "no movable file" and skip the FS move entirely.
    let exported = note.as_ref().and_then(|n| n.exported_path.clone());

    // Recover every attachment under the SOURCE gate before clearing a source-folder seal. Folder
    // reassignment + plaintext restore + blob clear then commit atomically, so a crash cannot leave
    // a stale source-CK blob attached to the open target.
    let attachment_rows = state.db.attachments_for_meeting(&meeting_id)?;
    let mut attachment_plaintext = std::collections::HashMap::with_capacity(attachment_rows.len());
    for attachment in &attachment_rows {
        attachment_plaintext.insert(
            attachment.id.clone(),
            plaintext_attachment_data(state, attachment)?,
        );
    }
    state.db.move_meeting_with_attachments_open(
        &meeting_id,
        folder_id.as_deref(),
        &attachment_plaintext,
    )?;

    // Best-effort FS move only when a plaintext .md exists (target is open here).
    if let Some(src_path) = exported {
        if let Some(vault) = vault_path(state) {
            let target_rel = match folder_id.as_deref() {
                Some(fid) => state.db.folder_by_id(fid)?.map(|f| f.path),
                None => None,
            };
            move_note_file(state, &meeting_id, &src_path, &vault, target_rel.as_deref())?;
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
    move_into_locked_folder_under_lifecycle(state, meeting_id, folder_id)
}

fn move_into_locked_folder_under_lifecycle(
    state: &AppState,
    meeting_id: &str,
    folder_id: &str,
) -> Result<(), AppError> {
    if !meeting_is_unlocked(state, meeting_id)? {
        return Err(AppError::Locked(
            "the source folder is locked — unlock it before moving the note".into(),
        ));
    }

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

    // A locked-folder association is also the boundary that declares every meeting-owned audio
    // artifact governed by this folder's CK. Resume an expired ARCHIVED cleanup first, then reread
    // fail-closed BEFORE assigning even one provider row. Active/FINALIZED/persistently ambiguous
    // plaintext therefore stays in its original open/root location; a transient cleanup failure
    // releases its targeted claim so the next same-process Move/auto-file attempt can retry.
    reconcile_released_generation_cleanup(state, meeting_id)?;
    if state
        .db
        .meeting_has_recording_recovery_ownership(meeting_id)?
    {
        return Err(AppError::Locked(
            "this meeting still has plaintext recording artifacts — finish recording recovery before moving it into a locked folder"
                .into(),
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

    // Prepare and verify every attachment seal under the TARGET CK before mutating ownership. Then
    // remove tracked plaintext exports before the transaction clears their retry metadata.
    let attachment_rows = state.db.attachments_for_meeting(meeting_id)?;
    let mut attachment_seals = std::collections::HashMap::with_capacity(attachment_rows.len());
    for attachment in &attachment_rows {
        let data = plaintext_attachment_data(state, attachment)?;
        let aad = attachment_aad(folder_id, &attachment.owner, &attachment.id);
        let blob = crate::crypto::encrypt(&ck, &data, &aad)?;
        if crate::crypto::decrypt(&ck, &blob, &aad)? != data {
            return Err(AppError::Storage(
                "attachment move-seal verification failed".into(),
            ));
        }
        attachment_seals.insert(attachment.id.clone(), blob);
    }
    remove_attachment_exports_before_move(&attachment_rows)?;

    // Reassign every provider row and install the verified image seals in the same transaction.
    bump_seal_epoch(state);
    state
        .db
        .move_meeting_with_attachments_sealed(meeting_id, folder_id, &attachment_seals)?;
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
    unseal_attachments_for_meeting(state, folder_id, meeting_id, &ck)?;
    // Re-index this one meeting so semantic search / related-meetings recover in-session (its note
    // markdown was just restored above). Model-gated (never a stub vector); best-effort — a re-index
    // hiccup must not fail (or half-undo) the completed move.
    let meeting_embedder = crate::embed::active_persistence_embedder_if_available();
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
    let _share_mutation = state.org_share_mutation_lock.try_lock().map_err(|_| {
        AppError::Unavailable(
            "sharing or folder privacy is changing; retry automatic filing".into(),
        )
    })?;
    if state
        .db
        .source_has_active_remote_share(Some(meeting_id), None)?
    {
        return Err(AppError::Unavailable(
            "revoke this meeting's shares before filing it into a locked folder".into(),
        ));
    }
    move_into_locked_folder(state, meeting_id, folder_id)
}

/// A narrowly-scoped, zeroizing folder key captured when a disk-salvage run starts while its
/// locked folder is session-unlocked. If screen-share/manual relock lands while Whisper is still
/// running, the global KEK is correctly zeroized, but this one-shot context lets the Drop finalizer
/// authenticate and seal the exact newly-produced transcript instead of deleting it or treating
/// audio re-transcription as an equivalent copy.
pub(crate) struct SalvageSealContext {
    folder_id: String,
    ck: Zeroizing<[u8; 32]>,
    /// Exact encrypted CK envelope observed with `ck`. Session relock leaves it unchanged; a
    /// permanent unlock/fresh lock rotates it. Equality is a defense-in-depth generation check
    /// immediately before any destructive seal write.
    wrapped_key: Vec<u8>,
}

/// Capture the exact folder CK before a long salvage run crosses any await. The lifecycle guard
/// makes the folder association, visibility gate and KEK unwrap one coherent snapshot. Open-folder
/// salvage needs no context; a locked folder that raced closed aborts before producing new output.
pub(crate) fn capture_salvage_seal_context(
    state: &AppState,
    meeting_id: &str,
) -> Result<Option<SalvageSealContext>, AppError> {
    let _lifecycle = lifecycle_guard(state);
    let Some(folder_id) = state.db.folder_for_meeting(meeting_id)? else {
        return Ok(None);
    };
    let Some(folder) = state.db.folder_by_id(&folder_id)? else {
        return Ok(None);
    };
    if !folder.locked {
        return Ok(None);
    }
    if !state
        .unlocked_folders
        .lock()
        .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
        .contains(&folder_id)
    {
        return Err(AppError::Locked(
            "the meeting folder relocked before transcription could start".into(),
        ));
    }
    let kek = state
        .master_kek
        .lock()
        .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?
        .clone()
        .ok_or_else(|| AppError::Locked("the unlocked folder has no cached session key".into()))?;
    let wrapped = state
        .db
        .folder_wrapped_key(&folder_id)?
        .ok_or_else(|| AppError::Storage("locked folder has no wrapped key".into()))?;
    let ck_bytes = Zeroizing::new(crate::crypto::decrypt(
        &kek,
        &wrapped,
        &aad_wrapped_ck(&folder_id),
    )?);
    let ck: [u8; 32] = ck_bytes
        .as_slice()
        .try_into()
        .map_err(|_| AppError::Storage("unwrapped content key has wrong length".into()))?;
    Ok(Some(SalvageSealContext {
        folder_id,
        ck: Zeroizing::new(ck),
        wrapped_key: wrapped,
    }))
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
///   had passed): use the entry-pinned, zeroizing CK to seal and decrypt-verify the exact fresh
///   transcript/timeline/audio. Audio is never treated as an equivalent transcript copy. If the
///   pinned context is unavailable or the seal fails, retain every byte behind the logical gate;
///   bounded SQLCipher-only residue is preferable to irreversible loss.
///
/// Best-effort by contract: every failure is logged (ids/counts only) and swallowed — the
/// finalizer must never mask the pipeline's own result.
#[cfg(test)]
pub(crate) fn finalize_salvage_lock_state(
    state: &AppState,
    meeting_id: &str,
    seal_context: Option<&SalvageSealContext>,
) {
    finalize_salvage_lock_state_with_notice(state, meeting_id, seal_context, || {});
}

pub(crate) fn finalize_salvage_lock_state_with_notice(
    state: &AppState,
    meeting_id: &str,
    seal_context: Option<&SalvageSealContext>,
    visibility_will_reduce: impl FnOnce(),
) {
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
    // From this point either seal path purges global-derived Ask history. Invalidate before the
    // best-effort finalizer can partially mutate and then return an error.
    visibility_will_reduce();
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
    let Some(context) = seal_context.filter(|ctx| ctx.folder_id == folder.id) else {
        tracing::error!(target: "lock", meeting_id = %meeting_id, "salvage raced a relock without its entry seal context; exact output retained behind the logical gate for recovery");
        return;
    };
    // Serialize with every lock transition, then revalidate the association before applying the
    // captured CK. Never seal under a key captured for a folder the meeting has since left.
    let _lifecycle = lifecycle_guard(state);
    let still_same_locked_folder = state
        .db
        .folder_for_meeting(meeting_id)
        .ok()
        .flatten()
        .is_some_and(|fid| fid == context.folder_id)
        && state
            .db
            .folder_by_id(&context.folder_id)
            .ok()
            .flatten()
            .is_some_and(|f| f.locked);
    if !still_same_locked_folder {
        tracing::error!(target: "lock", meeting_id = %meeting_id, "salvage folder changed before exact-output seal; output retained for recovery");
        return;
    }
    let same_key_generation = state
        .db
        .folder_wrapped_key(&context.folder_id)
        .ok()
        .flatten()
        .is_some_and(|wrapped| wrapped == context.wrapped_key);
    if !same_key_generation {
        tracing::error!(target: "lock", meeting_id = %meeting_id, "salvage folder key generation changed before exact-output seal; output retained without destructive writes");
        return;
    }
    bump_seal_epoch(state);
    if let Err(e) = seal_moved_note(state, &context.folder_id, meeting_id, &context.ck) {
        tracing::error!(target: "lock", meeting_id = %meeting_id, error = %e, "salvage exact-output seal after relock failed; output retained for recovery");
    } else {
        tracing::warn!(target: "lock", meeting_id = %meeting_id, "salvage raced a relock — exact output authenticated and sealed with the entry-pinned folder key");
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
    let mut exported_paths: Vec<(String, String)> = Vec::new();
    for n in &notes {
        if let Some(path) = n.exported_path.clone() {
            exported_paths.push((n.provider_id.clone(), path));
        }
        // Skip a row already sealed (blob present + markdown blanked) — idempotent.
        if let Some(blob) = n.content_blob.as_deref().filter(|_| n.markdown.is_empty()) {
            crate::crypto::decrypt(
                ck,
                blob,
                &aad_content(folder_id, meeting_id, &n.provider_id, "note"),
            )?;
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
    }
    ensure_no_external_edit_siblings(exported_paths.iter().map(|(_, path)| path))?;
    for (provider_id, path) in &exported_paths {
        let expected = state.db.get_note_exported_hash(meeting_id, provider_id)?;
        remove_note_export_if_unchanged(
            path,
            expected.as_deref(),
            "remove meeting-note export before sealing a moved note",
        )?;
    }
    for (provider_id, blob) in &sealed_rows {
        state.db.seal_note(meeting_id, provider_id, blob)?;
    }
    // Seal the moved meeting's transcript + timeline + audio under the SAME CK.
    seal_meeting_extras(&state.db, folder_id, meeting_id, ck)?;
    // Meeting-note exports were removed immediately before their row seal, while exported_path was
    // still durable cleanup authority. A retry can therefore never forget a failed plaintext unlink.
    // The note's chunks/vectors are plaintext-derived and a dense embedding is invertible, so they
    // must NOT survive at rest for a meeting now sealed into a locked folder — same invariant the
    // lock_folder / relock / startup-reconcile paths enforce. Covers both the manual move-into-locked
    // and the auto-file callers. (Re-indexed on unlock once indexing ships.) The same tx purges ALL
    // memory rollups (cross-meeting synthesis that may paraphrase the just-sealed facts) — remove
    // their exported vault `.md`s here, like the note `.md`s above.
    remove_rollup_exports_before_seal_purge(&state.db)?;
    let rollup_exports = state
        .db
        .purge_chunks_for_meetings(&[meeting_id.to_string()])?;
    remove_rollup_export_files(&rollup_exports);
    Ok(())
}

/// Move the exported `.md` to the target folder's vault subdir, preserving content. Re-points
/// the note's `exported_path`. Copy-then-remove so a failure never loses bytes.
fn move_note_file(
    state: &AppState,
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

/// RAII ownership for one from-disk salvage. The set entry is installed while holding the lock
/// lifecycle guard, so a key-changing folder operation can never slip between the salvage's
/// registration and its folder/CK snapshot. Drop removes only the opaque meeting id; the
/// [`SalvageLockFinalizer`] is declared after this permit and therefore runs first.
pub(crate) struct ActiveSalvagePermit<'a> {
    state: &'a AppState,
    meeting_id: String,
}

pub(crate) fn begin_active_salvage<'a>(
    state: &'a AppState,
    meeting_id: &str,
) -> Result<ActiveSalvagePermit<'a>, AppError> {
    let _lifecycle = lifecycle_guard(state);
    let mut active = state
        .active_salvages
        .lock()
        .map_err(|_| AppError::Storage("active-salvages mutex poisoned".into()))?;
    if !active.insert(meeting_id.to_string()) {
        return Err(AppError::Audio(
            "this meeting already has an active transcription recovery".into(),
        ));
    }
    Ok(ActiveSalvagePermit {
        state,
        meeting_id: meeting_id.to_string(),
    })
}

impl Drop for ActiveSalvagePermit<'_> {
    fn drop(&mut self) {
        let mut active = self
            .state
            .active_salvages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        active.remove(&self.meeting_id);
    }
}

/// Called only from a short [`lifecycle_guard`] critical section. Resolve folder membership from
/// the canonical DB at the time of the mutation rather than caching it in the permit: an internal
/// auto-file may legitimately assign the meeting while salvage is running.
pub(crate) fn ensure_no_active_salvage_in_folder(
    state: &AppState,
    folder_id: &str,
) -> Result<(), AppError> {
    let active: Vec<String> = state
        .active_salvages
        .lock()
        .map_err(|_| AppError::Storage("active-salvages mutex poisoned".into()))?
        .iter()
        .cloned()
        .collect();
    for meeting_id in active {
        if state.db.folder_for_meeting(&meeting_id)?.as_deref() == Some(folder_id) {
            return Err(AppError::Locked(
                "this folder has a transcription recovery in progress — wait for it to finish"
                    .into(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn ensure_no_active_salvage_for_meeting(
    state: &AppState,
    meeting_id: &str,
) -> Result<(), AppError> {
    if state
        .active_salvages
        .lock()
        .map_err(|_| AppError::Storage("active-salvages mutex poisoned".into()))?
        .contains(meeting_id)
    {
        return Err(AppError::Locked(
            "this meeting has a transcription recovery in progress — wait for it to finish".into(),
        ));
    }
    Ok(())
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

/// Refuse a seal while a collision-guard `<stem> (external edit …).md` sibling exists next to a
/// governed export. Murmur cannot silently delete user-authored bytes, but publishing `locked=1`
/// while leaving that known plaintext beside the sealed note is also unsafe. The folder therefore
/// stays open until the user explicitly reconciles the sibling. Paths/titles never enter the error
/// or logs.
pub(crate) fn ensure_no_external_edit_siblings<'a>(
    paths: impl Iterator<Item = &'a String>,
) -> Result<(), AppError> {
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
        return Err(AppError::Locked(format!(
            "cannot lock this folder while {siblings} preserved external-edit note file(s) remain; reconcile them in the vault first"
        )));
    }
    Ok(())
}

fn decode_sha256_hex(value: &str) -> Result<[u8; 32], AppError> {
    if value.len() != 64 {
        return Err(AppError::Storage(
            "managed note export has an invalid integrity baseline".into(),
        ));
    }
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        let start = index * 2;
        *byte = u8::from_str_radix(&value[start..start + 2], 16).map_err(|_| {
            AppError::Storage("managed note export has an invalid integrity baseline".into())
        })?;
    }
    Ok(out)
}

/// Preflight one governed Markdown export against its last Murmur-authored digest.
pub(crate) fn verify_note_export_unchanged(
    path: &str,
    expected_hash: Option<&str>,
    operation: &str,
) -> Result<(), AppError> {
    if !std::path::Path::new(path).exists() {
        return Ok(());
    }
    let expected = expected_hash.ok_or_else(|| {
        AppError::Locked(
            "cannot lock a legacy vault export without an integrity baseline; re-export the note first"
                .into(),
        )
    })?;
    let digest = decode_sha256_hex(expected)?;
    crate::crypto::verify_file_content(std::path::Path::new(path), None, &digest, operation)
}

/// Atomically quarantine, integrity-check and remove a governed Markdown export. A missing
/// baseline is fail-closed: legacy rows must be re-exported while open before they can be sealed,
/// otherwise Murmur cannot distinguish its own bytes from an external edit.
pub(crate) fn remove_note_export_if_unchanged(
    path: &str,
    expected_hash: Option<&str>,
    operation: &str,
) -> Result<(), AppError> {
    verify_note_export_unchanged(path, expected_hash, operation)?;
    if !std::path::Path::new(path).exists() {
        return Ok(());
    }
    let expected = expected_hash.ok_or_else(|| {
        AppError::Locked(
            "cannot lock a legacy vault export without an integrity baseline; re-export the note first"
                .into(),
        )
    })?;
    let digest = decode_sha256_hex(expected)?;
    crate::crypto::remove_file_verified_content(
        std::path::Path::new(path),
        None,
        &digest,
        operation,
    )
}

/// Remove every known plaintext vault replica of a folder selected from a lifecycle-consistent
/// session-unlocked snapshot. The caller owns the visibility ordering: single-folder manual relock
/// preflights while open, while emergency relock-all revokes gated reads first. Canonical bytes stay
/// in SQLCipher/sealed blobs, so a conflict is always recoverable.
pub(crate) fn prepare_folder_exports_before_relock(
    state: &AppState,
    folder_id: &str,
) -> Result<(), AppError> {
    let meeting_rows = state.db.meeting_note_export_rows_in_folder(folder_id)?;
    let note_rows = state.db.note_exported_path_rows_in_folder(folder_id)?;
    ensure_no_external_edit_siblings(
        meeting_rows
            .iter()
            .map(|(_, _, path, _)| path)
            .chain(note_rows.iter().map(|(_, path)| path)),
    )?;
    for (_, _, path, expected) in &meeting_rows {
        verify_note_export_unchanged(
            path,
            expected.as_deref(),
            "verify meeting-note export before relock",
        )?;
    }
    for (note_id, path) in &note_rows {
        let expected = state.db.get_note_doc_exported_hash(note_id)?;
        verify_note_export_unchanged(
            path,
            expected.as_deref(),
            "verify authored-note export before relock",
        )?;
    }
    let attachments = state.db.attachments_in_folder(folder_id)?;
    verify_attachment_exports(
        &attachments,
        "could not verify an exported image before relock",
    )?;

    for (_, _, path, expected) in &meeting_rows {
        remove_note_export_if_unchanged(
            path,
            expected.as_deref(),
            "remove meeting-note export before relock",
        )?;
    }
    for (note_id, path) in &note_rows {
        let expected = state.db.get_note_doc_exported_hash(note_id)?;
        remove_note_export_if_unchanged(
            path,
            expected.as_deref(),
            "remove authored-note export before relock",
        )?;
    }
    for (note_id, _) in note_rows {
        state.db.set_note_doc_exported_path(&note_id, None)?;
    }

    remove_attachment_exports(
        &attachments,
        "could not remove an exported image before relock",
    )?;
    for attachment in attachments {
        if attachment.exported_path.is_some() {
            state
                .db
                .set_attachment_exported_path(&attachment.id, None)?;
        }
    }
    Ok(())
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
/// AEC) would have to rely on their exact parent-owned stdin lifetime pipe and 4 h wall cap. A clean
/// exit finalizes + reaps the retained current-process children immediately. The next-launch
/// helper scan is detection-only and blocks overlap; it never guesses across PID generations.
///
/// The single `ActiveRecording` value is taken OUT of `AppState`; its component drops stop cpal,
/// terminate the system helper and close the durable spool. There are no split slots that can be
/// partially cleared or accidentally paired with another generation.
///
/// Panic-free: a POISONED slot mutex is skipped (`.lock().ok()`), a `stop()` error is ignored — this
/// is a last-chance exit hook with no `Result` to surface. Never touches the DB / lock model. NOTE:
/// call this ONLY on the true exit path, never on a mere window-hide (the app keeps recording in the
/// tray then — see `lib::relock_and_zeroize_on_lifecycle`).
pub(crate) fn stop_all_capture(state: &AppState) {
    let _active = state.recorder.lock().ok().and_then(|mut slot| slot.take());
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
pub(crate) fn aad_content(
    folder_id: &str,
    meeting_id: &str,
    provider_id: &str,
    record_type: &str,
) -> Vec<u8> {
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
    if path.ends_with(ENC_SUFFIX)
        || !crate::crypto::owned_regular_file_exists(
            std::path::Path::new(&path),
            "inspect plaintext audio before seal",
        )?
    {
        return Ok(None);
    }
    let enc_path = format!("{path}{ENC_SUFFIX}");
    crate::crypto::encrypt_file(
        ck,
        std::path::Path::new(&path),
        std::path::Path::new(&enc_path),
        aad,
    )?;
    crate::crypto::remove_file_verified_absent(
        std::path::Path::new(&path),
        "remove plaintext audio after verified seal",
    )?;
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

/// RE-BLANK (relock): re-seal any surviving plaintext under the retained session CK, then remove it
/// and re-point at the newly verified `.enc`. Re-encrypting instead of trusting mere `.enc`
/// existence prevents a corrupt/tampered ciphertext from becoming the only copy.
fn reblank_audio(
    ck: Option<&[u8; 32]>,
    path: Option<String>,
    aad: &[u8],
) -> Result<Option<String>, AppError> {
    let Some(path) = path else { return Ok(None) };
    if path.ends_with(ENC_SUFFIX) {
        // Crash window: decrypt publish succeeded but the DB setter did not. The row still points
        // at `.enc`, while its canonical plaintext sibling exists. Reblank must remove that sibling
        // too; treating the `.enc` pointer as an unconditional no-op strands plaintext forever.
        if !crate::crypto::owned_regular_file_exists(
            std::path::Path::new(&path),
            "verify encrypted audio during relock",
        )? {
            return Err(AppError::Storage(
                "sealed audio pointer has no encrypted artifact".into(),
            ));
        }
        let plain = path.trim_end_matches(ENC_SUFFIX).to_string();
        if crate::crypto::owned_regular_file_exists(
            std::path::Path::new(&plain),
            "inspect crash-window plaintext audio during relock",
        )? {
            let ck = ck.ok_or_else(|| {
                AppError::Locked(
                    "relock found crash-window plaintext audio without a session key".into(),
                )
            })?;
            return seal_audio_at_rest(ck, Some(plain), aad);
        }
        return Ok(None);
    }

    let plain_exists = crate::crypto::owned_regular_file_exists(
        std::path::Path::new(&path),
        "inspect plaintext audio during relock",
    )?;
    let enc_path = format!("{path}{ENC_SUFFIX}");
    if plain_exists {
        let ck = ck.ok_or_else(|| {
            AppError::Locked("relock found plaintext audio without a session key".into())
        })?;
        // Always regenerate+verify the sealed twin from this exact plaintext before deletion. An
        // older `.enc` may be corrupt; existence alone is not a loss-safety proof.
        return seal_audio_at_rest(ck, Some(path), aad);
    }
    if crate::crypto::owned_regular_file_exists(
        std::path::Path::new(&enc_path),
        "verify encrypted audio for dangling relock pointer",
    )? {
        return Ok(Some(enc_path));
    }
    Ok(None) // dangling DB pointer, but neither plaintext nor ciphertext exists.
}

/// PERMANENT-unseal preparation: durably decrypt `<file>.enc` → `<file>` but KEEP the `.enc` until
/// the caller has persisted the plaintext pointer and atomically flipped the folder open. This
/// ordering makes every crash recoverable: while the folder is still locked startup removes the
/// plaintext session copy; after the folder is open an orphan `.enc` is only redundant ciphertext.
/// Returns the plaintext path to persist (`None` if not sealed). `aads` is the role→role-less
/// decrypt ladder (see [`audio_decrypt_ladder`]) so a pre-role master still decrypts.
struct PermanentUnsealedAudio {
    plaintext_path: String,
    sealed_path: String,
}

fn permanent_unseal_audio(
    ck: &[u8; 32],
    stored_path: Option<String>,
    aads: &[&[u8]],
) -> Result<Option<PermanentUnsealedAudio>, AppError> {
    let Some(stored_path) = stored_path else {
        return Ok(None);
    };
    let (plain, sealed) = if stored_path.ends_with(ENC_SUFFIX) {
        (
            stored_path.trim_end_matches(ENC_SUFFIX).to_string(),
            stored_path,
        )
    } else {
        (stored_path.clone(), format!("{stored_path}{ENC_SUFFIX}"))
    };
    let sealed_path = std::path::Path::new(&sealed);
    if !crate::crypto::owned_regular_file_exists(
        sealed_path,
        "inspect retained encrypted audio during permanent unlock",
    )? {
        return Ok(None);
    }
    let plain_path = std::path::Path::new(&plain);
    if crate::crypto::owned_regular_file_exists(
        plain_path,
        "inspect plaintext audio during permanent unlock",
    )? {
        crate::crypto::verify_encrypted_file_matches_plaintext_multi(
            ck,
            sealed_path,
            plain_path,
            aads,
        )?;
    } else {
        crate::crypto::decrypt_file_multi(ck, sealed_path, plain_path, aads)?;
    }
    Ok(Some(PermanentUnsealedAudio {
        plaintext_path: plain,
        sealed_path: sealed,
    }))
}

/// SEAL every governed meeting's transcript + timeline under `ck`, then the audio WAV. Mirrors
/// `lock_folder`'s note seal: each blob is verified-decryptable BEFORE the plaintext is blanked /
/// the plaintext WAV is removed — content (transcript / audio) is never lost.
pub(crate) fn seal_folder_extras(db: &Db, folder_id: &str, ck: &[u8; 32]) -> Result<(), AppError> {
    seal_dashboards_in_folder(db, folder_id, ck)?;
    // TASKS LEAVE. A task's content lives in an org's E2EE store, so this folder's content key
    // cannot seal it — and a task left inside would stay exactly as readable as it was while the
    // user was told the folder is locked. Unfiling is the only outcome that keeps the lock's
    // promise literally true, and it loses nothing: the task is intact, unfiled, and still in the
    // Tasks view — the state it was in before anyone filed it.
    let unfiled = db.unfile_tasks_in_container(folder_id)?;
    if unfiled > 0 {
        tracing::info!(target: "lock", folder_id, unfiled, "unfiled tasks on seal");
    }
    let meeting_ids = db.meeting_ids_in_folder(folder_id)?;
    for mid in &meeting_ids {
        seal_meeting_extras(db, folder_id, mid, ck)?;
    }
    // Document ingestion: SEAL every uploaded document's text (USER-AUTHORED PRIMARY content,
    // SEALED-AND-RESTORED like the note markdown / typed notes — never lost). Encrypt the plaintext
    // under the folder CK, VERIFY it decrypts back byte-identical (verify-before-destroy), THEN blank
    // the plaintext. Done per FOLDER (documents anchor on the folder, not a meeting). An empty text ⇒
    // nothing to seal (blob stays NULL); an already-sealed document (blank text) is skipped.
    for d in db.raw_documents_in_folder(folder_id)? {
        if d.text.is_empty() {
            if let Some(blob) = &d.blob {
                // Idempotent repair must authenticate already-sealed rows too. Merely observing a
                // non-NULL blob is not proof that the only surviving copy is decryptable.
                crate::crypto::decrypt(ck, blob, &aad_document(folder_id, &d.id))?;
            }
            continue;
        }
        let aad = aad_document(folder_id, &d.id);
        let blob = crate::crypto::encrypt(ck, d.text.as_bytes(), &aad)?;
        if crate::crypto::decrypt(ck, &blob, &aad)? != d.text.as_bytes() {
            return Err(AppError::Storage(
                "document seal verification failed (blob mismatch)".into(),
            ));
        }
        db.seal_document(&d.id, &blob)?;
    }
    Ok(())
}

/// Authenticated startup completion for a folder whose durable `locked=1 + wrapped_key` marker
/// may have been published before every governed artifact finished sealing. This is deliberately
/// idempotent: surviving plaintext is freshly sealed under the recovered CK, existing ciphertext
/// is AEAD-verified, and no plaintext pathname is removed before a byte-valid replacement exists.
pub(crate) fn repair_locked_folder_at_rest(
    db: &Db,
    folder_id: &str,
    ck: &[u8; 32],
) -> Result<(), AppError> {
    // Attachments can carry both a session plaintext BLOB and a tracked plaintext vault export.
    // Authenticate every governed row before the first note export is deleted or any plaintext
    // column is blanked below. Missing ciphertext is repairable only when the exact canonical bytes
    // still exist in SQLCipher; a blob-less/blank row fails closed so an exported image is never
    // mistaken for a disposable duplicate.
    let attachments = db.attachments_in_folder(folder_id)?;
    let mut attachment_seals = Vec::new();
    for attachment in &attachments {
        if let Some(blob) =
            verify_or_prepare_attachment_relock_blob(ck, folder_id, attachment, true)?
        {
            attachment_seals.push((attachment.id.clone(), blob));
        }
    }
    if !attachment_seals.is_empty() {
        // Store only replacements already decrypt-verified against the exact SQLCipher plaintext.
        // `store_attachment_seals` does not blank `data`; later failures remain retryable.
        db.store_attachment_seals(&attachment_seals)?;
    }

    // Meeting-note exports must be removed while their recorded path still exists. `seal_note`
    // clears that cleanup authority, so delete/prove absent first on both initial seal and repair.
    for note in db.notes_in_folder(folder_id)? {
        let aad = aad_content(folder_id, &note.meeting_id, &note.provider_id, "note");
        let blob = if note.markdown.is_empty() {
            match note.content_blob {
                Some(blob) => {
                    crate::crypto::decrypt(ck, &blob, &aad)?;
                    blob
                }
                None => crate::crypto::encrypt(ck, b"", &aad)?,
            }
        } else {
            let blob = crate::crypto::encrypt(ck, note.markdown.as_bytes(), &aad)?;
            if crate::crypto::decrypt(ck, &blob, &aad)? != note.markdown.as_bytes() {
                return Err(AppError::Storage(
                    "startup note-seal verification failed".into(),
                ));
            }
            blob
        };
        if let Some(path) = note.exported_path.as_deref() {
            let expected = db.get_note_exported_hash(&note.meeting_id, &note.provider_id)?;
            remove_note_export_if_unchanged(
                path,
                expected.as_deref(),
                "remove meeting-note export during startup seal repair",
            )?;
        }
        db.seal_note(&note.meeting_id, &note.provider_id, &blob)?;
    }

    // Covers every transcript/timeline/manual-note/document row and every plaintext-pointing audio
    // column. Rows already sealed are skipped; crash-window plaintext is re-encrypted from source.
    seal_folder_extras(db, folder_id, ck)?;

    // Authored-note exports are a separate filesystem copy of `documents(kind='note').text`.
    // Delete/prove absent while each row still carries its cleanup path, then clear that one path.
    // A failure leaves the path durable and aborts startup before any content surface is exposed.
    for (document_id, path) in db.note_exported_path_rows_in_folder(folder_id)? {
        let expected = db.get_note_doc_exported_hash(&document_id)?;
        remove_note_export_if_unchanged(
            &path,
            expected.as_deref(),
            "remove authored-note export during startup seal repair",
        )?;
        db.set_note_doc_exported_path(&document_id, None)?;
    }

    for meeting_id in db.meeting_ids_in_folder(folder_id)? {
        let playback = db.get_meeting(&meeting_id)?.and_then(|m| m.audio_path);
        let (playback_role, playback_legacy) =
            audio_decrypt_ladder(&meeting_id, folder_id, StreamRole::Playback);
        if let Some(path) = repair_locked_audio_at_rest(
            ck,
            playback,
            &playback_role,
            &[&playback_role, &playback_legacy],
        )? {
            db.set_meeting_audio_path(&meeting_id, Some(&path))?;
        }

        let (mic, system) = db.get_meeting_master_paths(&meeting_id)?;
        let (mic_role, mic_legacy) = audio_decrypt_ladder(&meeting_id, folder_id, StreamRole::Mic);
        if let Some(path) =
            repair_locked_audio_at_rest(ck, mic, &mic_role, &[&mic_role, &mic_legacy])?
        {
            db.set_meeting_mic_master_path(&meeting_id, Some(&path))?;
        }
        let (system_role, system_legacy) =
            audio_decrypt_ladder(&meeting_id, folder_id, StreamRole::Sys);
        if let Some(path) =
            repair_locked_audio_at_rest(ck, system, &system_role, &[&system_role, &system_legacy])?
        {
            db.set_meeting_sys_master_path(&meeting_id, Some(&path))?;
        }
    }
    // Startup reconciliation checks the repair predicate before its global cleanup pass. Complete
    // the attachment half here now that every row was authenticated above: blank only rows with a
    // recoverable blob, then delete tracked exports with retry metadata preserved on failure.
    let attachment_exports = db.blank_attachments_in_folder(folder_id)?;
    delete_attachment_exports_with_retry(db, attachment_exports)?;
    Ok(())
}

/// Non-destructive predicate for the incomplete-seal/crash-unlock shapes that require the folder CK
/// before startup may re-blank or remove anything. It deliberately does not decrypt here, so a fully
/// sealed folder does not prompt for Touch ID on every launch; once any residue is found the repair
/// authenticates/reseals the whole folder before cleanup.
pub(crate) fn locked_folder_requires_authenticated_repair(
    db: &Db,
    folder_id: &str,
) -> Result<bool, AppError> {
    for note in db.notes_in_folder(folder_id)? {
        if !note.markdown.is_empty() || note.content_blob.is_none() || note.exported_path.is_some()
        {
            return Ok(true);
        }
    }
    if !db.note_exported_path_rows_in_folder(folder_id)?.is_empty() {
        return Ok(true);
    }
    if db
        .attachments_in_folder(folder_id)?
        .iter()
        .any(|attachment| {
            !attachment.data.is_empty()
                || attachment.data_blob.is_none()
                || attachment.exported_path.is_some()
        })
    {
        return Ok(true);
    }
    for document in db.raw_documents_in_folder(folder_id)? {
        // An empty blob-less document/note is a valid empty source; only surviving plaintext is a
        // repair marker here. Meeting-note rows above always carry a sealed blob, including empties.
        if !document.text.is_empty() {
            return Ok(true);
        }
    }
    for meeting_id in db.meeting_ids_in_folder(folder_id)? {
        if db
            .raw_segments(&meeting_id)?
            .iter()
            .any(|segment| !segment.text.is_empty() || segment.text_blob.is_none())
        {
            return Ok(true);
        }
        if db
            .raw_timeline(&meeting_id)?
            .is_some_and(|timeline| !timeline.data.is_empty() || timeline.data_blob.is_none())
        {
            return Ok(true);
        }
        if db
            .raw_manual_notes(&meeting_id)?
            .is_some_and(|notes| !notes.text.is_empty())
        {
            return Ok(true);
        }

        let playback = db
            .get_meeting(&meeting_id)?
            .and_then(|meeting| meeting.audio_path);
        let (mic, system) = db.get_meeting_master_paths(&meeting_id)?;
        for path in [playback, mic, system].into_iter().flatten() {
            if locked_audio_path_requires_repair(&path)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn locked_audio_path_requires_repair(path: &str) -> Result<bool, AppError> {
    if !path.ends_with(ENC_SUFFIX) {
        return Ok(true);
    }
    let encrypted = std::path::Path::new(path);
    if !crate::crypto::owned_regular_file_exists(
        encrypted,
        "inspect locked encrypted audio before startup repair",
    )? {
        return Ok(true);
    }
    let plain = std::path::Path::new(path.trim_end_matches(ENC_SUFFIX));
    crate::crypto::owned_regular_file_exists(
        plain,
        "inspect locked plaintext audio sibling before startup repair",
    )
}

fn repair_locked_audio_at_rest(
    ck: &[u8; 32],
    recorded_path: Option<String>,
    fresh_aad: &[u8],
    decrypt_aads: &[&[u8]],
) -> Result<Option<String>, AppError> {
    let Some(recorded_path) = recorded_path else {
        return Ok(None);
    };
    let (plain, encrypted) = if let Some(plain) = recorded_path.strip_suffix(ENC_SUFFIX) {
        (plain.to_string(), recorded_path)
    } else {
        (
            recorded_path.clone(),
            format!("{recorded_path}{ENC_SUFFIX}"),
        )
    };
    let plain_exists = crate::crypto::owned_regular_file_exists(
        std::path::Path::new(&plain),
        "inspect locked plaintext audio during startup repair",
    )?;
    if plain_exists {
        crate::crypto::encrypt_file(
            ck,
            std::path::Path::new(&plain),
            std::path::Path::new(&encrypted),
            fresh_aad,
        )?;
        crate::crypto::remove_file_verified_absent(
            std::path::Path::new(&plain),
            "remove plaintext audio after authenticated startup repair",
        )?;
        return Ok(Some(encrypted));
    }
    if !crate::crypto::owned_regular_file_exists(
        std::path::Path::new(&encrypted),
        "inspect encrypted audio during startup repair",
    )? {
        return Err(AppError::Storage(
            "locked audio has neither its plaintext nor encrypted artifact".into(),
        ));
    }
    crate::crypto::verify_encrypted_file_multi(ck, std::path::Path::new(&encrypted), decrypt_aads)?;
    Ok(Some(encrypted))
}

/// Seal ONE meeting's transcript + timeline + audio WAV under the folder CK (the per-meeting body of
/// [`seal_folder_extras`]). Reused by [`move_note`] to seal a note moved INTO a session-unlocked
/// locked folder (BLK-2) without touching the folder's other meetings. Verify-before-destroy
/// throughout (no transcript / audio loss); idempotent on already-sealed rows.
fn seal_meeting_extras(db: &Db, folder_id: &str, mid: &str, ck: &[u8; 32]) -> Result<(), AppError> {
    // Transcript: encrypt each segment's plaintext text, verify, then seal (blank text).
    let segs = db.raw_segments(mid)?;
    let mut sealed_segs: Vec<(i64, Vec<u8>)> = Vec::new();
    for s in &segs {
        // Skip rows already sealed (text_blob present, text blank) — idempotent.
        if s.text_blob.is_some() && s.text.is_empty() {
            crate::crypto::decrypt(
                ck,
                s.text_blob.as_deref().expect("checked above"),
                &aad_content(folder_id, mid, AAD_NO_PROVIDER, "segment"),
            )?;
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
        db.seal_segment(mid, *idx, blob)?;
    }

    // Timeline: encrypt the cached JSON (if any), verify, then seal (blank data).
    if let Some(tl) = db.raw_timeline(mid)? {
        if tl.data_blob.is_some() && tl.data.is_empty() {
            crate::crypto::decrypt(
                ck,
                tl.data_blob.as_deref().expect("checked above"),
                &aad_content(folder_id, mid, AAD_NO_PROVIDER, "timeline"),
            )?;
        } else {
            let aad = aad_content(folder_id, mid, AAD_NO_PROVIDER, "timeline");
            let blob = crate::crypto::encrypt(ck, tl.data.as_bytes(), &aad)?;
            if crate::crypto::decrypt(ck, &blob, &aad)? != tl.data.as_bytes() {
                return Err(AppError::Storage(
                    "timeline seal verification failed (blob mismatch)".into(),
                ));
            }
            db.seal_timeline(mid, &blob)?;
        }
    }

    // Audio at rest: the playback WAV + both masters, each encrypted → <file>.enc with
    // verify-before-destroy (inside encrypt_file), then the plaintext removed and the column
    // re-pointed at the .enc. Each blob is AAD-bound to (meeting|folder|STREAM-ROLE) so a sealed
    // audio file can't be swapped between contexts OR between the three streams of one meeting
    // (B7/B8 + stream-role hardening). The timeline was already sealed just above — do NOT re-seal it.
    if let Some(enc) = seal_audio_at_rest(
        ck,
        db.get_meeting(mid)?.and_then(|m| m.audio_path),
        &aad_audio_role(mid, folder_id, StreamRole::Playback),
    )? {
        db.set_meeting_audio_path(mid, Some(&enc))?;
    }
    let (mic, sys) = db.get_meeting_master_paths(mid)?;
    if let Some(enc) =
        seal_audio_at_rest(ck, mic, &aad_audio_role(mid, folder_id, StreamRole::Mic))?
    {
        db.set_meeting_mic_master_path(mid, Some(&enc))?;
    }
    if let Some(enc) =
        seal_audio_at_rest(ck, sys, &aad_audio_role(mid, folder_id, StreamRole::Sys))?
    {
        db.set_meeting_sys_master_path(mid, Some(&enc))?;
    }

    // brain2 realtime notes: SEAL the user's typed in-meeting notes (USER-AUTHORED PRIMARY content)
    // exactly like the timeline — encrypt the plaintext under the folder CK, VERIFY it decrypts back
    // byte-identical (verify-before-destroy), then blank the plaintext. NEVER blanked without the
    // sealed copy, and reversed by the matching unseal (session-unlock / remove-lock). An empty
    // buffer ⇒ nothing to seal (blob stays NULL); an already-sealed buffer (blank text) is skipped.
    if let Some(rn) = db.raw_manual_notes(mid)? {
        if !rn.text.is_empty() {
            let aad = aad_content(folder_id, mid, AAD_NO_PROVIDER, "manual_notes");
            let blob = crate::crypto::encrypt(ck, rn.text.as_bytes(), &aad)?;
            if crate::crypto::decrypt(ck, &blob, &aad)? != rn.text.as_bytes() {
                return Err(AppError::Storage(
                    "manual-notes seal verification failed (blob mismatch)".into(),
                ));
            }
            db.seal_manual_notes(mid, &blob)?;
        } else if let Some(blob) = &rn.blob {
            crate::crypto::decrypt(
                ck,
                blob,
                &aad_content(folder_id, mid, AAD_NO_PROVIDER, "manual_notes"),
            )?;
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

/// Seal every board filed in this folder: its title, and each tile's pointer + config.
///
/// A board is content twice over. Its title names the thing — "Q3 layoffs" is a disclosure on its
/// own — and its tiles say which meeting or note it is built from, which is precisely what a locked
/// folder exists to stop revealing. A container that could hold a board but never sealed one would
/// be a lock with a hole in it.
///
/// Verify-before-destroy throughout, exactly as the note and document seals do: encrypt, decrypt the
/// ciphertext back, compare byte-for-byte, and only then blank the plaintext.
fn seal_dashboards_in_folder(db: &Db, folder_id: &str, ck: &[u8; 32]) -> Result<(), AppError> {
    for board in db.dashboards_in_folder(folder_id)? {
        // Title, emoji AND tint, as one payload. The read path masks all three with the
        // rationale that "an emoji and an accent are weak signals, but they are still signals
        // about a thing the user asked to be unreadable" — and the seal used to cover only the
        // title, so those two sat in plaintext at rest. That is the gap the lock exists to close:
        // the promise is that a sealed folder is unreadable EVEN WITH THE DATABASE OPEN, and a
        // reader going straight to the row got the emoji and the accent every time. Masking on
        // read while leaving the columns at rest is protection against the app, not against an
        // attacker.
        // Whatever this condition covers, `Db::folder_has_plaintext_dashboards` must cover
        // too — it is the predicate the keyless relock refusal uses to decide whether anything
        // is still readable, and the two drifting apart is a silent leak, not a failed check.
        let cosmetics = serde_json::json!({
            "title": board.title,
            "emoji": board.emoji,
            "tint": board.tint,
        })
        .to_string();
        if !board.title.is_empty() || board.emoji.is_some() || board.tint.is_some() {
            let aad = aad_document(folder_id, &board.id);
            let blob = crate::crypto::encrypt(ck, cosmetics.as_bytes(), &aad)?;
            if crate::crypto::decrypt(ck, &blob, &aad)? != cosmetics.as_bytes() {
                return Err(AppError::Storage(
                    "dashboard title seal verification failed (blob mismatch)".into(),
                ));
            }
            db.seal_dashboard_title(&board.id, &blob)?;
        }
        for (tile_id, payload) in db.dashboard_tile_payloads(&board.id)? {
            let aad = aad_document(folder_id, &tile_id);
            let blob = crate::crypto::encrypt(ck, payload.as_bytes(), &aad)?;
            if crate::crypto::decrypt(ck, &blob, &aad)? != payload.as_bytes() {
                return Err(AppError::Storage(
                    "dashboard tile seal verification failed (blob mismatch)".into(),
                ));
            }
            db.seal_dashboard_tile(&tile_id, &blob)?;
        }
    }
    Ok(())
}

/// Restore every sealed board in this folder — the mirror of [`seal_dashboards_in_folder`].
///
/// A seal that cannot be undone is content loss, so this runs on both the session unlock and the
/// permanent one. It is idempotent: a board with no title blob is one that was never sealed (or was
/// already restored), and skipping it lets an interrupted unseal be retried without harm.
fn unseal_dashboards_in_folder(db: &Db, folder_id: &str, ck: &[u8; 32]) -> Result<(), AppError> {
    for (board_id, title_blob, tiles) in db.sealed_dashboard_blobs(folder_id)? {
        // The title and the tiles are restored INDEPENDENTLY. An earlier version skipped the
        // whole board when its title blob was absent, which is a real shape — a board titled
        // "" is never title-sealed — and it meant every sealed tile under such a board stayed
        // blanked forever. That is content loss, not a missed cosmetic.
        let cosmetics = match &title_blob {
            Some(blob) => {
                let bytes = crate::crypto::decrypt(ck, blob, &aad_document(folder_id, &board_id))?;
                let text = String::from_utf8(bytes).map_err(|_| {
                    AppError::Storage("sealed dashboard title is not UTF-8".into())
                })?;
                let parsed: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
                    AppError::Storage("sealed dashboard cosmetics are not valid JSON".into())
                })?;
                let field = |name: &str| {
                    parsed
                        .get(name)
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                };
                Some((
                    field("title").unwrap_or_default(),
                    field("emoji"),
                    field("tint"),
                ))
            }
            None => None,
        };
        let mut restored = Vec::with_capacity(tiles.len());
        for (tile_id, blob) in tiles {
            let bytes = crate::crypto::decrypt(ck, &blob, &aad_document(folder_id, &tile_id))?;
            let payload = String::from_utf8(bytes).map_err(|_| {
                AppError::Storage("sealed dashboard tile payload is not UTF-8".into())
            })?;
            let parsed: serde_json::Value = serde_json::from_str(&payload).map_err(|_| {
                AppError::Storage("sealed dashboard tile payload is not valid JSON".into())
            })?;
            // `None` for a JSON null, so a column that was NULL comes back NULL. There is no
            // older-payload case to defend against: `config_blob` and the `kind` field of this
            // payload were introduced together, so every blob that exists carries the key.
            let field = |name: &str| {
                parsed
                    .get(name)
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            };
            restored.push(crate::storage::dashboards_store::RestoredTile {
                id: tile_id,
                title: field("title"),
                ref_id: field("refId"),
                config: field("config"),
                kind: field("kind"),
            });
        }
        if cosmetics.is_none() && restored.is_empty() {
            continue;
        }
        db.unseal_dashboard(&board_id, cosmetics.as_ref(), &restored)?;
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
        for did in &restored_doc_ids {
            if let Err(e) = index_document_row_kind_routed(&state.db, did, meeting_embedder) {
                tracing::warn!(target: "rag", error = %e, "document re-index on unlock failed (text restored)");
            }
        }
    }
    unseal_attachments_in_folder(state, folder_id, ck, false)?;
    // MEETINGS: re-index the folder's meetings into note_chunks + vec_chunks so semantic /
    // related-meetings recover in-session (their note markdown was restored by `unlock_folder`
    // BEFORE this call). The caller supplies the model-gated `meeting_embedder` — never a stub
    // vector; mirrors the document re-embed above and the meeting half of `reindex_embeddings_inner`.
    reindex_meetings_after_unseal(state, &meeting_ids, meeting_embedder);

    // Boards LAST, deliberately. This used to run first, which made one undecryptable board blob
    // abort the whole unlock before a single meeting, note, document or audio file was restored —
    // a board is the least load-bearing kind in the folder, and it was gating access to every
    // other one. Running it last keeps the failure loud (the unlock still errors, nothing is
    // silently skipped) while everything that CAN be restored already has been.
    unseal_dashboards_in_folder(&state.db, folder_id, ck)?;

    Ok(())
}

/// Re-export every authored NOTE's vault `.md` only after the folder is either session-admitted or
/// durably open. The ordinary attachment owner gate therefore protects every byte written to disk.
/// Best-effort per note; a blank row is skipped inside [`write_note_to_vault`].
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

/// Drain the SQLCipher-backed lock-marker export outbox. Each exact vault path is scrubbed in place
/// (machine-owned block only), byte-verified and directory-synced before its rows are acknowledged.
/// External edits and user wikilinks outside the managed block survive; no collision sibling is
/// created because such a sibling would itself retain the sealed title. Path/title never enter logs.
pub(crate) fn drain_lock_marker_export_cleanup(db: &Db) -> Result<(), AppError> {
    let pending = db.pending_lock_marker_export_cleanup()?;
    if pending.is_empty() {
        return Ok(());
    }
    let vault_path = db
        .get_setting("vault_path")?
        .filter(|path| !path.is_empty())
        .ok_or_else(|| {
            AppError::Storage(
                "marker-export cleanup is pending but no configured vault root is available".into(),
            )
        })?;
    let vault = crate::export::MarkerCleanupVault::open(std::path::Path::new(&vault_path))?;
    let mut by_path: std::collections::BTreeMap<
        String,
        Vec<crate::storage::links::LockMarkerExportCleanup>,
    > = std::collections::BTreeMap::new();
    for row in pending {
        by_path
            .entry(row.exported_path.clone())
            .or_default()
            .push(row);
    }

    let mut drained = 0usize;
    for (path, rows) in by_path {
        let note_path = std::path::Path::new(&path);
        let titles: std::collections::HashSet<String> =
            rows.iter().map(|row| row.sealed_title.clone()).collect();
        let transform = |body: &str| {
            Db::strip_titles_from_managed_block(body, &titles).unwrap_or_else(|| body.to_string())
        };
        let mut final_body = None;
        let mut reconciled = false;
        // At most one recovery and one fresh publish are normally required. The third pass is a
        // bounded allowance for a crash-state rollback that preserved a concurrent vault edit.
        for _ in 0..3 {
            let publish = db.reserve_lock_marker_export_publish(&path)?;
            let note = vault.note(note_path, &publish.stage_name)?;
            if note.recover_marker_publish(
                db,
                &publish,
                crate::export::MAX_MARKER_CLEANUP_NOTE_BYTES,
                &transform,
            )? {
                continue;
            }
            let Some(snapshot) =
                note.read_owned_snapshot(crate::export::MAX_MARKER_CLEANUP_NOTE_BYTES)?
            else {
                note.sync_absent(crate::export::MAX_MARKER_CLEANUP_NOTE_BYTES, &transform)?;
                db.clear_lock_marker_export_publish(&publish)?;
                reconciled = true;
                break;
            };

            let scrubbed = transform(snapshot.text());
            // Rewrite even when already scrubbed. That replay case is a crash after publish but
            // before outbox acknowledgement; another durable atomic publish proves the file safe.
            note.overwrite_owned_snapshot(db, &publish, snapshot, &scrubbed, &transform)?;
            final_body = Some(scrubbed);
            reconciled = true;
            break;
        }
        if !reconciled {
            return Err(AppError::Storage(
                "marker-export cleanup exceeded bounded crash recovery attempts".into(),
            ));
        }
        if let Some(scrubbed) = final_body {
            let hash = crate::export::note_content_hash(&scrubbed);
            for owner_rows in marker_cleanup_owner_groups(&rows) {
                db.ack_lock_marker_export_cleanup(&owner_rows, Some(&scrubbed), Some(&hash))?;
            }
        } else {
            for owner_rows in marker_cleanup_owner_groups(&rows) {
                db.ack_lock_marker_export_cleanup(&owner_rows, None, None)?;
            }
        }
        drained += rows.len();
    }
    tracing::info!(target: "links", drained, "drained durable lock-marker export cleanup rows");
    Ok(())
}

fn marker_cleanup_owner_groups(
    rows: &[crate::storage::links::LockMarkerExportCleanup],
) -> Vec<Vec<crate::storage::links::LockMarkerExportCleanup>> {
    let mut groups: std::collections::BTreeMap<
        (String, String, String),
        Vec<crate::storage::links::LockMarkerExportCleanup>,
    > = std::collections::BTreeMap::new();
    for row in rows {
        groups
            .entry((
                row.source_kind.clone(),
                row.source_id.clone(),
                row.provider_id.clone(),
            ))
            .or_default()
            .push(row.clone());
    }
    groups.into_values().collect()
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
                Err(e) => {
                    tracing::warn!(target: "links", meeting_id = %source_id, error = %e, "marker-strip .md re-export: note read failed")
                }
            }
        } else if let Err(e) = export_note_to_vault_under_lifecycle_authorized(state, source_id) {
            // Note-doc source: the gate inside is satisfied (a stripped source is VISIBLE by
            // construction — the strip skips sealed-at-rest sources).
            tracing::warn!(target: "links", note_id = %source_id, error = %e, "marker-strip .md re-export (note) failed");
        }
    }
}

/// Brain-v3 audit Fix 4 — RELOCK helper: resolve the given folders' meeting + document ids, strip
/// their `[[Title]]` markers from VISIBLE source notes and journal the exact vault paths in the same
/// transaction. The caller finishes the folder-owned purge/reblank first and drains the journal as
/// its last privacy leg. Called BEFORE the link purge drops the naming edges.
pub(crate) fn enqueue_marker_cleanup_for_folders(
    state: &AppState,
    folder_ids: &std::collections::HashSet<String>,
) -> Result<(), AppError> {
    let mut meeting_ids: Vec<String> = Vec::new();
    let mut document_ids: Vec<String> = Vec::new();
    for fid in folder_ids {
        meeting_ids.append(&mut state.db.meeting_ids_in_folder(fid)?);
        document_ids.append(&mut state.db.document_ids_in_folder(fid)?);
    }
    state
        .db
        .strip_sealed_neighbour_markers(&meeting_ids, &document_ids)?;
    Ok(())
}

/// RE-BLANK (relock): re-blank the plaintext transcript + timeline of every governed meeting and
/// remove the decrypted session WAV, re-pointing audio_path back at the `.enc`. The `*_blob`
/// columns + the `.enc` stay (the folder is still `locked=1`). Idempotent.
#[cfg(test)]
pub(crate) fn reblank_folder_extras(state: &AppState, folder_id: &str) -> Result<(), AppError> {
    let verified = verify_relock_retained_blobs(state, folder_id)?;
    if verified.has_repairs() {
        prepare_folder_exports_before_relock(state, folder_id)?;
        verified.apply_repairs(&state.db)?;
    }
    reblank_folder_extras_after_verification(state, folder_id, verified.ck())
}

/// Authenticate every retained ciphertext whose current session plaintext is about to be
/// destroyed by relock, and prove that it decrypts byte-identical to that plaintext. A non-empty
/// plaintext with no blob is an interrupted-fresh-seal repair: build and decrypt-verify its
/// replacement ciphertext in memory, but do not blank anything yet. Relock-all constructs one plan
/// per folder before applying the FIRST repair/blank, so one corrupt/stale blob leaves every good
/// plaintext column untouched.
pub(crate) struct VerifiedRelockPlan {
    ck: Option<Zeroizing<[u8; 32]>>,
    attachment_seals: Vec<(String, Vec<u8>)>,
    note_seals: Vec<(String, String, Vec<u8>)>,
    segment_seals: Vec<(String, i64, Vec<u8>)>,
    timeline_seals: Vec<(String, Vec<u8>)>,
    manual_note_seals: Vec<(String, Vec<u8>)>,
    document_seals: Vec<(String, Vec<u8>)>,
}

impl VerifiedRelockPlan {
    pub(crate) fn ck(&self) -> Option<&[u8; 32]> {
        self.ck.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn has_repairs(&self) -> bool {
        !self.attachment_seals.is_empty()
            || !self.note_seals.is_empty()
            || !self.segment_seals.is_empty()
            || !self.timeline_seals.is_empty()
            || !self.manual_note_seals.is_empty()
            || !self.document_seals.is_empty()
    }

    /// Persist only ciphertexts already decrypt-verified against their exact captured plaintext.
    /// Each low-level `seal_*` call stores that recoverable blob and blanks the matching plaintext;
    /// a later DB failure therefore leaves completed rows recoverable and the cached KEK available
    /// for an idempotent retry.
    pub(crate) fn apply_repairs(&self, db: &Db) -> Result<(), AppError> {
        // Persist attachment replacements before any of the existing `seal_*` calls below can
        // blank a plaintext column. The batch itself retains `data`, so a later failure remains
        // recoverable and an idempotent retry can authenticate the newly retained blobs.
        if !self.attachment_seals.is_empty() {
            db.store_attachment_seals(&self.attachment_seals)?;
        }
        for (meeting_id, provider_id, blob) in &self.note_seals {
            db.seal_note(meeting_id, provider_id, blob)?;
        }
        for (meeting_id, idx, blob) in &self.segment_seals {
            db.seal_segment(meeting_id, *idx, blob)?;
        }
        for (meeting_id, blob) in &self.timeline_seals {
            db.seal_timeline(meeting_id, blob)?;
        }
        for (meeting_id, blob) in &self.manual_note_seals {
            db.seal_manual_notes(meeting_id, blob)?;
        }
        for (document_id, blob) in &self.document_seals {
            db.seal_document(document_id, blob)?;
        }
        Ok(())
    }
}

pub(crate) fn verify_relock_retained_blobs(
    state: &AppState,
    folder_id: &str,
) -> Result<VerifiedRelockPlan, AppError> {
    let notes = state.db.notes_in_folder(folder_id)?;
    let meeting_ids = state.db.meeting_ids_in_folder(folder_id)?;
    let documents = state.db.raw_documents_in_folder(folder_id)?;
    let attachments = state.db.attachments_in_folder(folder_id)?;
    let mut meeting_content = Vec::with_capacity(meeting_ids.len());
    for meeting_id in &meeting_ids {
        meeting_content.push((
            meeting_id.clone(),
            state.db.raw_segments(meeting_id)?,
            state.db.raw_timeline(meeting_id)?,
            state.db.raw_manual_notes(meeting_id)?,
        ));
    }
    let mut needs_verification = notes.iter().any(|note| !note.markdown.is_empty())
        || documents.iter().any(|document| !document.text.is_empty())
        || attachments.iter().any(|attachment| {
            !attachment.data.is_empty()
                || attachment.data_blob.is_none()
                || attachment.exported_path.is_some()
        });
    if !needs_verification {
        needs_verification = meeting_content
            .iter()
            .any(|(_, segments, timeline, manual)| {
                segments.iter().any(|segment| !segment.text.is_empty())
                    || timeline
                        .as_ref()
                        .is_some_and(|timeline| !timeline.data.is_empty())
                    || manual
                        .as_ref()
                        .is_some_and(|manual| !manual.text.is_empty())
            });
    }
    if !needs_verification {
        return Ok(VerifiedRelockPlan {
            ck: None,
            attachment_seals: Vec::new(),
            note_seals: Vec::new(),
            segment_seals: Vec::new(),
            timeline_seals: Vec::new(),
            manual_note_seals: Vec::new(),
            document_seals: Vec::new(),
        });
    }

    if state.db.folder_wrapped_key(folder_id)?.is_none() {
        return Err(AppError::Locked(
            "relock found plaintext without a wrapped folder key; content retained for authenticated repair"
                .into(),
        ));
    }
    let ck = session_folder_ck(state, folder_id)?;
    let mut attachment_seals = Vec::new();
    let mut note_seals = Vec::new();
    let mut segment_seals = Vec::new();
    let mut timeline_seals = Vec::new();
    let mut manual_note_seals = Vec::new();
    let mut document_seals = Vec::new();
    for attachment in &attachments {
        if attachment.data.is_empty()
            && attachment.data_blob.is_some()
            && attachment.exported_path.is_none()
        {
            continue;
        }
        if let Some(blob) =
            verify_or_prepare_attachment_relock_blob(&ck, folder_id, attachment, true)?
        {
            attachment_seals.push((attachment.id.clone(), blob));
        }
    }
    for note in &notes {
        if !note.markdown.is_empty() {
            if let Some(blob) = verify_or_prepare_relock_blob(
                &ck,
                note.content_blob.as_deref(),
                &aad_content(folder_id, &note.meeting_id, &note.provider_id, "note"),
                note.markdown.as_bytes(),
                "note",
            )? {
                note_seals.push((note.meeting_id.clone(), note.provider_id.clone(), blob));
            }
        }
    }
    for (meeting_id, segments, timeline, manual) in &meeting_content {
        for segment in segments {
            if !segment.text.is_empty() {
                if let Some(blob) = verify_or_prepare_relock_blob(
                    &ck,
                    segment.text_blob.as_deref(),
                    &aad_content(folder_id, meeting_id, AAD_NO_PROVIDER, "segment"),
                    segment.text.as_bytes(),
                    "segment",
                )? {
                    segment_seals.push((meeting_id.clone(), segment.idx, blob));
                }
            }
        }
        if let Some(timeline) = timeline {
            if !timeline.data.is_empty() {
                if let Some(blob) = verify_or_prepare_relock_blob(
                    &ck,
                    timeline.data_blob.as_deref(),
                    &aad_content(folder_id, meeting_id, AAD_NO_PROVIDER, "timeline"),
                    timeline.data.as_bytes(),
                    "timeline",
                )? {
                    timeline_seals.push((meeting_id.clone(), blob));
                }
            }
        }
        if let Some(manual) = manual {
            if !manual.text.is_empty() {
                if let Some(blob) = verify_or_prepare_relock_blob(
                    &ck,
                    manual.blob.as_deref(),
                    &aad_content(folder_id, meeting_id, AAD_NO_PROVIDER, "manual_notes"),
                    manual.text.as_bytes(),
                    "manual notes",
                )? {
                    manual_note_seals.push((meeting_id.clone(), blob));
                }
            }
        }
    }
    for document in &documents {
        if !document.text.is_empty() {
            if let Some(blob) = verify_or_prepare_relock_blob(
                &ck,
                document.blob.as_deref(),
                &aad_document(folder_id, &document.id),
                document.text.as_bytes(),
                "document",
            )? {
                document_seals.push((document.id.clone(), blob));
            }
        }
    }
    Ok(VerifiedRelockPlan {
        ck: Some(ck),
        attachment_seals,
        note_seals,
        segment_seals,
        timeline_seals,
        manual_note_seals,
        document_seals,
    })
}

/// Verify the attachment recovery copy for relock/startup without consulting an unlocked-content
/// reader. SQLCipher protects the canonical metadata; `byte_len + sha256` binds the decrypted bytes
/// to that metadata, while the attachment AAD binds the seal to its exact folder/owner/id.
fn verify_or_prepare_attachment_relock_blob(
    ck: &[u8; 32],
    folder_id: &str,
    attachment: &crate::storage::AttachmentRecord,
    require_recoverable_empty: bool,
) -> Result<Option<Vec<u8>>, AppError> {
    use sha2::{Digest, Sha256};

    let verify_metadata = |plaintext: &[u8]| -> Result<(), AppError> {
        let digest: [u8; 32] = Sha256::digest(plaintext).into();
        if plaintext.len() as u64 != attachment.byte_len || digest != attachment.sha256 {
            return Err(AppError::Storage(
                "attachment bytes do not match their authenticated metadata during relock".into(),
            ));
        }
        Ok(())
    };
    let aad = attachment_aad(folder_id, &attachment.owner, &attachment.id);

    if !attachment.data.is_empty() {
        verify_metadata(&attachment.data)?;
        return verify_or_prepare_relock_blob(
            ck,
            attachment.data_blob.as_deref(),
            &aad,
            &attachment.data,
            "attachment",
        );
    }

    let Some(blob) = attachment.data_blob.as_deref() else {
        if require_recoverable_empty {
            return Err(AppError::Storage(
                "attachment has neither plaintext nor a recoverable seal during relock".into(),
            ));
        }
        return Ok(None);
    };
    let restored = crate::crypto::decrypt(ck, blob, &aad)?;
    verify_metadata(&restored)?;
    Ok(None)
}

fn verify_or_prepare_relock_blob(
    ck: &[u8; 32],
    retained_blob: Option<&[u8]>,
    aad: &[u8],
    plaintext: &[u8],
    family: &'static str,
) -> Result<Option<Vec<u8>>, AppError> {
    if let Some(blob) = retained_blob {
        verify_relock_plaintext_copy(ck, blob, aad, plaintext, family)?;
        return Ok(None);
    }
    let blob = crate::crypto::encrypt(ck, plaintext, aad)?;
    verify_relock_plaintext_copy(ck, &blob, aad, plaintext, family)?;
    Ok(Some(blob))
}

fn verify_relock_plaintext_copy(
    ck: &[u8; 32],
    blob: &[u8],
    aad: &[u8],
    plaintext: &[u8],
    family: &'static str,
) -> Result<(), AppError> {
    let restored = crate::crypto::decrypt(ck, blob, aad)?;
    if restored != plaintext {
        return Err(AppError::Storage(format!(
            "{family} retained seal does not match session plaintext"
        )));
    }
    Ok(())
}

fn reblank_folder_extras_after_verification(
    state: &AppState,
    folder_id: &str,
    verified_ck: Option<&[u8; 32]>,
) -> Result<(), AppError> {
    // SEAL-NET KEY (2026-07-16 lock review of #356): resolve the folder CK from the SESSION-CACHED
    // KEK only — never a keychain/biometric prompt (this runs on relock / app-close / screen-share).
    // Both relock paths retain the KEK until every filesystem reblank succeeds, then zeroize it as
    // the final session-state commit. A failed reblank therefore remains retryable and never claims
    // a locked state while a known plaintext audio path survives.
    let resolved_ck: Option<Zeroizing<[u8; 32]>> = if verified_ck.is_none() {
        (|| {
            let kek = state.master_kek.lock().ok()?.clone()?;
            let wrapped = state.db.folder_wrapped_key(folder_id).ok().flatten()?;
            let bytes = Zeroizing::new(
                crate::crypto::decrypt(&kek, &wrapped, &aad_wrapped_ck(folder_id)).ok()?,
            );
            let arr: [u8; 32] = bytes.as_slice().try_into().ok()?;
            Some(Zeroizing::new(arr))
        })()
    } else {
        None
    };
    let seal_ck = verified_ck.or(resolved_ck.as_deref());
    // Boards, on the same terms as every other kind. A relock destroys the session plaintext of
    // a folder that is already durably locked; a kind the re-blank does not enumerate is a kind
    // whose plaintext SURVIVES that relock, readable in an open database. Seal and unseal
    // covering dashboards while this one did not would have made the very first relock after a
    // session-unlock leave a board's title and its tiles in the clear.
    // Boards are handled AFTER the loop below, deliberately — see the note there. Returning the
    // keyless refusal here first would have made a keyless relock blank NOTHING, when before this
    // diff it still blanked every segment.
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
        let playback_aad = aad_audio_role(&mid, folder_id, StreamRole::Playback);
        if let Some(enc) = reblank_audio(
            seal_ck,
            state.db.get_meeting(&mid)?.and_then(|m| m.audio_path),
            &playback_aad,
        )? {
            state.db.set_meeting_audio_path(&mid, Some(&enc))?;
        }
        let (mic, sys) = state.db.get_meeting_master_paths(&mid)?;
        let mic_aad = aad_audio_role(&mid, folder_id, StreamRole::Mic);
        if let Some(enc) = reblank_audio(seal_ck, mic, &mic_aad)? {
            state.db.set_meeting_mic_master_path(&mid, Some(&enc))?;
        }
        let sys_aad = aad_audio_role(&mid, folder_id, StreamRole::Sys);
        if let Some(enc) = reblank_audio(seal_ck, sys, &sys_aad)? {
            state.db.set_meeting_sys_master_path(&mid, Some(&enc))?;
        }
        // FAIL-CLOSED sweep (2026-07-16 reviews: #352 adversarial MEDIUM, then the #356 lock-review
        // FAIL that scoped it): segment rows carrying PLAINTEXT with NO sealed blob — a
        // pipeline/move killed before its seal step — are invisible to the blob-guarded re-blanks
        // above. A transcript is user-authored/corrected canonical content: audio re-transcription
        // is NOT a byte-identical recovery copy. Therefore the only safe action is to seal and
        // decrypt-verify these exact rows under the folder CK. Without a cached session key, retain
        // them behind the logical gate and return an error for an authenticated repair retry.
        let has_unsealed = state
            .db
            .raw_segments(&mid)?
            .iter()
            .any(|s| s.text_blob.is_none());
        if has_unsealed {
            if let Some(ck) = seal_ck {
                seal_meeting_extras(&state.db, folder_id, &mid, ck)?;
                tracing::warn!(target: "lock", meeting_id = %mid, "relock authenticated and sealed exact crash-window transcript rows in place under the folder CK");
            } else {
                return Err(AppError::Locked(
                    "relock found an exact transcript without a cached session key; content retained for authenticated repair".into(),
                ));
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
    // Export files were integrity-checked and removed before logical visibility was revoked by the
    // relock entrypoint. A startup-repair caller can still arrive with legacy retry metadata, so
    // keep this idempotent second pass fail-closed too.
    let relock_doc_rows = state.db.note_exported_path_rows_in_folder(folder_id)?;
    ensure_no_external_edit_siblings(relock_doc_rows.iter().map(|(_, p)| p))?;
    for (doc_id, p) in relock_doc_rows {
        let expected = state.db.get_note_doc_exported_hash(&doc_id)?;
        remove_note_export_if_unchanged(
            &p,
            expected.as_deref(),
            "remove re-exported note during relock",
        )?;
        state.db.set_note_doc_exported_path(&doc_id, None)?;
    }
    reblank_attachments_in_folder(state, folder_id)?;

    // TASKS LEAVE HERE TOO. `seal_folder_extras` unfiles them on the initial lock, but that is not
    // the only way a task gets into a sealed container: a folder that is durably locked and
    // SESSION-unlocked passes `folder_is_unlocked`, so `set_task_container` accepts it, and the
    // user can file a task into it. The relock then runs THIS function — which knew nothing about
    // tasks — and the task stayed inside a container reported as locked, which is exactly the
    // state the unfiling exists to prevent. Sealing and relocking are two doors into the same
    // room; fixing one of them is not fixing it.
    let unfiled = state.db.unfile_tasks_in_container(folder_id)?;
    if unfiled > 0 {
        tracing::info!(target: "lock", folder_id, unfiled, "unfiled tasks on relock");
    }

    // BOARDS LAST, and the position is the point. Every re-blank above works WITHOUT a content
    // key, because each row still holds its own ciphertext to fall back on. A board does not: the
    // unlock cleared `title_blob` / `config_blob`, so it must be re-encrypted to be blanked at all.
    //
    // So the keyless case can only be REFUSED, never skipped — skipping would report the folder
    // locked while its boards stayed readable, which is the leak this whole function exists to
    // prevent. Refusing EARLY, though, was its own regression: it returned before the segment and
    // document re-blanks, which need no key, so a keyless relock went from blanking everything it
    // could to blanking nothing at all. Doing the key-free work first and reporting the gap last
    // keeps both properties — nothing recoverable is skipped, and nothing unsealed is quietly left
    // readable.
    match seal_ck {
        Some(ck) => seal_dashboards_in_folder(&state.db, folder_id, ck)?,
        None => {
            if state.db.folder_has_plaintext_dashboards(folder_id)? {
                return Err(AppError::Locked(
                    "cannot relock: no content key available to reseal this folder's boards".into(),
                ));
            }
        }
    }
    Ok(())
}

/// PERMANENT remove-lock preparation: decrypt every governed meeting's transcript + timeline back
/// to plaintext and durably restore the plaintext WAV while RETAINING all `*_blob` columns. The
/// caller atomically clears those blobs together with `locked=0`, then retires returned redundant
/// `.enc` paths. This ordering makes every crash recoverable.
/// `meeting_embedder`: the caller-resolved, model-gated embedder for the MEETING re-index (see
/// [`unseal_folder_extras`] for why it is injected rather than resolved internally).
pub(crate) fn unseal_folder_extras_permanent(
    state: &AppState,
    folder_id: &str,
    ck: &[u8; 32],
    meeting_embedder: Option<&dyn crate::embed::Embedder>,
) -> Result<Vec<String>, AppError> {
    // Retire these ciphertexts only AFTER `remove_lock_inner` commits `locked=0`. Keeping them
    // through the whole restore closes the crash window where the DB still says locked but neither
    // its `.enc` pointer nor startup reconciliation can reconstruct the already-published WAV.
    let mut sealed_audio_to_retire = Vec::new();
    for mid in state.db.meeting_ids_in_folder(folder_id)? {
        // Transcript: restore each segment from its blob (or keep the in-memory text if the folder
        // was session-unlocked and the blob is absent). The final folder-open transaction clears
        // every blob only after all plaintext/audio restoration has succeeded.
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
        // Typed notes: permanently restore the plaintext from the blob (or keep the in-memory
        // plaintext if the folder was session-unlocked and the blob is absent). Keep the blob until
        // the final atomic folder-open commit.
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
        // Audio at rest: permanently restore the playback WAV + both masters from their .enc, each
        // decrypted through the role→role-less AAD ladder (a pre-role master still decrypts). The
        // `.enc` remains until the caller has committed this folder permanently open.
        let playback_enc = state.db.get_meeting(&mid)?.and_then(|m| m.audio_path);
        let (pb_role, pb_less) = audio_decrypt_ladder(&mid, folder_id, StreamRole::Playback);
        if let Some(restored) =
            permanent_unseal_audio(ck, playback_enc.clone(), &[&pb_role, &pb_less])?
        {
            state
                .db
                .set_meeting_audio_path(&mid, Some(&restored.plaintext_path))?;
            sealed_audio_to_retire.push(restored.sealed_path);
        }
        let (mic, sys) = state.db.get_meeting_master_paths(&mid)?;
        let (mic_role, mic_less) = audio_decrypt_ladder(&mid, folder_id, StreamRole::Mic);
        if let Some(restored) = permanent_unseal_audio(ck, mic.clone(), &[&mic_role, &mic_less])? {
            state
                .db
                .set_meeting_mic_master_path(&mid, Some(&restored.plaintext_path))?;
            sealed_audio_to_retire.push(restored.sealed_path);
        }
        let (sys_role, sys_less) = audio_decrypt_ladder(&mid, folder_id, StreamRole::Sys);
        if let Some(restored) = permanent_unseal_audio(ck, sys.clone(), &[&sys_role, &sys_less])? {
            state
                .db
                .set_meeting_sys_master_path(&mid, Some(&restored.plaintext_path))?;
            sealed_audio_to_retire.push(restored.sealed_path);
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
        restored_doc_ids.push(d.id.clone());
    }
    if !restored_doc_ids.is_empty() {
        // Chunks + FTS come back unconditionally (keyword retrieval works model-less); vectors only
        // when the REAL e5 model is present (never stub vectors). KIND-ROUTED (Brain v3 audit gap
        // #3): an authored note re-chunks through the front-matter-stripping path.
        for did in &restored_doc_ids {
            if let Err(e) = index_document_row_kind_routed(&state.db, did, meeting_embedder) {
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

    // Boards LAST here too. The session path was changed to run them last so one undecryptable
    // board blob could not abort an unlock before a single meeting, note, document or audio file
    // was restored — and this path is where that matters MORE, not less: a permanent unlock is the
    // one the user runs to get their content back for good, and aborting it early leaves the
    // folder half-restored with the lock still on. Fixing only the session path and leaving the
    // permanent one running boards first would have been a fix that looked complete and covered
    // the less important half.
    unseal_dashboards_in_folder(&state.db, folder_id, ck)?;

    Ok(sealed_audio_to_retire)
}

/// READ-GATE predicate (the user's actual complaint): a meeting is unlocked iff its folder is open
/// (NULL / not locked) OR its folder id is in the current session unlock set. Used by
/// `get_meeting_detail` / `get_segments` / `get_timeline` / `export_audio` to refuse a sealed-and-
/// not-session-unlocked meeting's content even though the SQLCipher DB is open.
/// Snapshot the live session unlock set (the same source `list_folders` / the graph reads use).
/// Passed to the `*_visible` DB reads (BLK-2b) so a sealed-and-not-unlocked meeting contributes
/// nothing to digests, search, last-note, topic threads, etc. — independent of at-rest blanking.
pub(crate) fn unlocked_snapshot(
    state: &AppState,
) -> Result<std::collections::HashSet<String>, AppError> {
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

// ── CONTAINER-CREATION GATE ──────────────────────────────────────────────────────────────────────
//
// Placing a container under another has to answer one question: what does the parent's seal mean for
// the child? Before the hierarchy this barely arose — the shipped UI only ever created containers at
// the root — so none of the three writers asked it, and a child created under a sealed parent was
// itself OPEN while its vault directory sat inside the sealed parent's directory. A plaintext `.md`
// could then be exported into a sealed tree with nothing to notice: the at-rest re-blank sweep keys
// off folder ids that are themselves marked locked, and the child is not one of them.
//
// Under Projects › Folders that is the ordinary way a folder is made, so the answer lives in ONE
// place that every writer consults. Two writers that disagree about this is how the gap survives.

/// What a parent's seal requires of a container being created inside it.
pub(crate) enum ParentSeal {
    /// The parent is open — create normally.
    Open,
    /// The parent is sealed AND session-unlocked, so its key is already available: create, then seal
    /// the child before returning. No additional user authorisation is needed and none is asked for.
    SealChild,
}

/// Decide what creating inside `parent_id` requires, or refuse.
///
/// Reads the IMMEDIATE parent only, which is a BOUND on this gate rather than a proof that no
/// such state exists. It stops every open container this change can create beneath a sealed one;
/// it does not speak for rows a shipped build already left, or for a lock applied to an ancestor
/// while a descendant was open. Locks are per-container today, so the ancestor case has no way to
/// arise from the lock commands either — but that is the lock model's property, not this gate's.
/// **Part 5 changes
/// that** — a project lock seals a whole subtree, and this gate must then consider the nearest
/// SEALED ANCESTOR rather than the link parent, because the child's directory is composed from an
/// ancestor path and can land inside a sealed tree without its own parent being sealed.
///
/// Refuses a sealed-and-NOT-session-unlocked parent with [`AppError::Locked`]: there is no key to
/// seal the child with, so the only alternatives would be to leave plaintext inside a sealed tree or
/// to prompt for authorisation from a code path the user did not initiate.
pub(crate) fn container_parent_seal(
    state: &AppState,
    parent_id: &str,
) -> Result<ParentSeal, AppError> {
    let parent = state
        .db
        .folder_by_id(parent_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no parent folder {parent_id}")))?;
    if !parent.locked {
        return Ok(ParentSeal::Open);
    }
    if folder_is_unlocked(state, parent_id)? {
        return Ok(ParentSeal::SealChild);
    }
    Err(AppError::Locked(
        "unlock this folder before creating something inside it".into(),
    ))
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
    reseal_document_if_locked_with_mode(state, folder_id, doc_id, title, text, updated_at, true)
}

fn reseal_document_if_locked_with_mode(
    state: &AppState,
    folder_id: &str,
    doc_id: &str,
    title: &str,
    text: &str,
    updated_at: i64,
    mark_org_dirty: bool,
) -> Result<(), AppError> {
    let locked = state
        .db
        .folder_by_id(folder_id)?
        .map(|f| f.locked)
        .unwrap_or(false);
    if !locked {
        return if mark_org_dirty {
            state.db.update_note_row(doc_id, title, text, updated_at)
        } else {
            state
                .db
                .update_note_row_debounced(doc_id, title, text, updated_at)
        };
    }
    let blob = sealed_document_blob(state, folder_id, doc_id, text)?;
    if mark_org_dirty {
        state
            .db
            .update_note_row_sealed(doc_id, title, text, &blob, updated_at)?;
    } else {
        state
            .db
            .update_note_row_sealed_debounced(doc_id, title, text, &blob, updated_at)?;
    }
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
    let attachment_owner = crate::storage::AttachmentOwner::Meeting {
        meeting_id: note.meeting_id.clone(),
        provider_id: note.provider_id.clone(),
    };
    validate_attachment_references_before_save(state, &attachment_owner, &note.markdown)?;
    let locked_folder = match state.db.folder_for_meeting(&note.meeting_id)? {
        Some(fid) => state.db.folder_by_id(&fid)?.filter(|f| f.locked),
        None => None,
    };
    if let Some(folder) = locked_folder {
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
    } else {
        state.db.upsert_note(note)?;
    }
    prune_unreferenced_attachments(state, &attachment_owner, &note.markdown)
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
    /// Content-free, durable latch set after the first successful account login. It lets the shell
    /// distinguish an intentionally local-only user from an account whose session was lost.
    pub account_expected: bool,
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

const ACCOUNT_EXPECTED_SETTING: &str = "sharing_account_expected";

/// Read the content-free account latch and backfill pre-feature installations that still have a
/// live or persisted session. Once observed it is one-way: a later definitive 401 may clear the
/// Keychain tokens, but the shell can still explain that sign-in is required. A logged-in upgrade
/// is not reported as durable until its metadata write succeeds.
fn account_expected_with_upgrade(state: &AppState, logged_in: bool) -> Result<bool, AppError> {
    account_expected_with_upgrade_with(state, logged_in, |state| {
        state.db.set_setting(ACCOUNT_EXPECTED_SETTING, "true")
    })
}

fn account_expected_with_upgrade_with(
    state: &AppState,
    logged_in: bool,
    persist_latch: impl FnOnce(&AppState) -> Result<(), AppError>,
) -> Result<bool, AppError> {
    let persisted = state
        .db
        .get_setting(ACCOUNT_EXPECTED_SETTING)?
        .map(|value| value == "true")
        .unwrap_or(false);
    if logged_in && !persisted {
        persist_latch(state)?;
    }
    Ok(persisted || logged_in)
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
    account_status_inner_with(
        state.inner(),
        crate::share::load_tokens,
        crate::secrets::keychain::account_mk_cached,
    )
}

fn account_status_inner_with(
    state: &AppState,
    load_tokens: impl FnOnce() -> Result<Option<crate::share::PersistedTokens>, AppError>,
    account_mk_cached: impl FnOnce() -> Result<bool, AppError>,
) -> Result<AccountStatus, AppError> {
    let session = state
        .account_session
        .lock()
        .map_err(|_| AppError::Storage("account-session mutex poisoned".into()))?;
    // Logged-in-but-locked survives a restart via the Keychain tokens even when MK isn't in RAM.
    let persisted_email = load_tokens()?.map(|t| t.email);
    let (logged_in, email, unlocked) = match session.as_ref() {
        Some(s) => (true, Some(s.email.clone()), true),
        None => match persisted_email {
            Some(e) => (true, Some(e), false),
            None => (false, None, false),
        },
    };
    let account_expected = account_expected_with_upgrade(state, logged_in)?;
    // Existence-only probe (NO Touch ID prompt): can a locked session be restored biometrically?
    // The no-prompt existence probe LIES for ACL'd data-protection items on current macOS
    // (2026-07-05 field incident: it reported not-found for items that a prompting read returns),
    // so a probe "false" must not HIDE the Touch ID button — on a macOS release build, offer it
    // whenever a logged-in-but-locked session exists; a tap with no real cached MK fails closed
    // ("no cached account key") and the FE falls back to the password CTA. The probe result still
    // short-circuits `true` when it does find the item.
    let probe_says_cached = account_mk_cached().unwrap_or(false);
    let biometric_unlock_available =
        logged_in && (probe_says_cached || cfg!(all(target_os = "macos", not(debug_assertions))));
    let cfg = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    Ok(AccountStatus {
        logged_in,
        account_expected,
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
    app: AppHandle,
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

    let persisted_tokens = crate::share::PersistedTokens {
        access_token: access_token.clone(),
        refresh_token: refresh_token.clone(),
        device_id: device_id.clone(),
        email: acct_id.clone(),
        account_id: acct_id.clone(),
        server_user_id: server_user_id.clone(),
        generation,
        access_expires_at: access_expires_at.clone(),
    };
    let session = crate::share::AccountSession {
        account_id: acct_id.clone(),
        email: acct_id.clone(),
        server_user_id,
        device_id,
        mk: Zeroizing::new(*mk),
        generation,
        access_token,
        access_expires_at,
        refresh_token,
    };
    establish_account_login_local_with(
        state.inner(),
        &persisted_tokens,
        session,
        crate::share::store_tokens,
    )?;

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
    if let Err(e) = org_reconcile_memberships_notifying(state.inner(), Some(&app)).await {
        tracing::warn!(target: "org", error = %brief_err(&e), "org membership reconcile at login failed (non-fatal)");
    }

    // Build every fallible field BEFORE stamping the one-way latch. Once the latch is set there are
    // no remaining `?` paths: this invocation is committed to return `Ok(AccountStatus)`.
    let cfg = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    let status = AccountStatus {
        logged_in: true,
        account_expected: true,
        email: Some(acct_id),
        unlocked_for_sharing: true,
        share_consented: cfg.share_egress_consented,
        server_configured: !cfg.share_base_url.trim().is_empty(),
        biometric_unlock_available: crate::secrets::keychain::account_mk_cached().unwrap_or(false)
            || cfg!(all(target_os = "macos", not(debug_assertions))),
    };
    drop(cfg);
    finish_account_login_success(state.inner(), status)
}

/// Establish the usable LOCAL half of a login, without touching the account-expectation latch.
/// Callers stamp that latch only at their final `Ok` boundary via [`finish_account_login_success`].
fn establish_account_login_local_with(
    state: &AppState,
    tokens: &crate::share::PersistedTokens,
    account_session: crate::share::AccountSession,
    store_tokens: impl FnOnce(&crate::share::PersistedTokens) -> Result<(), AppError>,
) -> Result<(), AppError> {
    store_tokens(tokens)?;
    {
        let mut session = state
            .account_session
            .lock()
            .map_err(|_| AppError::Storage("account-session mutex poisoned".into()))?;
        *session = Some(account_session);
    }
    Ok(())
}

/// Absolute successful-login commit boundary: durable content-free latch persistence is REQUIRED
/// for `Ok(AccountStatus)`, and no fallible operation runs afterwards. A storage failure therefore
/// cannot report an unqualified successful login with no lost-session marker.
fn finish_account_login_success(
    state: &AppState,
    status: AccountStatus,
) -> Result<AccountStatus, AppError> {
    finish_account_login_success_with(state, status, |state| {
        state.db.set_setting(ACCOUNT_EXPECTED_SETTING, "true")
    })
}

fn finish_account_login_success_with(
    state: &AppState,
    status: AccountStatus,
    persist_latch: impl FnOnce(&AppState) -> Result<(), AppError>,
) -> Result<AccountStatus, AppError> {
    persist_latch(state)?;
    Ok(status)
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

// ── Workspace hierarchy read surface: the container forest, its per-kind groups, and the gate ─────
#[cfg(test)]
#[path = "tests/workspace_tree_tests.rs"]
mod workspace_tree_tests;

// ── BLK-1 lifecycle-race + BLK-2 move-into-locked + BLK-3/BLK-4 config tests ──────────────────────
#[cfg(test)]
#[path = "tests/lifecycle_tests.rs"]
mod lifecycle_tests;

#[cfg(test)]
#[path = "tests/attachment_tests.rs"]
mod attachment_tests;

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

// ─── brain2 connectors — the Notion/ClickUp settings-DTO contract ───────────────────────────────
//
// Guards the two egress-adjacent invariants of the two new BYO-token READ connectors at the
// `AppConfigDto` boundary: (1) an OMITTED enable/workspace key PRESERVES the stored value (a caller
// that predates the field cannot silently clear a connector the user configured) while an explicit
// value still applies, and (2) the one-time egress CONSENT is preserve-only — a settings save can
// neither grant it nor clear it; only the dedicated `consent_to_*` command can.
#[cfg(test)]
mod connector_dto_tests {
    use super::*;

    /// A config with both new connectors fully switched on.
    fn configured() -> AppConfig {
        AppConfig {
            notion_enabled: true,
            notion_consented: true,
            clickup_enabled: true,
            clickup_consented: true,
            clickup_team_id: "9001".to_string(),
            ..AppConfig::default()
        }
    }

    /// RED-before-GREEN for the omission-safe shape: with plain `bool`/`String` DTO fields, an
    /// older caller that round-trips the whole DTO (the onboarding wizard) omits these keys, serde
    /// defaults them to `false`/`""`, and the user's enabled connector is silently CLEARED.
    #[test]
    fn omitted_connector_keys_preserve_the_stored_values() {
        let current = configured();
        let mut dto = config_to_dto(&current);
        dto.notion_enabled = None;
        dto.clickup_enabled = None;
        dto.clickup_team_id = None;
        let out = dto_to_config(dto, &current);
        assert!(
            out.notion_enabled,
            "an ABSENT notion_enabled must PRESERVE, never clear"
        );
        assert!(out.clickup_enabled, "an ABSENT clickup_enabled preserves");
        assert_eq!(
            out.clickup_team_id, "9001",
            "an ABSENT workspace id preserves"
        );
    }

    /// An OMITTED key can never ENABLE a connector either — preserve means preserve, in both
    /// directions (fail-closed: a partial save cannot turn on external egress).
    #[test]
    fn omitted_connector_keys_cannot_enable_a_disabled_connector() {
        let current = AppConfig::default(); // everything OFF
        let mut dto = config_to_dto(&current);
        dto.notion_enabled = None;
        dto.clickup_enabled = None;
        let out = dto_to_config(dto, &current);
        assert!(!out.notion_enabled);
        assert!(!out.clickup_enabled);
    }

    /// An EXPLICIT value still applies (the Settings UI always sends one), and the granted consent
    /// survives a save that carries `false`.
    #[test]
    fn explicit_values_apply_while_consent_stays_preserve_only() {
        let current = configured();
        let mut dto = config_to_dto(&current);
        dto.notion_enabled = Some(false);
        dto.clickup_enabled = Some(false);
        dto.clickup_team_id = Some(String::new());
        // A save that carries consent=false must NOT revoke the grant.
        dto.notion_consented = false;
        dto.clickup_consented = false;
        let out = dto_to_config(dto, &current);
        assert!(!out.notion_enabled, "an explicit false still disables");
        assert!(!out.clickup_enabled);
        assert_eq!(out.clickup_team_id, "", "an explicit empty id still clears");
        assert!(
            out.notion_consented,
            "notion consent is preserve-only from the DTO"
        );
        assert!(out.clickup_consented);
    }

    /// A settings save can NEVER GRANT external-egress consent — only `consent_to_notion` /
    /// `consent_to_clickup` may. RED if `dto_to_config` ever read `d.*_consented`.
    #[test]
    fn a_settings_save_can_never_grant_connector_consent() {
        let current = AppConfig::default(); // unconsented
        let mut dto = config_to_dto(&current);
        dto.notion_consented = true;
        dto.clickup_consented = true;
        let out = dto_to_config(dto, &current);
        assert!(
            !out.notion_consented,
            "a settings save must NEVER grant Notion egress consent"
        );
        assert!(
            !out.clickup_consented,
            "a settings save must NEVER grant ClickUp egress consent"
        );
    }
}

// ─── diarization + grounding settings DTO contract ────────────────────────────────────────────
#[cfg(test)]
mod analysis_defaults_dto_tests {
    use super::*;

    #[test]
    fn config_to_dto_exposes_the_real_defaults_without_enabling_voiceprints() {
        let dto = config_to_dto(&AppConfig::default());
        assert_eq!(dto.diarize_others, Some(true));
        assert_eq!(dto.ground_summary, Some(true));
        assert!(
            dto.voiceprint_enabled == Some(false),
            "default analysis aids must not opt users into voice biometrics"
        );
    }

    #[test]
    fn omitted_dto_flags_preserve_explicit_stored_choices() {
        let current = AppConfig {
            diarize_others: false,
            ground_summary: false,
            voiceprint_enabled: true,
            ..AppConfig::default()
        };
        let mut json = serde_json::to_value(config_to_dto(&current)).unwrap();
        let object = json.as_object_mut().unwrap();
        object.remove("diarizeOthers");
        object.remove("groundSummary");
        object.remove("voiceprintEnabled");

        let dto: AppConfigDto = serde_json::from_value(json).unwrap();
        assert_eq!(dto.diarize_others, None);
        assert_eq!(dto.ground_summary, None);
        assert_eq!(dto.voiceprint_enabled, None);

        let out = dto_to_config(dto, &current);
        assert!(!out.diarize_others);
        assert!(!out.ground_summary);
        assert!(
            out.voiceprint_enabled,
            "an omitted voiceprint flag must not revoke an explicit opt-in"
        );
    }

    #[test]
    fn explicit_dto_choices_apply() {
        let current = AppConfig::default();
        let mut dto = config_to_dto(&current);
        dto.diarize_others = Some(false);
        dto.ground_summary = Some(false);
        dto.voiceprint_enabled = Some(true);

        let out = dto_to_config(dto, &current);
        assert!(!out.diarize_others);
        assert!(!out.ground_summary);
        assert!(out.voiceprint_enabled);
    }
}
