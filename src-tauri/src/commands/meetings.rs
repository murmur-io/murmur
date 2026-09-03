//! Meetings read / detail / tags / speaker-reconcile commands — extracted verbatim from `commands`
//! (God-file split, a PURE MOVE — the read-gate + backend-mask LOGIC is UNCHANGED, only relocated).
//! This is the meetings read surface: focus tracking, the Brain overview counts, Library search + the
//! recent/by-tag lists, meeting-app detection, tag get/set, the speaker rename/suggest/reconcile
//! cluster, and the two per-meeting content reads (`get_timeline` / `get_meeting_detail`). EVERY
//! content read here keeps its EXACT prior gate.
//! `get_meeting_detail` fails to a MASKED DTO for a sealed-and-not-session-unlocked meeting via
//! `super::meeting_is_unlocked` → `super::masked_detail` (`locked: true`, title `🔒 Locked`, no
//! note/segments, `audio_path: None` — the `convertFileSrc`/`asset:` leak trap stays closed).
//! `get_timeline` returns an EMPTY `MeetingTimeline` for a sealed-not-unlocked meeting (same gate),
//! never re-deriving from now-blank segments. `list_meetings` / `list_meetings_by_tag_inner` route
//! every row through `super::mask_locked_meetings` so a sealed-not-unlocked meeting is masked at the
//! backend before the DTO crosses IPC. `search_meetings` / `brain_overview` snapshot the live session
//! `unlocked` set via `super::unlocked_snapshot` and read only VISIBLE content — byte-identical to
//! their pre-move form.
//! The shared mask/gate helpers `masked_detail`, `mask_locked_meetings`, `meeting_is_unlocked`,
//! `unlocked_snapshot`, plus `meeting_display_name` / `provisional_meeting_title` / `compute_duration_s`
//! (used by the STAYING companion-note + start-recording clusters) all STAY in `commands/mod.rs`;
//! `masked_detail`/`mask_locked_meetings` were promoted to `pub(crate)` (bodies byte-identical) so the
//! moved reads reach them through `use super::*`. The `*_inner` cores (`set_focus_meeting_inner`,
//! `brain_overview_inner`, `list_meetings_by_tag_inner`, `rename_speaker_inner`,
//! `suggest_speaker_labels_inner`) move WITH their commands and stay `pub(crate)` for the STAYING test
//! modules; `parse_others_cluster` / `reconcile_meeting_speakers` are private and used only by the
//! moved speaker-reconcile cores.
//! Every symbol keeps its EXACT prior body/signature and is re-exported at `crate::commands` via
//! `pub use meetings_commands::*;` in `commands/mod.rs`, so `generate_handler![commands::list_meetings]`
//! in `lib.rs` and every `crate::commands::…` caller resolve UNCHANGED. No gate/mask LOGIC changed —
//! only relocation.

use super::*;

/// PHASE 6 — set (or clear) the FOCUS meeting: the meeting the user is currently VIEWING /
/// anchored to, DISTINCT from the recording pointer (`state.current_meeting`, `Some` only while
/// recording). The FE calls this with `Some(id)` when it opens a meeting-detail / conversation
/// view and `None` when it closes, so the brain's Tier-1 "this meeting" scope
/// ([`crate::transcribe::live::resolve_scope_meeting`]) is deterministic even when nothing is
/// recording AND when a DIFFERENT meeting is recording — the backend safety-net for any assistant
/// path that falls back off an explicit FE `meeting_id` (the voice/wake twin). This stores ONLY an
/// id (never meeting content), so there is no seal/verify-before-destroy or clear-on-relock to do:
/// a relock re-masks the focused meeting's CONTENT through the existing `meeting_is_visible` gate
/// (`gated_live_context` fail-closes), and the stale id itself leaks nothing. A blank/whitespace
/// id is treated as clear (`None`). Fail-safe: a poisoned focus mutex recovers via `into_inner()`
/// (the pointer carries no invariant) rather than bricking the setter. No PII (opaque id only).
#[tauri::command]
pub fn set_focus_meeting(
    state: State<'_, AppState>,
    meeting_id: Option<String>,
) -> Result<(), AppError> {
    set_focus_meeting_inner(state.inner(), meeting_id);
    Ok(())
}

/// Inner of [`set_focus_meeting`] taking `&AppState` so it is headless-testable without a
/// `tauri::State`. Normalizes a blank/whitespace id to `None` (clear) and fail-safes on a poisoned
/// focus mutex via `into_inner()` (the pointer carries no invariant, so recovering it is safe and
/// never bricks the setter). No PII (opaque id only).
pub(crate) fn set_focus_meeting_inner(state: &AppState, meeting_id: Option<String>) {
    let normalized = meeting_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut focus = state
        .focus_meeting
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *focus = normalized;
    tracing::debug!(
        target: "brain",
        has_focus = focus.is_some(),
        "focus meeting updated"
    );
}

/// Headline counts + semantic flags for the Brain page ("what's in my brain"). All counts are over
/// VISIBLE/unlocked content only (a sealed-not-unlocked folder's items are never counted); carries
/// NO text. The two flags drive the "vectorize your brain" nudge (semantic off / model absent).
#[tauri::command]
pub fn brain_overview(state: State<'_, AppState>) -> Result<BrainOverview, AppError> {
    brain_overview_inner(state.inner())
}

/// Inner of [`brain_overview`] taking `&AppState` (unit-testable gate).
pub(crate) fn brain_overview_inner(state: &AppState) -> Result<BrainOverview, AppError> {
    let unlocked = unlocked_snapshot(state)?;
    let (meeting_count, document_count, note_count, indexed_chunk_count) =
        state.db.brain_counts(&unlocked)?;
    let semantic_enabled = state
        .config
        .lock()
        .map(|c| c.semantic_search_enabled)
        .unwrap_or(false);
    Ok(BrainOverview {
        meeting_count,
        document_count,
        note_count,
        indexed_chunk_count,
        semantic_enabled,
        embed_model_present: crate::embed::embed_model_present(),
    })
}

/// Full-text-ish search across meeting titles, transcripts, and notes (Library search).
#[tauri::command]
pub async fn search_meetings(app: AppHandle, query: String) -> Result<Vec<SearchHit>, AppError> {
    offload_read(app, move |state| {
        // BLK-2b: search only VISIBLE meetings (open/unlocked folders) so a sealed-and-not-unlocked
        // meeting's title/transcript/note never surfaces in a hit — independent of at-rest blanking.
        let unlocked = unlocked_snapshot(state)?;
        state.db.search_visible(&query, 100, &unlocked)
    })
    .await
}

/// Best-effort detection of a running meeting app (Zoom / Teams / Webex) to offer a
/// "start recording?" nudge. Browser-based Google Meet is NOT detectable this way.
#[tauri::command]
pub fn detect_meeting_app() -> Result<Option<String>, AppError> {
    let listing = match std::process::Command::new("ps")
        .arg("-axo")
        .arg("comm=")
        .output()
    {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(_) => return Ok(None),
    };
    for (needle, name) in [
        ("zoom.us", "Zoom"),
        ("Microsoft Teams", "Microsoft Teams"),
        ("Webex", "Webex"),
    ] {
        if listing.contains(needle) {
            return Ok(Some(name.to_string()));
        }
    }
    Ok(None)
}

/// Replace a meeting's tags (trimmed, de-duplicated by the DB).
#[tauri::command]
pub fn set_meeting_tags(
    state: State<'_, AppState>,
    meeting_id: String,
    tags: Vec<String>,
) -> Result<(), AppError> {
    state.db.set_meeting_tags(&meeting_id, &tags)
}

/// A meeting's tags (sorted).
#[tauri::command]
pub fn get_meeting_tags(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<String>, AppError> {
    state.db.get_meeting_tags(&meeting_id)
}

/// All distinct tags across meetings (for the Library filter).
#[tauri::command]
pub fn list_all_tags(state: State<'_, AppState>) -> Result<Vec<String>, AppError> {
    state.db.list_all_tags()
}

/// Meetings carrying a given tag, newest first. Sealed-and-not-session-unlocked meetings are
/// MASKED at the backend before the DTO crosses IPC, exactly like [`list_meetings`] — the tag
/// view is the same Library surface, so a sealed title / audio path must not leak through it
/// (rule: every content read is gated; see `mask_locked_meetings`).
#[tauri::command]
pub fn list_meetings_by_tag(
    state: State<'_, AppState>,
    tag: String,
) -> Result<Vec<Meeting>, AppError> {
    list_meetings_by_tag_inner(state.inner(), &tag)
}

/// Inner body of [`list_meetings_by_tag`] (unit-testable without a tauri `State`): the DB read
/// routed through the same backend mask as [`list_meetings`].
pub(crate) fn list_meetings_by_tag_inner(
    state: &AppState,
    tag: &str,
) -> Result<Vec<Meeting>, AppError> {
    let meetings = state.db.list_meetings_by_tag(tag)?;
    mask_locked_meetings(state, meetings)
}

/// Recent meetings for the Library list (newest first, capped). Sealed-and-not-session-unlocked
/// meetings are MASKED at the backend before the DTO crosses IPC (see [`mask_locked_meetings`]) —
/// the Library lock gate is enforced in code here, never trusted to the FE.
#[tauri::command]
pub async fn list_meetings(app: AppHandle) -> Result<Vec<Meeting>, AppError> {
    offload_read(app, |state| {
        let meetings = state.db.list_meetings(200)?;
        mask_locked_meetings(state, meetings)
    })
    .await
}

/// Rename a speaker across a meeting's cached timeline (e.g. "User 1" → "Sarah"). Persists to
/// the timelines cache and returns the updated timeline.
#[tauri::command]
pub fn rename_speaker(
    state: State<'_, AppState>,
    meeting_id: String,
    old_label: String,
    new_label: String,
) -> Result<MeetingTimeline, AppError> {
    rename_speaker_inner(state.inner(), &meeting_id, &old_label, new_label.trim())
}

/// Inner of [`rename_speaker`] taking `&AppState` (unit-testable gate + enroll). `new_label` is
/// already trimmed by the command wrapper.
pub(crate) fn rename_speaker_inner(
    state: &AppState,
    meeting_id: &str,
    old_label: &str,
    new_label: &str,
) -> Result<MeetingTimeline, AppError> {
    if new_label.is_empty() {
        return Err(AppError::InvalidArg("new speaker name is empty".into()));
    }
    // BLK-2b WRITE-GATE: a sealed-and-not-unlocked meeting's timeline `data` is blanked; refuse to
    // rename a speaker (would persist a near-empty plaintext timeline over the sealed blob in a
    // locked folder). Fail closed.
    if !meeting_is_unlocked(state, meeting_id)? {
        return Err(AppError::Locked(crate::errcode::tag(
                crate::errcode::MEETING_LOCKED,
                "this meeting's folder is locked — unlock it to rename a speaker",
            )));
    }
    let json = state
        .db
        .get_timeline_data(meeting_id)?
        .ok_or_else(|| AppError::InvalidArg("no timeline for this meeting yet".into()))?;
    let mut tl: crate::storage::models::MeetingTimeline = serde_json::from_str(&json)
        .map_err(|e| AppError::InvalidArg(format!("bad timeline data: {e}")))?;

    // Reconstruct the diarized CLUSTER for the OLD label BEFORE the rename rewrites it away. The FE
    // passes the DISPLAY label the lane shows ("Speaker 1"), not the raw `others-N` tag, so first try
    // the raw-tag parse (legacy / a raw-tag timeline), then fall back to segment↔turn overlap against
    // the still-original turns. A label with no overlapping diarized cluster (the "me" lane, or a
    // non-diarized meeting with no segments) → None → enroll nothing.
    let old_cluster = parse_others_cluster(old_label).or_else(|| {
        reconcile_meeting_speakers(state, meeting_id, Some(&tl.speakers))
            .cluster_for_label(old_label)
    });

    for turn in &mut tl.speakers {
        if turn.speaker == old_label {
            turn.speaker = new_label.to_string();
        }
    }
    let updated = serde_json::to_string(&tl)
        .map_err(|e| AppError::Storage(format!("serialize timeline: {e}")))?;
    // SEAM-F2 (2026-07-11 audit, edit lost): in a session-unlocked LOCKED folder the renamed timeline
    // must be RE-SEALED under the folder CK at write time — the pre-fix bare `set_timeline_data`
    // landed plaintext-only against the STALE sealed blob, so relock re-blanked the plaintext and the
    // next unlock restored the OLD speaker labels (the rename was destroyed). Open/rootless meeting →
    // the plain write inside the helper. Fail-closed on a missing session KEK.
    set_timeline_data_reseal_if_locked(state, meeting_id, &updated)?;

    // ENROLL-ON-RENAME (Phase 2, opt-in): if the OLD label resolves to a diarized cluster (either a
    // raw `others-{n}` tag or, via the reconciliation above, the display label the FE lane showed) and
    // the meeting produced a voiceprint for that cluster, bind the new person name to it so the next
    // meeting can re-identify this voice. Best-effort + no-op when: the opt-in is off, the label maps
    // to no cluster, or no voiceprint exists for that cluster (pre-opt-in recording). The rename itself
    // already succeeded regardless — a failed/absent enroll never fails the command. The WRITE is
    // anchored to THIS (already-unlocked) meeting; no other meeting's voiceprint is read/written.
    if let Some(cluster_index) = old_cluster {
        let enabled = state
            .config
            .lock()
            .map(|c| c.voiceprint_enabled)
            .unwrap_or(false);
        if enabled {
            match state
                .db
                .set_voiceprint_label_for_cluster(meeting_id, cluster_index, new_label)
            {
                Ok(n) => {
                    if n > 0 {
                        tracing::info!(
                            target: "transcribe", meeting_id = %meeting_id, cluster_index,
                            "enrolled a voiceprint on rename"
                        );
                    }
                }
                Err(e) => tracing::warn!(
                    target: "transcribe", error = %e,
                    "voiceprint enroll-on-rename failed (rename unaffected)"
                ),
            }
        }
    }
    Ok(tl)
}

/// Parse a diarized-cluster timeline label `others-{n}` → its cluster index, else None. The plain
/// `others` label (single remote speaker, no cluster suffix) and any human name return None.
fn parse_others_cluster(label: &str) -> Option<i64> {
    label
        .strip_prefix(crate::audio::merge::SPEAKER_OTHERS)?
        .strip_prefix('-')?
        .parse::<i64>()
        .ok()
}

/// Suggest a person label for each diarized `others-{n}` cluster of `meeting_id`, by cosine
/// re-identification against prior LABELED voiceprints. GATED: `meeting_is_unlocked` first (a locked
/// meeting yields no suggestions), then the candidate set is `list_voiceprints_visible` restricted to
/// labeled rows from OTHER meetings — a sealed prior is never in it. Only matches `>=`
/// `VOICEPRINT_MATCH_THRESHOLD` are returned. Empty when the opt-in is off, no voiceprint exists, or
/// nothing matches. NO PII is logged.
#[tauri::command]
pub fn suggest_speaker_labels(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<SpeakerSuggestion>, AppError> {
    suggest_speaker_labels_inner(state.inner(), &meeting_id)
}

/// Inner of [`suggest_speaker_labels`] taking `&AppState` (unit-testable gate).
pub(crate) fn suggest_speaker_labels_inner(
    state: &AppState,
    meeting_id: &str,
) -> Result<Vec<SpeakerSuggestion>, AppError> {
    use crate::transcribe::diarize::{
        suggest_voiceprint_labels, ClusterEmbeddingRef, LabeledEmbeddingRef,
        VOICEPRINT_MATCH_THRESHOLD,
    };
    // READ-GATE: a locked meeting surfaces nothing (its own clusters are invisible anyway, but fail
    // closed explicitly).
    if !meeting_is_unlocked(state, meeting_id)? {
        return Ok(Vec::new());
    }
    // The whole VISIBLE voiceprint corpus (sealed priors already excluded by the visibility clause).
    let unlocked = unlocked_snapshot(state)?;
    let all = state.db.list_voiceprints_visible(&unlocked)?;

    // THIS meeting's clusters (candidates to label) vs OTHER meetings' LABELED prints (the gallery).
    let mine: Vec<_> = all.iter().filter(|v| v.meeting_id == meeting_id).collect();
    if mine.is_empty() {
        return Ok(Vec::new());
    }
    let labeled_refs: Vec<LabeledEmbeddingRef<'_>> = all
        .iter()
        .filter(|v| v.meeting_id != meeting_id)
        .filter_map(|v| {
            v.label
                .as_deref()
                .filter(|l| !l.trim().is_empty())
                .map(|label| LabeledEmbeddingRef {
                    label,
                    embedding: &v.embedding,
                })
        })
        .collect();
    if labeled_refs.is_empty() {
        return Ok(Vec::new());
    }
    let cluster_refs: Vec<ClusterEmbeddingRef<'_>> = mine
        .iter()
        // Only suggest for clusters that are NOT already labeled in this meeting.
        .filter(|v| {
            v.label
                .as_deref()
                .map(|l| l.trim().is_empty())
                .unwrap_or(true)
        })
        .map(|v| ClusterEmbeddingRef {
            cluster_index: v.cluster_index as i32,
            embedding: &v.embedding,
        })
        .collect();

    let suggestions =
        suggest_voiceprint_labels(&cluster_refs, &labeled_refs, VOICEPRINT_MATCH_THRESHOLD);

    // RE-KEY by the DISPLAY label the FE lane actually shows: the timeline is LLM-generated, so lane
    // `speaker` = "Speaker 1"/a real name, NOT the raw `others-N` tag. Reconcile the cluster → that
    // display label via segment↔turn time-overlap so `suggestionByLabel().get(lane.speaker)` matches
    // for both multi-cluster and single-cluster 1:1. Best-effort: if the meeting has no timeline / no
    // segments (legacy, sealed-then-unlocked), reconciliation yields nothing → fall back to the raw
    // `others-N` tag (harmless — a legacy raw-tag timeline still matches; an LLM one just won't chip).
    let reconciliation = reconcile_meeting_speakers(state, meeting_id, None);
    Ok(suggestions
        .into_iter()
        .map(|s| {
            let cluster = s.cluster_index as i64;
            let speaker = reconciliation
                .label_for_cluster(cluster)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    format!(
                        "{}-{}",
                        crate::audio::merge::SPEAKER_OTHERS,
                        s.cluster_index
                    )
                });
            SpeakerSuggestion {
                speaker,
                suggested_label: s.label,
                score: s.score,
            }
        })
        .collect())
}

/// Build the cluster↔display-label reconciliation for THIS meeting from segment↔turn time-overlap.
/// Best-effort + gated by the caller (both call sites first pass `meeting_is_unlocked`): reads ONLY
/// this meeting's segments + its stored (or supplied) timeline turns — never another meeting's data,
/// so an enroll can never reach a sealed/other cluster. A missing timeline or missing segments (a
/// legacy or sealed-then-unlocked meeting) yields an empty reconciliation → no-suggestion / no-enroll,
/// never an error, never a fabricated cluster. Pass `turns` when the caller already parsed the
/// timeline (avoids a redundant DB read); pass `None` to load it from the DB. NO PII is logged.
fn reconcile_meeting_speakers(
    state: &AppState,
    meeting_id: &str,
    turns: Option<&[crate::storage::models::SpeakerTurn]>,
) -> crate::transcribe::diarize::SpeakerReconciliation {
    use crate::transcribe::diarize::{reconcile_speakers, TurnRef};
    let segments = state.db.get_segments(meeting_id).unwrap_or_default();
    // Own the turns when we have to load them, so the borrow outlives the ref view below.
    let loaded: Vec<crate::storage::models::SpeakerTurn> = match turns {
        Some(_) => Vec::new(),
        None => match state.db.get_timeline_data(meeting_id) {
            Ok(Some(json)) => serde_json::from_str::<MeetingTimeline>(&json)
                .map(|t| t.speakers)
                .unwrap_or_default(),
            _ => Vec::new(),
        },
    };
    let turns = turns.unwrap_or(&loaded);
    let turn_refs: Vec<TurnRef<'_>> = turns
        .iter()
        .map(|t| TurnRef {
            start_s: t.start_s,
            end_s: t.end_s,
            label: &t.speaker,
        })
        .collect();
    reconcile_speakers(&segments, &turn_refs)
}

/// Speaker + topic timeline for a meeting — READ-ONLY (cached-or-empty; NEVER generates).
///
/// perf-memory-audit / OOM: a passive Audio-tab open must not have a multi-GB side effect. Reading a
/// not-yet-cached timeline used to synchronously load the on-device Notes model (Qwen/Bielik) and
/// compile Metal shaders on first run, which swap-death-beachballed the whole Mac on OPEN. Generation
/// now lives in the SEPARATE, EXPLICIT `generate_timeline` command (auto-fired by the FE only for
/// cheap CLOUD providers; hidden behind a user click for on-device — see
/// `timeline_generation_on_device`).
#[tauri::command]
pub fn get_timeline(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<MeetingTimeline, AppError> {
    // Phase 0.5 READ-GATE: a sealed-and-not-unlocked meeting returns an EMPTY timeline (its
    // `timelines.data` is blanked at rest while sealed, but mask explicitly + skip regeneration so
    // we never re-derive a timeline from now-blank segments).
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Ok(MeetingTimeline::default());
    }
    // Return the CACHED timeline (coverage-repaired on read so a legacy cache — e.g. one ending at
    // 0:14 for a 0:45 recording — heals) or an EMPTY one when nothing is cached. No provider load.
    let segments = state.db.get_segments(&meeting_id)?;
    if let Some(json) = state.db.get_timeline_data(&meeting_id)? {
        if let Ok(mut t) = serde_json::from_str::<MeetingTimeline>(&json) {
            crate::summarize::timeline::repair_coverage(&mut t, &segments);
            return Ok(t);
        }
    }
    Ok(MeetingTimeline::default())
}

/// A meeting + its latest note for the Detail view.
/// Returns `None` if the meeting id is unknown.
///
/// PERF (Note-tab payload): the DTO's `segments` field is now emitted EMPTY. The FE Note tab never
/// renders the transcript — only the Audio tab does — yet packing the full `Vec<Segment>` here made
/// `detail()` carry ~0.5 MB for a 1h meeting (>1 MB for 2h), re-parsed/re-spread on every mutation.
/// The transcript is now fetched LAZILY by the Audio tab via [`get_meeting_segments`] (same read
/// gate). The `segments: Vec::new()` shape is byte-identical to the masked/locked DTO
/// (`super::masked_detail`) that every consumer already tolerates — no new DTO fork.
#[tauri::command]
pub async fn get_meeting_detail(
    app: AppHandle,
    meeting_id: String,
) -> Result<Option<MeetingDetailDto>, AppError> {
    offload_read(app, move |state| {
        get_meeting_detail_inner(state, &meeting_id)
    })
    .await
}

pub(crate) fn get_meeting_detail_inner(
    state: &AppState,
    meeting_id: &str,
) -> Result<Option<MeetingDetailDto>, AppError> {
    let _lifecycle = lifecycle_guard(state);
    let Some(anchor) = state.db.get_meeting_gate_anchor(meeting_id)? else {
        return Ok(None);
    };

    // Phase 0.5 READ-GATE: a meeting in a locked-and-NOT-session-unlocked folder returns a MASKED
    // DTO — `locked: true`, no note, no segments. The plaintext columns are blanked at rest while
    // sealed (and the audio is encrypted), but we mask explicitly so the FE never shows the empty
    // shell as if it were real content, and so the title can be masked too.
    //
    // `audio_path` is NULLED here too: the FE feeds it straight into `convertFileSrc` (the Tauri
    // `asset:` protocol, scoped to the audio dir) which serves the file to the webview WITHOUT
    // touching the `export_audio` command — i.e. the only audio read path that does NOT pass
    // through `meeting_is_unlocked`. While sealed the on-disk file is the AES-GCM `.enc` (so even a
    // leaked path serves ciphertext), but we must not depend on that single invariant: nulling the
    // path here means the gate covers the asset protocol regardless of the on-disk seal state, so a
    // plaintext WAV that briefly survives in the scoped dir (e.g. recorded into an already-sealed
    // folder, or a crash window) can never be served to a locked meeting's view.
    if !meeting_is_unlocked(state, meeting_id)? {
        return Ok(Some(masked_detail(anchor)));
    }

    let Some(meeting) = state.db.get_meeting(meeting_id)? else {
        return Ok(None);
    };

    let note_row = state.db.get_latest_note_for_meeting(meeting_id)?;
    // Phase 5: capture provenance from the note row BEFORE converting to NoteDto (NoteDto is a
    // subset and doesn't carry model fields). All three are None when the note is absent or when
    // the provider did not record provenance (pre-Phase-5 notes).
    let ai_provider = note_row.as_ref().map(|n| n.provider_id.clone());
    let ai_model = note_row.as_ref().and_then(|n| n.model_requested.clone());
    let model_served = note_row.as_ref().and_then(|n| n.model_served.clone());
    let note = note_row.map(|n| NoteDto {
        meeting_id: n.meeting_id,
        provider_id: n.provider_id,
        markdown: n.markdown,
        exported_path: n.exported_path,
    });
    // PERF: the transcript is NO LONGER packed into the detail DTO — the Note tab never renders it,
    // and it dominated the payload. The Audio tab fetches it lazily via `get_meeting_segments` (same
    // gate). Emit an empty Vec, keeping the field so the DTO shape is unchanged for every consumer.
    let segments = Vec::new();
    // GATED read: only past the `meeting_is_unlocked` gate above do we surface the persisted
    // assistant Q&A. The DB read is ALSO `visibility_clause`-gated (it returns empty for a sealed-
    // not-unlocked meeting) — defense-in-depth, double-gated exactly like the rest of the DTO.
    let unlocked = state
        .unlocked_folders
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    let assistant_interactions = state
        .db
        .list_assistant_interactions_visible(meeting_id, &unlocked)?;
    Ok(Some(MeetingDetailDto {
        meeting,
        note,
        segments,
        assistant_interactions,
        locked: false,
        ai_provider,
        ai_model,
        model_served,
    }))
}

/// LAZY transcript read for the Audio tab — the segments `get_meeting_detail` no longer ships in its
/// DTO (see the perf note there). Split into a thin `#[tauri::command]` wrapper over an `_inner` that
/// takes `&AppState` (the pervasive command-vs-`_inner` shape in this file — `set_focus_meeting`,
/// `brain_overview`, `rename_speaker`, …) so the read gate is unit-testable without a Tauri `State`.
#[tauri::command]
pub async fn get_meeting_segments(
    app: AppHandle,
    meeting_id: String,
) -> Result<Vec<Segment>, AppError> {
    offload_read(app, move |state| {
        get_meeting_segments_inner(state, &meeting_id)
    })
    .await
}

/// Phase 0.5 READ-GATE (mirrors `get_meeting_detail` / `get_timeline`): a sealed-and-NOT-session-
/// unlocked meeting returns an EMPTY Vec — never the raw rows. `db.get_segments` is a RAW,
/// non-visibility-gated read (it returns the still-present segment rows with their text BLANKED at
/// rest while sealed), so this gate is what keeps a locked meeting's transcript from leaking through
/// the lazy path. Returning `[]` (not `Locked`) matches the `get_timeline` / `masked_detail`
/// precedent — the Audio tab is unreachable while locked anyway.
pub(crate) fn get_meeting_segments_inner(
    state: &AppState,
    meeting_id: &str,
) -> Result<Vec<Segment>, AppError> {
    if !meeting_is_unlocked(state, meeting_id)? {
        return Ok(Vec::new());
    }
    state.db.get_segments(meeting_id)
}
