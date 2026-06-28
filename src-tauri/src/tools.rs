//! Unified tool registry — the single, transport-agnostic seam through which every read-only
//! "agent tool" call runs (Phase A plumbing).
//!
//! Today the MCP server (`mcp.rs`) is the only caller; tomorrow the local brain (Phase E) dispatches
//! the same [`ToolCall`]s from a parsed [`crate::audio::wake::VoiceIntent`]. Keeping ONE
//! [`execute_tool`] means there is exactly one gated implementation of each tool — a future surface
//! cannot accidentally grow an ungated path.
//!
//! ## Lock invariant (load-bearing)
//! `unlocked` is a NON-OPTIONAL `&HashSet<String>`: every read inside [`execute_tool`] is
//! visibility-gated against it (`search_visible` / `search_hybrid_visible` / `meeting_is_visible` /
//! `get_note_if_visible` / `list_meetings_visible` / `list_open_commitments` /
//! `build_dossier_data`), so a sealed-and-not-session-unlocked meeting is invisible to all of them.
//! There is no constructor that lets a caller skip the gate.
//!
//! ## Egress
//! NONE. `execute_tool` only reads the local SQLite DB and (for semantic search) runs the local
//! embedder. It never constructs a cloud provider or makes a network call.

use std::collections::HashSet;

use crate::error::{AppError, Result};
use crate::settings::AppConfig;
use crate::storage::Db;

/// A single read-only tool invocation. The six MCP tools are implemented today; the commented
/// variants below are the Phase-E extension points (voice-intent-driven WRITE actions) — they are
/// deliberately NOT yet part of the enum so no surface can dispatch them before the brain lands.
///
/// Room for future (Phase E, NOT implemented here):
///   - `NoteAside { text: String }`        — append an aside to the live note.
///   - `CreateReminder { text, due }`       — create a reminder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCall {
    /// Full-text search across titles/transcripts/notes.
    SearchMeetings { query: String },
    /// A meeting's AI note + full transcript by id.
    GetMeeting { meeting_id: String },
    /// The most recent meetings (already clamped to 1..=100 by the caller/parser).
    ListRecentMeetings { limit: i64 },
    /// Hybrid semantic + FTS search. Gated behind the `semantic_search_enabled` config flag.
    SearchSemantic { query: String },
    /// Roll up every OPEN action item, optionally filtered by owner.
    GetOpenCommitments { owner: Option<String> },
    /// Assemble the gated structured dossier for one entity (caller must pass a non-empty name/id).
    GetEntityDossier { entity: String },
}

/// Execute a read-only tool against an OPEN `Db`, gated on the live session `unlocked` set.
///
/// Returns the tool's text payload (the same strings the MCP surface returned before this seam was
/// extracted). Every branch routes through the visibility-gated DB readers; a sealed-and-not-unlocked
/// meeting can never surface. `config` carries the `semantic_search_enabled` flag so the caller owns
/// the gate decision (and the MCP reader thread reads it from the same whole-DB-encrypted settings).
pub fn execute_tool(
    call: &ToolCall,
    db: &Db,
    unlocked: &HashSet<String>,
    config: &AppConfig,
) -> Result<String> {
    match call {
        ToolCall::SearchMeetings { query } => {
            let q = query.as_str();
            match db.search_visible(q, 20, unlocked) {
                Ok(hits) if hits.is_empty() => Ok(format!("No meetings match \"{q}\".")),
                Ok(hits) => Ok(format_hits(&hits)),
                Err(e) => Err(AppError::Storage(format!("search failed: {e}"))),
            }
        }
        ToolCall::SearchSemantic { query } => {
            let q = query.as_str();
            // GATE: the master flag lives in the (whole-DB-encrypted) settings table. When OFF,
            // return an explicit "disabled" result — do NOT silently fall back to an ungated read.
            // No `vec_chunks` row is ever touched.
            if !config.semantic_search_enabled {
                return Ok(
                    "Semantic search is disabled. Enable it in Murmur settings to use this tool."
                        .to_string(),
                );
            }
            // Embed the query with the SAME active embedder used to index, then HYBRID-search through
            // the SAME visibility gate as `search_meetings` (both FTS + vector legs are gated).
            let embedder = crate::embed::active_embedder();
            let query_vec = match embedder.embed(std::slice::from_ref(&q.to_string())) {
                Ok(v) => v.into_iter().next().unwrap_or_default(),
                Err(e) => return Err(AppError::Summarize(format!("embed failed: {e}"))),
            };
            match db.search_hybrid_visible(q, &query_vec, 20, unlocked) {
                Ok(hits) if hits.is_empty() => Ok(format!("No meetings match \"{q}\".")),
                Ok(hits) => Ok(format_hits(&hits)),
                Err(e) => Err(AppError::Storage(format!("semantic search failed: {e}"))),
            }
        }
        ToolCall::GetMeeting { meeting_id } => {
            let mid = meeting_id.as_str();
            // A sealed-and-not-unlocked meeting is invisible — including its transcript.
            match db.meeting_is_visible(mid, unlocked) {
                Ok(false) => Ok(format!("No data for meeting {mid}.")),
                Err(e) => Err(AppError::Storage(format!("visibility check failed: {e}"))),
                Ok(true) => {
                    let note = db.get_note_if_visible(mid, unlocked).ok().flatten();
                    let segs = db.get_segments(mid).unwrap_or_default();
                    let transcript = segs
                        .iter()
                        .map(|s| s.text.trim())
                        .filter(|t| !t.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                    match note {
                        Some(n) => Ok(format!("NOTE:\n{}\n\nTRANSCRIPT:\n{transcript}", n.markdown)),
                        None if !transcript.is_empty() => Ok(format!("TRANSCRIPT:\n{transcript}")),
                        None => Ok(format!("No data for meeting {mid}.")),
                    }
                }
            }
        }
        ToolCall::ListRecentMeetings { limit } => {
            match db.list_meetings_visible(*limit, unlocked) {
                Ok(ms) => Ok(ms
                    .iter()
                    .map(|m| {
                        format!(
                            "- {} · {} · {:?} · id:{}",
                            m.title.clone().unwrap_or_else(|| "(untitled)".into()),
                            m.started_at,
                            m.status,
                            m.id
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")),
                Err(e) => Err(AppError::Storage(format!("list failed: {e}"))),
            }
        }
        ToolCall::GetOpenCommitments { owner } => {
            // GATE: routes through `list_open_commitments`, which double-gates on the same
            // `unlocked` set (`list_meetings_visible` + `get_note_if_visible`) — a sealed-and-not-
            // unlocked meeting's commitments are never read here, so they can never surface.
            let owner = owner.as_deref().map(str::trim).filter(|o| !o.is_empty());
            match db.list_open_commitments(unlocked, owner) {
                Ok(items) if items.is_empty() => Ok(match owner {
                    Some(o) => format!("No open commitments for \"{o}\"."),
                    None => "No open commitments.".to_string(),
                }),
                Ok(items) => Ok(format_commitments(&items)),
                Err(e) => Err(AppError::Storage(format!("commitments rollup failed: {e}"))),
            }
        }
        ToolCall::GetEntityDossier { entity } => {
            // EGRESS-FREE: returns the GATED STRUCTURED DATA for the CLIENT to synthesize. Every read
            // inside `build_dossier_data` is visibility-gated against `unlocked`, so a sealed-and-not-
            // unlocked meeting contributes nothing. No provider / `complete` is ever constructed here.
            let entity = entity.as_str();
            let id = match crate::summarize::dossier::resolve_entity_id(db, entity, unlocked) {
                Ok(Some(id)) => id,
                Ok(None) => return Ok(format!("No visible entity matching \"{entity}\".")),
                Err(e) => return Err(AppError::Storage(format!("entity resolve failed: {e}"))),
            };
            match crate::summarize::dossier::build_dossier_data(db, &id, unlocked) {
                Ok(Some(data)) => Ok(crate::summarize::dossier::format_dossier_client(&data)),
                Ok(None) => Ok(format!("No visible entity matching \"{entity}\".")),
                Err(e) => Err(AppError::Storage(format!("dossier build failed: {e}"))),
            }
        }
    }
}

/// Render the open-commitments rollup into the tool text payload — one line per item:
/// `- owner · due · "text" · [[Title]]` (owner/due omitted when absent).
fn format_commitments(items: &[crate::storage::models::Commitment]) -> String {
    items
        .iter()
        .map(|c| {
            let mut parts: Vec<String> = Vec::new();
            if let Some(o) = c.owner.as_deref().map(str::trim).filter(|o| !o.is_empty()) {
                parts.push(o.to_string());
            }
            if let Some(d) = c.due_date.as_deref().filter(|d| !d.is_empty()) {
                parts.push(format!("due {d}"));
            }
            parts.push(format!("\"{}\"", c.text.trim()));
            parts.push(format!("[[{}]]", c.meeting_title));
            format!("- {}", parts.join(" · "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render a list of search hits (FTS or hybrid) into the tool text payload — one line per meeting.
fn format_hits(hits: &[crate::storage::models::SearchHit]) -> String {
    hits.iter()
        .map(|h| {
            format!(
                "- {} ({}) [id:{}] — {}",
                h.meeting.title.clone().unwrap_or_else(|| "(untitled)".into()),
                h.meeting.started_at,
                h.meeting.id,
                h.snippet
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
