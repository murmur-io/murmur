//! NOTE-ENRICHMENT / VERIFY / SELECTION-ASSISTANT / SUPERSESSION / MEMORY-IMPORT command surface
//! (a GATED domain — every reader of meeting/note content keeps its gate, every note WRITE keeps its
//! write-gate + seal-on-write).
//!
//! Extracted verbatim from `commands/mod.rs` (God-file split, PURE MOVE — every gate/seal/guard body
//! is byte-identical, only relocated + visibility widened where a RETAINED test reaches a helper via
//! `super::…`). The clusters:
//!   - VERIFY (`verify_note_sources`, `apply_note_verify_markers`): read-GATE (`meeting_is_unlocked`
//!     → `AppError::Locked`), then the WRITE persists via the shared seal-on-write upsert
//!     (`upsert_note_reseal_if_locked`) so the callout SEALS with the note; the RAM-only verify cache
//!     is cleared on relock.
//!   - ENRICH (`enrich_note_context`, `apply_note_enrichment`): read-GATE before ANY connector egress;
//!     the write refuses a SEALED note and re-seals a session-unlocked LOCKED folder's fresh markdown.
//!   - SELECTION ASSISTANT (`note_assistant_action`): the five-way gate (action-enabled →
//!     note-unlocked via `folder_is_unlocked` → brain-grounded retrieval contributes only
//!     visible/unlocked sources via the `*_visible` readers → cloud path rides `provider_for`'s
//!     firewall+consent) — byte-identical, only relocated.
//!   - TASKS (`patch_note_tasks`): WRITE-GATE + seal-on-write, byte-identical.
//!   - SUPERSESSION / Re-Truth (`preview_supersessions`, `apply_supersessions`, `undo_supersessions`):
//!     each row is GATED on both the SOURCE (`source_is_stampable`) and the SUPERSEDING side
//!     (`meeting_is_unlocked`); apply RE-GATES at write time (the prune↔seal TOCTOU discipline) under
//!     the shared lifecycle guard, snapshots each note's pristine bytes for byte-identical undo.
//!   - MEMORY IMPORT (`import_memories`): ZERO egress (LOCAL-or-stub reasoner only); creates the
//!     synthetic anchor meeting only after reconcile finds ≥1 Add so a no-op import leaves nothing.
//!
//! SHARED helpers STAY in `commands/mod.rs` and are reached via `use super::*` (a `commands`
//! submodule sees its parent's private items, exactly like the sibling `commands/analytics.rs`):
//! the note-write/seal/gate web (`upsert_note_reseal_if_locked`, `overwrite_exported_note_guarded`,
//! `meeting_is_unlocked`, `folder_is_unlocked`, `lifecycle_guard`, `set_timeline_data_reseal_if_locked`,
//! `index_wikilinks_best_effort`, `auto_link_semantic_best_effort`), the SUPERSESSION gate helpers
//! (`source_is_stampable`, `folder_locked_on_disk`, `note_file_for`) which the sibling `analytics.rs`
//! also calls, and the memory helpers (`import_extraction_reasoner`, `user_memory_enabled`) which the
//! siblings `ask.rs`/`facts.rs` own. The enrich-ONLY helpers/DTOs (`ENRICH_SEARCH_HITS_PER_CONNECTOR`,
//! the whole note-assist helper web, `pristine_note_bytes`, `superseding_link_stem`, `SupersessionDto`,
//! `ApplyResult`) move here. The note-assist helpers a retained lifecycle test calls
//! (`note_assist_shape`, `note_assist_retrieval`, `NoteAssistRetrieval`, `gather_note_enhance_citations`,
//! `build_note_assist_prompt`, `note_edit_word_count`, `note_edit_max_tokens`,
//! `note_assistant_action_impl`) are promoted from private to `pub(crate)` so the glob re-export keeps
//! them reachable at `super::…` — the ONLY change to any moved item is that visibility widening.
//!
//! Bound as `enrich_commands` (via `#[path]`) to avoid colliding with the crate-level `crate::enrich`
//! module these commands call (E0255). The glob re-export makes every moved command resolve UNCHANGED
//! at `crate::commands::…` for `generate_handler!` in `lib.rs` and every caller (incl. the STAYING
//! test modules).

use super::*;

/// VERIFY PASS (read-only): extract Jira issue keys from the meeting's note and check each against
/// LIVE Jira. GATED: sealed-not-unlocked meetings refuse (a verify against a blanked note would be
/// nonsense AND a read-gate bypass). Consent-gated: rides the Jira connector's enable+consent+key
/// gate (fail-closed `NeedsConsent` maps to `AppError::Unavailable`). NEVER called proactively —
/// FE-invoked only. Findings are computed against the note WITH OLD MARKERS STRIPPED so line
/// numbers line up with `apply_verify_markers`' post-strip numbering.
#[tauri::command]
pub async fn verify_note_sources(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<crate::verify::VerifyFinding>, AppError> {
    verify_note_sources_inner(state.inner(), meeting_id).await
}

pub(crate) async fn verify_note_sources_inner(
    state: &AppState,
    meeting_id: String,
) -> Result<Vec<crate::verify::VerifyFinding>, AppError> {
    let visibility = capture_meeting_content_snapshot(state, &meeting_id)?;
    // Brain v2 L5 — SESSION verify cache, checked AFTER the read gate (the gate is never skipped
    // for a cache hit). A hit re-renders the panel without a second Jira egress; the cache is
    // RAM-only and cleared on relock_folder / relock_all, so it never outlives the session unlock.
    let cached = state
        .verify_cache
        .lock()
        .map_err(|_| AppError::Other(anyhow::anyhow!("verify cache lock")))?
        .get(&meeting_id)
        .cloned();
    if let Some(cached) = cached {
        require_current_meeting_content_snapshot(state, &meeting_id, &visibility)?;
        return Ok(cached);
    }
    let note = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    // Strip our own old CALLOUT first (its body lines carry issue keys of their own), THEN the
    // inline markers, so extraction/judgment sees the canonical note lines and line numbers line
    // up with `apply_verify_markers`' post-strip numbering.
    let base = crate::verify::apply_verify_callout(&note.markdown, &[], "");
    let stripped = crate::verify::apply_verify_markers(&base, &[]);
    let keys = crate::verify::extract_issue_keys(&stripped);
    if keys.is_empty() {
        require_current_meeting_content_snapshot(state, &meeting_id, &visibility)?;
        return Ok(Vec::new());
    }
    let config = state
        .config
        .lock()
        .map_err(|_| AppError::Other(anyhow::anyhow!("config lock")))?
        .clone();
    let registry = crate::connectors::ConnectorRegistry::build(&config);
    let lines: Vec<&str> = stripped.lines().collect();
    let mut findings = Vec::with_capacity(keys.len());
    for (line_no, key) in keys {
        let snap = registry.jira_lookup(&key).await.map_err(AppError::from)?;
        let line_text = lines.get(line_no - 1).copied().unwrap_or("");
        let (verdict, detail) = crate::verify::judge(line_text, &key, snap.as_ref());
        let url = snap.map(|s| s.url).unwrap_or_default();
        findings.push(crate::verify::VerifyFinding {
            line_no,
            key,
            verdict,
            detail,
            url,
        });
    }
    // Populate the session cache (RAM-only; cleared on relock). A poisoned lock only skips the
    // cache — the findings still return.
    let _lifecycle = lifecycle_guard(state);
    require_current_meeting_content_snapshot_under_lifecycle(state, &meeting_id, &visibility)?;
    if let Ok(mut cache) = state.verify_cache.lock() {
        cache.insert(meeting_id, findings.clone());
    }
    Ok(findings)
}

/// Apply verify markers to the note (WRITE — same gate + save/re-export tail as `update_note`).
/// Takes the findings the user just reviewed in the panel; validates every key's strict shape.
#[tauri::command]
pub fn apply_note_verify_markers(
    state: State<'_, AppState>,
    meeting_id: String,
    findings: Vec<crate::verify::VerifyFinding>,
) -> Result<NoteDto, AppError> {
    apply_note_verify_markers_inner(state.inner(), meeting_id, findings)
}

pub(crate) fn apply_note_verify_markers_inner(
    state: &AppState,
    meeting_id: String,
    findings: Vec<crate::verify::VerifyFinding>,
) -> Result<NoteDto, AppError> {
    // BLK-1 / TOCTOU (2026-07-10 audit F4): hold the lifecycle guard across gate+write so a
    // concurrent relock/seal cannot land between the unlock check and the upsert.
    let _lifecycle = lifecycle_guard(state);
    if !meeting_is_unlocked(state, &meeting_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
                crate::errcode::MEETING_LOCKED,
                "this meeting's folder is locked — unlock it to edit the note",
            )));
    }
    for f in &findings {
        let ok = crate::verify::extract_issue_keys(&f.key)
            .first()
            .map(|(_, k)| k == &f.key)
            .unwrap_or(false);
        if !ok {
            return Err(AppError::InvalidArg("invalid issue key in findings".into()));
        }
    }
    let existing = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    // Brain v2 L5 — strip our own old CALLOUT first (so `apply_verify_markers`' internal
    // marker-strip numbering matches the findings, which were computed against the
    // callout-stripped note), apply the inline markers, then append the fresh consolidated
    // `> [!verify]-` callout dated now. All three regions are self-managed + idempotent.
    let base = crate::verify::apply_verify_callout(&existing.markdown, &[], "");
    let marked = crate::verify::apply_verify_markers(&base, &findings);
    let as_of = chrono::Utc::now().to_rfc3339();
    let marked = crate::verify::apply_verify_callout(&marked, &findings, &as_of);
    // Save + re-export — the exact `update_note` tail, with `marked`. Persisting via the
    // seal-on-write upsert keeps the callout in the CANONICAL DB note markdown so it SEALS with the
    // note under the folder lock (the enrich.rs persistence lesson — and, for a session-unlocked
    // LOCKED folder, the fresh markdown is re-sealed into `content_blob` in the same write); the
    // vault `.md` re-export follows when one exists.
    let created_at = chrono::Utc::now().to_rfc3339();
    upsert_note_reseal_if_locked(
        state,
        &NoteRecord {
            meeting_id: meeting_id.clone(),
            provider_id: existing.provider_id.clone(),
            markdown: marked.clone(),
            created_at,
            exported_path: existing.exported_path.clone(),
            model_requested: existing.model_requested.clone(),
            model_served: existing.model_served.clone(),
            gateway_host: existing.gateway_host.clone(),
        },
    )?;
    if let Some(path) = existing.exported_path.as_deref() {
        overwrite_exported_note_guarded(state, &meeting_id, &existing.provider_id, path, &marked)?;
    }
    Ok(NoteDto {
        meeting_id,
        provider_id: existing.provider_id,
        markdown: marked,
        exported_path: existing.exported_path,
    })
}

/// `enrich_note_context(meeting_id) -> Vec<ContextHit>` — CONNECTOR-AGNOSTIC preview of live context
/// to fold into the note. Read side: gathers hits from EVERY exposed (enabled + consented + keyed)
/// connector via the same registry the brain uses. Two modes (see the research brief):
/// - **Identifier lookup** (precise, minimal egress): Jira issue keys already in the note → live
///   `jira_lookup`. Only a validated `PROJ-123` leaves the Mac — never note content.
/// - **Free-text search** (fuzzy): every OTHER exposed connector (Slack/web) is searched for the
///   meeting's TITLE, through the framework's redaction + content-free egress ledger.
///
/// This is the EGRESS moment (an explicit user action, like `verify_note_sources`); the returned
/// hits are reviewed in the FE and only WRITTEN by `apply_note_enrichment`. Lock-gated: a
/// sealed-not-unlocked meeting refuses BEFORE any connector call. Empty vec = nothing to add.
#[tauri::command]
pub async fn enrich_note_context(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<crate::enrich::ContextHit>, AppError> {
    enrich_note_context_inner(state.inner(), meeting_id).await
}

/// How many free-text search hits to keep per connector (bounds egress-result noise; the caller
/// still reviews + can drop each before applying).
const ENRICH_SEARCH_HITS_PER_CONNECTOR: usize = 3;

pub(crate) async fn enrich_note_context_inner(
    state: &AppState,
    meeting_id: String,
) -> Result<Vec<crate::enrich::ContextHit>, AppError> {
    // READ-GATE FIRST + bind the folder/epoch before ANY connector egress.
    let visibility = capture_meeting_content_snapshot(state, &meeting_id)?;
    let note = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    // Clean base = the note with our own prior context block stripped, so key-extraction sees the
    // canonical prose (never our appended callout).
    let base = crate::enrich::apply_context_markers(&note.markdown, &[], "");
    let title = state
        .db
        .get_meeting(&meeting_id)?
        .and_then(|m| m.title)
        .unwrap_or_default();

    let config = state
        .config
        .lock()
        .map_err(|_| AppError::Other(anyhow::anyhow!("config lock")))?
        .clone();
    let registry = crate::connectors::ConnectorRegistry::build(&config);

    let mut hits: Vec<crate::enrich::ContextHit> = Vec::new();

    // ── Identifier-lookup mode (precise): Jira issue keys → live status. Egresses only the key. ──
    if registry.has("jira") {
        for (_line, key) in crate::verify::extract_issue_keys(&base) {
            if let Ok(Some(snap)) = registry.jira_lookup(&key).await {
                let mut detail = format!("{} · {}", snap.key, snap.status);
                if let Some(due) = snap.due.as_deref().filter(|d| !d.is_empty()) {
                    detail.push_str(&format!(" · due {due}"));
                }
                if !snap.summary.is_empty() {
                    detail.push_str(&format!(" — {}", snap.summary));
                }
                hits.push(crate::enrich::ContextHit {
                    source: "Jira".to_string(),
                    detail,
                    url: Some(snap.url).filter(|u| !u.is_empty()),
                });
            }
        }
    }

    // ── Free-text search mode (fuzzy): every OTHER exposed connector, queried on the meeting title.
    // The query is redacted + ledgered by the registry; skip when there is no title to search on. ──
    if !title.trim().is_empty() {
        for id in registry.ids() {
            if id == "jira" {
                continue; // handled precisely above — never double-pull Jira.
            }
            if let Ok(results) = registry.search(id, &title).await {
                for hit in results.into_iter().take(ENRICH_SEARCH_HITS_PER_CONNECTOR) {
                    let detail = if hit.snippet.trim().is_empty() {
                        hit.title
                    } else {
                        format!("{} — {}", hit.title, hit.snippet)
                    };
                    hits.push(crate::enrich::ContextHit {
                        // Loud attribution from the connector itself (e.g. "Slack", "web · Brave").
                        source: hit.source_label,
                        detail,
                        url: Some(hit.url).filter(|u| !u.is_empty()),
                    });
                }
            }
        }
    }

    require_current_meeting_content_snapshot(state, &meeting_id, &visibility)?;
    Ok(hits)
}

/// `apply_note_enrichment(meeting_id, hits) -> NoteDto` — WRITE the reviewed context hits into the
/// note as one consolidated `> [!context]-` callout (dated now), via the EXACT `update_note` save +
/// re-export tail — so it persists in the CANONICAL DB note markdown and SEALS with the note under
/// the folder lock (NOT the vault-file-only path). No egress here (the hits were fetched by
/// `enrich_note_context`). Lock-gated. Passing an empty `hits` STRIPS the block (byte-exact undo).
#[tauri::command]
pub fn apply_note_enrichment(
    state: State<'_, AppState>,
    meeting_id: String,
    hits: Vec<crate::enrich::ContextHit>,
) -> Result<NoteDto, AppError> {
    apply_note_enrichment_inner(state.inner(), meeting_id, hits)
}

pub(crate) fn apply_note_enrichment_inner(
    state: &AppState,
    meeting_id: String,
    hits: Vec<crate::enrich::ContextHit>,
) -> Result<NoteDto, AppError> {
    // BLK-1 / TOCTOU: hold the lifecycle guard across the whole check-then-write so a concurrent seal
    // cannot slip between the gate and the `upsert_note`+`overwrite_note` (same leak class Lane A's
    // `link_related_notes_inner` guards). Lock order `lifecycle ⊃ db`.
    let _lifecycle = lifecycle_guard(state);
    if !meeting_is_unlocked(state, &meeting_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
                crate::errcode::MEETING_LOCKED,
                "this meeting's folder is locked — unlock it to edit the note",
            )));
    }
    // SEAL-SAFETY GATE (mirrors Lane A): a SEALED note (any provider row carries a content_blob) has a
    // TRANSIENT `markdown` column — blanked on relock, restored from `content_blob` on unlock. Writing
    // enriched markdown into it would be silently dropped on the next relock (content_blob is
    // canonical), and — for the auto-file-into-locked case where the column is blank but the folder is
    // session-unlocked — could re-materialize plaintext into a sealed note. So refuse enrichment on a
    // sealed note even when the session has it unlocked.
    let sealed = state
        .db
        .sealable_notes_for_meeting(&meeting_id)?
        .iter()
        .any(|n| n.content_blob.is_some());
    if sealed {
        return Err(AppError::Locked(
            "this meeting's note is sealed — enrichment can't be persisted while locked".into(),
        ));
    }
    let existing = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    let as_of = chrono::Utc::now().to_rfc3339();
    let enriched = crate::enrich::apply_context_markers(&existing.markdown, &hits, &as_of);
    let created_at = chrono::Utc::now().to_rfc3339();
    // Seal-on-write seam (audit F1): the sealed case was refused above, but a LOCKED folder whose
    // note was never sealed (auto-filed while session-unlocked) still re-seals the fresh markdown.
    upsert_note_reseal_if_locked(
        state,
        &NoteRecord {
            meeting_id: meeting_id.clone(),
            provider_id: existing.provider_id.clone(),
            markdown: enriched.clone(),
            created_at,
            exported_path: existing.exported_path.clone(),
            model_requested: existing.model_requested.clone(),
            model_served: existing.model_served.clone(),
            gateway_host: existing.gateway_host.clone(),
        },
    )?;
    if let Some(path) = existing.exported_path.as_deref() {
        overwrite_exported_note_guarded(
            state,
            &meeting_id,
            &existing.provider_id,
            path,
            &enriched,
        )?;
    }
    Ok(NoteDto {
        meeting_id,
        provider_id: existing.provider_id,
        markdown: enriched,
        exported_path: existing.exported_path,
    })
}

// ── NOTES — selection Brain-assistant (WP4) ──────────────────────────────────────────────────────
//
// The editor's selection popover calls `note_assistant_action`. Refine/Shorten rewrite the
// selection; Enhance retrieves related brain context (VISIBLE sources only, excluding the current
// note) and proposes an ADDITIVE passage with citations. Routing is `provider_for(Role::Notes)` —
// which gives local-Qwen-vs-cloud-Claude selection, the fail-closed consent gate, the
// `RedactingProvider` firewall, and the egress ledger FOR FREE — never a direct provider build.

/// RAII guard for ONE note-assist turn (residual W7 — the command-surface twin of
/// `transcribe::live::TurnGuard`): on drop — normal return, gate refusal, provider error, or a
/// panic unwinding through the async body — it decrements the per-note in-flight counter and,
/// when THIS turn raised it (`priority`, set only for a LOCAL decode), clears the user-turn
/// priority flag. The conditional clear means a concurrent live turn's flag is never stomped.
struct NoteAssistTurnGuard<'a> {
    state: &'a AppState,
    key: String,
    priority: bool,
}
impl Drop for NoteAssistTurnGuard<'_> {
    fn drop(&mut self) {
        if self.priority {
            self.state
                .user_turn_in_progress
                .store(false, std::sync::atomic::Ordering::Relaxed);
        }
        crate::transcribe::live::end_turn(&self.state.in_flight_turns, &self.key);
    }
}

/// The FULL known note-assistant action set (the FE catalog mirror). `custom` is always available
/// (the escape hatch) so it is intentionally OUTSIDE this list — it is handled explicitly. An action
/// not in this list and not `custom` is an unknown id → `InvalidArg`.
const NOTE_ASSIST_KNOWN_ACTIONS: &[&str] = &[
    // EDIT (replace)
    "refine",
    "grammar",
    "shorten",
    "expand",
    "simplify",
    "tone",
    "translate",
    // STRUCTURE
    "bullets",
    "table",
    "keypoints",
    // FROM YOUR BRAIN
    "enhance",
    "find_related",
    "link_entities",
    "fact_check",
    "ask",
    // EXTRACT
    "action_items",
    "decisions",
    // CREATE (artifact)
    "draft_followup",
    "spinoff_note",
];

/// The result shape the FE renders + applies off (see the seam contract table). MUST be one of
/// `"replace" | "insert" | "info" | "artifact"`. `custom` is a free-text replace.
pub(crate) fn note_assist_shape(action: &str) -> &'static str {
    match action {
        // EDIT + link_entities + custom rewrite the selection in place.
        "refine" | "grammar" | "shorten" | "expand" | "simplify" | "tone" | "translate"
        | "bullets" | "table" | "link_entities" | "custom" => "replace",
        // Keeps the text; appends after the selection.
        "keypoints" | "enhance" | "action_items" | "decisions" => "insert",
        // Read-only grounded answer + citations (no destructive edit).
        "find_related" | "fact_check" | "ask" => "info",
        // A drafted email/note (title + body).
        "draft_followup" | "spinoff_note" => "artifact",
        // Unknown ids never reach here (gated to InvalidArg upstream); default to the safest
        // non-destructive shape.
        _ => "info",
    }
}

/// Which citation-gathering strategy an action uses. Grounded brain actions reuse the enhance
/// readers (visibility-gated); `link_entities` uses the gated entity list; everything else needs
/// no retrieval.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoteAssistRetrieval {
    /// No retrieval (pure edit on the selection).
    None,
    /// The enhance readers: `search_visible` + `search_doc_chunks_*_visible` (excluding this note).
    BrainCitations,
    /// The gated entity list (`list_entities_visible`) → which names to wikilink.
    Entities,
}

pub(crate) fn note_assist_retrieval(action: &str) -> NoteAssistRetrieval {
    match action {
        "enhance" | "find_related" | "fact_check" | "ask" => NoteAssistRetrieval::BrainCitations,
        "link_entities" => NoteAssistRetrieval::Entities,
        _ => NoteAssistRetrieval::None,
    }
}

/// The selection Brain-assistant action. GATED five ways (normative order): (1) the action must be
/// ENABLED in config (else `Unavailable`); (2) the note's folder must be unlocked (never send a
/// sealed note's text off-device / to any model, else `Locked`); (3) brain-grounded retrieval
/// contributes ONLY visible/unlocked sources; (4) the cloud path rides the redaction firewall via
/// `provider_for`. `find_related` is retrieval-ONLY (no provider, no egress). Returns the suggestion
/// + citations + display metadata (modelLabel/mode/redacted/shape/title).
#[tauri::command]
pub async fn note_assistant_action(
    state: State<'_, AppState>,
    req: NoteAssistRequest,
) -> Result<NoteAssistResult, AppError> {
    note_assistant_action_inner(state.inner(), req).await
}

/// Core of [`note_assistant_action`] over `&AppState` (unit-testable headless). The gate order is
/// normative: config-enabled → note-unlocked → build provider (consent/firewall) → retrieve → call.
pub(crate) async fn note_assistant_action_inner(
    state: &AppState,
    req: NoteAssistRequest,
) -> Result<NoteAssistResult, AppError> {
    // Production path: build the NOTES-role provider through the full egress gate chain.
    note_assistant_action_impl(state, req, None).await
}

/// The testable core of [`note_assistant_action_inner`]. `provider_override` lets a unit test inject
/// a scripted fake provider (so the provider-dependent actions run headless without shelling out to
/// a real LLM); in production it is `None` and the NOTES-role provider is built via `provider_for`
/// (consent gate + redaction firewall + egress ledger). The gate order is IDENTICAL either way — the
/// override only replaces the provider CONSTRUCTION at step (6), never a gate.
pub(crate) async fn note_assistant_action_impl(
    state: &AppState,
    req: NoteAssistRequest,
    provider_override: Option<std::sync::Arc<dyn crate::summarize::provider::SummarizerProvider>>,
) -> Result<NoteAssistResult, AppError> {
    let action = req.action.trim().to_lowercase();
    // (1) ACTION ENABLED? A disabled action is refused BEFORE any read/egress.
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    // The 3 legacy actions keep their own bools (backward compat — the FE still sends all three).
    // `custom` is the always-on escape hatch. Every OTHER KNOWN action is enabled UNLESS the user
    // opted it OUT (`note_assist_actions_off`). An id that is neither known nor `custom` → InvalidArg
    // BEFORE any read/egress.
    let enabled = match action.as_str() {
        "refine" => config.note_assist_refine,
        "shorten" => config.note_assist_shorten,
        "enhance" => config.note_assist_enhance,
        "custom" => true,
        other if NOTE_ASSIST_KNOWN_ACTIONS.contains(&other) => {
            !config.note_assist_actions_off.iter().any(|a| a == other)
        }
        other => {
            return Err(AppError::InvalidArg(format!(
                "unknown note-assistant action: {other}"
            )));
        }
    };
    if !enabled {
        return Err(AppError::Unavailable(format!(
            "the {action} note action is turned off in Settings"
        )));
    }
    if req.selection.trim().is_empty() {
        return Err(AppError::InvalidArg("no text selected".into()));
    }

    // TURN DISCIPLINE (residual W7 — Brain v2 P0.3 parity with `spawn_assistant_turn`): at most ONE
    // note-assist turn per note id at a time. A second call while one is in flight is refused (the
    // double-click pile-up guard), so duplicate decodes never stack generations on shared Metal.
    // The key is namespaced (`note-assist:<id>`) so it can never collide with the live loop's
    // meeting-id keys. Opaque id only — no PII.
    let turn_key = format!("note-assist:{}", req.note_id);
    if !crate::transcribe::live::try_begin_turn(&state.in_flight_turns, &turn_key) {
        return Err(AppError::Unavailable(
            "the note assistant is already working on this note — wait for it to finish".into(),
        ));
    }
    // RAII: released on EVERY exit path below (gate refusal, provider error, success) — a wedged
    // key can never permanently refuse this note. Mirrors `transcribe::live::TurnGuard`.
    let mut turn = NoteAssistTurnGuard {
        state,
        key: turn_key,
        priority: false,
    };

    // (2) READ-GATE: resolve only the content-free folder anchor first, then select title/body while
    // holding the lifecycle mutex. Bind the snapshot to both folder identity and seal epoch because
    // provider inference below awaits and can span a relock or open-folder move.
    let (row, note_folder_id, note_seal_epoch) = {
        let _lifecycle = lifecycle_guard(state);
        let Some((folder_id, _created_at, _updated_at)) =
            state.db.note_gate_anchor(&req.note_id)?
        else {
            return Err(AppError::InvalidArg(format!("no note {}", req.note_id)));
        };
        if !folder_is_unlocked(state, &folder_id)? {
            return Err(AppError::Locked(
                "this note is locked — unlock its folder to use the assistant".into(),
            ));
        }
        let row = state
            .db
            .get_note_row(&req.note_id)?
            .ok_or_else(|| AppError::InvalidArg(format!("no note {}", req.note_id)))?;
        (
            row,
            folder_id,
            state.seal_epoch.load(std::sync::atomic::Ordering::SeqCst),
        )
    };

    let shape = note_assist_shape(&action).to_string();
    let retrieval = note_assist_retrieval(&action);

    // (3) FIND_RELATED: retrieval-ONLY — NO provider, NO egress. Gather visible citations through
    //     the SAME gated readers as enhance, build a one-line answer, and return `shape="info"`
    //     with `mode="local"`, `redacted=false`. This never raises the local user-turn priority
    //     flag (no decode contends for Metal) and never calls a model → a privacy win.
    if action == "find_related" {
        let citations = gather_note_enhance_citations(state, &config, &req)?;
        let suggestion = match citations.len() {
            0 => "No related sources found in your brain.".to_string(),
            1 => "1 related source in your brain.".to_string(),
            n => format!("{n} related sources in your brain."),
        };
        tracing::info!(
            target: "notes",
            action = %action,
            mode = "local",
            citations = citations.len(),
            redacted = false,
            "note assistant action completed (retrieval-only)"
        );
        require_current_note_assist_snapshot(
            state,
            &req.note_id,
            &note_folder_id,
            note_seal_epoch,
        )?;
        return Ok(NoteAssistResult {
            action,
            suggestion,
            citations,
            model_label: "Your brain (local search)".to_string(),
            mode: "local".to_string(),
            redacted: false,
            shape,
            title: None,
        });
    }

    // (4) Resolve the display metadata (modelLabel/mode) from the RESOLVED target BEFORE the call.
    let target =
        crate::summarize::roles::provider_target(crate::summarize::roles::Role::Notes, &config);
    let mode = if crate::summarize::egress_is_cloud(&target.connection, &config) {
        "cloud"
    } else {
        "local"
    };
    // USER-TURN PRIORITY (residual W7): a LOCAL note-assist decode contends for the on-device
    // engine (shared Metal) — raise the priority flag for the turn's duration so the background
    // Realtime-Reactions scan defers, exactly like the live loop's assistant turns. The guard
    // clears it on every exit path; a cloud call never touches the flag.
    if mode == "local" {
        state
            .user_turn_in_progress
            .store(true, std::sync::atomic::Ordering::Relaxed);
        turn.priority = true;
    }
    let model_requested = crate::summarize::effective_model_requested(&target, &config);
    let conn_label = crate::summarize::roles::connection_display_name(&target.connection);
    let model_label = if model_requested.trim().is_empty() {
        conn_label.to_string()
    } else {
        format!("{conn_label} · {model_requested}")
    };

    // (5) Retrieve VISIBLE grounding for the brain-grounded actions (EXCLUDING this note). Both the
    //     citation readers and `list_entities_visible` push the session unlock set through
    //     `visibility_clause`, so a sealed source never grounds a result.
    let (citations, entity_names) = match retrieval {
        NoteAssistRetrieval::BrainCitations => (
            gather_note_enhance_citations(state, &config, &req)?,
            Vec::new(),
        ),
        NoteAssistRetrieval::Entities => {
            let unlocked = unlocked_snapshot(state)?;
            let names: Vec<String> = state
                .db
                .list_entities_visible(&unlocked)?
                .into_iter()
                .map(|n| n.name)
                .collect();
            (Vec::new(), names)
        }
        NoteAssistRetrieval::None => (Vec::new(), Vec::new()),
    };

    // (6) Build the prompts, then call the NOTES-role provider (consent gate + redaction firewall +
    //     egress ledger ride inside `provider_for`/`complete_with_meta_opts`). The edit runs under a
    //     per-action token cap + low temperature (`GenOptions::edit_rewrite`) so a compression edit
    //     can't run away and LENGTHEN, and `generate_note_edit` enforces "shorten is actually shorter"
    //     with one stricter retry.
    let (system, user) = build_note_assist_prompt(
        &action,
        &req,
        &citations,
        &entity_names,
        &config.note_language,
    );
    let provider = match provider_override {
        Some(p) => p,
        None => crate::summarize::provider_for(
            crate::summarize::roles::Role::Notes,
            &config,
            &state.heavy_inference,
        )?,
    };
    let opts = crate::reason::GenOptions::edit_rewrite(note_edit_max_tokens(
        &action,
        req.selection.chars().count(),
    ));
    let input_words = note_edit_word_count(&req.selection);
    require_current_note_assist_snapshot(state, &req.note_id, &note_folder_id, note_seal_epoch)?;
    let (mut suggestion, meta) = generate_note_edit(
        provider.as_ref(),
        &action,
        &system,
        &user,
        opts,
        input_words,
    )
    .await?;
    require_current_note_assist_snapshot(state, &req.note_id, &note_folder_id, note_seal_epoch)?;
    suggestion = suggestion.trim().to_string();

    // Artifacts carry a title (email subject / note title). Derive it from the note's own title for
    // a follow-up draft, and from the drafted body's first line for a spin-off note (a title only —
    // never logged). Non-artifacts have no title.
    let title = if shape == "artifact" {
        Some(derive_artifact_title(
            &action,
            row.title.as_deref().unwrap_or(""),
            &suggestion,
        ))
    } else {
        None
    };

    // `redacted` = the firewall scrubbed at least one PII token on THIS call (only a cloud
    // RedactingProvider populates `meta.redactions`; a local provider leaves it None → false).
    let redacted = meta
        .redactions
        .as_ref()
        .map(|r| r.email + r.card + r.phone + r.name > 0)
        .unwrap_or(false);

    tracing::info!(
        target: "notes",
        action = %action,
        mode = %mode,
        shape = %shape,
        citations = citations.len(),
        redacted,
        "note assistant action completed"
    );

    Ok(NoteAssistResult {
        action,
        suggestion,
        citations,
        model_label,
        mode: mode.to_string(),
        redacted,
        shape,
        title,
    })
}

fn require_current_note_assist_snapshot(
    state: &AppState,
    note_id: &str,
    expected_folder_id: &str,
    expected_seal_epoch: u64,
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    if state.seal_epoch.load(std::sync::atomic::Ordering::SeqCst) != expected_seal_epoch {
        return Err(AppError::Locked(
            "this note was locked while the assistant was working — unlock it and retry".into(),
        ));
    }
    let Some((folder_id, _created_at, _updated_at)) = state.db.note_gate_anchor(note_id)? else {
        return Err(AppError::InvalidArg(format!("no note {note_id}")));
    };
    if folder_id != expected_folder_id || !folder_is_unlocked(state, &folder_id)? {
        return Err(AppError::Locked(
            "this note moved or was locked while the assistant was working — retry".into(),
        ));
    }
    Ok(())
}

/// Derive a non-PII-in-logs artifact title. `draft_followup` reuses the note's own title as an email
/// subject; `spinoff_note` uses the drafted body's first non-empty line (stripped of leading `#`),
/// falling back to the note title. The returned string is user-facing content (a title) — it is NEVER
/// logged.
fn derive_artifact_title(action: &str, note_title: &str, body: &str) -> String {
    let fallback = |t: &str| {
        let t = t.trim();
        if t.is_empty() {
            "Untitled".to_string()
        } else {
            t.to_string()
        }
    };
    match action {
        "draft_followup" => {
            let subj = note_title.trim();
            if subj.is_empty() {
                "Follow-up".to_string()
            } else {
                format!("Re: {subj}")
            }
        }
        // spinoff_note: first meaningful body line as the title.
        _ => body
            .lines()
            .map(|l| l.trim_start_matches('#').trim())
            .find(|l| !l.is_empty())
            .map(|l| l.chars().take(120).collect::<String>())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| fallback(note_title)),
    }
}

/// enhance-context retrieval: run the GATED brain readers (meeting `search_visible` + document/note
/// `search_doc_chunks_*_visible`), EXCLUDE the current note's own document id, cap at ≤6, and build
/// [`NoteCitation`]s. Only VISIBLE/unlocked sources contribute (both readers push the live session
/// unlock set through `visibility_clause`), so a sealed source never grounds an enhancement.
pub(crate) fn gather_note_enhance_citations(
    state: &AppState,
    config: &AppConfig,
    req: &NoteAssistRequest,
) -> Result<Vec<NoteCitation>, AppError> {
    const MAX_CITATIONS: usize = 6;
    let unlocked = unlocked_snapshot(state)?;
    // The query is the selection plus a little surrounding context (better recall than the raw
    // selection alone); the readers tokenize/defuse it safely.
    let mut query = req.selection.clone();
    if let Some(b) = &req.before {
        query.push(' ');
        query.push_str(b);
    }
    if let Some(a) = &req.after {
        query.push(' ');
        query.push_str(a);
    }

    let mut out: Vec<NoteCitation> = Vec::new();

    // Meeting notes/segments (FTS over visible meetings).
    for hit in state
        .db
        .search_visible(&query, MAX_CITATIONS as i64, &unlocked)?
    {
        out.push(NoteCitation {
            kind: "meeting".into(),
            id: hit.meeting.id.clone(),
            title: hit
                .meeting
                .title
                .clone()
                .unwrap_or_else(|| "Meeting".into()),
            snippet: hit.snippet,
        });
        if out.len() >= MAX_CITATIONS {
            break;
        }
    }

    // Other notes/documents (semantic when the e5 model is present, else FTS). EXCLUDE the current
    // note's own document id (never cite the note being edited).
    if out.len() < MAX_CITATIONS {
        // Resolve one REAL, pinned query embedder instead of checking the model directory and then
        // constructing a potentially different/stub handle. Besides closing a settings/download
        // TOCTOU, this keeps keyword retrieval honest in test builds (where real Metal forwards are
        // deliberately disabled even when this Mac happens to have the model on disk).
        let semantic_embedder = if config.semantic_search_enabled {
            crate::embed::active_persistence_embedder().ok()
        } else {
            None
        };
        let doc_hits = if let Some(embedder) = semantic_embedder {
            let qvecs = embedder.embed_query(std::slice::from_ref(&query))?;
            match qvecs.into_iter().next() {
                Some(qvec) => state.db.search_doc_chunks_visible(
                    &qvec,
                    MAX_CITATIONS as i64,
                    0.0,
                    &unlocked,
                    None,
                )?,
                None => Vec::new(),
            }
        } else {
            state
                .db
                .search_doc_chunks_fts_visible(&query, MAX_CITATIONS as i64, &unlocked, None)?
        };
        for hit in doc_hits {
            if hit.document_id == req.note_id {
                continue; // never cite the note being edited.
            }
            out.push(NoteCitation {
                kind: "note".into(),
                id: hit.document_id,
                title: hit.name,
                snippet: hit.snippet,
            });
            if out.len() >= MAX_CITATIONS {
                break;
            }
        }
    }

    // Org shared brain (deliberately-disclosed colleague content — outside the folder-lock domain,
    // gated on membership only via `org_brain_available`, same seam as the `org_brain_search` agent
    // tool / MCP `org_search`). RETRIEVAL-ONLY: no provider call, no egress — this is a private,
    // user-navigated discovery surface, matching `find_related`'s zero-provider-call invariant.
    if out.len() < MAX_CITATIONS && crate::tools::org_brain_available(&state.db, config) {
        let org_hits = crate::tools::search_org_brain_hits(&state.db, config, &query)?;
        for hit in org_hits {
            out.push(NoteCitation {
                kind: "org".into(),
                id: hit.item_id,
                title: hit.title,
                snippet: hit.snippet,
            });
            if out.len() >= MAX_CITATIONS {
                break;
            }
        }
    }
    Ok(out)
}

/// Format the retrieved brain citations as a numbered grounding block for the grounded actions.
fn note_assist_grounding_block(citations: &[NoteCitation]) -> String {
    let mut grounding = String::new();
    for (i, c) in citations.iter().enumerate() {
        grounding.push_str(&format!(
            "[{n}] ({kind}) {title}: {snippet}\n",
            n = i + 1,
            kind = c.kind,
            title = c.title,
            snippet = c.snippet
        ));
    }
    if grounding.is_empty() {
        grounding.push_str("(no related material found)\n");
    }
    grounding
}

/// Build the (system, user) prompts for a note-assistant action. EDIT actions rewrite the selection
/// with its surrounding context; STRUCTURE reshapes it; the FROM-YOUR-BRAIN actions ground on the
/// retrieved citations (or entity names) ONLY; the INFO actions (`fact_check`/`ask`) produce an
/// ANSWER, not an edit; CREATE actions draft an artifact body. `note_language` steers the reply
/// language (matching the rest of the note stack). Every prompt passes the preceding text as
/// READ-ONLY context ("do NOT continue it") with the SELECTION LAST — the discipline that fixed
/// "shorten made it longer".
pub(crate) fn build_note_assist_prompt(
    action: &str,
    req: &NoteAssistRequest,
    citations: &[NoteCitation],
    entity_names: &[String],
    note_language: &str,
) -> (String, String) {
    let lang = if note_language.trim().is_empty() || note_language == "auto" {
        "the same language as the selected text".to_string()
    } else {
        format!("language code '{note_language}'")
    };
    // EDIT actions (refine/grammar/shorten/expand/simplify/tone) rewrite a passage the user
    // ALREADY wrote in some language — they must always match ITS language, never the global
    // `note_language` pin (that pin is for content GENERATED from scratch: the full note, the
    // STRUCTURE/FROM-YOUR-BRAIN/INFO/CREATE actions, and `translate`'s explicit target). Forcibly
    // translating during a surgical edit was the bug: Shorten on an English passage under a
    // Polish-pinned note_language rewrote it into Polish instead of shortening it in English.
    let edit_lang = "the same language as the selected text".to_string();
    let before = req.before.as_deref().unwrap_or("");
    let preceding = if before.trim().is_empty() {
        String::new()
    } else {
        format!(
            "Preceding text (READ-ONLY context — do NOT reproduce or continue it):\n{before}\n\n"
        )
    };
    let sel = req.selection.as_str();
    let variant = req.variant.as_deref().unwrap_or("").trim();
    let instruction = req.instruction.as_deref().unwrap_or("").trim();
    match action {
        "refine" => {
            let system = format!(
                "You refine a passage of the user's own note: improve clarity, grammar, and flow \
                 WITHOUT changing its meaning, adding facts, or padding its length. Reply in \
                 {edit_lang}. Output ONLY the rewritten passage — no preamble, no quotes, no \
                 explanation."
            );
            let user = format!("{preceding}PASSAGE TO REFINE (rewrite ONLY this):\n{sel}");
            (system, user)
        }
        "grammar" => {
            let system = format!(
                "You are a copy-editor. Correct ONLY spelling, grammar, and punctuation in the \
                 passage. Do NOT restructure, rephrase, shorten, lengthen, or change the meaning or \
                 word choice beyond what a correction requires. Reply in {edit_lang}. Output ONLY \
                 the corrected passage — no preamble, no quotes, no explanation."
            );
            let user = format!("{preceding}PASSAGE TO CORRECT (fix ONLY this):\n{sel}");
            (system, user)
        }
        "shorten" => {
            let system = format!(
                "You shorten a passage of the user's own note. Rewrite it in ABOUT HALF the \
                 sentences: keep every decision, number, name, date, and commitment; cut hedging, \
                 repetition, filler, and throat-clearing. The result MUST be shorter than the \
                 original. Reply in {edit_lang}. Output ONLY the shortened passage — no preamble, \
                 no quotes, no explanation.\n\nExample —\nOriginal: I think that, honestly, we should \
                 probably consider maybe moving the deadline to Friday, because the team is quite \
                 busy right now and there is a lot going on.\nShortened: Move the deadline to \
                 Friday — the team is overloaded."
            );
            let user =
                format!("{preceding}PASSAGE TO SHORTEN (rewrite ONLY this, shorter):\n{sel}");
            (system, user)
        }
        "expand" => {
            let system = format!(
                "You expand a terse passage of the user's own note into fuller, clearer prose. \
                 Elaborate ONLY on what is already stated — spell out shorthand, join fragments into \
                 sentences, add connective phrasing. Do NOT invent facts, opinions, numbers, names, \
                 or commitments that are not in the passage. Reply in {edit_lang}. Output ONLY the \
                 expanded passage — no preamble, no quotes, no explanation."
            );
            let user = format!("{preceding}PASSAGE TO EXPAND (rewrite ONLY this, fuller):\n{sel}");
            (system, user)
        }
        "simplify" => {
            let system = format!(
                "You rewrite a passage of the user's own note in plain, jargon-free language a \
                 non-expert can follow. Keep every fact, number, name, and decision; replace \
                 jargon and convoluted phrasing with simple words and short sentences. Do NOT add or \
                 remove information. Reply in {edit_lang}. Output ONLY the simplified passage — no \
                 preamble, no quotes, no explanation."
            );
            let user = format!("{preceding}PASSAGE TO SIMPLIFY (rewrite ONLY this):\n{sel}");
            (system, user)
        }
        "tone" => {
            let tone = if variant.is_empty() {
                "professional"
            } else {
                variant
            };
            let system = format!(
                "You rewrite a passage of the user's own note in a {tone} tone WITHOUT changing its \
                 meaning, facts, or length beyond what the tone requires. Reply in {edit_lang}. \
                 Output ONLY the rewritten passage — no preamble, no quotes, no explanation."
            );
            let user = format!(
                "{preceding}PASSAGE TO REWRITE in a {tone} tone (rewrite ONLY this):\n{sel}"
            );
            (system, user)
        }
        "translate" => {
            // For translate the TARGET language is the variant, overriding note_language.
            let target = if variant.is_empty() {
                "the language the user most likely wants".to_string()
            } else {
                variant.to_string()
            };
            let system = format!(
                "You translate a passage of the user's own note into {target}. Preserve meaning, \
                 tone, formatting, names, numbers, and any markdown. Do NOT add or omit content. \
                 Output ONLY the translated passage — no preamble, no quotes, no explanation."
            );
            let user = format!(
                "{preceding}PASSAGE TO TRANSLATE into {target} (translate ONLY this):\n{sel}"
            );
            (system, user)
        }
        "bullets" => {
            let system = format!(
                "You reformat a passage of the user's own note into a markdown bullet list. Turn \
                 each distinct point into its own `- ` line, preserving every fact, number, name, and \
                 decision; do NOT add, remove, or invent content. Reply in {lang}. Output ONLY the \
                 markdown list — no preamble, no heading, no explanation."
            );
            let user = format!(
                "{preceding}PASSAGE TO CONVERT to a bullet list (convert ONLY this):\n{sel}"
            );
            (system, user)
        }
        "table" => {
            let system = format!(
                "You reformat a passage of the user's own note into a GitHub-flavored markdown table \
                 with a header row and a `---` separator. Infer sensible columns from the content; \
                 preserve every fact, number, name, and decision; do NOT add or invent data. Reply \
                 in {lang}. Output ONLY the markdown table — no preamble, no explanation."
            );
            let user =
                format!("{preceding}PASSAGE TO CONVERT to a table (convert ONLY this):\n{sel}");
            (system, user)
        }
        "keypoints" => {
            let system = format!(
                "You write a SHORT TL;DR digest of a passage of the user's own note: 2–4 markdown \
                 bullets capturing only the key points, decisions, and numbers. This is an ADDITIVE \
                 summary to insert AFTER the selection — do NOT rewrite or reproduce the original. \
                 Reply in {lang}. Output ONLY the bullet digest — no preamble, no heading, no \
                 explanation."
            );
            let user =
                format!("{preceding}PASSAGE TO SUMMARIZE (write a short digest of this):\n{sel}");
            (system, user)
        }
        "enhance" => {
            let grounding = note_assist_grounding_block(citations);
            let system = format!(
                "You expand the user's note by proposing a SHORT ADDITIVE passage that builds on \
                 the selection using ONLY the RELATED MATERIAL provided — never invent facts. If the \
                 material adds nothing, reply with an empty line. Reply in {lang}. Output ONLY the \
                 additive passage to INSERT after the selection — no preamble, no headings, no \
                 explanation."
            );
            let user = format!(
                "RELATED MATERIAL (from the user's own brain):\n{grounding}\nSELECTION TO EXPAND:\n{sel}"
            );
            (system, user)
        }
        "link_entities" => {
            let names = if entity_names.is_empty() {
                "(none — return the selection unchanged)".to_string()
            } else {
                entity_names.join(", ")
            };
            let system = format!(
                "You rewrite a passage of the user's own note, wrapping ONLY the known entity names \
                 listed below in `[[wikilinks]]` where they appear. Do NOT invent links, do NOT link \
                 any name not in the list, do NOT change any other word, spacing, or punctuation, and \
                 do NOT double-wrap a name already inside `[[...]]`. If a name does not appear in the \
                 passage, leave the passage as-is for that name. Reply in {lang}. Output ONLY the \
                 rewritten passage — no preamble, no quotes, no explanation."
            );
            let user = format!(
                "KNOWN ENTITY NAMES (link ONLY these):\n{names}\n\n{preceding}PASSAGE TO LINK (rewrite ONLY this):\n{sel}"
            );
            (system, user)
        }
        "fact_check" => {
            let grounding = note_assist_grounding_block(citations);
            let system = format!(
                "You fact-check the SELECTION against the user's OWN brain (the RELATED MATERIAL \
                 below) ONLY — never external knowledge. Flag any claim in the selection that \
                 CONTRADICTS or is UNSUPPORTED by the material, quoting the conflicting source. If \
                 everything checks out, say so briefly. This is an ANSWER, not an edit — do NOT \
                 rewrite the selection. Preserve the source's exact subject, scope, location, and \
                 modality in every correction: never broaden a pilot into the whole project, one \
                 cohort into the rollout, or a possibility into a fact. Quote the smallest exact \
                 source span that carries those qualifiers. Reply in {lang}. Output ONLY your \
                 findings — no preamble."
            );
            let user = format!(
                "RELATED MATERIAL (from the user's own brain):\n{grounding}\nSELECTION TO FACT-CHECK:\n{sel}"
            );
            (system, user)
        }
        "ask" => {
            let grounding = note_assist_grounding_block(citations);
            let question = if instruction.is_empty() {
                "What is the most important thing to know about this selection?"
            } else {
                instruction
            };
            let system = format!(
                "You answer the user's QUESTION about the SELECTION, grounded in the SELECTION and \
                 the RELATED MATERIAL from their own brain ONLY — never invent facts. If the answer \
                 is not in the material, say so. This is an ANSWER, not an edit — do NOT rewrite the \
                 selection. Reply in {lang}. Output ONLY the answer — no preamble."
            );
            let user = format!(
                "QUESTION:\n{question}\n\nRELATED MATERIAL (from the user's own brain):\n{grounding}\nSELECTION:\n{sel}"
            );
            (system, user)
        }
        "action_items" => {
            let contrastive = match note_language.trim().to_ascii_lowercase().as_str() {
                "pl" => "Przykład: `Iga wyśle plan. Może kiedyś zmienimy dostawcę; brak właściciela i terminu.` \
                         staje się WYŁĄCZNIE `- [ ] Iga — wysłać plan`.",
                "en" => "Example: `Iga will send the plan. Maybe change vendor someday; no owner or date.` \
                         becomes ONLY `- [ ] Iga — send the plan`.",
                _ => "Contrastive rule: extract only an explicitly assigned future commitment; \
                      omit any unassigned suggestion from the same passage.",
            };
            let system = format!(
                "You extract action items / TODOs from a passage of the user's own note into a \
                 markdown checklist. Each task is its own `- [ ] ` line; capture the owner and any \
                 due date if stated; do NOT invent tasks. An unchecked item is unfinished: preserve \
                 a future commitment as a future task and never rewrite it as already completed. \
                 Suggestions, possibilities, open questions, and unassigned ideas are NOT tasks; \
                 never invent meta-tasks such as choosing an owner, setting a deadline, investigating, \
                 or confirming unless the passage explicitly assigns that work. {contrastive} If \
                 there are no explicit commitments, reply with an \
                 empty line. This is an ADDITIVE list to insert AFTER the selection — do NOT reproduce \
                 the original. Reply in {lang}. Output ONLY the checklist — no preamble, no heading."
            );
            let user = format!("{preceding}PASSAGE TO SCAN for action items:\n{sel}");
            (system, user)
        }
        "decisions" => {
            let system = format!(
                "You extract the DECISIONS made in a passage of the user's own note into a short \
                 markdown bullet list. Capture only decisions actually made (not open questions or \
                 tasks); do NOT invent any. If there are no decisions, reply with an empty line. This \
                 is an ADDITIVE list to insert AFTER the selection — do NOT reproduce the original. \
                 Reply in {lang}. Output ONLY the list — no preamble, no heading."
            );
            let user = format!("{preceding}PASSAGE TO SCAN for decisions:\n{sel}");
            (system, user)
        }
        "draft_followup" => {
            let system = format!(
                "You draft a concise follow-up email or message based on the SELECTION from the \
                 user's own note. Cover the key points, decisions, and next steps; keep a {tone} \
                 tone. Do NOT invent facts beyond the selection. Do NOT include a subject line — just \
                 the message body. Reply in {lang}. Output ONLY the message body — no preamble, no \
                 explanation.",
                tone = if variant.is_empty() { "professional" } else { variant }
            );
            let user = format!("{preceding}SELECTION TO TURN INTO A FOLLOW-UP MESSAGE:\n{sel}");
            (system, user)
        }
        "custom" => {
            // The free-text "Ask Brain to edit…" instruction, applied to the SELECTION (shape=replace).
            // MUST have its own arm — without it `custom` fell through to the spinoff_note catch-all,
            // silently dropping the instruction and drafting an unrelated note that Accept then
            // destructively wrote over the selection. The instruction is woven into the directive; the
            // FE only sends `custom` with non-empty text, but an empty directive degrades to a refine.
            let directive = if instruction.is_empty() {
                "Improve the clarity, grammar, and flow of the passage without changing its meaning"
            } else {
                instruction
            };
            let system = format!(
                "You edit a passage of the user's own note by applying THIS instruction to it: \
                 \"{directive}\". Apply the instruction to the passage ONLY — do NOT invent facts \
                 beyond it, do NOT answer as chat, do NOT continue the surrounding text. Reply in \
                 {lang}. Output ONLY the edited passage — no preamble, no quotes, no explanation."
            );
            let user = format!(
                "{preceding}PASSAGE TO EDIT (apply the instruction, rewrite ONLY this):\n{sel}"
            );
            (system, user)
        }
        // spinoff_note
        _ => {
            let system = format!(
                "You draft a new standalone note from the SELECTION in the user's existing note. \
                 Start with a short `# ` heading that titles the new note, then write clean note body \
                 in markdown. Build ONLY on the selection — do NOT invent facts. Reply in {lang}. \
                 Output ONLY the new note (heading + body) — no preamble, no explanation."
            );
            let user = format!("{preceding}SELECTION TO TURN INTO A NEW NOTE:\n{sel}");
            (system, user)
        }
    }
}

#[cfg(test)]
mod quality_prompt_tests {
    use super::*;

    fn request(action: &str, selection: &str) -> NoteAssistRequest {
        NoteAssistRequest {
            note_id: "quality-prompt".to_string(),
            action: action.to_string(),
            selection: selection.to_string(),
            before: None,
            after: None,
            variant: None,
            instruction: None,
        }
    }

    /// RED-before-GREEN from the Qwen popup result: it changed a future commitment into completed
    /// past tense and promoted an unowned suggestion into two invented meta-tasks.
    #[test]
    fn action_items_prompt_preserves_commitment_tense_and_rejects_meta_tasks() {
        let req = request(
            "action_items",
            "Iga committed to send the plan. Someone suggested changing vendor someday.",
        );
        let (system, _) = build_note_assist_prompt("action_items", &req, &[], &[], "en");
        for needle in [
            "preserve a future commitment as a future task",
            "never rewrite it as already completed",
            "Suggestions, possibilities, open questions, and unassigned ideas are NOT tasks",
            "never invent meta-tasks",
        ] {
            assert!(system.contains(needle), "missing `{needle}` in: {system}");
        }
        let pl_req = request(
            "action_items",
            "Iga wyśle plan. Może kiedyś zmienimy dostawcę.",
        );
        let (pl_system, _) = build_note_assist_prompt("action_items", &pl_req, &[], &[], "pl");
        assert!(pl_system.contains("Iga wyśle plan"), "{pl_system}");
        assert!(pl_system.contains("Iga — wysłać plan"), "{pl_system}");
        assert!(!pl_system.contains("Iga will send the plan"), "{pl_system}");

        let mismatch = request(
            "action_items",
            "Iga will send the plan. Maybe change vendor someday.",
        );
        let (mismatch_system, _) =
            build_note_assist_prompt("action_items", &mismatch, &[], &[], "pl");
        assert!(mismatch_system.contains("Reply in language code 'pl'"));
        assert!(!mismatch_system.contains("the same language as the selected text"));

        let (auto_system, _) = build_note_assist_prompt("action_items", &pl_req, &[], &[], "auto");
        assert!(auto_system.contains("the same language as the selected text"));
        assert!(auto_system.contains("Contrastive rule:"));
        assert!(!auto_system.contains("becomes ONLY"));
        assert!(!auto_system.contains("staje się WYŁĄCZNIE"));
    }

    /// RED-before-GREEN from the grounded fact-check: the local model broadened a Krakow pilot date
    /// into a claim about the whole project.
    #[test]
    fn fact_check_prompt_preserves_the_sources_exact_subject_scope() {
        let req = request("fact_check", "Project Orchid starts November 10.");
        let (system, _) = build_note_assist_prompt("fact_check", &req, &[], &[], "en");
        assert!(
            system.contains("exact subject, scope, location, and modality"),
            "{system}"
        );
        assert!(
            system.contains("never broaden a pilot into the whole project"),
            "{system}"
        );
    }
}

/// Whitespace word count — the unit the note-edit length guard compares in (word/sentence targets
/// are followable by small models; character/token targets are not).
pub(crate) fn note_edit_word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// The RUNAWAY-GUARD token cap for a note edit, sized off the selection (rough chars/4 token
/// estimate). `shorten` is capped at ~the input length so it can't physically LENGTHEN; the
/// in-place edits (refine/grammar/simplify/tone/translate/bullets/table/link_entities/custom) get
/// modest headroom; the ADDITIVE / GENERATIVE actions (expand/enhance/keypoints/action_items/
/// decisions/fact_check/ask/drafts) get GENEROUS headroom because their output is new content, not a
/// rewrite of the selection. Floors keep a tiny selection from being truncated; ceilings keep a huge
/// selection inside the 4096-token on-device context budget. A safety net — the prompt's length
/// budget + [`generate_note_edit`]'s validation are the primary length controls.
pub(crate) fn note_edit_max_tokens(action: &str, input_chars: usize) -> usize {
    let input_tokens = (input_chars / 4).max(1);
    let (mult, floor, ceil) = match action {
        // Must not exceed ~input tokens (physically can't lengthen).
        "shorten" => (1.0_f64, 48usize, 1024usize),
        // Additive / generated output: the answer/draft/list is NEW content — give it room, and a
        // higher floor so a short selection can still yield a full draft or answer.
        "expand" | "enhance" | "keypoints" | "action_items" | "decisions" | "fact_check"
        | "ask" | "draft_followup" | "spinoff_note" => (3.0_f64, 256usize, 2048usize),
        // In-place edits can legitimately match or slightly exceed the input length.
        _ => (1.5_f64, 64usize, 1536usize),
    };
    ((input_tokens as f64 * mult).ceil() as usize).clamp(floor, ceil)
}

/// Generate one note edit through `provider`, and for `shorten` ENFORCE that the result is actually
/// shorter than the input — one stricter retry if the first attempt is not (the "shorten made it
/// longer" guard; the model otherwise ran unbounded on the fully-local path). Returns the shortest
/// candidate for `shorten`, the single result otherwise. Takes `&dyn SummarizerProvider` so it is
/// unit-testable with a scripted fake provider.
pub(crate) async fn generate_note_edit(
    provider: &dyn crate::summarize::provider::SummarizerProvider,
    action: &str,
    system: &str,
    user: &str,
    opts: crate::reason::GenOptions,
    input_words: usize,
) -> Result<(String, crate::summarize::meta::CallMeta), AppError> {
    // `opts` is `Copy` — each call gets its own copy (no `.clone()`, which would trip clippy).
    let (out, meta) = provider.complete_with_meta_opts(system, user, opts).await?;
    if action == "shorten" && note_edit_word_count(&out) >= input_words {
        // First attempt did not shorten — retry ONCE with a stricter instruction.
        let strict = format!(
            "{system}\n\nThe previous attempt was NOT shorter than the original. Return a STRICTLY \
             shorter version: fewer words than the original, keeping only the essential facts."
        );
        let (out2, meta2) = provider
            .complete_with_meta_opts(&strict, user, opts)
            .await?;
        if note_edit_word_count(&out2) < note_edit_word_count(&out) {
            return Ok((out2, meta2));
        }
    }
    Ok((out, meta))
}

/// Rewrite the note's action items into Obsidian Tasks format (📅 due dates) + re-write the
/// vault file in place. Returns the updated note.
#[tauri::command]
pub fn patch_note_tasks(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<NoteDto, AppError> {
    // BLK-1 / TOCTOU (2026-07-10 audit F4): hold the lifecycle guard across gate+write so a
    // concurrent relock/seal cannot land between the unlock check and the upsert.
    let _lifecycle = lifecycle_guard(state.inner());
    // D4 WRITE-GATE: refuse to rewrite a sealed-and-not-unlocked meeting's note (its plaintext is
    // blanked; patching would persist the blanked value over the sealed content). Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
                crate::errcode::MEETING_LOCKED,
                "this meeting's folder is locked — unlock it to rewrite the note's tasks",
            )));
    }
    let existing = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    let patched = crate::summarize::action_items::patch_tasks_markdown(&existing.markdown);
    let created_at = chrono::Utc::now().to_rfc3339();
    // Seal-on-write (audit F1): a session-unlocked LOCKED folder re-seals the patched markdown.
    upsert_note_reseal_if_locked(
        state.inner(),
        &NoteRecord {
            meeting_id: meeting_id.clone(),
            provider_id: existing.provider_id.clone(),
            markdown: patched.clone(),
            created_at,
            exported_path: existing.exported_path.clone(),
            model_requested: existing.model_requested.clone(),
            model_served: existing.model_served.clone(),
            gateway_host: existing.gateway_host.clone(),
        },
    )?;
    if let Some(path) = existing.exported_path.as_deref() {
        overwrite_exported_note_guarded(
            state.inner(),
            &meeting_id,
            &existing.provider_id,
            path,
            &patched,
        )?;
    }
    Ok(NoteDto {
        meeting_id,
        provider_id: existing.provider_id,
        markdown: patched,
        exported_path: existing.exported_path,
    })
}

/// Brain v2 L2.3 — IMPORT pasted memories from another AI assistant (a ChatGPT/Claude "what I
/// remember about you" export) into the user-memory store. Returns the number of NEW facts added.
///
/// Flow: extract candidates on the ON-DEVICE light reasoner (`user_memory::extract_imported_memories`
/// — stub ⇒ empty ⇒ 0) → deterministic `reconcile_facts` against ALL existing user facts (a
/// re-import of the same text reconciles to NoOps ⇒ 0 new) → ONLY when there is ≥1 Add, create a
/// SYNTHETIC anchor meeting (`import-<uuid>`, title "Memory Import", `Exported`, no audio, no
/// folder — so its facts are VISIBLE via the no-note arm of the visibility predicate) → stamp +
/// apply atomically. ORDER IS LOAD-BEARING: the meeting row is created only after reconcile found
/// something to add, so a stub/duplicate import leaves NO synthetic meeting behind. Deleting that
/// meeting undoes the whole import (`delete_meeting` purges its `user_facts` in-tx). ZERO egress:
/// extraction runs on [`import_extraction_reasoner`] — LOCAL-or-stub, NEVER cloud (the FE copy
/// promises on-device; a pasted third-party memory export must not ride the cloud Notes provider).
/// No local model ⇒ 0 imported (the FE hints the model may be missing). Runs on a blocking worker
/// (a local-model extraction can take seconds). Logs counts only.
#[tauri::command]
pub async fn import_memories(state: State<'_, AppState>, text: String) -> Result<usize, AppError> {
    let db = state.db.clone();
    let reasoner = import_extraction_reasoner(state.inner());
    let enabled = user_memory_enabled(state.inner());
    tauri::async_runtime::spawn_blocking(move || {
        import_memories_inner(&db, reasoner.as_ref(), enabled, &text)
    })
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("import task join failed: {e}")))?
}

/// Inner of [`import_memories`] (unit-testable: Db + reasoner + flag injected).
pub(crate) fn import_memories_inner(
    db: &crate::storage::Db,
    reasoner: &dyn crate::reason::LocalReasoner,
    memory_enabled: bool,
    text: &str,
) -> Result<usize, AppError> {
    if !memory_enabled {
        return Err(AppError::InvalidArg(
            "cross-meeting memory is turned off — enable it in Settings to import memories".into(),
        ));
    }
    if text.trim().is_empty() {
        return Err(AppError::InvalidArg(
            "nothing to import — paste the memory text".into(),
        ));
    }
    // 1) Best-effort extraction (stub / decode failure ⇒ empty ⇒ 0 imported, nothing persisted).
    let candidates = crate::user_memory::extract_imported_memories(reasoner, text);
    if candidates.is_empty() {
        return Ok(0);
    }
    // 2) Deterministic reconcile against ALL existing user facts — the dedup: re-importing the same
    //    export yields NoOps only.
    let existing = db.user_facts_all()?;
    let at = chrono::Utc::now().to_rfc3339();
    let mut ops = crate::facts::reconcile_facts(&existing, &candidates, &at);
    let adds = ops
        .iter()
        .filter(|o| matches!(o, crate::facts::FactOp::Add(_)))
        .count();
    if adds == 0 {
        return Ok(0); // pure dedup/no-op import — no synthetic meeting is created.
    }
    // 3) The synthetic anchor meeting — created ONLY now that something will be added, so deleting
    //    it undoes the import and a no-op import leaves nothing behind.
    let meeting_id = format!("import-{}", uuid::Uuid::new_v4());
    db.insert_meeting(&crate::storage::models::Meeting {
        id: meeting_id.clone(),
        started_at: at.clone(),
        ended_at: None,
        title: Some("Memory Import".to_string()),
        duration_s: 0,
        audio_path: None,
        status: crate::storage::models::MeetingStatus::Exported,
        folder_id: None,
    })?;
    // 4) Stamp the anchor onto the Adds (gating + purge anchor) and apply atomically. MEM-1: use the
    //    import-aware apply so every pre-existing fact this import SUPERSEDES (an Invalidate on a fact
    //    anchored to another meeting) is linked to the synthetic import id — deleting the import then
    //    REOPENS those facts, making "delete to undo" a FULL reversal instead of a partial one that
    //    leaves prior memories permanently closed.
    crate::facts::set_meeting_id(&mut ops, &meeting_id);
    db.apply_user_fact_ops_recording_import_supersedes(&ops, &meeting_id)?;
    tracing::info!(
        target: "user_memory",
        meeting_id = %meeting_id,
        added = adds,
        "memories imported (anchored to a synthetic meeting)"
    );
    Ok(adds)
}

// ── Re-Truth (the vault heals itself) — supersession review + one-tap stamp ──────────────────────

/// One supersession surfaced for review (camelCase for the FE). `sourceNotePath` is the absolute
/// on-disk `.md` the FE never shows (it shows `sourceNoteTitle`); `applied` reflects whether the
/// row has already been stamped (always `false` from `preview_supersessions`, which returns the
/// pending set).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupersessionDto {
    pub id: String,
    pub entity: String,
    pub predicate: String,
    pub old_value: String,
    pub new_value: String,
    pub source_note_title: String,
    pub source_note_path: String,
    pub source_meeting_id: String,
    pub superseding_meeting_id: String,
    /// The superseding note's title — `None` when that note is sealed (never leak a locked title).
    pub superseding_note_title: Option<String>,
    pub applied: bool,
}

/// Result of `apply_supersessions`: how many were stamped vs skipped because their source note sealed
/// (or lost its vault file) between preview and apply (the prune↔seal TOCTOU discipline).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResult {
    pub applied: usize,
    pub skipped_sealed: usize,
}

/// Preview the PENDING supersessions whose superseding meeting is `meeting_id`. GATED: a row is
/// included ONLY when its SOURCE meeting is stampable (open-on-disk + unlocked) AND has a vault `.md`;
/// a sealed-or-unexported source contributes NOTHING. The superseding note's title is surfaced only
/// when that note is itself unlocked. Returns `[]` when there are none.
#[tauri::command]
pub fn preview_supersessions(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<SupersessionDto>, AppError> {
    preview_supersessions_inner(state.inner(), &meeting_id)
}

pub(crate) fn preview_supersessions_inner(
    state: &AppState,
    meeting_id: &str,
) -> Result<Vec<SupersessionDto>, AppError> {
    let rows = state.db.unapplied_supersessions_for(meeting_id)?;
    let mut out = Vec::new();
    for r in rows {
        // GATE the SOURCE note (content-read + write-safety). A sealed/not-unlocked source is dropped.
        if !source_is_stampable(state, &r.source_meeting_id)? {
            continue;
        }
        // GATE the SUPERSEDING side TOO — defense-in-depth. `new_value` is derived from the superseding
        // meeting's fact, so a sealed-and-not-session-unlocked superseding meeting must surface NOTHING,
        // even in the brief race window where `lock_folder` has flipped `locked=1` but not yet purged
        // this row. Purge-on-seal normally removes it; this second-side gate closes the race regardless.
        if !meeting_is_unlocked(state, &r.superseding_meeting_id)? {
            continue;
        }
        let Some((path, stem)) = note_file_for(state, &r.source_meeting_id)? else {
            continue; // no vault file → nothing to stamp/show.
        };
        // The superseding meeting is now known-unlocked, so its title is safe to surface (`None` only
        // when it was never exported to the vault).
        let superseding_note_title =
            note_file_for(state, &r.superseding_meeting_id)?.map(|(_, s)| s);
        out.push(SupersessionDto {
            id: r.id,
            entity: r.entity,
            predicate: r.predicate,
            old_value: r.old_value,
            new_value: r.new_value,
            source_note_title: stem,
            source_note_path: path,
            source_meeting_id: r.source_meeting_id,
            superseding_meeting_id: r.superseding_meeting_id,
            superseding_note_title,
            applied: r.applied_at.is_some(),
        });
    }
    Ok(out)
}

/// APPLY the given supersessions: append a `[!superseded]` callout to each SOURCE note (and a mirror
/// backlink to the superseding note, when it too is open). RE-GATES each row at apply time — a source
/// that sealed since preview is SKIPPED (never stamped), the prune↔seal TOCTOU discipline. Snapshots
/// each note's exact bytes into the row's pre-image BEFORE the (append-only) write, so `undo` restores
/// them byte-identical. Idempotent: an already-applied row is a no-op, and the callout carries a
/// stable marker so re-stamping never duplicates.
#[tauri::command]
pub fn apply_supersessions(
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> Result<ApplyResult, AppError> {
    apply_supersessions_inner(state.inner(), &ids)
}

pub(crate) fn apply_supersessions_inner(
    state: &AppState,
    ids: &[String],
) -> Result<ApplyResult, AppError> {
    // Seal-vs-write TOCTOU (Phase-0 lock-review follow-up): hold the lifecycle guard across the
    // per-row re-gate + `.md` writes, so a concurrent lock/relock cannot land between
    // `source_is_stampable` and the append (the same guard `update_note_inner` and every other
    // vault-writing command already holds).
    let _lifecycle = lifecycle_guard(state);
    let mut applied = 0usize;
    let mut skipped_sealed = 0usize;
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();

    // PER-BATCH PRISTINE CACHE, keyed by note FILE path. Multiple rows in ONE heal touch the SAME
    // note — every row shares the superseding meeting's note, and two facts from one old note share a
    // SOURCE note too. The undo pre-image for a file MUST be its PRE-BATCH ("pristine") content,
    // captured the FIRST time this call touches that path — never the mid-batch, already-stamped
    // bytes a later row would otherwise read. So all rows sharing a note carry IDENTICAL pristine
    // pre-images and undo (restore-each-file-once) is order-independent + byte-identical for N≥2.
    let mut pristine: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();

    // PRE-PASS: seed the cache from any pre-images ALREADY durably stored by an earlier (crashed)
    // attempt, keyed by note path — so a row that still has to capture its pre-image finds the
    // pristine bytes here instead of re-reading a sibling-stamped file, regardless of the id order a
    // retry arrives in (retry-safe AND order-independent).
    for id in ids {
        let Some(row) = state.db.get_supersession(id)? else {
            continue;
        };
        if let Some(pre) = &row.source_pre_image {
            if source_is_stampable(state, &row.source_meeting_id)? {
                if let Some((path, _)) = note_file_for(state, &row.source_meeting_id)? {
                    pristine.entry(path).or_insert_with(|| pre.clone());
                }
            }
        }
        if let Some(pre) = &row.superseding_pre_image {
            let superseding_open = meeting_is_unlocked(state, &row.superseding_meeting_id)?
                && !folder_locked_on_disk(state, &row.superseding_meeting_id)?;
            if superseding_open {
                if let Some((path, _)) = note_file_for(state, &row.superseding_meeting_id)? {
                    pristine.entry(path).or_insert_with(|| pre.clone());
                }
            }
        }
    }

    for id in ids {
        let Some(row) = state.db.get_supersession(id)? else {
            continue; // unknown id — nothing to do.
        };
        if row.applied_at.is_some() {
            continue; // already stamped — idempotent no-op.
        }
        // TOCTOU re-gate: the source folder may have sealed since preview. A now-sealed/not-unlocked
        // (or unexported) source is SKIPPED, never stamped.
        if !source_is_stampable(state, &row.source_meeting_id)? {
            skipped_sealed += 1;
            continue;
        }
        let Some((source_path, source_stem)) = note_file_for(state, &row.source_meeting_id)? else {
            skipped_sealed += 1;
            continue;
        };

        // The superseding note gets a backlink ONLY when it is itself open-on-disk + unlocked (never
        // write into or reference a sealed note). Its stem feeds the source callout's `[[…]]` link.
        let superseding_open = meeting_is_unlocked(state, &row.superseding_meeting_id)?
            && !folder_locked_on_disk(state, &row.superseding_meeting_id)?;
        let superseding_file = if superseding_open {
            note_file_for(state, &row.superseding_meeting_id)?
        } else {
            None
        };

        // Resolve PRISTINE pre-images from the per-path cache (reads + UTF-8-validates each file on its
        // first batch-touch; every later row sharing the path reuses the SAME pristine bytes).
        let source_pre = pristine_note_bytes(&mut pristine, &source_path)?;
        let superseding_pre = match &superseding_file {
            Some((p, _)) => Some(pristine_note_bytes(&mut pristine, p)?),
            None => None,
        };

        // DURABLE-BEFORE-WRITE: persist the pristine pre-images BEFORE any `.md` write, so a crash
        // between write and mark-applied still leaves a recoverable un-stamped pre-image. `COALESCE`
        // never clobbers an already-stored pristine backup and, combined with the pristine cache, the
        // stored bytes are NEVER a re-snapshot of a stamped file.
        state.db.store_supersession_pre_images(
            id,
            Some(&source_pre),
            superseding_pre.as_deref(),
        )?;

        // Stamp the SOURCE note: append the callout to the CURRENT on-disk content (idempotent — a
        // retry over an already-stamped file is a no-op). The undo pre-image is the pristine cache
        // copy, never these current bytes.
        let current_source = std::fs::read_to_string(&source_path)
            .map_err(|e| AppError::Export(format!("read source note failed: {e}")))?;
        let new_source = crate::export::obsidian::append_supersession_callout(
            &current_source,
            &date,
            &row.predicate,
            &row.old_value,
            &row.new_value,
            superseding_file.as_ref().map(|(_, s)| s.as_str()),
        );
        crate::export::obsidian::overwrite_note(std::path::Path::new(&source_path), &new_source)?;
        // Export-collision guard: this is a read-modify-write APPEND of the CURRENT file (external
        // edits are preserved in place by construction — no sibling). The baseline is re-stamped
        // from the final written content ONLY when the pre-append bytes still matched it — see
        // the helper's laundering rationale.
        refresh_meeting_note_exported_hash(
            state,
            &row.source_meeting_id,
            &current_source,
            &new_source,
        )?;

        // Stamp the SUPERSEDING backlink (its pristine pre-image is now durably stored). Append to its
        // CURRENT content (idempotent).
        if let Some((sup_path, _)) = &superseding_file {
            let current_sup = std::fs::read_to_string(sup_path)
                .map_err(|e| AppError::Export(format!("read superseding note failed: {e}")))?;
            let new_sup = crate::export::obsidian::append_supersedes_callout(
                &current_sup,
                &date,
                &row.predicate,
                &row.old_value,
                &row.new_value,
                &source_stem,
            );
            crate::export::obsidian::overwrite_note(std::path::Path::new(sup_path), &new_sup)?;
            // Same conditional append-side baseline refresh as the source stamp above.
            refresh_meeting_note_exported_hash(
                state,
                &row.superseding_meeting_id,
                &current_sup,
                &new_sup,
            )?;
        }

        // APPLIED is the LAST write — flipped only after the note(s) are safely stamped.
        state
            .db
            .mark_supersession_applied(id, &chrono::Utc::now().to_rfc3339())?;
        applied += 1;
    }
    tracing::info!(target: "retruth", applied, skipped_sealed, "supersessions applied");
    Ok(ApplyResult {
        applied,
        skipped_sealed,
    })
}

/// Resolve the PRISTINE (pre-batch) bytes of a note file from the per-apply-call cache: on the first
/// touch of a path this batch, read + UTF-8-validate the file and cache it; every later touch reuses
/// the cached bytes. This is what makes all rows sharing a note carry identical pristine pre-images
/// (so a multi-row undo restores each file once, byte-identical). Refusing a non-UTF-8 file here —
/// before any write — keeps the stamp all-or-nothing.
fn pristine_note_bytes(
    cache: &mut std::collections::HashMap<String, Vec<u8>>,
    path: &str,
) -> Result<Vec<u8>, AppError> {
    if let Some(b) = cache.get(path) {
        return Ok(b.clone());
    }
    let bytes =
        std::fs::read(path).map_err(|e| AppError::Export(format!("read note failed: {e}")))?;
    String::from_utf8(bytes.clone())
        .map_err(|_| AppError::Export("note is not valid UTF-8".into()))?;
    cache.insert(path.to_string(), bytes.clone());
    Ok(bytes)
}

/// UNDO the given applied supersessions: restore each stamped note's byte-exact pre-image (atomic
/// overwrite) and clear the row's applied state + pre-images. A row that isn't applied is a no-op. A
/// note whose folder sealed since apply is SKIPPED (never re-materialize plaintext into a locked
/// folder) — the sealed content will return WITH the stamp on unlock.
#[tauri::command]
pub fn undo_supersessions(state: State<'_, AppState>, ids: Vec<String>) -> Result<(), AppError> {
    undo_supersessions_inner(state.inner(), &ids)
}

pub(crate) fn undo_supersessions_inner(state: &AppState, ids: &[String]) -> Result<(), AppError> {
    // Seal-vs-write TOCTOU (Phase-0 lock-review follow-up): same guard as
    // `apply_supersessions_inner` — the folder-open checks below and the restore writes must not
    // interleave with a concurrent lock/relock.
    let _lifecycle = lifecycle_guard(state);
    let undo_set: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();

    // Collect the DISTINCT affected note files touched by the UNDO SET (`path -> (meeting_id,
    // pristine pre-image)`). All rows in one heal sharing a note carry IDENTICAL pristine pre-images
    // (see apply's per-path cache), and `path ↔ meeting` is 1:1. Only files whose folder is still
    // OPEN are collected/rewritten (never re-materialize plaintext into a sealed folder — a sealed
    // note's stamp rides inside its sealed content already; and purge-on-seal has already dropped any
    // supersession referencing a sealed meeting).
    let mut affected: std::collections::HashMap<String, (String, Vec<u8>)> =
        std::collections::HashMap::new();
    let mut to_clear: Vec<&String> = Vec::new();
    for id in ids {
        let Some(row) = state.db.get_supersession(id)? else {
            continue;
        };
        if row.applied_at.is_none() {
            continue; // nothing applied to undo.
        }
        if let Some(pre) = &row.source_pre_image {
            if !folder_locked_on_disk(state, &row.source_meeting_id)? {
                if let Some((path, _)) = note_file_for(state, &row.source_meeting_id)? {
                    affected
                        .entry(path)
                        .or_insert_with(|| (row.source_meeting_id.clone(), pre.clone()));
                }
            }
        }
        if let Some(pre) = &row.superseding_pre_image {
            if !folder_locked_on_disk(state, &row.superseding_meeting_id)? {
                if let Some((path, _)) = note_file_for(state, &row.superseding_meeting_id)? {
                    affected
                        .entry(path)
                        .or_insert_with(|| (row.superseding_meeting_id.clone(), pre.clone()));
                }
            }
        }
        to_clear.push(id);
    }

    // For each affected file: rebuild it as pristine + the stamps of every SURVIVOR — a supersession
    // that touches THIS note's meeting, is NOT in the undo set, and remains applied. Because the
    // callout appends are idempotent + order-independent across distinct supersessions, replaying the
    // survivors reconstructs the exact on-disk state that matches the DB. A FULL undo (no survivors)
    // collapses to a plain pristine restore. This closes the partial-undo desync where restoring a
    // shared file to pristine silently stripped a still-applied sibling's on-disk stamp.
    for (path, (meeting_id, pristine)) in &affected {
        let mut text = String::from_utf8(pristine.clone())
            .map_err(|_| AppError::Export("stored pre-image is not valid UTF-8".into()))?;
        for s in state.db.supersessions_touching_meeting(meeting_id)? {
            if undo_set.contains(s.id.as_str()) || s.applied_at.is_none() {
                continue; // being undone, or not currently applied → no stamp to replay.
            }
            // Reproduce this survivor's stamp with its ORIGINAL date (the day it was applied) so the
            // replay byte-matches the original append.
            let date = s
                .applied_at
                .as_deref()
                .and_then(|a| a.split('T').next())
                .unwrap_or("")
                .to_string();
            if &s.source_meeting_id == meeting_id {
                // This file is the SURVIVOR's SOURCE note → re-append its `[!superseded]` callout,
                // reproducing the `· see [[…]]` link exactly as apply did (open superseding only).
                let sup_stem = superseding_link_stem(state, &s)?;
                text = crate::export::obsidian::append_supersession_callout(
                    &text,
                    &date,
                    &s.predicate,
                    &s.old_value,
                    &s.new_value,
                    sup_stem.as_deref(),
                );
            }
            if &s.superseding_meeting_id == meeting_id {
                // This file is the SURVIVOR's SUPERSEDING note → re-append its `[!supersedes]`
                // backlink referencing the survivor's SOURCE stem.
                if let Some((_, src_stem)) = note_file_for(state, &s.source_meeting_id)? {
                    text = crate::export::obsidian::append_supersedes_callout(
                        &text,
                        &date,
                        &s.predicate,
                        &s.old_value,
                        &s.new_value,
                        &src_stem,
                    );
                }
            }
        }
        // Export-collision guard: the undo rebuild is a FULL overwrite from the stored pre-image
        // (+ survivor replays), so an external edit made since apply is preserved as a sibling
        // first, and the baseline re-stamped from the rebuilt content. The no-note-row branch is
        // dead-defensive (`affected` is only ever keyed via `note_file_for`, which requires a
        // note row) — it still routes through the ONE guarded overwrite (Phase-0 follow-up): a
        // missing row reads a NULL baseline (grandfathered, no sibling) and its hash re-stamp
        // updates zero rows, so behavior is identical while the invariant "every full overwrite
        // of an exported note goes through the guard" holds structurally.
        let provider_id = state
            .db
            .get_latest_note_for_meeting(meeting_id)?
            .map(|n| n.provider_id)
            .unwrap_or_default();
        overwrite_exported_note_guarded(state, meeting_id, &provider_id, path, &text)?;
    }

    // Clear the applied state on the undone rows ONLY (pre-images dropped); survivors stay applied.
    for id in to_clear {
        state.db.clear_supersession_applied(id)?;
    }
    Ok(())
}

/// The `· see [[stem]]` link stem for a supersession's SOURCE-side callout: the superseding note's
/// file-stem when that note is open-on-disk + unlocked (exactly the apply-time condition), else
/// `None` (never leak a sealed meeting's title). Mirrors the apply path so a survivor replay
/// reproduces the original callout.
fn superseding_link_stem(
    state: &AppState,
    s: &crate::storage::models::SupersessionRow,
) -> Result<Option<String>, AppError> {
    let open = meeting_is_unlocked(state, &s.superseding_meeting_id)?
        && !folder_locked_on_disk(state, &s.superseding_meeting_id)?;
    if !open {
        return Ok(None);
    }
    Ok(note_file_for(state, &s.superseding_meeting_id)?.map(|(_, stem)| stem))
}
