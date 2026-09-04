//! Vault Audit v1 — DETERMINISTIC vault-health passes over the user's visible notes. Zero
//! egress, zero LLM: every finding is derived by pure string/graph/SQL analysis, staged as a
//! propose→accept row (`audit_findings`), and only ever APPLIED to a vault `.md` as an
//! append-only line under the managed `## Audit` section (see
//! [`crate::export::obsidian::AUDIT_SECTION`]).
//!
//! ## Lock model (audited by the lock-security review)
//! The corpus is built EXCLUSIVELY through the gated readers with the EMPTY unlock set —
//! `list_meetings_visible` + `get_note_if_visible` + `list_notes_visible` +
//! `note_markdown_if_visible` — the SAME discipline as the memory consolidation job
//! ([`crate::memory`]) and the scheduled-brief runner ([`crate::brief_runner`]): a background
//! job must never see session-unlocked plaintext, let alone sealed content. A pending finding's
//! `evidence_md` is derived plaintext (a quoted line, counts, suggested `[[links]]`), so it is
//! the SAME purge class as pending brief runs: every seal path purges pending findings whose
//! source or target is being sealed (`Db::purge_pending_audit_findings_tx`), and resolve
//! (accept OR dismiss) blanks `evidence_md`/`accept_action` — only PENDING rows ever hold
//! derived plaintext.
//!
//! ## The seal-epoch TOCTOU guard
//! Exactly like [`crate::memory::run_consolidation_pass`], the pass snapshots
//! `AppState::seal_epoch` before any read and re-checks it before EVERY finding insert: a seal /
//! relock / remove-lock interleaving with the pass aborts the write phase silently (the next
//! manual run re-derives against the post-seal visible set). Without this, a seal landing
//! between the gated corpus read and the findings write could persist just-sealed content into a
//! pending finding the seal's own purge had already run for.
//!
//! No PII in logs: ids, kinds, counts only.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::error::Result;
use crate::storage::Db;

/// Bound on the visible-meetings scan (mirrors the brief runner's corpus cap posture — a
/// deterministic bound, not pagination).
const AUDIT_MAX_MEETINGS: i64 = 10_000;

/// Max unlinked-mention findings emitted per SOURCE note per pass (spec cap).
const MAX_MENTIONS_PER_NOTE: usize = 3;

/// Max reconnection suggestions computed for one orphan finding (spec cap).
const MAX_ORPHAN_SUGGESTIONS: usize = 3;

/// Minimum title length (chars) for the unlinked-mention matcher — shorter titles ("Sync",
/// "Notes") false-positive on ordinary prose.
const MIN_MENTION_TITLE_CHARS: usize = 6;

/// Staleness thresholds: a meeting note is flagged when it carries at least this many facts…
const STALE_MIN_FACTS: usize = 3;
// …and at least half of them are closed (superseded). Expressed as closed*2 >= total.

// ── Wire DTOs (the pinned FE seam — the Angular builder codes against exactly these) ─────────

/// Summary of one completed (or seal-aborted) audit pass.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditRunSummary {
    pub run_id: String,
    pub started_at: i64,
    pub finished_at: i64,
    /// Findings newly staged THIS pass (post-dedupe, PRE-judge — the deterministic pass's count;
    /// `demoted` says how many of them the judge then deleted).
    pub findings_new: usize,
    /// Total pending findings after the pass (includes survivors of earlier passes; refreshed
    /// AFTER the judge tier when it ran).
    pub findings_total_pending: usize,
    /// New findings per kind, e.g. `{"broken_link": 2, "orphan": 1}`.
    pub counts: BTreeMap<String, usize>,
    /// Judge tier (Phase 3): how many of this run's `contradiction`/`stale` findings the LOCAL
    /// judge scored (0 when the light model is absent — the stub skip).
    pub judged: usize,
    /// …and how many it demoted (deleted outright as noise; they may re-stage next pass).
    pub demoted: usize,
}

/// The weekly-schedule wire DTO (`get_audit_schedule` / `set_audit_schedule`). Both timestamps
/// are EPOCH MILLISECONDS — the same unit as `AuditFinding.createdAt`/`resolvedAt` and the
/// `audit_runs` columns (everything on the audit surface flows from
/// `chrono::Utc::now().timestamp_millis()`). `next_due_at` is `last_run_at + 7 days` when
/// enabled; `None` when disabled, OR when enabled but never run — the never-ran case is due at
/// the NEXT hourly check (the FE reads `None` + `enabled` as "runs within the hour").
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditSchedule {
    pub enabled: bool,
    pub last_run_at: Option<i64>,
    pub next_due_at: Option<i64>,
}

/// The `explain_audit_finding` wire DTO. RETURN-ONLY — the explanation is never persisted
/// anywhere (no new derived-plaintext at rest); `provider` names the connection that produced it
/// (the egress-ledger row is the durable record of the call, content-free as always).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditExplanation {
    pub finding_id: String,
    pub explanation_md: String,
    pub provider: String,
}

/// One finding, FE-shaped. `evidence_md`/`accept_action` are blanked once resolved (only
/// PENDING rows hold derived plaintext).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditFinding {
    pub id: String,
    /// broken_link | orphan | stale | contradiction | unlinked_mention.
    pub kind: String,
    /// meeting | note.
    pub source_kind: String,
    pub source_id: String,
    pub source_title: String,
    pub target_title: Option<String>,
    pub evidence_md: String,
    /// Human description of what accept will do; empty = dismiss-only.
    pub accept_action: String,
    /// pending | accepted | dismissed.
    pub status: String,
    pub created_at: i64,
    pub resolved_at: Option<i64>,
}

// ── DB-shaped rows ────────────────────────────────────────────────────────────────────────────

/// A full `audit_findings` row (superset of the wire DTO: adds `target_id`/`dedupe_key`/`run_id`).
#[derive(Debug, Clone)]
pub struct AuditFindingRow {
    pub id: String,
    pub kind: String,
    pub source_kind: String,
    pub source_id: String,
    pub source_title: String,
    pub target_title: Option<String>,
    pub target_id: Option<String>,
    /// "meeting" | "note" when `target_id` is set — lets the list layer re-gate the TARGET side
    /// against the right table. Row-only (not on the pinned wire DTO).
    pub target_kind: Option<String>,
    pub evidence_md: String,
    pub accept_action: String,
    pub dedupe_key: String,
    pub status: String,
    pub run_id: String,
    pub created_at: i64,
    pub resolved_at: Option<i64>,
}

impl AuditFindingRow {
    pub fn into_dto(self) -> AuditFinding {
        AuditFinding {
            id: self.id,
            kind: self.kind,
            source_kind: self.source_kind,
            source_id: self.source_id,
            source_title: self.source_title,
            target_title: self.target_title,
            evidence_md: self.evidence_md,
            accept_action: self.accept_action,
            status: self.status,
            created_at: self.created_at,
            resolved_at: self.resolved_at,
        }
    }
}

/// A finding BEFORE insert (no id/run/timestamps — the insert assigns them). `dedupe_key` is the
/// stable identity across runs: an existing PENDING or DISMISSED row with the same key suppresses
/// re-creation (dismissed = "don't nag again"); an ACCEPTED one does not (evidence may recur).
#[derive(Debug, Clone)]
pub struct NewAuditFinding {
    pub kind: String,
    pub source_kind: String,
    pub source_id: String,
    pub source_title: String,
    pub target_title: Option<String>,
    pub target_id: Option<String>,
    /// "meeting" | "note" when `target_id` is set (the list layer's target re-gate key).
    pub target_kind: Option<String>,
    pub evidence_md: String,
    pub accept_action: String,
    pub dedupe_key: String,
}

/// One visible corpus document — a meeting note or a standalone authored note.
#[derive(Debug, Clone)]
pub(crate) struct CorpusDoc {
    /// "meeting" | "note".
    pub kind: &'static str,
    pub id: String,
    pub title: String,
    pub body: String,
}

// ── The pass ─────────────────────────────────────────────────────────────────────────────────

/// ONE deterministic audit pass: build the gated corpus (EMPTY unlock set), run the five passes,
/// stage new findings via the dedupe rule, record an `audit_runs` row, return the summary.
/// `now_ms` is injected (test determinism); `vault_dir` feeds the vault-wide title walker so a
/// user-authored vault file or entity stub never counts as a broken-link target.
pub fn run_audit_pass(
    db: &Db,
    vault_dir: Option<&Path>,
    now_ms: i64,
    seal_epoch: &AtomicU64,
) -> Result<AuditRunSummary> {
    run_audit_pass_at_background_epoch(db, vault_dir, now_ms, seal_epoch, None)
}

fn run_audit_pass_at_background_epoch(
    db: &Db,
    vault_dir: Option<&Path>,
    now_ms: i64,
    seal_epoch: &AtomicU64,
    background_epoch: Option<u64>,
) -> Result<AuditRunSummary> {
    // Snapshot BEFORE any read — every finding insert below re-checks against this (the
    // consolidation-pass TOCTOU discipline, see the module doc).
    let epoch_at_start = seal_epoch.load(Ordering::SeqCst);
    let seal_interleaved = || seal_epoch.load(Ordering::SeqCst) != epoch_at_start;

    let corpus = build_corpus(db)?;

    // The broken-link resolver union: vault file stems (user-authored files + entity stubs) +
    // the gated resolve_wikilink (notes/meetings/org items). Corpus titles resolve through
    // resolve_wikilink anyway; the stem set is the extra "anything already in the vault" leg.
    let vault_titles: HashSet<String> = match vault_dir {
        Some(dir) => match crate::export::obsidian::list_vault_titles(dir) {
            Ok(v) => v.into_iter().collect(),
            Err(e) => {
                tracing::warn!(target: "audit", error = %e, "vault title walk failed; resolving links against the DB only");
                HashSet::new()
            }
        },
        None => HashSet::new(),
    };
    let no_unlocks: HashSet<String> = HashSet::new();

    let mut findings: Vec<NewAuditFinding> = Vec::new();
    findings.extend(broken_link_pass(&corpus, &mut |title: &str| {
        if vault_titles.contains(title) {
            return Ok(true);
        }
        Ok(db.resolve_wikilink(title, &no_unlocks)?.is_some())
    })?);
    let entity_sets = entity_sets_for(db, &corpus)?;
    findings.extend(orphan_pass(&corpus, &entity_sets));
    findings.extend(unlinked_mention_pass(&corpus));
    findings.extend(stale_pass(db, &corpus)?);
    findings.extend(contradiction_pass(db, &corpus)?);

    // ── Write phase, seal-epoch guarded ──
    let run_id = uuid::Uuid::new_v4().to_string();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut findings_new = 0usize;
    for f in &findings {
        // A seal/relock landed since the snapshot ⇒ the corpus read above may include
        // now-sealed content. Stop staging findings (already-staged ones referencing the sealed
        // ids were purged by the seal tx itself). Ids/counts logged only.
        if seal_interleaved() {
            tracing::info!(
                target: "audit",
                staged = findings_new,
                dropped = findings.len() - findings_new,
                "seal epoch advanced mid-pass; remaining findings discarded"
            );
            break;
        }
        let inserted = match background_epoch {
            Some(epoch) => crate::perf::with_current_background_epoch(epoch, || {
                db.insert_audit_finding_if_new(f, &run_id, now_ms)
            })?,
            None => Some(db.insert_audit_finding_if_new(f, &run_id, now_ms)?),
        };
        let Some(inserted) = inserted else {
            break;
        };
        if inserted {
            findings_new += 1;
            *counts.entry(f.kind.clone()).or_default() += 1;
        }
    }
    // End-of-pass reconciliation (the TOCTOU shrink): if the epoch is observed advanced NOW —
    // catching a seal that landed after any insert's own pre-check — withdraw everything this
    // run staged (a stale row inserted after the seal's purge tx ran would otherwise survive).
    let (findings_new, counts) =
        reconcile_run_on_epoch_advance(db, &run_id, seal_interleaved(), findings_new, counts)?;

    // The run row is content-free (id + timestamps + per-kind counts) — safe to record even on
    // an aborted pass.
    let counts_json = serde_json::to_string(&counts)
        .map_err(|e| crate::error::AppError::Storage(format!("counts serialize failed: {e}")))?;
    match background_epoch {
        Some(epoch) => {
            let _ = crate::perf::with_current_background_epoch(epoch, || {
                db.insert_audit_run(&run_id, now_ms, now_ms, &counts_json)
            })?;
        }
        None => db.insert_audit_run(&run_id, now_ms, now_ms, &counts_json)?,
    }
    let findings_total_pending = db.count_pending_audit_findings()?;
    tracing::info!(
        target: "audit",
        run_id = %run_id,
        corpus = corpus.len(),
        new = findings_new,
        pending = findings_total_pending,
        "vault audit pass complete"
    );
    Ok(AuditRunSummary {
        run_id,
        started_at: now_ms,
        finished_at: now_ms,
        findings_new,
        findings_total_pending,
        counts,
        // The judge tier runs AFTER the deterministic pass (see `judge_run_findings`); the
        // caller folds its stats in.
        judged: 0,
        demoted: 0,
    })
}

/// End-of-pass seal-epoch reconciliation: when `advanced` (the epoch moved at ANY point during
/// the pass), every PENDING row this run staged is withdrawn in one statement and the pass
/// reports zero — closing the residual window where an insert lands AFTER the interleaving
/// seal's own purge tx already ran (that purge can't see a row inserted later). The next manual
/// run re-derives from the post-seal visible corpus. Content-free logging.
pub(crate) fn reconcile_run_on_epoch_advance(
    db: &Db,
    run_id: &str,
    advanced: bool,
    findings_new: usize,
    counts: BTreeMap<String, usize>,
) -> Result<(usize, BTreeMap<String, usize>)> {
    if !advanced {
        return Ok((findings_new, counts));
    }
    let withdrawn = db.delete_pending_audit_findings_for_run(run_id)?;
    tracing::info!(
        target: "audit",
        run_id = %run_id,
        withdrawn,
        "seal epoch advanced during the pass; the run's staged findings were withdrawn"
    );
    Ok((0, BTreeMap::new()))
}

// ── Phase 3: the weekly schedule ──────────────────────────────────────────────────────────────

/// One week in milliseconds — the scheduled-audit cadence.
pub const WEEKLY_AUDIT_INTERVAL_MS: i64 = 7 * 24 * 60 * 60 * 1_000;

/// Is a scheduled audit pass due? PURE (now + the last SCHEDULED run's `finished_at` are
/// injected). Due when enabled AND (never scheduled-ran, OR a full week has elapsed since the
/// last scheduled run — `>=`, the brief runner's catch-up semantics: a late tick still fires).
/// A claim row inserted before a pass counts as "ran", so a crashed/failed pass holds the week
/// (claim-before-run — no hourly retry storm).
pub fn weekly_due(now_ms: i64, last_scheduled_finished_at: Option<i64>, enabled: bool) -> bool {
    if !enabled {
        return false;
    }
    match last_scheduled_finished_at {
        None => true,
        Some(last) => now_ms.saturating_sub(last) >= WEEKLY_AUDIT_INTERVAL_MS,
    }
}

/// ONE hourly due-check (called from the `lib.rs` consolidation-cadence loop — the first check is
/// a full interval after launch, so run-on-launch-if-due comes free at +1h). Discipline:
/// - re-reads the LIVE flag + last-scheduled-run from `AppState` each tick;
/// - skips WITHOUT claiming on thermal ≥ Serious or a refusing RAM gate (retries next hour —
///   the deterministic pass is always allowed once the gates pass);
/// - CLAIMS the week (inserts the `scheduled = 1` run row) BEFORE the pass, so a crash/failure
///   cannot re-fire until next week;
/// - runs the SAME gated deterministic pass as the manual command (EMPTY unlock set inside
///   `run_audit_pass`), then the judge tier, then emits the count-only
///   [`crate::events::EVENT_AUDIT_UPDATED`] exactly like the manual command.
///
/// NEVER panics; every failure is a warn (ids/counts only — no PII).
pub async fn audit_weekly_tick(handle: &tauri::AppHandle) {
    use tauri::Manager;
    let Some(state) = handle.try_state::<crate::state::AppState>() else {
        return; // init failed — nothing to audit.
    };
    let background_epoch = crate::perf::background_epoch();
    if !crate::perf::background_epoch_is_current(background_epoch) {
        return;
    }
    let enabled = state
        .config
        .lock()
        .map(|c| c.vault_audit_weekly_enabled)
        .unwrap_or(false); // poisoned config ⇒ skip this tick (fail quiet, retry next hour).
    let now_ms = chrono::Utc::now().timestamp_millis();
    let last = match state.db.last_scheduled_audit_run_finished_at() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "audit", error = %e, "weekly tick: schedule read failed");
            return;
        }
    };
    if !weekly_due(now_ms, last, enabled) {
        return;
    }
    // Gates — checked BEFORE the claim so a hot/starved machine retries next hour instead of
    // burning its weekly slot on a skip.
    if crate::thermal::read_thermal_level() >= crate::thermal::ThermalLevel::Serious {
        tracing::info!(target: "audit", "weekly audit skipped: thermal pressure (retrying next hour)");
        return;
    }
    if !crate::transcribe::model::topic_backfill_ram_permits_now() {
        tracing::info!(target: "audit", "weekly audit skipped: low system RAM (retrying next hour)");
        return;
    }
    // CLAIM the week before running — a pass that crashes below cannot storm.
    let claim_id = uuid::Uuid::new_v4().to_string();
    match crate::perf::with_current_background_epoch(background_epoch, || {
        state.db.insert_scheduled_audit_run_claim(&claim_id, now_ms)
    }) {
        Ok(Some(())) => {}
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(target: "audit", error = %e, "weekly tick: claim insert failed; skipping");
            return;
        }
    }
    // The deterministic pass on a blocking worker (the manual command's exact shape).
    let tick_handle = handle.clone();
    let joined = tokio::task::spawn_blocking(move || {
        let state = tick_handle.state::<crate::state::AppState>();
        let vault = state
            .config
            .lock()
            .ok()
            .and_then(|c| c.vault_path.clone())
            .filter(|p| !p.is_empty());
        run_audit_pass_at_background_epoch(
            &state.db,
            vault.as_deref().map(Path::new),
            chrono::Utc::now().timestamp_millis(),
            &state.seal_epoch,
            Some(background_epoch),
        )
    })
    .await;
    let summary = match joined {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            tracing::warn!(target: "audit", error = %e, "weekly audit pass failed; next attempt next week");
            return;
        }
        Err(e) => {
            tracing::warn!(target: "audit", error = %e, "weekly audit task join failed; next attempt next week");
            return;
        }
    };
    // Deterministic DB/string work never pretends to be model residency: Record is free to start
    // while it runs. If that happened, stop here. Individual inserts were already atomic against
    // Start; do not perform a compensating DB write after recording has priority. Any rows committed
    // before Start remain ordinary pending findings and can be judged by a later explicit pass.
    if !crate::perf::background_epoch_is_current(background_epoch) {
        return;
    }
    let stats =
        judge_run_findings_at_epoch(state.inner(), &summary.run_id, Some(background_epoch)).await;
    let pending = state.db.count_pending_audit_findings().unwrap_or(0);
    crate::events::emit_audit_updated(handle, pending as u32);
    tracing::info!(
        target: "audit",
        run_id = %summary.run_id,
        new = summary.findings_new,
        judged = stats.judged,
        demoted = stats.demoted,
        pending,
        "weekly vault audit complete"
    );
}

// ── Phase 3: the LOCAL judge tier (noise demoter — NEVER cloud) ───────────────────────────────

/// Hard wall-clock budget for ONE whole judge stage (all findings together) — the rerank.rs
/// budget pattern, sized up because a judged finding is a full evidence block rather than a
/// snippet, and the stage runs at most weekly (plus on manual runs).
pub const JUDGE_STAGE_BUDGET_MS: u64 = 10_000;

/// Hard decode cap per pointwise judge call — the answer is a one-key JSON bool.
const JUDGE_MAX_TOKENS: usize = 32;

/// The finding kinds the judge may score. Deliberately ONLY the two heuristic-noise-prone kinds:
/// broken links / orphans / unlinked mentions are exact string/graph facts a model cannot
/// out-judge.
const JUDGED_KINDS: [&str; 2] = ["contradiction", "stale"];

/// Judge-stage outcome (counts only — the observability AND the summary fields).
#[derive(Debug, Default, Clone, Copy)]
pub struct JudgeStats {
    /// Findings whose judge call returned a parseable verdict.
    pub judged: usize,
    /// Findings deleted as noise (`keep = false`).
    pub demoted: usize,
}

/// The pointwise judge core (SYNC — runs on the blocking pool under the heavy-inference permit).
/// For each row: one strict tiny-JSON `{"keep": bool}` call on the LOCAL reasoner, deadline-
/// checked BETWEEN rows, each call bounded by [`JUDGE_MAX_TOKENS`] + the remaining wall-clock
/// budget (the rerank.rs pattern). The prompt carries ONLY the finding's own `kind` +
/// `evidence_md` — already visible-by-construction (staged by the gated pass, purged on seal) —
/// and reaches ONLY the on-device model (zero egress by construction; the orchestrator resolves
/// `ReasonerCell::light`, local-or-stub, never cloud).
///
/// LOSS-SAFE degrade contract (the rerank posture): any error, malformed reply, or deadline
/// expiry KEEPS the finding. `keep = false` DELETES the pending row outright — NOT dismiss, whose
/// `dedupe_key` suppression would permanently silence a real issue; a deleted finding re-stages
/// on the next pass if the evidence recurs. A row resolved/purged since the read is left alone
/// (`delete_pending_audit_finding` is pending-only). Logs counts only.
#[cfg(test)]
pub(crate) fn judge_findings_sync(
    db: &Db,
    reasoner: &dyn crate::reason::LocalReasoner,
    rows: &[AuditFindingRow],
    budget_ms: u64,
) -> JudgeStats {
    judge_findings_sync_guarded(db, reasoner, rows, budget_ms, None)
}

fn judge_findings_sync_guarded(
    db: &Db,
    reasoner: &dyn crate::reason::LocalReasoner,
    rows: &[AuditFindingRow],
    budget_ms: u64,
    background_epoch: Option<u64>,
) -> JudgeStats {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_millis(budget_ms);
    // Tiny schema (< 512 B serialized) — the strict-JSON pointwise shape.
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "keep": { "type": "boolean" } },
        "required": ["keep"]
    });
    let system = "You review automated vault-audit findings for noise. Reply ONLY with JSON: \
                  {\"keep\": true} if the finding flags a real, useful issue worth showing the \
                  user, {\"keep\": false} if it is noise (trivial, self-evident, or not \
                  actionable).";

    let mut stats = JudgeStats::default();
    for row in rows {
        let now = Instant::now();
        if now >= deadline {
            break; // out of budget — the rest are kept (degrade toward keeping everything).
        }
        let user = format!(
            "Finding kind: {}\n\nEvidence:\n{}",
            row.kind, row.evidence_md
        );
        let opts = crate::reason::GenOptions {
            max_tokens: Some(JUDGE_MAX_TOKENS),
            temperature: Some(0.0),
            enable_thinking: false,
            timeout: Some(deadline - now),
            ..crate::reason::GenOptions::default()
        };
        match reasoner.structured_with(system, &user, &schema, opts) {
            Ok(v) => {
                if background_epoch
                    .is_some_and(|epoch| !crate::perf::background_epoch_is_current(epoch))
                {
                    return JudgeStats::default();
                }
                let Some(keep) = v.get("keep").and_then(|b| b.as_bool()) else {
                    continue; // malformed shape ⇒ keep (uncounted).
                };
                stats.judged += 1;
                if !keep {
                    let deleted = match background_epoch {
                        Some(epoch) => crate::perf::with_current_background_epoch(epoch, || {
                            db.delete_pending_audit_finding(&row.id)
                        }),
                        None => db.delete_pending_audit_finding(&row.id).map(Some),
                    };
                    match deleted {
                        Ok(Some(true)) => stats.demoted += 1,
                        Ok(Some(false)) | Ok(None) => {}
                        Err(e) => {
                            tracing::warn!(target: "audit", error = %e, "judge demote delete failed; keeping")
                        }
                    }
                }
            }
            Err(_) => {
                // Any reasoner failure ⇒ keep. No PII in logs — the counts below are the
                // observability (the rerank.rs posture).
            }
        }
    }
    tracing::info!(
        target: "audit",
        candidates = rows.len(),
        judged = stats.judged,
        demoted = stats.demoted,
        "audit judge stage complete"
    );
    stats
}

/// Judge THIS run's newly staged `contradiction`/`stale` findings (each finding is judged exactly
/// once, at staging time — survivors are not re-judged by later passes). Resolves the LIGHT local
/// engine (local-or-stub, NEVER cloud — the consolidation-job discipline) and SKIPS on the stub;
/// takes the ONE global heavy-inference permit for the whole stage. Degrades to keep-everything
/// on any failure — this function never errors.
pub async fn judge_run_findings(state: &crate::state::AppState, run_id: &str) -> JudgeStats {
    judge_run_findings_at_epoch(state, run_id, None).await
}

async fn judge_run_findings_at_epoch(
    state: &crate::state::AppState,
    run_id: &str,
    background_epoch: Option<u64>,
) -> JudgeStats {
    if background_epoch.is_some_and(|epoch| !crate::perf::background_epoch_is_current(epoch)) {
        return JudgeStats::default();
    }
    let reasoner = state.reasoner.light();
    if reasoner.id() == "stub" {
        tracing::debug!(target: "audit", "no local light model; skipping the audit judge stage");
        return JudgeStats::default();
    }
    let rows: Vec<AuditFindingRow> = match state.db.list_audit_finding_rows("pending") {
        Ok(rows) => rows
            .into_iter()
            .filter(|r| r.run_id == run_id && JUDGED_KINDS.contains(&r.kind.as_str()))
            .collect(),
        Err(e) => {
            tracing::warn!(target: "audit", error = %e, "judge stage: pending read failed; keeping everything");
            return JudgeStats::default();
        }
    };
    if rows.is_empty() {
        return JudgeStats::default();
    }
    let db = state.db.clone();
    match crate::perf::run_blocking_serialized(&state.heavy_inference, move || {
        Ok(judge_findings_sync_guarded(
            &db,
            reasoner.as_ref(),
            &rows,
            JUDGE_STAGE_BUDGET_MS,
            background_epoch,
        ))
    })
    .await
    {
        Ok(stats) => stats,
        Err(e) => {
            tracing::warn!(target: "audit", error = %e, "judge stage failed; keeping everything");
            JudgeStats::default()
        }
    }
}

// ── Phase 3: the explain prompt (user-initiated; the PROVIDER seam owns consent/redaction) ─────

/// Char cap on the gated source-note excerpt the explanation prompt carries (a bounded snippet,
/// not the whole note — the brief runner's budget posture, sized for one note).
pub(crate) const EXPLAIN_SNIPPET_CHARS: usize = 4_000;

/// Compose the (system, user) prompt for one finding explanation. PURE. `source_snippet` is the
/// caller's ALREADY-GATED, already-capped excerpt of the source note (current session unlock
/// set); everything else comes off the pending row itself (pending rows hold the derived
/// plaintext by design).
pub(crate) fn build_explain_prompt(
    row: &AuditFindingRow,
    source_snippet: &str,
) -> (String, String) {
    let system = "You are Murmur's vault-health assistant. Explain ONE automated audit finding \
                  to the user: what was detected, why it matters for their notes, and what \
                  accepting the suggested action would do. Reply in concise markdown (a short \
                  paragraph, optionally a few bullets). Ground every statement in the provided \
                  evidence — do not invent content."
        .to_string();
    let mut user = format!(
        "Finding kind: {}\nSource note: {}\n",
        row.kind, row.source_title
    );
    if let Some(t) = row.target_title.as_deref().filter(|t| !t.is_empty()) {
        user.push_str(&format!("Related note: {t}\n"));
    }
    user.push_str(&format!("\nEvidence:\n{}\n", row.evidence_md));
    if !row.accept_action.is_empty() {
        user.push_str(&format!(
            "\nSuggested action on accept: {}\n",
            row.accept_action
        ));
    }
    if !source_snippet.trim().is_empty() {
        user.push_str(&format!(
            "\nSource note excerpt (may be truncated):\n{source_snippet}\n"
        ));
    }
    (system, user)
}

/// The gated corpus: every VISIBLE meeting note + standalone note, read with the EMPTY unlock
/// set (sealed-and-not-session-unlocked content never enters the audit — see the module doc).
fn build_corpus(db: &Db) -> Result<Vec<CorpusDoc>> {
    let no_unlocks: HashSet<String> = HashSet::new();
    let mut corpus = Vec::new();
    for m in db.list_meetings_visible(AUDIT_MAX_MEETINGS, &no_unlocks, None)? {
        let Some(note) = db.get_note_if_visible(&m.id, &no_unlocks)? else {
            continue;
        };
        if note.markdown.trim().is_empty() {
            continue;
        }
        corpus.push(CorpusDoc {
            kind: "meeting",
            id: m.id.clone(),
            title: m.title.clone().unwrap_or_default().trim().to_string(),
            body: note.markdown,
        });
    }
    for s in db.list_notes_visible(None, &no_unlocks)? {
        let Some(body) = db.note_markdown_if_visible(&s.id, &no_unlocks)? else {
            continue;
        };
        if body.trim().is_empty() {
            continue;
        }
        corpus.push(CorpusDoc {
            kind: "note",
            id: s.id,
            title: s.title.trim().to_string(),
            body,
        });
    }
    Ok(corpus)
}

/// Entity-mention sets per VISIBLE corpus MEETING (the orphan pass's Jaccard substrate). Keyed
/// on ids already gated into the corpus; the read itself is scoped to exactly those ids.
fn entity_sets_for(db: &Db, corpus: &[CorpusDoc]) -> Result<HashMap<String, HashSet<String>>> {
    let mut out = HashMap::new();
    for doc in corpus.iter().filter(|d| d.kind == "meeting") {
        let ids = db.entity_ids_for_meeting(&doc.id)?;
        if !ids.is_empty() {
            out.insert(doc.id.clone(), ids.into_iter().collect());
        }
    }
    Ok(out)
}

// ── Pass 1: broken links ─────────────────────────────────────────────────────────────────────

/// Every `[[wikilink]]` in a visible body that resolves NOWHERE (not a vault file stem, not a
/// visible note/meeting/org item). Evidence quotes ONLY the link line.
fn broken_link_pass(
    corpus: &[CorpusDoc],
    resolves: &mut dyn FnMut(&str) -> Result<bool>,
) -> Result<Vec<NewAuditFinding>> {
    let mut out = Vec::new();
    for doc in corpus {
        for title in crate::storage::db::extract_wikilink_titles(&doc.body) {
            if resolves(&title)? {
                continue;
            }
            let line = doc
                .body
                .lines()
                .find(|l| l.contains("[[") && l.contains(title.as_str()))
                .unwrap_or("")
                .trim();
            out.push(NewAuditFinding {
                kind: "broken_link".into(),
                source_kind: doc.kind.into(),
                source_id: doc.id.clone(),
                source_title: doc.title.clone(),
                target_title: Some(title.clone()),
                target_id: None,
                target_kind: None,
                evidence_md: format!("> {line}"),
                accept_action: "Append a [!broken-link] note under ## Audit".into(),
                dedupe_key: format!("broken_link|{}|{}", doc.id, dedupe_disc(&title)),
            });
        }
    }
    Ok(out)
}

// ── Pass 2: orphans ──────────────────────────────────────────────────────────────────────────

/// Notes with ZERO wikilinks out (none at all — even an unresolvable link is an outgoing edge;
/// flagging it is the broken-link pass's job) and ZERO corpus notes linking in. Reconnection
/// suggestions, deterministic: (i) other visible notes that MENTION this note's exact title
/// unlinked, then (ii) meeting-to-meeting entity overlap (Jaccard, ties broken by title then id).
fn orphan_pass(
    corpus: &[CorpusDoc],
    entity_sets: &HashMap<String, HashSet<String>>,
) -> Vec<NewAuditFinding> {
    // title → corpus indices (visible titles only; collisions keep all).
    let mut by_title: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, doc) in corpus.iter().enumerate() {
        if !doc.title.is_empty() {
            by_title.entry(doc.title.as_str()).or_default().push(i);
        }
    }
    let mut has_out = vec![false; corpus.len()];
    let mut has_in = vec![false; corpus.len()];
    for (i, doc) in corpus.iter().enumerate() {
        for t in crate::storage::db::extract_wikilink_titles(&doc.body) {
            has_out[i] = true; // any wikilink at all is an outgoing edge.
            if let Some(targets) = by_title.get(t.as_str()) {
                for &j in targets {
                    if j != i {
                        has_in[j] = true;
                    }
                }
            }
        }
    }

    let mut out = Vec::new();
    for (i, doc) in corpus.iter().enumerate() {
        if has_out[i] || has_in[i] {
            continue;
        }
        let mut suggestions: Vec<String> = Vec::new();
        // (i) other notes that mention this title (unlinked) — linking FROM them reconnects.
        if doc.title.chars().count() >= MIN_MENTION_TITLE_CHARS {
            for (j, other) in corpus.iter().enumerate() {
                if j == i || suggestions.len() >= MAX_ORPHAN_SUGGESTIONS {
                    continue;
                }
                if other.title.is_empty()
                    || crate::storage::db::is_untitled_title(&other.title)
                    || suggestions.contains(&other.title)
                {
                    continue; // never suggest the shared "Untitled" sentinel as a reconnection.
                }
                if find_unlinked_mention_line(&other.body, &doc.title).is_some() {
                    suggestions.push(other.title.clone());
                }
            }
        }
        // (ii) entity overlap (meetings only): Jaccard over entity_mentions sets, top-down.
        if suggestions.len() < MAX_ORPHAN_SUGGESTIONS && doc.kind == "meeting" {
            if let Some(mine) = entity_sets.get(&doc.id) {
                let mut scored: Vec<(f64, &CorpusDoc)> = corpus
                    .iter()
                    .enumerate()
                    .filter(|(j, o)| *j != i && o.kind == "meeting" && !o.title.is_empty())
                    .filter_map(|(_, o)| {
                        let theirs = entity_sets.get(&o.id)?;
                        let inter = mine.intersection(theirs).count();
                        if inter == 0 {
                            return None;
                        }
                        let union = mine.union(theirs).count();
                        Some((inter as f64 / union as f64, o))
                    })
                    .collect();
                scored.sort_by(|a, b| {
                    b.0.partial_cmp(&a.0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.1.title.cmp(&b.1.title))
                        .then_with(|| a.1.id.cmp(&b.1.id))
                });
                for (_, o) in scored {
                    if suggestions.len() >= MAX_ORPHAN_SUGGESTIONS {
                        break;
                    }
                    if o.title != doc.title && !suggestions.contains(&o.title) {
                        suggestions.push(o.title.clone());
                    }
                }
            }
        }
        // Evidence deliberately writes suggestion titles as [[links]] and NOTHING ELSE as a
        // wikilink — the accept path re-extracts them via `extract_wikilink_titles`.
        let (evidence_md, accept_action) = if suggestions.is_empty() {
            (
                "This note has no wikilinks in or out.".to_string(),
                String::new(), // dismiss-only.
            )
        } else {
            (
                format!(
                    "This note has no wikilinks in or out.\n\nSuggested connections: {}",
                    suggestions
                        .iter()
                        .map(|s| format!("[[{s}]]"))
                        .collect::<Vec<_>>()
                        .join(" · ")
                ),
                "Append suggested [[links]] under ## Audit".to_string(),
            )
        };
        out.push(NewAuditFinding {
            kind: "orphan".into(),
            source_kind: doc.kind.into(),
            source_id: doc.id.clone(),
            source_title: doc.title.clone(),
            target_title: None,
            target_id: None,
            target_kind: None,
            evidence_md,
            accept_action,
            dedupe_key: format!("orphan|{}|", doc.id),
        });
    }
    out
}

// ── Pass 3: unlinked mentions ────────────────────────────────────────────────────────────────

/// A visible body contains another visible note's EXACT title (≥6 chars, word-boundary, not
/// already inside a `[[..]]`, not inside a code fence) — suggest the `[[link]]`. Capped at 3
/// per source note.
fn unlinked_mention_pass(corpus: &[CorpusDoc]) -> Vec<NewAuditFinding> {
    let mut out = Vec::new();
    for src in corpus {
        let existing: HashSet<String> = crate::storage::db::extract_wikilink_titles(&src.body)
            .into_iter()
            .collect();
        let mut per_source = 0usize;
        for target in corpus {
            if per_source >= MAX_MENTIONS_PER_NOTE {
                break;
            }
            if target.id == src.id && target.kind == src.kind {
                continue;
            }
            let t = target.title.as_str();
            if t.chars().count() < MIN_MENTION_TITLE_CHARS || t == src.title {
                continue;
            }
            if existing.contains(t) {
                continue; // already linked somewhere in this body.
            }
            let Some(line) = find_unlinked_mention_line(&src.body, t) else {
                continue;
            };
            out.push(NewAuditFinding {
                kind: "unlinked_mention".into(),
                source_kind: src.kind.into(),
                source_id: src.id.clone(),
                source_title: src.title.clone(),
                target_title: Some(t.to_string()),
                target_id: Some(target.id.clone()),
                target_kind: Some(target.kind.to_string()),
                evidence_md: format!("> {}\n\nSuggested link: [[{t}]]", line.trim()),
                accept_action: "Append the suggested [[link]] under ## Audit".into(),
                dedupe_key: format!("unlinked_mention|{}|{}", src.id, dedupe_disc(t)),
            });
            per_source += 1;
        }
    }
    out
}

/// Title-free dedupe discriminator: the first 16 hex chars of sha256(text). A `dedupe_key`
/// OUTLIVES resolve (a dismissed row keeps its key forever, that's the suppression), so it must
/// carry ZERO title/content material at rest — the variable part of every key is hashed.
/// Deterministic across runs, so dismissed suppression still matches the regenerated key.
fn dedupe_disc(text: &str) -> String {
    crate::export::note_content_hash(text)[..16].to_string()
}

/// The deterministic mention matcher: the first body LINE carrying `title` as an exact,
/// word-bounded, un-linked occurrence outside any ``` code fence. PURE.
pub(crate) fn find_unlinked_mention_line<'a>(body: &'a str, title: &str) -> Option<&'a str> {
    if title.chars().count() < MIN_MENTION_TITLE_CHARS {
        return None;
    }
    // The never-named "Untitled" sentinel is not a unique title (a vault has many), so it must never
    // be suggested as a mention/link target. Both the `unlinked_mention_pass` target scan and the
    // `orphan_pass` reconnection scan route through here, so one guard covers both.
    // See `crate::storage::db::UNTITLED_TITLE`.
    if crate::storage::db::is_untitled_title(title) {
        return None;
    }
    let mut in_fence = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        for (idx, _) in line.match_indices(title) {
            // Word boundary on both sides (a title embedded in a longer word is not a mention).
            let before_ok = line[..idx]
                .chars()
                .next_back()
                .map(|c| !c.is_alphanumeric())
                .unwrap_or(true);
            let after_ok = line[idx + title.len()..]
                .chars()
                .next()
                .map(|c| !c.is_alphanumeric())
                .unwrap_or(true);
            if !before_ok || !after_ok {
                continue;
            }
            // Not already inside a [[..]]: an unclosed "[[" before the occurrence means the
            // occurrence is the link target (or its alias) itself.
            let prefix = &line[..idx];
            let last_open = prefix.rfind("[[");
            let last_close = prefix.rfind("]]");
            let inside_link = match (last_open, last_close) {
                (Some(o), Some(c)) => o > c,
                (Some(_), None) => true,
                _ => false,
            };
            if inside_link {
                continue;
            }
            // Not inside an INLINE `code span` either (adversarial LOW — fences alone missed
            // these): an ODD number of backticks before the occurrence means an open span.
            if prefix.matches('`').count() % 2 == 1 {
                continue;
            }
            return Some(line);
        }
    }
    None
}

// ── Pass 4: stale notes ──────────────────────────────────────────────────────────────────────

/// A meeting note most of whose facts have been superseded by later meetings. Attribution is the
/// `facts.meeting_id` anchor (verified: every fact row carries the source meeting; sealed
/// meetings' facts are additionally purged on seal). Flagged when total ≥ 3 AND closed/total ≥
/// 0.5. Superseding source titles are resolved through GATED reads — a not-visible superseding
/// source appears as "a later meeting", never its title.
fn stale_pass(db: &Db, corpus: &[CorpusDoc]) -> Result<Vec<NewAuditFinding>> {
    let no_unlocks: HashSet<String> = HashSet::new();
    // Open facts (visible sources only), indexed by the reconcile identity key.
    let open = db.list_open_facts_visible(&no_unlocks)?;
    let mut open_by_key: HashMap<(String, String, String), Vec<&crate::facts::Fact>> =
        HashMap::new();
    for f in &open {
        open_by_key
            .entry((
                f.entity_id.clone(),
                crate::facts::norm(&f.subject),
                crate::facts::norm(&f.predicate),
            ))
            .or_default()
            .push(f);
    }

    let mut out = Vec::new();
    for doc in corpus.iter().filter(|d| d.kind == "meeting") {
        let facts = db.fact_rows_for_meeting_visible(&doc.id, &no_unlocks)?;
        let total = facts.len();
        if total < STALE_MIN_FACTS {
            continue;
        }
        let closed: Vec<_> = facts.iter().filter(|f| f.valid_to.is_some()).collect();
        if closed.len() * 2 < total {
            continue;
        }
        // Up to 2 superseding sources: the OPEN fact sharing each closed fact's identity key,
        // sourced in a DIFFERENT meeting. Titles only through the gate.
        let mut labels: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        'outer: for cf in &closed {
            let key = (
                cf.entity_id.clone(),
                crate::facts::norm(&cf.subject),
                crate::facts::norm(&cf.predicate),
            );
            if let Some(opens) = open_by_key.get(&key) {
                for of in opens {
                    let Some(mid) = &of.meeting_id else { continue };
                    if mid == &doc.id || seen.contains(mid) {
                        continue;
                    }
                    seen.insert(mid.clone());
                    let label = if db.meeting_is_visible(mid, &no_unlocks)? {
                        db.get_meeting(mid)?
                            .and_then(|m| m.title)
                            .map(|t| t.trim().to_string())
                            .filter(|t| !t.is_empty())
                            .map(|t| format!("[[{t}]]"))
                            .unwrap_or_else(|| "a later meeting".to_string())
                    } else {
                        "a later meeting".to_string()
                    };
                    labels.push(label);
                    if labels.len() >= 2 {
                        break 'outer;
                    }
                }
            }
        }
        let see = if labels.is_empty() {
            String::new()
        } else {
            format!(" — see {}", labels.join(" and "))
        };
        out.push(NewAuditFinding {
            kind: "stale".into(),
            source_kind: "meeting".into(),
            source_id: doc.id.clone(),
            source_title: doc.title.clone(),
            target_title: None,
            target_id: None,
            target_kind: None,
            evidence_md: format!(
                "> {} of {} facts recorded in this note have since been superseded{see}",
                closed.len(),
                total
            ),
            accept_action: "Append a [!stale] callout under ## Audit".into(),
            dedupe_key: format!("stale|{}|", doc.id),
        });
    }
    Ok(out)
}

// ── Pass 5: contradictions ───────────────────────────────────────────────────────────────────

/// Two OPEN facts with the same reconcile identity key (entity + normalized subject+predicate),
/// DIFFERENT objects, from DIFFERENT source meetings — the open-open collisions
/// [`crate::facts::reconcile_facts`] never saw in one batch. Both sources must be IN THE CORPUS
/// (visible + carrying a note — accept stamps both notes). A pair already staged in the Re-Truth
/// supersession review queue (pending, same normalized predicate, same two meetings) is skipped —
/// one review surface per conflict.
fn contradiction_pass(db: &Db, corpus: &[CorpusDoc]) -> Result<Vec<NewAuditFinding>> {
    let no_unlocks: HashSet<String> = HashSet::new();
    let corpus_meetings: HashMap<&str, &CorpusDoc> = corpus
        .iter()
        .filter(|d| d.kind == "meeting")
        .map(|d| (d.id.as_str(), d))
        .collect();
    let open = db.list_open_facts_visible(&no_unlocks)?;
    let mut by_key: BTreeMap<(String, String, String), Vec<&crate::facts::Fact>> = BTreeMap::new();
    for f in &open {
        by_key
            .entry((
                f.entity_id.clone(),
                crate::facts::norm(&f.subject),
                crate::facts::norm(&f.predicate),
            ))
            .or_default()
            .push(f);
    }

    let mut out = Vec::new();
    for ((entity_id, _nsubj, npred), facts) in &by_key {
        if facts.len() < 2 {
            continue;
        }
        // Deterministic order: oldest valid_from first, then id.
        let mut facts = facts.clone();
        facts.sort_by(|a, b| {
            a.valid_from
                .cmp(&b.valid_from)
                .then_with(|| a.id.cmp(&b.id))
        });
        for i in 0..facts.len() {
            for j in (i + 1)..facts.len() {
                let (a, b) = (facts[i], facts[j]);
                if crate::facts::norm(&a.object) == crate::facts::norm(&b.object) {
                    continue;
                }
                let (Some(mid_a), Some(mid_b)) = (&a.meeting_id, &b.meeting_id) else {
                    continue;
                };
                if mid_a == mid_b {
                    continue; // same source — reconcile's own within-batch problem, not ours.
                }
                let (Some(doc_a), Some(doc_b)) = (
                    corpus_meetings.get(mid_a.as_str()),
                    corpus_meetings.get(mid_b.as_str()),
                ) else {
                    continue; // a source outside the visible corpus contributes nothing.
                };
                if db.pending_supersession_for_pair(npred, mid_a, mid_b)? {
                    continue; // already staged for Re-Truth review — don't double-surface.
                }
                let (min_id, max_id) = if mid_a <= mid_b {
                    (mid_a.as_str(), mid_b.as_str())
                } else {
                    (mid_b.as_str(), mid_a.as_str())
                };
                out.push(NewAuditFinding {
                    kind: "contradiction".into(),
                    source_kind: "meeting".into(),
                    source_id: doc_a.id.clone(),
                    source_title: doc_a.title.clone(),
                    target_title: Some(doc_b.title.clone()),
                    target_id: Some(doc_b.id.clone()),
                    target_kind: Some("meeting".into()),
                    evidence_md: format!(
                        "> **{} · {}** — \"{}\" (this note) vs \"{}\" ([[{}]])",
                        a.subject, a.predicate, a.object, b.object, doc_b.title
                    ),
                    accept_action: "Append [!conflict] callouts cross-linking both sources".into(),
                    // Meeting/entity ids are opaque; the predicate is FACT TEXT — hash the
                    // (entity, predicate) discriminator so the key carries no content material.
                    dedupe_key: format!(
                        "contradiction|{min_id}|{max_id}|{}",
                        dedupe_disc(&format!("{entity_id}|{npred}"))
                    ),
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{FactOp, NewFact};
    use crate::storage::models::{EntityKind, Folder, Meeting, MeetingStatus, NoteRecord};

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn file_db(label: &str) -> Db {
        let p = crate::storage::db::unique_temp_path(&format!("murmur-audit-{label}"), "sqlite");
        Db::open_with_key(&p, TEST_DEK).unwrap()
    }

    fn quiet_epoch() -> AtomicU64 {
        AtomicU64::new(0)
    }

    fn doc(kind: &'static str, id: &str, title: &str, body: &str) -> CorpusDoc {
        CorpusDoc {
            kind,
            id: id.to_string(),
            title: title.to_string(),
            body: body.to_string(),
        }
    }

    fn seed_meeting_note(db: &Db, id: &str, title: &str, note: &str, folder_id: Option<&str>) {
        db.insert_meeting(&Meeting {
            id: id.to_string(),
            started_at: format!("2026-07-0{}T09:00:00Z", (id.len() % 8) + 1),
            ended_at: None,
            title: Some(title.to_string()),
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
            created_at: "2026-07-01T09:05:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_meeting_folder(id, folder_id).unwrap();
    }

    fn make_folder(db: &Db, id: &str, name: &str) {
        db.insert_folder(&Folder {
            id: id.to_string(),
            name: name.to_string(),
            path: name.to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-01T00:00:00Z".to_string(),
        })
        .unwrap();
    }

    fn add_fact(
        db: &Db,
        entity_id: &str,
        subject: &str,
        predicate: &str,
        object: &str,
        at: &str,
        meeting_id: &str,
    ) {
        db.apply_fact_ops(&[FactOp::Add(NewFact {
            entity_id: entity_id.to_string(),
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            valid_from: at.to_string(),
            recorded_at: at.to_string(),
            confidence: 1.0,
            meeting_id: Some(meeting_id.to_string()),
        })])
        .unwrap();
    }

    /// Close (supersede) the open fact matching the key by reconciling a different object.
    fn supersede_fact(
        db: &Db,
        entity_id: &str,
        subject: &str,
        predicate: &str,
        new_object: &str,
        at: &str,
        meeting_id: &str,
    ) {
        let existing = db.list_facts_visible(entity_id, &HashSet::new()).unwrap();
        let mut ops = crate::facts::reconcile_facts(
            &existing,
            &[crate::facts::FactCandidate {
                entity_id: entity_id.to_string(),
                subject: subject.to_string(),
                predicate: predicate.to_string(),
                object: new_object.to_string(),
                confidence: 1.0,
            }],
            at,
        );
        crate::facts::set_meeting_id(&mut ops, meeting_id);
        db.apply_fact_ops(&ops).unwrap();
    }

    /// The PINNED FE seam: the Angular inbox codes against exactly these camelCase keys — a
    /// field rename silently breaks the wire contract.
    #[test]
    fn wire_dtos_serialize_camel_case_per_the_pinned_seam() {
        let summary = AuditRunSummary {
            run_id: "r1".into(),
            started_at: 1,
            finished_at: 2,
            findings_new: 3,
            findings_total_pending: 4,
            counts: [("broken_link".to_string(), 3usize)].into_iter().collect(),
            judged: 2,
            demoted: 1,
        };
        assert_eq!(
            serde_json::to_string(&summary).unwrap(),
            r#"{"runId":"r1","startedAt":1,"finishedAt":2,"findingsNew":3,"findingsTotalPending":4,"counts":{"broken_link":3},"judged":2,"demoted":1}"#
        );
        let schedule = AuditSchedule {
            enabled: true,
            last_run_at: Some(10),
            next_due_at: Some(10 + WEEKLY_AUDIT_INTERVAL_MS),
        };
        assert_eq!(
            serde_json::to_string(&schedule).unwrap(),
            r#"{"enabled":true,"lastRunAt":10,"nextDueAt":604800010}"#
        );
        let explanation = AuditExplanation {
            finding_id: "f1".into(),
            explanation_md: "because".into(),
            provider: "claude_code".into(),
        };
        assert_eq!(
            serde_json::to_string(&explanation).unwrap(),
            r#"{"findingId":"f1","explanationMd":"because","provider":"claude_code"}"#
        );
        let finding = AuditFinding {
            id: "f1".into(),
            kind: "orphan".into(),
            source_kind: "note".into(),
            source_id: "n1".into(),
            source_title: "T".into(),
            target_title: None,
            evidence_md: "e".into(),
            accept_action: "a".into(),
            status: "pending".into(),
            created_at: 1,
            resolved_at: None,
        };
        assert_eq!(
            serde_json::to_string(&finding).unwrap(),
            r#"{"id":"f1","kind":"orphan","sourceKind":"note","sourceId":"n1","sourceTitle":"T","targetTitle":null,"evidenceMd":"e","acceptAction":"a","status":"pending","createdAt":1,"resolvedAt":null}"#
        );
    }

    // ── mention matcher (pure) ──

    #[test]
    fn mention_matcher_negatives_and_positive() {
        // Positive: word-bounded, plain prose.
        assert!(
            find_unlinked_mention_line("We synced on Project Atlas today", "Project Atlas")
                .is_some()
        );
        // Too short a title (< 6 chars).
        assert!(find_unlinked_mention_line("Quick Sync today", "Sync").is_none());
        // Already linked — inside [[..]].
        assert!(
            find_unlinked_mention_line("See [[Project Atlas]] for details", "Project Atlas")
                .is_none()
        );
        // Aliased link — still inside [[..]].
        assert!(find_unlinked_mention_line(
            "See [[Project Atlas|the project]] for details",
            "Project Atlas"
        )
        .is_none());
        // Inside a code fence.
        assert!(find_unlinked_mention_line(
            "```\nProject Atlas config\n```\nnothing else",
            "Project Atlas"
        )
        .is_none());
        // Inside an INLINE code span (adversarial LOW): `Project Atlas` is code, not a mention.
        assert!(
            find_unlinked_mention_line("run `Project Atlas` locally", "Project Atlas").is_none()
        );
        // …but prose AFTER a closed span on the same line still matches.
        assert!(find_unlinked_mention_line(
            "run `the tool` then open Project Atlas",
            "Project Atlas"
        )
        .is_some());
        // Word boundary: embedded in a longer token.
        assert!(find_unlinked_mention_line("xProject Atlasy is not it", "Project Atlas").is_none());
        // A fence CLOSES again — mention after it is found.
        assert!(find_unlinked_mention_line(
            "```\ncode\n```\nProject Atlas resumed",
            "Project Atlas"
        )
        .is_some());
    }

    // ── broken links (pure, injected resolver) ──

    #[test]
    fn broken_link_flags_unresolvable_only_and_quotes_the_link_line() {
        let corpus = vec![doc(
            "meeting",
            "m1",
            "Kickoff",
            "# Kickoff\nintro line\n- see [[Known Note]] for context\n- and [[Ghost Note]] too\n",
        )];
        let known: HashSet<&str> = ["Known Note"].into_iter().collect();
        let found = broken_link_pass(&corpus, &mut |t| Ok(known.contains(t))).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].target_title.as_deref(), Some("Ghost Note"));
        assert_eq!(
            found[0].dedupe_key,
            format!("broken_link|m1|{}", dedupe_disc("Ghost Note")),
            "key = source id + hashed (title-free) target discriminator"
        );
        // Evidence quotes ONLY the link line — not the whole body.
        assert!(found[0].evidence_md.contains("[[Ghost Note]]"));
        assert!(!found[0].evidence_md.contains("intro line"));
    }

    // ── orphan degree + suggestions (pure) ──

    #[test]
    fn orphan_flags_zero_degree_only_and_suggests_mentioners() {
        let corpus = vec![
            // a → links to b: NOT an orphan (out-degree).
            doc("note", "a", "Alpha Note", "body [[Beta Note]]\n"),
            // b: linked FROM a: NOT an orphan (in-degree).
            doc("note", "b", "Beta Note", "no links here\n"),
            // c: no links either way → ORPHAN; "Gamma Note" is mentioned (unlinked) by d.
            doc("note", "c", "Gamma Note", "isolated content\n"),
            // d mentions c's title without linking it.
            doc(
                "note",
                "d",
                "Delta Note",
                "talked about Gamma Note today [[Alpha Note]]\n",
            ),
        ];
        let found = orphan_pass(&corpus, &HashMap::new());
        assert_eq!(found.len(), 1, "only the zero-degree note is an orphan");
        assert_eq!(found[0].source_id, "c");
        assert!(
            found[0].evidence_md.contains("[[Delta Note]]"),
            "the mentioning note is suggested: {}",
            found[0].evidence_md
        );
        assert!(
            !found[0].accept_action.is_empty(),
            "suggestions ⇒ acceptable"
        );
    }

    #[test]
    fn orphan_without_suggestions_is_dismiss_only() {
        let corpus = vec![doc(
            "note",
            "solo",
            "Lonely Note",
            "nothing links anywhere\n",
        )];
        let found = orphan_pass(&corpus, &HashMap::new());
        assert_eq!(found.len(), 1);
        assert!(
            found[0].accept_action.is_empty(),
            "no suggestions ⇒ dismiss-only"
        );
    }

    // ── unlinked mentions (pure) ──

    #[test]
    fn unlinked_mention_caps_at_three_and_skips_linked_and_fenced() {
        let body = "We covered Target One and Target Two and Target Three and Target Four.\n\
                    Also [[Target Five]] is already linked.\n\
                    ```\nTarget Sixxx appears only in code\n```\n";
        let mut corpus = vec![doc("note", "src", "Source Note", body)];
        for (i, t) in [
            "Target One",
            "Target Two",
            "Target Three",
            "Target Four",
            "Target Five",
            "Target Sixxx",
        ]
        .iter()
        .enumerate()
        {
            corpus.push(doc("note", &format!("t{i}"), t, "content\n"));
        }
        let found = unlinked_mention_pass(&corpus);
        let for_src: Vec<_> = found.iter().filter(|f| f.source_id == "src").collect();
        assert_eq!(for_src.len(), 3, "capped at 3 per source note");
        let targets: Vec<_> = for_src
            .iter()
            .map(|f| f.target_title.as_deref().unwrap())
            .collect();
        assert!(
            !targets.contains(&"Target Five"),
            "already-linked title skipped"
        );
        assert!(
            !targets.contains(&"Target Sixxx"),
            "code-fenced mention skipped"
        );
    }

    /// 2026-07-20 — the "Untitled" sentinel must not be a mention TARGET (same title-collision class
    /// as the phantom-mentions panel that #417 fixed for backlinks): a body naming the bare word
    /// "Untitled" must NOT suggest a `[[Untitled]]` link to every never-named note. RED before the
    /// `is_untitled_title` skip in the shared `find_unlinked_mention_line`.
    #[test]
    fn unlinked_mention_pass_ignores_untitled_sentinel_target() {
        let corpus = vec![
            // A real note whose body names the bare word "Untitled" (word-bounded).
            doc(
                "note",
                "src",
                "Notes Hub",
                "I saved it into my Untitled draft earlier.\n",
            ),
            // Two never-named notes sharing the sentinel (empty-body notes are dropped from the
            // corpus, so give them content).
            doc("note", "u1", "Untitled", "scratch one\n"),
            doc("note", "u2", "Untitled", "scratch two\n"),
        ];
        let found = unlinked_mention_pass(&corpus);
        assert!(
            !found
                .iter()
                .any(|f| f.target_title.as_deref() == Some("Untitled")),
            "must not suggest linking to the shared 'Untitled' sentinel; got {:?}",
            found
                .iter()
                .map(|f| f.target_title.clone())
                .collect::<Vec<_>>()
        );
    }

    // ── stale (file DB) ──

    #[test]
    fn stale_flags_majority_superseded_and_cites_gated_titles() {
        let db = file_db("stale");
        seed_meeting_note(&db, "m-old", "Planning", "# Plan\n", None);
        seed_meeting_note(&db, "m-new", "Review", "# Review\n", None);
        let e = db.upsert_entity("Atlas", EntityKind::Project).unwrap();

        // 4 facts from m-old; 2 then superseded from m-new → ratio 0.5, total ≥ 3 → flagged.
        add_fact(
            &db,
            &e,
            "Atlas",
            "status",
            "in-progress",
            "2026-07-01T09:00:00Z",
            "m-old",
        );
        add_fact(
            &db,
            &e,
            "Atlas",
            "owner",
            "Anna",
            "2026-07-01T09:00:00Z",
            "m-old",
        );
        add_fact(
            &db,
            &e,
            "Atlas",
            "deadline",
            "Q3",
            "2026-07-01T09:00:00Z",
            "m-old",
        );
        add_fact(
            &db,
            &e,
            "Atlas",
            "budget",
            "small",
            "2026-07-01T09:00:00Z",
            "m-old",
        );
        supersede_fact(
            &db,
            &e,
            "Atlas",
            "status",
            "shipped",
            "2026-07-05T09:00:00Z",
            "m-new",
        );
        supersede_fact(
            &db,
            &e,
            "Atlas",
            "owner",
            "Bob",
            "2026-07-05T09:00:00Z",
            "m-new",
        );

        let corpus = build_corpus(&db).unwrap();
        let found = stale_pass(&db, &corpus).unwrap();
        assert_eq!(found.len(), 1, "only the majority-superseded note is stale");
        assert_eq!(found[0].source_id, "m-old");
        assert!(
            found[0].evidence_md.contains("2 of 4"),
            "counts cited: {}",
            found[0].evidence_md
        );
        assert!(
            found[0].evidence_md.contains("[[Review]]"),
            "visible superseding title cited: {}",
            found[0].evidence_md
        );
    }

    #[test]
    fn stale_threshold_needs_three_facts_and_half_closed() {
        let db = file_db("stale-thresh");
        seed_meeting_note(&db, "m1", "Tiny", "# T\n", None);
        seed_meeting_note(&db, "m2", "Later", "# L\n", None);
        let e = db.upsert_entity("Beta", EntityKind::Project).unwrap();
        // Only 2 facts (below the ≥3 floor) even though both get superseded.
        add_fact(
            &db,
            &e,
            "Beta",
            "status",
            "alpha",
            "2026-07-01T09:00:00Z",
            "m1",
        );
        add_fact(
            &db,
            &e,
            "Beta",
            "owner",
            "Kim",
            "2026-07-01T09:00:00Z",
            "m1",
        );
        supersede_fact(
            &db,
            &e,
            "Beta",
            "status",
            "beta",
            "2026-07-05T09:00:00Z",
            "m2",
        );
        supersede_fact(
            &db,
            &e,
            "Beta",
            "owner",
            "Lee",
            "2026-07-05T09:00:00Z",
            "m2",
        );
        let corpus = build_corpus(&db).unwrap();
        assert!(
            stale_pass(&db, &corpus).unwrap().is_empty(),
            "below the fact floor"
        );

        // 4 facts, only 1 closed → ratio below 0.5 → not stale.
        let db2 = file_db("stale-ratio");
        seed_meeting_note(&db2, "m1", "Big", "# B\n", None);
        seed_meeting_note(&db2, "m2", "Later", "# L\n", None);
        let e2 = db2.upsert_entity("Gamma", EntityKind::Project).unwrap();
        for (p, o) in [
            ("status", "x"),
            ("owner", "y"),
            ("deadline", "z"),
            ("budget", "w"),
        ] {
            add_fact(&db2, &e2, "Gamma", p, o, "2026-07-01T09:00:00Z", "m1");
        }
        supersede_fact(
            &db2,
            &e2,
            "Gamma",
            "status",
            "x2",
            "2026-07-05T09:00:00Z",
            "m2",
        );
        let corpus2 = build_corpus(&db2).unwrap();
        assert!(
            stale_pass(&db2, &corpus2).unwrap().is_empty(),
            "ratio below half"
        );
    }

    // ── contradiction (file DB) ──

    #[test]
    fn contradiction_open_open_cross_source_only_and_queue_exclusion() {
        let db = file_db("contra");
        seed_meeting_note(&db, "m-a", "Meeting A", "# A\n", None);
        seed_meeting_note(&db, "m-b", "Meeting B", "# B\n", None);
        let e = db.upsert_entity("Atlas", EntityKind::Project).unwrap();

        // Two OPEN facts, same key, different object, different source → candidate.
        add_fact(
            &db,
            &e,
            "Atlas",
            "status",
            "shipped",
            "2026-07-01T09:00:00Z",
            "m-a",
        );
        add_fact(
            &db,
            &e,
            "Atlas",
            "Status",
            "cancelled",
            "2026-07-02T09:00:00Z",
            "m-b",
        );
        // A CLOSED pair on another predicate — excluded (only open-open collide).
        add_fact(
            &db,
            &e,
            "Atlas",
            "owner",
            "Anna",
            "2026-07-01T09:00:00Z",
            "m-a",
        );
        supersede_fact(
            &db,
            &e,
            "Atlas",
            "owner",
            "Bob",
            "2026-07-02T09:00:00Z",
            "m-b",
        );
        // A same-source open duplicate on a third predicate — excluded (same meeting).
        add_fact(
            &db,
            &e,
            "Atlas",
            "budget",
            "small",
            "2026-07-01T09:00:00Z",
            "m-a",
        );
        add_fact(
            &db,
            &e,
            "Atlas",
            "budget",
            "large",
            "2026-07-01T10:00:00Z",
            "m-a",
        );

        let corpus = build_corpus(&db).unwrap();
        let found = contradiction_pass(&db, &corpus).unwrap();
        assert_eq!(
            found.len(),
            1,
            "exactly the open-open cross-source pair: {found:#?}"
        );
        assert_eq!(
            found[0].source_id, "m-a",
            "source = the OLDER fact's meeting"
        );
        assert_eq!(found[0].target_id.as_deref(), Some("m-b"));
        assert!(found[0].evidence_md.contains("shipped"));
        assert!(found[0].evidence_md.contains("cancelled"));
        // Lock review: the key must carry no fact-text material (predicate is hashed).
        assert!(
            !found[0].dedupe_key.contains("status"),
            "contradiction key carries predicate text: {}",
            found[0].dedupe_key
        );

        // The identical pair already PENDING in the Re-Truth queue → excluded.
        db.record_supersessions(&[crate::storage::models::SupersessionRow {
            id: "s1".to_string(),
            superseding_meeting_id: "m-b".to_string(),
            source_meeting_id: "m-a".to_string(),
            entity: "Atlas".to_string(),
            predicate: "status".to_string(),
            old_value: "shipped".to_string(),
            new_value: "cancelled".to_string(),
            created_at: "2026-07-02T09:00:00Z".to_string(),
            applied_at: None,
            source_pre_image: None,
            superseding_pre_image: None,
        }])
        .unwrap();
        let found = contradiction_pass(&db, &corpus).unwrap();
        assert!(
            found.is_empty(),
            "a pair pending Re-Truth review is not re-surfaced"
        );
    }

    // ── the lock gate: sealed content contributes NOTHING ──

    #[test]
    fn sealed_folder_contributes_zero_findings() {
        let db = file_db("sealed");
        make_folder(&db, "f-lock", "Secret");
        // Sealed meeting whose note is FULL of audit bait: a broken link, an orphan shape, a
        // mention of the open note's title, facts.
        seed_meeting_note(
            &db,
            "m-sealed",
            "SECRET-ACQUISITION",
            "plan [[Nonexistent Ghost]] and Open Standup Notes mention\n",
            Some("f-lock"),
        );
        let e = db.upsert_entity("SecretCo", EntityKind::Project).unwrap();
        add_fact(
            &db,
            &e,
            "SecretCo",
            "price",
            "5M",
            "2026-07-01T09:00:00Z",
            "m-sealed",
        );
        // An open meeting (its own broken link keeps the pass productive). The sealed TITLE
        // deliberately exists ONLY on the sealed side — a visible body is always quotable, so
        // the leak assertion below is about the sealed side surfacing, not visible prose.
        seed_meeting_note(
            &db,
            "m-open",
            "Open Standup Notes",
            "regular sync today, [[Nowhere Note]]\n",
            None,
        );
        db.set_folder_locked("f-lock", true, None).unwrap();

        let summary = run_audit_pass(&db, None, 1_700_000_000_000, &quiet_epoch()).unwrap();
        assert!(
            summary.findings_new > 0,
            "the open note still yields findings"
        );
        let rows = db.list_audit_finding_rows("pending").unwrap();
        for r in &rows {
            assert_ne!(r.source_id, "m-sealed", "no finding sources sealed content");
            assert_ne!(r.target_id.as_deref(), Some("m-sealed"));
            let all_text = format!(
                "{} {} {} {}",
                r.source_title,
                r.target_title.clone().unwrap_or_default(),
                r.evidence_md,
                r.accept_action
            );
            assert!(
                !all_text.contains("SECRET-ACQUISITION"),
                "sealed TITLE must not leak into any finding text: {all_text}"
            );
            assert!(
                !all_text.contains("Nonexistent Ghost"),
                "sealed BODY content must not leak into any finding text: {all_text}"
            );
        }
        // The sealed body's mention of the open title produced nothing sourced at the sealed note.
        assert!(
            rows.iter()
                .all(|r| r.kind != "unlinked_mention" || r.source_id == "m-open"),
            "mentions only ever source from visible bodies"
        );
    }

    // ── dedupe / idempotence ──

    #[test]
    fn audit_pass_is_idempotent_on_pending_and_dismissed_but_accepted_recurs() {
        let db = file_db("dedupe");
        seed_meeting_note(&db, "m1", "Kickoff", "see [[Ghost Note]]\n", None);

        let first = run_audit_pass(&db, None, 1_700_000_000_000, &quiet_epoch()).unwrap();
        assert_eq!(
            first.findings_new, 1,
            "the broken link staged once: {first:?}"
        );
        let second = run_audit_pass(&db, None, 1_700_000_000_001, &quiet_epoch()).unwrap();
        assert_eq!(
            second.findings_new, 0,
            "a PENDING twin suppresses re-creation"
        );
        assert_eq!(second.findings_total_pending, 1);

        // DISMISS → still suppressed (don't nag again).
        let row = &db.list_audit_finding_rows("pending").unwrap()[0];
        db.resolve_audit_finding_row(&row.id, "dismissed", 1_700_000_000_002)
            .unwrap();
        let third = run_audit_pass(&db, None, 1_700_000_000_003, &quiet_epoch()).unwrap();
        assert_eq!(
            third.findings_new, 0,
            "a DISMISSED twin suppresses re-creation"
        );

        // ACCEPT (simulated status flip) → the evidence persisting means it MAY recur.
        let row_id = db.list_audit_finding_rows("dismissed").unwrap()[0]
            .id
            .clone();
        db.resolve_audit_finding_row(&row_id, "accepted", 1_700_000_000_004)
            .unwrap();
        let fourth = run_audit_pass(&db, None, 1_700_000_000_005, &quiet_epoch()).unwrap();
        assert_eq!(
            fourth.findings_new, 1,
            "an ACCEPTED twin does NOT suppress recurrence"
        );
    }

    // ── purge-on-seal / purge-on-delete (RED→GREEN both directions) ──

    fn stage_finding(db: &Db, source_id: &str, target_id: Option<&str>, key: &str) {
        db.insert_audit_finding_if_new(
            &NewAuditFinding {
                kind: "broken_link".into(),
                source_kind: "meeting".into(),
                source_id: source_id.to_string(),
                source_title: "T".into(),
                target_title: None,
                target_id: target_id.map(str::to_string),
                target_kind: target_id.map(|_| "meeting".to_string()),
                evidence_md: "> evidence".into(),
                accept_action: "".into(),
                dedupe_key: key.to_string(),
            },
            "run-x",
            1,
        )
        .unwrap();
    }

    /// A SEAL purges ALL pending findings — the memory-rollups posture (adversarial HIGH): a
    /// finding's evidence may cite THIRD-PARTY titles the id-matched purge can never cover, and a
    /// seal anywhere invalidates the pass's visibility snapshot. Resolved rows (blanked) survive.
    #[test]
    fn sealing_purges_all_pending_findings() {
        let db = file_db("purge-seal");
        make_folder(&db, "f1", "Secret");
        seed_meeting_note(&db, "m1", "Sealed soon", "# S\n", Some("f1"));
        stage_finding(&db, "m1", None, "k-source");
        stage_finding(&db, "m-other", Some("m1"), "k-target");
        // NOT id-related to m1 — the rollup posture still purges it on the seal.
        stage_finding(&db, "m-other", None, "k-unrelated");
        // An accepted row referencing m1 SURVIVES (evidence already blanked on accept).
        stage_finding(&db, "m1", None, "k-accepted");
        let accepted_id = db
            .list_audit_finding_rows("pending")
            .unwrap()
            .iter()
            .find(|r| r.dedupe_key == "k-accepted")
            .unwrap()
            .id
            .clone();
        db.resolve_audit_finding_row(&accepted_id, "accepted", 2)
            .unwrap();

        // The SEAL tx (lock_folder's purge leg) drops EVERY pending row.
        db.set_folder_locked("f1", true, None).unwrap();
        let _ = db.purge_chunks_for_meetings(&["m1".to_string()]).unwrap();

        let pending = db.list_audit_finding_rows("pending").unwrap();
        assert!(
            pending.is_empty(),
            "a seal purges ALL pending findings (rollup posture): {pending:#?}"
        );
        assert!(
            db.get_audit_finding(&accepted_id).unwrap().is_some(),
            "an accepted (already-blanked) row survives the seal"
        );
    }

    /// The adversarial HIGH repro, RED-first: an OPEN meeting's stale finding cites
    /// `see [[Review]]` in its evidence with `target_id = None` — sealing Review's folder must
    /// still remove it (nothing in the pending table may reference the sealed title).
    #[test]
    fn sealing_third_party_folder_purges_findings_citing_it() {
        let db = file_db("purge-thirdparty");
        make_folder(&db, "f-r", "ReviewVault");
        seed_meeting_note(&db, "m-old", "Planning", "# Plan\n", None);
        seed_meeting_note(&db, "m-new", "Review", "# Review\n", Some("f-r"));
        let e = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
        add_fact(
            &db,
            &e,
            "Atlas",
            "status",
            "in-progress",
            "2026-07-01T09:00:00Z",
            "m-old",
        );
        add_fact(
            &db,
            &e,
            "Atlas",
            "owner",
            "Anna",
            "2026-07-01T09:00:00Z",
            "m-old",
        );
        add_fact(
            &db,
            &e,
            "Atlas",
            "deadline",
            "Q3",
            "2026-07-01T09:00:00Z",
            "m-old",
        );
        supersede_fact(
            &db,
            &e,
            "Atlas",
            "status",
            "shipped",
            "2026-07-05T09:00:00Z",
            "m-new",
        );
        supersede_fact(
            &db,
            &e,
            "Atlas",
            "owner",
            "Bob",
            "2026-07-05T09:00:00Z",
            "m-new",
        );

        // Stage the REAL stale finding: source = the open m-old, evidence cites [[Review]],
        // target_id None — exactly the shape an id-matched purge can never reach.
        let corpus = build_corpus(&db).unwrap();
        let stale = stale_pass(&db, &corpus).unwrap();
        assert_eq!(stale.len(), 1);
        assert!(
            stale[0].evidence_md.contains("[[Review]]"),
            "precondition: the evidence cites the third-party title: {}",
            stale[0].evidence_md
        );
        assert!(
            stale[0].target_id.is_none(),
            "precondition: no id for the citation"
        );
        assert!(db
            .insert_audit_finding_if_new(&stale[0], "run-3p", 1)
            .unwrap());

        // Seal REVIEW's folder (the lock_folder purge leg for ITS meeting only).
        db.set_folder_locked("f-r", true, None).unwrap();
        let _ = db
            .purge_chunks_for_meetings(&["m-new".to_string()])
            .unwrap();

        let pending = db.list_audit_finding_rows("pending").unwrap();
        assert!(
            pending.iter().all(|r| !format!(
                "{} {} {}",
                r.evidence_md,
                r.source_title,
                r.target_title.clone().unwrap_or_default()
            )
            .contains("Review")),
            "no pending finding may still cite the sealed title: {pending:#?}"
        );
        assert!(
            pending.is_empty(),
            "the citing finding is GONE after the third-party seal: {pending:#?}"
        );
    }

    #[test]
    fn relock_tx_purges_all_pending_findings() {
        let db = file_db("purge-relock");
        make_folder(&db, "f1", "Secret");
        seed_meeting_note(&db, "m1", "Sealed", "# S\n", Some("f1"));
        db.insert_document("d1", "f1", "note-a", "body", "note", 1)
            .unwrap();
        stage_finding(&db, "m1", None, "k-meeting");
        stage_finding(&db, "d1", None, "k-doc");
        stage_finding(&db, "m-else", Some("d1"), "k-doc-target");
        stage_finding(&db, "m-else", None, "k-unrelated");

        let folders: HashSet<String> = ["f1".to_string()].into_iter().collect();
        let _ = db.blank_sealed_notes_in_folders(&folders).unwrap();

        let pending = db.list_audit_finding_rows("pending").unwrap();
        assert!(
            pending.is_empty(),
            "a relock purges ALL pending findings (rollup posture): {pending:#?}"
        );
    }

    #[test]
    fn startup_reconcile_purges_all_pending_findings_when_any_folder_locked() {
        let db = file_db("purge-startup");
        make_folder(&db, "f1", "Secret");
        seed_meeting_note(&db, "m1", "Sealed", "# S\n", Some("f1"));
        db.insert_document("d1", "f1", "note-a", "body", "note", 1)
            .unwrap();
        db.set_folder_locked("f1", true, None).unwrap();
        stage_finding(&db, "m1", None, "k-meeting");
        stage_finding(&db, "d1", None, "k-doc");
        stage_finding(&db, "m-else", None, "k-unrelated");

        let _ = db.reblank_locked_folders_at_rest().unwrap();

        let pending = db.list_audit_finding_rows("pending").unwrap();
        assert!(
            pending.is_empty(),
            "the startup reconcile purges ALL pending findings (rollup posture): {pending:#?}"
        );
    }

    #[test]
    fn meeting_and_note_delete_purge_pending_findings() {
        let db = file_db("purge-delete");
        seed_meeting_note(&db, "m1", "Doomed", "# D\n", None);
        make_folder(&db, "fn1", "Notes");
        db.insert_document("d1", "fn1", "note-a", "body", "note", 1)
            .unwrap();
        stage_finding(&db, "m1", None, "k-m");
        stage_finding(&db, "m-x", Some("m1"), "k-m-target");
        stage_finding(&db, "d1", None, "k-d");
        stage_finding(&db, "m-x", None, "k-keep");

        let _ = db.delete_meeting("m1").unwrap();
        db.delete_document("d1").unwrap();

        let pending = db.list_audit_finding_rows("pending").unwrap();
        assert_eq!(
            pending.len(),
            1,
            "delete purges by source AND target: {pending:#?}"
        );
        assert_eq!(pending[0].dedupe_key, "k-keep");
    }

    // ── migration idempotence (extended) ──

    #[test]
    fn audit_migration_is_idempotent_and_tables_exist() {
        let db = file_db("migrate");
        // migrate() already ran inside open_with_key; run it again — must be a no-op.
        db.migrate().unwrap();
        db.migrate().unwrap();
        // Both tables exist and accept rows.
        stage_finding(&db, "m1", None, "k1");
        db.insert_audit_run("r1", 1, 2, "{}").unwrap();
        assert_eq!(db.list_audit_finding_rows("pending").unwrap().len(), 1);
    }

    // ── resolve blanks evidence + ALL title material ──

    #[test]
    fn resolve_blanks_evidence_and_accept_action_on_both_paths() {
        let db = file_db("blank");
        db.insert_audit_finding_if_new(
            &NewAuditFinding {
                kind: "orphan".into(),
                source_kind: "note".into(),
                source_id: "n1".into(),
                source_title: "T".into(),
                target_title: None,
                target_id: None,
                target_kind: None,
                evidence_md: "> secret-ish derived text".into(),
                accept_action: "Append suggested [[links]] under ## Audit".into(),
                dedupe_key: "orphan|n1|".into(),
            },
            "r1",
            1,
        )
        .unwrap();
        let id = db.list_audit_finding_rows("pending").unwrap()[0].id.clone();
        db.resolve_audit_finding_row(&id, "dismissed", 5).unwrap();
        let row = db.get_audit_finding(&id).unwrap().unwrap();
        assert_eq!(row.status, "dismissed");
        assert_eq!(row.resolved_at, Some(5));
        assert!(
            row.evidence_md.is_empty(),
            "dismiss blanks the derived plaintext"
        );
        assert!(
            row.accept_action.is_empty(),
            "dismiss blanks the accept action"
        );

        db.insert_audit_finding_if_new(
            &NewAuditFinding {
                kind: "stale".into(),
                source_kind: "meeting".into(),
                source_id: "m1".into(),
                source_title: "T".into(),
                target_title: None,
                target_id: None,
                target_kind: None,
                evidence_md: "> counts".into(),
                accept_action: "Append a [!stale] callout under ## Audit".into(),
                dedupe_key: "stale|m1|".into(),
            },
            "r1",
            1,
        )
        .unwrap();
        let id2 = db
            .list_audit_finding_rows("pending")
            .unwrap()
            .iter()
            .find(|r| r.source_id == "m1")
            .unwrap()
            .id
            .clone();
        db.resolve_audit_finding_row(&id2, "accepted", 6).unwrap();
        let row2 = db.get_audit_finding(&id2).unwrap().unwrap();
        assert_eq!(row2.status, "accepted");
        assert!(
            row2.evidence_md.is_empty(),
            "accept blanks the derived plaintext too"
        );
        assert!(row2.accept_action.is_empty());
    }

    /// Lock review (BLOCKING leak): titles are content material too. Resolved rows survive every
    /// purge (pending-only) and the list's SOURCE re-gate — so a dismissed/accepted row keeping a
    /// later-sealed TARGET's title would serve it forever. Resolve must blank BOTH titles in the
    /// same UPDATE: only PENDING rows carry ANY title/evidence material at rest.
    #[test]
    fn resolved_rows_carry_no_title_material() {
        let db = file_db("blank-titles");
        db.insert_audit_finding_if_new(
            &NewAuditFinding {
                kind: "unlinked_mention".into(),
                source_kind: "meeting".into(),
                source_id: "m-open".into(),
                source_title: "Open Standup".into(),
                target_title: Some("SECRET-TARGET-TITLE".into()),
                target_id: Some("m-t".into()),
                target_kind: Some("meeting".into()),
                evidence_md: "> mentioned SECRET-TARGET-TITLE".into(),
                accept_action: "Append the suggested [[link]] under ## Audit".into(),
                dedupe_key: "k-titles-dismiss".into(),
            },
            "r1",
            1,
        )
        .unwrap();
        let id = db.list_audit_finding_rows("pending").unwrap()[0].id.clone();
        db.resolve_audit_finding_row(&id, "dismissed", 5).unwrap();
        let row = db.get_audit_finding(&id).unwrap().unwrap();
        assert!(
            row.source_title.is_empty(),
            "dismiss blanks the source title: {row:?}"
        );
        assert!(
            row.target_title.is_none(),
            "dismiss blanks the target title: {row:?}"
        );
        assert!(row.evidence_md.is_empty() && row.accept_action.is_empty());

        // The accepted path blanks identically (the seal of the target later must find NOTHING).
        db.insert_audit_finding_if_new(
            &NewAuditFinding {
                kind: "contradiction".into(),
                source_kind: "meeting".into(),
                source_id: "m-a".into(),
                source_title: "Meeting A".into(),
                target_title: Some("SECRET-OTHER-TITLE".into()),
                target_id: Some("m-b".into()),
                target_kind: Some("meeting".into()),
                evidence_md: "> objects".into(),
                accept_action: "Append [!conflict] callouts cross-linking both sources".into(),
                dedupe_key: "k-titles-accept".into(),
            },
            "r1",
            1,
        )
        .unwrap();
        let id2 = db
            .list_audit_finding_rows("pending")
            .unwrap()
            .iter()
            .find(|r| r.dedupe_key == "k-titles-accept")
            .unwrap()
            .id
            .clone();
        db.resolve_audit_finding_row(&id2, "accepted", 6).unwrap();
        let row2 = db.get_audit_finding(&id2).unwrap().unwrap();
        assert!(
            row2.source_title.is_empty(),
            "accept blanks the source title"
        );
        assert!(
            row2.target_title.is_none(),
            "accept blanks the target title"
        );
    }

    /// Lock review: `dedupe_key` outlives resolve (dismissed suppression), so it must carry ZERO
    /// title/content material — the variable part is hashed. Deterministic across runs (the
    /// suppression still matches) and distinct across targets.
    #[test]
    fn dedupe_keys_carry_no_title_material() {
        let corpus = vec![doc(
            "meeting",
            "m1",
            "Kickoff",
            "see [[SECRET Ghost Title]] and [[Other Missing One]]\nplus Mentioned Secret Note here\n",
        ),
        doc("note", "n-t", "Mentioned Secret Note", "content\n")];
        let broken = broken_link_pass(&corpus, &mut |_| Ok(false)).unwrap();
        assert_eq!(broken.len(), 2);
        for f in &broken {
            assert!(
                !f.dedupe_key.contains("SECRET")
                    && !f.dedupe_key.contains("Ghost")
                    && !f.dedupe_key.contains("Missing"),
                "broken_link key carries title material: {}",
                f.dedupe_key
            );
        }
        assert_ne!(
            broken[0].dedupe_key, broken[1].dedupe_key,
            "distinct per target"
        );
        // Deterministic: a second pass over the same corpus regenerates identical keys (this is
        // what keeps dismissed suppression working).
        let again = broken_link_pass(&corpus, &mut |_| Ok(false)).unwrap();
        assert_eq!(broken[0].dedupe_key, again[0].dedupe_key);

        let mentions = unlinked_mention_pass(&corpus);
        let mention = mentions
            .iter()
            .find(|f| f.source_id == "m1")
            .expect("the unlinked mention staged");
        assert!(
            !mention.dedupe_key.contains("Secret") && !mention.dedupe_key.contains("Mentioned"),
            "unlinked_mention key carries title material: {}",
            mention.dedupe_key
        );
    }

    /// Lock review + adversarial A: `discard_folder_seal` purges ALL pending findings too (a
    /// TOCTOU-orphaned row — including one merely CITING this folder's titles — must not survive
    /// the discard; same rollup posture as every other lock-surface mutation).
    #[test]
    fn discard_folder_seal_purges_all_pending_findings() {
        let db = file_db("purge-discard");
        make_folder(&db, "f1", "Secret");
        seed_meeting_note(&db, "m1", "Sealed", "# S\n", Some("f1"));
        db.insert_document("d1", "f1", "note-a", "body", "note", 1)
            .unwrap();
        db.set_folder_locked("f1", true, None).unwrap();
        stage_finding(&db, "m1", None, "k-m");
        stage_finding(&db, "d1", None, "k-d");
        stage_finding(&db, "m-else", Some("m1"), "k-t");
        stage_finding(&db, "m-else", None, "k-unrelated");

        let _ = db.discard_folder_seal("f1").unwrap();

        let pending = db.list_audit_finding_rows("pending").unwrap();
        assert!(
            pending.is_empty(),
            "a discard purges ALL pending findings: {pending:#?}"
        );
    }

    /// Lock review (TOCTOU shrink): a run whose seal epoch advanced is withdrawn wholesale — the
    /// staged rows are deleted by run_id and the pass reports zero; a quiet run keeps its rows.
    #[test]
    fn epoch_advance_withdraws_staged_run_rows() {
        let db = file_db("epoch-withdraw");
        stage_finding(&db, "m1", None, "k-race-1"); // run-x (the helper's run id)
        stage_finding(&db, "m2", None, "k-race-2");
        db.insert_audit_finding_if_new(
            &NewAuditFinding {
                kind: "orphan".into(),
                source_kind: "meeting".into(),
                source_id: "m3".into(),
                source_title: "T".into(),
                target_title: None,
                target_id: None,
                target_kind: None,
                evidence_md: "> e".into(),
                accept_action: "".into(),
                dedupe_key: "k-other-run".into(),
            },
            "run-other",
            1,
        )
        .unwrap();

        // Quiet epoch (advanced = false): everything stays.
        let counts: BTreeMap<String, usize> =
            [("broken_link".to_string(), 2usize)].into_iter().collect();
        let (n, c) =
            reconcile_run_on_epoch_advance(&db, "run-x", false, 2, counts.clone()).unwrap();
        assert_eq!((n, c.len()), (2, 1));
        assert_eq!(db.list_audit_finding_rows("pending").unwrap().len(), 3);

        // Advanced epoch: the run's rows are withdrawn, the OTHER run's row survives, zeros out.
        let (n, c) = reconcile_run_on_epoch_advance(&db, "run-x", true, 2, counts).unwrap();
        assert_eq!(n, 0, "an interleaved run reports zero staged");
        assert!(c.is_empty());
        let pending = db.list_audit_finding_rows("pending").unwrap();
        assert_eq!(
            pending.len(),
            1,
            "only the other run's row survives: {pending:#?}"
        );
        assert_eq!(pending[0].dedupe_key, "k-other-run");
    }

    // ── Phase 3: the weekly schedule ──

    /// The pure due predicate: enabled + (never-ran OR a full week elapsed, `>=` catch-up).
    #[test]
    fn weekly_due_matrix() {
        let now = 1_700_000_000_000i64;
        // Never scheduled-ran + enabled → due.
        assert!(weekly_due(now, None, true));
        // Disabled → never due, even never-ran.
        assert!(!weekly_due(now, None, false));
        assert!(!weekly_due(
            now,
            Some(now - WEEKLY_AUDIT_INTERVAL_MS - 1),
            false
        ));
        // Ran 6 days ago → holds.
        assert!(!weekly_due(
            now,
            Some(now - WEEKLY_AUDIT_INTERVAL_MS + 1),
            true
        ));
        // EXACTLY a week ago → fires (>= — a late hourly tick still catches up).
        assert!(weekly_due(now, Some(now - WEEKLY_AUDIT_INTERVAL_MS), true));
        // Over a week ago → fires.
        assert!(weekly_due(
            now,
            Some(now - WEEKLY_AUDIT_INTERVAL_MS - 3_600_000),
            true
        ));
        // Clock skew (a claim stamped in the future) → holds, never a storm.
        assert!(!weekly_due(now, Some(now + 60_000), true));
    }

    /// CLAIM-BEFORE-RUN: the scheduled claim row alone (no completed pass — the crash case) holds
    /// the week; manual (unscheduled) run rows never count toward due-ness. Also proves the
    /// additive `scheduled` migration is idempotent and the claim/read round-trips.
    #[test]
    fn scheduled_claim_holds_the_week_and_manual_runs_do_not() {
        let db = file_db("sched-claim");
        db.migrate().unwrap(); // second run — the scheduled column migration is idempotent.
        assert_eq!(db.last_scheduled_audit_run_finished_at().unwrap(), None);

        // A MANUAL pass's bookkeeping row (scheduled = 0) leaves the weekly runner due.
        let t0 = 1_700_000_000_000i64;
        db.insert_audit_run("r-manual", t0, t0, "{}").unwrap();
        assert_eq!(
            db.last_scheduled_audit_run_finished_at().unwrap(),
            None,
            "manual runs never push the weekly cadence"
        );
        assert!(weekly_due(t0 + 1, None, true));

        // The CLAIM row (inserted BEFORE the pass) holds the week even though no pass completed.
        db.insert_scheduled_audit_run_claim("r-claim", t0).unwrap();
        assert_eq!(db.last_scheduled_audit_run_finished_at().unwrap(), Some(t0));
        assert!(
            !weekly_due(t0 + 3_600_000, Some(t0), true),
            "a failed/crashed pass cannot re-fire within the claimed week"
        );
        assert!(weekly_due(t0 + WEEKLY_AUDIT_INTERVAL_MS, Some(t0), true));

        // The NEWEST claim wins.
        let t1 = t0 + WEEKLY_AUDIT_INTERVAL_MS;
        db.insert_scheduled_audit_run_claim("r-claim-2", t1)
            .unwrap();
        assert_eq!(db.last_scheduled_audit_run_finished_at().unwrap(), Some(t1));
    }

    // ── Phase 3: the local judge tier ──

    /// A test reasoner with a canned verdict stream (or a hard failure).
    struct VerdictReasoner {
        /// `Some(bool)` = `{"keep": bool}`; `None` = malformed reply (no `keep` key).
        verdicts: Vec<Option<bool>>,
        calls: std::sync::atomic::AtomicUsize,
        fail: bool,
        delay_ms: u64,
    }
    impl VerdictReasoner {
        fn keeping(verdicts: Vec<Option<bool>>) -> Self {
            Self {
                verdicts,
                calls: std::sync::atomic::AtomicUsize::new(0),
                fail: false,
                delay_ms: 0,
            }
        }
    }
    impl crate::reason::LocalReasoner for VerdictReasoner {
        fn id(&self) -> &str {
            "judge-test"
        }
        fn reason(&self, _s: &str, _u: &str) -> Result<String> {
            Ok(String::new())
        }
        fn structured(
            &self,
            _s: &str,
            _u: &str,
            _schema: &serde_json::Value,
        ) -> Result<serde_json::Value> {
            if self.delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(self.delay_ms));
            }
            if self.fail {
                return Err(crate::error::AppError::Unavailable("model wedged".into()));
            }
            let i = self.calls.fetch_add(1, Ordering::SeqCst);
            match self.verdicts.get(i).copied().flatten() {
                Some(keep) => Ok(serde_json::json!({ "keep": keep })),
                None => Ok(serde_json::json!({ "not_the_schema": 1 })),
            }
        }
    }

    /// Simulates the monotonic priority-epoch change made by a recording start racing with an
    /// already-dispatched local judge call. The coordinator tests separately bind that epoch bump
    /// to `begin_recording_session`.
    struct RecordingStartsDuringVerdict;
    impl crate::reason::LocalReasoner for RecordingStartsDuringVerdict {
        fn id(&self) -> &str {
            "judge-recording-race"
        }

        fn reason(&self, _s: &str, _u: &str) -> Result<String> {
            Ok(String::new())
        }

        fn structured(
            &self,
            _s: &str,
            _u: &str,
            _schema: &serde_json::Value,
        ) -> Result<serde_json::Value> {
            crate::perf::invalidate_background_epoch_for_test();
            Ok(serde_json::json!({ "keep": false }))
        }
    }

    /// Stage one judged-kind finding and return its row.
    fn stage_judged_finding(db: &Db, key: &str, kind: &str) -> AuditFindingRow {
        db.insert_audit_finding_if_new(
            &NewAuditFinding {
                kind: kind.into(),
                source_kind: "meeting".into(),
                source_id: "m-j".into(),
                source_title: "T".into(),
                target_title: None,
                target_id: None,
                target_kind: None,
                evidence_md: "> a vs b".into(),
                accept_action: "".into(),
                dedupe_key: key.to_string(),
            },
            "run-judge",
            1,
        )
        .unwrap();
        db.list_audit_finding_rows("pending")
            .unwrap()
            .into_iter()
            .find(|r| r.dedupe_key == key)
            .unwrap()
    }

    /// `keep = false` DELETES the row outright (not dismiss) — so the SAME dedupe key can be
    /// re-staged by a later pass (a real issue recurs); `keep = true` keeps it pending.
    #[test]
    fn judge_demote_deletes_and_the_finding_can_restage() {
        let db = file_db("judge-demote");
        let kept = stage_judged_finding(&db, "k-keep", "contradiction");
        let demoted = stage_judged_finding(&db, "k-demote", "stale");

        let reasoner = VerdictReasoner::keeping(vec![Some(true), Some(false)]);
        let stats = judge_findings_sync(
            &db,
            &reasoner,
            &[kept.clone(), demoted.clone()],
            JUDGE_STAGE_BUDGET_MS,
        );
        assert_eq!((stats.judged, stats.demoted), (2, 1));
        assert!(
            db.get_audit_finding(&kept.id).unwrap().is_some(),
            "keep=true survives"
        );
        assert!(
            db.get_audit_finding(&demoted.id).unwrap().is_none(),
            "keep=false is DELETED (not dismissed)"
        );
        // Deletion (unlike dismissal) leaves NO dedupe twin — the issue can re-stage next pass.
        assert!(
            db.insert_audit_finding_if_new(
                &NewAuditFinding {
                    kind: "stale".into(),
                    source_kind: "meeting".into(),
                    source_id: "m-j".into(),
                    source_title: "T".into(),
                    target_title: None,
                    target_id: None,
                    target_kind: None,
                    evidence_md: "> a vs b".into(),
                    accept_action: "".into(),
                    dedupe_key: "k-demote".into(),
                },
                "run-next",
                2,
            )
            .unwrap(),
            "a demoted finding re-stages on the next pass"
        );
    }

    /// The degrade matrix: an exhausted budget, a malformed reply, and a hard reasoner failure
    /// ALL keep every finding (loss-safe — the judge can only ever delete on an explicit false).
    #[test]
    fn judge_degrades_keep_everything_on_budget_malformed_and_error() {
        let db = file_db("judge-degrade");
        let r1 = stage_judged_finding(&db, "k-1", "contradiction");
        let r2 = stage_judged_finding(&db, "k-2", "stale");
        let rows = vec![r1, r2];

        // Budget already exhausted (0 ms) → nothing judged, nothing deleted.
        let demote_all = VerdictReasoner::keeping(vec![Some(false), Some(false)]);
        let stats = judge_findings_sync(&db, &demote_all, &rows, 0);
        assert_eq!((stats.judged, stats.demoted), (0, 0));
        assert_eq!(db.list_audit_finding_rows("pending").unwrap().len(), 2);

        // Malformed replies (no `keep` key) → kept, uncounted.
        let malformed = VerdictReasoner::keeping(vec![None, None]);
        let stats = judge_findings_sync(&db, &malformed, &rows, JUDGE_STAGE_BUDGET_MS);
        assert_eq!((stats.judged, stats.demoted), (0, 0));
        assert_eq!(db.list_audit_finding_rows("pending").unwrap().len(), 2);

        // Hard failures → kept.
        let failing = VerdictReasoner {
            verdicts: vec![],
            calls: std::sync::atomic::AtomicUsize::new(0),
            fail: true,
            delay_ms: 0,
        };
        let stats = judge_findings_sync(&db, &failing, &rows, JUDGE_STAGE_BUDGET_MS);
        assert_eq!((stats.judged, stats.demoted), (0, 0));
        assert_eq!(db.list_audit_finding_rows("pending").unwrap().len(), 2);

        // The STUB reasoner's reply carries no `keep` key either → the same keep-everything path
        // (the orchestrator additionally skips the stage entirely on id() == "stub").
        let stats = judge_findings_sync(
            &db,
            &crate::reason::StubReasoner,
            &rows,
            JUDGE_STAGE_BUDGET_MS,
        );
        assert_eq!((stats.judged, stats.demoted), (0, 0));
        assert_eq!(db.list_audit_finding_rows("pending").unwrap().len(), 2);
    }

    /// A verdict returned after recording priority starts is discarded wholesale. In particular,
    /// `keep=false` must not delete the pending finding even when the recording owner has already
    /// gone away by the time the caller checks the epoch.
    #[test]
    fn judge_discards_post_dispatch_output_after_recording_start() {
        let db = file_db("judge-recording-epoch");
        let row = stage_judged_finding(&db, "k-recording-race", "stale");
        let epoch = crate::perf::background_epoch();

        let stats = judge_findings_sync_guarded(
            &db,
            &RecordingStartsDuringVerdict,
            std::slice::from_ref(&row),
            JUDGE_STAGE_BUDGET_MS,
            Some(epoch),
        );

        assert_eq!((stats.judged, stats.demoted), (0, 0));
        assert!(
            db.get_audit_finding(&row.id).unwrap().is_some(),
            "stale post-dispatch output cannot delete a finding"
        );
    }

    /// A slow first call blows the stage deadline: later findings are NEVER judged (kept), and
    /// a row resolved between the read and the verdict is left alone (pending-only delete).
    #[test]
    fn judge_deadline_and_resolved_row_are_loss_safe() {
        let db = file_db("judge-deadline");
        let r1 = stage_judged_finding(&db, "k-slow-1", "contradiction");
        let r2 = stage_judged_finding(&db, "k-slow-2", "stale");

        // 30 ms budget vs a 60 ms call: the FIRST verdict (false) lands past its own in-call
        // timeout budget but the call itself is judged at t≈0; the SECOND row falls past the
        // deadline and is kept.
        let slow = VerdictReasoner {
            verdicts: vec![Some(false), Some(false)],
            calls: std::sync::atomic::AtomicUsize::new(0),
            fail: false,
            delay_ms: 60,
        };
        let stats = judge_findings_sync(&db, &slow, &[r1.clone(), r2.clone()], 30);
        assert!(
            stats.judged <= 1,
            "at most the first row is judged: {stats:?}"
        );
        assert!(
            db.get_audit_finding(&r2.id).unwrap().is_some(),
            "a past-deadline row is kept"
        );

        // Pending-only delete: a row the user resolved between read and verdict survives.
        let r3 = stage_judged_finding(&db, "k-resolved", "stale");
        db.resolve_audit_finding_row(&r3.id, "accepted", 5).unwrap();
        let demote = VerdictReasoner::keeping(vec![Some(false)]);
        let stats = judge_findings_sync(
            &db,
            &demote,
            std::slice::from_ref(&r3),
            JUDGE_STAGE_BUDGET_MS,
        );
        assert_eq!(stats.demoted, 0, "a resolved row is never deleted");
        assert_eq!(
            db.get_audit_finding(&r3.id).unwrap().unwrap().status,
            "accepted",
            "the resolved row is untouched"
        );
    }

    // ── Phase 3: the explain prompt (pure) ──

    #[test]
    fn explain_prompt_carries_evidence_action_and_snippet_only() {
        let row = AuditFindingRow {
            id: "f1".into(),
            kind: "contradiction".into(),
            source_kind: "meeting".into(),
            source_id: "m-a".into(),
            source_title: "Meeting A".into(),
            target_title: Some("Meeting B".into()),
            target_id: Some("m-b".into()),
            target_kind: Some("meeting".into()),
            evidence_md: "> \"shipped\" vs \"cancelled\"".into(),
            accept_action: "Append [!conflict] callouts cross-linking both sources".into(),
            dedupe_key: "k".into(),
            status: "pending".into(),
            run_id: "r".into(),
            created_at: 1,
            resolved_at: None,
        };
        let (system, user) = build_explain_prompt(&row, "the gated excerpt");
        assert!(
            system.contains("do not invent"),
            "grounding instruction present"
        );
        assert!(user.contains("contradiction"));
        assert!(user.contains("Meeting A") && user.contains("Meeting B"));
        assert!(user.contains("shipped") && user.contains("cancelled"));
        assert!(user.contains("Append [!conflict]"));
        assert!(user.contains("the gated excerpt"));

        // No snippet / no action / no target → those sections are simply absent.
        let mut bare = row.clone();
        bare.target_title = None;
        bare.accept_action = String::new();
        let (_, user) = build_explain_prompt(&bare, "");
        assert!(!user.contains("Related note:"));
        assert!(!user.contains("Suggested action"));
        assert!(!user.contains("excerpt"));
    }
}
