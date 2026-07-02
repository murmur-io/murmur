//! Proactive brain P1 — ZERO-EGRESS recall surfacing during a recording.
//!
//! While a recording is live, the deterministic matcher here periodically scans the NEW portion
//! of the live-caption tail against the local substrates (entities, open commitments, current
//! facts, FTS over past meetings) and surfaces AT MOST one dismissible recall card
//! ("you discussed this on Jun 12 → [[meeting]]", "open commitment: Anna — pricing doc") via
//! [`crate::events::EVENT_PROACTIVE_HINT`].
//!
//! Design contract (spec `docs/superpowers/specs/2026-07-02-proactive-brain-design.md`):
//! - **D1 — deterministic, zero egress.** NO LLM call, NO provider, NO consent gate anywhere in
//!   this path: candidate generation and ranking are pure local reads. Nothing leaves the process.
//! - **D2 — the throttle IS the contract.** At most one card per [`COOLDOWN_TICKS`] (≥120 s),
//!   session-level dedup by `(kind, target_id)`, a minimum relevance score
//!   ([`SCORE_THRESHOLD`]), and a backend-side mute (`proactive_hints_enabled` OFF ⇒ the matcher
//!   never runs — not just UI hiding). All state is per-recording.
//! - **D3 — every surfaced item is visibility-gated at the source.** All DB reads go through the
//!   EXISTING gated helpers (`list_entities_visible`, `search_visible`, `list_open_commitments`,
//!   `list_facts_visible`, `entity_mentions_visible`) — a sealed-and-not-session-unlocked meeting
//!   contributes NOTHING. The unlocked set is re-read fresh per scan (same discipline as
//!   `GatedToolExecutor`).
//! - **D4 — piggyback the live loop.** [`scan_tick`] runs inside `transcribe::live::run()` every
//!   tick; a scan only happens every [`SCAN_EVERY_TICKS`] ticks over the tail DELTA since the
//!   last scan. The payload carries IDs + a SHORT title from an already-visible row — never
//!   content bodies.
//!
//! The matcher core ([`gather_candidates`] + [`ProactiveState::step`]) is pure over injected
//! inputs (a `Db`, the unlocked set, the buffer, an injected `now`), so the whole contract is
//! headless-testable without the audio loop.

use std::collections::HashSet;

use chrono::{DateTime, Utc};

use crate::events::ProactiveHintPayload;
use crate::storage::{Commitment, Db, GraphNode, Meeting};

// ── Throttle constants (D2) ────────────────────────────────────────────────────────────────────

/// Scan every K live ticks. The live loop ticks every ~3 s (`transcribe::live::TICK`), so 10
/// ticks ≈ 30 s — the spec D4 default cadence. Ticks are the deterministic clock here (no
/// wall-clock reads inside the matcher), which keeps the throttle headless-testable.
pub const SCAN_EVERY_TICKS: u32 = 10;

/// Minimum ticks between two EMISSIONS: 40 × 3 s = 120 s — the spec D2 hard cooldown. While the
/// cooldown is live the scan is skipped entirely (the delta simply accrues for the next scan).
pub const COOLDOWN_TICKS: u32 = 40;

/// Minimum relevance for ANY emission (D2: noise is the #1 product risk — conservative). On the
/// weight scale below: an entity-exact match always clears it while fresh; a term-only FTS match
/// clears it only at rank 0 on a recent meeting; everything weaker is dropped.
pub const SCORE_THRESHOLD: f32 = 0.4;

// ── Matcher constants ──────────────────────────────────────────────────────────────────────────

/// Max chars of freshly-accumulated live text one scan looks at (spec §2 perf envelope: an
/// entity scan over a ≤2 kB delta). When scans were skipped (cooldown / flag off) and more text
/// accrued, the most RECENT tail wins — recency is what a live hint is for.
const MAX_DELTA_CHARS: usize = 2_000;

/// Trailing chars remembered from the previous scan position, used to RE-LOCATE the scan point
/// after the live buffer front-trims at its cap (`MAX_LIVE_TRANSCRIPT_CHARS` in
/// `transcribe::live`) — once trimmed, plain offset arithmetic silently points at the WRONG text.
const ANCHOR_CHARS: usize = 120;

/// Top-N rare tokens of the delta run through gated FTS — bounds the scan at ≤5 `search_visible`
/// queries (spec §2 perf envelope).
const MAX_TERMS: usize = 5;

/// FTS candidates kept per term (spec §2: `search_visible(t, limit 3)`).
const FTS_LIMIT: i64 = 3;

/// Max entities matched per scan — bounds the per-entity fact/mention queries. The gated entity
/// list is ordered by visible mention count DESC, so the cap keeps the most prominent matches.
const MAX_MATCHED_ENTITIES: usize = 3;

/// Max open-commitment candidates per scan (they are pre-sorted by due date, soonest first).
const MAX_COMMITMENT_CANDIDATES: usize = 5;

/// Minimum token length (chars) for the rare-term FTS leg — shorter tokens are noise.
const MIN_TERM_CHARS: usize = 5;

/// Minimum entity-name length eligible for delta matching (mirrors the GraphRAG-lite resolver's
/// guard in `storage::db`): 1–2 char names are too noisy as spoken-text matches.
const MIN_ENTITY_CHARS: usize = 3;

/// Max chars of a card title. Titles come from already-visible rows (meeting title / commitment
/// line / fact triple) — short display strings, never content bodies.
const TITLE_MAX_CHARS: usize = 120;

// ── Scoring (spec §2: specificity × recency decay × rank) ─────────────────────────────────────

/// An OPEN commitment for a matched entity. No recency decay: an open item stays relevant until
/// checked off (the gated reader already drops `- [x]` lines), and surfacing a forgotten old one
/// is exactly the value.
const W_COMMITMENT: f32 = 1.0;

/// The last VISIBLE meeting that mentioned a matched entity ("you discussed this on …"). Entity
/// exact > term, decayed by meeting age.
const W_ENTITY_MEETING: f32 = 0.9;

/// A CURRENT fact (`valid_to IS NULL`) about a matched entity. Flat — a currently-valid fact is
/// current by definition; slightly under the meeting/commitment legs so it surfaces when it is
/// the only signal.
const W_FACT: f32 = 0.8;

/// A rare-term FTS hit (BM25) — the weakest specificity; additionally decayed by meeting age and
/// divided by (1 + rank), so only a rank-0 hit on a recent meeting clears [`SCORE_THRESHOLD`].
const W_TERM: f32 = 0.5;

/// Recency half-life in days for the decaying legs. 90 days keeps a last-quarter discussion
/// surfaceable while pushing year-old term noise under the threshold. Tuning this (and the
/// threshold) is the planned dogfooding pass — see the spec's honesty bar.
const RECENCY_HALF_LIFE_DAYS: f32 = 90.0;

/// Decay floor, so an ancient-but-exact signal is dampened, not annihilated (it still needs the
/// kind weight to clear the threshold).
const RECENCY_FLOOR: f32 = 0.2;

/// What a card points at. Serialized as the payload's `kind` string — the FE switch key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HintKind {
    /// A past VISIBLE meeting relevant to what is being said ("you discussed this on …").
    PastMeeting,
    /// An OPEN `- [ ]` action item from a visible meeting, owned by / mentioning a matched entity.
    OpenCommitment,
    /// A currently-valid (`valid_to IS NULL`) fact about a matched entity.
    Fact,
}

impl HintKind {
    /// Stable lowercase token carried in [`ProactiveHintPayload::kind`].
    pub fn as_str(self) -> &'static str {
        match self {
            HintKind::PastMeeting => "past_meeting",
            HintKind::OpenCommitment => "open_commitment",
            HintKind::Fact => "fact",
        }
    }
}

/// One scored candidate produced by [`gather_candidates`]. `target_id` is the session-dedup key
/// (meeting id for past-meeting + commitment cards, fact id for fact cards); `title` is a short
/// display string taken from an already-VISIBLE row.
#[derive(Debug, Clone)]
pub struct HintCandidate {
    pub kind: HintKind,
    pub title: String,
    pub target_id: String,
    pub meeting_id: Option<String>,
    pub score: f32,
}

impl HintCandidate {
    fn into_payload(self) -> ProactiveHintPayload {
        ProactiveHintPayload {
            kind: self.kind.as_str().to_string(),
            title: self.title,
            target_id: self.target_id,
            meeting_id: self.meeting_id,
            score: self.score,
        }
    }
}

// ── The per-recording scanner state (throttle + dedup + delta tracking) ───────────────────────

/// Tracks how far into the live buffer the scanner has already looked, surviving the buffer's
/// front-trim at its cap. Offsets are in CHARS (the buffer is trimmed on char boundaries).
#[derive(Default)]
struct DeltaTracker {
    /// Chars of the buffer already scanned — valid only while the buffer is append-only.
    offset: usize,
    /// The last ≤[`ANCHOR_CHARS`] chars ending at `offset` at the previous scan. When the buffer
    /// front-trims (offset arithmetic then lies), the scan point is re-located by finding this
    /// text; when even the anchor was trimmed away, the scan falls back to the bounded recent
    /// tail — the session dedup + cooldown absorb any re-scan of already-seen text.
    anchor: String,
}

impl DeltaTracker {
    /// The NEW text since the last scan (most recent ≤[`MAX_DELTA_CHARS`] of it), advancing the
    /// tracker. Never returns stale text as "new" on a plain append; on a front-trim/rewrite it
    /// degrades to a bounded recent-tail re-scan rather than ever slicing at a lying offset.
    fn take_delta(&mut self, buf: &str) -> String {
        let chars: Vec<char> = buf.chars().collect();
        let total = chars.len();
        let anchor: Vec<char> = self.anchor.chars().collect();
        // Fast path: append-only growth — the remembered anchor still sits right before `offset`.
        let anchored = self.offset <= total
            && anchor.len() <= self.offset
            && chars[self.offset - anchor.len()..self.offset] == anchor[..];
        let start = if anchored {
            self.offset
        } else if !anchor.is_empty() {
            // The buffer front-trimmed at its cap (or was rewritten): re-locate the previous
            // scan point by its anchor text; if the anchor itself was trimmed away, scan only
            // the bounded recent tail.
            match find_last_run(&chars, &anchor) {
                Some(pos) => pos + anchor.len(),
                None => total.saturating_sub(MAX_DELTA_CHARS),
            }
        } else {
            0 // first scan of this recording — everything is new.
        };
        // Keep the most RECENT window when more than the per-scan budget accrued.
        let start = start.min(total).max(total.saturating_sub(MAX_DELTA_CHARS));
        let delta: String = chars[start..].iter().collect();
        self.offset = total;
        self.anchor = chars[total.saturating_sub(ANCHOR_CHARS)..].iter().collect();
        delta
    }
}

/// Rightmost index where `needle` occurs in `hay` as a contiguous run of chars.
fn find_last_run(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).rev().find(|&i| hay[i..i + needle.len()] == needle[..])
}

/// Per-recording state of the proactive scanner: the tick/cooldown clock, the session dedup set,
/// and the delta tracker. Created fresh by the live loop at every recording start, so nothing
/// (cooldown, dedup, offsets) leaks across recordings.
#[derive(Default)]
pub struct ProactiveState {
    /// Live ticks seen this recording (~3 s each).
    tick: u32,
    /// Ticks remaining until the next emission is allowed (0 = free).
    cooldown: u32,
    /// `(kind, target_id)` pairs already emitted this recording — never emitted twice (D2).
    emitted: HashSet<(HintKind, String)>,
    delta: DeltaTracker,
}

impl ProactiveState {
    /// One live-loop tick. Advances the clock every call; on every [`SCAN_EVERY_TICKS`]-th tick —
    /// when `enabled` and outside the cooldown — extracts the new delta, gathers gated candidates,
    /// and returns at most one payload to emit. `enabled == false` mutes the matcher at the
    /// source: no delta is consumed, no DB read happens (D2 backend-side mute).
    pub fn step(
        &mut self,
        enabled: bool,
        db: &Db,
        unlocked: &HashSet<String>,
        current_meeting_id: &str,
        live_buf: &str,
        now: DateTime<Utc>,
    ) -> Option<ProactiveHintPayload> {
        self.tick = self.tick.wrapping_add(1);
        if self.cooldown > 0 {
            self.cooldown -= 1;
        }
        if !enabled {
            return None; // muted: the matcher never runs (the delta accrues untouched).
        }
        if self.tick % SCAN_EVERY_TICKS != 0 {
            return None;
        }
        if self.cooldown > 0 {
            return None; // D2 cooldown: skip the whole scan; the delta accrues for the next one.
        }
        let delta = self.delta.take_delta(live_buf);
        if delta.trim().is_empty() {
            return None; // nothing new was said — no reads, no event.
        }
        let candidates = gather_candidates(db, unlocked, current_meeting_id, &delta, now);
        self.pick(candidates)
    }

    /// Threshold + session-dedup + best-of selection (the D2 emission contract): the highest-
    /// scoring candidate that clears [`SCORE_THRESHOLD`] and was not already emitted this
    /// recording. On an emission the cooldown re-arms and the `(kind, target_id)` is recorded;
    /// a rejected candidate is NOT recorded (a stronger later signal for the same target may
    /// still surface it).
    fn pick(&mut self, candidates: Vec<HintCandidate>) -> Option<ProactiveHintPayload> {
        let best = candidates
            .into_iter()
            .filter(|c| c.score >= SCORE_THRESHOLD)
            .filter(|c| !self.emitted.contains(&(c.kind, c.target_id.clone())))
            .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal))?;
        self.emitted.insert((best.kind, best.target_id.clone()));
        self.cooldown = COOLDOWN_TICKS;
        Some(best.into_payload())
    }
}

/// One live-loop tick of the proactive scanner, wired to the running app: snapshot the LIVE
/// config flag + unlocked set + current meeting + live buffer (re-read per scan — the same
/// freshness discipline as `GatedToolExecutor`), then run the deterministic matcher. The caller
/// (the live loop) emits the returned payload. Best-effort + panic-free: a poisoned config lock
/// counts as DISABLED and a poisoned unlocked-set lock as EMPTY (fail-closed on both).
pub fn scan_tick(
    app: &tauri::AppHandle,
    state: &mut ProactiveState,
) -> Option<ProactiveHintPayload> {
    use tauri::Manager;
    let app_state = app.state::<crate::state::AppState>();
    let enabled = app_state
        .config
        .lock()
        .map(|c| c.proactive_hints_enabled)
        .unwrap_or(false);
    let unlocked = app_state
        .unlocked_folders
        .lock()
        .map(|u| u.clone())
        .unwrap_or_default();
    let meeting_id = app_state
        .current_meeting
        .lock()
        .ok()
        .and_then(|m| m.map(|id| id.to_string()))
        .unwrap_or_default();
    let buf = app_state
        .live_transcript
        .lock()
        .map(|b| b.clone())
        .unwrap_or_default();
    state.step(enabled, &app_state.db, &unlocked, &meeting_id, &buf, Utc::now())
}

// ── Candidate gathering (D3: every read through an EXISTING gated helper) ─────────────────────

/// Gather scored candidates for one delta. ALL reads are through the existing visibility-gated
/// helpers over the injected `unlocked` set — a sealed-and-not-session-unlocked meeting can
/// contribute neither a candidate nor a title. The CURRENT meeting is excluded everywhere (a
/// live hint must point at the PAST). Best-effort per leg: a failed read skips that leg (logged
/// non-PII), never the scan.
pub fn gather_candidates(
    db: &Db,
    unlocked: &HashSet<String>,
    current_meeting_id: &str,
    delta: &str,
    now: DateTime<Utc>,
) -> Vec<HintCandidate> {
    let mut candidates = Vec::new();

    // (a) Entity leg — the GATED entity list (an entity mentioned only in sealed meetings is
    // already absent from it). Ordered by visible mention count DESC → the cap keeps prominent ones.
    let entities = db.list_entities_visible(unlocked).unwrap_or_else(|e| {
        tracing::debug!(target: "proactive", error = %e, "entity list read failed; skipping leg");
        Vec::new()
    });
    let matched = match_entities(delta, &entities);

    if !matched.is_empty() {
        // Open commitments: ONE gated pass, filtered in-memory to the matched entities (owner
        // match or a whole-token mention in the item text).
        match db.list_open_commitments(unlocked, None) {
            Ok(commitments) => {
                let mut kept = 0usize;
                for c in commitments {
                    if c.meeting_id == current_meeting_id || kept >= MAX_COMMITMENT_CANDIDATES {
                        continue;
                    }
                    let hit = matched.iter().any(|e| {
                        c.owner.as_deref().is_some_and(|o| fold(o) == fold(&e.name))
                            || contains_token_run(&fold_tokens(&c.text), &fold_tokens(&e.name))
                    });
                    if !hit {
                        continue;
                    }
                    kept += 1;
                    candidates.push(HintCandidate {
                        kind: HintKind::OpenCommitment,
                        title: commitment_title(&c),
                        target_id: c.meeting_id.clone(),
                        meeting_id: Some(c.meeting_id),
                        score: W_COMMITMENT,
                    });
                }
            }
            Err(e) => {
                tracing::debug!(target: "proactive", error = %e, "commitments read failed; skipping leg");
            }
        }

        for entity in &matched {
            // Current facts (`valid_to IS NULL`) about the entity — gated + fail-closed on
            // unattributed rows inside `list_facts_visible`.
            match db.list_facts_visible(&entity.id, unlocked) {
                Ok(facts) => {
                    if let Some(f) = facts
                        .iter()
                        .filter(|f| f.valid_to.is_none())
                        .find(|f| f.meeting_id.as_deref() != Some(current_meeting_id))
                    {
                        candidates.push(HintCandidate {
                            kind: HintKind::Fact,
                            title: truncate_chars(
                                &format!("{} {} {}", f.subject, f.predicate, f.object),
                                TITLE_MAX_CHARS,
                            ),
                            target_id: f.id.clone(),
                            meeting_id: f.meeting_id.clone(),
                            score: W_FACT,
                        });
                    }
                }
                Err(e) => {
                    tracing::debug!(target: "proactive", error = %e, "facts read failed; skipping leg");
                }
            }
            // Last VISIBLE meeting that mentioned the entity (newest first) — "you discussed
            // this on …".
            match db.entity_mentions_visible(&entity.id, unlocked) {
                Ok(mentions) => {
                    if let Some(m) =
                        mentions.iter().find(|m| m.meeting_id != current_meeting_id)
                    {
                        candidates.push(HintCandidate {
                            kind: HintKind::PastMeeting,
                            title: truncate_chars(
                                if m.title.trim().is_empty() { "(untitled)" } else { &m.title },
                                TITLE_MAX_CHARS,
                            ),
                            target_id: m.meeting_id.clone(),
                            meeting_id: Some(m.meeting_id.clone()),
                            score: W_ENTITY_MEETING * recency(&m.started_at, now),
                        });
                    }
                }
                Err(e) => {
                    tracing::debug!(target: "proactive", error = %e, "mentions read failed; skipping leg");
                }
            }
        }
    }

    // (b) Term leg — top-N rare tokens of the delta through gated FTS/BM25.
    for term in extract_rare_terms(delta, MAX_TERMS) {
        let hits = match db.search_visible(&term, FTS_LIMIT, unlocked) {
            Ok(h) => h,
            Err(e) => {
                tracing::debug!(target: "proactive", error = %e, "term search failed; skipping term");
                continue;
            }
        };
        for (rank, hit) in hits.iter().enumerate() {
            if hit.meeting.id == current_meeting_id {
                continue;
            }
            candidates.push(HintCandidate {
                kind: HintKind::PastMeeting,
                title: meeting_title(&hit.meeting),
                target_id: hit.meeting.id.clone(),
                meeting_id: Some(hit.meeting.id.clone()),
                score: W_TERM * recency(&hit.meeting.started_at, now) / (1.0 + rank as f32),
            });
        }
    }

    candidates
}

// ── Pure text matching helpers ─────────────────────────────────────────────────────────────────

/// Entities whose name occurs in the delta as a contiguous, in-order WHOLE-TOKEN run, matched
/// case- and (Polish-)diacritic-insensitively — "z Gawronskim" does not match, "z Gawroński" does
/// via its token; "atlasian" never matches "Atlas" (no substring matching). Capped at
/// [`MAX_MATCHED_ENTITIES`] in the input order (visible mention count DESC).
fn match_entities<'a>(delta: &str, entities: &'a [GraphNode]) -> Vec<&'a GraphNode> {
    let delta_tokens = fold_tokens(delta);
    let mut out = Vec::new();
    for entity in entities {
        if entity.name.chars().count() < MIN_ENTITY_CHARS {
            continue;
        }
        let name_tokens = fold_tokens(&entity.name);
        if name_tokens.is_empty() {
            continue;
        }
        if contains_token_run(&delta_tokens, &name_tokens) {
            out.push(entity);
            if out.len() >= MAX_MATCHED_ENTITIES {
                break;
            }
        }
    }
    out
}

/// Whether `needle` occurs in `hay` as a contiguous in-order run (whole tokens only).
fn contains_token_run(hay: &[String], needle: &[String]) -> bool {
    !needle.is_empty()
        && needle.len() <= hay.len()
        && hay.windows(needle.len()).any(|w| w == needle)
}

/// Unicode-lowercase + fold Polish diacritics (the codebase's PL-first normalization — mirrors
/// `transcribe::live::is_filler_token`). COMPARISON ONLY — displayed text keeps original bytes.
fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ź' | 'ż' => 'z',
            other => other,
        })
        .collect()
}

/// Fold + tokenize on non-alphanumeric boundaries; empty tokens dropped.
fn fold_tokens(s: &str) -> Vec<String> {
    fold(s)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// High-signal rare tokens of the delta for the FTS leg: ≥[`MIN_TERM_CHARS`] chars, not a
/// stopword, not all-numeric, deduped case/diacritic-insensitively; CAPITALIZED tokens first
/// (likely proper nouns — spec §2 "prefer capitalized"), then encounter order; first `n` kept.
/// Original casing is preserved (the FTS tokenizer is diacritics-insensitive anyway).
fn extract_rare_terms(delta: &str, n: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut capitalized = Vec::new();
    let mut plain = Vec::new();
    for raw in delta.split(|c: char| !c.is_alphanumeric()) {
        if raw.chars().count() < MIN_TERM_CHARS || raw.chars().all(|c| c.is_numeric()) {
            continue;
        }
        let key = fold(raw);
        if STOPWORDS.contains(&key.as_str()) || !seen.insert(key) {
            continue;
        }
        if raw.chars().next().is_some_and(char::is_uppercase) {
            capitalized.push(raw.to_string());
        } else {
            plain.push(raw.to_string());
        }
    }
    capitalized.into_iter().chain(plain).take(n).collect()
}

/// Common PL + EN words (≥5 chars, stored in FOLDED form) that carry no recall signal. Small and
/// conservative on purpose — a missed stopword costs one wasted (still gated, still local) FTS
/// query, never a leak.
const STOPWORDS: &[&str] = &[
    // English
    "about", "above", "actually", "after", "again", "always", "anything", "because", "before",
    "being", "below", "between", "could", "different", "doing", "during", "every", "everything",
    "first", "going", "gonna", "having", "little", "maybe", "might", "nothing", "other", "people",
    "pretty", "probably", "really", "right", "should", "since", "something", "sometimes", "still",
    "their", "there", "these", "thing", "things", "think", "those", "through", "today", "under",
    "until", "where", "which", "while", "would",
    // Polish (folded: diacritics stripped)
    "bardzo", "bedzie", "bedziemy", "chyba", "czyli", "dlatego", "dobra", "dobrze", "dzisiaj",
    "jakas", "jakies", "jakis", "jednak", "jeszcze", "jutro", "kazdy", "kiedy", "ktora", "ktore",
    "ktorego", "ktorej", "ktory", "mozemy", "mozna", "musimy", "nawet", "potem", "przeciez",
    "robimy", "rowniez", "sobie", "swoje", "szybko", "takze", "teraz", "troche", "trzeba",
    "tutaj", "wiadomo", "wlasciwie", "wlasnie", "wszyscy", "wszystko", "wtedy", "zaraz", "zeby",
];

// ── Scoring helpers ────────────────────────────────────────────────────────────────────────────

/// Exponential recency decay with a floor: `0.5^(age_days / half_life)`, clamped to
/// [`RECENCY_FLOOR`]. An unparseable timestamp scores the FLOOR (never a boost).
fn recency(started_at: &str, now: DateTime<Utc>) -> f32 {
    let Ok(t) = DateTime::parse_from_rfc3339(started_at) else {
        return RECENCY_FLOOR;
    };
    let age_days = (now - t.with_timezone(&Utc)).num_seconds().max(0) as f32 / 86_400.0;
    0.5f32.powf(age_days / RECENCY_HALF_LIFE_DAYS).max(RECENCY_FLOOR)
}

/// Short display title for a meeting row (already visible — it came out of a gated reader).
fn meeting_title(m: &Meeting) -> String {
    let t = m.title.as_deref().map(str::trim).filter(|t| !t.is_empty()).unwrap_or("(untitled)");
    truncate_chars(t, TITLE_MAX_CHARS)
}

/// Short display title for a commitment: `owner: text` when owned, else the item text.
fn commitment_title(c: &Commitment) -> String {
    let base = match c.owner.as_deref().map(str::trim).filter(|o| !o.is_empty()) {
        Some(owner) => format!("{owner}: {}", c.text),
        None => c.text.clone(),
    };
    truncate_chars(&base, TITLE_MAX_CHARS)
}

/// First `n` chars of `s`, `…`-suffixed when truncated (char-boundary safe).
fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let head: String = s.chars().take(n).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{EntityKind, Folder, Meeting, MeetingStatus, NoteRecord};

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn tmp_db(tag: &str) -> Db {
        let p = crate::storage::db::unique_temp_path(&format!("murmur-proactive-{tag}"), "sqlite");
        let _ = std::fs::remove_file(&p);
        Db::open_with_key(&p, TEST_DEK).unwrap()
    }

    fn seed_folder(db: &Db, id: &str, name: &str) {
        db.insert_folder(&Folder {
            id: id.to_string(),
            name: name.to_string(),
            path: name.to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-01T00:00:00Z".to_string(),
        })
        .unwrap();
    }

    fn seed_note(db: &Db, meeting_id: &str, title: &str, markdown: &str, folder_id: Option<&str>) {
        db.insert_meeting(&Meeting {
            id: meeting_id.to_string(),
            started_at: "2026-06-26T09:00:00Z".to_string(),
            ended_at: None,
            title: Some(title.to_string()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: meeting_id.to_string(),
            provider_id: "claude_code".to_string(),
            markdown: markdown.to_string(),
            created_at: "2026-06-26T09:05:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder(meeting_id, folder_id).unwrap();
    }

    fn seed_entity(db: &Db, name: &str, meeting_id: &str) -> String {
        let id = db.upsert_entity(name, EntityKind::Project).unwrap();
        db.add_mention(&id, meeting_id).unwrap();
        id
    }

    fn seed_fact(db: &Db, entity_id: &str, predicate: &str, object: &str, meeting_id: &str) {
        db.apply_fact_ops(&[crate::facts::FactOp::Add(crate::facts::NewFact {
            entity_id: entity_id.to_string(),
            subject: "Atlas".to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            valid_from: "2026-06-20T00:00:00Z".to_string(),
            recorded_at: "2026-06-20T00:00:00Z".to_string(),
            confidence: 1.0,
            meeting_id: Some(meeting_id.to_string()),
        })])
        .unwrap();
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-02T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn cand(kind: HintKind, target: &str, score: f32) -> HintCandidate {
        HintCandidate {
            kind,
            title: "t".to_string(),
            target_id: target.to_string(),
            meeting_id: Some(target.to_string()),
            score,
        }
    }

    // ── D3: a sealed-and-not-session-unlocked meeting NEVER surfaces (the P1 leak test) ────────

    /// RED evidence: swapping the term leg's `search_visible` for the ungated `search` (or
    /// dropping the gated entity list) makes this fail — the sealed Kraken meeting surfaces.
    /// GREEN: every leg reads through the existing gated helpers, so the sealed meeting
    /// contributes nothing; a session unlock makes it eligible again (the gate is the LIVE set,
    /// not a wipe).
    #[test]
    fn sealed_candidate_never_surfaces_and_reappears_on_unlock() {
        let db = tmp_db("sealed");
        // Visible meeting about Atlas.
        seed_note(&db, "m-open", "Atlas sync", "Atlas roadmap review", None);
        seed_entity(&db, "Atlas", "m-open");
        // Sealed meeting about Kraken (lock the folder flag directly — the rows survive at rest,
        // so this proves the READ GATE, independent of purge-on-seal).
        seed_folder(&db, "f-lock", "Secret");
        seed_note(&db, "m-sealed", "Kraken sync", "Kraken pricing plan", Some("f-lock"));
        seed_entity(&db, "Kraken", "m-sealed");
        db.set_folder_locked("f-lock", true, None).unwrap();

        let delta = "we should sync on Atlas and Kraken pricing this week";
        let sealed_set = HashSet::new();
        let candidates = gather_candidates(&db, &sealed_set, "m-cur", delta, now());
        assert!(!candidates.is_empty(), "the visible Atlas candidate must be found");
        for c in &candidates {
            assert_ne!(c.target_id, "m-sealed", "sealed meeting id leaked as a target");
            assert_ne!(
                c.meeting_id.as_deref(),
                Some("m-sealed"),
                "sealed meeting id leaked as a source"
            );
            assert!(
                !c.title.contains("Kraken"),
                "sealed content leaked into a title: {}",
                c.title
            );
        }

        // Session unlock → the same scan MAY now surface the Kraken meeting (reversible gate).
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let candidates = gather_candidates(&db, &unlocked, "m-cur", delta, now());
        assert!(
            candidates.iter().any(|c| c.meeting_id.as_deref() == Some("m-sealed")),
            "an unlocked folder's meeting must be eligible again"
        );
    }

    // ── D2: cooldown ≥120 s (40 ticks) between emissions ───────────────────────────────────────

    /// RED evidence: removing the cooldown skip in `step` (or zeroing `COOLDOWN_TICKS`) emits the
    /// second hint at tick 20 and fails the `50` assertion.
    #[test]
    fn cooldown_enforces_forty_ticks_between_emissions() {
        let db = tmp_db("cooldown");
        seed_note(&db, "m-a", "Anna 1:1", "- [ ] Anna — send the pricing document", None);
        seed_entity(&db, "Anna", "m-a");
        seed_note(&db, "m-b", "Robert 1:1", "- [ ] Robert — prepare the budget review", None);
        seed_entity(&db, "Robert", "m-b");

        let mut st = ProactiveState::default();
        let unlocked = HashSet::new();
        let mut buf = String::new();
        let mut emissions: Vec<(u32, ProactiveHintPayload)> = Vec::new();
        for tick in 1..=60u32 {
            if tick == 5 {
                buf.push_str("Anna wspominała o dokumencie cenowym");
            }
            if tick == 15 {
                buf.push_str(" Robert mówił o przeglądzie budżetu");
            }
            if let Some(p) = st.step(true, &db, &unlocked, "m-cur", &buf, now()) {
                emissions.push((tick, p));
            }
        }
        assert_eq!(emissions.len(), 2, "exactly two hints across 60 ticks");
        assert_eq!(emissions[0].0, 10, "first hint on the first scan tick");
        assert_eq!(
            emissions[1].0, 50,
            "second hint only after the 40-tick (≥120 s) cooldown — not at the tick-20/30/40 scans"
        );
        assert_eq!(emissions[0].1.kind, "open_commitment");
        assert_eq!(emissions[0].1.target_id, "m-a");
        assert_eq!(emissions[1].1.target_id, "m-b", "the delta-scoped scan surfaces Robert next");
    }

    // ── D2: session dedup by (kind, target_id) ─────────────────────────────────────────────────

    /// RED evidence: removing the `emitted.insert` (or the dedup filter) in `pick` re-emits the
    /// same commitment and fails both assertions.
    #[test]
    fn session_dedup_never_reemits_same_kind_and_target() {
        let mut st = ProactiveState::default();
        assert!(
            st.pick(vec![cand(HintKind::OpenCommitment, "m-1", 1.0)]).is_some(),
            "first emission goes out"
        );
        st.cooldown = 0; // force the cooldown out of the way — this test isolates dedup.
        assert!(
            st.pick(vec![cand(HintKind::OpenCommitment, "m-1", 1.0)]).is_none(),
            "the SAME (kind, target_id) must never emit twice in one recording"
        );
        // A different KIND for the same target is a different dedup key (a meeting card and a
        // commitment card about the same meeting are different information).
        assert!(
            st.pick(vec![cand(HintKind::PastMeeting, "m-1", 0.9)]).is_some(),
            "a different kind for the same target is not deduped"
        );
    }

    /// Integration flavor: across a long run with the same entity re-mentioned in every scan
    /// window, no (kind, target_id) pair is ever emitted twice.
    #[test]
    fn session_dedup_holds_across_repeated_mentions() {
        let db = tmp_db("dedup");
        seed_note(&db, "m-a", "Anna 1:1", "- [ ] Anna — send the pricing document", None);
        seed_entity(&db, "Anna", "m-a");

        let mut st = ProactiveState::default();
        let unlocked = HashSet::new();
        let mut buf = String::new();
        let mut emitted_keys = Vec::new();
        for tick in 1..=200u32 {
            if tick % 9 == 0 {
                buf.push_str(" znowu rozmawiamy o Anna i dokumencie");
            }
            if let Some(p) = st.step(true, &db, &unlocked, "m-cur", &buf, now()) {
                emitted_keys.push((p.kind.clone(), p.target_id.clone()));
            }
        }
        assert!(!emitted_keys.is_empty(), "the repeated mention emits at least once");
        let unique: HashSet<_> = emitted_keys.iter().cloned().collect();
        assert_eq!(
            unique.len(),
            emitted_keys.len(),
            "a (kind, target_id) pair re-emitted within one recording: {emitted_keys:?}"
        );
    }

    // ── D2: flag OFF ⇒ the matcher never runs (backend-side mute) ──────────────────────────────

    /// RED evidence: removing the `enabled` early-return in `step` emits at tick 10 and fails the
    /// first assertion.
    #[test]
    fn flag_off_never_scans_and_delta_is_not_consumed() {
        let db = tmp_db("flag-off");
        seed_note(&db, "m-a", "Anna 1:1", "- [ ] Anna — send the pricing document", None);
        seed_entity(&db, "Anna", "m-a");

        let mut st = ProactiveState::default();
        let unlocked = HashSet::new();
        let buf = "Anna wspominała o dokumencie cenowym".to_string();
        for _ in 1..=40u32 {
            assert!(
                st.step(false, &db, &unlocked, "m-cur", &buf, now()).is_none(),
                "flag OFF must never emit"
            );
        }
        assert!(st.emitted.is_empty(), "flag OFF must not record any emission state");
        // Flipping the flag ON mid-recording: the very next scan tick sees the text accrued while
        // muted as its delta — proof the muted ticks never consumed it.
        let mut emitted = None;
        for _ in 41..=50u32 {
            if let Some(p) = st.step(true, &db, &unlocked, "m-cur", &buf, now()) {
                emitted = Some(p);
            }
        }
        assert!(
            emitted.is_some(),
            "after unmuting, the accrued (unconsumed) delta must produce the hint"
        );
    }

    // ── Empty / unchanged delta ⇒ no reads, no event ───────────────────────────────────────────

    /// RED evidence: scanning the WHOLE buffer instead of the delta makes the post-cooldown scan
    /// at tick 50 re-match "Anna" and emit her FACT card (a different, non-deduped kind) —
    /// failing the second assertion. The delta-scoped scan sees nothing new and stays silent.
    #[test]
    fn unchanged_buffer_yields_no_second_event() {
        let db = tmp_db("empty-delta");
        seed_note(&db, "m-a", "Anna 1:1", "- [ ] Anna — send the pricing document", None);
        let anna = seed_entity(&db, "Anna", "m-a");
        seed_fact(&db, &anna, "role", "QA owner", "m-a");

        let mut st = ProactiveState::default();
        let unlocked = HashSet::new();
        let buf = "Anna wspominała o dokumencie cenowym".to_string();
        let mut emissions = Vec::new();
        for tick in 1..=120u32 {
            if let Some(p) = st.step(true, &db, &unlocked, "m-cur", &buf, now()) {
                emissions.push((tick, p));
            }
        }
        assert_eq!(emissions.len(), 1, "an UNCHANGED buffer must never produce a second event");
        assert_eq!(emissions[0].0, 10);
    }

    #[test]
    fn empty_buffer_never_emits() {
        let db = tmp_db("empty-buf");
        let mut st = ProactiveState::default();
        let unlocked = HashSet::new();
        for _ in 1..=30u32 {
            assert!(st.step(true, &db, &unlocked, "m-cur", "", now()).is_none());
        }
    }

    // ── Spec §3: an entity match surfaces its open commitment ──────────────────────────────────

    #[test]
    fn entity_match_surfaces_open_commitment() {
        let db = tmp_db("commitment");
        seed_note(
            &db,
            "m-a",
            "Anna 1:1",
            "# Notes\n- [ ] Anna — send the pricing document\n- [x] Anna — book the room",
            None,
        );
        seed_entity(&db, "Anna", "m-a");

        let candidates =
            gather_candidates(&db, &HashSet::new(), "m-cur", "co z dokumentem od Anna?", now());
        let commitment = candidates
            .iter()
            .find(|c| c.kind == HintKind::OpenCommitment)
            .expect("the entity match must surface its OPEN commitment");
        assert_eq!(commitment.target_id, "m-a");
        assert!(commitment.title.contains("pricing document"), "title: {}", commitment.title);
        assert!(
            commitment.score >= SCORE_THRESHOLD,
            "an open commitment for an exact entity match must clear the threshold"
        );
        assert!(
            !candidates.iter().any(|c| c.title.contains("book the room")),
            "a checked-off `- [x]` item is not an open commitment"
        );

        // Composed: the live-loop step emits it as the best candidate.
        let mut st = ProactiveState::default();
        let mut payload = None;
        for _ in 1..=10u32 {
            if let Some(p) =
                st.step(true, &db, &HashSet::new(), "m-cur", "co z dokumentem od Anna?", now())
            {
                payload = Some(p);
            }
        }
        let p = payload.expect("step must emit on the first scan tick");
        assert_eq!(p.kind, "open_commitment");
        assert_eq!(p.meeting_id.as_deref(), Some("m-a"));
    }

    // ── Spec §3: the threshold rejects weak matches ─────────────────────────────────────────────

    /// RED evidence: removing the `score >= SCORE_THRESHOLD` filter in `pick` emits the weak
    /// candidate and fails the first assertion.
    #[test]
    fn sub_threshold_match_is_rejected_and_not_dedup_poisoned() {
        let mut st = ProactiveState::default();
        assert!(
            st.pick(vec![cand(HintKind::PastMeeting, "m-1", SCORE_THRESHOLD - 0.01)]).is_none(),
            "a sub-threshold candidate must not emit"
        );
        // The rejection did NOT record the pair — a later, stronger signal still surfaces it.
        assert!(
            st.pick(vec![cand(HintKind::PastMeeting, "m-1", 0.9)]).is_some(),
            "a rejected candidate must stay eligible for a stronger later match"
        );
    }

    /// The weakest leg end-to-end: a term-only FTS hit clears the threshold ONLY at rank 0 on a
    /// recent meeting; rank ≥1 falls under it.
    #[test]
    fn term_leg_scores_rank_zero_above_threshold_and_rank_one_below() {
        let db = tmp_db("term");
        seed_note(&db, "m-t", "Q3 planning", "the Kwantyfikator initiative kickoff", None);

        let candidates = gather_candidates(
            &db,
            &HashSet::new(),
            "m-cur",
            "wracamy do tematu Kwantyfikator",
            now(),
        );
        let hit = candidates
            .iter()
            .find(|c| c.kind == HintKind::PastMeeting && c.target_id == "m-t")
            .expect("the rare term must surface the meeting through gated FTS");
        assert!(hit.score >= SCORE_THRESHOLD, "rank-0 recent term hit clears: {}", hit.score);
        // Rank 1 with the same recency is halved — under the threshold by construction.
        assert!(hit.score / 2.0 < SCORE_THRESHOLD, "a rank-1 term hit must fall under");
    }

    // ── Delta tracker: append, cap, front-trim relocation, reset ───────────────────────────────

    #[test]
    fn delta_tracker_returns_only_new_text_on_append() {
        let mut t = DeltaTracker::default();
        let a = "alpha bravo charlie".to_string();
        assert_eq!(t.take_delta(&a), a, "first scan sees everything");
        let b = format!("{a} delta echo");
        assert_eq!(t.take_delta(&b), " delta echo", "second scan sees only the appended tail");
        assert_eq!(t.take_delta(&b), "", "unchanged buffer yields an empty delta");
    }

    /// The live buffer front-trims at its 16k cap: the old offset then points at the WRONG text.
    /// The tracker re-locates the scan point via its anchor and returns ONLY the new tail. RED
    /// evidence: slicing at the raw offset returns a mid-word fragment of OLD text here.
    #[test]
    fn delta_tracker_survives_front_trim_via_anchor() {
        let mut t = DeltaTracker::default();
        // A buffer comfortably longer than the anchor, so the anchor survives the trim.
        let long: String = (0..40).map(|i| format!("word{i} ")).collect();
        let _ = t.take_delta(&long);
        // Front-trim 60 chars off (as the cap does) and append genuinely-new text.
        let trimmed = format!("{} FRESH tail here", &long[60..].trim_end());
        let delta = t.take_delta(&trimmed);
        assert_eq!(
            delta.trim_start(),
            "FRESH tail here",
            "only the new tail after relocation, got {delta:?}"
        );
        assert!(!delta.contains("word"), "no already-scanned text re-surfaced as new: {delta:?}");
    }

    #[test]
    fn delta_tracker_falls_back_to_bounded_tail_when_anchor_is_gone() {
        let mut t = DeltaTracker::default();
        let _ = t.take_delta("completely original first buffer content");
        // The buffer was fully replaced (anchor nowhere): degrade to a bounded re-scan of the
        // new content — never a panic, never a lying-offset slice.
        let replaced = "entirely different second buffer";
        assert_eq!(t.take_delta(replaced), replaced);
    }

    #[test]
    fn delta_tracker_caps_delta_to_recent_window() {
        let mut t = DeltaTracker::default();
        let _ = t.take_delta("start");
        let huge = format!("start{}", " x".repeat(3 * MAX_DELTA_CHARS));
        let delta = t.take_delta(&huge);
        assert_eq!(delta.chars().count(), MAX_DELTA_CHARS, "delta bounded to the recent window");
    }

    // ── Pure matching helpers ──────────────────────────────────────────────────────────────────

    #[test]
    fn match_entities_is_case_and_diacritic_insensitive_whole_token() {
        let node = |id: &str, name: &str| GraphNode {
            id: id.to_string(),
            name: name.to_string(),
            kind: EntityKind::Project,
            mention_count: 1,
        };
        let entities = vec![node("e1", "Atlas"), node("e2", "Gawroński"), node("e3", "Anna Kowalska")];
        // Case-insensitive token match.
        assert_eq!(match_entities("mówiliśmy o atlas wczoraj", &entities).len(), 1);
        // Diacritic-insensitive both ways.
        assert_eq!(match_entities("dzwonił Gawronski rano", &entities)[0].id, "e2");
        // Multi-token names need the contiguous in-order run.
        assert_eq!(match_entities("spotkanie z Anna Kowalska jutro", &entities)[0].id, "e3");
        assert!(match_entities("Kowalska Anna odwrotnie", &entities).is_empty());
        // NEVER a substring: "atlasian" must not match "Atlas".
        assert!(match_entities("we use atlasian tools", &entities).is_empty());
    }

    #[test]
    fn extract_rare_terms_prefers_capitalized_filters_noise() {
        let terms = extract_rare_terms(
            "Właśnie rozmawiamy o Kwantyfikator i jeszcze about 12345 harmonogramie",
            3,
        );
        assert_eq!(terms[0], "Kwantyfikator", "capitalized proper noun ranked first: {terms:?}");
        assert!(terms.contains(&"harmonogramie".to_string()), "long plain token kept: {terms:?}");
        assert!(!terms.contains(&"Właśnie".to_string()), "stopword dropped (folded match)");
        assert!(!terms.contains(&"about".to_string()), "EN stopword dropped");
        assert!(!terms.contains(&"12345".to_string()), "all-numeric token dropped");
        assert!(terms.len() <= 3, "top-N cap respected");
    }

    #[test]
    fn recency_decays_with_floor_and_handles_garbage() {
        let n = now();
        assert!(recency("2026-07-02T11:00:00Z", n) > 0.99, "an hour old ≈ no decay");
        let quarter = recency("2026-04-03T12:00:00Z", n); // ~90 days = one half-life
        assert!((0.45..0.55).contains(&quarter), "one half-life ≈ 0.5, got {quarter}");
        assert_eq!(recency("2020-01-01T00:00:00Z", n), RECENCY_FLOOR, "ancient hits the floor");
        assert_eq!(recency("not-a-date", n), RECENCY_FLOOR, "garbage timestamp = floor, no panic");
    }

    // ── Payload contract (FE consumes verbatim) ────────────────────────────────────────────────

    #[test]
    fn payload_serializes_camel_case_for_the_fe() {
        let p = cand(HintKind::OpenCommitment, "m-1", 0.8).into_payload();
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json.get("kind").and_then(|v| v.as_str()), Some("open_commitment"));
        assert_eq!(json.get("targetId").and_then(|v| v.as_str()), Some("m-1"));
        assert_eq!(json.get("meetingId").and_then(|v| v.as_str()), Some("m-1"));
        assert!(json.get("title").is_some());
        assert!(json.get("score").is_some());
        assert!(json.get("target_id").is_none(), "snake_case must not leak over IPC");
    }
}
