//! Analytics / egress-ledger / Vault-Audit commands — extracted verbatim from `commands` (God-file
//! split, a PURE MOVE — the visibility-gate logic is UNCHANGED, only relocated). Three read/act
//! surfaces: (1) the VISIBLE-content-only dashboard `get_analytics` (snapshots the live `unlocked`
//! set via `super::unlocked_snapshot`, so a sealed folder's meetings enter no count/duration); (2)
//! the content-free `get_egress_ledger` + its camelCase DTOs; (3) the Vault-Audit inbox —
//! `list_audit_findings` (DEFENSIVELY re-gates every row's SOURCE+TARGET against the live unlock set),
//! `resolve_audit_finding` (accept re-gates at apply time, per source kind), the accept/stamp
//! helpers, and the weekly schedule. EVERY read/act here is GATED — the gate is byte-identical to its
//! pre-move form. Every symbol keeps its EXACT prior body/signature and is re-exported at
//! `crate::commands` via `pub use analytics_commands::*;` in `commands/mod.rs`, so
//! `generate_handler![commands::get_analytics]` in `lib.rs` and every `crate::commands::…` caller
//! resolve UNCHANGED. `use super::*` brings in the shared types + the gate/vault helpers this domain
//! calls but that stay in `commands/mod.rs` (`unlocked_snapshot`, `source_is_stampable`,
//! `note_file_for`, `folder_locked_on_disk`, `note_display_title`, `update_note_doc_inner`,
//! `broken_link_target_resolves`, `refresh_meeting_note_exported_hash`, `file_stem_of`, `vault_path`).
//! `audit_row_visible` is promoted to `pub(crate)` so the `explain_audit_finding` command (kept in
//! `commands/mod.rs`) still shares the SAME re-gate, and `emit_audit_updated_after_purge` is promoted
//! so the many purge-bearing mutation commands (kept in `commands/mod.rs`) still ping the inbox.
//! The `*_inner` cores (`list_audit_findings_inner` / `resolve_audit_finding_inner` /
//! `get_audit_schedule_inner` / `set_audit_schedule_inner`) stay `pub(crate)` so the audit test module
//! (kept in `commands/mod.rs`) reaches them through the re-export.

use super::*;

/// Aggregate analytics for the dashboard + Analytics tab. VISIBLE-content only — a sealed-and-
/// not-session-unlocked folder's meetings are excluded from every count/duration/breakdown (same
/// gate as `brain_overview`/`list_meetings`), so the Analytics tab can never reveal the size or
/// activity pattern of content the user has deliberately locked.
#[tauri::command]
pub async fn get_analytics(app: AppHandle) -> Result<Analytics, AppError> {
    offload_read(app, |state| {
        let unlocked = unlocked_snapshot(state)?;
        state.db.analytics(&unlocked)
    })
    .await
}

/// Per-model token-usage roll-up for `EgressLedger.byModel`.
///
/// Fields are `u64` so a large all-time cumulative sum (mirroring `EgressModelUsage`) cannot
/// silently wrap. JavaScript `number` (f64) handles these values without precision loss up to
/// 2^53 (~9 petaTokens), which is far beyond any realistic usage window.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsageDto {
    pub model: String,
    pub calls: u64,
    pub tokens: u64,
}

/// Per-day token-usage roll-up for `EgressLedger.byDay`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayUsageDto {
    /// ISO-8601 date string ("YYYY-MM-DD") in UTC.
    pub day: String,
    pub tokens: u64,
}

/// Redaction-count totals for the queried window.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedactionTotalsDto {
    pub email: u64,
    pub card: u64,
    pub phone: u64,
    pub name: u64,
}

/// One row from the `egress_log` table (content-free: counts + ids only).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EgressRowDto {
    /// Unix epoch (seconds) of the call.
    pub ts: i64,
    pub provider_id: String,
    pub destination: String,
    pub model_served: Option<String>,
    pub total_tokens: Option<u32>,
    pub redactions: RedactionTotalsDto,
}

/// Aggregated egress ledger for a rolling window (`days` days back from now).
///
/// Shape matches `EgressLedger` in `src/app/core/models.ts` (camelCase).
/// Every aggregate handles an empty `egress_log` gracefully — totals are zero, vecs empty.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EgressLedgerDto {
    pub total_calls: u64,
    pub total_tokens: u64,
    pub by_model: Vec<ModelUsageDto>,
    pub by_day: Vec<DayUsageDto>,
    pub total_redactions: RedactionTotalsDto,
    /// Last ≤20 rows from `egress_log`, newest first.
    pub recent: Vec<EgressRowDto>,
}

/// Aggregate the content-free `egress_log` table for the given rolling window and return the
/// ledger for the "Egress & Usage" Analytics panel.
///
/// `days` is the window width; pass `30` for the default 30-day view. The window is computed as
/// `ts >= (now_unix - days * 86400)`. An empty table (no cloud calls yet) returns all-zero totals
/// and empty vecs — never an error.
///
/// Read-only: queries `egress_log` only. No content columns are touched.
#[tauri::command]
pub fn get_egress_ledger(
    days: i64,
    state: State<'_, AppState>,
) -> Result<EgressLedgerDto, AppError> {
    let ledger = state.db.egress_summary(days)?;
    Ok(EgressLedgerDto {
        total_calls: ledger.total_calls,
        total_tokens: ledger.total_tokens,
        by_model: ledger
            .by_model
            .into_iter()
            .map(|m| ModelUsageDto {
                model: m.model,
                calls: m.calls,
                tokens: m.tokens,
            })
            .collect(),
        by_day: ledger
            .by_day
            .into_iter()
            .map(|d| DayUsageDto {
                day: d.day,
                tokens: d.tokens,
            })
            .collect(),
        total_redactions: RedactionTotalsDto {
            email: ledger.total_redactions.email,
            card: ledger.total_redactions.card,
            phone: ledger.total_redactions.phone,
            name: ledger.total_redactions.name,
        },
        recent: ledger
            .recent
            .into_iter()
            .map(|r| EgressRowDto {
                ts: r.ts,
                provider_id: r.provider_id,
                destination: r.destination,
                model_served: r.model_served,
                total_tokens: r.total_tokens,
                redactions: RedactionTotalsDto {
                    email: r.redactions.email,
                    card: r.redactions.card,
                    phone: r.redactions.phone,
                    name: r.redactions.name,
                },
            })
            .collect(),
    })
}

/// List findings by status (default `pending`). DEFENSIVELY re-filters each row's SOURCE
/// visibility against the LIVE session unlock set before returning — belt-and-braces on top of
/// purge-on-seal (which already drops pending rows, by source AND target id, inside every seal
/// tx), so a finding whose folder sealed between pass and list can never surface.
#[tauri::command]
pub fn list_audit_findings(
    state: State<'_, AppState>,
    status: Option<String>,
) -> Result<Vec<crate::audit::AuditFinding>, AppError> {
    list_audit_findings_inner(state.inner(), status.as_deref())
}

pub(crate) fn list_audit_findings_inner(
    state: &AppState,
    status: Option<&str>,
) -> Result<Vec<crate::audit::AuditFinding>, AppError> {
    let status = status.unwrap_or("pending");
    if !matches!(status, "pending" | "accepted" | "dismissed") {
        return Err(AppError::InvalidArg(
            "status must be pending, accepted or dismissed".into(),
        ));
    }
    let unlocked = unlocked_snapshot(state)?;
    let rows = state.db.list_audit_finding_rows(status)?;
    let mut out = Vec::new();
    for r in rows {
        // SOURCE + TARGET re-gate (lock review, defense in depth): a pending row's titles/
        // evidence reference both sides, so either side sealing between pass and list hides the
        // row. `audit_row_visible` fails CLOSED on an untyped target (`target_id` without
        // `target_kind` — such a row cannot be re-gated against the right table), and the
        // explain command shares the same helper so the two gates can never diverge.
        if !audit_row_visible(state, &r, &unlocked)? {
            continue; // sealed since the pass — hide, never mask.
        }
        out.push(r.into_dto());
    }
    Ok(out)
}

/// Resolve one PENDING finding: `"accept"` applies its append-only vault action (re-gated at
/// apply time), `"dismiss"` just flips the status. BOTH paths blank `evidence_md` +
/// `accept_action` — only pending rows ever hold derived plaintext. Returns the updated finding.
#[tauri::command]
pub fn resolve_audit_finding(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    action: String,
) -> Result<crate::audit::AuditFinding, AppError> {
    let out = resolve_audit_finding_inner(state.inner(), &id, &action)?;
    let pending = state.db.count_pending_audit_findings().unwrap_or(0);
    crate::events::emit_audit_updated(&app, pending as u32);
    Ok(out)
}

pub(crate) fn resolve_audit_finding_inner(
    state: &AppState,
    id: &str,
    action: &str,
) -> Result<crate::audit::AuditFinding, AppError> {
    // Seal-vs-write TOCTOU: the lifecycle guard is taken INSIDE the accept path, per source kind
    // (see `apply_audit_accept`) — NOT here. The note-canonical writer delegates to
    // `update_note_doc_inner`, which takes the same NON-REENTRANT guard itself; holding it across
    // that call would self-deadlock. The dismiss path and the status flip are single atomic DB
    // statements and need no guard.
    let row = state
        .db
        .get_audit_finding(id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no audit finding {id}")))?;
    if row.status != "pending" {
        return Err(AppError::InvalidArg(
            "this finding was already handled".into(),
        ));
    }
    let status = match action {
        "dismiss" => "dismissed",
        "accept" => {
            if row.accept_action.is_empty() {
                return Err(AppError::InvalidArg(
                    "this finding is dismiss-only — it has no accept action".into(),
                ));
            }
            apply_audit_accept(state, &row)?;
            "accepted"
        }
        _ => {
            return Err(AppError::InvalidArg(
                "action must be \"accept\" or \"dismiss\"".into(),
            ))
        }
    };
    state
        .db
        .resolve_audit_finding_row(id, status, chrono::Utc::now().timestamp_millis())?;
    let updated = state
        .db
        .get_audit_finding(id)?
        .ok_or_else(|| AppError::Storage("finding vanished during resolve".into()))?;
    tracing::info!(target: "audit", finding_id = %id, kind = %updated.kind, status = %status, "audit finding resolved");
    Ok(updated.into_dto())
}

/// APPLY one finding's accept action: an APPEND-ONLY stamp under `## Audit` in the source's
/// exported `.md` (both sources for a contradiction). Every path RE-GATES at apply time
/// (the prune↔seal TOCTOU discipline — a source sealed since the pass refuses with
/// `AppError::Locked`, never stamps), and every appended `[[link]]` is re-resolved against the
/// LIVE vault/session first (the `list_vault_titles` anti-hallucination rule).
fn apply_audit_accept(
    state: &AppState,
    row: &crate::audit::AuditFindingRow,
) -> Result<(), AppError> {
    // Seal-vs-write TOCTOU, split by source kind: MEETING flows stamp the exported FILE and have
    // no inner guard — hold the lifecycle guard across their gate+append (the
    // `apply_supersessions_inner` discipline). NOTE flows write through the canonical store via
    // `update_note_doc_inner`, which takes the SAME non-reentrant guard itself — holding it here
    // would self-deadlock; their gate+write atomicity lives inside that writer, and a seal racing
    // the pre-read fails CLOSED at its re-gate.
    let _lifecycle = (row.source_kind == "meeting").then(|| lifecycle_guard(state));
    // Re-gate the SOURCE up front, so a sealed-since-the-pass source refuses with the honest
    // `Locked` BEFORE any per-kind validation (link re-resolution etc.) can mask it with a
    // misleading InvalidArg. The write paths re-gate again at their own boundary.
    let source_open = match row.source_kind.as_str() {
        "meeting" => source_is_stampable(state, &row.source_id)?,
        _ => {
            let unlocked = unlocked_snapshot(state)?;
            state.db.note_is_visible(&row.source_id, &unlocked)?
        }
    };
    if !source_open {
        return Err(AppError::Locked(
            "this note's folder is locked — unlock it to apply the audit action".into(),
        ));
    }
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    match row.kind.as_str() {
        "broken_link" => {
            let target = row.target_title.as_deref().unwrap_or("").trim().to_string();
            if target.is_empty() {
                return Err(AppError::InvalidArg(
                    "this finding has no link target".into(),
                ));
            }
            // LIVE re-resolve (lock review): the target may have been created since the pass —
            // the stamp would then assert a falsehood. REFUSE, loss-safe: nothing is written and
            // the row stays pending with its evidence, so the user dismisses or re-runs the
            // audit (chosen over auto-dismiss so no status flips without an explicit choice).
            if broken_link_target_resolves(state, &target)? {
                return Err(AppError::InvalidArg(
                    "the link target now resolves — dismiss this finding or re-run the audit"
                        .into(),
                ));
            }
            // The [[..]] rides in a CODE SPAN so the stamp itself never becomes another
            // (broken) wikilink — the one deliberate exception to "appended links resolve".
            // Worded as an observation at audit time, not a standing claim.
            let body = format!("> `[[{target}]]` — link target was not found at audit time");
            stamp_audit_source(state, row, |md| {
                crate::export::obsidian::append_audit_callout(md, &date, "broken-link", &body)
            })
        }
        "stale" => {
            // Counts recomputed LIVE through the gated reader (they may have moved since the
            // pass); the callout carries counts only — never superseding titles (leak-safe even
            // if a superseding source sealed since the pass).
            let unlocked = unlocked_snapshot(state)?;
            let facts = state
                .db
                .fact_rows_for_meeting_visible(&row.source_id, &unlocked)?;
            let total = facts.len();
            let closed = facts.iter().filter(|f| f.valid_to.is_some()).count();
            let body = format!(
                "> {closed} of {total} facts recorded in this note have since been superseded by later meetings"
            );
            stamp_audit_source(state, row, |md| {
                crate::export::obsidian::append_audit_callout(md, &date, "stale", &body)
            })
        }
        "contradiction" => {
            let other_id = row
                .target_id
                .as_deref()
                .ok_or_else(|| AppError::InvalidArg("this finding has no counterpart".into()))?;
            // Re-gate BOTH sides BEFORE stamping either (never stamp into, or reference, a
            // sealed note — and never leave a half-stamped pair). Both sides are MEETINGS by
            // construction (the contradiction pass is facts-over-meetings only).
            let source_path = audit_meeting_source_file(state, &row.source_id)?;
            let other_path = audit_meeting_source_file(state, other_id)?;
            let source_stem = file_stem_of(&source_path);
            let other_stem = file_stem_of(&other_path);
            append_to_audit_meeting_file(state, &row.source_id, &source_path, |md| {
                crate::export::obsidian::append_audit_callout(
                    md,
                    &date,
                    "conflict",
                    &format!("> conflicting facts recorded here and in [[{other_stem}]] — review which is current"),
                )
            })?;
            append_to_audit_meeting_file(state, other_id, &other_path, |md| {
                crate::export::obsidian::append_audit_callout(
                    md,
                    &date,
                    "conflict",
                    &format!("> conflicting facts recorded here and in [[{source_stem}]] — review which is current"),
                )
            })
        }
        "unlinked_mention" => {
            let target = row.target_title.as_deref().unwrap_or("").trim().to_string();
            if target.is_empty() {
                return Err(AppError::InvalidArg(
                    "this finding has no link target".into(),
                ));
            }
            if !audit_link_target_ok(state, &target)? {
                return Err(AppError::InvalidArg(
                    "the suggested link target no longer exists — dismiss this finding instead"
                        .into(),
                ));
            }
            let line = format!("- Suggested link: [[{target}]]");
            stamp_audit_source(state, row, |md| {
                crate::export::obsidian::append_audit_line(md, &line)
            })
        }
        "orphan" => {
            // The suggestions live as [[links]] inside the (still-pending) evidence — re-extract
            // and RE-RESOLVE each against the live vault/session; only survivors are appended.
            let mut suggestions = Vec::new();
            for t in crate::storage::db::extract_wikilink_titles(&row.evidence_md) {
                if audit_link_target_ok(state, &t)? {
                    suggestions.push(format!("[[{t}]]"));
                }
            }
            if suggestions.is_empty() {
                return Err(AppError::InvalidArg(
                    "no suggested link still resolves — dismiss this finding instead".into(),
                ));
            }
            let line = format!("- Suggested links: {}", suggestions.join(" · "));
            stamp_audit_source(state, row, |md| {
                crate::export::obsidian::append_audit_line(md, &line)
            })
        }
        _ => Err(AppError::InvalidArg(format!(
            "unknown finding kind {}",
            row.kind
        ))),
    }
}

/// Resolve + stamp a finding's (single) source in one step, dispatched by SOURCE KIND: a
/// meeting note is stamped on its exported `.md` (the Re-Truth precedent — the file IS its
/// projection surface); an authored NOTE is stamped through its CANONICAL DB text (live-bug fix
/// 2026-07-16: a Brain note created in-app may have NO exported file, and the DB text is
/// canonical for every note regardless).
fn stamp_audit_source(
    state: &AppState,
    row: &crate::audit::AuditFindingRow,
    stamp: impl Fn(&str) -> String,
) -> Result<(), AppError> {
    match row.source_kind.as_str() {
        "meeting" => {
            let path = audit_meeting_source_file(state, &row.source_id)?;
            append_to_audit_meeting_file(state, &row.source_id, &path, stamp)
        }
        "note" => stamp_audit_note_source(state, &row.source_id, stamp),
        other => Err(AppError::InvalidArg(format!(
            "unknown finding source kind {other}"
        ))),
    }
}

/// Stamp a NOTE-source finding through the note's CANONICAL store — never through the exported
/// file. Reads the markdown through the GATED reader (sealed-and-not-session-unlocked → Locked),
/// applies the idempotent append to the TEXT (the marker lives in the DB text), and persists via
/// [`update_note_doc_inner`] — the note editor's own save path — so the seal-on-write gate, the
/// re-index, and the vault re-export all ride along. The re-export CREATES the `.md` and stamps
/// `exported_path`/`exported_hash` for a never-exported note (healing it), and `write_note`
/// never clobbers different bytes, so an externally-edited exported file survives untouched.
fn stamp_audit_note_source(
    state: &AppState,
    note_id: &str,
    stamp: impl Fn(&str) -> String,
) -> Result<(), AppError> {
    let (markdown, title) = {
        let _lifecycle = lifecycle_guard(state);
        let Some((folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(note_id)? else {
            return Err(AppError::InvalidArg(format!("no note {note_id}")));
        };
        if !folder_is_unlocked(state, &folder_id)? {
            return Err(AppError::Locked(
                "this note's folder is locked — unlock it to apply the audit action".into(),
            ));
        }
        let row = state
            .db
            .get_note_row(note_id)?
            .ok_or_else(|| AppError::InvalidArg(format!("no note {note_id}")))?;
        let title = note_display_title(&row);
        (row.text, title)
    };
    let written = stamp(&markdown);
    if written == markdown {
        return Ok(()); // already stamped — idempotent no-op.
    }
    update_note_doc_inner(state, note_id, &title, &written)?;
    Ok(())
}

/// Read-modify-write APPEND of one audit stamp on a MEETING note's exported `.md`: read the
/// CURRENT file, apply the (idempotent) append, write only on change, then refresh the
/// export-collision baseline with the Phase-0 CONDITIONAL rule
/// ([`refresh_meeting_note_exported_hash`] — an externally-edited file keeps its stale baseline,
/// so the next full overwrite preserves edit + stamp as a sibling instead of laundering).
fn append_to_audit_meeting_file(
    state: &AppState,
    meeting_id: &str,
    path: &str,
    stamp: impl Fn(&str) -> String,
) -> Result<(), AppError> {
    let current = std::fs::read_to_string(path)
        .map_err(|e| AppError::Export(format!("read note before audit stamp failed: {e}")))?;
    let written = stamp(&current);
    if written == current {
        return Ok(()); // already stamped — idempotent no-op.
    }
    crate::export::obsidian::overwrite_note(std::path::Path::new(path), &written)?;
    refresh_meeting_note_exported_hash(state, meeting_id, &current, &written)
}

/// Re-gate + resolve a MEETING finding source's exported `.md` at APPLY time (the TOCTOU
/// re-check — the Re-Truth `source_is_stampable` posture): session-unlocked AND open-on-disk,
/// with an exported file. Refusals are `AppError::Locked`; a missing file says what to do.
fn audit_meeting_source_file(state: &AppState, source_id: &str) -> Result<String, AppError> {
    if !source_is_stampable(state, source_id)? {
        return Err(AppError::Locked(
            "this note's folder is locked — unlock it to apply the audit action".into(),
        ));
    }
    match note_file_for(state, source_id)? {
        Some((path, _)) => Ok(path),
        None => Err(AppError::InvalidArg(
            "this meeting's note has no exported vault file — configure a vault and re-export the note first".into(),
        )),
    }
}

/// The anti-hallucination re-check for an appended `[[link]]`: the title must resolve NOW —
/// through the GATED `resolve_wikilink` (notes/meetings/org, live session set) to a target whose
/// folder is also open ON DISK (a link into a merely session-unlocked folder would break at
/// relock — the `superseding_link_stem` bar), or be an existing vault file stem.
fn audit_link_target_ok(state: &AppState, title: &str) -> Result<bool, AppError> {
    let unlocked = unlocked_snapshot(state)?;
    match state.db.resolve_wikilink(title, &unlocked)? {
        Some(t) if t.kind == "meeting" => Ok(!folder_locked_on_disk(state, &t.id)?),
        Some(t) if t.kind == "note" => {
            let Some((folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(&t.id)? else {
                return Ok(false);
            };
            Ok(state
                .db
                .folder_by_id(&folder_id)?
                .map(|f| !f.locked)
                .unwrap_or(false))
        }
        Some(_) => Ok(true), // org item — deliberately-disclosed, outside the folder-lock domain.
        None => {
            // Not a note/meeting/org — an existing user-authored vault file still resolves.
            let Some(vault) = vault_path(state) else {
                return Ok(false);
            };
            Ok(
                crate::export::obsidian::list_vault_titles(std::path::Path::new(&vault))?
                    .iter()
                    .any(|t| t == title),
            )
        }
    }
}

/// Ping the FE audit inbox after a purge-bearing lock/relock/delete/discard/move-into-locked
/// mutation (count-only payload, adversarial B): pending findings vanish reactively instead of
/// on the next manual refetch. Mirrors the screen-share relock's emit posture — best-effort by
/// construction (`emit_audit_updated` swallows failures).
pub(crate) fn emit_audit_updated_after_purge(app: &AppHandle, state: &AppState) {
    let pending = state.db.count_pending_audit_findings().unwrap_or(0);
    crate::events::emit_audit_updated(app, pending as u32);
}

/// Re-gate ONE finding row against a session unlock set: the SOURCE must be visible, and when a
/// target id rides on the row its TARGET must be visible too, checked against the table its
/// `target_kind` names. FAIL-CLOSED on an UNTYPED target (`target_id` set, `target_kind` NULL):
/// such a row cannot be target-re-gated, so it is treated as not visible (lock-review NIT,
/// 2026-07-16 — the previous inline check silently SKIPPED the target re-gate for untyped rows,
/// which fails open exactly when the target seals between pass and read). Shared by the list
/// re-gate and `explain_audit_finding` so the two can never diverge.
pub(crate) fn audit_row_visible(
    state: &AppState,
    row: &crate::audit::AuditFindingRow,
    unlocked: &std::collections::HashSet<String>,
) -> Result<bool, AppError> {
    let source_visible = match row.source_kind.as_str() {
        "meeting" => state.db.meeting_is_visible(&row.source_id, unlocked)?,
        _ => state.db.note_is_visible(&row.source_id, unlocked)?,
    };
    if !source_visible {
        return Ok(false);
    }
    if let Some(tid) = &row.target_id {
        let Some(tkind) = row.target_kind.as_deref() else {
            return Ok(false); // untyped target — cannot be re-gated ⇒ fail closed.
        };
        let target_visible = match tkind {
            "meeting" => state.db.meeting_is_visible(tid, unlocked)?,
            _ => state.db.note_is_visible(tid, unlocked)?,
        };
        if !target_visible {
            return Ok(false);
        }
    }
    Ok(true)
}

// ── Vault Audit Phase 3 — weekly schedule + cloud explain ───────────────────────────────────────

/// The weekly-audit schedule for the FE settings surface: the enabled flag + the last SCHEDULED
/// run + the derived next due time (see [`crate::audit::AuditSchedule`] for the `nextDueAt`
/// semantics). Read-only; content-free.
#[tauri::command]
pub fn get_audit_schedule(
    state: State<'_, AppState>,
) -> Result<crate::audit::AuditSchedule, AppError> {
    get_audit_schedule_inner(state.inner())
}

pub(crate) fn get_audit_schedule_inner(
    state: &AppState,
) -> Result<crate::audit::AuditSchedule, AppError> {
    let enabled = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?
        .vault_audit_weekly_enabled;
    let last_run_at = state.db.last_scheduled_audit_run_finished_at()?;
    let next_due_at = if enabled {
        last_run_at.map(|l| l + crate::audit::WEEKLY_AUDIT_INTERVAL_MS)
    } else {
        None
    };
    Ok(crate::audit::AuditSchedule {
        enabled,
        last_run_at,
        next_due_at,
    })
}

/// Enable/disable the weekly scheduled audit. The ONLY mutator of the flag (preserve-only on the
/// settings DTO), persisted through the dedicated `AppConfig::set_vault_audit_weekly` (persist
/// first, flip in-memory on durable success). Returns the updated schedule.
#[tauri::command]
pub fn set_audit_schedule(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<crate::audit::AuditSchedule, AppError> {
    set_audit_schedule_inner(state.inner(), enabled)
}

pub(crate) fn set_audit_schedule_inner(
    state: &AppState,
    enabled: bool,
) -> Result<crate::audit::AuditSchedule, AppError> {
    {
        let mut cache = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        cache.set_vault_audit_weekly(&state.db, enabled)?;
    }
    tracing::info!(target: "audit", enabled, "weekly audit schedule updated");
    get_audit_schedule_inner(state)
}
