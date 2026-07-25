//! Realtime Reactions — the "whisper" layer (spec §4). During a recording, far-side utterances are
//! checked against the user's own meeting history and, when they CONTRADICT a known fact, a private
//! card is surfaced to the user alone. This module is the on-device brain of that feature.
//!
//! ## Trustworthy by construction (the load-bearing idea)
//! The LLM NEVER judges. Its only job is upstream: turn a 2–3-sentence live window into
//! entity·predicate·object triples ([`crate::facts::extract_fact_candidates`]). The VERDICT — "this
//! contradicts a fact you already have" — is the DETERMINISTIC [`crate::facts::reconcile_facts`]: an
//! `Invalidate` op means the same `(entity, subject, predicate)` now carries a different value, and
//! the OLD fact (with its source `meeting_id`) is the EXTRACTIVE citation. A bad extraction yields a
//! missed card (safe); it can never fabricate a hallucinated accusation. [`cards_from_reconcile`] is
//! therefore a PURE function — headless-testable RED-before-GREEN — and carries the feature's value.
//!
//! ## Gating + privacy (spec §4.4)
//! The live detection ([`detect_reactions`]) reads existing facts ONLY through the visibility-gated
//! [`crate::storage::Db::list_facts_visible`] over the session `unlocked` set, and never for the
//! CURRENT meeting — a sealed-and-not-unlocked meeting's facts can surface in NO card. The reasoner is
//! the LOCAL light engine (Brain Live), so nothing egresses; a missing model yields zero candidates
//! (feature degraded, never a cloud call). Cards are ephemeral events, never persisted (P1–2).

use std::collections::HashSet;

use serde::Serialize;

use crate::facts::{extract_fact_candidates, reconcile_facts, Fact, FactCandidate, FactOp};
use crate::reason::{GenOptions, LocalReasoner};
use crate::storage::Db;

/// The kind of realtime whisper. (Deterministic `Recall` / commitment cards ship separately via
/// `proactive.rs`; this module owns the LLM-extraction-driven `Contradiction` class.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WhisperKind {
    /// The far side just asserted a value that conflicts with a fact you already recorded.
    Contradiction,
}

/// One realtime whisper card. Ephemeral (emitted as an event, not persisted). `old_quote` is the
/// EXTRACTIVE citation — a real prior fact value, never model-generated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhisperCard {
    pub kind: WhisperKind,
    /// Neutral one-line framing ("earlier X said Y") — never accusatory.
    pub summary: String,
    /// The old fact's value — the extractive citation.
    pub old_quote: String,
    /// The entity (subject) the fact is about.
    pub entity: String,
    /// The attribute that changed.
    pub predicate: String,
    /// The source meeting to open ([[wikilink]] / click-through), when known.
    pub source_meeting_id: Option<String>,
}

/// PURE core (spec §4.1 Stage 2): run the deterministic reconcile of live-window `candidates` against
/// the `existing` gated facts and turn each `Invalidate` (a changed value on a known
/// `(entity, subject, predicate)`) into a Contradiction card citing the OLD fact. `NoOp` (same value)
/// and `Add` (new topic) produce NOTHING — a paraphrase or a fresh fact is never a contradiction. The
/// ops are NOT applied (dry-run): a live recording mutates no facts.
pub fn cards_from_reconcile(
    existing: &[Fact],
    candidates: &[FactCandidate],
    at: &str,
) -> Vec<WhisperCard> {
    let ops = reconcile_facts(existing, candidates, at);
    let mut cards = Vec::new();
    for op in &ops {
        let FactOp::Invalidate { id, .. } = op else {
            continue; // NoOp / Add ⇒ no contradiction.
        };
        // The fact being closed is the citation. (Its object is the prior value the far side just
        // contradicted; its meeting_id is the source to open.)
        if let Some(old) = existing.iter().find(|f| &f.id == id) {
            cards.push(WhisperCard {
                kind: WhisperKind::Contradiction,
                summary: format!(
                    "Earlier, {} {} was \u{201c}{}\u{201d}",
                    old.subject, old.predicate, old.object
                ),
                old_quote: old.object.clone(),
                entity: old.subject.clone(),
                predicate: old.predicate.clone(),
                source_meeting_id: old.meeting_id.clone(),
            });
        }
    }
    cards
}

/// The IMPURE live-detection flow (spec §4.1 Stages 1–2), thin over the pure core. Extract triples
/// from the `window` via the LOCAL light `reasoner` (capped decode — [`GenOptions::light_extraction`]),
/// load the existing facts for the resolved entities through the GATED reader, and reconcile. Returns
/// the contradiction cards. Best-effort + panic-free: a stub/absent model yields no candidates ⇒ no
/// cards. `entities` is the caller's resolved (entity_id, name) set for the window (gated upstream);
/// `at` is the reconcile instant.
///
/// NOTE: Metal performance (per-call latency, contention with whisper) is Mac-verified only — this is
/// the on-device inference path the mistral.rs honest-scope header covers.
pub fn detect_reactions(
    reasoner: &dyn LocalReasoner,
    db: &Db,
    unlocked: &HashSet<String>,
    entities: &[(String, String)],
    window: &str,
    note_language: &str,
    at: &str,
) -> Vec<WhisperCard> {
    if entities.is_empty() || window.trim().is_empty() {
        return Vec::new();
    }
    // Stage 1 — light LLM extraction of the window into candidates (capped decode). The extractor is
    // language-pinned like the persisted path (facts.rs) so a Polish window can't emit a PL+EN twin.
    let candidates = extract_fact_candidates_capped(reasoner, window, entities, note_language);
    if candidates.is_empty() {
        return Vec::new();
    }
    // Load existing facts for exactly these entities through the GATED reader (a sealed meeting's
    // facts are absent). Per-entity read; a failed read skips that entity (never the scan).
    let mut existing: Vec<Fact> = Vec::new();
    for (entity_id, _) in entities {
        match db.list_facts_visible(entity_id, unlocked) {
            Ok(mut facts) => existing.append(&mut facts),
            Err(e) => {
                tracing::debug!(target: "reactions", error = %e, "gated facts read failed; skipping entity");
            }
        }
    }
    // Stage 2 — deterministic dry-run reconcile → contradiction cards.
    cards_from_reconcile(&existing, &candidates, at)
}

/// Extract candidates with the LIGHT realtime preset (capped output). Wraps
/// [`crate::facts::extract_fact_candidates`], which uses the reasoner's `structured` call; the cap is
/// applied via the reasoner's [`LocalReasoner::structured_with`] where honored (mistralrs), a no-op on
/// the stub/cloud. Best-effort — empty on stub/no model/decode failure.
fn extract_fact_candidates_capped(
    reasoner: &dyn LocalReasoner,
    window: &str,
    entities: &[(String, String)],
    note_language: &str,
) -> Vec<FactCandidate> {
    // The title is unused for a live window; pass a short marker. The cap rides GenOptions through the
    // extractor's structured_with call (see facts.rs).
    extract_fact_candidates(
        reasoner,
        "live",
        window,
        entities,
        note_language,
        GenOptions::light_extraction(),
    )
}

/// Chars of the live-transcript TAIL fed to one reactions scan — a bounded recent window (the far
/// side's latest utterances), kept small so the light extraction stays a ~2–3-sentence task.
/// LEGACY substrate: used only when the running bullets are empty/off (see [`reaction_window`]).
const REACTION_WINDOW_CHARS: usize = 600;

/// Chars of VERBATIM transcript tail appended after the running bullets in the L4 substrate —
/// the bullets carry the meeting's accumulated context, so the verbatim part only needs the very
/// latest utterance.
const REACTION_VERBATIM_TAIL_CHARS: usize = 300;

/// Brain v2 L4 — assemble the scan window (PURE): with non-empty running `bullets` the substrate
/// is `bullets + the last `[`REACTION_VERBATIM_TAIL_CHARS`]` chars verbatim` (the bullets give the
/// extraction the meeting's context, the tail gives it the fresh utterance); with bullets
/// empty/off it is EXACTLY the legacy [`REACTION_WINDOW_CHARS`] tail — byte-identical behavior.
///
/// ACCEPTED POSTURE (lock-security nit, 2026-07-10): the `bullets` input is the UNGATED RAM read
/// of `AppState::live_bullets` in [`reactions_scan`] — the same class as the pre-existing ungated
/// `live_transcript` tail (the CURRENT recording's own captions, already on-screen). It feeds
/// ONLY the on-device light engine (never a cloud call); the widened exposure is that a
/// WhisperCard can QUOTE from the whole-meeting bullets digest (up to 4k chars) rather than the
/// 600-char raw tail, and the boundary queue (≤30s max-hold + drain-on-stop) can emit such a card
/// shortly after a mid-recording relock. Accepted because the substrate never leaves the device,
/// the RAM buffer is cleared at recording start/Stop + the lock-surface idle hygiene, and every
/// PERSISTED / prompt-injected read of the bullets IS gated (`gated_live_bullets`, fail-closed).
pub(crate) fn reaction_window(bullets: &str, live: &str) -> String {
    let b = bullets.trim();
    if b.is_empty() {
        return tail(live, REACTION_WINDOW_CHARS);
    }
    format!("{b}\n{}", tail(live, REACTION_VERBATIM_TAIL_CHARS))
}

/// The gated result of one live-loop reactions scan: the cards + whether they should be EMITTED (the
/// contradiction sub-toggle is ON) or SHADOW-counted (OFF). Computed OFF the live tick thread.
pub struct ReactionScan {
    pub cards: Vec<WhisperCard>,
    pub emit: bool,
}

/// One Realtime-Reactions scan wired to the running app (spec §4). GATED: no cards unless Brain Live
/// is ON and the LIGHT model is present (a stub reasoner yields nothing — feature degraded, NEVER a
/// cloud call). Reads the scan substrate ([`reaction_window`] — running bullets + a short verbatim
/// tail when the L4 bullets are on, the legacy raw tail otherwise) and runs [`detect_reactions`] on
/// the light engine. `emit` mirrors the `brain_contradiction_cards` sub-toggle (OFF ⇒ shadow: count,
/// don't show). IMPURE, Mac-verified inference path; the CALLER runs it OFF the live tick thread.
///
/// Brain v2 L4: `entities` is the caller's tick-thread [`crate::transcribe::novelty::RefreshableEntityCache`]
/// snapshot (the gated `list_entities_visible` read, refreshed every ~60 ticks) — passed in so the
/// scan stops re-fetching the list per scan. Staleness affects TRIGGER/candidate names only, never
/// content: the fact reads inside [`detect_reactions`] re-check `list_facts_visible` against the
/// FRESH unlocked set, so a mid-recording relock still surfaces nothing.
pub fn reactions_scan(
    app: &tauri::AppHandle,
    now: &str,
    entities: &[(String, String)],
) -> ReactionScan {
    use tauri::Manager;
    let empty = ReactionScan {
        cards: Vec::new(),
        emit: false,
    };
    let st = app.state::<crate::state::AppState>();
    // Brain v2 P0.3 — USER-TURN PRIORITY: a user-initiated assistant turn is in flight → defer this
    // background scan entirely (before any config/model/DB work), so the background light-model
    // extraction never competes with a user-facing answer for the on-device engine. The next
    // scheduled scan (~21 s later) picks up naturally once the turn's RAII guard clears the flag.
    if should_defer_scan(&st.user_turn_in_progress) {
        tracing::debug!(target: "reactions", "user turn in flight; deferring scan");
        return empty;
    }
    let (brain_live, emit, note_language) = match st.config.lock() {
        Ok(c) => (
            c.brain_live,
            c.brain_contradiction_cards,
            c.note_language.clone(),
        ),
        Err(_) => return empty,
    };
    if !brain_live {
        return empty;
    }
    let reasoner = st.reasoner.light();
    if reasoner.id() == "stub" {
        return empty; // no light model ⇒ no extraction (never a cloud call)
    }
    let unlocked = st
        .unlocked_folders
        .lock()
        .map(|u| u.clone())
        .unwrap_or_default();
    // Brain v2 L4 substrate: running bullets (RAM — populated only when `live_bullets_enabled`
    // AND a light model is present, so empty ⇒ the legacy raw tail, byte-identical behavior).
    // Both inputs stay on-device: the window feeds ONLY the local light extraction below.
    let bullets = st
        .live_bullets
        .lock()
        .map(|b| b.clone())
        .unwrap_or_default();
    let live = st
        .live_transcript
        .lock()
        .map(|b| b.clone())
        .unwrap_or_default();
    let window = reaction_window(&bullets, &live);
    if window.trim().is_empty() {
        return ReactionScan {
            cards: Vec::new(),
            emit,
        };
    }
    let entities = filter_window_entities(entities, &window);
    let cards = detect_reactions(
        &*reasoner,
        &st.db,
        &unlocked,
        &entities,
        &window,
        &note_language,
        now,
    );
    // SESSION dedup (deep-review): a contradiction surfaces at most ONCE per recording — else the same
    // card re-emits every ~21 s scan and (in shadow mode) re-inflates the calibration count. Keyed on
    // (entity | predicate | old-value); `HashSet::insert` returns true only for a NOT-yet-seen key.
    let cards = {
        let mut seen = match st.reactions_emitted.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        cards
            .into_iter()
            .filter(|c| seen.insert(format!("{}|{}|{}", c.entity, c.predicate, c.old_quote)))
            .collect()
    };
    ReactionScan { cards, emit }
}

/// Brain v2 P0.3 — the PURE deferral decision for [`reactions_scan`]: defer while a user-initiated
/// assistant turn is in flight (`AppState::user_turn_in_progress`). Factored off `AppHandle` so the
/// contract is headless-testable; `Relaxed` suffices — this is an advisory scheduling hint, not a
/// synchronization edge.
fn should_defer_scan(user_turn_in_progress: &std::sync::atomic::AtomicBool) -> bool {
    user_turn_in_progress.load(std::sync::atomic::Ordering::Relaxed)
}

/// The last `n` chars of `s` on a char boundary.
fn tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        s.to_string()
    } else {
        s.chars().skip(count - n).collect()
    }
}

/// Filter the caller-supplied VISIBLE entity list (the tick thread's shared cache — gated at fetch
/// time by `list_entities_visible`) down to the entities whose name appears in the `window`
/// (case-insensitive substring), for fact lookup. The current meeting is not special-cased: its
/// facts aren't persisted mid-recording, so there is nothing of its own to surface. Pure —
/// headless-testable; the content gate stays downstream (`list_facts_visible` on the FRESH
/// unlocked set inside [`detect_reactions`]).
pub(crate) fn filter_window_entities(
    entities: &[(String, String)],
    window: &str,
) -> Vec<(String, String)> {
    let wl = window.to_lowercase();
    entities
        .iter()
        .filter(|(_, name)| {
            let name = name.trim();
            !name.is_empty() && wl.contains(&name.to_lowercase())
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(id: &str, subject: &str, predicate: &str, object: &str, meeting: &str) -> Fact {
        Fact {
            id: id.to_string(),
            entity_id: "e-atlas".to_string(),
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            valid_from: "2026-03-03T00:00:00Z".to_string(),
            valid_to: None,
            recorded_at: "2026-03-03T00:00:00Z".to_string(),
            meeting_id: Some(meeting.to_string()),
            confidence: 1.0,
        }
    }

    fn cand(subject: &str, predicate: &str, object: &str) -> FactCandidate {
        FactCandidate {
            entity_id: "e-atlas".to_string(),
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            confidence: 1.0,
        }
    }

    #[test]
    fn planted_contradiction_yields_one_card_citing_the_old_fact() {
        // RED-before-GREEN: existing "firm", far side now says "flexible" ⇒ exactly one Contradiction
        // card citing the OLD value + its source meeting.
        let existing = vec![fact(
            "f1",
            "Project Atlas",
            "deadline",
            "May 30 firm",
            "m-kickoff",
        )];
        let cands = vec![cand("Project Atlas", "deadline", "flexible")];
        let cards = cards_from_reconcile(&existing, &cands, "2026-07-04T00:00:00Z");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].kind, WhisperKind::Contradiction);
        assert_eq!(cards[0].old_quote, "May 30 firm");
        assert_eq!(cards[0].source_meeting_id.as_deref(), Some("m-kickoff"));
        assert!(cards[0].summary.contains("deadline"));
    }

    #[test]
    fn same_value_paraphrase_yields_no_card() {
        // NoOp: the far side restates the SAME value ⇒ no contradiction (false-positive guard).
        let existing = vec![fact(
            "f1",
            "Project Atlas",
            "deadline",
            "May 30 firm",
            "m-kickoff",
        )];
        let cands = vec![cand("Project Atlas", "deadline", "May 30 firm")];
        assert!(cards_from_reconcile(&existing, &cands, "2026-07-04T00:00:00Z").is_empty());
    }

    #[test]
    fn brand_new_fact_yields_no_card() {
        // Add: a topic with no prior fact ⇒ nothing to contradict.
        let existing = vec![fact(
            "f1",
            "Project Atlas",
            "deadline",
            "May 30 firm",
            "m-kickoff",
        )];
        let cands = vec![cand("Project Atlas", "owner", "Marcus")];
        assert!(cards_from_reconcile(&existing, &cands, "2026-07-04T00:00:00Z").is_empty());
    }

    #[test]
    fn no_candidates_no_cards() {
        let existing = vec![fact(
            "f1",
            "Project Atlas",
            "deadline",
            "May 30 firm",
            "m-kickoff",
        )];
        assert!(cards_from_reconcile(&existing, &[], "2026-07-04T00:00:00Z").is_empty());
    }

    /// Brain v2 P0.3 — the deferral decision: the scan defers exactly while the user-turn priority
    /// flag is set, and resumes once the turn's RAII guard clears it. (The `reactions_scan` wiring
    /// itself needs an `AppHandle` and is exercised on a real Mac; this pins the pure decision.)
    #[test]
    fn scan_defers_only_while_a_user_turn_is_in_flight() {
        let flag = std::sync::atomic::AtomicBool::new(false);
        assert!(!should_defer_scan(&flag), "idle: the scan runs");
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(should_defer_scan(&flag), "user turn in flight: defer");
        flag.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(!should_defer_scan(&flag), "turn ended: the scan resumes");
    }

    /// Brain v2 L4 — the scan substrate: with bullets EMPTY the window is byte-identical to the
    /// legacy 600-char tail; with bullets present it is `bullets + last-300-chars verbatim`.
    #[test]
    fn reaction_window_is_legacy_tail_without_bullets_and_bullets_plus_tail_with() {
        let live = "x".repeat(1_000);
        // Empty / whitespace bullets ⇒ EXACTLY the legacy tail.
        assert_eq!(reaction_window("", &live), tail(&live, 600));
        assert_eq!(reaction_window("   \n", &live), tail(&live, 600));
        // Bullets present ⇒ bullets first, then only the short verbatim tail.
        let w = reaction_window("- [deal]: pricing agreed", &live);
        assert!(w.starts_with("- [deal]: pricing agreed\n"));
        let verbatim = w.split_once('\n').unwrap().1;
        assert_eq!(
            verbatim.chars().count(),
            300,
            "verbatim tail bounded at 300"
        );
    }

    /// Brain v2 L4 — the entity filter over the SHARED tick-thread cache: whole-window
    /// case-insensitive containment, empty names dropped, non-mentioned entities dropped.
    #[test]
    fn filter_window_entities_matches_case_insensitively() {
        let entities = vec![
            ("e1".to_string(), "Atlas".to_string()),
            ("e2".to_string(), "Kraken".to_string()),
            ("e3".to_string(), "  ".to_string()),
        ];
        let hits =
            filter_window_entities(&entities, "wracamy do tematu atlas w przyszłym tygodniu");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "e1");
        assert!(filter_window_entities(&entities, "nic o nich").is_empty());
    }

    #[test]
    fn tail_returns_last_n_chars_on_char_boundary() {
        assert_eq!(tail("hello", 3), "llo");
        assert_eq!(tail("hi", 5), "hi");
        // Multi-byte safe (Polish diacritics): never splits a char.
        assert_eq!(tail("zażółć", 3), "ółć");
    }
}
