//! Cross-meeting USER MEMORY (Phase 3) — a durable, user-scoped "what the brain knows about you"
//! layer that persists facts/preferences/commitments across ALL meetings and injects an auditable
//! brief into the agentic loop. It REUSES the bitemporal facts substrate (`crate::facts`) — the same
//! valid_from/valid_to, invalidate-not-delete, deterministic reconcile — but scoped to the USER
//! rather than a graph entity.
//!
//! ## Why a separate table, not `facts` rows
//! Entity facts are keyed on `(entity_id, subject, predicate)` and carry an FK to `entities`. User
//! facts are about "me" — there is no entity — so they live in a PARALLEL `user_facts` table with no
//! entity FK. Reusing the pure `crate::facts::reconcile_facts` needs SOME key in the `entity_id`
//! slot, so every user-fact op/row carries the fixed [`USER_SCOPE`] sentinel there — a reconcile key
//! ONLY, never a persisted column. Keeping the tables separate means the entity-graph reads
//! (`list_facts_visible`, MCP dossier) can NEVER surface a user fact and vice-versa.
//!
//! ## Lock model (see `.claude/rules/lock-model.md`, design spec D3)
//! A user fact carries the SOURCE `meeting_id` it was derived from (the gating + purge anchor). Like
//! `facts` / `note_chunks` / `correction_log`, user facts are DERIVED content: PURGED on seal in the
//! same atomic tx (`Db::purge_user_facts_tx`) and every USER-FACING read is visibility-gated
//! (`Db::list_user_facts_visible`). The memory brief is DERIVED data — never sealed, always
//! REGENERATED from the currently-VISIBLE user facts — so a sealed-and-not-session-unlocked meeting's
//! user facts surface NOTHING in the audit view AND are injected into NO prompt.
//!
//! ## Determinism
//! [`synthesize_brief`] is a PURE function (no LLM, no DB, no clock) — the headless-testable heart.
//! The only non-deterministic part is [`extract_user_fact_candidates`], which is BEST-EFFORT: it
//! asks the on-device reasoner for user·predicate·object preferences and degrades to an EMPTY result
//! (never an error, never a panic, never a block) when the brain/model is unavailable.

use serde::{Deserialize, Serialize};

use crate::facts::{Fact, FactCandidate};
use crate::reason::LocalReasoner;

/// The reserved `entity_id` sentinel that scopes a fact to the USER (not a graph entity). It is a
/// reconcile-key placeholder ONLY — it is never written to the `user_facts` table (which has no
/// entity column) and never surfaces to the FE. Chosen to be impossible as a real UUID entity id.
pub const USER_SCOPE: &str = "__user__";

/// Max chars of the synthesized memory brief injected into the agentic system prompt. Small on
/// purpose (design spec D2): the brief is always-injected, so it must stay a tight budget.
pub const MEMORY_BRIEF_MAX_CHARS: usize = 2_000;

/// Max user facts fed into a single brief (belt-and-braces bound alongside the char budget).
const MAX_BRIEF_FACTS: usize = 40;

/// Max note chars fed to the user-fact extractor (bounds the prompt / leak surface, like facts.rs).
const EXTRACT_EXCERPT_CHARS: usize = 8_000;

/// Max chars of the meeting's @brain THREAD TURNS fed to the extractor. Bounded like the note/notes
/// excerpts (design spec D5): thread turns are the HIGHEST-signal source (an explicit "zapamiętaj,
/// że…"), but the section must still stay a tight, leak-bounded budget.
const THREAD_TURNS_MAX_CHARS: usize = 4_000;

/// One current user-memory fact, as surfaced to the FE audit view. Content-bearing (subject /
/// predicate / object) BUT only ever built from VISIBLE facts (`Db::list_user_facts_visible`), so a
/// sealed source never reaches here. `sourceMeetingId` is the provenance link (the audit "where did
/// the brain learn this" affordance).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UserMemoryFact {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    /// Valid-time origin (the source meeting's time) — when the brain learned it was true.
    pub valid_from: String,
    /// The meeting this was derived from (the provenance link + purge anchor). Always `Some` for a
    /// visible fact (the gated reader drops NULL-source rows fail-closed).
    pub source_meeting_id: Option<String>,
    pub confidence: f64,
}

impl UserMemoryFact {
    /// Project a persisted [`Fact`] (already visibility-filtered) into the FE audit DTO.
    pub fn from_fact(f: &Fact) -> Self {
        Self {
            id: f.id.clone(),
            subject: f.subject.clone(),
            predicate: f.predicate.clone(),
            object: f.object.clone(),
            valid_from: f.valid_from.clone(),
            source_meeting_id: f.meeting_id.clone(),
            confidence: f.confidence,
        }
    }
}

/// The full audit payload for the Brain-page Memory section: the current (visible, open) user facts
/// plus the synthesized brief that is injected into grounding. Both are derived from EXACTLY the same
/// visible-facts set, so the audit view is a faithful mirror of what the brain actually injects.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UserMemory {
    pub facts: Vec<UserMemoryFact>,
    /// The injected brief (same text the agentic loop receives), or empty when memory is empty.
    pub brief: String,
    /// TRUE when cross-meeting memory is turned OFF entirely (config `user_memory_enabled == false`).
    /// In that state `facts`/`brief` are EMPTY and NOTHING is injected into any prompt — the FE shows
    /// a "memory is off" affordance rather than an empty list. Default FALSE (memory on) so a payload
    /// that omits it (older FE) reads as enabled.
    #[serde(default)]
    pub disabled: bool,
}

impl UserMemory {
    /// The explicit "memory is turned OFF" payload: empty facts, empty brief, `disabled: true`. Used
    /// by `get_user_memory` when the config gate is off so the FE can render a distinct "memory is
    /// off" affordance instead of an "empty memory" one — and NOTHING content-bearing is surfaced.
    pub fn disabled() -> Self {
        Self {
            facts: Vec::new(),
            brief: String::new(),
            disabled: true,
        }
    }
}

/// DETERMINISTIC brief synthesis (no LLM, no DB, no clock) — the headless-testable core. Assemble the
/// currently-valid `facts` (already visibility-filtered by the caller) into a compact markdown brief:
/// one `- <subject> <predicate>: <object>` bullet per fact, newest valid_from first (the caller's
/// order is preserved), bounded by [`MAX_BRIEF_FACTS`] and hard-truncated to [`MEMORY_BRIEF_MAX_CHARS`]
/// on a char boundary. EMPTY facts ⇒ EMPTY string (no header) so injection is a pure no-op when there
/// is no memory. NEVER panics; skips malformed (empty subject/predicate/object) rows defensively.
pub fn synthesize_brief(facts: &[Fact]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for f in facts.iter().take(MAX_BRIEF_FACTS) {
        let subject = f.subject.trim();
        let predicate = f.predicate.trim();
        let object = f.object.trim();
        if subject.is_empty() || predicate.is_empty() || object.is_empty() {
            continue; // never emit a junk bullet.
        }
        lines.push(format!("- {subject} {predicate}: {object}"));
    }
    if lines.is_empty() {
        return String::new();
    }
    let body = lines.join("\n");
    // Hard char budget (on a char boundary) — the brief is always-injected.
    let brief: String = body.chars().take(MEMORY_BRIEF_MAX_CHARS).collect();
    brief
}

/// Brain v2 L2.2 — max user facts fed into a RELEVANCE-FILTERED brief (the FTS top-k). Smaller than
/// [`MAX_BRIEF_FACTS`] on purpose: when a query is in hand, a tight, on-topic brief beats breadth.
pub const RELEVANT_BRIEF_FACTS: usize = 8;

/// Brain v2 L2.2 — the RELEVANCE-FILTERED memory brief: when a user `query` is in hand, rank the
/// visible open user facts by BM25 against it (`Db::search_user_facts_visible` — the SAME
/// visibility predicate as `list_user_facts_visible`, so a sealed-not-unlocked meeting's facts are
/// NEVER read) and synthesize the brief from the top [`RELEVANT_BRIEF_FACTS`] hits.
///
/// BEHAVIOR-PRESERVING FALLBACK: an empty/punctuation-only query (the note-gen path), an FTS error,
/// or ZERO hits all fall back to today's full-list `list_user_facts_visible` + [`synthesize_brief`]
/// — so the brief is never emptier than the pre-L2.2 one just because relevance filtering found
/// nothing. [`MEMORY_BRIEF_MAX_CHARS`] applies unchanged on both paths. Best-effort like the
/// existing brief call sites: a DB read error degrades to an empty brief, never a failure. The
/// CALLER still owns the `user_memory_enabled` flag check (this fn only assembles).
pub fn build_memory_brief(
    db: &crate::storage::Db,
    query: &str,
    unlocked: &std::collections::HashSet<String>,
) -> String {
    if !query.trim().is_empty() {
        if let Ok(hits) = db.search_user_facts_visible(query, RELEVANT_BRIEF_FACTS, unlocked) {
            if !hits.is_empty() {
                return synthesize_brief(&hits);
            }
        }
    }
    let facts = db.list_user_facts_visible(unlocked).unwrap_or_default();
    synthesize_brief(&facts)
}

/// The shape the reasoner must emit. Best-effort: parse failures degrade to no facts.
#[derive(Debug, Deserialize)]
struct UserFactsReply {
    #[serde(default)]
    facts: Vec<RawUserFact>,
}

#[derive(Debug, Deserialize)]
struct RawUserFact {
    #[serde(default)]
    predicate: String,
    #[serde(default)]
    object: String,
}

const EXTRACT_SYSTEM: &str = "You extract durable facts, preferences and commitments ABOUT THE \
USER (the person whose notes these are — first person \"I / me / my\") from a meeting note, the \
user's own typed notes, and the user's own messages to the brain assistant. Output STRICT JSON \
ONLY (no prose, no code fences): {\"facts\":[{\"predicate\":\"short attribute\",\"object\":\"value\"}]}.\n\
- Extract ONLY durable, user-scoped state worth remembering across meetings: the user's own \
preferences (\"prefers replies in Polish\"), ongoing work (\"works on Project Atlas\"), commitments \
and recurring context (\"deadline Q3 = 2026-09-15\"). Prefer things the user explicitly asks to \
remember (\"remember that…\", \"zapamiętaj, że…\") — an explicit ask in a THREAD TURN is the \
highest-signal source.\n\
- predicate is a short, stable attribute (e.g. \"prefers\", \"works on\", \"role\", \"deadline\").\n\
- object is the current value (e.g. \"Polish replies\", \"Project Atlas\", \"2026-09-15\").\n\
- Do NOT extract facts about OTHER people or projects (those are entity facts, handled elsewhere), \
one-off remarks, or anything not clearly about the USER. Precision over recall — a wrong memory is \
worse than a missing one. Empty array if none.\n\
Output ONLY the JSON.";

/// The stable subject stored on every user fact (the memory is about "you"). A single constant
/// subject keeps the reconcile key `(USER_SCOPE, subject, predicate)` effectively
/// `(predicate)`-keyed — so a later note that changes the user's answer to the SAME predicate
/// supersedes it, exactly like entity facts.
const USER_SUBJECT: &str = "You";

/// PURE + headless-testable assembly of the extraction USER prompt from the meeting's title, note
/// markdown, the user's own typed notes, and the meeting's own @brain THREAD TURNS (design spec D5 —
/// an explicit "zapamiętaj, że…" in a thread is the highest-signal source). Each free-text source is
/// bounded (note/notes → [`EXTRACT_EXCERPT_CHARS`], thread turns → [`THREAD_TURNS_MAX_CHARS`]) and an
/// empty source is omitted entirely so an all-empty payload stays minimal. Split out so the
/// prompt-assembly + the source ordering can be unit-tested without a live model.
fn build_extraction_user_prompt(
    title: &str,
    note_markdown: &str,
    typed_notes: &str,
    thread_turns: &str,
) -> String {
    let excerpt: String = note_markdown.chars().take(EXTRACT_EXCERPT_CHARS).collect();
    let notes_excerpt: String = typed_notes.chars().take(EXTRACT_EXCERPT_CHARS).collect();
    let thread_excerpt: String = thread_turns.chars().take(THREAD_TURNS_MAX_CHARS).collect();
    let mut user = format!("MEETING: {title}");
    // THREAD TURNS first after the title: the highest-signal, most explicit source.
    if !thread_excerpt.trim().is_empty() {
        user.push_str(&format!(
            "\n\nUSER'S OWN MESSAGES TO THE BRAIN (THREAD TURNS):\n{thread_excerpt}"
        ));
    }
    if !notes_excerpt.trim().is_empty() {
        user.push_str(&format!("\n\nUSER'S OWN TYPED NOTES:\n{notes_excerpt}"));
    }
    user.push_str(&format!("\n\nNOTE:\n{excerpt}"));
    user
}

/// BEST-EFFORT extraction of user-scoped fact candidates from a meeting's note markdown, the user's
/// own typed notes, and the meeting's own @brain THREAD TURNS (design spec D5 — the highest-signal
/// source). Uses the on-device reasoner's `structured` decode; on ANY failure (stub reasoner / no
/// model / decode error / parse error) returns an EMPTY vec — never an error, never a panic, never a
/// block beyond the reasoner call itself. The RECONCILE is the load-bearing deterministic core; this
/// is the soft front-end that feeds it. Every candidate carries the [`USER_SCOPE`] sentinel + the
/// [`USER_SUBJECT`] so `reconcile_facts` keys them on the predicate.
pub fn extract_user_fact_candidates(
    reasoner: &dyn LocalReasoner,
    title: &str,
    note_markdown: &str,
    typed_notes: &str,
    thread_turns: &str,
) -> Vec<FactCandidate> {
    // No real brain (the default build / no model) → no extraction. The deterministic reconcile +
    // synthesis are still exercised on whatever candidates a real brain would produce.
    if reasoner.id() == "stub" {
        return Vec::new();
    }
    let user = build_extraction_user_prompt(title, note_markdown, typed_notes, thread_turns);
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "facts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "predicate": { "type": "string" },
                        "object": { "type": "string" }
                    },
                    "required": ["predicate", "object"]
                }
            }
        },
        "required": ["facts"]
    });

    let value = match reasoner.structured(EXTRACT_SYSTEM, &user, &schema) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(target: "user_memory", error = %e, "user-fact extraction failed; no candidates (best-effort)");
            return Vec::new();
        }
    };
    let reply: UserFactsReply = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(target: "user_memory", error = %e, "user-fact extraction reply unparseable; no candidates");
            return Vec::new();
        }
    };
    candidates_from_raw(reply.facts)
}

/// Brain v2 L2.3 — max chars of a pasted memory export fed to the import extractor (bounds the
/// prompt / leak surface exactly like the note excerpt).
const IMPORT_EXCERPT_CHARS: usize = 8_000;

/// PURE + headless-testable assembly of the MEMORY-IMPORT user prompt (Brain v2 L2.3): the pasted
/// text is labelled as pre-extracted memories from ANOTHER AI assistant (ChatGPT/Claude memory
/// export), hard-bounded to [`IMPORT_EXCERPT_CHARS`]. Split out so the labelling + the bound can be
/// unit-tested without a live model.
fn build_import_user_prompt(text: &str) -> String {
    let excerpt: String = text.chars().take(IMPORT_EXCERPT_CHARS).collect();
    format!(
        "PRE-EXTRACTED MEMORIES FROM ANOTHER AI ASSISTANT (the user pasted their memory export; \
         each line is already a durable fact/preference about the user — convert them, do not \
         re-summarize):\n{excerpt}"
    )
}

/// Brain v2 L2.3 — BEST-EFFORT extraction of user-scoped fact candidates from a PASTED memory
/// export (ChatGPT/Claude "what the assistant remembers" text). Reuses the [`EXTRACT_SYSTEM`]
/// machinery (same schema, same [`candidates_from_raw`] scoping) with an import-specific user
/// prompt. Same degradation contract as [`extract_user_fact_candidates`]: the stub reasoner / any
/// decode or parse failure ⇒ an EMPTY vec — never an error, never a panic. ZERO egress: the
/// command resolves the LIGHT local-or-stub reasoner (`import_extraction_reasoner` in commands.rs
/// — NEVER cloud; the FE copy promises on-device, and a pasted third-party memory export must not
/// ride the cloud Notes provider). No local model ⇒ nothing extracted ⇒ 0 imported.
pub fn extract_imported_memories(reasoner: &dyn LocalReasoner, text: &str) -> Vec<FactCandidate> {
    if reasoner.id() == "stub" {
        return Vec::new();
    }
    let user = build_import_user_prompt(text);
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "facts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "predicate": { "type": "string" },
                        "object": { "type": "string" }
                    },
                    "required": ["predicate", "object"]
                }
            }
        },
        "required": ["facts"]
    });
    let value = match reasoner.structured(EXTRACT_SYSTEM, &user, &schema) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(target: "user_memory", error = %e, "memory import extraction failed; no candidates (best-effort)");
            return Vec::new();
        }
    };
    let reply: UserFactsReply = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(target: "user_memory", error = %e, "memory import reply unparseable; no candidates");
            return Vec::new();
        }
    };
    candidates_from_raw(reply.facts)
}

/// Map raw extracted (predicate, object) pairs to [`FactCandidate`]s scoped to the user. Pure +
/// headless-testable (no reasoner needed). Drops empty predicate/object pairs (never invent).
fn candidates_from_raw(raw: Vec<RawUserFact>) -> Vec<FactCandidate> {
    let mut out = Vec::new();
    for r in raw {
        let predicate = r.predicate.trim();
        let object = r.object.trim();
        if predicate.is_empty() || object.is_empty() {
            continue;
        }
        out.push(FactCandidate {
            entity_id: USER_SCOPE.to_string(),
            subject: USER_SUBJECT.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            confidence: 1.0,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ufact(subject: &str, predicate: &str, object: &str) -> Fact {
        Fact {
            id: format!("uf-{predicate}-{object}"),
            entity_id: USER_SCOPE.to_string(),
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            valid_from: "2026-07-01T00:00:00Z".to_string(),
            valid_to: None,
            recorded_at: "2026-07-01T00:00:00Z".to_string(),
            meeting_id: Some("m1".to_string()),
            confidence: 1.0,
        }
    }

    /// EMPTY memory ⇒ EMPTY brief (no header) — so injection is a byte-for-byte no-op when the user
    /// has no memory yet. This is the "empty when memory is empty" contract.
    #[test]
    fn synthesize_brief_is_empty_when_no_facts() {
        assert_eq!(synthesize_brief(&[]), "");
    }

    /// A present fact becomes a bullet with subject/predicate/object; order is the caller's order.
    #[test]
    fn synthesize_brief_lists_facts_as_bullets() {
        let facts = vec![
            ufact("You", "prefer", "Polish replies"),
            ufact("You", "work on", "Project Atlas"),
        ];
        let brief = synthesize_brief(&facts);
        assert!(brief.contains("- You prefer: Polish replies"));
        assert!(brief.contains("- You work on: Project Atlas"));
    }

    /// Malformed (empty field) facts never emit a junk bullet; if ALL are junk the brief is empty.
    #[test]
    fn synthesize_brief_skips_malformed_and_can_be_empty() {
        let facts = vec![ufact("You", "", "x"), ufact("You", "prefer", "  ")];
        assert_eq!(synthesize_brief(&facts), "");
    }

    /// The brief is hard-bounded to the char budget (always-injected → must stay small).
    #[test]
    fn synthesize_brief_enforces_char_budget() {
        let big_object = "x".repeat(MEMORY_BRIEF_MAX_CHARS * 2);
        let facts = vec![ufact("You", "note", &big_object)];
        let brief = synthesize_brief(&facts);
        assert!(brief.chars().count() <= MEMORY_BRIEF_MAX_CHARS);
    }

    /// The audit DTO carries the provenance link (source meeting id) so "where did the brain learn
    /// this" is answerable in the UI.
    #[test]
    fn user_memory_fact_carries_provenance() {
        let f = ufact("You", "prefer", "Polish replies");
        let dto = UserMemoryFact::from_fact(&f);
        assert_eq!(dto.source_meeting_id.as_deref(), Some("m1"));
        assert_eq!(dto.predicate, "prefer");
        assert_eq!(dto.object, "Polish replies");
    }

    /// D5 — the meeting's own @brain THREAD TURNS are appended to the extraction prompt as a
    /// dedicated, clearly-labelled section (the highest-signal source). This binds the seam a real
    /// model would see: an explicit "zapamiętaj że wolę odpowiedzi po polsku" reaches the extractor.
    /// RED before D5: `build_extraction_user_prompt` did not take/emit thread turns at all.
    #[test]
    fn extraction_prompt_includes_thread_turns_section() {
        let prompt = build_extraction_user_prompt(
            "Sync",
            "note body",
            "",
            "User: zapamiętaj że wolę odpowiedzi po polsku",
        );
        assert!(prompt.contains("THREAD TURNS"));
        assert!(prompt.contains("zapamiętaj że wolę odpowiedzi po polsku"));
        // The note body is still present (thread turns AUGMENT, never replace, the note source).
        assert!(prompt.contains("note body"));
    }

    /// Empty thread turns ⇒ NO thread-turns section at all (an all-note payload stays minimal, and
    /// the extractor never sees a bare empty header). Note is always present.
    #[test]
    fn extraction_prompt_omits_empty_thread_turns() {
        let prompt = build_extraction_user_prompt("Sync", "note body", "", "   ");
        assert!(!prompt.contains("THREAD TURNS"));
        assert!(prompt.contains("note body"));
    }

    /// The thread-turns section is hard-bounded to its char budget (bounds prompt / leak surface).
    #[test]
    fn extraction_prompt_bounds_thread_turns() {
        let huge = "z".repeat(THREAD_TURNS_MAX_CHARS * 3);
        let prompt = build_extraction_user_prompt("Sync", "n", "", &huge);
        let z_count = prompt.chars().filter(|c| *c == 'z').count();
        assert!(z_count <= THREAD_TURNS_MAX_CHARS);
    }

    /// Brain v2 L2.2 (RED-first): with a QUERY in hand the brief is RELEVANCE-FILTERED — only the
    /// BM25-matching fact appears; an EMPTY query falls back to the full list (behavior-preserving).
    /// Zero FTS hits also fall back to the full list (the brief is never emptier than before).
    /// RED before L2.2: `build_memory_brief`/`search_user_facts_visible` did not exist and every
    /// surface injected the unfiltered full-list brief.
    #[test]
    fn build_memory_brief_filters_by_query_and_falls_back() {
        use crate::facts::{FactOp, NewFact};
        use crate::storage::models::{Meeting, MeetingStatus};
        use crate::storage::Db;

        let path = crate::storage::db::unique_temp_path("murmur-brief-filter", "sqlite");
        let db = Db::open_with_key(
            &path,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        db.insert_meeting(&Meeting {
            id: "m1".to_string(),
            started_at: "2026-07-01T09:00:00Z".to_string(),
            ended_at: None,
            title: Some("Sync".to_string()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        let add = |predicate: &str, object: &str| {
            FactOp::Add(NewFact {
                entity_id: USER_SCOPE.to_string(),
                subject: "You".to_string(),
                predicate: predicate.to_string(),
                object: object.to_string(),
                valid_from: "2026-07-01T09:00:00Z".to_string(),
                recorded_at: "2026-07-01T09:00:00Z".to_string(),
                confidence: 1.0,
                meeting_id: Some("m1".to_string()),
            })
        };
        db.apply_user_fact_ops(&[
            add("prefer", "Polish replies"),
            add("works on", "Project Atlas"),
        ])
        .unwrap();
        let unlocked = std::collections::HashSet::new();

        // Query matching ONE fact → the brief contains ONLY that fact.
        let brief = build_memory_brief(&db, "what is the deadline for Atlas?", &unlocked);
        assert!(
            brief.contains("Project Atlas"),
            "matching fact present: {brief}"
        );
        assert!(
            !brief.contains("Polish replies"),
            "non-matching fact filtered out: {brief}"
        );

        // Empty query → the full-list fallback (both facts, exactly the pre-L2.2 brief).
        let brief_all = build_memory_brief(&db, "", &unlocked);
        assert!(brief_all.contains("Project Atlas"));
        assert!(brief_all.contains("Polish replies"));

        // Zero hits → the full-list fallback too (never emptier than before).
        let brief_nohit = build_memory_brief(&db, "qqqzzz nonexistent", &unlocked);
        assert!(brief_nohit.contains("Project Atlas"));
        assert!(brief_nohit.contains("Polish replies"));
    }

    /// FIX 3 RED (adversarial finding 2, reproduced): a query sharing ONLY STOPWORDS with an
    /// irrelevant fact must NOT displace the full-list fallback. Facts {the darker theme / Polish
    /// natively}; the question "what language should the assistant use?" shares only "the" with the
    /// theme fact — on the pre-fix code the OR-joined FTS returned the theme fact as the whole
    /// "relevant" set, so the brief contained ONLY the theme fact and the actually-relevant
    /// language fact was DISPLACED. Post-fix: stopwords/<3-char tokens are dropped before the OR,
    /// no content term matches, and the brief falls back to the FULL list — the language fact is
    /// always present.
    #[test]
    fn stopword_only_overlap_never_displaces_the_full_list_fallback() {
        use crate::facts::{FactOp, NewFact};
        use crate::storage::models::{Meeting, MeetingStatus};
        use crate::storage::Db;

        let path = crate::storage::db::unique_temp_path("murmur-brief-stopword", "sqlite");
        let db = Db::open_with_key(
            &path,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        db.insert_meeting(&Meeting {
            id: "m1".to_string(),
            started_at: "2026-07-01T09:00:00Z".to_string(),
            ended_at: None,
            title: Some("Sync".to_string()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        let add = |predicate: &str, object: &str| {
            FactOp::Add(NewFact {
                entity_id: USER_SCOPE.to_string(),
                subject: "You".to_string(),
                predicate: predicate.to_string(),
                object: object.to_string(),
                valid_from: "2026-07-01T09:00:00Z".to_string(),
                recorded_at: "2026-07-01T09:00:00Z".to_string(),
                confidence: 1.0,
                meeting_id: Some("m1".to_string()),
            })
        };
        db.apply_user_fact_ops(&[
            add("prefer", "the darker theme"),
            add("speaks", "Polish natively"),
        ])
        .unwrap();
        let unlocked = std::collections::HashSet::new();

        // The adversarial repro query: only "the" overlaps (with the WRONG fact).
        let brief = build_memory_brief(&db, "what language should the assistant use?", &unlocked);
        assert!(
            brief.contains("Polish natively"),
            "the language fact must never be displaced by a stopword-only match: {brief}"
        );
        assert!(
            brief.contains("darker theme"),
            "zero content-word hits ⇒ the FULL-list fallback (both facts): {brief}"
        );
    }

    /// L2.3 — the import prompt labels the pasted text as pre-extracted memories from another
    /// assistant and hard-bounds it (bounds the prompt / leak surface).
    #[test]
    fn import_prompt_labels_source_and_bounds_text() {
        let prompt = build_import_user_prompt("I prefer replies in Polish");
        assert!(prompt.contains("ANOTHER AI ASSISTANT"));
        assert!(prompt.contains("I prefer replies in Polish"));

        // 'q' does not occur in the label text (unlike 'z' — "re-summarize"), so the count below
        // measures ONLY the pasted excerpt.
        let huge = "q".repeat(IMPORT_EXCERPT_CHARS * 3);
        let bounded = build_import_user_prompt(&huge);
        let q_count = bounded.chars().filter(|c| *c == 'q').count();
        assert!(q_count <= IMPORT_EXCERPT_CHARS);
    }

    /// L2.3 — the stub reasoner (default install, no model) imports NOTHING, gracefully.
    #[test]
    fn extract_imported_memories_stub_is_empty() {
        let out = extract_imported_memories(&crate::reason::StubReasoner, "remember: I like tea");
        assert!(out.is_empty());
    }

    /// candidates_from_raw scopes every candidate to the user + drops empty pairs.
    #[test]
    fn candidates_from_raw_scopes_to_user_and_drops_empties() {
        let raw = vec![
            RawUserFact {
                predicate: "prefers".into(),
                object: "Polish replies".into(),
            },
            RawUserFact {
                predicate: "".into(),
                object: "x".into(),
            },
            RawUserFact {
                predicate: "role".into(),
                object: "".into(),
            },
        ];
        let cands = candidates_from_raw(raw);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].entity_id, USER_SCOPE);
        assert_eq!(cands[0].subject, USER_SUBJECT);
        assert_eq!(cands[0].predicate, "prefers");
        assert_eq!(cands[0].object, "Polish replies");
    }
}
