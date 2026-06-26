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

    (system, user)
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
