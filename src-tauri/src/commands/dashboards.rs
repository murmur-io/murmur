//! DASHBOARDS command surface (2026-08-03) — user-composed boards of tiles over sources that
//! already exist in the vault.
//!
//! ## The one rule that governs this file
//! A dashboard is a set of POINTERS. It stores no meeting content, and **every tile payload is
//! resolved through an existing gated reader at read time** — `Db::*_visible` /
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
//! Board-scoped Ask reuses the SHIPPED `ask_vault(explicit_sources: …)` path verbatim
//! ([`get_dashboard_sources`] just hands it the board's visible sources), so this feature adds
//! **no new AI path, no new egress surface, and no new redaction seam**.

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::error::AppError;
use crate::links::LinkKind;
use crate::state::AppState;
use crate::storage::dashboards_store::{
    MAX_DASHBOARDS, MAX_SPAN, MAX_TILES_PER_BOARD, MIN_SPAN, TILE_KINDS,
};
use crate::storage::models::{Dashboard, DashboardTile, SourceRef};

/// Longest accepted board title / tile heading. Bounds the DTO and the `.canvas` export.
const MAX_TITLE_LEN: usize = 120;
/// Longest accepted `config` JSON blob (the Living-answer question + cached answer).
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
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
    /// A pinned question plus the answer last computed for it (by the FE, through `ask_vault`).
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

/// The persisted `config` bag. Every field optional — an older row deserializes fine.
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

/// Decide whether a cached Living answer may still be shown.
///
/// Split out as a PURE function on purpose: `resolve_tile` needs a `State<AppState>`, so nothing
/// could test the gate in place — and an independent verifier proved that disabling the previous
/// gate left the entire suite green. This one has a direct oracle
/// (`living_answer_gate_withholds_when_a_folder_stopped_being_readable`).
///
/// Fail-closed: an answer with no recorded readable set, or an empty one, is withheld.
pub(crate) fn living_answer_withheld(
    has_answer: bool,
    recorded_readable: &[String],
    currently_readable: &std::collections::HashSet<String>,
) -> bool {
    if !has_answer {
        return false;
    }
    if recorded_readable.is_empty() {
        return true; // un-gateable (legacy row) ⇒ withhold
    }
    !recorded_readable
        .iter()
        .all(|id| currently_readable.contains(id))
}

/// Every folder the session can currently read: unlocked outright, or sealed but session-unlocked.
fn readable_folder_ids(
    db: &crate::storage::Db,
    unlocked: &std::collections::HashSet<String>,
) -> Result<std::collections::HashSet<String>, AppError> {
    Ok(db
        .list_folders()?
        .into_iter()
        .filter(|f| !f.locked || unlocked.contains(&f.id))
        .map(|f| f.id)
        .collect())
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

/// Every board with its layout metadata. No gated read happens here — a summary carries only the
/// user's own chrome (title/emoji/tint) plus tile kinds, never a source's title.
#[tauri::command]
pub fn list_dashboards(state: State<'_, AppState>) -> Result<Vec<DashboardSummaryDto>, AppError> {
    let boards = state.db.list_dashboards()?;
    let kinds = state.db.dashboard_tile_kinds()?;
    let out = boards
        .into_iter()
        .map(|d| {
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
) -> Result<Dashboard, AppError> {
    if state.db.dashboard_count()? >= MAX_DASHBOARDS {
        return Err(AppError::InvalidArg(format!(
            "dashboard limit reached ({MAX_DASHBOARDS})"
        )));
    }
    let title = clean_title(&title, "title")?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso();
    state.db.insert_dashboard(
        &id,
        &title,
        clean_emoji(emoji).as_deref(),
        clean_tint(tint).as_deref(),
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
    let title = match title {
        Some(t) => Some(clean_title(&t, "title")?),
        None => None,
    };
    let found = state.db.update_dashboard(
        &id,
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
        .get_dashboard(&id)?
        .ok_or_else(|| AppError::Storage("dashboard vanished after update".into()))
}

#[tauri::command]
pub fn delete_dashboard(state: State<'_, AppState>, id: String) -> Result<bool, AppError> {
    state.db.delete_dashboard(&id)
}

#[tauri::command]
pub fn reorder_dashboards(state: State<'_, AppState>, ids: Vec<String>) -> Result<(), AppError> {
    state.db.reorder_dashboards(&ids)
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
    if let Some(c) = config.as_deref() {
        if c.len() > MAX_CONFIG_LEN {
            return Err(AppError::InvalidArg("tile config too large".into()));
        }
        serde_json::from_str::<TileConfig>(c)
            .map_err(|e| AppError::InvalidArg(format!("invalid tile config: {e}")))?;
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso();
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
    let Some(existing) = state.db.get_dashboard_tile(&id)? else {
        return Err(AppError::InvalidArg(format!("no tile with id {id}")));
    };
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
    state.db.touch_dashboard(&existing.dashboard_id, &now_iso())?;
    // Same reason as `add_dashboard_tile`: never return the raw row. Reachable proof this
    // mattered — the Arrange-mode resize control fires for a SEALED tile, so returning the row
    // would push that tile's stored title/config into the webview.
    Ok(())
}

/// Persist a Living-answer result, stamping the readable-folder snapshot that gates it.
///
/// The FE cannot compute that snapshot (it has no folder/lock view), and letting it write the
/// answer through the generic `update_dashboard_tile` is what made the cache un-gateable. So the
/// backend owns this write end-to-end.
#[tauri::command]
pub fn set_dashboard_answer(
    state: State<'_, AppState>,
    id: String,
    question: String,
    answer: String,
) -> Result<(), AppError> {
    let Some(existing) = state.db.get_dashboard_tile(&id)? else {
        return Err(AppError::InvalidArg(format!("no tile with id {id}")));
    };
    if existing.kind != "living_answer" {
        return Err(AppError::InvalidArg(
            "only a living-answer tile stores an answer".into(),
        ));
    }
    let question = clean_title(&question, "question")?;
    if answer.len() > MAX_CONFIG_LEN {
        return Err(AppError::InvalidArg("answer too large".into()));
    }
    // Snapshot under the lifecycle guard so a relock cannot land between computing the readable
    // set and storing it — otherwise the answer would be stamped with a MORE permissive set than
    // was actually in force, which is the one direction that would weaken the gate.
    let _lifecycle = super::lifecycle_guard(state.inner());
    let unlocked = super::unlocked_snapshot(state.inner())?;
    let readable: Vec<String> = readable_folder_ids(&state.db, &unlocked)?
        .into_iter()
        .collect();

    let mut cfg = parse_config(existing.config.as_deref());
    cfg.question = Some(question);
    cfg.answer = Some(answer);
    cfg.answered_at = Some(now_iso());
    cfg.answer_readable_folders = Some(readable);
    let encoded = serde_json::to_string(&cfg)
        .map_err(|e| AppError::Storage(format!("encoding tile config failed: {e}")))?;
    if encoded.len() > MAX_CONFIG_LEN {
        return Err(AppError::InvalidArg("answer too large".into()));
    }
    state
        .db
        .update_dashboard_tile(&id, None, None, Some(&encoded))?;
    state.db.touch_dashboard(&existing.dashboard_id, &now_iso())?;
    Ok(())
}

#[tauri::command]
pub fn delete_dashboard_tile(state: State<'_, AppState>, id: String) -> Result<bool, AppError> {
    let board = state.db.get_dashboard_tile(&id)?.map(|t| t.dashboard_id);
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
    state.db.reorder_dashboard_tiles(&dashboard_id, &tile_ids)?;
    state.db.touch_dashboard(&dashboard_id, &now_iso())
}

// ── the board, resolved ────────────────────────────────────────────────────────────────────────

/// One board with every tile resolved through the gated readers.
#[tauri::command]
pub fn get_dashboard(
    state: State<'_, AppState>,
    id: String,
) -> Result<Option<DashboardDetailDto>, AppError> {
    // TOCTOU: hold the lock lifecycle guard across the gate AND every tile read, exactly like
    // `commands/meetings.rs::get_meeting_detail`. Without it a relock landing between the
    // `unlocked_snapshot` below and a later tile's read would resolve that tile against a stale
    // "unlocked" view and return content from a folder that is sealed by the time it ships.
    let _lifecycle = super::lifecycle_guard(state.inner());
    let Some(dashboard) = state.db.get_dashboard(&id)? else {
        return Ok(None);
    };
    let tiles = state.db.list_dashboard_tiles(&id)?;
    let unlocked = super::unlocked_snapshot(state.inner())?;
    let resolved = tiles
        .into_iter()
        .map(|tile| {
            let data = resolve_tile(&state.db, &tile, &unlocked)?;
            Ok(ResolvedTileDto {
                tile: redact_tile_chrome(tile, &data),
                data,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(Some(DashboardDetailDto {
        dashboard,
        tiles: resolved,
    }))
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
fn redact_tile_chrome(mut tile: DashboardTile, data: &TileData) -> DashboardTile {
    let withheld = match data {
        TileData::Locked | TileData::Missing | TileData::Unconfigured => true,
        // `entity_name` yields the placeholder when the entity is not visible.
        TileData::Drift { entity, .. }
        | TileData::Numbers { entity, .. }
        | TileData::Pulse { entity, .. } => entity == ENTITY_HIDDEN,
        TileData::LivingAnswer { withheld, .. } => *withheld,
        _ => false,
    };
    if withheld {
        tile.title = None;
        tile.config = None;
    }
    tile
}

/// The board's VISIBLE sources, ready to hand to `ask_vault(explicit_sources: …)`. A sealed source
/// is absent — so a board-scoped Ask can never retrieve from a locked folder, and the answer is
/// deterministic over exactly what the user composed.
#[tauri::command]
pub fn get_dashboard_sources(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<SourceRef>, AppError> {
    // Same TOCTOU discipline as `get_dashboard`: gate and read under one lifecycle guard.
    let _lifecycle = super::lifecycle_guard(state.inner());
    let tiles = state.db.list_dashboard_tiles(&id)?;
    let unlocked = super::unlocked_snapshot(state.inner())?;
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
            _ => continue,
        };
        let candidate = SourceRef {
            kind,
            id: ref_id.to_string(),
        };
        // GATE: only a source that survives its own gated reader may enter the Ask scope.
        if !source_is_visible(&state.db, &candidate, &unlocked)? {
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
        LinkKind::Note => db.note_markdown_if_visible(&source.id, unlocked)?.is_some(),
        LinkKind::Meeting => db.meeting_is_visible(&source.id, unlocked)?,
        LinkKind::Document => db.get_document_if_visible(&source.id, unlocked)?.is_some(),
    })
}

/// Resolve ONE tile. Every arm reads through a gated reader; nothing here queries raw content.
pub(crate) fn resolve_tile(
    db: &crate::storage::Db,
    tile: &DashboardTile,
    unlocked: &std::collections::HashSet<String>,
) -> Result<TileData, AppError> {
    let cfg = parse_config(tile.config.as_deref());

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
            // `list_notes_visible(None, …)` is the SAME gated reader the Notes list uses; a sealed
            // note is simply absent from it, which is exactly the Locked signal we want.
            let Some(note) = db
                .list_notes_visible(None, unlocked)?
                .into_iter()
                .find(|n| n.id == ref_id)
            else {
                // Absent can mean sealed OR deleted. Distinguish with an EXISTENCE-only probe —
                // it returns a bool, never a title or folder.
                return Ok(if db.dashboard_ref_exists("note", &ref_id)? {
                    TileData::Locked
                } else {
                    TileData::Missing
                });
            };
            if note.locked {
                return Ok(TileData::Locked);
            }
            Ok(TileData::Note {
                id: note.id,
                title: note.title,
                snippet: snippet_of(&note.snippet, 220),
                updated_at: note.updated_at,
            })
        }
        "meeting" => {
            if !db.meeting_is_visible(&ref_id, unlocked)? {
                return Ok(if db.get_meeting(&ref_id)?.is_some() {
                    TileData::Locked
                } else {
                    TileData::Missing
                });
            }
            let Some(m) = db.get_meeting(&ref_id)? else {
                return Ok(TileData::Missing);
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
            let Some(doc) = db.get_document_if_visible(&ref_id, unlocked)? else {
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
            let Some(node) = db
                .list_entities_visible(unlocked)?
                .into_iter()
                .find(|e| e.id == ref_id)
            else {
                // An entity visible ONLY through sealed meetings is indistinguishable from an
                // unknown one — the same non-leak contract `get_entity_detail` keeps, and the
                // reason this arm must NOT fall back to any stored name. `redact_tile_chrome`
                // strips the tile's chrome for `Missing`, so no legacy copied name survives here.
                return Ok(TileData::Missing);
            };
            let open = db
                .list_open_commitments(unlocked, Some(&node.name))?
                .len() as i64;
            Ok(TileData::Person {
                id: node.id,
                name: node.name,
                mention_count: node.mention_count,
                open_commitments: open,
            })
        }
        "reminders" => {
            // Reminders are the user's OWN data (independent of any meeting), and the store
            // already drops sealed source anchors. Show the soonest open ones.
            let mut rows: Vec<(i64, TileRow)> = db
                .list_stored_reminders()?
                .into_iter()
                .filter(|r| matches!(r.state, crate::storage::models::ReminderState::Active))
                .map(|r| {
                    // Due in the past ⇒ "due" (needs attention), else "open".
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    let status = if r.due_at <= now_ms { "due" } else { "open" };
                    (
                        r.due_at,
                        TileRow {
                            text: r.title,
                            meta: Some(format_epoch_day(r.due_at)),
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
            if facts.is_empty() {
                return Ok(TileData::Drift {
                    entity: entity_name(db, &ref_id, unlocked)?,
                    predicate: cfg.predicate.unwrap_or_default(),
                    rows: vec![],
                });
            }
            // Pick the predicate with the most recorded values (the one that actually moved),
            // unless the tile pins one.
            let predicate = cfg.predicate.clone().unwrap_or_else(|| {
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
            });
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
                entity: entity_name(db, &ref_id, unlocked)?,
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
            let owner = cfg.owner.clone();
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
            let has_answer = cfg.answer.as_ref().is_some_and(|a| !a.trim().is_empty());
            let recorded = cfg.answer_readable_folders.unwrap_or_default();
            let withheld =
                living_answer_withheld(has_answer, &recorded, &readable_folder_ids(db, unlocked)?);
            Ok(TileData::LivingAnswer {
                question: cfg.question.unwrap_or_default(),
                answer: if withheld { None } else { cfg.answer },
                answered_at: if withheld { None } else { cfg.answered_at },
                withheld,
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
            let q = if question.trim().is_empty() {
                heading("living answer")
            } else {
                question.clone()
            };
            if *withheld {
                format!("- living answer: {q}\n    [saved answer withheld — a source is sealed]")
            } else {
                format!(
                    "- living answer: {q}\n    {}",
                    answer.as_deref().unwrap_or("(not answered yet)")
                )
            }
        }
    }
}

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
        .list_entities_visible(unlocked)?
        .into_iter()
        .find(|e| e.id == entity_id)
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
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%b %-d")
                .to_string()
        })
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
