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
//! There is no constructor that lets a caller skip the gate. The WRITE tools (`save_note`,
//! `create_reminder`) live on [`GatedToolExecutor`] (NOT [`execute_tool`]): they run only when the
//! executor was built with `allow_writes`, and `save_note` re-checks `meeting_is_visible` against the
//! live `unlocked` set BEFORE it appends to `manual_notes` — a sealed-not-unlocked meeting refuses.
//! The `propose_note` tool (advertised on note-capable surfaces via `note_drafts`) writes NO DB AT
//! ALL — it only records a note DRAFT in interior-mutable scratch for the FE to offer "Add to
//! notes"; the user commits it on Accept. So it needs no gate (it touches no content store) and
//! carries no leak surface.
//!
//! ## Egress
//! [`execute_tool`] EGRESSES NOTHING — it only reads the local SQLite DB and (for semantic search)
//! runs the local embedder; it never constructs a cloud provider or makes a network call. The
//! `create_reminder` write reaches the local macOS Reminders app via osascript (on-device, no network);
//! `save_note` writes only the local DB.

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
    /// CONNECTOR — LOCAL CALENDAR lookup ("who's in my next meeting", a pre-meeting brief). Reads the
    /// user's on-device macOS calendar via the bundled EventKit sidecar. Unlike [`Self::WebSearch`]
    /// this EGRESSES NOTHING (it is [`crate::connectors::EgressClass::Local`]) — but it still needs an
    /// `AppHandle` to resolve + drive the sidecar, which the synchronous, AppHandle-free
    /// [`execute_tool`] does not have. So, like `WebSearch`, it is dispatched ONLY via the async
    /// [`execute_calendar_search`], NEVER through `execute_tool`.
    CalendarLookup { query: String },
}

/// Model-facing description of one tool the agentic brain may call. `parameters` is a JSON-schema
/// object (the same shape `mcp.rs` advertises). A `write: true` tool MUTATES state: it is exposed to
/// the agentic loop ONLY when the executor was built with `allow_writes` (the in-meeting loop), and
/// even then every write goes through the gated executor (write only to an UNLOCKED/visible meeting —
/// a sealed-not-unlocked meeting refuses). The MCP read-only surface (which constructs no executor
/// with writes) never sees them. This is the single source of truth for the model-facing catalog.
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: serde_json::Value,
    pub write: bool,
}

/// The model-facing tool catalog. Built per call (cheap; ~11 entries). The read tools map 1:1 onto
/// gated `execute_tool` / connector dispatchers. `propose_note` is `write: false` (advertised on
/// surfaces built with `note_drafts` — the in-meeting loops; NOT the vault-wide Ask page, which has
/// no Accept affordance): it has NO DB side effect — it only RECORDS a note DRAFT for the user to
/// review/accept via the FE, so it is the model-driven "the user asked for a note" signal that
/// needs no write capability. The two
/// `write: true` entries (`save_note`, `create_reminder`) map onto the gated write arms of
/// `GatedToolExecutor::run` and are advertised ONLY when the executor was built with `allow_writes`.
pub fn tool_specs() -> Vec<ToolSpec> {
    let str_arg = |prop: &'static str, desc: &str| -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { prop: { "type": "string", "description": desc } },
            "required": [prop]
        })
    };
    vec![
        ToolSpec {
            name: "search_meetings",
            description: "Full-text search across the user's past meeting titles, notes, transcripts, \
                          and imported documents/brain notes.",
            parameters: str_arg("query", "Search terms, in the user's own language."),
            write: false,
        },
        ToolSpec {
            name: "search_semantic",
            description: "Hybrid semantic + keyword search over meetings and imported documents/brain \
                          notes (finds related-by-meaning content). Falls back to keyword-only \
                          matching when semantic search is disabled.",
            parameters: str_arg("query", "A natural-language description of what to find."),
            write: false,
        },
        ToolSpec {
            name: "get_meeting",
            description: "Fetch one meeting's AI note and full transcript by its id (from a prior search hit).",
            parameters: str_arg("meetingId", "The meeting id from a prior search result."),
            write: false,
        },
        ToolSpec {
            name: "list_recent_meetings",
            description: "List the most recent meetings (newest first).",
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "limit": { "type": "integer", "description": "How many (1..=100)." } }
            }),
            write: false,
        },
        ToolSpec {
            name: "get_open_commitments",
            description: "Roll up every open action item, optionally filtered by owner.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "owner": { "type": "string", "description": "Optional owner filter." } }
            }),
            write: false,
        },
        ToolSpec {
            name: "get_entity_dossier",
            description: "Assemble what the vault knows about one person / project / entity.",
            parameters: str_arg("entity", "The entity name to look up."),
            write: false,
        },
        ToolSpec {
            name: "web_search",
            description: "Search the public web. Only available when the user has enabled + consented to web search; the result is loud-attributed '(via web)'.",
            parameters: str_arg("query", "What to look up on the web."),
            write: false,
        },
        ToolSpec {
            name: "calendar_lookup",
            description: "Look up the user's local (on-device) calendar for recent/upcoming events.",
            parameters: str_arg("query", "What meeting / agenda detail to find."),
            write: false,
        },
        ToolSpec {
            name: "propose_note",
            description: "When the user asks you to MAKE / SAVE / DRAFT / WRITE a note (e.g. \"make me \
                          a note about the decisions\", \"save that we ship Friday\", \"zapisz notatkę \
                          o …\"), call this with the note content, enriched with the relevant meeting \
                          context. This DRAFTS a note for the user to review and accept — it does NOT \
                          save anything itself. Do NOT call it for plain questions or conversation — \
                          just answer those normally.",
            parameters: str_arg("content", "The drafted note content, in the user's own language, enriched with meeting context."),
            write: false,
        },
        ToolSpec {
            name: "save_note",
            description: "Save a note for the user about the meeting currently being recorded. Use this \
                          when the user asks you to write/note/save/jot/remember something for THIS \
                          meeting (\"note that …\", \"save that I send the deck to Anna\"). The text is \
                          appended to the user's own meeting notes and folds into the finalized note.",
            parameters: str_arg("text", "The note text to save, in the user's own language."),
            write: true,
        },
        ToolSpec {
            name: "create_reminder",
            description: "Create a follow-up reminder in the user's Reminders app. Use this when the \
                          user asks to be reminded to DO something later (\"remind me to email Bob\", \
                          \"przypomnij mi …\"), i.e. an action with a future due — NOT a note about the \
                          meeting (use save_note for that).",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "What to be reminded about." },
                    "due": { "type": "string", "description": "Optional natural-language due time." }
                },
                "required": ["text"]
            }),
            write: true,
        },
    ]
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
            // Documents/brain notes ride the SAME fan-out — the gated keyword (FTS/BM25) doc leg
            // works WITHOUT the e5 model, so ingested content is reachable through the primary
            // search tool on a default install. `search_doc_chunks_fts_visible` applies the SAME
            // `visibility_clause` against `unlocked` as the meeting legs.
            let docs = db
                .search_doc_chunks_fts_visible(q, 20, unlocked)
                .unwrap_or_default();
            match db.search_visible(q, 20, unlocked) {
                Ok(hits) if hits.is_empty() && docs.is_empty() => {
                    Ok(format!("No meetings or documents match \"{q}\"."))
                }
                Ok(hits) => Ok(format_hits_and_docs(&hits, &docs)),
                Err(e) => Err(AppError::Storage(format!("search failed: {e}"))),
            }
        }
        ToolCall::SearchSemantic { query } => {
            let q = query.as_str();
            // GATE: the master flag lives in the (whole-DB-encrypted) settings table. When OFF,
            // DEGRADE HONESTLY to gated keyword (BM25) matching — never an ungated read, and no
            // `vec_chunks`/`doc_vec_chunks` row is ever touched (no stub-vector KNN). The output is
            // labelled as keyword matching so the model is never told a semantic search ran.
            if !config.semantic_search_enabled {
                let hits = db
                    .search_visible(q, 20, unlocked)
                    .map_err(|e| AppError::Storage(format!("search failed: {e}")))?;
                let docs = db
                    .search_doc_chunks_fts_visible(q, 20, unlocked)
                    .unwrap_or_default();
                if hits.is_empty() && docs.is_empty() {
                    return Ok(format!(
                        "No meetings or documents match \"{q}\" (keyword match — semantic search is off)."
                    ));
                }
                return Ok(format!(
                    "Keyword (exact-word) matches — semantic search is off:\n{}",
                    format_hits_and_docs(&hits, &docs)
                ));
            }
            // Embed the query with the SAME active embedder used to index, then HYBRID-search through
            // the SAME visibility gate as `search_meetings` (both FTS + vector legs are gated).
            let embedder = crate::embed::active_embedder();
            // QUERY side: e5 `query:` prefix (asymmetric with the `passage:` index side).
            let query_vec = match embedder.embed_query(std::slice::from_ref(&q.to_string())) {
                Ok(v) => v.into_iter().next().unwrap_or_default(),
                Err(e) => return Err(AppError::Summarize(format!("embed failed: {e}"))),
            };
            // Document ingestion: ALSO surface uploaded md/txt that match — the vector-KNN and
            // keyword-FTS doc legs are RRF-fused, and BOTH are gated by the SAME `visibility_clause`
            // against `unlocked`, so a locked-and-not-unlocked folder's documents are invisible here
            // exactly like a sealed meeting. Appended as a `DOCUMENTS:` section.
            let knn_docs = db
                .search_doc_chunks_visible(&query_vec, 20, unlocked)
                .unwrap_or_default();
            let fts_docs = db
                .search_doc_chunks_fts_visible(q, 20, unlocked)
                .unwrap_or_default();
            let docs = crate::embed::fuse_doc_hits(knn_docs, fts_docs);
            match db.search_hybrid_visible(q, &query_vec, 20, unlocked) {
                Ok(hits) if hits.is_empty() && docs.is_empty() => {
                    Ok(format!("No meetings or documents match \"{q}\"."))
                }
                Ok(hits) => Ok(format_hits_and_docs(&hits, &docs)),
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
        ToolCall::CalendarLookup { .. } => {
            // The local-calendar connector egresses NOTHING, but it needs an `AppHandle` to drive
            // the bundled EventKit sidecar — which the synchronous, AppHandle-free `execute_tool`
            // (the MCP surface's only entry) does not have. It is dispatched exclusively via the
            // async `execute_calendar_search`. Reaching here is a programming error in a caller —
            // refuse loudly. (This is NOT a leak: nothing was read, nothing egressed.)
            Err(AppError::InvalidArg(
                "CalendarLookup needs the async AppHandle path and cannot run through the \
                 synchronous tool path; use execute_calendar_search"
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

/// CONNECTOR DISPATCH — run a LOCAL CALENDAR lookup through the connector seam, returning the same
/// text-payload shape the vault/web tools return (so the brain treats calendar context identically).
/// ASYNC because it drives the bundled EventKit sidecar via [`crate::calendar::fetch_events`], which
/// needs the [`AppHandle`] to resolve the bundled binary — kept OUT of the synchronous, AppHandle-free
/// [`execute_tool`].
///
/// EGRESS: NONE. The macOS calendar is read ON-DEVICE; this is [`crate::connectors::EgressClass::Local`]
/// and is therefore NOT consent-gated. `fetch_events` degrades to an empty `Vec` on EVERY failure
/// (sidecar missing, denied Calendars permission, timeout, malformed JSON), so a denied permission
/// just yields no hits — graceful, never an error. (If this text is later folded into a CLOUD brain
/// prompt it rides the EXISTING make_provider redaction firewall + consent — no new egress class.)
///
/// Window: `[now - 60m, now + 720m]` (recent + the next ~12h), matching `commands.rs`'s
/// `calendar_context_for`. Returns a `"No calendar events …"` sentinel (matched by the brain's
/// `is_empty_result`) when nothing matches, so an empty calendar never pollutes the grounding.
pub async fn execute_calendar_search(query: &str, app: &tauri::AppHandle) -> Result<String> {
    let q = query.trim();
    // ON-DEVICE read: drive the bundled sidecar; ANY failure → empty Vec (never an error).
    let events = crate::calendar::fetch_events(app, 60, 720).await;
    // `Connector::search` in scope for the call below (the connector impls the trait).
    use crate::connectors::Connector as _;
    let hits = crate::connectors::calendar::CalendarConnector::new(events)
        .search(q)
        .await?;
    if hits.is_empty() {
        return Ok(if q.is_empty() {
            "No calendar events in the window.".to_string()
        } else {
            format!("No calendar events match \"{q}\".")
        });
    }
    Ok(format_calendar_hits(&hits))
}

/// Render local-calendar connector hits into the tool text payload — one block per event, each LOUD
/// with its source label: `[calendar] Title — <bounded Meeting/When/Attendees/Agenda context>`. The
/// snippet already carries newlines (the CalendarContext block); we keep it intact so the brain sees
/// the full who/when/agenda, just prefixed with the loud `[calendar]` attribution.
fn format_calendar_hits(hits: &[crate::connectors::ConnectorHit]) -> String {
    hits.iter()
        .map(|h| {
            let mut line = format!("[{}] {}", h.source_label, h.title.trim());
            let snippet = h.snippet.trim();
            if !snippet.is_empty() {
                line.push_str(&format!(" — {snippet}"));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n\n")
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

/// Format meeting hits + gated document-chunk hits (the document-ingestion search leg). Documents are
/// NOT meetings (no id/date citation), so they get their own `DOCUMENTS:` section listing the source
/// name + snippet. Both inputs are already visibility-gated by the caller.
fn format_hits_and_docs(
    hits: &[crate::storage::models::SearchHit],
    docs: &[crate::storage::models::DocChunkHit],
) -> String {
    let mut out = String::new();
    if !hits.is_empty() {
        out.push_str(&format_hits(hits));
    }
    if !docs.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str("DOCUMENTS:\n");
        out.push_str(
            &docs
                .iter()
                .map(|d| format!("- {} — {}", d.name, d.snippet))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    out
}

/// THE one gated, egress-aware tool executor shared by the agentic loop (cloud + local, voice + text).
/// Holds the LIVE session `unlocked` set behind its `Mutex` and RE-READS it on EVERY tool call (not
/// once at loop start), so a mid-loop screen-share auto-relock is honored immediately — a folder
/// relocked during a multi-second turn becomes invisible to the very next tool call. Every read
/// routes through the visibility gate regardless of what the model requests: the model can ASK for a
/// search, but a sealed-not-unlocked meeting is invisible by construction. Connectors (web/calendar)
/// need the `AppHandle`; write tools need `allow_writes` (ON for the in-meeting loop, where the agent
/// DECIDES — via tool-use, no hardcoded regex — whether to answer or write; OFF for the MCP/read
/// surfaces). A write executes ONLY when `allow_writes` AND the target meeting is visible to the live
/// `unlocked` set — a sealed-not-unlocked meeting refuses (`AppError::Locked`), never resurrecting
/// plaintext behind a lock.
///
/// `proposed_note` is interior-mutable scratch for the always-on `propose_note` tool: when the model
/// decides the user asked for a NOTE (vs a plain answer), it calls `propose_note(content)`, which
/// records the draft HERE (no DB write). The caller (`run_informational`) reads it after the loop and
/// threads it onto the result so the FE can offer "Add to notes" — the user commits on Accept.
pub struct GatedToolExecutor<'a> {
    pub db: &'a Db,
    pub unlocked: &'a std::sync::Mutex<HashSet<String>>,
    pub config: &'a AppConfig,
    pub meeting_id: &'a str,
    pub app: Option<&'a tauri::AppHandle>,
    pub allow_writes: bool,
    /// Advertise the DB-free `propose_note` DRAFT tool on this surface. TRUE for the in-meeting
    /// surfaces (they have a notes flow + an "Add to notes" Accept affordance); FALSE for surfaces
    /// with no notes flow (the vault-wide Ask page), where a drafted note could never be accepted.
    /// The tool stays implemented either way — un-advertised, the `run()` allowlist refuses it.
    pub note_drafts: bool,
    /// The note draft the model proposed this turn (via `propose_note`), if any. `None` ⇒ the reply
    /// is a plain ANSWER; `Some(content)` ⇒ a NOTE PROPOSAL the FE should offer to add. No DB effect.
    pub proposed_note: std::sync::Mutex<Option<String>>,
}

impl crate::agent::ToolExecutor for GatedToolExecutor<'_> {
    fn specs(&self) -> Vec<ToolSpec> {
        let has_app = self.app.is_some();
        let allow_writes = self.allow_writes;
        tool_specs()
            .into_iter()
            .filter(|s| match s.name {
                // Connectors require the AppHandle (async sidecar / consent path).
                "web_search" | "calendar_lookup" => has_app,
                // The draft tool is advertised only on surfaces with a notes flow / Accept
                // affordance (in-meeting yes, the vault-wide Ask page no).
                "propose_note" => self.note_drafts,
                // Write actions require explicit allow_writes (off in the v1 loop).
                _ if s.write => allow_writes,
                _ => true,
            })
            .collect()
    }

    fn run(&self, name: &str, args: &serde_json::Value) -> Result<String> {
        // ENFORCE the allowlist: the model can NEVER run a tool we did not advertise this turn.
        if !self.specs().iter().any(|s| s.name == name) {
            return Err(AppError::InvalidArg(format!("tool '{name}' is not available")));
        }
        // RE-READ the live unlocked set on THIS call (C6): a folder relocked mid-loop is gated out
        // immediately, never seen through a snapshot taken at loop start.
        let unlocked = self
            .unlocked
            .lock()
            .map_err(|_| AppError::Other(anyhow::anyhow!("unlocked set mutex poisoned")))?
            .clone();
        let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match name {
            "search_meetings" => {
                execute_tool(&ToolCall::SearchMeetings { query: s("query") }, self.db, &unlocked, self.config)
            }
            "search_semantic" => {
                execute_tool(&ToolCall::SearchSemantic { query: s("query") }, self.db, &unlocked, self.config)
            }
            "get_meeting" => {
                execute_tool(&ToolCall::GetMeeting { meeting_id: s("meetingId") }, self.db, &unlocked, self.config)
            }
            "list_recent_meetings" => {
                let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(10).clamp(1, 100);
                execute_tool(&ToolCall::ListRecentMeetings { limit }, self.db, &unlocked, self.config)
            }
            "get_open_commitments" => {
                let owner = args.get("owner").and_then(|v| v.as_str()).map(str::to_string);
                execute_tool(&ToolCall::GetOpenCommitments { owner }, self.db, &unlocked, self.config)
            }
            "get_entity_dossier" => {
                execute_tool(&ToolCall::GetEntityDossier { entity: s("entity") }, self.db, &unlocked, self.config)
            }
            "web_search" => match self.app {
                Some(_) => block_on_tool(execute_web_search(&s("query"), self.config)),
                None => Err(AppError::InvalidArg("web_search needs an AppHandle".into())),
            },
            "calendar_lookup" => match self.app {
                Some(app) => block_on_tool(execute_calendar_search(&s("query"), app)),
                None => Err(AppError::InvalidArg("calendar_lookup needs an AppHandle".into())),
            },
            // ── PROPOSE (always-on, NO DB side effect): the model signals the user asked for a note.
            //    Records the draft in interior-mutable scratch; the caller threads it onto the result so
            //    the FE can offer "Add to notes". Writes NOTHING — the user commits on Accept.
            "propose_note" => self.propose_note(&s("content")),
            // ── WRITE tools (advertised only when `allow_writes`; the allowlist check above already
            //    refused them otherwise). Each is GATED to a VISIBLE/unlocked meeting before it mutates.
            "save_note" => self.save_note(&s("text"), &unlocked),
            "create_reminder" => {
                let due = args.get("due").and_then(|v| v.as_str()).filter(|d| !d.trim().is_empty());
                self.create_reminder(&s("text"), due)
            }
            other => Err(AppError::InvalidArg(format!("unknown tool '{other}'"))),
        }
    }
}

impl GatedToolExecutor<'_> {
    /// PROPOSE (no DB side effect) — the model decided the user asked for a NOTE: record the drafted
    /// `content` in the interior-mutable `proposed_note` scratch so the caller can thread it onto the
    /// result and the FE can offer "Add to notes". This NEVER writes `manual_notes` or any DB row — the
    /// user commits the draft on Accept (→ `save_manual_notes`). PII rule: log id/len ONLY, never the
    /// proposed content. The LAST proposal wins if the model (mistakenly) proposes twice in one turn.
    fn propose_note(&self, content: &str) -> Result<String> {
        let content = content.trim();
        if content.is_empty() {
            return Err(AppError::InvalidArg("nothing to propose".into()));
        }
        // Record the draft (no DB write). A poisoned mutex is a programming error, not a leak.
        *self
            .proposed_note
            .lock()
            .map_err(|_| AppError::Other(anyhow::anyhow!("proposed_note mutex poisoned")))? =
            Some(content.to_string());
        tracing::debug!(target: "agent", len = content.len(), "agent proposed a note draft (not written)");
        Ok("Drafted a note for the user to review.".to_string())
    }

    /// WRITE — append the agent's note to the CURRENT meeting's durable typed-notes buffer
    /// (`meetings.manual_notes`, Feature A): the sealed-and-restored, folds-into-the-note home. Read
    /// the existing buffer, append the new line, and write it back — a non-destructive append (the
    /// folder seal lifecycle still owns verify-before-destroy; this only ever GROWS plaintext for a
    /// VISIBLE meeting). GATED: refuses (`AppError::Locked`) when there is no live meeting or the
    /// meeting is sealed-not-unlocked, so the agent can never resurrect/write plaintext behind a lock.
    fn save_note(&self, text: &str, unlocked: &HashSet<String>) -> Result<String> {
        let text = text.trim();
        if text.is_empty() {
            return Err(AppError::InvalidArg("nothing to note".into()));
        }
        let meeting_id = self.meeting_id;
        if meeting_id.is_empty() {
            return Err(AppError::Locked(
                "no meeting is being recorded — there is nothing to attach the note to".into(),
            ));
        }
        // GATE: write ONLY to a meeting the live session can see. The in-progress recording has no
        // note row yet → trivially visible; a sealed-not-unlocked meeting is refused, never written.
        if !self.db.meeting_is_visible(meeting_id, unlocked)? {
            return Err(AppError::Locked(
                "this meeting is locked — unlock it to save a note".into(),
            ));
        }
        // Non-destructive APPEND onto the durable buffer (newline-separated). `get_manual_notes`
        // returns "" for an empty/never-set buffer, so the first note seeds it cleanly.
        let existing = self.db.get_manual_notes(meeting_id).unwrap_or_default();
        let merged = if existing.trim().is_empty() {
            text.to_string()
        } else {
            format!("{existing}\n{text}")
        };
        self.db.set_manual_notes(meeting_id, &merged)?;
        // PII rule: log the meeting id + new-buffer length only — never the note text.
        tracing::debug!(target: "agent", meeting_id = %meeting_id, len = merged.len(), "agent saved a note to manual_notes");
        Ok("Saved a note to this meeting.".to_string())
    }

    /// WRITE — create a follow-up reminder in the user's Reminders app via the existing blocking
    /// osascript path. No meeting content is touched (it is an external action, not a vault write), so
    /// there is no sealed-content surface here; an empty text is refused. The `allow_writes` gate + the
    /// per-turn allowlist (checked in `run`) are what authorize reaching this at all.
    fn create_reminder(&self, text: &str, due: Option<&str>) -> Result<String> {
        let text = text.trim();
        if text.is_empty() {
            return Err(AppError::InvalidArg("nothing to remind about".into()));
        }
        crate::commands::add_reminder_blocking(text, due)?;
        // PII rule: never log the reminder text (it is the user's own words).
        tracing::debug!(target: "agent", "agent created a reminder");
        Ok("Created a follow-up reminder.".to_string())
    }
}

/// Drive an async connector dispatcher to completion from the synchronous executor without panicking,
/// regardless of caller context (the loop may run inside the async note pipeline). Mirrors
/// `reason::block_on_complete` / `voice_action::web_search_blocking`: a dedicated scoped OS thread with
/// its own current-thread runtime, so we never "start a runtime within a runtime" and the future never
/// crosses a thread boundary (only the `Result<String>` does).
fn block_on_tool(
    fut: impl std::future::Future<Output = Result<String>> + Send,
) -> Result<String> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| AppError::Other(anyhow::anyhow!("tool runtime build failed: {e}")))?
                    .block_on(fut)
            })
            .join()
            .map_err(|_| AppError::Other(anyhow::anyhow!("tool worker thread panicked")))?
    })
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

    /// EGRESS/PLUMBING GUARD: the synchronous, AppHandle-free `execute_tool` MUST refuse a
    /// `CalendarLookup` (it needs the async sidecar/AppHandle path) — exactly like `WebSearch`.
    #[test]
    fn sync_execute_tool_refuses_calendar_lookup() {
        let db = tmp_db();
        let nothing = HashSet::new();
        let cfg = AppConfig::default();
        let res = execute_tool(
            &ToolCall::CalendarLookup { query: "standup".into() },
            &db,
            &nothing,
            &cfg,
        );
        assert!(
            matches!(res, Err(AppError::InvalidArg(_))),
            "the synchronous tool path must refuse CalendarLookup (needs the async AppHandle path)"
        );
    }

    /// LOUD: calendar hits render with their `[calendar]` source label + the bounded context block,
    /// so a calendar-grounded answer is visibly attributed to the user's calendar.
    #[test]
    fn format_calendar_hits_is_loud_with_source_and_context() {
        let hits = vec![
            crate::connectors::ConnectorHit {
                title: "Sprint Planning".into(),
                snippet: "Meeting: Sprint Planning\nAttendees: Alice, Bob\nAgenda:\n- velocity".into(),
                url: String::new(),
                source_label: "calendar".into(),
            },
            crate::connectors::ConnectorHit {
                title: "1:1".into(),
                snippet: "Meeting: 1:1".into(),
                url: String::new(),
                source_label: "calendar".into(),
            },
        ];
        let out = format_calendar_hits(&hits);
        assert!(out.contains("[calendar] Sprint Planning"), "loud source label: {out}");
        assert!(out.contains("Attendees: Alice, Bob"), "context block preserved: {out}");
        assert!(out.contains("velocity"), "agenda preserved: {out}");
        assert!(out.contains("[calendar] 1:1"), "every event labelled: {out}");
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

    // ── In-meeting WRITE tools: advertised only with allow_writes, executed, and GATED ──────────────
    // The agentic loop's NEW write surface (conversation-first design, 2026-06-30). The agent DECIDES
    // (answer vs write) — but every write is host-gated here: a sealed-not-unlocked meeting refuses.

    use crate::agent::ToolExecutor;
    use crate::storage::models::{Folder, Meeting, MeetingStatus, NoteRecord};
    use std::sync::Mutex;

    /// A live, un-foldered (visible) recording meeting — the in-progress recording the agent writes to.
    fn seed_live_meeting(db: &Db, id: &str) {
        db.insert_meeting(&Meeting {
            id: id.into(),
            started_at: "2026-06-30T09:00:00Z".into(),
            ended_at: None,
            title: Some("Live".into()),
            duration_s: 0,
            audio_path: None,
            status: MeetingStatus::Recording,
            folder_id: None,
        })
        .unwrap();
    }

    /// A SEALED-NOT-UNLOCKED meeting: a locked folder + a blanked-plaintext note row (the at-rest
    /// shape). With nothing in the `unlocked` set this meeting is invisible to `meeting_is_visible`.
    fn seed_sealed_meeting(db: &Db, id: &str, folder: &str) {
        db.insert_folder(&Folder {
            id: folder.into(),
            name: "Secret".into(),
            path: "Secret".into(),
            parent_id: None,
            locked: true,
            created_at: "2026-06-30T00:00:00Z".into(),
        })
        .unwrap();
        db.insert_meeting(&Meeting {
            id: id.into(),
            started_at: "2026-06-30T08:00:00Z".into(),
            ended_at: None,
            title: Some("Sealed".into()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: id.into(),
            provider_id: "claude_code".into(),
            markdown: String::new(), // blanked plaintext + a folder ⇒ sealed-not-unlocked for the gate.
            created_at: "2026-06-30T08:05:00Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder(id, Some(folder)).unwrap();
    }

    /// With `allow_writes: true`, the in-meeting executor ADVERTISES the write tools to the model
    /// (`save_note` + `create_reminder`) — the catalog the agent reads. With `allow_writes: false`
    /// (the MCP/read surface) they are HIDDEN. RED-able: drop the `_ if s.write => allow_writes` filter
    /// and the read surface would leak the write tools.
    #[test]
    fn write_tools_advertised_only_when_allow_writes() {
        let db = tmp_db();
        let cfg = AppConfig::default();
        let unlocked = Mutex::new(HashSet::new());

        let writeable = GatedToolExecutor {
            db: &db,
            unlocked: &unlocked,
            config: &cfg,
            meeting_id: "live1",
            app: None,
            allow_writes: true,
            note_drafts: true,
            proposed_note: Mutex::new(None),
        };
        let names: Vec<&str> = writeable.specs().iter().map(|s| s.name).collect();
        assert!(names.contains(&"save_note"), "the write loop must advertise save_note: {names:?}");
        assert!(
            names.contains(&"create_reminder"),
            "the write loop must advertise create_reminder: {names:?}"
        );
        // propose_note is always advertised (write: false), regardless of allow_writes.
        assert!(names.contains(&"propose_note"), "propose_note must always be advertised: {names:?}");

        let readonly = GatedToolExecutor {
            db: &db,
            unlocked: &unlocked,
            config: &cfg,
            meeting_id: "live1",
            app: None,
            allow_writes: false,
            note_drafts: true,
            proposed_note: Mutex::new(None),
        };
        let ro_names: Vec<&str> = readonly.specs().iter().map(|s| s.name).collect();
        assert!(
            !ro_names.iter().any(|n| *n == "save_note" || *n == "create_reminder"),
            "a read-only executor must NOT advertise write tools: {ro_names:?}"
        );
        // propose_note (write: false, no DB effect) is STILL advertised on a read-only executor — it is
        // the always-on path the model uses to signal a note draft.
        assert!(
            ro_names.contains(&"propose_note"),
            "propose_note must be advertised even when allow_writes is false: {ro_names:?}"
        );
    }

    /// The `save_note` write tool APPENDS to the meeting's durable `manual_notes` buffer (Feature A's
    /// sealed-and-restored, folds-into-the-note home — NOT the orphaned `notes_asides`). Asserts the
    /// column GROWS with each appended note (newline-separated, prior content preserved).
    #[test]
    fn save_note_appends_to_manual_notes() {
        let db = tmp_db();
        seed_live_meeting(&db, "live1");
        let cfg = AppConfig::default();
        let unlocked = Mutex::new(HashSet::new());
        let exec = GatedToolExecutor {
            db: &db,
            unlocked: &unlocked,
            config: &cfg,
            meeting_id: "live1",
            app: None,
            allow_writes: true,
            note_drafts: true,
            proposed_note: Mutex::new(None),
        };

        // First note SEEDS the empty buffer.
        let out = exec
            .run("save_note", &serde_json::json!({ "text": "send the deck to Anna" }))
            .unwrap();
        assert!(out.to_lowercase().contains("saved"), "confirmation text: {out}");
        assert_eq!(db.get_manual_notes("live1").unwrap(), "send the deck to Anna");

        // A second note APPENDS (newline-separated) — the buffer GROWS, the first note is preserved.
        exec.run("save_note", &serde_json::json!({ "text": "follow up with QA on Friday" }))
            .unwrap();
        assert_eq!(
            db.get_manual_notes("live1").unwrap(),
            "send the deck to Anna\nfollow up with QA on Friday",
            "save_note must APPEND to manual_notes, never overwrite the prior buffer"
        );

        // The orphaned notes_asides store is NOT touched (the agent writes the durable buffer).
        assert!(
            db.list_note_asides("live1").unwrap().is_empty(),
            "save_note must write manual_notes, NOT notes_asides"
        );
    }

    /// GATE: `save_note` REFUSES a write to a SEALED-not-unlocked meeting (`AppError::Locked`) and
    /// writes NOTHING. RED-able: drop the `meeting_is_visible` check in `save_note` and a locked
    /// meeting's buffer would be written (resurrecting plaintext behind a lock).
    #[test]
    fn save_note_refused_for_sealed_meeting() {
        let db = tmp_db();
        seed_sealed_meeting(&db, "sealed1", "fsec");
        let nothing = HashSet::new();
        // Seed self-check: the sealed meeting must be gated before we prove the refusal.
        assert!(
            !db.meeting_is_visible("sealed1", &nothing).unwrap(),
            "seed fixture: the sealed meeting must be sealed-not-unlocked"
        );

        let cfg = AppConfig::default();
        let unlocked = Mutex::new(HashSet::new()); // nothing unlocked ⇒ sealed1 invisible.
        let exec = GatedToolExecutor {
            db: &db,
            unlocked: &unlocked,
            config: &cfg,
            meeting_id: "sealed1",
            app: None,
            allow_writes: true,
            note_drafts: true,
            proposed_note: Mutex::new(None),
        };
        let res = exec.run("save_note", &serde_json::json!({ "text": "secret note" }));
        assert!(
            matches!(res, Err(AppError::Locked(_))),
            "a write to a sealed-not-unlocked meeting must be refused with AppError::Locked: {res:?}"
        );
        // And NOTHING was written (the buffer stays blanked/empty).
        assert_eq!(
            db.get_manual_notes("sealed1").unwrap(),
            "",
            "no plaintext may be written behind a lock"
        );
    }

    /// GATE: `save_note` with NO active recording (empty `meeting_id`) refuses — there is nothing to
    /// attach a note to, and we never write to an empty/unknown meeting.
    #[test]
    fn save_note_refused_without_active_recording() {
        let db = tmp_db();
        let cfg = AppConfig::default();
        let unlocked = Mutex::new(HashSet::new());
        let exec = GatedToolExecutor {
            db: &db,
            unlocked: &unlocked,
            config: &cfg,
            meeting_id: "", // no active recording
            app: None,
            allow_writes: true,
            note_drafts: true,
            proposed_note: Mutex::new(None),
        };
        let res = exec.run("save_note", &serde_json::json!({ "text": "orphan note" }));
        assert!(
            matches!(res, Err(AppError::Locked(_))),
            "no active recording ⇒ save_note refuses: {res:?}"
        );
    }

    /// ALLOWLIST: a read-only executor that does NOT advertise the write tools must REFUSE to run them
    /// even if a (mis-)caller names one directly — the `run` allowlist check fails closed.
    #[test]
    fn read_only_executor_refuses_to_run_write_tools() {
        let db = tmp_db();
        seed_live_meeting(&db, "live1");
        let cfg = AppConfig::default();
        let unlocked = Mutex::new(HashSet::new());
        let exec = GatedToolExecutor {
            db: &db,
            unlocked: &unlocked,
            config: &cfg,
            meeting_id: "live1",
            app: None,
            allow_writes: false, // read-only — write tools not advertised
            note_drafts: true,
            proposed_note: Mutex::new(None),
        };
        let res = exec.run("save_note", &serde_json::json!({ "text": "should not run" }));
        assert!(
            matches!(res, Err(AppError::InvalidArg(_))),
            "an un-advertised write tool must be refused by the allowlist: {res:?}"
        );
        // Nothing was written.
        assert_eq!(db.get_manual_notes("live1").unwrap(), "");
    }

    // ── propose_note: the always-on, DB-free NOTE-DRAFT signal (propose-then-accept, Rev 2) ─────────
    // The model DECIDES (no regex in our code) whether its reply is a plain answer or a note proposal;
    // when it proposes, the FE shows "Add to notes". The executor only RECORDS the draft — no DB write.

    /// `propose_note` is ADVERTISED regardless of `allow_writes` (it is `write: false` — no DB effect),
    /// on BOTH a read-only and a write-capable executor. The model-driven note-draft path is always on.
    #[test]
    fn propose_note_advertised_regardless_of_allow_writes() {
        let db = tmp_db();
        let cfg = AppConfig::default();
        let unlocked = Mutex::new(HashSet::new());
        for allow_writes in [false, true] {
            let exec = GatedToolExecutor {
                db: &db,
                unlocked: &unlocked,
                config: &cfg,
                meeting_id: "live1",
                app: None,
                allow_writes,
                note_drafts: true,
                proposed_note: Mutex::new(None),
            };
            let names: Vec<&str> = exec.specs().iter().map(|s| s.name).collect();
            assert!(
                names.contains(&"propose_note"),
                "propose_note must be advertised with allow_writes={allow_writes}: {names:?}"
            );
            // It is a read tool (write: false), so it is NOT gated out on the read surface.
            let spec = tool_specs().into_iter().find(|s| s.name == "propose_note").unwrap();
            assert!(!spec.write, "propose_note must be write: false (no DB side effect)");
        }
    }

    /// The `propose_note` arm RECORDS the drafted content into the executor's `proposed_note` scratch
    /// and writes NO DB — `manual_notes` is unchanged, NOTHING is persisted. RED-able: were the arm to
    /// call `set_manual_notes`, the assertion that the buffer stays empty would fail.
    #[test]
    fn propose_note_records_draft_and_writes_no_db() {
        let db = tmp_db();
        seed_live_meeting(&db, "live1");
        let cfg = AppConfig::default();
        let unlocked = Mutex::new(HashSet::new());
        let exec = GatedToolExecutor {
            db: &db,
            unlocked: &unlocked,
            config: &cfg,
            meeting_id: "live1",
            app: None,
            allow_writes: false, // propose works even read-only
            note_drafts: true,
            proposed_note: Mutex::new(None),
        };

        // Before any call the scratch is None ⇒ the reply would be a plain ANSWER.
        assert!(exec.proposed_note.lock().unwrap().is_none(), "no proposal until propose_note is called");

        let out = exec
            .run("propose_note", &serde_json::json!({ "content": "Decision: ship Friday; Anna owns QA." }))
            .unwrap();
        assert!(out.to_lowercase().contains("draft"), "confirmation mentions a draft: {out}");

        // The draft is RECORDED in scratch (this is what the caller threads onto the result).
        assert_eq!(
            exec.proposed_note.lock().unwrap().as_deref(),
            Some("Decision: ship Friday; Anna owns QA."),
            "propose_note must record the drafted content"
        );
        // And NOTHING was written to the DB — manual_notes is untouched (no persistence on propose).
        assert_eq!(
            db.get_manual_notes("live1").unwrap(),
            "",
            "propose_note must NOT write manual_notes (the user commits on Accept)"
        );
        assert!(
            db.list_note_asides("live1").unwrap().is_empty(),
            "propose_note must NOT write notes_asides either"
        );
    }

    /// `proposed_note` stays None when the model does NOT call `propose_note` (a plain answer turn) —
    /// only a read tool ran, so the scratch is never set.
    #[test]
    fn proposed_note_is_none_when_propose_not_called() {
        let db = tmp_db();
        seed_live_meeting(&db, "live1");
        let cfg = AppConfig::default();
        let unlocked = Mutex::new(HashSet::new());
        let exec = GatedToolExecutor {
            db: &db,
            unlocked: &unlocked,
            config: &cfg,
            meeting_id: "live1",
            app: None,
            allow_writes: false,
            note_drafts: true,
            proposed_note: Mutex::new(None),
        };
        // A plain read tool (the kind a question would use) does NOT set a proposal.
        let _ = exec.run("search_meetings", &serde_json::json!({ "query": "anything" })).unwrap();
        assert!(
            exec.proposed_note.lock().unwrap().is_none(),
            "an answer turn (no propose_note) must leave proposed_note None"
        );
    }

    /// An empty/whitespace `content` is refused (`InvalidArg`) and records NO draft — we never surface
    /// an empty proposal the FE would render as a blank "Add to notes".
    #[test]
    fn propose_note_refuses_empty_content() {
        let db = tmp_db();
        let cfg = AppConfig::default();
        let unlocked = Mutex::new(HashSet::new());
        let exec = GatedToolExecutor {
            db: &db,
            unlocked: &unlocked,
            config: &cfg,
            meeting_id: "live1",
            app: None,
            allow_writes: false,
            note_drafts: true,
            proposed_note: Mutex::new(None),
        };
        let res = exec.run("propose_note", &serde_json::json!({ "content": "   " }));
        assert!(matches!(res, Err(AppError::InvalidArg(_))), "empty proposal refused: {res:?}");
        assert!(exec.proposed_note.lock().unwrap().is_none(), "no draft recorded for an empty proposal");
    }
}
