//! "Recipes": run a builtin or saved prompt template over ONE meeting's transcript via the
//! configured provider's complete() — grounded recap emails, decision logs, work tickets,
//! and per-meeting-type recaps (1:1 / standup / sales / interview). Mirrors chat.rs/timeline.rs.

use crate::summarize::template::language_directive;

const MAX_TRANSCRIPT_CHARS: usize = 40_000;

/// Built-in recipes shown as quick chips: (id, label, instruction prompt).
pub const BUILTIN_RECIPES: &[(&str, &str, &str)] = &[
    (
        "grounded-email",
        "Follow-up email",
        "Write a concise, ready-to-send follow-up email recapping this meeting: one line of \
context, the key decisions, and a per-attendee list of action items with owners and due \
dates where stated. Flag anything uncertain as '(to confirm)' — never invent commitments. \
Clear, professional tone.",
    ),
    (
        "decision-log",
        "Decision log",
        "Extract ONLY the decisions made in this meeting as a clean list. For each: the \
decision, who made/owns it, and the rationale if stated. If none, say 'No decisions recorded.'",
    ),
    (
        "ticket",
        "Work ticket",
        "Turn the most important action item or problem discussed into a ready-to-paste work \
ticket: Title, Description, Acceptance criteria (bullets), Owner if mentioned.",
    ),
    (
        "1on1",
        "1:1 recap",
        "Summarize this 1:1: wins/progress, blockers/concerns, feedback exchanged, and agreed \
next steps with owners. Keep it warm and personal.",
    ),
    (
        "standup",
        "Standup notes",
        "Summarize as standup notes: per-person Done / Doing / Blockers, then a short list of \
team-level follow-ups.",
    ),
    (
        "sales",
        "Sales recap",
        "Summarize as a sales call recap: prospect context, pain points, objections, buying \
signals, next steps + owner, and a deal-risk note.",
    ),
    (
        "interview",
        "Interview notes",
        "Summarize as structured interview notes: candidate strengths, concerns, signal per \
competency discussed, and a hire / no-hire lean with reasoning grounded only in what was said.",
    ),
];

/// Build the (system, user) prompt pair for running `recipe_prompt` over `transcript`.
/// Grounded: the model must stick to the transcript. Output language follows `note_language`.
pub fn build_recipe_prompt(
    transcript: &str,
    recipe_prompt: &str,
    note_language: &str,
) -> (String, String) {
    let t = if transcript.chars().count() > MAX_TRANSCRIPT_CHARS {
        let head: String = transcript.chars().take(MAX_TRANSCRIPT_CHARS).collect();
        format!("{head}\n[transcript truncated]")
    } else {
        transcript.to_string()
    };
    let system = format!(
        "You produce a specific artifact from ONE meeting transcript. Base everything STRICTLY \
on the transcript — never invent facts, names, decisions, or commitments; if something isn't \
in the transcript, omit it or mark it uncertain. Be concise and well-formatted in Markdown.\n\n\
TASK: {recipe_prompt}\n\n{lang}",
        lang = language_directive(note_language)
    );
    let user = format!("TRANSCRIPT:\n{t}");
    (system, user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_prompt_with_task_and_transcript() {
        let (s, u) = build_recipe_prompt("Alice: hi", "Write an email.", "auto");
        assert!(s.contains("Write an email."));
        assert!(u.contains("Alice: hi"));
    }

    #[test]
    fn truncates_long_transcript() {
        let long = "x".repeat(MAX_TRANSCRIPT_CHARS + 1000);
        let (_s, u) = build_recipe_prompt(&long, "t", "auto");
        assert!(u.contains("[transcript truncated]"));
    }

    #[test]
    fn has_builtin_recipes() {
        assert!(BUILTIN_RECIPES
            .iter()
            .any(|(id, _, _)| *id == "grounded-email"));
        assert!(BUILTIN_RECIPES.len() >= 5);
    }
}
