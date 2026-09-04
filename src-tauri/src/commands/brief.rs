//! WEEKLY DIGEST + scheduled-BRIEFS command surface (a GATED domain where it reads content).
//!
//! Extracted verbatim from `commands/mod.rs` (God-file split, PURE MOVE — every body is
//! byte-identical, only relocated). Two clusters:
//!   1. `generate_digest` — synthesizes a Weekly Vault Digest. GATED: the cloud corpus is built from
//!      VISIBLE meetings + VISIBLE notes only (`list_meetings_visible(unlocked, None, None)` +
//!      `get_note_if_visible(unlocked)` — the same `visibility_clause` predicate MCP uses), so a
//!      sealed-and-not-session-unlocked meeting's TITLE and markdown NEVER leave the device. The
//!      NOTES-role provider is built through `provider_for` (consent gate + redaction firewall).
//!   2. Scheduled-briefs (`list/create/update/delete_brief_schedule`, `list_brief_runs`,
//!      `accept_brief`, `dismiss_brief` + the `validate_brief_schedule` helper). The schedules are
//!      config rows (labels/timing/hints — no content). `list_brief_runs` returns PENDING proposal
//!      cards whose `note_md` was synthesized by the runner from VISIBLE-ONLY content AND is purged
//!      from any meeting that seals after the proposal (`Db::purge_pending_brief_runs_tx`, inside the
//!      seal tx) — that pair is what makes the read safe without a per-meeting gate (documented
//!      posture; the gate LOGIC is byte-identical, only relocated). `accept_brief` exports to the
//!      vault via `crate::export::write_note` under the shared `vault_path` (which STAYS in
//!      `commands/mod.rs`, reached via `super::`).
//!
//! Bound as `brief_commands` (via `#[path]`) to keep it clearly distinct and avoid any future name
//! shadow with the crate-level `crate::brief_runner`. The glob re-export makes every moved command
//! resolve UNCHANGED at `crate::commands::…` for `generate_handler!` in `lib.rs` and every caller.
//! Shared imports (`AppError`, `AppState`, `State`, `DigestResult`, `unlocked_snapshot`,
//! `vault_path`, …) come in via `use super::*`.

use super::*;

/// Generate a Weekly Vault Digest synthesizing meetings from the last `days` days; writes it
/// into the vault's Digests/ folder and returns the markdown + path.
#[tauri::command]
pub async fn generate_digest(
    state: State<'_, AppState>,
    days: i64,
) -> Result<DigestResult, AppError> {
    let days = days.clamp(1, 90);
    let visibility = capture_content_visibility_snapshot(state.inner());
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
    // Budget on the NOTES-role provider's RESOLVED connection — the corpus egresses to it
    // (identical to `provider_id` while role keys are absent).
    let notes_conn =
        crate::summarize::roles::provider_target(crate::summarize::roles::Role::Notes, &config)
            .connection;
    let budget = if notes_conn == "ollama" {
        4_000
    } else {
        80_000
    };
    // Finding 2 + BLK-2b: build the cloud corpus from VISIBLE meetings + VISIBLE notes only, so a
    // sealed-and-not-unlocked meeting's TITLE (the `### [[title]]` header) AND markdown never leave
    // the device. `list_meetings_visible` + `get_note_if_visible` push the session unlock set
    // through the same predicate as MCP — correctness no longer depends on at-rest blanking.
    let unlocked = unlocked_snapshot(state.inner())?;
    // Collect the VISIBLE + SUMMARIZED meetings inside the window (newest-first), keeping the
    // visibility gate EXACTLY as-is: `list_meetings_visible` + `get_note_if_visible` push the
    // session unlock set through the same `visibility_clause` predicate MCP uses. Assembling the
    // corpus (the whole-note-or-skip budgeting + the omitted-count marker) is factored into the
    // pure `assemble_digest_corpus` so it is unit-testable without a live provider.
    let mut entries: Vec<DigestEntry> = Vec::new();
    for m in state.db.list_meetings_visible(300, &unlocked, None)? {
        if m.started_at.as_str() < cutoff.as_str() {
            continue;
        }
        let Some(note) = state.db.get_note_if_visible(&m.id, &unlocked)? else {
            continue;
        };
        let title = m.title.clone().unwrap_or_else(|| "(untitled)".to_string());
        let date = m
            .started_at
            .split(['T', ' '])
            .next()
            .unwrap_or("")
            .to_string();
        entries.push(DigestEntry {
            title,
            date,
            markdown: note.markdown,
        });
    }
    let assembled = assemble_digest_corpus(&entries, budget);
    if assembled.included == 0 {
        return Err(AppError::InvalidArg(format!(
            "no summarized meetings in the last {days} days"
        )));
    }
    // Non-PII: counts only (no titles/content) — records whether any meetings were dropped for budget.
    tracing::info!(
        target: "digest",
        included = assembled.included,
        omitted = assembled.omitted,
        "assembled weekly-digest corpus"
    );
    let corpus = assembled.corpus;
    let range_label = format!("the last {days} days");
    let provider = crate::summarize::provider_for(
        crate::summarize::roles::Role::Notes,
        &config,
        &state.heavy_inference,
    )?;
    let (system, user) =
        crate::summarize::digest::build_digest_prompt(&corpus, &range_label, &config.note_language);
    let markdown = provider.complete(&system, &user).await?;

    // The digest is derived from a multi-folder corpus. Revalidate and keep the lifecycle guard
    // through vault publication so a relock cannot purge exports and then be followed by this late
    // plaintext write.
    let _lifecycle = lifecycle_guard(state.inner());
    require_current_content_visibility_snapshot_under_lifecycle(state.inner(), visibility)?;
    let exported_path = match config.vault_path.as_deref().filter(|p| !p.is_empty()) {
        Some(vault) => {
            let now = chrono::Utc::now().to_rfc3339();
            crate::export::write_note(
                std::path::Path::new(vault),
                Some("Digests"),
                "Weekly Digest",
                &now,
                &markdown,
            )
            .ok()
            .map(|p| p.to_string_lossy().to_string())
        }
        None => None,
    };
    Ok(DigestResult {
        markdown,
        exported_path,
    })
}

/// One VISIBLE + SUMMARIZED meeting in the digest window (already gated by
/// `list_meetings_visible` / `get_note_if_visible` at the call site).
struct DigestEntry {
    title: String,
    date: String,
    markdown: String,
}

/// The assembled digest corpus plus how many notes were included vs omitted for budget.
struct AssembledDigest {
    corpus: String,
    included: usize,
    omitted: usize,
}

/// Assemble the digest corpus from newest-first entries.
///
/// Two invariants (the 2026-07-25 silent-drop + mid-note-truncation fix):
///   1. A note is included WHOLE or skipped ENTIRELY — never truncated mid-content. (The old
///      `note.markdown.chars().take(remaining)` cut the boundary note in the middle of a line.)
///   2. When one or more visible-and-summarized meetings in the window are dropped for budget,
///      an explicit human-readable marker with the correct omitted COUNT is appended so nothing
///      is silently lost. (The old loop `break`-ed past budget with no marker.)
///
/// The newest note is always admitted (so a non-empty window never yields an empty digest); once a
/// whole note would exceed the budget the rest of the (older) window is omitted and counted.
fn assemble_digest_corpus(entries: &[DigestEntry], budget: usize) -> AssembledDigest {
    let mut corpus = String::new();
    let mut included = 0usize;
    let mut omitted = 0usize;
    let mut budget_reached = false;
    for entry in entries {
        let header = format!("\n\n### [[{}]] · {}\n", entry.title, entry.date);
        let would_be = corpus.len() + header.len() + entry.markdown.len();
        // Always admit the first (newest) note; after that, a whole note that would push the
        // corpus over budget is omitted — and so is every older note behind it.
        if budget_reached || (included > 0 && would_be > budget) {
            budget_reached = true;
            omitted += 1;
            continue;
        }
        corpus.push_str(&header);
        corpus.push_str(&entry.markdown);
        included += 1;
    }
    if omitted > 0 {
        let noun = if omitted == 1 { "meeting" } else { "meetings" };
        corpus.push_str(&format!(
            "\n\n_({omitted} earlier {noun} omitted — over the digest size budget)_\n"
        ));
    }
    AssembledDigest {
        corpus,
        included,
        omitted,
    }
}

// ── Brain v2 L5 — scheduled briefs (schedule CRUD + propose-accept runs) ─────────────────────────

/// All brief schedules (config rows — labels, timing, hints; no meeting content).
#[tauri::command]
pub fn list_brief_schedules(
    state: State<'_, AppState>,
) -> Result<Vec<crate::storage::models::BriefSchedule>, AppError> {
    state.db.list_brief_schedules()
}

/// Validate a brief schedule's user-editable fields (shared by create + update).
fn validate_brief_schedule(s: &crate::storage::models::BriefSchedule) -> Result<(), AppError> {
    if s.label.trim().is_empty() {
        return Err(AppError::InvalidArg("brief label is empty".into()));
    }
    if let Some(d) = s.day_of_week {
        if !(0..=6).contains(&d) {
            return Err(AppError::InvalidArg(
                "day_of_week must be 0 (Monday) … 6 (Sunday)".into(),
            ));
        }
    }
    if !(0..=23).contains(&s.hour_local) {
        return Err(AppError::InvalidArg("hour must be 0…23".into()));
    }
    if !(0..=59).contains(&s.minute_local) {
        return Err(AppError::InvalidArg("minute must be 0…59".into()));
    }
    if !(1..=90).contains(&s.scope_days) {
        return Err(AppError::InvalidArg("scope_days must be 1…90".into()));
    }
    Ok(())
}

/// Create one brief schedule. `day_of_week`: 0 = Monday … 6 = Sunday, `None` = daily. The runner
/// (`crate::brief_runner`) fires it at most once per local day; the first fire is the first 60s
/// tick at/after `hour:minute` local.
#[tauri::command]
pub fn create_brief_schedule(
    state: State<'_, AppState>,
    label: String,
    day_of_week: Option<i64>,
    hour_local: i64,
    minute_local: i64,
    scope_days: Option<i64>,
    prompt_hint: Option<String>,
) -> Result<crate::storage::models::BriefSchedule, AppError> {
    let schedule = crate::storage::models::BriefSchedule {
        id: uuid::Uuid::new_v4().simple().to_string(),
        label: label.trim().to_string(),
        day_of_week,
        hour_local,
        minute_local,
        scope_days: scope_days.unwrap_or(7),
        prompt_hint: prompt_hint
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty()),
        enabled: true,
        last_run_at: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    validate_brief_schedule(&schedule)?;
    state.db.insert_brief_schedule(&schedule)?;
    Ok(schedule)
}

/// Update one brief schedule's editable fields (label / timing / window / hint / enabled).
#[tauri::command]
pub fn update_brief_schedule(
    state: State<'_, AppState>,
    schedule: crate::storage::models::BriefSchedule,
) -> Result<(), AppError> {
    validate_brief_schedule(&schedule)?;
    state.db.update_brief_schedule(&schedule)
}

/// Delete one brief schedule AND its staged runs.
#[tauri::command]
pub fn delete_brief_schedule(
    state: State<'_, AppState>,
    schedule_id: String,
) -> Result<(), AppError> {
    state.db.delete_brief_schedule(&schedule_id)
}

/// The PENDING (proposed, not yet accepted/dismissed) brief runs — the FE's proposal cards.
/// `note_md` was synthesized by the runner from VISIBLE-ONLY content (empty unlock set — the
/// consolidation-job discipline), so it cannot contain sealed content AT synthesis time; and a
/// meeting sealed AFTER the proposal purges its pending runs inside the seal tx
/// (`Db::purge_pending_brief_runs_tx` — the lock-security LEAK fix, 2026-07-10), so a row this
/// returns never paraphrases a currently-sealed meeting. That pair is what makes this read safe
/// without a per-meeting gate (documented posture, see `crate::brief_runner` + `migrate_briefs`).
#[tauri::command]
pub fn list_brief_runs(
    state: State<'_, AppState>,
) -> Result<Vec<crate::storage::models::BriefRun>, AppError> {
    state.db.list_pending_brief_runs()
}

/// ACCEPT a proposed brief: export its markdown to `<vault>/Briefs/` (atomic write) and CONSUME
/// the staged `note_md` (the vault `.md` becomes the only copy). Returns the exported path.
#[tauri::command]
pub fn accept_brief(state: State<'_, AppState>, run_id: String) -> Result<String, AppError> {
    let run = state
        .db
        .get_brief_run(&run_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no brief run {run_id}")))?;
    if run.status != "pending" {
        return Err(AppError::InvalidArg(
            "this brief was already handled".into(),
        ));
    }
    if run.note_md.trim().is_empty() {
        return Err(AppError::InvalidArg("this brief has no content".into()));
    }
    let vault = vault_path(state.inner())
        .ok_or_else(|| AppError::InvalidArg("set an Obsidian vault first (Settings)".into()))?;
    let label = state
        .db
        .list_brief_schedules()?
        .into_iter()
        .find(|s| s.id == run.schedule_id)
        .map(|s| s.label)
        .unwrap_or_else(|| "Brief".to_string());
    let path = crate::export::write_note(
        std::path::Path::new(&vault),
        Some("Briefs"),
        &label,
        &run.proposed_at,
        &run.note_md,
    )?;
    state
        .db
        .accept_brief_run(&run_id, &chrono::Utc::now().to_rfc3339())?;
    Ok(path.to_string_lossy().to_string())
}

/// DISMISS a proposed brief: the staged row (markdown included) is deleted outright.
#[tauri::command]
pub fn dismiss_brief(state: State<'_, AppState>, run_id: String) -> Result<(), AppError> {
    state.db.delete_brief_run(&run_id)
}

#[cfg(test)]
mod tests {
    use super::{assemble_digest_corpus, DigestEntry};

    /// A multi-line note ending in a stable terminal sentinel, so a mid-content truncation is
    /// detectable (the sentinel would be missing).
    fn note_body(n: usize) -> String {
        let mut s = String::new();
        for line in 0..20 {
            s.push_str(&format!(
                "meeting {n} line {line} lorem ipsum dolor sit amet consectetur\n"
            ));
        }
        s.push_str(&format!("[END-OF-NOTE-{n}]"));
        s
    }

    /// RED-before-GREEN regression for the 2026-07-25 silent-drop + mid-note-truncation bug:
    /// seed > budget chars of visible summarized notes across the window and assert (i) the
    /// omitted-count marker is present with the correct count, and (ii) every INCLUDED note body
    /// is byte-complete (never cut mid-content).
    ///
    /// Against the OLD code this fails twice: the boundary note was truncated via
    /// `.chars().take(remaining)` (sentinel missing) and the loop `break`-ed with NO marker.
    #[test]
    fn digest_never_truncates_and_marks_omitted() {
        let budget = 4_000usize;
        let entries: Vec<DigestEntry> = (0..10)
            .map(|n| DigestEntry {
                title: format!("Title {n}"),
                date: "2026-07-20".to_string(),
                markdown: note_body(n),
            })
            .collect();
        // Sanity: the corpus of all notes is well over budget so omission MUST happen.
        assert!(
            entries.iter().map(|e| e.markdown.len()).sum::<usize>() > budget,
            "test setup must exceed the budget"
        );

        let assembled = assemble_digest_corpus(&entries, budget);

        // (i) some meetings were omitted, and every window meeting is accounted for.
        assert!(
            assembled.omitted > 0,
            "expected some meetings omitted over budget"
        );
        assert!(
            assembled.included > 0,
            "the newest note is always admitted so the digest is never empty"
        );
        assert_eq!(
            assembled.included + assembled.omitted,
            entries.len(),
            "every visible+summarized meeting is either included or counted as omitted"
        );

        // ...and the explicit human-readable marker carries the CORRECT omitted count.
        let noun = if assembled.omitted == 1 {
            "meeting"
        } else {
            "meetings"
        };
        let expected_marker = format!(
            "_({} earlier {noun} omitted — over the digest size budget)_",
            assembled.omitted
        );
        assert!(
            assembled.corpus.contains(&expected_marker),
            "digest corpus must carry an explicit omitted-count marker; got tail: {:?}",
            &assembled.corpus[assembled.corpus.len().saturating_sub(160)..]
        );

        // (ii) every INCLUDED note is byte-complete — never cut mid-content. For each note whose
        // header appears, its FULL body (including the terminal sentinel) must be present verbatim.
        for n in 0..10 {
            let header = format!("### [[Title {n}]]");
            if assembled.corpus.contains(&header) {
                let body = note_body(n);
                assert!(
                    assembled.corpus.contains(&body),
                    "note {n} was included but is truncated mid-content (body not byte-complete)"
                );
                assert!(
                    assembled.corpus.contains(&format!("[END-OF-NOTE-{n}]")),
                    "note {n} was included but its terminal sentinel is missing (mid-content cut)"
                );
            }
        }
    }

    /// The marker uses the singular noun when exactly one meeting is omitted.
    #[test]
    fn omitted_marker_is_singular_for_one() {
        // Two notes each ~900 chars; budget admits only the first, omitting exactly one.
        let entries: Vec<DigestEntry> = (0..2)
            .map(|n| DigestEntry {
                title: format!("Title {n}"),
                date: "2026-07-20".to_string(),
                markdown: note_body(n),
            })
            .collect();
        let one_note = entries[0].markdown.len();
        let assembled = assemble_digest_corpus(&entries, one_note + 40);
        assert_eq!(assembled.included, 1);
        assert_eq!(assembled.omitted, 1);
        assert!(
            assembled
                .corpus
                .contains("_(1 earlier meeting omitted — over the digest size budget)_"),
            "singular-noun marker missing; tail: {:?}",
            &assembled.corpus[assembled.corpus.len().saturating_sub(120)..]
        );
    }

    /// No omission → no marker at all (don't cry wolf).
    #[test]
    fn no_marker_when_nothing_omitted() {
        let entries = vec![DigestEntry {
            title: "Only".to_string(),
            date: "2026-07-20".to_string(),
            markdown: "short body\n[END-OF-NOTE-0]".to_string(),
        }];
        let assembled = assemble_digest_corpus(&entries, 80_000);
        assert_eq!(assembled.included, 1);
        assert_eq!(assembled.omitted, 0);
        assert!(!assembled.corpus.contains("omitted"));
        assert!(assembled.corpus.contains("[END-OF-NOTE-0]"));
    }
}
