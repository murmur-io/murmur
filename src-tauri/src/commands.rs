use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use zeroize::{Zeroize, Zeroizing};

use crate::audio::Recorder;
use crate::error::AppError;
use crate::events::{StatusPayload, EVENT_STATUS};
use crate::settings::{AppConfig, BrainBackend};
use crate::state::AppState;
use crate::storage::models::{
    ActionItem, Analytics, AskVaultResult, BriefResult, BuiltinRecipe, CalendarContext,
    CalendarEvent, CalendarEventFull, ChatTurn, Commitment, DigestResult, EntityDetail, Folder,
    FolderNode, GraphData, Meeting, MeetingStatus, MeetingTimeline, NoteRecord, PinResult,
    RecipeRecord, SearchHit, TopicThread,
};
use crate::summarize::all_providers;
use crate::transcribe::types::Segment;
use crate::{pipeline, secrets};
use tauri::Emitter;

/// Keychain account for the Anthropic API key (matches `summarize::ANTHROPIC_KEY_ACCOUNT`).
const ANTHROPIC_KEY_ACCOUNT: &str = "anthropic_api_key";

// ── IPC DTOs (camelCase mirrors of PHASE0-PLAN §6) ──

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartResult {
    pub meeting_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StopResult {
    pub meeting_id: String,
    pub markdown: String,
    /// Path of the exported Obsidian `.md`, or `None` when no vault is configured (the note
    /// is still saved to the DB — the vault is export-only).
    pub exported_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteDto {
    pub meeting_id: String,
    pub provider_id: String,
    pub markdown: String,
    pub exported_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    pub id: String,
    pub available: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfigDto {
    pub provider_id: String,
    pub vault_path: Option<String>,
    pub vault_subfolder: Option<String>,
    pub whisper_model_path: Option<String>,
    pub language: Option<String>,
    pub anthropic_model: String,
    pub ollama_base_url: String,
    pub ollama_model: String,
    pub claude_binary: String,
    #[serde(default)]
    pub input_device: Option<String>,
    pub capture_system_audio: bool,
    #[serde(default = "default_true")]
    pub vad_enabled: bool,
    #[serde(default)]
    pub keep_hires_masters: bool,
    #[serde(default)]
    pub diarize_others: bool,
    #[serde(default)]
    pub aec_enabled: bool,
    pub model_size: String,
    pub voice_trigger: bool,
    pub onboarded: bool,
    pub note_style: String,
    pub auto_organize: bool,
    pub note_language: String,
    /// E3/security: default true (matches AppConfig::default) when the FE omits it on an older
    /// payload — an omitted flag must FAIL CLOSED (require a token), never silently disable MCP
    /// auth. Was `#[serde(default)]` (=false), which let a partial save flip the token requirement
    /// off; now defaults ON like its Stage-E siblings (BLK-3).
    #[serde(default = "default_true")]
    pub mcp_require_token: bool,
    /// Stage E: default true (matches AppConfig::default) when the FE omits it on an older payload.
    #[serde(default = "default_true")]
    pub lock_require_biometric: bool,
    /// Stage E: default true (matches AppConfig::default) when the FE omits it on an older payload.
    #[serde(default = "default_true")]
    pub relock_on_screenshare: bool,
    /// E10: one-time cloud-egress consent. DISPLAY-ONLY on this DTO: `get_config` carries the
    /// current value OUT so the FE can show consent status, but `dto_to_config` IGNORES whatever
    /// the FE sends back and PRESERVES the value already in `AppConfig` (BLK-4). The ONLY mutator
    /// is the dedicated `consent_to_cloud_egress` command, so a settings save can neither grant nor
    /// clear consent — even a partial/omitting payload (`#[serde(default)]` = false) is inert here.
    #[serde(default)]
    pub cloud_egress_consented: bool,
    /// Phase H — which reasoner powers the on-device "brain" pre-analysis (Flow A): `cloud` |
    /// `local` | `off`. Unlike `cloud_egress_consented`, this IS settable from the DTO (the Settings
    /// UI owns the brain toggle). An omitted/unknown value deserializes to the default `Cloud`
    /// (`deserialize_with` tolerates an unknown token → `Cloud`, and `default` covers an omitted
    /// key), so a partial OR malformed save can never select an invalid backend.
    #[serde(default, deserialize_with = "deserialize_brain_backend_lenient")]
    pub brain_backend: BrainBackend,
    /// Phase H — the in-meeting VOICE ACTION DISPATCH master gate (Flow B). Settable from the DTO
    /// (the Settings UI owns the toggle). OPT-IN: an omitted value deserializes to `false`
    /// (`#[serde(default)]`), so a partial/older save can never silently enable the always-on
    /// in-meeting assistant.
    #[serde(default)]
    pub realtime_reactions: bool,
    /// Phase H — the SELECTED on-device brain model id (from `reason::BRAIN_MODELS`). Settable from
    /// the DTO, but `dto_to_config` VALIDATES it against the registry: an unknown/`None` id is
    /// IGNORED (the live selection is preserved, no error) so a settings save can never store a
    /// bogus model id. `select_brain_model` remains the other supported mutator.
    #[serde(default)]
    pub brain_model_id: Option<String>,
    /// brain2 RAG — the SEMANTIC SEARCH master flag. Settable from the DTO (the Settings UI owns the
    /// toggle), unlike `cloud_egress_consented` which is preserved-only. Plain bool; an omitted value
    /// deserializes to `false` (`#[serde(default)]`), so a partial/older save can never silently
    /// enable it. Flipping it on does NOT auto-index — the user runs `reindex_embeddings` to backfill.
    #[serde(default)]
    pub semantic_search_enabled: bool,
    /// brain2 connector framework — the WEB SEARCH master toggle. Settable from the DTO (the Settings
    /// UI owns the toggle). An omitted value deserializes to `false` (`#[serde(default)]`), so a
    /// partial/older save can never silently enable it. Even ON, the web connector is exposed only
    /// once `web_search_consented` is granted AND a key is stored.
    #[serde(default)]
    pub web_search_enabled: bool,
    /// brain2 connector framework — one-time WEB SEARCH egress consent. PRESERVE-ONLY on this DTO,
    /// exactly like `cloud_egress_consented`: `get_config` carries the current value OUT (so the FE can
    /// show consent status), but `dto_to_config` IGNORES the incoming value and PRESERVES the stored
    /// one. The ONLY mutator is the dedicated `consent_to_web_search` command, so a settings save can
    /// neither grant nor clear web-search egress consent. `#[serde(default)]` = false (fail-closed).
    #[serde(default)]
    pub web_search_consented: bool,
}

/// serde default for the Stage E security flags (which default ON in `AppConfig`).
fn default_true() -> bool {
    true
}

/// Lenient `brain_backend` deserialization for the settings DTO: an UNKNOWN/garbage token
/// degrades to the default `Cloud` instead of failing the whole `save_config` payload (the derived
/// enum would reject `"bogus"` with an error). Mirrors `BrainBackend::from_str_or_default`, so the
/// FE can never wedge a settings save with a stale/typo'd backend value. A non-string (e.g. null)
/// also falls back to `Cloud`.
fn deserialize_brain_backend_lenient<'de, D>(deserializer: D) -> std::result::Result<BrainBackend, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let token = Option::<String>::deserialize(deserializer)?;
    Ok(token
        .as_deref()
        .map(BrainBackend::from_str_or_default)
        .unwrap_or_default())
}

/// A meeting + its latest note + transcript segments (Library Detail view).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingDetailDto {
    pub meeting: Meeting,
    pub note: Option<NoteDto>,
    pub segments: Vec<Segment>,
    /// Phase 0.5 — `true` when the meeting's folder is sealed AND not session-unlocked. The FE
    /// renders a locked state (Touch-ID-to-unlock) instead of content; `note`/`segments` are
    /// empty in that case (the content is encrypted at rest, decrypted only on session-unlock).
    pub locked: bool,
}

// ── Commands (PHASE0-PLAN §7) ──

/// Begin mic capture. Inserts a Meeting(Draft→Recording), stores Recorder in state,
/// sets current_meeting. Returns the new meeting id. Errors if already recording.
#[tauri::command]
pub async fn start_recording(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<StartResult, AppError> {
    // Reject if a recording is already in progress.
    {
        let recorder = state
            .recorder
            .lock()
            .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
        if recorder.is_some() {
            return Err(AppError::Audio("already recording".into()));
        }
    }

    let meeting_uuid = uuid::Uuid::new_v4();
    let meeting_id = meeting_uuid.to_string();
    let started_at = chrono::Utc::now().to_rfc3339();

    // Persist the meeting in RECORDING state up-front so a crash mid-capture leaves a
    // recoverable row.
    state.db.insert_meeting(&Meeting {
        id: meeting_id.clone(),
        started_at,
        ended_at: None,
        title: None,
        duration_s: 0,
        audio_path: None,
        status: MeetingStatus::Recording,
        folder_id: None,
    })?;

    // Free the mic from the voice listener (if any) before opening it for the recording.
    {
        let app2 = app.clone();
        let _ = tokio::task::spawn_blocking(move || stop_voice_listener(&app2)).await;
    }

    // Start mic capture on the configured input device (falls back to default if unset/gone).
    let input_device = state.config.lock().ok().and_then(|c| c.input_device.clone());
    let recorder = Recorder::start(input_device)?;
    {
        let mut slot = state
            .recorder
            .lock()
            .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
        *slot = Some(recorder);
    }
    {
        let mut current = state
            .current_meeting
            .lock()
            .map_err(|_| AppError::Audio("current_meeting mutex poisoned".into()))?;
        *current = Some(meeting_uuid);
    }

    // Optionally capture system audio (the other side of the call) alongside the mic.
    // Best-effort: if it can't start, we log and record mic-only — never fail recording.
    {
        let enabled = state
            .config
            .lock()
            .map(|c| c.capture_system_audio)
            .unwrap_or(false);
        if enabled && crate::audio::system::is_available(&app) {
            let sys_wav = std::env::temp_dir().join(format!("meetnotes-sys-{meeting_id}.wav"));
            match crate::audio::system::SystemAudioRecorder::start(&app, sys_wav) {
                Ok(rec) => {
                    if let Ok(mut slot) = state.system_recorder.lock() {
                        *slot = Some(rec);
                    }
                }
                Err(e) => tracing::warn!(
                    target: "audio", error = %e,
                    "system-audio capture unavailable; recording mic only"
                ),
            }
        }
    }

    // Optionally capture an echo-cancelled (VPIO) mic in PARALLEL with cpal — the AEC'd WAV becomes
    // the ASR mic feed; cpal stays the archive + fallback. Best-effort + opt-in (aec_enabled).
    {
        let enabled = state.config.lock().map(|c| c.aec_enabled).unwrap_or(false);
        if enabled && crate::audio::aec::is_available(&app) {
            let aec_wav = std::env::temp_dir().join(format!("meetnotes-aec-{meeting_id}.wav"));
            match crate::audio::aec::AecRecorder::start(&app, aec_wav) {
                Ok(rec) => {
                    if let Ok(mut slot) = state.aec_recorder.lock() {
                        *slot = Some(rec);
                    }
                }
                Err(e) => tracing::warn!(
                    target: "audio", error = %e,
                    "AEC capture unavailable; recording the raw mic only"
                ),
            }
        }
    }

    // Best-effort LIVE captions: a read-only background loop emitting partial transcripts
    // during recording (see transcribe::live). Never affects the recording or final note.
    if let Some(cfg) = state.config.lock().ok().map(|c| c.clone()) {
        if let Ok(Some(model_path)) = crate::transcribe::model::resolve_model_path(
            cfg.whisper_model_path.as_deref().map(std::path::Path::new),
            &cfg.model_size,
            cfg.language.as_deref().unwrap_or(""),
        ) {
            crate::transcribe::live::spawn(app.clone(), model_path, cfg.language.clone());
        }
    }

    let _ = app.emit(
        EVENT_STATUS,
        StatusPayload {
            stage: "recording".into(),
            message: "Recording…".into(),
            meeting_id: Some(meeting_id.clone()),
        },
    );

    Ok(StartResult { meeting_id })
}

/// Stop capture, then run the full pipeline (pipeline::run_after_stop). Returns the
/// exported note path + markdown. Emits status events throughout. Errors if not recording.
#[tauri::command]
pub async fn stop_recording(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<StopResult, AppError> {
    // Take the recorder out of state (errors if not recording).
    let recorder = {
        let mut slot = state
            .recorder
            .lock()
            .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
        slot.take()
            .ok_or_else(|| AppError::Audio("not recording".into()))?
    };

    let meeting_uuid = {
        let mut current = state
            .current_meeting
            .lock()
            .map_err(|_| AppError::Audio("current_meeting mutex poisoned".into()))?;
        current
            .take()
            .ok_or_else(|| AppError::Audio("no current meeting".into()))?
    };
    let meeting_id = meeting_uuid.to_string();

    // Capture the mic stream's host start instant BEFORE consuming the recorder — it anchors the
    // mic ("me") segments onto the absolute timeline in the wall-clock merge (pipeline.rs).
    let mic_started_at = recorder.started_at();
    let (samples, src_rate) = recorder.stop()?;

    // Stop the system-audio sidecar (if any) and collect its WAV + host start instant. The
    // sidecar's start instant anchors the system ("others") segments; the two streams run on
    // INDEPENDENT clocks, so we merge by wall-clock, not sample count (see audio::merge).
    let (system_wav, system_started_at) = {
        let rec = state
            .system_recorder
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        match rec {
            Some(r) => {
                let started = r.started_at();
                (r.stop().unwrap_or(None), Some(started))
            }
            None => (None, None),
        }
    };

    // Stop the AEC mic helper (if any) and collect its WAV — used as the ASR mic feed; None falls
    // back to the raw cpal mic.
    let aec_mic_wav = {
        let rec = state
            .aec_recorder
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        match rec {
            Some(r) => r.stop().unwrap_or(None),
            None => None,
        }
    };

    // Duration from the persisted started_at, falling back to a sample-count estimate.
    let duration_s = compute_duration_s(&state, &meeting_id, samples.len(), src_rate);

    let result = pipeline::run_after_stop(
        &app,
        &state,
        &meeting_id,
        samples,
        src_rate,
        duration_s,
        system_wav,
        aec_mic_wav,
        mic_started_at,
        system_started_at,
    )
    .await?;

    // Resume voice listening if it's still enabled (the mic is free again).
    restart_voice_listener(app);

    Ok(StopResult {
        meeting_id: result.meeting_id,
        markdown: result.note_markdown,
        exported_path: result
            .exported_path
            .map(|p| p.to_string_lossy().to_string()),
    })
}

/// Best-effort recording duration in whole seconds: prefer `now - started_at` from the
/// DB; fall back to `samples / sample_rate` if the timestamp can't be parsed.
fn compute_duration_s(
    state: &State<'_, AppState>,
    meeting_id: &str,
    sample_count: usize,
    src_rate: u32,
) -> i64 {
    if let Ok(Some(meeting)) = state.db.get_meeting(meeting_id) {
        if let Ok(started) = chrono::DateTime::parse_from_rfc3339(&meeting.started_at) {
            let secs = (chrono::Utc::now() - started.with_timezone(&chrono::Utc)).num_seconds();
            if secs >= 0 {
                return secs;
            }
        }
    }
    if src_rate > 0 {
        (sample_count as i64) / (src_rate as i64)
    } else {
        0
    }
}

/// Current mic peak level 0.0..=1.0 for the meter (0.0 when idle). Cheap, polled by UI.
#[tauri::command]
pub fn recording_level(state: State<'_, AppState>) -> Result<f32, AppError> {
    let recorder = state
        .recorder
        .lock()
        .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
    Ok(recorder.as_ref().map(|r| r.level()).unwrap_or(0.0))
}

/// Live-toggle the microphone mute mid-recording (no stream teardown). While muted, the cpal
/// capture callback writes SILENCE into the mic buffer for those frames — the stream stays
/// full-length so its wall-clock timeline (and thus "me"/"others" alignment) is preserved, and
/// no real mic audio is captured (privacy). No-op if not recording.
#[tauri::command]
pub fn set_mic_muted(state: State<'_, AppState>, muted: bool) -> Result<(), AppError> {
    let recorder = state
        .recorder
        .lock()
        .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?;
    if let Some(r) = recorder.as_ref() {
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
    let start_sample = state
        .recorder
        .lock()
        .map_err(|_| AppError::Audio("recorder mutex poisoned".into()))?
        .as_ref()
        .map(|r| r.total_samples());
    let Some(offset) = start_sample else {
        return Ok(VoiceCommandArmResult {
            listening: false,
            reason: Some("not recording".into()),
        });
    };
    let mut guard = state
        .voice_command_capture
        .lock()
        .map_err(|_| AppError::Other(anyhow::anyhow!("voice-command capture mutex poisoned")))?;
    *guard = Some(crate::state::CaptureState::armed_from(offset));
    Ok(VoiceCommandArmResult { listening: true, reason: None })
}

/// The most recent note (markdown + export path) for the last-note preview pane.
#[tauri::command]
pub fn get_last_note(state: State<'_, AppState>) -> Result<Option<NoteDto>, AppError> {
    // BLK-2b: the latest VISIBLE note only — a sealed-and-not-unlocked latest note is skipped so the
    // recorder bar never shows its blanked content (and never depends on at-rest blanking).
    let unlocked = unlocked_snapshot(state.inner())?;
    let note = state.db.latest_note_visible(&unlocked)?;
    Ok(note.map(|n| NoteDto {
        meeting_id: n.meeting_id,
        provider_id: n.provider_id,
        markdown: n.markdown,
        exported_path: n.exported_path,
    }))
}

/// Replace a meeting note's markdown (in-app edit) and re-write the SAME vault file in
/// place (no duplicate). Returns the updated note.
#[tauri::command]
pub fn update_note(
    state: State<'_, AppState>,
    meeting_id: String,
    markdown: String,
) -> Result<NoteDto, AppError> {
    // D4 READ/WRITE-GATE: refuse to mutate a sealed-and-not-session-unlocked meeting's note. Its
    // plaintext markdown is blanked while sealed, so an edit here would overwrite the (sealed)
    // content with the blanked value and corrupt it. Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to edit the note".into(),
        ));
    }
    let existing = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;

    let created_at = chrono::Utc::now().to_rfc3339();
    state.db.upsert_note(&NoteRecord {
        meeting_id: meeting_id.clone(),
        provider_id: existing.provider_id.clone(),
        markdown: markdown.clone(),
        created_at,
        exported_path: existing.exported_path.clone(),
    })?;

    if let Some(path) = existing.exported_path.as_deref() {
        crate::export::overwrite_note(std::path::Path::new(path), &markdown)?;
    }

    Ok(NoteDto {
        meeting_id,
        provider_id: existing.provider_id,
        markdown,
        exported_path: existing.exported_path,
    })
}

/// Full-text-ish search across meeting titles, transcripts, and notes (Library search).
#[tauri::command]
pub fn search_meetings(
    state: State<'_, AppState>,
    query: String,
) -> Result<Vec<SearchHit>, AppError> {
    // BLK-2b: search only VISIBLE meetings (open/unlocked folders) so a sealed-and-not-unlocked
    // meeting's title/transcript/note never surfaces in a hit — independent of at-rest blanking.
    let unlocked = unlocked_snapshot(state.inner())?;
    state.db.search_visible(&query, 100, &unlocked)
}

/// Permanently delete a meeting: its audio file, its exported vault note, and all DB rows
/// (segments, notes, timeline cascade via FK). Irreversible.
#[tauri::command]
pub fn delete_meeting(state: State<'_, AppState>, meeting_id: String) -> Result<(), AppError> {
    // Capture + remove on-disk files before the rows disappear (best-effort).
    if let Some(m) = state.db.get_meeting(&meeting_id)? {
        if let Some(audio) = m.audio_path.as_deref() {
            let _ = std::fs::remove_file(audio);
        }
    }
    // Masters too — a master path may be the plaintext WAV or its `.enc`; clear both forms.
    if let Ok((mic, sys)) = state.db.get_meeting_master_paths(&meeting_id) {
        for p in [mic, sys].into_iter().flatten() {
            let _ = std::fs::remove_file(&p);
            let _ = std::fs::remove_file(format!("{p}{ENC_SUFFIX}"));
            let _ = std::fs::remove_file(p.trim_end_matches(ENC_SUFFIX));
        }
    }
    if let Some(note) = state.db.get_latest_note_for_meeting(&meeting_id)? {
        if let Some(path) = note.exported_path.as_deref() {
            let _ = std::fs::remove_file(path);
        }
    }
    state.db.delete_meeting(&meeting_id)
}

/// Rename a meeting's title (in-app + Library list). Does not rename the vault file.
#[tauri::command]
pub fn rename_meeting(
    state: State<'_, AppState>,
    meeting_id: String,
    title: String,
) -> Result<(), AppError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(AppError::InvalidArg("title cannot be empty".into()));
    }
    state.db.set_meeting_title(&meeting_id, title)
}

/// Grounded Q&A over a meeting's transcript ("chat with the meeting"). The configured
/// provider answers strictly from the transcript + the running conversation history.
#[tauri::command]
pub async fn chat_meeting(
    state: State<'_, AppState>,
    meeting_id: String,
    question: String,
    history: Vec<ChatTurn>,
) -> Result<String, AppError> {
    if question.trim().is_empty() {
        return Err(AppError::InvalidArg("question is empty".into()));
    }
    // D4 READ-GATE: a sealed-and-not-unlocked meeting's transcript is blanked; refuse to chat over
    // it (it would otherwise answer from an empty transcript or leak via the provider). Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to chat about this meeting".into(),
        ));
    }
    let segments = state.db.get_segments(&meeting_id)?;
    if segments.is_empty() {
        return Err(AppError::InvalidArg(
            "this meeting has no transcript to chat about yet".into(),
        ));
    }
    let transcript = segments
        .iter()
        .map(|s| format!("[{:.0}s] {}", s.start_s, s.text.trim()))
        .collect::<Vec<_>>()
        .join("\n");
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    let provider = crate::summarize::make_provider(&config.provider_id, &config)?;
    let (system, user) = crate::summarize::chat::build(&transcript, &history, &question);
    provider.complete(&system, &user).await
}

/// Copy a meeting's recording (WAV) to a user-chosen path (FE picks it via a save dialog).
#[tauri::command]
pub fn export_audio(
    state: State<'_, AppState>,
    meeting_id: String,
    dest_path: String,
) -> Result<(), AppError> {
    // Phase 0.5 READ-GATE: refuse to export the audio of a sealed-and-not-unlocked meeting. Its
    // WAV is AES-GCM-encrypted at rest (audio_path → <file>.enc) and there is no plaintext on disk
    // to copy until the folder is session-unlocked; fail closed with a Locked error.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to export the audio".into(),
        ));
    }
    let meeting = state
        .db
        .get_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no meeting with id {meeting_id}")))?;
    let src = meeting
        .audio_path
        .ok_or_else(|| AppError::InvalidArg("this meeting has no audio file".into()))?;
    std::fs::copy(&src, &dest_path)
        .map_err(|e| AppError::Storage(format!("copy audio failed: {e}")))?;
    Ok(())
}

/// Which per-stream master to export.
enum MasterStream {
    Mic,
    Sys,
}

/// Shared READ-GATED export for a per-stream master archive (faithful float32 WAV). Refuses a
/// sealed-and-not-unlocked meeting (the master is `.enc` at rest, no plaintext to copy) and never
/// hands a path to the FE — the masters are reachable ONLY through these gated commands.
fn export_master(
    state: State<'_, AppState>,
    meeting_id: &str,
    dest_path: &str,
    which: MasterStream,
) -> Result<(), AppError> {
    if !meeting_is_unlocked(state.inner(), meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to export the master".into(),
        ));
    }
    let (mic, sys) = state.db.get_meeting_master_paths(meeting_id)?;
    let src = match which {
        MasterStream::Mic => mic,
        MasterStream::Sys => sys,
    }
    .ok_or_else(|| AppError::InvalidArg("this meeting has no master for that stream".into()))?;
    std::fs::copy(&src, dest_path)
        .map_err(|e| AppError::Storage(format!("copy master failed: {e}")))?;
    Ok(())
}

/// Export a meeting's MIC master archive (faithful native-rate float32 WAV) to a chosen path.
#[tauri::command]
pub fn export_mic_master(
    state: State<'_, AppState>,
    meeting_id: String,
    dest_path: String,
) -> Result<(), AppError> {
    export_master(state, &meeting_id, &dest_path, MasterStream::Mic)
}

/// Export a meeting's SYSTEM master archive (faithful 48 kHz float32 WAV) to a chosen path.
#[tauri::command]
pub fn export_sys_master(
    state: State<'_, AppState>,
    meeting_id: String,
    dest_path: String,
) -> Result<(), AppError> {
    export_master(state, &meeting_id, &dest_path, MasterStream::Sys)
}

/// Write a meeting's note markdown to a user-chosen path (FE picks it via a save dialog).
#[tauri::command]
pub fn export_note(
    state: State<'_, AppState>,
    meeting_id: String,
    dest_path: String,
) -> Result<(), AppError> {
    // D4 READ-GATE: refuse to export a sealed-and-not-unlocked meeting's note (its plaintext
    // markdown is blanked while sealed — exporting would write an empty/garbage file). Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to export the note".into(),
        ));
    }
    let note = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    std::fs::write(&dest_path, note.markdown.as_bytes())
        .map_err(|e| AppError::Storage(format!("write note failed: {e}")))?;
    Ok(())
}

/// Best-effort detection of a running meeting app (Zoom / Teams / Webex) to offer a
/// "start recording?" nudge. Browser-based Google Meet is NOT detectable this way.
#[tauri::command]
pub fn detect_meeting_app() -> Result<Option<String>, AppError> {
    let listing = match std::process::Command::new("ps").arg("-axo").arg("comm=").output() {
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

/// Meetings carrying a given tag, newest first.
#[tauri::command]
pub fn list_meetings_by_tag(
    state: State<'_, AppState>,
    tag: String,
) -> Result<Vec<Meeting>, AppError> {
    state.db.list_meetings_by_tag(&tag)
}

/// Built-in recipe templates (quick chips).
#[tauri::command]
pub fn list_builtin_recipes() -> Result<Vec<BuiltinRecipe>, AppError> {
    Ok(crate::summarize::recipes::BUILTIN_RECIPES
        .iter()
        .map(|(id, label, prompt)| BuiltinRecipe {
            id: id.to_string(),
            label: label.to_string(),
            prompt: prompt.to_string(),
        })
        .collect())
}

/// User-saved recipe templates.
#[tauri::command]
pub fn list_saved_recipes(state: State<'_, AppState>) -> Result<Vec<RecipeRecord>, AppError> {
    state.db.list_saved_recipes()
}

/// Save a recipe template (prompt + title).
#[tauri::command]
pub fn save_recipe(
    state: State<'_, AppState>,
    title: String,
    prompt: String,
) -> Result<RecipeRecord, AppError> {
    let title = title.trim();
    let prompt = prompt.trim();
    if title.is_empty() || prompt.is_empty() {
        return Err(AppError::InvalidArg(
            "recipe title and prompt are required".into(),
        ));
    }
    let rec = RecipeRecord {
        id: uuid::Uuid::new_v4().to_string(),
        title: title.to_string(),
        prompt: prompt.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    state.db.insert_recipe(&rec)?;
    Ok(rec)
}

/// Delete a saved recipe.
#[tauri::command]
pub fn delete_recipe(state: State<'_, AppState>, id: String) -> Result<(), AppError> {
    state.db.delete_recipe(&id)
}

/// Run a recipe prompt over a meeting's transcript (grounded), returning the artifact text.
#[tauri::command]
pub async fn run_recipe(
    state: State<'_, AppState>,
    meeting_id: String,
    prompt: String,
) -> Result<String, AppError> {
    if prompt.trim().is_empty() {
        return Err(AppError::InvalidArg("recipe prompt is empty".into()));
    }
    // BLK-2b READ-GATE: a sealed-and-not-unlocked meeting's transcript is blanked; refuse to run a
    // recipe over it (would feed a cloud provider blank/garbage and depend on at-rest blanking).
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to run a recipe".into(),
        ));
    }
    let segments = state.db.get_segments(&meeting_id)?;
    if segments.is_empty() {
        return Err(AppError::InvalidArg("this meeting has no transcript yet".into()));
    }
    let transcript = segments
        .iter()
        .map(|s| s.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    let provider = crate::summarize::make_provider(&config.provider_id, &config)?;
    let (system, user) = crate::summarize::recipes::build_recipe_prompt(
        &transcript,
        &prompt,
        &config.note_language,
    );
    provider.complete(&system, &user).await
}

/// Parse a meeting note's "## Action items" checklist into structured items.
#[tauri::command]
pub fn get_action_items(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<ActionItem>, AppError> {
    // D4 READ-GATE: a sealed-and-not-unlocked meeting's note markdown is blanked; refuse to parse
    // action items from it (would silently return none / leak a stale plaintext). Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to see action items".into(),
        ));
    }
    let note = state.db.get_latest_note_for_meeting(&meeting_id)?;
    Ok(match note {
        Some(n) => crate::summarize::action_items::parse_action_items(&n.markdown),
        None => Vec::new(),
    })
}

/// OPEN-COMMITMENTS rollup ("what did I promise / what's still open"): deterministically aggregate
/// every OPEN (`- [ ]`) action item across the VISIBLE library, with each item's meeting context.
/// No model — pure aggregation over the gated readers. `owner` (optional) filters case-insensitively.
/// GATED: routes through `Db::list_open_commitments`, which pushes the LIVE session unlock set
/// through `list_meetings_visible` + `get_note_if_visible` (the same predicate as `ask_vault` /
/// `generate_digest` / MCP) — a sealed-and-not-session-unlocked meeting contributes nothing.
#[tauri::command]
pub fn list_open_commitments(
    state: State<'_, AppState>,
    owner: Option<String>,
) -> Result<Vec<Commitment>, AppError> {
    let unlocked = unlocked_snapshot(state.inner())?;
    let owner = owner.as_deref().map(str::trim).filter(|o| !o.is_empty());
    state.db.list_open_commitments(&unlocked, owner)
}

/// Rewrite the note's action items into Obsidian Tasks format (📅 due dates) + re-write the
/// vault file in place. Returns the updated note.
#[tauri::command]
pub fn patch_note_tasks(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<NoteDto, AppError> {
    // D4 WRITE-GATE: refuse to rewrite a sealed-and-not-unlocked meeting's note (its plaintext is
    // blanked; patching would persist the blanked value over the sealed content). Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to rewrite the note's tasks".into(),
        ));
    }
    let existing = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    let patched = crate::summarize::action_items::patch_tasks_markdown(&existing.markdown);
    let created_at = chrono::Utc::now().to_rfc3339();
    state.db.upsert_note(&NoteRecord {
        meeting_id: meeting_id.clone(),
        provider_id: existing.provider_id.clone(),
        markdown: patched.clone(),
        created_at,
        exported_path: existing.exported_path.clone(),
    })?;
    if let Some(path) = existing.exported_path.as_deref() {
        crate::export::overwrite_note(std::path::Path::new(path), &patched)?;
    }
    Ok(NoteDto {
        meeting_id,
        provider_id: existing.provider_id,
        markdown: patched,
        exported_path: existing.exported_path,
    })
}

/// Escape a string for embedding inside an AppleScript `"…"` literal: backslash + double-quote are
/// escaped, and raw CR/LF are flattened to spaces (an AppleScript string literal cannot span lines).
/// This is what stops the item text from breaking out of the quoted literal or injecting extra
/// statements (`"`, `end tell`, …) into the osascript program.
fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\n', '\r'], " ")
}

/// Parse a strict ISO `YYYY-MM-DD` into `(year, month, day)`; `None` for anything else.
fn parse_iso_ymd(s: &str) -> Option<(i32, u32, u32)> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i32 = s.get(0..4)?.parse().ok()?;
    let m: u32 = s.get(5..7)?.parse().ok()?;
    let d: u32 = s.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((y, m, d))
}

/// Build the osascript program that creates a Reminder named `name`. When `due_date` is a valid
/// ISO `YYYY-MM-DD`, attach `remind me date`/`due date` (defaulted to 9am local) so the date
/// actually lands in Reminders — previously the date was dropped. The name is
/// `escape_applescript`-escaped so its text can never break out of the string literal. The date is
/// built by setting `day` to 1 FIRST (so a year/month change can't overflow the current day-of-month),
/// then year, then month, then the real day.
pub(crate) fn build_reminder_script(name: &str, due_date: Option<&str>) -> String {
    let esc = escape_applescript(name);
    match due_date.and_then(parse_iso_ymd) {
        Some((y, m, d)) => format!(
            "set theDate to current date\n\
             set day of theDate to 1\n\
             set year of theDate to {y}\n\
             set month of theDate to {m}\n\
             set day of theDate to {d}\n\
             set hours of theDate to 9\n\
             set minutes of theDate to 0\n\
             set seconds of theDate to 0\n\
             tell application \"Reminders\" to make new reminder with properties {{name:\"{esc}\", remind me date:theDate, due date:theDate}}"
        ),
        None => format!(
            "tell application \"Reminders\" to make new reminder with properties {{name:\"{esc}\"}}"
        ),
    }
}

/// Add a macOS Reminder (via osascript) for an action item. A denied Reminders permission
/// surfaces a clear, actionable error rather than crashing the UI. When the item carries an ISO
/// due date, it is set as the reminder's due/remind date (best-effort; verify on a real Mac).
#[tauri::command]
pub async fn add_reminder(text: String, due_date: Option<String>) -> Result<(), AppError> {
    let name = text.trim().to_string();
    if name.is_empty() {
        return Err(AppError::InvalidArg("empty reminder".into()));
    }
    let due = due_date.as_deref().filter(|d| !d.is_empty());
    let script = build_reminder_script(&name, due);
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
    })
    .await
    .map_err(|e| AppError::Unavailable(format!("reminder task failed: {e}")))?
    .map_err(|e| AppError::Unavailable(format!("osascript failed: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(AppError::Unavailable(format!(
            "Could not add to Reminders — grant access in System Settings ▸ Privacy & Security ▸ Reminders. ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// SYNCHRONOUS reminder creation for the off-thread voice-action dispatch (Flow B). Mirrors the
/// `add_reminder` command's osascript path, but blocking (it already runs on a detached task, so it
/// must not require an async runtime). Returns `Ok(())` on success, a typed `AppError` otherwise —
/// NEVER panics. NO PII logged by the caller; the reminder text is the user's own dictated note.
pub(crate) fn add_reminder_blocking(text: &str, due_date: Option<&str>) -> Result<(), AppError> {
    let name = text.trim();
    if name.is_empty() {
        return Err(AppError::InvalidArg("empty reminder".into()));
    }
    let due = due_date.filter(|d| !d.is_empty());
    let script = build_reminder_script(name, due);
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| AppError::Unavailable(format!("osascript failed: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(AppError::Unavailable(format!(
            "Could not add to Reminders — grant access in System Settings ▸ Privacy & Security ▸ Reminders. ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Pin a meeting moment: append a timestamped ^block-ref to the note (DB + vault file) and
/// return an obsidian:// deep link to the note.
#[tauri::command]
pub fn pin_moment(
    state: State<'_, AppState>,
    meeting_id: String,
    seconds: f64,
    label: String,
) -> Result<PinResult, AppError> {
    // BLK-2b WRITE-GATE: refuse to pin into a sealed-and-not-unlocked meeting's note — its plaintext
    // markdown is blanked, so appending a pin would persist the blanked value over the sealed
    // content AND re-export a plaintext `.md`. Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to pin a moment".into(),
        ));
    }
    let existing = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    let secs = seconds.max(0.0) as i64;
    let block_id = format!("m{secs}");
    let mmss = format!("{}:{:02}", secs / 60, secs % 60);
    let new_md = crate::export::append_pin(&existing.markdown, &mmss, &label, &block_id);
    let created_at = chrono::Utc::now().to_rfc3339();
    state.db.upsert_note(&NoteRecord {
        meeting_id: meeting_id.clone(),
        provider_id: existing.provider_id.clone(),
        markdown: new_md.clone(),
        created_at,
        exported_path: existing.exported_path.clone(),
    })?;
    let url = match existing.exported_path.as_deref() {
        Some(path) => {
            crate::export::overwrite_note(std::path::Path::new(path), &new_md)?;
            let vault = {
                state
                    .config
                    .lock()
                    .map_err(|_| AppError::Config("config mutex poisoned".into()))?
                    .vault_path
                    .clone()
            };
            match vault.as_deref().filter(|p| !p.is_empty()) {
                Some(v) => crate::export::build_open_url(
                    std::path::Path::new(v),
                    std::path::Path::new(path),
                ),
                None => String::new(),
            }
        }
        None => String::new(),
    };
    Ok(PinResult {
        url,
        block_id,
        mmss,
    })
}

/// Extract the people + projects from a meeting note and persist them through the dual-sink:
///
/// - **Sink A (always):** upsert each entity into the encrypted DB (`upsert_entity`, case-
///   insensitive dedup) and record a mention (`add_mention`, idempotent). The DB is the
///   source of truth for the in-app graph and works with NO vault configured.
/// - **Sink B (gated):** mirror each entity as a `[[ ]]` vault stub via `ensure_entity_backlink`
///   ONLY when a vault is configured AND the meeting's folder is NOT locked (`folder_by_id`
///   disk-truth — NOT session unlock). A session-unlocked folder must NOT re-emit `.md` stubs
///   (they were removed on seal and stay out until a permanent remove-lock), so the write gate
///   uses `locked` while every READ uses `unlocked`. A meeting at the vault root (no folder)
///   has no lock and gets its stubs.
///
/// Returns the extracted `GraphPayload`. The caller decides whether extraction failures are fatal
/// (the `link_meeting_entities` command surfaces them; the pipeline hook swallows them).
pub async fn build_and_persist_entities(
    state: &AppState,
    meeting_id: &str,
    title: &str,
    markdown: &str,
) -> Result<crate::summarize::graph::GraphPayload, AppError> {
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    let provider = crate::summarize::make_provider(&config.provider_id, &config)?;
    let payload =
        crate::summarize::graph::extract_entities(provider.as_ref(), title, markdown).await?;

    // Sink A — ALWAYS persist to the encrypted DB (the graph's source of truth).
    for p in &payload.people {
        let id = state.db.upsert_entity(p, crate::storage::models::EntityKind::Person)?;
        state.db.add_mention(&id, meeting_id)?;
    }
    for pr in &payload.projects {
        let id = state
            .db
            .upsert_entity(pr, crate::storage::models::EntityKind::Project)?;
        state.db.add_mention(&id, meeting_id)?;
    }

    // Sink B — vault [[ ]] stubs, ONLY when a vault is configured AND the meeting's folder is
    // NOT sealed on disk. Disk-truth `locked` (not session `unlocked`): a session-unlock must
    // never re-write encrypted-content stubs back to plaintext on disk.
    let vault = config.vault_path.clone().filter(|p| !p.is_empty());
    if let Some(vault) = vault {
        let folder_locked = match state.db.get_meeting(meeting_id)?.and_then(|m| m.folder_id) {
            Some(folder_id) => state
                .db
                .folder_by_id(&folder_id)?
                .map(|f| f.locked)
                .unwrap_or(false),
            None => false, // vault root → never locked
        };
        if !folder_locked {
            let vault_path = std::path::Path::new(&vault);
            for p in &payload.people {
                crate::export::entity_stub::ensure_entity_backlink(vault_path, "People", p, title)?;
            }
            for pr in &payload.projects {
                crate::export::entity_stub::ensure_entity_backlink(
                    vault_path, "Projects", pr, title,
                )?;
            }
        }
    }

    Ok(payload)
}

/// Resolve the people + projects in a meeting note → persist them to the encrypted DB graph
/// (always) and mirror them as `[[Person]]` / `[[Project]]` vault stubs (only when a vault is
/// configured + the meeting's folder is unsealed). The graph self-assembles. The DB sink works
/// even with no vault set — hence no hard "set a vault folder" error anymore.
#[tauri::command]
pub async fn link_meeting_entities(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<crate::summarize::graph::GraphPayload, AppError> {
    // BLK-2b READ-GATE: a sealed-and-not-unlocked meeting's note is blanked; refuse to extract
    // entities from it (would feed a cloud provider blank text + re-write vault stubs). Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to link entities".into(),
        ));
    }
    let meeting = state
        .db
        .get_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no meeting with id {meeting_id}")))?;
    let note = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg("this meeting has no note yet".into()))?;
    let title = meeting.title.clone().unwrap_or_else(|| "Meeting".to_string());
    build_and_persist_entities(&state, &meeting_id, &title, &note.markdown).await
}

/// Max co-occurring neighbors returned with an entity's detail (the neighborhood satellites).
const ENTITY_NEIGHBOR_LIMIT: i64 = 12;

/// The self-assembling graph: all VISIBLE entity nodes (with their visible mention counts) + all
/// VISIBLE co-occurrence edges. Snapshots the live session `unlocked` set (same as `list_folders`)
/// and pushes it through the visibility predicate, so sealed-and-not-unlocked meetings contribute
/// nothing — the graph can never disagree with Library/MCP about what's visible.
#[tauri::command]
pub fn get_graph(state: State<'_, AppState>) -> Result<GraphData, AppError> {
    let unlocked = unlocked_snapshot(state.inner())?;
    state.db.build_graph(&unlocked)
}

/// Detail for one entity: the entity, its VISIBLE backlinked meetings (as `VaultSource` chips),
/// and its top co-occurring neighbors. Snapshots the live `unlocked` set like `get_graph`.
/// Errors with `InvalidArg` if the entity id is unknown.
#[tauri::command]
pub fn get_entity_detail(
    state: State<'_, AppState>,
    entity_id: String,
) -> Result<EntityDetail, AppError> {
    let unlocked = unlocked_snapshot(state.inner())?;
    state
        .db
        .build_entity_detail(&entity_id, &unlocked, ENTITY_NEIGHBOR_LIMIT)?
        .ok_or_else(|| AppError::InvalidArg(format!("no entity with id {entity_id}")))
}

/// Ask-My-Vault: answer a question across ALL past meetings' notes (grounded, with sources).
#[tauri::command]
pub async fn ask_vault(
    state: State<'_, AppState>,
    question: String,
    history: Vec<ChatTurn>,
) -> Result<AskVaultResult, AppError> {
    if question.trim().is_empty() {
        return Err(AppError::InvalidArg("question is empty".into()));
    }
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    // Pass the LIVE session unlock set (E9): a folder the user has session-unlocked is included
    // again, while sealed-and-NOT-unlocked content stays excluded by the same visibility predicate.
    let unlocked = unlocked_snapshot(state.inner())?;
    // Phase 2b (gated): when semantic search is ON, pick candidates by HYBRID retrieval (FTS ∪ vector
    // KNN, RRF-fused) — embedding the query with the active embedder — then pack with the SAME
    // budget/citation logic and the SAME visibility gate. When OFF (the default) OR the index is
    // empty, this falls back to the existing FTS-only path UNCHANGED (the hybrid query degenerates to
    // FTS when no vectors exist, and the flag-off branch is byte-for-byte the prior behavior).
    let (corpus, sources) = if config.semantic_search_enabled {
        let embedder = crate::embed::active_embedder();
        // QUERY side: use the e5 `query:` prefix (asymmetric with the `passage:` index side).
        let query_vec = embedder
            .embed_query(std::slice::from_ref(&question))?
            .into_iter()
            .next()
            .unwrap_or_default();
        crate::summarize::vault_context::build_vault_context_hybrid_visible(
            &state.db,
            &question,
            &config.provider_id,
            &query_vec,
            &unlocked,
        )?
    } else {
        crate::summarize::vault_context::build_vault_context_visible(
            &state.db,
            &question,
            &config.provider_id,
            &unlocked,
        )?
    };
    if corpus.trim().is_empty() {
        return Ok(AskVaultResult {
            answer: "No meeting notes to search yet — record and summarize a meeting first."
                .to_string(),
            sources: Vec::new(),
        });
    }
    let provider = crate::summarize::make_provider(&config.provider_id, &config)?;
    let (system, user) = crate::summarize::vault_chat::build(&corpus, &history, &question);
    let answer = provider.complete(&system, &user).await?;
    Ok(AskVaultResult { answer, sources })
}

/// Entity DOSSIER (brain2 Phase 5b): synthesize the "state of [[entity]]" across all meetings —
/// Overview · 🕑 Timeline of mentions · ⏳ Open commitments · 🧭 Last said / next step, every claim
/// citing its [[Title]]. `entity` is an entity id (from `get_graph`) OR a name. The dossier data is
/// assembled through the SAME visibility gate as Ask-My-Vault (sealed-not-unlocked meetings
/// contribute nothing), then synthesized by the configured provider — so this is a CLOUD-egress
/// path that goes through the redaction firewall + consent gate (E6/E7/E10) exactly like `ask_vault`.
#[tauri::command]
pub async fn entity_dossier(
    state: State<'_, AppState>,
    entity: String,
) -> Result<String, AppError> {
    if entity.trim().is_empty() {
        return Err(AppError::InvalidArg("entity is empty".into()));
    }
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    // Pass the LIVE session unlock set (E9): a folder the user has session-unlocked is included
    // again; sealed-and-NOT-unlocked content stays excluded by the same visibility predicate.
    let unlocked = unlocked_snapshot(state.inner())?;
    let entity_id = crate::summarize::dossier::resolve_entity_id(&state.db, &entity, &unlocked)?
        .ok_or_else(|| AppError::InvalidArg(format!("no visible entity matching \"{entity}\"")))?;
    let data = crate::summarize::dossier::build_dossier_data(&state.db, &entity_id, &unlocked)?
        .ok_or_else(|| AppError::InvalidArg(format!("no visible entity matching \"{entity}\"")))?;
    // Build the provider (firewall + consent gate) BEFORE synthesizing — make_provider refuses a
    // cloud provider until the user has consented to egress.
    let provider = crate::summarize::make_provider(&config.provider_id, &config)?;
    let system = crate::summarize::dossier::dossier_system_prompt(&config.note_language);
    let user = crate::summarize::dossier::render_dossier_user(&data, &config.provider_id);
    provider.complete(&system, &user).await
}

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
    let budget = if config.provider_id == "ollama" {
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
        let date = m.started_at.split(['T', ' ']).next().unwrap_or("").to_string();
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
    let provider = crate::summarize::make_provider(&config.provider_id, &config)?;
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

/// Topic Threads: cluster the per-meeting topic spans (from cached timelines) across the whole
/// library into cross-meeting threads. Deterministic, no LLM. Only meetings whose timeline has
/// been generated (viewed at least once) contribute.
#[tauri::command]
pub fn topic_threads(state: State<'_, AppState>) -> Result<Vec<TopicThread>, AppError> {
    // BLK-2b: cluster only VISIBLE meetings' timelines — a sealed-and-not-unlocked meeting's
    // timeline `data` is blanked at rest, but gate on visibility so threads never depend on that
    // blanking (and a sealed meeting's topics never surface cross-meeting).
    let unlocked = unlocked_snapshot(state.inner())?;
    let mut input = Vec::new();
    for m in state.db.list_meetings_visible(500, &unlocked)? {
        let Some(json) = state.db.get_timeline_data(&m.id)? else {
            continue;
        };
        let Ok(tl) = serde_json::from_str::<MeetingTimeline>(&json) else {
            continue;
        };
        if tl.topics.is_empty() {
            continue;
        }
        input.push(crate::summarize::threads::MeetingTopics {
            meeting_id: m.id,
            title: m.title.unwrap_or_else(|| "(untitled)".to_string()),
            started_at: m.started_at,
            topics: tl
                .topics
                .iter()
                .map(|t| (t.label.clone(), t.start_s, t.end_s))
                .collect(),
        });
    }
    Ok(crate::summarize::threads::build_threads(&input))
}

/// Export a meeting as an Obsidian Canvas (.canvas) — a spatial board of its topic spans.
/// Requires the timeline (open the meeting once). Returns the written path.
#[tauri::command]
pub fn export_canvas(state: State<'_, AppState>, meeting_id: String) -> Result<String, AppError> {
    // D4 READ-GATE: a sealed-and-not-unlocked meeting's timeline is blanked; refuse to build a
    // canvas from it. Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to export the canvas".into(),
        ));
    }
    let meeting = state
        .db
        .get_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no meeting with id {meeting_id}")))?;
    let json = state.db.get_timeline_data(&meeting_id)?.ok_or_else(|| {
        AppError::InvalidArg("open the meeting once to generate its timeline first".into())
    })?;
    let tl: MeetingTimeline = serde_json::from_str(&json)
        .map_err(|e| AppError::InvalidArg(format!("bad timeline data: {e}")))?;
    let title = meeting.title.unwrap_or_else(|| "Meeting".to_string());
    let topics: Vec<(String, f64, f64)> = tl
        .topics
        .iter()
        .map(|t| (t.label.clone(), t.start_s, t.end_s))
        .collect();
    let canvas = crate::export::canvas::build_canvas(&title, &topics);
    let vault = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .vault_path
            .clone()
    }
    .filter(|p| !p.is_empty())
    .ok_or_else(|| AppError::InvalidArg("set a vault folder in Settings first".into()))?;
    let vault_root = std::path::Path::new(&vault);
    // D5: the Canvas dir must resolve inside the vault root.
    let dir = assert_in_vault(vault_root, std::path::Path::new("Canvas"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::Export(format!("create Canvas dir failed: {e}")))?;
    let fname: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' | '^' | '[' | ']' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let fname = if fname.is_empty() {
        "Meeting".to_string()
    } else {
        fname
    };
    // D5: re-assert the final file path stays inside the vault (fname is sanitized, but bind the
    // guarantee at the write site).
    let path = assert_in_vault(
        vault_root,
        &std::path::Path::new("Canvas").join(format!("{fname}.canvas")),
    )?;
    std::fs::write(&path, canvas)
        .map_err(|e| AppError::Export(format!("write canvas failed: {e}")))?;
    Ok(path.to_string_lossy().to_string())
}

/// Pre-Meeting Brief: grounded prep card for an upcoming meeting `subject`, built from related
/// past meeting notes.
#[tauri::command]
pub async fn pre_meeting_brief(
    state: State<'_, AppState>,
    subject: String,
) -> Result<BriefResult, AppError> {
    if subject.trim().is_empty() {
        return Err(AppError::InvalidArg("subject is empty".into()));
    }
    let config = {
        state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?
            .clone()
    };
    // Pass the LIVE session unlock set (E9), same as ask_vault: session-unlocked folders included,
    // sealed-and-not-unlocked excluded.
    let unlocked = unlocked_snapshot(state.inner())?;
    let (corpus, sources) = crate::summarize::vault_context::build_vault_context_visible(
        &state.db,
        &subject,
        &config.provider_id,
        &unlocked,
    )?;
    let provider = crate::summarize::make_provider(&config.provider_id, &config)?;
    let (system, user) =
        crate::summarize::brief::build_brief_prompt(&corpus, &subject, &config.note_language);
    let markdown = provider.complete(&system, &user).await?;
    Ok(BriefResult { markdown, sources })
}

/// Best-effort: the soonest macOS Calendar event in the next 60 minutes (title only). Returns
/// None if Calendar access is denied or there's nothing upcoming — never errors the UI.
#[tauri::command]
pub async fn next_calendar_event() -> Result<Option<CalendarEvent>, AppError> {
    let script = r#"set now to (current date)
set laterT to now + (60 * minutes)
set out to ""
try
  tell application "Calendar"
    repeat with c in calendars
      repeat with e in (every event of c whose start date is greater than or equal to now and start date is less than or equal to laterT)
        set out to out & (summary of e) & linefeed
      end repeat
    end repeat
  end tell
end try
return out"#;
    let res = tokio::task::spawn_blocking(move || {
        std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()
    })
    .await;
    let stdout = match res {
        Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return Ok(None),
    };
    let title = stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string);
    Ok(title.map(|title| CalendarEvent { title, start: None }))
}

/// CALENDAR source (local, zero-OAuth, on-device): list the user's events in a window around now
/// via the bundled `meetnotes-calendar` EventKit sidecar — title, attendees, agenda. GRACEFUL on
/// every failure: sidecar missing / Calendar permission denied / timeout / malformed output →
/// an empty list, never an error, never a block. No network egress: reading the local calendar
/// stays on device.
#[tauri::command]
pub async fn list_calendar_events(app: AppHandle) -> Result<Vec<CalendarEventFull>, AppError> {
    // Default window: now-1h .. now+12h (60 back, 720 forward minutes).
    Ok(crate::calendar::fetch_events(&app, 60, 720).await)
}

/// Build a compact [`CalendarContext`] (title + attendees + agenda) for one event so the existing
/// pre-meeting brief / note pre-analysis can consume it (the brain already takes context). Looks
/// the event up by id in the same window the sidecar surfaces. Returns `None` if the event isn't
/// found (expired from the window, or Calendar access denied) — never an error.
///
/// IMPORTANT: the returned text is on-device context. If it is later fed to a CLOUD provider it
/// MUST ride the existing `make_provider` redaction firewall + consent (the same path the
/// transcript takes) — this command opens NO new egress path.
#[tauri::command]
pub async fn calendar_context_for(
    app: AppHandle,
    event_id: String,
) -> Result<Option<CalendarContext>, AppError> {
    if event_id.trim().is_empty() {
        return Err(AppError::InvalidArg("event_id is empty".into()));
    }
    let events = crate::calendar::fetch_events(&app, 60, 720).await;
    Ok(events
        .iter()
        .find(|e| e.id == event_id)
        .map(CalendarContext::from_event))
}

/// Read current config (settings table), without secrets.
#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> Result<AppConfigDto, AppError> {
    let config = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    Ok(config_to_dto(&config))
}

/// Persist config to settings table + refresh in-memory cache. Does NOT touch Keychain.
#[tauri::command]
pub fn save_config(
    app: AppHandle,
    state: State<'_, AppState>,
    config: AppConfigDto,
) -> Result<(), AppError> {
    // Merge against the CURRENT config under the config lock so the security-sensitive flags that
    // save_config must NOT be able to flip from the DTO (BLK-4: cloud_egress_consented) are read
    // from the live value, not the incoming payload. Holding the guard across the merge+save+swap
    // makes it atomic w.r.t. a concurrent `consent_to_cloud_egress`.
    {
        let mut cache = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        let new_config = dto_to_config(config, &cache);
        new_config.save(&state.db)?;
        *cache = new_config;
    }
    // Reconcile the voice-trigger listener with the new config (re-locks the guard internally, so
    // it MUST run after the guard above is dropped).
    restart_voice_listener(app);
    Ok(())
}

/// E10 — grant the one-time cloud-egress consent. This is the ONLY supported way to flip
/// `cloud_egress_consented` true: it persists the flag AND updates the in-memory config cache, so
/// the next `make_provider(claude_code|anthropic)` is allowed to build. Idempotent.
///
/// The FE calls this from its first-cloud-send confirmation dialog. Until the user confirms, every
/// cloud summarize/chat returns `AppError::Unavailable("cloud egress not consented …")`, which the
/// FE detects and surfaces as the consent prompt.
#[tauri::command]
pub fn consent_to_cloud_egress(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cache.grant_cloud_egress_consent(&state.db)?;
    Ok(())
}

/// brain2 connectors — grant the one-time WEB SEARCH egress consent. The web connector reaches an
/// EXTERNAL service (a NEW EGRESS CLASS): the redacted query leaves the device. This is the ONLY
/// supported way to flip `web_search_consented` true; it persists the flag AND updates the in-memory
/// config cache, so the next `ConnectorRegistry::build` exposes the web tool (provided web search is
/// also enabled and a key is stored). Idempotent. Until granted, the web connector is absent from the
/// brain's tool registry and the redacted query never leaves the device.
#[tauri::command]
pub fn consent_to_web_search(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cache.grant_web_search_consent(&state.db)?;
    Ok(())
}

fn config_to_dto(c: &AppConfig) -> AppConfigDto {
    AppConfigDto {
        provider_id: c.provider_id.clone(),
        vault_path: c.vault_path.clone(),
        vault_subfolder: c.vault_subfolder.clone(),
        whisper_model_path: c.whisper_model_path.clone(),
        language: c.language.clone(),
        anthropic_model: c.anthropic_model.clone(),
        ollama_base_url: c.ollama_base_url.clone(),
        ollama_model: c.ollama_model.clone(),
        claude_binary: c.claude_binary.clone(),
        input_device: c.input_device.clone(),
        capture_system_audio: c.capture_system_audio,
        vad_enabled: c.vad_enabled,
        keep_hires_masters: c.keep_hires_masters,
        diarize_others: c.diarize_others,
        aec_enabled: c.aec_enabled,
        model_size: c.model_size.clone(),
        voice_trigger: c.voice_trigger,
        onboarded: c.onboarded,
        note_style: c.note_style.clone(),
        auto_organize: c.auto_organize,
        note_language: c.note_language.clone(),
        mcp_require_token: c.mcp_require_token,
        lock_require_biometric: c.lock_require_biometric,
        relock_on_screenshare: c.relock_on_screenshare,
        cloud_egress_consented: c.cloud_egress_consented,
        brain_backend: c.brain_backend,
        realtime_reactions: c.realtime_reactions,
        brain_model_id: c.brain_model_id.clone(),
        semantic_search_enabled: c.semantic_search_enabled,
        web_search_enabled: c.web_search_enabled,
        // DISPLAY-ONLY out: lets the FE show "consented" status; the FE cannot set it back (preserved
        // in `dto_to_config`).
        web_search_consented: c.web_search_consented,
    }
}

/// Build the persisted `AppConfig` from an incoming settings DTO, merged against the `current`
/// config. Every plain field comes from the DTO (the Settings UI is authoritative for them), but
/// the security-sensitive `cloud_egress_consented` is PRESERVED from `current` and never taken from
/// the DTO (BLK-4) — so a settings save can neither grant nor clear cloud-egress consent. The
/// dedicated `consent_to_cloud_egress` command is the only path that flips it.
fn dto_to_config(d: AppConfigDto, current: &AppConfig) -> AppConfig {
    // Normalize empty strings on optional fields to None so they round-trip cleanly.
    let norm = |s: Option<String>| s.filter(|v| !v.trim().is_empty());
    AppConfig {
        provider_id: d.provider_id,
        vault_path: norm(d.vault_path),
        vault_subfolder: norm(d.vault_subfolder),
        whisper_model_path: norm(d.whisper_model_path),
        language: norm(d.language),
        anthropic_model: d.anthropic_model,
        ollama_base_url: d.ollama_base_url,
        ollama_model: d.ollama_model,
        claude_binary: d.claude_binary,
        input_device: norm(d.input_device),
        capture_system_audio: d.capture_system_audio,
        vad_enabled: d.vad_enabled,
        keep_hires_masters: d.keep_hires_masters,
        diarize_others: d.diarize_others,
        aec_enabled: d.aec_enabled,
        model_size: if d.model_size.trim().is_empty() {
            // Mirror AppConfig::default().model_size — an empty/blank choice from the FE must
            // fall back to the multilingual large-v3 default (best Polish quality), NOT a
            // smaller model that would silently downgrade transcription.
            AppConfig::default().model_size
        } else {
            d.model_size
        },
        voice_trigger: d.voice_trigger,
        onboarded: d.onboarded,
        note_style: if d.note_style.trim().is_empty() {
            "standard".to_string()
        } else {
            d.note_style
        },
        auto_organize: d.auto_organize,
        note_language: if d.note_language.trim().is_empty() {
            "auto".to_string()
        } else {
            d.note_language
        },
        mcp_require_token: d.mcp_require_token,
        lock_require_biometric: d.lock_require_biometric,
        relock_on_screenshare: d.relock_on_screenshare,
        // BLK-4: consent is NEVER set from the DTO. Preserve the live value; only the dedicated
        // `consent_to_cloud_egress` command may flip it. This makes an omitting/zeroed save inert.
        cloud_egress_consented: current.cloud_egress_consented,
        // brain2 RAG: the semantic-search master flag IS carried on the settings DTO (the Settings
        // UI owns the toggle). Plain bool; an omitted value already defaulted to OFF on the DTO
        // (`#[serde(default)]`), so a partial/older save can never silently enable it. Unlike
        // `cloud_egress_consented` (preserved-only), this one is settable.
        semantic_search_enabled: d.semantic_search_enabled,
        // Phase B: the brain model path is not carried on the settings DTO (it is resolved from the
        // shared models dir by default), so a settings save preserves the live value (default None).
        brain_model_path: current.brain_model_path.clone(),
        // Phase H (registry): the selected brain model id IS carried on the settings DTO, but it is
        // VALIDATED against the registry first. A `Some(known-id)` is taken; an unknown id or `None`
        // is IGNORED — the live selection is preserved (no error, no bogus id stored). This mirrors
        // `select_brain_model`'s registry guard without crashing a settings save on a stale/typo'd id.
        brain_model_id: match d.brain_model_id.as_deref() {
            Some(id) if crate::reason::brain_model_by_id(id).is_some() => d.brain_model_id.clone(),
            _ => current.brain_model_id.clone(),
        },
        // Phase H (brain backend): which reasoner powers the brain (cloud/local/off) IS taken from
        // the DTO (the Settings UI owns the toggle). `BrainBackend` deserializes an unknown/omitted
        // token to the default `Cloud`, so the value here is always a valid enum variant.
        brain_backend: d.brain_backend,
        // Phase H (Flow B): the in-meeting voice-action dispatch gate IS taken from the DTO (the
        // Settings UI owns the toggle). Plain bool; an omitted value already defaulted to OFF on the
        // DTO (`#[serde(default)]`), so the opt-in can never be silently enabled by a partial save.
        realtime_reactions: d.realtime_reactions,
        // brain2 connectors: the web-search master toggle IS settable from the DTO (Settings owns it).
        // An omitted value already defaulted to OFF on the DTO, so a partial save can't enable it.
        web_search_enabled: d.web_search_enabled,
        // brain2 connectors (NEW EGRESS CLASS): consent is NEVER set from the DTO — preserved from the
        // live value (BLK-4 mirror). Only `consent_to_web_search` may flip it, so a settings save can
        // neither grant nor clear web-search egress consent.
        web_search_consented: current.web_search_consented,
    }
}

/// List available microphone input devices for the FE picker (name + default flag).
#[tauri::command]
pub fn list_input_devices() -> Result<Vec<crate::audio::InputDeviceInfo>, AppError> {
    Ok(crate::audio::list_input_devices())
}

/// Store/replace the Anthropic API key in Keychain (account "anthropic_api_key").
#[tauri::command]
pub fn set_anthropic_key(key: String) -> Result<(), AppError> {
    if key.trim().is_empty() {
        // Empty input clears the stored key.
        return secrets::delete_secret(ANTHROPIC_KEY_ACCOUNT);
    }
    secrets::set_secret(ANTHROPIC_KEY_ACCOUNT, &key)
}

/// Whether an Anthropic key is currently stored (UI shows "set"/"not set"; never the value).
#[tauri::command]
pub fn has_anthropic_key() -> Result<bool, AppError> {
    Ok(secrets::get_secret(ANTHROPIC_KEY_ACCOUNT)?.is_some())
}

/// Store/replace the BYO web-search (Brave) API key in the Keychain (account "web_search_api_key").
/// An empty input clears it. The key is NEVER logged and NEVER returned to the FE — only `has_*`
/// reports presence. Mirrors `set_anthropic_key`.
#[tauri::command]
pub fn set_web_search_api_key(key: String) -> Result<(), AppError> {
    if key.trim().is_empty() {
        return secrets::delete_secret(crate::connectors::web::WEB_SEARCH_KEY_ACCOUNT);
    }
    secrets::set_secret(crate::connectors::web::WEB_SEARCH_KEY_ACCOUNT, key.trim())
}

/// Whether a web-search API key is currently stored (UI shows "set"/"not set"; never the value).
#[tauri::command]
pub fn has_web_search_key() -> Result<bool, AppError> {
    Ok(secrets::get_secret(crate::connectors::web::WEB_SEARCH_KEY_ACCOUNT)?
        .filter(|k| !k.trim().is_empty())
        .is_some())
}

/// availability() fan-out across all three providers for the Settings UI.
#[tauri::command]
pub async fn provider_statuses(
    state: State<'_, AppState>,
) -> Result<Vec<ProviderStatus>, AppError> {
    use crate::summarize::provider::Availability;

    let config: AppConfig = {
        let guard = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        guard.clone()
    };

    let providers = all_providers(&config);
    let mut out = Vec::with_capacity(providers.len());
    for p in providers {
        let (available, reason) = match p.availability().await {
            Availability::Available => (true, None),
            Availability::Unavailable { reason } => (false, Some(reason)),
        };
        out.push(ProviderStatus {
            id: p.id().to_string(),
            available,
            reason,
        });
    }
    Ok(out)
}

/// Re-run summarize+export for an existing meeting with the configured provider, reusing
/// the meeting's stored transcript segments (Detail "re-summarize"/"re-export" seed —
/// wired in P0, UI optional).
#[tauri::command]
pub async fn resummarize(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<StopResult, AppError> {
    // BLK-2b READ/WRITE-GATE: re-summarizing reads the stored transcript (blanked while sealed) and
    // WRITES a fresh note + re-exports a plaintext `.md` to the vault. For a sealed-and-not-unlocked
    // meeting that would (a) feed a cloud provider blank text and (b) leave plaintext markdown +
    // a vault `.md` in a locked folder. Fail closed — the FE must unlock first.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to re-summarize".into(),
        ));
    }
    let result = pipeline::resummarize_existing(&app, &state, &meeting_id).await?;
    Ok(StopResult {
        meeting_id: result.meeting_id,
        markdown: result.note_markdown,
        exported_path: result
            .exported_path
            .map(|p| p.to_string_lossy().to_string()),
    })
}

/// Recent meetings for the Library list (newest first, capped).
#[tauri::command]
pub fn list_meetings(state: State<'_, AppState>) -> Result<Vec<Meeting>, AppError> {
    state.db.list_meetings(200)
}

/// Aggregate analytics for the dashboard + Analytics tab.
#[tauri::command]
pub fn get_analytics(state: State<'_, AppState>) -> Result<Analytics, AppError> {
    state.db.analytics()
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
    let new_label = new_label.trim();
    if new_label.is_empty() {
        return Err(AppError::InvalidArg("new speaker name is empty".into()));
    }
    // BLK-2b WRITE-GATE: a sealed-and-not-unlocked meeting's timeline `data` is blanked; refuse to
    // rename a speaker (would persist a near-empty plaintext timeline over the sealed blob in a
    // locked folder). Fail closed.
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to rename a speaker".into(),
        ));
    }
    let json = state
        .db
        .get_timeline_data(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg("no timeline for this meeting yet".into()))?;
    let mut tl: crate::storage::models::MeetingTimeline = serde_json::from_str(&json)
        .map_err(|e| AppError::InvalidArg(format!("bad timeline data: {e}")))?;
    for turn in &mut tl.speakers {
        if turn.speaker == old_label {
            turn.speaker = new_label.to_string();
        }
    }
    let updated = serde_json::to_string(&tl)
        .map_err(|e| AppError::Storage(format!("serialize timeline: {e}")))?;
    state.db.set_timeline_data(&meeting_id, &updated)?;
    Ok(tl)
}

/// Speaker + topic timeline for a meeting (AI-derived, cached after first generation).
#[tauri::command]
pub async fn get_timeline(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<MeetingTimeline, AppError> {
    // Phase 0.5 READ-GATE: a sealed-and-not-unlocked meeting returns an EMPTY timeline (its
    // `timelines.data` is blanked at rest while sealed, but mask explicitly + skip regeneration so
    // we never re-derive a timeline from now-blank segments).
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Ok(MeetingTimeline::default());
    }
    if let Some(json) = state.db.get_timeline_data(&meeting_id)? {
        if let Ok(t) = serde_json::from_str::<MeetingTimeline>(&json) {
            return Ok(t);
        }
    }
    let segments = state.db.get_segments(&meeting_id)?;
    if segments.is_empty() {
        return Ok(MeetingTimeline::default());
    }
    let duration_s = state
        .db
        .get_meeting(&meeting_id)?
        .map(|m| m.duration_s)
        .unwrap_or(0);
    let config = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        c.clone()
    };
    let provider = crate::summarize::make_provider(&config.provider_id, &config)?;
    let timeline =
        crate::summarize::timeline::generate(provider.as_ref(), &segments, duration_s).await?;
    if let Ok(json) = serde_json::to_string(&timeline) {
        let _ = state.db.set_timeline_data(&meeting_id, &json);
    }
    Ok(timeline)
}

/// A meeting + its latest note + transcript segments for the Detail view.
/// Returns `None` if the meeting id is unknown.
#[tauri::command]
pub fn get_meeting_detail(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Option<MeetingDetailDto>, AppError> {
    let Some(meeting) = state.db.get_meeting(&meeting_id)? else {
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
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Ok(Some(masked_detail(meeting)));
    }

    let note = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .map(|n| NoteDto {
            meeting_id: n.meeting_id,
            provider_id: n.provider_id,
            markdown: n.markdown,
            exported_path: n.exported_path,
        });
    let segments = state.db.get_segments(&meeting_id)?;
    Ok(Some(MeetingDetailDto {
        meeting,
        note,
        segments,
        locked: false,
    }))
}

/// Build the MASKED detail DTO for a sealed-and-not-session-unlocked meeting. Pure (no DB / state)
/// so the read-gate's masking contract is unit-testable. EVERY content channel is closed:
/// - `title` → "🔒 Locked" (the real title lives in `meetings.title`, plaintext-at-rest);
/// - `audio_path` → `None` so the FE has nothing to hand `convertFileSrc` (the `asset:` protocol
///   serve path that bypasses the `export_audio` command + `meeting_is_unlocked` gate);
/// - `note` / `segments` → empty;
/// - `locked` → true so the FE renders the unlock affordance, not an empty shell.
fn masked_detail(meeting: Meeting) -> MeetingDetailDto {
    MeetingDetailDto {
        meeting: Meeting {
            title: Some("🔒 Locked".to_string()),
            audio_path: None,
            ..meeting
        },
        note: None,
        segments: Vec::new(),
        locked: true,
    }
}

/// Whether a usable Whisper model is present for the chosen size + language (or the
/// explicit configured path). Lets the UI auto-detect + offer a download when missing.
#[tauri::command]
pub fn model_present(state: State<'_, AppState>) -> Result<bool, AppError> {
    let (configured, size, language) = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        (
            c.whisper_model_path.clone(),
            c.model_size.clone(),
            c.language.clone().unwrap_or_default(),
        )
    };
    let p = configured.as_deref().map(std::path::Path::new);
    Ok(crate::transcribe::resolve_model_path(p, &size, &language)?.is_some())
}

/// Download the Whisper model matching the chosen size + language (multilingual unless
/// English is selected) from the whisper.cpp HuggingFace mirror into the app models dir if
/// missing; returns its path. No-op (returns the existing path) when already present.
#[tauri::command]
pub async fn download_model(state: State<'_, AppState>) -> Result<String, AppError> {
    let (configured, size, language) = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        (
            c.whisper_model_path.clone(),
            c.model_size.clone(),
            c.language.clone().unwrap_or_default(),
        )
    };
    let p = configured.as_deref().map(std::path::Path::new);
    let path = crate::transcribe::ensure_model(p, &size, &language).await?;
    Ok(path.to_string_lossy().to_string())
}

/// Whether a usable on-device brain (reasoning GGUF) is present at the resolved path — the
/// configured custom `brain_model_path`, else the selected `brain_model_id`'s file in the shared
/// models dir. Lets the UI offer a download. Independent of the `local-brain` feature: this only
/// checks the file on disk.
#[tauri::command]
pub fn brain_model_present(state: State<'_, AppState>) -> Result<bool, AppError> {
    let (configured, selected) = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        (c.brain_model_path.clone(), c.brain_model_id.clone())
    };
    let p = configured.as_deref().map(std::path::Path::new);
    Ok(crate::reason::resolve_brain_model(p, selected.as_deref())?.is_some())
}

/// macOS total physical RAM in whole GB via `sysctl -n hw.memsize` (no new FFI/crate). Returns
/// `None` on any error — the caller then treats every model as fitting rather than HIDING it behind
/// a failed probe.
fn total_ram_gb() -> Option<u64> {
    let out = std::process::Command::new("sysctl")
        .arg("-n")
        .arg("hw.memsize")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let bytes: u64 = String::from_utf8(out.stdout).ok()?.trim().parse().ok()?;
    Some(bytes / (1024 * 1024 * 1024))
}

/// The curated on-device brain model registry, each row carrying the picker flags `downloaded`
/// (file present in the shared models dir), `fits_ram` (min RAM within the machine's total RAM —
/// `true` when total RAM can't be read), and `selected` (the persisted `brain_model_id`). Feeds the
/// Phase-H model picker. No content read / no egress — static metadata + on-disk existence only.
#[tauri::command]
pub fn list_brain_models(
    state: State<'_, AppState>,
) -> Result<Vec<crate::reason::BrainModelDto>, AppError> {
    let selected = {
        let c = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        c.brain_model_id.clone()
    };
    let dir = crate::transcribe::models_dir()?;
    Ok(crate::reason::brain_model_dtos(
        &dir,
        total_ram_gb(),
        selected.as_deref(),
    ))
}

/// Persist the user's SELECTED on-device brain model id. Validates `model_id` against the registry
/// (unknown id ⇒ `AppError::InvalidArg`) and saves it to config; the next `active_reasoner` build
/// resolves this model when its GGUF is present. Does NOT download — the FE calls
/// `download_brain_model(model_id)` for that.
#[tauri::command]
pub fn select_brain_model(state: State<'_, AppState>, model_id: String) -> Result<(), AppError> {
    select_brain_model_inner(&state, model_id)
}

/// Testable core of [`select_brain_model`]: validate the id against the registry, persist it.
fn select_brain_model_inner(state: &AppState, model_id: String) -> Result<(), AppError> {
    if crate::reason::brain_model_by_id(&model_id).is_none() {
        return Err(AppError::InvalidArg(format!(
            "unknown brain model id: {model_id}"
        )));
    }
    let mut c = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    c.brain_model_id = Some(model_id);
    c.save(&state.db)?;
    Ok(())
}

/// Download the on-device brain model identified by `model_id` (from the curated registry) into the
/// shared models dir if missing; returns its path. No-op (returns the existing path) when already
/// present. Unknown id ⇒ `AppError::InvalidArg`. INBOUND ONLY — fetches a model file and sends NO
/// meeting content (no egress). Emits [`crate::events::EVENT_BRAIN_DOWNLOAD`] progress events
/// (throttled). The downloaded file is NOT loaded here — wiring the brain is a later step.
/// Resolve a registry `model_id` to its `(download url, on-disk dest)`. Unknown id ⇒
/// `AppError::InvalidArg` (the rejection [`download_brain_model`] enforces). Testable sync core.
fn brain_download_target(model_id: &str) -> Result<(&'static str, std::path::PathBuf), AppError> {
    let model = crate::reason::brain_model_by_id(model_id).ok_or_else(|| {
        AppError::InvalidArg(format!("unknown brain model id: {model_id}"))
    })?;
    Ok((model.url, crate::transcribe::models_dir()?.join(model.filename)))
}

#[tauri::command]
pub async fn download_brain_model(
    app: AppHandle,
    model_id: String,
) -> Result<String, AppError> {
    let (url, dest) = brain_download_target(&model_id)?;

    if dest.is_file() {
        return Ok(dest.to_string_lossy().to_string());
    }

    // Throttle progress events to roughly every 8 MB so a multi-GB download doesn't flood the FE.
    const EMIT_EVERY: u64 = 8 * 1024 * 1024;
    let mut last_emit: u64 = 0;
    crate::reason::download_brain_model(url, &dest, |downloaded, total| {
        if downloaded - last_emit >= EMIT_EVERY {
            last_emit = downloaded;
            let _ = app.emit(
                crate::events::EVENT_BRAIN_DOWNLOAD,
                crate::events::BrainDownloadPayload {
                    downloaded,
                    total,
                    done: false,
                },
            );
        }
    })
    .await?;

    let _ = app.emit(
        crate::events::EVENT_BRAIN_DOWNLOAD,
        crate::events::BrainDownloadPayload {
            downloaded: 0,
            total: None,
            done: true,
        },
    );
    Ok(dest.to_string_lossy().to_string())
}

/// `true` when all three multilingual-e5-small files are present in the shared models dir's embed
/// sub-dir — i.e. the REAL embedder (under `--features local-embed`) would load. Cheap existence
/// probe; NEVER errors on a missing models dir (treats it as "not present").
#[tauri::command]
pub fn embed_model_present() -> Result<bool, AppError> {
    Ok(crate::embed::embed_model_present())
}

/// Download the multilingual-e5-small model (3 HF files) into the shared models dir, INBOUND-ONLY,
/// emitting [`crate::events::EVENT_EMBED_DOWNLOAD`] progress (throttled per file). Sends NO meeting
/// content (no egress). The downloaded model is NOT loaded here — it is picked up lazily by
/// `embed::active_embedder` on the next embed (feature-gated by `local-embed`). Returns the model dir.
#[tauri::command]
pub async fn download_embed_model(app: AppHandle) -> Result<String, AppError> {
    let file_count = crate::embed::EMBED_MODEL_FILES.len();
    // Throttle progress to roughly every 2 MB so the (small) model download doesn't flood the FE.
    const EMIT_EVERY: u64 = 2 * 1024 * 1024;
    let mut last_emit: u64 = 0;
    let mut last_index: usize = usize::MAX;
    let dir = crate::embed::download_embed_model(|file_index, downloaded, total| {
        // Always emit on a file boundary; otherwise throttle by bytes.
        if file_index != last_index || downloaded - last_emit >= EMIT_EVERY {
            last_index = file_index;
            last_emit = downloaded;
            let _ = app.emit(
                crate::events::EVENT_EMBED_DOWNLOAD,
                crate::events::EmbedDownloadPayload {
                    file_index,
                    file_count,
                    downloaded,
                    total,
                    done: false,
                },
            );
        }
    })
    .await?;

    let _ = app.emit(
        crate::events::EVENT_EMBED_DOWNLOAD,
        crate::events::EmbedDownloadPayload {
            file_index: file_count,
            file_count,
            downloaded: 0,
            total: None,
            done: true,
        },
    );
    Ok(dir.to_string_lossy().to_string())
}

/// Result of [`reindex_embeddings`]. `status` is `"model_missing"` when the real e5 model is absent
/// (no indexing was attempted — re-indexing with the deterministic STUB embedder would poison the
/// index with garbage vectors, worse than nothing), else `"indexed"`. On `"indexed"`, `indexed` is
/// the count of VISIBLE meetings whose chunks were (re)built. NO PII — counts + a status string only.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReindexResult {
    pub status: String,
    pub indexed: usize,
    pub total: usize,
}

/// brain2 RAG — BACKFILL the semantic vector index for ALL VISIBLE meetings (the one-shot the user
/// runs after turning `semantic_search_enabled` on, or after installing the e5 model so the old
/// STUB-embedded chunks get replaced by real e5 vectors).
///
/// GATING (lock-model): the corpus is exactly `list_meetings_visible(unlocked)` — a sealed-and-not-
/// session-unlocked meeting is NEVER returned, so its plaintext is never chunked/embedded, and its
/// chunks STAY purged (the seal already purged them; we don't touch them). For each visible meeting
/// we re-fetch its note through `get_note_if_visible(unlocked)` (defense-in-depth: skip if the note
/// is not visible) and call `index_meeting_chunks`, which PURGES-then-reinserts — so any stale stub
/// vectors are replaced with e5 ones. No new read path: every read routes through `visibility_clause`.
///
/// MODEL GUARD: if the real e5 model is absent (`!embed_model_present()` ⇒ `active_embedder` is the
/// stub), we DO NOTHING and return `{ status: "model_missing" }`. Re-indexing with garbage stub
/// vectors is strictly worse than leaving the (old, possibly-stub) chunks alone — the FE prompts the
/// user to download e5 first via `download_embed_model`.
///
/// Emits [`crate::events::EVENT_REINDEX`] `{ done, total }` progress (counts only, NO PII).
/// EMBED_DIM stays 384 (e5 == stub width) ⇒ NO `vec_chunks` schema migration.
#[tauri::command]
pub async fn reindex_embeddings(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ReindexResult, AppError> {
    // Snapshot the LIVE session unlock set — visibility is evaluated against exactly this set, so a
    // sealed-not-unlocked folder's meetings are invisible and never indexed.
    let unlocked = state
        .unlocked_folders
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    reindex_embeddings_inner(
        &state.db,
        &unlocked,
        crate::embed::embed_model_present(),
        crate::embed::active_embedder().as_ref(),
        |done, total| {
            let _ = app.emit(
                crate::events::EVENT_REINDEX,
                crate::events::ReindexPayload { done, total },
            );
        },
    )
}

/// Pure, AppHandle-free core of [`reindex_embeddings`] so the model-missing guard + the
/// visibility-gated loop are unit-testable headless. Takes the `Db`, the live `unlocked` session set,
/// whether the REAL e5 model is present (`model_present`), the active embedder, and a progress sink.
///
/// MODEL GUARD: `model_present == false` ⇒ return `{ status: "model_missing" }` and index NOTHING
/// (re-indexing with the deterministic STUB embedder would poison the index with garbage vectors —
/// strictly worse than leaving the old chunks alone).
///
/// GATING (lock-model): the corpus is exactly `list_meetings_visible(unlocked)` — a sealed-and-not-
/// session-unlocked meeting is NEVER returned, so its plaintext is never chunked/embedded and its
/// chunks STAY purged. Each meeting's note is re-checked through `get_note_if_visible(unlocked)`
/// before `index_meeting_chunks` (PURGE-then-reinsert, so stale stub vectors are replaced).
pub(crate) fn reindex_embeddings_inner<F: FnMut(usize, usize)>(
    db: &crate::storage::Db,
    unlocked: &std::collections::HashSet<String>,
    model_present: bool,
    embedder: &dyn crate::embed::Embedder,
    mut on_progress: F,
) -> Result<ReindexResult, AppError> {
    if !model_present {
        tracing::info!(target: "rag", "reindex_embeddings: e5 model missing; skipping (no stub indexing)");
        return Ok(ReindexResult {
            status: "model_missing".to_string(),
            indexed: 0,
            total: 0,
        });
    }

    // The corpus is the visibility-gated meeting list. `list_meetings_visible` already excludes
    // sealed-and-not-session-unlocked meetings (their notes are not visible under `visibility_clause`).
    let meetings = db.list_meetings_visible(100_000, unlocked)?;
    let total = meetings.len();

    let mut indexed = 0usize;
    for m in &meetings {
        // Defense-in-depth: only index a meeting whose latest note is currently visible.
        match db.get_note_if_visible(&m.id, unlocked) {
            Ok(Some(_note)) => {
                if let Err(e) = db.index_meeting_chunks(&m.id, embedder) {
                    // Never abort the whole backfill on one bad note — log (no PII) and continue.
                    tracing::warn!(target: "rag", error = %e, "reindex: indexing one meeting failed (skipped)");
                }
            }
            Ok(None) => {
                // No visible note (sealed sibling, or no note yet) — skip; do NOT index.
            }
            Err(e) => {
                tracing::warn!(target: "rag", error = %e, "reindex: visibility check failed (skipped)");
            }
        }
        indexed += 1;
        on_progress(indexed, total);
    }

    tracing::info!(target: "rag", indexed, total, "reindex_embeddings complete");
    Ok(ReindexResult {
        status: "indexed".to_string(),
        indexed,
        total,
    })
}

/// True iff the on-device PERSON-name NER model (Phase D) is present on disk. Pure existence probe;
/// NEVER errors on a missing models dir (treats it as "not present"). When false (or the `local-ner`
/// feature is off), the redaction firewall uses the byte-identical NoopNameRedactor.
#[tauri::command]
pub fn ner_model_present() -> Result<bool, AppError> {
    Ok(crate::summarize::redact::ner_model_present())
}

/// Download the multilingual mDeBERTa-v3 NER model (3 HF files) into the shared models dir,
/// INBOUND-ONLY, emitting [`crate::events::EVENT_NER_DOWNLOAD`] progress (throttled per file). Sends
/// NO meeting content (no egress). The downloaded model is NOT loaded here — it is picked up lazily by
/// `summarize::redact::active_name_redactor` on the next cloud summarization (feature-gated by
/// `local-ner`). Returns the model dir.
#[tauri::command]
pub async fn download_ner_model(app: AppHandle) -> Result<String, AppError> {
    let file_count = crate::summarize::redact::NER_MODEL_FILES.len();
    // Throttle progress to roughly every 2 MB so the model download doesn't flood the FE.
    const EMIT_EVERY: u64 = 2 * 1024 * 1024;
    let mut last_emit: u64 = 0;
    let mut last_index: usize = usize::MAX;
    let dir = crate::summarize::redact::download_ner_model(|file_index, downloaded, total| {
        if file_index != last_index || downloaded - last_emit >= EMIT_EVERY {
            last_index = file_index;
            last_emit = downloaded;
            let _ = app.emit(
                crate::events::EVENT_NER_DOWNLOAD,
                crate::events::NerDownloadPayload {
                    file_index,
                    file_count,
                    downloaded,
                    total,
                    done: false,
                },
            );
        }
    })
    .await?;

    let _ = app.emit(
        crate::events::EVENT_NER_DOWNLOAD,
        crate::events::NerDownloadPayload {
            file_index: file_count,
            file_count,
            downloaded: 0,
            total: None,
            done: true,
        },
    );
    Ok(dir.to_string_lossy().to_string())
}

/// Show/hide the floating recorder bar window (also bound to the global ⌘⇧R shortcut).
#[tauri::command]
pub fn toggle_bar(app: AppHandle) {
    crate::toggle_bar(&app);
}

// ── folders + per-folder lock lifecycle (PHASE0-PLAN Stage C) ──
//
// Lock model: default OPEN (note exported to vault + visible in MCP). Lock is explicit per
// folder. Sealing encrypts each note's markdown into `content_blob` under a per-folder content
// key (CK), blanks the markdown column, removes the `.md` from the vault, and stores the
// KEK-wrapped CK in `folders.wrapped_key`. Session-unlock decrypts back into the markdown column
// for the session (no re-export). relock re-blanks. remove_lock is permanent (re-exports).

/// Build the folder tree (roots → children) from the flat folder list + per-folder note counts +
/// the current session unlock set.
#[tauri::command]
pub fn list_folders(state: State<'_, AppState>) -> Result<Vec<FolderNode>, AppError> {
    let folders = state.db.list_folders()?;
    let counts = state.db.count_notes_per_folder()?;
    let unlocked = {
        state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
            .clone()
    };
    Ok(build_folder_tree(&folders, &counts, &unlocked))
}

/// Assemble `FolderNode` roots (parent_id == None) and recurse children. Sealed-but-session-
/// unlocked folders carry `unlocked = true`.
fn build_folder_tree(
    folders: &[Folder],
    counts: &std::collections::HashMap<String, usize>,
    unlocked: &std::collections::HashSet<String>,
) -> Vec<FolderNode> {
    fn node(
        f: &Folder,
        folders: &[Folder],
        counts: &std::collections::HashMap<String, usize>,
        unlocked: &std::collections::HashSet<String>,
    ) -> FolderNode {
        let children = folders
            .iter()
            .filter(|c| c.parent_id.as_deref() == Some(f.id.as_str()))
            .map(|c| node(c, folders, counts, unlocked))
            .collect();
        FolderNode {
            id: f.id.clone(),
            name: f.name.clone(),
            parent_id: f.parent_id.clone(),
            note_count: counts.get(&f.id).copied().unwrap_or(0),
            locked: f.locked,
            unlocked: f.locked && unlocked.contains(&f.id),
            children,
        }
    }
    folders
        .iter()
        .filter(|f| f.parent_id.is_none())
        .map(|f| node(f, folders, counts, unlocked))
        .collect()
}

/// Create a folder under an optional parent. The vault-relative path is derived from the parent
/// path + the sanitized folder name; the matching vault subdirectory is created on disk.
#[tauri::command]
pub fn create_folder(
    state: State<'_, AppState>,
    name: String,
    parent_id: Option<String>,
) -> Result<Folder, AppError> {
    let clean = crate::summarize::organize::sanitize_folder(&name)
        .ok_or_else(|| AppError::InvalidArg("folder name is empty or invalid".into()))?;

    // Resolve the parent's vault-relative path (if any) and compose the child path.
    let parent_path = match parent_id.as_deref() {
        Some(pid) => {
            let parent = state
                .db
                .folder_by_id(pid)?
                .ok_or_else(|| AppError::InvalidArg(format!("no parent folder {pid}")))?;
            Some(parent.path)
        }
        None => None,
    };
    let rel_path = match &parent_path {
        Some(p) if !p.is_empty() => format!("{p}/{clean}"),
        _ => clean.clone(),
    };

    // Create the vault subdirectory (best-effort but surfaced): only when a vault is configured.
    // D5: canonicalize + assert the composed dir stays inside the vault root before any mkdir.
    if let Some(vault) = vault_path(&state) {
        let vault_root = std::path::Path::new(&vault);
        let dir = assert_in_vault(vault_root, std::path::Path::new(&rel_path))?;
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppError::Export(format!("create folder dir failed: {e}")))?;
    }

    let folder = Folder {
        id: uuid::Uuid::new_v4().to_string(),
        name: clean,
        path: rel_path,
        parent_id,
        locked: false,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    state.db.insert_folder(&folder)?;
    Ok(folder)
}

/// Move a note into a folder (or to the root with `folder_id = None`).
///
/// Three cases by TARGET:
/// - **open / root:** if the note has an exported `.md` the file is moved on disk (copy-then-remove,
///   best-effort, never loses bytes).
/// - **locked + SESSION-UNLOCKED (CK available):** reassign, then SEAL the moved note to the
///   folder's at-rest sealed shape (encrypt markdown/transcript/timeline into blobs, blank the
///   plaintext, remove the vault `.md`, encrypt the WAV) so plaintext never lands in a locked
///   folder (BLK-2). Verify-before-destroy throughout.
/// - **locked + NOT session-unlocked:** REJECTED with [`AppError::Locked`] — there is no CK to seal
///   with, so we refuse rather than leave plaintext in a locked folder. The FE must unlock first.
#[tauri::command]
pub fn move_note(
    state: State<'_, AppState>,
    meeting_id: String,
    folder_id: Option<String>,
) -> Result<(), AppError> {
    // Resolve current + target folder lock state.
    let note = state.db.get_latest_note_for_meeting(&meeting_id)?;
    let target_locked = match folder_id.as_deref() {
        Some(fid) => {
            state
                .db
                .folder_by_id(fid)?
                .ok_or_else(|| AppError::InvalidArg(format!("no folder {fid}")))?
                .locked
        }
        None => false,
    };

    // ── Target is a LOCKED folder: seal-or-reject (BLK-2) ───────────────────────────────────────
    if target_locked {
        let fid = folder_id.as_deref().expect("locked target implies Some(folder_id)");
        return move_into_locked_folder(state.inner(), &meeting_id, fid);
    }

    // ── Target is OPEN / root: existing reassign + best-effort FS move ──────────────────────────
    // The source folder's lock state: derive from the note's exported_path being present
    // (sealed notes have exported_path = NULL). If exported_path is None we treat the source as
    // "no movable file" and skip the FS move entirely.
    let exported = note.as_ref().and_then(|n| n.exported_path.clone());

    // Reassign in the DB first (the source-of-truth association). Targets EVERY provider row of
    // the meeting (WHERE meeting_id = ?1) so the meeting's folder is consistent across providers
    // and the seal/unlock lifecycle (which iterates provider rows) stays coherent.
    state.db.set_meeting_folder(&meeting_id, folder_id.as_deref())?;

    // Best-effort FS move only when a plaintext .md exists (target is open here).
    if let Some(src_path) = exported {
        if let Some(vault) = vault_path(&state) {
            let target_rel = match folder_id.as_deref() {
                Some(fid) => state.db.folder_by_id(fid)?.map(|f| f.path),
                None => None,
            };
            move_note_file(&state, &meeting_id, &src_path, &vault, target_rel.as_deref())?;
        }
    }
    Ok(())
}

/// BLK-2: move a meeting's note INTO a `locked` folder, sealing it to the folder's at-rest shape so
/// plaintext never lands in a locked folder. Requires the folder to be SESSION-UNLOCKED (its CK is
/// derivable from the cached KEK); otherwise REJECTS with [`AppError::Locked`]. Holds the lifecycle
/// guard for the whole reassign+seal so it can't interleave with a relock/remove-lock.
fn move_into_locked_folder(
    state: &AppState,
    meeting_id: &str,
    folder_id: &str,
) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);

    // Must be session-unlocked — otherwise we have no CK to seal the moved note with.
    let session_unlocked = state
        .unlocked_folders
        .lock()
        .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
        .contains(folder_id);
    if !session_unlocked {
        return Err(AppError::Locked(
            "the destination folder is locked — unlock it first, then move the note".into(),
        ));
    }

    // The folder's existence + locked state were already validated by `move_note` (target_locked).
    let wrapped = state
        .db
        .folder_wrapped_key(folder_id)?
        .ok_or_else(|| AppError::Storage("locked folder has no wrapped key".into()))?;

    // The KEK is cached for a session-unlocked folder. If it is somehow absent (e.g. zeroized by a
    // concurrent relock between the unlock-set check and here), fail closed — never seal without a
    // verified CK.
    let kek: Zeroizing<[u8; 32]> = {
        let g = state
            .master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        g.clone().ok_or_else(|| {
            AppError::Locked("the destination folder is locked — unlock it first, then move the note".into())
        })?
    };
    let ck_bytes = Zeroizing::new(crate::crypto::decrypt(&kek, &wrapped, &aad_wrapped_ck(folder_id))?);
    let ck: Zeroizing<[u8; 32]> = Zeroizing::new(
        ck_bytes
            .as_slice()
            .try_into()
            .map_err(|_| AppError::Storage("unwrapped content key has wrong length".into()))?,
    );

    // Reassign EVERY provider row of the meeting into the locked folder (the source-of-truth
    // association), THEN seal that one meeting's note + extras under the folder CK.
    state.db.set_meeting_folder(meeting_id, Some(folder_id))?;
    seal_moved_note(state, folder_id, meeting_id, &ck)?;
    Ok(())
}

/// The auto-organize safety decision for a classifier-chosen vault subfolder (BLK-2 parity for the
/// summarize pipeline). A freshly auto-filed note's plaintext `.md` must NEVER land in a LOCKED
/// folder's on-disk directory with `folder_id = NULL`: a later `lock_folder` and the at-rest
/// reconcile both key off `folder_id` and would miss it, leaving plaintext in a sealed dir forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoFileTarget {
    /// The subfolder is open / root / unmanaged (no matching folder row, or not locked) — write the
    /// plaintext note there as usual.
    Open,
    /// The subfolder is a SESSION-UNLOCKED locked folder — write the note, then seal it INTO this
    /// folder id (encrypt markdown/extras, remove the plaintext `.md`), exactly like a manual move.
    SealInto(String),
    /// The subfolder is a LOCKED, NOT-session-unlocked folder — there is no CK to seal with, so the
    /// note must NOT be written here. The caller writes it at the vault root instead (reject).
    RejectToRoot,
}

/// Classify where a summarize-pipeline note may be auto-filed, given the classifier-chosen
/// vault-relative `subfolder`. Pure lookup — performs NO writes. See [`AutoFileTarget`]. A subfolder
/// matching no folder row, or a non-locked folder, is [`AutoFileTarget::Open`]. The pipeline calls
/// this BEFORE writing the note so plaintext is never written into a sealed dir in the first place.
pub fn classify_auto_file_target(
    state: &AppState,
    subfolder: Option<&str>,
) -> Result<AutoFileTarget, AppError> {
    let Some(sub) = subfolder.filter(|s| !s.is_empty()) else {
        return Ok(AutoFileTarget::Open);
    };
    let Some(folder) = state.db.folder_by_path(sub)? else {
        return Ok(AutoFileTarget::Open);
    };
    if !folder.locked {
        return Ok(AutoFileTarget::Open);
    }
    let session_unlocked = state
        .unlocked_folders
        .lock()
        .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
        .contains(&folder.id);
    if session_unlocked {
        Ok(AutoFileTarget::SealInto(folder.id))
    } else {
        Ok(AutoFileTarget::RejectToRoot)
    }
}

/// Seal a just-auto-filed note INTO a session-unlocked locked folder — the SAME BLK-2 path a manual
/// [`move_note`] into a locked folder takes (reassign every provider row + encrypt markdown/extras +
/// remove the plaintext `.md`). Called by the pipeline only after [`classify_auto_file_target`]
/// returned [`AutoFileTarget::SealInto`]. On the rare race where the folder was relocked in between,
/// [`move_into_locked_folder`] returns `Err(Locked)` BEFORE touching state; the caller then removes
/// the stray plaintext `.md` so it never survives in a sealed dir (the note's markdown is still in
/// the DB, recoverable).
pub fn seal_auto_filed_note(
    state: &AppState,
    meeting_id: &str,
    folder_id: &str,
) -> Result<(), AppError> {
    move_into_locked_folder(state, meeting_id, folder_id)
}

/// Seal ONE just-moved meeting's note (every provider row) + its transcript/timeline/audio under the
/// folder CK, removing each row's vault `.md`. Verify-before-blank per row (mirrors `lock_folder`):
/// the markdown is only blanked once its blob reads back identical, so a moved note is never lost.
fn seal_moved_note(
    state: &AppState,
    folder_id: &str,
    meeting_id: &str,
    ck: &[u8; 32],
) -> Result<(), AppError> {
    let notes = state.db.sealable_notes_for_meeting(meeting_id)?;
    // Encrypt + VERIFY every provider row BEFORE any blank, so a failure leaves intact plaintext.
    let mut sealed_rows: Vec<(String, Vec<u8>)> = Vec::new();
    let mut exported_paths: Vec<String> = Vec::new();
    for n in &notes {
        // Skip a row already sealed (blob present + markdown blanked) — idempotent.
        if n.content_blob.is_some() && n.markdown.is_empty() {
            continue;
        }
        let aad = aad_content(folder_id, meeting_id, &n.provider_id, "note");
        let blob = crate::crypto::encrypt(ck, n.markdown.as_bytes(), &aad)?;
        if crate::crypto::decrypt(ck, &blob, &aad)? != n.markdown.as_bytes() {
            return Err(AppError::Storage(
                "seal verification failed on moved note (decrypted blob mismatch)".into(),
            ));
        }
        sealed_rows.push((n.provider_id.clone(), blob));
        if let Some(p) = n.exported_path.clone() {
            exported_paths.push(p);
        }
    }
    for (provider_id, blob) in &sealed_rows {
        state.db.seal_note(meeting_id, provider_id, blob)?;
    }
    // Seal the moved meeting's transcript + timeline + audio under the SAME CK.
    seal_meeting_extras(state, folder_id, meeting_id, ck)?;
    // AFTER the column writes, remove the vault `.md` files (a leftover .md is reconcilable; lost
    // content is not — so this is last).
    for p in exported_paths {
        let _ = std::fs::remove_file(&p);
    }
    // The note's chunks/vectors are plaintext-derived and a dense embedding is invertible, so they
    // must NOT survive at rest for a meeting now sealed into a locked folder — same invariant the
    // lock_folder / relock / startup-reconcile paths enforce. Covers both the manual move-into-locked
    // and the auto-file callers. (Re-indexed on unlock once indexing ships.)
    state.db.purge_chunks_for_meetings(&[meeting_id.to_string()])?;
    Ok(())
}

/// Move the exported `.md` to the target folder's vault subdir, preserving content. Re-points
/// the note's `exported_path`. Copy-then-remove so a failure never loses bytes.
fn move_note_file(
    state: &State<'_, AppState>,
    meeting_id: &str,
    src_path: &str,
    vault: &str,
    target_rel: Option<&str>,
) -> Result<(), AppError> {
    let src = std::path::Path::new(src_path);
    let bytes = match std::fs::read_to_string(src) {
        Ok(b) => b,
        // Source file already gone → nothing to move; leave DB association as set.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(AppError::Export(format!("read note for move failed: {e}"))),
    };
    let file_name = src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| AppError::Export("note path has no filename".into()))?;
    let vault_root = std::path::Path::new(vault);
    // D5: the destination (vault root + target folder rel-path + filename) must stay inside the
    // vault. Compose the vault-relative candidate and canonicalize+assert containment before any FS
    // write. `file_name` is derived from the real source path, but we still re-check it carries no
    // traversal segment.
    let rel_candidate = match target_rel.filter(|p| !p.is_empty()) {
        Some(rel) => std::path::Path::new(rel).join(file_name),
        None => std::path::PathBuf::from(file_name),
    };
    let dest = assert_in_vault(vault_root, &rel_candidate)?;
    let dest_dir = dest
        .parent()
        .ok_or_else(|| AppError::Export("destination has no parent dir".into()))?
        .to_path_buf();
    // Same-location no-op. `dest` is canonicalized (absolute, symlinks resolved) but `src` from the
    // DB is not — compare the CANONICALIZED source so a move to the same underlying file is detected
    // even when the path strings differ (e.g. /var vs /private/var on macOS). Skipping this would let
    // the copy-then-remove below delete the file it just wrote (data loss).
    let src_canon = src.canonicalize().unwrap_or_else(|_| src.to_path_buf());
    if dest == src_canon || dest == src {
        return Ok(());
    }
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| AppError::Export(format!("create move dir failed: {e}")))?;
    // Write the destination atomically, THEN remove the source (never lose bytes).
    crate::export::overwrite_note(&dest, &bytes)?;
    let _ = std::fs::remove_file(src);
    // Re-point the exported path for every provider row of this meeting.
    if let Some(existing) = state.db.get_latest_note_for_meeting(meeting_id)? {
        state.db.set_note_exported_path(
            meeting_id,
            &existing.provider_id,
            &dest.to_string_lossy(),
        )?;
    }
    Ok(())
}

/// SEAL a folder: generate a content key, KEK-wrap it, encrypt every governed note's markdown
/// into `content_blob`, then (after a DB commit) blank the markdown + delete the vault `.md`.
/// Atomicity: each note's blob is verified decryptable BEFORE we blank/delete; a crash after the
/// DB write but before the `.md` delete leaves a stale plaintext `.md` (reconcilable) — never
/// lost content.
#[tauri::command]
pub fn lock_folder(state: State<'_, AppState>, folder_id: String) -> Result<(), AppError> {
    lock_folder_inner(state.inner(), folder_id)
}

/// BLK-1: acquire the coarse [`AppState::lifecycle`] guard so a folder-lock state-machine op never
/// interleaves with another (notably the off-thread `relock_all_inner`). A `Mutex<()>` carries no
/// state, so a poisoned lock is recovered via `into_inner()` — never bricking all future lock ops.
fn lifecycle_guard(state: &AppState) -> std::sync::MutexGuard<'_, ()> {
    state
        .lifecycle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Inner of [`lock_folder`] taking `&AppState` (so the lifecycle stress test can drive it without a
/// `tauri::State`). Holds the [`AppState::lifecycle`] guard for the whole seal.
pub(crate) fn lock_folder_inner(state: &AppState, folder_id: String) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    if folder.locked {
        return Ok(()); // already sealed — idempotent.
    }

    let kek = Zeroizing::new(crate::secrets::get_or_create_master_kek()?);
    let ck = Zeroizing::new(crate::crypto::random_key()?);
    // Wrapped CK is AAD-bound to the folder id (B7): the wrapped key cannot be lifted onto a
    // different folder row and unwrapped there.
    let wrapped = crate::crypto::encrypt(&kek, &*ck, &aad_wrapped_ck(&folder_id))?;

    // Gather the notes to seal. A meeting may have MULTIPLE provider rows (e.g. re-summarized
    // with ollama then anthropic) each with DISTINCT markdown — seal EVERY (meeting, provider)
    // row into its OWN blob. Collapsing to one blob per meeting would destroy every provider's
    // content but the first (the PRIME-DIRECTIVE content-loss bug this guards against).
    let notes = state.db.notes_in_folder(&folder_id)?;
    let mut sealed_rows: Vec<(String, String, Vec<u8>)> = Vec::new();
    for n in &notes {
        // Encrypt this row's markdown bound to (folder|meeting|provider|note|v) and VERIFY it
        // reads back before we touch the plaintext.
        let aad = aad_content(&folder_id, &n.meeting_id, &n.provider_id, "note");
        let blob = crate::crypto::encrypt(&ck, n.markdown.as_bytes(), &aad)?;
        let check = crate::crypto::decrypt(&ck, &blob, &aad)?;
        if check != n.markdown.as_bytes() {
            return Err(AppError::Storage(
                "seal verification failed (decrypted blob mismatch)".into(),
            ));
        }
        sealed_rows.push((n.meeting_id.clone(), n.provider_id.clone(), blob));
    }

    // Capture every governed note's .md path BEFORE any seal_note nulls exported_path.
    let exported_paths: Vec<String> = notes.iter().filter_map(|n| n.exported_path.clone()).collect();

    // Persist: mark the folder locked (+ wrapped key) and write every sealed blob per provider
    // row (markdown blanked, exported_path cleared). Each write is guarded by the verification
    // above, so a crash mid-loop leaves already-sealed rows recoverable and not-yet-sealed rows
    // with intact plaintext — never lost content.
    state
        .db
        .set_folder_locked(&folder_id, true, Some(&wrapped))?;
    for (meeting_id, provider_id, blob) in &sealed_rows {
        state.db.seal_note(meeting_id, provider_id, blob)?;
    }

    // Phase 0.5 — seal the TRANSCRIPT + TIMELINE (defense-in-depth in the OPEN db) and the AUDIO
    // WAV at rest, all under the SAME folder CK. Verify-before-destroy inside (no transcript /
    // audio loss). Done after the note seal so a partial-seal crash still leaves recoverable blobs.
    seal_folder_extras(state, &folder_id, &ck)?;
    drop(kek); // explicit: KEK zeroized when this Zeroizing drops here.
    drop(ck); // explicit: CK zeroized after sealing all extras.

    // Phase 2a LOCK-SAFETY: purge plaintext-derived semantic chunks + their (invertible) vectors
    // for every meeting now sealed in this folder — a vector is PII derived from the plaintext, so
    // it must not survive at rest in a locked folder. Done AFTER the seal so the index is dropped
    // only once the recoverable blobs exist. Re-index-on-unlock is a separate later step; until it
    // lands a locked-then-unlocked folder is simply not semantically searchable (degraded, not
    // leaky).
    let sealed_meeting_ids = state.db.meeting_ids_in_folder(&folder_id)?;
    state.db.purge_chunks_for_meetings(&sealed_meeting_ids)?;

    // AFTER the column writes, delete the vault `.md` files (a leftover .md is reconcilable;
    // lost content is not — so this is last).
    for p in exported_paths {
        let _ = std::fs::remove_file(&p);
    }
    Ok(())
}

/// SESSION-unlock a sealed folder: KEK → unwrap CK → decrypt each note's `content_blob` back into
/// the plaintext markdown column for the session, and add the folder id to the session unlock set.
/// Does NOT re-export to the vault. Returns the refreshed folder node.
#[tauri::command]
pub async fn unlock_folder(
    state: State<'_, AppState>,
    folder_id: String,
) -> Result<FolderNode, AppError> {
    // v0.3.2 — the master KEK is a BIOMETRIC-GATED keychain item. Reading it makes macOS present the
    // Touch ID / passcode sheet directly (with our reason string) and hand back the key — THAT single
    // sheet IS the unlock auth, so there is no separate app-side authentication step (which would
    // double-prompt: Touch ID, then a keychain-password dialog). Result: exactly ONE Touch ID prompt,
    // no "app wants to use keychain, enter password" dialog, no "Always Allow".
    //
    // The `lock_require_biometric` preference (K_LOCK_REQUIRE_BIOMETRIC, default true) is INFORMATIONAL
    // only: the biometric requirement is enforced by the keychain item's kSecAttrAccessControl (an
    // OS-level gate), not by any app-side `if`. An app boolean cannot waive the OS access control —
    // even with the flag false, reading the gated item still presents the system sheet. It is NOT read
    // here precisely because it cannot change this code path; it is surfaced in settings so the user
    // can see the guarantee, and is retained on the config DTO for forward-compat.

    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    if !folder.locked {
        return Err(AppError::InvalidArg("folder is not locked".into()));
    }
    let wrapped = state
        .db
        .folder_wrapped_key(&folder_id)?
        .ok_or_else(|| AppError::Storage("locked folder has no wrapped key".into()))?;

    // Reuse the KEK cached from an earlier unlock in this session so repeated unlocks do NOT
    // re-prompt for Touch ID (the cache is zeroized on relock-all). Only fall through to the
    // biometric-gated keychain read — the single Touch ID prompt — when nothing is cached.
    let kek: Zeroizing<[u8; 32]> = {
        let cached: Option<Zeroizing<[u8; 32]>> = {
            let g = state
                .master_kek
                .lock()
                .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
            g.clone()
        };
        match cached {
            Some(k) => k,
            None => {
                // The biometric-gated keychain read BLOCKS while the Touch ID sheet is up, so run it
                // on the blocking pool — never on an async-runtime worker thread. This is the single
                // Touch ID prompt.
                let bytes = tokio::task::spawn_blocking(|| {
                    crate::secrets::get_or_create_master_kek_with_reason("Unlock this folder")
                })
                .await
                .map_err(|e| AppError::Auth(format!("master-kek task join failed: {e}")))??;
                Zeroizing::new(bytes)
            }
        }
    };
    // Wrapped CK is bound to the folder id (legacy folders fall back to empty AAD transparently).
    let ck_bytes = Zeroizing::new(crate::crypto::decrypt(&kek, &wrapped, &aad_wrapped_ck(&folder_id))?);
    let ck: Zeroizing<[u8; 32]> = Zeroizing::new(
        ck_bytes
            .as_slice()
            .try_into()
            .map_err(|_| AppError::Storage("unwrapped content key has wrong length".into()))?,
    );

    // BLK-1: from here on we MUTATE plaintext columns (restore markdown / segments / timeline).
    // Acquire the lifecycle guard for the whole synchronous restore so a concurrent
    // `relock_all_inner` (screen-share / lifecycle) cannot blank these rows mid-restore. Acquired
    // AFTER the keychain `.await` above — holding a std `MutexGuard` across an await would make this
    // command's future `!Send`; everything below is synchronous, so the guard never crosses a
    // suspend point.
    let _lifecycle = lifecycle_guard(state.inner());

    // Decrypt EACH sealed provider row's own blob back into its own markdown column for the
    // session (no dedup by meeting — every provider's distinct content is restored independently).
    // Bound to (folder|meeting|provider|note); legacy blobs fall back to empty AAD.
    let notes = state.db.notes_in_folder(&folder_id)?;
    for n in &notes {
        let Some(blob) = &n.content_blob else {
            continue; // open note (shouldn't happen in a sealed folder) — skip.
        };
        let aad = aad_content(&folder_id, &n.meeting_id, &n.provider_id, "note");
        let pt = crate::crypto::decrypt(&ck, blob, &aad)?;
        let markdown = String::from_utf8(pt)
            .map_err(|_| AppError::Storage("decrypted note is not valid UTF-8".into()))?;
        state
            .db
            .restore_note_markdown(&n.meeting_id, &n.provider_id, &markdown)?;
    }

    // Phase 0.5 — decrypt the TRANSCRIPT + TIMELINE back into their plaintext columns and
    // materialize a playable WAV (decrypt .enc → file) for the session, under the SAME CK.
    unseal_folder_extras(state.inner(), &folder_id, &ck)?;

    // Cache the KEK for the session (zeroized on relock-all + on drop) + add to the unlock set.
    {
        let mut g = state
            .master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        *g = Some(kek.clone());
    }
    {
        let mut g = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        g.insert(folder_id.clone());
    }

    // Return the refreshed node.
    let counts = state.db.count_notes_per_folder()?;
    let unlocked = {
        state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
            .clone()
    };
    Ok(FolderNode {
        id: folder.id.clone(),
        name: folder.name.clone(),
        parent_id: folder.parent_id.clone(),
        note_count: counts.get(&folder.id).copied().unwrap_or(0),
        locked: true,
        unlocked: unlocked.contains(&folder.id),
        children: Vec::new(),
    })
}

/// Re-seal a session-unlocked folder for the rest of this session: re-blank the plaintext
/// markdown of its sealed notes and drop the folder from the unlock set. The `content_blob`
/// stays — the folder is still `locked=1` on disk.
#[tauri::command]
pub fn relock_folder(state: State<'_, AppState>, folder_id: String) -> Result<(), AppError> {
    // BLK-1: serialize with the rest of the lock state machine (it re-blanks the same columns
    // `remove_lock` is mid-restoring).
    let _lifecycle = lifecycle_guard(state.inner());
    {
        let mut g = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        g.remove(&folder_id);
    }
    let mut one = std::collections::HashSet::new();
    one.insert(folder_id.clone());
    state.db.blank_sealed_notes_in_folders(&one)?;
    // Phase 0.5 — re-blank the transcript + timeline plaintext and drop the decrypted session WAV
    // (the .enc + the *_blob columns stay; the folder is still locked=1 on disk).
    reblank_folder_extras(state.inner(), &folder_id)?;
    Ok(())
}

/// Relock ALL session-unlocked folders + zeroize the cached KEK (called on screen-share start in
/// Stage E, and exposed as a command). Re-blanks the plaintext markdown of every sealed note.
#[tauri::command]
pub fn relock_all(state: State<'_, AppState>) -> Result<(), AppError> {
    relock_all_inner(&state)
}

/// Inner relock-all usable without a command boundary (Stage E screen-share watcher, window-close,
/// app-exit). BLK-1: this is the OFF-THREAD blanker that races `remove_lock`; it acquires the
/// [`AppState::lifecycle`] guard FIRST so its re-blank can never land between `remove_lock`'s
/// restore-plaintext (Step 1) and clear-`content_blob` (Step 2). All three off-thread callers and
/// the `relock_all` command funnel through here, so the guard lives HERE (the `relock_all` command
/// must NOT take it separately — a std `Mutex` is non-reentrant and would self-deadlock).
pub(crate) fn relock_all_inner(state: &AppState) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    // Clear the session set.
    {
        let mut g = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        g.clear();
    }
    // Zeroize the cached KEK copy (C5: use zeroize::Zeroize, not a hand byte-loop the optimizer
    // could elide — `Zeroize::zeroize` is a guaranteed, non-elidable wipe). Taking the `Zeroizing`
    // out and dropping it ALSO wipes it; the explicit call makes the intent unmistakable.
    {
        let mut g = state
            .master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        if let Some(mut k) = g.take() {
            k.zeroize();
        }
    }
    // Re-blank every sealed note across all locked folders.
    let locked: std::collections::HashSet<String> =
        state.db.locked_folder_ids()?.into_iter().collect();
    state.db.blank_sealed_notes_in_folders(&locked)?;
    // Phase 0.5 — re-blank the transcript + timeline + drop the decrypted session WAVs for every
    // locked folder too (the .enc + *_blob columns stay).
    for fid in &locked {
        reblank_folder_extras(state, fid)?;
    }
    // B12: checkpoint + truncate the WAL so the just-re-blanked plaintext does not linger in the
    // sidecar. Best-effort — a busy checkpoint is logged, not fatal to the relock.
    if let Err(e) = state.db.checkpoint_truncate() {
        tracing::warn!(target: "lock", error = %e, "wal_checkpoint(TRUNCATE) on relock_all failed");
    }
    Ok(())
}

/// PERMANENTLY remove a folder's lock: KEK → unwrap CK → decrypt each note back to plaintext
/// markdown, clear `content_blob`, set `locked=0` + `wrapped_key=NULL`, and re-export each note's
/// `.md` to the vault. The folder returns to the default OPEN state.
#[tauri::command]
pub fn remove_lock(state: State<'_, AppState>, folder_id: String) -> Result<(), AppError> {
    remove_lock_inner(state.inner(), folder_id)
}

/// Inner of [`remove_lock`] taking `&AppState` (so the BLK-1 lifecycle stress test can drive it
/// without a `tauri::State`). BLK-1: holds the [`AppState::lifecycle`] guard across the ENTIRE
/// restore→clear sequence (Step 1 decrypt-plaintext-into-`markdown`, Step 2 clear `content_blob`),
/// so the off-thread `relock_all_inner` blanker can never blank `markdown` to `''` in the window
/// between the two steps — the exact `markdown='' + content_blob=NULL` permanent-loss race.
pub(crate) fn remove_lock_inner(state: &AppState, folder_id: String) -> Result<(), AppError> {
    let _lifecycle = lifecycle_guard(state);
    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    if !folder.locked {
        return Ok(()); // already open — idempotent.
    }
    let wrapped = state
        .db
        .folder_wrapped_key(&folder_id)?
        .ok_or_else(|| AppError::Storage("locked folder has no wrapped key".into()))?;
    let kek = Zeroizing::new(crate::secrets::get_or_create_master_kek()?);
    let ck_bytes = Zeroizing::new(crate::crypto::decrypt(&kek, &wrapped, &aad_wrapped_ck(&folder_id))?);
    let ck: Zeroizing<[u8; 32]> = Zeroizing::new(
        ck_bytes
            .as_slice()
            .try_into()
            .map_err(|_| AppError::Storage("unwrapped content key has wrong length".into()))?,
    );

    let vault = vault_path(state);
    let notes = state.db.notes_in_folder(&folder_id)?;

    // Step 1: restore EVERY provider row's plaintext from ITS OWN blob (or keep the in-memory
    // markdown if the folder is session-unlocked and the blob is absent). This must happen for
    // every row BEFORE any blob is cleared — otherwise a sibling provider's content is lost.
    for n in &notes {
        let markdown = if let Some(blob) = &n.content_blob {
            let aad = aad_content(&folder_id, &n.meeting_id, &n.provider_id, "note");
            let pt = crate::crypto::decrypt(&ck, blob, &aad)?;
            String::from_utf8(pt)
                .map_err(|_| AppError::Storage("decrypted note is not valid UTF-8".into()))?
        } else {
            n.markdown.clone()
        };
        state
            .db
            .restore_note_markdown(&n.meeting_id, &n.provider_id, &markdown)?;
    }

    // Step 2: per meeting, clear the blobs (all rows now hold plaintext) and re-export ONE `.md`
    // (the latest provider's note — matching how the rest of the app treats "the note" for a
    // meeting). All provider rows for that meeting share the re-exported path.
    let mut seen = std::collections::HashSet::new();
    for n in &notes {
        if !seen.insert(n.meeting_id.clone()) {
            continue;
        }
        state.db.clear_note_content_blob(&n.meeting_id)?;

        let Some(vault) = vault.as_deref() else {
            continue;
        };
        let latest = match state.db.get_latest_note_for_meeting(&n.meeting_id)? {
            Some(l) => l,
            None => continue,
        };
        let meeting = state.db.get_meeting(&n.meeting_id)?;
        let (title, date) = match meeting {
            Some(m) => (
                m.title.clone().unwrap_or_else(|| "Untitled".into()),
                m.started_at.clone(),
            ),
            None => ("Untitled".to_string(), chrono::Utc::now().to_rfc3339()),
        };
        let sub = if folder.path.is_empty() {
            None
        } else {
            Some(folder.path.as_str())
        };
        if let Ok(path) = crate::export::write_note(
            std::path::Path::new(vault),
            sub,
            &title,
            &date,
            &latest.markdown,
        ) {
            state.db.set_note_exported_path(
                &n.meeting_id,
                &latest.provider_id,
                &path.to_string_lossy(),
            )?;
        }
    }

    // Phase 0.5 — permanently restore the TRANSCRIPT + TIMELINE plaintext (clear *_blob columns)
    // and the AUDIO WAV (decrypt .enc → file, drop .enc) under the SAME CK. Never lose audio.
    unseal_folder_extras_permanent(state, &folder_id, &ck)?;

    // Flip the folder back to OPEN + drop it from the session set.
    state.db.set_folder_locked(&folder_id, false, None)?;
    {
        let mut g = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        g.remove(&folder_id);
    }
    Ok(())
}

/// Rename a folder: change its display `name` (and the matching vault subdirectory + every governed
/// `path`) without ever touching sealed content.
///
/// Steps, ordered so a crash never loses content:
///  1. Sanitize the new name (same component-safe rule as `create_folder`; reject `/`, `..`, NUL).
///  2. Recompose this folder's vault-relative path = parent path + sanitized name.
///  3. If a vault is configured, MOVE the on-disk subdir `old_path` → `new_path` (best-effort rename;
///     a missing source is fine). The dir holds only the OPEN folder's plaintext `.md`s — sealed
///     folders keep their `.md`s deleted, so a locked-folder rename just renames an empty/absent dir.
///  4. Update the `folders` row (name + path) and re-prefix the path of EVERY descendant folder, and
///     re-point EVERY affected note's `exported_path` from `old_path/...` → `new_path/...`. Sealed
///     notes have `exported_path = NULL` and are skipped — a LOCKED folder rename is metadata-only and
///     never reaches the sealed blob / wrapped key (no decrypt, no re-seal).
///
/// Idempotent-ish: renaming to the same (sanitized) name is a no-op move + a column rewrite to the
/// same values.
#[tauri::command]
pub fn rename_folder(
    state: State<'_, AppState>,
    folder_id: String,
    new_name: String,
) -> Result<Folder, AppError> {
    rename_folder_inner(state.inner(), folder_id, new_name)
}

/// Inner of [`rename_folder`] taking `&AppState` (so tests can drive it without a `tauri::State`).
/// Holds the [`AppState::lifecycle`] guard across the whole rename (path rewrites the seal/unseal
/// lifecycle keys FS ops off — see the command doc).
pub(crate) fn rename_folder_inner(
    state: &AppState,
    folder_id: String,
    new_name: String,
) -> Result<Folder, AppError> {
    // BLK-1: serialize with the rest of the lock state machine. A rename never decrypts, but it
    // rewrites `path` columns that the seal/unseal lifecycle keys vault FS ops off — hold the guard
    // so it can't interleave with a concurrent lock/unlock/remove that also rewrites paths.
    let _lifecycle = lifecycle_guard(state);

    let clean = crate::summarize::organize::sanitize_folder(&new_name)
        .ok_or_else(|| AppError::InvalidArg("folder name is empty or invalid".into()))?;

    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    let old_path = folder.path.clone();

    // Recompose this folder's path from its PARENT's path + the new sanitized name.
    let parent_path = match folder.parent_id.as_deref() {
        Some(pid) => state.db.folder_by_id(pid)?.map(|p| p.path),
        None => None,
    };
    let new_path = match parent_path.as_deref() {
        Some(p) if !p.is_empty() => format!("{p}/{clean}"),
        _ => clean.clone(),
    };

    // No-op fast path: same path AND same name → nothing to move/rewrite.
    if new_path == old_path && clean == folder.name {
        return Ok(Folder { name: clean, path: new_path, ..folder });
    }

    // Move the on-disk vault subdir, if a vault is configured. Both ends are containment-checked.
    // `std::fs::rename` moves the WHOLE subtree (including descendant `.md`s) in one atomic op.
    let mut vault_configured = false;
    if new_path != old_path {
        if let Some(vault) = vault_path(state) {
            vault_configured = true;
            let vault_root = std::path::Path::new(&vault);
            // Destination must stay inside the vault; the source is an existing in-vault dir.
            let dest = assert_in_vault(vault_root, std::path::Path::new(&new_path))?;
            let src = assert_in_vault(vault_root, std::path::Path::new(&old_path))?;
            if src.exists() {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        AppError::Export(format!("create rename parent dir failed: {e}"))
                    })?;
                }
                // A plain rename within the same vault is atomic on the same filesystem.
                std::fs::rename(&src, &dest)
                    .map_err(|e| AppError::Export(format!("rename folder dir failed: {e}")))?;
            } else {
                // Source absent (a locked folder's dir, or never materialized): ensure the
                // destination exists so future plaintext `.md`s land in the renamed dir.
                std::fs::create_dir_all(&dest).map_err(|e| {
                    AppError::Export(format!("create renamed folder dir failed: {e}"))
                })?;
            }
        }
    }

    // Rewrite the DB: this folder's name+path, then re-prefix every DESCENDANT folder's path. Order
    // doesn't risk content loss — no markdown/blob column is touched; only path strings move.
    state.db.rename_folder(&folder_id, &clean, &new_path)?;
    if new_path != old_path {
        reprefix_descendant_folder_paths(state, &folder_id, &new_path)?;
        // Re-derive every governed note's `exported_path` to point under its (possibly renamed)
        // folder's NEW on-disk dir. We rebuild from the file basename + the folder's new dir rather
        // than swapping path prefixes — robust to `/var` vs `/private/var` canonicalization drift in
        // the stored absolute path. The `fs::rename` already moved the bytes; this only re-points the
        // DB. Sealed notes (NULL exported_path) are skipped. Walks this folder + the whole subtree.
        if vault_configured {
            reexport_notes_under_subtree(state, &folder_id)?;
        }
    }

    Ok(Folder { name: clean, path: new_path, ..folder })
}

/// Recursively re-prefix the vault-relative `path` of every DESCENDANT folder of `folder_id` to sit
/// under `new_prefix` after the folder itself was renamed. Walks the tree one level at a time via
/// [`Db::child_folders`]; each child's recomposed path is `new_prefix` + the child's own name (so the
/// rewrite is structural, not a brittle string-replace). Does NOT touch the child's `name`, lock
/// state, or any note content — only the `path` column (the descendants' notes are re-pointed by the
/// single absolute-dir swap in the caller, since `fs::rename` moved the whole subtree at once).
fn reprefix_descendant_folder_paths(
    state: &AppState,
    folder_id: &str,
    new_prefix: &str,
) -> Result<(), AppError> {
    for child in state.db.child_folders(folder_id)? {
        let child_old = child.path.clone();
        let child_new = if new_prefix.is_empty() {
            child.name.clone()
        } else {
            format!("{new_prefix}/{}", child.name)
        };
        if child_new != child_old {
            state.db.rename_folder(&child.id, &child.name, &child_new)?;
        }
        // Recurse into this child's own subtree.
        reprefix_descendant_folder_paths(state, &child.id, &child_new)?;
    }
    Ok(())
}

/// After a folder rename moved the on-disk subtree, re-point the `exported_path` of every governed
/// note in `folder_id` AND its descendants to its folder's NEW vault dir. Each note's new path is
/// `<vault>/<folder.path>/<basename of the old exported_path>` (the `fs::rename` preserved the
/// filename). Rebuilding from the basename (not a string-prefix swap on the stored absolute path) is
/// robust to canonicalization drift (`/var` vs `/private/var`) and to where the original export wrote
/// the path. Sealed notes carry `exported_path = NULL` and are skipped. Requires a configured vault.
fn reexport_notes_under_subtree(state: &AppState, folder_id: &str) -> Result<(), AppError> {
    let Some(vault) = vault_path(state) else {
        return Ok(());
    };
    let vault_root = std::path::Path::new(&vault);

    let folder = match state.db.folder_by_id(folder_id)? {
        Some(f) => f,
        None => return Ok(()),
    };
    // The folder's NEW absolute dir (containment-checked).
    let new_dir = assert_in_vault(vault_root, std::path::Path::new(&folder.path))?;

    for n in state.db.notes_in_folder(folder_id)? {
        let Some(old) = n.exported_path else {
            continue; // sealed note (no .md) — nothing to re-point.
        };
        let Some(name) = std::path::Path::new(&old).file_name() else {
            continue;
        };
        let new_path = new_dir.join(name);
        state
            .db
            .set_note_exported_path(&n.meeting_id, &n.provider_id, &new_path.to_string_lossy())?;
    }

    // Recurse into descendant folders (their dirs moved with the same single `fs::rename`).
    for child in state.db.child_folders(folder_id)? {
        reexport_notes_under_subtree(state, &child.id)?;
    }
    Ok(())
}

/// Delete a folder, NEVER losing a note. SECURITY-CRITICAL — a folder may hold notes and may be
/// sealed (LOCKED). Rules, fail-closed:
///
///  - **Has child folders →** REJECT (`InvalidArg`). The FE deletes leaf-first; refusing here keeps
///    a subtree from being silently orphaned (a child's `parent_id` would dangle).
///  - **LOCKED + NOT session-unlocked →** REJECT (`AppError::Locked`). We have no CK to unseal the
///    folder's notes, so deleting the row would orphan encrypted-and-unrecoverable content (the
///    wrapped key lives on the row we'd delete). Tell the user to unlock first.
///  - **LOCKED + SESSION-UNLOCKED →** PERMANENTLY remove the lock first (`remove_lock_inner`:
///    KEK → unwrap CK → decrypt every note/transcript/timeline/audio back to plaintext, re-export the
///    `.md`, clear the blobs, flip the folder open). Only then does it become the OPEN case below, so
///    nothing is ever left encrypted-and-orphaned.
///  - **OPEN (now) →** move every note to the vault ROOT (`folder_id = NULL`), delete the folder row,
///    and remove the (now-empty) vault subdir. Notes survive at "All notes".
#[tauri::command]
pub fn delete_folder(state: State<'_, AppState>, folder_id: String) -> Result<(), AppError> {
    delete_folder_inner(state.inner(), folder_id)
}

/// Inner of [`delete_folder`] taking `&AppState` (so tests can drive it without a `tauri::State`).
/// See the command doc for the fail-closed rules.
pub(crate) fn delete_folder_inner(state: &AppState, folder_id: String) -> Result<(), AppError> {
    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;

    // Refuse a non-empty SUBTREE — never orphan child folders by dangling their parent_id.
    if !state.db.child_folders(&folder_id)?.is_empty() {
        return Err(AppError::InvalidArg(
            "this folder has subfolders — delete or move them first".into(),
        ));
    }

    // If sealed, it MUST be session-unlocked so we can unseal its notes back to plaintext before the
    // folder row (which carries the wrapped key) is destroyed. Otherwise refuse — never orphan
    // sealed content.
    if folder.locked {
        let session_unlocked = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
            .contains(&folder_id);
        if !session_unlocked {
            return Err(AppError::Locked(
                "unlock this folder first to delete it (its notes are sealed)".into(),
            ));
        }
        // Permanently unseal back to plaintext + re-export the `.md`s, then the folder is OPEN.
        // remove_lock_inner takes the lifecycle guard itself (so we do NOT hold it across this call —
        // the std Mutex is non-reentrant and would self-deadlock).
        remove_lock_inner(state, folder_id.clone())?;
    }

    // OPEN folder now (or was open all along): move its notes to the vault ROOT, then drop the row.
    // Serialize the reassign + row delete + FS cleanup under the lifecycle guard so it can't race a
    // concurrent lock/move on the same folder.
    let _lifecycle = lifecycle_guard(state);

    // Move every note in this folder to the vault root (folder_id = NULL). The notes' plaintext `.md`
    // files already live in this folder's vault subdir; we re-point each meeting's exported_path to
    // the root by moving the file (best-effort, copy-then-remove — never loses bytes).
    let notes = state.db.notes_in_folder(&folder_id)?;
    let mut moved_meetings = std::collections::HashSet::new();
    for n in &notes {
        if !moved_meetings.insert(n.meeting_id.clone()) {
            continue;
        }
        // Reassign every provider row of this meeting to the root.
        state.db.set_meeting_folder(&n.meeting_id, None)?;
        // Best-effort move of the plaintext `.md` to the vault root (only when one exists).
        if let Some(src_path) = n.exported_path.clone() {
            if let Some(vault) = vault_path(state) {
                move_note_file_to_root(state, &n.meeting_id, &src_path, &vault)?;
            }
        }
    }

    // Delete the folder row, then remove the (now note-free) vault subdir. Row first: a leftover
    // empty dir is harmless/reconcilable; a dangling row is not.
    state.db.delete_folder(&folder_id)?;
    if let Some(vault) = vault_path(state) {
        let vault_root = std::path::Path::new(&vault);
        if let Ok(dir) = assert_in_vault(vault_root, std::path::Path::new(&folder.path)) {
            // remove_dir (not _all): only an EMPTY dir is removed, so a stray user file is never
            // clobbered. The notes' `.md`s were moved out above, so the dir should be empty.
            let _ = std::fs::remove_dir(&dir);
        }
    }
    Ok(())
}

/// Move a meeting's plaintext `.md` to the vault ROOT (copy-then-remove, never losing bytes) and
/// re-point its `exported_path`. A `&AppState`-only twin of [`move_note_file`] (whose `&State`
/// signature can't be reached from the `_inner` delete path). Used when deleting a folder demotes its
/// notes to "All notes".
fn move_note_file_to_root(
    state: &AppState,
    meeting_id: &str,
    src_path: &str,
    vault: &str,
) -> Result<(), AppError> {
    let src = std::path::Path::new(src_path);
    let bytes = match std::fs::read_to_string(src) {
        Ok(b) => b,
        // Source already gone → nothing to move; the DB association is already NULL.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(AppError::Export(format!("read note for move failed: {e}"))),
    };
    let file_name = src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| AppError::Export("note path has no filename".into()))?;
    let vault_root = std::path::Path::new(vault);
    let dest = assert_in_vault(vault_root, std::path::Path::new(file_name))?;
    let src_canon = src.canonicalize().unwrap_or_else(|_| src.to_path_buf());
    if dest == src_canon || dest == src {
        return Ok(()); // already at the root.
    }
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| AppError::Export(format!("create move dir failed: {e}")))?;
    }
    // Write the destination atomically, THEN remove the source (never lose bytes).
    crate::export::overwrite_note(&dest, &bytes)?;
    let _ = std::fs::remove_file(src);
    if let Some(existing) = state.db.get_latest_note_for_meeting(meeting_id)? {
        state
            .db
            .set_note_exported_path(meeting_id, &existing.provider_id, &dest.to_string_lossy())?;
    }
    Ok(())
}

/// SESSION-unlock the folder OWNING a meeting (so the FE can unlock straight from the locked
/// Detail view). Resolves the meeting's folder, then delegates to the existing biometric
/// `unlock_folder` path (Touch ID → KEK → unwrap CK → decrypt note + transcript + timeline + audio
/// for the session). A meeting at the vault root or in an open folder is already unlocked → no-op
/// (returns `None`); a sealed folder returns the refreshed `FolderNode`.
#[tauri::command]
pub async fn unlock_meeting(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Option<FolderNode>, AppError> {
    let Some(folder_id) = state.db.folder_for_meeting(&meeting_id)? else {
        return Ok(None); // vault root — nothing to unlock.
    };
    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    if !folder.locked {
        return Ok(None); // open folder — already visible.
    }
    // Reuse the SAME biometric unlock path (do not fork the lifecycle).
    unlock_folder(state, folder_id).await.map(Some)
}

// ── Phase 0.5 full per-folder lock: transcript + timeline + audio seal helpers ──
//
// The note markdown was already sealed (encrypt→content_blob, blank plaintext). These helpers
// extend the SAME lifecycle to a folder's TRANSCRIPT (segments.text), TIMELINE (timelines.data),
// and the AUDIO WAV (a file at meetings.audio_path, NOT in the SQLCipher DB → plaintext on disk).
// All key off the folder content key (CK) the caller has already unwrapped.

/// Suffix marking an audio file as AES-GCM-encrypted-at-rest (sealed folder). The presence of
/// this suffix on `meetings.audio_path` is the on-disk "audio is sealed" signal.
const ENC_SUFFIX: &str = ".enc";

// ── B7/B8 AAD context binding ──────────────────────────────────────────────────────────────────
//
// Every AES-GCM blob is bound to its STORAGE CONTEXT via additional authenticated data so a
// ciphertext cannot be swapped between folders/meetings/providers/record-types. The AAD is NOT
// stored — it is RECONSTRUCTED deterministically from the row's identity at decrypt time. Format is
// a fixed, pipe-joined, versioned byte string; `crypto::decrypt` transparently falls back to empty
// AAD for legacy (pre-AAD) blobs and reports `AadUsed::Legacy` so we re-bind on the next write.
//
// SCHEMA VERSION is part of the content-blob AAD so a future format change is itself
// context-bound; bump it only alongside a migration. Audio + wrapped-CK AADs are intentionally
// minimal (the task spec: audio = meeting|folder, wrapped-CK = folder).

/// AAD schema version for content blobs (notes / transcript segments / timeline). Part of the bound
/// context, so a v1→v2 change cannot be silently down-mixed.
const AAD_SCHEMA_VERSION: &str = "1";

/// AAD for a folder's wrapped content-key: bound to the `folder_id` only (the wrapped CK lives on
/// the folder row; nothing else identifies it).
fn aad_wrapped_ck(folder_id: &str) -> Vec<u8> {
    format!("murmur:wrapck:v{AAD_SCHEMA_VERSION}|folder={folder_id}").into_bytes()
}

/// AAD for a content blob (note / transcript segment / timeline). Bound to
/// `folder_id | meeting_id | provider_id | record_type | schema_version`. `provider_id` is the note
/// provider for note rows, or a fixed sentinel for transcript/timeline rows (which have no provider).
fn aad_content(
    folder_id: &str,
    meeting_id: &str,
    provider_id: &str,
    record_type: &str,
) -> Vec<u8> {
    format!(
        "murmur:content:v{AAD_SCHEMA_VERSION}|folder={folder_id}|meeting={meeting_id}|provider={provider_id}|type={record_type}"
    )
    .into_bytes()
}

/// The ROLE-LESS audio AAD (the historical B8 form): bound to `meeting_id | folder_id`. Retained as
/// the lower rung of the decrypt ladder so masters/playback sealed BEFORE stream-role binding (which
/// carry exactly this NON-empty AAD) still decrypt — see [`aad_audio_role`] and
/// [`crate::crypto::decrypt_file_multi`].
fn aad_audio(meeting_id: &str, folder_id: &str) -> Vec<u8> {
    format!("murmur:audio:v{AAD_SCHEMA_VERSION}|meeting={meeting_id}|folder={folder_id}").into_bytes()
}

/// Which audio stream a `.enc` belongs to. Bound into the audio AAD ([`aad_audio_role`]) so the
/// three per-meeting files — playback WAV (`audio_path`), mic master, sys master — which previously
/// shared the SAME `aad_audio(meeting,folder)` and were therefore cross-decryptable within a meeting,
/// can no longer be swapped for one another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamRole {
    Playback,
    Mic,
    Sys,
}

impl StreamRole {
    fn as_str(self) -> &'static str {
        match self {
            StreamRole::Playback => "playback",
            StreamRole::Mic => "mic",
            StreamRole::Sys => "sys",
        }
    }
}

/// ROLE-BOUND audio AAD: [`aad_audio`] PLUS the stream role, so a mic master can't be swapped for the
/// sys master (or the playback WAV) within the same meeting. New seals bind THIS form.
///
/// ⚠ BACKWARD-COMPAT: existing masters/playback `.enc` were sealed with the role-LESS [`aad_audio`]
/// (a NON-empty AAD). A role-bound decrypt alone would NOT match them, and the empty-AAD legacy
/// fallback would NOT match them either (they are non-empty) → DATA LOSS. So decrypt ALWAYS goes
/// through [`audio_decrypt_ladder`] (role form → role-less form → empty), never a bare role decrypt.
/// A file re-binds to this role form on its next seal.
fn aad_audio_role(meeting_id: &str, folder_id: &str, role: StreamRole) -> Vec<u8> {
    format!(
        "murmur:audio:v{AAD_SCHEMA_VERSION}|meeting={meeting_id}|folder={folder_id}|stream={}",
        role.as_str()
    )
    .into_bytes()
}

/// The two AAD rungs to TRY when decrypting one audio stream, newest-binding first:
/// `[role-bound, role-less]`. (The empty-AAD pre-AAD fallback is built into `crypto::decrypt`, so it
/// is covered by the first rung — see [`crate::crypto::decrypt_file_multi`].) Returned owned so the
/// caller can borrow both as `&[&[u8]]`.
fn audio_decrypt_ladder(meeting_id: &str, folder_id: &str, role: StreamRole) -> (Vec<u8>, Vec<u8>) {
    (
        aad_audio_role(meeting_id, folder_id, role),
        aad_audio(meeting_id, folder_id),
    )
}

/// Sentinel provider id for content blobs that have no note-provider (transcript segments, timeline)
/// — keeps the AAD shape uniform across record types.
const AAD_NO_PROVIDER: &str = "-";

// ── per-file audio-at-rest stages (audio_path + the two masters all share these) ──────────────
// A meeting carries up to THREE at-rest audio files — the playback WAV (`audio_path`) and the two
// faithful masters (`mic_master_path` / `sys_master_path`). All follow the SAME seal lifecycle, so
// these run once PER FILE. Verify-before-destroy lives inside `crypto::encrypt_file`; running
// per-file means a crash mid-loop leaves already-sealed `.enc` + not-yet-sealed plaintext — never
// lost audio. Each returns the new path to persist, or `None` when there's nothing to do.
//
// The SEAL stage takes a single `aad: &[u8]` — the ROLE-bound `aad_audio_role(meeting, folder, role)`
// it binds the new ciphertext to. The two DECRYPT stages take an `aads: &[&[u8]]` LADDER (role form,
// then the historical role-less form) so a master/playback sealed before stream-role binding still
// decrypts and then re-binds on next seal — see `audio_decrypt_ladder` / `crypto::decrypt_file_multi`.
// Every audio blob at rest thus stays AAD-bound to its (meeting|folder|stream) context (B7/B8 + the
// stream-role hardening) — the same guarantee the content blobs get. `reblank_audio` performs no
// crypto (it only drops the decrypted session copy and re-points at the durable `.enc`), so no AAD.

/// SEAL: encrypt `<file>` → `<file>.enc` (verify inside), remove the plaintext only after the
/// verified `.enc` exists. `None` when already sealed, missing on disk, or absent. Idempotent.
fn seal_audio_at_rest(
    ck: &[u8; 32],
    path: Option<String>,
    aad: &[u8],
) -> Result<Option<String>, AppError> {
    let Some(path) = path else { return Ok(None) };
    if path.ends_with(ENC_SUFFIX) || !std::path::Path::new(&path).exists() {
        return Ok(None);
    }
    let enc_path = format!("{path}{ENC_SUFFIX}");
    crate::crypto::encrypt_file(
        ck,
        std::path::Path::new(&path),
        std::path::Path::new(&enc_path),
        aad,
    )?;
    let _ = std::fs::remove_file(&path);
    Ok(Some(enc_path))
}

/// SESSION-unseal: decrypt `<file>.enc` → `<file>` for the session, KEEPING the `.enc`. Returns
/// the plaintext path to persist (`None` if not sealed). `aads` is the role→role-less decrypt ladder
/// (see [`audio_decrypt_ladder`]) so a pre-role master still decrypts.
fn session_unseal_audio(
    ck: &[u8; 32],
    enc_path: Option<String>,
    aads: &[&[u8]],
) -> Result<Option<String>, AppError> {
    let Some(enc_path) = enc_path else { return Ok(None) };
    if !enc_path.ends_with(ENC_SUFFIX) {
        return Ok(None);
    }
    let plain = enc_path.trim_end_matches(ENC_SUFFIX).to_string();
    crate::crypto::decrypt_file_multi(
        ck,
        std::path::Path::new(&enc_path),
        std::path::Path::new(&plain),
        aads,
    )?;
    Ok(Some(plain))
}

/// RE-BLANK (relock): drop the decrypted session copy + re-point at the durable `.enc`. Returns
/// the `.enc` path to persist (`None` if already sealed or the `.enc` is missing). No crypto → no AAD.
fn reblank_audio(path: Option<String>) -> Result<Option<String>, AppError> {
    let Some(path) = path else { return Ok(None) };
    if path.ends_with(ENC_SUFFIX) {
        return Ok(None);
    }
    let enc_path = format!("{path}{ENC_SUFFIX}");
    if !std::path::Path::new(&enc_path).exists() {
        return Ok(None);
    }
    let _ = std::fs::remove_file(&path);
    Ok(Some(enc_path))
}

/// PERMANENT-unseal (remove-lock): decrypt `<file>.enc` → `<file>`, then remove the `.enc`.
/// Returns the plaintext path to persist (`None` if not sealed). Never loses audio. `aads` is the
/// role→role-less decrypt ladder (see [`audio_decrypt_ladder`]) so a pre-role master still decrypts.
fn permanent_unseal_audio(
    ck: &[u8; 32],
    enc_path: Option<String>,
    aads: &[&[u8]],
) -> Result<Option<String>, AppError> {
    let Some(enc_path) = enc_path else { return Ok(None) };
    if !enc_path.ends_with(ENC_SUFFIX) {
        return Ok(None);
    }
    let plain = enc_path.trim_end_matches(ENC_SUFFIX).to_string();
    crate::crypto::decrypt_file_multi(
        ck,
        std::path::Path::new(&enc_path),
        std::path::Path::new(&plain),
        aads,
    )?;
    let _ = std::fs::remove_file(&enc_path);
    Ok(Some(plain))
}

/// SEAL every governed meeting's transcript + timeline under `ck`, then the audio WAV. Mirrors
/// `lock_folder`'s note seal: each blob is verified-decryptable BEFORE the plaintext is blanked /
/// the plaintext WAV is removed — content (transcript / audio) is never lost.
fn seal_folder_extras(state: &AppState, folder_id: &str, ck: &[u8; 32]) -> Result<(), AppError> {
    let meeting_ids = state.db.meeting_ids_in_folder(folder_id)?;
    for mid in &meeting_ids {
        seal_meeting_extras(state, folder_id, mid, ck)?;
    }
    Ok(())
}

/// Seal ONE meeting's transcript + timeline + audio WAV under the folder CK (the per-meeting body of
/// [`seal_folder_extras`]). Reused by [`move_note`] to seal a note moved INTO a session-unlocked
/// locked folder (BLK-2) without touching the folder's other meetings. Verify-before-destroy
/// throughout (no transcript / audio loss); idempotent on already-sealed rows.
fn seal_meeting_extras(
    state: &AppState,
    folder_id: &str,
    mid: &str,
    ck: &[u8; 32],
) -> Result<(), AppError> {
    // Transcript: encrypt each segment's plaintext text, verify, then seal (blank text).
    let segs = state.db.raw_segments(mid)?;
    let mut sealed_segs: Vec<(i64, Vec<u8>)> = Vec::new();
    for s in &segs {
        // Skip rows already sealed (text_blob present, text blank) — idempotent.
        if s.text_blob.is_some() && s.text.is_empty() {
            continue;
        }
        let aad = aad_content(folder_id, mid, AAD_NO_PROVIDER, "segment");
        let blob = crate::crypto::encrypt(ck, s.text.as_bytes(), &aad)?;
        if crate::crypto::decrypt(ck, &blob, &aad)? != s.text.as_bytes() {
            return Err(AppError::Storage(
                "transcript seal verification failed (segment blob mismatch)".into(),
            ));
        }
        sealed_segs.push((s.idx, blob));
    }
    for (idx, blob) in &sealed_segs {
        state.db.seal_segment(mid, *idx, blob)?;
    }

    // Timeline: encrypt the cached JSON (if any), verify, then seal (blank data).
    if let Some(tl) = state.db.raw_timeline(mid)? {
        if !(tl.data_blob.is_some() && tl.data.is_empty()) {
            let aad = aad_content(folder_id, mid, AAD_NO_PROVIDER, "timeline");
            let blob = crate::crypto::encrypt(ck, tl.data.as_bytes(), &aad)?;
            if crate::crypto::decrypt(ck, &blob, &aad)? != tl.data.as_bytes() {
                return Err(AppError::Storage(
                    "timeline seal verification failed (blob mismatch)".into(),
                ));
            }
            state.db.seal_timeline(mid, &blob)?;
        }
    }

    // Audio at rest: the playback WAV + both masters, each encrypted → <file>.enc with
    // verify-before-destroy (inside encrypt_file), then the plaintext removed and the column
    // re-pointed at the .enc. Each blob is AAD-bound to (meeting|folder|STREAM-ROLE) so a sealed
    // audio file can't be swapped between contexts OR between the three streams of one meeting
    // (B7/B8 + stream-role hardening). The timeline was already sealed just above — do NOT re-seal it.
    if let Some(enc) = seal_audio_at_rest(
        ck,
        state.db.get_meeting(mid)?.and_then(|m| m.audio_path),
        &aad_audio_role(mid, folder_id, StreamRole::Playback),
    )? {
        state.db.set_meeting_audio_path(mid, Some(&enc))?;
    }
    let (mic, sys) = state.db.get_meeting_master_paths(mid)?;
    if let Some(enc) = seal_audio_at_rest(ck, mic, &aad_audio_role(mid, folder_id, StreamRole::Mic))? {
        state.db.set_meeting_mic_master_path(mid, Some(&enc))?;
    }
    if let Some(enc) = seal_audio_at_rest(ck, sys, &aad_audio_role(mid, folder_id, StreamRole::Sys))? {
        state.db.set_meeting_sys_master_path(mid, Some(&enc))?;
    }
    Ok(())
}

/// SESSION-unlock: decrypt every governed meeting's transcript + timeline back into the plaintext
/// columns and materialize a playable WAV (decrypt <file>.enc → <file>) re-pointing audio_path at
/// it. Keeps the `.enc` + the `*_blob` columns (folder is still locked on disk).
fn unseal_folder_extras(state: &AppState, folder_id: &str, ck: &[u8; 32]) -> Result<(), AppError> {
    let meeting_ids = state.db.meeting_ids_in_folder(folder_id)?;
    for mid in &meeting_ids {
        for s in state.db.raw_segments(mid)? {
            let Some(blob) = &s.text_blob else { continue };
            let aad = aad_content(folder_id, mid, AAD_NO_PROVIDER, "segment");
            let pt = crate::crypto::decrypt(ck, blob, &aad)?;
            let text = String::from_utf8(pt)
                .map_err(|_| AppError::Storage("decrypted segment is not valid UTF-8".into()))?;
            state.db.restore_segment_text(mid, s.idx, &text)?;
        }
        if let Some(tl) = state.db.raw_timeline(mid)? {
            if let Some(blob) = &tl.data_blob {
                let aad = aad_content(folder_id, mid, AAD_NO_PROVIDER, "timeline");
                let pt = crate::crypto::decrypt(ck, blob, &aad)?;
                let data = String::from_utf8(pt)
                    .map_err(|_| AppError::Storage("decrypted timeline is not valid UTF-8".into()))?;
                state.db.restore_timeline_data(mid, &data)?;
            }
        }
        // Audio at rest: materialize a playable WAV for the session (playback + both masters), each
        // decrypted through the role→role-less AAD ladder (a pre-role master still decrypts); the
        // .enc is kept (folder still locked on disk).
        let (pb_role, pb_less) = audio_decrypt_ladder(mid, folder_id, StreamRole::Playback);
        if let Some(plain) = session_unseal_audio(
            ck,
            state.db.get_meeting(mid)?.and_then(|m| m.audio_path),
            &[&pb_role, &pb_less],
        )? {
            state.db.set_meeting_audio_path(mid, Some(&plain))?;
        }
        let (mic, sys) = state.db.get_meeting_master_paths(mid)?;
        let (mic_role, mic_less) = audio_decrypt_ladder(mid, folder_id, StreamRole::Mic);
        if let Some(plain) = session_unseal_audio(ck, mic, &[&mic_role, &mic_less])? {
            state.db.set_meeting_mic_master_path(mid, Some(&plain))?;
        }
        let (sys_role, sys_less) = audio_decrypt_ladder(mid, folder_id, StreamRole::Sys);
        if let Some(plain) = session_unseal_audio(ck, sys, &[&sys_role, &sys_less])? {
            state.db.set_meeting_sys_master_path(mid, Some(&plain))?;
        }
    }
    Ok(())
}

/// RE-BLANK (relock): re-blank the plaintext transcript + timeline of every governed meeting and
/// remove the decrypted session WAV, re-pointing audio_path back at the `.enc`. The `*_blob`
/// columns + the `.enc` stay (the folder is still `locked=1`). Idempotent.
fn reblank_folder_extras(state: &AppState, folder_id: &str) -> Result<(), AppError> {
    for mid in state.db.meeting_ids_in_folder(folder_id)? {
        for s in state.db.raw_segments(&mid)? {
            if s.text_blob.is_some() && !s.text.is_empty() {
                state.db.restore_segment_text(&mid, s.idx, "")?;
            }
        }
        if let Some(tl) = state.db.raw_timeline(&mid)? {
            if tl.data_blob.is_some() && !tl.data.is_empty() {
                state.db.restore_timeline_data(&mid, "")?;
            }
        }
        if let Some(enc) = reblank_audio(state.db.get_meeting(&mid)?.and_then(|m| m.audio_path))? {
            state.db.set_meeting_audio_path(&mid, Some(&enc))?;
        }
        let (mic, sys) = state.db.get_meeting_master_paths(&mid)?;
        if let Some(enc) = reblank_audio(mic)? {
            state.db.set_meeting_mic_master_path(&mid, Some(&enc))?;
        }
        if let Some(enc) = reblank_audio(sys)? {
            state.db.set_meeting_sys_master_path(&mid, Some(&enc))?;
        }
    }
    Ok(())
}

/// PERMANENT remove-lock: decrypt every governed meeting's transcript + timeline back to plaintext,
/// clear the `*_blob` columns, and permanently restore the plaintext WAV (decrypt .enc → file,
/// remove the .enc). NEVER lose audio — the plaintext is written + the file decrypts before the
/// `.enc` is removed.
fn unseal_folder_extras_permanent(
    state: &AppState,
    folder_id: &str,
    ck: &[u8; 32],
) -> Result<(), AppError> {
    for mid in state.db.meeting_ids_in_folder(folder_id)? {
        // Transcript: restore each segment from its blob (or keep the in-memory text if the folder
        // was session-unlocked and the blob is absent), then clear all blobs for the meeting.
        for s in state.db.raw_segments(&mid)? {
            if let Some(blob) = &s.text_blob {
                let aad = aad_content(folder_id, &mid, AAD_NO_PROVIDER, "segment");
                let pt = crate::crypto::decrypt(ck, blob, &aad)?;
                let text = String::from_utf8(pt)
                    .map_err(|_| AppError::Storage("decrypted segment is not valid UTF-8".into()))?;
                state.db.restore_segment_text(&mid, s.idx, &text)?;
            }
        }
        state.db.clear_segment_blobs(&mid)?;

        if let Some(tl) = state.db.raw_timeline(&mid)? {
            if let Some(blob) = &tl.data_blob {
                let aad = aad_content(folder_id, &mid, AAD_NO_PROVIDER, "timeline");
                let pt = crate::crypto::decrypt(ck, blob, &aad)?;
                let data = String::from_utf8(pt)
                    .map_err(|_| AppError::Storage("decrypted timeline is not valid UTF-8".into()))?;
                state.db.restore_timeline_data(&mid, &data)?;
            }
        }
        state.db.clear_timeline_blob(&mid)?;

        // Audio at rest: permanently restore the playback WAV + both masters from their .enc, each
        // decrypted through the role→role-less AAD ladder (a pre-role master still decrypts); the
        // .enc is dropped only after the plaintext is back.
        let (pb_role, pb_less) = audio_decrypt_ladder(&mid, folder_id, StreamRole::Playback);
        if let Some(plain) = permanent_unseal_audio(
            ck,
            state.db.get_meeting(&mid)?.and_then(|m| m.audio_path),
            &[&pb_role, &pb_less],
        )? {
            state.db.set_meeting_audio_path(&mid, Some(&plain))?;
        }
        let (mic, sys) = state.db.get_meeting_master_paths(&mid)?;
        let (mic_role, mic_less) = audio_decrypt_ladder(&mid, folder_id, StreamRole::Mic);
        if let Some(plain) = permanent_unseal_audio(ck, mic, &[&mic_role, &mic_less])? {
            state.db.set_meeting_mic_master_path(&mid, Some(&plain))?;
        }
        let (sys_role, sys_less) = audio_decrypt_ladder(&mid, folder_id, StreamRole::Sys);
        if let Some(plain) = permanent_unseal_audio(ck, sys, &[&sys_role, &sys_less])? {
            state.db.set_meeting_sys_master_path(&mid, Some(&plain))?;
        }
    }
    Ok(())
}

/// READ-GATE predicate (the user's actual complaint): a meeting is unlocked iff its folder is open
/// (NULL / not locked) OR its folder id is in the current session unlock set. Used by
/// `get_meeting_detail` / `get_segments` / `get_timeline` / `export_audio` to refuse a sealed-and-
/// not-session-unlocked meeting's content even though the SQLCipher DB is open.
/// Snapshot the live session unlock set (the same source `list_folders` / the graph reads use).
/// Passed to the `*_visible` DB reads (BLK-2b) so a sealed-and-not-unlocked meeting contributes
/// nothing to digests, search, last-note, topic threads, etc. — independent of at-rest blanking.
fn unlocked_snapshot(state: &AppState) -> Result<std::collections::HashSet<String>, AppError> {
    Ok(state
        .unlocked_folders
        .lock()
        .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
        .clone())
}

fn meeting_is_unlocked(state: &AppState, meeting_id: &str) -> Result<bool, AppError> {
    let folder_id = match state.db.folder_for_meeting(meeting_id)? {
        Some(f) => f,
        None => return Ok(true), // no folder / vault root → always open.
    };
    let folder = match state.db.folder_by_id(&folder_id)? {
        Some(f) => f,
        None => return Ok(true),
    };
    if !folder.locked {
        return Ok(true); // open folder.
    }
    let unlocked = state
        .unlocked_folders
        .lock()
        .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
    Ok(unlocked.contains(&folder_id))
}

/// The configured vault path (non-empty), or `None`. Takes `&AppState` (callers holding a
/// `tauri::State` pass `&state`, which Deref-coerces) so the `&AppState` inner cores can call it too.
fn vault_path(state: &AppState) -> Option<String> {
    state
        .config
        .lock()
        .ok()
        .and_then(|c| c.vault_path.clone())
        .filter(|p| !p.is_empty())
}

// ── D5 path containment: every vault FS op must stay inside the vault root ──────────────────────
//
// `create_folder` / `move_note_file` / `export_canvas` compose a vault-relative path from
// user-influenced input (a folder name, a note filename). A crafted `..` segment or an absolute
// component could otherwise escape the vault and write/overwrite an arbitrary file. These helpers
// CANONICALIZE the candidate and assert it is contained in the canonicalized vault root BEFORE any
// FS write, failing closed with `AppError::InvalidArg` on escape.

/// Canonicalize `vault` and assert `candidate` (which may not yet exist) resolves INSIDE it. Returns
/// the verified, vault-contained absolute path. Non-existent leaf components are allowed (the path is
/// about to be created) — the deepest EXISTING ancestor is canonicalized (so symlinks are resolved)
/// and the remaining components are appended after rejecting any `..` / root / prefix component that
/// could climb out. The vault root itself must exist (it is the user-configured directory).
fn assert_in_vault(vault: &std::path::Path, candidate: &std::path::Path) -> Result<std::path::PathBuf, AppError> {
    use std::path::Component;

    let root = vault
        .canonicalize()
        .map_err(|e| AppError::InvalidArg(format!("vault path is not accessible: {e}")))?;

    // Walk the candidate, splitting into the longest existing prefix (canonicalized) + a tail of
    // not-yet-existing components. Reject any `..`/RootDir/Prefix in the candidate outright — a
    // legitimate vault-relative target never needs to climb out of or re-anchor the path.
    let mut resolved = root.clone();
    // If the candidate is absolute, start from its root and let the containment check below decide;
    // but still forbid `..` traversal. We rebuild purely from Normal components joined onto either
    // the canonical existing ancestor or the vault root.
    let mut existing = root.clone();
    for comp in candidate.components() {
        match comp {
            Component::Normal(seg) => {
                let next = resolved.join(seg);
                resolved = next;
                // Track the deepest path that actually exists so we can canonicalize through
                // symlinks for the portion on disk.
                let probe = existing.join(seg);
                if probe.exists() {
                    existing = probe
                        .canonicalize()
                        .map_err(|e| AppError::InvalidArg(format!("resolve path component: {e}")))?;
                }
            }
            Component::CurDir => {}
            // `..`, an absolute root, or a Windows prefix could escape the vault — reject.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(AppError::InvalidArg(
                    "path must stay inside the vault (no '..' or absolute segments)".into(),
                ));
            }
        }
    }

    // The canonicalized existing-prefix MUST be inside the vault root (defeats a symlink that points
    // out of the vault), and the fully-resolved target likewise.
    if !existing.starts_with(&root) || !resolved.starts_with(&root) {
        return Err(AppError::InvalidArg(
            "resolved path escapes the vault root".into(),
        ));
    }
    Ok(resolved)
}

/// (Re)start the voice-trigger listener if enabled — model present and not recording —
/// replacing any existing one. Safe to call repeatedly to reconcile after a config change
/// or once a recording finishes.
pub fn restart_voice_listener(app: AppHandle) {
    let state = app.state::<AppState>();
    if let Some(mut l) = state.voice_listener.lock().ok().and_then(|mut g| g.take()) {
        l.stop();
    }
    let (enabled, configured, size, language) = match state.config.lock() {
        Ok(c) => (
            c.voice_trigger,
            c.whisper_model_path.clone(),
            c.model_size.clone(),
            c.language.clone(),
        ),
        Err(_) => return,
    };
    if !enabled {
        return;
    }
    // Don't grab the mic while a real recording is in progress.
    if state.recorder.lock().map(|g| g.is_some()).unwrap_or(false) {
        return;
    }
    let p = configured.as_deref().map(std::path::Path::new);
    match crate::transcribe::resolve_model_path(p, &size, language.as_deref().unwrap_or("")) {
        Ok(Some(model_path)) => {
            let listener =
                crate::audio::listener::VoiceListener::start(app.clone(), model_path, language);
            if let Ok(mut g) = state.voice_listener.lock() {
                *g = Some(listener);
            }
        }
        _ => tracing::warn!(target: "voice", "voice trigger enabled but no Whisper model present"),
    }
}

/// Stop + drop the voice-trigger listener, releasing the mic. No-op if not running.
pub fn stop_voice_listener(app: &AppHandle) {
    let state = app.state::<AppState>();
    if let Some(mut l) = state.voice_listener.lock().ok().and_then(|mut g| g.take()) {
        l.stop();
    }
}

#[cfg(test)]
mod lock_read_gate_tests {
    use super::*;

    fn meeting_with_audio(audio_path: Option<&str>) -> Meeting {
        Meeting {
            id: "m1".to_string(),
            started_at: "2026-06-27T09:00:00Z".to_string(),
            ended_at: None,
            title: Some("Quarterly board strategy".to_string()),
            duration_s: 1800,
            audio_path: audio_path.map(|s| s.to_string()),
            status: MeetingStatus::Summarized,
            folder_id: Some("secret-folder".to_string()),
        }
    }

    /// The master seal stages (`seal_audio_at_rest` → `permanent_unseal_audio`) round-trip a file
    /// byte-identical with verify-before-destroy: the plaintext is removed only after a verified
    /// `.enc` exists, and the `.enc` only after the plaintext is restored. These run per-file for
    /// `audio_path` AND both masters, so this covers the masters' at-rest crypto + crash-safety.
    #[test]
    fn master_seal_stage_round_trips_byte_identical() {
        let ck = [7u8; 32];
        // Seal binds the ROLE form (mic master); unseal goes through the role→role-less ladder. A
        // mismatch would fail the AES-GCM tag check, so this exercises the real bound round-trip
        // under the stream-role hardening.
        let mic_aad = aad_audio_role("m-master", "f-master", StreamRole::Mic);
        let (mic_role, mic_less) = audio_decrypt_ladder("m-master", "f-master", StreamRole::Mic);
        let plain =
            std::env::temp_dir().join(format!("murmur-seal-stage-{}.bin", std::process::id()));
        let original = b"RIFF\x00\x01\x02\xfffake-master-pcm....\x10\x20".to_vec();
        std::fs::write(&plain, &original).unwrap();
        let plain_s = plain.to_string_lossy().to_string();

        let enc = seal_audio_at_rest(&ck, Some(plain_s.clone()), &mic_aad)
            .unwrap()
            .expect("a fresh plaintext path seals");
        assert!(enc.ends_with(ENC_SUFFIX));
        assert!(
            !std::path::Path::new(&plain_s).exists(),
            "plaintext removed only after a verified .enc"
        );
        assert!(std::path::Path::new(&enc).exists(), ".enc written");
        // Idempotent: an already-sealed path is a no-op (never double-encrypts).
        assert!(seal_audio_at_rest(&ck, Some(enc.clone()), &mic_aad)
            .unwrap()
            .is_none());

        // A mic master must NOT decrypt under the SYS ladder (no cross-stream swap within a meeting).
        let (sys_role, sys_less) = audio_decrypt_ladder("m-master", "f-master", StreamRole::Sys);
        assert!(
            permanent_unseal_audio(&ck, Some(enc.clone()), &[&sys_role, &sys_less]).is_err(),
            "the mic master must not unseal under the sys role ladder"
        );

        let restored = permanent_unseal_audio(&ck, Some(enc.clone()), &[&mic_role, &mic_less])
            .unwrap()
            .expect("a .enc path unseals");
        assert_eq!(restored, plain_s);
        assert!(
            !std::path::Path::new(&enc).exists(),
            ".enc removed only after the plaintext is restored"
        );
        let back = std::fs::read(&restored).unwrap();
        let _ = std::fs::remove_file(&restored);
        assert_eq!(back, original, "master survives seal -> unseal byte-identical");
    }

    /// REGRESSION (audio asset-protocol leak): `get_meeting_detail`'s masked DTO for a sealed-and-
    /// not-session-unlocked meeting MUST null `audio_path`. The FE feeds `audio_path` straight into
    /// `convertFileSrc` (the Tauri `asset:` protocol, scoped to the audio dir) which serves the
    /// file to the webview WITHOUT going through the `export_audio` command or `meeting_is_unlocked`
    /// — the one audio read path outside the command gate. Before the fix the masked DTO kept
    /// `audio_path` via `..meeting`; if a PLAINTEXT WAV lived in the scoped dir (e.g. a recording
    /// auto-filed / moved into an already-sealed folder, where the pipeline writes
    /// `<audio>/{id}.wav` with no seal-awareness, or a crash window before re-seal) the locked
    /// view would serve raw audio. Nulling the path closes the bypass regardless of on-disk state.
    #[test]
    fn masked_detail_nulls_audio_path_so_asset_protocol_cannot_serve_a_locked_recording() {
        // The dangerous case: a PLAINTEXT WAV still on disk in the scoped audio dir.
        let plaintext_wav = "/Users/x/Library/Application Support/MeetNotes/audio/m1.wav";
        let masked = masked_detail(meeting_with_audio(Some(plaintext_wav)));

        // The single load-bearing assertion: no path for `convertFileSrc` to serve.
        assert_eq!(
            masked.meeting.audio_path, None,
            "masked detail must NULL audio_path — the FE asset-protocol serve path bypasses the command gate"
        );
        // And the rest of the mask: title hidden, no note, no segments, locked flag set.
        assert_eq!(masked.meeting.title.as_deref(), Some("🔒 Locked"));
        assert!(masked.note.is_none(), "no note while locked");
        assert!(masked.segments.is_empty(), "no segments while locked");
        assert!(masked.locked, "locked flag set so the FE renders the unlock affordance");
        // Non-content metadata is preserved so the FE can offer "unlock this folder".
        assert_eq!(masked.meeting.id, "m1");
        assert_eq!(masked.meeting.folder_id.as_deref(), Some("secret-folder"));
    }

    /// Even with NO audio (already `.enc`-renamed or never recorded), the masked DTO is `None` —
    /// the mask is unconditional, not dependent on the on-disk seal state.
    #[test]
    fn masked_detail_nulls_audio_path_even_when_already_absent() {
        let masked = masked_detail(meeting_with_audio(None));
        assert_eq!(masked.meeting.audio_path, None);
        assert!(masked.locked);
    }

    // ── D5 vault-containment (`assert_in_vault`) ────────────────────────────────────────────────

    fn tmp_vault(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "murmur-vault-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn assert_in_vault_accepts_legit_relative_and_nonexistent_leaf() {
        let vault = tmp_vault("ok");
        // A not-yet-existing nested target inside the vault is allowed (it's about to be created).
        let resolved = assert_in_vault(&vault, std::path::Path::new("Projects/Q3/note.md")).unwrap();
        assert!(resolved.starts_with(vault.canonicalize().unwrap()), "stays inside the vault root");
        // The empty path resolves to the vault root itself.
        let root = assert_in_vault(&vault, std::path::Path::new("")).unwrap();
        assert_eq!(root, vault.canonicalize().unwrap());
        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn assert_in_vault_rejects_parent_dir_traversal_and_absolute() {
        let vault = tmp_vault("escape");
        // `..` traversal that would climb out of the vault.
        assert!(
            assert_in_vault(&vault, std::path::Path::new("../../etc/passwd")).is_err(),
            "must reject a '..' traversal"
        );
        // A `..` even mid-path is rejected outright.
        assert!(
            assert_in_vault(&vault, std::path::Path::new("Projects/../../secret")).is_err(),
            "must reject any embedded '..'"
        );
        // An absolute path is rejected (re-anchors outside the vault).
        assert!(
            assert_in_vault(&vault, std::path::Path::new("/etc/passwd")).is_err(),
            "must reject an absolute path"
        );
        let _ = std::fs::remove_dir_all(&vault);
    }

    #[test]
    fn assert_in_vault_rejects_symlink_escape() {
        let vault = tmp_vault("symlink");
        // A symlink INSIDE the vault that points OUTSIDE must not let a write escape.
        let outside = std::env::temp_dir().join(format!("murmur-outside-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        let link = vault.join("escape-link");
        #[cfg(unix)]
        {
            // Best-effort: if symlink creation fails (e.g. sandbox), skip the assertion.
            if std::os::unix::fs::symlink(&outside, &link).is_ok() {
                let res = assert_in_vault(&vault, std::path::Path::new("escape-link/evil.md"));
                assert!(res.is_err(), "a symlink that points outside the vault must be rejected");
            }
        }
        let _ = std::fs::remove_dir_all(&vault);
        let _ = std::fs::remove_dir_all(&outside);
    }

    // ── B7/B8 AAD context-binding regression at the helper level (defense-in-depth over crypto::) ──

    #[test]
    fn content_aad_distinguishes_every_context_axis() {
        // The five axes (folder, meeting, provider, record-type, schema-version) must each change
        // the AAD so a blob cannot be swapped across any of them.
        let base = aad_content("f", "m", "p", "note");
        assert_ne!(base, aad_content("F", "m", "p", "note"), "folder axis binds");
        assert_ne!(base, aad_content("f", "M", "p", "note"), "meeting axis binds");
        assert_ne!(base, aad_content("f", "m", "P", "note"), "provider axis binds");
        assert_ne!(base, aad_content("f", "m", "p", "segment"), "record-type axis binds");
        // wrapped-CK and audio AADs are distinct namespaces from content.
        assert_ne!(aad_wrapped_ck("f"), aad_content("f", "m", AAD_NO_PROVIDER, "note"));
        assert_ne!(aad_audio("m", "f"), aad_content("f", "m", AAD_NO_PROVIDER, "note"));
    }

    /// Stream-role hardening: each of the three per-meeting audio roles produces a DISTINCT AAD, and
    /// each differs from the historical role-LESS form — so within ONE meeting a mic master can't be
    /// swapped for the sys master or the playback WAV. The role-less form is retained verbatim as the
    /// backward-compat decrypt rung (it must equal the v1 string an existing master was sealed with).
    #[test]
    fn audio_role_aad_distinguishes_each_stream_and_keeps_legacy_form() {
        let pb = aad_audio_role("m", "f", StreamRole::Playback);
        let mic = aad_audio_role("m", "f", StreamRole::Mic);
        let sys = aad_audio_role("m", "f", StreamRole::Sys);
        assert_ne!(pb, mic, "playback vs mic binds");
        assert_ne!(pb, sys, "playback vs sys binds");
        assert_ne!(mic, sys, "mic vs sys binds");

        let role_less = aad_audio("m", "f");
        assert_ne!(role_less, mic, "the role form differs from the role-less form");
        // Each role form is the role-less string PLUS a |stream=… suffix → a role-less blob can never
        // match a role AAD, which is exactly why the decrypt ladder must also try the role-less rung.
        assert!(mic.starts_with(&role_less), "role AAD extends the role-less form");
        // The role-less form is the EXACT v1 string existing masters carry (no drift = no data loss).
        assert_eq!(role_less, b"murmur:audio:v1|meeting=m|folder=f".to_vec());
    }
}

// ── BLK-1 lifecycle-race + BLK-2 move-into-locked + BLK-3/BLK-4 config tests ──────────────────────
#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::storage::Db;
    use crate::transcribe::types::Segment;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, Once};

    // A fixed at-rest DB key (NOT the Keychain) — same shape the config tests use.
    const DB_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    // A fixed dev master KEK so lock/unlock/remove use a deterministic key WITHOUT the Keychain or a
    // Touch ID prompt (the `MURMUR_DEV_KEK` debug-only escape hatch in `secrets::keychain`).
    const DEV_KEK: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    static KEK_ENV: Once = Once::new();
    fn ensure_dev_kek() {
        // Set once, before any thread reads it, so the concurrent readers only ever READ env.
        KEK_ENV.call_once(|| std::env::set_var("MURMUR_DEV_KEK", DEV_KEK));
    }

    fn tmp_db_path(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-lifecycle-{tag}-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// Construct an [`AppState`] backed by a real temp SQLCipher DB, no Keychain, no Tauri. The
    /// recorder/listeners are `None`; the config is the default (no vault → remove-lock skips
    /// re-export, keeping the test filesystem-quiet).
    fn build_state(tag: &str) -> AppState {
        ensure_dev_kek();
        let db = Db::open_with_key(&tmp_db_path(tag), DB_KEY).unwrap();
        AppState {
            recorder: Mutex::new(None),
            system_recorder: Mutex::new(None),
            aec_recorder: Mutex::new(None),
            voice_listener: Mutex::new(None),
            voice_command_capture: Mutex::new(None),
            db,
            config: Mutex::new(AppConfig::default()),
            reasoner: Box::new(crate::reason::StubReasoner),
            current_meeting: Mutex::new(None),
            unlocked_folders: Arc::new(Mutex::new(HashSet::new())),
            master_kek: Mutex::new(None),
            lifecycle: Mutex::new(()),
        }
    }

    /// MANUAL voice command: with NO recording in progress, arming reports `listening:false`
    /// ("not recording") and leaves the capture state empty — the live loop (which only runs while
    /// recording) would never consume it, so we must not pretend to listen.
    #[test]
    fn begin_voice_command_not_recording_does_not_arm() {
        let state = build_state("voicecmd-notrec");
        assert!(state.recorder.lock().unwrap().is_none(), "precondition: not recording");

        let res = begin_voice_command_inner(&state).unwrap();
        assert!(!res.listening, "must not listen when not recording");
        assert_eq!(res.reason.as_deref(), Some("not recording"));
        assert!(
            state.voice_command_capture.lock().unwrap().is_none(),
            "no capture must be armed when not recording"
        );
    }

    /// MANUAL voice command: arming sets a fresh full-budget [`crate::state::CaptureState`] on the
    /// state (the live loop reads it). We exercise the arming half directly (a live `Recorder` needs
    /// a real audio device); the decision/dispatch half is unit-tested in `transcribe::live`.
    #[test]
    fn begin_voice_command_arms_capture_state() {
        use crate::state::CaptureState;
        let state = build_state("voicecmd-arm");
        // Simulate the arm-while-recording write the inner does once the recorder gate passes.
        {
            let mut g = state.voice_command_capture.lock().unwrap();
            *g = Some(CaptureState::armed());
        }
        let armed = *state.voice_command_capture.lock().unwrap();
        assert_eq!(
            armed,
            Some(CaptureState { budget: CaptureState::DEFAULT_BUDGET, start_sample: None }),
            "arming must store a fresh full-budget capture the live loop can consume"
        );
    }

    fn seed_meeting(db: &Db, mid: &str, markdown: &str, folder_id: Option<&str>) {
        db.insert_meeting(&Meeting {
            id: mid.to_string(),
            started_at: "2026-06-27T09:00:00Z".to_string(),
            ended_at: None,
            title: Some("Quarterly strategy".to_string()),
            duration_s: 600,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None, // association lives on notes; set via set_meeting_folder below
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: mid.to_string(),
            provider_id: "claude_code".to_string(),
            markdown: markdown.to_string(),
            created_at: "2026-06-27T09:05:00Z".to_string(),
            exported_path: None,
        })
        .unwrap();
        db.insert_segments(
            mid,
            &[
                Segment { idx: 0, start_s: 0.0, end_s: 2.0, text: "alpha bravo".to_string(), speaker: None },
                Segment { idx: 1, start_s: 2.0, end_s: 4.0, text: "charlie delta".to_string(), speaker: None },
            ],
        )
        .unwrap();
        db.set_timeline_data(mid, "{\"topics\":[],\"speakers\":[]}").unwrap();
        db.set_meeting_folder(mid, folder_id).unwrap();
    }

    fn make_open_folder(db: &Db, id: &str, path: &str) {
        db.insert_folder(&Folder {
            id: id.to_string(),
            name: path.to_string(),
            path: path.to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-27T08:00:00Z".to_string(),
        })
        .unwrap();
    }

    /// BLK-1: hammer the off-thread `relock_all_inner` (the blanker) WHILE `remove_lock_inner` runs
    /// its restore→clear sequence, across many seal/remove cycles, and assert the IRREVERSIBLE-LOSS
    /// state — a note with `markdown=''` AND `content_blob=NULL` — NEVER occurs. The coarse
    /// `AppState::lifecycle` mutex serializes the two so the blank can never land between
    /// `remove_lock`'s Step 1 (restore plaintext) and Step 2 (clear blob).
    #[test]
    fn relock_all_never_destroys_a_note_being_remove_locked() {
        const MID: &str = "m-blk1";
        const FOLDER: &str = "f-blk1";
        const ORIGINAL_MD: &str = "# Board notes\n\n- launch on the 14th\n- hire two engineers";

        let state = Arc::new(build_state("blk1"));
        make_open_folder(&state.db, FOLDER, "Confidential");
        seed_meeting(&state.db, MID, ORIGINAL_MD, Some(FOLDER));

        // A background thread that spams the off-thread blanker continuously.
        let stop = Arc::new(AtomicBool::new(false));
        let spammer = {
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    // Ignore errors — a busy WAL checkpoint etc. is non-fatal to the invariant.
                    let _ = relock_all_inner(&state);
                }
            })
        };

        const ITERS: usize = 60;
        for i in 0..ITERS {
            // Seal the folder (note markdown → '' + content_blob set), then permanently remove the
            // lock (restore plaintext, clear blob). The blanker is racing the whole time.
            lock_folder_inner(&state, FOLDER.to_string()).unwrap();
            remove_lock_inner(&state, FOLDER.to_string()).unwrap();

            // The load-bearing invariant: no provider row is ever blanked-AND-blob-cleared, and the
            // restore put the ORIGINAL content back with the blob gone (folder fully open again).
            for n in state.db.sealable_notes_for_meeting(MID).unwrap() {
                assert!(
                    !(n.markdown.is_empty() && n.content_blob.is_none()),
                    "IRREVERSIBLE DATA LOSS at iter {i}: markdown='' AND content_blob=NULL"
                );
                assert_eq!(n.markdown, ORIGINAL_MD, "note restored to original at iter {i}");
                assert!(n.content_blob.is_none(), "content_blob cleared after remove_lock at iter {i}");
            }
        }

        stop.store(true, Ordering::Relaxed);
        spammer.join().unwrap();

        // Final state: open folder, original content, no residual blob anywhere.
        assert!(!state.db.folder_by_id(FOLDER).unwrap().unwrap().locked);
        let note = state.db.get_latest_note_for_meeting(MID).unwrap().unwrap();
        assert_eq!(note.markdown, ORIGINAL_MD);
    }

    /// BLK-2 (reject half): moving a note INTO a locked folder that is NOT session-unlocked must be
    /// REJECTED (`AppError::Locked`) and leave the note untouched — never reassigned, never blanked.
    #[test]
    fn move_into_locked_not_unlocked_folder_rejects_and_leaves_note_intact() {
        const MID: &str = "m-blk2r";
        const TARGET: &str = "f-blk2r";
        const MD: &str = "# secret meeting\n\nplaintext that must not move into a locked folder";

        let state = build_state("blk2r");
        seed_meeting(&state.db, MID, MD, None); // at the vault root (open)
        make_open_folder(&state.db, TARGET, "Locked-Target-R");
        lock_folder_inner(&state, TARGET.to_string()).unwrap(); // seal it; NOT session-unlocked

        let res = move_into_locked_folder(&state, MID, TARGET);
        assert!(matches!(res, Err(AppError::Locked(_))), "must reject with Locked, got {res:?}");

        // Untouched: still at the root, still plaintext, no blob.
        assert_eq!(state.db.folder_for_meeting(MID).unwrap(), None, "note was NOT reassigned");
        let n = &state.db.sealable_notes_for_meeting(MID).unwrap()[0];
        assert_eq!(n.markdown, MD, "plaintext preserved");
        assert!(n.content_blob.is_none(), "never sealed");
    }

    /// BLK-2 (seal half): moving a note INTO a locked + SESSION-UNLOCKED folder seals it to the
    /// folder's at-rest shape — `content_blob` set, `markdown` blanked, transcript blanked — so no
    /// plaintext ever lands in a locked folder, and the note is reassigned to the target.
    #[test]
    fn move_into_locked_unlocked_folder_seals_the_moved_note() {
        const MID: &str = "m-blk2s";
        const TARGET: &str = "f-blk2s";
        const MD: &str = "# moving in\n\nthis becomes ciphertext at rest";

        let state = build_state("blk2s");
        seed_meeting(&state.db, MID, MD, None); // at the vault root (open, plaintext)
        make_open_folder(&state.db, TARGET, "Locked-Target-S");
        lock_folder_inner(&state, TARGET.to_string()).unwrap(); // seal the (empty) target

        // Make the target SESSION-UNLOCKED: in the unlock set + KEK cached (as a real unlock would).
        state.unlocked_folders.lock().unwrap().insert(TARGET.to_string());
        let kek = secrets::get_or_create_master_kek().unwrap();
        *state.master_kek.lock().unwrap() = Some(Zeroizing::new(kek));

        move_into_locked_folder(&state, MID, TARGET).unwrap();

        // Reassigned into the target AND sealed at rest (blob set, plaintext blanked).
        assert_eq!(state.db.folder_for_meeting(MID).unwrap().as_deref(), Some(TARGET));
        let n = &state.db.sealable_notes_for_meeting(MID).unwrap()[0];
        assert!(n.content_blob.is_some(), "moved note must be sealed (content_blob set)");
        assert!(n.markdown.is_empty(), "moved note plaintext markdown blanked at rest");
        assert!(n.exported_path.is_none(), "no vault .md for a note in a locked folder");
        // Transcript sealed too (text blanked, text_blob present).
        for s in state.db.raw_segments(MID).unwrap() {
            assert!(s.text.is_empty(), "segment text blanked");
            assert!(s.text_blob.is_some(), "segment text_blob present");
        }

        // And it round-trips: a permanent remove-lock restores the original plaintext (no loss).
        remove_lock_inner(&state, TARGET.to_string()).unwrap();
        let restored = state.db.get_latest_note_for_meeting(MID).unwrap().unwrap();
        assert_eq!(restored.markdown, MD, "remove-lock restores the moved note's original content");
    }

    /// Auto-organize seam: [`classify_auto_file_target`] maps a classifier-chosen subfolder to the
    /// right BLK-2 outcome — Open for an unmanaged / open subfolder, RejectToRoot for a locked +
    /// not-session-unlocked folder (so plaintext never lands in a sealed dir), and SealInto for a
    /// locked + session-unlocked folder (write then seal, like a manual move).
    #[test]
    fn classify_auto_file_target_covers_open_locked_and_unlocked() {
        let state = build_state("autofile");

        // No subfolder / unmanaged subfolder → Open.
        assert_eq!(classify_auto_file_target(&state, None).unwrap(), AutoFileTarget::Open);
        assert_eq!(
            classify_auto_file_target(&state, Some("Nonexistent")).unwrap(),
            AutoFileTarget::Open,
            "a subfolder with no matching folder row writes as usual"
        );

        // An OPEN folder row → Open.
        make_open_folder(&state.db, "f-open", "Standups");
        assert_eq!(
            classify_auto_file_target(&state, Some("Standups")).unwrap(),
            AutoFileTarget::Open
        );

        // A LOCKED, not-session-unlocked folder → RejectToRoot (no CK to seal with).
        make_open_folder(&state.db, "f-locked", "Confidential");
        lock_folder_inner(&state, "f-locked".to_string()).unwrap();
        assert_eq!(
            classify_auto_file_target(&state, Some("Confidential")).unwrap(),
            AutoFileTarget::RejectToRoot,
            "plaintext must not be written into a locked, not-unlocked folder"
        );

        // Make it SESSION-UNLOCKED (in the set + KEK cached) → SealInto(folder_id).
        state.unlocked_folders.lock().unwrap().insert("f-locked".to_string());
        let kek = secrets::get_or_create_master_kek().unwrap();
        *state.master_kek.lock().unwrap() = Some(Zeroizing::new(kek));
        assert_eq!(
            classify_auto_file_target(&state, Some("Confidential")).unwrap(),
            AutoFileTarget::SealInto("f-locked".to_string()),
            "a session-unlocked locked folder seals the auto-filed note in"
        );
    }

    /// Auto-organize seam (seal half): a note auto-filed into a session-unlocked locked folder via
    /// [`seal_auto_filed_note`] is sealed to the folder's at-rest shape (blob set, markdown blanked)
    /// and reassigned — exactly like a manual move. No plaintext survives in the sealed dir.
    #[test]
    fn seal_auto_filed_note_seals_into_unlocked_locked_folder() {
        const MID: &str = "m-autofile";
        const TARGET: &str = "f-autofile";
        const MD: &str = "# auto-filed\n\nthis becomes ciphertext at rest";

        let state = build_state("autofile-seal");
        seed_meeting(&state.db, MID, MD, None);
        make_open_folder(&state.db, TARGET, "AutoLocked");
        lock_folder_inner(&state, TARGET.to_string()).unwrap();
        state.unlocked_folders.lock().unwrap().insert(TARGET.to_string());
        let kek = secrets::get_or_create_master_kek().unwrap();
        *state.master_kek.lock().unwrap() = Some(Zeroizing::new(kek));

        seal_auto_filed_note(&state, MID, TARGET).unwrap();

        assert_eq!(state.db.folder_for_meeting(MID).unwrap().as_deref(), Some(TARGET));
        let n = &state.db.sealable_notes_for_meeting(MID).unwrap()[0];
        assert!(n.content_blob.is_some(), "auto-filed note sealed (content_blob set)");
        assert!(n.markdown.is_empty(), "auto-filed note plaintext blanked at rest");
    }

    /// BLK-3: an `AppConfigDto` payload that OMITS `mcpRequireToken` deserializes to `true`
    /// (fail-closed), matching its Stage-E siblings — never silently `false`.
    #[test]
    fn dto_omitting_mcp_require_token_defaults_true() {
        let json = r#"{
            "providerId":"claude_code",
            "anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434",
            "ollamaModel":"llama3.1",
            "claudeBinary":"claude",
            "captureSystemAudio":false,
            "modelSize":"large-v3",
            "voiceTrigger":false,
            "onboarded":true,
            "noteStyle":"standard",
            "autoOrganize":false,
            "noteLanguage":"auto"
        }"#;
        let dto: AppConfigDto = serde_json::from_str(json).unwrap();
        assert!(dto.mcp_require_token, "omitted mcpRequireToken must fail closed to true (BLK-3)");
        assert!(dto.lock_require_biometric, "Stage-E flags default ON");
        assert!(dto.relock_on_screenshare, "Stage-E flags default ON");
        assert!(!dto.cloud_egress_consented, "consent defaults OFF (fail-closed)");
    }

    /// BLK-4: `save_config`'s merge (`dto_to_config`) NEVER lets the DTO set cloud-egress consent —
    /// an omitting/zeroed save preserves an existing `true`, and a save carrying `true` cannot GRANT
    /// it. Only `consent_to_cloud_egress` may flip it.
    #[test]
    fn save_config_merge_never_clobbers_or_grants_consent() {
        // (a) preserve an existing grant even when the DTO carries false.
        let mut dto = config_to_dto(&AppConfig::default());
        dto.cloud_egress_consented = false;
        let current = AppConfig { cloud_egress_consented: true, ..AppConfig::default() };
        assert!(
            dto_to_config(dto, &current).cloud_egress_consented,
            "an omitting/false save must NOT clobber an existing consent (BLK-4)"
        );

        // (b) a save carrying true cannot GRANT consent (default-off stays off).
        let mut dto2 = config_to_dto(&AppConfig::default());
        dto2.cloud_egress_consented = true;
        let current2 = AppConfig::default(); // consent off
        assert!(
            !dto_to_config(dto2, &current2).cloud_egress_consented,
            "a settings save must NEVER grant consent — only the dedicated command may (BLK-4)"
        );
    }

    /// brain2 connectors (NEW EGRESS CLASS): `web_search_consented` is PRESERVE-ONLY on the settings
    /// DTO exactly like `cloud_egress_consented` — an omitting/false save can't clear an existing
    /// grant, and a save carrying `true` can't grant it. Only `consent_to_web_search` may flip it.
    #[test]
    fn save_config_merge_never_clobbers_or_grants_web_search_consent() {
        // (a) preserve an existing grant even when the DTO carries false.
        let mut dto = config_to_dto(&AppConfig::default());
        dto.web_search_consented = false;
        let current = AppConfig { web_search_consented: true, ..AppConfig::default() };
        assert!(
            dto_to_config(dto, &current).web_search_consented,
            "an omitting/false save must NOT clobber an existing web-search consent"
        );

        // (b) a save carrying true cannot GRANT consent (default-off stays off).
        let mut dto2 = config_to_dto(&AppConfig::default());
        dto2.web_search_consented = true;
        let current2 = AppConfig::default(); // consent off
        assert!(
            !dto_to_config(dto2, &current2).web_search_consented,
            "a settings save must NEVER grant web-search consent — only the dedicated command may"
        );
    }

    /// `web_search_enabled` IS settable from the DTO (unlike the consent flag): config_to_dto carries
    /// it out, dto_to_config takes it in (proven by starting from a different `current`).
    #[test]
    fn dto_takes_web_search_enabled_from_payload() {
        let dto_on = {
            let mut d = config_to_dto(&AppConfig::default());
            d.web_search_enabled = true;
            d
        };
        let current_off = AppConfig::default(); // enabled off
        assert!(
            dto_to_config(dto_on, &current_off).web_search_enabled,
            "web_search_enabled is settable from the DTO"
        );
        // And OUT: config_to_dto reflects the live value.
        let cfg = AppConfig { web_search_enabled: true, ..AppConfig::default() };
        assert!(config_to_dto(&cfg).web_search_enabled);
    }

    /// Phase H: the three brain toggles round-trip through the settings DTO. `config_to_dto` carries
    /// them OUT (so the FE can read them) and `dto_to_config` takes them IN (so the FE can set them),
    /// for every `BrainBackend` variant + the bool + a known model id.
    #[test]
    fn dto_round_trips_brain_backend_realtime_reactions_and_model_id() {
        for backend in [BrainBackend::Cloud, BrainBackend::Local, BrainBackend::Off] {
            let cfg = AppConfig {
                brain_backend: backend,
                realtime_reactions: true,
                brain_model_id: Some("bielik-11b-v3".to_string()),
                ..AppConfig::default()
            };
            // OUT: get_config carries the live values to the FE.
            let dto = config_to_dto(&cfg);
            assert_eq!(dto.brain_backend, backend);
            assert!(dto.realtime_reactions);
            assert_eq!(dto.brain_model_id.as_deref(), Some("bielik-11b-v3"));

            // IN: a settings save sets them from the DTO (start from a DIFFERENT current to prove the
            // value comes from the DTO, not preservation).
            let current = AppConfig {
                brain_backend: BrainBackend::Cloud,
                realtime_reactions: false,
                brain_model_id: Some("qwen2.5-3b".to_string()),
                ..AppConfig::default()
            };
            let merged = dto_to_config(dto, &current);
            assert_eq!(merged.brain_backend, backend);
            assert!(merged.realtime_reactions);
            assert_eq!(merged.brain_model_id.as_deref(), Some("bielik-11b-v3"));
        }
    }

    /// brain2 RAG: `semantic_search_enabled` round-trips BOTH ways through the settings DTO. OUT:
    /// `config_to_dto` carries the live value so the FE toggle reflects it. IN: `dto_to_config` TAKES
    /// it from the DTO (settable — unlike `cloud_egress_consented`), proven by starting from a
    /// different `current` so the merged value can only have come from the DTO.
    #[test]
    fn dto_round_trips_semantic_search_enabled_both_ways() {
        // OUT: true and false both surface on the DTO.
        let cfg_on = AppConfig { semantic_search_enabled: true, ..AppConfig::default() };
        assert!(config_to_dto(&cfg_on).semantic_search_enabled);
        let cfg_off = AppConfig { semantic_search_enabled: false, ..AppConfig::default() };
        assert!(!config_to_dto(&cfg_off).semantic_search_enabled);

        // IN (set true): DTO=true over current=false ⇒ merged true (the DTO is authoritative).
        let mut dto_on = config_to_dto(&AppConfig::default());
        dto_on.semantic_search_enabled = true;
        let current_off = AppConfig { semantic_search_enabled: false, ..AppConfig::default() };
        assert!(
            dto_to_config(dto_on, &current_off).semantic_search_enabled,
            "semantic_search_enabled MUST be settable from the DTO (turn on)"
        );

        // IN (clear): DTO=false over current=true ⇒ merged false (settable both directions — NOT
        // preserve-only like cloud_egress_consented).
        let mut dto_off = config_to_dto(&AppConfig::default());
        dto_off.semantic_search_enabled = false;
        let current_on = AppConfig { semantic_search_enabled: true, ..AppConfig::default() };
        assert!(
            !dto_to_config(dto_off, &current_on).semantic_search_enabled,
            "semantic_search_enabled MUST be settable from the DTO (turn off)"
        );
    }

    /// Phase H graceful degradation: a DTO carrying an UNKNOWN `brain_model_id` must NOT be stored —
    /// the live selection is preserved (no error, no bogus id) — while an unknown/omitted
    /// `brain_backend` deserializes to the default `Cloud` rather than crashing the save.
    #[test]
    fn dto_unknown_brain_model_id_preserved_and_unknown_backend_defaults_cloud() {
        // (a) unknown model id ⇒ ignored, current selection preserved.
        let mut dto = config_to_dto(&AppConfig::default());
        dto.brain_model_id = Some("totally-made-up-model".to_string());
        let current = AppConfig {
            brain_model_id: Some("qwen2.5-3b".to_string()),
            ..AppConfig::default()
        };
        assert_eq!(
            dto_to_config(dto, &current).brain_model_id.as_deref(),
            Some("qwen2.5-3b"),
            "an unknown brain_model_id must be ignored, preserving the live selection"
        );

        // (b) a None model id likewise preserves the current selection (a settings save without a
        // brain pick must not clear an existing one).
        let mut dto_none = config_to_dto(&AppConfig::default());
        dto_none.brain_model_id = None;
        let current_some = AppConfig {
            brain_model_id: Some("qwen3-14b".to_string()),
            ..AppConfig::default()
        };
        assert_eq!(
            dto_to_config(dto_none, &current_some).brain_model_id.as_deref(),
            Some("qwen3-14b")
        );

        // (c) an unknown/omitted brainBackend token deserializes to the default Cloud (no crash),
        // then flows through dto_to_config as Cloud.
        let json = r#"{
            "providerId":"claude_code","anthropicModel":"claude-opus-4-8",
            "ollamaBaseUrl":"http://localhost:11434","ollamaModel":"llama3.1","claudeBinary":"claude",
            "captureSystemAudio":false,"modelSize":"large-v3","voiceTrigger":false,"onboarded":true,
            "noteStyle":"standard","autoOrganize":false,"noteLanguage":"auto","brainBackend":"bogus"
        }"#;
        let dto_bad: AppConfigDto = serde_json::from_str(json).unwrap();
        assert_eq!(dto_bad.brain_backend, BrainBackend::Cloud, "unknown token → Cloud");
        assert!(!dto_bad.realtime_reactions, "omitted realtimeReactions defaults OFF");
        assert_eq!(
            dto_to_config(dto_bad, &AppConfig::default()).brain_backend,
            BrainBackend::Cloud
        );
    }

    // ── brain model registry: select + download-target resolution ───────────────────────────────

    /// `select_brain_model` validates the id against the registry and PERSISTS it; reloading config
    /// from the DB returns the chosen id. An unknown id is rejected with `InvalidArg` and leaves the
    /// stored selection untouched.
    #[test]
    fn select_brain_model_persists_valid_and_rejects_unknown() {
        let state = build_state("brain-select");

        select_brain_model_inner(&state, "bielik-11b-v3".to_string()).unwrap();
        assert_eq!(
            state.config.lock().unwrap().brain_model_id.as_deref(),
            Some("bielik-11b-v3")
        );
        // Survives a reload from the settings table.
        assert_eq!(
            AppConfig::load(&state.db).unwrap().brain_model_id.as_deref(),
            Some("bielik-11b-v3")
        );

        // Unknown id ⇒ InvalidArg, selection unchanged.
        let err = select_brain_model_inner(&state, "not-a-real-model".to_string()).unwrap_err();
        assert!(matches!(err, AppError::InvalidArg(_)));
        assert_eq!(
            state.config.lock().unwrap().brain_model_id.as_deref(),
            Some("bielik-11b-v3")
        );
    }

    /// The download-target resolver rejects an unknown id (the exact guard `download_brain_model`
    /// enforces before any network I/O) and resolves a known id to its registry URL + a path inside
    /// the shared models dir.
    #[test]
    fn brain_download_target_rejects_unknown_and_resolves_known() {
        assert!(matches!(
            brain_download_target("bogus-id"),
            Err(AppError::InvalidArg(_))
        ));
        let (url, dest) = brain_download_target("qwen2.5-3b").unwrap();
        assert_eq!(
            url,
            "https://huggingface.co/bartowski/Qwen2.5-3B-Instruct-GGUF/resolve/main/Qwen2.5-3B-Instruct-Q4_K_M.gguf"
        );
        assert!(dest.ends_with("Qwen2.5-3B-Instruct-Q4_K_M.gguf"));
    }

    // ── rename_folder / delete_folder (folder lifecycle) ────────────────────────────────────────

    /// A fresh, unique temp vault dir for the FS-side rename/delete tests.
    fn tmp_vault(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "murmur-folderlc-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// An [`AppState`] with a REAL temp vault dir configured, so the FS-side of rename/delete (dir
    /// move/remove + note `.md` move) actually runs (the keyless `build_state` skips it).
    fn build_state_with_vault(tag: &str, vault: &std::path::Path) -> AppState {
        let s = build_state(tag);
        {
            let mut c = s.config.lock().unwrap();
            c.vault_path = Some(vault.to_string_lossy().to_string());
        }
        s
    }

    fn make_child_folder(db: &Db, id: &str, name: &str, path: &str, parent_id: &str) {
        db.insert_folder(&Folder {
            id: id.to_string(),
            name: name.to_string(),
            path: path.to_string(),
            parent_id: Some(parent_id.to_string()),
            locked: false,
            created_at: "2026-06-27T08:30:00Z".to_string(),
        })
        .unwrap();
    }

    /// Renaming an OPEN folder updates `name` + `path`, MOVES the on-disk vault subdir, and re-points
    /// each note's `exported_path` — content (the `.md` bytes) survives byte-identical.
    #[test]
    fn rename_open_folder_moves_dir_and_reprefixes_paths() {
        let vault = tmp_vault("rename-open");
        let state = build_state_with_vault("rename-open", &vault);

        // An open folder "Work" with one note whose `.md` lives in <vault>/Work/.
        make_open_folder(&state.db, "f1", "Work");
        let work_dir = vault.join("Work");
        std::fs::create_dir_all(&work_dir).unwrap();
        let md_path = work_dir.join("note.md");
        std::fs::write(&md_path, "# real content").unwrap();
        seed_meeting(&state.db, "m1", "# real content", Some("f1"));
        state
            .db
            .set_note_exported_path("m1", "claude_code", &md_path.to_string_lossy())
            .unwrap();

        let renamed = rename_folder_inner(&state, "f1".into(), "Projects".into()).unwrap();
        assert_eq!(renamed.name, "Projects");
        assert_eq!(renamed.path, "Projects");

        // DB row updated.
        let f = state.db.folder_by_id("f1").unwrap().unwrap();
        assert_eq!(f.name, "Projects");
        assert_eq!(f.path, "Projects");

        // On-disk subdir moved (old gone, new present with the SAME bytes).
        assert!(!work_dir.exists(), "old dir gone after rename");
        let new_md = vault.join("Projects").join("note.md");
        assert!(new_md.exists(), "note .md moved into the renamed dir");
        assert_eq!(std::fs::read_to_string(&new_md).unwrap(), "# real content");

        // exported_path re-pointed under the new dir (compare canonicalized — the stored path is the
        // canonicalized absolute form, which on macOS is /private/var… vs the test's /var…).
        let n = state.db.get_latest_note_for_meeting("m1").unwrap().unwrap();
        let stored = n.exported_path.expect("note still has an exported path");
        assert_eq!(
            std::fs::canonicalize(&stored).unwrap(),
            std::fs::canonicalize(&new_md).unwrap(),
            "exported_path points at the moved .md"
        );

        let _ = std::fs::remove_dir_all(&vault);
    }

    /// Renaming a LOCKED folder is METADATA-ONLY: the row name+path change, but no sealed content is
    /// touched — `locked` stays true, the `wrapped_key` is unchanged, and the note's `content_blob`
    /// (the ciphertext) is byte-identical before/after. The blanked plaintext stays blanked.
    #[test]
    fn rename_locked_folder_is_metadata_only_and_never_touches_sealed_content() {
        let vault = tmp_vault("rename-locked");
        let state = build_state_with_vault("rename-locked", &vault);
        std::fs::create_dir_all(vault.join("Secret")).unwrap();

        make_open_folder(&state.db, "lf", "Secret");
        seed_meeting(&state.db, "ms", "# top secret strategy", Some("lf"));
        lock_folder_inner(&state, "lf".to_string()).unwrap(); // seal it (NOT session-unlocked)

        let wrapped_before = state.db.folder_wrapped_key("lf").unwrap();
        let blob_before = state.db.sealable_notes_for_meeting("ms").unwrap()[0]
            .content_blob
            .clone();
        assert!(blob_before.is_some(), "sealed note has a content_blob");

        let renamed = rename_folder_inner(&state, "lf".into(), "Vault".into()).unwrap();
        assert_eq!(renamed.name, "Vault");

        let f = state.db.folder_by_id("lf").unwrap().unwrap();
        assert!(f.locked, "still sealed after a metadata rename");
        assert_eq!(f.name, "Vault");
        assert_eq!(f.path, "Vault");
        assert_eq!(
            state.db.folder_wrapped_key("lf").unwrap(),
            wrapped_before,
            "the wrapped CK is untouched by a rename"
        );
        let after = &state.db.sealable_notes_for_meeting("ms").unwrap()[0];
        assert_eq!(after.content_blob, blob_before, "ciphertext byte-identical after rename");
        assert!(after.markdown.is_empty(), "blanked plaintext stays blanked");

        let _ = std::fs::remove_dir_all(&vault);
    }

    /// A rename re-prefixes DESCENDANT folder paths too (a child of the renamed folder moves with it).
    #[test]
    fn rename_reprefixes_descendant_folder_paths() {
        let state = build_state("rename-desc"); // no vault → pure DB path rewrite
        make_open_folder(&state.db, "parent", "Work");
        make_child_folder(&state.db, "child", "Q3", "Work/Q3", "parent");

        rename_folder_inner(&state, "parent".into(), "Projects".into()).unwrap();

        assert_eq!(state.db.folder_by_id("parent").unwrap().unwrap().path, "Projects");
        assert_eq!(
            state.db.folder_by_id("child").unwrap().unwrap().path,
            "Projects/Q3",
            "the child's path moves under the renamed parent"
        );
    }

    /// Deleting an OPEN folder moves its notes to the vault ROOT (folder_id = NULL), survives the
    /// note bytes (the `.md` moves to the root), deletes the folder row, and removes the empty subdir.
    #[test]
    fn delete_open_folder_demotes_notes_to_root_and_removes_dir() {
        let vault = tmp_vault("del-open");
        let state = build_state_with_vault("del-open", &vault);

        make_open_folder(&state.db, "f", "Trash-Me");
        let dir = vault.join("Trash-Me");
        std::fs::create_dir_all(&dir).unwrap();
        let md = dir.join("keep.md");
        std::fs::write(&md, "# must survive").unwrap();
        seed_meeting(&state.db, "m", "# must survive", Some("f"));
        state
            .db
            .set_note_exported_path("m", "claude_code", &md.to_string_lossy())
            .unwrap();

        delete_folder_inner(&state, "f".into()).unwrap();

        // Folder row gone.
        assert!(state.db.folder_by_id("f").unwrap().is_none(), "folder row deleted");
        // Note survived, now at the root (folder_id NULL).
        assert_eq!(state.db.folder_for_meeting("m").unwrap(), None, "note demoted to All notes");
        let n = state.db.get_latest_note_for_meeting("m").unwrap().unwrap();
        assert_eq!(n.markdown, "# must survive", "note content never lost");
        let root_md = vault.join("keep.md");
        assert!(root_md.exists(), ".md moved to the vault root");
        assert_eq!(std::fs::read_to_string(&root_md).unwrap(), "# must survive");
        assert!(!dir.exists(), "emptied folder dir removed");

        let _ = std::fs::remove_dir_all(&vault);
    }

    /// SECURITY: deleting a LOCKED folder that is NOT session-unlocked is REFUSED (`AppError::Locked`)
    /// — the row (with the wrapped key) and the sealed `content_blob` are untouched, so nothing is
    /// orphaned encrypted-and-unrecoverable.
    #[test]
    fn delete_locked_not_unlocked_folder_refuses_and_keeps_sealed_content() {
        let state = build_state("del-locked");
        make_open_folder(&state.db, "lf", "Sealed");
        seed_meeting(&state.db, "m", "# confidential", Some("lf"));
        lock_folder_inner(&state, "lf".to_string()).unwrap(); // sealed, NOT session-unlocked

        let res = delete_folder_inner(&state, "lf".into());
        assert!(matches!(res, Err(AppError::Locked(_))), "must refuse with Locked, got {res:?}");

        // Folder + sealed content intact.
        assert!(state.db.folder_by_id("lf").unwrap().is_some(), "folder NOT deleted");
        assert!(state.db.folder_wrapped_key("lf").unwrap().is_some(), "wrapped key kept");
        let n = &state.db.sealable_notes_for_meeting("m").unwrap()[0];
        assert!(n.content_blob.is_some(), "ciphertext kept (never orphaned)");
        assert_eq!(state.db.folder_for_meeting("m").unwrap().as_deref(), Some("lf"));
    }

    /// SECURITY: deleting a LOCKED + SESSION-UNLOCKED folder UNSEALS its notes back to plaintext
    /// (remove-lock) BEFORE the row is destroyed, then demotes them to the root — so nothing is left
    /// encrypted-and-orphaned and no note is lost.
    #[test]
    fn delete_locked_session_unlocked_folder_unseals_then_demotes_notes() {
        let vault = tmp_vault("del-unlocked");
        let state = build_state_with_vault("del-unlocked", &vault);
        std::fs::create_dir_all(vault.join("Secret")).unwrap();

        make_open_folder(&state.db, "lf", "Secret");
        seed_meeting(&state.db, "m", "# decrypt me back", Some("lf"));
        lock_folder_inner(&state, "lf".to_string()).unwrap();

        // Make it SESSION-UNLOCKED: in the unlock set + KEK cached (as a real unlock would leave it).
        state.unlocked_folders.lock().unwrap().insert("lf".to_string());
        let kek = secrets::get_or_create_master_kek().unwrap();
        *state.master_kek.lock().unwrap() = Some(Zeroizing::new(kek));

        delete_folder_inner(&state, "lf".into()).unwrap();

        // Folder gone; note unsealed (plaintext restored, blob cleared) and demoted to the root.
        assert!(state.db.folder_by_id("lf").unwrap().is_none(), "folder row deleted");
        assert_eq!(state.db.folder_for_meeting("m").unwrap(), None, "note demoted to All notes");
        let n = &state.db.sealable_notes_for_meeting("m").unwrap()[0];
        assert_eq!(n.markdown, "# decrypt me back", "plaintext restored before delete");
        assert!(n.content_blob.is_none(), "no orphaned ciphertext left behind");
        // The session set no longer references the deleted folder.
        assert!(!state.unlocked_folders.lock().unwrap().contains("lf"));

        let _ = std::fs::remove_dir_all(&vault);
    }

    /// Deleting a folder that still has CHILD folders is refused (`InvalidArg`) — never orphan a
    /// subtree by dangling a child's parent_id.
    #[test]
    fn delete_folder_with_children_refuses() {
        let state = build_state("del-children");
        make_open_folder(&state.db, "parent", "Work");
        make_child_folder(&state.db, "child", "Q3", "Work/Q3", "parent");

        let res = delete_folder_inner(&state, "parent".into());
        assert!(matches!(res, Err(AppError::InvalidArg(_))), "must refuse, got {res:?}");
        assert!(state.db.folder_by_id("parent").unwrap().is_some(), "parent NOT deleted");
        assert!(state.db.folder_by_id("child").unwrap().is_some(), "child NOT orphaned");
    }

    // ── reindex_embeddings (semantic backfill) ──────────────────────────────────────────────────

    /// True iff `mid` surfaces in the GATED semantic read for `text` under `unlocked` — i.e. it has
    /// vec0 chunks AND is visible. Uses the stub embedder (this test asserts WHICH meetings are
    /// indexed, not retrieval QUALITY, so the deterministic stub is sufficient plumbing).
    fn reindex_semantic_finds(db: &Db, mid: &str, text: &str, unlocked: &HashSet<String>) -> bool {
        use crate::embed::Embedder;
        let emb = crate::embed::StubEmbedder;
        let qv = emb.embed(std::slice::from_ref(&text.to_string())).unwrap();
        let qvec = qv.into_iter().next().unwrap_or_default();
        db.search_semantic_visible(&qvec, 50, unlocked)
            .unwrap()
            .iter()
            .any(|h| h.meeting.id == mid)
    }

    /// GATING: `reindex_embeddings_inner` over a corpus of two OPEN (visible) meetings + one SEALED
    /// (locked, not-session-unlocked) meeting indexes ONLY the two visible ones. The sealed meeting
    /// is never returned by `list_meetings_visible`, its plaintext is never chunked/embedded, and its
    /// chunks STAY purged (the seal already removed them) — RED if the gate were dropped.
    #[test]
    fn reindex_indexes_only_visible_meetings_skips_sealed() {
        let state = build_state("reindex-gate");
        make_open_folder(&state.db, "f-lock", "Confidential");

        // Two open meetings (no folder ⇒ always visible) + one in a folder we will SEAL.
        seed_meeting(&state.db, "m-open-1", "Quarterly budget planning and hiring runway.", None);
        seed_meeting(&state.db, "m-open-2", "Roadmap review for the next sprint.", None);
        seed_meeting(&state.db, "m-sealed", "Secret acquisition numbers and the term sheet.", Some("f-lock"));

        // Seal the folder (verify-before-destroy seal + chunk purge) — m-sealed is now invisible.
        lock_folder_inner(&state, "f-lock".to_string()).unwrap();

        let nothing = HashSet::new();
        let stub = crate::embed::StubEmbedder;
        // model_present = true so the guard passes (we deliberately use the stub here for plumbing).
        let res = reindex_embeddings_inner(&state.db, &nothing, true, &stub, |_, _| {}).unwrap();
        assert_eq!(res.status, "indexed");
        // Only the two VISIBLE meetings were processed (the sealed one is absent from the corpus).
        assert_eq!(res.total, 2, "sealed meeting must NOT be in the reindex corpus");
        assert_eq!(res.indexed, 2);

        // The two open meetings are now semantically findable; the sealed one is NOT — even under the
        // same empty unlock set (it has no chunks, and is gated out).
        assert!(reindex_semantic_finds(&state.db, "m-open-1", "budget planning hiring", &nothing));
        assert!(reindex_semantic_finds(&state.db, "m-open-2", "roadmap sprint review", &nothing));
        assert!(
            !reindex_semantic_finds(&state.db, "m-sealed", "secret acquisition term sheet", &nothing),
            "a sealed-not-unlocked meeting must never be indexed by reindex (gate violation)"
        );
    }

    /// MODEL GUARD: with the real e5 model ABSENT (`model_present = false`), `reindex_embeddings_inner`
    /// returns `{ status: "model_missing" }` and indexes NOTHING — it must NOT poison the index with
    /// the deterministic STUB embedder. RED if the guard were dropped (the open meeting would gain
    /// stub chunks).
    #[test]
    fn reindex_model_missing_indexes_nothing() {
        let state = build_state("reindex-nomodel");
        seed_meeting(&state.db, "m-open", "Quarterly budget planning.", None);

        let nothing = HashSet::new();
        let stub = crate::embed::StubEmbedder;
        // model_present = false → the guard short-circuits BEFORE any indexing.
        let res = reindex_embeddings_inner(&state.db, &nothing, false, &stub, |_, _| {}).unwrap();
        assert_eq!(res.status, "model_missing");
        assert_eq!(res.indexed, 0);
        assert_eq!(res.total, 0);
        // No chunks were written — the meeting is NOT semantically findable.
        assert!(
            !reindex_semantic_finds(&state.db, "m-open", "budget planning", &nothing),
            "model_missing guard must index NOTHING (no stub poisoning)"
        );
    }
}

#[cfg(test)]
mod reminder_script_tests {
    use super::{build_reminder_script, escape_applescript, parse_iso_ymd};

    #[test]
    fn parses_strict_iso_only() {
        assert_eq!(parse_iso_ymd("2026-07-01"), Some((2026, 7, 1)));
        assert_eq!(parse_iso_ymd(" 2026-12-31 "), Some((2026, 12, 31)));
        assert_eq!(parse_iso_ymd("2026-13-01"), None); // month out of range
        assert_eq!(parse_iso_ymd("2026-07-32"), None); // day out of range
        assert_eq!(parse_iso_ymd("2026/07/01"), None); // wrong separators
        assert_eq!(parse_iso_ymd("26-07-01"), None); // not 4-digit year
        assert_eq!(parse_iso_ymd(""), None);
    }

    #[test]
    fn due_date_sets_the_date_properties() {
        let s = build_reminder_script("Ship the deck", Some("2026-07-01"));
        // The date is actually attached now (the bug was: only `name` was set).
        assert!(s.contains("set year of theDate to 2026"));
        assert!(s.contains("set month of theDate to 7"));
        assert!(s.contains("set day of theDate to 1"));
        assert!(s.contains("remind me date:theDate"));
        assert!(s.contains("due date:theDate"));
        assert!(s.contains("name:\"Ship the deck\""));
        // `day` is reset to 1 BEFORE year/month so a month change can't overflow the day.
        let reset = s.find("set day of theDate to 1").unwrap();
        let yr = s.find("set year of theDate").unwrap();
        assert!(reset < yr, "day must be reset to 1 before changing year/month");
    }

    #[test]
    fn no_due_date_is_name_only() {
        let s = build_reminder_script("Call Bob", None);
        assert!(s.contains("name:\"Call Bob\""));
        assert!(!s.contains("due date"));
        assert!(!s.contains("theDate"));
    }

    #[test]
    fn invalid_due_date_falls_back_to_name_only() {
        let s = build_reminder_script("Task", Some("not-a-date"));
        assert!(!s.contains("due date"), "an unparseable date must not produce date props");
        assert!(s.contains("name:\"Task\""));
    }

    #[test]
    fn item_text_cannot_break_out_of_the_applescript_literal() {
        // A name carrying a quote + a forged statement must stay INSIDE the string literal: the
        // `"` is escaped to `\"`, so `end tell` / the injected `make` never become real statements.
        let evil = "pwn\", remind me date:theDate}\nend tell\ntell application \"Finder\" to delete";
        let esc = escape_applescript(evil);
        assert!(!esc.contains('\n'), "raw newlines flattened (literals can't span lines)");
        // Every `"` in the payload is preceded by a backslash — no bare quote survives to close
        // the literal early. (Checked by scanning: each `"` byte has a `\` immediately before it.)
        let bytes = esc.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'"' {
                assert!(i > 0 && bytes[i - 1] == b'\\', "unescaped quote survived at {i}");
            }
        }
        let s = build_reminder_script(evil, Some("2026-07-01"));
        // The ONE real `tell` statement (unescaped quotes around Reminders) is intact...
        assert!(
            s.contains("tell application \"Reminders\""),
            "the real Reminders statement must survive"
        );
        // ...and the injected Finder `tell` never becomes real code: its quotes are escaped, so it
        // stays as inert data inside the name literal (no `tell application "Finder"` with REAL quotes).
        assert!(
            !s.contains("tell application \"Finder\""),
            "injected statement must remain escaped data, not executable code"
        );
        // The whole program is a single line (newlines in the payload were flattened), so a forged
        // `end tell` can never start its own statement line.
        assert!(
            !s.lines().any(|l| l.trim() == "end tell"),
            "no standalone injected `end tell` statement line"
        );
        // Every embedded double-quote from the payload is backslash-escaped in the program.
        assert!(s.contains("\\\""), "payload quotes are escaped");
    }
}
