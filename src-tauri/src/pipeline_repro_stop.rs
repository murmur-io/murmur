//! EMPIRICAL REPRO HARNESS (2026-07-16) — "stop_recording pipeline dies silently on long
//! recordings with large whisper models".
//!
//! Production symptom (release app, ~20-min dual-stream recording, ggml-large-v3-turbo-q8_0):
//! after Stop the `stop_recording` command future dies silently — spindump shows the process
//! alive but fully idle (all tokio workers parked, zero whisper/ggml/Metal frames), the meeting
//! row stuck at RECORDING and "TOTAL TIME 0s" in the UI, i.e. death BEFORE
//! `state.db.finalize_meeting` (pipeline.rs, stage 1) and long before
//! `MeetingStatus::Transcribed`. A panic anywhere on the async section of `run_inner` unwinds
//! into the tauri/tokio task, is swallowed, and the invoke Promise never settles — exactly this
//! shape. (A panic INSIDE the `perf::run_heavy` closure would instead surface as
//! `AppError::Other("heavy inference task panicked")` → status Error, which is NOT what the user
//! sees — more evidence the death is in the async stage-1 code or the future was dropped.)
//!
//! These `#[ignore]` env-driven tests exercise the REAL post-Stop compute path (the exact
//! functions `run_inner` calls, in its exact order, with the DEFAULT release config values)
//! on LONG real audio, WITHOUT a Tauri `AppHandle`/DB (the `emit_status`/DB writes are the only
//! bypassed pieces — they are not the compute). They follow the crate's env-driven `#[ignore]`
//! precedent (`asr_ab_harness_from_env`, `live_duty_cycle_sim_from_env`): missing env ⇒
//! skip-soft, never a spurious red.
//!
//! Env (all paths must be COPIES — never the live app's files):
//!   MURMUR_REPRO_MIC_WAV   path to the mic master WAV (any rate/format hound reads)
//!   MURMUR_REPRO_SYS_WAV   path to the system-audio master WAV
//!   MURMUR_REPRO_TILE      how many times to tile (concatenate) each stream (default 4)
//!   MURMUR_REPRO_MODEL     ggml whisper model path (ASR test only)
//!   MURMUR_REPRO_LANG      optional forced language ("pl"/"en"); unset = auto (config default)
//!   MURMUR_REPRO_OUT       scratch dir for written WAVs (default: std::env::temp_dir())
//!   MURMUR_REPRO_TIMEBOX_MIN  per-stream ASR wall-clock timebox in minutes (default 40)
//!
//! Run with `--ignored --nocapture` and `RUST_BACKTRACE=full`; each test installs a panic hook
//! that prints the payload + location + a forced backtrace so a swallowed-in-production panic
//! becomes VISIBLE here.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::decode_windows;
use crate::audio;
use crate::audio::merge::{merge_streams, StreamInput, SPEAKER_ME, SPEAKER_OTHERS};
use crate::error::Result;
use crate::transcribe::{Transcriber, TranscribeQuality};

/// Print panics loudly: payload + location (via `info`'s Display) + a FORCED backtrace
/// (independent of RUST_BACKTRACE). In production this panic is swallowed by tokio; here it
/// becomes the primary evidence.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        eprintln!("\n================ PANIC CAPTURED BY REPRO HARNESS ================");
        eprintln!("{info}");
        eprintln!("--- forced backtrace ---\n{bt}");
        eprintln!("==================================================================\n");
    }));
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name).ok().map(PathBuf::from)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn out_dir() -> PathBuf {
    env_path("MURMUR_REPRO_OUT").unwrap_or_else(std::env::temp_dir)
}

/// Concatenate `samples` with itself `n` times (n>=1) — simulates a long recording with REAL
/// speech content (the user's failing case is ~4× the available 5-min masters).
fn tile(samples: &[f32], n: usize) -> Vec<f32> {
    let n = n.max(1);
    let mut out = Vec::with_capacity(samples.len() * n);
    for _ in 0..n {
        out.extend_from_slice(samples);
    }
    out
}

/// Stage timer: prints start + elapsed around `f` so the last-printed stage pinpoints where a
/// panic/hang happened.
fn stage<T>(name: &str, f: impl FnOnce() -> T) -> T {
    eprintln!("[stage] {name} …");
    let t0 = Instant::now();
    let out = f();
    eprintln!("[stage] {name} done in {:.2}s", t0.elapsed().as_secs_f64());
    out
}

struct LoadedStreams {
    /// Tiled mic at its native rate (the `samples` arg of `run_after_stop`).
    mic_native: Vec<f32>,
    mic_rate: u32,
    /// Tiled system-audio at its native rate (what `read_wav_mono(system_wav)` yields).
    sys_native: Vec<f32>,
    sys_rate: u32,
}

/// Load + tile both source WAVs through the REAL `audio::read_wav_mono` (the same hound decode
/// `run_inner` uses for the system scratch WAV). Skip-soft (None) when env is missing/unreadable.
fn load_streams() -> Option<LoadedStreams> {
    let Some(mic_path) = env_path("MURMUR_REPRO_MIC_WAV") else {
        eprintln!("SKIP: set MURMUR_REPRO_MIC_WAV to a mic WAV COPY");
        return None;
    };
    let Some(sys_path) = env_path("MURMUR_REPRO_SYS_WAV") else {
        eprintln!("SKIP: set MURMUR_REPRO_SYS_WAV to a system-audio WAV COPY");
        return None;
    };
    let tile_n = env_usize("MURMUR_REPRO_TILE", 4);

    let (mic_raw, mic_rate) = match audio::read_wav_mono(&mic_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("SKIP: mic WAV unreadable: {e}");
            return None;
        }
    };
    let (sys_raw, sys_rate) = match audio::read_wav_mono(&sys_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("SKIP: sys WAV unreadable: {e}");
            return None;
        }
    };
    eprintln!(
        "[input] mic: {} samples @ {} Hz ({:.1}s) | sys: {} samples @ {} Hz ({:.1}s) | tile ×{}",
        mic_raw.len(),
        mic_rate,
        mic_raw.len() as f64 / mic_rate as f64,
        sys_raw.len(),
        sys_rate,
        sys_raw.len() as f64 / sys_rate as f64,
        tile_n
    );
    let mic_native = tile(&mic_raw, tile_n);
    let sys_native = tile(&sys_raw, tile_n);
    eprintln!(
        "[input] tiled: mic {:.1}s | sys {:.1}s",
        mic_native.len() as f64 / mic_rate as f64,
        sys_native.len() as f64 / sys_rate as f64,
    );
    Some(LoadedStreams {
        mic_native,
        mic_rate,
        sys_native,
        sys_rate,
    })
}

/// TEST A — the EARLY stage of `run_inner` (everything BEFORE the `perf::run_heavy` ASR
/// closure) on long buffers, in `run_inner`'s exact call order with DEFAULT release config
/// (vad_enabled=true → normalize_for_asr; post_aec_enabled=false → no offline AEC; no AEC
/// sidecar feed; keep_hires_masters=true mirrored because the user's masters exist on disk —
/// the source WAVs of this repro ARE those masters). Spindump evidence favors death in exactly
/// this region (before `finalize_meeting` → "TOTAL TIME 0s" + status RECORDING).
#[test]
#[ignore = "empirical repro: needs MURMUR_REPRO_MIC_WAV + MURMUR_REPRO_SYS_WAV; compute-heavy"]
fn repro_a_early_stage_on_long_buffers() {
    install_panic_hook();
    let Some(s) = load_streams() else { return };
    let total = Instant::now();

    // run_inner stage 1, verbatim order:
    // 1. mic resample (samples arrive at the recorder's native rate).
    let mut mic_16k = stage("resample mic → 16k", || {
        audio::resample_to_16k(&s.mic_native, s.mic_rate)
    })
    .expect("mic resample returned AppError");
    // keep_hires_masters=true ⇒ `samples` is RETAINED (not dropped) — mirrored by keeping
    // s.mic_native alive to the end of this test.

    // (aec_mic_wav = None in the default path — no VPIO feed swap.)

    // 2. system stream read (done above via the same read_wav_mono) + resample.
    let sys_16k = stage("resample sys → 16k", || {
        audio::resample_to_16k(&s.sys_native, s.sys_rate)
    })
    .expect("sys resample returned AppError");

    // 3. mic↔system offset + leak estimate (raw mic probe — no AEC feed in the default path).
    let leak = stage("estimate_stream_offset", || {
        audio::align::estimate_stream_offset(&mic_16k, &sys_16k)
    });
    match &leak {
        Some(l) => eprintln!(
            "[leak] offset_s={:.3} correlation={:.3}",
            l.offset_s, l.correlation
        ),
        None => eprintln!("[leak] none (headphones / unreliable)"),
    }

    // post_aec_enabled=false (default) ⇒ offline AEC skipped.

    // 4. archive delays + aligned mix. Anchor instants mirror the real recorder/sidecar start
    //    delta (~hundreds of ms; the sidecar spawns after the mic).
    let base = Instant::now();
    let mic_started = base;
    let sys_started = base + Duration::from_millis(300);
    let (mic_delay, sys_delay) = audio::align::archive_delays(
        leak.as_ref(),
        mic_started,
        Some(sys_started),
        audio::TARGET_RATE_HZ,
    );
    eprintln!("[mix] mic_delay={mic_delay} sys_delay={sys_delay}");
    let archive_16k = stage("mix_aligned (archive)", || {
        audio::mix_aligned(&mic_16k, mic_delay, &sys_16k, sys_delay)
    });

    // 5. write the archive WAV (same writer run_inner uses).
    let wav_path = out_dir().join("murmur-repro-archive.wav");
    stage("write_wav_16k_mono (archive)", || {
        audio::write_wav_16k_mono(&wav_path, &archive_16k, audio::TARGET_RATE_HZ)
    })
    .expect("archive WAV write returned AppError");
    // (state.db.finalize_meeting happens HERE in run_inner — DB write, not compute.)

    // 6. keep_hires_masters=true parity: the pre-resample master writes.
    let mic_master = out_dir().join("murmur-repro-mic-master.wav");
    stage("write_wav_f32 (mic master)", || {
        audio::write_wav_f32(&mic_master, &s.mic_native, s.mic_rate, 1)
    })
    .expect("mic master write returned AppError");
    let sys_master = out_dir().join("murmur-repro-sys-master.wav");
    stage("write_wav_f32 (sys master)", || {
        audio::write_wav_f32(&sys_master, &s.sys_native, s.sys_rate, 1)
    })
    .expect("sys master write returned AppError");

    // 7. vad_enabled=true (default) ⇒ ASR-feed loudness normalization.
    let mut sys_16k = sys_16k;
    stage("normalize_for_asr (both feeds)", || {
        audio::normalize_for_asr(&mut mic_16k);
        audio::normalize_for_asr(&mut sys_16k);
    });

    // Clean up the written scratch WAVs (plaintext hygiene).
    let _ = std::fs::remove_file(&wav_path);
    let _ = std::fs::remove_file(&mic_master);
    let _ = std::fs::remove_file(&sys_master);

    println!(
        "EARLY_STAGE_RESULT ok=1 total_s={:.1} mic_16k={} sys_16k={} archive={} leak={}",
        total.elapsed().as_secs_f64(),
        mic_16k.len(),
        sys_16k.len(),
        archive_16k.len(),
        leak.is_some() as u8,
    );
}

/// MIRROR of the private `pipeline::transcribe_stream` — the SAME calls in the SAME order
/// (`VadSegmenter::speech_regions` → `decode_windows` (120 s cap) → per-window
/// `Transcriber::transcribe_with(.., Accurate)` → idx/offset re-stamp), with three additions
/// that change no compute: per-window eprintln timing, a wall-clock TIMEBOX checked between
/// windows, and a heartbeat counter so an intra-window wedge is visible from the monitor
/// thread. Keep in lock-step with `transcribe_stream` (pipeline.rs).
#[allow(clippy::too_many_arguments)]
fn transcribe_stream_instrumented(
    name: &str,
    transcriber: &Transcriber,
    vad: Option<&mut crate::transcribe::vad::VadSegmenter>,
    samples_16k: &[f32],
    lang: Option<&str>,
    timebox: Duration,
    window_counter: &AtomicUsize,
) -> Result<Vec<crate::transcribe::types::Segment>> {
    // `MAX_WINDOW_S` is a fn-local const inside the real `transcribe_stream`; mirrored here.
    const MAX_WINDOW_S: usize = 120;
    let window_len = MAX_WINDOW_S * audio::TARGET_RATE_HZ as usize;

    let started = Instant::now();
    let regions: Vec<(usize, usize)> = match vad {
        Some(v) => stage(&format!("{name}: VAD speech_regions"), || {
            v.speech_regions(samples_16k)
        })?,
        None => vec![(0, samples_16k.len())],
    };
    let speech_s: f64 = regions
        .iter()
        .map(|(a, b)| (b - a) as f64 / audio::TARGET_RATE_HZ as f64)
        .sum();
    eprintln!(
        "[{name}] {} speech regions, {:.1}s speech of {:.1}s audio",
        regions.len(),
        speech_s,
        samples_16k.len() as f64 / audio::TARGET_RATE_HZ as f64
    );

    let mut out: Vec<crate::transcribe::types::Segment> = Vec::new();
    let mut idx: i64 = 0;
    let mut win_no = 0usize;
    for (start, end) in regions {
        for (win_start, win_end) in decode_windows(start, end, window_len) {
            win_no += 1;
            window_counter.store(win_no, Ordering::SeqCst);
            if started.elapsed() > timebox {
                eprintln!(
                    "[{name}] TIMEBOX EXCEEDED after {:.0}s at window {win_no} — aborting this stream (CRITICAL data point: processing time >> recording length)",
                    started.elapsed().as_secs_f64()
                );
                return Err(crate::error::AppError::Transcribe(format!(
                    "repro timebox exceeded on {name} at window {win_no}"
                )));
            }
            let w0 = Instant::now();
            let offset_s = win_start as f64 / audio::TARGET_RATE_HZ as f64;
            let tx = transcriber.transcribe_with(
                &samples_16k[win_start..win_end],
                lang,
                TranscribeQuality::Accurate,
            )?;
            let win_audio_s = (win_end - win_start) as f64 / audio::TARGET_RATE_HZ as f64;
            let wall = w0.elapsed().as_secs_f64();
            eprintln!(
                "[{name}] window {win_no} [{:.0}s..{:.0}s] ({:.0}s audio): {:.1}s wall (rtf {:.2}), {} segs, total {:.0}s",
                offset_s,
                win_end as f64 / audio::TARGET_RATE_HZ as f64,
                win_audio_s,
                wall,
                wall / win_audio_s.max(0.001),
                tx.segments.len(),
                started.elapsed().as_secs_f64()
            );
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

/// TEST B/C/D — the FULL ASR stage of `run_inner`'s `run_heavy` closure on real long audio:
/// whisper load (flash_attn(true), GGML_METAL_NO_RESIDENCY set inside `Transcriber::load`),
/// Silero VAD load, Accurate-profile decode of BOTH streams, then the real
/// `merge_streams` + `suppress_cross_stream_echo` + `build_transcript_feed`.
/// Diarization is SKIPPED — `diarize_others` defaults to false (settings/config.rs), so the
/// default release path never loads the diarizer. Matrix legs are driven by env:
///   B: TILE=4 + large-v3-turbo-q8_0   C: TILE=1 + large-v3-turbo-q8_0   D: TILE=4 + small
#[test]
#[ignore = "empirical repro: needs MURMUR_REPRO_{MIC,SYS}_WAV + MURMUR_REPRO_MODEL; VERY compute-heavy (Metal)"]
fn repro_b_full_asr_from_env() {
    install_panic_hook();
    let Some(model) = env_path("MURMUR_REPRO_MODEL") else {
        eprintln!("SKIP: set MURMUR_REPRO_MODEL to a ggml whisper model path");
        return;
    };
    let Some(s) = load_streams() else { return };
    let lang_env = std::env::var("MURMUR_REPRO_LANG").ok();
    let lang = lang_env.as_deref(); // None = auto-detect (config.language default)
    let timebox = Duration::from_secs(60 * env_usize("MURMUR_REPRO_TIMEBOX_MIN", 40) as u64);
    let total = Instant::now();

    // Prepare the 16k feeds exactly as run_inner would have by the time the closure runs
    // (resample → leak estimate → normalize; post-AEC off by default).
    let mut mic_16k = stage("resample mic → 16k", || {
        audio::resample_to_16k(&s.mic_native, s.mic_rate)
    })
    .expect("mic resample returned AppError");
    let mut sys_16k = stage("resample sys → 16k", || {
        audio::resample_to_16k(&s.sys_native, s.sys_rate)
    })
    .expect("sys resample returned AppError");
    // Free the native buffers — default config (keep_hires_masters=false) drops `samples`
    // before transcription; parity for the ASR stage's memory shape.
    drop(s);
    let leak = stage("estimate_stream_offset", || {
        audio::align::estimate_stream_offset(&mic_16k, &sys_16k)
    });
    stage("normalize_for_asr (both feeds)", || {
        audio::normalize_for_asr(&mut mic_16k);
        audio::normalize_for_asr(&mut sys_16k);
    });

    // ── the run_heavy closure body ──
    let transcriber = match stage("Transcriber::load", || Transcriber::load(&model)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("ASR_RESULT outcome=ERROR stage=model_load err={e}");
            panic!("model load failed: {e}");
        }
    };
    eprintln!(
        "[env] GGML_METAL_NO_RESIDENCY={:?} (set inside Transcriber::load — parity with the release app)",
        std::env::var("GGML_METAL_NO_RESIDENCY").ok()
    );
    // Silero VAD from the SHARED models dir (models_dir() hard-codes `MeetNotes`, same file the
    // release app resolves via ensure_vad_model). Best-effort → None = whole-buffer decode,
    // mirroring run_inner's warn-and-continue.
    let vad_path = crate::transcribe::model::models_dir()
        .map(|d| d.join(crate::transcribe::model::VAD_MODEL_FILE))
        .ok();
    let mut vad = vad_path.as_deref().and_then(|p| {
        match crate::transcribe::vad::VadSegmenter::load(p) {
            Ok(v) => {
                eprintln!("[vad] Silero loaded");
                Some(v)
            }
            Err(e) => {
                eprintln!("[vad] load failed ({e}); transcribing whole buffer — NOTE: default release config has VAD ON, so this leg diverges");
                None
            }
        }
    });

    // Heartbeat monitor: proves the difference between "slow but alive" and "wedged" — prints
    // the current stream/window every 60 s from a separate thread.
    let done = Arc::new(AtomicBool::new(false));
    let window_counter = Arc::new(AtomicUsize::new(0));
    let stream_label = Arc::new(std::sync::Mutex::new(String::from("mic")));
    {
        let done = done.clone();
        let window_counter = window_counter.clone();
        let stream_label = stream_label.clone();
        let t0 = Instant::now();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(60));
            if done.load(Ordering::SeqCst) {
                break;
            }
            let label = stream_label
                .lock()
                .map(|s| s.clone())
                .unwrap_or_else(|_| "?".into());
            eprintln!(
                "[heartbeat] {:.0}s elapsed — decoding stream={label} window={}",
                t0.elapsed().as_secs_f64(),
                window_counter.load(Ordering::SeqCst)
            );
        });
    }

    // MIC stream (Accurate, VAD-segmented — the exact run_inner call).
    let mic_t0 = Instant::now();
    let mic_audio_s = mic_16k.len() as f64 / audio::TARGET_RATE_HZ as f64;
    let mic_segments = match transcribe_stream_instrumented(
        "mic",
        &transcriber,
        vad.as_mut(),
        &mic_16k,
        lang,
        timebox,
        &window_counter,
    ) {
        Ok(v) => v,
        Err(e) => {
            done.store(true, Ordering::SeqCst);
            eprintln!("ASR_RESULT outcome=ERROR stage=mic_asr err={e}");
            panic!("mic transcription failed: {e}");
        }
    };
    let mic_wall = mic_t0.elapsed().as_secs_f64();
    eprintln!(
        "[mic] DONE {:.1}s wall for {:.1}s audio (rtf {:.2}), {} segments",
        mic_wall,
        mic_audio_s,
        mic_wall / mic_audio_s.max(0.001),
        mic_segments.len()
    );

    // SYS stream.
    if let Ok(mut l) = stream_label.lock() {
        *l = "sys".into();
    }
    window_counter.store(0, Ordering::SeqCst);
    let sys_t0 = Instant::now();
    let sys_audio_s = sys_16k.len() as f64 / audio::TARGET_RATE_HZ as f64;
    let sys_segments = match transcribe_stream_instrumented(
        "sys",
        &transcriber,
        vad.as_mut(),
        &sys_16k,
        lang,
        timebox,
        &window_counter,
    ) {
        Ok(v) => v,
        Err(e) => {
            done.store(true, Ordering::SeqCst);
            eprintln!("ASR_RESULT outcome=ERROR stage=sys_asr err={e}");
            panic!("sys transcription failed: {e}");
        }
    };
    let sys_wall = sys_t0.elapsed().as_secs_f64();
    eprintln!(
        "[sys] DONE {:.1}s wall for {:.1}s audio (rtf {:.2}), {} segments",
        sys_wall,
        sys_audio_s,
        sys_wall / sys_audio_s.max(0.001),
        sys_segments.len()
    );
    done.store(true, Ordering::SeqCst);

    // Free whisper + VAD before merge (run_inner drops them before the diarizer would load).
    drop(transcriber);
    drop(vad);

    // Wall-clock merge + echo dedup + feed build — the real functions, real order.
    let base = Instant::now();
    let mic_started = base;
    let sys_started = base + Duration::from_millis(300);
    let streams = vec![
        StreamInput {
            segments: mic_segments,
            started_at: mic_started,
            speaker: SPEAKER_ME,
        },
        StreamInput {
            segments: sys_segments,
            started_at: sys_started,
            speaker: SPEAKER_OTHERS,
        },
    ];
    let (merged, echo_suppressed) = stage("merge + suppress_cross_stream_echo", || {
        crate::audio::merge::suppress_cross_stream_echo(merge_streams(streams), leak.as_ref())
    });
    let feed = stage("build_transcript_feed", || {
        super::build_transcript_feed(&merged)
    });
    eprintln!(
        "[feed] merged={} echo_suppressed={} summary_chars={} empty={}",
        merged.len(),
        echo_suppressed,
        feed.summary_text.chars().count(),
        feed.summary_text.trim().is_empty() as u8
    );

    let model_name = model
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("custom");
    println!(
        "ASR_RESULT outcome=COMPLETED model={model_name} tile={} lang={} mic_wall_s={:.1} sys_wall_s={:.1} mic_rtf={:.3} sys_rtf={:.3} mic_segs={} sys_segs={} merged={} suppressed={} total_s={:.1}",
        env_usize("MURMUR_REPRO_TILE", 4),
        lang.unwrap_or("auto"),
        mic_wall,
        sys_wall,
        mic_wall / mic_audio_s.max(0.001),
        sys_wall / sys_audio_s.max(0.001),
        merged.iter().filter(|s| s.speaker.as_deref() == Some(SPEAKER_ME)).count(),
        merged.iter().filter(|s| s.speaker.as_deref() == Some(SPEAKER_OTHERS)).count(),
        merged.len(),
        echo_suppressed,
        total.elapsed().as_secs_f64(),
    );
}
