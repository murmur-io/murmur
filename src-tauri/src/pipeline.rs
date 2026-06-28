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
    /// Path of the exported Obsidian `.md`, or `None` when no vault folder is configured.
    /// The note is ALWAYS persisted to the canonical DB (`upsert_note`); the vault is
    /// export-only, so a missing vault yields `None` here — not an error.
    pub exported_path: Option<PathBuf>,
    pub meeting_id: String,
}

/// Audio subdir (under the app-data folder) for recorded WAVs. The parent folder name comes from
/// [`crate::state::app_dir_name`] so dev's recordings live under `MeetNotes-dev`, isolated from the
/// installed release's `MeetNotes` — the same split as the DB dir.
const AUDIO_SUBDIR: &str = "audio";

/// RAII guard that deletes a transient sidecar scratch WAV (AEC mic / system-audio) when it
/// drops — on EVERY exit path of the pipeline, success OR error OR panic-unwind. These helper
/// outputs are PLAINTEXT audio fragments in `$TMPDIR`; the old code removed them only on the
/// happy path, so a read/resample/transcribe error left a plaintext fragment behind (a
/// data-at-rest leak). Holding one guard per scratch file for the whole of `run_inner` makes the
/// cleanup unconditional, regardless of which `?` returns first.
struct ScratchWav(Option<PathBuf>);

impl ScratchWav {
    fn new(path: Option<PathBuf>) -> Self {
        Self(path)
    }

    /// Borrow the wrapped path (if any) for reading — without giving up the delete-on-drop.
    fn path(&self) -> Option<&Path> {
        self.0.as_deref()
    }
}

impl Drop for ScratchWav {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            // Best-effort: an already-gone file or a remove error must never abort cleanup.
            let _ = std::fs::remove_file(p);
        }
    }
}

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

/// `<app-data>/<app_dir_name()>/audio`, created if absent (`MeetNotes` release, `MeetNotes-dev` dev).
fn audio_dir() -> Result<PathBuf> {
    let base = dirs::data_dir()
        .ok_or_else(|| AppError::Storage("could not resolve app-data directory".into()))?;
    let dir = base.join(crate::state::app_dir_name()).join(AUDIO_SUBDIR);
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
    // Wrap the transient sidecar scratch WAVs in delete-on-drop guards so the plaintext fragments
    // are removed on EVERY exit path below (success, any `?`-error, or panic) — not just the Ok
    // branch. Previously a `read_wav_mono` failure (AEC + system) and a `resample_to_16k` failure
    // (system, whose `?` ran BEFORE the remove) each stranded a plaintext audio fragment in $TMPDIR.
    let aec_scratch = ScratchWav::new(aec_mic_wav);
    let system_scratch = ScratchWav::new(system_wav);

    let mut mic_16k = audio::resample_to_16k(&samples, src_rate)?;
    // AEC'd mic for the ASR feed (rec #5): when the VPIO helper produced a WAV, transcribe THAT and
    // keep the RAW cpal mic for the archive (`mic_16k_archive`); otherwise archive == ASR (today).
    // `mem::replace` avoids cloning the (large) mic buffer in the common no-AEC path.
    let mut mic_16k_archive: Option<Vec<f32>> = None;
    if let Some(aec) = aec_scratch.path() {
        match audio::read_wav_mono(aec) {
            Ok((a, rate)) => {
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
    let mut sys_16k: Option<Vec<f32>> = match system_scratch.path() {
        Some(path) => match audio::read_wav_mono(path) {
            Ok((sys, sys_rate)) => {
                let resampled = audio::resample_to_16k(&sys, sys_rate)?;
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
    // Both scratch WAVs are now read into memory (or were absent/unreadable); delete them NOW to
    // keep the plaintext-on-disk window minimal. The guards already covered every early-return path
    // above; this just reclaims them as soon as they're no longer needed (and is a safe no-op if a
    // later `?` re-drops them).
    drop(aec_scratch);
    drop(system_scratch);

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

    // Rebuild the full transcript text from the merged, time-ordered segments — EXCLUDING segments
    // the user spoke TO the assistant ("Klaudku, sprawdź pogodę"). Those are assistant commands, not
    // meeting content: fed to the summarizer they get mangled into owner-less action items. The
    // assistant's ANSWER lives in the persisted Q&A log (assistant_interactions), not the note.
    let mut full_text = String::new();
    for seg in &merged_segments {
        let t = seg.text.trim();
        if t.is_empty() || crate::audio::wake::is_assistant_directed(t) {
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

/// Summarize a transcript with the configured provider, persist the note to the canonical
/// DB, and — when a vault folder is configured — export it to the Obsidian vault and update
/// the meeting status/title. When NO vault is configured the note is still fully saved
/// (DB + title), `exported_path` is `None`, and the meeting finishes in `Summarized` (NOT
/// `Error`): the vault is export-only. Shared by the full pipeline and `resummarize_existing`.
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

    // brain2 RAG Phase 4 — RETRIEVAL-AUGMENTED NOTE GENERATION (ALWAYS ON). Ground the new note in
    // related PRIOR notes so notes compound ("last time you decided X"). GATED by the LIVE session
    // unlock set: the retrieval routes through `search_visible` + `get_note_if_visible` (inside
    // `build_grounding_context` → `build_related_context`), so a sealed-and-not-session-unlocked
    // prior note contributes NOTHING. This matters because the corpus EGRESSES to the provider in
    // the prompt — but it is the SAME provider call (`make_provider` → RedactingProvider +
    // fail-closed `cloud_egress_consented` gate) the summary already makes, so always-on adds NO
    // new egress class and does NOT bypass consent (local provider stays local; no consent → the
    // summary already fails closed). Best-effort: a retrieval error logs (target rag, no PII) and
    // proceeds with NO context, and an empty corpus yields `None` (byte-identical to no-context) —
    // it NEVER fails the pipeline.
    let unlocked = state
        .unlocked_folders
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    let related_title = state
        .db
        .get_meeting(meeting_id)
        .ok()
        .flatten()
        .and_then(|m| m.title);
    // Phase B step 3 (Flow A) — the local brain DECIDES what context to fetch. With no real model
    // (the StubReasoner, the default build) `orchestrate_context` falls through to the EXACT
    // deterministic `build_grounding_context` path below (byte-identical, zero behavior change). With
    // a real reasoner it runs a gated pre-analysis → retrieval plan, but the deterministic
    // salient-query path stays the FALLBACK FLOOR. The reasoner call is synchronous, so this keeps
    // the existing inline shape (no extra await). Best-effort + GATED: same egress/consent envelope.
    let related_context = crate::orchestrate::orchestrate_context(
        &*state.reasoner,
        &state.db,
        meeting_id,
        related_title.as_deref(),
        transcript_text,
        &unlocked,
        config,
    );

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
        related_context,
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

    // brain2 RAG Phase 2b — index the just-persisted note into the on-device vector layer, GATED by
    // the master flag AND the meeting's visibility (never index a sealed-not-unlocked folder's
    // plaintext). Best-effort: a failure logs (no PII) and NEVER fails the pipeline. Flag-off ⇒ this
    // does literally nothing (no embedder built, no writes). A note that is subsequently sealed into
    // a locked folder (the `SealInto` auto-file path below) has its chunks purged by the seal, so the
    // net at-rest state never carries a sealed meeting's vectors.
    if config.semantic_search_enabled {
        let unlocked = state
            .unlocked_folders
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        let embedder = crate::embed::active_embedder();
        if let Err(e) =
            index_meeting_if_enabled(&state.db, meeting_id, true, &unlocked, embedder.as_ref())
        {
            tracing::warn!(target: "rag", error = %e, "semantic index of new note failed (note unaffected)");
        }
    }

    // No vault configured → the note is ALREADY fully saved in Murmur's canonical DB above
    // (upsert_note + status Summarized). The Obsidian vault is EXPORT-ONLY: skip the whole
    // export / auto-organize / seal block, leave `exported_path = None`, and finish in the
    // terminal `Summarized` state — NOT `Error`. Recording without a vault is fully supported.
    let Some(vault_path) = config.vault_path.as_deref().filter(|p| !p.is_empty()) else {
        // Title the meeting so the library/detail view shows it (same title we'd derive on export).
        let title = finalize_note_without_vault(&state.db, meeting_id, &markdown, date_iso)?;
        emit_status(
            app,
            "saved",
            "Saved to Murmur — set a vault folder in Settings to export to Obsidian.",
            meeting_id,
        );
        // Best-effort self-assembling graph: the encrypted DB (Sink A) is the canonical store and
        // works without a vault; never fail the saved note on a graph hiccup.
        if let Err(e) =
            crate::commands::build_and_persist_entities(state, meeting_id, &title, &markdown).await
        {
            tracing::warn!(target: "graph", error = %e, "graph entity persist failed (note saved unaffected)");
        }
        return Ok(PipelineResult {
            note_markdown: markdown,
            exported_path: None,
            meeting_id: meeting_id.to_string(),
        });
    };

    emit_status(app, "exporting", "Writing note to vault…", meeting_id);

    let title = derive_title(&markdown, date_iso);
    let subfolder =
        resolve_subfolder(config, provider.as_ref(), vault_path, &title, &markdown).await;

    // Auto-organize safety (BLK-2 parity): if the classifier chose a subfolder that maps to a LOCKED
    // folder, plaintext must NEVER land in its sealed on-disk dir — not even transiently. Decide
    // BEFORE writing.
    let auto_file = crate::commands::classify_auto_file_target(state, subfolder.as_deref())?;
    let write_subfolder = match auto_file {
        // Open / root / unmanaged subfolder → write into the chosen subfolder as usual.
        crate::commands::AutoFileTarget::Open => subfolder.as_deref(),
        // Any LOCKED target → write at the vault ROOT instead. `SealInto` then seals the note INTO
        // the folder (encrypting it + removing this root `.md`); `RejectToRoot` simply leaves it at
        // the root. Either way no plaintext is ever written into the sealed on-disk dir.
        crate::commands::AutoFileTarget::SealInto(_)
        | crate::commands::AutoFileTarget::RejectToRoot => None,
    };
    let exported_path = export::write_note(
        Path::new(vault_path),
        write_subfolder,
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

    // Seal the note INTO a session-unlocked locked folder (encrypts the markdown/extras and removes
    // the plaintext `.md` we just wrote) — the same outcome a manual move would produce.
    if let crate::commands::AutoFileTarget::SealInto(folder_id) = &auto_file {
        if let Err(e) = crate::commands::seal_auto_filed_note(state, meeting_id, folder_id) {
            // Rare relock race: the `.md` is on disk but the folder is now locked. Never leave
            // plaintext in a sealed dir — drop the stray `.md` (the note markdown is still in the DB).
            let _ = std::fs::remove_file(&exported_path);
            return Err(e);
        }
    }

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
        exported_path: Some(exported_path),
        meeting_id: meeting_id.to_string(),
    })
}

/// Finalize a summarized note when NO vault folder is configured. By this point the caller
/// (`summarize_and_export`) has ALREADY upserted the note markdown to the canonical DB with
/// `exported_path = None` and moved the meeting to `Summarized`; here we only title the meeting
/// from the note so the library/detail view shows it. The Obsidian vault is EXPORT-ONLY, so the
/// no-vault path is a SUCCESS — it NEVER builds an `Export` error. Returns the derived title (for
/// the graph-entity step). DB-only (no `AppHandle`/provider) so the no-vault contract is
/// unit-testable in isolation.
fn finalize_note_without_vault(
    db: &crate::storage::Db,
    meeting_id: &str,
    markdown: &str,
    date_iso: &str,
) -> Result<String> {
    let title = derive_title(markdown, date_iso);
    db.set_meeting_title(meeting_id, &title)?;
    Ok(title)
}

/// brain2 RAG Phase 2b — the GATED entry point for indexing a note into the vector layer. Pure +
/// AppState-free (takes only the `Db`, the flag, the live `unlocked` set, and the embedder) so the
/// gate is unit-testable in isolation. Two short-circuits before any embedding/write:
///   1. `enabled == false` ⇒ no-op (the prod-safe default ⇒ NOTHING is indexed).
///   2. the meeting is sealed-and-not-session-unlocked (`meeting_is_visible == false`) ⇒ no-op, so a
///      locked folder's plaintext is NEVER chunked/embedded (same visibility predicate as every read).
///
/// Only a visible meeting under an ENABLED flag reaches `index_meeting_chunks`.
pub(crate) fn index_meeting_if_enabled(
    db: &crate::storage::Db,
    meeting_id: &str,
    enabled: bool,
    unlocked: &std::collections::HashSet<String>,
    embedder: &dyn crate::embed::Embedder,
) -> Result<()> {
    if !enabled {
        return Ok(());
    }
    if !db.meeting_is_visible(meeting_id, unlocked)? {
        return Ok(()); // sealed-not-unlocked: never index its plaintext.
    }
    db.index_meeting_chunks(meeting_id, embedder)
}

/// brain2 RAG Phase 4 — RETRIEVAL-AUGMENTED NOTE GENERATION (ALWAYS ON). Build the GATED corpus of
/// related PRIOR notes used to ground a new note. AppState-free (takes only the `Db`, the live
/// `unlocked` session set, the meeting id/title/transcript, and the provider id) so the always-on
/// path + its gate are unit-testable in isolation.
///
/// LOCK INVARIANT (load-bearing): the returned corpus is injected into the summarization prompt and
/// therefore EGRESSES to the provider. The retrieval routes through `search_visible` +
/// `get_note_if_visible` on the LIVE `unlocked` set (inside `build_related_context`), and the
/// meeting being summarized is self-excluded — a sealed-and-not-session-unlocked prior note
/// contributes NOTHING. Egress is the SAME provider call the summary already makes (no new egress
/// class, no consent bypass).
///
/// BEST-EFFORT: a retrieval `Err` logs (target `rag`, no PII) and yields `None`; an empty corpus
/// (fresh vault / no related notes / all-stopword transcript) also yields `None`, so
/// `render_user_content` stays byte-identical to the no-context path. It NEVER fails the pipeline.
pub(crate) fn build_grounding_context(
    db: &crate::storage::Db,
    unlocked: &std::collections::HashSet<String>,
    meeting_id: &str,
    title: Option<&str>,
    transcript: &str,
    provider_id: &str,
) -> Option<String> {
    let query = crate::summarize::related_context::salient_query(title, transcript);
    match crate::summarize::related_context::build_related_context(
        db,
        meeting_id,
        &query,
        unlocked,
        provider_id,
    ) {
        Ok((corpus, sources)) if !corpus.trim().is_empty() => {
            tracing::info!(target: "rag", related = sources.len(), "grounding note in related prior notes");
            Some(corpus)
        }
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(target: "rag", error = %e, "related-context retrieval failed (note unaffected)");
            None
        }
    }
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

    // Rebuild full text from segments (same joining as the transcriber), EXCLUDING assistant-
    // directed utterances ("Klaudku, …") so they never reach the action-items extraction.
    let mut full_text = String::new();
    for seg in &segments {
        let t = seg.text.trim();
        if t.is_empty() || crate::audio::wake::is_assistant_directed(t) {
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

    /// The scratch-WAV guard deletes its plaintext file when dropped — the leak fix's core
    /// guarantee, covering the error/early-return paths the old happy-path-only remove missed.
    #[test]
    fn scratch_wav_removed_on_drop() {
        let p = std::env::temp_dir().join(format!(
            "murmur-scratch-drop-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&p, b"RIFF....plaintext-audio-fragment").unwrap();
        assert!(p.exists());
        {
            let _guard = ScratchWav::new(Some(p.clone()));
        } // dropped here
        assert!(!p.exists(), "scratch WAV must be removed when its guard drops");
    }

    /// A `None` scratch guard (no AEC/system WAV produced) and a guard whose file was already
    /// removed must both drop without panicking.
    #[test]
    fn scratch_wav_none_or_missing_is_noop() {
        drop(ScratchWav::new(None));
        let p = std::env::temp_dir().join(format!(
            "murmur-scratch-missing-{}.wav",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        drop(ScratchWav::new(Some(p))); // file doesn't exist — must not panic
    }

    /// Fixed SQLCipher key for the file-backed test DB (NOT a Keychain DEK).
    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn temp_db_path(label: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-pipeline-test-{}-{}-{}.sqlite",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
    }

    /// The no-vault branch of `summarize_and_export`: with no vault configured the note is saved
    /// to the canonical DB (markdown + `exported_path = None`) and the meeting finishes in the
    /// terminal `Summarized` state — NOT `Error`. We exercise the real DB-only tail
    /// (`finalize_note_without_vault`) plus the pre-branch writes `summarize_and_export` performs
    /// before it (the provider call + `AppHandle` emit are not unit-testable, so they're excluded;
    /// everything that determines success-vs-error and what lands in the DB is covered here).
    #[test]
    fn finalize_note_without_vault_saves_note_summarized_not_error() {
        use crate::storage::models::Meeting;

        let db = crate::storage::Db::open_with_key(&temp_db_path("no-vault"), TEST_DEK).unwrap();
        let mid = "m-no-vault";

        // A freshly recorded meeting, mid-pipeline (still Recording, untitled).
        db.insert_meeting(&Meeting {
            id: mid.to_string(),
            started_at: "2026-06-27T09:00:00Z".to_string(),
            ended_at: Some("2026-06-27T09:10:00Z".to_string()),
            title: None,
            duration_s: 600,
            audio_path: Some("/audio/m-no-vault.wav".to_string()),
            status: MeetingStatus::Recording,
            folder_id: None,
        })
        .unwrap();

        // What `summarize_and_export` writes BEFORE the vault branch: status → Summarized and the
        // note markdown upserted with NO exported path (the vault is export-only).
        let markdown = "---\ntitle: Q3 Planning\n---\n# Q3 Planning\n\nBody.";
        db.update_meeting_status(mid, MeetingStatus::Summarized)
            .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: mid.to_string(),
            provider_id: "claude_code".to_string(),
            markdown: markdown.to_string(),
            created_at: "2026-06-27T09:11:00Z".to_string(),
            exported_path: None,
        })
        .unwrap();

        // The no-vault tail: titles the meeting and returns Ok (never the Export error).
        let title = finalize_note_without_vault(&db, mid, markdown, "2026-06-27").unwrap();
        assert_eq!(title, "Q3 Planning");

        // Note is saved to the canonical DB with NO export path.
        let note = db.get_note(mid, "claude_code").unwrap().expect("note saved");
        assert_eq!(note.markdown, markdown);
        assert!(
            note.exported_path.is_none(),
            "no-vault note must have exported_path = None"
        );

        // Meeting is titled and terminal in Summarized — NOT Error.
        let meeting = db.get_meeting(mid).unwrap().expect("meeting exists");
        assert_eq!(meeting.title.as_deref(), Some("Q3 Planning"));
        assert_eq!(
            meeting.status,
            MeetingStatus::Summarized,
            "no-vault recording must finish Summarized, never Error"
        );
        assert_ne!(meeting.status, MeetingStatus::Error);
    }

    // ── Phase 2b: gated semantic indexing on note creation ──────────────────────────────────────

    use crate::storage::models::{Folder, Meeting, NoteRecord as Note};
    use std::collections::HashSet;

    fn seed_meeting_note(db: &crate::storage::Db, mid: &str, folder: Option<&str>) {
        db.insert_meeting(&Meeting {
            id: mid.to_string(),
            started_at: "2026-06-27T09:00:00Z".to_string(),
            ended_at: None,
            title: Some("Budget Sync".to_string()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&Note {
            meeting_id: mid.to_string(),
            provider_id: "claude_code".to_string(),
            markdown: "Budget planning for the next quarter and hiring runway.".to_string(),
            created_at: "2026-06-27T09:05:00Z".to_string(),
            exported_path: None,
        })
        .unwrap();
        db.set_note_folder(mid, folder).unwrap();
    }

    /// Count vec0 rows for a meeting via the GATED public read: if a chunk exists for `mid` AND the
    /// meeting is visible under `unlocked`, the query vector (same text) returns it. Returns true iff
    /// the meeting surfaces in the semantic results.
    fn semantic_finds(
        db: &crate::storage::Db,
        mid: &str,
        unlocked: &HashSet<String>,
    ) -> bool {
        let emb = crate::embed::active_embedder();
        let qv = emb
            .embed(std::slice::from_ref(&"budget planning hiring quarter".to_string()))
            .unwrap();
        let qvec = qv.into_iter().next().unwrap_or_default();
        db.search_semantic_visible(&qvec, 10, unlocked)
            .unwrap()
            .iter()
            .any(|h| h.meeting.id == mid)
    }

    /// ALWAYS-ON (no flag): the pipeline path now builds a related-context corpus for a meeting that
    /// has a VISIBLE related prior note — there is no `augment_notes_with_context` gate anymore.
    #[test]
    fn grounding_context_built_without_flag_for_visible_related_note() {
        let db = crate::storage::Db::open_with_key(&temp_db_path("rag-on"), TEST_DEK).unwrap();
        // The meeting being summarized and a genuinely related, open (visible) prior note.
        seed_meeting_note(&db, "m-self", None);
        seed_meeting_note(&db, "m-prior", None);

        let nothing = HashSet::new();
        let ctx = build_grounding_context(
            &db,
            &nothing,
            "m-self",
            Some("Budget Planning"),
            "Budget planning for the next quarter and hiring runway.",
            "anthropic",
        );

        let corpus = ctx.expect("a visible related prior note must ground the new note (no flag)");
        assert!(corpus.contains("id:m-prior"), "related prior note must be cited: {corpus}");
        // Self-exclusion: the meeting being summarized is never grounded in itself.
        assert!(!corpus.contains("id:m-self"), "a note must never be grounded in itself");
    }

    /// ALWAYS-ON graceful no-op: a fresh vault (no related prior notes) yields `related_context =
    /// None`, so the rendered prompt stays byte-identical to the no-context path — never a panic,
    /// never a failure.
    #[test]
    fn grounding_context_none_for_fresh_vault() {
        let db = crate::storage::Db::open_with_key(&temp_db_path("rag-empty"), TEST_DEK).unwrap();
        // Only the meeting being summarized exists — no prior notes to ground in.
        seed_meeting_note(&db, "m-self", None);

        let nothing = HashSet::new();
        let ctx = build_grounding_context(
            &db,
            &nothing,
            "m-self",
            Some("Budget Planning"),
            "Budget planning for the next quarter and hiring runway.",
            "anthropic",
        );
        assert!(ctx.is_none(), "no related notes ⇒ related_context = None (graceful no-op)");
    }

    /// LOCK INVARIANT under always-on: a sealed-and-NOT-session-unlocked related meeting must
    /// contribute NOTHING to the cloud-bound corpus, and must reappear once its folder is
    /// session-unlocked. The always-on path keeps the visibility gate (`build_related_context`
    /// routes through `search_visible` + `get_note_if_visible`).
    #[test]
    fn grounding_context_excludes_sealed_until_unlocked() {
        let db = crate::storage::Db::open_with_key(&temp_db_path("rag-sealed"), TEST_DEK).unwrap();
        seed_meeting_note(&db, "m-self", None);
        db.insert_folder(&Folder {
            id: "f-lock".to_string(),
            name: "Secret".to_string(),
            path: "Secret".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
        seed_meeting_note(&db, "m-sealed", Some("f-lock"));
        db.set_folder_locked("f-lock", true, None).unwrap();

        let nothing = HashSet::new();
        let sealed_ctx = build_grounding_context(
            &db,
            &nothing,
            "m-self",
            Some("Budget Planning"),
            "Budget planning for the next quarter and hiring runway.",
            "anthropic",
        );
        // Sealed + not unlocked → the related note is absent from the cloud-bound corpus.
        assert!(
            sealed_ctx.map(|c| !c.contains("id:m-sealed")).unwrap_or(true),
            "sealed-not-unlocked related content leaked into the cloud grounding corpus (gate violation)"
        );

        // Session-unlock the folder → the related note is now legitimately available + cited.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let unlocked_ctx = build_grounding_context(
            &db,
            &unlocked,
            "m-self",
            Some("Budget Planning"),
            "Budget planning for the next quarter and hiring runway.",
            "anthropic",
        );
        assert!(
            unlocked_ctx.map(|c| c.contains("id:m-sealed")).unwrap_or(false),
            "unlocked related content must reappear in the grounding corpus"
        );
    }

    /// Flag OFF (the prod-safe default): `index_meeting_if_enabled` must write NOTHING — a
    /// pipeline-equivalent note insert leaves zero chunks, so the semantic read returns nothing.
    #[test]
    fn index_gate_flag_off_writes_no_chunks() {
        let db = crate::storage::Db::open_with_key(&temp_db_path("idx-off"), TEST_DEK).unwrap();
        seed_meeting_note(&db, "m1", None);
        let nothing = HashSet::new();
        let emb = crate::embed::active_embedder();
        // enabled = false → no-op.
        index_meeting_if_enabled(&db, "m1", false, &nothing, emb.as_ref()).unwrap();
        assert!(
            !semantic_finds(&db, "m1", &nothing),
            "flag OFF must not index any chunk (prod-safe default)"
        );
    }

    /// Flag ON + visible meeting: indexing runs on note creation, and the meeting surfaces in the
    /// gated semantic read.
    #[test]
    fn index_gate_flag_on_indexes_visible_meeting() {
        let db = crate::storage::Db::open_with_key(&temp_db_path("idx-on"), TEST_DEK).unwrap();
        seed_meeting_note(&db, "m1", None); // open (no folder) → visible.
        let nothing = HashSet::new();
        let emb = crate::embed::active_embedder();
        index_meeting_if_enabled(&db, "m1", true, &nothing, emb.as_ref()).unwrap();
        assert!(
            semantic_finds(&db, "m1", &nothing),
            "flag ON must index a visible meeting's note"
        );
    }

    /// Flag ON but the meeting is sealed-and-NOT-session-unlocked: the visibility gate stops the
    /// index BEFORE any plaintext is chunked — no chunk row is ever written. Once the folder is
    /// session-unlocked, indexing proceeds and the meeting reappears (mirrors
    /// `vec_semantic_search_is_gated_by_visibility`).
    #[test]
    fn index_gate_skips_sealed_meeting_until_unlocked() {
        let db = crate::storage::Db::open_with_key(&temp_db_path("idx-sealed"), TEST_DEK).unwrap();
        db.insert_folder(&Folder {
            id: "f-lock".to_string(),
            name: "Secret".to_string(),
            path: "Secret".to_string(),
            parent_id: None,
            locked: true,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
        seed_meeting_note(&db, "m1", Some("f-lock"));

        let emb = crate::embed::active_embedder();
        let nothing = HashSet::new();
        // Sealed + not unlocked → index is a no-op (gate), so the semantic read (even under the same
        // empty set) finds nothing.
        index_meeting_if_enabled(&db, "m1", true, &nothing, emb.as_ref()).unwrap();
        assert!(
            !semantic_finds(&db, "m1", &nothing),
            "a sealed-not-unlocked meeting must never be indexed (gate holds)"
        );

        // Session-unlock the folder → indexing now proceeds and the meeting reappears.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        index_meeting_if_enabled(&db, "m1", true, &unlocked, emb.as_ref()).unwrap();
        assert!(
            semantic_finds(&db, "m1", &unlocked),
            "an unlocked folder's meeting must be indexed + visible"
        );
    }
}
