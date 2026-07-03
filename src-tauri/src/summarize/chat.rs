//! "Chat with the meeting": grounded Q&A over a single meeting's transcript. The provider
//! answers strictly from the transcript (passed as the system prompt) plus the running
//! conversation history.

use crate::storage::models::ChatTurn;

/// Cap the transcript sent as context so a very long meeting can't blow the prompt budget.
const MAX_TRANSCRIPT_CHARS: usize = 40_000;

/// Build the (system, user) prompt pair for a meeting chat turn. Pure + testable.
///
/// `memory_brief` is the gated cross-meeting USER MEMORY brief (durable preferences/commitments
/// about the user). When non-empty it is injected as a small "WHAT YOU KNOW ABOUT THE USER" block so
/// the meeting answer honors the user's standing preferences (e.g. "prefers replies in Polish");
/// EMPTY ⇒ the block is omitted entirely, so the prompt is BYTE-IDENTICAL to the pre-memory prompt
/// (the caller passes `""` when memory is empty or disabled). The brief is DERIVED from VISIBLE facts
/// only (gated by the caller) and rides this surface's existing redaction + consent egress.
pub fn build(
    transcript: &str,
    history: &[ChatTurn],
    question: &str,
    memory_brief: &str,
) -> (String, String) {
    let t = if transcript.chars().count() > MAX_TRANSCRIPT_CHARS {
        let head: String = transcript.chars().take(MAX_TRANSCRIPT_CHARS).collect();
        format!("{head}\n[transcript truncated]")
    } else {
        transcript.to_string()
    };

    // Present brief ⇒ a labelled block BEFORE the transcript; empty ⇒ byte-identical to before.
    let memory = if memory_brief.trim().is_empty() {
        String::new()
    } else {
        format!(
            "WHAT YOU KNOW ABOUT THE USER (their durable preferences / commitments — honor them):\n\
{memory_brief}\n\n"
        )
    };

    let system = format!(
        "You are a helpful assistant answering questions about ONE meeting. Base your answers \
strictly on the transcript below. If the answer is not in the transcript, say you don't know \
— do not invent facts, decisions, or attributions. Be concise and concrete, and reference \
what was said (and roughly when) when useful.\n\n{memory}TRANSCRIPT:\n{t}"
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

    fn turn(role: &str, content: &str) -> ChatTurn {
        ChatTurn {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn includes_transcript_and_question() {
        let (system, user) = build("Alice: hi\nBob: bye", &[], "Who said hi?", "");
        assert!(system.contains("Alice: hi"));
        assert!(user.contains("User: Who said hi?"));
        assert!(user.trim_end().ends_with("Assistant:"));
    }

    #[test]
    fn renders_prior_history() {
        let history = [turn("user", "What was decided?"), turn("assistant", "To ship Friday.")];
        let (_s, user) = build("t", &history, "By whom?", "");
        assert!(user.contains("User: What was decided?"));
        assert!(user.contains("Assistant: To ship Friday."));
        assert!(user.contains("User: By whom?"));
    }

    #[test]
    fn truncates_very_long_transcript() {
        let long = "x".repeat(MAX_TRANSCRIPT_CHARS + 5_000);
        let (system, _u) = build(&long, &[], "q", "");
        assert!(system.contains("[transcript truncated]"));
    }

    /// EMPTY memory brief ⇒ byte-identical to the pre-memory prompt (no block); a present brief ⇒
    /// the labelled block appears BEFORE the transcript.
    #[test]
    fn memory_brief_injects_and_empty_is_byte_identical() {
        let (s_empty, _) = build("t", &[], "q", "");
        let (s_blank, _) = build("t", &[], "q", "   ");
        assert_eq!(s_empty, s_blank);
        assert!(!s_empty.contains("WHAT YOU KNOW ABOUT THE USER"));

        let (s_mem, _) = build("transcript body", &[], "q", "- You prefer: Polish replies");
        assert!(s_mem.contains("WHAT YOU KNOW ABOUT THE USER"));
        assert!(s_mem.contains("Polish replies"));
        let mem_at = s_mem.find("Polish replies").unwrap();
        let tx_at = s_mem.find("transcript body").unwrap();
        assert!(mem_at < tx_at, "memory must precede the transcript");
    }
}
