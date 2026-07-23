//! Brain v2 L4 — the LIVE-loop timing brains, all PURE and headless-testable:
//!
//! - [`NoveltyState`] — the NOVELTY GATEKEEPER that decides WHEN the reactions/bullets worker is
//!   worth spawning. It replaces the fixed `% REACTIONS_SCAN_EVERY == 0` cadence in
//!   `transcribe::live::run` with content-driven triggers (new speech, a question, a known-entity
//!   mention, a lull) under a hard minimum interval, so the light on-device engine runs when there
//!   is something to react to instead of on a blind clock.
//! - [`RefreshableEntityCache`] — a tick-driven cache of the VISIBLE entity `(id, name)` list,
//!   refreshed every [`ENTITY_CACHE_REFRESH_TICKS`] ticks and shared between the gatekeeper's
//!   entity-hit trigger and `brain_reactions::reactions_scan` (which previously re-fetched the
//!   gated list on every scan).
//! - [`BoundaryGate`] — the BOUNDARY-TIMED SURFACING decision: queued whisper cards / proactive
//!   hints are emitted at a conversational boundary (a short lull, or the caption ending on
//!   sentence-final punctuation), with a hard max-hold force-emit so nothing waits forever.
//!
//! Nothing in this module does I/O, touches an `AppHandle`, or reads the DB — the cache takes a
//! `fetch` closure so its refresh cadence is testable without a database. The wiring (worker
//! spawn, mpsc queue, event emit) lives in `transcribe::live::run` and is real-Mac-verified only.
//!
//! GATING NOTE: the cached entity list is (at most [`ENTITY_CACHE_REFRESH_TICKS`] ticks) STALE
//! against a mid-recording relock. That staleness affects only TRIGGER SENSITIVITY — a name match
//! may fire a scan — never content: every fact/content read downstream
//! (`Db::list_facts_visible`, `gated_live_context`) re-reads the LIVE unlocked set fresh, so a
//! sealed meeting's content still surfaces nowhere. No PII lives here beyond the in-RAM names the
//! gated fetch returned; nothing is logged.

// ── Novelty-gatekeeper constants (spec §L4) ─────────────────────────────────────────────────────

/// New chars accrued since the last fire that alone justify a scan (~a couple of sentences).
pub const NOVELTY_NEW_CHARS: usize = 120;

/// Ticks of TOTAL silence (zero new chars) after which pending text is scanned anyway —
/// 14 ticks × ~3 s = ~42 s: the speaker paused, so react to what was said.
pub const NOVELTY_LULL_TICKS: u32 = 14;

/// HARD floor between two fires: never faster than 5 ticks (~15 s), whatever the triggers say —
/// the light engine must never be re-spawned per tick on a chatty stream.
pub const MIN_FIRE_INTERVAL_TICKS: u32 = 5;

/// Ticks between refreshes of the visible-entity cache (~3 min at the 3 s tick).
pub const ENTITY_CACHE_REFRESH_TICKS: u32 = 60;

// ── Boundary-gate constants (spec §L4) ──────────────────────────────────────────────────────────

/// Ticks with zero new transcript chars that count as a conversational lull (~8–9 s).
pub const BOUNDARY_LULL_TICKS: u32 = 3;

/// Hard cap on how many ticks a queued surface may be HELD waiting for a boundary (10 × 3 s =
/// 30 s) — past it the queue force-emits mid-sentence rather than sitting on a stale card.
pub const MAX_HOLD_TICKS: u32 = 10;

/// Max chars of freshly-accumulated live text one gatekeeper tick inspects (mirrors the proactive
/// scanner's per-scan budget — recency is what a live trigger is for).
const MAX_DELTA_CHARS: usize = 2_000;

/// Trailing chars remembered from the previous tick position, used to RE-LOCATE the scan point
/// after the live buffer front-trims at its cap (mirrors `proactive::DeltaTracker` — once the
/// buffer trims, plain offset arithmetic silently points at the WRONG text).
const ANCHOR_CHARS: usize = 120;

/// Anchor-disciplined per-tick delta over the (front-trimming) live buffer. A deliberate mirror of
/// `proactive::DeltaTracker` (which stays private to its module and must not be touched — L4
/// scope): offsets are in CHARS; on a front-trim the scan point is re-located by the remembered
/// anchor text, and when even the anchor is gone it degrades to the bounded recent tail rather
/// than ever slicing at a lying offset.
#[derive(Default)]
pub struct TickDelta {
    /// Chars of the buffer already seen — valid only while the buffer is append-only.
    offset: usize,
    /// The last ≤[`ANCHOR_CHARS`] chars ending at `offset` at the previous tick.
    anchor: String,
}

impl TickDelta {
    /// The NEW text since the last tick (most recent ≤[`MAX_DELTA_CHARS`] of it), advancing the
    /// tracker. Never returns stale text as "new" on a plain append; on a front-trim/rewrite it
    /// degrades to a bounded recent-tail re-scan.
    pub fn take_delta(&mut self, buf: &str) -> String {
        let chars: Vec<char> = buf.chars().collect();
        let total = chars.len();
        let anchor: Vec<char> = self.anchor.chars().collect();
        let anchored = self.offset <= total
            && anchor.len() <= self.offset
            && chars[self.offset - anchor.len()..self.offset] == anchor[..];
        let start = if anchored {
            self.offset
        } else if !anchor.is_empty() {
            match find_last_run(&chars, &anchor) {
                Some(pos) => pos + anchor.len(),
                None => total.saturating_sub(MAX_DELTA_CHARS),
            }
        } else {
            0
        };
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
    (0..=hay.len() - needle.len())
        .rev()
        .find(|&i| hay[i..i + needle.len()] == needle[..])
}

/// What one gatekeeper tick decided. `new_chars` is the size of this tick's transcript delta
/// (chars) — the boundary gate consumes it as its "was anything said" signal, so the loop computes
/// the delta exactly once per tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoveltyTick {
    /// Spawn the reactions/bullets worker THIS tick.
    pub fire: bool,
    /// Chars of genuinely-new transcript text this tick.
    pub new_chars: usize,
}

/// The per-recording NOVELTY GATEKEEPER state. Created fresh by the live loop at every recording
/// start (nothing leaks across recordings). Advanced once per tick via [`NoveltyState::on_tick`].
#[derive(Default)]
pub struct NoveltyState {
    tracker: TickDelta,
    /// Ticks since the last FIRE (saturating). Starts at 0, so the earliest possible first fire is
    /// tick [`MIN_FIRE_INTERVAL_TICKS`] — the floor binds from recording start too.
    ticks_since_fire: u32,
    /// Consecutive ticks with ZERO new chars (the lull clock).
    quiet_ticks: u32,
    /// New chars accrued since the last fire.
    pending_chars: usize,
    /// A `?` appeared in the accrued delta since the last fire.
    pending_question: bool,
    /// A visible-entity name appeared in the accrued delta since the last fire.
    pending_entity_hit: bool,
}

impl NoveltyState {
    /// One live tick: take the buffer delta, fold it into the pending triggers, and decide whether
    /// the worker fires now. Fires when ANY of (≥[`NOVELTY_NEW_CHARS`] pending chars / a `?` in the
    /// pending delta / a visible-entity name in the pending delta / a [`NOVELTY_LULL_TICKS`] lull
    /// with pending text) holds — but NEVER within [`MIN_FIRE_INTERVAL_TICKS`] of the previous
    /// fire. On a fire the pending triggers reset; unspent triggers otherwise accrue.
    pub fn on_tick(&mut self, live_buf: &str, entities: &[(String, String)]) -> NoveltyTick {
        self.ticks_since_fire = self.ticks_since_fire.saturating_add(1);
        let delta = self.tracker.take_delta(live_buf);
        let new_chars = if delta.trim().is_empty() {
            0
        } else {
            delta.chars().count()
        };
        if new_chars == 0 {
            self.quiet_ticks = self.quiet_ticks.saturating_add(1);
        } else {
            self.quiet_ticks = 0;
            self.pending_chars = self.pending_chars.saturating_add(new_chars);
            if delta.contains('?') {
                self.pending_question = true;
            }
            if entity_hit(&delta, entities) {
                self.pending_entity_hit = true;
            }
        }
        let has_pending = self.pending_chars > 0;
        let trigger = self.pending_chars >= NOVELTY_NEW_CHARS
            || self.pending_question
            || self.pending_entity_hit
            || (has_pending && self.quiet_ticks >= NOVELTY_LULL_TICKS);
        let fire = trigger && self.ticks_since_fire >= MIN_FIRE_INTERVAL_TICKS;
        if fire {
            self.ticks_since_fire = 0;
            self.pending_chars = 0;
            self.pending_question = false;
            self.pending_entity_hit = false;
        }
        NoveltyTick { fire, new_chars }
    }
}

/// Case-insensitive substring match of any visible entity NAME in the delta (the same coarse
/// containment `brain_reactions::filter_window_entities` uses — a trigger, not a gate; names ≤2
/// chars are skipped as noise).
fn entity_hit(delta: &str, entities: &[(String, String)]) -> bool {
    let dl = delta.to_lowercase();
    entities.iter().any(|(_, name)| {
        let n = name.trim();
        n.chars().count() >= 3 && dl.contains(&n.to_lowercase())
    })
}

/// Tick-driven cache of the VISIBLE entity `(id, name)` list. The live loop calls
/// [`Self::on_tick`] once per tick with a `fetch` closure (the gated `list_entities_visible` read
/// over the FRESH unlocked set); the closure runs only on the first tick and then every
/// [`ENTITY_CACHE_REFRESH_TICKS`] ticks. Pure over the injected closure — headless-testable.
#[derive(Default)]
pub struct RefreshableEntityCache {
    entities: Vec<(String, String)>,
    /// Ticks until the next refresh; 0 = refresh NOW (the default, so the first tick fetches).
    ticks_until_refresh: u32,
}

impl RefreshableEntityCache {
    /// Advance one tick, refreshing via `fetch` when due, and return the cached list.
    pub fn on_tick(
        &mut self,
        fetch: impl FnOnce() -> Vec<(String, String)>,
    ) -> &[(String, String)] {
        if self.ticks_until_refresh == 0 {
            self.entities = fetch();
            self.ticks_until_refresh = ENTITY_CACHE_REFRESH_TICKS;
        }
        self.ticks_until_refresh -= 1;
        &self.entities
    }
}

/// Sentence-final punctuation the boundary gate treats as "the speaker finished a thought".
fn ends_sentence(caption: &str) -> bool {
    matches!(
        caption.trim_end().chars().last(),
        Some('.') | Some('!') | Some('?') | Some('…')
    )
}

/// The BOUNDARY-TIMED SURFACING decision (pure). The live loop queues whisper cards / proactive
/// hints and asks this gate once per tick whether NOW is a good moment to emit them: a
/// conversational lull ([`BOUNDARY_LULL_TICKS`] ticks with no new transcript chars), the latest
/// caption ending on sentence-final punctuation, or the [`MAX_HOLD_TICKS`] force-emit. Emitting
/// mid-sentence interrupts the user's reading of the live caption — holding a stale card 30 s+
/// makes it useless; this is the compromise the spec fixes. Drain-on-stop is the CALLER's job
/// (the loop emits everything left when the recording ends).
#[derive(Default)]
pub struct BoundaryGate {
    /// Consecutive ticks with zero new transcript chars.
    quiet_ticks: u32,
    /// Ticks the current pending batch has been held (0 while nothing is pending).
    hold_ticks: u32,
}

impl BoundaryGate {
    /// One live tick: `has_pending` = anything queued, `had_new_chars` = this tick's transcript
    /// delta was non-empty, `last_caption` = the most recent caption text (for the sentence-final
    /// check). Returns `true` when the caller should emit the whole queue now.
    pub fn on_tick(&mut self, has_pending: bool, had_new_chars: bool, last_caption: &str) -> bool {
        if had_new_chars {
            self.quiet_ticks = 0;
        } else {
            self.quiet_ticks = self.quiet_ticks.saturating_add(1);
        }
        if !has_pending {
            self.hold_ticks = 0;
            return false;
        }
        self.hold_ticks = self.hold_ticks.saturating_add(1);
        let emit = self.quiet_ticks >= BOUNDARY_LULL_TICKS
            || ends_sentence(last_caption)
            || self.hold_ticks >= MAX_HOLD_TICKS;
        if emit {
            self.hold_ticks = 0;
        }
        emit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ents(names: &[&str]) -> Vec<(String, String)> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| (format!("e{i}"), n.to_string()))
            .collect()
    }

    /// Feed `state` `n` silent ticks (unchanged buffer) asserting none fires.
    fn silent_ticks(state: &mut NoveltyState, buf: &str, n: u32) {
        for _ in 0..n {
            assert!(!state.on_tick(buf, &[]).fire, "a silent tick must not fire");
        }
    }

    // ── Novelty gatekeeper ──────────────────────────────────────────────────────────────────────

    #[test]
    fn fires_on_enough_new_chars_and_resets_pending() {
        let mut st = NoveltyState::default();
        let mut buf = String::new();
        // Small trickle: below the char threshold, no question, no entity → never fires.
        buf.push_str("krótka wypowiedź");
        assert!(!st.on_tick(&buf, &[]).fire);
        silent_ticks(&mut st, &buf, 6);
        // A burst of ≥120 new chars → fires (the floor of 5 ticks has long passed).
        buf.push_str(&" omawiamy szczegóły harmonogramu wdrożenia oraz podział obowiązków w zespole na najbliższe dwa tygodnie sprintu produktowego".repeat(2));
        let tick = st.on_tick(&buf, &[]);
        assert!(tick.fire, "≥120 accrued new chars must fire");
        assert!(tick.new_chars >= NOVELTY_NEW_CHARS);
        // Pending reset: the very next silent tick does not re-fire.
        assert!(!st.on_tick(&buf, &[]).fire);
    }

    #[test]
    fn fires_on_question_mark_in_delta() {
        let mut st = NoveltyState::default();
        let mut buf = String::new();
        silent_ticks(&mut st, &buf, 5); // clear the min-interval floor with silence
        buf.push_str("a co z budżetem?");
        assert!(
            st.on_tick(&buf, &[]).fire,
            "a '?' in the delta must fire even under the char threshold"
        );
    }

    #[test]
    fn fires_on_visible_entity_name_hit() {
        let mut st = NoveltyState::default();
        let entities = ents(&["Atlas", "Anna Kowalska"]);
        let mut buf = String::new();
        silent_ticks(&mut st, &buf, 5);
        buf.push_str("wracamy do tematu atlas");
        assert!(
            st.on_tick(&buf, &entities).fire,
            "a visible-entity name in the delta must fire"
        );
        // A too-short name (≤2 chars) never triggers.
        let mut st2 = NoveltyState::default();
        let noisy = ents(&["A"]);
        let mut buf2 = String::new();
        silent_ticks(&mut st2, &buf2, 5);
        buf2.push_str("a tam cokolwiek");
        assert!(
            !st2.on_tick(&buf2, &noisy).fire,
            "1-char names are noise, not a trigger"
        );
    }

    #[test]
    fn lull_fires_pending_text_after_fourteen_quiet_ticks() {
        let mut st = NoveltyState::default();
        let mut buf = String::new();
        silent_ticks(&mut st, &buf, 5);
        buf.push_str("krótka uwaga bez pytania"); // under every instant trigger
        assert!(!st.on_tick(&buf, &[]).fire);
        // 13 quiet ticks: still holding.
        silent_ticks(&mut st, &buf, NOVELTY_LULL_TICKS - 1);
        // The 14th quiet tick = the 42 s lull → fire on the pending text.
        assert!(
            st.on_tick(&buf, &[]).fire,
            "the lull must flush pending text"
        );
        // With NOTHING pending, a lull alone never fires.
        let mut idle = NoveltyState::default();
        for _ in 0..40 {
            assert!(
                !idle.on_tick("", &[]).fire,
                "an empty recording must never fire"
            );
        }
    }

    #[test]
    fn min_interval_floor_binds_every_trigger() {
        let mut st = NoveltyState::default();
        // A question on tick 1: trigger present, but the 5-tick floor holds it.
        let buf1 = "czy zdążymy?".to_string();
        assert!(!st.on_tick(&buf1, &[]).fire, "tick 1 is inside the floor");
        // Ticks 2–4 (unchanged buffer): trigger stays pending, floor still holds.
        for _ in 0..3 {
            assert!(!st.on_tick(&buf1, &[]).fire);
        }
        // Tick 5: floor cleared → the pending question fires.
        assert!(
            st.on_tick(&buf1, &[]).fire,
            "the pending trigger fires at the floor"
        );
        // Immediately after a fire, a fresh question waits the full floor again.
        let buf2 = format!("{buf1} no dobrze, a termin?");
        assert!(
            !st.on_tick(&buf2, &[]).fire,
            "tick 1 after a fire is inside the floor"
        );
        for _ in 0..3 {
            assert!(!st.on_tick(&buf2, &[]).fire);
        }
        assert!(st.on_tick(&buf2, &[]).fire);
    }

    /// The live buffer front-trims at its 16k cap: the tracker must re-locate via its anchor and
    /// count ONLY the genuinely-new tail — never re-count already-seen text as new.
    #[test]
    fn tick_delta_survives_front_trim_via_anchor() {
        let mut t = TickDelta::default();
        let long: String = (0..40).map(|i| format!("word{i} ")).collect();
        let _ = t.take_delta(&long);
        let trimmed = format!("{} FRESH tail here", &long[60..].trim_end());
        let delta = t.take_delta(&trimmed);
        assert_eq!(
            delta.trim_start(),
            "FRESH tail here",
            "only the new tail, got {delta:?}"
        );
        assert!(
            !delta.contains("word"),
            "no already-seen text re-surfaced: {delta:?}"
        );
        // Anchor fully gone (buffer replaced) → bounded recent-tail rescan, never a lying slice.
        let mut t2 = TickDelta::default();
        let _ = t2.take_delta("completely original first buffer content");
        assert_eq!(t2.take_delta("entirely different"), "entirely different");
    }

    // ── Entity cache ────────────────────────────────────────────────────────────────────────────

    #[test]
    fn entity_cache_fetches_on_first_tick_then_every_sixty() {
        let mut cache = RefreshableEntityCache::default();
        let mut fetches = 0u32;
        for tick in 1..=(ENTITY_CACHE_REFRESH_TICKS * 2 + 1) {
            let got = cache
                .on_tick(|| {
                    fetches += 1;
                    ents(&["Atlas"])
                })
                .len();
            assert_eq!(got, 1, "cache always serves the list (tick {tick})");
        }
        // Tick 1 fetches, tick 61 fetches, tick 121 fetches: exactly 3 across 121 ticks.
        assert_eq!(
            fetches, 3,
            "refresh exactly every {ENTITY_CACHE_REFRESH_TICKS} ticks"
        );
    }

    // ── Boundary gate ───────────────────────────────────────────────────────────────────────────

    #[test]
    fn holds_mid_sentence_while_speech_continues() {
        let mut g = BoundaryGate::default();
        for _ in 0..(MAX_HOLD_TICKS - 1) {
            assert!(
                !g.on_tick(true, true, "and then we decided to move the"),
                "continuous speech mid-sentence must hold the queue"
            );
        }
    }

    #[test]
    fn emits_on_lull() {
        let mut g = BoundaryGate::default();
        assert!(!g.on_tick(true, true, "mid sentence still going and"));
        // 3 quiet ticks (~9 s of silence) → boundary.
        assert!(!g.on_tick(true, false, "mid sentence still going and"));
        assert!(!g.on_tick(true, false, "mid sentence still going and"));
        assert!(
            g.on_tick(true, false, "mid sentence still going and"),
            "the {BOUNDARY_LULL_TICKS}-tick lull is a boundary"
        );
    }

    #[test]
    fn emits_on_sentence_final_punctuation() {
        let mut g = BoundaryGate::default();
        assert!(
            g.on_tick(true, true, "so that is what we will do."),
            "a sentence-final '.' is a boundary even while speech continues"
        );
        let mut g2 = BoundaryGate::default();
        assert!(
            g2.on_tick(true, true, "czy na pewno zdążymy?"),
            "'?' ends a sentence"
        );
        let mut g3 = BoundaryGate::default();
        assert!(g3.on_tick(true, true, "no i tyle…"), "'…' ends a sentence");
    }

    #[test]
    fn max_hold_force_emits() {
        let mut g = BoundaryGate::default();
        let mut emitted_at = None;
        for tick in 1..=MAX_HOLD_TICKS {
            if g.on_tick(
                true,
                true,
                "endless run-on sentence that never terminates and",
            ) {
                emitted_at = Some(tick);
                break;
            }
        }
        assert_eq!(
            emitted_at,
            Some(MAX_HOLD_TICKS),
            "a queue held {MAX_HOLD_TICKS} ticks must force-emit"
        );
    }

    #[test]
    fn empty_queue_never_emits_and_resets_hold() {
        let mut g = BoundaryGate::default();
        for _ in 0..20 {
            assert!(
                !g.on_tick(false, false, "done."),
                "no pending ⇒ nothing to emit"
            );
        }
        // A fresh pending batch after a long idle emits on merit, not on stale hold ticks.
        assert!(
            g.on_tick(true, false, "sentence over."),
            "sentence boundary applies"
        );
    }
}
