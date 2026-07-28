//! Deterministic parser for the note's LABELLED BULLET sections — `## Decisions` and
//! `## Risks & open questions`.
//!
//! #6 — the knowledge layer never read the most structured thing the app itself produces. A note
//! with five decisions, nine key points and seven open risks reported `0 total decision(s) on
//! record`, because the ONLY writer into the fact registry is the LLM triple extractor, and
//! `facts.rs::EXTRACT_SYSTEM` narrows it by design to short key-value attributes ("predicate is a
//! short, stable attribute (e.g. \"status\", \"owner\", \"deadline\", \"role\")"). A decision is a
//! SENTENCE, not an attribute, so it was never a candidate. Grepping `Decisions` across the crate
//! found only prompt-authoring sites — zero parsers.
//!
//! Pure and dependency-free, like `action_items::parse_action_items`: the note already carries the
//! headings, so this reads what the model was told to write rather than asking a model again.

/// One parsed bullet, tagged with the section heading it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabeledBullet {
    /// A stable machine key for the section: `decision` | `risk`.
    pub kind: &'static str,
    pub text: String,
}

/// Section headings that carry DECISIONS, lowercased. EN + PL, matching every built-in note style
/// (`template.rs::template_for_style`) and the Polish notes a real vault contains.
const DECISION_HEADINGS: &[&str] = &["decisions", "decyzje", "ustalenia"];

/// Section headings that carry RISKS / open questions.
const RISK_HEADINGS: &[&str] = &[
    "risks & open questions",
    "risks and open questions",
    "risks",
    "open questions",
    "follow-ups",
    "ryzyka",
    "otwarte pytania",
    "ryzyka i otwarte pytania",
];

/// Parse `## Decisions` / `## Risks & open questions` bullets out of a note.
///
/// Only genuine list items count — a prose paragraph under the heading is not a decision. The
/// model is explicitly instructed to write "None recorded" when there were none, so that exact
/// placeholder (EN + PL) is dropped rather than stored as a decision that reads like one.
pub fn parse_labeled_bullets(markdown: &str) -> Vec<LabeledBullet> {
    let mut out = Vec::new();
    let mut kind: Option<&'static str> = None;
    let mut in_fence = false;
    for line in markdown.lines() {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(title) = t.strip_prefix('#') {
            let title = title.trim_start_matches('#').trim().to_lowercase();
            // A heading always RESETS the section, so a bullet can never be attributed to a
            // section that ended several headings ago.
            kind = if DECISION_HEADINGS.iter().any(|h| title.starts_with(h)) {
                Some("decision")
            } else if RISK_HEADINGS.iter().any(|h| title.starts_with(h)) {
                Some("risk")
            } else {
                None
            };
            continue;
        }
        let Some(kind) = kind else { continue };
        let Some(body) = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("* "))
            .or_else(|| t.strip_prefix("+ "))
        else {
            continue;
        };
        // Skip a checklist item — that is an ACTION, already owned by `parse_action_items`.
        let body = body.trim();
        if body.starts_with("[ ]") || body.starts_with("[x]") || body.starts_with("[X]") {
            continue;
        }
        if is_none_placeholder(body) {
            continue;
        }
        if body.is_empty() {
            continue;
        }
        out.push(LabeledBullet {
            kind,
            text: body.to_string(),
        });
    }
    out
}

/// The "nothing to record" placeholder the templates instruct the model to emit. Storing it would
/// create a decision whose text says there were no decisions.
fn is_none_placeholder(body: &str) -> bool {
    let b = body.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    matches!(
        b.as_str(),
        "none" | "none recorded" | "n/a" | "brak" | "brak ustaleń" | "brak decyzji"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R6/#6 (regression). A note with three `## Decisions` bullets must report three, not zero.
    #[test]
    fn decisions_and_risks_are_parsed_from_their_sections() {
        let md = "# Title\n\
                  \n## Summary\nsome prose that is not a decision\n\
                  \n## Decisions\n\
                  - Rename the servers to interfaces\n\
                  - Ship the operator in hybrid mode\n\
                  - Keep the control plane authoritative\n\
                  \n## Action items\n\
                  - [ ] Anna — write the migration\n\
                  \n## Risks & open questions\n\
                  - The version bump may break older clients\n";
        let got = parse_labeled_bullets(md);
        let decisions: Vec<_> = got.iter().filter(|b| b.kind == "decision").collect();
        let risks: Vec<_> = got.iter().filter(|b| b.kind == "risk").collect();
        assert_eq!(decisions.len(), 3, "three decisions: {got:?}");
        assert_eq!(risks.len(), 1, "one risk: {got:?}");
        assert_eq!(decisions[0].text, "Rename the servers to interfaces");
    }

    /// An ACTION item lives under its own heading and belongs to `parse_action_items`; a checklist
    /// bullet that happens to sit under Decisions must not be double-counted as one.
    #[test]
    fn checklist_items_are_not_decisions() {
        let md = "## Decisions\n- [ ] Anna — do the thing\n- A real decision\n";
        let got = parse_labeled_bullets(md);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "A real decision");
    }

    /// The templates instruct the model to write "None recorded" when there were none. Storing that
    /// would create a decision whose text says there were no decisions.
    #[test]
    fn the_none_placeholder_is_not_a_decision() {
        for md in [
            "## Decisions\n- None recorded\n",
            "## Decisions\n- None\n",
            "## Decyzje\n- Brak\n",
        ] {
            assert!(parse_labeled_bullets(md).is_empty(), "{md:?}");
        }
    }

    /// A heading RESETS the section, so a later bullet is never attributed to a section that ended.
    #[test]
    fn a_following_heading_ends_the_section() {
        let md = "## Decisions\n- kept\n\n## Notes\n- not a decision\n";
        let got = parse_labeled_bullets(md);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "kept");
    }

    /// Polish notes are a real case in this vault, not a hypothetical.
    #[test]
    fn polish_headings_are_recognised() {
        let md = "## Decyzje\n- Zmieniamy nazwę serwerów\n\n## Ryzyka\n- Może zepsuć starsze klienty\n";
        let got = parse_labeled_bullets(md);
        assert_eq!(got.iter().filter(|b| b.kind == "decision").count(), 1);
        assert_eq!(got.iter().filter(|b| b.kind == "risk").count(), 1);
    }

    /// A fenced code block is never content.
    #[test]
    fn code_fences_are_skipped() {
        let md = "## Decisions\n```\n- not a decision, just sample markdown\n```\n- a real one\n";
        let got = parse_labeled_bullets(md);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "a real one");
    }
}
