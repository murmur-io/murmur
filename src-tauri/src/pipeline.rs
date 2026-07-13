use std::path::{Path, PathBuf};

use tauri::{AppHandle, Emitter};

use crate::error::{AppError, Result};
use crate::events::{StatusPayload, EVENT_STATUS};
use crate::settings::AppConfig;
use crate::state::AppState;
use crate::storage::models::{MeetingStatus, NoteRecord};
use crate::summarize::provider::{MeetingMeta, SummarizeRequest};
use crate::summarize::roles::Role;
use crate::summarize::{provider_for, template};
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

/// Max allowed gap between the AEC ASR feed and the raw mic, in seconds. Beyond this the AEC feed
/// is rejected (raw mic used for ASR) so the transcript timestamps stay aligned with the played
/// archive. VPIO startup latency is sub-second; the bug this guards (a malformed multi-channel VPIO
/// capture decoding to ~8 s for a 51 s recording → an 8 s timeline over 51 s of audio) is tens of
/// seconds off.
const AEC_FEED_MAX_DRIFT_S: f64 = 2.0;

/// Whether the AEC ASR feed (16 kHz) covers the same span as the raw mic (16 kHz) within tolerance.
/// `false` → the feed is malformed/short (e.g. a 9-channel VPIO device format, or cpal/VPIO
/// coexistence starvation) and using it for ASR would desync the timeline from playback, so the
/// caller MUST fall back to the raw mic. AEC is best-effort; timeline correctness beats echo
/// cancellation.
fn aec_feed_matches_raw(raw_16k_len: usize, aec_16k_len: usize) -> bool {
    let raw_s = raw_16k_len as f64 / audio::TARGET_RATE_HZ as f64;
    let aec_s = aec_16k_len as f64 / audio::TARGET_RATE_HZ as f64;
    (raw_s - aec_s).abs() <= AEC_FEED_MAX_DRIFT_S
}

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
pub(crate) fn audio_dir() -> Result<PathBuf> {
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

    // Bound each whisper decode to a fixed window so the mel-spectrogram + decoder working set stay
    // O(window), not O(recording length). Without this, the VAD-less fallback region (the WHOLE
    // buffer) or a very long continuous speech region hands an hour of audio to whisper.cpp in ONE
    // decode — a full-length mel + a 16384-max-text-ctx allocation (P1 whisper-batch-cap). Each
    // window re-offsets its timestamps exactly like the per-region loop already does; boundaries fall
    // at most every MAX_WINDOW_S (a rare, minor continuity cost only inside a >2-min unbroken region).
    const MAX_WINDOW_S: usize = 120;
    let window_len = MAX_WINDOW_S * crate::audio::TARGET_RATE_HZ as usize;

    let mut out: Vec<crate::transcribe::types::Segment> = Vec::new();
    let mut idx: i64 = 0;
    for (start, end) in regions {
        for (win_start, win_end) in decode_windows(start, end, window_len) {
            let offset_s = win_start as f64 / crate::audio::TARGET_RATE_HZ as f64;
            let tx = transcriber.transcribe_with(
                &samples_16k[win_start..win_end],
                lang,
                TranscribeQuality::Accurate,
            )?;
            for mut seg in tx.segments {
                seg.idx = idx;
                seg.start_s += offset_s;
                seg.end_s += offset_s;
                idx += 1;
                out.push(seg);
            }
        }
    }
    Ok(out)
}

/// Split a `[start, end)` sample range into fixed-length decode windows of at most `window_len`
/// samples, so a single whisper decode never spans more than one window (bounds whisper's mel +
/// decoder working memory to O(window), not O(recording length)). Windows tile the range with no
/// gaps or overlaps; the final window is whatever remains. An empty/degenerate range yields no
/// windows. Pure + deterministic, so the windowing contract is unit-tested without a whisper model.
fn decode_windows(start: usize, end: usize, window_len: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if end <= start {
        return out;
    }
    if window_len == 0 {
        out.push((start, end));
        return out;
    }
    let mut s = start;
    while s < end {
        let e = (s + window_len).min(end);
        out.push((s, e));
        s = e;
    }
    out
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
    // P1 Stop-time peak: in the common case (no hi-res masters — the default) the source-rate mic
    // buffer (~0.7 GB/hr at 48 kHz) is never needed again, so free it NOW — before the 16k AEC / mix /
    // transcription buffers allocate — instead of holding it to the end of `run_inner`. This roughly
    // halves the pre-whisper Stop-time peak (source + mic_16k + sys_16k + archive_16k → just the 16k
    // set). When masters ARE kept we must retain it for the faithful PRE-resample `.mic.wav` written
    // after finalize (below), so keep it only in that case.
    let samples: Option<Vec<f32>> = if config.keep_hires_masters {
        Some(samples)
    } else {
        drop(samples);
        None
    };
    // AEC'd mic for the ASR feed (rec #5): when the VPIO helper produced a WAV, transcribe THAT and
    // keep the RAW cpal mic for the archive (`mic_16k_archive`); otherwise archive == ASR (today).
    // `mem::replace` avoids cloning the (large) mic buffer in the common no-AEC path.
    let mut mic_16k_archive: Option<Vec<f32>> = None;
    if let Some(aec) = aec_scratch.path() {
        match audio::read_wav_mono(aec) {
            Ok((a, rate)) => {
                let asr = audio::resample_to_16k(&a, rate)?;
                // GUARD: the AEC feed MUST span the same wall-clock as the raw mic, or the segment
                // timestamps (derived from the AEC feed) desync from the played archive (the raw
                // mic) — the bug where a 51 s recording rendered an 8 s timeline. VPIO yielded a
                // malformed multi-channel/short capture; reject any divergent feed and keep the raw
                // mic for ASR. The matching `mic_16k_archive = None` means archive == ASR feed.
                if aec_feed_matches_raw(mic_16k.len(), asr.len()) {
                    mic_16k_archive = Some(std::mem::replace(&mut mic_16k, asr));
                } else {
                    tracing::warn!(
                        target: "audio",
                        raw_s = mic_16k.len() as f64 / audio::TARGET_RATE_HZ as f64,
                        aec_s = asr.len() as f64 / audio::TARGET_RATE_HZ as f64,
                        "AEC feed duration diverges from raw mic; using raw mic for ASR to keep the timeline in sync with playback"
                    );
                }
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

    // Measure the mic↔system offset + speaker-leak strength ONCE, on the RAW mic (never an
    // AEC'd feed — the leak only exists in the raw capture). Drives BOTH the aligned archive
    // mix below and the echo dedup after transcription. Best-effort: None ⇒ wall-clock pads
    // and strict-tier-only dedup (today's behaviour, minus the echo). The scoped borrow of the
    // mic buffer ends here, before `mic_16k`/`sys_16k` are moved into the transcription task.
    let leak: Option<audio::align::EchoLeak> = {
        let raw_probe: &[f32] = mic_16k_archive.as_ref().unwrap_or(&mic_16k);
        sys_16k
            .as_ref()
            .and_then(|sys| audio::align::estimate_stream_offset(raw_probe, sys))
    };
    if let Some(l) = &leak {
        tracing::info!(
            target: "audio",
            offset_s = l.offset_s,
            correlation = l.correlation,
            "mic/system offset measured (speaker-leak evidence)"
        );
    }

    // Post-hoc AEC (on-device, offline): cancel the system-audio reference out of the RAW mic.
    // Runs only with a system stream, the flag on, and a measured leak (headphones ⇒ no echo
    // energy ⇒ skip entirely). On success the AEC'd buffer becomes BOTH the ASR feed and the
    // archive-mix input (feed == archive by construction, so the 51 s→8 s timeline-desync class
    // is impossible). NOTE: this makes the AEC'd mic the ONLY mic audio in the playback archive;
    // the faithful RAW mic survives at rest ONLY when `keep_hires_masters` is on (the `.mic.wav`
    // master, written from the pre-resample `samples` below). Best-effort: any anomaly keeps raw.
    if config.post_aec_enabled {
        if let (Some(sys), Some(l)) = (sys_16k.as_ref(), leak.as_ref()) {
            let sys_lead = (l.offset_s.max(0.0) * audio::TARGET_RATE_HZ as f64).round() as usize;
            let raw_len = mic_16k_archive.as_ref().unwrap_or(&mic_16k).len();
            let aec_result = {
                let raw_mic: &[f32] = mic_16k_archive.as_ref().unwrap_or(&mic_16k);
                audio::aec_offline::cancel_echo_offline(raw_mic, sys, sys_lead)
            };
            match aec_result {
                Ok(clean) if clean.len() == raw_len => {
                    mic_16k = clean;
                    mic_16k_archive = None; // feed and archive now share the AEC'd buffer
                    tracing::info!(target: "audio", "offline AEC applied to the mic track");
                }
                Ok(_) => {
                    tracing::warn!(target: "audio", "offline AEC length mismatch; raw mic kept")
                }
                Err(e) => {
                    tracing::warn!(target: "audio", error = %e, "offline AEC failed; raw mic kept")
                }
            }
        }
    }

    // Archive WAV = the MIX (for playback only). Mic-only when there's no system stream.
    // `archive_src` = the AEC'd mic when offline AEC ran above (mic_16k_archive was cleared to
    // None), else the raw cpal mic — mixed with system audio, offset-aligned so the two streams
    // line up on the wall clock (kills most of the audible double-hearing on speakers).
    let archive_src = mic_16k_archive.as_ref().unwrap_or(&mic_16k);
    let archive_16k = match &sys_16k {
        Some(sys) => {
            let (mic_delay, sys_delay) = audio::align::archive_delays(
                leak.as_ref(),
                mic_started_at,
                system_started_at,
                audio::TARGET_RATE_HZ,
            );
            tracing::info!(
                target: "audio",
                mic_delay,
                sys_delay,
                "archiving mixed mic + system-audio track (offset-aligned)"
            );
            audio::mix_aligned(archive_src, mic_delay, sys, sys_delay)
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
        // `samples` is retained as `Some` exactly when keep_hires_masters is on (see the resample
        // site, where it is dropped early otherwise). Written from the PRE-resample buffer so it
        // stays faithful; path persistence + error handling are byte-identical to before.
        if let Some(samples) = &samples {
            let mic_master = wav_dir.join(format!("{meeting_id}.mic.wav"));
            if let Err(e) = audio::write_wav_f32(&mic_master, samples, src_rate, 1) {
                tracing::warn!(target: "audio", error = %e, "mic master write failed");
            } else if let Err(e) = state.db.set_meeting_mic_master_path(
                meeting_id,
                Some(mic_master.to_string_lossy().as_ref()),
            ) {
                // Best-effort: a stranded, untracked master plaintext is unreferenced + ungated-out;
                // never fail the recording over it.
                tracing::warn!(target: "audio", error = %e, "persisting mic master path failed");
            }
        }
        if let Some((sys, sys_rate)) = &sys_native {
            let sys_master = wav_dir.join(format!("{meeting_id}.sys.wav"));
            if let Err(e) = audio::write_wav_f32(&sys_master, sys, *sys_rate, 1) {
                tracing::warn!(target: "audio", error = %e, "system master write failed");
            } else if let Err(e) = state.db.set_meeting_sys_master_path(
                meeting_id,
                Some(sys_master.to_string_lossy().as_ref()),
            ) {
                tracing::warn!(target: "audio", error = %e, "persisting system master path failed");
            }
        }
    }

    // Storage retention (opt-in): if the user set a cap + enabled auto-prune, delete the
    // OLDEST recordings' audio to stay under it — never THIS recording (excluded), never a
    // locked folder's, never notes/transcripts. Best-effort: a prune error never fails the
    // recording.
    let prune_result = {
        // Hold the seal lifecycle guard across the SYNCHRONOUS prune ONLY — a std Mutex guard must
        // never be held across an `.await`, so it is scoped to this block and dropped before the
        // next await below. Same guard `lock_folder`/`unlock_folder`/`relock_*` hold, so the prune
        // can never interleave with a folder seal (poison-tolerant, like `commands::lifecycle_guard`).
        let _lifecycle = state
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::storage::usage::maybe_prune(
            &state.db,
            &wav_dir,
            config.audio_storage_limit_gb,
            config.audio_auto_prune,
            Some(meeting_id),
        )
    };
    match prune_result {
        Ok(s) if s.freed_bytes > 0 => {
            tracing::info!(target: "storage", freed = s.freed_bytes, count = s.pruned_count, "auto-pruned old recordings to stay under the storage cap");
            let _ = app.emit(
                crate::events::EVENT_STORAGE_PRUNED,
                crate::events::StoragePrunedPayload {
                    freed_bytes: s.freed_bytes,
                    pruned_count: s.pruned_count,
                },
            );
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(target: "storage", error = %e, "auto-prune failed (non-fatal)"),
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
    let diarize_models: Option<(std::path::PathBuf, std::path::PathBuf)> = if config.diarize_others
        && has_system
    {
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

    // VOICEPRINTS (opt-in, default OFF): capture a per-cluster voice biometric for the diarized
    // "others" clusters, but ONLY when diarization is on AND the explicit `voiceprint_enabled` flag is
    // set. The extractor loads from the SAME CAM++ embedding model the diarizer used; keep a clone of
    // that path so the closure can build a standalone extractor after diarizing. PRIVACY: these
    // embeddings are stored on-device (SQLCipher, folder-lock-sealed, purged on seal) and NEVER
    // egressed. Capturing a non-consenting participant's voiceprint is why this is an explicit opt-in.
    let voiceprint_enabled = config.voiceprint_enabled && config.diarize_others;
    let voiceprint_emb_path: Option<std::path::PathBuf> = if voiceprint_enabled {
        diarize_models.as_ref().map(|(_seg, emb)| emb.clone())
    } else {
        None
    };

    // Routed through the shared heavy-inference gate (perf::run_heavy), not a bare spawn_blocking
    // — this closure loads Whisper AND (further in) the diarizer, so it must serialize against any
    // OTHER heavy native-runtime call (embedder, NER, brain sidecar) running concurrently, not just
    // against itself.
    //
    // 2026-07-13 — per-stage wall-clock telemetry: a user reported a ~15min recording taking
    // ~15min end-to-end (Stop → note ready) and force-quit thinking the app hung. Diagnosis
    // couldn't pin the dominant cost without real numbers from the affected machine (candidates:
    // Accurate-profile whisper decode of BOTH streams, diarization, note-gen, brain-sidecar fact
    // extraction, embedding — all now strictly sequential through this same closure + the
    // downstream summarize_and_export call). Logging elapsed_ms per stage (never audio/transcript
    // content) so the NEXT report comes with a real breakdown instead of another guess.
    let asr_diarize_started = std::time::Instant::now();
    let (merged_segments, echo_suppressed, cluster_voiceprints) = crate::perf::run_heavy(&state.heavy_inference, move || -> Result<(Vec<crate::transcribe::types::Segment>, usize, Vec<crate::transcribe::diarize::ClusterVoiceprint>)> {
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
        // NOTE: the diarizer is loaded further below, lazily, right before `d.diarize(&sys)` — NOT
        // here. Loading it this early meant it sat resident through BOTH stream transcriptions, and
        // (before the drop() below existed) Whisper stayed resident through diarization too — two
        // heavy ML runtimes (whisper.cpp + sherpa-onnx/Pyannote) peaking RAM/CPU simultaneously
        // (2026-07-13 RCA: matched a reported system freeze + macOS cpu_resource.diag hotspots
        // through Diarizer::diarize). Their ASR/diarize work is inherently sequential — deferring
        // the diarizer's load until Whisper is actually dropped costs nothing.

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

        // Per-cluster voiceprints computed inside the blocking closure (where the system samples +
        // spans live); persisted after the closure with `state.db`. Empty unless voiceprints are on.
        let mut cluster_voiceprints: Vec<crate::transcribe::diarize::ClusterVoiceprint> = Vec::new();

        if let (Some(sys), Some(sys_started)) = (sys_16k, system_started_at) {
            let mut sys_segments =
                transcribe_stream(&transcriber, vad.as_mut(), &sys, lang_owned.as_deref())?;

            // ASR is done for both streams — free Whisper (+ VAD, also unused from here on) BEFORE
            // loading the diarizer, so the two heavy ML runtimes are never resident together.
            drop(transcriber);
            drop(vad);

            // Diarizer (best-effort): loaded HERE, only once Whisper has been released, and only
            // when there's actually a system stream to diarize.
            let diarizer = diarize_models.and_then(|(seg, emb)| {
                match crate::transcribe::diarize::Diarizer::load(&seg, &emb) {
                    Ok(d) => Some(d),
                    Err(e) => {
                        tracing::warn!(target: "transcribe", error = %e, "diarizer load failed; single 'others' label");
                        None
                    }
                }
            });

            // N-way diarization on the "others" stream ONLY — relabel segments to others-0/1/2.
            if let Some(d) = &diarizer {
                match d.diarize(&sys) {
                    Ok(spans) => {
                        crate::transcribe::diarize::relabel_others(&mut sys_segments, &spans);
                        // VOICEPRINTS (opt-in): compute one L2-normalized CAM++ embedding per distinct
                        // cluster from the SAME system samples fed to the diarizer, at the diarizer's
                        // sample rate. Best-effort: any sherpa failure → empty (labels unaffected).
                        if let Some(emb_path) = &voiceprint_emb_path {
                            cluster_voiceprints =
                                crate::transcribe::diarize::compute_cluster_voiceprints(
                                    emb_path,
                                    &sys,
                                    &spans,
                                    d.sample_rate(),
                                );
                        }
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
        // merge sorted by absolute start, drop empty (e.g. muted-mic) segments, label "me"/"others" —
        // then drop mic-echo copies of others' speech (speakers → mic bleed; leak-gated). `leak` is
        // Copy, captured by this move closure.
        let (merged, echo_suppressed) =
            crate::audio::merge::suppress_cross_stream_echo(merge_streams(streams), leak.as_ref());
        Ok((merged, echo_suppressed, cluster_voiceprints))
    })
    .await?;
    tracing::info!(
        target: "perf",
        stage = "asr_diarize",
        elapsed_ms = asr_diarize_started.elapsed().as_millis() as u64,
        duration_s,
        "pipeline stage complete"
    );

    // Persist the (opt-in) per-cluster voiceprints — one row per diarized "others" cluster, label
    // NULL until enrolled by rename. Best-effort: a persist failure is logged (no PII) and never
    // fails the pipeline (the transcript + labels are already the source of truth). The rows are
    // provenance-anchored to this meeting → gated on read + purged on seal.
    if !cluster_voiceprints.is_empty() {
        let now = chrono::Utc::now().to_rfc3339();
        for vp in &cluster_voiceprints {
            let id = uuid::Uuid::new_v4().to_string();
            if let Err(e) = state.db.insert_voiceprint(
                &id,
                meeting_id,
                vp.cluster_index as i64,
                None,
                &vp.embedding,
                &now,
            ) {
                tracing::warn!(
                    target: "transcribe", error = %e,
                    "persisting a voiceprint failed (diarization/labels unaffected)"
                );
            }
        }
        tracing::info!(
            target: "transcribe",
            clusters = cluster_voiceprints.len(),
            "captured per-cluster voiceprints"
        );
    }

    if has_system {
        tracing::info!(target: "transcribe", segments = merged_segments.len(), "merged mic + system streams (me/others)");
    }

    state.db.insert_segments(meeting_id, &merged_segments)?;
    if echo_suppressed > 0 {
        tracing::info!(target: "transcribe", suppressed = echo_suppressed, "cross-stream echo segments removed");
        let _ = app.emit(
            crate::events::EVENT_ECHO_SUPPRESSED,
            crate::events::EchoSuppressedPayload {
                suppressed: echo_suppressed,
                meeting_id: meeting_id.to_string(),
            },
        );
    }
    state
        .db
        .update_meeting_status(meeting_id, MeetingStatus::Transcribed)?;

    // Rebuild the transcript from the merged, time-ordered segments — EXCLUDING segments the user
    // spoke TO the assistant ("Klaudku, sprawdź pogodę"). Those are assistant commands, not meeting
    // content: fed to the summarizer they get mangled into owner-less action items. The assistant's
    // ANSWER lives in the persisted Q&A log (assistant_interactions), not the note.
    // TIER 0: `build_transcript_feed` yields a SPEAKER-LABELED `summary_text` (`[start-end] (speaker)
    // text`, the exact shape the timeline already consumes) when the meeting has ≥2 distinct speakers
    // — so the note can attribute owners/decisions — and stays byte-identical flat text for the
    // default solo-`me` meeting. `retrieval_text` is always the flat join (RAG query unchanged).
    let feed = build_transcript_feed(&merged_segments);

    if feed.summary_text.trim().is_empty() {
        return Err(AppError::Transcribe(
            "No speech detected in the recording — nothing to transcribe. \
             Check your microphone input and try recording again."
                .into(),
        ));
    }

    // ── 4 + 5. Summarize with the configured provider, then export ───────────
    // NOTE: no config is passed — the summarize step re-reads the LIVE config (consent may have
    // been revoked during the minutes of transcription above; see `resolve_summarize_egress`).
    let summarize_started = std::time::Instant::now();
    let result = summarize_and_export(
        app,
        state,
        meeting_id,
        &feed,
        lang.map(str::to_string),
        duration_s,
        &date_iso,
        &ended_at,
    )
    .await;
    tracing::info!(
        target: "perf",
        stage = "summarize_and_export",
        elapsed_ms = summarize_started.elapsed().as_millis() as u64,
        ok = result.is_ok(),
        "pipeline stage complete"
    );
    result
}

/// TIER 0 — the summarizer's transcript, in two forms.
///
/// The pipeline computes diarized, timestamped segments but historically fed the summarizer a flat,
/// speaker-stripped wall of text — so the note (the one artifact every user reads) could never
/// attribute owners/decisions even when system-audio capture labeled the far side. `summary_text`
/// fixes that: for a meeting with ≥2 DISTINCT speakers it is the `[start-end] (speaker) text` shape
/// the timeline already uses ([`crate::summarize::timeline`]), so the model can attribute. For the
/// default solo-`me` meeting (one distinct speaker) it stays the flat join — BYTE-IDENTICAL to the
/// old note input. `retrieval_text` is ALWAYS the flat join: it feeds only the RAG salient-query
/// path (`orchestrate_context`), which must not be perturbed by tag tokens.
///
/// The labeled `summary_text` becomes `SummarizeRequest.transcript`, so any real NAME that ever
/// occupies a `(speaker)` tag is scrubbed by the SAME RedactingProvider/DebertaNameRedactor firewall
/// as the rest of the transcript before any cloud egress — it rides no side channel.
pub(crate) struct TranscriptFeed {
    /// Flat text (assistant-directed segments dropped) for retrieval — byte-identical to the legacy join.
    /// `#[allow(dead_code)]`: Phase 1 (two-stage notes) removed the pre-generation cross-meeting
    /// grounding injection (`orchestrate_context`) that was this field's only PRODUCTION consumer, so
    /// the note is now generated from `summary_text` alone. The flat-join field is retained as the
    /// canonical retrieval text (its no-regression properties are still pinned by the feed tests, and
    /// it is the seam any future/optional grounding re-wire keys off) — not dead history.
    #[allow(dead_code)]
    pub retrieval_text: String,
    /// What the model summarizes: speaker-labeled when `labeled`, else identical to `retrieval_text`.
    pub summary_text: String,
    /// Whether `summary_text` carries `(speaker)` tags (i.e. ≥2 distinct speakers).
    pub labeled: bool,
}

/// Build the [`TranscriptFeed`] from the merged, time-ordered segments. Drops empty and
/// assistant-directed ("Klaudku, …") segments with the IDENTICAL predicate the flat loops used.
pub(crate) fn build_transcript_feed(
    segments: &[crate::transcribe::types::Segment],
) -> TranscriptFeed {
    use crate::audio::merge::SPEAKER_ME;
    let kept: Vec<&crate::transcribe::types::Segment> = segments
        .iter()
        .filter(|s| {
            let t = s.text.trim();
            !t.is_empty() && !crate::audio::wake::is_assistant_directed(t)
        })
        .collect();

    let mut retrieval_text = String::new();
    for s in &kept {
        if !retrieval_text.is_empty() {
            retrieval_text.push(' ');
        }
        retrieval_text.push_str(s.text.trim());
    }

    // ≥2 distinct speaker labels (None counts as `me`, the mic stream) ⇒ worth tagging. A default
    // mono-`me` meeting has exactly one distinct label ⇒ stays flat (byte-identical to before).
    let distinct: std::collections::HashSet<&str> = kept
        .iter()
        .map(|s| s.speaker.as_deref().unwrap_or(SPEAKER_ME))
        .collect();
    let labeled = distinct.len() >= 2;

    // Tier 3b/A3 — PREVENTIVE [UNCLEAR] MARKING. Prefix an acoustically-shaky segment (ASR
    // `confidence < LOW_CONFIDENCE_P`) so the summarizer is TOLD which spans are garbled and does not
    // confidently mint an action item from bad audio. NON-ACTIVATING BY DEFAULT: `confidence` is only
    // computed on the Accurate batch path with a real model — on CI / model-less installs every
    // segment is `None` ⇒ no prefix ⇒ `summary_text` is BYTE-IDENTICAL to before (the mono-`me`
    // default egress is unchanged). The marker is a fixed on-device token (no PII) and rides
    // `req.transcript` through the SAME RedactingProvider firewall as the rest of the feed — no new
    // egress class, no side channel. `retrieval_text` is NEVER prefixed (retrieval stays flat).
    let unclear = |s: &crate::transcribe::types::Segment| -> &'static str {
        if s.confidence
            .map(|c| c < crate::summarize::grounding::LOW_CONFIDENCE_P)
            .unwrap_or(false)
        {
            "[UNCLEAR] "
        } else {
            ""
        }
    };

    let summary_text = if labeled {
        // The line shape is IDENTICAL to summarize::timeline's feed so the model reads one convention.
        kept.iter()
            .map(|s| {
                format!(
                    "[{:.1}-{:.1}] ({}) {}{}",
                    s.start_s,
                    s.end_s,
                    s.speaker.as_deref().unwrap_or(SPEAKER_ME),
                    unclear(s),
                    s.text.trim()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        // Flat join, but with the [UNCLEAR] prefix on shaky segments. All-`None` ⇒ every prefix is
        // "" ⇒ this equals `retrieval_text` exactly (the single-speaker byte-identical guarantee).
        kept.iter()
            .map(|s| format!("{}{}", unclear(s), s.text.trim()))
            .collect::<Vec<_>>()
            .join(" ")
    };

    TranscriptFeed {
        retrieval_text,
        summary_text,
        labeled,
    }
}

/// Resolve the summarize step's (config, NOTES provider) from the LIVE state — the egress
/// decision of the pipeline.
///
/// STALE-CONSENT LEAK FIX: `run_inner` snapshots the config at Stop and then transcribes for
/// potentially MINUTES before summarizing. A cloud-egress consent REVOKE (`revoke_cloud_egress`)
/// or provider switch landing in that window MUST be honored, so the config is RE-READ here, at
/// the moment of egress — never taken from the caller. `summarize_and_export` deliberately has NO
/// config parameter anymore: the only config in its scope is this fresh snapshot, so feeding the
/// fail-closed `provider_for` gate a stale `cloud_egress_consented` is unrepresentable.
/// Regression: `summarize_egress_resolution_honors_revoke_landed_after_stop_snapshot`.
fn resolve_summarize_egress(
    state: &AppState,
) -> Result<(
    AppConfig,
    std::sync::Arc<dyn crate::summarize::provider::SummarizerProvider>,
)> {
    let config: AppConfig = {
        let guard = state
            .config
            .lock()
            .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
        guard.clone()
    };
    let provider = provider_for(Role::Notes, &config, &state.heavy_inference)?;
    Ok((config, provider))
}

/// Content-FREE per-note PRIVACY RECEIPT facts (Tier 4c), derived purely from in-hand egress
/// metadata at note-generation time. All fields are booleans / non-PII destination labels /
/// integer counts — NEVER note text, transcript, attendee names, titles, keys, or DEK/KEK/CK.
///
/// This is a plain self-declared record, NOT a cryptographic attestation and NOT a
/// verifiable/provable claim — it is exactly as trustworthy as the app that wrote it. Its value is
/// that a local-only summary can state, in one screenshot-able front-matter line, that nothing left
/// the device.
pub(crate) struct PrivacyReceiptFacts {
    /// `true` when nothing left the device to produce this note (a loopback-ollama / on-device
    /// summary) — the honest `privacy-cloud-calls: 0` headline. Reuses [`egress_is_cloud`], the
    /// SAME classifier the consent gate enforces, so it can never under-claim "local" (a REMOTE
    /// `ollama_base_url` correctly classifies as cloud).
    ///
    /// [`egress_is_cloud`]: crate::summarize::egress_is_cloud
    pub local_only: bool,
    /// Non-PII destination LABEL for a cloud summary (`api.anthropic.com`, a gateway `host:port`,
    /// …); `None` for a local summary (nothing left the device to declare).
    pub egress_host: Option<String>,
    /// The redaction firewall's REAL scrub total for THIS summary call. `None` for a local provider
    /// (it returns unwrapped — no firewall ran), so no `privacy-pii-redacted` key is stamped.
    pub redacted_pii: Option<u32>,
}

/// Compute the [`PrivacyReceiptFacts`] for a just-generated note. Pure: no I/O, no state, no
/// egress-ledger query (the ledger's `meeting_id` is always `None` → not per-note attributable).
///
/// The `egress_host` labels MIRROR `make_provider_resolved`'s own `destination` match (literal
/// constants, no drift risk); a numeric cloud-CALL count is deliberately NOT reported (would
/// under-count, since entity-extraction / auto-organize also call the cloud — the dangerous
/// direction for a privacy claim), so the honest cloud receipt is the host + the PII count.
pub(crate) fn privacy_receipt_facts(
    connection: &str,
    config: &AppConfig,
    gateway_host: Option<&str>,
    call_meta: &crate::summarize::meta::CallMeta,
) -> PrivacyReceiptFacts {
    let local_only = !crate::summarize::egress_is_cloud(connection, config);
    let egress_host: Option<String> = if local_only {
        None
    } else {
        match connection {
            crate::summarize::PROVIDER_CLAUDE_CODE => {
                Some("claude_code (Anthropic CLI)".to_string())
            }
            crate::summarize::PROVIDER_ANTHROPIC => Some("api.anthropic.com".to_string()),
            crate::summarize::PROVIDER_GATEWAY => gateway_host.map(str::to_string),
            crate::summarize::PROVIDER_OLLAMA => reqwest::Url::parse(&config.ollama_base_url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string)),
            other => Some(other.to_string()),
        }
    };
    let redacted_pii = call_meta
        .redactions
        .as_ref()
        .map(|r| r.email + r.card + r.phone + r.name);
    PrivacyReceiptFacts {
        local_only,
        egress_host,
        redacted_pii,
    }
}

/// Summarize a transcript with the configured provider, persist the note to the canonical
/// DB, and — when a vault folder is configured — export it to the Obsidian vault and update
/// the meeting status/title. When NO vault is configured the note is still fully saved
/// (DB + title), `exported_path` is `None`, and the meeting finishes in `Summarized` (NOT
/// `Error`): the vault is export-only. Shared by the full pipeline and `resummarize_existing`.
/// The provider/consent config is re-read from the live state at entry (see
/// [`resolve_summarize_egress`]) — callers must NOT pass their own snapshot.
#[allow(clippy::too_many_arguments)]
async fn summarize_and_export(
    app: &AppHandle,
    state: &AppState,
    meeting_id: &str,
    feed: &TranscriptFeed,
    language: Option<String>,
    duration_s: i64,
    date_iso: &str,
    when_iso: &str,
) -> Result<PipelineResult> {
    // Egress gate FIRST, on the LIVE config: a consent revoke that landed during transcription
    // refuses here (fail-closed), before any status/corpus work. The provider is pure
    // construction (no I/O), and every config read below uses this same fresh snapshot.
    let (config, provider) = resolve_summarize_egress(state)?;
    let config = &config;

    // The NOTES-role provider target — with role keys absent this is EXACTLY the legacy
    // (provider_id, provider_model, provider_effort) triple, so every use below is byte-identical
    // to the pre-role code. Resolved ONCE so the status line, the corpus budget, and the
    // provenance row all name the SAME connection the summarize call actually uses.
    let notes_target = crate::summarize::roles::provider_target(Role::Notes, config);
    emit_status(
        app,
        "summarizing",
        &format!(
            "Summarizing with {}…",
            crate::summarize::roles::connection_display_name(&notes_target.connection)
        ),
        meeting_id,
    );

    let vault_titles = match config.vault_path.as_deref() {
        Some(p) if !p.is_empty() => export::list_vault_titles(Path::new(p)).unwrap_or_default(),
        _ => Vec::new(),
    };

    // STAGE 1 (two-stage note redesign — Phase 1). The note is generated PROVABLY from ONLY this
    // meeting's transcript + the user's typed notes: NO cross-meeting context is injected into the
    // generation prompt (`related_context: None` below). This eliminates the cross-meeting `## Action
    // items` bleed class BY CONSTRUCTION — the weak on-device model can no longer be handed another
    // meeting's tasks in its one prompt. The retrieval machinery stays in the tree UNTOUCHED for
    // Phase 2 (a deferred, additive, link-only post-pass on the FINISHED note):
    // `orchestrate::orchestrate_context`, `build_grounding_context`, and everything in
    // `related_context.rs` are retained, just not called here. `render_user_content` for
    // `related_context: None` is byte-identical to the pre-injection prompt (test
    // `render_user_content_none_is_unchanged`), so Stage 1's prompt carries only transcript +
    // user_notes + vault_titles. See docs/research/2026-07-06-note-and-brain-architecture.md
    // (§3 Stage 1, §8 Phase 1).

    // ENHANCE-MY-NOTES: fetch the typed-notes buffer BEFORE building the request — in
    // "enhance" mode the notes ride INSIDE the prompt as the skeleton (a NEW, deliberate,
    // REDACTED egress of user-typed content — see summarize/redact.rs); in "append" mode
    // (or with an empty buffer) they stay out of the prompt exactly as before. The buffer
    // read is ungated by design here (the pipeline is the producer of the note plaintext;
    // resummarize is gated upstream by meeting_is_unlocked).
    let manual_notes = state.db.get_manual_notes(meeting_id).unwrap_or_default();

    // Brain v2 L4 — the RUNNING LIVE BULLETS captured during the recording (the crash-recovery
    // `live_bullets` row `transcribe::bullets::bullets_tick` maintained; the RAM buffer was
    // already cleared at Stop). PRODUCER-side read like `manual_notes` above (ungated by design
    // here — the pipeline produces this meeting's own note plaintext; a sealed meeting has no row
    // anyway, purge-on-seal). Present ⇒ rendered as the "Live notes (auto)" section before the
    // transcript (riding the RedactingProvider firewall like every other prompt field) and
    // CONSUMED (row cleared) after the note persists below; absent (bullets off / stub /
    // resummarize after the consume) ⇒ the prompt is byte-identical to before.
    let live_bullets = state
        .db
        .get_live_bullets(meeting_id)
        .unwrap_or_default()
        .filter(|b| !b.trim().is_empty());

    let request = SummarizeRequest {
        // TIER 0: the SPEAKER-LABELED transcript (or the flat one for a solo-`me` meeting). It rides
        // `req.transcript`, so the RedactingProvider firewall scrubs any name in a `(speaker)` tag.
        transcript: feed.summary_text.clone(),
        meta: MeetingMeta {
            date_iso: date_iso.to_string(),
            title_hint: None,
            duration_s,
            language,
        },
        template: template::build_template(&config.note_style, &config.note_language, feed.labeled),
        vault_titles,
        // STAGE 1 — no cross-meeting context in the generation prompt (Phase 1). Phase 2 will add
        // links additively on the finished note, not via this field.
        related_context: None,
        user_notes: if config.notes_mode == "enhance" && !manual_notes.trim().is_empty() {
            Some(manual_notes.clone())
        } else {
            None
        },
        live_bullets: live_bullets.clone(),
    };

    let (generated, call_meta) = provider.summarize_with_meta(&request).await?;

    // brain2 realtime notes FINALIZE: `finalize_note_markdown` either stamps the enhance
    // marker (notes were in the prompt) or appends `## My notes` verbatim (append mode).
    // The `manual_notes` buffer stays the DURABLE CANONICAL store — never blanked here, so
    // every (re)summarize re-reads it fresh in EITHER mode; empty buffer ⇒ byte-identical.
    let markdown = finalize_note_markdown(&generated, &manual_notes, &config.notes_mode);

    // GROUNDING reads THIS meeting's OWN segments (the SAME plaintext the pipeline just summarized) —
    // NO new read path, NO cross-meeting read, NO egress (pure local string ops). A `get_segments`
    // failure degrades to no segments ⇒ the opt-in grounding pass below returns the note
    // byte-identical; it NEVER fails the pipeline. Runs BEFORE `upsert_note` + the vault write so the
    // DB copy and the exported `.md` carry the same grounded body.
    //
    // NOTE (Phase 1): the always-on lexical anti-bleed pass (`strip_ungrounded_action_items`, gated by
    // `strip_applies_for_language`) that used to run here has been DELETED. It existed ONLY to clean up
    // the cross-meeting injection removed above (Stage 1); with the note now transcript-only, the bleed
    // class is gone by construction and the lexical deletion (which risked removing real generated
    // content) is dead weight.
    // Tier 3b (B) — DETERMINISTIC GROUNDING (anti-hallucination). Annotate summary units this
    // meeting's OWN transcript does not support with a non-destructive `> unverified` line (or
    // `(low audio confidence)` when the overlap was acoustically shaky). Gated by `ground_summary`
    // (default OFF / opt-in until the overlap thresholds are calibrated on real data — an over-flag
    // would `> unverified` a legitimately-abstractive sentence in the note 100% of users read).
    // The markers become part of the note markdown and are sealed WITH it. The meeting's own
    // segments are fetched ONLY on this opt-in path (the default OFF path does no wasted DB read).
    let markdown = if config.ground_summary {
        let segments = state.db.get_segments(meeting_id).unwrap_or_default();
        crate::summarize::grounding::annotate_unverified(&markdown, &segments)
    } else {
        markdown
    };

    state
        .db
        .update_meeting_status(meeting_id, MeetingStatus::Summarized)?;

    let created_at = chrono::Utc::now().to_rfc3339();
    // Phase 5 — model provenance: capture the requested model ID and (when available) the model
    // actually served by the gateway/API. `model_requested` is the resolved EFFECTIVE model —
    // shared source-of-truth with the egress ledger (`effective_model_requested`), which also
    // fixes the anthropic gap: with `provider_model` empty the request carries `anthropic_model`,
    // and that is now what gets recorded (previously None). `gateway_host` is present ONLY for the
    // `gateway` provider so it's non-PII and identifies the endpoint (not the content). All three
    // are optional and omitted for providers that don't return model metadata (the default
    // `*_with_meta` falls back to empty `CallMeta`).
    let model_requested = {
        let m = crate::summarize::effective_model_requested(&notes_target, config);
        let m = m.trim().to_string();
        // A "" stays None so legacy notes don't get a blank string.
        if m.is_empty() {
            None
        } else {
            Some(m)
        }
    };
    let gateway_host = if notes_target.connection == crate::summarize::PROVIDER_GATEWAY {
        reqwest::Url::parse(&config.gateway_base_url)
            .ok()
            .and_then(|u| {
                u.host_str().map(|h| {
                    if let Some(port) = u.port() {
                        format!("{h}:{port}")
                    } else {
                        h.to_string()
                    }
                })
            })
    } else {
        None
    };
    // Save clones for frontmatter injection (the NoteRecord takes ownership below).
    let model_served_for_fm = call_meta.model_served.clone();
    let model_requested_for_fm = model_requested.clone();
    // Provenance names the RESOLVED connection that actually served the note (identical to
    // `provider_id` while role keys are absent).
    let provider_id_for_fm = notes_target.connection.clone();
    // Tier 4c — compute the content-FREE per-note PRIVACY RECEIPT facts HERE, from in-hand egress
    // metadata, BEFORE `gateway_host` is moved into the `NoteRecord` below (the helper only borrows
    // it). See `privacy_receipt_facts` for the honest-self-report rationale.
    let privacy = privacy_receipt_facts(
        &notes_target.connection,
        config,
        gateway_host.as_deref(),
        &call_meta,
    );
    // 2026-07-10 audit F1 (the re-summarize case): a meeting whose folder is LOCKED and
    // session-unlocked passes the `resummarize` gate — the fresh markdown must be RE-SEALED into
    // `content_blob` in the same write (a plaintext-only upsert against the stale lock-time blob is
    // destroyed by the next relock; a NEW provider row would be ungoverned). Open/rootless meetings
    // (every first-time pipeline run — the auto-file `SealInto` below assigns the folder later)
    // take the plain upsert inside the helper.
    //
    // Residual W4 (BLK-1 parity): the persist + the locked-state read run UNDER the lifecycle
    // guard, so `lock_folder`'s multi-step seal (read rows → encrypt → verify → blank) can never
    // interleave with this write — a persist landing between lock's read and its blank would leave
    // fresh plaintext over a stale blob (relock destroys the fresh note). The guard is scoped to
    // this SYNC block only (never held across an `await`; no callee here re-takes it — the reseal
    // helper touches only the DB + master-KEK mutexes).
    let meeting_locked = {
        let _lifecycle = crate::commands::lifecycle_guard(state);
        crate::commands::upsert_note_reseal_if_locked(
            state,
            &NoteRecord {
                meeting_id: meeting_id.to_string(),
                provider_id: notes_target.connection.clone(),
                markdown: markdown.clone(),
                created_at,
                exported_path: None,
                model_requested,
                model_served: call_meta.model_served.clone(),
                gateway_host,
            },
        )?;
        // Read the meeting's locked state under the SAME guard as the persist: the export-skip
        // decision below is anchored to the exact lock state this write was sealed against.
        match state.db.folder_for_meeting(meeting_id)? {
            Some(fid) => state
                .db
                .folder_by_id(&fid)?
                .map(|f| f.locked)
                .unwrap_or(false),
            None => false,
        }
    };

    // Brain v2 L4 — the live bullets are now folded into the persisted note: CONSUME the
    // crash-recovery row (best-effort; a failed clear only means a later resummarize would see
    // the same bullets again — never content loss). A summarize FAILURE above keeps the row, so
    // a retry / crash-salvage re-run still gets its bullets.
    if live_bullets.is_some() {
        if let Err(e) = state.db.clear_live_bullets(meeting_id) {
            tracing::debug!(target: "bullets", error = %e, "live-bullets consume (row clear) failed");
        }
    }

    // brain2 RAG Phase 2b — index the just-persisted note into the on-device vector layer, GATED by
    // the master flag AND the meeting's visibility (never index a sealed-not-unlocked folder's
    // plaintext). Best-effort: a failure logs (no PII) and NEVER fails the pipeline. Flag-off ⇒ this
    // does literally nothing (no embedder built, no writes). A note that is subsequently sealed into
    // a locked folder (the `SealInto` auto-file path below) has its chunks purged by the seal, so the
    // net at-rest state never carries a sealed meeting's vectors.
    //
    // ALSO gated on the real embed model being present (`should_auto_index`): with the flag ON but
    // the model ABSENT, `active_embedder()` is the hash StubEmbedder (not semantic) — writing those
    // into `vec_chunks` would pollute the index, so we skip (mirrors `reindex_embeddings`'s stub
    // refusal). This closes the flag-on-without-model gap.
    if should_auto_index(
        config.semantic_search_enabled,
        crate::embed::embed_model_present(),
    ) {
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
            crate::commands::build_and_persist_entities(app, state, meeting_id, &title, &markdown).await
        {
            tracing::warn!(target: "graph", error = %e, "graph entity persist failed (note saved unaffected)");
        }
        return Ok(PipelineResult {
            note_markdown: markdown,
            exported_path: None,
            meeting_id: meeting_id.to_string(),
        });
    };

    // 2026-07-10 audit F1 (re-summarize case, vault half): a meeting ALREADY governed by a LOCKED
    // folder (session-unlocked — the `resummarize` gate refused otherwise) must NOT re-materialize a
    // plaintext vault `.md`: the seal deleted its `.md` + NULLed `exported_path`, a session unlock
    // never re-exports MEETING notes, and the relock cleanup would not know about a fresh root-level
    // export. The note is already durably (re-)sealed in the DB by the upsert above, so finish
    // exactly like the no-vault path (title + `Summarized`, `exported_path: None`). The
    // `meeting_locked` decision was read under the SAME lifecycle guard as the persist (W4).
    if meeting_locked {
        let title = finalize_note_without_vault(&state.db, meeting_id, &markdown, date_iso)?;
        emit_status(
            app,
            "saved",
            "Saved to Murmur — this folder is locked, so no plaintext note was exported.",
            meeting_id,
        );
        // Same best-effort graph persist as the no-vault path (Sink B's own gate skips vault stubs
        // for a locked folder; Sink A rows are visibility-gated on read).
        if let Err(e) =
            crate::commands::build_and_persist_entities(app, state, meeting_id, &title, &markdown).await
        {
            tracing::warn!(target: "graph", error = %e, "graph entity persist failed (note saved unaffected)");
        }
        return Ok(PipelineResult {
            note_markdown: markdown,
            exported_path: None,
            meeting_id: meeting_id.to_string(),
        });
    }

    emit_status(app, "exporting", "Writing note to vault…", meeting_id);

    let title = derive_title(&markdown, date_iso);
    let subfolder =
        resolve_subfolder(config, provider.as_ref(), vault_path, &title, &markdown).await;
    // Phase 5 — inject model-provenance keys into the YAML frontmatter before the vault write.
    // The note markdown already has the `---` / `---` block generated by the LLM; we append
    // `ai-provider:` and `ai-model:` to it. Pure + idempotent: if the keys are already present
    // (e.g. a re-summarize that re-uses the same markdown), the call is a no-op and the markdown
    // bytes are identical. Never fails — absent / malformed frontmatter ⇒ markdown unchanged.
    let markdown = export::inject_provenance_frontmatter(
        &markdown,
        &provider_id_for_fm,
        model_requested_for_fm.as_deref(),
        model_served_for_fm.as_deref(),
    );
    // Tier 4c — stamp the content-free per-note PRIVACY RECEIPT (facts computed in the prep block
    // above). Local/on-device summary ⇒ `privacy-cloud-calls: 0` (nothing left the device — the
    // honest local headline). Cloud summary ⇒ `privacy-egress-host: <label>` + `privacy-pii-redacted:
    // <n>` (no cloud-CALL count is claimed — not per-note attributable and would under-count).
    // Pure + idempotent, same contract as the provenance injector above; a (re)summarize rebuilds
    // `markdown` fresh so the keys REFRESH rather than duplicate. Vault-write-only (like the
    // provenance keys): no new content read, no DB write, no egress-ledger query.
    let markdown = export::inject_privacy_receipt_frontmatter(
        &markdown,
        privacy.local_only,
        privacy.egress_host.as_deref(),
        privacy.redacted_pii,
    );

    // Residual W4 (BLK-1 parity, vault half): the export DECISION + the vault write + the
    // `exported_path` persist run UNDER ONE lifecycle guard. The `resolve_subfolder` await above is
    // a window where `lock_folder` can land — writing the plaintext `.md` on the stale pre-await
    // decision would leave an UNTRACKED plaintext export (the lock's `.md` cleanup collects targets
    // from `exported_path`, which is only set after the write). Under the guard: re-check the
    // meeting's locked state (a locked-meanwhile meeting takes the sealed finish below — its fresh
    // markdown was already sealed by that very `lock_folder`), then decide/write/persist as one
    // un-interleavable step. DEADLOCK-CHECKED: nothing in this block re-takes the lifecycle mutex
    // (`classify_auto_file_target` locks only `unlocked_folders`; the rest is DB + fs) — the
    // guard-taking `seal_auto_filed_note` runs AFTER the block, and no `await` occurs while held.
    let written: Option<(std::path::PathBuf, crate::commands::AutoFileTarget)> = {
        let _lifecycle = crate::commands::lifecycle_guard(state);
        let locked_now = match state.db.folder_for_meeting(meeting_id)? {
            Some(fid) => state
                .db
                .folder_by_id(&fid)?
                .map(|f| f.locked)
                .unwrap_or(false),
            None => false,
        };
        if locked_now {
            None
        } else {
            // Auto-organize safety (BLK-2 parity): if the classifier chose a subfolder that maps to
            // a LOCKED folder, plaintext must NEVER land in its sealed on-disk dir — not even
            // transiently. Decide BEFORE writing (and inside the same guarded section as the write).
            let auto_file = crate::commands::classify_auto_file_target(state, subfolder.as_deref())?;
            let write_subfolder = match &auto_file {
                // Open / root / unmanaged subfolder → write into the chosen subfolder as usual.
                crate::commands::AutoFileTarget::Open => subfolder.as_deref(),
                // Any LOCKED target → write at the vault ROOT instead. `SealInto` then seals the
                // note INTO the folder (encrypting it + removing this root `.md`); `RejectToRoot`
                // simply leaves it at the root. Either way no plaintext is ever written into the
                // sealed on-disk dir.
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
            persist_note_exported_path(&state.db, config, meeting_id, &exported_path)?;
            state.db.set_meeting_title(meeting_id, &title)?;
            state
                .db
                .update_meeting_status(meeting_id, MeetingStatus::Exported)?;
            Some((exported_path, auto_file))
        }
    };
    let Some((exported_path, auto_file)) = written else {
        // The meeting's folder was locked during the `resolve_subfolder` await: NO plaintext `.md`
        // is written (the note is already durably sealed in the DB by that lock). Finish exactly
        // like the meeting-locked path above.
        let title = finalize_note_without_vault(&state.db, meeting_id, &markdown, date_iso)?;
        emit_status(
            app,
            "saved",
            "Saved to Murmur — this folder is locked, so no plaintext note was exported.",
            meeting_id,
        );
        if let Err(e) =
            crate::commands::build_and_persist_entities(app, state, meeting_id, &title, &markdown).await
        {
            tracing::warn!(target: "graph", error = %e, "graph entity persist failed (note saved unaffected)");
        }
        return Ok(PipelineResult {
            note_markdown: markdown,
            exported_path: None,
            meeting_id: meeting_id.to_string(),
        });
    };

    // Seal the note INTO a session-unlocked locked folder (encrypts the markdown/extras and removes
    // the plaintext `.md` we just wrote) — the same outcome a manual move would produce. Runs
    // OUTSIDE the guarded block above: `seal_auto_filed_note` → `move_into_locked_folder` takes the
    // lifecycle guard itself (holding it here would self-deadlock); the `.md` is already tracked by
    // `exported_path`, so an interleaving lock's cleanup covers it.
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
        crate::commands::build_and_persist_entities(app, state, meeting_id, &title, &markdown).await
    {
        tracing::warn!(target: "graph", error = %e, "graph entity persist failed (note export unaffected)");
    }

    // Stage 2 / Lane A — DEFERRED, best-effort cross-meeting LINKING. Runs AFTER the note reaches
    // `Exported` so the first `.md` is written promptly; the `[[links]]` + Related block land seconds
    // later on a DB-canonical re-persist + re-export. Fully LOCAL (ZERO egress) + lock/seal-safe (see
    // `link_related_notes_inner`), so it is auto-eligible on finalize. It must NEVER fail or delay the
    // pipeline: a detached worker (the same `std::thread::spawn` + `app.state::<AppState>()` shape the
    // proactive/reactions workers use), any error swallowed with an IDs-only log (no PII).
    {
        let app_bg = app.clone();
        let mid = meeting_id.to_string();
        std::thread::spawn(move || {
            use tauri::Manager;
            let state = app_bg.state::<AppState>();
            if let Err(e) = crate::commands::link_related_notes_inner(state.inner(), &mid) {
                tracing::warn!(
                    target: "stage2",
                    meeting_id = %mid,
                    error = %e,
                    "deferred cross-meeting linking failed (note unaffected)"
                );
            }
        });
    }

    Ok(PipelineResult {
        note_markdown: markdown,
        exported_path: Some(exported_path),
        meeting_id: meeting_id.to_string(),
    })
}

/// Point the note row `summarize_and_export` just upserted at its exported vault `.md`.
///
/// PAIRED-WRITE INVARIANT (seal-critical): the row key MUST be the RESOLVED notes connection —
/// the SAME key the `upsert_note` above used (`provider_target(Role::Notes, …).connection`) —
/// NEVER the raw `config.provider_id`. Under an explicit `role_notes_connection` override the two
/// differ, the `UPDATE … WHERE meeting_id AND provider_id` matches 0 rows silently, and a NULL
/// `exported_path` means the seal path (which collects its vault-`.md` deletion targets from
/// `exported_path`) leaves the plaintext `.md` alive in the vault after a lock.
/// Regression: `exported_path_lands_on_the_role_overridden_note_row`.
fn persist_note_exported_path(
    db: &crate::storage::Db,
    config: &AppConfig,
    meeting_id: &str,
    exported_path: &Path,
) -> Result<()> {
    let connection = crate::summarize::roles::provider_target(Role::Notes, config).connection;
    db.set_note_exported_path(meeting_id, &connection, &exported_path.to_string_lossy())
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

/// brain2 realtime notes FINALIZE-FOLD: append the user's typed in-meeting notes to the generated
/// note markdown under a `## My notes` section. Empty / whitespace-only `manual_notes` ⇒ the markdown
/// is returned UNCHANGED (byte-identical to the generated note), so a meeting with no typed notes
/// produces exactly today's output. The folded notes then ride the EXISTING note seal (`content_blob`)
/// — never a second plaintext copy. Pure + Db-free so it is unit-testable in isolation.
fn fold_manual_notes(markdown: &str, manual_notes: &str) -> String {
    let notes = manual_notes.trim();
    if notes.is_empty() {
        return markdown.to_string();
    }
    format!("{markdown}\n\n## My notes\n\n{notes}\n")
}

/// ENHANCE-MY-NOTES finalize: decide how the typed notes reach the stored note.
/// - "enhance" + non-blank notes ⇒ the notes were already IN the prompt as the skeleton
///   (Task: user_notes on SummarizeRequest); do NOT append them again — stamp the
///   `murmur_enhanced: true` front-matter marker instead (the FE badge + honest provenance).
/// - anything else (append mode, empty buffer, unknown mode) ⇒ the legacy verbatim fold,
///   whose empty case is byte-identical passthrough. Pure + Db-free (unit-testable).
fn finalize_note_markdown(generated: &str, manual_notes: &str, notes_mode: &str) -> String {
    if notes_mode == "enhance" && !manual_notes.trim().is_empty() {
        mark_enhanced(generated)
    } else {
        fold_manual_notes(generated, manual_notes)
    }
}

/// Insert `murmur_enhanced: true` as the first YAML front-matter line — a DETERMINISTIC
/// backend stamp (never model-generated, so it can't be forgotten or hallucinated).
/// No/unterminated front-matter ⇒ returned unchanged; already stamped ⇒ unchanged.
fn mark_enhanced(markdown: &str) -> String {
    if markdown.contains("murmur_enhanced:") {
        return markdown.to_string();
    }
    match markdown.strip_prefix("---\n") {
        Some(rest) if rest.contains("\n---") => {
            format!("---\nmurmur_enhanced: true\n{rest}")
        }
        _ => markdown.to_string(),
    }
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
    // Load the meeting's RESTORED/visible plaintext segments so transcript chunks (source_type=
    // 'transcript') are indexed alongside the note-summary chunks. A sealed meeting is already excluded
    // above, so these are visible plaintext; an empty transcript simply yields zero transcript chunks.
    let segments = db.get_segments(meeting_id)?;
    db.index_meeting_chunks(meeting_id, &segments, embedder)?;
    // Brain v2 L1.1 — TOPIC chunks ride the same gate, AFTER the note/transcript index (whose
    // clean-replace purge covers all chunk classes via the shared `purge_chunks_tx` choke point).
    db.index_meeting_topic_chunks(meeting_id, &segments, embedder, unlocked)
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

    // Rebuild the transcript from segments — EXCLUDING assistant-directed utterances ("Klaudku, …")
    // so they never reach action-item extraction. TIER 0: `build_transcript_feed` speaker-labels the
    // summary input when ≥2 distinct speakers, else stays byte-identical flat text.
    let feed = build_transcript_feed(&segments);

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
        meeting_id,
        &feed,
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
            "no Whisper model found — pick a language + model in Settings and download it".into(),
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

/// Whether the post-note AUTO semantic index should run: requires BOTH the master flag AND the real
/// embed model present on disk. With the flag ON but no model, `active_embedder()` is the hash
/// StubEmbedder (semantically meaningless) — auto-indexing it would pollute `vec_chunks` with noise,
/// so we skip. Mirrors `reindex_embeddings`, which already refuses the stub. Pure + unit-tested.
fn should_auto_index(semantic_enabled: bool, embed_model_present: bool) -> bool {
    semantic_enabled && embed_model_present
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── whisper decode windowing (P1 whisper-batch-cap) ────────────────────────────────────────
    /// A long region is tiled into ≤window_len windows that fully cover it with no gaps/overlaps, so
    /// whisper never decodes more than one window at a time. Pre-cap the whole region was one decode
    /// (this helper + tiling contract did not exist).
    #[test]
    fn decode_windows_tiles_long_region() {
        let hz = crate::audio::TARGET_RATE_HZ as usize;
        let w = 120 * hz;
        let wins = decode_windows(0, 300 * hz, w);
        assert_eq!(
            wins,
            vec![(0, 120 * hz), (120 * hz, 240 * hz), (240 * hz, 300 * hz)]
        );
        assert_eq!(wins.first().unwrap().0, 0);
        assert_eq!(wins.last().unwrap().1, 300 * hz);
        for pair in wins.windows(2) {
            assert_eq!(pair[0].1, pair[1].0, "windows must be contiguous");
        }
        for (s, e) in &wins {
            assert!(e - s <= w, "no window exceeds the cap");
        }
    }

    /// A region already within budget is a single window (byte-identical to the pre-cap behavior);
    /// a non-zero start offset is preserved.
    #[test]
    fn decode_windows_short_region_is_one_window() {
        let hz = crate::audio::TARGET_RATE_HZ as usize;
        assert_eq!(decode_windows(0, 30 * hz, 120 * hz), vec![(0, 30 * hz)]);
        assert_eq!(
            decode_windows(5 * hz, 20 * hz, 120 * hz),
            vec![(5 * hz, 20 * hz)]
        );
    }

    /// Degenerate ranges yield nothing (subsumes the old `if end <= start { continue }`).
    #[test]
    fn decode_windows_empty_on_degenerate_range() {
        assert!(decode_windows(100, 100, 16_000).is_empty());
        assert!(decode_windows(200, 100, 16_000).is_empty());
    }

    // ── Tier 4c: privacy_receipt_facts (per-note egress self-report wiring) ────────────────────
    //
    // These guard the WIRING the pure `inject_privacy_receipt_frontmatter` tests (in
    // export/obsidian.rs) cannot: that `summarize_and_export` derives the right facts from a
    // provider connection + config + `CallMeta` and feeds them to the injector. The injector was
    // defined + tested but UN-CALLED before this change; without a facts test the vault note would
    // silently carry no `privacy-*` key at all (the "compiles but inert" gap).
    use crate::summarize::meta::{CallMeta, RedactionCounts};

    /// LOCAL provider (loopback ollama) ⇒ `local_only`, no host, no PII count — and the injector
    /// stamps ONLY the honest `privacy-cloud-calls: 0` headline. The task's explicit requirement.
    #[test]
    fn privacy_receipt_facts_local_ollama_stamps_zero_cloud_calls() {
        let cfg = AppConfig {
            ollama_base_url: "http://localhost:11434".into(),
            ..AppConfig::default()
        };
        // A local provider returns UNWRAPPED (no RedactingProvider), so no firewall ran → None.
        let meta = CallMeta::default();
        let facts = privacy_receipt_facts(crate::summarize::PROVIDER_OLLAMA, &cfg, None, &meta);
        assert!(
            facts.local_only,
            "loopback ollama = nothing left the device"
        );
        assert_eq!(facts.egress_host, None);
        assert_eq!(facts.redacted_pii, None);
        // End-to-end through the SAME injector the pipeline calls.
        let out = crate::export::inject_privacy_receipt_frontmatter(
            "---\ntitle: T\n---\n# T\n\nBody.\n",
            facts.local_only,
            facts.egress_host.as_deref(),
            facts.redacted_pii,
        );
        assert!(
            out.contains("privacy-cloud-calls: 0"),
            "local headline stamped: {out}"
        );
        assert!(
            !out.contains("privacy-egress-host"),
            "no host for a local note: {out}"
        );
        assert!(
            !out.contains("privacy-pii-redacted"),
            "no pii key for a local note: {out}"
        );
    }

    /// ANTHROPIC (cloud) ⇒ the non-PII host label + the firewall's REAL scrub total (summed across
    /// buckets) reach the receipt; NO cloud-CALL integer is claimed (would under-count).
    #[test]
    fn privacy_receipt_facts_anthropic_stamps_host_and_real_pii_count() {
        let cfg = AppConfig::default();
        let meta = CallMeta {
            redactions: Some(RedactionCounts {
                email: 2,
                card: 0,
                phone: 1,
                name: 3,
            }),
            ..Default::default()
        };
        let facts = privacy_receipt_facts(crate::summarize::PROVIDER_ANTHROPIC, &cfg, None, &meta);
        assert!(!facts.local_only, "anthropic is cloud egress");
        assert_eq!(facts.egress_host.as_deref(), Some("api.anthropic.com"));
        assert_eq!(
            facts.redacted_pii,
            Some(6),
            "2+0+1+3 = the real firewall total"
        );
        let out = crate::export::inject_privacy_receipt_frontmatter(
            "---\ntitle: T\n---\nBody.",
            facts.local_only,
            facts.egress_host.as_deref(),
            facts.redacted_pii,
        );
        assert!(
            out.contains("privacy-egress-host: api.anthropic.com"),
            "host: {out}"
        );
        assert!(out.contains("privacy-pii-redacted: 6"), "real count: {out}");
        assert!(
            !out.contains("privacy-cloud-calls"),
            "no cloud-call count claimed: {out}"
        );
    }

    /// GATEWAY ⇒ the receipt reuses the already-computed non-PII endpoint `host:port` label.
    #[test]
    fn privacy_receipt_facts_gateway_uses_endpoint_host() {
        let facts = privacy_receipt_facts(
            crate::summarize::PROVIDER_GATEWAY,
            &AppConfig::default(),
            Some("127.0.0.1:4000"),
            &CallMeta::default(),
        );
        assert!(
            !facts.local_only,
            "gateway is always cloud (a localhost gateway may forward)"
        );
        assert_eq!(facts.egress_host.as_deref(), Some("127.0.0.1:4000"));
    }

    /// A REMOTE `ollama_base_url` classifies as CLOUD — the receipt can NEVER under-claim "local".
    #[test]
    fn privacy_receipt_facts_remote_ollama_is_cloud_not_local() {
        let cfg = AppConfig {
            ollama_base_url: "https://ollama.remote.example/api".into(),
            ..AppConfig::default()
        };
        let meta = CallMeta {
            redactions: Some(RedactionCounts::default()),
            ..Default::default()
        };
        let facts = privacy_receipt_facts(crate::summarize::PROVIDER_OLLAMA, &cfg, None, &meta);
        assert!(
            !facts.local_only,
            "a remote ollama endpoint is cloud egress, not local"
        );
        assert_eq!(facts.egress_host.as_deref(), Some("ollama.remote.example"));
        assert_eq!(
            facts.redacted_pii,
            Some(0),
            "wrapped-but-zero scrubs = an honest 0"
        );
    }

    /// REFRESH-on-resummarize: each (re)summarize hands the injector a FRESH markdown (the LLM
    /// regenerates the note carrying no `privacy-*` key), so switching provider between runs
    /// REFRESHES the receipt — a run-2 cloud note does NOT retain run-1's `privacy-cloud-calls: 0`.
    #[test]
    fn privacy_receipt_refreshes_when_provider_changes_between_summaries() {
        // Run 1: local ollama.
        let local_cfg = AppConfig {
            ollama_base_url: "http://localhost:11434".into(),
            ..AppConfig::default()
        };
        let f1 = privacy_receipt_facts(
            crate::summarize::PROVIDER_OLLAMA,
            &local_cfg,
            None,
            &CallMeta::default(),
        );
        let run1 = crate::export::inject_privacy_receipt_frontmatter(
            "---\ntitle: T\n---\nBody.",
            f1.local_only,
            f1.egress_host.as_deref(),
            f1.redacted_pii,
        );
        assert!(
            run1.contains("privacy-cloud-calls: 0"),
            "run 1 = local: {run1}"
        );

        // Run 2 (resummarize): provider switched to anthropic; the model produced a FRESH note.
        let f2 = privacy_receipt_facts(
            crate::summarize::PROVIDER_ANTHROPIC,
            &AppConfig::default(),
            None,
            &CallMeta {
                redactions: Some(RedactionCounts {
                    email: 1,
                    card: 0,
                    phone: 0,
                    name: 0,
                }),
                ..Default::default()
            },
        );
        let run2 = crate::export::inject_privacy_receipt_frontmatter(
            "---\ntitle: T\n---\nBody.", // fresh markdown, no stale privacy keys
            f2.local_only,
            f2.egress_host.as_deref(),
            f2.redacted_pii,
        );
        assert!(
            run2.contains("privacy-egress-host: api.anthropic.com"),
            "run 2 = cloud host: {run2}"
        );
        assert!(
            run2.contains("privacy-pii-redacted: 1"),
            "run 2 real count: {run2}"
        );
        assert!(
            !run2.contains("privacy-cloud-calls: 0"),
            "the local headline did NOT bleed into the cloud re-summary: {run2}"
        );
    }

    /// LEAK regression (lock-security review 2026-07-02): a cloud-egress consent REVOKE landing
    /// DURING transcription — between `run_inner`'s Stop-time config snapshot and the summarize
    /// step — must be honored. The pre-fix code fed the STALE consented snapshot to
    /// `provider_for`, so the redacted transcript still egressed. Discrimination is the
    /// established gate-probe pattern (bogus provider id, zero network / keychain / CLI):
    /// `Unavailable` = refused AT the consent gate; `InvalidArg` = PAST the gate (the factory
    /// rejecting the bogus id — i.e. a real provider would have egressed).
    #[test]
    fn summarize_egress_resolution_honors_revoke_landed_after_stop_snapshot() {
        let p = crate::storage::db::unique_temp_path("murmur-pipeline-revoke", "sqlite");
        let state = AppState::init_at(
            &p,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        {
            let mut c = state.config.lock().unwrap();
            c.provider_id = "no_such_provider_for_gate_probe".into();
            c.grant_cloud_egress_consent(&state.db).unwrap();
        }

        // run_inner's Stop-time snapshot (consented) — exactly what the pre-fix code carried
        // across the minutes of transcription and fed to `provider_for`.
        let stop_snapshot = state.config.lock().unwrap().clone();

        // The user revokes WHILE transcription runs (the real `revoke_cloud_egress` mutator,
        // as the Tauri command performs it).
        state
            .config
            .lock()
            .unwrap()
            .revoke_cloud_egress(&state.db)
            .unwrap();

        // FIXED: the summarize step resolves config + provider through the LIVE state → the
        // fail-closed consent gate refuses.
        match resolve_summarize_egress(&state) {
            Err(AppError::Unavailable(_)) => {}
            Err(other) => panic!("summarize egress must refuse AT the consent gate, got {other:?}"),
            Ok(_) => panic!("summarize egress must refuse AT the consent gate, got Ok"),
        }

        // THE LEAK the fix closes (kept as the RED half): the stale Stop-time snapshot sails
        // PAST the gate — `InvalidArg` is the factory rejecting the bogus id, proving a real
        // provider id would have been built and egressed.
        match provider_for(Role::Notes, &stop_snapshot, &state.heavy_inference) {
            Err(AppError::InvalidArg(_)) => {}
            Err(other) => panic!("stale snapshot should pass the gate (the leak), got {other:?}"),
            Ok(_) => panic!("bogus provider id must not construct"),
        }

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn auto_index_requires_both_flag_and_model() {
        // The gap this closes: flag ON but model ABSENT must NOT auto-index (would write hash-stub
        // vectors into vec_chunks). Only flag-AND-model-present writes real vectors.
        assert!(
            should_auto_index(true, true),
            "flag on + model present → index"
        );
        assert!(
            !should_auto_index(true, false),
            "flag on WITHOUT the model must NOT write stub vectors"
        );
        assert!(!should_auto_index(false, true), "flag off → never index");
        assert!(!should_auto_index(false, false));
    }

    #[test]
    fn derive_title_prefers_frontmatter() {
        let md = "---\ntitle: Q3 Planning\ndate: 2026-06-24\n---\n# Heading\n";
        assert_eq!(derive_title(md, "2026-06-24"), "Q3 Planning");
    }

    // ── TIER 0: speaker-aware transcript feed ────────────────────────────────
    fn seg(
        idx: i64,
        start: f64,
        end: f64,
        speaker: Option<&str>,
        text: &str,
    ) -> crate::transcribe::types::Segment {
        crate::transcribe::types::Segment {
            idx,
            start_s: start,
            end_s: end,
            text: text.to_string(),
            speaker: speaker.map(str::to_string),
            confidence: None,
        }
    }

    /// The default mono-`me` meeting (one distinct label, incl. `None`→`me`): NOT labeled;
    /// `summary_text` == `retrieval_text` == the exact legacy flat join. Guards no-regression on the
    /// artifact 100% of users read. RED on any change that tags a single-speaker meeting.
    #[test]
    fn feed_single_speaker_is_flat_and_byte_identical() {
        let segs = vec![
            seg(0, 0.0, 2.0, Some("me"), "  hello everyone  "),
            seg(1, 2.0, 5.0, None, "let's begin"),
        ];
        let feed = build_transcript_feed(&segs);
        assert!(!feed.labeled, "one distinct speaker ⇒ not labeled");
        assert_eq!(feed.summary_text, "hello everyone let's begin");
        assert_eq!(feed.summary_text, feed.retrieval_text, "flat == retrieval");
        assert!(!feed.summary_text.contains('(') && !feed.summary_text.contains('['));
    }

    /// ≥2 distinct speakers ⇒ labeled with the EXACT `[start-end] (speaker) text` timeline shape,
    /// while `retrieval_text` stays FLAT (no tag/timestamp tokens perturb the RAG query).
    #[test]
    fn feed_multi_speaker_is_labeled_timeline_format() {
        let segs = vec![
            seg(0, 0.0, 2.0, Some("me"), "hi there"),
            seg(1, 2.0, 6.0, Some("others"), "great to meet you"),
        ];
        let feed = build_transcript_feed(&segs);
        assert!(feed.labeled);
        assert_eq!(
            feed.summary_text,
            "[0.0-2.0] (me) hi there\n[2.0-6.0] (others) great to meet you"
        );
        assert_eq!(feed.retrieval_text, "hi there great to meet you");
        assert!(!feed.retrieval_text.contains('[') && !feed.retrieval_text.contains("(me)"));
    }

    /// Empty + assistant-directed ("Klaudku, …") segments are dropped identically to the old flat
    /// loop; the surviving multi-speaker lines are labeled and carry no assistant command.
    #[test]
    fn feed_filters_empty_and_assistant_directed() {
        let segs = vec![
            seg(0, 0.0, 1.0, Some("me"), "   "),
            seg(1, 1.0, 3.0, Some("me"), "Klaudku, sprawdź pogodę"),
            seg(2, 3.0, 5.0, Some("me"), "let's plan the launch"),
            seg(3, 5.0, 8.0, Some("others"), "sounds good"),
        ];
        let feed = build_transcript_feed(&segs);
        assert!(feed.labeled);
        assert!(!feed.summary_text.contains("Klaudku") && !feed.summary_text.contains("pogodę"));
        assert!(feed.summary_text.contains("(me) let's plan the launch"));
        assert!(feed.summary_text.contains("(others) sounds good"));
    }

    /// Tier 3b/A3: a shaky segment (`confidence < LOW_CONFIDENCE_P`) is prefixed `[UNCLEAR]` in the
    /// summarizer feed; high-confidence + `None` segments are NOT; `retrieval_text` NEVER carries the
    /// marker; and a feed with ALL-`None` confidence is byte-identical to today (no-regression proof
    /// for the model-less / CI default).
    #[test]
    fn feed_marks_low_confidence_segments_unclear() {
        let mut low = seg(0, 0.0, 2.0, Some("me"), "the garbled acoustic span");
        low.confidence = Some(0.30);
        let mut hi = seg(1, 2.0, 5.0, Some("others"), "the clear response here");
        hi.confidence = Some(0.95);
        let feed = build_transcript_feed(&[low, hi]);
        assert!(feed.labeled);
        // The low-confidence line is marked; the confident one is not.
        assert!(
            feed.summary_text
                .contains("(me) [UNCLEAR] the garbled acoustic span"),
            "low-confidence span must be prefixed [UNCLEAR]; got: {}",
            feed.summary_text
        );
        assert!(feed
            .summary_text
            .contains("(others) the clear response here"));
        assert!(!feed.summary_text.contains("[UNCLEAR] the clear"));
        // Retrieval text is never perturbed by the marker.
        assert!(!feed.retrieval_text.contains("[UNCLEAR]"));

        // No-regression: ALL-None confidence ⇒ summary_text is byte-identical to the pre-A3 output
        // (both the labeled and the flat single-speaker paths).
        let none_multi = vec![
            seg(0, 0.0, 2.0, Some("me"), "hi there"),
            seg(1, 2.0, 6.0, Some("others"), "great to meet you"),
        ];
        assert_eq!(
            build_transcript_feed(&none_multi).summary_text,
            "[0.0-2.0] (me) hi there\n[2.0-6.0] (others) great to meet you"
        );
        let none_solo = vec![
            seg(0, 0.0, 2.0, Some("me"), "hello everyone"),
            seg(1, 2.0, 5.0, None, "let's begin"),
        ];
        let solo = build_transcript_feed(&none_solo);
        assert_eq!(solo.summary_text, "hello everyone let's begin");
        assert_eq!(solo.summary_text, solo.retrieval_text);
    }

    const HZ: usize = crate::audio::TARGET_RATE_HZ as usize;

    /// RED-before-GREEN: the shipped 51 s recording produced an 8 s AEC ASR feed (malformed 9-ch
    /// VPIO capture) and the OLD code accepted it unconditionally → the timeline desynced from the
    /// played raw-mic archive. The guard MUST reject a feed this far off.
    #[test]
    fn aec_feed_51s_recording_8s_feed_is_rejected() {
        assert!(
            !aec_feed_matches_raw(52 * HZ, 8 * HZ),
            "an 8 s AEC feed for a 52 s recording must be rejected (would desync the timeline)"
        );
    }

    #[test]
    fn aec_feed_matching_raw_is_accepted() {
        // Same length, and a sub-second VPIO startup gap, are both fine.
        assert!(aec_feed_matches_raw(52 * HZ, 52 * HZ));
        assert!(aec_feed_matches_raw(52 * HZ, 52 * HZ - HZ / 2)); // 0.5 s shorter
    }

    #[test]
    fn aec_feed_just_past_tolerance_is_rejected() {
        // > 2 s shorter → reject; exactly within 2 s → accept (boundary either side of the const).
        assert!(aec_feed_matches_raw(60 * HZ, 60 * HZ - 2 * HZ)); // exactly 2 s → within tolerance
        assert!(!aec_feed_matches_raw(60 * HZ, 60 * HZ - 3 * HZ)); // 3 s → rejected
    }

    /// A longer-than-raw feed (e.g. a multi-channel mis-decode inflating the buffer) is also rejected.
    #[test]
    fn aec_feed_longer_than_raw_is_rejected() {
        assert!(!aec_feed_matches_raw(10 * HZ, 90 * HZ));
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
        assert!(
            !p.exists(),
            "scratch WAV must be removed when its guard drops"
        );
    }

    /// A `None` scratch guard (no AEC/system WAV produced) and a guard whose file was already
    /// removed must both drop without panicking.
    #[test]
    fn scratch_wav_none_or_missing_is_noop() {
        drop(ScratchWav::new(None));
        let p =
            std::env::temp_dir().join(format!("murmur-scratch-missing-{}.wav", std::process::id()));
        let _ = std::fs::remove_file(&p);
        drop(ScratchWav::new(Some(p))); // file doesn't exist — must not panic
    }

    /// Fixed SQLCipher key for the file-backed test DB (NOT a Keychain DEK).
    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn temp_db_path(label: &str) -> std::path::PathBuf {
        crate::storage::db::unique_temp_path(&format!("murmur-pipeline-test-{label}"), "sqlite")
    }

    /// REGRESSION (adversarial find, seal content-leak class): `summarize_and_export` upserts the
    /// note row keyed on the RESOLVED notes connection (`provider_target(Role::Notes, …)`), so the
    /// export-path update MUST use the SAME key. The pre-fix code passed the raw
    /// `config.provider_id`: under an explicit `role_notes_connection` override the keys differ,
    /// the `UPDATE … WHERE meeting_id AND provider_id` matches 0 rows SILENTLY, `exported_path`
    /// stays NULL — and the seal path (`seal_meeting_into_folder` / `lock_folder`) collects its
    /// vault-`.md` deletion targets from `exported_path`, so the plaintext `.md` would SURVIVE a
    /// seal. This drives the REAL paired write (`upsert_note` with the pipeline's key +
    /// `persist_note_exported_path`) and asserts the path lands on the row AND that the seal
    /// path's deletion-target view (`sealable_notes_for_meeting`) sees it.
    #[test]
    fn exported_path_lands_on_the_role_overridden_note_row() {
        use crate::storage::models::Meeting;

        let db = crate::storage::Db::open_with_key(&temp_db_path("role-export"), TEST_DEK).unwrap();
        let meeting = |id: &str| Meeting {
            id: id.to_string(),
            started_at: "2026-07-02T09:00:00Z".to_string(),
            ended_at: Some("2026-07-02T09:10:00Z".to_string()),
            title: Some("Role export".to_string()),
            duration_s: 600,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        };
        let note_row = |id: &str, connection: &str| NoteRecord {
            meeting_id: id.to_string(),
            provider_id: connection.to_string(),
            markdown: "# body".to_string(),
            created_at: "2026-07-02T09:11:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        };

        // (a) EXPLICIT role override: the pipeline's row key is the resolved connection
        // ("anthropic"), NOT provider_id ("claude_code").
        let config = AppConfig {
            provider_id: "claude_code".to_string(),
            role_notes_connection: "anthropic".to_string(),
            ..AppConfig::default()
        };
        let notes_target = crate::summarize::roles::provider_target(Role::Notes, &config);
        assert_eq!(notes_target.connection, "anthropic");
        let mid = "m-role-export";
        db.insert_meeting(&meeting(mid)).unwrap();
        db.upsert_note(&note_row(mid, &notes_target.connection))
            .unwrap();

        // The REAL production paired write.
        persist_note_exported_path(&db, &config, mid, Path::new("/vault/Role export.md")).unwrap();

        let note = db
            .get_latest_note_for_meeting(mid)
            .unwrap()
            .expect("note row");
        assert_eq!(note.provider_id, "anthropic");
        assert_eq!(
            note.exported_path.as_deref(),
            Some("/vault/Role export.md"),
            "exported_path must land on the row the pipeline upserted — NULL here means the seal \
             path never sees the vault .md and the plaintext survives a lock"
        );
        // The seal path's deletion-target collection view sees the .md.
        assert!(
            db.sealable_notes_for_meeting(mid)
                .unwrap()
                .iter()
                .any(|n| n.exported_path.as_deref() == Some("/vault/Role export.md")),
            "the seal deletion-target collection must see the exported .md"
        );

        // (b) LEGACY fallback (no role keys): the key is provider_id — unchanged behavior.
        let legacy_cfg = AppConfig::default(); // provider_id = claude_code, keys absent
        let mid2 = "m-legacy-export";
        db.insert_meeting(&meeting(mid2)).unwrap();
        db.upsert_note(&note_row(
            mid2,
            &crate::summarize::roles::provider_target(Role::Notes, &legacy_cfg).connection,
        ))
        .unwrap();
        persist_note_exported_path(&db, &legacy_cfg, mid2, Path::new("/vault/Legacy.md")).unwrap();
        let legacy_note = db
            .get_latest_note_for_meeting(mid2)
            .unwrap()
            .expect("note row");
        assert_eq!(legacy_note.provider_id, "claude_code");
        assert_eq!(
            legacy_note.exported_path.as_deref(),
            Some("/vault/Legacy.md")
        );
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
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();

        // The no-vault tail: titles the meeting and returns Ok (never the Export error).
        let title = finalize_note_without_vault(&db, mid, markdown, "2026-06-27").unwrap();
        assert_eq!(title, "Q3 Planning");

        // Note is saved to the canonical DB with NO export path.
        let note = db
            .get_note(mid, "claude_code")
            .unwrap()
            .expect("note saved");
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

    /// brain2 realtime notes: `fold_manual_notes` appends a `## My notes` section with the typed
    /// notes, and returns the note UNCHANGED (byte-identical) for an empty / whitespace-only buffer.
    #[test]
    fn fold_manual_notes_appends_section_and_empty_is_byte_identical() {
        let note = "# Sync\n\n- decided X";
        // Empty / whitespace-only ⇒ byte-identical to the generated note (no behavior change).
        assert_eq!(fold_manual_notes(note, ""), note);
        assert_eq!(fold_manual_notes(note, "   \n\t "), note);
        // Present ⇒ appended under a `## My notes` heading, verbatim (trimmed).
        let folded = fold_manual_notes(note, "ship Friday; Anna owns QA");
        assert!(folded.starts_with(note), "preserves the generated note");
        assert!(
            folded.contains("## My notes"),
            "adds the My notes heading: {folded}"
        );
        assert!(
            folded.contains("ship Friday; Anna owns QA"),
            "embeds the typed notes verbatim"
        );
    }

    /// ENHANCE-MY-NOTES: the mode switch. Empty notes ⇒ byte-identical in BOTH modes (the hard
    /// invariant); append + notes ⇒ exactly today's verbatim fold; enhance + notes ⇒ the
    /// front-matter marker is stamped, NO verbatim `## My notes` section, body preserved;
    /// unknown mode ⇒ defensive fall-back to the legacy fold.
    #[test]
    fn finalize_note_markdown_switches_between_enhance_and_append() {
        let note = "---\ntitle: Sync\n---\n# Sync\n\n- decided X";
        assert_eq!(finalize_note_markdown(note, "", "enhance"), note);
        assert_eq!(finalize_note_markdown(note, "", "append"), note);
        assert_eq!(finalize_note_markdown(note, "   \n\t ", "enhance"), note);
        assert_eq!(
            finalize_note_markdown(note, "ship Friday", "append"),
            fold_manual_notes(note, "ship Friday"),
            "append mode is byte-identical to today's fold"
        );
        let enhanced = finalize_note_markdown(note, "ship Friday", "enhance");
        assert!(
            enhanced.contains("murmur_enhanced: true"),
            "marker stamped: {enhanced}"
        );
        assert!(
            !enhanced.contains("## My notes"),
            "no verbatim section in enhance mode"
        );
        assert!(enhanced.contains("# Sync"), "generated body preserved");
        assert_eq!(
            finalize_note_markdown(note, "x", "banana"),
            fold_manual_notes(note, "x"),
            "unknown mode falls back to the legacy fold"
        );
    }

    /// The marker is a deterministic backend stamp: inserted as the first front-matter line,
    /// idempotent, and a no-op when the provider output has no front-matter (defensive —
    /// ollama output may lack it).
    #[test]
    fn mark_enhanced_stamps_front_matter_idempotently() {
        let note = "---\ntitle: T\n---\n# T";
        let stamped = mark_enhanced(note);
        assert!(
            stamped.starts_with("---\nmurmur_enhanced: true\ntitle: T\n"),
            "marker is the first front-matter line: {stamped}"
        );
        assert_eq!(mark_enhanced(&stamped), stamped, "idempotent");
        let bare = "# No front matter";
        assert_eq!(mark_enhanced(bare), bare, "no front-matter ⇒ unchanged");
        let unterminated = "---\ntitle: broken";
        assert_eq!(
            mark_enhanced(unterminated),
            unterminated,
            "unterminated fm ⇒ unchanged"
        );
    }

    /// brain2 realtime notes FOLD + COUNTEREXAMPLE B (the seam `summarize_and_export` runs, minus
    /// the AppHandle/provider): get buffer → fold into the generated note → upsert. The buffer is
    /// the DURABLE canonical store — NOT blanked — so a RESUMMARIZE (which regenerates a fresh note
    /// with no `## My notes`) re-reads the buffer and re-folds it, and the typed notes are NEVER
    /// dropped. RED on the prior blank-at-finalize code: the buffer would be "" on resummarize and
    /// the regenerated note would lose `## My notes`.
    #[test]
    fn finalize_folds_manual_notes_durably_and_resummarize_refolds() {
        use crate::storage::models::Meeting;

        let db = crate::storage::Db::open_with_key(&temp_db_path("fold-manual"), TEST_DEK).unwrap();
        let mid = "m-fold";
        db.insert_meeting(&Meeting {
            id: mid.to_string(),
            started_at: "2026-06-30T09:00:00Z".to_string(),
            ended_at: Some("2026-06-30T09:10:00Z".to_string()),
            title: None,
            duration_s: 600,
            audio_path: None,
            status: MeetingStatus::Recording,
            folder_id: None,
        })
        .unwrap();
        let typed = "DECISION: cut scope to MVP; revisit auth next sprint";
        db.set_manual_notes(mid, typed).unwrap();

        // Helper mirroring the EXACT finalize fold seam (no blank — the buffer stays durable).
        let finalize = |generated: &str| {
            let manual_notes = db.get_manual_notes(mid).unwrap();
            let markdown = fold_manual_notes(generated, &manual_notes);
            db.upsert_note(&NoteRecord {
                meeting_id: mid.to_string(),
                provider_id: "claude_code".to_string(),
                markdown,
                created_at: "2026-06-30T09:11:00Z".to_string(),
                exported_path: None,
                model_requested: None,
                model_served: None,
                gateway_host: None,
            })
            .unwrap();
        };

        // First summarize: the note carries the typed notes under `## My notes`.
        finalize("---\ntitle: Roadmap\n---\n# Roadmap\n\nBody.");
        let note = db
            .get_note(mid, "claude_code")
            .unwrap()
            .expect("note saved");
        assert!(
            note.markdown.contains("## My notes"),
            "folded section present: {}",
            note.markdown
        );
        assert!(
            note.markdown.contains(typed),
            "typed notes folded into the note"
        );
        // The buffer is DURABLE — still holds the canonical typed notes (not blanked).
        assert_eq!(
            db.get_manual_notes(mid).unwrap(),
            typed,
            "manual_notes buffer is the durable canonical store"
        );

        // COUNTEREXAMPLE B: Resummarize regenerates a FRESH note (no `## My notes`); the fold re-reads
        // the durable buffer and re-appends it. The typed notes are NOT lost (and not duplicated).
        finalize("---\ntitle: Roadmap v2\n---\n# Roadmap v2\n\nDifferent body.");
        let resummarized = db
            .get_note(mid, "claude_code")
            .unwrap()
            .expect("note saved");
        assert!(
            resummarized.markdown.contains("Roadmap v2"),
            "note regenerated"
        );
        assert!(
            resummarized.markdown.contains("## My notes"),
            "resummarize keeps the typed-notes section"
        );
        assert!(
            resummarized.markdown.contains(typed),
            "resummarize re-folds the durable typed notes (B fixed)"
        );
        assert_eq!(
            resummarized.markdown.matches("## My notes").count(),
            1,
            "exactly one My notes section (no dup)"
        );

        let _ = std::fs::remove_file(temp_db_path("fold-manual"));
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
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder(mid, folder).unwrap();
    }

    /// Count vec0 rows for a meeting via the GATED public read: if a chunk exists for `mid` AND the
    /// meeting is visible under `unlocked`, the query vector (same text) returns it. Returns true iff
    /// the meeting surfaces in the semantic results.
    fn semantic_finds(db: &crate::storage::Db, mid: &str, unlocked: &HashSet<String>) -> bool {
        let emb = crate::embed::active_embedder();
        let qv = emb
            .embed(std::slice::from_ref(
                &"budget planning hiring quarter".to_string(),
            ))
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
        assert!(
            corpus.contains("id:m-prior"),
            "related prior note must be cited: {corpus}"
        );
        // Self-exclusion: the meeting being summarized is never grounded in itself.
        assert!(
            !corpus.contains("id:m-self"),
            "a note must never be grounded in itself"
        );
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
        assert!(
            ctx.is_none(),
            "no related notes ⇒ related_context = None (graceful no-op)"
        );
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
            unlocked_ctx
                .map(|c| c.contains("id:m-sealed"))
                .unwrap_or(false),
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
