//! WEEKLY DIGEST + scheduled-BRIEFS command surface (a GATED domain where it reads content).
//!
//! Extracted verbatim from `commands/mod.rs` (God-file split, PURE MOVE — every body is
//! byte-identical, only relocated). Two clusters:
//!   1. `generate_digest` — synthesizes a Weekly Vault Digest. GATED: the cloud corpus is built from
//!      VISIBLE meetings + VISIBLE notes only (`list_meetings_visible(unlocked)` +
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
    let mut corpus = String::new();
    let mut count = 0usize;
    for m in state.db.list_meetings_visible(300, &unlocked)? {
        if m.started_at.as_str() < cutoff.as_str() {
            continue;
        }
        if corpus.len() >= budget {
            break;
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
        let header = format!("\n\n### [[{title}]] · {date}\n");
        let remaining = budget.saturating_sub(corpus.len() + header.len());
        if remaining < 200 {
            break;
        }
        corpus.push_str(&header);
        corpus.push_str(&note.markdown.chars().take(remaining).collect::<String>());
        count += 1;
    }
    if count == 0 {
        return Err(AppError::InvalidArg(format!(
            "no summarized meetings in the last {days} days"
        )));
    }
    let range_label = format!("the last {days} days");
    let provider = crate::summarize::provider_for(crate::summarize::roles::Role::Notes, &config, &state.heavy_inference)?;
    let (system, user) =
        crate::summarize::digest::build_digest_prompt(&corpus, &range_label, &config.note_language);
    let markdown = provider.complete(&system, &user).await?;

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
        prompt_hint: prompt_hint.map(|h| h.trim().to_string()).filter(|h| !h.is_empty()),
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
        return Err(AppError::InvalidArg("this brief was already handled".into()));
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

