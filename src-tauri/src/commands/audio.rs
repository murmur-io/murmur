//! Audio / recording-lifecycle / voice-command / storage / voiceprint commands — extracted verbatim
//! from `commands` (God-file split, a PURE MOVE — the gate + at-rest-seal logic is UNCHANGED, only
//! relocated). This is the capture-and-audio read surface: the live recording meter + status resync,
//! mic mute, the manual voice-command arm/stop, the input-device picker, the recording-storage report
//! with manual prune, the audio-output probe, and the voiceprint management list/forget/clear. The
//! gated and at-rest-seal-sensitive members keep their EXACT prior body.
//! `list_voiceprints`/`list_voiceprints_inner` snapshot the LIVE session `unlocked` set via
//! `super::unlocked_snapshot` and push it through `list_voiceprints_visible`, so a
//! sealed-and-not-session-unlocked meeting's voiceprint is EXCLUDED (and the raw embedding is never
//! returned) — byte-identical to its pre-move form. `free_up_space` holds `super::lifecycle_guard`
//! across the prune so it can never interleave with a folder seal (`lock_folder`), exactly as before —
//! the audio-at-rest `.enc` seal invariant is untouched (this file adds no new seal/at-rest-write path;
//! it never hands the FE an on-disk audio path — the masked-`audio_path` rule stays in
//! `get_meeting_detail`/`masked_detail` in `commands/mod.rs`).
//! Every symbol keeps its EXACT prior body/signature and is re-exported at `crate::commands` via
//! `pub use audio_commands::*;` in `commands/mod.rs`, so `generate_handler![commands::recording_level]`
//! in `lib.rs` and every `crate::commands::…` caller resolve UNCHANGED. `use super::*` brings in the
//! shared DTOs, the crate `use`s, and the gate helpers `unlocked_snapshot` / `lifecycle_guard` (which
//! STAY in `commands/mod.rs`, already `pub(crate)`). No gate/mask/seal LOGIC changed — only relocation.

use super::*;

/// Live recording state, so a freshly-loaded webview can resync to a capture that is STILL running
/// in the (long-lived) Rust process. In `tauri dev` a frontend hot-reload swaps the webview without
/// restarting the backend, so the FE store resets to `idle` while `AppState.recorder` is still
/// `Some(..)` — the desync that made the next Start fail with "already recording". This exposes only
/// the ACTIVELY-recording meeting (which cannot be sealed — it's a fresh in-progress draft), so it
/// leaks no locked content.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatus {
    /// True while the backend recorder is actively capturing.
    pub recording: bool,
    /// The in-progress meeting id, or `None` when idle.
    pub meeting_id: Option<String>,
    /// The in-progress meeting's `started_at` (RFC3339), so the FE anchors its elapsed timer to the
    /// real start instead of an epoch-sized value. `None` when idle or the row can't be read.
    pub started_at: Option<String>,
    /// Whether SYSTEM audio capture is positively live right now (`None` when idle, or when this
    /// recording never asked for it).
    ///
    /// The helper can die mid-recording — it is a separate process — and until now nothing told the
    /// user. The mic keeps recording, the timer keeps counting, and the far side of the call is
    /// simply missing from the transcript, discovered after the meeting is over and unrepeatable.
    /// The backend already watches this every 100 ms to decide whether to un-mute the mic
    /// (`audio::system::mic_must_be_restored`); this surfaces the same fact.
    pub system_capture_alive: Option<bool>,
    /// Why system capture is not live, when it is not. Plain words, no PII, `None` while healthy.
    pub system_capture_note: Option<String>,
}

/// One stored voiceprint surfaced to a management view (opt-in voice biometrics). NEVER carries the
/// raw embedding — only the label + provenance + dimension the FE needs to list/forget. Read ONLY
/// through the gated `list_voiceprints` command (a sealed meeting's row never reaches here).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceprintInfo {
    pub id: String,
    pub meeting_id: String,
    /// The diarized cluster index within its source meeting (the `others-{n}` suffix).
    pub cluster_index: i64,
    /// The bound person name once the cluster is enrolled by rename (None until then).
    pub label: Option<String>,
    /// Embedding dimensionality (a harmless count; NOT the embedding itself).
    pub dim: i64,
    pub created_at: String,
}

/// Current mic peak level 0.0..=1.0 for the meter (0.0 when idle). Cheap, polled by UI ~10x/s.
///
/// This is ALSO the detection site for the 4h `MAX_RECORDING_SECONDS` hard TIME cap. The live-caption
/// loop (`transcribe::live`) is only spawned when a whisper model resolves — a user with no model
/// downloaded gets no live loop, yet the recording (and the cap) still happen — so the live loop is
/// NOT a reliable place to detect the cap. The FE polls THIS command every 100 ms while recording
/// (`recorder.store.ts` `level`), unconditionally, for the whole recording — making it the site that
/// ALWAYS runs. On the RISING edge (cap reached, notice not yet emitted this recording) we emit
/// [`crate::events::EVENT_RECORDING_CAPPED`] exactly once so the FE can surface the notice and call
/// `stop_recording` to finalize the meeting (the capped buffer is intact — Stop still yields a note).
/// Best-effort: a failed emit only warns; the meter read is unaffected.
#[tauri::command]
pub fn recording_level(app: AppHandle, state: State<'_, AppState>) -> Result<f32, AppError> {
    let recorder = state
        .recorder
        .lock()
        .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
    let Some(r) = recorder.as_ref() else {
        return Ok(0.0);
    };
    let level = r.level();
    if let Some(fault) = r.fault() {
        if !state
            .capture_fault_notified
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            crate::events::emit_recording_capture_fault(
                &app,
                fault,
                r.total_samples() as u64,
                r.source_sample_rate(),
            );
        }
    }
    // 4h TIME cap: fire the "maximum recording length reached" notice exactly ONCE per recording,
    // on the false→true transition. `capped_notified` is the per-recording rising-edge latch,
    // re-armed at each `start_recording`.
    if r.cap_reached() {
        let already = state
            .capped_notified
            .load(std::sync::atomic::Ordering::Relaxed);
        if crate::audio::recorder::should_emit_cap_notice(true, already) {
            state
                .capped_notified
                .store(true, std::sync::atomic::Ordering::Relaxed);
            // PII rule (§8): log the flag only — never any content.
            tracing::warn!(
                target: "audio",
                "maximum recording length reached — surfacing cap notice to finalize the meeting"
            );
            crate::events::emit_recording_capped(&app);
        }
    }
    Ok(level)
}

/// Report whether the backend is CURRENTLY capturing, plus the in-progress meeting id and its start
/// time. A freshly-loaded webview calls this ONCE on init to resync: a `tauri dev` frontend hot-reload
/// (or any webview reload / Cmd-R / webview crash) swaps the FE without restarting the long-lived Rust
/// process, so `AppState.recorder` can still be `Some(..)` (genuinely recording to disk) while the FE
/// `RecorderStore` has reset to `idle`. Without this resync the next Start hits `start_recording`'s
/// `already recording` guard, and the Record screen disagrees with the still-`RECORDING` meeting row.
/// Read-only + leak-safe: the actively-recording meeting is a fresh in-progress draft that cannot be
/// sealed, so no `meeting_is_unlocked` gate is needed (it returns no note/transcript/audio content).
#[tauri::command]
pub fn recording_status(state: State<'_, AppState>) -> Result<RecordingStatus, AppError> {
    // The live recorder — NOT the lingering `current_meeting` — is the source of truth for "am I
    // recording". After a full process restart the recorder is `None` again, so idle is reported even
    // if a ghost row somehow survived reconcile.
    let recording = {
        let recorder = state
            .recorder
            .lock()
            .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
        recorder.is_some()
    };
    let meeting_id = if recording {
        let current = state
            .current_meeting
            .lock()
            .map_err(|_| AppError::Audio("current_meeting mutex poisoned".into()))?;
        current.map(|u| u.to_string())
    } else {
        None
    };
    // Ask the SAME predicate the 100 ms watchdog uses, so the UI and the mic-restore decision can
    // never disagree about whether the helper is alive.
    let system_capture = if recording {
        let mut recorder = state
            .recorder
            .lock()
            .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
        recorder.as_mut().and_then(|r| {
            r.system.as_mut().map(|sys| {
                if sys.can_cover_muted_mic() {
                    (true, None)
                } else {
                    (
                        false,
                        Some("System audio stopped — only your microphone is being recorded.".into()),
                    )
                }
            })
        })
    } else {
        None
    };
    Ok(recording_status_dto(
        &state.db,
        recording,
        meeting_id,
        system_capture,
    ))
}

/// Assemble the [`RecordingStatus`] DTO from the recorder-presence flag + the in-progress meeting id,
/// resolving the start time from the persisted row. Split out of the command so both branches are
/// unit-testable WITHOUT a live [`Recorder`] (which needs mic hardware and can't be built headless).
/// The `started_at` lookup is best-effort: a missing/unreadable row just drops the anchor (the FE
/// falls back to "now") — it never fails the status read.
pub(crate) fn recording_status_dto(
    db: &crate::storage::Db,
    recording: bool,
    meeting_id: Option<String>,
    system_capture: Option<(bool, Option<String>)>,
) -> RecordingStatus {
    if !recording {
        return RecordingStatus {
            recording: false,
            meeting_id: None,
            started_at: None,
            system_capture_alive: None,
            system_capture_note: None,
        };
    }
    let started_at = meeting_id
        .as_deref()
        .and_then(|id| db.get_meeting(id).ok().flatten())
        .map(|m| m.started_at);
    let (system_capture_alive, system_capture_note) = match system_capture {
        Some((alive, note)) => (Some(alive), note),
        None => (None, None),
    };
    RecordingStatus {
        recording: true,
        meeting_id,
        started_at,
        system_capture_alive,
        system_capture_note,
    }
}

/// Live-toggle the microphone mute mid-recording (no stream teardown). While muted, the cpal
/// capture callback writes SILENCE into the mic buffer for those frames — the stream stays
/// full-length so its wall-clock timeline (and thus "me"/"others" alignment) is preserved, and
/// no real mic audio is captured (privacy). No-op if not recording.
#[tauri::command]
pub fn set_mic_muted(state: State<'_, AppState>, muted: bool) -> Result<(), AppError> {
    let mut recorder = state
        .recorder
        .lock()
        .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
    if let Some(r) = recorder.as_mut() {
        // Never silence the only audio source we have actually observed. System capture is
        // best-effort at Start and can degrade to mic-only on a Mac with missing TCC access or a
        // device-specific helper failure. A real frame plus a currently-live helper is the
        // minimum positive proof that "others" is still being recorded; without both the UI
        // must keep the mic live.
        if crate::audio::system::mic_must_be_restored(muted, r.system.as_mut()) {
            return Err(AppError::Audio(
                "microphone stayed on because system audio has not produced a frame; check Audio Recording or Screen Recording access"
                    .into(),
            ));
        }
        r.set_muted(muted);
    }
    Ok(())
}

/// Whether the mic is currently muted on the live recorder (false when not recording).
#[tauri::command]
pub fn is_mic_muted(state: State<'_, AppState>) -> Result<bool, AppError> {
    let recorder = state
        .recorder
        .lock()
        .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
    Ok(recorder.as_ref().map(|r| r.is_muted()).unwrap_or(false))
}

/// Result of arming a MANUAL voice command (the button trigger).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceCommandArmResult {
    /// True when the live loop is now armed to capture the next utterance as a command.
    pub listening: bool,
    /// Short, non-PII reason when `listening` is false (e.g. "not recording").
    pub reason: Option<String>,
}

/// ARM the MANUAL voice-command capture: the user clicked "ask the assistant", so the next spoken
/// utterance is taken as a command — NO wake word, NO word-order requirement. This command does NOT
/// itself transcribe; it sets [`crate::state::CaptureState`] on `AppState` so the already-running
/// live-caption loop (`transcribe::live`) collects + dispatches the command over the SAME gated +
/// consent-gated `handle_voice_action` path as the wake trigger (no new egress class). Opt-in PER
/// CLICK — independent of the `realtime_reactions` toggle.
///
/// The live loop only runs DURING recording, so if no recording is in progress we arm nothing and
/// return `listening: false` with a clear reason (the FE should enable the button only while
/// recording). Emits [`crate::events::EVENT_VOICE_COMMAND_LISTENING`] so the FE can show the
/// "listening…" state; the answer arrives later via `EVENT_VOICE_ACTION_RESULT`.
#[tauri::command]
pub fn begin_voice_command(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<VoiceCommandArmResult, AppError> {
    let result = begin_voice_command_inner(state.inner())?;
    if result.listening {
        tracing::info!(target: "voice", "manual voice command armed");
        let _ = app.emit(
            crate::events::EVENT_VOICE_COMMAND_LISTENING,
            crate::events::VoiceCommandListeningPayload { active: true },
        );
    }
    Ok(result)
}

/// Headless core of [`begin_voice_command`]: arm the manual-capture state on `AppState`, returning
/// whether the live loop is now listening. The live loop only runs DURING recording, so when no
/// recording is in progress we arm nothing and report `listening: false` with a reason (arming a
/// capture nothing will ever consume would leave the FE stuck "listening"). No `AppHandle`/IPC here,
/// so it is unit-testable without Tauri.
pub(crate) fn begin_voice_command_inner(
    state: &AppState,
) -> Result<VoiceCommandArmResult, AppError> {
    // Latch the recorder's current total-sample offset AT CLICK TIME so the live loop transcribes
    // only the POST-CLICK utterance (the command the user is about to speak), cleanly isolated from
    // any prior speech in the rolling buffer. `None` (no recorder) ⇒ not recording ⇒ arm nothing.
    let recorder = state
        .recorder
        .lock()
        .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
    let capture_bounds = recorder
        .as_ref()
        .and_then(|recorder| recorder.manual_capture_bounds());
    let live_running = state.live_running.load(std::sync::atomic::Ordering::SeqCst);
    match voice_command_arm_decision(capture_bounds, live_running) {
        // A refusal (not recording / no live consumer) — arm NOTHING and return the reason.
        Err(refusal) => Ok(refusal),
        // Cleared to arm: latch the fresh capture the live loop consumes.
        Ok((offset, max_end_sample)) => {
            // Lock order is recorder -> capture -> pending. Holding `recorder` until AFTER the
            // capture write closes the meeting-Stop race: Stop cannot remove ActiveRecording,
            // perform its terminal capture latch, and then have this Begin publish a fresh armed
            // generation that nobody owns.
            let mut guard = state.voice_command_capture.lock().map_err(|_| {
                AppError::Other(anyhow::anyhow!("voice-command capture mutex poisoned"))
            })?;
            let pending = state.pending_manual_command.lock().map_err(|_| {
                AppError::Other(anyhow::anyhow!("pending manual-command mutex poisoned"))
            })?;
            if guard.is_some() {
                return Ok(VoiceCommandArmResult {
                    listening: false,
                    reason: Some("voice command is already listening".into()),
                });
            }
            if pending.is_some() {
                return Ok(VoiceCommandArmResult {
                    listening: false,
                    reason: Some("previous voice command is still processing".into()),
                });
            }
            *guard = Some(crate::state::CaptureState::armed_from(
                offset,
                max_end_sample,
            ));
            Ok(VoiceCommandArmResult {
                listening: true,
                reason: None,
            })
        }
    }
}

/// PURE arm decision for [`begin_voice_command_inner`] (no state, no locks → unit-testable without a
/// real `Recorder`). `recording_bounds` carries click offset + absolute cap while recording;
/// `live_running` is whether the live-caption loop (the ONLY consumer of a voice capture) is running.
///
/// Returns `Ok((offset, max_end))` when the click may arm, else `Err(refusal)` with the reason:
/// - not recording ⇒ "not recording" (arm nothing — the live loop only runs during a recording);
/// - TP-F1: recording but NO live consumer ⇒ "voice needs the live model" (the fresh-install
///   heavy-turbo default spawns no live loop, so an armed capture would WEDGE with nothing to
///   transcribe/dispatch it — refuse cleanly instead of arming a consumer-less generation).
pub(crate) fn voice_command_arm_decision(
    recording_bounds: Option<(usize, usize)>,
    live_running: bool,
) -> std::result::Result<(usize, usize), VoiceCommandArmResult> {
    let Some((offset, max_end_sample)) = recording_bounds else {
        return Err(VoiceCommandArmResult {
            listening: false,
            reason: Some("not recording".into()),
        });
    };
    if !live_running {
        return Err(VoiceCommandArmResult {
            listening: false,
            reason: Some("voice needs the live model".into()),
        });
    }
    Ok((offset, max_end_sample))
}

/// STOP the MANUAL voice-command capture (CLICK-TO-STOP): the user clicked "stop" / "done", so the
/// FULL accumulated post-click utterance is the command. This does NOT itself transcribe or dispatch;
/// it latches the recorder's exact source-frame position and flips the armed
/// [`crate::state::CaptureState`]'s `ended` flag. The already-running live loop
/// (`transcribe::live`) waits for that exact certified spool prefix, then dispatches the FULL
/// accumulated command over the SAME gated + consent-gated `handle_voice_action` path (no new
/// read/egress class). Audio spoken after the click is never included just because the next live
/// tick is delayed.
///
/// The dispatch + the "thinking…" PROCESSING event are emitted by the live loop, so the answer still
/// arrives via [`crate::events::EVENT_VOICE_ACTION_RESULT`]. On a NOT-armed state (no capture in
/// progress — the user double-clicked, or it already auto-stopped at the backstop) this is a graceful
/// no-op (`stopped: false`), never an error.
#[tauri::command]
pub fn end_voice_command(state: State<'_, AppState>) -> Result<VoiceCommandEndResult, AppError> {
    let result = end_voice_command_inner(state.inner())?;
    if result.stopped {
        tracing::info!(target: "voice", "manual voice command stopped by user — dispatching");
    }
    Ok(result)
}

/// Result of stopping a MANUAL voice command (the "stop" click).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceCommandEndResult {
    /// True when an armed capture was found and flagged to dispatch; false when nothing was armed
    /// (graceful no-op — the live loop already cleared it via dispatch or the backstop).
    pub stopped: bool,
}

/// Narrow the command's existing hard cap to an observed Stop position. Repeated Stops can only
/// preserve or shorten the range; a stale/early observation clamps to an empty range at `start`.
pub(crate) fn latch_manual_end_sample(
    start_sample: Option<usize>,
    existing_max: Option<usize>,
    observed_stop: Option<usize>,
) -> Option<usize> {
    let Some(observed_stop) = observed_stop else {
        return existing_max;
    };
    Some(
        observed_stop
            .min(existing_max.unwrap_or(observed_stop))
            .max(start_sample.unwrap_or(0)),
    )
}

/// Headless core of [`end_voice_command`]: atomically (with respect to arm's recorder→capture lock
/// order) latch the exact source-frame Stop position and flag the capture. A NOT-armed state is a
/// graceful no-op (`stopped: false`). No `AppHandle`/IPC here, so it remains unit-testable without
/// Tauri or an audio device.
pub(crate) fn end_voice_command_inner(state: &AppState) -> Result<VoiceCommandEndResult, AppError> {
    // Keep the same lock order as `begin_voice_command_inner`: recorder, then capture. Holding the
    // recorder guard through the capture update prevents a concurrent re-arm from receiving an old
    // command's Stop position.
    let recorder = state
        .recorder
        .lock()
        .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
    let observed_stop = recorder.as_ref().map(|recorder| recorder.total_samples());
    let mut guard = state
        .voice_command_capture
        .lock()
        .map_err(|_| AppError::Other(anyhow::anyhow!("voice-command capture mutex poisoned")))?;
    match guard.as_mut() {
        Some(capture) => {
            capture.max_end_sample = latch_manual_end_sample(
                capture.start_sample,
                capture.max_end_sample,
                observed_stop,
            );
            capture.ended = true;
            Ok(VoiceCommandEndResult { stopped: true })
        }
        // Nothing armed (already dispatched / backstop-stopped / never started) → graceful no-op.
        None => Ok(VoiceCommandEndResult { stopped: false }),
    }
}

/// List available microphone input devices for the FE picker (name + default flag).
#[tauri::command]
pub fn list_input_devices() -> Result<Vec<crate::audio::InputDeviceInfo>, AppError> {
    Ok(crate::audio::list_input_devices())
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageReportDto {
    pub audio_dir: String,
    pub used_bytes: u64,
    pub limit_bytes: Option<u64>,
    pub playback_bytes: u64,
    pub masters_bytes: u64,
    pub sealed_bytes: u64,
    pub recording_count: u64,
    pub auto_prune: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PruneSummaryDto {
    pub freed_bytes: u64,
    pub pruned_count: u64,
    pub masters_deleted: u64,
}

/// Recording-storage usage report: on-disk audio path, byte totals bucketed by category,
/// recording count, and the current cap + auto-prune flag. Sizes only — no content.
#[tauri::command]
pub fn get_storage_report(state: State<'_, AppState>) -> Result<StorageReportDto, AppError> {
    let (limit_bytes, auto_prune) = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        (
            c.audio_storage_limit_gb
                .map(|g| g as u64 * crate::storage::usage::BYTES_PER_GB),
            c.audio_auto_prune,
        )
    };
    let dir = crate::pipeline::audio_dir()?;
    let u = crate::storage::usage::scan_audio_usage(&dir)?;
    Ok(StorageReportDto {
        audio_dir: dir.to_string_lossy().into_owned(),
        used_bytes: u.used_bytes,
        limit_bytes,
        playback_bytes: u.playback_bytes,
        masters_bytes: u.masters_bytes,
        sealed_bytes: u.sealed_bytes,
        recording_count: u.recording_count,
        auto_prune,
    })
}

/// Manual "Free up space": prune oldest recordings to the cap NOW (works even when auto-prune
/// is off). Requires a cap — with none set it is an inert zero summary (the FE disables the
/// button). Never touches notes or locked audio.
#[tauri::command]
pub fn free_up_space(state: State<'_, AppState>) -> Result<PruneSummaryDto, AppError> {
    let limit_bytes = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        c.audio_storage_limit_gb
            // `Some(0)` is not a "delete everything" cap → no cap (mirrors `AppConfig::load`).
            .filter(|g| *g > 0)
            .map(|g| g as u64 * crate::storage::usage::BYTES_PER_GB)
    };
    let Some(limit) = limit_bytes else {
        return Ok(PruneSummaryDto {
            freed_bytes: 0,
            pruned_count: 0,
            masters_deleted: 0,
        });
    };
    let dir = crate::pipeline::audio_dir()?;
    // Hold the seal lifecycle guard across the prune so it can never interleave with a folder
    // seal (`lock_folder`) — the same guard every other multi-step audio-path mutator holds.
    // Acquired AFTER the config lock is released (single lock order: lifecycle ⊃ db, never
    // config held while holding lifecycle).
    let _lifecycle = lifecycle_guard(state.inner());
    let s = crate::storage::usage::prune_to_limit(&state.db, &dir, limit, None)?;
    Ok(PruneSummaryDto {
        freed_bytes: s.freed_bytes,
        pruned_count: s.pruned_count,
        masters_deleted: s.masters_deleted,
    })
}

/// Reveal the recordings folder in Finder (macOS `open`). No content read.
#[tauri::command]
pub fn reveal_audio_dir() -> Result<(), AppError> {
    let dir = crate::pipeline::audio_dir()?;
    std::process::Command::new("open")
        .arg(&dir)
        .spawn()
        .map_err(|e| AppError::Storage(format!("reveal audio dir: {e}")))?;
    Ok(())
}

/// Whether the CURRENT default audio output is the built-in speakers (echo risk while
/// capturing system audio). Best-effort introspection — `None` when undeterminable.
#[tauri::command]
pub fn output_is_builtin_speakers() -> Result<Option<bool>, AppError> {
    Ok(crate::audio::output::default_output_is_builtin_speakers())
}

/// List stored voiceprints for a management view (label + source meeting + cluster + dim), GATED —
/// a sealed-not-unlocked meeting's voiceprint is EXCLUDED. The raw embedding is NEVER returned.
#[tauri::command]
pub fn list_voiceprints(state: State<'_, AppState>) -> Result<Vec<VoiceprintInfo>, AppError> {
    list_voiceprints_inner(state.inner())
}

/// Inner of [`list_voiceprints`] taking `&AppState` (unit-testable gate).
pub(crate) fn list_voiceprints_inner(state: &AppState) -> Result<Vec<VoiceprintInfo>, AppError> {
    let unlocked = unlocked_snapshot(state)?;
    let rows = state.db.list_voiceprints_visible(&unlocked)?;
    Ok(rows
        .into_iter()
        .map(|v| VoiceprintInfo {
            id: v.id,
            meeting_id: v.meeting_id,
            cluster_index: v.cluster_index,
            label: v.label,
            dim: v.dim,
            created_at: v.created_at,
        })
        .collect())
}

/// FORGET one stored voiceprint by id (hard delete — a voice biometric the user chose to erase).
/// Idempotent. Content-free logging (the id only). Not itself a content READ, so no gate is needed
/// (a delete widens no visibility); the management list it feeds IS gated.
#[tauri::command]
pub fn forget_voiceprint(state: State<'_, AppState>, id: String) -> Result<(), AppError> {
    let removed = state.db.delete_voiceprint(&id)?;
    tracing::info!(target: "transcribe", voiceprint_id = %id, removed, "voiceprint forgotten");
    Ok(())
}

/// CLEAR every stored voiceprint (the "forget all captured voices" affordance). Content-free
/// logging (a count only).
#[tauri::command]
pub fn clear_voiceprints(state: State<'_, AppState>) -> Result<(), AppError> {
    let n = state.db.clear_voiceprints()?;
    tracing::info!(target: "transcribe", count = n, "all voiceprints cleared");
    Ok(())
}

/// Whether the OPTIONAL parakeet live-ASR engine's four int8 models are all present on disk (in
/// `<models_dir>/parakeet-tdt-0.6b-v3-int8`). A file-on-disk check only — no content read / no
/// egress. Feeds the Settings ▸ Transcription engine picker (offer the download when absent).
#[tauri::command]
pub fn parakeet_models_present() -> Result<bool, AppError> {
    Ok(crate::transcribe::model::parakeet_models_present())
}

/// (Re)start the voice-trigger listener if enabled — model present and not recording —
/// replacing any existing one. Safe to call repeatedly to reconcile after a config change
/// or once a recording finishes.
pub fn restart_voice_listener(app: AppHandle) {
    let state = app.state::<AppState>();
    let _transition = state
        .voice_listener_lifecycle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Ok(mut guard) = state.voice_listener.lock() {
        if let Some(listener) = guard.as_mut() {
            match listener.stop_with_timeout(std::time::Duration::from_secs(5)) {
                Ok(true) => *guard = None,
                Ok(false) => {
                    tracing::warn!(target: "voice", "previous voice listener did not stop before restart deadline; keeping it owned and skipping replacement");
                    return;
                }
                Err(error) => {
                    tracing::warn!(target: "voice", error = %error, "previous voice listener failed during restart; clearing finished owner");
                    *guard = None;
                }
            }
        }
    }
    crate::audio::listener::release_wake_transcriber_cache();
    let (enabled, language) = match state.config.lock() {
        Ok(c) => (c.voice_trigger, c.language.clone()),
        Err(_) => return,
    };
    if !enabled {
        return;
    }
    // Don't grab the mic while real capture is active OR during Start's await-heavy preparation
    // window before the recorder slot is installed. The transition mutex makes this check atomic
    // with listener construction/storage relative to `stop_voice_listener` in Start.
    if state
        .recording_starting
        .load(std::sync::atomic::Ordering::Acquire)
        || state.recorder.lock().map(|g| g.is_some()).unwrap_or(true)
    {
        return;
    }
    // T1.3 — the standby wake listener decodes a ~2.2 s mic window every few seconds for the
    // WHOLE time it is armed, so it must run the SMALLEST downloaded model (tiny → base →
    // small, first present), NEVER the configured model (a medium/large standby decode is a
    // continuous heat + RAM source). Wake-phrase matching needs rough text only. With none of
    // the three small sizes downloaded the listener does not start (download `tiny`/`base`/
    // `small` to enable it) — it never silently escalates to a big model.
    match crate::transcribe::model::smallest_wake_model(language.as_deref().unwrap_or("")) {
        Some(model_path) => {
            let listener =
                crate::audio::listener::VoiceListener::start(app.clone(), model_path, language);
            if let Ok(mut g) = state.voice_listener.lock() {
                *g = Some(listener);
            }
        }
        None => tracing::warn!(
            target: "voice",
            "voice trigger enabled but no tiny/base/small whisper model downloaded; listener not started"
        ),
    }
}

/// Stop + drop the voice-trigger listener within a fixed deadline, releasing the mic. A timed-out
/// worker remains in AppState with its stop flag set; callers must fail before opening real capture
/// and may retry reaping it later.
pub fn stop_voice_listener(app: &AppHandle) -> Result<(), AppError> {
    let state = app.state::<AppState>();
    let _transition = state
        .voice_listener_lifecycle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut guard = state
        .voice_listener
        .lock()
        .map_err(|_| AppError::Audio("voice-listener mutex poisoned".into()))?;
    let Some(listener) = guard.as_mut() else {
        crate::audio::listener::release_wake_transcriber_cache();
        return Ok(());
    };
    match listener.stop_with_timeout(std::time::Duration::from_secs(5)) {
        Ok(true) => {
            *guard = None;
            drop(guard);
            crate::audio::listener::release_wake_transcriber_cache();
            Ok(())
        }
        Ok(false) => Err(AppError::Unavailable(
            "voice listener did not stop before the recording-start deadline".into(),
        )),
        Err(error) => {
            *guard = None;
            drop(guard);
            crate::audio::listener::release_wake_transcriber_cache();
            Err(error)
        }
    }
}
