//! Brain v2 L5 — SCHEDULED BRIEFS: a 60s background runner that turns the user's structured
//! schedules (`brief_schedules` — day/hour/minute columns, NO cron crate) into PROPOSED brief
//! notes (`brief_runs`), which the user accepts (vault export) or dismisses in the FE.
//!
//! ## Lock model (audited by the lock-security review)
//! The corpus is built EXCLUSIVELY through the gated deterministic readers with the EMPTY unlock
//! set — `list_meetings_visible` + `get_note_if_visible` + `list_open_commitments` +
//! `list_user_facts_visible` — the SAME discipline as the memory consolidation job
//! ([`crate::memory`]): a background job must never see session-unlocked plaintext, let alone
//! sealed content. `brief_runs.note_md` therefore cannot contain sealed content BY CONSTRUCTION;
//! `brief_runs.meeting_ids` carries opaque ids only. (A meeting sealed AFTER a brief was proposed
//! is a documented, accepted posture: the brief was synthesized while that content was visible to
//! everyone with the DB open — same class as an already-exported digest.)
//!
//! ## Egress
//! Synthesis rides the CURRENT Notes provider seam ([`crate::summarize::provider_for`]) — the same
//! consent gate + redaction firewall + egress ledger as note generation. A local/`ollama` Notes
//! provider means zero cloud egress; an unconsented cloud provider fails closed
//! (`AppError::Unavailable`) and the schedule simply skips that day.
//!
//! ## Failure posture
//! The 60s loop never exits: every per-schedule failure is `tracing::warn` + continue. A due
//! schedule CLAIMS its day (`last_run_at` = today) BEFORE synthesis, so a failing provider can
//! never turn into a once-a-minute retry/egress storm — at most ONE attempt per schedule per local
//! day. QUIET-IF-EMPTY: an empty corpus writes no row and emits no event.
//! No PII in logs: schedule ids, counts, durations only.

use std::collections::HashSet;

use chrono::{Datelike, Timelike};
use tauri::{Emitter, Manager};

use crate::error::Result;
use crate::storage::models::{BriefRun, BriefSchedule};
use crate::storage::Db;

/// Runner tick interval — checked every minute so an `HH:MM` schedule fires promptly.
pub const BRIEF_TICK_SECS: u64 = 60;

/// Char budget for the synthesis corpus (mirrors `generate_digest`'s cloud budget).
const BRIEF_CORPUS_BUDGET: usize = 80_000;

/// Smaller corpus budget when the Notes provider resolves to a local `ollama` connection
/// (mirrors `generate_digest`).
const BRIEF_CORPUS_BUDGET_OLLAMA: usize = 4_000;

/// Cap on the commitments / facts sections (bounds the prompt tail).
const MAX_SECTION_ITEMS: usize = 30;

/// Is `schedule` due at `now_local`? PURE (the local wall-clock is injected). Fires when:
/// - the schedule is enabled, AND
/// - the weekday matches (`day_of_week` `None` = daily; 0 = Monday … 6 = Sunday), AND
/// - the local time has REACHED `hour_local:minute_local` (>= — so a tick that lands a few
///   seconds/minutes late, or an app launched after the scheduled time, still fires that day), AND
/// - it has not already run today (`last_run_at` != today's local date — max ONE run per day).
pub fn should_fire(schedule: &BriefSchedule, now_local: &chrono::NaiveDateTime) -> bool {
    if !schedule.enabled {
        return false;
    }
    if let Some(dow) = schedule.day_of_week {
        if i64::from(now_local.weekday().num_days_from_monday()) != dow {
            return false;
        }
    }
    let reached = (i64::from(now_local.hour()), i64::from(now_local.minute()))
        >= (schedule.hour_local, schedule.minute_local);
    if !reached {
        return false;
    }
    let today = now_local.date().to_string(); // YYYY-MM-DD
    schedule.last_run_at.as_deref() != Some(today.as_str())
}

/// The gated corpus one brief synthesizes from. `None` = QUIET-IF-EMPTY (no visible meeting in
/// the window ⇒ no row, no event).
pub struct BriefCorpus {
    /// The rendered corpus: meeting notes (`### [[Title]] · date` headers, the digest shape) +
    /// an open-commitments section + a current-facts section.
    pub corpus: String,
    /// The source meeting ids (opaque provenance for `brief_runs.meeting_ids`).
    pub meeting_ids: Vec<String>,
}

/// Build the brief corpus over the last `scope_days` days ending at `now_utc` (RFC3339), reading
/// EVERYTHING with the EMPTY unlock set — sealed-and-not-session-unlocked content never enters a
/// brief (see the module doc). Returns `Ok(None)` when NO visible meeting note falls in the
/// window (commitments/facts alone don't justify a brief — they derive from the same meetings).
pub fn build_brief_corpus(
    db: &Db,
    scope_days: i64,
    now_utc: &str,
    budget: usize,
) -> Result<Option<BriefCorpus>> {
    let no_unlocks: HashSet<String> = HashSet::new();
    let cutoff = chrono::DateTime::parse_from_rfc3339(now_utc)
        .map(|now| (now - chrono::Duration::days(scope_days.clamp(1, 90))).to_rfc3339())
        .unwrap_or_default();

    let mut corpus = String::new();
    let mut meeting_ids = Vec::new();
    for m in db.list_meetings_visible(300, &no_unlocks, None)? {
        if m.started_at.as_str() < cutoff.as_str() {
            continue;
        }
        if corpus.len() >= budget {
            break;
        }
        let Some(note) = db.get_note_if_visible(&m.id, &no_unlocks)? else {
            continue;
        };
        if note.markdown.trim().is_empty() {
            continue;
        }
        let title = m.title.clone().unwrap_or_else(|| "(untitled)".to_string());
        let date = m
            .started_at
            .split(['T', ' '])
            .next()
            .unwrap_or("")
            .to_string();
        let header = format!("\n\n### [[{title}]] · {date}\n");
        let remaining = budget.saturating_sub(corpus.len() + header.len());
        if remaining < 200 {
            break;
        }
        corpus.push_str(&header);
        corpus.push_str(&note.markdown.chars().take(remaining).collect::<String>());
        meeting_ids.push(m.id.clone());
    }
    if meeting_ids.is_empty() {
        return Ok(None); // quiet-if-empty — no row, no event.
    }

    // Open commitments (double-gated inside `list_open_commitments` on the same empty set).
    let commitments = db.list_open_commitments(&no_unlocks, None)?;
    if !commitments.is_empty() {
        corpus.push_str("\n\nOPEN COMMITMENTS (unfinished action items across these notes):\n");
        for c in commitments.iter().take(MAX_SECTION_ITEMS) {
            let owner = c.owner.as_deref().unwrap_or("(unassigned)");
            let due = c
                .due_date
                .as_deref()
                .map(|d| format!(" · due {d}"))
                .unwrap_or_default();
            corpus.push_str(&format!(
                "- {owner}{due} · \"{}\" · [[{}]]\n",
                c.text.trim(),
                c.meeting_title
            ));
        }
    }

    // Current user facts (gated reader, empty unlock set).
    let facts = db.list_user_facts_visible(&no_unlocks)?;
    if !facts.is_empty() {
        corpus.push_str("\nCURRENT FACTS ABOUT THE USER (durable memory):\n");
        for f in facts.iter().take(MAX_SECTION_ITEMS) {
            corpus.push_str(&format!("- {} {}: {}\n", f.subject, f.predicate, f.object));
        }
    }

    Ok(Some(BriefCorpus {
        corpus,
        meeting_ids,
    }))
}

/// Synthesize + stage ONE brief for a due `schedule`. Reads gated (empty unlock set), synthesizes
/// via the CURRENT Notes provider (consent-gated + redacted + ledgered — the note-gen seam),
/// inserts the pending `brief_runs` row. `Ok(None)` = quiet-if-empty.
async fn run_one_brief(
    state: &crate::state::AppState,
    schedule: &BriefSchedule,
    background_epoch: u64,
) -> Result<Option<BriefRun>> {
    if !crate::perf::background_epoch_is_current(background_epoch) {
        return Ok(None);
    }
    let config = state
        .config
        .lock()
        .map_err(|_| crate::error::AppError::Config("config mutex poisoned".into()))?
        .clone();
    // Budget keys on the RESOLVED Notes connection, exactly like `generate_digest`.
    let notes_conn =
        crate::summarize::roles::provider_target(crate::summarize::roles::Role::Notes, &config)
            .connection;
    let budget = if notes_conn == "ollama" {
        BRIEF_CORPUS_BUDGET_OLLAMA
    } else {
        BRIEF_CORPUS_BUDGET
    };
    let now_utc = chrono::Utc::now().to_rfc3339();
    let Some(built) = build_brief_corpus(&state.db, schedule.scope_days, &now_utc, budget)? else {
        return Ok(None);
    };

    // Re-check after the gated content read and immediately before provider construction/dispatch.
    if !crate::perf::background_epoch_is_current(background_epoch) {
        return Ok(None);
    }

    // Build the provider (consent gate + redaction firewall) BEFORE composing the prompt — an
    // unconsented cloud provider fails closed here and NOTHING egresses.
    let provider = crate::summarize::provider_for(
        crate::summarize::roles::Role::Notes,
        &config,
        &state.heavy_inference,
    )?;
    let range_label = format!("the last {} days", schedule.scope_days.clamp(1, 90));
    let (system, mut user) = crate::summarize::digest::build_digest_prompt(
        &built.corpus,
        &range_label,
        &config.note_language,
    );
    if let Some(hint) = schedule
        .prompt_hint
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
    {
        user.push_str(&format!("\n\nUSER FOCUS for this brief: {hint}"));
    }
    // Local GGUF and loopback Ollama providers acquire their own precise generation admission;
    // cloud providers hold no local-model lease across the network await.
    if !crate::perf::background_epoch_is_current(background_epoch) {
        return Ok(None);
    }
    let markdown = provider.complete(&system, &user).await?;
    // A Start installed priority after dispatch. Discard the answer; the already-written day claim
    // intentionally prevents a retry/egress storm.
    if !crate::perf::background_epoch_is_current(background_epoch) {
        return Ok(None);
    }
    if markdown.trim().is_empty() {
        return Ok(None); // an empty synthesis proposes nothing.
    }

    let run = BriefRun {
        id: uuid::Uuid::new_v4().to_string(),
        schedule_id: schedule.id.clone(),
        status: "pending".to_string(),
        note_md: markdown,
        meeting_ids: built.meeting_ids,
        proposed_at: now_utc,
        accepted_at: None,
    };
    if crate::perf::with_current_background_epoch(background_epoch, || {
        state.db.insert_brief_run(&run)
    })?
    .is_none()
    {
        return Ok(None);
    }
    Ok(Some(run))
}

/// ONE production tick (called every [`BRIEF_TICK_SECS`] from the `lib.rs` setup loop): re-read
/// the LIVE schedules, fire each due one (claiming its day FIRST so a failure never retry-storms),
/// emit [`crate::events::EVENT_BRIEF_PROPOSED`] per staged run. NEVER panics; every failure warns
/// and continues. Logs ids/counts only (no PII).
pub async fn brief_tick(handle: &tauri::AppHandle) {
    let Some(state) = handle.try_state::<crate::state::AppState>() else {
        return; // init failed — nothing to run.
    };
    if crate::perf::recording_has_priority() {
        return;
    }
    let schedules = match state.db.list_brief_schedules() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(target: "briefs", error = %e, "brief tick: schedule read failed");
            return;
        }
    };
    if schedules.is_empty() {
        return;
    }
    let now_local = chrono::Local::now().naive_local();
    let today = now_local.date().to_string();
    for schedule in schedules.iter().filter(|s| should_fire(s, &now_local)) {
        let epoch = crate::perf::background_epoch();
        if !crate::perf::background_epoch_is_current(epoch) {
            return;
        }
        // CLAIM the day BEFORE synthesis: max one attempt per schedule per local day, so a
        // persistently-failing provider can never become a once-a-minute egress storm.
        let claim = crate::perf::with_current_background_epoch(epoch, || {
            state.db.set_brief_schedule_last_run(&schedule.id, &today)
        });
        match claim {
            Ok(Some(())) => {}
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(target: "briefs", schedule_id = %schedule.id, error = %e, "brief tick: day-claim failed; skipping");
                continue;
            }
        }
        match run_one_brief(&state, schedule, epoch).await {
            Ok(Some(run)) => {
                tracing::info!(
                    target: "briefs",
                    schedule_id = %schedule.id,
                    run_id = %run.id,
                    meetings = run.meeting_ids.len(),
                    chars = run.note_md.len(),
                    "brief proposed"
                );
                if let Err(e) = handle.emit(
                    crate::events::EVENT_BRIEF_PROPOSED,
                    crate::events::BriefProposedPayload {
                        run_id: run.id.clone(),
                        label: schedule.label.clone(),
                        char_count: run.note_md.len(),
                    },
                ) {
                    tracing::warn!(target: "briefs", error = %e, "brief-proposed emit failed");
                }
            }
            Ok(None) => {
                // QUIET-IF-EMPTY: no visible meetings in the window — no row, no event.
                tracing::debug!(target: "briefs", schedule_id = %schedule.id, "brief skipped: empty corpus");
            }
            Err(e) => {
                tracing::warn!(target: "briefs", schedule_id = %schedule.id, error = %e, "brief synthesis failed; next attempt tomorrow");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{Folder, Meeting, MeetingStatus, NoteRecord};

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn file_db(label: &str) -> Db {
        let p = crate::storage::db::unique_temp_path(&format!("murmur-brief-{label}"), "sqlite");
        Db::open_with_key(&p, TEST_DEK).unwrap()
    }

    fn schedule(day: Option<i64>, hour: i64, minute: i64, last_run: Option<&str>) -> BriefSchedule {
        BriefSchedule {
            id: "s1".into(),
            label: "Morning brief".into(),
            day_of_week: day,
            hour_local: hour,
            minute_local: minute,
            scope_days: 7,
            prompt_hint: None,
            enabled: true,
            last_run_at: last_run.map(String::from),
            created_at: "2026-07-01T00:00:00Z".into(),
        }
    }

    fn at(date: &str, time: &str) -> chrono::NaiveDateTime {
        chrono::NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M:%S")
            .unwrap()
    }

    /// The pure firing predicate: enabled + day match + time reached + not-yet-today. 2026-07-10
    /// is a FRIDAY (num_days_from_monday = 4).
    #[test]
    fn should_fire_matrix() {
        let now = at("2026-07-10", "09:01:00");
        // Daily schedule, due time reached, never run → fires.
        assert!(should_fire(&schedule(None, 9, 0, None), &now));
        // Time not reached yet → holds.
        assert!(!should_fire(&schedule(None, 9, 5, None), &now));
        // Already ran today → holds (max one run per day).
        assert!(!should_fire(
            &schedule(None, 9, 0, Some("2026-07-10")),
            &now
        ));
        // Ran YESTERDAY → fires again today.
        assert!(should_fire(&schedule(None, 9, 0, Some("2026-07-09")), &now));
        // Weekly on Friday (4) → fires; on Monday (0) → holds.
        assert!(should_fire(&schedule(Some(4), 9, 0, None), &now));
        assert!(!should_fire(&schedule(Some(0), 9, 0, None), &now));
        // Disabled → never fires.
        let mut off = schedule(None, 9, 0, None);
        off.enabled = false;
        assert!(!should_fire(&off, &now));
        // A LATE tick (app asleep at 09:00, awake at 17:30) still fires that day.
        assert!(should_fire(
            &schedule(None, 9, 0, None),
            &at("2026-07-10", "17:30:00")
        ));
    }

    fn seed_meeting_with_note(db: &Db, id: &str, started_at: &str, note: &str) {
        db.insert_meeting(&Meeting {
            id: id.to_string(),
            started_at: started_at.to_string(),
            ended_at: None,
            title: Some(format!("Meeting {id}")),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: id.to_string(),
            provider_id: "claude_code".to_string(),
            markdown: note.to_string(),
            created_at: started_at.to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
    }

    /// QUIET-IF-EMPTY: an empty vault (or a window with no visible notes) builds NO corpus.
    #[test]
    fn empty_window_builds_no_corpus() {
        let db = file_db("empty");
        let out = build_brief_corpus(&db, 7, "2026-07-10T09:00:00+00:00", 80_000).unwrap();
        assert!(out.is_none(), "no meetings ⇒ quiet (no row, no event)");

        // A meeting OUTSIDE the window is also quiet.
        seed_meeting_with_note(&db, "old", "2026-05-01T09:00:00Z", "- [ ] Bob: send deck");
        let out = build_brief_corpus(&db, 7, "2026-07-10T09:00:00+00:00", 80_000).unwrap();
        assert!(out.is_none(), "out-of-window meetings ⇒ quiet");
    }

    /// The corpus carries the digest-shaped note headers + the commitments + facts sections, and
    /// records the source meeting ids (ids only).
    #[test]
    fn corpus_includes_notes_commitments_and_facts() {
        let db = file_db("corpus");
        seed_meeting_with_note(
            &db,
            "m1",
            "2026-07-08T09:00:00Z",
            "# Sync\n- decided to ship\n- [ ] Anna: send the deck (due: 2026-07-12)\n",
        );
        db.apply_user_fact_ops(&[crate::facts::FactOp::Add(crate::facts::NewFact {
            entity_id: crate::user_memory::USER_SCOPE.to_string(),
            subject: "You".to_string(),
            predicate: "works on".to_string(),
            object: "Project Atlas".to_string(),
            valid_from: "2026-07-08T09:00:00Z".to_string(),
            recorded_at: "2026-07-08T09:00:00Z".to_string(),
            confidence: 1.0,
            meeting_id: Some("m1".to_string()),
        })])
        .unwrap();

        let out = build_brief_corpus(&db, 7, "2026-07-10T09:00:00+00:00", 80_000)
            .unwrap()
            .expect("one visible meeting in the window");
        assert!(out.corpus.contains("### [[Meeting m1]] · 2026-07-08"));
        assert!(out.corpus.contains("decided to ship"));
        assert!(out.corpus.contains("OPEN COMMITMENTS"));
        assert!(out.corpus.contains("Anna"));
        assert!(out.corpus.contains("CURRENT FACTS"));
        assert!(out.corpus.contains("Project Atlas"));
        assert_eq!(out.meeting_ids, vec!["m1".to_string()]);
    }

    /// GATE (the load-bearing brief posture): a SEALED-and-not-unlocked meeting contributes
    /// NOTHING — not its note, not its commitments, not its facts. The runner reads with the
    /// EMPTY unlock set by design (the consolidation-job discipline).
    #[test]
    fn corpus_excludes_sealed_meetings() {
        let db = file_db("sealed");
        db.insert_folder(&Folder {
            id: "f-lock".to_string(),
            name: "Secret".to_string(),
            path: "Secret".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-01T00:00:00Z".to_string(),
        })
        .unwrap();
        seed_meeting_with_note(
            &db,
            "m-sealed",
            "2026-07-08T09:00:00Z",
            "SECRET-ACQUISITION plan\n- [ ] Kim: wire five million\n",
        );
        db.set_note_folder("m-sealed", Some("f-lock")).unwrap();
        db.apply_user_fact_ops(&[crate::facts::FactOp::Add(crate::facts::NewFact {
            entity_id: crate::user_memory::USER_SCOPE.to_string(),
            subject: "You".to_string(),
            predicate: "salary".to_string(),
            object: "CONFIDENTIAL-NUMBER".to_string(),
            valid_from: "2026-07-08T09:00:00Z".to_string(),
            recorded_at: "2026-07-08T09:00:00Z".to_string(),
            confidence: 1.0,
            meeting_id: Some("m-sealed".to_string()),
        })])
        .unwrap();
        db.set_folder_locked("f-lock", true, None).unwrap();

        // The sealed meeting is the ONLY meeting → quiet-if-empty, and nothing sealed anywhere.
        let out = build_brief_corpus(&db, 7, "2026-07-10T09:00:00+00:00", 80_000).unwrap();
        assert!(out.is_none(), "a sealed-only window must be QUIET");

        // With a sibling OPEN meeting, the brief exists but carries ZERO sealed content.
        seed_meeting_with_note(&db, "m-open", "2026-07-09T09:00:00Z", "- open sync note\n");
        let out = build_brief_corpus(&db, 7, "2026-07-10T09:00:00+00:00", 80_000)
            .unwrap()
            .expect("the open meeting builds a brief");
        assert!(
            !out.corpus.contains("SECRET-ACQUISITION"),
            "sealed note text must not leak"
        );
        assert!(
            !out.corpus.contains("five million"),
            "sealed commitments must not leak"
        );
        assert!(
            !out.corpus.contains("CONFIDENTIAL-NUMBER"),
            "sealed facts must not leak"
        );
        assert!(
            !out.corpus.contains("Meeting m-sealed"),
            "the sealed TITLE must not leak"
        );
        assert_eq!(out.meeting_ids, vec!["m-open".to_string()]);
    }

    /// Schedule CRUD + run staging round-trip: insert → list → update → last-run stamp →
    /// pending runs → accept CONSUMES note_md → dismiss deletes → schedule delete removes runs.
    #[test]
    fn brief_schedule_and_run_rows_round_trip() {
        let db = file_db("crud");
        let s = schedule(Some(0), 8, 30, None);
        db.insert_brief_schedule(&s).unwrap();
        let listed = db.list_brief_schedules().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].label, "Morning brief");
        assert_eq!(listed[0].day_of_week, Some(0));
        assert!(listed[0].enabled);

        let mut edited = listed[0].clone();
        edited.label = "Monday kickoff".into();
        edited.enabled = false;
        edited.prompt_hint = Some("focus on blockers".into());
        db.update_brief_schedule(&edited).unwrap();
        let listed = db.list_brief_schedules().unwrap();
        assert_eq!(listed[0].label, "Monday kickoff");
        assert!(!listed[0].enabled);
        assert_eq!(listed[0].prompt_hint.as_deref(), Some("focus on blockers"));

        db.set_brief_schedule_last_run("s1", "2026-07-10").unwrap();
        assert_eq!(
            db.list_brief_schedules().unwrap()[0].last_run_at.as_deref(),
            Some("2026-07-10")
        );

        let run = BriefRun {
            id: "r1".into(),
            schedule_id: "s1".into(),
            status: "pending".into(),
            note_md: "## Brief\n- ship it".into(),
            meeting_ids: vec!["m1".into(), "m2".into()],
            proposed_at: "2026-07-10T09:00:00Z".into(),
            accepted_at: None,
        };
        db.insert_brief_run(&run).unwrap();
        let pending = db.list_pending_brief_runs().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].meeting_ids, vec!["m1", "m2"]);
        assert_eq!(pending[0].note_md, "## Brief\n- ship it");

        // Accept CONSUMES the markdown (the vault .md becomes the copy) and leaves the pending list.
        db.accept_brief_run("r1", "2026-07-10T09:05:00Z").unwrap();
        assert!(db.list_pending_brief_runs().unwrap().is_empty());
        let r = db.get_brief_run("r1").unwrap().unwrap();
        assert_eq!(r.status, "accepted");
        assert_eq!(r.note_md, "", "note_md must be consumed on accept");
        assert_eq!(r.accepted_at.as_deref(), Some("2026-07-10T09:05:00Z"));

        // Dismiss deletes; deleting the schedule removes its remaining runs too.
        let run2 = BriefRun {
            id: "r2".into(),
            ..run.clone()
        };
        db.insert_brief_run(&run2).unwrap();
        db.delete_brief_run("r2").unwrap();
        assert!(db.get_brief_run("r2").unwrap().is_none());
        let run3 = BriefRun {
            id: "r3".into(),
            ..run
        };
        db.insert_brief_run(&run3).unwrap();
        db.delete_brief_schedule("s1").unwrap();
        assert!(db.list_brief_schedules().unwrap().is_empty());
        assert!(
            db.get_brief_run("r3").unwrap().is_none(),
            "schedule delete removes its runs"
        );
    }
}
