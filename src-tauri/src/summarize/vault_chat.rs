//! Prompt builder for "Ask-My-Vault": grounded Q&A across many meetings' notes, with inline
//! [[Title]] citations. Pure function, mirrors chat.rs.

use crate::storage::models::ChatTurn;

/// Build the (system, user) prompt pair: the meeting-notes corpus as system context, the
/// running conversation + question as the user message.
pub fn build(corpus: &str, history: &[ChatTurn], question: &str) -> (String, String) {
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
MEETING NOTES:\n{corpus}"
    );

    (system, render_conversation(history, question))
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
pub fn agentic_system() -> String {
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
     ## ✅ Decisions). Never output YAML or front-matter."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_corpus_and_question() {
        let (s, u) = build("### [[Sync]] · 2026-07-01 · id:1\nWe shipped.", &[], "What shipped?");
        assert!(s.contains("[[Sync]]"));
        assert!(u.contains("User: What shipped?"));
        assert!(u.trim_end().ends_with("Assistant:"));
    }
}
