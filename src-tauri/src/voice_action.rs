//! In-meeting VOICE ACTION DISPATCH — the BACKEND ENGINE (Phase E, Flow B).
//!
//! When the user addresses the assistant mid-meeting ("Claudku, zrób research o X"), the live loop
//! ([`crate::transcribe::live`]) has already DETECTED the wake + parsed a [`VoiceIntent`] (Phase A).
//! This module EXECUTES that intent — best-effort, gated, panic-free — and returns a
//! [`VoiceActionResult`] the live loop emits as a live event for the UI (the rich card is Phase H).
//!
//! ## Lock / egress invariants (load-bearing)
//! - **Every content read is GATED.** Research / Recall route through [`crate::tools::execute_tool`]
//!   over the LIVE session `unlocked` set, so every DB read is `visibility_clause`-gated — a
//!   sealed-and-not-session-unlocked meeting contributes NOTHING. There is no ungated read here.
//! - **Brain egress is consent-gated.** The reasoner is [`crate::reason::active_reasoner`], honoring
//!   the user's backend choice. With `BrainBackend::Cloud` the call routes through the SAME
//!   `make_provider` consent gate + RedactingProvider as the note summary; with no consent it fails
//!   closed → this returns a graceful "needs cloud consent" result, never a leak and never a panic.
//! - **NoteAside is additive + gated.** It records the aside in the `notes_asides` store ONLY when
//!   the current meeting is visible to the live unlocked set; it never blanks/clobbers sealed note
//!   content.
//!
//! ## Best-effort, never panic
//! Any tool/brain/IO error becomes a `VoiceActionResult { status: "error", .. }` with a NON-PII
//! message — the live loop spawns this off the transcription tick, so a dispatch failure can never
//! disrupt the recording or the caption.
//!
//! ## NOT verified headless
//! The real-mic wake precision and the live cloud round-trip LATENCY are the Mac step — `cargo test`
//! exercises the dispatch/gating/parse logic with a MOCK reasoner + seeded gated data only.

use std::collections::HashSet;

use serde::Serialize;

use crate::audio::wake::VoiceIntent;
use crate::error::AppError;
use crate::reason::LocalReasoner;
use crate::settings::AppConfig;
use crate::storage::Db;
use crate::tools::{execute_tool, ToolCall};

/// The outcome of dispatching one [`VoiceIntent`], emitted to the FE as a live event. Carries NO raw
/// transcript beyond the user's own dictated command — `summary` is the brain's answer (research/
/// recall) or a short status line; `citations` are the `[[Title]]` wikilinks the answer was grounded
/// on, extracted from the GATED tool output (so they only ever name VISIBLE meetings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceActionResult {
    /// Coarse intent discriminant: `research` | `recall` | `create_reminder` | `note_aside` |
    /// `slack_search` | `unknown`. Lets the FE pick the card style without re-parsing.
    pub intent_kind: String,
    /// `ok` (action completed) | `unavailable` (a deferred capability, e.g. Slack) | `unrecognized`
    /// (nothing actionable) | `needs_consent` (cloud brain refused, fail-closed) | `error`
    /// (best-effort failure — message is non-PII).
    pub status: String,
    /// The brain answer (research/recall) or a short, non-sensitive status line.
    pub summary: String,
    /// The HEARD command — exactly what the user dictated (their OWN spoken words, no other
    /// transcript), so the FE card can show "usłyszano: {command}". Empty when nothing was heard.
    /// This is the user's own dictation, not meeting content, so it carries no other party's speech.
    pub command: String,
    /// `[[Title]]` wikilink citations the answer was grounded on (VISIBLE meetings only). Empty for
    /// non-RAG intents.
    pub citations: Vec<String>,
}

impl VoiceActionResult {
    fn new(intent_kind: &str, status: &str, summary: impl Into<String>) -> Self {
        Self {
            intent_kind: intent_kind.to_string(),
            status: status.to_string(),
            summary: summary.into(),
            command: String::new(),
            citations: Vec::new(),
        }
    }

    /// Thread the HEARD command through onto a result (builder-style), so every dispatch path can
    /// surface what the user actually said without re-plumbing each constructor.
    pub fn with_command(mut self, command: &str) -> Self {
        self.command = command.to_string();
        self
    }

    /// Map a converged agentic-loop [`crate::agent::AgentOutcome`] onto the FE result DTO. The intent
    /// KIND comes from the resolved intent (recall vs research); the answer + GATED citations come
    /// straight off the loop. `command` is threaded on by the caller via [`Self::with_command`].
    pub fn from_agent(intent: &VoiceIntent, outcome: crate::agent::AgentOutcome) -> Self {
        let intent_kind = match intent {
            VoiceIntent::Recall { .. } => "recall",
            _ => "research",
        };
        Self {
            intent_kind: intent_kind.to_string(),
            status: "ok".to_string(),
            summary: outcome.answer,
            command: String::new(),
            citations: outcome.citations,
        }
    }

    /// The graceful outcome when a MANUAL voice-command capture's budget expired with NOTHING heard
    /// (the user never spoke after clicking). NOT a confusing "didn't catch an action" — a friendly
    /// Polish nudge to click + speak again. Empty `command` (nothing was heard) and no citations.
    pub fn nothing_heard() -> Self {
        VoiceActionResult::new(
            "unknown",
            "nothing_heard",
            "Nie usłyszałem polecenia — kliknij i powiedz jeszcze raz.",
        )
    }
}

/// Dispatch one parsed [`VoiceIntent`] over the GATED vault + consent-gated brain. Synchronous (the
/// live loop spawns it off-thread) and PANIC-FREE: every fallible step degrades to a graceful
/// `VoiceActionResult`.
///
/// `unlocked` is the LIVE session unlock set (so Research/Recall see exactly what the session can
/// see); `meeting_id` is the in-progress recording (for `NoteAside`).
///
/// `literal_command` is the user's OWN dictated words (the raw heard command), used ONLY for
/// Research/Recall RETRIEVAL so the vault FTS keys off the user's actual language (e.g. Polish
/// "pogoda"), NOT a brain-translated/normalized topic that the exact-term FTS would miss. The brain
/// topic is still used for SYNTHESIS + as an additional retrieval leg; the literal terms are the
/// must-have. Empty/whitespace falls back to the intent topic alone.
#[allow(clippy::too_many_arguments)] // cohesive dispatch surface: intent + gated state + the AppHandle.
pub fn handle_voice_action(
    intent: &VoiceIntent,
    reasoner: &dyn LocalReasoner,
    db: &Db,
    unlocked: &HashSet<String>,
    config: &AppConfig,
    meeting_id: &str,
    literal_command: &str,
    app: Option<&tauri::AppHandle>,
) -> VoiceActionResult {
    match intent {
        VoiceIntent::Research { topic } => {
            rag_answer("research", topic, literal_command, reasoner, db, unlocked, config, app)
        }
        VoiceIntent::Recall { entity } => {
            rag_answer("recall", entity, literal_command, reasoner, db, unlocked, config, app)
        }
        VoiceIntent::CreateReminder { text, due } => {
            let text = text.trim();
            if text.is_empty() {
                return VoiceActionResult::new(
                    "create_reminder",
                    "error",
                    "Nothing to remind about.",
                );
            }
            match crate::commands::add_reminder_blocking(text, due.as_deref()) {
                Ok(()) => VoiceActionResult::new(
                    "create_reminder",
                    "ok",
                    "Added a reminder.",
                ),
                Err(e) => VoiceActionResult::new("create_reminder", "error", non_pii_error(&e)),
            }
        }
        VoiceIntent::NoteAside { text } => {
            let text = text.trim();
            if text.is_empty() {
                return VoiceActionResult::new("note_aside", "error", "Nothing to note.");
            }
            note_aside(text, db, unlocked, meeting_id)
        }
        VoiceIntent::SlackSearch { .. } => VoiceActionResult::new(
            "slack_search",
            "unavailable",
            "Slack search isn't available yet.",
        ),
        VoiceIntent::Unknown { .. } => VoiceActionResult::new(
            "unknown",
            "unrecognized",
            "Sorry, I didn't catch an action I can run.",
        ),
    }
}

/// Map a FREE-FORM, keyword-unrecognized command to a known [`VoiceIntent`] using the BRAIN (natural
/// language), best-effort. Called ONLY when `parse_voice_intent` returned `Unknown` for a NON-EMPTY
/// command — so any phrasing/order works ("zrób research o wakacjach", "poszukaj mi info o X").
///
/// EGRESS: rides the SAME consent-gated reasoner as every other voice action — `structured` over the
/// reasoner the caller passes (a Cloud reasoner routes through `make_provider`'s consent gate +
/// RedactingProvider; no new egress class). The command is the user's OWN dictation (≤ one short
/// utterance), a strict subset of what the summary already sends.
///
/// Returns the brain-mapped intent, or — if the brain is unavailable / no-consent / returns garbage
/// (`Ok(None)` or `Err`) — `None`, so the caller falls back to the keyword result (Research over the
/// literal command is a fine default for a non-empty command). NEVER panics: every failure is a
/// graceful `None`.
pub fn interpret_with_brain(reasoner: &dyn LocalReasoner, command: &str) -> Option<VoiceIntent> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    let system = "You map a short spoken assistant command to ONE structured action. The actions \
                  are: \"research\" (look something up / investigate a topic), \"recall\" (what do we \
                  know about an entity), \"reminder\" (remind me to do something), \"note\" (jot a \
                  quick aside). Pick the single best action and extract its argument (the topic / \
                  entity / reminder text / note text) from the command. If none fits, use action \
                  \"unknown\".";
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ["research", "recall", "reminder", "note", "unknown"] },
            "argument": { "type": "string" }
        },
        "required": ["action", "argument"]
    });
    let user = format!("Command: {command}");

    let value = match reasoner.structured(system, &user, &schema) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(target: "voice", error = %e, "brain intent-interpret failed; falling back to keyword result");
            return None;
        }
    };
    let action = value.get("action").and_then(|a| a.as_str())?.trim().to_lowercase();
    // The argument the brain extracted; fall back to the whole command when it omits/empties it.
    let argument = value
        .get("argument")
        .and_then(|a| a.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(command)
        .to_string();
    match action.as_str() {
        "research" => Some(VoiceIntent::Research { topic: argument }),
        "recall" => Some(VoiceIntent::Recall { entity: argument }),
        "reminder" => Some(VoiceIntent::CreateReminder { text: argument, due: None }),
        "note" => Some(VoiceIntent::NoteAside { text: argument }),
        // "unknown" or any unexpected label ⇒ no mapping; caller uses the keyword fallback.
        _ => None,
    }
}

/// Build the set of RETRIEVAL queries for a Research/Recall, must-have-first:
///
/// 1. The user's LITERAL salient terms (`salient_query` over their raw dictated command) — the
///    cross-lingual fix: the vault FTS is EXACT-TERM, so a Polish note ("pogoda") only matches when
///    we search the user's ACTUAL Polish words, NOT a brain-translated English topic
///    ("yesterday's weather"). This is the must-have leg.
/// 2. The brain/parser `topic` (which may be normalized/translated) — kept as an ADDITIONAL leg so a
///    well-formed topic still helps, but never at the expense of the literal terms.
///
/// De-duped, first-seen order (literal terms first). Both legs feed the SAME gated
/// `execute_tool`/`search_visible` — no new read path. An empty result is dropped.
fn retrieval_queries(topic: &str, literal_command: &str) -> Vec<String> {
    let mut queries: Vec<String> = Vec::new();
    let mut push = |q: String| {
        let q = q.trim().to_string();
        if !q.is_empty() && !queries.contains(&q) {
            queries.push(q);
        }
    };
    // MUST-HAVE: the user's literal salient terms (their own language/words).
    push(crate::summarize::related_context::salient_query(None, literal_command));
    // ADDITIONAL: the brain/parser topic verbatim, and its salient terms.
    push(topic.to_string());
    push(crate::summarize::related_context::salient_query(None, topic));
    queries
}

/// Local-first RAG over the user's OWN gated vault (NOT a web search): run the gated read tools for
/// the topic/entity, feed ONLY the gated results to the brain, return a brief cited answer.
///
/// RETRIEVAL keys off the user's LITERAL terms first (see [`retrieval_queries`]) so same-language
/// recall works (Polish query ↔ Polish note); the brain `topic` is still used for the answer
/// SYNTHESIS prompt.
#[allow(clippy::too_many_arguments)] // cohesive RAG surface: topic/literal + gated state + AppHandle.
fn rag_answer(
    intent_kind: &str,
    topic: &str,
    literal_command: &str,
    reasoner: &dyn LocalReasoner,
    db: &Db,
    unlocked: &HashSet<String>,
    config: &AppConfig,
    app: Option<&tauri::AppHandle>,
) -> VoiceActionResult {
    let topic = topic.trim();
    if topic.is_empty() && literal_command.trim().is_empty() {
        return VoiceActionResult::new(intent_kind, "error", "No topic to look up.");
    }
    // The queries we RETRIEVE with — literal user terms first, brain topic added. The `topic` is
    // still what we hand the brain for SYNTHESIS below.
    let queries = retrieval_queries(topic, literal_command);
    // A coherent display query for the "couldn't find" / "found notes about" lines: prefer the brain
    // topic (human-readable), else the first literal query.
    let display_query = if !topic.is_empty() {
        topic.to_string()
    } else {
        queries.first().cloned().unwrap_or_default()
    };

    // GATE: every tool below routes through visibility_clause over the live `unlocked` set.
    // SemanticSearch is itself gated behind `config.semantic_search_enabled` inside `execute_tool`
    // (it returns a "disabled" string when off, never an ungated read). For `recall` we also pull
    // the entity dossier; for `research` we add open commitments for grounding. Each retrieval query
    // (literal terms + brain topic) gets its own gated FTS + semantic leg; the gated results are
    // UNIONED into the grounding below.
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for q in &queries {
        tool_calls.push(ToolCall::SearchMeetings { query: q.clone() });
        tool_calls.push(ToolCall::SearchSemantic { query: q.clone() });
    }
    if intent_kind == "recall" {
        // The dossier resolves an ENTITY by name — use the literal terms first (most likely the
        // user's actual entity word), then the brain topic.
        for q in &queries {
            tool_calls.push(ToolCall::GetEntityDossier { entity: q.clone() });
        }
    }

    let mut grounding = String::new();
    for call in &tool_calls {
        match execute_tool(call, db, unlocked, config) {
            Ok(text) => {
                let text = text.trim();
                // Skip a tool's "nothing"/"disabled" placeholder so it doesn't pollute grounding
                // (a non-empty placeholder would otherwise count as real grounding).
                if !text.is_empty() && !is_empty_tool_result(text) {
                    grounding.push_str(text);
                    grounding.push_str("\n\n");
                }
            }
            // A single tool error is non-fatal — drop it and keep whatever the others returned.
            Err(e) => tracing::debug!(
                target: "voice",
                error = %e,
                "voice-action tool call failed; continuing with partial grounding"
            ),
        }
    }
    // WEB LEG (research about the WORLD): for a `research` intent, if the web connector is exposed
    // (enabled + consented + keyed), ALSO run a live web search and fold its LOUD, source-labelled
    // hits into the grounding. This is the brain CHOOSING web vs vault — the vault legs above always
    // run (so "what do we know about project X" still works); the web leg adds "what's the weather /
    // who won Y" coverage when the vault has nothing to say. When the connector is not exposed the
    // call returns a graceful sentinel (no egress) that is filtered out below, so behavior is
    // unchanged from before for users without web search. Recall (entity dossier) stays vault-only.
    let mut web_lines: Vec<String> = Vec::new();
    if intent_kind == "research" {
        let web_query = if !topic.is_empty() {
            topic.to_string()
        } else {
            literal_command.trim().to_string()
        };
        if !web_query.is_empty() {
            match web_search_blocking(&web_query, config) {
                Ok(text) => {
                    let text = text.trim();
                    if !text.is_empty() && !is_empty_tool_result(text) {
                        // Capture the loud "[web · …]" lines for citations, and add to grounding.
                        for line in text.lines() {
                            if line.trim_start().starts_with("- [") {
                                web_lines.push(line.to_string());
                            }
                        }
                        if !grounding.is_empty() {
                            grounding.push_str("\n\n");
                        }
                        grounding.push_str(text);
                        grounding.push_str("\n\n");
                    }
                }
                // A web failure is non-fatal — the vault grounding still answers.
                Err(e) => tracing::debug!(
                    target: "voice",
                    error = %e,
                    "voice-action web search failed; continuing with vault grounding"
                ),
            }
        }
    }
    // CALENDAR LEG (LOCAL, on-device — NO egress, NEVER consent-gated): when the request reads like a
    // calendar/meeting question ("who's in my next meeting", "what's on my agenda"), ALSO pull the
    // user's local calendar context and fold its LOUD, source-labelled hits into the grounding. It is
    // INTENT-GATED (not fired on every research) so a plain "what's the weather" doesn't drag in
    // calendar noise. Requires an `AppHandle` to drive the bundled EventKit sidecar; when none is in
    // scope (headless tests) the leg is simply skipped. `fetch_events` degrades to empty on ANY
    // failure (denied Calendars permission, missing sidecar), so the deterministic vault floor stays
    // intact and the brain call is unaffected when calendar is empty/unavailable.
    let mut calendar_lines: Vec<String> = Vec::new();
    if let Some(app) = app {
        let cal_query = if !topic.is_empty() {
            topic.to_string()
        } else {
            literal_command.trim().to_string()
        };
        if wants_calendar(&cal_query, literal_command) {
            match calendar_search_blocking(&cal_query, app) {
                Ok(text) => {
                    let text = text.trim();
                    if !text.is_empty() && !is_empty_tool_result(text) {
                        // Capture the loud "[calendar] …" lines for citations, and add to grounding.
                        for line in text.lines() {
                            if line.trim_start().starts_with("[calendar]") {
                                calendar_lines.push(line.to_string());
                            }
                        }
                        if !grounding.is_empty() {
                            grounding.push_str("\n\n");
                        }
                        grounding.push_str(text);
                        grounding.push_str("\n\n");
                    }
                }
                // A calendar failure is non-fatal — the vault (+ web) grounding still answers.
                Err(e) => tracing::debug!(
                    target: "voice",
                    error = %e,
                    "voice-action calendar lookup failed; continuing with vault grounding"
                ),
            }
        }
    }
    let grounding = grounding.trim();

    // CITATIONS: derived from a GATED `search_visible` over the SAME live unlocked set — every hit
    // names a VISIBLE meeting (a sealed-not-unlocked meeting is filtered out by visibility_clause),
    // rendered as `[[Title]]`. Plus any `[[Title]]` wikilinks the tool grounding already carried
    // (commitments / dossier emit them). De-duped, first-seen order.
    let mut citations: Vec<String> = Vec::new();
    let mut push_cite = |c: String| {
        if !c.is_empty() && !citations.contains(&c) {
            citations.push(c);
        }
    };
    // Cite over EVERY retrieval query (literal terms + brain topic) so a hit found via the user's
    // literal Polish terms is still cited even when the brain topic missed.
    for q in &queries {
        match db.search_visible(q, 8, unlocked) {
            Ok(hits) => {
                for h in &hits {
                    if let Some(t) =
                        h.meeting.title.as_deref().map(str::trim).filter(|t| !t.is_empty())
                    {
                        push_cite(format!("[[{t}]]"));
                    }
                }
            }
            Err(e) => {
                tracing::debug!(target: "voice", error = %e, "voice-action citation search failed");
            }
        }
    }
    for c in extract_citations(grounding) {
        push_cite(c);
    }
    // WEB citations: each loud "- [web · …] Title — … (url)" line becomes a "(web) Title — url"
    // citation, so the card visibly attributes the web-sourced facts (LOUD). Kept distinct from the
    // `[[Title]]` vault wikilinks.
    for line in &web_lines {
        if let Some(c) = web_citation_from_line(line) {
            push_cite(c);
        }
    }
    // CALENDAR citations: each loud "[calendar] Title — …" line becomes a "(calendar) Title"
    // citation, so the card visibly attributes the calendar-sourced context (LOUD). Kept distinct
    // from the `[[Title]]` vault wikilinks and the "(web) …" web citations.
    for line in &calendar_lines {
        if let Some(c) = calendar_citation_from_line(line) {
            push_cite(c);
        }
    }

    // No grounding at all → don't burn a brain call; return a clean "nothing found".
    if grounding.is_empty() {
        return VoiceActionResult::new(
            intent_kind,
            "ok",
            format!("I couldn't find anything in your vault about \"{display_query}\"."),
        );
    }

    // The brain may now ground its answer on BOTH the user's vault notes AND any web results. The
    // prompt allows web facts (attributed) so a "what's the weather" question the vault can't answer
    // is still answered from the web leg, while vault-answerable questions stay vault-grounded.
    let system = "You are an in-meeting assistant. Answer the user's request CONCISELY (2-4 \
                  sentences) using ONLY the provided context: the user's own meeting notes AND any \
                  WEB results (lines beginning \"[web\"). Cite vault meetings by their [[Title]] \
                  wikilink and attribute web facts as \"(via web)\". If the context doesn't cover \
                  it, say so plainly. Do not invent facts.";
    let user = format!("Request: {display_query}\n\nNotes from the vault:\n{grounding}");

    // EGRESS SEAM: a Cloud reasoner routes through make_provider (consent gate + RedactingProvider).
    // A no-consent refusal comes back as AppError::Unavailable → graceful "needs consent", no leak.
    match reasoner.reason(system, &user) {
        Ok(answer) => {
            let answer = answer.trim();
            let summary = if answer.is_empty() {
                format!("Found notes about \"{display_query}\" in your vault.")
            } else {
                answer.to_string()
            };
            VoiceActionResult {
                intent_kind: intent_kind.to_string(),
                status: "ok".to_string(),
                summary,
                command: String::new(),
                citations,
            }
        }
        Err(AppError::Unavailable(_)) => VoiceActionResult {
            intent_kind: intent_kind.to_string(),
            status: "needs_consent".to_string(),
            // Surface the gated citations even when the brain can't run, so the card is still useful.
            summary: "The cloud brain needs your one-time consent to answer (Settings ▸ Privacy). \
                      I found related meetings in your vault."
                .to_string(),
            command: String::new(),
            citations,
        },
        Err(e) => VoiceActionResult {
            intent_kind: intent_kind.to_string(),
            status: "error".to_string(),
            summary: non_pii_error(&e),
            command: String::new(),
            citations,
        },
    }
}

/// Record a spoken aside against the CURRENT meeting — additive + gated. The aside lands only when
/// the meeting is visible to the live unlocked set (the in-progress recording is foldered/sealed
/// later, so it is trivially visible now); a sealed-not-unlocked meeting is refused, never written.
fn note_aside(
    text: &str,
    db: &Db,
    unlocked: &HashSet<String>,
    meeting_id: &str,
) -> VoiceActionResult {
    // GATE: never write an aside against a meeting the session can't see.
    match db.meeting_is_visible(meeting_id, unlocked) {
        Ok(true) => {}
        Ok(false) => {
            return VoiceActionResult::new(
                "note_aside",
                "error",
                "This meeting is locked — unlock it to add a note.",
            );
        }
        Err(e) => return VoiceActionResult::new("note_aside", "error", non_pii_error(&e)),
    }
    let created_at = chrono::Utc::now().to_rfc3339();
    match db.insert_note_aside(meeting_id, text, &created_at) {
        Ok(_) => VoiceActionResult::new("note_aside", "ok", "Noted."),
        Err(e) => VoiceActionResult::new("note_aside", "error", non_pii_error(&e)),
    }
}

/// Run the async web-search connector to completion from this SYNCHRONOUS dispatch (the live loop
/// spawns `handle_voice_action` off-thread). Mirrors `reason::block_on_complete`: a dedicated scoped
/// OS thread with its own current-thread runtime, so we never "start a runtime within a runtime" and
/// the future never crosses a thread boundary (only the `Result<String>` does). The egress/consent/
/// redaction discipline all lives inside `execute_web_search` → the connector registry.
fn web_search_blocking(query: &str, config: &AppConfig) -> crate::error::Result<String> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| AppError::Summarize(format!("web search runtime build: {e}")))?;
                rt.block_on(crate::tools::execute_web_search(query, config))
            })
            .join()
            .map_err(|_| AppError::Summarize("web search worker thread panicked".into()))?
    })
}

/// INTENT GATE for the calendar leg: does this request read like a calendar/meeting question? We
/// fire the local calendar lookup ONLY for these — so a plain "what's the weather" research never
/// drags in calendar noise, while "who's in my next meeting" / "what's on my agenda" / "kto jest na
/// spotkaniu" do. Case-insensitive substring match over a small bilingual (EN/PL) keyword set,
/// checked against BOTH the brain topic and the user's literal command. Deterministic, no regex.
fn wants_calendar(topic: &str, literal_command: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        // English
        "calendar", "meeting", "meetings", "agenda", "next meeting", "who's in", "who is in",
        "attendee", "attendees", "schedule", "scheduled", "appointment", "invite",
        // Polish
        "kalendarz", "spotkanie", "spotkania", "spotkaniu", "agenda", "kto jest", "uczestnik",
        "uczestnicy", "harmonogram", "termin",
    ];
    let hay = format!("{} {}", topic.to_lowercase(), literal_command.to_lowercase());
    KEYWORDS.iter().any(|k| hay.contains(k))
}

/// Run the async LOCAL-CALENDAR connector to completion from this SYNCHRONOUS dispatch (the live loop
/// spawns `handle_voice_action` off-thread). Mirrors [`web_search_blocking`]: a dedicated scoped OS
/// thread with its own current-thread runtime, so we never "start a runtime within a runtime" and the
/// future never crosses a thread boundary (only the `Result<String>` does). NO egress — the calendar
/// read is on-device — and `fetch_events` degrades to empty on every failure inside
/// `execute_calendar_search`.
fn calendar_search_blocking(
    query: &str,
    app: &tauri::AppHandle,
) -> crate::error::Result<String> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| AppError::Summarize(format!("calendar runtime build: {e}")))?;
                rt.block_on(crate::tools::execute_calendar_search(query, app))
            })
            .join()
            .map_err(|_| AppError::Summarize("calendar worker thread panicked".into()))?
    })
}

/// Turn a loud calendar grounding line `[calendar] Title — <context block>` into a compact citation
/// `(calendar) Title`. Returns `None` for a non-calendar line. Deterministic, no regex.
fn calendar_citation_from_line(line: &str) -> Option<String> {
    let line = line.trim();
    let rest = line.strip_prefix("[calendar]")?.trim();
    // Title is everything up to the first " — " (the context-block separator).
    let title = rest.split(" — ").next().unwrap_or(rest).trim();
    if title.is_empty() {
        return None;
    }
    Some(format!("(calendar) {title}"))
}

/// Turn a loud web grounding line `- [web · Brave] Title — snippet (https://…)` into a compact
/// citation `(web) Title — https://…`. Returns `None` for a non-web line. Deterministic, no regex.
fn web_citation_from_line(line: &str) -> Option<String> {
    let line = line.trim();
    let after_label = line.strip_prefix("- [")?;
    let close = after_label.find(']')?;
    let rest = after_label[close + 1..].trim();
    // Title is everything up to the first " — " (snippet sep) or " (" (url); whichever comes first.
    let title_end = [rest.find(" — "), rest.find(" (")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(rest.len());
    let title = rest[..title_end].trim();
    if title.is_empty() {
        return None;
    }
    // Extract a trailing "(url)" if present.
    let url = rest
        .rfind('(')
        .and_then(|i| rest[i + 1..].strip_suffix(')').map(str::trim))
        .filter(|u| u.starts_with("http"));
    Some(match url {
        Some(u) => format!("(web) {title} — {u}"),
        None => format!("(web) {title}"),
    })
}

/// Whether a tool result is a "nothing found" / "disabled" placeholder rather than real content,
/// so it can be excluded from the brain grounding (an included placeholder would falsely count as
/// grounding and trigger a brain call on an empty vault). Matches the deterministic prefixes
/// `execute_tool` emits (`No meetings match`, `No data`, `No open commitments`, `No visible
/// entity`, and the `Semantic search is disabled` notice).
fn is_empty_tool_result(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("No meetings match")
        || t.starts_with("No data")
        || t.starts_with("No open commitments")
        || t.starts_with("No visible entity")
        || t.starts_with("Semantic search is disabled")
        || t.starts_with("No web results")
        || t.starts_with("Web search is not available")
        || t.starts_with("No calendar events")
}

/// Pull distinct `[[Title]]` wikilinks out of the gated tool grounding text, in first-seen order.
/// Deterministic, no regex. Only the tool output is scanned, so a citation can only ever name a
/// VISIBLE meeting (the tools never emit a sealed-not-unlocked title). `pub(crate)` so the agentic
/// loop (`crate::agent`) reuses the SAME gated-citation extraction instead of duplicating it.
pub(crate) fn extract_citations(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            if let Some(rel_end) = text[i + 2..].find("]]") {
                let inner = text[i + 2..i + 2 + rel_end].trim();
                if !inner.is_empty() {
                    let cite = format!("[[{inner}]]");
                    if !out.contains(&cite) {
                        out.push(cite);
                    }
                }
                i = i + 2 + rel_end + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// A short, NON-PII summary string for an error result. Uses the AppError VARIANT name only — never
/// the inner message (which could carry a path/title) — per the PII-in-logs rule applied to the FE
/// event too. The exact technical detail still goes to the debug log via the caller.
fn non_pii_error(e: &AppError) -> String {
    let kind = match e {
        AppError::Unavailable(_) => "the action is unavailable right now",
        AppError::Locked(_) => "the content is locked",
        AppError::InvalidArg(_) => "the request was incomplete",
        AppError::Storage(_) | AppError::Migration(_) => "a storage error occurred",
        AppError::Summarize(_) => "the brain couldn't complete the request",
        _ => "the action couldn't be completed",
    };
    format!("Sorry — {kind}.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reason::LocalReasoner;
    use crate::storage::{Folder, Meeting, MeetingStatus, NoteRecord};
    use serde_json::Value;

    const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn tmp_db() -> Db {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-voiceaction-{}-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = std::fs::remove_file(&p);
        Db::open_with_key(&p, KEY).unwrap()
    }

    /// A reasoner that echoes a fixed answer and records the grounding it was handed, so we can
    /// assert the brain saw ONLY gated content. No network, no model.
    struct MockReasoner {
        answer: String,
        last_user: std::sync::Mutex<Option<String>>,
    }
    impl MockReasoner {
        fn new(answer: &str) -> Self {
            Self {
                answer: answer.to_string(),
                last_user: std::sync::Mutex::new(None),
            }
        }
    }
    impl LocalReasoner for MockReasoner {
        fn id(&self) -> &str {
            "mock"
        }
        fn reason(&self, _system: &str, user: &str) -> crate::error::Result<String> {
            *self.last_user.lock().unwrap() = Some(user.to_string());
            Ok(self.answer.clone())
        }
        fn structured(
            &self,
            _system: &str,
            _user: &str,
            _schema: &Value,
        ) -> crate::error::Result<Value> {
            Ok(Value::Null)
        }
    }

    /// A reasoner that always errors — proves the dispatch degrades to a graceful "error" result
    /// (no panic), and an `Unavailable` error maps to the fail-closed "needs_consent" status.
    struct ErrReasoner(AppError);
    impl LocalReasoner for ErrReasoner {
        fn id(&self) -> &str {
            "err"
        }
        fn reason(&self, _system: &str, _user: &str) -> crate::error::Result<String> {
            Err(clone_err(&self.0))
        }
        fn structured(
            &self,
            _system: &str,
            _user: &str,
            _schema: &Value,
        ) -> crate::error::Result<Value> {
            Err(clone_err(&self.0))
        }
    }
    fn clone_err(e: &AppError) -> AppError {
        match e {
            AppError::Unavailable(m) => AppError::Unavailable(m.clone()),
            AppError::Summarize(m) => AppError::Summarize(m.clone()),
            _ => AppError::Other(anyhow::anyhow!("err")),
        }
    }

    /// Seed a visible meeting (no folder) with a note, plus a SEALED meeting in a locked folder
    /// whose note plaintext is blanked (sealed-not-unlocked). Returns the db.
    fn seed_visible_and_sealed(db: &Db) {
        // Visible meeting: open folder (None), note mentions "Atlas".
        db.insert_meeting(&Meeting {
            id: "open1".into(),
            started_at: "2026-06-01T09:00:00Z".into(),
            ended_at: None,
            title: Some("Atlas Kickoff".into()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: "open1".into(),
            provider_id: "claude_code".into(),
            markdown: "We discussed the Atlas migration plan and pricing.".into(),
            created_at: "2026-06-01T09:05:00Z".into(),
            exported_path: None,
        })
        .unwrap();

        // Sealed meeting: locked folder, note plaintext BLANKED (the sealed-at-rest shape).
        db.insert_folder(&Folder {
            id: "fsec".into(),
            name: "Secret".into(),
            path: "Secret".into(),
            parent_id: None,
            locked: true,
            created_at: "2026-06-01T00:00:00Z".into(),
        })
        .unwrap();
        db.insert_meeting(&Meeting {
            id: "sealed1".into(),
            started_at: "2026-06-02T09:00:00Z".into(),
            ended_at: None,
            title: Some("Atlas Secret Terms".into()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        // Blanked plaintext + a non-null content_blob marks it sealed-not-unlocked for the gate.
        db.upsert_note(&NoteRecord {
            meeting_id: "sealed1".into(),
            provider_id: "claude_code".into(),
            markdown: String::new(),
            created_at: "2026-06-02T09:05:00Z".into(),
            exported_path: None,
        })
        .unwrap();
        db.set_note_folder("sealed1", Some("fsec")).unwrap();
    }

    fn empty_unlocked() -> HashSet<String> {
        HashSet::new()
    }

    /// LOAD-BEARING (verification 2026-06-30): the AGENTIC LOOP must NEVER surface SEALED content.
    /// Proven two ways. (1) The gated executor itself hides the sealed meeting from BOTH a direct
    /// `get_meeting` and a `search_meetings` — RED-able: were the executor to bypass the live
    /// `unlocked` set, the sealed id would appear in the search results. (2) The full loop, driven by
    /// a brain scripted to exfiltrate the sealed meeting (fetch its id, then search its title) and
    /// then answer, surfaces NOTHING sealed in its citations. The model can REQUEST a read, but the
    /// host-held gate filters it.
    #[test]
    fn agentic_loop_and_executor_never_surface_sealed_content() {
        use crate::agent::run_agentic_loop;
        use crate::agent::ToolExecutor;
        use crate::tools::GatedToolExecutor;

        let db = tmp_db();
        seed_visible_and_sealed(&db);

        // SEED SELF-CHECK: prove the seed produced the EXPECTED gate state BEFORE exercising the loop,
        // so this test is unambiguous about what it proves. If this ever trips it is a SEED/harness
        // fault (a flaked fixture), NOT a gate bypass — it fails loudly here, never disguised as a leak
        // through the executor below. (The production gate `meeting_is_visible` is exercised by ~100
        // other tests; this guards the FIXTURE so the executor assertions below are meaningful.)
        let nothing = HashSet::new();
        assert!(
            db.meeting_is_visible("open1", &nothing).unwrap(),
            "seed fixture: the visible meeting must be visible"
        );
        assert!(
            !db.meeting_is_visible("sealed1", &nothing).unwrap(),
            "seed fixture: the sealed meeting must be gated (sealed-not-unlocked)"
        );

        let cfg = AppConfig::default();
        // The executor holds the live set behind its Mutex (re-read per call). Nothing unlocked here
        // → the sealed folder is invisible to every gated read.
        let unlocked = std::sync::Mutex::new(empty_unlocked());
        let exec = GatedToolExecutor {
            db: &db,
            unlocked: &unlocked,
            config: &cfg,
            meeting_id: "",
            app: None,
            allow_writes: false,
        };

        // (1) Direct gate proof on the executor (RED-able).
        let got = exec
            .run("get_meeting", &serde_json::json!({ "meetingId": "sealed1" }))
            .unwrap();
        assert!(got.starts_with("No data"), "sealed meeting fetch must be gated: {got}");
        let searched = exec
            .run("search_meetings", &serde_json::json!({ "query": "Atlas" }))
            .unwrap();
        assert!(!searched.contains("sealed1"), "sealed meeting must NOT appear in gated search: {searched}");
        assert!(searched.contains("open1"), "the VISIBLE meeting is still found");

        // (2) The full loop routes every read through that gate.
        struct Exfil {
            steps: std::sync::Mutex<std::collections::VecDeque<Value>>,
        }
        impl LocalReasoner for Exfil {
            fn id(&self) -> &str {
                "exfil"
            }
            fn reason(&self, _s: &str, _u: &str) -> crate::error::Result<String> {
                Ok("floored".into())
            }
            fn structured(&self, _s: &str, _u: &str, _schema: &Value) -> crate::error::Result<Value> {
                Ok(self
                    .steps
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(serde_json::json!({ "answer": "done" })))
            }
        }
        let brain = Exfil {
            steps: std::sync::Mutex::new(
                vec![
                    serde_json::json!({ "tool": "get_meeting", "args": { "meetingId": "sealed1" } }),
                    serde_json::json!({ "tool": "search_meetings", "args": { "query": "Atlas Secret Terms" } }),
                    serde_json::json!({ "answer": "Here is what I found." }),
                ]
                .into(),
            ),
        };
        let outcome = run_agentic_loop(&brain, "sys", "what are the secret terms?", &exec, 5, None)
            .unwrap()
            .expect("the brain answered");
        assert!(
            !outcome.citations.iter().any(|c| c.contains("Atlas Secret Terms")),
            "the loop must never cite the sealed meeting: {:?}",
            outcome.citations
        );
        assert!(outcome.steps.iter().all(|s| s.ok), "gated tool calls ran without panic");
    }

    #[test]
    fn research_returns_cited_summary_built_only_from_visible_content() {
        let db = tmp_db();
        seed_visible_and_sealed(&db);
        let reasoner = MockReasoner::new("Atlas is migrating; see [[Atlas Kickoff]].");
        let cfg = AppConfig::default();

        let res = handle_voice_action(
            &VoiceIntent::Research { topic: "Atlas".into() },
            &reasoner,
            &db,
            &empty_unlocked(),
            &cfg,
            "live-mtg",
            "",
            None,
        );

        assert_eq!(res.intent_kind, "research");
        assert_eq!(res.status, "ok");
        assert!(res.summary.contains("Atlas"));
        // Citation is the VISIBLE meeting, extracted from gated tool output.
        assert!(res.citations.contains(&"[[Atlas Kickoff]]".to_string()));

        // THE GATE: the sealed meeting's title must NEVER reach the brain grounding nor the
        // citations, even though it also matches "Atlas".
        let grounding = reasoner.last_user.lock().unwrap().clone().unwrap();
        assert!(
            grounding.contains("Atlas Kickoff"),
            "visible meeting must be in grounding"
        );
        assert!(
            !grounding.contains("Atlas Secret Terms"),
            "SEALED meeting must NOT leak into the brain grounding"
        );
        assert!(
            !res.citations.iter().any(|c| c.contains("Secret")),
            "SEALED meeting must NOT be cited"
        );
    }

    #[test]
    fn recall_returns_ok_and_excludes_sealed() {
        let db = tmp_db();
        seed_visible_and_sealed(&db);
        let reasoner = MockReasoner::new("Recall: [[Atlas Kickoff]] covered the plan.");
        let res = handle_voice_action(
            &VoiceIntent::Recall { entity: "Atlas".into() },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live-mtg",
            "",
            None,
        );
        assert_eq!(res.intent_kind, "recall");
        assert_eq!(res.status, "ok");
        let grounding = reasoner.last_user.lock().unwrap().clone().unwrap();
        assert!(!grounding.contains("Atlas Secret Terms"), "sealed excluded from recall too");
    }

    #[test]
    fn research_with_no_vault_match_is_ok_without_brain_call() {
        let db = tmp_db();
        seed_visible_and_sealed(&db);
        let reasoner = MockReasoner::new("SHOULD-NOT-BE-CALLED");
        let res = handle_voice_action(
            &VoiceIntent::Research { topic: "nonexistent-topic-zzz".into() },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live-mtg",
            "",
            None,
        );
        assert_eq!(res.status, "ok");
        assert!(res.summary.contains("couldn't find"));
        // No grounding ⇒ no brain call (so the mock never recorded a user prompt).
        assert!(reasoner.last_user.lock().unwrap().is_none(), "brain must not be called with empty grounding");
    }

    #[test]
    fn note_aside_records_against_visible_meeting() {
        let db = tmp_db();
        // A live, un-foldered (visible) meeting.
        db.insert_meeting(&Meeting {
            id: "live1".into(),
            started_at: "2026-06-03T09:00:00Z".into(),
            ended_at: None,
            title: Some("Live".into()),
            duration_s: 0,
            audio_path: None,
            status: MeetingStatus::Recording,
            folder_id: None,
        })
        .unwrap();
        let reasoner = MockReasoner::new("");
        let res = handle_voice_action(
            &VoiceIntent::NoteAside { text: "deadline is friday".into() },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live1",
            "",
            None,
        );
        assert_eq!(res.intent_kind, "note_aside");
        assert_eq!(res.status, "ok");
        let asides = db.list_note_asides("live1").unwrap();
        assert_eq!(asides.len(), 1);
        assert_eq!(asides[0].0, "deadline is friday");
    }

    #[test]
    fn note_aside_refused_for_sealed_meeting() {
        let db = tmp_db();
        seed_visible_and_sealed(&db);
        let reasoner = MockReasoner::new("");
        // sealed1 is in a locked folder, not in the unlocked set ⇒ not visible ⇒ refuse the write.
        let res = handle_voice_action(
            &VoiceIntent::NoteAside { text: "secret aside".into() },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "sealed1",
            "",
            None,
        );
        assert_eq!(res.status, "error");
        assert!(db.list_note_asides("sealed1").unwrap().is_empty(), "no aside written to a sealed meeting");
    }

    #[test]
    fn slack_search_is_unavailable() {
        let db = tmp_db();
        let reasoner = MockReasoner::new("");
        let res = handle_voice_action(
            &VoiceIntent::SlackSearch { query: "raport".into() },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live1",
            "",
            None,
        );
        assert_eq!(res.intent_kind, "slack_search");
        assert_eq!(res.status, "unavailable");
    }

    #[test]
    fn unknown_is_unrecognized_and_echoes_nothing_sensitive() {
        let db = tmp_db();
        let reasoner = MockReasoner::new("");
        let res = handle_voice_action(
            &VoiceIntent::Unknown { raw: "qwer asdf secret-thing".into() },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live1",
            "",
            None,
        );
        assert_eq!(res.status, "unrecognized");
        assert!(!res.summary.contains("secret-thing"), "raw command must not be echoed back");
    }

    #[test]
    fn reasoner_error_degrades_gracefully_without_panic() {
        let db = tmp_db();
        seed_visible_and_sealed(&db);
        // A generic Summarize error → status "error", non-PII message, citations still surfaced.
        let reasoner = ErrReasoner(AppError::Summarize("boom internal detail".into()));
        let res = handle_voice_action(
            &VoiceIntent::Research { topic: "Atlas".into() },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live1",
            "",
            None,
        );
        assert_eq!(res.status, "error");
        assert!(!res.summary.contains("boom internal detail"), "internal error detail must not leak");
        assert!(res.citations.contains(&"[[Atlas Kickoff]]".to_string()));
    }

    #[test]
    fn cloud_no_consent_maps_to_needs_consent_failclosed() {
        let db = tmp_db();
        seed_visible_and_sealed(&db);
        // The make_provider consent gate returns AppError::Unavailable when consent is OFF.
        let reasoner = ErrReasoner(AppError::Unavailable("cloud egress not consented".into()));
        let res = handle_voice_action(
            &VoiceIntent::Research { topic: "Atlas".into() },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live1",
            "",
            None,
        );
        assert_eq!(res.status, "needs_consent");
        // Gated citations are still useful even when the brain can't run.
        assert!(res.citations.contains(&"[[Atlas Kickoff]]".to_string()));
    }

    /// A reasoner whose `structured` returns a fixed brain mapping, to prove `interpret_with_brain`
    /// maps free-form text to the right intent + argument.
    struct MapReasoner(Value);
    impl LocalReasoner for MapReasoner {
        fn id(&self) -> &str {
            "map"
        }
        fn reason(&self, _s: &str, _u: &str) -> crate::error::Result<String> {
            Ok(String::new())
        }
        fn structured(&self, _s: &str, _u: &str, _schema: &Value) -> crate::error::Result<Value> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn interpret_with_brain_maps_each_action() {
        let cases = [
            (serde_json::json!({"action":"research","argument":"wakacjach"}),
             VoiceIntent::Research { topic: "wakacjach".into() }),
            (serde_json::json!({"action":"recall","argument":"atlas"}),
             VoiceIntent::Recall { entity: "atlas".into() }),
            (serde_json::json!({"action":"reminder","argument":"call bob"}),
             VoiceIntent::CreateReminder { text: "call bob".into(), due: None }),
            (serde_json::json!({"action":"note","argument":"deadline friday"}),
             VoiceIntent::NoteAside { text: "deadline friday".into() }),
        ];
        for (map, want) in cases {
            let got = interpret_with_brain(&MapReasoner(map), "some free-form command").unwrap();
            assert_eq!(got, want);
        }
    }

    #[test]
    fn interpret_with_brain_empty_argument_falls_back_to_whole_command() {
        // When the brain omits/empties the argument, the whole command is used as the argument.
        let r = MapReasoner(serde_json::json!({ "action": "research", "argument": "" }));
        let got = interpret_with_brain(&r, "look into the pricing model").unwrap();
        assert_eq!(got, VoiceIntent::Research { topic: "look into the pricing model".into() });
    }

    #[test]
    fn interpret_with_brain_unknown_or_error_returns_none() {
        // action="unknown" ⇒ None (caller uses the keyword fallback).
        let unknown = MapReasoner(serde_json::json!({ "action": "unknown", "argument": "x" }));
        assert!(interpret_with_brain(&unknown, "gibberish").is_none());
        // An erroring/unavailable brain ⇒ None, never a panic.
        let err = ErrReasoner(AppError::Unavailable("no consent".into()));
        assert!(interpret_with_brain(&err, "gibberish").is_none());
        // An empty command never calls the brain ⇒ None.
        assert!(interpret_with_brain(&unknown, "   ").is_none());
    }

    #[test]
    fn with_command_surfaces_the_heard_command_on_a_result() {
        let r = VoiceActionResult::new("research", "ok", "answer").with_command("zrób research o X");
        assert_eq!(r.command, "zrób research o X");
        // nothing_heard carries an empty command (nothing was heard).
        assert!(VoiceActionResult::nothing_heard().command.is_empty());
    }

    #[test]
    fn extract_citations_dedups_in_first_seen_order() {
        let text = "- A ([id:1]) — x [[One]]\n- B — y [[Two]]\nagain [[One]]";
        let cites = extract_citations(text);
        assert_eq!(cites, vec!["[[One]]".to_string(), "[[Two]]".to_string()]);
    }

    // ── CALENDAR connector leg: intent gate + loud citation extraction (headless, NO EventKit) ─────

    #[test]
    fn wants_calendar_fires_only_on_calendar_meeting_intent() {
        // EN + PL calendar/meeting phrasings fire the leg.
        assert!(wants_calendar("who's in my next meeting", "who's in my next meeting"));
        assert!(wants_calendar("what's on my agenda", ""));
        assert!(wants_calendar("", "kto jest na spotkaniu"));
        assert!(wants_calendar("kalendarz na dziś", ""));
        // A plain research question does NOT fire it (no calendar noise on "weather").
        assert!(!wants_calendar("what's the weather in Kraków", "jaka jest pogoda"));
        assert!(!wants_calendar("research the Atlas pricing model", ""));
    }

    #[test]
    fn calendar_citation_from_line_extracts_title_and_skips_non_calendar() {
        // A loud "[calendar] Title — <context>" line → "(calendar) Title".
        let c = calendar_citation_from_line(
            "[calendar] Sprint Planning — Meeting: Sprint Planning\nAttendees: Alice",
        );
        assert_eq!(c, Some("(calendar) Sprint Planning".to_string()));
        // A title with no context separator still yields a citation.
        assert_eq!(
            calendar_citation_from_line("[calendar] 1:1"),
            Some("(calendar) 1:1".to_string())
        );
        // A non-calendar line is ignored.
        assert!(calendar_citation_from_line("- [web · Brave] Weather — Sunny").is_none());
    }

    #[test]
    fn is_empty_tool_result_matches_calendar_sentinel() {
        // The calendar "nothing found" sentinels are filtered out of the grounding.
        assert!(is_empty_tool_result("No calendar events match \"standup\"."));
        assert!(is_empty_tool_result("No calendar events in the window."));
        // A real calendar block is NOT a sentinel.
        assert!(!is_empty_tool_result("[calendar] Sprint Planning — Meeting: Sprint Planning"));
    }

    // ── FIX 1: cross-lingual RETRIEVAL uses the user's LITERAL terms, not the brain topic ─────────

    #[test]
    fn retrieval_queries_uses_literal_terms_first() {
        // The user's literal Polish words drive retrieval; the salient term "pogoda" survives (the
        // stopword "jaka"/"byla" are dropped). The brain-translated English topic is an ADDITIONAL
        // leg, never a replacement.
        let qs = retrieval_queries("yesterday's weather", "jaka była pogoda");
        let joined = qs.join(" | ");
        assert!(
            qs.iter().any(|q| q.contains("pogoda")),
            "literal Polish term 'pogoda' MUST be a retrieval query; got: {joined}"
        );
        // The literal-terms leg comes FIRST (must-have).
        assert!(
            qs[0].contains("pogoda"),
            "the literal salient terms must be the FIRST retrieval query; got: {joined}"
        );
        // The brain topic is still present as an additional leg.
        assert!(
            qs.iter().any(|q| q.contains("weather")),
            "the brain topic should still be an additional retrieval leg; got: {joined}"
        );
    }

    /// THE BUG, end-to-end: a Polish note ("pogoda") seeded into the vault, a Research dispatched
    /// with the brain's ENGLISH-translated topic ("yesterday's weather") but the user's LITERAL
    /// Polish command "jaka była pogoda". With the literal-terms retrieval the note is FOUND; with
    /// ONLY the translated topic (literal command empty) it MISSES — proving the fix is load-bearing.
    #[test]
    fn research_uses_literal_polish_terms_not_translation() {
        let db = tmp_db();
        // A Polish weather note — exact-term FTS only matches the Polish word "pogoda".
        db.insert_meeting(&Meeting {
            id: "pl1".into(),
            started_at: "2026-06-27T09:00:00Z".into(),
            ended_at: None,
            title: Some("Notatka o pogodzie".into()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: "pl1".into(),
            provider_id: "claude_code".into(),
            markdown: "Dzisiaj pogoda była słoneczna i ciepła, bez deszczu.".into(),
            created_at: "2026-06-27T09:05:00Z".into(),
            exported_path: None,
        })
        .unwrap();

        let reasoner = MockReasoner::new("Pogoda była słoneczna; zobacz [[Notatka o pogodzie]].");
        let cfg = AppConfig::default();

        // WITH the literal Polish command → the Polish note is FOUND, brain is called, cited.
        let res = handle_voice_action(
            &VoiceIntent::Research { topic: "yesterday's weather".into() },
            &reasoner,
            &db,
            &empty_unlocked(),
            &cfg,
            "live-mtg",
            "jaka była pogoda",
            None,
        );
        assert_eq!(res.status, "ok");
        assert!(
            res.citations.contains(&"[[Notatka o pogodzie]]".to_string()),
            "the Polish note must be cited when retrieval uses the literal Polish terms"
        );
        let grounding = reasoner
            .last_user
            .lock()
            .unwrap()
            .clone()
            .expect("brain MUST be called — the Polish note was found via the literal terms");
        assert!(
            grounding.contains("pogoda"),
            "the found Polish note must be in the brain grounding"
        );

        // CONTROL: the SAME English-translated topic with NO literal command misses the Polish note
        // entirely (the exact-term FTS can't match "weather" against "pogoda") → no grounding, no
        // brain call. This is the bug the user hit.
        let reasoner2 = MockReasoner::new("SHOULD-NOT-BE-CALLED");
        let res2 = handle_voice_action(
            &VoiceIntent::Research { topic: "yesterday's weather".into() },
            &reasoner2,
            &db,
            &empty_unlocked(),
            &cfg,
            "live-mtg",
            "",
            None,
        );
        assert_eq!(res2.status, "ok");
        assert!(
            res2.summary.contains("couldn't find"),
            "the English-only topic must MISS the Polish note (the demonstrated bug)"
        );
        assert!(
            reasoner2.last_user.lock().unwrap().is_none(),
            "no grounding ⇒ no brain call on the translated-only path"
        );
    }
}
