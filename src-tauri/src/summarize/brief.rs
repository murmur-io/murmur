//! Pre-Meeting Brief: a short, grounded prep card for an upcoming meeting, built from the
//! user's own past meeting notes (who you've met, what's still open, what to raise).

use crate::summarize::template::language_directive;

/// Build the (system, user) prompt: past-meeting corpus + the upcoming-meeting subject.
pub fn build_brief_prompt(corpus: &str, subject: &str, note_language: &str) -> (String, String) {
    let system = format!(
        "You write a SHORT pre-meeting BRIEF for an upcoming meeting about: {subject}. Use ONLY \
the past meeting notes below (each headed by `### [[Title]] · date`). Output concise Markdown:\n\
- **Context** — 1-2 lines on prior meetings about these people / this topic,\n\
- **Still open** — unresolved action items or decisions to follow up on (cite [[Title]]),\n\
- **Talking points** — 3 concrete things worth raising.\n\
If there is little relevant history, say so in one line. Never invent.\n\n{lang}\n\n\
PAST MEETING NOTES:\n{corpus}",
        lang = language_directive(note_language)
    );
    let user = format!("Upcoming meeting: {subject}");
    (system, user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_brief_prompt() {
        let (s, u) = build_brief_prompt("### [[1:1 Anna]] · 2026-07-01\nDiscussed raise.", "1:1 with Anna", "auto");
        assert!(s.contains("Talking points"));
        assert!(s.contains("[[1:1 Anna]]"));
        assert!(u.contains("1:1 with Anna"));
    }
}
