use std::path::{Path, PathBuf};

use tauri::{AppHandle, Emitter};

use crate::error::{AppError, Result};
use crate::events::{StatusPayload, EVENT_STATUS};
use crate::settings::AppConfig;
use crate::state::AppState;
use crate::storage::models::{MeetingStatus, NoteRecord};
use crate::summarize::provider::{MeetingMeta, SummarizeRequest};
use crate::summarize::{make_provider, template};
use crate::transcribe::{self, Transcriber};
use crate::{audio, export};

pub struct PipelineResult {
    pub note_markdown: String,
    pub exported_path: PathBuf,
    pub meeting_id: String,
}

/// App-data folder + audio subdir for recorded WAVs.
const APP_DIR: &str = "MeetNotes";
const AUDIO_SUBDIR: &str = "audio";

/// Emit a `StatusPayload` on `EVENT_STATUS`. Best-effort: a failed emit is logged but
/// never aborts the pipeline.
fn emit_status(app: &AppHandle, stage: &str, message: &str, meeting_id: &str) {
    let payload = StatusPayload {
        stage: stage.to_string(),
        message: message.to_string(),
        meeting_id: Some(meeting_id.to_string()),
    };
    if let Err(e) = app.emit(EVENT_STATUS, payload) {
        tracing::warn!(target: "pipeline", error = %e, "failed to emit status event");
    }
}

/// `<app-data>/MeetNotes/audio`, created if absent.
fn audio_dir() -> Result<PathBuf> {
    let base = dirs::data_dir()
        .ok_or_else(|| AppError::Storage("could not resolve app-data directory".into()))?;
    let dir = base.join(APP_DIR).join(AUDIO_SUBDIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::Storage(format!("create audio dir: {e}")))?;
    Ok(dir)
}

/// Full post-Stop pipeline: write WAV → transcribe → persist segments → summarize with
/// the configured provider → persist note → export `.md` → update meeting status. Emits
/// a `StatusPayload` on `EVENT_STATUS` at each stage. Returns the exported note path +
/// markdown.
///
/// On any error the meeting is marked `Error` and an `error`-stage status is emitted
/// before the error is propagated to the caller (the Tauri command).
#[allow(clippy::too_many_arguments)]
pub async fn run_after_stop(
    app: &AppHandle,
    state: &AppState,
    meeting_id: &str,
    samples: Vec<f32>,
    src_rate: u32,
    duration_s: i64,
    system_wav: Option<PathBuf>,
) -> Result<PipelineResult> {
    match run_inner(app, state, meeting_id, samples, src_rate, duration_s, system_wav).await {
        Ok(result) => Ok(result),
        Err(e) => {
            // Persist the failure and surface it to the UI without leaking PII.
            let _ = state
                .db
                .update_meeting_status(meeting_id, MeetingStatus::Error);
            emit_status(app, "error", &e.to_string(), meeting_id);
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_inner(
    app: &AppHandle,
    state: &AppState,
    meeting_id: &str,
    samples: Vec<f32>,
    src_rate: u32,
    duration_s: i64,
    system_wav: Option<PathBuf>,
) -> Result<PipelineResult> {
    // Snapshot config under the lock, then release it for the rest of the async work.
    let config: AppConfig = {
        let guard = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        guard.clone()
    };

    let now = chrono::Utc::now();
    let date_iso = now.format("%Y-%m-%d").to_string();
    let ended_at = now.to_rfc3339();

    // ── 1. Build 16 kHz mono samples (mic, optionally mixed with system audio) ──
    let mic_16k = audio::resample_to_16k(&samples, src_rate)?;
    let samples_16k = match system_wav {
        Some(path) => match audio::read_wav_mono(&path) {
            Ok((sys, sys_rate)) => {
                let sys_16k = audio::resample_to_16k(&sys, sys_rate)?;
                let _ = std::fs::remove_file(&path); // transient sidecar output
                tracing::info!(target: "audio", "mixed mic + system-audio tracks");
                audio::mix(&mic_16k, &sys_16k)
            }
            Err(e) => {
                tracing::warn!(target: "audio", error = %e, "could not read system-audio track; using mic only");
                mic_16k
            }
        },
        None => mic_16k,
    };

    // Persist the (possibly mixed) 16 kHz mono WAV for archive.
    let wav_dir = audio_dir()?;
    let wav_path = wav_dir.join(format!("{meeting_id}.wav"));
    audio::write_wav_16k_mono(&wav_path, &samples_16k, audio::TARGET_RATE_HZ)?;
    state.db.finalize_meeting(
        meeting_id,
        &ended_at,
        duration_s,
        &wav_path.to_string_lossy(),
    )?;

    // ── 2. Transcribe ───────────────────────────────────────────────────────
    emit_status(app, "transcribing", "Transcribing audio…", meeting_id);

    let model_path = resolve_model_path(&config)?;
    let lang = config.language.as_deref();

    // Whisper load + inference are CPU/GPU-bound and blocking; run off the async
    // runtime's worker so we don't stall other tasks. The model path / lang are
    // owned copies so the closure is 'static.
    let model_path_owned = model_path.clone();
    let lang_owned = lang.map(str::to_string);
    let transcript = tokio::task::spawn_blocking(move || -> Result<_> {
        let transcriber = Transcriber::load(&model_path_owned)?;
        // BATCH path → Accurate profile (beam search + temperature fallback +
        // anti-hallucination thresholds + previous-text conditioning). Live captions and the
        // voice trigger keep the Fast greedy profile for latency — see transcribe::whisper.
        transcriber.transcribe_with(
            &samples_16k,
            lang_owned.as_deref(),
            crate::transcribe::TranscribeQuality::Accurate,
        )
    })
    .await
    .map_err(|e| AppError::Transcribe(format!("transcription task panicked: {e}")))??;

    state
        .db
        .insert_segments(meeting_id, &transcript.segments)?;
    state
        .db
        .update_meeting_status(meeting_id, MeetingStatus::Transcribed)?;

    if transcript.full_text.trim().is_empty() {
        return Err(AppError::Transcribe(
            "No speech detected in the recording — nothing to transcribe. \
             Check your microphone input and try recording again."
                .into(),
        ));
    }

    // ── 3 + 4. Summarize with the configured provider, then export ───────────
    summarize_and_export(
        app,
        state,
        &config,
        meeting_id,
        &transcript.full_text,
        transcript.language.clone().or_else(|| lang.map(str::to_string)),
        duration_s,
        &date_iso,
        &ended_at,
    )
    .await
}

/// Summarize a transcript with the configured provider, persist the note, export it to
/// the Obsidian vault, and update the meeting status/title. Shared by the full pipeline
/// and `resummarize_existing`.
#[allow(clippy::too_many_arguments)]
async fn summarize_and_export(
    app: &AppHandle,
    state: &AppState,
    config: &AppConfig,
    meeting_id: &str,
    transcript_text: &str,
    language: Option<String>,
    duration_s: i64,
    date_iso: &str,
    when_iso: &str,
) -> Result<PipelineResult> {
    emit_status(
        app,
        "summarizing",
        &format!("Summarizing with provider '{}'…", config.provider_id),
        meeting_id,
    );

    let vault_titles = match config.vault_path.as_deref() {
        Some(p) if !p.is_empty() => export::list_vault_titles(Path::new(p)).unwrap_or_default(),
        _ => Vec::new(),
    };

    let request = SummarizeRequest {
        transcript: transcript_text.to_string(),
        meta: MeetingMeta {
            date_iso: date_iso.to_string(),
            title_hint: None,
            duration_s,
            language,
        },
        template: template::build_template(&config.note_style, &config.note_language),
        vault_titles,
    };

    let provider = make_provider(&config.provider_id, config)?;
    let markdown = provider.summarize(&request).await?;

    state
        .db
        .update_meeting_status(meeting_id, MeetingStatus::Summarized)?;

    let created_at = chrono::Utc::now().to_rfc3339();
    state.db.upsert_note(&NoteRecord {
        meeting_id: meeting_id.to_string(),
        provider_id: config.provider_id.clone(),
        markdown: markdown.clone(),
        created_at,
        exported_path: None,
    })?;

    emit_status(app, "exporting", "Writing note to vault…", meeting_id);

    let vault_path = config
        .vault_path
        .as_deref()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| {
            AppError::Export("no vault folder configured — set one in Settings".into())
        })?;

    let title = derive_title(&markdown, date_iso);
    let subfolder =
        resolve_subfolder(config, provider.as_ref(), vault_path, &title, &markdown).await;
    let exported_path = export::write_note(
        Path::new(vault_path),
        subfolder.as_deref(),
        &title,
        when_iso,
        &markdown,
    )?;

    state.db.set_note_exported_path(
        meeting_id,
        &config.provider_id,
        &exported_path.to_string_lossy(),
    )?;
    state.db.set_meeting_title(meeting_id, &title)?;
    state
        .db
        .update_meeting_status(meeting_id, MeetingStatus::Exported)?;

    emit_status(app, "done", "Note exported.", meeting_id);

    // Best-effort self-assembling graph: persist entities/mentions to the encrypted DB (Sink A)
    // + mirror vault stubs for unsealed folders (Sink B). NEVER fail the note on a graph error —
    // a graph-extraction LLM hiccup must not block note export. `add_mention` idempotency makes
    // the `resummarize_existing` path safe (re-extraction refreshes without double-counting).
    if let Err(e) =
        crate::commands::build_and_persist_entities(state, meeting_id, &title, &markdown).await
    {
        tracing::warn!(target: "graph", error = %e, "graph entity persist failed (note export unaffected)");
    }

    Ok(PipelineResult {
        note_markdown: markdown,
        exported_path,
        meeting_id: meeting_id.to_string(),
    })
}

/// Pick the vault subfolder for a note: AI thematic filing (nested under the configured
/// subfolder, if any) when `auto_organize` is on; otherwise just the configured subfolder.
/// Classification failures degrade gracefully to the configured subfolder.
async fn resolve_subfolder(
    config: &AppConfig,
    provider: &dyn crate::summarize::provider::SummarizerProvider,
    vault_path: &str,
    title: &str,
    markdown: &str,
) -> Option<String> {
    if !config.auto_organize {
        return config.vault_subfolder.clone();
    }
    let existing = export::list_subfolders(Path::new(vault_path)).unwrap_or_default();
    match crate::summarize::organize::classify_subfolder(provider, title, markdown, &existing).await
    {
        Some(folder) => match config.vault_subfolder.as_deref().filter(|s| !s.is_empty()) {
            Some(base) => Some(format!("{base}/{folder}")),
            None => Some(folder),
        },
        None => config.vault_subfolder.clone(),
    }
}

/// Re-run summarize + export for an existing meeting using its already-stored transcript
/// segments. No re-capture or re-transcription. Errors if the meeting or its segments are
/// missing.
pub async fn resummarize_existing(
    app: &AppHandle,
    state: &AppState,
    meeting_id: &str,
) -> Result<PipelineResult> {
    let config: AppConfig = {
        let guard = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        guard.clone()
    };

    let meeting = state
        .db
        .get_meeting(meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no meeting with id {meeting_id}")))?;

    let segments = state.db.get_segments(meeting_id)?;
    if segments.is_empty() {
        return Err(AppError::Transcribe(
            "meeting has no stored transcript to re-summarize".into(),
        ));
    }

    // Rebuild full text from segments (same joining as the transcriber).
    let mut full_text = String::new();
    for seg in &segments {
        let t = seg.text.trim();
        if t.is_empty() {
            continue;
        }
        if !full_text.is_empty() {
            full_text.push(' ');
        }
        full_text.push_str(t);
    }

    let date_iso = meeting
        .started_at
        .split(['T', ' '])
        .next()
        .unwrap_or(&meeting.started_at)
        .to_string();
    let when_iso = meeting.ended_at.clone().unwrap_or(meeting.started_at);

    match summarize_and_export(
        app,
        state,
        &config,
        meeting_id,
        &full_text,
        config.language.clone(),
        meeting.duration_s,
        &date_iso,
        &when_iso,
    )
    .await
    {
        Ok(result) => Ok(result),
        Err(e) => {
            let _ = state
                .db
                .update_meeting_status(meeting_id, MeetingStatus::Error);
            emit_status(app, "error", &e.to_string(), meeting_id);
            Err(e)
        }
    }
}

/// Resolve the Whisper model path from config, falling back to a model already present
/// in the app-data models dir. Returns a clear error if none is available (we do NOT
/// auto-download inside the pipeline in Phase 0 — that would surprise the user mid-run).
fn resolve_model_path(config: &AppConfig) -> Result<PathBuf> {
    let configured = config.whisper_model_path.as_deref().map(Path::new);
    let language = config.language.as_deref().unwrap_or("");
    match transcribe::resolve_model_path(configured, &config.model_size, language)? {
        Some(p) => Ok(p),
        None => Err(AppError::Transcribe(
            "no Whisper model found — pick a language + model in Settings and download it"
                .into(),
        )),
    }
}

/// Derive a note title from the generated markdown's YAML front-matter `title:` key, or
/// the first `# heading`, falling back to a date-stamped default. Pure text — no PII
/// concerns beyond what the user already sees in the note.
fn derive_title(markdown: &str, date_iso: &str) -> String {
    // Try front-matter `title:` first.
    for line in markdown.lines().take(20) {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("title:") {
            let t = rest.trim().trim_matches('"').trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    // Then the first markdown H1.
    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# ") {
            let t = rest.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    format!("Meeting {date_iso}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_title_prefers_frontmatter() {
        let md = "---\ntitle: Q3 Planning\ndate: 2026-06-24\n---\n# Heading\n";
        assert_eq!(derive_title(md, "2026-06-24"), "Q3 Planning");
    }

    #[test]
    fn derive_title_quoted_frontmatter() {
        let md = "---\ntitle: \"Weekly Sync\"\n---\n";
        assert_eq!(derive_title(md, "2026-06-24"), "Weekly Sync");
    }

    #[test]
    fn derive_title_falls_back_to_h1() {
        let md = "---\ndate: 2026-06-24\n---\n# Standup Notes\nbody";
        assert_eq!(derive_title(md, "2026-06-24"), "Standup Notes");
    }

    #[test]
    fn derive_title_default_when_nothing() {
        let md = "plain text with no title";
        assert_eq!(derive_title(md, "2026-06-24"), "Meeting 2026-06-24");
    }
}
