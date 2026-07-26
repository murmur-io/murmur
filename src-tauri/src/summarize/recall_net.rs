//! ACTION-ITEM RECALL NET — a deterministic, ZERO-EGRESS cue scan of THIS meeting's own transcript.
//!
//! Notes already have a PRECISION net: the opt-in grounding pass
//! ([`crate::summarize::grounding::annotate_unverified`]) flags note lines the transcript does NOT
//! support. What was missing is the RECALL half — a commitment the model simply never wrote down
//! ("I'll send the deck by Friday") leaves no trace at all: no marker, no gap, nothing to notice.
//! The whole note is one generation, and `action_items::parse_action_items` can only parse what that
//! generation happened to emit.
//!
//! This pass closes that half WITHOUT a second model call: it scans the meeting's OWN transcript
//! segments for commitment CUES ("I'll", "I will", "can you", "let's", "by Friday", an ISO date, the
//! PL equivalents…), drops every cue-bearing line already covered by a parsed `- [ ]` action item,
//! and appends whatever is left under a clearly-marked, OPT-IN [`MISSED_HEADING`] section.
//!
//! ## Non-destructive, exactly like the grounding pass
//! - The existing note body is emitted BYTE-IDENTICAL; this only APPENDS a trailing section, so
//!   `action_items::parse_action_items` / `patch_tasks_markdown` still see the same lines as before.
//! - The surfaced candidates are PLAIN bullets (`- "…"`), deliberately **not** `- [ ]` checklist
//!   lines: a cue match is a SUSPICION, not a task. Rendering them as checkboxes would feed them
//!   straight back into `parse_action_items` (and Obsidian Tasks), turning a low-precision heuristic
//!   into fake action items — the exact failure this section exists to make visible instead.
//! - Idempotent: a note that already carries the section is returned byte-identical.
//! - Empty result ⇒ the note is returned byte-identical (never an empty section).
//!
//! ## LOCK / EGRESS posture (see `.claude/rules/lock-model.md`)
//! Pure over `(note_markdown, segments)`: no DB, no clock, no LLM, no network — 100% local string
//! ops. It runs INSIDE the pipeline that just produced this note's plaintext, over that meeting's
//! OWN segments (`Db::get_segments`, the same read the grounding pass already makes on its opt-in
//! path) — NO new read path, NO cross-meeting read, NO egress. The appended section becomes part of
//! the note markdown, so it is sealed WITH the note by `seal_store::Db::seal_note` (no plaintext
//! survives a lock) and blanked by the same relock.
//!
//! HONEST RESIDUAL (documented, not hidden): the section quotes SHORT verbatim transcript lines,
//! whereas the rest of a note is model PARAPHRASE. A user who opts in AND then shares that note
//! (`share::envelope::clean_note_body`) would ship those quotes with it — they are not stripped,
//! because the share-egress strips are header-gated managed blocks owned by `enrich.rs` /
//! `share/envelope.rs`. Mitigated here by keeping the feature OFF by default, capping each quote at
//! [`MAX_QUOTE_CHARS`] and the section at [`MAX_MISSED`] entries; a `murmur:recall` managed block
//! with its own share-egress strip is the follow-up that would remove the residual entirely.
//!
//! ## Precision honesty
//! A lexical cue scan is deliberately LOW-precision — it will surface lines that are not real
//! commitments. That is why the section is (a) opt-in, (b) separated at the very end of the note,
//! (c) explicitly labelled as an unverified machine scan, and (d) capped. The cue list + overlap
//! thresholds are conservative defaults; the right operating point (recall vs noise on real
//! meetings) needs the maintainer's gold labels on a real vault — NOT provable by `cargo test
//! --lib`, which locks the LOGIC only.

use std::collections::HashSet;

use crate::summarize::action_items::{find_date, parse_action_items};
use crate::summarize::related_context::{is_stopword, tokenize};
use crate::transcribe::types::Segment;

/// The exact heading of the appended section. Fixed English (like the grounding pass's
/// `> unverified` marker) so the idempotency check and any future reader have ONE literal to match,
/// independent of `note_language`.
pub const MISSED_HEADING: &str = "## Possible missed items";

/// The label above the bullets — states plainly that this is a machine cue scan of the transcript,
/// not model output, so nobody reads a candidate as a confirmed task. A PLAIN blockquote (not an
/// Obsidian `[!callout]`) on purpose: the managed-block strips in `enrich.rs` are header-gated on
/// their own callout headers, and this section must never look like one of them.
const MISSED_PREAMBLE: &str = concat!(
    "> Deterministic transcript scan (opt-in) — NOT model output and NOT verified. ",
    "These lines from this meeting sound like a commitment but are not covered by an action item ",
    "above. Review them and promote the real ones by hand."
);

/// Min DISTINCT SIGNAL tokens a transcript line needs before it can be surfaced. Mirrors
/// `grounding::GROUND_MIN_CONTENT_TOKENS`: a bare "I'll do it" carries too little signal to be a
/// useful reminder, and short lines dominate the false-positive tail.
const MIN_SIGNAL_TOKENS: usize = 3;

/// Tokens that survive the shared stopword list but carry no TOPIC signal here — the fragments a
/// commitment cue leaves behind (`I'll` tokenizes to `i` + `ll`) plus the high-frequency English
/// function words the EN+PL retrieval stoplist does not list. Dropped from a line's token set so
/// that (a) "I'll do it." stays below [`MIN_SIGNAL_TOKENS`] instead of being surfaced as a
/// commitment, and (b) the "already captured by an action item" overlap compares TOPIC words
/// ("send / deck / Friday") rather than being diluted by cue glue. This is the ONE deliberate
/// divergence from `grounding::content_tokens`, and it only ever makes this pass QUIETER.
const LOW_SIGNAL_TOKENS: &[&str] = &[
    "i", "ll", "m", "s", "t", "d", "re", "ve", "we", "you", "it", "its", "he", "she", "them", "us",
    "my", "me", "on", "of", "in", "at", "up", "be", "is", "are", "am", "as", "if", "so", "or",
    "to", "an", "a",
];

/// A cue-bearing line counts as ALREADY CAPTURED when this fraction of its distinct signal tokens
/// appears in a single parsed `- [ ]` action item. Same "half its words are already there" bar the
/// grounding pass uses (`GROUND_MIN_COVERAGE`), so the two passes agree on what "supported" means.
const COVERED_MIN_OVERLAP: f64 = 0.5;

/// Two candidates that share this fraction of tokens are the SAME commitment restated (a speaker
/// repeating themselves across segments) — only the first is surfaced.
const DEDUP_MIN_OVERLAP: f64 = 0.6;

/// Hard cap on surfaced candidates, so a cue-heavy meeting can never bury the note under a wall of
/// quotes. Overflow is COUNTED and reported in the section (never silently truncated).
const MAX_MISSED: usize = 8;

/// Max chars of each quoted transcript line (ellipsized past it). Keeps the section skimmable and
/// keeps the verbatim-quote residual documented above as small as possible.
const MAX_QUOTE_CHARS: usize = 160;

/// Commitment / request / deadline cues, matched against a whitespace-normalized, lowercased,
/// SPACE-PADDED form of the line (see [`normalized_padded`]), so every entry is space-padded here
/// and matches on WORD boundaries only. Punctuation is normalized to spaces first, which is what
/// makes `I'll` → `i ll`, `let's` → `let s` and `action:` → `action` match.
///
/// EN + PL (Murmur's two working languages), with the diacritic-free spellings of the Polish cues
/// listed explicitly — the transcript may carry either. Deliberately a small, auditable literal
/// list: no regex crate, no new dependency, fully deterministic.
const COMMITMENT_CUES: &[&str] = &[
    // ── English: first-person commitment ────────────────────────────────────────────────────────
    " i ll ",
    " i will ",
    " we ll ",
    " we will ",
    " i m going to ",
    " we re going to ",
    " i can do ",
    " i owe ",
    " on me ",
    " i got it ",
    // ── English: request / assignment ───────────────────────────────────────────────────────────
    " can you ",
    " could you ",
    " would you ",
    " please ",
    " let s ",
    " we need to ",
    " i need to ",
    " you need to ",
    " needs to ",
    " make sure ",
    " remind me ",
    " follow up ",
    " action item ",
    " action items ",
    " todo ",
    " take care of ",
    " get back to you ",
    // ── English: deadline ───────────────────────────────────────────────────────────────────────
    " deadline ",
    " due date ",
    " by tomorrow ",
    " by monday ",
    " by tuesday ",
    " by wednesday ",
    " by thursday ",
    " by friday ",
    " by saturday ",
    " by sunday ",
    " by end of ",
    " next week ",
    " asap ",
    // ── Polish: first-person commitment ─────────────────────────────────────────────────────────
    " zrobię ",
    " zrobie ",
    " wyślę ",
    " wysle ",
    " przygotuję ",
    " przygotuje ",
    " sprawdzę ",
    " sprawdze ",
    " dam znać ",
    " dam znac ",
    " odezwę się ",
    " odezwe sie ",
    " zajmę się ",
    " zajme sie ",
    " biorę na siebie ",
    " biore na siebie ",
    // ── Polish: request / assignment ────────────────────────────────────────────────────────────
    " czy możesz ",
    " czy mozesz ",
    " proszę ",
    " prosze ",
    " musimy ",
    " muszę ",
    " musze ",
    " trzeba ",
    " zróbmy ",
    " zrobmy ",
    " ustalmy ",
    " przypomnij ",
    " przygotuj ",
    " wyślij ",
    " wyslij ",
    " sprawdź ",
    " sprawdz ",
    " zadanie ",
    // ── Polish: deadline ────────────────────────────────────────────────────────────────────────
    " termin ",
    " do jutra ",
    " do poniedziałku ",
    " do poniedzialku ",
    " do wtorku ",
    " do środy ",
    " do srody ",
    " do czwartku ",
    " do piątku ",
    " do piatku ",
    " do soboty ",
    " do niedzieli ",
    " na jutro ",
    " w przyszłym tygodniu ",
    " w przyszlym tygodniu ",
];

/// Append the [`MISSED_HEADING`] section to `note_markdown` for every cue-bearing line in `segments`
/// that no parsed `- [ ]` action item already covers. Deterministic, idempotent and pure; returns
/// the note BYTE-IDENTICAL when there is nothing to add (no segments, no cue hit, everything already
/// captured, or the section is already present).
pub fn append_possible_missed_items(note_markdown: &str, segments: &[Segment]) -> String {
    // Idempotency: the pass already ran on this markdown (e.g. a re-summarize over a stored note).
    if has_missed_section(note_markdown) {
        return note_markdown.to_string();
    }

    // What the note ALREADY captured: one content-token set per parsed checklist item. Note that
    // `parse_action_items` drops assistant-directed lines ("Klaudku, …") — the candidate side below
    // filters the identical class out of the transcript, so the two sides stay consistent.
    let captured: Vec<HashSet<String>> = parse_action_items(note_markdown)
        .into_iter()
        .map(|it| signal_tokens(&it.text))
        .filter(|t| !t.is_empty())
        .collect();

    let mut picked: Vec<(String, HashSet<String>)> = Vec::new();
    let mut overflow = 0usize;

    for seg in segments {
        let text = seg.text.trim();
        // Same exclusion the summarizer feed + the grounding pass use: a line the user spoke TO the
        // in-meeting assistant is a voice command, never a commitment to a person.
        if text.is_empty() || crate::audio::wake::is_assistant_directed(text) {
            continue;
        }
        if !carries_commitment_cue(text) {
            continue;
        }
        let toks = signal_tokens(text);
        if toks.len() < MIN_SIGNAL_TOKENS {
            continue;
        }
        // Already an action item in the note ⇒ nothing missed, say nothing.
        if captured
            .iter()
            .any(|item| overlap_ratio(&toks, item) >= COVERED_MIN_OVERLAP)
        {
            continue;
        }
        // The same commitment restated later in the transcript ⇒ surface it once.
        if picked
            .iter()
            .any(|(_, prev)| overlap_ratio(&toks, prev) >= DEDUP_MIN_OVERLAP)
        {
            continue;
        }
        if picked.len() >= MAX_MISSED {
            overflow += 1;
            continue;
        }
        picked.push((quote_of(text), toks));
    }

    if picked.is_empty() {
        return note_markdown.to_string();
    }

    let mut out = note_markdown.trim_end().to_string();
    out.push_str("\n\n");
    out.push_str(MISSED_HEADING);
    out.push_str("\n\n");
    out.push_str(MISSED_PREAMBLE);
    out.push_str("\n\n");
    for (quote, _) in &picked {
        out.push_str("- \"");
        out.push_str(quote);
        out.push_str("\"\n");
    }
    // NO SILENT CAP: say how many cue-bearing lines the cap hid.
    if overflow > 0 {
        let cap = MAX_MISSED;
        out.push_str(&format!(
            "- _(+{overflow} more cue-bearing line(s) not shown — this scan shows at most {cap}.)_\n"
        ));
    }
    out
}

/// Whether `note_markdown` already carries the appended section (line-exact heading match, so a
/// mention of the phrase inside prose does not count).
fn has_missed_section(note_markdown: &str) -> bool {
    note_markdown.lines().any(|l| l.trim() == MISSED_HEADING)
}

/// Whether `text` carries a commitment / request / deadline cue: a [`COMMITMENT_CUES`] phrase on
/// word boundaries, or an explicit ISO date (`2026-07-31`) — reusing `action_items::find_date`, the
/// same date scanner the checklist due-date patcher uses (one date notion, not two).
fn carries_commitment_cue(text: &str) -> bool {
    let padded = normalized_padded(text);
    COMMITMENT_CUES.iter().any(|cue| padded.contains(cue)) || find_date(text).is_some()
}

/// Lowercase `text`, map every non-alphanumeric char to a single space, and pad both ends with a
/// space — so a space-padded cue matches on WORD boundaries only (` i ll ` never matches "chill").
/// Unicode-aware (`char::to_lowercase`), so PL diacritics survive intact.
fn normalized_padded(text: &str) -> String {
    let mut s = String::with_capacity(text.len() + 2);
    s.push(' ');
    let mut at_space = true;
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for lc in ch.to_lowercase() {
                s.push(lc);
            }
            at_space = false;
        } else if !at_space {
            s.push(' ');
            at_space = true;
        }
    }
    if !at_space {
        s.push(' ');
    }
    s
}

/// Distinct lowercased TOPIC tokens: the SAME tokenizer + stopword list the grounding pass and
/// retrieval use (so "covered by an action item" keeps the same tokenization everywhere), minus the
/// [`LOW_SIGNAL_TOKENS`] cue glue.
fn signal_tokens(s: &str) -> HashSet<String> {
    tokenize(s)
        .into_iter()
        .filter(|t| !is_stopword(t) && !LOW_SIGNAL_TOKENS.contains(&t.as_str()))
        .collect()
}

/// Fraction of `candidate`'s distinct tokens that also appear in `other` (`0.0` for an empty
/// candidate — an empty set is never "covered").
fn overlap_ratio(candidate: &HashSet<String>, other: &HashSet<String>) -> f64 {
    if candidate.is_empty() {
        return 0.0;
    }
    let hit = candidate.iter().filter(|t| other.contains(*t)).count();
    hit as f64 / candidate.len() as f64
}

/// Render a transcript line as a safe, skimmable quote: whitespace collapsed and HTML-comment
/// delimiters neutralized by the shared `enrich::sanitize` (so a quoted line can never forge a
/// managed-block fence), the embedded `"` swapped for `'` so the surrounding quotes stay balanced,
/// and the whole thing ellipsized at [`MAX_QUOTE_CHARS`] on a CHAR boundary.
fn quote_of(text: &str) -> String {
    let clean = crate::enrich::sanitize(text).replace('"', "'");
    if clean.chars().count() <= MAX_QUOTE_CHARS {
        return clean;
    }
    let mut out: String = clean.chars().take(MAX_QUOTE_CHARS).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(idx: i64, text: &str) -> Segment {
        Segment {
            idx,
            start_s: idx as f64 * 10.0,
            end_s: idx as f64 * 10.0 + 5.0,
            text: text.to_string(),
            speaker: Some("me".to_string()),
            confidence: Some(0.9),
        }
    }

    const NOTE_WITHOUT_THE_DECK: &str = "# Sync\n\n## Summary\n\nWe discussed the launch.\n\n\
         ## Action items\n\n- [ ] Anna — publish the blog post\n";

    /// THE RECALL GAP (RED before this pass existed): the transcript carries "I'll send the deck by
    /// Friday", the generated note never turned it into an action item — and nothing anywhere told
    /// the user. Now it surfaces under the marked section.
    #[test]
    fn surfaces_a_commitment_the_note_missed() {
        let segments = [
            seg(0, "So about the launch, we are mostly on track."),
            seg(1, "I'll send the deck by Friday."),
        ];
        let out = append_possible_missed_items(NOTE_WITHOUT_THE_DECK, &segments);
        assert!(
            out.contains(MISSED_HEADING),
            "the missed-items section must be appended: {out}"
        );
        assert!(
            out.contains("I'll send the deck by Friday."),
            "the missed commitment must be quoted: {out}"
        );
        // NON-DESTRUCTIVE: the original note is a byte-identical PREFIX of the result.
        assert!(
            out.starts_with(NOTE_WITHOUT_THE_DECK.trim_end()),
            "the existing note body must be emitted byte-identical: {out}"
        );
        // The candidate is a PLAIN bullet, never a checklist line — so it can't become a fake task.
        assert_eq!(
            parse_action_items(&out).len(),
            1,
            "the surfaced candidate must NOT parse as an action item: {out}"
        );
    }

    /// The other half of the contract: a commitment the note DID capture as `- [ ]` is not
    /// duplicated into the section (and with nothing else to say, the note is byte-identical).
    #[test]
    fn does_not_duplicate_a_captured_commitment() {
        let note = "# Sync\n\n## Action items\n\n- [ ] Send the deck by Friday\n";
        let segments = [seg(0, "I'll send the deck by Friday.")];
        let out = append_possible_missed_items(note, &segments);
        assert_eq!(
            out, note,
            "an already-captured commitment adds nothing: {out}"
        );
    }

    /// Polish transcripts get the same net (the app's second working language), including the
    /// diacritic-free spellings whisper sometimes emits.
    #[test]
    fn surfaces_polish_commitments() {
        let note = "# Spotkanie\n\n## Zadania\n\n- [ ] Anna — opublikuje wpis na blogu\n";
        let segments = [
            seg(0, "Wyślę prezentację do piątku."),
            seg(1, "Przygotuje raport sprzedazowy na jutro."),
        ];
        let out = append_possible_missed_items(note, &segments);
        assert!(out.contains(MISSED_HEADING));
        assert!(out.contains("Wyślę prezentację do piątku."), "{out}");
        assert!(
            out.contains("Przygotuje raport sprzedazowy na jutro."),
            "{out}"
        );
    }

    /// Chatter without a cue, and cue-bearing lines that are too short to carry signal, add nothing.
    #[test]
    fn ignores_chatter_and_signal_free_lines() {
        let note = "# Sync\n\n## Action items\n\n- [ ] Anna — publish the blog post\n";
        let segments = [
            seg(0, "The weather was terrible this morning."),
            seg(1, "Yeah, totally."),
            // A cue ("I'll") but under MIN_SIGNAL_TOKENS distinct signal tokens — a vague
            // "I'll do it" is exactly the noise this bar exists to keep out of the note.
            seg(2, "I'll do it."),
        ];
        assert_eq!(append_possible_missed_items(note, &segments), note);
    }

    /// A line the user spoke TO the in-meeting assistant is a voice command, not a commitment —
    /// the same exclusion the summarizer feed and the grounding pass apply.
    #[test]
    fn ignores_assistant_directed_lines() {
        let note = "# Sync\n\n## Action items\n\n- [ ] Anna — publish the blog post\n";
        let segments = [seg(0, "Klaudku, przypomnij mi o prezentacji do piątku")];
        assert!(
            crate::audio::wake::is_assistant_directed(&segments[0].text),
            "precondition: the line is assistant-directed"
        );
        assert_eq!(append_possible_missed_items(note, &segments), note);
    }

    /// No transcript ⇒ byte-identical (nothing to scan; never invent a section).
    #[test]
    fn empty_transcript_is_byte_identical() {
        assert_eq!(
            append_possible_missed_items(NOTE_WITHOUT_THE_DECK, &[]),
            NOTE_WITHOUT_THE_DECK
        );
    }

    /// Running the pass twice is a no-op (mirrors `grounding`'s idempotency contract) — a
    /// re-summarize over an already-annotated note never stacks a second section.
    #[test]
    fn is_idempotent() {
        let segments = [seg(0, "I'll send the deck by Friday.")];
        let once = append_possible_missed_items(NOTE_WITHOUT_THE_DECK, &segments);
        let twice = append_possible_missed_items(&once, &segments);
        assert_eq!(once, twice, "second run must be a no-op");
        assert_eq!(
            once.matches(MISSED_HEADING).count(),
            1,
            "exactly one section"
        );
    }

    /// The same commitment restated across segments is surfaced ONCE.
    #[test]
    fn deduplicates_a_restated_commitment() {
        let segments = [
            seg(0, "I'll send the deck by Friday."),
            seg(1, "Right, I will send the deck by Friday then."),
        ];
        let out = append_possible_missed_items(NOTE_WITHOUT_THE_DECK, &segments);
        assert_eq!(
            out.matches("send the deck").count(),
            1,
            "restatements collapse to one entry: {out}"
        );
    }

    /// The cap is HARD but never silent: overflow is counted and reported.
    #[test]
    fn caps_the_section_and_reports_the_overflow() {
        // Every line carries DISTINCT topic words, so the dedup pass cannot collapse them and the
        // cap is what limits the section.
        let segments: Vec<Segment> = (0..MAX_MISSED + 3)
            .map(|i| seg(i as i64, &format!("I'll alpha{i} beta{i} gamma{i}")))
            .collect();
        let out = append_possible_missed_items(NOTE_WITHOUT_THE_DECK, &segments);
        let bullets = out.lines().filter(|l| l.starts_with("- \"")).count();
        assert_eq!(bullets, MAX_MISSED, "hard cap on surfaced entries: {out}");
        assert!(
            out.contains("+3 more cue-bearing line(s)"),
            "the cap must report what it hid: {out}"
        );
    }

    /// An ISO date alone is a cue (a spoken deadline the model may have dropped).
    #[test]
    fn iso_date_is_a_cue() {
        let segments = [seg(0, "The migration window opens 2026-08-14 for everyone")];
        let out = append_possible_missed_items(NOTE_WITHOUT_THE_DECK, &segments);
        assert!(out.contains("2026-08-14"), "{out}");
    }

    /// A quoted line can neither break the bullet's quoting nor forge a managed-block fence, and a
    /// long line is ellipsized on a char boundary (no panic on multi-byte input).
    #[test]
    fn quotes_are_sanitized_and_bounded() {
        let long = format!(
            "I'll prepare the {} \"budget\" <!-- murmur:links --> ą",
            "ą".repeat(400)
        );
        let segments = [seg(0, &long)];
        let out = append_possible_missed_items(NOTE_WITHOUT_THE_DECK, &segments);
        assert!(!out.contains("<!-- murmur:links -->"), "{out}");
        let bullet = out
            .lines()
            .find(|l| l.starts_with("- \""))
            .expect("a bullet was surfaced");
        assert_eq!(
            bullet.matches('"').count(),
            2,
            "exactly the two wrapping quotes: {bullet}"
        );
        assert!(bullet.chars().count() <= MAX_QUOTE_CHARS + 8, "{bullet}");
    }
}
