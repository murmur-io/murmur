//! In-meeting voice trigger — the HEADLESS, deterministic CORE (Phase 7a).
//!
//! Two pure functions, no I/O, no DB, no FFI, no egress — pure INPUT PARSING:
//!
//! 1. [`detect_wake`] — robustly find the assistant wake name ("Claude" / Polish vocative
//!    "Claudku") at the head of a transcript tail, FUZZY + PHONETIC because Whisper mangles a
//!    French/English proper noun inside Polish speech ("Klałd", "Cloud", "Klałdku", "Klod",
//!    "Claud", "klołdku", …). It NORMALIZES (lowercase → strip Polish diacritics → collapse the
//!    systematic au/ał/ou/oł→o and cl→kl confusions → squeeze repeats), then matches the name
//!    token by a hand-rolled Levenshtein against a small wake-lexicon, ANCHORED at an utterance
//!    boundary (absolute start, or right after a short interjection like "hej"/"ok"). Precision is
//!    favoured over recall — a false fire mid-meeting is the failure mode to avoid.
//! 2. [`parse_voice_intent`] — map the recognized command tail to a structured [`VoiceIntent`]
//!    with a deterministic PL+EN keyword/pattern parser. Structured so a future
//!    `LocalReasoner`-backed parser ([`crate::reason`]) is a drop-in replacement.
//!
//! ⚠️ NOT WIRED: this is the testable core only. Integration into the live mic / live-caption
//! loop ([`crate::transcribe::live`]), the Whisper `set_initial_prompt` bias toward the wake
//! lexicon, and real-mic precision are the real-Mac step — `cargo test` is NOT proof for those.
//! See `detect_wake`'s "residual false-fire risk" note.

/// A successful wake detection: the original on-screen token that matched, plus the trimmed
/// command tail that followed it (which [`parse_voice_intent`] then classifies).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WakeHit {
    /// The original (un-normalized) word that matched the wake lexicon, e.g. "Klałdku".
    pub matched_phrase: String,
    /// Everything after the wake token, trimmed (may be empty).
    pub command: String,
}

/// The ONLY firing wake-lexicon: the distinctive Polish vocative diminutive of the assistant's
/// name ("Claudku"/"Klaudku"/"Klałdku" → "klodku", "kladku"). PRECISION-FIRST anchor.
///
/// The bare French/English name forms "klod"/"klode" (cloud/claude) were DELIBERATELY DROPPED from
/// the firing set: they sit within edit-distance 1 of ubiquitous meeting words ("loud"→"lod",
/// "cloud"→"klod", "close"→"klose"), so firing on them is hopelessly imprecise. We anchor instead
/// on the 3-syllable vocative the user actually says — a long (len 6), `'d'`-skeletoned token that
/// ENDS IN the vocative `'u'`. See [`detect_wake`]'s recall trade-off note.
const WAKE_LEXICON: [&str; 2] = ["klodku", "kladku"];

/// Tight edit-distance threshold — normalization already absorbs the systematic confusions, so
/// only minor residual error is allowed on top. Safe ONLY because the lexicon words are long
/// (len 6) + distinctive, so their 1-edit neighbourhood is small. Bigger = more recall, more
/// false fires.
const WAKE_THRESHOLD: usize = 1;

/// Short interjections that may precede the wake name ("hej claude"). NOT a wake anchor on their
/// own ("hej"/"ok" alone are far too common).
const INTERJECTIONS: [&str; 11] = [
    "hej", "ej", "ok", "okej", "okay", "hey", "hi", "halo", "sluchaj", "dobra", "no",
];

/// Detect the assistant wake name at the head of `text` and split off the command tail.
///
/// Returns `Some(WakeHit)` only when a [`WAKE_LEXICON`] (vocative "Claudku") match sits at an
/// utterance boundary:
/// - at token index 0 (absolute start of an utterance), or
/// - at token index 1 IF token 0 is an [`INTERJECTIONS`] member ("hej"/"ok"/"okej" …).
///
/// Pure + deterministic. PRECISION-FIRST.
///
/// ## Recall trade-off (DOCUMENTED, by design)
/// The firing anchor is ONLY the distinctive Polish vocative "Claudku" — the form the user
/// actually utters when addressing the assistant. Consequently the bare French/English name
/// mis-transcriptions "Cloud"/"Claude" NO LONGER fire, even after an interjection ("hej Cloud",
/// "ok Claude" are silent). That is the correct trade for a meeting context: those bare forms are
/// within edit-distance 1 of "loud"/"cloud"/"close" and firing on them produced real false fires
/// ("ok loud and clear", "no loud and clear", "cloud computing"). We accept losing the bare-form
/// recall to win the precision. The vocative is killed for the Polish padlock noun "kłódka"/"kłódkę"
/// (→"klodka"/"klodke") by the `'u'`-ending guard in [`matches_wake`].
///
/// ## Residual false-fire risk (real-mic only, NOT covered by unit tests)
/// On real audio the live tail is a multi-second window, not a clean single utterance; the
/// integration must feed the trailing sentence so the boundary anchor holds. This fix eliminates
/// the demonstrated TEXT-level false fires only — real-mic acoustic precision still needs tuning
/// of `WAKE_THRESHOLD` / the lexicon on a real Mac.
pub fn detect_wake(text: &str) -> Option<WakeHit> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }

    // Index 0: the distinctive vocative at the absolute start of an utterance.
    let n0 = normalize_name(words[0]);
    if matches_wake(&n0) {
        return Some(make_hit(&words, 0));
    }

    // Index 1: the same vocative, after an interjection that clearly addresses the assistant.
    if words.len() >= 2 && is_interjection(words[0]) {
        let n1 = normalize_name(words[1]);
        if matches_wake(&n1) {
            return Some(make_hit(&words, 1));
        }
    }

    None
}

/// Build a [`WakeHit`] for the wake token at `idx`, with the command = the rest of the words.
fn make_hit(words: &[&str], idx: usize) -> WakeHit {
    let command = words[idx + 1..]
        .join(" ")
        .trim()
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .to_string();
    WakeHit { matched_phrase: words[idx].to_string(), command }
}

/// Whether `cand` (already name-normalized) matches the vocative [`WAKE_LEXICON`] within
/// [`WAKE_THRESHOLD`]. THREE precision guards, all of which every real "Claudku" form satisfies:
///
/// 1. **`'u'` ending** — the vocative case-ending. This is the load-bearing guard: it kills the
///    Polish padlock noun "kłódka"→"klodka" (ends `'a'`) and "kłódkę"→"klodke" (ends `'e'`) while
///    keeping "Claudku"/"Klałdku"→"klodku" (ends `'u'`). It also rejects "loud"→"lod" and
///    "cloud"→"klod" (end `'d'`), which are additionally ≥2 edits away anyway.
/// 2. **`'d'` skeleton** — carries the "Cla**ud**ku" `'d'`; rejects "close"→"klose", "klocku" …
/// 3. **length ≥ 5** — the distinctive long vocative; a 1-edit neighbourhood of a len-6 token is
///    small and distinctive (unlike the len-4 bare "klod", which we dropped entirely).
fn matches_wake(cand: &str) -> bool {
    if cand.chars().count() < 5 || !cand.ends_with('u') || !cand.contains('d') {
        return false;
    }
    WAKE_LEXICON.iter().any(|w| levenshtein(cand, w) <= WAKE_THRESHOLD)
}

/// Whether `word` (raw) is a leading interjection that may precede the wake name.
fn is_interjection(word: &str) -> bool {
    let w = strip_diacritics(&word.to_lowercase());
    let w = w.trim_matches(|c: char| !c.is_alphanumeric());
    INTERJECTIONS.contains(&w)
}

/// PHONETIC normalizer for a single candidate NAME token. Lowercases, keeps only letters, collapses
/// the diphthongs Whisper produces for the French "Claude" vowel (`au`/`ał`/`aw`/`ou`/`oł`/`ow`/`eau`
/// → `o`), folds `cl→kl`, strips remaining Polish diacritics, and squeezes repeated letters. The
/// goal: every realistic mis-transcription of the wake name collapses onto a small canonical set
/// ("klod"/"klode"/"klodku").
fn normalize_name(token: &str) -> String {
    // Lowercase + letters only (drops commas, digits, etc.).
    let mut s: String = token.to_lowercase().chars().filter(|c| c.is_alphabetic()).collect();

    // Collapse the "Claude"-vowel diphthongs to a single 'o'. Longer forms first so e.g. "eau"
    // wins over a later "au". (ł/diacritics are still present here on purpose.)
    for (from, to) in [
        ("eau", "o"),
        ("au", "o"),
        ("ał", "o"),
        ("aw", "o"),
        ("ou", "o"),
        ("oł", "o"),
        ("ow", "o"),
    ] {
        if s.contains(from) {
            s = s.replace(from, to);
        }
    }

    // c+l → k+l (Cloud/Claude vs Klod confusion).
    if s.contains("cl") {
        s = s.replace("cl", "kl");
    }

    // Strip any remaining Polish diacritics (ł→l, etc.).
    s = strip_diacritics(&s);

    squeeze_repeats(&s)
}

/// Map Polish diacritics to their ASCII base (1:1, char-length preserving). Used both by the name
/// normalizer and by the intent parser's keyword matching.
fn strip_diacritics(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ź' => 'z',
            'ż' => 'z',
            other => other,
        })
        .collect()
}

/// Collapse runs of the same character to a single one ("klołd"→… "klod", "cool"→"col").
fn squeeze_repeats(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last: Option<char> = None;
    for c in s.chars() {
        if Some(c) != last {
            out.push(c);
            last = Some(c);
        }
    }
    out
}

/// Classic two-row Levenshtein edit distance (no crate). Operates on `char`s so it is UTF-8-safe.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

// ───────────────────────────── intent parser ─────────────────────────────

/// A structured, deterministic interpretation of a wake command tail. The dispatch/execution of
/// these intents is a LATER step — this module only PARSES. A future `LocalReasoner`-backed parser
/// (`reason::structured`) can replace [`parse_voice_intent`] while keeping this enum as the schema.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VoiceIntent {
    /// "zrób research o X" / "research X" / "do research on X".
    Research { topic: String },
    /// "wyszukaj w slacku X" / "slack X" / "search slack for X".
    SlackSearch { query: String },
    /// "co wiemy o X" / "recall X" / "what do we know about X".
    Recall { entity: String },
    /// "przypomnij mi …" / "remind me …" — with a best-effort `due` if a temporal marker is present.
    CreateReminder { text: String, due: Option<String> },
    /// "zapisz …" / "notatka …" / "note that …".
    NoteAside { text: String },
    /// Nothing matched — keep the raw command for a future smarter parser / for logging.
    Unknown { raw: String },
}

/// Parse a recognized command tail into a [`VoiceIntent`]. Deterministic PL+EN keyword/prefix
/// matcher. Matching is done on a diacritic-stripped lowercase copy; the returned payload preserves
/// the lowercased text (diacritics intact). Categories are tried most-specific-first.
pub fn parse_voice_intent(command: &str) -> VoiceIntent {
    let raw = command.trim();
    if raw.is_empty() {
        return VoiceIntent::Unknown { raw: String::new() };
    }
    let lower = raw.to_lowercase();
    let norm = strip_diacritics(&lower);
    let nwords: Vec<&str> = norm.split_whitespace().collect();
    let owords: Vec<&str> = lower.split_whitespace().collect();

    // Reminder first: "przypomnij" / "remind" are unambiguous verbs.
    if let Some(rest) = match_prefix(
        &nwords,
        &owords,
        &[
            &["przypomnij", "mi"],
            &["przypomnij"],
            &["ustaw", "przypomnienie"],
            &["remind", "me"],
            &["remind"],
        ],
    ) {
        let (text, due) = extract_due(&rest);
        return VoiceIntent::CreateReminder { text, due };
    }

    // Slack search.
    if let Some(rest) = match_prefix(
        &nwords,
        &owords,
        &[
            &["wyszukaj", "w", "slacku"],
            &["przeszukaj", "slacka"],
            &["przeszukaj", "slack"],
            &["search", "slack", "for"],
            &["search", "slack"],
            &["w", "slacku"],
            &["slack"],
        ],
    ) {
        return VoiceIntent::SlackSearch { query: rest };
    }

    // Research.
    if let Some(rest) = match_prefix(
        &nwords,
        &owords,
        &[
            &["zrob", "research", "na", "temat"],
            &["zrob", "research", "o"],
            &["zrob", "research"],
            &["research", "na", "temat"],
            &["research", "o"],
            &["do", "research", "on"],
            &["zbadaj", "temat"],
            &["zbadaj"],
            &["research"],
        ],
    ) {
        return VoiceIntent::Research { topic: rest };
    }

    // Recall.
    if let Some(rest) = match_prefix(
        &nwords,
        &owords,
        &[
            &["co", "wiemy", "o"],
            &["co", "wiemy"],
            &["what", "do", "we", "know", "about"],
            &["recall"],
        ],
    ) {
        return VoiceIntent::Recall { entity: rest };
    }

    // Note aside.
    if let Some(rest) = match_prefix(
        &nwords,
        &owords,
        &[
            &["zanotuj"],
            &["zapisz"],
            &["notatka"],
            &["make", "a", "note"],
            &["take", "a", "note"],
            &["note", "that"],
            &["note"],
        ],
    ) {
        return VoiceIntent::NoteAside { text: rest };
    }

    VoiceIntent::Unknown { raw: raw.to_string() }
}

/// If `nwords` starts with any of `patterns` (matched on the normalized tokens), return the
/// remaining ORIGINAL-lowercase words joined + trimmed of leading punctuation (may be empty).
fn match_prefix(nwords: &[&str], owords: &[&str], patterns: &[&[&str]]) -> Option<String> {
    for pat in patterns {
        if nwords.len() >= pat.len() && &nwords[..pat.len()] == *pat {
            let rest = owords[pat.len()..]
                .join(" ")
                .trim()
                .trim_start_matches(|c: char| !c.is_alphanumeric())
                .trim()
                .to_string();
            return Some(rest);
        }
    }
    None
}

/// Best-effort temporal extraction for a reminder body. Returns `(text, due)` where `text` is the
/// whole body unchanged and `due` is the first recognized marker, if any. Deterministic, no regex.
fn extract_due(body: &str) -> (String, Option<String>) {
    let otoks: Vec<&str> = body.split_whitespace().collect();
    const SINGLE: [&str; 9] = [
        "jutro", "dzisiaj", "dzis", "pojutrze", "wieczorem", "rano", "tomorrow", "today", "tonight",
    ];
    for (i, ot) in otoks.iter().enumerate() {
        let t = strip_diacritics(&ot.to_lowercase());
        let t = t.trim_matches(|c: char| !c.is_alphanumeric());
        if SINGLE.contains(&t) {
            let clean = ot.trim_matches(|c: char| !c.is_alphanumeric());
            return (body.to_string(), Some(clean.to_string()));
        }
        // "o 15" / "at 3pm" — a time preposition followed by a token containing a digit.
        if (t == "o" || t == "at") && i + 1 < otoks.len() && otoks[i + 1].chars().any(|c| c.is_ascii_digit())
        {
            return (body.to_string(), Some(format!("{} {}", ot, otoks[i + 1])));
        }
    }
    (body.to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── normalizer / distance unit checks ──────────────────────────────────

    #[test]
    fn normalizer_collapses_wake_variants_to_canonical() {
        // Every realistic mis-transcription collapses onto the small canonical set.
        assert_eq!(normalize_name("claude"), "klode");
        assert_eq!(normalize_name("Claud"), "klod");
        assert_eq!(normalize_name("cloud"), "klod");
        assert_eq!(normalize_name("Klałd"), "klod");
        assert_eq!(normalize_name("klod"), "klod");
        assert_eq!(normalize_name("claudku"), "klodku");
        assert_eq!(normalize_name("klaudku"), "klodku");
        assert_eq!(normalize_name("Klałdku"), "klodku");
        assert_eq!(normalize_name("klołdku"), "klodku");
        // Near-collisions land OFF the canonical set.
        assert_eq!(normalize_name("claudia"), "klodia");
        assert_eq!(normalize_name("close"), "klose");
        assert_eq!(normalize_name("cool"), "col");
    }

    #[test]
    fn levenshtein_basics() {
        assert_eq!(levenshtein("klod", "klod"), 0);
        assert_eq!(levenshtein("klod", "klode"), 1);
        assert_eq!(levenshtein("klodia", "klodku"), 2);
        assert_eq!(levenshtein("klose", "klode"), 1);
    }

    // ── RECALL: detect_wake fires on mangled wake utterances ────────────────

    #[test]
    fn recall_fires_on_mistranscribed_vocative_wake_utterances() {
        // (input, expected command tail). The firing anchor is the distinctive Polish vocative
        // "Claudku" (→ "klodku"/"kladku") — ONLY these fire. Index 0 (utterance start) and index 1
        // (after an interjection) are both covered. PL + EN command tails, code-switched.
        let cases: &[(&str, &str)] = &[
            ("klałdku zrób research o konkurencji", "zrób research o konkurencji"),
            ("klołdku zapisz to", "zapisz to"),
            ("klałdku, co wiemy o atlasie", "co wiemy o atlasie"),
            ("klaudku zrób research", "zrób research"),
            ("klodku do research on pricing", "do research on pricing"),
            ("Klałdku, przypomnij mi o spotkaniu", "przypomnij mi o spotkaniu"),
            // index 1: interjection + vocative.
            ("hej klałdku wyszukaj w slacku raport", "wyszukaj w slacku raport"),
            ("okej klaudku note that we shipped", "note that we shipped"),
            ("ok klodku przypomnij mi", "przypomnij mi"),
        ];
        for (input, want_cmd) in cases {
            let hit = detect_wake(input).unwrap_or_else(|| panic!("expected wake fire on {input:?}"));
            assert_eq!(hit.command, *want_cmd, "wrong command tail for {input:?}");
        }
    }

    // ── PRECISION: detect_wake is SILENT on ordinary meeting speech ─────────

    #[test]
    fn precision_silent_on_normal_meeting_speech() {
        // The load-bearing test: ZERO false fires. Includes every demonstrated false fire the
        // adversarial verifier reproduced, plus the hard near-collisions.
        let negatives: &[&str] = &[
            // ── demonstrated false fires that MUST now be silent ──
            "ok loud and clear",   // "loud"→"lod" used to match bare "klod"
            "no loud and clear",   // "loud"→"lod" used to match bare "klod"
            "kłódka",              // padlock → "klodka" (ends 'a', not the vocative 'u')
            "kłódkę",              // padlock → "klodke" (ends 'e')
            "ok zamknij kłódkę",   // padlock in a sentence
            "cloud computing",     // "cloud"→"klod" (ends 'd')
            "the cloud was down",
            "ok close the door",
            "hej close the laptop",
            "applaud the team",    // "applaud"→"aplod"
            "claudia",
            "klaudia",
            "hej claudia",
            "ok let's",
            "hej",
            "ok",
            "",
            // ── other ordinary meeting speech / near-collisions ──
            "we are migrating everything to the cloud next quarter",
            "moving workloads to cloud computing this sprint",
            "the cloud infrastructure is down again",
            "claudia please review the document",
            "hej claudia możesz to sprawdzić",
            "klaudia z działu hr dołączy do nas",
            "klaudia i tomek przygotują deck",
            "ok let's start the meeting",
            "ok cool let's move on",
            "hej everyone thanks for joining",
            "let's talk about the budget for friday",
            "i think we should close this deal",
            // ── bare French/English name forms: NO LONGER fire by design (recall trade-off) ──
            "claude can you research this",
            "cloud native is the future",
            "hej cloud do research on pricing",
            "ok claude przypomnij mi",
        ];
        for n in negatives {
            assert!(detect_wake(n).is_none(), "FALSE FIRE on negative: {n:?}");
        }
    }

    #[test]
    fn precision_only_the_vocative_fires_bare_or_after_interjection() {
        // Bare "Claude"/"Cloud" is intentionally NOT a trigger anywhere — not at the start, and
        // (the recall trade-off) NOT even after an interjection: those forms collide with
        // "loud"/"cloud"/"close". ONLY the distinctive "Claudku" vocative fires.
        assert!(detect_wake("claude can you research this").is_none());
        assert!(detect_wake("cloud native is the future").is_none());
        assert!(detect_wake("hej claude can you research this").is_none());
        assert!(detect_wake("ok cloud do research").is_none());
        // …the distinctive vocative fires bare (index 0):
        assert!(detect_wake("klałdku zrób research").is_some());
        // …and after an interjection (index 1):
        assert!(detect_wake("hej klałdku zrób research").is_some());
        // …but the padlock noun "kłódka" (ends 'a', not the vocative 'u') stays silent:
        assert!(detect_wake("kłódka jest zamknięta").is_none());
    }

    #[test]
    fn wake_hit_reports_original_matched_phrase() {
        let hit = detect_wake("Klałdku, zrób research").unwrap();
        assert_eq!(hit.matched_phrase, "Klałdku,");
        assert_eq!(hit.command, "zrób research");
    }

    // ── INTENT PARSER ──────────────────────────────────────────────────────

    #[test]
    fn intent_research_pl_and_en() {
        assert_eq!(
            parse_voice_intent("zrób research o konkurencji"),
            VoiceIntent::Research { topic: "konkurencji".into() }
        );
        assert_eq!(
            parse_voice_intent("zrob research na temat atlasa"),
            VoiceIntent::Research { topic: "atlasa".into() }
        );
        assert_eq!(
            parse_voice_intent("research the pricing model"),
            VoiceIntent::Research { topic: "the pricing model".into() }
        );
        assert_eq!(
            parse_voice_intent("do research on competitor pricing"),
            VoiceIntent::Research { topic: "competitor pricing".into() }
        );
    }

    #[test]
    fn intent_slack_pl_and_en() {
        assert_eq!(
            parse_voice_intent("wyszukaj w slacku raport q3"),
            VoiceIntent::SlackSearch { query: "raport q3".into() }
        );
        assert_eq!(
            parse_voice_intent("slack deployment thread"),
            VoiceIntent::SlackSearch { query: "deployment thread".into() }
        );
        assert_eq!(
            parse_voice_intent("search slack for the incident"),
            VoiceIntent::SlackSearch { query: "the incident".into() }
        );
    }

    #[test]
    fn intent_recall_pl_and_en() {
        assert_eq!(
            parse_voice_intent("co wiemy o atlasie"),
            VoiceIntent::Recall { entity: "atlasie".into() }
        );
        assert_eq!(
            parse_voice_intent("recall project orion"),
            VoiceIntent::Recall { entity: "project orion".into() }
        );
        assert_eq!(
            parse_voice_intent("what do we know about acme"),
            VoiceIntent::Recall { entity: "acme".into() }
        );
    }

    #[test]
    fn intent_reminder_with_and_without_due() {
        assert_eq!(
            parse_voice_intent("przypomnij mi o spotkaniu jutro"),
            VoiceIntent::CreateReminder {
                text: "o spotkaniu jutro".into(),
                due: Some("jutro".into())
            }
        );
        assert_eq!(
            parse_voice_intent("remind me to call bob at 3pm"),
            VoiceIntent::CreateReminder {
                text: "to call bob at 3pm".into(),
                due: Some("at 3pm".into())
            }
        );
        assert_eq!(
            parse_voice_intent("przypomnij mi żeby wysłać raport"),
            VoiceIntent::CreateReminder { text: "żeby wysłać raport".into(), due: None }
        );
    }

    #[test]
    fn intent_note_pl_and_en() {
        assert_eq!(
            parse_voice_intent("zapisz że deadline to piątek"),
            VoiceIntent::NoteAside { text: "że deadline to piątek".into() }
        );
        assert_eq!(
            parse_voice_intent("note that we shipped v2"),
            VoiceIntent::NoteAside { text: "we shipped v2".into() }
        );
        assert_eq!(
            parse_voice_intent("notatka kolejny krok to deploy"),
            VoiceIntent::NoteAside { text: "kolejny krok to deploy".into() }
        );
    }

    #[test]
    fn intent_unknown_for_gibberish_and_empty() {
        assert_eq!(
            parse_voice_intent("asdf qwer zxcv"),
            VoiceIntent::Unknown { raw: "asdf qwer zxcv".into() }
        );
        assert_eq!(parse_voice_intent("   "), VoiceIntent::Unknown { raw: String::new() });
    }

    #[test]
    fn wake_then_intent_end_to_end() {
        // The two functions compose: detect → parse.
        let hit = detect_wake("klałdku zrób research o konkurencji").unwrap();
        assert_eq!(
            parse_voice_intent(&hit.command),
            VoiceIntent::Research { topic: "konkurencji".into() }
        );
    }
}
