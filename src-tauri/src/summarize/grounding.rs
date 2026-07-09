//! Tier 3b (B) — DETERMINISTIC GROUNDING: flag summary units the transcript does not support.
//!
//! The #1 category complaint about AI notes is confident fabrication — a hallucinated action item
//! ("Anna owns the rollout by Friday") that nobody actually said. This pass attacks it with a pure,
//! on-device, ZERO-EGRESS check: after the model writes the note, every candidate unit (an action
//! item, a bullet, a prose line) is scored against the meeting's OWN transcript segments. A unit
//! whose content words are largely ABSENT from the transcript is annotated with a NON-DESTRUCTIVE
//! `> unverified` blockquote line immediately after it.
//!
//! It is modeled on [`crate::summarize::timeline::repair_coverage`]: deterministic, idempotent, and
//! pure over `(note_markdown, segments)`. Crucially it NEVER rewrites or deletes a line — the
//! original bullet/checklist stays BYTE-IDENTICAL, so `action_items::parse_action_items` /
//! `patch_tasks_markdown` still parse it exactly as before; grounding only APPENDS a marker.
//!
//! LOCK / EGRESS posture (see `.claude/rules/lock-model.md`): this runs INSIDE the pipeline that
//! just produced the plaintext note, over that meeting's OWN segments (`Db::get_segments`) — it adds
//! NO new read path, NO cross-meeting read, and NO egress (100% local string ops). The `> unverified`
//! lines become part of the note markdown, so they are sealed WITH the note by `seal_note` (no
//! plaintext survives a lock). `segments.confidence` is non-content metadata (a probability).
//!
//! ASR-CONFIDENCE SURFACE (Tier 3b/A → B): the note is an LLM summary with no raw transcript, so the
//! only honest, guaranteed way to render whisper's per-segment confidence "in the exported note" is
//! here — when a flagged unit's best-overlapping transcript segments were ALL acoustically shaky
//! (`Segment.confidence < LOW_CONFIDENCE_P`), the marker becomes `> unverified (low audio confidence)`
//! so the reader learns the ASR itself was unsure, not that the model invented the claim.
//!
//! CALIBRATION HONESTY: the thresholds below are conservative defaults chosen to minimize false
//! `> unverified`; the RIGHT operating point (precision/recall of the flag vs a human's judgment of
//! which sentences are truly hallucinated) needs the maintainer's gold labels — a documented
//! follow-up, NOT provable by `cargo test --lib`. The grounding LOGIC is what these tests lock.

use std::collections::HashSet;

use crate::summarize::related_context::{is_stopword, tokenize};
use crate::transcribe::types::Segment;

/// Min DISTINCT content tokens a unit needs before we judge it. Shorter units (a bare title, a
/// two-word bullet, a bold label) carry too little signal to call "unsupported" without false
/// positives, so they are never flagged.
const GROUND_MIN_CONTENT_TOKENS: usize = 3;

/// A unit is UNVERIFIED when fewer than this fraction of its distinct content tokens appear anywhere
/// in the transcript. Conservative on purpose (flag only clearly-unsupported units). The exact
/// operating point is a maintainer-calibration follow-up (needs gold labels).
const GROUND_MIN_COVERAGE: f64 = 0.5;

/// Per-segment ASR confidence below this reads as "acoustically shaky". When a flagged unit's
/// best-overlapping segments are ALL this low, the annotation names the likely cause ("low audio
/// confidence") instead of a plain "unverified". Aligns with the whisper compute (higher = more
/// certain); the value is a placeholder pending signed-Mac calibration. `pub(crate)` because the
/// summarizer-feed `[UNCLEAR]` prefix (A3, `pipeline::build_transcript_feed`) uses the SAME
/// threshold — one operating point to calibrate, not two.
pub(crate) const LOW_CONFIDENCE_P: f32 = 0.55;

/// The non-destructive markers appended after an unsupported unit. Both start with `> unverified`,
/// so the idempotency check (`next line is already a marker`) covers either variant.
const UNVERIFIED: &str = "> unverified";
const UNVERIFIED_LOW_AUDIO: &str = "> unverified (low audio confidence)";

/// Annotate `note_markdown`, appending a `> unverified` line after every candidate unit whose content
/// is not supported by `segments`. Deterministic, idempotent, and pure. The YAML front-matter, the
/// `## My notes` / `## Related prior notes` sections (which legitimately hold content NOT in THIS
/// transcript), headings, code fences, existing blockquotes, and wikilink-only lines are all left
/// untouched. With no usable transcript, the note is returned BYTE-IDENTICAL (nothing to verify
/// against — never guess a leak or a false flag).
pub fn annotate_unverified(note_markdown: &str, segments: &[Segment]) -> String {
    // Build the per-segment content-token sets ONCE, dropping empty + assistant-directed spans with
    // the IDENTICAL predicate the summarizer feed uses (`pipeline::build_transcript_feed`) — an
    // assistant command ("Klaudku, …") is not transcript evidence.
    let seg_tokens: Vec<(HashSet<String>, Option<f32>)> = segments
        .iter()
        .filter(|s| {
            let t = s.text.trim();
            !t.is_empty() && !crate::audio::wake::is_assistant_directed(t)
        })
        .map(|s| (content_tokens(&s.text), s.confidence))
        .collect();

    // The global support set: every content token spoken anywhere in the (filtered) transcript.
    let transcript: HashSet<String> = seg_tokens
        .iter()
        .flat_map(|(set, _)| set.iter().cloned())
        .collect();

    // Nothing to ground against ⇒ return byte-identical (can't verify against an empty transcript).
    if transcript.is_empty() {
        return note_markdown.to_string();
    }

    // Split off the YAML front-matter (never annotated); `body` is what we walk.
    let (front, body) = split_frontmatter(note_markdown);

    let lines: Vec<&str> = body.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 8);
    let mut in_code_fence = false;
    let mut in_skipped_section = false;

    for (i, &line) in lines.iter().enumerate() {
        // Always keep the original line verbatim (non-destructive).
        out.push(line.to_string());
        let trimmed = line.trim();

        // Code fences: toggle state; never annotate the fence line or anything inside a fence.
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_fence = !in_code_fence;
            continue;
        }
        if in_code_fence {
            continue;
        }

        // Headings: re-evaluate the skip-section state from THIS heading's own title, then move on
        // (a heading is never a candidate unit). A `## My notes` / `## Related prior notes` heading
        // enters a skipped section; any other heading leaves it.
        if trimmed.starts_with('#') {
            in_skipped_section = is_skipped_heading(trimmed);
            continue;
        }
        if in_skipped_section {
            continue;
        }

        // Non-candidate lines: blanks, existing blockquotes (incl. our own markers → idempotency).
        if trimmed.is_empty() || trimmed.starts_with('>') {
            continue;
        }

        // Strip the list/checkbox marker so it doesn't pollute tokens; skip wikilink-only citations.
        let unit = unit_content(trimmed);
        if is_wikilink_only(unit) {
            continue;
        }

        let toks = content_tokens(unit);
        if toks.len() < GROUND_MIN_CONTENT_TOKENS {
            continue;
        }
        let covered = toks.iter().filter(|t| transcript.contains(*t)).count();
        let coverage = covered as f64 / toks.len() as f64;
        if coverage >= GROUND_MIN_COVERAGE {
            continue;
        }

        // Idempotency: if the next non-empty ORIGINAL line is already a `> unverified` marker, skip
        // (mirrors `repair_is_idempotent` — running the pass twice is a no-op).
        if next_nonempty_is_marker(&lines, i + 1) {
            continue;
        }

        // ASR-confidence surface: when the unit's best-overlapping segments are ALL known-low
        // confidence, name the likely cause; otherwise a plain "unverified".
        let marker = if best_overlap_all_low_confidence(&toks, &seg_tokens) {
            UNVERIFIED_LOW_AUDIO
        } else {
            UNVERIFIED
        };
        out.push(marker.to_string());
    }

    let annotated_body = out.join("\n");
    match front {
        Some(fm) => format!("{fm}{annotated_body}"),
        None => annotated_body,
    }
}

/// Distinct, lowercased, stopword-stripped content tokens (reusing the retrieval tokenizer +
/// stopword list — one source of truth, PL-aware).
fn content_tokens(s: &str) -> HashSet<String> {
    tokenize(s)
        .into_iter()
        .filter(|t| !is_stopword(t))
        .collect()
}

/// Strip a leading list / checklist / numbered marker from a trimmed line so the marker glyphs
/// (`- [ ]`, `* `, `1. `) never count as content tokens. Returns the remainder verbatim.
fn unit_content(trimmed: &str) -> &str {
    for cb in [
        "- [ ] ", "- [x] ", "- [X] ", "* [ ] ", "* [x] ", "* [X] ", "+ [ ] ", "+ [x] ", "+ [X] ",
    ] {
        if let Some(r) = trimmed.strip_prefix(cb) {
            return r;
        }
    }
    for b in ["- ", "* ", "+ "] {
        if let Some(r) = trimmed.strip_prefix(b) {
            return r;
        }
    }
    // Numbered list marker: leading ASCII digits then ". " or ") ".
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 {
        let after = &trimmed[digits..];
        if let Some(r) = after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))
        {
            return r;
        }
    }
    trimmed
}

/// True when `content` is ONLY `[[wikilink]]` citations (plus punctuation/whitespace) — a link line
/// legitimately references another note's title, which is not spoken in THIS transcript, so it must
/// never be flagged. Detected by removing every `[[…]]` span and checking the remainder for any
/// alphanumeric content.
fn is_wikilink_only(content: &str) -> bool {
    let mut remainder = String::new();
    let mut rest = content;
    while let Some(open) = rest.find("[[") {
        remainder.push_str(&rest[..open]);
        match rest[open + 2..].find("]]") {
            Some(close_rel) => rest = &rest[open + 2 + close_rel + 2..],
            None => {
                // Unbalanced `[[` — keep the tail so a stray bracket alone isn't treated as a link.
                remainder.push_str(&rest[open..]);
                rest = "";
                break;
            }
        }
    }
    remainder.push_str(rest);
    !remainder.chars().any(|c| c.is_alphanumeric())
}

/// The lowercased, `#`-stripped title of a heading line (`"## My Notes"` → `"my notes"`). One
/// source of truth for the EN+PL heading matchers.
fn section_title(trimmed_heading: &str) -> String {
    trimmed_heading
        .trim_start_matches('#')
        .trim()
        .to_lowercase()
}

/// Whether a heading opens a PROTECTED section that legitimately carries content NOT in THIS
/// transcript (the user's typed `## My notes`, the `## Related prior notes` grounding corpus, an
/// `## Also discussed` recap) — those must NEVER be flagged unverified NOR have a unit removed by
/// the anti-bleed pass. EN + PL, case-insensitive, suffix-tolerant.
fn is_skipped_heading(trimmed_heading: &str) -> bool {
    let title = section_title(trimmed_heading);
    const KEYS: &[&str] = &[
        // English
        "my notes",
        "related prior notes",
        "also discussed",
        // Polish
        "moje notatki",
        "powiązane notatki",
        "pozostałe",
    ];
    KEYS.iter().any(|k| title.starts_with(k))
}

/// Whether the first non-empty line at/after `start` is already a `> unverified` marker (either
/// variant). Guards idempotency.
fn next_nonempty_is_marker(lines: &[&str], start: usize) -> bool {
    lines
        .get(start..)
        .and_then(|rest| rest.iter().find(|l| !l.trim().is_empty()))
        .map(|l| l.trim().starts_with(UNVERIFIED))
        .unwrap_or(false)
}

/// Whether the segments that OVERLAP a unit the most were all acoustically shaky. Returns true only
/// when there IS overlapping support AND every max-overlap segment has a KNOWN confidence below
/// [`LOW_CONFIDENCE_P`] (a `None` confidence is UNKNOWN, not low — we never over-claim "low audio").
fn best_overlap_all_low_confidence(
    unit_tokens: &HashSet<String>,
    seg_tokens: &[(HashSet<String>, Option<f32>)],
) -> bool {
    let mut best = 0usize;
    let mut best_confs: Vec<Option<f32>> = Vec::new();
    for (toks, conf) in seg_tokens {
        let overlap = unit_tokens.iter().filter(|t| toks.contains(*t)).count();
        if overlap == 0 {
            continue;
        }
        if overlap > best {
            best = overlap;
            best_confs.clear();
            best_confs.push(*conf);
        } else if overlap == best {
            best_confs.push(*conf);
        }
    }
    !best_confs.is_empty()
        && best_confs
            .iter()
            .all(|c| c.map(|v| v < LOW_CONFIDENCE_P).unwrap_or(false))
}

/// Split a note into `(front_matter, body)`. The front-matter is a leading `---\n … \n---\n` block
/// (mirrors `export::inject_provenance_frontmatter`'s split); it is returned whole so a re-join is
/// byte-exact and is NEVER annotated. A note without front-matter is all body. A note that is ONLY
/// front-matter (or a malformed unterminated block) yields an empty body ⇒ no annotation.
fn split_frontmatter(md: &str) -> (Option<&str>, &str) {
    let Some(rest) = md.strip_prefix("---\n") else {
        return (None, md);
    };
    let open = "---\n".len();
    if let Some(pos) = rest.find("\n---\n") {
        let end = open + pos + "\n---\n".len();
        (Some(&md[..end]), &md[end..])
    } else {
        // Opened but never closed with content after it (whole-note front-matter or malformed) — be
        // conservative: treat all of it as front-matter, leaving an empty body (nothing annotated).
        (Some(md), "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(text: &str, confidence: Option<f32>) -> Segment {
        Segment {
            idx: 0,
            start_s: 0.0,
            end_s: 1.0,
            text: text.into(),
            speaker: Some("others".into()),
            confidence,
        }
    }

    /// RED-before-GREEN: an action item the transcript never supports gets a following `> unverified`
    /// line, while a SUPPORTED summary sentence stays clean. Reverting `annotate_unverified` to a
    /// no-op drops the marker (RED). The marker here is the PLAIN variant (no overlapping segment).
    #[test]
    fn unsupported_action_item_gets_unverified() {
        let segments = vec![
            seg(
                "we shipped the login page and the payment flow this week",
                None,
            ),
            seg("the database migration is done", None),
        ];
        let note = "## Summary\n\nThe team shipped the login page.\n\n## Action items\n\n- [ ] Anna own the rollout by Friday\n";
        let out = annotate_unverified(note, &segments);

        // The fabricated action item (anna/own/rollout/friday absent from the transcript) is flagged.
        assert!(
            out.contains("- [ ] Anna own the rollout by Friday\n> unverified\n"),
            "unsupported action item must gain a plain `> unverified` line; got:\n{out}"
        );
        // No overlapping segment ⇒ NOT the low-audio variant.
        assert!(
            !out.contains("low audio confidence"),
            "no segment overlaps ⇒ plain marker"
        );
        // The supported summary sentence (login/page/shipped are in the transcript) stays clean.
        assert!(
            out.contains("The team shipped the login page.\n\n## Action items"),
            "a supported sentence must not be annotated; got:\n{out}"
        );
    }

    /// Running the pass twice is byte-identical to running it once (mirrors `repair_is_idempotent`).
    #[test]
    fn annotate_is_idempotent() {
        let segments = vec![seg("we shipped the login page this week", None)];
        let note = "## Action items\n\n- [ ] Zenon rebuild the reactor core tomorrow\n";
        let once = annotate_unverified(note, &segments);
        let twice = annotate_unverified(&once, &segments);
        assert_eq!(once, twice, "annotate must be idempotent");
        // And it actually annotated on the first pass (guards a vacuous idempotency).
        assert!(once.contains("> unverified"));
    }

    /// No false positives: front-matter, headings, wikilink-only lines, pre-existing blockquotes, and
    /// the `## My notes` section are all left byte-identical even though their words are NOT in the
    /// transcript. RED if the wikilink-only skip or the section skip regresses.
    #[test]
    fn frontmatter_headings_wikilinks_and_user_sections_untouched() {
        let segments = vec![seg("completely different words were spoken aloud", None)];
        let note = "---\ntitle: Test\ntags: [a]\n---\n\n# Summary\n\n[[Some Wikilink Page Reference Title Here]]\n\n> a pre-existing quote with several unsupported words indeed present\n\n## My notes\n\nmy typed personal reminder that never appears in the transcript whatsoever\n";
        let out = annotate_unverified(note, &segments);
        assert_eq!(
            out, note,
            "front-matter / headings / wikilink-only / blockquote / `## My notes` must be untouched"
        );
    }

    /// With no usable transcript (empty slice, or only assistant-directed spans), the note is
    /// returned byte-identical — grounding never invents a flag when it has nothing to verify against.
    #[test]
    fn empty_or_assistant_only_transcript_yields_no_annotations() {
        let note = "## Summary\n\n- [ ] some unsupported action item written here today\n";
        // Empty segments.
        assert_eq!(annotate_unverified(note, &[]), note);
        // Only an assistant-directed command → excluded → transcript empty → unchanged.
        let cmd = vec![seg("Klaudku, sprawdź pogodę w Warszawie na jutro", None)];
        assert_eq!(annotate_unverified(note, &cmd), note);
    }

    /// Parse safety: after annotation, `action_items::parse_action_items` returns the SAME count and
    /// owners — the `- [ ]` lines stay byte-identical and the appended `> unverified` line is not a
    /// checklist item, so task parsing/patching is unaffected.
    #[test]
    fn action_item_parsing_unchanged_after_annotation() {
        use crate::summarize::action_items::parse_action_items;
        let segments = vec![seg(
            "we agreed to ship the invoice export next sprint",
            None,
        )];
        let note = "## Action items\n\n- [ ] Bob — ship the invoice export next sprint\n- [ ] Zoltan — colonize the moon by Q3\n";
        let before = parse_action_items(note);
        let out = annotate_unverified(note, &segments);

        // The supported item stays clean; the fabricated one is flagged.
        assert!(out.contains("Zoltan — colonize the moon by Q3\n> unverified"));
        assert!(!out.contains("invoice export next sprint\n> unverified"));

        let after = parse_action_items(&out);
        assert_eq!(before.len(), after.len(), "task count must be unchanged");
        let owners_before: Vec<_> = before.iter().map(|a| a.owner.clone()).collect();
        let owners_after: Vec<_> = after.iter().map(|a| a.owner.clone()).collect();
        assert_eq!(owners_before, owners_after, "task owners must be unchanged");
    }

    /// ASR-confidence surface (A → B): a flagged unit whose best-overlapping segment was acoustically
    /// shaky (`confidence < LOW_CONFIDENCE_P`) gets the `(low audio confidence)` variant, landing
    /// whisper's confidence signal verbatim in the exported note.
    #[test]
    fn low_confidence_best_overlap_names_the_cause() {
        // The one segment overlaps the unit on "quarterly" but was decoded at low confidence.
        let segments = vec![seg(
            "mumbling something about the quarterly figures",
            Some(0.30),
        )];
        let note =
            "## Summary\n\nThe quarterly revenue tripled after the acquisition finally closed.\n";
        let out = annotate_unverified(note, &segments);
        assert!(
            out.contains("> unverified (low audio confidence)"),
            "a flagged unit whose overlap is all-low-confidence must name the cause; got:\n{out}"
        );
    }

    /// A high-confidence overlapping segment on an otherwise-unsupported unit yields the PLAIN marker
    /// (the ASR was sure; the model still over-reached) — proves the low-audio branch is confidence-
    /// gated, not just overlap-gated.
    #[test]
    fn high_confidence_overlap_stays_plain() {
        let segments = vec![seg("something about the quarterly figures", Some(0.95))];
        let note =
            "## Summary\n\nThe quarterly revenue tripled after the acquisition finally closed.\n";
        let out = annotate_unverified(note, &segments);
        assert!(out.contains("> unverified"));
        assert!(
            !out.contains("low audio confidence"),
            "a confident overlap must not be blamed on audio; got:\n{out}"
        );
    }

    /// A too-short unit (below `GROUND_MIN_CONTENT_TOKENS` distinct content tokens) is never flagged,
    /// even if unsupported — avoids annotating bare labels / two-word bullets.
    #[test]
    fn short_unit_is_not_flagged() {
        let segments = vec![seg(
            "we discussed the budget and the roadmap at length",
            None,
        )];
        let note = "## Summary\n\n- Xanadu teleporter\n";
        let out = annotate_unverified(note, &segments);
        assert_eq!(
            out, note,
            "a 2-token unsupported bullet is below the min and stays clean"
        );
    }
}
