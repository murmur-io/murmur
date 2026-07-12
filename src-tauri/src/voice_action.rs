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
use crate::reason::{GenOptions, LocalReasoner};
use crate::settings::AppConfig;
use crate::storage::Db;
use crate::tools::{execute_tool, ToolCall};

/// Which BRAIN CASCADE tier answered this turn (Phase 5) — set DETERMINISTICALLY by the ladder from
/// the tier that CONVERGED, never string-sniffed from the answer. Surfaced to the FE (as
/// `answeredFrom`) so it can render a "answered from: this meeting / your vault / connectors" chip.
/// `None` ⇒ this result did not run through the cascade (the deterministic floor, an error, or a
/// non-cascade surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnsweredFrom {
    /// Tier 1 — answered from the current meeting in isolation.
    CurrentMeeting,
    /// Tier 2 — answered from the owned vault.
    Vault,
    /// Tier 3 — answered with the help of connectors/web.
    Connectors,
}

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
    /// The model's NOTE DRAFT for this turn, set ONLY when the agent decided the user asked for a note
    /// (it called the `propose_note` tool). `None` ⇒ the reply is a plain ANSWER (a conversation);
    /// `Some(content)` ⇒ a NOTE PROPOSAL the FE shows "Add to notes" on. No DB write happens server-side
    /// — the FE commits the accepted draft via `save_manual_notes`. Serializes to `proposedNote`.
    pub proposed_note: Option<String>,
    /// Opaque id of the conversation THREAD this result belongs to (the FE-supplied @brain thread
    /// id, or the backend-generated UUID for a voice/wake turn), so simultaneous threads resolve
    /// the right pending bubble. Serializes to `threadId`; the dispatch threads it on via
    /// [`Self::with_thread_id`]. NOT PII (an opaque UUID).
    pub thread_id: Option<String>,
    /// Which BRAIN CASCADE tier answered (Phase 5), set DETERMINISTICALLY by the ladder from the tier
    /// that converged — never string-sniffed. Serializes to `answeredFrom`. `None` when this result
    /// did not run through the cascade (the deterministic floor / error / a non-cascade surface).
    pub answered_from: Option<AnsweredFrom>,
}

impl VoiceActionResult {
    fn new(intent_kind: &str, status: &str, summary: impl Into<String>) -> Self {
        Self {
            intent_kind: intent_kind.to_string(),
            status: status.to_string(),
            summary: summary.into(),
            command: String::new(),
            citations: Vec::new(),
            proposed_note: None,
            thread_id: None,
            answered_from: None,
        }
    }

    /// Thread the HEARD command through onto a result (builder-style), so every dispatch path can
    /// surface what the user actually said without re-plumbing each constructor.
    pub fn with_command(mut self, command: &str) -> Self {
        self.command = command.to_string();
        self
    }

    /// Thread the turn's THREAD id onto a result (builder-style) — the FE uses it to attribute the
    /// answer (and its trace chips) to the right open thread.
    pub fn with_thread_id(mut self, thread_id: &str) -> Self {
        self.thread_id = Some(thread_id.to_string());
        self
    }

    /// Thread a model NOTE PROPOSAL onto a result (builder-style). `Some(content)` marks the reply as
    /// a note draft (the FE offers "Add to notes"); `None` is a no-op (plain answer). The caller reads
    /// the proposal off the executor after the agentic loop and threads it here.
    pub fn with_proposed_note(mut self, proposed: Option<String>) -> Self {
        if let Some(content) = proposed {
            let content = content.trim();
            if !content.is_empty() {
                self.proposed_note = Some(content.to_string());
            }
        }
        self
    }

    /// Map a converged agentic-loop [`crate::agent::AgentOutcome`] onto the FE result DTO. The intent
    /// KIND comes from the resolved intent (recall vs research); the answer + GATED citations come
    /// straight off the loop. `command` is threaded on by the caller via [`Self::with_command`].
    ///
    /// Phase 5: `answered_from` is set DETERMINISTICALLY by the ladder to the tier that converged
    /// (never string-sniffed). `extra_citations` lets the ladder PREPEND tier-specific attributions
    /// that the loop's `gathered`-scraped citations miss: Tier 1's own `[[Title]]` (prompt-injected
    /// current-meeting content produces no wikilink in `gathered`), and Tier 3 connector loud-lines
    /// (like `rag_answer`). They are merged in FIRST-SEEN order, de-duplicated against the loop's own.
    pub fn from_agent(
        intent: &VoiceIntent,
        outcome: crate::agent::AgentOutcome,
        answered_from: AnsweredFrom,
        extra_citations: Vec<String>,
    ) -> Self {
        let intent_kind = match intent {
            VoiceIntent::Recall { .. } => "recall",
            _ => "research",
        };
        // Merge tier-specific citations FIRST, then the loop's gated ones, de-duplicated in
        // first-seen order (mirrors `extract_citations`'s de-dup discipline).
        let mut citations: Vec<String> = Vec::new();
        for c in extra_citations.into_iter().chain(outcome.citations) {
            let c = c.trim().to_string();
            if !c.is_empty() && !citations.contains(&c) {
                citations.push(c);
            }
        }
        Self {
            intent_kind: intent_kind.to_string(),
            status: "ok".to_string(),
            summary: outcome.answer,
            command: String::new(),
            citations,
            // The caller (`run_informational`) threads any `propose_note` draft on via
            // `with_proposed_note` after reading the executor; the loop outcome itself has none.
            proposed_note: None,
            thread_id: None,
            answered_from: Some(answered_from),
        }
    }

    /// The graceful outcome when a MANUAL voice-command capture's budget expired with NOTHING heard
    /// (the user never spoke after clicking). NOT a confusing "didn't catch an action" — a friendly
    /// Polish nudge to click + speak again. Empty `command` (nothing was heard) and no citations.
    pub fn nothing_heard() -> Self {
        VoiceActionResult::new(
            "unknown",
            "nothing_heard",
            "I didn't hear a command — click and say it again.",
        )
    }
}

/// The honest notice surfaced when a RAG floor request found VISIBLE vault matches but NO AI model
/// is available to SYNTHESIZE an answer — i.e. the active reasoner is the deterministic
/// [`crate::reason::StubReasoner`] (`id() == "stub"`: brain backend Off, or Local with no GGUF
/// downloaded yet). The stub's `reason()` returns a DIAGNOSTIC ECHO, never an answer; surfacing that
/// echo as the assistant's reply is a bug, so the floor returns THIS message + keeps the gated
/// citations (still useful) under a non-"ok" status the FE renders as a non-answer notice.
const NO_MODEL_ANSWER_NOTICE: &str = "No AI model is available to answer — showing matching notes \
    instead. Pick a provider or download an on-device model in Settings.";

// RECORDING-AWARENESS phrases — SINGLE-SOURCED in `crate::prompts` (Brain v2 L3): the THREE
// load-bearing substrings BOTH the CLOUD cascade prompt
// (`crate::transcribe::live::assistant_system_prompt`, which now interpolates them) AND the
// deterministic FLOOR's prose below (`rag_answer`) compose in, so the two prompts cannot drift.
// Re-exported here so every historical `voice_action::*_PHRASE` path keeps working.
pub(crate) use crate::prompts::{
    MEETING_JUST_STARTED_PHRASE, NO_SUBSTITUTE_OTHER_MEETINGS_PHRASE, RECORDING_NOW_PHRASE,
};

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
///
/// `current_meeting_context` is the CURRENT (recording / focused-and-viewed) meeting's gated live
/// context — its live transcript tail + the user's typed notes, ALREADY visibility-gated by the
/// caller through `gated_live_context` (a sealed-not-visible meeting yields an EMPTY string, so this
/// path never reads ungated content). When non-empty it is PREPENDED to the vault grounding as the
/// PRIMARY, clearly-labeled source so the floor answers about THIS meeting first (fixing the
/// "describes other meetings" symptom on the local/reasoner-only backend that skips the cascade).
/// EMPTY ⇒ vault-only grounding, byte-identical to the prior floor.
///
/// `recording_in_progress` is the recorder-lock flag (`state.recorder.lock().map(|g| g.is_some())`),
/// NOT a content read — the same bool the CLOUD cascade computes. It gives the FLOOR RECORDING
/// AWARENESS so a live recording is framed as the current meeting being **recorded RIGHT NOW** (see
/// [`RECORDING_NOW_PHRASE`] and the two recording branches in [`rag_answer`]). `false` ⇒ the prior
/// viewed-past-meeting / idle behavior is byte-compatible.
#[allow(clippy::too_many_arguments)] // cohesive dispatch surface: intent + gated state + the AppHandle.
pub fn handle_voice_action(
    intent: &VoiceIntent,
    reasoner: &dyn LocalReasoner,
    db: &Db,
    unlocked: &HashSet<String>,
    config: &AppConfig,
    meeting_id: &str,
    literal_command: &str,
    current_meeting_context: &str,
    recording_in_progress: bool,
    app: Option<&tauri::AppHandle>,
) -> VoiceActionResult {
    match intent {
        VoiceIntent::Research { topic } => rag_answer(
            "research",
            topic,
            literal_command,
            current_meeting_context,
            recording_in_progress,
            reasoner,
            db,
            unlocked,
            config,
            app,
        ),
        VoiceIntent::Recall { entity } => rag_answer(
            "recall",
            entity,
            literal_command,
            current_meeting_context,
            recording_in_progress,
            reasoner,
            db,
            unlocked,
            config,
            app,
        ),
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
                Ok(()) => VoiceActionResult::new("create_reminder", "ok", "Added a reminder."),
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
        VoiceIntent::SlackSearch { query } => {
            // Ride the REAL connector: dispatch through `execute_slack_search` (fail-closed,
            // redacted, egress-ledgered inside the connector framework). When the Slack connector is
            // not exposed (default: disabled + unconsented) the tool returns its own graceful
            // "not available" sentinel — a non-answer that EGRESSES NOTHING, exactly like before,
            // but now a consented + configured user gets live Slack results. A real external failure
            // degrades to the same "not available" notice (non-PII), never a panic.
            let q = query.trim();
            let query_for_retrieval = if q.is_empty() {
                literal_command.trim()
            } else {
                q
            };
            match slack_search_blocking(query_for_retrieval, config) {
                Ok(text) => {
                    let text = text.trim();
                    if text.is_empty() || is_empty_tool_result(text) {
                        // Not exposed / no results → non-answer notice, still zero egress.
                        VoiceActionResult::new("slack_search", "unavailable", text.to_string())
                    } else {
                        VoiceActionResult::new("slack_search", "ok", text.to_string())
                    }
                }
                Err(e) => {
                    VoiceActionResult::new("slack_search", "unavailable", non_pii_error(&e))
                }
            }
        }
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
    let action = value
        .get("action")
        .and_then(|a| a.as_str())?
        .trim()
        .to_lowercase();
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
        "reminder" => Some(VoiceIntent::CreateReminder {
            text: argument,
            due: None,
        }),
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
    push(crate::summarize::related_context::salient_query(
        None,
        literal_command,
    ));
    // ADDITIONAL: the brain/parser topic verbatim, and its salient terms.
    push(topic.to_string());
    push(crate::summarize::related_context::salient_query(
        None, topic,
    ));
    queries
}

/// The coarse `intent_kind` discriminant for a [`VoiceIntent`] — the SAME mapping the card path uses
/// (`from_agent`): `Recall` → `"recall"`, everything else → `"research"`. Used by the deterministic
/// current-first floor to badge its Tier-1 answer consistently with the fan-out floor. Since the
/// floor only ever runs read intents (`floor_intent_for` demotes writes to `Research`), this is
/// always `research` or `recall` on the floor.
pub(crate) fn intent_kind_str(intent: &VoiceIntent) -> &'static str {
    match intent {
        VoiceIntent::Recall { .. } => "recall",
        _ => "research",
    }
}

/// TIER-1 CLASSIFIER (deterministic, EN + PL) — is the user asking about the CURRENT meeting itself
/// ("what is THIS meeting about", "summarize this recording", "co tu ustaliliśmy")? A CONSERVATIVE
/// match: it fires ONLY on a clear "this meeting / this conversation / here" question so the
/// current-first floor can answer from the current meeting in ISOLATION (no vault fan-out, no web
/// leg). When unsure it returns `false`, and the floor behaves EXACTLY as before (the vault + web
/// fan-out), so cross-meeting ("co ustaliliśmy z Weroniką") and world ("jaka pogoda") questions are
/// untouched.
///
/// This is the structural fix for "o czym jest to spotkanie" web-searching the WORD "meeting":
/// deterministically recognizing a current-meeting question lets the floor STOP at the current
/// meeting instead of fanning a `research` intent out to the web.
///
/// The matcher is intentionally lexical (substring over a normalized command), not a model call —
/// the whole point of the deterministic floor is that a weak local model can't be trusted to route.
pub(crate) fn is_about_current_meeting(command: &str) -> bool {
    // Normalize: lowercase + strip Polish diacritics so "rozmowę"/"rozmowe" and "spotkaniu" match on
    // stems, and collapse whitespace. We match on stems/substrings, never whole-word equality, so
    // inflected Polish endings (spotkani-e/-u/-a, rozmow-a/-e/-ie) all hit.
    let norm = normalize_for_match(command);
    let n = norm.as_str();
    if n.is_empty() {
        return false;
    }

    // A "HERE" DEICTIC — "tu"/"tutaj"/"here" ONLY (NOT "to"/"this"), for the `pl_here_verb` rule
    // ("co TU ustaliliśmy"). Deliberately excludes "to"/"this": the Polish relative pronoun "to co"
    // ("that which") in a CROSS-MEETING "podsumuj TO CO ustaliliśmy na spotkaniach" carries "to" but
    // is NOT a here-anchor, and matching it stole that request into current-meeting isolation
    // (adversarial 2026-07-09 round 2). Whole-token "tu" so it never sub-matches "sta**tu**s".
    let has_here = n.contains(" tu ")
        || n.starts_with("tu ")
        || n.ends_with(" tu")
        || n.contains(" tutaj")
        || n.contains("tutaj ")
        || n.contains(" here")
        || n.contains("here ");

    // A "THIS MEETING" PHRASE — the deictic IMMEDIATELY ADJACENT to a current-meeting noun (EN + PL
    // singular, diacritics stripped). This is the load-bearing discriminant: matching a bare "this"
    // + a "meeting" noun ANYWHERE in the string false-matched "summarize THIS WEEK'S MEETINGS" /
    // "podsumuj TO co ustaliliśmy NA SPOTKANIACH" (deictic modifies "week's", not the meeting; the
    // noun is plural/cross-meeting) → those got stolen from the vault fan-out (adversarial 2026-07-09).
    // Requiring adjacency ("this meeting" / "to spotkanie" / "tę rozmowę") keeps the current-meeting
    // cases and rejects "this week's meetings" (not adjacent). PL feminine list uses singular forms
    // only ("ta/te/tej rozmow…", "te rozmowe" = tę rozmowę acc) so "te rozmowy" (plural) doesn't hit.
    const THIS_MEETING_PHRASES: &[&str] = &[
        // English "this <meeting-noun>".
        "this meeting", "this conversation", "this call", "this recording",
        // Polish neuter (spotkanie / nagranie): to/tego/tym <noun>.
        "to spotkani", "tego spotkani", "tym spotkani", "to nagrani", "tego nagrani", "tym nagrani",
        // Polish feminine (rozmowa) — SINGULAR only.
        "ta rozmow", "te rozmowe", "tej rozmow",
    ];
    let has_this_meeting_phrase = THIS_MEETING_PHRASES.iter().any(|p| n.contains(p));

    // "what is X about" / "o czym (jest) X" — the canonical describe-this question.
    let about_phrasing = n.contains("what is")
        || n.contains("what's")
        || n.contains("whats ")
        || n.contains("o czym");
    if about_phrasing && has_this_meeting_phrase {
        return true;
    }

    // A reference to ANOTHER PARTY ("with X" / "z X") makes it a CROSS-MEETING question
    // ("co ustaliliśmy z Weroniką", "what did we decide with Weronika") — never current-first.
    let has_other_party = n.contains(" with ") || n.contains(" z ");

    // "summarize / recap / streść / podsumuj THIS meeting|recording|conversation". REQUIRES the
    // adjacent "this <meeting-noun>" phrase AND no other party — so a CROSS-MEETING summarize
    // ("podsumuj moje spotkania", "summarize my meetings", "summarize this week's meetings",
    // "podsumuj spotkania z Weroniką") is NOT stolen from the vault fan-out. A bare "summarize this"
    // (verb + deictic, no meeting noun) is deliberately NOT matched — it degrades to the fan-out floor
    // (a MISS is safe; a false-steal is a regression).
    let summarize_verb = n.contains("summarize")
        || n.contains("summarise")
        || n.contains("recap")
        || n.contains("podsumuj")
        || n.contains("streszcz") // streść → strescz after diacritic-strip of ś→s, ć→c
        || n.contains("stresc");
    if summarize_verb && has_this_meeting_phrase && !has_other_party {
        return true;
    }

    // PL "co (tu/tutaj) ustaliliśmy / omówiliśmy / zdecydowaliśmy / powiedziano" — a "what happened
    // HERE" question. REQUIRES the HERE deictic ("tu"/"tutaj") — NOT a bare "to" — so a cross-meeting
    // "co ustaliliśmy z Weroniką" (no here-anchor) and "podsumuj to co ustaliliśmy na spotkaniach"
    // (relative "to co", plural "na spotkaniach") do NOT match.
    let pl_here_verb = n.contains("ustalili")
        || n.contains("omowili")
        || n.contains("zdecydowali")
        || n.contains("powiedziano")
        || n.contains("powiedzielismy");
    if pl_here_verb && has_here && !has_other_party {
        return true;
    }

    // EN "what did we (just) discuss / decide (here)" — SELF-ANCHORING to the current conversation
    // ("we, now"), so it fires WITHOUT a separate deictic — UNLESS it references another party
    // ("...with Weronika"), which makes it cross-meeting.
    let en_self_verb = n.contains("did we discuss")
        || n.contains("did we decide")
        || n.contains("we just discuss")
        || n.contains("we just decide")
        || n.contains("we just talk");
    if en_self_verb && !has_other_party {
        return true;
    }

    false
}

/// Lowercase + strip common Polish diacritics + collapse internal whitespace, so the current-meeting
/// classifier can stem-match inflected Polish ("rozmowę"→"rozmowe", "streść"→"stresc") without a
/// dependency. Pure + total; never panics.
fn normalize_for_match(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_ws = true; // trim leading whitespace
    for ch in s.chars() {
        let lc = match ch {
            'ą' | 'Ą' => 'a',
            'ć' | 'Ć' => 'c',
            'ę' | 'Ę' => 'e',
            'ł' | 'Ł' => 'l',
            'ń' | 'Ń' => 'n',
            'ó' | 'Ó' => 'o',
            'ś' | 'Ś' => 's',
            'ż' | 'Ż' | 'ź' | 'Ź' => 'z',
            other => other,
        };
        if lc.is_whitespace() {
            if !last_ws {
                out.push(' ');
                last_ws = true;
            }
        } else {
            for c in lc.to_lowercase() {
                out.push(c);
            }
            last_ws = false;
        }
    }
    // trim trailing whitespace
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// TIER-1 ISOLATED ANSWER (deterministic current-first floor): synthesize a concise answer over ONLY
/// the CURRENT meeting's own content — NO vault fan-out, NO web leg, NO calendar. The caller
/// (`run_informational`) has already assembled `current_content` through a VISIBILITY-GATED reader
/// (live buffer via `gated_live_context`, OR a viewed past meeting's `get_segments` +
/// `get_note_if_visible` under `meeting_is_visible`); a sealed-not-unlocked meeting yields an EMPTY
/// string, so this path never reads or synthesizes sealed content.
///
/// EMPTY `current_content` ⇒ an HONEST short notice in the user's language (recording → "just
/// started / nothing captured yet"; viewed past meeting → "no transcript/notes to summarize"), with
/// NO fan-out. This is what stops "o czym jest to spotkanie" from web-searching the word "meeting".
///
/// The prompt deliberately does NOT hand the model a literal "THIS MEETING" section header (which the
/// weak local model would echo as its opening words) — it frames the content as "the transcript and
/// notes below" and instructs the model not to preface with a label.
///
/// `meeting_title` is the current meeting's OWN gated title (resolved by the caller via
/// `get_meeting`, VISIBLE-only) — cited as `[[Title]]` so the answer attributes itself to the
/// current meeting, never to a vault/web source.
///
/// EGRESS: rides the SAME consent-gated reasoner as every other floor answer (a Cloud reasoner routes
/// through `make_provider`'s consent gate + RedactingProvider). No new egress class; the ONLY content
/// that reaches the model is the already-gated current meeting.
pub(crate) fn answer_current_meeting_isolated(
    intent_kind: &str,
    literal_command: &str,
    current_content: &str,
    recording_in_progress: bool,
    meeting_title: Option<&str>,
    reasoner: &dyn LocalReasoner,
) -> VoiceActionResult {
    let content = current_content.trim();
    let citations: Vec<String> = meeting_title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| vec![format!("[[{t}]]")])
        .unwrap_or_default();

    // EMPTY current content ⇒ honest "just started / no content" notice, in the user's language. NO
    // fan-out, NO web, NO other-meeting substitution. We ask the reasoner ONLY to translate a short
    // honest sentence into the user's language (with an English fallback baked in), so even here the
    // model never sees vault/web content and never describes another meeting.
    if content.is_empty() {
        let english = if recording_in_progress {
            "This meeting just started — nothing has been captured from it yet, so there is \
             nothing to summarize."
        } else {
            "This meeting has no transcript or notes to summarize."
        };
        // STUB / no brain: return the English notice verbatim (no translation available). Honest,
        // no fan-out.
        if reasoner.id() == "stub" {
            return VoiceActionResult {
                intent_kind: intent_kind.to_string(),
                status: "ok".to_string(),
                summary: english.to_string(),
                command: String::new(),
                citations,
                proposed_note: None,
                thread_id: None,
                answered_from: Some(AnsweredFrom::CurrentMeeting),
            };
        }
        let system = "You are an in-meeting assistant. Reply with EXACTLY the sentence below, \
                      translated into the SAME language the user wrote their request in (look at \
                      the user's OWN words, NOT this English instruction). Do not add anything, do \
                      not mention other meetings, do not search anything.";
        let user = format!(
            "User's request (their own words): {}\n\nSentence to translate: {english}",
            literal_command.trim()
        );
        // P0.3: a LIVE user-facing answer rides the live preset (capped decode; the GGUF path also
        // gets the 30 s wall-clock timeout). Best-effort no-op on stub/cloud reasoners.
        let summary = match reasoner.reason_with(system, &user, GenOptions::live_answer()) {
            Ok(a) if !a.trim().is_empty() => a.trim().to_string(),
            // Any failure (incl. no-consent Unavailable) ⇒ the honest English notice, never a
            // fan-out. The user still gets a truthful, non-leaking answer.
            _ => english.to_string(),
        };
        return VoiceActionResult {
            intent_kind: intent_kind.to_string(),
            status: "ok".to_string(),
            summary,
            command: String::new(),
            citations,
            proposed_note: None,
            thread_id: None,
            answered_from: Some(AnsweredFrom::CurrentMeeting),
        };
    }

    // STUB / no real brain: cannot synthesize. Return the honest no-model notice + KEEP the current
    // meeting's own citation (mirrors the `rag_answer` stub shim), never a fan-out.
    if reasoner.id() == "stub" {
        return VoiceActionResult {
            intent_kind: intent_kind.to_string(),
            status: "unavailable".to_string(),
            summary: NO_MODEL_ANSWER_NOTICE.to_string(),
            command: String::new(),
            citations,
            proposed_note: None,
            thread_id: None,
            answered_from: Some(AnsweredFrom::CurrentMeeting),
        };
    }

    // ISOLATED SYNTHESIS over ONLY the current meeting. No "THIS MEETING" label the model can parrot;
    // no instruction to consult the vault or the web. Language directive mirrors the fan-out floor.
    let system = "You are summarizing the meeting the user is currently in. Answer their question \
                  about it CONCISELY (2-4 sentences) using ONLY the transcript and notes provided \
                  below — do not use any other source and do not invent facts. Do NOT preface your \
                  answer with a label or heading (no \"This meeting:\"); answer naturally. Write \
                  your answer in the SAME language the user actually wrote in — look at the user's \
                  OWN words below, NOT the language of this instruction or of the transcript. If the \
                  user wrote in Polish, answer in Polish; NEVER default to English.";
    let user = format!(
        "User's request (their own words): {}\n\nThe meeting's transcript and the user's notes:\n{content}",
        literal_command.trim()
    );
    // P0.3: LIVE answer preset — bounded decode so the isolated Tier-1 synthesis can't run away.
    match reasoner.reason_with(system, &user, GenOptions::live_answer()) {
        Ok(answer) if !answer.trim().is_empty() => VoiceActionResult {
            intent_kind: intent_kind.to_string(),
            status: "ok".to_string(),
            summary: answer.trim().to_string(),
            command: String::new(),
            citations,
            proposed_note: None,
            thread_id: None,
            answered_from: Some(AnsweredFrom::CurrentMeeting),
        },
        Ok(_) => VoiceActionResult {
            intent_kind: intent_kind.to_string(),
            status: "ok".to_string(),
            summary: "I have this meeting's transcript but couldn't compose a summary just now."
                .to_string(),
            command: String::new(),
            citations,
            proposed_note: None,
            thread_id: None,
            answered_from: Some(AnsweredFrom::CurrentMeeting),
        },
        // No cloud consent ⇒ fail closed with a graceful notice — NEVER a fan-out to the web.
        Err(AppError::Unavailable(_)) => VoiceActionResult {
            intent_kind: intent_kind.to_string(),
            status: "needs_consent".to_string(),
            summary: "The cloud brain needs your one-time consent to answer (Settings ▸ Privacy)."
                .to_string(),
            command: String::new(),
            citations,
            proposed_note: None,
            thread_id: None,
            answered_from: Some(AnsweredFrom::CurrentMeeting),
        },
        Err(e) => VoiceActionResult {
            intent_kind: intent_kind.to_string(),
            status: "error".to_string(),
            summary: non_pii_error(&e),
            command: String::new(),
            citations,
            proposed_note: None,
            thread_id: None,
            answered_from: Some(AnsweredFrom::CurrentMeeting),
        },
    }
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
    current_meeting_context: &str,
    recording_in_progress: bool,
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
    // ORG LEG (SHARED BRAIN): when the caller has JOINED an org, ALSO search the org partition and
    // fold its LOUD, provenance-labelled hits into the grounding. Runs for BOTH `recall` and
    // `research` (unlike the web leg, which is research-only) — a colleague's shared note is exactly
    // the kind of fact "what do we know about X" (recall) should surface, not just open-ended
    // research. ONE call (not looped per retrieval query) to avoid multiplying embedding calls, using
    // the same `display_query` the other single-call legs (web/calendar) key off. The org text is
    // UNTRUSTED multi-writer content but `search_org_brain` ALREADY fence-neutralizes it
    // (`neutralize_murmur_fences`, tools.rs) before returning — treat the string as already-safe data,
    // exactly like the web leg treats web results; do not re-neutralize or skip that step.
    let mut org_lines: Vec<String> = Vec::new();
    if crate::tools::org_brain_available(db, config) {
        let org_query = if !display_query.is_empty() {
            display_query.clone()
        } else {
            literal_command.trim().to_string()
        };
        if !org_query.is_empty() {
            match crate::tools::search_org_brain(db, config, &org_query) {
                Ok(text) => {
                    let text = text.trim();
                    if !text.is_empty() && !is_empty_tool_result(text) {
                        // Capture the loud "[org · …]" lines for citations, and add to grounding.
                        for line in text.lines() {
                            if line.trim_start().starts_with("- [org") {
                                org_lines.push(line.to_string());
                            }
                        }
                        if !grounding.is_empty() {
                            grounding.push_str("\n\n");
                        }
                        grounding.push_str(text);
                        grounding.push_str("\n\n");
                    }
                }
                // An org lookup failure is non-fatal — the vault (+ web/calendar) grounding still
                // answers.
                Err(e) => tracing::debug!(
                    target: "voice",
                    error = %e,
                    "voice-action org-brain search failed; continuing with vault grounding"
                ),
            }
        }
    }
    let grounding = grounding.trim();

    // CURRENT-MEETING-FIRST (fixes the "describes other meetings" symptom on the local/reasoner-only
    // floor): the caller already fetched THIS meeting's live transcript + typed notes through the
    // GATED `gated_live_context` (a sealed-not-visible meeting yields ""), so it is safe to use here —
    // no ungated read happens in this function. When present, it becomes the PRIMARY, clearly-labeled
    // grounding section, PREPENDED before the vault notes, so the brain answers about THIS meeting
    // first and only reaches for the vault for cross-meeting context. EMPTY ⇒ vault-only, byte-
    // identical to the prior floor. It does NOT feed vault-citation extraction below (it is the
    // current meeting, not a cross-note citation) and its presence alone counts as real grounding.
    let current_ctx = current_meeting_context.trim();
    let has_current = !current_ctx.is_empty();

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
                    if let Some(t) = h
                        .meeting
                        .title
                        .as_deref()
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
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
    // ORG citations: each loud "- [org · <author>] Title — snippet" line becomes a distinct
    // "(org · <author>) Title" citation, so org-derived facts stay attributed to their untrusted
    // colleague-provenance and are visibly distinguishable from the `[[Title]]` vault wikilinks.
    for line in &org_lines {
        if let Some(c) = org_citation_from_line(line) {
            push_cite(c);
        }
    }

    // No grounding at all → don't burn a brain call; return a clean "nothing found". The CURRENT
    // meeting's gated context counts as grounding, so a live/focused meeting is always answerable from
    // itself even when the vault legs found nothing.
    if grounding.is_empty() && !has_current {
        return VoiceActionResult::new(
            intent_kind,
            "ok",
            format!("I couldn't find anything in your vault about \"{display_query}\"."),
        );
    }

    // STUB-SHIM (the floor): no real brain is available to SYNTHESIZE an answer. The active reasoner
    // is the deterministic `StubReasoner` (`id() == "stub"`: backend Off, or Local with no GGUF
    // downloaded), whose `reason()` returns a DIAGNOSTIC ECHO ("[stub-reason] system=… user=…"),
    // NEVER an answer — surfacing that echo as the reply reads like a broken answer. Mirror the exact
    // `reasoner.id() == "stub"` guard the note-context floor uses (`orchestrate.rs`): do NOT call
    // `reason()`, return an HONEST notice + KEEP the gated citations (still useful), under a non-"ok"
    // status the FE already renders as a non-answer notice. Placed AFTER grounding so the useful
    // citations are still surfaced; the visible-only citation set was built above.
    if reasoner.id() == "stub" {
        return VoiceActionResult {
            intent_kind: intent_kind.to_string(),
            status: "unavailable".to_string(),
            summary: NO_MODEL_ANSWER_NOTICE.to_string(),
            command: String::new(),
            citations,
            proposed_note: None,
            thread_id: None,
            // The deterministic floor is not a cascade tier.
            answered_from: None,
        };
    }

    // The brain may now ground its answer on BOTH the user's vault notes AND any web results. The
    // prompt allows web facts (attributed) so a "what's the weather" question the vault can't answer
    // is still answered from the web leg, while vault-answerable questions stay vault-grounded.
    //
    // CURRENT-FIRST: when THIS meeting's gated context is present the prompt tells the brain to answer
    // about "this meeting" from its own transcript FIRST and treat the vault notes as SECONDARY (cross-
    // meeting) context only — this is what stops the local floor from describing OTHER meetings.
    //
    // LANGUAGE (fixes the English-answer symptom): mirror the cascade/agent directive
    // (agent.rs) — the final answer must be in the SAME language the USER actually wrote in (their own
    // words in the request, NOT the language of these English instructions). Polish in → Polish out.
    // The user's literal command is echoed into the `user` message below so the model can see the
    // original words even when `display_query` is a brain-translated/normalized topic.
    // RECORDING AWARENESS (back-ported from the CLOUD cascade's `assistant_system_prompt`): when a
    // recording is IN PROGRESS the floor must frame the current meeting as the LIVE recording, not a
    // viewed past meeting — and, critically, when the live buffer is EMPTY (meeting just started) it
    // must NOT let the vault grounding be described as if it were this meeting. The exact wording of
    // the three load-bearing phrases is SHARED with the cascade via the `*_PHRASE` consts so the two
    // prompts cannot drift. Three cases (see the four-way matrix on recording × has_current):
    //   1. recording + buffer HAS content  → THIS MEETING section is the live transcript; answer from
    //      it FIRST, vault is cross-meeting context only.
    //   2. recording + buffer EMPTY         → the gap this task closes: say plainly the meeting just
    //      started; forbid substituting other saved meetings even though vault grounding is present.
    //   3. NOT recording                    → unchanged prior behavior (viewed-past-meeting clause when
    //      has_current; empty otherwise) — no regression for the Ask page / idle / viewed-meeting.
    let current_clause = if recording_in_progress && has_current {
        format!(
            " A meeting is being {RECORDING_NOW_PHRASE} and the THIS MEETING section is its LIVE \
             transcript (plus the user's own typed notes) so far — answer about THIS meeting from that \
             section FIRST. When the user asks what \"this meeting\"/\"this conversation\"/\"ta \
             rozmowa\" is about, answer FROM that live transcript; {NO_SUBSTITUTE_OTHER_MEETINGS_PHRASE} \
             and describe them as if they were this one. The vault notes are cross-meeting context only, \
             never the primary subject."
        )
    } else if recording_in_progress {
        // Buffer EMPTY while recording: the meeting just started (or the user hasn't spoken / captions
        // lag). Vault grounding may still be present for an EXPLICIT saved-notes question, but the
        // framing FORBIDS treating it as the current meeting.
        format!(
            " A meeting is being {RECORDING_NOW_PHRASE} but nothing has been transcribed from it yet \
             (it just started or the user has not spoken much). If the user asks about THIS \
             meeting/\"this conversation\"/\"ta rozmowa\", say plainly the {MEETING_JUST_STARTED_PHRASE} \
             and little has been captured so far — {NO_SUBSTITUTE_OTHER_MEETINGS_PHRASE} and describe \
             them as if they were this one. Use the vault notes ONLY when the user EXPLICITLY asks about \
             their saved notes/past meetings."
        )
    } else if has_current {
        " The provided context includes a THIS MEETING section (the current meeting's live transcript \
         and the user's own typed notes) — answer about THIS meeting from that section FIRST, and use \
         the vault notes only for cross-meeting context, not as the primary subject."
            .to_string()
    } else {
        String::new()
    };
    let system = format!(
        "You are an in-meeting assistant. Answer the user's request CONCISELY (2-4 sentences) using \
         ONLY the provided context: the current meeting, the user's own meeting notes, any WEB results \
         (lines beginning \"[web\") AND any org/Shared Brain results from colleagues (lines beginning \
         \"- [org ·\").{current_clause} Cite vault meetings by their [[Title]] wikilink, attribute web \
         facts as \"(via web)\", and attribute org facts by their \"[org · author]\" provenance so it's \
         clear they came from a colleague, not the user's own notes. If the context doesn't cover it, \
         say so plainly. Do not invent facts. Write your final answer in the SAME language the USER \
         actually wrote in — look at the user's OWN words in their request below, NOT at the language \
         of these instructions or the surrounding context (which are in English). If the user wrote in \
         Polish, answer in Polish; match the user's language exactly and NEVER default to English."
    );
    // The user message leads with the user's ORIGINAL dictated words (so the model can match their
    // language even when `display_query` is a normalized/translated topic), then the CURRENT meeting's
    // gated context as the PRIMARY source, then the vault notes as SECONDARY context.
    let original_words = {
        let lit = literal_command.trim();
        if lit.is_empty() { display_query.as_str() } else { lit }
    };
    let mut user = format!("Request (user's own words): {original_words}");
    if !display_query.is_empty() && display_query != original_words {
        user.push_str(&format!("\nTopic: {display_query}"));
    }
    if has_current {
        user.push_str(&format!(
            "\n\nTHIS MEETING (current transcript + your notes):\n{current_ctx}"
        ));
    }
    if !grounding.is_empty() {
        user.push_str(&format!("\n\nNotes from the vault (secondary context):\n{grounding}"));
    }

    // EGRESS SEAM: a Cloud reasoner routes through make_provider (consent gate + RedactingProvider).
    // A no-consent refusal comes back as AppError::Unavailable → graceful "needs consent", no leak.
    // P0.3: the floor's user-facing synthesis is a LIVE answer — ride the live (capped) preset.
    match reasoner.reason_with(&system, &user, GenOptions::live_answer()) {
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
                proposed_note: None,
                thread_id: None,
                // The deterministic floor is not a cascade tier.
                answered_from: None,
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
            proposed_note: None,
            thread_id: None,
            answered_from: None,
        },
        Err(e) => VoiceActionResult {
            intent_kind: intent_kind.to_string(),
            status: "error".to_string(),
            summary: non_pii_error(&e),
            command: String::new(),
            citations,
            proposed_note: None,
            thread_id: None,
            answered_from: None,
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

/// Run the async SLACK-search connector to completion from this SYNCHRONOUS dispatch. Mirrors
/// [`web_search_blocking`]: a dedicated scoped OS thread with its own current-thread runtime, so we
/// never "start a runtime within a runtime" and the future never crosses a thread boundary (only the
/// `Result<String>` does). The egress/consent/redaction discipline all lives inside
/// `execute_slack_search` → the connector registry (fail-closed when the Slack connector is absent).
fn slack_search_blocking(query: &str, config: &AppConfig) -> crate::error::Result<String> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| AppError::Summarize(format!("slack search runtime build: {e}")))?;
                rt.block_on(crate::tools::execute_slack_search(query, config))
            })
            .join()
            .map_err(|_| AppError::Summarize("slack search worker thread panicked".into()))?
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
        "calendar",
        "meeting",
        "meetings",
        "agenda",
        "next meeting",
        "who's in",
        "who is in",
        "attendee",
        "attendees",
        "schedule",
        "scheduled",
        "appointment",
        "invite",
        // Polish
        "kalendarz",
        "spotkanie",
        "spotkania",
        "spotkaniu",
        "agenda",
        "kto jest",
        "uczestnik",
        "uczestnicy",
        "harmonogram",
        "termin",
    ];
    let hay = format!(
        "{} {}",
        topic.to_lowercase(),
        literal_command.to_lowercase()
    );
    KEYWORDS.iter().any(|k| hay.contains(k))
}

/// Run the async LOCAL-CALENDAR connector to completion from this SYNCHRONOUS dispatch (the live loop
/// spawns `handle_voice_action` off-thread). Mirrors [`web_search_blocking`]: a dedicated scoped OS
/// thread with its own current-thread runtime, so we never "start a runtime within a runtime" and the
/// future never crosses a thread boundary (only the `Result<String>` does). NO egress — the calendar
/// read is on-device — and `fetch_events` degrades to empty on every failure inside
/// `execute_calendar_search`.
fn calendar_search_blocking(query: &str, app: &tauri::AppHandle) -> crate::error::Result<String> {
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

/// Turn a loud org-brain grounding line `- [org · <author>] Title — snippet` into a compact citation
/// `(org · <author>) Title`. Returns `None` for a non-org line. Deterministic, no regex. Kept distinct
/// from [`web_citation_from_line`]/[`calendar_citation_from_line`] so org-derived facts stay
/// attributed to their untrusted colleague-provenance, never blended with the user's own `[[Title]]`
/// vault wikilinks.
fn org_citation_from_line(line: &str) -> Option<String> {
    let line = line.trim();
    let after_label = line.strip_prefix("- [org")?;
    let close = after_label.find(']')?;
    // `after_label` looks like " · <author>] Title — snippet" — the author sits between " · " and "]".
    let author = after_label[..close]
        .trim()
        .trim_start_matches('·')
        .trim();
    let rest = after_label[close + 1..].trim();
    let title = rest.split(" — ").next().unwrap_or(rest).trim();
    if title.is_empty() {
        return None;
    }
    if author.is_empty() {
        Some(format!("(org) {title}"))
    } else {
        Some(format!("(org · {author}) {title}"))
    }
}

/// Whether a tool result is a "nothing found" / "disabled" placeholder rather than real content,
/// so it can be excluded from the brain grounding (an included placeholder would falsely count as
/// grounding and trigger a brain call on an empty vault). Matches the deterministic prefixes
/// `execute_tool` emits (`No meetings or documents match` — including `search_semantic`'s flag-off
/// keyword-fallback variant — `No data`, `No open commitments`, `No visible entity`; the legacy
/// `No meetings match` / `Semantic search is disabled` prefixes are kept so an old-shape sentinel
/// can never miscount as grounding) plus `search_org_brain`'s own sentinels (`No org-brain results`).
fn is_empty_tool_result(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("No meetings match")
        || t.starts_with("No meetings or documents match")
        || t.starts_with("No data")
        || t.starts_with("No open commitments")
        || t.starts_with("No visible entity")
        || t.starts_with("Semantic search is disabled")
        || t.starts_with("No web results")
        || t.starts_with("Web search is not available")
        || t.starts_with("No Slack results")
        || t.starts_with("Slack search is not available")
        || t.starts_with("No calendar events")
        || t.starts_with("No org-brain results")
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
        let p = crate::storage::db::unique_temp_path("murmur-voiceaction", "sqlite");
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
            model_requested: None,
            model_served: None,
            gateway_host: None,
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
            model_requested: None,
            model_served: None,
            gateway_host: None,
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
            note_drafts: true,
            scope: crate::tools::AssistantScope::Full,
            seal: None,
            proposed_note: std::sync::Mutex::new(None),
        };

        // (1) Direct gate proof on the executor (RED-able).
        let got = exec
            .run(
                "get_meeting",
                &serde_json::json!({ "meetingId": "sealed1" }),
            )
            .unwrap();
        assert!(
            got.starts_with("No data"),
            "sealed meeting fetch must be gated: {got}"
        );
        let searched = exec
            .run("search_meetings", &serde_json::json!({ "query": "Atlas" }))
            .unwrap();
        assert!(
            !searched.contains("sealed1"),
            "sealed meeting must NOT appear in gated search: {searched}"
        );
        assert!(
            searched.contains("open1"),
            "the VISIBLE meeting is still found"
        );

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
            fn structured(
                &self,
                _s: &str,
                _u: &str,
                _schema: &Value,
            ) -> crate::error::Result<Value> {
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
        let outcome = run_agentic_loop(
            &brain,
            "sys",
            "what are the secret terms?",
            &exec,
            5,
            None,
            crate::reason::GenOptions::default(),
        )
        .unwrap()
        .expect("the brain answered");
        assert!(
            !outcome
                .citations
                .iter()
                .any(|c| c.contains("Atlas Secret Terms")),
            "the loop must never cite the sealed meeting: {:?}",
            outcome.citations
        );
        assert!(
            outcome.steps.iter().all(|s| s.ok),
            "gated tool calls ran without panic"
        );
    }

    #[test]
    fn research_returns_cited_summary_built_only_from_visible_content() {
        let db = tmp_db();
        seed_visible_and_sealed(&db);
        let reasoner = MockReasoner::new("Atlas is migrating; see [[Atlas Kickoff]].");
        let cfg = AppConfig::default();

        let res = handle_voice_action(
            &VoiceIntent::Research {
                topic: "Atlas".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &cfg,
            "live-mtg",
            "",
            "",
            false,
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

    /// RED-before-GREEN (adversarial bug 2026-07-04): on the DETERMINISTIC FLOOR (brain backend Off,
    /// or Local with no GGUF downloaded), `ReasonerCell` dispatches the `StubReasoner`, whose
    /// `reason()` returns a DIAGNOSTIC ECHO ("[stub-reason] system=… user=…"). Before the fix, with
    /// non-empty vault grounding this echo became the `summary` under status "ok" and rendered in the
    /// FE thread as the assistant's ANSWER — a broken-looking reply. The floor MUST NOT surface the
    /// stub echo: it returns an honest "no model available" notice (non-"ok" status) while KEEPING
    /// the gated citations. RED on the old code (summary == the "[stub-reason] …" echo, status "ok").
    #[test]
    fn floor_with_stub_reasoner_never_surfaces_stub_echo() {
        let db = tmp_db();
        seed_visible_and_sealed(&db); // gives a VISIBLE "Atlas Kickoff" note ⇒ non-empty grounding.
        let stub = crate::reason::StubReasoner;
        let res = handle_voice_action(
            &VoiceIntent::Research {
                topic: "Atlas".into(),
            },
            &stub,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live-mtg",
            "",
            "",
            false,
            None,
        );

        // THE BUG: the stub's diagnostic echo must NEVER become the assistant's answer.
        assert!(
            !res.summary.contains("[stub-reason]"),
            "stub echo leaked as the answer: {}",
            res.summary
        );
        assert!(
            !res.summary.contains("stub-reason"),
            "no stub-echo shape may surface: {}",
            res.summary
        );
        // Honest, non-"ok" status the FE renders as a non-answer notice (reused: the SlackSearch arm's
        // "unavailable" — an existing VoiceActionStatus already rendered on the FE).
        assert_eq!(
            res.status, "unavailable",
            "stub floor must be a non-'ok' notice, not an answer"
        );
        assert!(
            res.summary.contains("No AI model is available"),
            "the honest notice must be surfaced: {}",
            res.summary
        );
        // The gated citations are still useful and MUST be kept (VISIBLE meeting only).
        assert!(
            res.citations.contains(&"[[Atlas Kickoff]]".to_string()),
            "the visible-only citations must be preserved: {:?}",
            res.citations
        );
        // ...and the SEALED meeting stays out of the citations even on the stub floor.
        assert!(
            !res.citations.iter().any(|c| c.contains("Secret")),
            "SEALED meeting must NOT be cited on the stub floor: {:?}",
            res.citations
        );
    }

    /// P0.2 (Brain v2) — the stub-echo guard, pinned END-TO-END on both deterministic-floor answer
    /// paths `run_informational` dispatches to (the fan-out floor via `handle_voice_action` and the
    /// Tier-1 isolated current-meeting answer). With the StubReasoner (brain Off / no GGUF), the
    /// user-facing summary must (a) NEVER carry the `[stub-reason]` diagnostic echo and (b) point
    /// the user at Settings so they know how to enable a real brain.
    ///
    /// HONEST RED/GREEN NOTE: this test was run BEFORE any P0 change and already PASSED — the
    /// per-call-site guards shipped in #172 ("don't surface StubReasoner echo") and the 0.8.0
    /// cascade work already cover every `reason()` call on the floor. It is kept as the regression
    /// pin for the P0.2 invariant rather than a RED-first bug capture.
    #[test]
    fn stub_floor_hints_settings_and_never_echoes_stub() {
        // 1) The fan-out floor over a seeded vault (non-empty grounding → the synthesis point).
        let db = tmp_db();
        seed_visible_and_sealed(&db);
        let stub = crate::reason::StubReasoner;
        let res = handle_voice_action(
            &VoiceIntent::Research {
                topic: "Atlas".into(),
            },
            &stub,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live-mtg",
            "",
            "",
            false,
            None,
        );
        assert!(
            !res.summary.contains("[stub-reason]"),
            "the stub echo must never surface on the fan-out floor: {}",
            res.summary
        );
        assert!(
            res.summary.contains("Settings"),
            "the no-model notice must point at Settings: {}",
            res.summary
        );

        // 2) The Tier-1 isolated current-meeting answer with content present.
        let res2 = answer_current_meeting_isolated(
            "research",
            "o czym to spotkanie",
            "Transcript:\n[0s] Budget approved.",
            true,
            Some("Budget Review"),
            &stub,
        );
        assert!(
            !res2.summary.contains("[stub-reason]"),
            "the stub echo must never surface on the Tier-1 isolated answer: {}",
            res2.summary
        );
        assert!(
            res2.summary.contains("Settings"),
            "the Tier-1 no-model notice must point at Settings: {}",
            res2.summary
        );
    }

    #[test]
    fn recall_returns_ok_and_excludes_sealed() {
        let db = tmp_db();
        seed_visible_and_sealed(&db);
        let reasoner = MockReasoner::new("Recall: [[Atlas Kickoff]] covered the plan.");
        let res = handle_voice_action(
            &VoiceIntent::Recall {
                entity: "Atlas".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live-mtg",
            "",
            "",
            false,
            None,
        );
        assert_eq!(res.intent_kind, "recall");
        assert_eq!(res.status, "ok");
        let grounding = reasoner.last_user.lock().unwrap().clone().unwrap();
        assert!(
            !grounding.contains("Atlas Secret Terms"),
            "sealed excluded from recall too"
        );
    }

    #[test]
    fn research_with_no_vault_match_is_ok_without_brain_call() {
        let db = tmp_db();
        seed_visible_and_sealed(&db);
        let reasoner = MockReasoner::new("SHOULD-NOT-BE-CALLED");
        let res = handle_voice_action(
            &VoiceIntent::Research {
                topic: "nonexistent-topic-zzz".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live-mtg",
            "",
            "",
            false,
            None,
        );
        assert_eq!(res.status, "ok");
        assert!(res.summary.contains("couldn't find"));
        // No grounding ⇒ no brain call (so the mock never recorded a user prompt).
        assert!(
            reasoner.last_user.lock().unwrap().is_none(),
            "brain must not be called with empty grounding"
        );
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
            &VoiceIntent::NoteAside {
                text: "deadline is friday".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live1",
            "",
            "",
            false,
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
            &VoiceIntent::NoteAside {
                text: "secret aside".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "sealed1",
            "",
            "",
            false,
            None,
        );
        assert_eq!(res.status, "error");
        assert!(
            db.list_note_asides("sealed1").unwrap().is_empty(),
            "no aside written to a sealed meeting"
        );
    }

    #[test]
    fn slack_search_is_unavailable() {
        let db = tmp_db();
        let reasoner = MockReasoner::new("");
        let res = handle_voice_action(
            &VoiceIntent::SlackSearch {
                query: "raport".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live1",
            "",
            "",
            false,
            None,
        );
        assert_eq!(res.intent_kind, "slack_search");
        // The SlackSearch intent now rides the REAL connector: with a default (unconsented) config it
        // returns the tool's OWN fail-closed sentinel — a non-answer under the "unavailable" status,
        // still zero egress (no token, no network).
        assert_eq!(res.status, "unavailable");
        assert!(
            res.summary.contains("not available"),
            "fail-closed connector sentinel flows through (zero egress): {}",
            res.summary
        );
    }

    #[test]
    fn unknown_is_unrecognized_and_echoes_nothing_sensitive() {
        let db = tmp_db();
        let reasoner = MockReasoner::new("");
        let res = handle_voice_action(
            &VoiceIntent::Unknown {
                raw: "qwer asdf secret-thing".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live1",
            "",
            "",
            false,
            None,
        );
        assert_eq!(res.status, "unrecognized");
        assert!(
            !res.summary.contains("secret-thing"),
            "raw command must not be echoed back"
        );
    }

    #[test]
    fn reasoner_error_degrades_gracefully_without_panic() {
        let db = tmp_db();
        seed_visible_and_sealed(&db);
        // A generic Summarize error → status "error", non-PII message, citations still surfaced.
        let reasoner = ErrReasoner(AppError::Summarize("boom internal detail".into()));
        let res = handle_voice_action(
            &VoiceIntent::Research {
                topic: "Atlas".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live1",
            "",
            "",
            false,
            None,
        );
        assert_eq!(res.status, "error");
        assert!(
            !res.summary.contains("boom internal detail"),
            "internal error detail must not leak"
        );
        assert!(res.citations.contains(&"[[Atlas Kickoff]]".to_string()));
    }

    #[test]
    fn cloud_no_consent_maps_to_needs_consent_failclosed() {
        let db = tmp_db();
        seed_visible_and_sealed(&db);
        // The make_provider consent gate returns AppError::Unavailable when consent is OFF.
        let reasoner = ErrReasoner(AppError::Unavailable("cloud egress not consented".into()));
        let res = handle_voice_action(
            &VoiceIntent::Research {
                topic: "Atlas".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live1",
            "",
            "",
            false,
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
            (
                serde_json::json!({"action":"research","argument":"wakacjach"}),
                VoiceIntent::Research {
                    topic: "wakacjach".into(),
                },
            ),
            (
                serde_json::json!({"action":"recall","argument":"atlas"}),
                VoiceIntent::Recall {
                    entity: "atlas".into(),
                },
            ),
            (
                serde_json::json!({"action":"reminder","argument":"call bob"}),
                VoiceIntent::CreateReminder {
                    text: "call bob".into(),
                    due: None,
                },
            ),
            (
                serde_json::json!({"action":"note","argument":"deadline friday"}),
                VoiceIntent::NoteAside {
                    text: "deadline friday".into(),
                },
            ),
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
        assert_eq!(
            got,
            VoiceIntent::Research {
                topic: "look into the pricing model".into()
            }
        );
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
        let r =
            VoiceActionResult::new("research", "ok", "answer").with_command("zrób research o X");
        assert_eq!(r.command, "zrób research o X");
        // nothing_heard carries an empty command (nothing was heard).
        assert!(VoiceActionResult::nothing_heard().command.is_empty());
    }

    /// `proposed_note` round-trips through `VoiceActionResult`: defaults to None (a plain ANSWER),
    /// `with_proposed_note(Some(..))` marks the reply as a NOTE PROPOSAL, and it serializes to the
    /// camelCase `proposedNote` field the FE reads (so it can show "Add to notes" only when present).
    #[test]
    fn voice_action_result_round_trips_proposed_note() {
        // Default: no proposal ⇒ plain answer.
        let plain = VoiceActionResult::new("research", "ok", "an answer");
        assert_eq!(
            plain.proposed_note, None,
            "a result defaults to no note proposal (a plain answer)"
        );

        // With a proposal: the reply IS a note draft.
        let proposed = plain
            .clone()
            .with_proposed_note(Some("Decision: ship Friday.".to_string()));
        assert_eq!(
            proposed.proposed_note.as_deref(),
            Some("Decision: ship Friday.")
        );

        // None / whitespace are no-ops (still a plain answer).
        assert_eq!(plain.clone().with_proposed_note(None).proposed_note, None);
        assert_eq!(
            plain.with_proposed_note(Some("   ".into())).proposed_note,
            None
        );

        // Serializes to the camelCase `proposedNote` field the FE consumes; absent (null) ⇒ plain.
        let json = serde_json::to_value(
            VoiceActionResult::new("research", "ok", "draft reply")
                .with_proposed_note(Some("note body".into())),
        )
        .unwrap();
        assert_eq!(
            json.get("proposedNote").and_then(|v| v.as_str()),
            Some("note body")
        );
        let null_json =
            serde_json::to_value(VoiceActionResult::new("research", "ok", "x")).unwrap();
        assert!(
            null_json.get("proposedNote").unwrap().is_null(),
            "no proposal ⇒ proposedNote is null"
        );
    }

    /// PR D: `thread_id` round-trips through `VoiceActionResult` — default None, threaded on via
    /// `with_thread_id`, serialized to the camelCase `threadId` the FE uses to resolve the right
    /// open thread's pending bubble.
    #[test]
    fn voice_action_result_round_trips_thread_id() {
        let plain = VoiceActionResult::new("research", "ok", "an answer");
        assert_eq!(
            plain.thread_id, None,
            "a result defaults to no thread identity"
        );

        let threaded = plain.with_thread_id("t-123");
        assert_eq!(threaded.thread_id.as_deref(), Some("t-123"));
        let json = serde_json::to_value(&threaded).unwrap();
        assert_eq!(json.get("threadId").and_then(|v| v.as_str()), Some("t-123"));

        let null_json =
            serde_json::to_value(VoiceActionResult::new("research", "ok", "x")).unwrap();
        assert!(
            null_json.get("threadId").unwrap().is_null(),
            "no thread ⇒ threadId is null"
        );
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
        assert!(wants_calendar(
            "who's in my next meeting",
            "who's in my next meeting"
        ));
        assert!(wants_calendar("what's on my agenda", ""));
        assert!(wants_calendar("", "kto jest na spotkaniu"));
        assert!(wants_calendar("kalendarz na dziś", ""));
        // A plain research question does NOT fire it (no calendar noise on "weather").
        assert!(!wants_calendar(
            "what's the weather in Kraków",
            "jaka jest pogoda"
        ));
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
        assert!(is_empty_tool_result(
            "No calendar events match \"standup\"."
        ));
        assert!(is_empty_tool_result("No calendar events in the window."));
        // A real calendar block is NOT a sentinel.
        assert!(!is_empty_tool_result(
            "[calendar] Sprint Planning — Meeting: Sprint Planning"
        ));
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
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();

        let reasoner = MockReasoner::new("Pogoda była słoneczna; zobacz [[Notatka o pogodzie]].");
        let cfg = AppConfig::default();

        // WITH the literal Polish command → the Polish note is FOUND, brain is called, cited.
        let res = handle_voice_action(
            &VoiceIntent::Research {
                topic: "yesterday's weather".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &cfg,
            "live-mtg",
            "jaka była pogoda",
            "",
            false,
            None,
        );
        assert_eq!(res.status, "ok");
        assert!(
            res.citations
                .contains(&"[[Notatka o pogodzie]]".to_string()),
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
            &VoiceIntent::Research {
                topic: "yesterday's weather".into(),
            },
            &reasoner2,
            &db,
            &empty_unlocked(),
            &cfg,
            "live-mtg",
            "",
            "",
            false,
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

    /// A reasoner that records BOTH the system and user strings it was handed, so we can assert the
    /// synthesis prompt (system) and the grounding order (user). Echoes a fixed answer.
    struct CaptureReasoner {
        last_system: std::sync::Mutex<Option<String>>,
        last_user: std::sync::Mutex<Option<String>>,
    }
    impl CaptureReasoner {
        fn new() -> Self {
            Self {
                last_system: std::sync::Mutex::new(None),
                last_user: std::sync::Mutex::new(None),
            }
        }
    }
    impl LocalReasoner for CaptureReasoner {
        fn id(&self) -> &str {
            "capture"
        }
        fn reason(&self, system: &str, user: &str) -> crate::error::Result<String> {
            *self.last_system.lock().unwrap() = Some(system.to_string());
            *self.last_user.lock().unwrap() = Some(user.to_string());
            Ok("ok".to_string())
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

    /// (A) RED-before-GREEN (2026-07-08, local-brain English-answer bug): the deterministic floor's
    /// synthesis SYSTEM prompt MUST carry a same-language directive (Polish in → Polish out), mirroring
    /// the cloud cascade/agent path. RED on the pre-fix prompt (no language line at all).
    #[test]
    fn floor_synthesis_prompt_has_same_language_directive() {
        let db = tmp_db();
        seed_visible_and_sealed(&db); // non-empty vault grounding ⇒ the brain is actually called.
        let reasoner = CaptureReasoner::new();
        let res = handle_voice_action(
            &VoiceIntent::Research {
                topic: "Atlas".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live-mtg",
            "",
            "", // no current-meeting context on this leg — vault-only, tests the language line
            false,
            None,
        );
        assert_eq!(res.status, "ok");
        let system = reasoner
            .last_system
            .lock()
            .unwrap()
            .clone()
            .expect("the brain MUST be called with vault grounding present");
        assert!(
            system.contains("SAME language"),
            "synthesis prompt must instruct answering in the user's OWN language; got: {system}"
        );
        assert!(
            system.contains("Polish"),
            "the language directive must name the Polish→Polish case like the agent path; got: {system}"
        );
    }

    /// (B) RED-before-GREEN: given a CURRENT-meeting grounding string, it MUST appear in the reasoner's
    /// USER input labeled as THIS meeting AND BEFORE the vault notes (current-FIRST). Vault retrieval is
    /// PRESERVED — the vault notes are still present, just SECONDARY. RED on the pre-fix `rag_answer`
    /// (no current-meeting param, no "THIS MEETING" section).
    #[test]
    fn floor_prepends_current_meeting_before_vault_notes() {
        let db = tmp_db();
        seed_visible_and_sealed(&db); // "Atlas Kickoff" note is the VAULT (secondary) grounding.
        let reasoner = CaptureReasoner::new();
        let current = "Live transcript (so far):\nAlice: let's finalize the CURRENT-Q3-BUDGET today.";
        let res = handle_voice_action(
            &VoiceIntent::Research {
                topic: "Atlas".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live-mtg",
            "",
            current,
            false,
            None,
        );
        assert_eq!(res.status, "ok");
        let user = reasoner
            .last_user
            .lock()
            .unwrap()
            .clone()
            .expect("the brain MUST be called");
        let this_meeting_at = user
            .find("THIS MEETING")
            .expect("the current meeting must be labeled 'THIS MEETING' in the brain input");
        assert!(
            user.contains("CURRENT-Q3-BUDGET"),
            "the current-meeting transcript must reach the brain input: {user}"
        );
        // Vault retrieval is PRESERVED (current-FIRST, not vault-only): the vault note is still there…
        let vault_at = user
            .find("Atlas Kickoff")
            .expect("the vault note must STILL be present as secondary grounding");
        // …but AFTER the current meeting.
        assert!(
            this_meeting_at < vault_at,
            "the current meeting MUST come BEFORE the vault notes (current-first); user: {user}"
        );
        // The synthesis prompt must tell the brain the current meeting is the primary subject.
        let system = reasoner.last_system.lock().unwrap().clone().unwrap();
        assert!(
            system.contains("THIS MEETING"),
            "the system prompt must scope the answer to THIS meeting first; got: {system}"
        );
    }

    /// (B2) Byte-identical guard: an EMPTY current-meeting context yields NO "THIS MEETING" section —
    /// the vault-only floor behaves exactly as before. Guards the backward-compatibility promise for
    /// every existing caller (which passes "").
    #[test]
    fn floor_empty_current_context_is_vault_only_no_this_meeting_section() {
        let db = tmp_db();
        seed_visible_and_sealed(&db);
        let reasoner = CaptureReasoner::new();
        let res = handle_voice_action(
            &VoiceIntent::Research {
                topic: "Atlas".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live-mtg",
            "",
            "", // empty ⇒ no current-meeting section
            false,
            None,
        );
        assert_eq!(res.status, "ok");
        let user = reasoner.last_user.lock().unwrap().clone().unwrap();
        assert!(
            !user.contains("THIS MEETING"),
            "empty current context must NOT inject a THIS MEETING section: {user}"
        );
        // Vault grounding is still present and drives the answer.
        assert!(
            user.contains("Atlas Kickoff"),
            "the vault note must still be the grounding when no current context: {user}"
        );
    }

    /// (C) RED-before-GREEN (2026-07-09, recording-awareness back-port): the CORE SYMPTOM. During a
    /// LIVE recording whose buffer is still EMPTY (meeting just started), asking the LOCAL floor about
    /// "this meeting" must NOT let the vault grounding be described as if it were the current meeting.
    /// The synthesis SYSTEM prompt MUST say a meeting is being recorded RIGHT NOW, offer the honest
    /// "meeting just started" answer, and FORBID substituting other saved meetings — the SAME wording
    /// the cloud cascade already uses (shared via the `*_PHRASE` consts). RED on the pre-fix floor:
    /// with `recording_in_progress` absent the empty-buffer prompt carried no recording awareness at
    /// all, so the brain summarized unrelated vault meetings as "this meeting". Vault grounding is
    /// present here (seeded), so the early "nothing found" return does NOT fire — we reach the brain.
    #[test]
    fn floor_recording_empty_buffer_forbids_substituting_other_meetings() {
        let db = tmp_db();
        seed_visible_and_sealed(&db); // non-empty vault grounding ("Atlas Kickoff") ⇒ the brain runs.
        let reasoner = CaptureReasoner::new();
        let res = handle_voice_action(
            &VoiceIntent::Research {
                topic: "Atlas".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live-mtg",
            "",
            "", // EMPTY current context — the meeting just started
            true, // recording_in_progress
            None,
        );
        assert_eq!(res.status, "ok");
        let system = reasoner
            .last_system
            .lock()
            .unwrap()
            .clone()
            .expect("the brain MUST be called (vault grounding present)");
        assert!(
            system.contains(RECORDING_NOW_PHRASE),
            "must tell the model a meeting is being recorded now; got: {system}"
        );
        assert!(
            system.contains(MEETING_JUST_STARTED_PHRASE),
            "must offer the honest 'meeting just started' answer for an empty buffer; got: {system}"
        );
        assert!(
            system.contains(NO_SUBSTITUTE_OTHER_MEETINGS_PHRASE),
            "must forbid substituting other saved meetings for the current one; got: {system}"
        );
        // These exact substrings are the SAME ones the cloud cascade prompt test pins — the shared
        // consts guarantee the floor and cascade cannot drift.
        assert_eq!(RECORDING_NOW_PHRASE, "recorded RIGHT NOW");
        assert_eq!(MEETING_JUST_STARTED_PHRASE, "meeting just started");
        assert_eq!(
            NO_SUBSTITUTE_OTHER_MEETINGS_PHRASE,
            "do NOT search the vault for other saved meetings"
        );
    }

    /// (D) RED-before-GREEN: during a LIVE recording WITH transcribed content, the floor must frame the
    /// THIS MEETING section as the live recording ("recorded RIGHT NOW"), still current-first with the
    /// vault as secondary. RED on the pre-fix floor: the has_current clause never mentioned an active
    /// recording (it read like a viewed PAST meeting).
    #[test]
    fn floor_recording_with_content_frames_live_transcript_current_first() {
        let db = tmp_db();
        seed_visible_and_sealed(&db); // "Atlas Kickoff" = the SECONDARY vault grounding.
        let reasoner = CaptureReasoner::new();
        let current = "Live transcript (so far):\nAlice: let's finalize the CURRENT-Q3-BUDGET today.";
        let res = handle_voice_action(
            &VoiceIntent::Research {
                topic: "Atlas".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live-mtg",
            "",
            current,
            true, // recording_in_progress
            None,
        );
        assert_eq!(res.status, "ok");
        let system = reasoner.last_system.lock().unwrap().clone().unwrap();
        assert!(
            system.contains(RECORDING_NOW_PHRASE),
            "a live recording with content must be framed as recorded RIGHT NOW; got: {system}"
        );
        assert!(
            system.contains("THIS MEETING"),
            "still scopes the answer to THIS meeting first; got: {system}"
        );
        // Current-first ordering is PRESERVED: the current transcript reaches the brain BEFORE the vault.
        let user = reasoner.last_user.lock().unwrap().clone().unwrap();
        let this_at = user.find("THIS MEETING").expect("THIS MEETING labeled in user input");
        let vault_at = user
            .find("Atlas Kickoff")
            .expect("vault note still present as secondary grounding");
        assert!(
            this_at < vault_at,
            "the live meeting must come BEFORE the vault notes (current-first); user: {user}"
        );
    }

    /// (E) NO-REGRESSION: when NOT recording (viewed past meeting / idle / Ask page), the floor must
    /// make NO "recorded RIGHT NOW" claim — the language directive + current-first from the prior fix
    /// stay, but nothing frames the context as a live recording. Guards byte-compatibility for the
    /// `recording_in_progress=false` path that every legacy caller now passes.
    #[test]
    fn floor_not_recording_makes_no_live_recording_claim() {
        let db = tmp_db();
        seed_visible_and_sealed(&db);
        let reasoner = CaptureReasoner::new();
        // A viewed PAST meeting (has_current) but NOT recording.
        let viewed = "Live transcript (so far):\nWe reviewed last quarter's numbers.";
        let res = handle_voice_action(
            &VoiceIntent::Research {
                topic: "Atlas".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig::default(),
            "live-mtg",
            "",
            viewed,
            false, // NOT recording — viewed past meeting
            None,
        );
        assert_eq!(res.status, "ok");
        let system = reasoner.last_system.lock().unwrap().clone().unwrap();
        assert!(
            !system.contains(RECORDING_NOW_PHRASE),
            "no live-recording claim when not recording; got: {system}"
        );
        assert!(
            !system.contains(MEETING_JUST_STARTED_PHRASE),
            "no 'meeting just started' claim when not recording; got: {system}"
        );
        // The prior viewed-past-meeting behavior is intact: still THIS-MEETING-first + the language line.
        assert!(
            system.contains("THIS MEETING"),
            "viewed-meeting current-first framing preserved; got: {system}"
        );
        assert!(
            system.contains("SAME language"),
            "the same-language directive from the prior fix stays; got: {system}"
        );
    }

    // ── Tier-1 current-first classifier + isolated answer (2026-07-09 structural fix) ─────────────

    /// The current-meeting classifier fires on CLEAR "this meeting" questions (EN + PL, inflected),
    /// and does NOT fire on cross-meeting or world questions (so the fan-out stays for those).
    #[test]
    fn is_about_current_meeting_matches_this_meeting_questions() {
        // Polish "what is this meeting/recording/conversation about".
        for q in [
            "o czym jest to spotkanie",
            "o czym jest to spotkanie?",
            "o czym jest ta rozmowa",
            "o czym to nagranie",
            "o czym jest ta rozmowę", // inflected accusative — stem-matches
            "Claudku, o czym jest to spotkanie",
        ] {
            assert!(is_about_current_meeting(q), "should match PL about-this: {q}");
        }
        // Polish "summarize / streść THIS meeting/recording/conversation" — the summarize rule
        // requires the deictic AND a meeting-noun (a bare "streść to" with no meeting-noun is a
        // deliberate MISS → fan-out, so a cross-meeting "podsumuj moje spotkania" can't slip in).
        for q in [
            "podsumuj to spotkanie",
            "podsumuj to nagranie",
            "streść tę rozmowę",
        ] {
            assert!(is_about_current_meeting(q), "should match PL summarize-this: {q}");
        }
        // Polish "what did we (here) decide / discuss".
        for q in [
            "co tu ustaliliśmy",
            "co tutaj ustaliliśmy",
            "co tu omówiliśmy",
            "co tu zdecydowaliśmy",
        ] {
            assert!(is_about_current_meeting(q), "should match PL here-verb: {q}");
        }
        // English.
        for q in [
            "what is this meeting about",
            "what's this conversation about",
            "summarize this meeting",
            "recap this call",
            "what did we just discuss",
            "what did we decide here",
        ] {
            assert!(is_about_current_meeting(q), "should match EN about-this: {q}");
        }
    }

    /// Conservative: cross-meeting and world questions do NOT match, so they still fan out to the
    /// vault / web on the floor (no regression). This is the load-bearing negative case.
    #[test]
    fn is_about_current_meeting_ignores_cross_note_and_world_questions() {
        for q in [
            "co ustaliliśmy z Weroniką",           // cross-meeting (a person), NOT "here"
            "jaka pogoda",                          // world / web
            "jaka jest pogoda w Warszawie",         // world / web
            "moje otwarte zadania",                 // vault, not this-meeting
            "co wiemy o projekcie Atlas",           // cross-note recall
            "what's the weather",                   // world / web
            "who won the game yesterday",           // world / web
            "what did we decide with Weronika",     // cross-meeting (no deictic)
            // CROSS-MEETING SUMMARIZE (adversarial 2026-07-09 regression): a summarize verb + a
            // PLURAL/possessive-other meeting noun with NO "this/here" deictic must fan out to the
            // vault, NOT be stolen to the current-meeting isolation.
            "podsumuj moje spotkania",              // summarize MY meetings (plural, no deictic)
            "summarize my meetings",
            "podsumuj wszystkie spotkania",         // summarize ALL meetings
            "streść wszystkie moje rozmowy",        // summarize ALL my conversations
            "podsumuj spotkania z Weroniką",        // summarize meetings WITH a person
            "recap my last call with Bob",          // recap a call WITH a person
            "podsumuj nasze rozmowy z tego tygodnia", // summarize this week's conversations (plural)
            // DEICTIC-NOT-ADJACENT (adversarial 2026-07-09 round 2): "this" modifies a TIME word, not
            // the meeting-noun → cross-meeting/vault, must NOT be stolen to the current-meeting isolation.
            "summarize this week's meetings",       // "this" → "week's", meetings is plural/cross
            "summarize this month's meetings",
            "recap this week's calls",
            "podsumuj to co ustaliliśmy na spotkaniach", // relative "to co" + plural "na spotkaniach"
            "",                                     // empty
            "   ",                                  // whitespace
        ] {
            assert!(
                !is_about_current_meeting(q),
                "must NOT match (should fan out): {q:?}"
            );
        }
    }

    /// Tier-1 ISOLATION: given a current-meeting question WITH current content, the answer is
    /// synthesized from ONLY that content — the model's user message contains the current content and
    /// NOTHING vault/web, the system prompt has NO "THIS MEETING" label to parrot, and the citation is
    /// the current meeting's own [[Title]] (never a vault/web source).
    #[test]
    fn tier1_isolated_answer_uses_only_current_content_no_fanout_no_label() {
        let reasoner = CaptureReasoner::new();
        let current = "Transcript:\n[0s] We agreed to ship the connector on Friday.";
        let res = answer_current_meeting_isolated(
            "research",
            "o czym jest to spotkanie",
            current,
            /* recording */ true,
            Some("Connector Sync"),
            &reasoner,
        );
        assert_eq!(res.status, "ok");
        assert_eq!(res.answered_from, Some(AnsweredFrom::CurrentMeeting));
        // The citation is the CURRENT meeting's own title — not a vault/web source.
        assert_eq!(res.citations, vec!["[[Connector Sync]]".to_string()]);
        // The model saw ONLY the current content — nothing was fanned out.
        let user = reasoner.last_user.lock().unwrap().clone().unwrap();
        assert!(
            user.contains("We agreed to ship the connector on Friday"),
            "the current content must be handed to the model: {user}"
        );
        assert!(
            !user.contains("vault") && !user.contains("web") && !user.to_lowercase().contains("secondary"),
            "NO vault/web/secondary grounding in the isolated Tier-1 prompt: {user}"
        );
        // NO literal "THIS MEETING" label the weak model would echo as its opening words.
        let system = reasoner.last_system.lock().unwrap().clone().unwrap();
        assert!(
            !system.contains("THIS MEETING"),
            "the Tier-1 prompt must not hand the model a 'THIS MEETING' label: {system}"
        );
        // The same-language directive is preserved (Polish in → Polish out).
        assert!(
            system.contains("SAME language"),
            "the Tier-1 isolated prompt keeps the same-language directive: {system}"
        );
    }

    /// Tier-1 EMPTY: an about-current question with NO current content → an HONEST "just started / no
    /// content" answer, NO fan-out, NO web. The model is only asked to TRANSLATE a short honest
    /// sentence (it never sees vault/web/other-meeting content).
    #[test]
    fn tier1_empty_current_content_is_honest_and_never_fans_out() {
        // Recording, buffer empty → "just started" phrasing. Reasoner echoes; assert honest content.
        let reasoner = CaptureReasoner::new();
        let res = answer_current_meeting_isolated(
            "research",
            "o czym jest to spotkanie",
            "", // NO current content
            /* recording */ true,
            None,
            &reasoner,
        );
        assert_eq!(res.status, "ok");
        assert_eq!(res.answered_from, Some(AnsweredFrom::CurrentMeeting));
        let user = reasoner.last_user.lock().unwrap().clone().unwrap();
        // The ONLY thing handed to the model is an honest sentence to translate — no vault/web.
        assert!(
            user.contains("just started"),
            "recording+empty ⇒ 'just started' honest sentence: {user}"
        );
        assert!(
            !user.to_lowercase().contains("vault") && !user.to_lowercase().contains("[["),
            "empty Tier-1 must NOT fan out to the vault: {user}"
        );

        // A STUB reasoner (no brain) ⇒ the honest English notice verbatim, still no fan-out.
        let stub = crate::reason::StubReasoner;
        let res2 = answer_current_meeting_isolated(
            "research",
            "summarize this meeting",
            "",
            /* recording */ false, // viewed past meeting with no content
            None,
            &stub,
        );
        assert_eq!(res2.status, "ok");
        assert!(
            res2.summary.contains("no transcript or notes"),
            "viewed+empty ⇒ 'no transcript or notes' honest notice: {}",
            res2.summary
        );
        assert!(res2.citations.is_empty(), "no citations when there is no content");
    }

    /// Tier-1 with a STUB reasoner but PRESENT content ⇒ honest no-model notice + KEEP the current
    /// meeting's own citation, still NO fan-out.
    #[test]
    fn tier1_stub_with_content_returns_no_model_notice_and_own_citation() {
        let stub = crate::reason::StubReasoner;
        let res = answer_current_meeting_isolated(
            "research",
            "o czym to spotkanie",
            "Transcript:\n[0s] Budget approved.",
            true,
            Some("Budget Review"),
            &stub,
        );
        assert_eq!(res.status, "unavailable");
        assert_eq!(res.summary, NO_MODEL_ANSWER_NOTICE);
        assert_eq!(res.citations, vec!["[[Budget Review]]".to_string()]);
        assert_eq!(res.answered_from, Some(AnsweredFrom::CurrentMeeting));
    }

    // ── A1 — deterministic floor ORG LEG (Shared Brain) ─────────────────────────────────────────

    /// Join org-1 for this session (mirrors `tools::tests::seed_org`).
    fn seed_org(db: &Db) {
        db.upsert_org_state(&crate::storage::OrgState {
            org_id: "org-1".to_string(),
            name: "Acme".to_string(),
            role: "member".to_string(),
            joined_at: "2026-07-10T00:00:00Z".to_string(),
            consented: true,
            last_seq: 0,
            generation: 1,
            context_enabled: true,
        })
        .unwrap();
    }

    /// Ingest one org-brain item into the local replica (mirrors `tools::tests::ingest_org`).
    fn ingest_org(db: &Db, item_id: &str, author: &str, title: &str, body: &str, sha: &[u8]) {
        db.upsert_org_item(
            item_id,
            "org-1",
            1,
            author,
            title,
            body,
            "2026-07-10T09:00:00Z",
            1,
            1,
            sha,
            None,
            Some(&crate::embed::StubEmbedder),
        )
        .unwrap();
    }

    /// RED-before-GREEN (A1, the reported bug): the deterministic floor must fold ORG BRAIN results
    /// into its grounding — an org item with NO vault match must still reach the brain input,
    /// attributed with the `[org · author]` provenance line, and surface as a citation. Pre-fix,
    /// `rag_answer` had no org leg at all, so a colleague's shared note (org-only, nothing in the
    /// user's own vault) was silently omitted.
    #[test]
    fn floor_folds_org_brain_results_into_grounding_with_provenance() {
        let db = tmp_db();
        seed_org(&db);
        // Semantic off so the test is embedder-independent (FTS leg only, mirrors tools.rs org tests).
        ingest_org(
            &db,
            "it-1",
            "anna",
            "Anna's roadmap",
            "the apollo migration ships friday",
            &[3u8; 32],
        );
        let reasoner = CaptureReasoner::new();
        let res = handle_voice_action(
            &VoiceIntent::Research {
                topic: "apollo migration".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig {
                semantic_search_enabled: false,
                ..AppConfig::default()
            },
            "live-mtg",
            "",
            "",
            false,
            None,
        );
        assert_eq!(res.status, "ok");
        let user = reasoner
            .last_user
            .lock()
            .unwrap()
            .clone()
            .expect("the brain MUST be called with org grounding present");
        assert!(
            user.contains("[org · anna]") && user.contains("Anna's roadmap"),
            "org-only content (no vault match) must reach the brain input, attributed: {user}"
        );
        assert!(
            res.citations.iter().any(|c| c.contains("(org · anna)")),
            "the org hit must surface as a distinct '(org · author)' citation: {:?}",
            res.citations
        );
        // The system prompt must also name org notes as a third context class.
        let system = reasoner.last_system.lock().unwrap().clone().unwrap();
        assert!(
            system.contains("[org ·") || system.contains("org"),
            "the system prompt must mention org-sourced context: {system}"
        );
    }

    /// RED companion — recall must ALSO get the org leg (not just research, unlike the web leg).
    #[test]
    fn floor_folds_org_brain_results_for_recall_intent_too() {
        let db = tmp_db();
        seed_org(&db);
        ingest_org(
            &db,
            "it-2",
            "bob",
            "Bob's plan",
            "the siema onboarding checklist",
            &[7u8; 32],
        );
        let reasoner = CaptureReasoner::new();
        let res = handle_voice_action(
            &VoiceIntent::Recall {
                entity: "siema onboarding".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig {
                semantic_search_enabled: false,
                ..AppConfig::default()
            },
            "live-mtg",
            "",
            "",
            false,
            None,
        );
        assert_eq!(res.status, "ok");
        let user = reasoner.last_user.lock().unwrap().clone().unwrap();
        assert!(
            user.contains("[org · bob]") && user.contains("Bob's plan"),
            "recall must ALSO fold in org grounding: {user}"
        );
    }

    /// Fail-closed companion (mirrors `tools::tests::search_org_brain_seam_fails_closed_for_a_non_member`):
    /// a caller who has NOT joined any org (`org_brain_available` false) never gets an org leg
    /// attempted — the deterministic floor stays vault/web/calendar-only, byte-shape unchanged.
    #[test]
    fn floor_never_attempts_org_leg_for_a_non_member() {
        let db = tmp_db();
        seed_visible_and_sealed(&db); // vault grounding present, but NO org_state joined.
        let reasoner = CaptureReasoner::new();
        let res = handle_voice_action(
            &VoiceIntent::Research {
                topic: "Atlas".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig {
                semantic_search_enabled: false,
                ..AppConfig::default()
            },
            "live-mtg",
            "",
            "",
            false,
            None,
        );
        assert_eq!(res.status, "ok");
        let user = reasoner.last_user.lock().unwrap().clone().unwrap();
        assert!(
            !user.contains("[org ·") && !user.contains("org-brain"),
            "a non-member must NEVER get an org leg, even attempted: {user}"
        );
        assert!(
            res.citations.iter().all(|c| !c.contains("(org")),
            "a non-member must never surface an org citation: {:?}",
            res.citations
        );
    }

    /// PER-INSTANCE ORG TOGGLE, end-to-end through the deterministic floor (RED-before-GREEN, the
    /// user's hard mandate — this is A1's original bug scenario, now with the toggle): a member of
    /// TWO orgs who has disabled ONE of them on this install must NEVER see that org's content reach
    /// the brain grounding — even though it matches the query and would surface if enabled. The
    /// STILL-enabled org's content must keep working normally. Proves "disabled means truly gone",
    /// not just deprioritized, at the actual consumer surface a real user hits.
    #[test]
    fn floor_never_folds_a_disabled_orgs_content_into_grounding_while_the_enabled_org_still_works() {
        let db = tmp_db();
        seed_org(&db); // org-1, enabled by default
        db.upsert_org_state(&crate::storage::OrgState {
            org_id: "org-2".to_string(),
            name: "Beta".to_string(),
            role: "member".to_string(),
            joined_at: "2026-07-11T00:00:00Z".to_string(),
            consented: true,
            last_seq: 0,
            generation: 1,
            context_enabled: true,
        })
        .unwrap();
        ingest_org(
            &db,
            "it-disabled",
            "anna",
            "Disabled org roadmap",
            "the horizon launch plan for the beta release",
            &[31u8; 32],
        );
        db.upsert_org_item(
            "it-enabled",
            "org-2",
            1,
            "carol",
            "Enabled org roadmap",
            "the horizon launch timeline for the beta release",
            "2026-07-10T09:00:00Z",
            1,
            1,
            &[32u8; 32],
            None,
            Some(&crate::embed::StubEmbedder),
        )
        .unwrap();
        db.set_org_context_enabled("org-1", false).unwrap();

        let reasoner = CaptureReasoner::new();
        let res = handle_voice_action(
            &VoiceIntent::Research {
                topic: "horizon launch".into(),
            },
            &reasoner,
            &db,
            &empty_unlocked(),
            &AppConfig {
                semantic_search_enabled: false,
                ..AppConfig::default()
            },
            "live-mtg",
            "",
            "",
            false,
            None,
        );
        assert_eq!(res.status, "ok");
        let user = reasoner.last_user.lock().unwrap().clone().unwrap();
        assert!(
            !user.contains("Disabled org roadmap") && !user.contains("[org · anna]"),
            "the disabled org's content must NEVER reach the brain input: {user}"
        );
        assert!(
            user.contains("[org · carol]") && user.contains("Enabled org roadmap"),
            "the still-enabled org's content must keep reaching the brain input: {user}"
        );
        assert!(
            res.citations.iter().all(|c| !c.contains("anna")),
            "the disabled org's author must never surface as a citation: {:?}",
            res.citations
        );
        assert!(
            res.citations.iter().any(|c| c.contains("(org · carol)")),
            "the enabled org's citation must still surface: {:?}",
            res.citations
        );
    }
}
