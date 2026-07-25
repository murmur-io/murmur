//! Facts / cross-meeting USER MEMORY commands + persistence — extracted verbatim from `commands`
//! (God-file split, a PURE MOVE — the visibility-gate logic is UNCHANGED, only relocated). Two
//! surfaces over the bitemporal fact stores: (1) the USER-MEMORY audit/edit commands
//! (`get_user_memory` — GATED to facts whose SOURCE meeting is visible under the live unlock snapshot;
//! `forget_user_fact` / `clear_user_memory` — bitemporal INVALIDATE, never a silent delete); and (2)
//! the post-summary PERSISTENCE hooks that reconcile + apply entity FACTS + USER facts + Re-Truth
//! SUPERSESSIONS for a just-summarized meeting. `gated_memory_brief_for_injection` is the gated brief
//! folded into the non-agentic Ask / meeting-chat prompts (regenerated from the currently-VISIBLE user
//! facts, so a sealed-not-unlocked meeting injects NOTHING). Every symbol keeps its EXACT prior
//! body/signature and is re-exported at `crate::commands` via `pub use facts_commands::*;` in
//! `commands/mod.rs`, so `generate_handler![commands::get_user_memory]` in `lib.rs` and every
//! `crate::commands::…` caller resolve UNCHANGED.
//!
//! `use super::*` brings in the shared types + the gate helpers this domain calls but that stay in
//! `commands/mod.rs` (`unlocked_snapshot`, `gated_meeting_thread_turns`). The `*_inner` cores stay
//! `pub(crate)` (their pre-move visibility). Promoted to `pub(crate)` so the pipeline hook
//! `build_and_persist_entities`, the `ask_vault`/meeting-chat commands, the settings-DTO mappers, and
//! the fact/supersession test modules — ALL kept in `commands/mod.rs` — still reach them through the
//! re-export: `persist_facts_for_meeting`, `persist_user_facts_for_meeting`, `user_memory_enabled`,
//! `gated_memory_brief_for_injection`, `build_supersession_rows`. `record_supersessions_for_meeting`
//! keeps its (private) visibility — it is called only by `persist_facts_for_meeting`, which moved with
//! it.

use super::*;

/// List the current user-memory facts (open + visible) with provenance, plus the synthesized brief
/// that is injected into grounding. GATED: only facts whose SOURCE meeting is visible under the live
/// unlocked snapshot are returned — a sealed-not-unlocked meeting's user memory surfaces NOTHING.
#[tauri::command]
pub fn get_user_memory(
    state: State<'_, AppState>,
) -> Result<crate::user_memory::UserMemory, AppError> {
    get_user_memory_inner(state.inner())
}

/// Inner of [`get_user_memory`] taking `&AppState` (unit-testable gate).
pub(crate) fn get_user_memory_inner(
    state: &AppState,
) -> Result<crate::user_memory::UserMemory, AppError> {
    // FLAG: memory turned OFF entirely ⇒ the explicit disabled marker (empty facts + empty brief +
    // `disabled: true`), so the FE shows a "memory is off" affordance and NOTHING is surfaced. This
    // mirrors the injection paths, which are also flag-suppressed — the audit view can never show
    // facts the brain would not inject.
    if !user_memory_enabled(state) {
        return Ok(crate::user_memory::UserMemory::disabled());
    }
    let unlocked = unlocked_snapshot(state)?;
    let facts = state.db.list_user_facts_visible(&unlocked)?;
    // The audit view and the injected brief are derived from EXACTLY the same visible set, so the UI
    // faithfully mirrors what the brain actually injects.
    let brief = crate::user_memory::synthesize_brief(&facts);
    let dtos = facts
        .iter()
        .map(crate::user_memory::UserMemoryFact::from_fact)
        .collect();
    Ok(crate::user_memory::UserMemory {
        facts: dtos,
        brief,
        disabled: false,
    })
}

/// Forget ONE user-memory fact (bitemporal invalidate — the row is CLOSED, never silently deleted,
/// so history is preserved). After this the fact drops out of `get_user_memory` and the regenerated
/// brief. Idempotent. Content-free logging (the fact id only, never its text).
#[tauri::command]
pub fn forget_user_fact(state: State<'_, AppState>, id: String) -> Result<(), AppError> {
    forget_user_fact_inner(state.inner(), &id)
}

/// Inner of [`forget_user_fact`] taking `&AppState` (unit-testable).
pub(crate) fn forget_user_fact_inner(state: &AppState, id: &str) -> Result<(), AppError> {
    let at = chrono::Utc::now().to_rfc3339();
    let closed = state.db.forget_user_fact(id, &at)?;
    tracing::info!(target: "user_memory", fact_id = %id, closed, "user fact forgotten (invalidated)");
    Ok(())
}

/// Clear ALL user memory: bitemporal-close every currently-open user fact (invalidate, never delete —
/// closed history stays). After this `get_user_memory` and the brief are empty. Content-free logging
/// (a count only).
#[tauri::command]
pub fn clear_user_memory(state: State<'_, AppState>) -> Result<(), AppError> {
    clear_user_memory_inner(state.inner())
}

/// Inner of [`clear_user_memory`] taking `&AppState` (unit-testable).
pub(crate) fn clear_user_memory_inner(state: &AppState) -> Result<(), AppError> {
    let at = chrono::Utc::now().to_rfc3339();
    let n = state.db.clear_user_facts(&at)?;
    tracing::info!(target: "user_memory", count = n, "user memory cleared (all facts invalidated)");
    Ok(())
}

/// brain2 R2 — extract → reconcile → apply bitemporal FACTS for one summarized meeting. Pulled out
/// of [`build_and_persist_entities`] so it can fail in isolation (its caller logs + swallows): a
/// facts hiccup must NEVER block the note pipeline. Steps:
///   1. BEST-EFFORT extract candidates from the note about `entity_refs` (empty with the stub/no
///      model — the deterministic core is what carries the value),
///   2. load the EXISTING facts for those entities (un-gated lifecycle read),
///   3. run the PURE deterministic [`crate::facts::reconcile_facts`] at the meeting's time,
///   4. stamp the source meeting onto the Add ops and apply them in ONE atomic tx.
///
/// `at` is the meeting's `started_at` (the fact's valid-time origin), falling back to now.
pub(crate) fn persist_facts_for_meeting(
    state: &AppState,
    meeting_id: &str,
    title: &str,
    markdown: &str,
    entity_refs: &[(String, String)],
    recording_model_token: Option<&crate::perf::RecordingSessionToken>,
    visibility: &MeetingContentSnapshot,
) -> Result<(), AppError> {
    if entity_refs.is_empty() {
        return Ok(());
    }
    // 1) Best-effort extraction (panic-free, empty on stub/no model/decode failure). The reasoner
    //    is re-resolved from the LIVE config, so a consent/backend change applies without restart.
    let reasoner = match recording_model_token {
        Some(token) => state
            .reasoner
            .extraction_reasoner_for_recording(token.clone()),
        None => state.reasoner.extraction_reasoner(),
    };
    // Pin the extractor's OUTPUT language to the note-language config knob (default "auto") so a
    // Polish-dominant note can't emit the same fact in two languages. Fail-safe like
    // `user_memory_enabled`: a poisoned config mutex falls back to "auto".
    let note_language = state
        .config
        .lock()
        .map(|c| c.note_language.clone())
        .unwrap_or_else(|_| "auto".to_string());
    let candidates = crate::facts::extract_fact_candidates(
        // Brain Live ON ⇒ the LOCAL light engine (facts stop egressing); OFF ⇒ today's Notes reasoner.
        reasoner.as_ref(),
        title,
        markdown,
        entity_refs,
        &note_language,
        // Post-call extraction over the full note: no tight cap (the realtime path uses a capped preset).
        crate::reason::GenOptions::default(),
    );
    if candidates.is_empty() {
        return Ok(()); // nothing to reconcile — common in the default (no-model) build.
    }
    // Inference above intentionally runs without the lifecycle mutex. Rebind the exact meeting
    // authorization generation and hold it through every derived DB write so relock cannot purge
    // facts and then be followed by this late reconcile.
    let _lifecycle = lifecycle_guard(state);
    require_current_meeting_content_snapshot_under_lifecycle(state, meeting_id, visibility)?;
    // 2) Existing facts for exactly these entities.
    let entity_ids: Vec<String> = entity_refs.iter().map(|(id, _)| id.clone()).collect();
    let existing = state.db.facts_for_entities(&entity_ids)?;
    // 3) Deterministic reconcile at the meeting's time (valid-time origin).
    let at = state
        .db
        .get_meeting(meeting_id)?
        .map(|m| m.started_at)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let mut ops = crate::facts::reconcile_facts(&existing, &candidates, &at);
    // 4) Stamp the source meeting (gating + purge anchor) and apply atomically.
    crate::facts::set_meeting_id(&mut ops, meeting_id);
    state.db.apply_fact_ops(&ops)?;
    // Re-Truth (the vault heals itself): capture SUPERSESSIONS — every Invalidate that closed a fact
    // sourced in an OLDER meeting is recorded for one-tap review + stamping. BEST-EFFORT and NEVER
    // fails the note: a hiccup is logged (non-PII: counts only) and swallowed, exactly like the facts
    // hook that wraps this whole fn.
    if let Err(e) = record_supersessions_for_meeting(state, meeting_id, &existing, &ops) {
        tracing::warn!(target: "retruth", error = %e, "supersession capture failed (facts unaffected)");
    }
    Ok(())
}

/// Re-Truth: derive + persist the supersessions for one reconcile batch (called after
/// `apply_fact_ops`). Best-effort; returns `Ok` with nothing recorded when there are no cross-note
/// supersessions. Non-PII log line (a count only).
fn record_supersessions_for_meeting(
    state: &AppState,
    superseding_meeting_id: &str,
    existing: &[crate::facts::Fact],
    ops: &[crate::facts::FactOp],
) -> Result<(), AppError> {
    let now = chrono::Utc::now().to_rfc3339();
    let rows = build_supersession_rows(superseding_meeting_id, existing, ops, &now);
    if rows.is_empty() {
        return Ok(());
    }
    let recorded = state.db.record_supersessions(&rows)?;
    if recorded > 0 {
        tracing::info!(target: "retruth", recorded, "supersessions captured for review");
    }
    Ok(())
}

/// PURE (no DB, no clock — `now` injected): build the SUPERSESSION rows for one reconcile batch. For
/// each `FactOp::Invalidate` (which closed an OLD fact), resolve that old fact in `existing`
/// (entity/predicate/old_value + its source meeting) and the matching `FactOp::Add` (the new value
/// that superseded it, same entity+subject+predicate key reconcile used). SKIPS: an Invalidate whose
/// old fact is absent or has no source meeting, a self-supersession (the older fact came from the SAME
/// meeting), or a change with no matching Add. Deterministic + headless-testable.
pub(crate) fn build_supersession_rows(
    superseding_meeting_id: &str,
    existing: &[crate::facts::Fact],
    ops: &[crate::facts::FactOp],
    now: &str,
) -> Vec<crate::storage::models::SupersessionRow> {
    use crate::facts::FactOp;
    let norm = |s: &str| s.trim().to_lowercase();
    let mut out = Vec::new();
    for op in ops {
        let FactOp::Invalidate { id, .. } = op else {
            continue;
        };
        let Some(old) = existing.iter().find(|f| &f.id == id) else {
            continue;
        };
        let Some(source) = old.meeting_id.as_deref().filter(|m| !m.is_empty()) else {
            continue; // legacy/unattributed old fact — no source note to stamp.
        };
        if source == superseding_meeting_id {
            continue; // a meeting refining its own earlier fact is not a cross-note supersession.
        }
        // The new value is the Add op sharing the old fact's (entity_id, subject, predicate) key.
        let Some(new_value) = ops.iter().find_map(|o| match o {
            FactOp::Add(nf)
                if nf.entity_id == old.entity_id
                    && norm(&nf.subject) == norm(&old.subject)
                    && norm(&nf.predicate) == norm(&old.predicate) =>
            {
                Some(nf.object.clone())
            }
            _ => None,
        }) else {
            continue;
        };
        out.push(crate::storage::models::SupersessionRow {
            id: uuid::Uuid::new_v4().to_string(),
            superseding_meeting_id: superseding_meeting_id.to_string(),
            source_meeting_id: source.to_string(),
            entity: old.subject.clone(),
            predicate: old.predicate.clone(),
            old_value: old.object.clone(),
            new_value,
            created_at: now.to_string(),
            applied_at: None,
            source_pre_image: None,
            superseding_pre_image: None,
        });
    }
    out
}

/// Phase 3 CROSS-MEETING USER MEMORY — extract → reconcile → apply USER-SCOPED facts for one
/// summarized meeting. Mirrors [`persist_facts_for_meeting`] but for the user, not entities:
///   1. BEST-EFFORT extract user·predicate·object candidates from the note + the user's own typed
///      notes (empty with the stub / no model — the deterministic core is what carries the value),
///   2. load the EXISTING user facts (un-gated lifecycle read — reconcile runs before any seal),
///   3. run the PURE deterministic [`crate::facts::reconcile_facts`] at the meeting's time (the
///      user-scope sentinel in `entity_id` keys the reconcile),
///   4. stamp the source meeting onto the Add ops and apply them to `user_facts` in ONE atomic tx.
///
/// The (derived) memory brief is regenerated lazily on the next read/turn — no cache to invalidate.
pub(crate) fn persist_user_facts_for_meeting(
    state: &AppState,
    meeting_id: &str,
    title: &str,
    markdown: &str,
    recording_model_token: Option<&crate::perf::RecordingSessionToken>,
    visibility: &MeetingContentSnapshot,
) -> Result<(), AppError> {
    // FLAG: when the user has turned cross-meeting memory OFF, skip extraction ENTIRELY — no
    // reasoner call, no candidates, nothing new persisted. (Existing facts stay; the user can
    // forget/clear them, and the gated reads/injection are separately flag-suppressed.)
    if !user_memory_enabled(state) {
        return Ok(());
    }
    // The user's OWN typed notes for this meeting are a high-signal memory source (an explicit
    // "remember that…"). Empty when none (best-effort read).
    let typed_notes = state.db.get_manual_notes(meeting_id).unwrap_or_default();
    // D5 — the meeting's own @brain THREAD TURNS are the HIGHEST-signal source (an explicit
    // "zapamiętaj, że…" in a thread). GATED like every content read: the just-finished meeting is
    // its own unlocked meeting, so `list_assistant_interactions_visible` under the live unlock
    // snapshot returns its turns (and NOTHING for a sealed-not-unlocked meeting — fail-closed). We
    // feed the USER COMMAND text (the high-signal part), never the assistant's answer.
    let thread_turns = gated_meeting_thread_turns(state, meeting_id);
    // 1) Best-effort extraction (panic-free, empty on stub/no model/decode failure). The reasoner is
    //    re-resolved from the LIVE config so a consent/backend change applies without restart.
    let reasoner = match recording_model_token {
        Some(token) => state
            .reasoner
            .extraction_reasoner_for_recording(token.clone()),
        None => state.reasoner.extraction_reasoner(),
    };
    // Pin the extractor's output language (default "auto") — same fail-safe as `user_memory_enabled`
    // — so a Polish-dominant note can't duplicate a "me" fact as a PL+EN twin.
    let note_language = state
        .config
        .lock()
        .map(|c| c.note_language.clone())
        .unwrap_or_else(|_| "auto".to_string());
    let candidates = crate::user_memory::extract_user_fact_candidates(
        // Brain Live ON ⇒ the LOCAL light engine (user facts stop egressing); OFF ⇒ today's reasoner.
        reasoner.as_ref(),
        title,
        markdown,
        &typed_notes,
        &thread_turns,
        &note_language,
    );
    if candidates.is_empty() {
        return Ok(()); // nothing to reconcile — common in the default (no-model) build.
    }
    let _lifecycle = lifecycle_guard(state);
    require_current_meeting_content_snapshot_under_lifecycle(state, meeting_id, visibility)?;
    // 2) Existing user facts (all of them — the reconcile input).
    let existing = state.db.user_facts_all()?;
    // 3) Deterministic reconcile at the meeting's time (valid-time origin).
    let at = state
        .db
        .get_meeting(meeting_id)?
        .map(|m| m.started_at)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let mut ops = crate::facts::reconcile_facts(&existing, &candidates, &at);
    // 4) Stamp the source meeting (gating + purge anchor) and apply atomically.
    crate::facts::set_meeting_id(&mut ops, meeting_id);
    state.db.apply_user_fact_ops(&ops)?;
    Ok(())
}

/// Whether cross-meeting USER MEMORY is enabled (config `user_memory_enabled`, default TRUE). When
/// OFF: no extraction runs, no brief is injected into ANY surface, and `get_user_memory` reports the
/// disabled marker. Fail-safe: a poisoned config mutex reports ENABLED (the default) so a transient
/// lock error never silently disables the feature.
pub(crate) fn user_memory_enabled(state: &AppState) -> bool {
    state
        .config
        .lock()
        .map(|c| c.user_memory_enabled)
        .unwrap_or(true)
}

/// The gated cross-meeting USER MEMORY brief for injection into the non-agentic Ask / meeting-chat
/// surfaces (design spec: parity with the @brain agentic loop, which already injects it). It is
/// DERIVED data — never sealed, always REGENERATED from the currently-VISIBLE user facts under the
/// passed `unlocked` snapshot — so a sealed-not-unlocked meeting's user facts inject NOTHING. When
/// memory is disabled (config `user_memory_enabled == false`) it returns EMPTY, so the prompt is
/// byte-identical to the pre-memory prompt. Rides the EXISTING redaction + consent egress of the
/// surface it is injected into — no new egress class.
///
/// Brain v2 L2.2: `query` is the user's question when the surface has one in hand — the brief is
/// then RELEVANCE-FILTERED (BM25 top-k over the SAME visible set, `build_memory_brief`); an empty
/// query or zero hits falls back to the full-list brief (behavior-preserving).
pub(crate) fn gated_memory_brief_for_injection(
    state: &AppState,
    unlocked: &std::collections::HashSet<String>,
    query: &str,
) -> String {
    if !user_memory_enabled(state) {
        return String::new();
    }
    crate::user_memory::build_memory_brief(&state.db, query, unlocked)
}
