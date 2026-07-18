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

use serde::{Deserialize, Serialize};

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

// ── RECEIPTS (Brain v3 PR-5) ─────────────────────────────────────────────────────────────────────
//
// The GROUNDING pass above answers "is this claim supported?" with a `> unverified` marker. Receipts
// answer the dual question — "WHERE (which second of audio) did this claim come from?" — using the
// SAME deterministic token-overlap math, no LLM, no egress. For every candidate note line (a bullet,
// a checklist item, a prose sentence) we find the transcript segment it overlaps most, and emit that
// segment's audio coordinates (`start_s`/`end_s`), speaker, and ASR confidence so the UI can seek the
// already-shipped audio player/timeline to that second and prove the claim.
//
// PURE + on-device: `align_claims_to_segments` reads NOTHING but the passed note lines + segments —
// no DB, no clock, no LLM. The command wrapper (`commands::get_note_receipts`) is what gates the read.
// Timestamps are RAW SECONDS straight off `Segment` (never a formatted MM:SS string — the 2h-wrap bug
// the perf work fought), so the FE seeks with a plain `float` and formats for display itself.

/// The minimum token-overlap ratio (overlapping distinct content tokens / the claim's distinct
/// content tokens) for a claim to earn a receipt. Below this the claim is left UN-aligned (no
/// `ClaimAlignment` emitted) rather than pointing the user at a weakly-related second of audio — a
/// wrong receipt is worse than none. Chosen to match `GROUND_MIN_COVERAGE` (the same "half its words
/// were actually spoken" bar the grounding pass already uses); calibration of the operating point on
/// paraphrased LLM lines is a dev-app follow-up, not provable by `cargo test --lib`.
const RECEIPT_MIN_OVERLAP: f32 = 0.5;

/// One claim → the transcript segment it most likely derives from. Emitted only for note lines that
/// clear [`RECEIPT_MIN_OVERLAP`]; a paraphrased/unsupported line gets no entry (the FE renders no
/// receipt chip for it). Serialized camelCase to match `Segment`'s FE convention. Carries NO note or
/// transcript TEXT — only the claim's line index, the segment's audio coordinates, and non-content
/// metadata (speaker label + ASR probability) — so the DTO is safe to hand a caller that has already
/// passed the read gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimAlignment {
    /// Index of the claim in the `note_lines` slice passed to [`align_claims_to_segments`] (the FE
    /// maps this back to the rendered note line to place the receipt chip).
    pub claim_index: usize,
    /// `Segment.idx` of the best-matching transcript segment (stable id the FE can flash/highlight).
    pub segment_id: i64,
    /// Segment start in RAW SECONDS — the audio player/timeline seek target.
    pub start_s: f64,
    /// Segment end in RAW SECONDS.
    pub end_s: f64,
    /// `"me"` / `"others"` / `None` — straight from the segment (no new field, no diarization).
    pub speaker: Option<String>,
    /// Per-segment ASR confidence in `[0,1]`, or `None` when whisper did not compute it. Straight
    /// from the segment; the FE shows it in the receipt tooltip.
    pub confidence: Option<f32>,
    /// The token-overlap ratio that won this alignment (`[RECEIPT_MIN_OVERLAP, 1.0]`) — lets the FE
    /// tier receipt strength if it wants to.
    pub overlap: f32,
}

/// Deterministically align each candidate note line to the transcript segment it most likely derives
/// from, by normalized content-token overlap (the SAME `content_tokens` math the grounding pass uses).
///
/// - `note_lines`: the note's RAW lines are fine — a LEADING `---` YAML front-matter block is
///   skipped here (metadata like `title:`/`attendees:` is never a claim, even when the attendees'
///   names are spoken), and this fn also skips headings / code fences / blockquotes / wikilink-only
///   / protected sections / too-short lines itself. Skipping (not re-slicing) keeps every emitted
///   `claim_index` in the ORIGINAL numbering of the passed lines.
/// - For each candidate line: score overlap against every non-empty, non-assistant-directed segment;
///   pick the segment with the MOST overlapping distinct content tokens (ties → the EARLIEST segment,
///   for determinism); emit a [`ClaimAlignment`] iff `overlapping / claim_tokens >= RECEIPT_MIN_OVERLAP`.
/// - Below threshold ⇒ NO entry for that line (a paraphrased LLM line the transcript doesn't clearly
///   support gets no — possibly wrong — receipt).
/// - POLARITY VETO: a segment whose negator presence mismatches the claim's (see
///   [`NEGATOR_TOKENS`]) can never be its receipt — "we will NOT ship X" must not "prove"
///   "we will ship X" just because "not" is a stopword.
///
/// Pure: no DB, no clock, no LLM, no I/O. Deterministic: same inputs ⇒ byte-identical output.
pub fn align_claims_to_segments(note_lines: &[&str], segments: &[Segment]) -> Vec<ClaimAlignment> {
    // Per-segment content-token set + negator flag, keeping the segment's audio coordinates +
    // metadata. Filtered with the IDENTICAL predicate as the grounding pass (drop empty +
    // assistant-directed spans — an assistant command is not transcript evidence to point a
    // receipt at).
    let seg_index: Vec<(HashSet<String>, bool, &Segment)> = segments
        .iter()
        .filter(|s| {
            let t = s.text.trim();
            !t.is_empty() && !crate::audio::wake::is_assistant_directed(t)
        })
        .map(|s| (content_tokens(&s.text), has_negator(&s.text), s))
        .collect();

    if seg_index.is_empty() {
        return Vec::new();
    }

    // A leading YAML front-matter block is metadata, never a claim. Skipped by index (not
    // re-sliced) so claim indices keep the original numbering the FE renders.
    let fm_end = frontmatter_end(note_lines);

    let mut out: Vec<ClaimAlignment> = Vec::new();
    let mut in_code_fence = false;
    let mut in_skipped_section = false;

    for (i, &line) in note_lines.iter().enumerate() {
        if i < fm_end {
            continue;
        }
        let trimmed = line.trim();

        // Code fences: toggle state; never a claim.
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_fence = !in_code_fence;
            continue;
        }
        if in_code_fence {
            continue;
        }
        // Headings re-evaluate the protected-section state; a heading is never a claim.
        if trimmed.starts_with('#') {
            in_skipped_section = is_skipped_heading(trimmed);
            continue;
        }
        if in_skipped_section {
            continue;
        }
        // Blanks, blockquotes (incl. our own `> unverified` markers) — never a claim.
        if trimmed.is_empty() || trimmed.starts_with('>') {
            continue;
        }

        // Strip the list/checkbox marker; skip wikilink-only citation lines.
        let unit = unit_content(trimmed);
        if is_wikilink_only(unit) {
            continue;
        }
        let toks = content_tokens(unit);
        if toks.len() < GROUND_MIN_CONTENT_TOKENS {
            continue;
        }

        let claim_negated = has_negator(unit);

        // Best-matching segment = most overlapping distinct content tokens; ties → EARLIEST (first
        // wins because we only replace on a STRICTLY greater overlap). Deterministic. A candidate
        // whose negator presence mismatches the claim's is VETOED outright (it can never be the
        // receipt) — negators are stopwords, so without this a claim and its negation are
        // token-identical and a receipt could "prove" the opposite of what was said. A
        // polarity-CONSISTENT weaker segment may still win.
        let mut best_overlap = 0usize;
        let mut best: Option<&Segment> = None;
        for (seg_toks, seg_negated, seg) in &seg_index {
            if *seg_negated != claim_negated {
                continue;
            }
            let overlap = toks.iter().filter(|t| seg_toks.contains(*t)).count();
            if overlap > best_overlap {
                best_overlap = overlap;
                best = Some(seg);
            }
        }

        let Some(seg) = best else { continue };
        let ratio = best_overlap as f32 / toks.len() as f32;
        if ratio < RECEIPT_MIN_OVERLAP {
            continue; // paraphrase / unsupported ⇒ no (possibly wrong) receipt.
        }

        out.push(ClaimAlignment {
            claim_index: i,
            segment_id: seg.idx,
            start_s: seg.start_s,
            end_s: seg.end_s,
            speaker: seg.speaker.clone(),
            confidence: seg.confidence,
            overlap: ratio,
        });
    }

    out
}

/// Distinct, lowercased, stopword-stripped content tokens (reusing the retrieval tokenizer +
/// stopword list — one source of truth, PL-aware).
fn content_tokens(s: &str) -> HashSet<String> {
    tokenize(s)
        .into_iter()
        .filter(|t| !is_stopword(t))
        .collect()
}

/// Negator tokens for the receipts polarity veto — a deterministic, documented HEURISTIC used by
/// the receipts pass ONLY (retrieval's tokenizer/stopwords are untouched). Standalone EN + PL
/// forms; the EN contractions (can't / won't / don't / doesn't / didn't / isn't / aren't / wasn't
/// / wouldn't / shouldn't) are covered by the `n't` substring scan in [`has_negator`], because
/// `tokenize` splits on the apostrophe (`won't` → `won`,`t`) so no listed token could ever match
/// them — and their stems (`won`, `can`) are common affirmative words we must not flag.
const NEGATOR_TOKENS: &[&str] = &[
    // English
    "not", "no", "never", "none", "cannot",
    // Polish
    "nie", "nigdy", "żaden", "żadna", "żadne", "bez",
];

/// Whether `text` carries at least one negator: an `n't` contraction (ASCII or typographic
/// apostrophe) in the raw lowercased text, or a [`NEGATOR_TOKENS`] token. Pure + deterministic.
fn has_negator(text: &str) -> bool {
    let lower = text.to_lowercase();
    if lower.contains("n't") || lower.contains("n’t") {
        return true;
    }
    tokenize(text)
        .iter()
        .any(|t| NEGATOR_TOKENS.contains(&t.as_str()))
}

/// The number of leading lines occupied by a YAML front-matter block (a `---` fence on line 0
/// through the closing `---`), or 0 when there is none. Mirrors [`split_frontmatter`]'s semantics
/// over a pre-split line slice, including the conservative unterminated case (an opened-but-never-
/// closed block makes EVERY line front-matter — never chip a line we cannot prove is body).
fn frontmatter_end(lines: &[&str]) -> usize {
    if lines.first().copied() != Some("---") {
        return 0;
    }
    match lines[1..].iter().position(|l| *l == "---") {
        Some(close) => close + 2, // fence line 0 + offset into the tail + the closing fence
        None => lines.len(),
    }
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

    /// A fully-specified segment for the receipts (`align_claims_to_segments`) tests: distinct
    /// idx/timestamps/speaker/confidence so a receipt's audio coordinates can be asserted exactly.
    fn seg_full(
        idx: i64,
        start_s: f64,
        end_s: f64,
        speaker: &str,
        confidence: Option<f32>,
        text: &str,
    ) -> Segment {
        Segment {
            idx,
            start_s,
            end_s,
            text: text.into(),
            speaker: Some(speaker.into()),
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

    // ── RECEIPTS (align_claims_to_segments) ──────────────────────────────────────────────────────

    /// RED-before-GREEN: a claim whose words are largely IN a segment aligns to THAT segment, carrying
    /// its exact audio coordinates + speaker + confidence. Reverting `align_claims_to_segments` to
    /// return `Vec::new()` drops the receipt (RED). Also proves the best-match picks the RIGHT segment
    /// (the login one, not the migration one) and the claim_index points at the real claim line (not a
    /// heading/blank).
    #[test]
    fn exact_overlap_claim_aligns_to_its_segment() {
        let segments = vec![
            seg_full(
                7,
                12.5,
                18.0,
                "me",
                Some(0.91),
                "we finally shipped the login page and the payment flow this week",
            ),
            seg_full(
                8,
                40.0,
                47.0,
                "others",
                Some(0.80),
                "the database migration is completely done now",
            ),
        ];
        // Line indices: 0 heading, 1 blank, 2 the claim.
        let lines: Vec<&str> = "## Summary\n\nWe shipped the login page and the payment flow."
            .split('\n')
            .collect();
        let out = align_claims_to_segments(&lines, &segments);

        assert_eq!(out.len(), 1, "exactly one claim line aligns; got:\n{out:?}");
        let a = &out[0];
        assert_eq!(a.claim_index, 2, "claim_index points at the real claim line");
        assert_eq!(a.segment_id, 7, "aligned to the LOGIN segment, not migration");
        assert_eq!(a.start_s, 12.5, "raw-seconds start seek target");
        assert_eq!(a.end_s, 18.0);
        assert_eq!(a.speaker.as_deref(), Some("me"));
        assert_eq!(a.confidence, Some(0.91));
        assert!(
            a.overlap >= RECEIPT_MIN_OVERLAP,
            "overlap ratio clears the receipt threshold"
        );
    }

    /// A paraphrased claim whose distinct content tokens fall BELOW the overlap threshold earns NO
    /// receipt (better none than a wrong second of audio). RED if the threshold is dropped/removed.
    #[test]
    fn paraphrase_below_threshold_gets_no_alignment() {
        let segments = vec![seg_full(
            1,
            5.0,
            9.0,
            "others",
            None,
            "we should probably revisit the pricing tiers next quarter",
        )];
        // Shares only "quarter" (1 of 4 distinct content tokens after stopword-strip) → ratio < 0.5.
        let lines: Vec<&str> =
            "The acquisition negotiations concluded successfully this quarter."
                .split('\n')
                .collect();
        let out = align_claims_to_segments(&lines, &segments);
        assert!(
            out.is_empty(),
            "a below-threshold paraphrase must earn no receipt; got:\n{out:?}"
        );
    }

    /// Determinism: same inputs ⇒ byte-identical output. And on an overlap TIE, the EARLIEST segment
    /// wins (first-seen kept because we only replace on strictly-greater overlap).
    #[test]
    fn alignment_is_deterministic_and_ties_pick_earliest() {
        // Both segments overlap the claim on the SAME two tokens (roadmap, budget) — a tie.
        let segments = vec![
            seg_full(3, 1.0, 4.0, "me", None, "we reviewed the roadmap and the budget"),
            seg_full(4, 30.0, 34.0, "others", None, "roadmap and budget again later on"),
        ];
        let lines: Vec<&str> = "We reviewed the roadmap and the budget in detail."
            .split('\n')
            .collect();
        let first = align_claims_to_segments(&lines, &segments);
        let second = align_claims_to_segments(&lines, &segments);
        assert_eq!(first, second, "same inputs must give identical output");
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0].segment_id, 3,
            "an overlap tie resolves to the EARLIEST segment"
        );
    }

    /// Headings, blanks, code fences, blockquotes, wikilink-only lines and too-short bullets are never
    /// claims — a receipt's claim_index always refers to a REAL claim line.
    #[test]
    fn non_claim_lines_never_get_receipts() {
        let segments = vec![seg_full(
            1,
            0.0,
            5.0,
            "me",
            None,
            "we shipped the invoice export feature and the reporting dashboard",
        )];
        let note = "# Heading with shipped invoice export reporting words\n\n> a blockquote mentioning shipped invoice export dashboard\n\n[[Shipped Invoice Export Reporting Note]]\n\n- ok\n\n```\nshipped invoice export reporting dashboard code\n```\n\nWe shipped the invoice export and the reporting dashboard.";
        let lines: Vec<&str> = note.split('\n').collect();
        let out = align_claims_to_segments(&lines, &segments);
        assert_eq!(out.len(), 1, "only the real prose claim aligns; got:\n{out:?}");
        assert_eq!(
            lines[out[0].claim_index].trim(),
            "We shipped the invoice export and the reporting dashboard."
        );
    }

    /// With no usable transcript (empty, or only assistant-directed spans) there are no receipts —
    /// never point a chip at nothing.
    #[test]
    fn empty_or_assistant_only_segments_yield_no_receipts() {
        let lines: Vec<&str> = "We shipped the login page and the payment flow today."
            .split('\n')
            .collect();
        assert!(align_claims_to_segments(&lines, &[]).is_empty());
        let cmd = vec![seg_full(
            1,
            0.0,
            3.0,
            "me",
            None,
            "Klaudku, shipped the login page and payment flow",
        )];
        assert!(
            align_claims_to_segments(&lines, &cmd).is_empty(),
            "assistant-directed spans are not transcript evidence for a receipt"
        );
    }

    /// RED-before-GREEN (audit MED, front-matter receipts): YAML front-matter lines (`title:`,
    /// `attendees:`) are METADATA, not claims — even when the attendees' names are spoken in the
    /// meeting they must never earn a receipt chip. Before the fix the walk treated every raw line
    /// as a claim, so `attendees: Anna, Bob, …` aligned to the segment that spoke those names.
    /// The body claim's `claim_index` must keep the ORIGINAL note's line numbering (the FE maps it
    /// straight into `markdown.split('\n')`).
    #[test]
    fn frontmatter_lines_never_earn_receipts() {
        let segments = vec![seg_full(
            1,
            10.0,
            16.0,
            "me",
            Some(0.9),
            "anna and bob agreed the quarterly budget review is approved",
        )];
        // Raw note exactly as the FE renders it: lines 0..=3 are the front-matter block,
        // line 5 the heading, line 7 the real claim.
        let note = "---\ntitle: Budget sync\nattendees: Anna, Bob, quarterly budget\n---\n\n## Summary\n\nAnna and Bob approved the quarterly budget review.";
        let lines: Vec<&str> = note.split('\n').collect();
        let out = align_claims_to_segments(&lines, &segments);
        assert!(
            out.iter().all(|a| a.claim_index > 3),
            "front-matter lines (0..=3) must never earn a receipt; got:\n{out:?}"
        );
        assert_eq!(
            out.len(),
            1,
            "the real body claim still earns its receipt; got:\n{out:?}"
        );
        assert_eq!(
            out[0].claim_index, 7,
            "claim_index keeps the ORIGINAL note's line numbering"
        );
    }

    /// Conservative unterminated front-matter (mirrors `split_frontmatter`): a note that OPENS a
    /// `---` block and never closes it is all metadata — no receipts, rather than chipping lines
    /// we cannot prove are body.
    #[test]
    fn unterminated_frontmatter_yields_no_receipts() {
        let segments = vec![seg_full(
            1,
            0.0,
            5.0,
            "me",
            None,
            "anna and bob agreed the quarterly budget review is approved",
        )];
        let note = "---\ntitle: Budget sync\nAnna and Bob approved the quarterly budget review.";
        let lines: Vec<&str> = note.split('\n').collect();
        assert!(
            align_claims_to_segments(&lines, &segments).is_empty(),
            "an unterminated front-matter block must yield no receipts"
        );
    }

    /// RED-before-GREEN (audit MED, negation-blind alignment fails UNSAFE): "not" is a stopword,
    /// so a claim and its negation are token-identical — before the veto, the affirmative claim
    /// "we will ship X in May" earned a receipt from the segment "we will NOT ship X in May",
    /// i.e. the receipt "proved" the opposite of what was said. Polarity mismatch ⇒ no receipt.
    #[test]
    fn negation_mismatch_vetoes_the_receipt() {
        let segments = vec![seg_full(
            2,
            33.0,
            37.0,
            "others",
            Some(0.9),
            "we will not ship the billing exporter in May",
        )];
        let lines: Vec<&str> = "We will ship the billing exporter in May."
            .split('\n')
            .collect();
        let out = align_claims_to_segments(&lines, &segments);
        assert!(
            out.is_empty(),
            "a negated segment must never receipt an affirmative claim; got:\n{out:?}"
        );
    }

    /// Symmetric polarity is ALLOWED: when the claim and the segment are BOTH negated, the claim
    /// faithfully reports the negation and the receipt stands — proves the veto is a MISMATCH
    /// check, not a blanket negation blocklist.
    #[test]
    fn matching_negation_on_both_sides_keeps_the_receipt() {
        let segments = vec![seg_full(
            2,
            33.0,
            37.0,
            "others",
            Some(0.9),
            "we will not ship the billing exporter in May",
        )];
        let lines: Vec<&str> = "We will not ship the billing exporter in May."
            .split('\n')
            .collect();
        let out = align_claims_to_segments(&lines, &segments);
        assert_eq!(
            out.len(),
            1,
            "both-negated claim+segment must still align; got:\n{out:?}"
        );
        assert_eq!(out[0].segment_id, 2);
    }

    /// RED-before-GREEN (contraction negators): `tokenize` splits on the apostrophe (`won't` →
    /// `won`,`t`), so contraction negators can never match a token list — the veto must scan the
    /// RAW text for the `n't` form. An affirmative claim vs a "won't" segment gets no receipt.
    #[test]
    fn contraction_negator_mismatch_vetoes_the_receipt() {
        let segments = vec![seg_full(
            3,
            50.0,
            55.0,
            "me",
            None,
            "we won't renew the vendor contract this quarter",
        )];
        let lines: Vec<&str> = "We will renew the vendor contract this quarter."
            .split('\n')
            .collect();
        let out = align_claims_to_segments(&lines, &segments);
        assert!(
            out.is_empty(),
            "a contraction-negated segment must never receipt an affirmative claim; got:\n{out:?}"
        );
    }

    /// The Polish negator list binds too: "nigdy nie wdrożymy …" must not receipt the affirmative
    /// "wdrożymy …" claim ("nie" is a PL stopword, so without the veto they are token-compatible).
    #[test]
    fn polish_negator_mismatch_vetoes_the_receipt() {
        let segments = vec![seg_full(
            5,
            60.0,
            66.0,
            "me",
            None,
            "nigdy nie wdrożymy tego eksportu faktur w maju",
        )];
        let lines: Vec<&str> = "Wdrożymy eksport faktur w maju.".split('\n').collect();
        let out = align_claims_to_segments(&lines, &segments);
        assert!(
            out.is_empty(),
            "a PL-negated segment must never receipt an affirmative claim; got:\n{out:?}"
        );
    }
}
