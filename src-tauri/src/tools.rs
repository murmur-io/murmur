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
    /// CONNECTOR — live WEB SEARCH ("research about the world"). This is the ONE [`ToolCall`] that can
    /// EGRESS: it reaches an external search service through the consent-gated, redacting connector
    /// framework ([`crate::connectors`]). It is NOT runnable through the synchronous [`execute_tool`]
    /// (which is egress-free, and is the MCP surface's only entry) — it is dispatched ONLY via the
    /// async [`execute_web_search`], and ONLY when the web connector is exposed (enabled + consented +
    /// keyed). The brain decides web-vs-vault; see `orchestrate.rs` / `voice_action.rs`.
    WebSearch { query: String },
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
            // QUERY side: e5 `query:` prefix (asymmetric with the `passage:` index side).
            let query_vec = match embedder.embed_query(std::slice::from_ref(&q.to_string())) {
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
        ToolCall::WebSearch { .. } => {
            // EGRESS GUARD: the synchronous, egress-free `execute_tool` is the MCP surface's only
            // entry, so it MUST NOT run a connector (which reaches off-device). Web search is
            // dispatched exclusively via the async `execute_web_search`. Reaching here is a
            // programming error in a caller, never a leak — refuse loudly, egress nothing.
            Err(AppError::InvalidArg(
                "WebSearch is an egress connector and cannot run through the egress-free tool path; \
                 use execute_web_search"
                    .to_string(),
            ))
        }
    }
}

/// CONNECTOR DISPATCH — run a live WEB SEARCH through the consent-gated, redacting connector
/// framework, returning the same text-payload shape the vault tools return (so the brain treats web
/// hits and vault hits identically). ASYNC + egress-bearing, kept OUT of [`execute_tool`] so the
/// synchronous MCP surface can never reach it.
///
/// EGRESS DISCIPLINE (all three enforced before anything leaves the device):
/// - **Consent-gated, fail-closed:** the registry exposes the web connector ONLY when
///   `web_search_enabled && web_search_consented && a key is present`. When it is not exposed this
///   returns the explicit `"Web search is not available …"` sentinel and EGRESSES NOTHING (the
///   underlying [`crate::connectors::ConnectorError::NeedsConsent`]).
/// - **Redacted:** [`crate::connectors::ConnectorRegistry::search`] scrubs the query through the
///   redaction firewall BEFORE the provider call.
/// - **Loud:** every line carries the hit's `source_label` (e.g. "web · Brave") so the answer is
///   attributed to the web.
///
/// Returns a `"No web results …"` / `"Web search is not available …"` sentinel (matched by the
/// brain's `is_empty_result`) when nothing usable comes back, so an unavailable/empty web tool never
/// pollutes the grounding. A real network failure surfaces as `Err`.
pub async fn execute_web_search(query: &str, config: &AppConfig) -> Result<String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok("No web results for an empty query.".to_string());
    }
    let registry = crate::connectors::ConnectorRegistry::build(config);
    match registry.search("web", q).await {
        Ok(hits) if hits.is_empty() => Ok(format!("No web results for \"{q}\".")),
        Ok(hits) => Ok(format_web_hits(&hits)),
        // Fail-closed / not-exposed → a graceful sentinel (NOT an error), so the brain just skips it.
        Err(crate::connectors::ConnectorError::NeedsConsent) => {
            Ok("Web search is not available (not enabled, not consented, or no API key set).".to_string())
        }
        Err(crate::connectors::ConnectorError::Unconfigured(_)) => {
            Ok("Web search is not available (not configured).".to_string())
        }
        // A real external failure (network/HTTP/parse) is surfaced; the caller logs + skips it.
        Err(e @ crate::connectors::ConnectorError::Failed(_)) => Err(e.into()),
    }
}

/// Render web connector hits into the tool text payload — one line per result, each LOUD with its
/// source label + URL: `- [web · Brave] Title — snippet (url)`.
fn format_web_hits(hits: &[crate::connectors::ConnectorHit]) -> String {
    hits.iter()
        .map(|h| {
            let mut line = format!("- [{}] {}", h.source_label, h.title.trim());
            let snippet = h.snippet.trim();
            if !snippet.is_empty() {
                line.push_str(&format!(" — {snippet}"));
            }
            let url = h.url.trim();
            if !url.is_empty() {
                line.push_str(&format!(" ({url})"));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::AppConfig;

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn tmp_db() -> Db {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-tools-web-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        Db::open_with_key(&p, TEST_DEK).unwrap()
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// EGRESS GUARD: the synchronous, egress-free `execute_tool` (the MCP surface's only entry) MUST
    /// refuse a `WebSearch` call — it can never run a connector that reaches off-device.
    #[test]
    fn sync_execute_tool_refuses_websearch() {
        let db = tmp_db();
        let nothing = HashSet::new();
        let cfg = AppConfig::default();
        let res = execute_tool(
            &ToolCall::WebSearch { query: "weather".into() },
            &db,
            &nothing,
            &cfg,
        );
        assert!(
            matches!(res, Err(AppError::InvalidArg(_))),
            "the egress-free tool path must refuse WebSearch (no connector through MCP)"
        );
    }

    /// FAIL-CLOSED: with the default config (web search disabled + unconsented), `execute_web_search`
    /// returns the graceful "not available" sentinel and EGRESSES NOTHING — no key, no network.
    #[test]
    fn web_search_fail_closed_returns_sentinel_no_egress() {
        let cfg = AppConfig::default(); // web_search_enabled = false, consented = false
        let out = block_on(execute_web_search("what's the weather in Kraków", &cfg)).unwrap();
        assert!(
            out.starts_with("Web search is not available"),
            "unexposed web search must return the not-available sentinel: {out}"
        );
    }

    /// An empty query never builds the registry / reaches a connector.
    #[test]
    fn web_search_empty_query_is_inert() {
        let cfg = AppConfig::default();
        let out = block_on(execute_web_search("   ", &cfg)).unwrap();
        assert!(out.starts_with("No web results"));
    }

    /// LOUD: web hits render with their source label + URL, so the answer is attributed "via web".
    #[test]
    fn format_web_hits_is_loud_with_source_and_url() {
        let hits = vec![
            crate::connectors::ConnectorHit {
                title: "Kraków weather".into(),
                snippet: "Sunny, 22°C".into(),
                url: "https://w.example/krakow".into(),
                source_label: "web · Brave".into(),
            },
            crate::connectors::ConnectorHit {
                title: "No URL result".into(),
                snippet: String::new(),
                url: String::new(),
                source_label: "web · Brave".into(),
            },
        ];
        let out = format_web_hits(&hits);
        assert!(out.contains("[web · Brave] Kraków weather"), "loud source label: {out}");
        assert!(out.contains("Sunny, 22°C"), "snippet present: {out}");
        assert!(out.contains("(https://w.example/krakow)"), "url present: {out}");
        // A hit with no snippet/url still renders its labelled title.
        assert!(out.contains("[web · Brave] No URL result"), "labelled even without url: {out}");
    }
}
