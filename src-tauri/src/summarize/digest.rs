//! Weekly Vault Digest: synthesize several recent meetings into ONE Obsidian note —
//! recurring themes, decisions, open action items rolled forward ("who owes what"), and a
//! linked [[Title]] list of the source meetings. Pure prompt builder; reuses provider.complete.

use crate::summarize::template::language_directive;

/// Build the (system, user) digest prompt over an already-assembled `corpus` of meeting notes
/// (each headed by `### [[Title]] · date`), for the human-readable `range_label` (e.g.
/// "the last 7 days").
pub fn build_digest_prompt(
    corpus: &str,
    range_label: &str,
    note_language: &str,
) -> (String, String) {
    let system = format!(
        "You write ONE Obsidian DIGEST note synthesizing several meetings from {range_label}. \
Base everything ONLY on the meeting notes provided (each headed by `### [[Title]] · date`). \
Output clean, scannable Markdown with these sections (keep the emoji):\n\
- a 2-3 sentence overview of the period,\n\
- ## 🔁 Recurring themes,\n\
- ## ✅ Decisions (each citing its [[Title]] source),\n\
- ## ⏳ Open action items — rolled forward and grouped by owner ('who owes what'),\n\
- ## 🗂 Meetings — a bullet [[Title]] list of every source.\n\
Cite sources with their [[Title]] exactly as given. Never invent facts, decisions, or owners. \
Do not emit YAML front-matter.\n\n{lang}",
        lang = language_directive(note_language)
    );
    let user = format!("MEETING NOTES ({range_label}):\n{corpus}");
    (system, user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_digest_prompt() {
        let (s, u) = build_digest_prompt("### [[Sync]] · 2026-07-01\nWe shipped.", "the last 7 days", "auto");
        assert!(s.contains("DIGEST"));
        assert!(s.contains("who owes what"));
        assert!(u.contains("[[Sync]]"));
    }
}
