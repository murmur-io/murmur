//! "Chat with the meeting": grounded Q&A over a single meeting's transcript plus any
//! visibility-gated sources the user explicitly pinned. The provider receives the grounding
//! material in the system prompt plus the running conversation history.

use crate::storage::models::ChatTurn;

/// Cap the transcript sent as context so a very long meeting can't blow the prompt budget.
const MAX_TRANSCRIPT_CHARS: usize = 40_000;
/// Give explicit sources their OWN bounded budget. They must not sit behind (and get truncated
/// together with) a long anchor transcript: choosing a source is an explicit user instruction.
pub(crate) const MAX_PINNED_SOURCE_CHARS: usize = 40_000;

/// Backward-compatible transcript-only entry point. Keeping this wrapper makes the no-source path
/// byte-identical and leaves existing callers/tests explicit about when cross-item context exists.
pub fn build(
    transcript: &str,
    history: &[ChatTurn],
    question: &str,
    memory_brief: &str,
) -> (String, String) {
    build_with_sources(transcript, "", history, question, memory_brief)
}

/// Build the (system, user) prompt pair for a meeting chat turn with optional user-pinned sources.
/// Transcript and sources are capped INDEPENDENTLY: a >40k-char meeting can no longer consume the
/// source budget and erase a note the user explicitly attached.
///
/// `memory_brief` is the gated cross-meeting USER MEMORY brief (durable preferences/commitments
/// about the user). When non-empty it is injected as a small "WHAT YOU KNOW ABOUT THE USER" block so
/// the meeting answer honors the user's standing preferences (e.g. "prefers replies in Polish");
/// EMPTY ⇒ the block is omitted entirely, so the prompt is BYTE-IDENTICAL to the pre-memory prompt
/// (the caller passes `""` when memory is empty or disabled). `pinned_sources` is likewise assembled
/// exclusively by the caller's visibility-gated source packer and rides the same existing provider
/// consent/redaction seam. This function performs no reads and opens no new egress path.
pub fn build_with_sources(
    transcript: &str,
    pinned_sources: &str,
    history: &[ChatTurn],
    question: &str,
    memory_brief: &str,
) -> (String, String) {
    build_with_composite_sources(
        transcript,
        pinned_sources,
        history,
        question,
        memory_brief,
        false,
    )
}

pub fn build_with_composite_sources(
    transcript: &str,
    pinned_sources: &str,
    history: &[ChatTurn],
    question: &str,
    memory_brief: &str,
    dashboard_scope: bool,
) -> (String, String) {
    let t = if transcript.chars().count() > MAX_TRANSCRIPT_CHARS {
        let head: String = transcript.chars().take(MAX_TRANSCRIPT_CHARS).collect();
        format!("{head}\n[transcript truncated]")
    } else {
        transcript.to_string()
    };
    let pinned = if pinned_sources.chars().count() > MAX_PINNED_SOURCE_CHARS {
        let head: String = pinned_sources
            .chars()
            .take(MAX_PINNED_SOURCE_CHARS)
            .collect();
        format!("{head}\n[pinned sources truncated]")
    } else {
        pinned_sources.to_string()
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

    let system = if dashboard_scope {
        format!(
            "You are a helpful assistant answering questions about ONE primary meeting within a \
USER-COMPOSED DASHBOARD. Base answers strictly on the meeting transcript and readable dashboard \
grounding below. The dashboard grounding can include notes, recordings, documents, capped active \
linked context, and derived views. Preserve the meeting as the primary anchor while treating the \
dashboard as a mixed-source corpus. If the answer is not in either section, say you \
don't know. Never invent facts, decisions, or attributions. Do not claim there is no dashboard \
context merely because privacy gating omitted some items. Be concise and concrete.\n\n\
MEETING TRANSCRIPT:\n{t}\n\nDASHBOARD GROUNDING:\n{pinned}"
        )
    } else if pinned.trim().is_empty() {
        // Keep the original transcript-only prompt byte-for-byte when the picker is empty or every
        // selected source was gated away.
        format!(
            "You are a helpful assistant answering questions about ONE meeting. Base your answers \
strictly on the transcript below. If the answer is not in the transcript, say you don't know \
— do not invent facts, decisions, or attributions. Be concise and concrete, and reference \
what was said (and roughly when) when useful.\n\n{memory}TRANSCRIPT:\n{t}"
        )
    } else {
        format!(
            "You are a helpful assistant answering questions about ONE meeting and sources the user \
explicitly pinned. Base your answers strictly on the grounding material below: the meeting \
transcript and the user-pinned sources. If the answer is not in either, say you don't know — do \
not invent facts, decisions, or attributions. Be concise and concrete. When an answer comes from \
the transcript, reference what was said (and roughly when) when useful; when it comes from a \
pinned source, name that source.\n\n{memory}MEETING TRANSCRIPT:\n{t}\n\nUSER-PINNED SOURCES:\n{pinned}"
        )
    };

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
        let history = [
            turn("user", "What was decided?"),
            turn("assistant", "To ship Friday."),
        ];
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

    #[test]
    fn pinned_sources_have_an_independent_budget_and_explicit_contract() {
        let long = "x".repeat(MAX_TRANSCRIPT_CHARS + 5_000);
        let (system, _u) = build_with_sources(
            &long,
            "### [[Launch plan]]\nCodename: Orchid",
            &[],
            "What is the codename?",
            "",
        );
        assert!(system.contains("[transcript truncated]"));
        assert!(system.contains("USER-PINNED SOURCES"));
        assert!(system.contains("Codename: Orchid"));
        assert!(system.contains("name that source"));
    }

    #[test]
    fn blank_pinned_sources_keep_the_legacy_prompt_byte_identical() {
        let legacy = build("transcript", &[], "question", "memory");
        let blank = build_with_sources("transcript", "   ", &[], "question", "memory");
        assert_eq!(blank, legacy);
    }

    #[test]
    fn meeting_dashboard_prompt_keeps_anchor_and_names_composite_scope() {
        let (system, _) = build_with_composite_sources(
            "primary transcript",
            "### [[Plan]]\ndocument body\n\n- promises\n    · ship it",
            &[],
            "q",
            "must not appear",
            true,
        );
        assert!(system.contains("ONE primary meeting"));
        assert!(system.contains("USER-COMPOSED DASHBOARD"));
        assert!(system.contains("DASHBOARD GROUNDING"));
        assert!(!system.contains("meeting notes only"));
        assert!(!system.contains("must not appear"));
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
