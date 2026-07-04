//! NOTES bake-off harness (spec §7 P3-gate; adversarial review code-truth #2) — the missing
//! note-GENERATION quality infra, DISTINCT from the retrieval bake-off (`eval/bakeoff.rs`, which
//! scores FTS/semantic/RRF ranking and CANNOT score a generated note). This decides whether a local
//! Q4 GGUF writes an acceptable meeting note vs the cloud default BEFORE Fully-Local Notes ships as a
//! choice (spec §2.1 / change-map #13 is gated on this).
//!
//! ## What is / isn't testable headless
//! - The STRUCTURAL METRICS (this file) are PURE and unit-tested — no model, runs in `cargo test --lib`.
//! - The REAL RUN generates notes through actual providers (the local heavy model vs the cloud
//!   default) over real meetings and is a MANUAL Mac step (models on disk + Metal + optional cloud
//!   consent). A green build proves the harness typechecks, NOT that any model writes well — that
//!   verdict is a human's, blending these metrics with actually READING the notes (esp. Polish).
//!
//! ## Honesty
//! `NoteMetrics` are STRUCTURAL PROXIES ("does it look like a meeting note"), never a quality score:
//! a Q4 model can hit every structural target and still read poorly, and a great note can be terse.
//! They flag GROSS failures (empty, no action items, no headings) and give a human a fast diff; the
//! blind side-by-side READ is the real gate.

use serde::Serialize;

/// Structural, model-free metrics of one generated note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteMetrics {
    /// Whitespace-delimited word count.
    pub words: usize,
    /// Markdown headings (`#`-prefixed lines).
    pub headings: usize,
    /// Task checkboxes (`- [ ]` / `- [x]`).
    pub action_items: usize,
    /// List bullets (`- ` / `* `), including the checkboxes.
    pub bullets: usize,
    /// Obsidian `[[wikilinks]]`.
    pub wikilinks: usize,
}

/// Compute the structural metrics of a note. Pure; UTF-8-safe (Polish diacritics don't affect the
/// ASCII structural markers).
pub fn note_metrics(note: &str) -> NoteMetrics {
    let mut headings = 0;
    let mut action_items = 0;
    let mut bullets = 0;
    for raw in note.lines() {
        let line = raw.trim_start();
        if line.starts_with('#') {
            headings += 1;
        }
        let is_bullet = line.starts_with("- ") || line.starts_with("* ");
        if is_bullet {
            bullets += 1;
        }
        // Checkbox forms after a bullet marker: "- [ ] " / "- [x] " (case-insensitive x).
        let after = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .unwrap_or("");
        if after.starts_with("[ ]") || after.starts_with("[x]") || after.starts_with("[X]") {
            action_items += 1;
        }
    }
    NoteMetrics {
        words: note.split_whitespace().count(),
        headings,
        action_items,
        bullets,
        wikilinks: count_wikilinks(note),
    }
}

/// Count non-overlapping `[[...]]` spans (a link must have a non-empty target).
fn count_wikilinks(s: &str) -> usize {
    let mut n = 0;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            if let Some(rel) = s[i + 2..].find("]]") {
                if rel > 0 {
                    n += 1;
                    i += 2 + rel + 2;
                    continue;
                }
            }
        }
        i += 1;
    }
    n
}

/// A side-by-side comparison of two notes over the SAME meeting (e.g. local `a` vs cloud `b`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteComparison {
    pub a: NoteMetrics,
    pub b: NoteMetrics,
}

/// Compare two notes structurally. The Mac run collects one of these per meeting across the labeled
/// set; a human reads the pairs + weighs the metric deltas to decide the local-notes go/no-go.
pub fn compare(a: &str, b: &str) -> NoteComparison {
    NoteComparison {
        a: note_metrics(a),
        b: note_metrics(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_note_is_all_zero() {
        assert_eq!(
            note_metrics(""),
            NoteMetrics {
                words: 0,
                headings: 0,
                action_items: 0,
                bullets: 0,
                wikilinks: 0
            }
        );
    }

    #[test]
    fn structured_note_counts_correctly() {
        let note = "# Q2 Roadmap\n\n## Decisions\n- Atlas ships May 30 — see [[Kickoff]]\n\n## Action items\n- [ ] Sarah circulates deck\n- [x] Anya sent mocks\n* plain bullet\n";
        let m = note_metrics(note);
        assert_eq!(m.headings, 3);
        assert_eq!(m.action_items, 2);
        assert_eq!(m.bullets, 4); // two checkboxes + the Atlas line + the plain '*' bullet
        assert_eq!(m.wikilinks, 1);
        assert!(m.words > 10);
    }

    #[test]
    fn wikilinks_require_a_target_and_dont_overlap() {
        assert_eq!(count_wikilinks("[[a]] and [[b]]"), 2);
        assert_eq!(count_wikilinks("[[]] empty is not a link"), 0);
        assert_eq!(count_wikilinks("no links here"), 0);
        // Polish target is fine (ASCII brackets).
        assert_eq!(count_wikilinks("see [[Spotkanie Zarządu]]"), 1);
    }

    #[test]
    fn compare_pairs_both_notes() {
        let c = compare("# A\n- [ ] x", "just prose");
        assert_eq!(c.a.headings, 1);
        assert_eq!(c.a.action_items, 1);
        assert_eq!(c.b.headings, 0);
        assert_eq!(c.b.action_items, 0);
    }
}
