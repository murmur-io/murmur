use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::audio::Recorder;
use crate::error::AppError;
use crate::events::{StatusPayload, EVENT_STATUS};
use crate::settings::AppConfig;
use crate::state::AppState;
use crate::storage::models::{
    Analytics, BuiltinRecipe, ChatTurn, Meeting, MeetingStatus, MeetingTimeline, NoteRecord,
    RecipeRecord, SearchHit,
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
    pub exported_path: String,
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
    pub capture_system_audio: bool,
    pub model_size: String,
    pub voice_trigger: bool,
    pub onboarded: bool,
    pub note_style: String,
    pub auto_organize: bool,
    pub note_language: String,
}

/// A meeting + its latest note + transcript segments (Library Detail view).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingDetailDto {
    pub meeting: Meeting,
    pub note: Option<NoteDto>,
    pub segments: Vec<Segment>,
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
    })?;

    // Free the mic from the voice listener (if any) before opening it for the recording.
    {
        let app2 = app.clone();
        let _ = tokio::task::spawn_blocking(move || stop_voice_listener(&app2)).await;
    }

    // Start mic capture.
    let recorder = Recorder::start()?;
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
        if enabled && crate::audio::system::is_available() {
            let sys_wav = std::env::temp_dir().join(format!("meetnotes-sys-{meeting_id}.wav"));
            match crate::audio::system::SystemAudioRecorder::start(sys_wav) {
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

    let (samples, src_rate) = recorder.stop()?;

    // Stop the system-audio sidecar (if any) and collect its WAV for mixing.
    let system_wav = {
        let rec = state
            .system_recorder
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
        &app, &state, &meeting_id, samples, src_rate, duration_s, system_wav,
    )
    .await?;

    // Resume voice listening if it's still enabled (the mic is free again).
    restart_voice_listener(app);

    Ok(StopResult {
        meeting_id: result.meeting_id,
        markdown: result.note_markdown,
        exported_path: result.exported_path.to_string_lossy().to_string(),
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

/// The most recent note (markdown + export path) for the last-note preview pane.
#[tauri::command]
pub fn get_last_note(state: State<'_, AppState>) -> Result<Option<NoteDto>, AppError> {
    let note = state.db.latest_note()?;
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
    state.db.search(&query, 100)
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

/// Write a meeting's note markdown to a user-chosen path (FE picks it via a save dialog).
#[tauri::command]
pub fn export_note(
    state: State<'_, AppState>,
    meeting_id: String,
    dest_path: String,
) -> Result<(), AppError> {
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
    let new_config = dto_to_config(config);
    new_config.save(&state.db)?;
    {
        let mut cache = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        *cache = new_config;
    }
    // Reconcile the voice-trigger listener with the new config.
    restart_voice_listener(app);
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
        capture_system_audio: c.capture_system_audio,
        model_size: c.model_size.clone(),
        voice_trigger: c.voice_trigger,
        onboarded: c.onboarded,
        note_style: c.note_style.clone(),
        auto_organize: c.auto_organize,
        note_language: c.note_language.clone(),
    }
}

fn dto_to_config(d: AppConfigDto) -> AppConfig {
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
        capture_system_audio: d.capture_system_audio,
        model_size: if d.model_size.trim().is_empty() {
            "small".to_string()
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
    }
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
    let result = pipeline::resummarize_existing(&app, &state, &meeting_id).await?;
    Ok(StopResult {
        meeting_id: result.meeting_id,
        markdown: result.note_markdown,
        exported_path: result.exported_path.to_string_lossy().to_string(),
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

/// Speaker + topic timeline for a meeting (AI-derived, cached after first generation).
#[tauri::command]
pub async fn get_timeline(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<MeetingTimeline, AppError> {
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
    }))
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

/// Show/hide the floating recorder bar window (also bound to the global ⌘⇧R shortcut).
#[tauri::command]
pub fn toggle_bar(app: AppHandle) {
    crate::toggle_bar(&app);
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
