//! DASHBOARDS command surface (2026-08-03) — user-composed boards of tiles over sources that
//! already exist in the vault.
//!
//! ## The one rule that governs this file
//! A dashboard is primarily a set of POINTERS. Its one content-bearing tile, Living Answer, stores
//! only a backend-generated cache with exact readable-folder and composite-context provenance.
//! **Every tile payload is resolved through a gated reader at read time** — `Db::*_visible` /
//! `get_*_if_visible` / `list_open_commitments` — never by a fresh ungated query. A tile whose
//! source is sealed-and-not-session-unlocked resolves to [`TileData::Locked`], which carries no
//! title, no snippet, no counts, and no dates. That is what keeps a board from becoming a back
//! door into a locked folder (see `.claude/rules/lock-model.md`).
//!
//! Two consequences worth stating explicitly, because both were deliberate:
//! * The tile's *stored* `title` is user-authored chrome (what the user typed on the board), so it
//!   is safe to return for a locked tile — but we DON'T: a locked tile returns the generic
//!   "🔒 Locked" heading, because a user-typed title routinely paraphrases the sealed content.
//! * `ref_id` is never validated for existence at write time. A tile pointing at a deleted row
//!   resolves to [`TileData::Missing`] rather than erroring the whole board.
//!
//! Board-scoped Ask is assembled by [`dashboard_composite_context`], then dispatched through the
//! existing prepacked authorized provider path. Consent, redaction, egress ledger, lifecycle
//! admission, and exact post-await witness validation remain at that shared backend seam.

use serde::{Deserialize, Serialize};
use tauri::{ipc::Response, AppHandle, State};

use crate::commands::offload_read;
use crate::error::AppError;
use crate::links::LinkKind;
use crate::state::AppState;
use crate::storage::dashboards_store::{
    LivingAnswerCacheState, MAX_DASHBOARDS, MAX_SPAN, MAX_TILES_PER_BOARD, MIN_SPAN, TILE_KINDS,
};
use crate::storage::models::{Dashboard, DashboardTile, SourceRef};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DashboardContextWitness {
    pub(crate) dashboard_id: String,
    pub(crate) generation: i64,
    pub(crate) ask_dispatch_generation: i64,
    pub(crate) input_digest: String,
    corpus_budget: usize,
    additional_sources: Vec<SourceRef>,
    excluded_meeting_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct DashboardCompositeContext {
    pub(crate) witness: DashboardContextWitness,
    pub(crate) packed_corpus: String,
    pub(crate) packed_sources: Vec<crate::storage::models::VaultSource>,
}

/// Longest accepted board title / tile heading. Bounds the DTO and the `.canvas` export.
const MAX_TITLE_LEN: usize = 120;
/// Longest accepted legacy/general `config` JSON blob. Living-answer content is persisted only in
/// its provenance-separated columns after the backend has sanitized the add request.
const MAX_CONFIG_LEN: usize = 8 * 1024;
/// Rows shown inside a list-shaped tile (reminders / promises / drift steps / numbers).
const TILE_ROWS: usize = 6;
/// How many recent mentions the Pulse tile samples.
const PULSE_SAMPLE: i64 = 200;

// ── DTOs ───────────────────────────────────────────────────────────────────────────────────────

/// One board in the LIST view. `tileKinds` drives the miniature preview without paying for a
/// single gated payload read — it is layout metadata (kind + span), never content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSummaryDto {
    #[serde(flatten)]
    pub dashboard: Dashboard,
    pub tile_count: i64,
    pub tile_kinds: Vec<TilePreviewDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TilePreviewDto {
    pub kind: String,
    pub span: i64,
}

/// A board plus every tile, each already resolved (and gated).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardDetailDto {
    #[serde(flatten)]
    pub dashboard: Dashboard,
    pub tiles: Vec<ResolvedTileDto>,
    /// Device-private task references resolved from SQLCipher at read time. Tasks never enter the
    /// dashboard Ask corpus; this is a separate Work projection for navigation and execution.
    pub work: Vec<super::tasks::TaskDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedTileDto {
    #[serde(flatten)]
    pub tile: DashboardTile,
    pub data: TileData,
}

/// One row inside a list-shaped tile. Deliberately flat + display-ready so the FE renders it
/// without re-deriving anything from content it should not hold.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TileRow {
    /// The primary line.
    pub text: String,
    /// Secondary/meta line (who, when, source title).
    pub meta: Option<String>,
    /// A short status word the FE maps to a pill (`ok` / `late` / `due` / `open`).
    pub status: Option<String>,
    /// Where this row came from, for click-through. `None` when the row is not navigable.
    pub source: Option<SourceRef>,
}

/// The resolved payload of a tile, discriminated by `kind` on the wire.
///
/// BOTH rename attributes are load-bearing and they do different jobs. On an ENUM,
/// `rename_all` renames the VARIANTS (`Meeting` → `"meeting"`, the `kind` tag the FE switches
/// on); `rename_all_fields` renames the FIELDS INSIDE each variant. With only the first, this
/// enum shipped `{"kind":"meeting","started_at":…,"duration_s":…,"has_audio":…}` while
/// `models.ts` declares `startedAt` / `durationS` / `hasAudio` — so every one of them read
/// `undefined` in the browser, silently, with both sides "typed".
///
/// The damage was not cosmetic and it did not surface here. `DashboardTileComponent.formatDate`
/// falls back to `iso.slice(0, 10)` when `Date.parse` returns NaN, so an `undefined` timestamp
/// THREW — and an exception from a template binding aborts the rest of that change-detection
/// pass, blanking every binding after it, including the Add-a-tile palette rendered later in the
/// same pass by `app-shell`. Boards with only note tiles were unaffected (note timestamps merely
/// read "recently" forever), which is why it presented as "I can't add a tile once I have more
/// than two" and cost six fixes aimed at the palette's positioning.
///
/// Pinned by `every_tile_payload_field_is_camel_case_on_the_wire` in `dashboard_cmd_tests` —
/// against the real serializer, because the e2e fixtures are written from the TS type and so can
/// only ever assert the shape they already assume.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TileData {
    /// The tile's source exists but is sealed and not session-unlocked. Carries NOTHING.
    Locked,
    /// The tile's source row is gone (deleted, or never existed).
    Missing,
    /// A tile kind that needs a `refId` but was stored without one.
    Unconfigured,
    Note {
        id: String,
        title: String,
        snippet: String,
        updated_at: i64,
    },
    Meeting {
        id: String,
        title: String,
        started_at: String,
        duration_s: i64,
        has_audio: bool,
    },
    Document {
        id: String,
        title: String,
        snippet: String,
    },
    Person {
        id: String,
        name: String,
        mention_count: i64,
        open_commitments: i64,
    },
    Reminders {
        rows: Vec<TileRow>,
        due_count: i64,
    },
    /// The bitemporal drift of ONE predicate on an entity: how a value moved over time.
    Drift {
        entity: String,
        predicate: String,
        rows: Vec<TileRow>,
    },
    /// Currently-valid facts whose object reads as a figure, with the value they superseded.
    Numbers {
        entity: String,
        rows: Vec<TileRow>,
    },
    /// Mentions per week + how long the entity has been quiet.
    Pulse {
        entity: String,
        weekly: Vec<i64>,
        total: i64,
        quiet_days: Option<i64>,
    },
    /// Open commitments ("who owes what"), optionally scoped to one owner.
    Promises {
        owner: Option<String>,
        rows: Vec<TileRow>,
    },
    /// A pinned question plus the answer last computed by the backend-owned refresh command.
    ///
    /// `withheld` is the lock-model half: a cached answer is a PARAPHRASE of the sources it was
    /// built from, so once any of those sources is sealed-and-not-unlocked the answer stops being
    /// returned (and `answer` is `None`) until they are unlocked again.
    LivingAnswer {
        question: String,
        answer: Option<String>,
        answered_at: Option<String>,
        withheld: bool,
    },
}

/// Compatibility/request shape for historical tile configs and Living-answer creation. Resolved
/// Living-answer content never hydrates this bag; it comes from dedicated provenance-gated columns.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TileConfig {
    /// `drift` — which predicate to track. Absent ⇒ the entity's most-superseded predicate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
    /// `promises` — restrict to one owner name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// `living_answer` — the pinned question and its last answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    /// Backend-owned readable-folder snapshot for the question itself. `None` is legacy and
    /// unprovable; `Some([])` is a valid question created while no folders were readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_readable_folders: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<String>,
    /// `living_answer` — the board's tile sources at answer time. Kept for provenance display;
    /// it is NOT the gate (see `answer_readable_folders` for why it cannot be).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_sources: Option<Vec<SourceRef>>,
    /// `living_answer` — every folder that was READABLE when the answer was computed.
    ///
    /// This, not the source list, is what makes a cached answer gate-able. `ask_vault`'s pinned
    /// path does not answer from the pinned sources alone: `summarize/vault_context.rs`
    /// (`build_vault_context_pinned_visible`) additionally packs up to `LINK_CONTEXT_CAP` linked
    /// NEIGHBOURS into the corpus, and the note/document arm of that expansion does not even
    /// report them back in `AskVaultResult.sources`. So no list of sources the caller knows about
    /// can bound what the answer paraphrases.
    ///
    /// A folder set can. The answer could only ever have drawn on content that was readable at the
    /// time, so recording the readable set and withholding the moment any of it stops being
    /// readable is sound regardless of how retrieval expanded. Absent ⇒ un-gateable ⇒ withheld.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_readable_folders: Option<Vec<String>>,
}

/// Every folder the session can currently read: unlocked outright, or sealed but session-unlocked.
fn readable_folder_ids(
    db: &crate::storage::Db,
    unlocked: &std::collections::HashSet<String>,
) -> Result<std::collections::HashSet<String>, AppError> {
    Ok(db.visible_folder_ids(unlocked)?.into_iter().collect())
}

// ── helpers ────────────────────────────────────────────────────────────────────────────────────

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn clean_title(raw: &str, field: &str) -> Result<String, AppError> {
    let t = raw.trim();
    if t.is_empty() {
        return Err(AppError::InvalidArg(format!("{field} is empty")));
    }
    if t.chars().count() > MAX_TITLE_LEN {
        return Err(AppError::InvalidArg(format!(
            "{field} is longer than {MAX_TITLE_LEN} characters"
        )));
    }
    Ok(t.to_string())
}

/// Cosmetic emoji: at most 2 grapheme-ish chars, no control characters, never persisted raw HTML.
fn clean_emoji(raw: Option<String>) -> Option<String> {
    let e = raw?;
    let e = e.trim();
    if e.is_empty() || e.chars().count() > 4 || e.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(e.to_string())
}

/// Tint is a design-token NAME, never a raw colour — the FE maps it to `var(--accent-*)`.
const TINTS: &[&str] = &["indigo", "amber", "mint", "orchid", "azure", "coral"];
fn clean_tint(raw: Option<String>) -> Option<String> {
    let t = raw?.trim().to_ascii_lowercase();
    TINTS.contains(&t.as_str()).then_some(t)
}

fn parse_config(raw: Option<&str>) -> TileConfig {
    raw.and_then(|s| serde_json::from_str::<TileConfig>(s).ok())
        .unwrap_or_default()
}

fn sanitize_living_answer_config_for_add(
    raw: Option<&str>,
    readable: &std::collections::HashSet<String>,
) -> Result<String, AppError> {
    let mut cfg = raw
        .map(serde_json::from_str::<TileConfig>)
        .transpose()
        .map_err(|e| AppError::InvalidArg(format!("invalid tile config: {e}")))?
        .unwrap_or_default();
    cfg.question = Some(clean_title(
        cfg.question.as_deref().unwrap_or_default(),
        "question",
    )?);
    cfg.question_readable_folders = Some(readable.iter().cloned().collect());
    cfg.answer = None;
    cfg.answered_at = None;
    cfg.answer_sources = None;
    cfg.answer_readable_folders = None;
    serde_json::to_string(&cfg)
        .map_err(|e| AppError::Storage(format!("encoding tile config failed: {e}")))
}

fn snippet_of(text: &str, max: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    let cut: String = t.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

/// Does this object read as a figure? Deliberately conservative: a leading digit, currency symbol,
/// or a date-ish token. Used ONLY to pick which facts the Numbers tile shows.
fn looks_numeric(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    let first = t.chars().next().unwrap_or(' ');
    first.is_ascii_digit()
        || matches!(first, '$' | '€' | '£' | '+' | '-')
        || t.chars().filter(|c| c.is_ascii_digit()).count() >= 2
}

fn iso_to_epoch_secs(iso: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|d| d.timestamp())
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(iso, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|d| d.and_utc().timestamp())
        })
}

// ── board CRUD ─────────────────────────────────────────────────────────────────────────────────

/// Every board with its layout metadata — GATED.
///
/// The doc here used to say no gated read happens, because a summary carries only the user's own
/// chrome. That was true while a board could not live in a folder: with no container there was no
/// lock that could cover it. Now that a board can be filed, its own title is content — "Q3 layoffs"
/// names the thing whether or not a single tile is readable — so a board whose container is sealed
/// and not session-unlocked comes back masked.
#[tauri::command]
pub async fn list_dashboards(app: AppHandle) -> Result<Vec<DashboardSummaryDto>, AppError> {
    offload_read(app, list_dashboards_inner).await
}

/// Body of [`list_dashboards`], taking `&AppState`.
///
/// Split out because the sealed-board masking below lives HERE, at the command layer, and a gate
/// no test can reach is a gate whose tests end up aimed one layer down — which is exactly what
/// happened: the first round asserted on `Db::list_dashboards_visible`, which masks the row, and
/// stayed green while the tile count and kinds assembled here still shipped.
pub(crate) fn list_dashboards_inner(
    state: &AppState,
) -> Result<Vec<DashboardSummaryDto>, AppError> {
    // Hold the lifecycle guard across the gate AND the tile read, for the same reason
    // `get_dashboard` does: a relock landing between the two would judge tiles against a
    // snapshot that is already stale.
    let _lifecycle = super::lifecycle_guard(state);
    let unlocked = crate::commands::unlocked_snapshot(state)?;
    let boards = state.db.list_dashboards_visible(&unlocked)?;
    let kinds = state.db.dashboard_tile_kinds()?;
    let out = boards
        .into_iter()
        .map(|d| {
            // A sealed board discloses NO tiles — not their kinds and not their number.
            // `list_dashboards_visible` masks the row, but the summary is assembled from a
            // SEPARATE ungated read, so masking the row alone still shipped "this locked board
            // is built from three meetings and a note". Tile shape is exactly the kind of
            // structural fact a lock exists to withhold.
            if d.locked {
                return DashboardSummaryDto {
                    tile_count: 0,
                    tile_kinds: Vec::new(),
                    dashboard: d,
                };
            }
            let tile_kinds: Vec<TilePreviewDto> = kinds
                .iter()
                .filter(|(board_id, _, _)| board_id == &d.id)
                .map(|(_, kind, span)| TilePreviewDto {
                    kind: kind.clone(),
                    span: *span,
                })
                .collect();
            DashboardSummaryDto {
                tile_count: tile_kinds.len() as i64,
                tile_kinds,
                dashboard: d,
            }
        })
        .collect();
    Ok(out)
}

#[tauri::command]
pub fn create_dashboard(
    state: State<'_, AppState>,
    title: String,
    emoji: Option<String>,
    tint: Option<String>,
    folder_id: Option<String>,
) -> Result<Dashboard, AppError> {
    let _lifecycle = super::lifecycle_guard(state.inner());
    if state.db.dashboard_count()? >= MAX_DASHBOARDS {
        return Err(AppError::InvalidArg(format!(
            "dashboard limit reached ({MAX_DASHBOARDS})"
        )));
    }
    let title = clean_title(&title, "title")?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso();
    // A board created INTO a sealed container would be born readable inside a sealed tree —
    // the same hazard the container-creation guard closes for folders. The seal is per
    // container and runs on lock, so refuse rather than create something the existing lock
    // will not cover.
    if let Some(folder) = folder_id.as_deref() {
        // EXISTENCE first. `folder_is_unlocked` answers "is this folder sealed", and an id that
        // names nothing is not sealed — so the guard passed and the board was inserted with a
        // dangling `folder_id`: invisible in the tree (the LEFT JOIN finds no row) and outside
        // any future lock, which is precisely the state the container anchor exists to prevent.
        if state.db.folder_by_id(folder)?.is_none() {
            return Err(AppError::InvalidArg(format!("no such container: {folder}")));
        }
        if !crate::commands::folder_is_unlocked(state.inner(), folder)? {
            return Err(AppError::Locked(
                "unlock this folder before creating a dashboard in it".into(),
            ));
        }
    }
    state.db.insert_dashboard_in_folder(
        &id,
        &title,
        clean_emoji(emoji).as_deref(),
        clean_tint(tint).as_deref(),
        folder_id.as_deref(),
        &now,
    )?;
    state
        .db
        .get_dashboard(&id)?
        .ok_or_else(|| AppError::Storage("dashboard vanished after insert".into()))
}

#[tauri::command]
pub fn update_dashboard(
    state: State<'_, AppState>,
    id: String,
    title: Option<String>,
    emoji: Option<String>,
    tint: Option<String>,
    pinned: Option<bool>,
) -> Result<Dashboard, AppError> {
    update_dashboard_inner(
        state.inner(),
        &id,
        title.as_deref(),
        emoji,
        tint,
        pinned,
    )
}

pub(crate) fn update_dashboard_inner(
    state: &AppState,
    id: &str,
    title: Option<&str>,
    emoji: Option<String>,
    tint: Option<String>,
    pinned: Option<bool>,
) -> Result<Dashboard, AppError> {
    let _lifecycle = super::lifecycle_guard(state);
    require_board_writable(state, id)?;
    let title = title.map(str::to_string);
    let title = match title {
        Some(t) => Some(clean_title(&t, "title")?),
        None => None,
    };
    let found = state.db.update_dashboard(
        id,
        title.as_deref(),
        clean_emoji(emoji).as_deref(),
        clean_tint(tint).as_deref(),
        pinned,
        &now_iso(),
    )?;
    if !found {
        return Err(AppError::InvalidArg(format!("no dashboard with id {id}")));
    }
    state
        .db
        .get_dashboard(id)?
        .ok_or_else(|| AppError::Storage("dashboard vanished after update".into()))
}

#[tauri::command]
pub fn delete_dashboard(state: State<'_, AppState>, id: String) -> Result<bool, AppError> {
    delete_dashboard_inner(state.inner(), &id)
}

pub(crate) fn delete_dashboard_inner(state: &AppState, id: &str) -> Result<bool, AppError> {
    let _lifecycle = super::lifecycle_guard(state);
    // Deleting is a write like any other, and it is the one that cannot be undone: a sealed
    // board's plaintext lives only in its blobs, and the row carries the pointer to them.
    require_board_writable(state, id)?;
    state.db.delete_dashboard(id)
}

#[tauri::command]
pub fn reorder_dashboards(state: State<'_, AppState>, ids: Vec<String>) -> Result<(), AppError> {
    reorder_dashboards_inner(state.inner(), &ids)
}

pub(crate) fn reorder_dashboards_inner(
    state: &AppState,
    ids: &[String],
) -> Result<(), AppError> {
    let _lifecycle = super::lifecycle_guard(state);
    // Ordering is board-level position, not content, so this refuses only if the CALLER named a
    // board it may not touch — a sealed board must not be draggable into a new position by a
    // caller that cannot see it.
    for id in ids {
        require_board_writable(state, id)?;
    }
    state.db.reorder_dashboards(ids)
}

// ── tile CRUD ──────────────────────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn add_dashboard_tile(
    state: State<'_, AppState>,
    dashboard_id: String,
    kind: String,
    ref_id: Option<String>,
    title: Option<String>,
    span: Option<i64>,
    config: Option<String>,
) -> Result<(), AppError> {
    add_dashboard_tile_inner(state.inner(), dashboard_id, kind, ref_id, title, span, config)
}

pub(crate) fn add_dashboard_tile_inner(
    state: &AppState,
    dashboard_id: String,
    kind: String,
    ref_id: Option<String>,
    title: Option<String>,
    span: Option<i64>,
    config: Option<String>,
) -> Result<(), AppError> {
    let _lifecycle = super::lifecycle_guard(state);
    require_board_writable(state, &dashboard_id)?;
    if !TILE_KINDS.contains(&kind.as_str()) {
        return Err(AppError::InvalidArg(format!("unknown tile kind: {kind}")));
    }
    if state.db.get_dashboard(&dashboard_id)?.is_none() {
        return Err(AppError::InvalidArg(format!(
            "no dashboard with id {dashboard_id}"
        )));
    }
    if state.db.dashboard_tile_count(&dashboard_id)? >= MAX_TILES_PER_BOARD {
        return Err(AppError::InvalidArg(format!(
            "tile limit reached ({MAX_TILES_PER_BOARD} per board)"
        )));
    }
    let title = match title {
        Some(t) if !t.trim().is_empty() => Some(clean_title(&t, "tile title")?),
        _ => None,
    };
    let title = (kind != "living_answer").then_some(title).flatten();
    if let Some(c) = config.as_deref() {
        if c.len() > MAX_CONFIG_LEN {
            return Err(AppError::InvalidArg("tile config too large".into()));
        }
        serde_json::from_str::<TileConfig>(c)
            .map_err(|e| AppError::InvalidArg(format!("invalid tile config: {e}")))?;
    }
    let mut question_provenance = None;
    let config = if kind == "living_answer" {
        let unlocked = super::unlocked_snapshot(state)?;
        let readable = readable_folder_ids(&state.db, &unlocked)?;
        let encoded = sanitize_living_answer_config_for_add(config.as_deref(), &readable)?;
        let question = parse_config(Some(&encoded)).question.unwrap_or_default();
        question_provenance = Some((
            question,
            serde_json::to_string(&readable)
                .map_err(|e| AppError::Storage(format!("encoding provenance failed: {e}")))?,
        ));
        None
    } else {
        config
    };
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso();
    if let Some((question, provenance)) = question_provenance.as_ref() {
        state.db.insert_dashboard_living_answer_tile(
            &id,
            &dashboard_id,
            span.unwrap_or(4),
            question,
            provenance,
            &now,
        )?;
        state.db.touch_dashboard(&dashboard_id, &now)?;
        return Ok(());
    }
    state.db.insert_dashboard_tile(
        &id,
        &dashboard_id,
        &kind,
        ref_id.as_deref().filter(|s| !s.trim().is_empty()),
        title.as_deref(),
        span.unwrap_or(4).clamp(MIN_SPAN, MAX_SPAN),
        config.as_deref(),
        &now,
    )?;
    state.db.touch_dashboard(&dashboard_id, &now)?;
    // Deliberately returns nothing. Handing back the stored row would ship `title`/`config`
    // unredacted — the same "on the wire regardless" hole `redact_tile_chrome` closes for
    // `get_dashboard`, on a command it does not cover. The FE reloads the board anyway.
    Ok(())
}

#[tauri::command]
pub fn update_dashboard_tile(
    state: State<'_, AppState>,
    id: String,
    title: Option<String>,
    span: Option<i64>,
    config: Option<String>,
) -> Result<(), AppError> {
    update_dashboard_tile_inner(state.inner(), id, title, span, config)
}

pub(crate) fn update_dashboard_tile_inner(
    state: &AppState,
    id: String,
    title: Option<String>,
    span: Option<i64>,
    config: Option<String>,
) -> Result<(), AppError> {
    let _lifecycle = super::lifecycle_guard(state);
    require_tile_writable(state, &id)?;
    let Some((dashboard_id, existing_kind)) = state.db.dashboard_tile_metadata(&id)? else {
        return Err(AppError::InvalidArg(format!("no tile with id {id}")));
    };
    if existing_kind == "living_answer" && config.is_some() {
        return Err(AppError::InvalidArg(
            "living-answer config is backend-owned".into(),
        ));
    }
    let title = match title {
        Some(t) if !t.trim().is_empty() => Some(clean_title(&t, "tile title")?),
        _ => None,
    };
    if let Some(c) = config.as_deref() {
        if c.len() > MAX_CONFIG_LEN {
            return Err(AppError::InvalidArg("tile config too large".into()));
        }
        serde_json::from_str::<TileConfig>(c)
            .map_err(|e| AppError::InvalidArg(format!("invalid tile config: {e}")))?;
    }
    state
        .db
        .update_dashboard_tile(&id, title.as_deref(), span, config.as_deref())?;
    state.db.touch_dashboard(&dashboard_id, &now_iso())?;
    // Same reason as `add_dashboard_tile`: never return the raw row. Reachable proof this
    // mattered — the Arrange-mode resize control fires for a SEALED tile, so returning the row
    // would push that tile's stored title/config into the webview.
    Ok(())
}

#[tauri::command]
pub fn delete_dashboard_tile(state: State<'_, AppState>, id: String) -> Result<bool, AppError> {
    delete_dashboard_tile_inner(state.inner(), id)
}

pub(crate) fn delete_dashboard_tile_inner(
    state: &AppState,
    id: String,
) -> Result<bool, AppError> {
    let _lifecycle = super::lifecycle_guard(state);
    require_tile_writable(state, &id)?;
    let board = state
        .db
        .dashboard_tile_metadata(&id)?
        .map(|(dashboard_id, _kind)| dashboard_id);
    let removed = state.db.delete_dashboard_tile(&id)?;
    if let Some(b) = board {
        state.db.touch_dashboard(&b, &now_iso())?;
    }
    Ok(removed)
}

#[tauri::command]
pub fn reorder_dashboard_tiles(
    state: State<'_, AppState>,
    dashboard_id: String,
    tile_ids: Vec<String>,
) -> Result<(), AppError> {
    reorder_dashboard_tiles_inner(state.inner(), dashboard_id, tile_ids)
}

pub(crate) fn reorder_dashboard_tiles_inner(
    state: &AppState,
    dashboard_id: String,
    tile_ids: Vec<String>,
) -> Result<(), AppError> {
    let _lifecycle = super::lifecycle_guard(state);
    require_board_writable(state, &dashboard_id)?;
    state.db.reorder_dashboard_tiles(&dashboard_id, &tile_ids)?;
    state.db.touch_dashboard(&dashboard_id, &now_iso())
}

// ── the board, resolved ────────────────────────────────────────────────────────────────────────

/// Refuse any WRITE against a board whose container is sealed and not session-unlocked.
///
/// Before this change a board could not live in a folder at all, so no dashboard mutation needed a
/// lock gate and none had one — the reads were the whole surface. Giving boards a container made
/// every one of those writes a way to put user content inside a sealed tree: a renamed board, a
/// new tile, a reordered layout. Each would be written in PLAINTEXT into a folder the user has
/// been told is unreadable, and the seal has already run, so nothing would come back to encrypt
/// it. The next unseal would then overwrite it from the stored blob, so the write is also lost.
///
/// A board with no container is unaffected: there is no folder whose key would seal it, so there
/// is nothing to be inside of.
fn require_board_writable(state: &AppState, dashboard_id: &str) -> Result<(), AppError> {
    let Some(board) = state.db.get_dashboard(dashboard_id)? else {
        return Ok(()); // absent board — the caller's own not-found path reports it.
    };
    let Some(folder) = board.folder_id.as_deref() else {
        return Ok(());
    };
    if !crate::commands::folder_is_unlocked(state, folder)? {
        return Err(AppError::Locked(
            "unlock this board's container before changing it".into(),
        ));
    }
    Ok(())
}

/// The tile twin of [`require_board_writable`], resolving the tile's board first.
fn require_tile_writable(state: &AppState, tile_id: &str) -> Result<(), AppError> {
    let Some((dashboard_id, _kind)) = state.db.dashboard_tile_metadata(tile_id)? else {
        return Ok(()); // absent tile — the caller reports not-found.
    };
    require_board_writable(state, &dashboard_id)
}

/// Move a board into a container, or unfile it with `folderId: null`.
///
/// Both ends are gated, and they refuse for different reasons.
///
/// The TARGET must be unlocked, because a board arriving in a sealed container would land in
/// plaintext inside a tree the user has been told is unreadable — the same hazard
/// `create_dashboard` refuses, and the reason `move_note` refuses a sealed destination it has no
/// key for.
///
/// The SOURCE must be unlocked too, and that one is easy to miss: a board sitting in a sealed
/// container has its title and tiles in ciphertext, bound by `aad_document` to THAT container's
/// id. Moving the row without unsealing it would carry blobs into a container whose key cannot
/// open them and whose unseal enumerates them — content that can never be recovered. Unlock the
/// source first and the ordinary session-unlock has already restored the plaintext, so the move
/// is a plain re-parent.
#[tauri::command]
pub fn move_dashboard_to_container(
    state: State<'_, AppState>,
    id: String,
    folder_id: Option<String>,
) -> Result<(), AppError> {
    move_dashboard_to_container_inner(state.inner(), &id, folder_id.as_deref())
}

/// Body of [`move_dashboard_to_container`], taking `&AppState` so the two refusals above can be
/// driven directly. A gate whose only caller needs a Tauri `State` is a gate no test can bind.
pub(crate) fn move_dashboard_to_container_inner(
    state: &AppState,
    id: &str,
    folder_id: Option<&str>,
) -> Result<(), AppError> {
    let _lifecycle = super::lifecycle_guard(state);
    let unlocked = super::unlocked_snapshot(state)?;
    let Some(board) = state.db.get_dashboard_visible(id, &unlocked)? else {
        return Err(AppError::InvalidArg(format!("no such dashboard: {id}")));
    };
    if board.locked {
        return Err(AppError::Locked(
            "unlock this board's container before moving it".into(),
        ));
    }
    if let Some(folder) = folder_id {
        if state.db.folder_by_id(folder)?.is_none() {
            return Err(AppError::InvalidArg(format!("no such container: {folder}")));
        }
        if !crate::commands::folder_is_unlocked(state, folder)? {
            return Err(AppError::Locked(
                "unlock this container before moving a dashboard into it".into(),
            ));
        }
    }
    if !state.db.set_dashboard_folder(id, folder_id)? {
        return Err(AppError::InvalidArg(format!("no such dashboard: {id}")));
    }
    Ok(())
}

/// One board with every tile resolved through the gated readers.
#[tauri::command]
pub async fn get_dashboard(app: AppHandle, id: String) -> Result<Response, AppError> {
    offload_read(app, move |state| {
        let payload = get_dashboard_inner(state, &id)?;
        serde_json::to_string(&payload)
            .map(Response::new)
            .map_err(|_| AppError::Unavailable("dashboard response encoding failed".into()))
    })
    .await
}

/// Body of [`get_dashboard`], returning the DTO rather than its serialization.
///
/// Split for the same reason as [`list_dashboards_inner`]: the sealed-board gate below is a
/// command-layer decision, and a gate reachable only through a `Response` string is one whose
/// tests end up asserting on the storage layer instead — where the masking it performs does not
/// live.
pub(crate) fn get_dashboard_inner(
    state: &AppState,
    id: &str,
) -> Result<Option<DashboardDetailDto>, AppError> {
    // TOCTOU: hold the lock lifecycle guard across the gate AND every tile read, exactly like
    // `commands/meetings.rs::get_meeting_detail`. Without it a relock landing between the
    // `unlocked_snapshot` below and a later tile's read would resolve that tile against a stale
    // "unlocked" view and return content from a folder that is sealed by the time it ships.
    let _lifecycle = super::lifecycle_guard(state);
    let unlocked_for_board = super::unlocked_snapshot(state)?;
    let Some(dashboard) = state.db.get_dashboard_visible(id, &unlocked_for_board)? else {
        return Ok(None);
    };
    // A sealed board returns its MASKED row and NO tiles at all.
    //
    // Masking the row alone was not enough, and `list_dashboards` was changed in this same diff
    // for exactly this reason: the tile columns are blank at rest, but the tile ROWS still exist,
    // so resolving them shipped one entry per tile — the count, and each tile's span and position.
    // "This locked board is built from three things, laid out like this" is a fact about sealed
    // content, and shape is one of the easier things to recognise a board by.
    if dashboard.locked {
        return Ok(Some(DashboardDetailDto {
            dashboard,
            tiles: Vec::new(),
            // The Work projection goes too. It is the board's task references, so it discloses
            // both how many there are and which tasks a sealed board is about — the same class of
            // fact as the tile shape, reached by a different field.
            work: Vec::new(),
        }));
    }
    let tiles = state.db.list_dashboard_tile_structures(id)?;
    let unlocked = super::unlocked_snapshot(state)?;
    let resolved = tiles
        .into_iter()
        .map(|tile| {
            let data = resolve_tile(&state.db, &tile, &unlocked)?;
            let mut tile = DashboardTile {
                title: None,
                ..tile
            };
            tile.config = None;
            Ok(ResolvedTileDto { tile, data })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let work = dashboard_work_inner(state, id)?;
    Ok(Some(DashboardDetailDto {
        dashboard,
        tiles: resolved,
        work,
    }))
}

pub(crate) fn dashboard_work_inner(
    state: &AppState,
    dashboard_id: &str,
) -> Result<Vec<super::tasks::TaskDto>, AppError> {
    let task_org_ids = state.db.dashboard_task_org_ids(dashboard_id)?;
    if task_org_ids.is_empty() || super::session_server_user_id(state).is_err() {
        return Ok(Vec::new());
    }
    for org_id in task_org_ids {
        super::tasks::require_task_read_context(state, &org_id)?;
    }
    state
        .db
        .dashboard_task_rows(dashboard_id)?
        .into_iter()
        .map(|row| super::tasks::task_dto(state, row))
        .collect()
}

/// Could this tile's payload NOT be fully resolved for the current session?
///
/// ONE definition, deliberately shared by [`redact_tile_chrome`] (which strips the
/// stored title/config) and [`dashboard_brief_inner`] (which drops the tile from the
/// prompt entirely). Two copies of this predicate would drift, and the direction
/// they drift in is a leak: a brief that used a looser notion of "withheld" than
/// the redactor would put a sealed tile's material into a model call.
///
/// The `Drift`/`Numbers`/`Pulse` arm is the subtle one. Those kinds degrade to an
/// EMPTY VIEW rather than to `Locked` when their entity stops being visible, so
/// the payload still carries `weekly` / `total` / `quiet_days`. Keying on
/// `entity == ENTITY_HIDDEN` is what stops a "quiet for 9d" chip from leaking
/// timing about a sealed entity.
pub(crate) fn tile_is_withheld(data: &TileData) -> bool {
    match data {
        TileData::Locked | TileData::Missing | TileData::Unconfigured => true,
        // `entity_name` yields the placeholder when the entity is not visible.
        TileData::Drift { entity, .. }
        | TileData::Numbers { entity, .. }
        | TileData::Pulse { entity, .. } => entity == ENTITY_HIDDEN,
        TileData::LivingAnswer { withheld, .. } => *withheld,
        _ => false,
    }
}

/// Strip the tile's stored chrome whenever its payload could NOT be fully resolved.
///
/// Defense-in-depth for a leak found by review (2026-08-03). The first line of defence is that the
/// FE no longer persists a source-derived title at all (see `tile-palette.component.ts`), so
/// `dashboard_tiles.title` should only ever hold something the user typed. This is the second: any
/// row written by an OLDER build still carries a copied source title, and `ResolvedTileDto`
/// flattens the whole tile — so a withheld payload shipped alongside that title would put the
/// sealed source's name on the wire regardless.
///
/// Withheld means: `Locked` / `Missing` / `Unconfigured`, **and** the entity-anchored kinds whose
/// entity is not currently visible — those deliberately degrade to an empty view rather than to
/// `Locked`, so they would otherwise keep a stale entity name as their heading.
///
/// `ref_id`, `span` and `position` stay: pure layout, no content, and the FE needs them to keep the
/// board's shape stable while a folder is sealed.
pub(crate) fn redact_tile_chrome(mut tile: DashboardTile, data: &TileData) -> DashboardTile {
    let withheld = tile_is_withheld(data);
    if withheld {
        tile.title = None;
    }
    // Config is an internal persistence envelope (and can contain cached derived text). TileData
    // is the only authorized wire projection; no caller needs the raw config.
    tile.config = None;
    tile
}

/// The board's currently visible material pointers. This IPC is retained for non-AI UI consumers;
/// dashboard Ask resolves material and derived context together through `dashboard_composite_context`.
#[tauri::command]
pub fn get_dashboard_sources(state: State<'_, AppState>, id: String) -> Result<Response, AppError> {
    let payload = get_dashboard_sources_inner(state.inner(), &id)?;
    serde_json::to_string(&payload)
        .map(Response::new)
        .map_err(|_| AppError::Unavailable("dashboard response encoding failed".into()))
}

/// Body of [`get_dashboard_sources`], returning the list rather than its serialization.
///
/// Split for the same reason as its siblings: the board-level gate below is the thing worth
/// testing, and a gate reachable only through a `Response` string is one whose test ends up
/// aimed at the resolver instead — which withholds source CONTENT, not the list.
pub(crate) fn get_dashboard_sources_inner(
    state: &AppState,
    id: &str,
) -> Result<Vec<SourceRef>, AppError> {
    // Same TOCTOU discipline as `get_dashboard`: gate and read under one lifecycle guard.
    let _lifecycle = super::lifecycle_guard(state);
    let unlocked = super::unlocked_snapshot(state)?;
    // And the same BOARD-level gate. Each source below is resolved through the visibility set, so
    // no individual source leaks — but the LIST itself is a fact about a sealed board: how many
    // things it is built from, and that it is built from anything at all. `get_dashboard` was
    // given this early return in the same diff; a second read path that skipped it would be the
    // hole the first one closed.
    let sealed = state
        .db
        .get_dashboard_visible(id, &unlocked)?
        .is_some_and(|board| board.locked);
    if sealed {
        return Ok(Vec::new());
    }
    let tiles = state.db.list_dashboard_tile_structures(id)?;
    dashboard_sources_inner(&state.db, tiles, &unlocked)
}

/// The header the board brief is packed under. Instructional on purpose: without
/// the second clause the model re-derives the ledger it was just handed, and the
/// answer reads as if the board were not there.
const BRIEF_HEADER: &str =
    "WHAT THIS BOARD ALREADY SHOWS (the user composed these views; do not re-derive them):";

/// Flat hard cap on the brief. [`brief_allowance`] additionally limits it to one
/// quarter of the resolved corpus budget, so small providers retain source room.
const MAX_BRIEF_CHARS: usize = 4000;

/// What a board's DERIVED tiles SAY, as prompt text.
///
/// Material pointers pack as full gated sources; this renderer supplies the other derived views
/// to `dashboard_composite_context`, so the provider sees the same composed board the user sees.
///
/// This adds no new gated reader and no new egress class. It walks the SHIPPED
/// path (`resolve_tile` → [`tile_is_withheld`] → `render_tile_for_agent`), which is
/// the same one the MCP surface already reads, so the two cannot disagree about
/// what a tile says.
#[cfg(test)]
pub(crate) fn dashboard_brief_inner(
    db: &crate::storage::Db,
    tiles: Vec<DashboardTile>,
    unlocked: &std::collections::HashSet<String>,
    max_chars: usize,
) -> Result<String, AppError> {
    let lines = dashboard_brief_lines(db, tiles, unlocked)?;
    Ok(render_dashboard_brief(&lines, max_chars))
}

/// Resolve every derived tile exactly once. The complete canonical render and its budget-capped
/// provider projection must be two views of these same values, not two independent DB read passes.
fn dashboard_brief_lines(
    db: &crate::storage::Db,
    tiles: Vec<DashboardTile>,
    unlocked: &std::collections::HashSet<String>,
) -> Result<Vec<String>, AppError> {
    let mut lines: Vec<String> = Vec::new();
    for tile in tiles {
        // Material kinds are already packed as SOURCES, with their full text. Repeating
        // a snippet here would spend budget to say something the corpus says better.
        if matches!(tile.kind.as_str(), "note" | "meeting" | "document") {
            continue;
        }
        let data = resolve_tile(db, &tile, unlocked)?;
        // A withheld tile contributes NOTHING — not a title, not a count, not a kind
        // label. `render_tile_for_agent` would print a neutral "[sealed tile]" marker,
        // which is right for MCP (an agent listing a board should know something is
        // there) and pointless here: the prompt gains nothing, and the safest number
        // of bytes to spend on a sealed tile is zero.
        if tile_is_withheld(&data) {
            continue;
        }
        // NO EXTRA FILTER HERE, deliberately.
        //
        // An earlier cut suppressed row-list tiles whose rows were all empty, on the
        // grounds that `- promises` with nothing under it is a heading that says
        // nothing. Review was right that this is wrong, for two reasons:
        //
        //   1. An empty ledger IS an answer. "Nobody currently owes anything" is a
        //      real reply to the board's own suggested question, and suppressing the
        //      tile made the floor fall through to the generic "no meeting notes to
        //      search yet" instead.
        //   2. `get_dashboard` and the MCP surface render the same tile through the
        //      same `render_tile_for_agent`. A filter applied at THIS sink only
        //      recreates the very asymmetry this change exists to remove — board Ask
        //      seeing less of the board than an external agent does — just pointing
        //      the other way.
        //
        // The only suppression is `tile_is_withheld` above, which is the LOCK gate and
        // is shared with `redact_tile_chrome`. If a rendered view should ever change
        // shape, change the shared renderer so every sink changes together.
        let tile = redact_tile_chrome(tile, &data);
        let rendered = render_tile_for_agent(&tile, &data);
        if !rendered.trim().is_empty() {
            lines.push(rendered);
        }
    }
    Ok(lines)
}

fn render_dashboard_brief(lines: &[String], max_chars: usize) -> String {
    if lines.is_empty() {
        return String::new();
    }
    // The cap must bound the OUTPUT, so a budget too small to hold the label yields
    // nothing rather than a header that already breaches it.
    if max_chars < BRIEF_HEADER.chars().count() {
        return String::new();
    }
    let mut out = String::from(BRIEF_HEADER);
    let mut wrote = 0usize;
    for tile_text in lines {
        let remaining = max_chars.saturating_sub(out.chars().count());
        // The whole tile fits — the common case.
        // `+ 1` is the newline this push would add; clippy prefers the strict form.
        if tile_text.chars().count() < remaining {
            out.push('\n');
            out.push_str(tile_text);
            wrote += 1;
            continue;
        }
        // It does not. Dropping it WHOLESALE was a real defect: on a small provider
        // budget a single Promise ledger with enough rows exceeded the allowance and
        // vanished, so the one case this feature exists for produced an empty brief.
        //
        // Keep the heading and as many COMPLETE rows as fit, then say how many were
        // left out. A partial row would be the lie the whole-tile rule guards against;
        // a COUNTED remainder is not — the model can see it is looking at a prefix.
        //
        // `truncate_tile` yields "" when not even ONE complete row fits. Counting that
        // as written left a BARE HEADER — the exact "views that do not exist" lie the
        // empty-board path is careful to avoid, reintroduced by an unconditional
        // increment. Nothing written means nothing counted.
        let partial = truncate_tile(tile_text, remaining);
        if !partial.is_empty() {
            out.push_str(&partial);
            wrote += 1;
        }
        break;
    }
    // A bare header is the same "views that do not exist" lie an empty board is
    // careful to avoid.
    if wrote == 0 {
        return String::new();
    }
    out
}

/// Keep a rendered tile's heading plus whole rows within `budget`, and count the rest.
///
/// Returns `""` when not even the heading and one row fit, so the caller emits nothing
/// rather than a heading with no content under it.
fn truncate_tile(tile_text: &str, budget: usize) -> String {
    let mut rows = tile_text.lines();
    let Some(heading) = rows.next() else {
        return String::new();
    };
    let rows: Vec<&str> = rows.collect();
    let mut kept: Vec<&str> = Vec::new();
    // Reserve room for the marker up front, so adding it can never breach the budget.
    let marker_room = 24;
    let mut used = heading.chars().count() + 1;
    for row in &rows {
        let next = used + row.chars().count() + 1;
        if next + marker_room > budget {
            break;
        }
        used = next;
        kept.push(row);
    }
    if kept.is_empty() {
        return String::new();
    }
    let dropped = rows.len() - kept.len();
    let mut out = format!("\n{heading}");
    for row in kept {
        out.push('\n');
        out.push_str(row);
    }
    if dropped > 0 {
        out.push_str(&format!("\n    · … {dropped} more"));
    }
    out
}

/// Resolve one ID-only dashboard into its complete, current AI scope under the caller's lifecycle
/// interval. Missing/deleted IDs fail closed; an existing but empty/all-sealed board remains a real
/// empty scope and never means vault-wide.
pub(crate) fn dashboard_composite_context(
    db: &crate::storage::Db,
    dashboard_id: &str,
    unlocked: &std::collections::HashSet<String>,
    corpus_budget: usize,
    additional_sources: &[SourceRef],
    excluded_meeting_id: Option<&str>,
) -> Result<DashboardCompositeContext, AppError> {
    dashboard_composite_context_inner(
        db,
        dashboard_id,
        unlocked,
        corpus_budget,
        additional_sources,
        excluded_meeting_id,
        false,
    )
}

/// Living Answers are cached syntheses, not evidence for regenerating themselves. Excluding every
/// Living Answer (not only the target) keeps two answer tiles from recursively conditioning each
/// other and makes the stored corpus digest stable after the cache write.
pub(crate) fn living_answer_composite_context(
    db: &crate::storage::Db,
    dashboard_id: &str,
    unlocked: &std::collections::HashSet<String>,
    corpus_budget: usize,
) -> Result<DashboardCompositeContext, AppError> {
    dashboard_composite_context_inner(db, dashboard_id, unlocked, corpus_budget, &[], None, true)
}

#[allow(clippy::too_many_arguments)]
fn dashboard_composite_context_inner(
    db: &crate::storage::Db,
    dashboard_id: &str,
    unlocked: &std::collections::HashSet<String>,
    corpus_budget: usize,
    additional_sources: &[SourceRef],
    excluded_meeting_id: Option<&str>,
    exclude_living_answers: bool,
) -> Result<DashboardCompositeContext, AppError> {
    use sha2::{Digest, Sha256};

    let (generation, exists_now) = if exclude_living_answers {
        db.dashboard_structural_context_state(dashboard_id)?
    } else {
        db.dashboard_context_state(dashboard_id)?
    };
    if !exists_now {
        return Err(AppError::Locked("dashboard is unavailable".into()));
    }
    let mut tiles = db.list_dashboard_tile_structures(dashboard_id)?;
    if exclude_living_answers {
        tiles.retain(|tile| tile.kind != "living_answer");
    }
    let mut source_refs = additional_sources.to_vec();
    for source in dashboard_sources_inner(db, tiles.clone(), unlocked)? {
        if !source_refs
            .iter()
            .any(|existing| existing.kind == source.kind && existing.id == source.id)
        {
            source_refs.push(source);
        }
    }
    source_refs.retain(|source| {
        excluded_meeting_id.map_or(true, |meeting_id| {
            source.kind != crate::links::LinkKind::Meeting || source.id != meeting_id
        })
    });
    let brief_lines = dashboard_brief_lines(db, tiles, unlocked)?;
    let full_derived_render = render_dashboard_brief(&brief_lines, usize::MAX);
    let brief = render_dashboard_brief(&brief_lines, brief_allowance(corpus_budget));
    let resolved_inputs =
        crate::summarize::vault_context::resolve_vault_context_pinned_visible_inputs(
            db,
            &source_refs,
            unlocked,
        )?;
    const COMPOSITE_SEPARATOR: &str = "\n\n";
    let has_resolved_sources =
        !resolved_inputs.explicit_sources.is_empty() || !resolved_inputs.neighbours.is_empty();
    let separator_budget = usize::from(!brief.is_empty() && has_resolved_sources)
        * COMPOSITE_SEPARATOR.chars().count();
    let source_budget = corpus_budget
        .saturating_sub(brief.chars().count())
        .saturating_sub(separator_budget);
    let (source_corpus, packed_sources) =
        crate::summarize::vault_context::build_vault_context_resolved_visible_with_budget(
            db,
            &resolved_inputs,
            source_budget,
            unlocked,
        )?;
    let packed_corpus = match (brief.is_empty(), source_corpus.is_empty()) {
        (true, _) => source_corpus,
        (_, true) => brief.clone(),
        (false, false) => format!("{brief}{COMPOSITE_SEPARATOR}{source_corpus}"),
    };
    // The provider corpus is intentionally budget-capped; the lifecycle witness is not. Hash the
    // complete canonical gated input graph so a source-tail edit or selected neighbour replacement
    // that falls wholly beyond the packing cutoff still invalidates post-await admission.
    let mut manifest = Sha256::new();
    manifest_field(&mut manifest, "domain", b"murmur:dashboard-full-input:v1");
    manifest_field(&mut manifest, "dashboard_id", dashboard_id.as_bytes());
    manifest_field(
        &mut manifest,
        "corpus_budget",
        corpus_budget.to_string().as_bytes(),
    );
    manifest_field(
        &mut manifest,
        "exclude_living_answers",
        if exclude_living_answers { b"1" } else { b"0" },
    );
    manifest_field(
        &mut manifest,
        "excluded_meeting_id",
        excluded_meeting_id.unwrap_or("").as_bytes(),
    );
    for source in additional_sources {
        manifest_source_ref(&mut manifest, "additional_source", source);
    }
    manifest_field(
        &mut manifest,
        "full_derived_render",
        full_derived_render.as_bytes(),
    );
    manifest_field(&mut manifest, "packed_corpus", packed_corpus.as_bytes());
    for source in &resolved_inputs.explicit_sources {
        manifest_source_ref(&mut manifest, "explicit_source", &source.source);
        manifest_field(
            &mut manifest,
            "explicit_full_input",
            &source.manifest_digest,
        );
    }
    for neighbour in &resolved_inputs.neighbours {
        manifest_source_ref(&mut manifest, "neighbour_source", &neighbour.source);
        manifest_field(
            &mut manifest,
            "neighbour_full_input",
            &neighbour.manifest_digest,
        );
    }
    let input_digest = format!("{:x}", manifest.finalize());
    let ask_dispatch_generation = db.ask_dispatch_generation()?;
    Ok(DashboardCompositeContext {
        witness: DashboardContextWitness {
            dashboard_id: dashboard_id.to_string(),
            generation,
            ask_dispatch_generation,
            input_digest,
            corpus_budget,
            additional_sources: additional_sources.to_vec(),
            excluded_meeting_id: excluded_meeting_id.map(str::to_string),
        },
        packed_corpus,
        packed_sources,
    })
}

/// Canonical unambiguous manifest field: both the label and value are byte-length-prefixed. This
/// avoids delimiter collisions even when authored content contains arbitrary punctuation/newlines.
fn manifest_field(hasher: &mut sha2::Sha256, label: &str, value: &[u8]) {
    use sha2::Digest;

    let label_len = u64::try_from(label.len()).unwrap_or(u64::MAX);
    let value_len = u64::try_from(value.len()).unwrap_or(u64::MAX);
    hasher.update(label_len.to_be_bytes());
    hasher.update(label.as_bytes());
    hasher.update(value_len.to_be_bytes());
    hasher.update(value);
}

fn manifest_source_ref(hasher: &mut sha2::Sha256, role: &str, source: &SourceRef) {
    manifest_field(hasher, "source_role", role.as_bytes());
    manifest_field(hasher, "source_kind", source.kind.as_str().as_bytes());
    manifest_field(hasher, "source_id", source.id.as_bytes());
}

pub(crate) fn require_dashboard_context_witness(
    db: &crate::storage::Db,
    witness: &DashboardContextWitness,
    unlocked: &std::collections::HashSet<String>,
) -> Result<(), AppError> {
    let current = dashboard_composite_context(
        db,
        &witness.dashboard_id,
        unlocked,
        witness.corpus_budget,
        &witness.additional_sources,
        witness.excluded_meeting_id.as_deref(),
    )?;
    let (_, exists_now) = db.dashboard_context_state(&witness.dashboard_id)?;
    if !exists_now || current.witness != *witness {
        return Err(AppError::Locked(
            "dashboard changed while generating the answer".into(),
        ));
    }
    Ok(())
}

fn current_ask_corpus_budget_under_lifecycle(
    state: &AppState,
    witness: &DashboardContextWitness,
) -> Result<usize, AppError> {
    let config = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    let budget = super::resolved_ask_corpus_budget(&config);
    Ok(if witness.excluded_meeting_id.is_some() {
        budget.min(crate::summarize::chat::MAX_PINNED_SOURCE_CHARS)
    } else {
        budget
    })
}

/// Revalidate both the dashboard material and the live Ask-provider packing budget while the
/// caller owns the lifecycle guard. A witness is authorization for one exact provider input, not
/// permission to reconstruct that input later with its now-stale budget.
pub(crate) fn require_current_dashboard_context_witness_under_lifecycle(
    state: &AppState,
    witness: &DashboardContextWitness,
    unlocked: &std::collections::HashSet<String>,
) -> Result<(), AppError> {
    if witness.corpus_budget != current_ask_corpus_budget_under_lifecycle(state, witness)? {
        return Err(AppError::Locked(
            "Ask provider changed while generating the answer".into(),
        ));
    }
    require_dashboard_context_witness(&state.db, witness, unlocked)
}

pub(crate) fn require_living_answer_context_witness(
    db: &crate::storage::Db,
    witness: &DashboardContextWitness,
    unlocked: &std::collections::HashSet<String>,
) -> Result<(), AppError> {
    let current = living_answer_composite_context(
        db,
        &witness.dashboard_id,
        unlocked,
        witness.corpus_budget,
    )?;
    if current.witness != *witness {
        return Err(AppError::Locked(
            "dashboard changed while generating the answer".into(),
        ));
    }
    Ok(())
}

pub(crate) fn require_current_living_answer_context_witness_under_lifecycle(
    state: &AppState,
    witness: &DashboardContextWitness,
    unlocked: &std::collections::HashSet<String>,
) -> Result<(), AppError> {
    if witness.corpus_budget != current_ask_corpus_budget_under_lifecycle(state, witness)? {
        return Err(AppError::Locked(
            "Ask provider changed while generating the answer".into(),
        ));
    }
    require_living_answer_context_witness(&state.db, witness, unlocked)
}

fn decode_folder_provenance(raw: Option<&str>) -> Option<Vec<String>> {
    let folders = serde_json::from_str::<Vec<String>>(raw?).ok()?;
    folders
        .iter()
        .all(|folder| !folder.trim().is_empty())
        .then_some(folders)
}

fn living_answer_withheld() -> TileData {
    TileData::LivingAnswer {
        question: String::new(),
        answer: None,
        answered_at: None,
        withheld: true,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn persist_dashboard_living_answer_after_await_with_dispatch(
    state: &AppState,
    visibility: &super::DurableScopeSnapshot,
    ask_dispatch: &super::AskDispatchSnapshot,
    witness: &DashboardContextWitness,
    readable_folders: &[String],
    dashboard_id: &str,
    tile_id: &str,
    question: &str,
    answer: &str,
    answered_at: &str,
) -> Result<Response, AppError> {
    let _lifecycle = super::lifecycle_guard(state);
    super::require_durable_scope_under_lifecycle(state, visibility)?;
    super::require_current_ask_dispatch_under_lifecycle(state, ask_dispatch)?;
    let unlocked = super::unlocked_snapshot(state)?;
    require_current_living_answer_context_witness_under_lifecycle(state, witness, &unlocked)?;
    let currently_readable = readable_folder_ids(&state.db, &unlocked)?;
    if readable_folders
        .iter()
        .any(|folder| !currently_readable.contains(folder))
    {
        return Err(AppError::Locked(
            "content visibility changed while generating the answer".into(),
        ));
    }
    let readable_json = serde_json::to_string(readable_folders)
        .map_err(|e| AppError::Storage(format!("encoding provenance failed: {e}")))?;
    if !state.db.store_dashboard_living_answer_cas_with_dispatch(
        tile_id,
        dashboard_id,
        question,
        answer,
        answered_at,
        &readable_json,
        witness.generation,
        &witness.input_digest,
        witness.corpus_budget,
        witness.ask_dispatch_generation,
    )? {
        return Err(AppError::Locked(
            "dashboard changed while generating the answer".into(),
        ));
    }
    let payload = serde_json::to_string(&TileData::LivingAnswer {
        question: question.to_string(),
        answer: Some(answer.to_string()),
        answered_at: Some(answered_at.to_string()),
        withheld: false,
    })
    .map_err(|_| AppError::Unavailable("dashboard response encoding failed".into()))?;
    Ok(Response::new(payload))
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn persist_dashboard_living_answer_after_await(
    state: &AppState,
    visibility: &super::DurableScopeSnapshot,
    witness: &DashboardContextWitness,
    readable_folders: &[String],
    dashboard_id: &str,
    tile_id: &str,
    question: &str,
    answer: &str,
    answered_at: &str,
) -> Result<Response, AppError> {
    let ask_dispatch = {
        let _lifecycle = super::lifecycle_guard(state);
        let config = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone();
        super::capture_ask_dispatch_snapshot_under_lifecycle(state, &config)?
    };
    persist_dashboard_living_answer_after_await_with_dispatch(
        state,
        visibility,
        &ask_dispatch,
        witness,
        readable_folders,
        dashboard_id,
        tile_id,
        question,
        answer,
        answered_at,
    )
}

pub(crate) type LivingAnswerRefreshPreflight = (
    super::DurableScopeSnapshot,
    DashboardCompositeContext,
    Vec<String>,
    crate::settings::config::AppConfig,
    super::AskDispatchSnapshot,
);

/// Resolve the exact Living Answer dispatch inputs inside one lifecycle interval. Keeping the
/// complete preflight in a headless helper makes the non-reentrant-lock contract executable: the
/// command and its regression test run this same path, including the under-lifecycle visibility
/// snapshot capture.
pub(crate) fn refresh_dashboard_answer_preflight(
    state: &AppState,
    dashboard_id: &str,
    tile_id: &str,
    question: &str,
) -> Result<LivingAnswerRefreshPreflight, AppError> {
    let _lifecycle = super::lifecycle_guard(state);
    let config = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?
        .clone();
    let ask_dispatch = super::capture_ask_dispatch_snapshot_under_lifecycle(state, &config)?;
    let unlocked = super::unlocked_snapshot(state)?;
    let Some((owner, kind)) = state.db.dashboard_tile_metadata(tile_id)? else {
        return Err(AppError::InvalidArg("living answer is unavailable".into()));
    };
    if owner != dashboard_id || kind != "living_answer" {
        return Err(AppError::InvalidArg("living answer is unavailable".into()));
    }
    let Some(preflight) = state.db.dashboard_living_answer_preflight(tile_id)? else {
        return Err(AppError::InvalidArg("living answer is unavailable".into()));
    };
    let readable = readable_folder_ids(&state.db, &unlocked)?;
    let question_folders =
        decode_folder_provenance(preflight.question_readable_folders_json.as_deref())
            .ok_or_else(|| AppError::Locked("living answer is unavailable".into()))?;
    if !question_folders
        .iter()
        .all(|folder| readable.contains(folder))
    {
        return Err(AppError::Locked("living answer is unavailable".into()));
    }
    let stored_question = state
        .db
        .dashboard_living_question_after_preflight(tile_id)?
        .ok_or_else(|| AppError::Locked("living answer is unavailable".into()))?;
    if stored_question != question {
        return Err(AppError::InvalidArg(
            "living-answer question is backend-owned".into(),
        ));
    }
    let ask_conn =
        crate::summarize::roles::provider_target(crate::summarize::roles::Role::Ask, &config)
            .connection;
    let context = living_answer_composite_context(
        &state.db,
        dashboard_id,
        &unlocked,
        crate::summarize::vault_context::budget_for(&ask_conn),
    )?;
    let mut readable_folders = readable.into_iter().collect::<Vec<_>>();
    readable_folders.sort();
    Ok((
        super::DurableScopeSnapshot::Vault(
            super::capture_content_visibility_snapshot_under_lifecycle(state),
        ),
        context,
        readable_folders,
        config,
        ask_dispatch,
    ))
}

/// Generate and persist one Living Answer without accepting model output or provenance from the
/// WebView. The existing dashboard Ask core owns provider selection, consent, redaction and the
/// content-free egress ledger. This command owns only the cache admission around that call.
#[tauri::command]
pub async fn refresh_dashboard_answer(
    app: AppHandle,
    state: State<'_, AppState>,
    dashboard_id: String,
    tile_id: String,
    question: String,
) -> Result<Response, AppError> {
    let question = clean_title(&question, "question")?;
    let (visibility, context, readable_folders, config, ask_dispatch) =
        refresh_dashboard_answer_preflight(state.inner(), &dashboard_id, &tile_id, &question)?;
    let witness = context.witness.clone();
    let dispatch_visibility = visibility.clone();
    let dispatch_witness = witness.clone();
    let dispatch_ask = ask_dispatch.clone();
    let dispatch_admission = crate::state::ContentDispatchAdmission::new(&app, move |current| {
        super::require_durable_scope_under_lifecycle(current, &dispatch_visibility)?;
        super::require_current_ask_dispatch_under_lifecycle(current, &dispatch_ask)?;
        let unlocked = super::unlocked_snapshot(current)?;
        require_current_living_answer_context_witness_under_lifecycle(
            current,
            &dispatch_witness,
            &unlocked,
        )
    });
    let result = super::ask_vault_prepacked_dashboard_authorized(
        &context,
        &config,
        &question,
        &[],
        &state.heavy_inference,
        dispatch_admission,
    )
    .await?;
    if result.answer.trim().is_empty() || result.answer.len() > MAX_CONFIG_LEN {
        return Err(AppError::InvalidArg("answer too large or empty".into()));
    }

    let answered_at = now_iso();
    persist_dashboard_living_answer_after_await_with_dispatch(
        state.inner(),
        &visibility,
        &ask_dispatch,
        &witness,
        &readable_folders,
        &dashboard_id,
        &tile_id,
        &question,
        &result.answer,
        &answered_at,
    )
}

/// How much of the prompt the brief may take.
///
/// `MAX_BRIEF_CHARS` where the budget allows it, but never more than a QUARTER of
/// the corpus budget: `budget_for` is 4000 chars TOTAL on ollama, so the flat cap
/// alone would let the brief consume the entire prompt and leave nothing for the
/// sources the answer has to stand on. Stated as a function, and tested, rather
/// than buried as an inline `/ 8` at the packing site — which is where it lived
/// when it silently reduced a contracted ~4000-char brief to ~500.
pub(crate) fn brief_allowance(corpus_budget: usize) -> usize {
    MAX_BRIEF_CHARS.min((corpus_budget / 4).max(1))
}

/// The scope rule itself, free of `AppState` so it can be tested directly.
///
/// Extracted 2026-08-04: this function DEFINES what a board-scoped Ask may read,
/// and it had no Rust test at all — its only coverage was Playwright mocks, which
/// assert the frontend's assumption about the answer rather than the answer.
/// See `commands/tests/dashboard_source_scope_tests.rs`.
pub(crate) fn dashboard_sources_inner(
    db: &crate::storage::Db,
    tiles: Vec<DashboardTile>,
    unlocked: &std::collections::HashSet<String>,
) -> Result<Vec<SourceRef>, AppError> {
    let mut out: Vec<SourceRef> = Vec::new();
    for tile in tiles {
        let Some(ref_id) = tile.ref_id.as_deref() else {
            continue;
        };
        let kind = match tile.kind.as_str() {
            "note" => LinkKind::Note,
            "meeting" => LinkKind::Meeting,
            "document" => LinkKind::Document,
            // Derived tiles (drift/numbers/pulse/promises/person/reminders) are VIEWS over the
            // vault, not retrievable documents — they contribute no source of their own.
            //
            // This is deliberate and stays. What a derived tile SHOWS still has to reach the
            // model, but it reaches it as a rendered brief, never as a `SourceRef`: that type's
            // `kind` is a `LinkKind`, and a drift lane is not a retrievable document.
            _ => continue,
        };
        let candidate = SourceRef {
            kind,
            id: ref_id.to_string(),
        };
        // GATE: only a source that survives its own gated reader may enter the Ask scope.
        if !source_is_visible(db, &candidate, unlocked)? {
            continue;
        }
        if !out.iter().any(|s| s.id == candidate.id && s.kind == kind) {
            out.push(candidate);
        }
    }
    Ok(out)
}

/// Is this source readable in the current session? ONE helper so the Ask scope and the
/// Living-answer cache gate can never drift apart — each arm delegates to the shipped gated
/// reader for that kind.
fn source_is_visible(
    db: &crate::storage::Db,
    source: &SourceRef,
    unlocked: &std::collections::HashSet<String>,
) -> Result<bool, AppError> {
    Ok(match source.kind {
        LinkKind::Note => db.note_is_visible(&source.id, unlocked)?,
        LinkKind::Meeting => {
            db.meeting_is_visible(&source.id, unlocked)?
                && db.dashboard_ref_exists("meeting", &source.id)?
        }
        LinkKind::Document => db.document_is_visible(&source.id, unlocked)?,
        // Metadata-only endpoints (`is_content_source == false`): a Shared Brain relation is
        // somebody else's document, and a `container` names a PLACE that holds no text of its own.
        // Neither may enter an Ask scope — and a container in particular must NEVER be expanded
        // into what it contains.
        LinkKind::Org | LinkKind::Container => false,
    })
}

/// Resolve ONE tile. Every arm reads through a gated reader; nothing here queries raw content.
pub(crate) fn resolve_tile(
    db: &crate::storage::Db,
    tile: &DashboardTile,
    unlocked: &std::collections::HashSet<String>,
) -> Result<TileData, AppError> {
    // Kinds that need an anchor.
    let needs_ref = matches!(
        tile.kind.as_str(),
        "note" | "meeting" | "document" | "person" | "drift" | "numbers" | "pulse"
    );
    let ref_id = match (needs_ref, tile.ref_id.as_deref()) {
        (true, None) => return Ok(TileData::Unconfigured),
        (_, r) => r.unwrap_or_default().to_string(),
    };

    match tile.kind.as_str() {
        "note" => {
            let Some(note) = db.get_document_if_visible_kind(&ref_id, "note", unlocked)? else {
                // Absent can mean sealed OR deleted. Distinguish with an EXISTENCE-only probe —
                // it returns a bool, never a title or folder.
                return Ok(if db.dashboard_ref_exists("note", &ref_id)? {
                    TileData::Locked
                } else {
                    TileData::Missing
                });
            };
            Ok(TileData::Note {
                id: note.id,
                title: note.title.unwrap_or(note.name),
                snippet: snippet_of(&note.markdown, 220),
                updated_at: note.updated_at.unwrap_or(note.created_at),
            })
        }
        "meeting" => {
            let Some(m) = db.get_meeting_if_visible(&ref_id, unlocked)? else {
                return Ok(if db.dashboard_ref_exists("meeting", &ref_id)? {
                    TileData::Locked
                } else {
                    TileData::Missing
                });
            };
            Ok(TileData::Meeting {
                id: m.id,
                title: m.title.unwrap_or_else(|| "(untitled)".to_string()),
                started_at: m.started_at,
                duration_s: m.duration_s,
                has_audio: m.audio_path.is_some(),
            })
        }
        "document" => {
            let Some(doc) = db.get_document_if_visible_kind(&ref_id, "document", unlocked)? else {
                return Ok(if db.dashboard_ref_exists("document", &ref_id)? {
                    TileData::Locked
                } else {
                    TileData::Missing
                });
            };
            Ok(TileData::Document {
                title: doc
                    .title
                    .clone()
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or(doc.name),
                snippet: snippet_of(&doc.markdown, 220),
                id: doc.id,
            })
        }
        "person" => {
            let Some(node) = db.get_entity_node_visible(&ref_id, unlocked)? else {
                // An entity visible ONLY through sealed meetings is indistinguishable from an
                // unknown one — the same non-leak contract `get_entity_detail` keeps, and the
                // reason this arm must NOT fall back to any stored name. `redact_tile_chrome`
                // strips the tile's chrome for `Missing`, so no legacy copied name survives here.
                return Ok(TileData::Missing);
            };
            let open = db.list_open_commitments(unlocked, Some(&node.name))?.len() as i64;
            Ok(TileData::Person {
                id: node.id,
                name: node.name,
                mention_count: node.mention_count,
                open_commitments: open,
            })
        }
        "reminders" => {
            // Reminders are the user's OWN data, so a reminder with no source anchor always
            // shows. But `list_stored_reminders` is UNGATED (it takes no `unlocked` set and
            // applies no `visibility_clause`) — an earlier comment here claimed the store drops
            // sealed anchors, and that was simply false.
            //
            // That mattered little while this tile only rendered on screen; it matters now that
            // a board is readable by agents (local MCP and the cloud-capable Ask loop). A
            // "smart" reminder's TITLE is authored from meeting content, so a reminder whose
            // every anchor points at a source the session cannot read is withheld here.
            let mut rows: Vec<(i64, TileRow)> = db
                .list_dashboard_reminders_visible(unlocked)?
                .into_iter()
                .map(|(title, due_at)| {
                    // Due in the past ⇒ "due" (needs attention), else "open".
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    let status = if due_at <= now_ms { "due" } else { "open" };
                    (
                        due_at,
                        TileRow {
                            text: title,
                            meta: Some(format_epoch_day(due_at)),
                            status: Some(status.to_string()),
                            source: None,
                        },
                    )
                })
                .collect();
            rows.sort_by_key(|(due, _)| *due);
            let due_count = rows.len() as i64;
            Ok(TileData::Reminders {
                rows: rows.into_iter().take(TILE_ROWS).map(|(_, r)| r).collect(),
                due_count,
            })
        }
        "drift" => {
            // The bitemporal fact history for the entity, already gated by `list_facts_visible`.
            let facts = db.list_facts_visible(&ref_id, unlocked)?;
            let entity = entity_name(db, &ref_id, unlocked)?;
            if entity == ENTITY_HIDDEN {
                return Ok(TileData::Drift {
                    entity,
                    predicate: String::new(),
                    rows: vec![],
                });
            }
            if facts.is_empty() {
                return Ok(TileData::Drift {
                    // A hidden entity emits NO stored predicate either. Nothing writes
                    // `config.predicate` today, but the day a UI lets a user pin one chosen from
                    // an entity's facts, echoing it back for a sealed entity would be a leak.
                    predicate: String::new(),
                    entity,
                    rows: vec![],
                });
            }
            // Pick the predicate with the most recorded values (the one that actually moved),
            // unless the tile pins one.
            let predicate = {
                let mut counts: std::collections::HashMap<&str, usize> =
                    std::collections::HashMap::new();
                for f in &facts {
                    *counts.entry(f.predicate.as_str()).or_default() += 1;
                }
                counts
                    .into_iter()
                    .max_by_key(|(_, n)| *n)
                    .map(|(p, _)| p.to_string())
                    .unwrap_or_default()
            };
            let mut steps: Vec<_> = facts
                .iter()
                .filter(|f| f.predicate == predicate)
                .collect::<Vec<_>>();
            steps.sort_by(|a, b| a.valid_from.cmp(&b.valid_from));
            let last = steps.len().saturating_sub(1);
            let rows = steps
                .iter()
                .enumerate()
                .rev()
                .take(TILE_ROWS)
                .map(|(i, f)| TileRow {
                    text: f.object.clone(),
                    meta: Some(short_day(&f.valid_from)),
                    status: Some(if i == last { "now" } else { "old" }.to_string()),
                    source: f.meeting_id.as_ref().map(|id| SourceRef {
                        kind: LinkKind::Meeting,
                        id: id.clone(),
                    }),
                })
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            Ok(TileData::Drift {
                entity,
                predicate,
                rows,
            })
        }
        "numbers" => {
            let facts = db.list_facts_visible(&ref_id, unlocked)?;
            // Currently-valid numeric facts, each annotated with the value it replaced.
            let mut rows = Vec::new();
            for f in facts.iter().filter(|f| f.valid_to.is_none()) {
                if !looks_numeric(&f.object) {
                    continue;
                }
                let was = facts
                    .iter()
                    .filter(|p| p.predicate == f.predicate && p.valid_to.is_some())
                    .max_by(|a, b| a.valid_from.cmp(&b.valid_from))
                    .map(|p| format!("was {}", p.object));
                rows.push(TileRow {
                    text: f.object.clone(),
                    meta: Some(f.predicate.clone()),
                    status: was,
                    source: f.meeting_id.as_ref().map(|id| SourceRef {
                        kind: LinkKind::Meeting,
                        id: id.clone(),
                    }),
                });
                if rows.len() >= TILE_ROWS {
                    break;
                }
            }
            Ok(TileData::Numbers {
                entity: entity_name(db, &ref_id, unlocked)?,
                rows,
            })
        }
        "pulse" => {
            let mentions = db.entity_mention_pulse_visible(&ref_id, PULSE_SAMPLE, unlocked)?;
            let now = chrono::Utc::now().timestamp();
            const WEEKS: usize = 12;
            let mut weekly = vec![0i64; WEEKS];
            let mut newest: Option<i64> = None;
            for iso in &mentions {
                let Some(ts) = iso_to_epoch_secs(iso) else {
                    continue;
                };
                newest = Some(newest.map_or(ts, |n: i64| n.max(ts)));
                let weeks_ago = ((now - ts).max(0) / (7 * 86_400)) as usize;
                if weeks_ago < WEEKS {
                    // index 0 = oldest bucket on the chart
                    weekly[WEEKS - 1 - weeks_ago] += 1;
                }
            }
            Ok(TileData::Pulse {
                entity: entity_name(db, &ref_id, unlocked)?,
                total: mentions.len() as i64,
                quiet_days: newest.map(|ts| (now - ts).max(0) / 86_400),
                weekly,
            })
        }
        "promises" => {
            let owner = None;
            let items = db.list_open_commitments(unlocked, owner.as_deref())?;
            let rows = items
                .into_iter()
                .take(TILE_ROWS)
                .map(|c| TileRow {
                    text: c.text,
                    meta: Some(match (&c.owner, &c.due_date) {
                        (Some(o), Some(d)) => format!("{o} · due {d}"),
                        (Some(o), None) => o.clone(),
                        (None, Some(d)) => format!("due {d}"),
                        (None, None) => c.meeting_title.clone(),
                    }),
                    status: Some(commitment_status(c.due_date.as_deref())),
                    source: Some(SourceRef {
                        kind: LinkKind::Meeting,
                        id: c.meeting_id,
                    }),
                })
                .collect();
            Ok(TileData::Promises { owner, rows })
        }
        "living_answer" => {
            // GATE the CACHE. The stored answer is a paraphrase of the sources it was built from,
            // so it must disappear the moment any of them is sealed-and-not-session-unlocked —
            // otherwise a board keeps quoting a locked folder back at the user forever.
            //
            // Fail-closed on a legacy row: an answer with no recorded sources cannot be checked,
            // so it is withheld rather than trusted.
            let readable = readable_folder_ids(db, unlocked)?;
            let Some(preflight) = db.dashboard_living_answer_preflight(&tile.id)? else {
                return Ok(living_answer_withheld());
            };
            let Some(question_provenance) =
                decode_folder_provenance(preflight.question_readable_folders_json.as_deref())
            else {
                return Ok(living_answer_withheld());
            };
            if !question_provenance
                .iter()
                .all(|folder| readable.contains(folder))
            {
                return Ok(living_answer_withheld());
            }
            match &preflight.answer {
                LivingAnswerCacheState::Empty => {
                    let Some(question) = db.dashboard_living_question_after_preflight(&tile.id)?
                    else {
                        return Ok(living_answer_withheld());
                    };
                    return Ok(TileData::LivingAnswer {
                        question,
                        answer: None,
                        answered_at: None,
                        withheld: false,
                    });
                }
                LivingAnswerCacheState::Malformed => return Ok(living_answer_withheld()),
                LivingAnswerCacheState::Valid {
                    readable_folders_json,
                    context_generation,
                    context_digest,
                    context_budget,
                    ask_dispatch_generation,
                } => {
                    let Some(answer_folders) =
                        decode_folder_provenance(Some(readable_folders_json))
                    else {
                        return Ok(living_answer_withheld());
                    };
                    let Ok(context_budget) = usize::try_from(*context_budget) else {
                        return Ok(living_answer_withheld());
                    };
                    if context_budget == 0 || context_budget > 200_000 {
                        return Ok(living_answer_withheld());
                    }
                    let current = living_answer_composite_context(
                        db,
                        &tile.dashboard_id,
                        unlocked,
                        context_budget,
                    )?;
                    if *ask_dispatch_generation != current.witness.ask_dispatch_generation {
                        let Some(question) =
                            db.dashboard_living_question_after_preflight(&tile.id)?
                        else {
                            return Ok(living_answer_withheld());
                        };
                        return Ok(TileData::LivingAnswer {
                            question,
                            answer: None,
                            answered_at: None,
                            withheld: false,
                        });
                    }
                    if *context_generation != current.witness.generation
                        || *context_digest != current.witness.input_digest
                        || !answer_folders
                            .iter()
                            .all(|folder| readable.contains(folder))
                    {
                        return Ok(living_answer_withheld());
                    }
                }
            }
            let ask_dispatch_generation = match preflight.answer {
                LivingAnswerCacheState::Valid {
                    ask_dispatch_generation,
                    ..
                } => ask_dispatch_generation,
                LivingAnswerCacheState::Empty | LivingAnswerCacheState::Malformed => {
                    return Ok(living_answer_withheld());
                }
            };
            let Some(content) = db.dashboard_living_answer_content_after_preflight_with_dispatch(
                &tile.id,
                ask_dispatch_generation,
                match &preflight.answer {
                    LivingAnswerCacheState::Valid {
                        context_generation, ..
                    } => *context_generation,
                    LivingAnswerCacheState::Empty | LivingAnswerCacheState::Malformed => -1,
                },
                match &preflight.answer {
                    LivingAnswerCacheState::Valid { context_digest, .. } => context_digest,
                    LivingAnswerCacheState::Empty | LivingAnswerCacheState::Malformed => "",
                },
                match &preflight.answer {
                    LivingAnswerCacheState::Valid { context_budget, .. } => *context_budget,
                    LivingAnswerCacheState::Empty | LivingAnswerCacheState::Malformed => -1,
                },
            )?
            else {
                return Ok(living_answer_withheld());
            };
            let question = content.question;
            let answer = content.answer;
            let answered_at = content.answered_at;
            if question.trim().is_empty()
                || answer.as_ref().is_some_and(|value| value.trim().is_empty())
                || answered_at
                    .as_ref()
                    .is_some_and(|value| value.trim().is_empty())
                || (answer.is_some() != answered_at.is_some())
            {
                return Ok(living_answer_withheld());
            }
            Ok(TileData::LivingAnswer {
                question,
                answer,
                answered_at,
                withheld: false,
            })
        }
        other => Err(AppError::InvalidArg(format!("unknown tile kind: {other}"))),
    }
}

/// Render ONE resolved tile as plain text for an agent (the local MCP surface and the in-app
/// agentic Ask loop both read this).
///
/// It applies the SAME redaction discipline as the UI, in the same place: a withheld payload
/// prints its state and nothing else — never the tile's stored title, never a cached answer. That
/// is why this lives next to `redact_tile_chrome` rather than in `tools.rs`; keeping the two
/// together is what stops the agent surface from drifting away from the screen surface.
pub(crate) fn render_tile_for_agent(tile: &DashboardTile, data: &TileData) -> String {
    let heading = |fallback: &str| -> String {
        match tile.title.as_deref() {
            Some(t) if !t.trim().is_empty() => t.to_string(),
            _ => fallback.to_string(),
        }
    };
    let rows = |rows: &[TileRow]| -> String {
        rows.iter()
            .map(|r| {
                let meta = r.meta.as_deref().unwrap_or("");
                let status = r.status.as_deref().unwrap_or("");
                format!("    · {} {} {}", r.text, meta, status)
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    match data {
        // Withheld states print their state ONLY — no stored title, no config.
        TileData::Locked => "- [sealed tile — redacted; unlock the source to read it]".to_string(),
        TileData::Missing => "- [tile whose source no longer exists]".to_string(),
        TileData::Unconfigured => "- [tile with no source configured]".to_string(),
        TileData::Note {
            title, snippet, id, ..
        } => format!("- note: {title} · id:{id}\n    {snippet}"),
        TileData::Meeting {
            title,
            started_at,
            duration_s,
            id,
            ..
        } => format!("- recording: {title} · id:{id} · {started_at} · {duration_s}s"),
        TileData::Document { title, snippet, id } => {
            format!("- document: {title} · id:{id}\n    {snippet}")
        }
        TileData::Person {
            name,
            mention_count,
            open_commitments,
            id,
        } => format!(
            "- person: {name} · id:{id} · visibleMeetings:{mention_count} · openCommitments:{open_commitments}"
        ),
        TileData::Reminders { rows: r, due_count } => {
            format!("- reminders ({due_count} open)\n{}", rows(r))
        }
        TileData::Drift {
            entity,
            predicate,
            rows: r,
        } => format!("- drift: {entity} · {predicate}\n{}", rows(r)),
        TileData::Numbers { entity, rows: r } => {
            format!("- numbers: {entity}\n{}", rows(r))
        }
        TileData::Pulse {
            entity,
            total,
            quiet_days,
            ..
        } => format!(
            "- pulse: {entity} · mentions12w:{total} · quietDays:{}",
            quiet_days.map_or("n/a".to_string(), |d| d.to_string())
        ),
        TileData::Promises { owner, rows: r } => format!(
            "- promises{}\n{}",
            owner
                .as_deref()
                .map_or(String::new(), |o| format!(" ({o})")),
            rows(r)
        ),
        TileData::LivingAnswer {
            question,
            answer,
            withheld,
            ..
        } => {
            // NEVER fall back to the stored title when the answer is withheld: that title is
            // exactly the field a legacy row copied from the source. An unnamed withheld tile is
            // anonymous, which is the correct outcome.
            if *withheld {
                "- living answer\n    [saved answer withheld — a source is sealed]".to_string()
            } else {
                let q = if question.trim().is_empty() {
                    heading("living answer")
                } else {
                    question.clone()
                };
                format!(
                    "- living answer: {q}\n    {}",
                    answer.as_deref().unwrap_or("(not answered yet)")
                )
            }
        }
    }
}

/// Is a reminder's PROVENANCE readable in this session?
///
/// `true` when a manual reminder has no anchors (the user's own data), or when every recorded
/// anchor resolves through its gated reader. A smart reminder with no anchors is unprovable and
/// fails closed. A title can be derived jointly from multiple anchors, so one readable anchor
/// never declassifies text influenced by a sealed one.
/// The placeholder an entity-anchored tile shows when its entity is not currently visible. Also
/// the signal `redact_tile_chrome` reads to strip that tile's stored chrome.
const ENTITY_HIDDEN: &str = "—";

/// The VISIBLE display name of an entity, or a neutral placeholder. Never leaks a name that only
/// exists behind a sealed meeting (`list_entities_visible` already drops those).
fn entity_name(
    db: &crate::storage::Db,
    entity_id: &str,
    unlocked: &std::collections::HashSet<String>,
) -> Result<String, AppError> {
    Ok(db
        .get_entity_node_visible(entity_id, unlocked)?
        .map(|e| e.name)
        .unwrap_or_else(|| ENTITY_HIDDEN.to_string()))
}

/// `late` when the due date is in the past, `due` when it is today, else `open`.
///
/// The comparison is against the user's LOCAL calendar day, not UTC: a due date is a date a human
/// wrote down, so around local midnight east of UTC a UTC "today" would mark a commitment late a
/// day early.
fn commitment_status(due: Option<&str>) -> String {
    let Some(due) = due else {
        return "open".to_string();
    };
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    match due.get(..10) {
        Some(d) if d < today.as_str() => "late".to_string(),
        Some(d) if d == today => "due".to_string(),
        _ => "open".to_string(),
    }
}

fn format_epoch_day(ms_or_s: i64) -> String {
    // Reminder due times are epoch MILLISECONDS in this codebase; tolerate seconds defensively.
    let secs = if ms_or_s.abs() > 4_000_000_000 {
        ms_or_s / 1000
    } else {
        ms_or_s
    };
    // Rendered in the user's LOCAL zone — a due date shown as "Jun 13" when the reminder fires on
    // the 14th is the same off-by-a-day bug as in `commitment_status`.
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%b %-d").to_string())
        .unwrap_or_default()
}

fn short_day(iso: &str) -> String {
    iso_to_epoch_secs(iso)
        .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
        .map(|d| d.format("%b %-d, %Y").to_string())
        .unwrap_or_else(|| iso.chars().take(10).collect())
}

#[cfg(test)]
#[path = "tests/dashboard_cmd_tests.rs"]
mod dashboard_cmd_tests;

#[cfg(test)]
#[path = "tests/dashboard_source_scope_tests.rs"]
mod dashboard_source_scope_tests;

#[cfg(test)]
#[path = "tests/dashboard_brief_tests.rs"]
mod dashboard_brief_tests;
