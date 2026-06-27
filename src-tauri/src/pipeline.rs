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
    aec_mic_wav: Option<PathBuf>,
    mic_started_at: std::time::Instant,
    system_started_at: Option<std::time::Instant>,
) -> Result<PipelineResult> {
    match run_inner(
        app,
        state,
        meeting_id,
        samples,
        src_rate,
        duration_s,
        system_wav,
        aec_mic_wav,
        mic_started_at,
        system_started_at,
    )
    .await
    {
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

/// Transcribe one 16 kHz stream at the Accurate profile, VAD-segmented. With a `VadSegmenter`,
/// only speech REGIONS are decoded — each region is a SEPARATE `transcribe_with` call (fresh
/// whisper state → `condition_on_previous_text` reset across long gaps) whose timestamps are
/// re-offset back onto the stream timeline. Without VAD it decodes the whole buffer; an empty
/// region list (VAD ran, found only silence) yields no segments — the "skip muted/silence" path.
fn transcribe_stream(
    transcriber: &Transcriber,
    vad: Option<&mut crate::transcribe::vad::VadSegmenter>,
    samples_16k: &[f32],
    lang: Option<&str>,
) -> Result<Vec<crate::transcribe::types::Segment>> {
    use crate::transcribe::TranscribeQuality;

    let regions: Vec<(usize, usize)> = match vad {
        Some(v) => v.speech_regions(samples_16k)?,
        None => vec![(0, samples_16k.len())],
    };

    let mut out: Vec<crate::transcribe::types::Segment> = Vec::new();
    let mut idx: i64 = 0;
    for (start, end) in regions {
        if end <= start {
            continue;
        }
        let offset_s = start as f64 / crate::audio::TARGET_RATE_HZ as f64;
        let tx =
            transcriber.transcribe_with(&samples_16k[start..end], lang, TranscribeQuality::Accurate)?;
        for mut seg in tx.segments {
            seg.idx = idx;
            seg.start_s += offset_s;
            seg.end_s += offset_s;
            idx += 1;
            out.push(seg);
        }
    }
    Ok(out)
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
    aec_mic_wav: Option<PathBuf>,
    mic_started_at: std::time::Instant,
    system_started_at: Option<std::time::Instant>,
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

    // ── 1. Build 16 kHz mono streams + archive the COMBINED mix ──────────────
    //
    // DUAL-STREAM: mic and system audio are resampled to 16 kHz SEPARATELY and transcribed
    // separately (§2). `mixer::mix` is used ONLY to produce the combined archive WAV for
    // playback — its output is NEVER fed to Whisper. If there's no system stream (capture off
    // or ScreenCaptureKit permission denied → sidecar produced no WAV), we fall back to the
    // mic-only single pass (today's behaviour, everything attributed "me").
    let mut mic_16k = audio::resample_to_16k(&samples, src_rate)?;
    // AEC'd mic for the ASR feed (rec #5): when the VPIO helper produced a WAV, transcribe THAT and
    // keep the RAW cpal mic for the archive (`mic_16k_archive`); otherwise archive == ASR (today).
    // `mem::replace` avoids cloning the (large) mic buffer in the common no-AEC path.
    let mut mic_16k_archive: Option<Vec<f32>> = None;
    if let Some(aec) = &aec_mic_wav {
        match audio::read_wav_mono(aec) {
            Ok((a, rate)) => {
                let _ = std::fs::remove_file(aec); // transient helper output
                let asr = audio::resample_to_16k(&a, rate)?;
                mic_16k_archive = Some(std::mem::replace(&mut mic_16k, asr));
            }
            Err(e) => {
                tracing::warn!(target: "audio", error = %e, "AEC mic unreadable; raw mic for ASR")
            }
        }
    }
    // Native system audio (pre-resample) — kept ONLY for the optional hi-res master.
    let mut sys_native: Option<(Vec<f32>, u32)> = None;
    let mut sys_16k: Option<Vec<f32>> = match &system_wav {
        Some(path) => match audio::read_wav_mono(path) {
            Ok((sys, sys_rate)) => {
                let resampled = audio::resample_to_16k(&sys, sys_rate)?;
                let _ = std::fs::remove_file(path); // transient sidecar output
                if config.keep_hires_masters {
                    sys_native = Some((sys, sys_rate));
                }
                Some(resampled)
            }
            Err(e) => {
                tracing::warn!(target: "audio", error = %e, "could not read system-audio track; using mic only");
                None
            }
        },
        None => None,
    };

    // Archive WAV = the MIX (for playback only). Mic-only when there's no system stream.
    // Archive = the RAW (cpal) mic mixed with system audio — never the AEC'd ASR feed.
    let archive_src = mic_16k_archive.as_ref().unwrap_or(&mic_16k);
    let archive_16k = match &sys_16k {
        Some(sys) => {
            tracing::info!(target: "audio", "archiving mixed mic + system-audio track");
            audio::mix(archive_src, sys)
        }
        None => archive_src.clone(),
    };
    let wav_dir = audio_dir()?;
    let wav_path = wav_dir.join(format!("{meeting_id}.wav"));
    audio::write_wav_16k_mono(&wav_path, &archive_16k, audio::TARGET_RATE_HZ)?;
    state.db.finalize_meeting(
        meeting_id,
        &ended_at,
        duration_s,
        &wav_path.to_string_lossy(),
    )?;

    // Rec #3: faithful per-stream MASTER archives (opt-in). Written from the PRE-resample,
    // PRE-normalize buffers so they stay faithful; they live in the audio dir and are sealed at
    // rest by the lock lifecycle exactly like audio_path. Best-effort: a write failure never fails
    // the recording. NOT exposed to the FE — reachable only via the gated export commands.
    if config.keep_hires_masters {
        let mic_master = wav_dir.join(format!("{meeting_id}.mic.wav"));
        if let Err(e) = audio::write_wav_f32(&mic_master, &samples, src_rate, 1) {
            tracing::warn!(target: "audio", error = %e, "mic master write failed");
        } else if let Err(e) = state
            .db
            .set_meeting_mic_master_path(meeting_id, Some(mic_master.to_string_lossy().as_ref()))
        {
            // Best-effort: a stranded, untracked master plaintext is unreferenced + ungated-out;
            // never fail the recording over it.
            tracing::warn!(target: "audio", error = %e, "persisting mic master path failed");
        }
        if let Some((sys, sys_rate)) = &sys_native {
            let sys_master = wav_dir.join(format!("{meeting_id}.sys.wav"));
            if let Err(e) = audio::write_wav_f32(&sys_master, sys, *sys_rate, 1) {
                tracing::warn!(target: "audio", error = %e, "system master write failed");
            } else if let Err(e) = state
                .db
                .set_meeting_sys_master_path(meeting_id, Some(sys_master.to_string_lossy().as_ref()))
            {
                tracing::warn!(target: "audio", error = %e, "persisting system master path failed");
            }
        }
    }

    // ── 2 + 3. Transcribe EACH stream separately, then MERGE by wall-clock ────
    emit_status(app, "transcribing", "Transcribing audio…", meeting_id);

    // Loudness-normalise the ASR FEEDS only (rec #6). The archive WAV was written above from the
    // un-normalised mix, so the master stays faithful. Gated by the same flag as VAD so the whole
    // batch-ASR enhancement is one reversible switch.
    if config.vad_enabled {
        audio::normalize_for_asr(&mut mic_16k);
        if let Some(sys) = sys_16k.as_mut() {
            audio::normalize_for_asr(sys);
        }
    }

    let model_path = resolve_model_path(&config)?;
    let lang = config.language.as_deref();

    // Resolve the Silero VAD model (best-effort async download on first use). Any failure → None
    // → transcribe the whole buffer (today's behaviour).
    let vad_model_path: Option<std::path::PathBuf> = if config.vad_enabled {
        match crate::transcribe::ensure_vad_model().await {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(target: "transcribe", error = %e, "VAD model unavailable; transcribing whole buffer");
                None
            }
        }
    } else {
        None
    };

    // Whisper load + inference are CPU/GPU-bound and blocking; run the WHOLE transcription off
    // the async runtime's worker so we don't stall other tasks. We load the model ONCE and reuse
    // it for both streams. Owned copies keep the closure 'static.
    let model_path_owned = model_path.clone();
    let lang_owned = lang.map(str::to_string);
    let has_system = sys_16k.is_some();

    // Resolve diarization models (best-effort async download) when diarization is ON and there's a
    // system stream to diarize. Any failure → None → keep the single "others" label.
    let diarize_models: Option<(std::path::PathBuf, std::path::PathBuf)> =
        if config.diarize_others && has_system {
            match crate::transcribe::ensure_diarization_models().await {
                Ok(m) => Some(m),
                Err(e) => {
                    tracing::warn!(target: "transcribe", error = %e, "diarization models unavailable; single 'others' label");
                    None
                }
            }
        } else {
            None
        };

    let merged_segments = tokio::task::spawn_blocking(move || -> Result<Vec<crate::transcribe::types::Segment>> {
        use crate::audio::merge::{merge_streams, StreamInput, SPEAKER_ME, SPEAKER_OTHERS};

        let transcriber = Transcriber::load(&model_path_owned)?;
        // Load the Silero VAD once and reuse it for both streams (best-effort; None → whole buffer).
        let mut vad = vad_model_path.as_deref().and_then(|p| {
            match crate::transcribe::vad::VadSegmenter::load(p) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(target: "transcribe", error = %e, "VAD load failed; transcribing whole buffer");
                    None
                }
            }
        });
        // Diarizer (best-effort): created once when models resolved — used on the system stream only.
        let diarizer = diarize_models.and_then(|(seg, emb)| {
            match crate::transcribe::diarize::Diarizer::load(&seg, &emb) {
                Ok(d) => Some(d),
                Err(e) => {
                    tracing::warn!(target: "transcribe", error = %e, "diarizer load failed; single 'others' label");
                    None
                }
            }
        });

        // BATCH path → Accurate profile for BOTH streams, VAD-segmented so each speech region is a
        // FRESH decode (context reset across long gaps; never decode through silence). Live captions
        // + voice trigger keep the Fast greedy profile for latency — see transcribe::whisper.
        let mic_segments =
            transcribe_stream(&transcriber, vad.as_mut(), &mic_16k, lang_owned.as_deref())?;

        let mut streams = vec![StreamInput {
            segments: mic_segments,
            started_at: mic_started_at,
            speaker: SPEAKER_ME,
        }];

        if let (Some(sys), Some(sys_started)) = (sys_16k, system_started_at) {
            let mut sys_segments =
                transcribe_stream(&transcriber, vad.as_mut(), &sys, lang_owned.as_deref())?;
            // N-way diarization on the "others" stream ONLY — relabel segments to others-0/1/2.
            if let Some(d) = &diarizer {
                match d.diarize(&sys) {
                    Ok(spans) => {
                        crate::transcribe::diarize::relabel_others(&mut sys_segments, &spans)
                    }
                    Err(e) => tracing::warn!(
                        target: "transcribe", error = %e,
                        "diarization failed; single 'others' label"
                    ),
                }
            }
            streams.push(StreamInput {
                segments: sys_segments,
                started_at: sys_started,
                speaker: SPEAKER_OTHERS,
            });
        }

        // Anchor each stream's segments to its capture-start host instant → absolute timeline,
        // merge sorted by absolute start, drop empty (e.g. muted-mic) segments, label "me"/"others".
        Ok(merge_streams(streams))
    })
    .await
    .map_err(|e| AppError::Transcribe(format!("transcription task panicked: {e}")))??;

    if has_system {
        tracing::info!(target: "transcribe", segments = merged_segments.len(), "merged mic + system streams (me/others)");
    }

    state.db.insert_segments(meeting_id, &merged_segments)?;
    state
        .db
        .update_meeting_status(meeting_id, MeetingStatus::Transcribed)?;

    // Rebuild the full transcript text from the merged, time-ordered segments.
    let mut full_text = String::new();
    for seg in &merged_segments {
        let t = seg.text.trim();
        if t.is_empty() {
            continue;
        }
        if !full_text.is_empty() {
            full_text.push(' ');
        }
        full_text.push_str(t);
    }

    if full_text.trim().is_empty() {
        return Err(AppError::Transcribe(
            "No speech detected in the recording — nothing to transcribe. \
             Check your microphone input and try recording again."
                .into(),
        ));
    }

    // ── 4 + 5. Summarize with the configured provider, then export ───────────
    summarize_and_export(
        app,
        state,
        &config,
        meeting_id,
        &full_text,
        lang.map(str::to_string),
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
