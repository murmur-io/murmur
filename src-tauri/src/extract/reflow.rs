//! READ-TIME de-fragmentation of pathologically letter-spaced PDF text (Brain v3 doc-preview fix).
//!
//! THE BUG this repairs: Apple PDFKit's `page.string()` (see [`crate::extract::pdf`]) can return
//! per-glyph / per-syllable text with spurious newlines for PDFs built with letter-spacing / custom
//! glyph layout (a common CV / résumé style). A real uploaded CV stores in `documents.text` as e.g.
//! `"Fron\nt\nend engineer"` — one word shattered across three lines. That mangled text feeds BOTH the
//! preview (unreadable) AND the retrieval chunker (a query for "frontend" never matches "Fron").
//!
//! THE FIX: [`reflow_fragmented_text`] is a pure, READ-ONLY string→string transform applied at the two
//! CONSUMPTION points only — display rendering ([`crate::extract::render_display_text`]) and chunk
//! input ([`crate::storage::Db::index_document_chunks`]). It NEVER mutates `documents.text` at rest, so
//! there is NO new seal path, the stored ciphertext / seal round-trip is byte-identical, and md/txt/
//! note/legacy rows are provably untouched (they never reach the sentinel branch, and even if they did
//! the conservative gate below is a no-op on normal prose).
//!
//! IT IS SELF-TARGETING via a conservative FRAGMENTATION GATE (see [`looks_fragmented`]): reflow only
//! fires when a high fraction of lines are pathologically short, so normal prose / an invoice / a spec
//! / markdown returns UNCHANGED. A false positive here would corrupt a clean document's preview, so the
//! gate is deliberately strict.
//!
//! IDEMPOTENT: `reflow(reflow(x)) == reflow(x)` — a reflowed document is no longer fragmented, so a
//! second pass hits the no-op branch (the [`reflow_is_idempotent`] test proves it).

/// A line is "short" (a fragmentation signal) when it has at most this many NON-whitespace chars.
/// Letter-spaced glyph fragments are 1–3 chars (`"O"`, `"ng"`, `"t"`); real prose lines are far
/// longer, so `3` separates the pathological case from normal text without catching short headings in
/// isolation (the gate also requires a high FRACTION of such lines, not just their presence).
const SHORT_LINE_MAX_NONWS: usize = 3;

/// The MINIMUM number of short lines before the gate can fire. A handful of short lines is normal
/// (a title, a date, a bullet); a fragmented document has dozens. This floor stops the gate ever
/// firing on a small clean block.
const MIN_SHORT_LINES: usize = 5;

/// The MINIMUM fraction of non-empty lines that must be short for the text to count as fragmented.
/// A real letter-spaced PDF is >50% short lines; normal prose is a few percent. `0.30` is well above
/// any normal document yet comfortably below the pathological case, so it fires on the mangled CV and
/// no-ops on the clean invoice.
const MIN_SHORT_FRACTION: f64 = 0.30;

/// The MINIMUM fraction of non-empty lines that must be a SINGLE alphabetic character for the text to
/// count as letter-spacing-fragmented. This is the DISTINGUISHING FINGERPRINT of the glyph-spacing
/// pathology: a real letter-spaced CV shatters words into single-letter lines (`"O"`, `"S"`, `"t"`,
/// `"u"`, `"i"`, `"l"`, `"A"`…) so single-char-ALPHA lines run ~0.29 of the page. A TOC / form / vertical
/// short-word list has NONE (its short lines are whole words like `"AWS"` or PAGE-NUMBER digits like
/// `"1"` — digits are excluded on purpose so a TOC's page numbers can't spoof the signature). Without
/// this second condition the short-line-fraction gate false-positives on those pages and the join then
/// welds separate tokens (`"Intro\n1"`→`"Intro1"`). `0.15` sits well below the real CV's 0.29 yet far
/// above the counterexamples' 0.00, so the CV still fires and the TOC/form/list stay a NO-OP.
const MIN_SINGLE_ALPHA_FRACTION: f64 = 0.15;

/// De-fragment pathologically letter-spaced text ON READ. Conservative: returns `s.to_string()`
/// UNCHANGED unless the [fragmentation gate](looks_fragmented) fires (normal prose / invoice / md /
/// txt / notes hit the no-op branch). Idempotent. Never mutates anything at rest — the caller applies
/// this to a COPY of the stored text at the display / chunk-input seam.
pub fn reflow_fragmented_text(s: &str) -> String {
    if s.is_empty() || !looks_fragmented(s) {
        return s.to_string();
    }
    reflow(s)
}

/// The conservative fragmentation gate. True only when a document is PATHOLOGICALLY letter-spaced:
/// enough short lines ([`MIN_SHORT_LINES`]) AND a high enough short-line fraction
/// ([`MIN_SHORT_FRACTION`]) AND a high enough SINGLE-ALPHA-line fraction
/// ([`MIN_SINGLE_ALPHA_FRACTION`], the glyph-spacing fingerprint) that it cannot be a normal
/// short-line page (TOC / form / vertical short-word list). All three are required; a page that has
/// many short lines but NO shattered single-letter lines (a TOC's page numbers, a form's label/value
/// pairs, an acronym list) is a NO-OP. Counts NON-empty lines only (blank paragraph breaks don't
/// dilute the ratio).
fn looks_fragmented(s: &str) -> bool {
    let mut non_empty = 0usize;
    let mut short = 0usize;
    let mut single_alpha = 0usize;
    for line in s.lines() {
        // Collect the non-whitespace chars of the line once so we can inspect both the count and the
        // sole char (for the single-alphabetic-line fingerprint).
        let mut nonws_chars = line.chars().filter(|c| !c.is_whitespace());
        let first = nonws_chars.next();
        let Some(first) = first else {
            continue; // blank line — a paragraph break, not a fragmentation signal.
        };
        let nonws = 1 + nonws_chars.count();
        non_empty += 1;
        if nonws <= SHORT_LINE_MAX_NONWS {
            short += 1;
        }
        // A SINGLE alphabetic char (letters only — digits are page numbers, not shattered glyphs) is
        // the letter-spacing signature. `is_alphabetic()` covers the CV's Unicode `Ł` too.
        if nonws == 1 && first.is_alphabetic() {
            single_alpha += 1;
        }
    }
    if short < MIN_SHORT_LINES || non_empty == 0 {
        return false;
    }
    (short as f64) / (non_empty as f64) >= MIN_SHORT_FRACTION
        && (single_alpha as f64) / (non_empty as f64) >= MIN_SINGLE_ALPHA_FRACTION
}

/// Rejoin fragmented lines into readable text (called ONLY when the gate has fired). Walk lines;
/// preserve blank-line paragraph breaks; within a paragraph, join a line to the running text with NO
/// separator only when the break is genuinely MID-WORD, otherwise with a single space. Runs of spaces
/// are collapsed to one per line already (each source line is a fragment).
///
/// The join rule (see [`join_kind`]) recovers body words (`"Fron\nt\nend"` → `"Frontend"`,
/// `"A\nng\nular"` → `"Angular"`, `"realt\nime"` → `"realtime"`) and split numbers (the CV phone
/// `"90\n7"` → `"907"`) while REFUSING to weld across a label→value / entry→page-number DIGIT boundary
/// (`"Age\n42"` → `"Age 42"`, `"Intro\n1"` → `"Intro 1"`) and keeping ALLCAPS heading words
/// space-separated (an uppercase-leading fragment starts a new word).
fn reflow(s: &str) -> String {
    let mut out = String::new();
    // The running paragraph text; flushed (with a paragraph break) on a blank line.
    let mut para = String::new();

    let flush_para = |out: &mut String, para: &mut String| {
        let trimmed = para.trim();
        if !trimmed.is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(trimmed);
        }
        para.clear();
    };

    for line in s.lines() {
        let piece = collapse_spaces(line.trim());
        if piece.is_empty() {
            // Blank line → paragraph break.
            flush_para(&mut out, &mut para);
            continue;
        }
        if para.is_empty() {
            para.push_str(&piece);
            continue;
        }
        // Decide the separator between the running text and this fragment.
        let prev = last_nonspace_char(&para);
        let next = piece.chars().next();
        if join_kind(prev, next) == Join::Space {
            para.push(' ');
        }
        para.push_str(&piece);
    }
    flush_para(&mut out, &mut para);
    out
}

/// How to join a fragment to the running paragraph text.
#[derive(PartialEq, Eq, Debug)]
enum Join {
    /// Weld with NO separator (a genuine mid-word / mid-number break).
    Weld,
    /// Insert a single space (a new word / a label→value / entry→page-number boundary).
    Space,
}

/// Decide the join between the previous emitted non-space char (`prev`) and the first char of the next
/// fragment (`next`). This is the fix for the weld false-positives: a bare digit is welded ONLY when it
/// continues a digit run — otherwise it is a form value / page number and gets a space.
///
/// - `next` is a LOWERCASE letter → [`Join::Weld`] iff `prev` is alphanumeric (mid-word: `"Fron\nt"` →
///   `"Front"`, `"realt\nime"` → `"realtime"`).
/// - `next` is a DIGIT → [`Join::Weld`] ONLY if `prev` is ALSO a digit (a split number: the CV phone
///   `"90\n7"` → `"907"`); otherwise [`Join::Space`] (`"Age\n42"` → `"Age 42"`, `"Intro\n1"` →
///   `"Intro 1"`) — a label→value / entry→page-number boundary is NOT a mid-word break.
/// - otherwise (uppercase letter / punctuation / anything else) → [`Join::Space`] (unchanged: ALLCAPS
///   heading words and new words stay space-separated).
fn join_kind(prev: Option<char>, next: Option<char>) -> Join {
    match next {
        Some(n) if n.is_alphabetic() && n.is_lowercase() => {
            if matches!(prev, Some(p) if p.is_alphanumeric()) {
                Join::Weld
            } else {
                Join::Space
            }
        }
        Some(n) if n.is_ascii_digit() => {
            if matches!(prev, Some(p) if p.is_ascii_digit()) {
                Join::Weld
            } else {
                Join::Space
            }
        }
        _ => Join::Space,
    }
}

/// The last non-space char of a string (the char a following fragment would attach to), or `None`.
fn last_nonspace_char(s: &str) -> Option<char> {
    s.chars().rev().find(|c| !c.is_whitespace())
}

/// Collapse every run of ASCII/Unicode whitespace WITHIN a line to a single space (the line is already
/// trimmed by the caller). Keeps intra-line words separated by exactly one space.
fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The VERBATIM fragmented-CV excerpt pulled from the live DB (the real bug). Used as the
    /// regression fixture: reflow must recover the mangled body words.
    const CV_FRAGMENT: &str = "O\nSKAR\nO\nRŁ\nO\nWSKI\nS\ntaff Fron\nt\nend Engineer · Realt\nime Trading Plat\nforms & Web Archi\nt\nec\nture at\nScale\nWarsaw , Po\nland · +48 786 327 90\n7 ·oskar .orl\now@wp.pl\nS UMMAR Y\nFron\nt\nend engineering leader wi\nt\nh 10+ years b\nu\ni\nlding large-scale realt\nime web plat\nforms in\nA\nng\nular, Reac\nt, and TypeScript. A\nt XTB, drives fron\nt\nend\narchi\nt\nec\nture for a mult\ni-asse\nt trading plat\nform serving 1.4M+ traders worldwide";

    /// A real invoice excerpt that extracts CLEANLY — the gate must NOT fire on it (no false positive).
    const INVOICE_CLEAN: &str = "INVOICE\nInvoice # INV-9752r79-2026-5\nIssue date May 21, 2026\nDue date May 29, 2026\nBILL FROM:\nJakub Gawronski\nAl.Lawendowa 9\nCzłuchów, 77-300\nPoland\nTOTAL DUE\nPLN zł49,883.33";

    /// A table-of-contents page: many short lines (entry title + PAGE NUMBER), but ZERO shattered
    /// single-LETTER lines (the page numbers are single DIGITS, not glyph fragments). The short-line
    /// gate alone would fire and the old join would weld `"Introduction\n1"` → `"Introduction1"`. The
    /// single-alpha fingerprint must keep the gate a NO-OP, and even if it fired the digit-boundary weld
    /// rule must never produce `"Introduction1"`.
    const TOC_PAGE: &str = "Introduction\n1\nOverview\n2\nArchitecture\n3\nStorage\n4\nAppendix\n5";

    /// A form page: label / value pairs down the page. The verifier's `"Age\n42"` case: welding across
    /// the label→value DIGIT boundary would produce `"Age42"`.
    const FORM_PAGE: &str = "Name\nJohn\nAge\n42\nCity\nNewYork\nRole\nEngineer";

    /// A vertical list of short ACRONYMS. High short-line fraction (0.83) but ZERO single-letter lines —
    /// the gate must not fire and the tokens must stay separate (`"AWS\nGCP"` → not `"AWSGCP"`).
    const ACRONYM_LIST: &str = "AWS\nGCP\nAzure\nk8s\nEC2\nRDS";

    /// RED baseline: the RAW fragmented fixture does NOT contain the recovered words and DOES still
    /// carry the `"Fron\nt"` fragment. This proves the fixture is genuinely broken (the fix has work to
    /// do) — without it a passing GREEN test could be vacuous.
    #[test]
    fn cv_fixture_is_fragmented_before_reflow_red_baseline() {
        assert!(
            !CV_FRAGMENT.contains("Frontend"),
            "the raw fixture must NOT already contain the recovered word (else the test is vacuous)"
        );
        assert!(
            CV_FRAGMENT.contains("Fron\nt"),
            "the raw fixture must still carry the shattered fragment (RED state)"
        );
        assert!(
            !CV_FRAGMENT.contains("building large-scale realtime web platforms"),
            "the raw fixture must NOT already contain the recovered phrase (RED state)"
        );
    }

    /// GREEN: reflow recovers the fragmented body words / phrases and drops the shattered fragment.
    #[test]
    fn cv_fixture_reflows_to_recovered_words() {
        let out = reflow_fragmented_text(CV_FRAGMENT);
        for needle in [
            "Frontend",
            "TypeScript",
            "Angular",
            "React",
            "Staff Frontend Engineer",
            "building large-scale realtime web platforms",
        ] {
            assert!(
                out.contains(needle),
                "reflow must recover {needle:?}\n---\n{out}\n---"
            );
        }
        assert!(
            !out.contains("Fron\nt"),
            "reflow must not leave the shattered fragment\n---\n{out}\n---"
        );
    }

    /// The invoice extracts cleanly → the gate must NOT fire; its key strings survive intact. Ideally
    /// the whole thing is byte-identical (a clean document is never touched).
    #[test]
    fn invoice_gate_does_not_fire_and_is_unchanged() {
        assert!(
            !looks_fragmented(INVOICE_CLEAN),
            "the gate must NOT fire on the clean invoice"
        );
        let out = reflow_fragmented_text(INVOICE_CLEAN);
        assert_eq!(
            out, INVOICE_CLEAN,
            "a clean invoice must pass through byte-identical"
        );
        assert!(out.contains("Invoice # INV-9752r79-2026-5"));
        assert!(out.contains("PLN zł49,883.33"));
    }

    /// RED baseline for the WELD false-positive: prove the OLD unconditional digit-weld (a digit was
    /// welded to any alphanumeric `prev`) would corrupt these — this documents the exact bug the fix
    /// closes. The `join_kind` rule below asserts the FIXED behavior; this asserts the fixture is a
    /// genuine trigger (previous non-space char is a LETTER, next is a DIGIT — the label→value shape).
    #[test]
    fn weld_counterexamples_have_the_label_then_digit_shape_red_context() {
        // "Age" then "42": prev='e' (letter), next='4' (digit) — the shape the old code welded.
        assert_eq!(
            join_kind(Some('e'), Some('4')),
            Join::Space,
            "label→digit value must space, not weld"
        );
        // "Intro" then "1": prev='o', next='1'.
        assert_eq!(
            join_kind(Some('o'), Some('1')),
            Join::Space,
            "entry→page-number must space, not weld"
        );
        // "Soup" then "5".
        assert_eq!(join_kind(Some('p'), Some('5')), Join::Space);
        // But a SPLIT NUMBER still welds: "90" then "7" (the CV phone) — prev='0' is a digit.
        assert_eq!(
            join_kind(Some('0'), Some('7')),
            Join::Weld,
            "a split number must still weld"
        );
        // And a genuine mid-word still welds: "Fron" then "t".
        assert_eq!(
            join_kind(Some('n'), Some('t')),
            Join::Weld,
            "mid-word must still weld"
        );
    }

    /// A TOC page must NOT weld its entry titles onto their page numbers. Either the gate no-ops (the
    /// single-alpha fingerprint is 0) or, if it fired, the digit-boundary rule spaces — either way
    /// `"Introduction1"` must NEVER appear. (RED against the old code: it welded to `Introduction1`.)
    #[test]
    fn toc_page_is_not_welded() {
        assert!(
            !looks_fragmented(TOC_PAGE),
            "the single-alpha fingerprint (0 single-letter lines) must keep the gate a no-op on a TOC"
        );
        let out = reflow_fragmented_text(TOC_PAGE);
        assert!(
            !out.contains("Introduction1"),
            "a TOC entry must never weld onto its page number\n---\n{out}\n---"
        );
        // Concrete verifier counterexamples: `Intro\n1`→ not `Intro1`, `Soup\n5`→ not `Soup5`.
        assert!(!reflow_fragmented_text("Intro\n1").contains("Intro1"));
        assert!(!reflow_fragmented_text("Soup\n5").contains("Soup5"));
    }

    /// A form page must NOT weld a label onto its value (`"Age\n42"` → not `"Age42"`).
    #[test]
    fn form_page_is_not_welded() {
        let out = reflow_fragmented_text(FORM_PAGE);
        assert!(
            !out.contains("Age42"),
            "a form label must never weld onto its value\n---\n{out}\n---"
        );
        // The gate also should not fire (2 short lines < floor; and 0 single-alpha lines).
        assert!(
            !looks_fragmented(FORM_PAGE),
            "a form page must not trip the gate"
        );
    }

    /// NON-VACUOUS join proof: embed the label→value pair inside a document whose gate GENUINELY FIRES
    /// (letter-spaced fragments give it the single-alpha fingerprint) so the join rule is actually
    /// exercised. The OLD unconditional digit-weld produced `"Age42"` here; the digit-boundary rule must
    /// space it to `"Age 42"` while still recovering the mid-word fragments in the same block.
    #[test]
    fn label_value_not_welded_even_when_gate_fires() {
        let frag = "Fron\nt\nend\nA\nng\nular\nb\nu\ni\nld\nRealt\nime\nAge\n42";
        assert!(
            looks_fragmented(frag),
            "this mixed block must trip the gate (letter-spaced fragments)"
        );
        let out = reflow_fragmented_text(frag);
        assert!(
            out.contains("Frontend"),
            "mid-word fragments still recover\n---\n{out}\n---"
        );
        assert!(
            !out.contains("Age42"),
            "a label→value digit boundary must NOT weld even inside a firing block\n---\n{out}\n---"
        );
        assert!(
            out.contains("Age 42"),
            "the label→value pair must be space-joined\n---\n{out}\n---"
        );
    }

    /// A vertical acronym list must not fire the gate (no single-letter lines) nor weld into one token.
    #[test]
    fn acronym_list_is_not_welded() {
        assert!(
            !looks_fragmented(ACRONYM_LIST),
            "a short-word list with no single-letter lines must not trip the gate"
        );
        let out = reflow_fragmented_text(ACRONYM_LIST);
        assert!(
            !out.contains("AWSGCP"),
            "acronyms must stay separate tokens\n---\n{out}\n---"
        );
        assert_eq!(out, ACRONYM_LIST, "gate no-op → byte-identical passthrough");
    }

    /// The real letter-spaced CV must STILL trip the tightened gate (its single-alpha fraction ≈ 0.29 is
    /// well above the 0.15 threshold). Without this the fix could over-tighten and stop repairing the
    /// actual bug.
    #[test]
    fn cv_fixture_still_trips_the_tightened_gate() {
        assert!(
            looks_fragmented(CV_FRAGMENT),
            "the tightened gate must still fire on the genuinely letter-spaced CV"
        );
    }

    /// The CV's split phone number is welded back (a digit continues a digit run): `"...327 90\n7"` →
    /// `"...907"`.
    #[test]
    fn cv_phone_number_split_digits_reweld() {
        let out = reflow_fragmented_text(CV_FRAGMENT);
        assert!(
            out.contains("907"),
            "the split phone number must re-weld to 907\n---\n{out}\n---"
        );
    }

    /// Normal prose / markdown is returned UNCHANGED (the gate no-ops). This is the same body the
    /// extract-module regression guard uses.
    #[test]
    fn normal_prose_is_unchanged() {
        let prose = "# Spec\n\nThe budget is 100k.\n\nAnna owns delivery.";
        assert!(
            !looks_fragmented(prose),
            "normal prose must not trip the gate"
        );
        assert_eq!(reflow_fragmented_text(prose), prose);
    }

    /// A longer normal paragraph (many words per line, few short lines) is untouched.
    #[test]
    fn long_prose_is_unchanged() {
        let prose = "The frontend engineering team ships a realtime trading platform.\n\
                     It serves over one million traders worldwide across many asset classes.\n\
                     Anna owns delivery and the quarterly roadmap is on track for May.";
        assert!(!looks_fragmented(prose));
        assert_eq!(reflow_fragmented_text(prose), prose);
    }

    /// Empty and tiny inputs are returned unchanged (below the short-line floor / empty).
    #[test]
    fn empty_and_tiny_inputs_unchanged() {
        assert_eq!(reflow_fragmented_text(""), "");
        assert_eq!(reflow_fragmented_text("hi"), "hi");
        assert_eq!(reflow_fragmented_text("a\nb\nc"), "a\nb\nc"); // 3 short lines < MIN_SHORT_LINES.
    }

    /// Idempotency: reflowing a reflowed document changes nothing (the reflowed text no longer trips
    /// the gate).
    #[test]
    fn reflow_is_idempotent() {
        let once = reflow_fragmented_text(CV_FRAGMENT);
        let twice = reflow_fragmented_text(&once);
        assert_eq!(once, twice, "reflow must be idempotent");
    }

    /// Blank-line paragraph breaks are preserved across a reflow (structure isn't flattened to one
    /// blob). A fragmented doc with two paragraphs stays two paragraphs.
    #[test]
    fn paragraph_breaks_are_preserved() {
        let frag =
            "Fron\nt\nend one\nA\nng\nular two\n\nRealt\nime three\nplat\nform four\nb\nu\ni\nld";
        let out = reflow_fragmented_text(frag);
        assert!(
            out.contains("\n\n"),
            "a blank-line paragraph break must survive\n{out}"
        );
    }
}
