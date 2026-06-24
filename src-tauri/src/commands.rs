use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::audio::Recorder;
use crate::error::AppError;
use crate::events::{StatusPayload, EVENT_STATUS};
use crate::settings::AppConfig;
use crate::state::AppState;
use crate::storage::models::{Meeting, MeetingStatus};
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
pub fn save_config(state: State<'_, AppState>, config: AppConfigDto) -> Result<(), AppError> {
    let new_config = dto_to_config(config);
    new_config.save(&state.db)?;
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    *cache = new_config;
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
