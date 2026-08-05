//! Prompt builder for "Ask-My-Vault": grounded Q&A across many meetings' notes, with inline
//! [[Title]] citations. Pure function, mirrors chat.rs.

use crate::storage::models::ChatTurn;

/// Shared coordinate/polarity discipline for both the deterministic corpus floor and the agentic
/// Ask persona. The Qwen bake-off exposed a clause-contamination failure: an OPEN budget was used
/// to negate a separately approved launch. Single-sourcing keeps the two Ask routes aligned.
const FACT_COORDINATE_RULES: &str = "\
- Answer every sub-question separately; do not collapse independent decisions or statuses.\n\
- Preserve the exact owners, dates, numbers, locations, and decision status stated in the source.\n\
- Preserve polarity per clause: one negative or open status must never negate a separate approved decision.\n\
- Do not omit a stated owner deadline when the user asks for that deadline; do not volunteer unrelated details.";

/// Build the (system, user) prompt pair: the meeting-notes corpus as system context, the
/// running conversation + question as the user message.
///
/// `memory_brief` is the gated cross-meeting USER MEMORY brief (durable preferences/commitments
/// about the user). When non-empty it is injected as a small "WHAT YOU KNOW ABOUT THE USER" block so
/// the vault answer honors the user's standing preferences (e.g. "prefers replies in Polish");
/// EMPTY ⇒ the block is omitted entirely, so the prompt is BYTE-IDENTICAL to the pre-memory prompt
/// (the caller passes `""` when memory is empty or disabled). The brief is DERIVED from VISIBLE facts
/// only (gated by the caller) and rides this surface's existing redaction + consent egress.
pub fn build(
    corpus: &str,
    history: &[ChatTurn],
    question: &str,
    memory_brief: &str,
) -> (String, String) {
    let system = format!(
        "You answer questions across a user's PAST MEETINGS, using ONLY the meeting notes \
provided below. Each note is headed by `### [[Title]] · date · id:...`. \n\
Rules:\n\
- Answer strictly from these notes — if the answer isn't there, say you don't know. Never \
invent facts, decisions, or attributions.\n\
- Cite the meetings you rely on inline using their [[Title]] exactly as given.\n\
- When something evolved across meetings, trace it chronologically.\n\
- Be concise and concrete.\n\
{coordinates}\n\
- Format as clean, scannable Markdown: a one-line **bold takeaway** first, then tight bullets \
or short `##` sections. A tasteful emoji in a section header is welcome (e.g. ## ✅ Decisions). \
Never output YAML or front-matter.\n\n\
{memory}MEETING NOTES:\n{corpus}",
        memory = memory_block(memory_brief),
        coordinates = FACT_COORDINATE_RULES,
    );

    (system, render_conversation(history, question))
}

/// Render the optional USER MEMORY block. EMPTY brief ⇒ EMPTY string (byte-identical prompt); a
/// present brief ⇒ a small labelled block terminated by a blank line so the following section header
/// stays on its own line. Shared by the corpus floor ([`build`]) and the agentic persona
/// ([`agentic_system`]) so both surfaces inject the brief identically.
fn memory_block(memory_brief: &str) -> String {
    if memory_brief.trim().is_empty() {
        return String::new();
    }
    format!(
        "WHAT YOU KNOW ABOUT THE USER (their durable preferences / commitments — honor them):\n\
{memory_brief}\n\n"
    )
}

/// Render the running conversation + latest question into the user message. Shared VERBATIM by the
/// corpus floor ([`build`]) and the agentic Ask loop, so both brains see the exact same conversation
/// (the floor stays byte-identical to the pre-agentic implementation).
pub fn render_conversation(history: &[ChatTurn], question: &str) -> String {
    let mut user = String::new();
    for turn in history {
        let who = if turn.role == "assistant" {
            "Assistant"
        } else {
            "User"
        };
        user.push_str(&format!("{who}: {}\n", turn.content.trim()));
    }
    user.push_str(&format!("User: {}\nAssistant:", question.trim()));
    user
}

/// The vault-QA persona for the AGENTIC Ask surface (PR G, ask-unify): the same grounded / cited /
/// concise rules as the corpus prompt, but the grounding arrives through GATED TOOLS instead of a
/// pre-packed corpus (the agentic loop appends the tool catalog + JSON protocol itself). Deliberately
/// NO live-transcript / typed-notes injection — the Ask page is not a recording surface.
///
/// `memory_brief` is the gated cross-meeting USER MEMORY brief, injected identically to [`build`]:
/// non-empty ⇒ a "WHAT YOU KNOW ABOUT THE USER" block; EMPTY ⇒ byte-identical to the pre-memory
/// persona. Gated by the caller (VISIBLE facts only), rides the surface's existing egress.
///
/// `org_available` (A2, Shared Brain): when `true`, appends one explicit fallback-steering sentence
/// telling the model to ALSO try `org_brain_search` before concluding it doesn't know — the model
/// already has the tool advertised (gated identically by `org_brain_available`, tools.rs) but may
/// never CHOOSE to call it without this nudge. `false` ⇒ BYTE-IDENTICAL to the pre-org persona (the
/// non-member / org-unavailable path is completely unchanged).
pub fn agentic_system(memory_brief: &str, org_available: bool) -> String {
    format!(
        "You answer questions across a user's PAST MEETINGS, imported documents, and brain notes \
     (their private, on-device vault).\n\
     Rules:\n\
     - Ground every claim in tool results — search before answering unless you already have \
     enough grounding. If the answer isn't in the vault, say you don't know. Never invent \
     facts, decisions, or attributions.\n\
     - Cite the meetings you rely on inline using their [[Title]] exactly as the tools return \
     it; attribute web facts as \"(via web)\".\n\
     - When something evolved across meetings, trace it chronologically.\n\
     - Be concise and concrete.\n\
     {coordinates}\n\
     - For a direct fact, decision, owner, or date question, use search_meetings / \
     search_semantic first; do not start with list_dashboards unless the user asks about a \
     dashboard/board or broad curated scope. A search hit can include a useful snippet; if it does \
     not contain every fact the user asked for, do not declare those facts unknown yet. Open one \
     matching `[meeting:<id>]` with get_meeting or `[document:<kind>:<id>]` with get_document first. \
     For dashboard facts, follow list_dashboards with get_dashboard.\n\
     - Format as clean, scannable Markdown: a one-line **bold takeaway** first, then tight \
     bullets or short `##` sections. A tasteful emoji in a section header is welcome (e.g. \
     ## ✅ Decisions). Never output YAML or front-matter.{org}{memory}",
        org = org_fallback_suffix(org_available),
        memory = agentic_memory_suffix(memory_brief),
        coordinates = FACT_COORDINATE_RULES,
    )
}

/// The agentic persona's optional ORG FALLBACK sentence (A2). `false` ⇒ EMPTY string (byte-identical
/// to the pre-org persona); `true` ⇒ one explicit steering sentence appended after the base rules.
fn org_fallback_suffix(org_available: bool) -> String {
    if !org_available {
        return String::new();
    }
    "\n     - If the user's own vault doesn't answer this, ALSO try org_brain_search (the Shared \
     Brain) before concluding you don't know."
        .to_string()
}

/// Brain v2 L3 — the agentic persona WITH just-in-time retrieval seeding (behind the
/// `ask_jit_retrieval` flag). `listing` is the compact, GATED meeting listing
/// ([`crate::summarize::vault_context::build_meeting_listing_visible`]: `- id | title | date`
/// lines, ids/titles/dates ONLY — never content). Non-empty ⇒ the base persona plus a
/// search-then-`get_meeting` instruction block and the listing, so the model reads only the
/// meetings it needs instead of a pre-stuffed corpus. EMPTY (the flag-off path and the
/// nothing-visible vault) ⇒ BYTE-IDENTICAL to [`agentic_system`] — the flag-off legacy-prompt
/// contract, pinned by `agentic_system_jit_empty_listing_is_byte_identical`.
///
/// `org_available` (A2) is threaded straight through to [`agentic_system`] — see its doc for the
/// byte-identical-when-false contract.
pub fn agentic_system_jit(memory_brief: &str, listing: &str, org_available: bool) -> String {
    let base = agentic_system(memory_brief, org_available);
    let listing = listing.trim();
    if listing.is_empty() {
        return base;
    }
    format!(
        "{base}\n\nRETRIEVAL (just-in-time): you start with NO meeting content — only the \
         candidate MEETING LISTING below (`id | title | date`). Use search_meetings / \
         search_semantic to find candidates and call get_meeting with a listed (or found) id to \
         READ a meeting's note + transcript before answering. Read only the few meetings you \
         actually need.\n\nMEETING LISTING (top candidates for this question):\n{listing}"
    )
}

/// The agentic persona's optional USER MEMORY suffix. EMPTY brief ⇒ EMPTY string (byte-identical to
/// the pre-memory persona); a present brief ⇒ a leading-newline-separated labelled block.
fn agentic_memory_suffix(memory_brief: &str) -> String {
    if memory_brief.trim().is_empty() {
        return String::new();
    }
    format!(
        "\n\nWHAT YOU KNOW ABOUT THE USER (their durable preferences / commitments — honor them):\n\
{memory_brief}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_corpus_and_question() {
        let (s, u) = build(
            "### [[Sync]] · 2026-07-01 · id:1\nWe shipped.",
            &[],
            "What shipped?",
            "",
        );
        assert!(s.contains("[[Sync]]"));
        assert!(u.contains("User: What shipped?"));
        assert!(u.trim_end().ends_with("Assistant:"));
    }

    /// RED-before-GREEN from the local Qwen bake-off: one negative budget status must not negate an
    /// independently approved launch, and stated owner deadlines must not disappear from a
    /// multi-part answer.
    #[test]
    fn floor_prompt_pins_subquestion_coverage_exact_coordinates_and_polarity() {
        let (system, _) = build("corpus", &[], "question", "");
        for needle in [
            "Answer every sub-question separately",
            "exact owners, dates, numbers, locations, and decision status",
            "one negative or open status must never negate a separate approved decision",
            "Do not omit a stated owner deadline",
        ] {
            assert!(system.contains(needle), "missing `{needle}` in: {system}");
        }
    }

    /// EMPTY memory brief ⇒ the floor prompt is BYTE-IDENTICAL to the pre-memory prompt (no block,
    /// no stray whitespace difference). This is the "empty memory => byte-identical prompt" contract
    /// that keeps the floor faithful to the pre-memory behavior.
    #[test]
    fn empty_memory_brief_is_byte_identical() {
        let corpus = "### [[Sync]] · 2026-07-01 · id:1\nWe shipped.";
        let (s_empty, _) = build(corpus, &[], "q", "");
        let (s_blank, _) = build(corpus, &[], "q", "   ");
        assert_eq!(s_empty, s_blank);
        // And it is EXACTLY the historical prompt (no memory block spliced in).
        assert!(!s_empty.contains("WHAT YOU KNOW ABOUT THE USER"));
        assert!(s_empty.contains("MEETING NOTES:"));
    }

    /// A present memory brief is injected as a labelled block, BEFORE the meeting-notes corpus, so
    /// the model honors the user's standing preferences while still grounding in the notes.
    #[test]
    fn present_memory_brief_is_injected_before_corpus() {
        let (s, _) = build("corpus text", &[], "q", "- You prefer: Polish replies");
        assert!(s.contains("WHAT YOU KNOW ABOUT THE USER"));
        assert!(s.contains("Polish replies"));
        let mem_at = s.find("Polish replies").unwrap();
        let corpus_at = s.find("corpus text").unwrap();
        assert!(mem_at < corpus_at, "memory must precede the corpus");
    }

    /// Brain v2 L3 (flag-off contract): an EMPTY/blank JIT listing yields the BYTE-IDENTICAL
    /// legacy agentic persona — `ask_jit_retrieval = false` (which passes "") can never change the
    /// prompt. A present listing appends the search-then-get instructions + the listing block.
    #[test]
    fn agentic_system_jit_empty_listing_is_byte_identical() {
        for brief in ["", "- You prefer: Polish replies"] {
            assert_eq!(
                agentic_system_jit(brief, "", false),
                agentic_system(brief, false)
            );
            assert_eq!(
                agentic_system_jit(brief, "   ", false),
                agentic_system(brief, false)
            );
        }
        let with = agentic_system_jit("", "- m1 | Standup | 2026-07-01", false);
        assert!(
            with.starts_with(&agentic_system("", false)),
            "the base persona prefix is unchanged"
        );
        assert!(with.contains("MEETING LISTING"));
        assert!(with.contains("get_meeting"));
        assert!(with.contains("- m1 | Standup | 2026-07-01"));
    }

    /// The agentic persona injects the SAME brief the same way: empty ⇒ byte-identical, present ⇒
    /// the labelled block appears.
    #[test]
    fn agentic_system_injects_memory_brief_and_empty_is_byte_identical() {
        let base = agentic_system("", false);
        assert_eq!(base, agentic_system("   ", false));
        assert!(!base.contains("WHAT YOU KNOW ABOUT THE USER"));
        let with = agentic_system("- You prefer: Polish replies", false);
        assert!(with.contains("WHAT YOU KNOW ABOUT THE USER"));
        assert!(with.contains("Polish replies"));
        assert!(with.starts_with(&base), "the persona prefix is unchanged");
    }

    // ── A2 — org fallback steering ───────────────────────────────────────────────────────────────

    /// RED-before-GREEN: `org_available=true` must append the explicit org_brain_search fallback
    /// sentence to the agentic persona; `org_available=false` must produce a BYTE-IDENTICAL prompt to
    /// today (no parameter at all, pre-fix). Mirrors the `agentic_system_jit_empty_listing_is_byte_
    /// identical` pattern for this new parameter.
    #[test]
    fn agentic_system_org_hint_appears_only_when_available() {
        for memory in ["", "- You prefer: Polish replies"] {
            let off = agentic_system(memory, false);
            assert!(
                !off.contains("org_brain_search"),
                "org_available=false must never mention org_brain_search: {off}"
            );
            let on = agentic_system(memory, true);
            assert!(
                on.contains("org_brain_search"),
                "org_available=true must add the org fallback sentence: {on}"
            );
            // The base persona (everything before the memory suffix) must be UNCHANGED — the org
            // sentence is inserted between the base rules and the memory block, never mutating
            // either. Compare against the memory-off shape of each so insertion position doesn't
            // trip up a direct substring/prefix check when a memory brief is also present.
            let base = agentic_system("", false);
            assert!(
                on.starts_with(&base),
                "the base persona prefix must be unchanged when org is available: {on}"
            );
            if !memory.trim().is_empty() {
                assert!(
                    on.contains("WHAT YOU KNOW ABOUT THE USER") && on.contains(memory),
                    "the memory block must still be present when org is ALSO available: {on}"
                );
            }
        }
    }

    /// `org_available=false` must omit only the org fallback while retaining the current shared
    /// factual-coordinate and unknown-after-search retry contract.
    #[test]
    fn agentic_system_org_unavailable_keeps_grounding_without_org_hint() {
        let prompt = agentic_system("", false);
        assert!(!prompt.contains("org_brain_search"));
        assert!(prompt.contains("Answer every sub-question separately"));
        assert!(prompt.contains("do not declare those facts unknown yet"));
        assert!(prompt.contains("`[meeting:<id>]` with get_meeting"));
        assert!(prompt.contains("`[document:<kind>:<id>]` with get_document"));
        assert!(prompt.contains("follow list_dashboards with get_dashboard"));
    }

    /// `agentic_system_jit` threads `org_available` through unchanged: false ⇒ no org sentence in
    /// either the flag-off (empty listing) or flag-on (listing present) shapes.
    #[test]
    fn agentic_system_jit_org_hint_threads_through() {
        let off = agentic_system_jit("", "- m1 | Standup | 2026-07-01", false);
        assert!(!off.contains("org_brain_search"));
        let on = agentic_system_jit("", "- m1 | Standup | 2026-07-01", true);
        assert!(on.contains("org_brain_search"));
    }
}
