//! Prompt builder for "Ask-My-Vault": grounded Q&A across many meetings' notes, with inline
//! [[Title]] citations. Pure function, mirrors chat.rs.

use crate::storage::models::ChatTurn;

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
- Format as clean, scannable Markdown: a one-line **bold takeaway** first, then tight bullets \
or short `##` sections. A tasteful emoji in a section header is welcome (e.g. ## ✅ Decisions). \
Never output YAML or front-matter.\n\n\
{memory}MEETING NOTES:\n{corpus}",
        memory = memory_block(memory_brief),
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
pub fn agentic_system(memory_brief: &str) -> String {
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
     - Format as clean, scannable Markdown: a one-line **bold takeaway** first, then tight \
     bullets or short `##` sections. A tasteful emoji in a section header is welcome (e.g. \
     ## ✅ Decisions). Never output YAML or front-matter.{memory}",
        memory = agentic_memory_suffix(memory_brief),
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
        let (s, u) =
            build("### [[Sync]] · 2026-07-01 · id:1\nWe shipped.", &[], "What shipped?", "");
        assert!(s.contains("[[Sync]]"));
        assert!(u.contains("User: What shipped?"));
        assert!(u.trim_end().ends_with("Assistant:"));
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

    /// The agentic persona injects the SAME brief the same way: empty ⇒ byte-identical, present ⇒
    /// the labelled block appears.
    #[test]
    fn agentic_system_injects_memory_brief_and_empty_is_byte_identical() {
        let base = agentic_system("");
        assert_eq!(base, agentic_system("   "));
        assert!(!base.contains("WHAT YOU KNOW ABOUT THE USER"));
        let with = agentic_system("- You prefer: Polish replies");
        assert!(with.contains("WHAT YOU KNOW ABOUT THE USER"));
        assert!(with.contains("Polish replies"));
        assert!(with.starts_with(&base), "the persona prefix is unchanged");
    }
}
