//! Brain v2 L4 — INCREMENTAL RUNNING BULLETS ("live notes"). While a recording is in progress the
//! reactions worker (gated by the novelty gatekeeper + `reactions_busy`, see `transcribe::live`)
//! feeds the NEW transcript delta plus the bullets-so-far to the LOCAL light reasoner and appends
//! at most 3 new `- [topic]: point` lines. The bullets become:
//! - the SUBSTRATE for reactions ([`crate::brain_reactions::reaction_window`]: bullets + a short
//!   verbatim tail instead of the raw 600-char tail),
//! - the tighter live-question inject (`transcribe::live::compose_live_inject`: 2k bullets + 2k
//!   verbatim tail instead of the 6k raw tail), and
//! - a Stage-1 note input at Stop (`SummarizeRequest::live_bullets`, rendered as the
//!   "LIVE NOTES (auto)" section by `summarize::template::render_user_content`).
//!
//! ## Privacy / lock model
//! The model call is the LOCAL light engine ONLY (`ReasonerCell::light` — local-or-stub, NEVER a
//! cloud fallback): nothing egresses from this module. State lives in RAM
//! (`AppState::live_bullets`, capped at [`MAX_BULLETS_CHARS`], cleared at recording start + Stop +
//! the lock-surface idle hygiene) plus the additive `live_bullets` DB row (crash recovery for the
//! Stop-time note input). The ROW is derived meeting content and follows the L2 lesson: PURGED on
//! EVERY seal path (`lock_folder`/move-into-locked via `purge_chunks_for_meetings`, relock via
//! `blank_sealed_notes_in_folders`, startup reblank via `reblank_locked_folders_at_rest`, the
//! unrecoverable-key escape hatch via `discard_folder_seal`) and on `delete_meeting`; the row
//! WRITE additionally refuses in-tx when the meeting is sealed at rest
//! (`Db::upsert_live_bullets` — the mid-recording-lock TOCTOU); the only production read is the
//! note pipeline (the plaintext PRODUCER — same
//! ungated-by-design classification as `manual_notes` there) and the gated prompt inject
//! (`gated_live_bullets`, fail-closed on `meeting_is_visible`). At Stop the row rides
//! `SummarizeRequest.live_bullets` through the `RedactingProvider` firewall like every other
//! prompt field. No PII in logs (counts + error strings only).
//!
//! Bullet QUALITY (does the light model produce useful lines? at what latency?) is real-Mac-only —
//! `cargo test` proves the plumbing, never the model.

use crate::reason::{GenOptions, LocalReasoner};

/// Hard cap on the accumulated bullets (chars) — bounds RAM + the prompt injects over a multi-hour
/// meeting; the OLDEST lines are trimmed from the front (the recent running notes matter most).
pub const MAX_BULLETS_CHARS: usize = 4_000;

/// Minimum transcript-delta size (chars) worth a model call — a shorter fragment (a word or two,
/// e.g. a lull flush of a tiny remark) yields `None` without spinning the engine.
pub const MIN_DELTA_CHARS: usize = 40;

/// Max NEW bullet lines accepted from one update (the prompt asks for ≤3; this enforces it).
pub const MAX_NEW_BULLETS: usize = 3;

/// Token cap for one bullets update (≤3 short lines — spec §L4).
const BULLETS_MAX_TOKENS: usize = 200;

/// Low-variance sampling for the note-taking task (spec §L4).
const BULLETS_TEMPERATURE: f64 = 0.2;

/// Wall-clock bound on one background bullets generation (the live-path discipline — a hung
/// update must degrade one tick, never wedge the worker).
///
/// CAVEAT (pre-existing class, shared with the reactions scan's own `GenOptions::light()`
/// timeout): `GenOptions.timeout` is honored ONLY by the Mistral/GGUF backend's `reason_with`
/// override — the `LocalReasoner` trait DEFAULT ignores the options and delegates to `reason`
/// (`reason.rs`, `fn reason_with` default body). On a non-GGUF light backend a hung generation
/// is therefore unbounded and wedges `reactions_busy` (no bullets AND no reaction scans) for the
/// rest of the recording. Not fixed here — it is the same exposure the reactions scan already
/// carries; a per-backend timeout belongs in `reason.rs`, not per call site.
const BULLETS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Max chars of transcript delta folded into one bullets update (mirrors the gatekeeper's per-tick
/// budget — the inner `novelty::TickDelta` already caps at the same figure, this is
/// belt-and-braces for the prompt).
const MAX_BULLETS_DELTA_CHARS: usize = 2_000;

/// The bullets' OWN delta tracker over the live buffer (`AppState::live_bullets_tracker`) —
/// separate from the gatekeeper's, because the WORKER consumes transcript at its own (busy-gated)
/// pace: a skipped scan's text must reach the NEXT bullets update, not vanish. Same anchor
/// discipline as `novelty::TickDelta` / `proactive::DeltaTracker`: survives the live buffer's
/// front-trim at its 16k cap; degrades to a bounded recent-tail rescan when the anchor is gone.
#[derive(Default)]
pub struct BulletsTracker {
    inner: crate::transcribe::novelty::TickDelta,
}

impl BulletsTracker {
    /// The transcript text NEW since the last bullets update (most recent
    /// ≤[`MAX_BULLETS_DELTA_CHARS`] of it), advancing the tracker.
    pub fn take_delta(&mut self, live_buf: &str) -> String {
        let delta = self.inner.take_delta(live_buf);
        // The inner tracker caps at its own budget (2k) — enforce ours on a char boundary anyway.
        let count = delta.chars().count();
        if count <= MAX_BULLETS_DELTA_CHARS {
            delta
        } else {
            delta.chars().skip(count - MAX_BULLETS_DELTA_CHARS).collect()
        }
    }
}

/// ONE incremental bullets update (pure over the injected reasoner): previous bullets + the new
/// transcript `delta` → `Some(new bullet lines)` to APPEND, or `None` when there is nothing to add
/// — on the stub reasoner (no local model ⇒ feature degraded, NEVER a cloud call), on a too-short
/// delta, on a reasoner error/timeout, or when the model answered the literal
/// [`crate::prompts::LIVE_BULLETS_NOTHING`]. The returned block is at most [`MAX_NEW_BULLETS`]
/// `- `-prefixed lines, model prose stripped.
pub fn update_bullets(
    reasoner: &dyn LocalReasoner,
    previous_bullets: &str,
    delta: &str,
) -> Option<String> {
    if reasoner.id() == "stub" {
        return None; // canonical stub guard — a missing model degrades, never egresses.
    }
    if delta.trim().chars().count() < MIN_DELTA_CHARS {
        return None;
    }
    let user = crate::prompts::live_bullets_user(previous_bullets, delta);
    let opts = GenOptions {
        max_tokens: Some(BULLETS_MAX_TOKENS),
        temperature: Some(BULLETS_TEMPERATURE),
        enable_thinking: false,
        timeout: Some(BULLETS_TIMEOUT),
        ..GenOptions::default()
    };
    let out = match reasoner.reason_with(crate::prompts::LIVE_BULLETS_SYSTEM, &user, opts) {
        Ok(o) => o,
        Err(e) => {
            // Best-effort background task: log (no PII — no transcript/bullet text) and skip.
            tracing::debug!(target: "bullets", error = %e, "bullets update failed; skipping tick");
            return None;
        }
    };
    parse_new_bullets(&out)
}

/// Extract the accepted bullet lines from one model reply: `- `-prefixed lines only (prose /
/// numbering / fences dropped), capped at [`MAX_NEW_BULLETS`]; the literal
/// [`crate::prompts::LIVE_BULLETS_NOTHING`] (any case) or an empty / bullet-less reply ⇒ `None`.
fn parse_new_bullets(out: &str) -> Option<String> {
    let t = out.trim();
    if t.is_empty() || t.eq_ignore_ascii_case(crate::prompts::LIVE_BULLETS_NOTHING) {
        return None;
    }
    let lines: Vec<&str> = t
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("- ") && l.len() > 2)
        .take(MAX_NEW_BULLETS)
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(lines.join("\n"))
}

/// Append `new_lines` to the accumulated `existing` bullets, enforcing [`MAX_BULLETS_CHARS`] by
/// dropping the OLDEST lines from the FRONT (whole lines — never a mid-line cut). Pure.
pub fn append_bullets(existing: &str, new_lines: &str) -> String {
    let mut merged = if existing.trim().is_empty() {
        new_lines.trim().to_string()
    } else {
        format!("{}\n{}", existing.trim_end(), new_lines.trim())
    };
    while merged.chars().count() > MAX_BULLETS_CHARS {
        match merged.find('\n') {
            Some(nl) => merged = merged[nl + 1..].to_string(),
            None => {
                // A single line over the cap (degenerate) — keep the most recent tail.
                let count = merged.chars().count();
                merged = merged.chars().skip(count - MAX_BULLETS_CHARS).collect();
                break;
            }
        }
    }
    merged
}

/// Clear the RAM bullets buffer + reset the delta tracker (recording start / Stop / the
/// lock-surface idle hygiene). Best-effort: a poisoned lock is ignored — the buffer is re-cleared
/// at the next recording start anyway. Logs a COUNT only (no bullet text — PII rule).
pub fn clear_ram(
    bullets: &std::sync::Mutex<String>,
    tracker: &std::sync::Mutex<BulletsTracker>,
) {
    if let Ok(mut b) = bullets.lock() {
        if !b.is_empty() {
            tracing::debug!(target: "bullets", chars = b.chars().count(), "cleared live-bullets buffer");
            b.clear();
        }
    }
    if let Ok(mut t) = tracker.lock() {
        *t = BulletsTracker::default();
    }
}

/// ONE bullets tick, wired to the running app — runs on the reactions WORKER thread (behind
/// `reactions_busy`, BEFORE `detect_reactions`), never the live tick thread. Flag- and
/// stub-gated: `live_bullets_enabled` OFF or no local light model ⇒ a no-op (legacy behavior).
/// The body lives in [`bullets_tick_with`] — the reasoner-injected seam headless tests drive.
pub fn bullets_tick(app: &tauri::AppHandle) {
    use tauri::Manager;
    let st = app.state::<crate::state::AppState>();
    let enabled = st
        .config
        .lock()
        .map(|c| c.live_bullets_enabled)
        .unwrap_or(false);
    if !enabled {
        return;
    }
    let reasoner = st.reasoner.light();
    if reasoner.id() == "stub" {
        return; // no light model ⇒ feature degraded (never a cloud call).
    }
    bullets_tick_with(&st, &*reasoner);
}

/// The tick body with the light reasoner injected (production enters via [`bullets_tick`]; tests
/// drive it with a fake reasoner + a headless `AppState`). Reads the live-buffer delta through
/// `AppState::live_bullets_tracker`, runs ONE [`update_bullets`] model call, and COMMITS the
/// result behind a FRESH `current_meeting` re-check taken AFTER the (≤30s) call — the SAME check
/// gates BOTH the RAM write and the crash-recovery row write ([`commit_bullets_decision`]): a
/// worker in flight across Stop(A)→Start(B) DISCARDS its result entirely (no RAM write, no row
/// write), so meeting A's bullets can never contaminate meeting B's substrate / prompt inject /
/// row / note. The RAM write happens while the `current_meeting` guard is still HELD (atomic with
/// the check w.r.t. Stop/Start, which mutate `current_meeting` under the same mutex — only the
/// String swap runs under the guard, never the DB write); the row write follows after the guard
/// drops, its residual stale window backstopped by the sealed-at-rest refuse inside
/// `Db::upsert_live_bullets` + the Stop-consume / purge-on-seal / at-rest-reconcile paths.
///
/// DISCARD POLICY on a stale worker (same as the NOTHING/error policy below): the tracker delta
/// stays CONSUMED — the meeting switch already reset the tracker via the new recording's
/// [`clear_ram`], so there is no stale text to re-feed and nothing to roll back. Best-effort +
/// panic-free throughout (the worker's `BusyReset` guard is the outer safety net).
pub(crate) fn bullets_tick_with(st: &crate::state::AppState, reasoner: &dyn LocalReasoner) {
    // The meeting this tick belongs to — captured NOW, re-checked FRESH after the model call.
    let Some(meeting_id) = st
        .current_meeting
        .lock()
        .ok()
        .and_then(|m| m.map(|id| id.to_string()))
    else {
        return; // not recording — nothing to note.
    };
    let buf = st
        .live_transcript
        .lock()
        .map(|b| b.clone())
        .unwrap_or_default();
    let delta = match st.live_bullets_tracker.lock() {
        Ok(mut t) => t.take_delta(&buf),
        Err(_) => return,
    };
    let prev = st
        .live_bullets
        .lock()
        .map(|b| b.clone())
        .unwrap_or_default();
    // NOTE: the tracker delta is CONSUMED even when the model answers NOTHING / errors — by
    // design (the model judged the fragment noteless) / accepted degradation (one lost fragment
    // beats re-feeding stale text forever).
    let Some(new_lines) = update_bullets(reasoner, &prev, &delta) else {
        return;
    };
    let merged = append_bullets(&prev, &new_lines);
    // FRESH `current_meeting` snapshot AFTER the (≤30s) model call — the ONE check gating BOTH
    // writes (adversarial CRITICAL, 2026-07-10: the RAM write used to run unconditionally, so a
    // worker spanning Stop(A)→Start(B) re-populated B's RAM with A's bullets AFTER every
    // clear_ram). The RAM write stays INSIDE the guard so Stop/Start (which mutate
    // `current_meeting` under this same mutex) cannot interleave between check and write; only
    // the in-RAM String swap runs under the guard — the DB write below never does.
    let committed = {
        let Ok(current) = st.current_meeting.lock() else {
            return; // poisoned ⇒ discard (best-effort tick; next tick re-derives).
        };
        let current_id = current.map(|id| id.to_string());
        let Some(m) = commit_bullets_decision(&meeting_id, current_id.as_deref(), merged) else {
            return; // stale worker (stopped or switched meeting) — DISCARD entirely.
        };
        if let Ok(mut b) = st.live_bullets.lock() {
            *b = m.clone();
        }
        m
    };
    // Crash-recovery row — behind the SAME fresh check as the RAM write above. The guard is
    // dropped before this DB write; the microscopic residual (a Stop landing right here) can only
    // re-write a row for a meeting that WAS recording a moment ago, and is backstopped by the
    // sealed-at-rest refuse inside `upsert_live_bullets`, the Stop-time consume, and the
    // purge-on-seal / at-rest-reconcile paths.
    let now = chrono::Utc::now().to_rfc3339();
    if let Err(e) = st.db.upsert_live_bullets(&meeting_id, &committed, &now) {
        tracing::debug!(target: "bullets", error = %e, "live-bullets row upsert failed; RAM copy unaffected");
    }
}

/// PURE post-model commit decision (the headless-testable seam of the stale-worker guard): the
/// worker's `merged` bullets are committed only while the CURRENT recording is still the worker's
/// own meeting; `None` = DISCARD the result entirely (no RAM write, no row write).
pub(crate) fn commit_bullets_decision(
    worker_meeting: &str,
    current_meeting: Option<&str>,
    merged: String,
) -> Option<String> {
    (current_meeting == Some(worker_meeting)).then_some(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;
    use serde_json::Value;

    /// A test reasoner returning a fixed reply (id deliberately NOT "stub").
    struct FixedReasoner(&'static str);
    impl LocalReasoner for FixedReasoner {
        fn id(&self) -> &str {
            "fixed-test"
        }
        fn reason(&self, _system: &str, _user: &str) -> Result<String> {
            Ok(self.0.to_string())
        }
        fn structured(&self, _system: &str, _user: &str, _schema: &Value) -> Result<Value> {
            Ok(Value::Null)
        }
    }

    const LONG_DELTA: &str = "omówiliśmy budżet projektu Atlas i ustaliliśmy, że Anna przejmuje \
                              testy QA przed piątkowym wydaniem";

    #[test]
    fn stub_reasoner_yields_none() {
        // The canonical stub guard: no local model ⇒ no bullets, never a cloud call.
        let out = update_bullets(&crate::reason::StubReasoner, "", LONG_DELTA);
        assert!(out.is_none(), "stub must yield None");
    }

    #[test]
    fn nothing_reply_yields_none() {
        let r = FixedReasoner("NOTHING");
        assert!(update_bullets(&r, "- [x]: y", LONG_DELTA).is_none());
        // Case-insensitive + whitespace-tolerant.
        let r2 = FixedReasoner("  nothing \n");
        assert!(update_bullets(&r2, "", LONG_DELTA).is_none());
    }

    #[test]
    fn short_delta_yields_none_without_model_call() {
        // A panicking reasoner proves the short-circuit happens BEFORE any model call.
        struct PanicReasoner;
        impl LocalReasoner for PanicReasoner {
            fn id(&self) -> &str {
                "panic-test"
            }
            fn reason(&self, _s: &str, _u: &str) -> Result<String> {
                panic!("must not be called for a short delta")
            }
            fn structured(&self, _s: &str, _u: &str, _j: &Value) -> Result<Value> {
                panic!("never")
            }
        }
        assert!(update_bullets(&PanicReasoner, "", "za krótko").is_none());
    }

    #[test]
    fn bullets_are_parsed_capped_and_appended() {
        // Model returns prose + 4 bullets → only the first 3 `- ` lines are accepted.
        let r = FixedReasoner(
            "Sure, here are the notes:\n- [budżet]: 10k zatwierdzone\n- [QA]: Anna przejmuje testy\n- [termin]: wydanie w piątek\n- [extra]: czwarta linia\nhope this helps",
        );
        let new = update_bullets(&r, "- [start]: kickoff", LONG_DELTA).expect("bullets parsed");
        assert_eq!(new.lines().count(), MAX_NEW_BULLETS, "capped at {MAX_NEW_BULLETS}");
        assert!(new.starts_with("- [budżet]:"));
        assert!(!new.contains("czwarta linia"), "the 4th bullet is dropped");
        assert!(!new.contains("Sure"), "prose stripped");
        // Append keeps the existing bullets in front.
        let merged = append_bullets("- [start]: kickoff", &new);
        assert!(merged.starts_with("- [start]: kickoff\n- [budżet]:"));
    }

    #[test]
    fn reply_with_no_bullet_lines_yields_none() {
        let r = FixedReasoner("I could not find anything noteworthy in this fragment.");
        assert!(update_bullets(&r, "", LONG_DELTA).is_none());
    }

    #[test]
    fn append_enforces_char_cap_by_dropping_oldest_lines() {
        // Build bullets just under the cap, then append — the OLDEST lines must fall off the
        // front, the newest must survive, and the cap must hold.
        let line = format!("- [t]: {}", "x".repeat(93)); // 100 chars per line
        let existing = std::iter::repeat_with(|| line.clone())
            .take(40) // 40 × ~101 ≈ 4040 chars > cap after append
            .collect::<Vec<_>>()
            .join("\n");
        let merged = append_bullets(&existing, "- [new]: the freshest bullet");
        assert!(
            merged.chars().count() <= MAX_BULLETS_CHARS,
            "cap enforced, got {}",
            merged.chars().count()
        );
        assert!(merged.ends_with("- [new]: the freshest bullet"), "newest survives");
        assert!(merged.starts_with("- "), "front-trim lands on a line boundary");
    }

    // ── bullets_tick_with: the stale-worker guard (adversarial CRITICAL, 2026-07-10) ────────────

    /// Fixed at-rest DB key for the headless `AppState` (same shape as commands.rs lifecycle
    /// tests — never the Keychain).
    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn test_state(tag: &str) -> std::sync::Arc<crate::state::AppState> {
        let p = crate::storage::db::unique_temp_path(&format!("murmur-bullets-{tag}"), "sqlite");
        let _ = std::fs::remove_file(&p);
        std::sync::Arc::new(crate::state::AppState::init_at(&p, TEST_DEK).unwrap())
    }

    fn insert_meeting(st: &crate::state::AppState, id: &uuid::Uuid) {
        st.db
            .insert_meeting(&crate::storage::models::Meeting {
                id: id.to_string(),
                started_at: "2026-07-10T10:00:00Z".to_string(),
                ended_at: None,
                title: None,
                duration_s: 0,
                audio_path: None,
                status: crate::storage::models::MeetingStatus::Recording,
                folder_id: None,
            })
            .unwrap();
    }

    /// A reasoner that simulates Stop(A) → Start(B) landing INSIDE the (≤30s) model call: both
    /// clear the RAM bullets (`clear_ram`), then the new recording takes `current_meeting` — the
    /// exact interleaving of `stop_recording` + `start_recording` around a worker in flight.
    struct StopStartReasoner {
        st: std::sync::Arc<crate::state::AppState>,
        next: uuid::Uuid,
    }
    impl LocalReasoner for StopStartReasoner {
        fn id(&self) -> &str {
            "switch-test"
        }
        fn reason(&self, _s: &str, _u: &str) -> Result<String> {
            clear_ram(&self.st.live_bullets, &self.st.live_bullets_tracker);
            if let Ok(mut cur) = self.st.current_meeting.lock() {
                *cur = Some(self.next);
            }
            Ok("- [stale]: meeting A leftovers".to_string())
        }
        fn structured(&self, _s: &str, _u: &str, _j: &Value) -> Result<Value> {
            Ok(Value::Null)
        }
    }

    /// THE CRITICAL (RED before the ordering fix): a worker whose model call spans
    /// Stop(A)→Start(B) must DISCARD its result — pre-fix the RAM write ran unconditionally
    /// BEFORE the `current_meeting` re-check, so meeting A's bullets landed in meeting B's RAM
    /// (→ B's reaction substrate, prompt inject, row, and Stop-time note).
    #[test]
    fn stale_worker_discards_ram_and_row_when_meeting_switches_mid_call() {
        let st = test_state("stale-switch");
        let a = uuid::Uuid::new_v4();
        let b = uuid::Uuid::new_v4();
        insert_meeting(&st, &a);
        insert_meeting(&st, &b);
        *st.current_meeting.lock().unwrap() = Some(a);
        *st.live_transcript.lock().unwrap() = LONG_DELTA.to_string();

        let reasoner = StopStartReasoner {
            st: std::sync::Arc::clone(&st),
            next: b,
        };
        bullets_tick_with(&st, &reasoner);

        assert_eq!(
            st.live_bullets.lock().unwrap().as_str(),
            "",
            "stale worker's RAM write must be discarded (meeting switched mid-call)"
        );
        assert_eq!(
            st.db.get_live_bullets(&a.to_string()).unwrap(),
            None,
            "no row resurrected for the stopped meeting A"
        );
        assert_eq!(
            st.db.get_live_bullets(&b.to_string()).unwrap(),
            None,
            "meeting B must not inherit A's bullets row"
        );
    }

    /// Happy path: the meeting is UNCHANGED across the model call → the same fresh check commits
    /// BOTH writes (RAM + crash-recovery row).
    #[test]
    fn same_meeting_commits_ram_and_row() {
        let st = test_state("happy-commit");
        let a = uuid::Uuid::new_v4();
        insert_meeting(&st, &a);
        *st.current_meeting.lock().unwrap() = Some(a);
        *st.live_transcript.lock().unwrap() = LONG_DELTA.to_string();

        let r = FixedReasoner("- [budżet]: 10k zatwierdzone");
        bullets_tick_with(&st, &r);

        assert_eq!(
            st.live_bullets.lock().unwrap().as_str(),
            "- [budżet]: 10k zatwierdzone",
            "RAM committed for the still-recording meeting"
        );
        assert_eq!(
            st.db.get_live_bullets(&a.to_string()).unwrap().as_deref(),
            Some("- [budżet]: 10k zatwierdzone"),
            "crash-recovery row committed for the still-recording meeting"
        );
    }

    #[test]
    fn commit_decision_discards_on_meeting_switch_or_stop() {
        assert_eq!(
            commit_bullets_decision("A", Some("A"), "x".to_string()),
            Some("x".to_string()),
            "same meeting ⇒ commit"
        );
        assert_eq!(
            commit_bullets_decision("A", Some("B"), "x".to_string()),
            None,
            "switched meeting ⇒ discard"
        );
        assert_eq!(
            commit_bullets_decision("A", None, "x".to_string()),
            None,
            "stopped (no recording) ⇒ discard"
        );
    }

    #[test]
    fn tracker_survives_front_trim_and_caps_delta() {
        let mut t = BulletsTracker::default();
        let long: String = (0..40).map(|i| format!("word{i} ")).collect();
        let _ = t.take_delta(&long);
        // Front-trim (as the live buffer's 16k cap does) + genuinely-new tail.
        let trimmed = format!("{} FRESH tail here", &long[60..].trim_end());
        let delta = t.take_delta(&trimmed);
        assert_eq!(delta.trim_start(), "FRESH tail here", "anchor relocation, got {delta:?}");
        assert!(!delta.contains("word"), "no re-fed old text: {delta:?}");
    }
}
