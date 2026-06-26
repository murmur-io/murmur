use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::audio::Recorder;
use crate::error::AppError;
use crate::events::{StatusPayload, EVENT_STATUS};
use crate::settings::AppConfig;
use crate::state::AppState;
use crate::storage::models::{
    ActionItem, Analytics, AskVaultResult, BriefResult, BuiltinRecipe, CalendarEvent, ChatTurn,
    DigestResult, EntityDetail, Folder, FolderNode, GraphData, Meeting, MeetingStatus,
    MeetingTimeline, NoteRecord, PinResult, RecipeRecord, SearchHit, TopicThread,
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
    #[serde(default)]
    pub mcp_require_token: bool,
    /// Stage E: default true (matches AppConfig::default) when the FE omits it on an older payload.
    #[serde(default = "default_true")]
    pub lock_require_biometric: bool,
    /// Stage E: default true (matches AppConfig::default) when the FE omits it on an older payload.
    #[serde(default = "default_true")]
    pub relock_on_screenshare: bool,
}

/// serde default for the Stage E security flags (which default ON in `AppConfig`).
fn default_true() -> bool {
    true
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
        folder_id: None,
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

/// Parse a meeting note's "## Action items" checklist into structured items.
#[tauri::command]
pub fn get_action_items(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<ActionItem>, AppError> {
    let note = state.db.get_latest_note_for_meeting(&meeting_id)?;
    Ok(match note {
        Some(n) => crate::summarize::action_items::parse_action_items(&n.markdown),
        None => Vec::new(),
    })
}

/// Rewrite the note's action items into Obsidian Tasks format (📅 due dates) + re-write the
/// vault file in place. Returns the updated note.
#[tauri::command]
pub fn patch_note_tasks(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<NoteDto, AppError> {
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

/// Add a macOS Reminder (via osascript) for an action item. A denied Reminders permission
/// surfaces a clear, actionable error rather than crashing the UI.
#[tauri::command]
pub async fn add_reminder(text: String, due_date: Option<String>) -> Result<(), AppError> {
    let name = match due_date.as_deref().filter(|d| !d.is_empty()) {
        Some(d) => format!("{} 📅 {}", text.trim(), d),
        None => text.trim().to_string(),
    };
    if name.is_empty() {
        return Err(AppError::InvalidArg("empty reminder".into()));
    }
    let esc = name.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "tell application \"Reminders\" to make new reminder with properties {{name:\"{esc}\"}}"
    );
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

/// Pin a meeting moment: append a timestamped ^block-ref to the note (DB + vault file) and
/// return an obsidian:// deep link to the note.
#[tauri::command]
pub fn pin_moment(
    state: State<'_, AppState>,
    meeting_id: String,
    seconds: f64,
    label: String,
) -> Result<PinResult, AppError> {
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
    let unlocked = {
        state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
            .clone()
    };
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
    let unlocked = {
        state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?
            .clone()
    };
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
    let (corpus, sources) = crate::summarize::vault_context::build_vault_context(
        &state.db,
        &question,
        &config.provider_id,
    )?;
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
    let mut corpus = String::new();
    let mut count = 0usize;
    for m in state.db.list_meetings(300)? {
        if m.started_at.as_str() < cutoff.as_str() {
            continue;
        }
        if corpus.len() >= budget {
            break;
        }
        let Some(note) = state.db.get_latest_note_for_meeting(&m.id)? else {
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
    let mut input = Vec::new();
    for m in state.db.list_meetings(500)? {
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
    let dir = std::path::Path::new(&vault).join("Canvas");
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
    let path = dir.join(format!("{fname}.canvas"));
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
    let (corpus, sources) = crate::summarize::vault_context::build_vault_context(
        &state.db,
        &subject,
        &config.provider_id,
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
        mcp_require_token: c.mcp_require_token,
        lock_require_biometric: c.lock_require_biometric,
        relock_on_screenshare: c.relock_on_screenshare,
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
        mcp_require_token: d.mcp_require_token,
        lock_require_biometric: d.lock_require_biometric,
        relock_on_screenshare: d.relock_on_screenshare,
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
    if let Some(vault) = vault_path(&state) {
        let dir = std::path::Path::new(&vault).join(&rel_path);
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

/// Move a note into a folder (or to the root with `folder_id = None`). If the note has an
/// exported `.md` and NEITHER the source nor the target folder is locked, the file is moved on
/// disk (copy-then-remove, best-effort, never loses bytes). Moving into/out of a locked folder
/// does not touch the vault here (sealing/unsealing owns that lifecycle).
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
    // The source folder's lock state: derive from the note's exported_path being present
    // (sealed notes have exported_path = NULL). If exported_path is None we treat the source as
    // "no movable file" and skip the FS move entirely.
    let exported = note.as_ref().and_then(|n| n.exported_path.clone());

    // Reassign in the DB first (the source-of-truth association). Targets EVERY provider row of
    // the meeting (WHERE meeting_id = ?1) so the meeting's folder is consistent across providers
    // and the seal/unlock lifecycle (which iterates provider rows) stays coherent.
    state.db.set_meeting_folder(&meeting_id, folder_id.as_deref())?;

    // Best-effort FS move only when a plaintext .md exists and the target is open.
    if let (Some(src_path), false) = (exported, target_locked) {
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
    let dest_dir = match target_rel.filter(|p| !p.is_empty()) {
        Some(rel) => std::path::Path::new(vault).join(rel),
        None => std::path::Path::new(vault).to_path_buf(),
    };
    let dest = dest_dir.join(file_name);
    if dest == src {
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
    let folder = state
        .db
        .folder_by_id(&folder_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no folder {folder_id}")))?;
    if folder.locked {
        return Ok(()); // already sealed — idempotent.
    }

    let kek = crate::secrets::get_or_create_master_kek()?;
    let ck = crate::crypto::random_key()?;
    let wrapped = crate::crypto::encrypt(&kek, &ck)?;

    // Gather the notes to seal. A meeting may have MULTIPLE provider rows (e.g. re-summarized
    // with ollama then anthropic) each with DISTINCT markdown — seal EVERY (meeting, provider)
    // row into its OWN blob. Collapsing to one blob per meeting would destroy every provider's
    // content but the first (the PRIME-DIRECTIVE content-loss bug this guards against).
    let notes = state.db.notes_in_folder(&folder_id)?;
    let mut sealed_rows: Vec<(String, String, Vec<u8>)> = Vec::new();
    for n in &notes {
        // Encrypt this row's markdown and VERIFY it reads back before we touch the plaintext.
        let blob = crate::crypto::encrypt(&ck, n.markdown.as_bytes())?;
        let check = crate::crypto::decrypt(&ck, &blob)?;
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
    // Biometric gate (Stage E teeth): require a passing Touch ID / device-owner auth before we
    // release the KEK and decrypt any sealed note. Gated by K_LOCK_REQUIRE_BIOMETRIC (default on)
    // so it can be disabled. On hardware/policy unavailability the gate degrades to allow (see
    // biometric::authenticate), so this never locks out a Mac without Touch ID.
    let require_biometric = {
        let cfg = state
            .config
            .lock()
            .map_err(|_| AppError::Storage("config mutex poisoned".into()))?;
        cfg.lock_require_biometric
    };
    if require_biometric && !crate::biometric::authenticate("Unlock this folder").await? {
        return Err(AppError::Auth("biometric authentication failed".into()));
    }

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

    let kek = crate::secrets::get_or_create_master_kek()?;
    let ck_bytes = crate::crypto::decrypt(&kek, &wrapped)?;
    let ck: [u8; 32] = ck_bytes
        .as_slice()
        .try_into()
        .map_err(|_| AppError::Storage("unwrapped content key has wrong length".into()))?;

    // Decrypt EACH sealed provider row's own blob back into its own markdown column for the
    // session (no dedup by meeting — every provider's distinct content is restored independently).
    let notes = state.db.notes_in_folder(&folder_id)?;
    for n in &notes {
        let Some(blob) = &n.content_blob else {
            continue; // open note (shouldn't happen in a sealed folder) — skip.
        };
        let pt = crate::crypto::decrypt(&ck, blob)?;
        let markdown = String::from_utf8(pt)
            .map_err(|_| AppError::Storage("decrypted note is not valid UTF-8".into()))?;
        state
            .db
            .restore_note_markdown(&n.meeting_id, &n.provider_id, &markdown)?;
    }

    // Cache the KEK for the session (zeroized on relock-all) + add to the unlock set.
    {
        let mut g = state
            .master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        *g = Some(kek);
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
    {
        let mut g = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        g.remove(&folder_id);
    }
    let mut one = std::collections::HashSet::new();
    one.insert(folder_id);
    state.db.blank_sealed_notes_in_folders(&one)?;
    Ok(())
}

/// Relock ALL session-unlocked folders + zeroize the cached KEK (called on screen-share start in
/// Stage E, and exposed as a command). Re-blanks the plaintext markdown of every sealed note.
#[tauri::command]
pub fn relock_all(state: State<'_, AppState>) -> Result<(), AppError> {
    relock_all_inner(&state)
}

/// Inner relock-all usable without a command boundary (Stage E screen-share watcher).
pub(crate) fn relock_all_inner(state: &AppState) -> Result<(), AppError> {
    // Clear the session set.
    {
        let mut g = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Storage("unlocked-folders mutex poisoned".into()))?;
        g.clear();
    }
    // Zeroize the cached KEK copy.
    {
        let mut g = state
            .master_kek
            .lock()
            .map_err(|_| AppError::Storage("master-kek mutex poisoned".into()))?;
        if let Some(mut k) = g.take() {
            k.iter_mut().for_each(|b| *b = 0);
        }
    }
    // Re-blank every sealed note across all locked folders.
    let locked: std::collections::HashSet<String> =
        state.db.locked_folder_ids()?.into_iter().collect();
    state.db.blank_sealed_notes_in_folders(&locked)?;
    Ok(())
}

/// PERMANENTLY remove a folder's lock: KEK → unwrap CK → decrypt each note back to plaintext
/// markdown, clear `content_blob`, set `locked=0` + `wrapped_key=NULL`, and re-export each note's
/// `.md` to the vault. The folder returns to the default OPEN state.
#[tauri::command]
pub fn remove_lock(state: State<'_, AppState>, folder_id: String) -> Result<(), AppError> {
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
    let kek = crate::secrets::get_or_create_master_kek()?;
    let ck_bytes = crate::crypto::decrypt(&kek, &wrapped)?;
    let ck: [u8; 32] = ck_bytes
        .as_slice()
        .try_into()
        .map_err(|_| AppError::Storage("unwrapped content key has wrong length".into()))?;

    let vault = vault_path(&state);
    let notes = state.db.notes_in_folder(&folder_id)?;

    // Step 1: restore EVERY provider row's plaintext from ITS OWN blob (or keep the in-memory
    // markdown if the folder is session-unlocked and the blob is absent). This must happen for
    // every row BEFORE any blob is cleared — otherwise a sibling provider's content is lost.
    for n in &notes {
        let markdown = if let Some(blob) = &n.content_blob {
            let pt = crate::crypto::decrypt(&ck, blob)?;
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

/// The configured vault path (non-empty), or `None`.
fn vault_path(state: &State<'_, AppState>) -> Option<String> {
    state
        .config
        .lock()
        .ok()
        .and_then(|c| c.vault_path.clone())
        .filter(|p| !p.is_empty())
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
