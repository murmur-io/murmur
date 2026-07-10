use std::path::Path;

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::error::{AppError, Result};
use crate::transcribe::types::{Segment, Transcript};

/// Whisper emits segment timestamps in centiseconds (1/100 s). Divide to get seconds.
const CENTISECONDS_PER_SECOND: f64 = 100.0;

/// Decoding profile — selects the latency/quality trade-off for a transcription run.
///
/// The two paths genuinely want different decoders, so we parameterise the ONE
/// `transcribe` implementation with this enum instead of duplicating the body:
///
/// - [`TranscribeQuality::Accurate`] — the BATCH path (`pipeline.rs`, run once after Stop).
///   Beam search + temperature fallback + anti-hallucination thresholds + previous-text
///   conditioning. Slower, but maximises wording/inflection quality — critical for
///   Polish, which is heavily inflected and where greedy decoding hallucinates more.
///
/// - [`TranscribeQuality::Fast`] — the LIVE path (`live.rs` captions) and the
///   VOICE-TRIGGER path (`audio/listener.rs`). Greedy, single-best, no fallback ladder:
///   lowest latency so a caption/wake-word tick stays snappy. These run on short
///   overlapping windows many times per recording, so beam search there would burn CPU
///   for output the user barely reads — quality is not the goal on those paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscribeQuality {
    /// Low-latency greedy decoding for live captions + voice-trigger windows.
    Fast,
    /// High-quality batch decoding for the authoritative post-Stop transcript.
    Accurate,
}

// ── Accurate (batch) decoding constants — see `build_params`. ──
//
// whisper.cpp's defaults already encode the canonical OpenAI-Whisper anti-hallucination
// values; we set them EXPLICITLY so the batch profile is self-documenting and immune to a
// future upstream default change.

/// Beam width for the batch path. 5 = OpenAI Whisper's reference beam size.
const BATCH_BEAM_SIZE: i32 = 5;
/// `patience` is unimplemented in whisper.cpp (v1.7.x); -1.0 keeps its documented default.
const BATCH_BEAM_PATIENCE: f32 = -1.0;
/// Start of the temperature-fallback ladder (greedy/deterministic first pass).
const BATCH_TEMPERATURE: f32 = 0.0;
/// Ladder step: 0.0 → 0.2 → … → 1.0 (6 rungs). Whisper falls back up the ladder only when
/// a decode trips the entropy/logprob gates below.
const BATCH_TEMPERATURE_INC: f32 = 0.2;
/// Entropy (a.k.a. gzip-compression-ratio) gate. whisper.cpp uses token entropy as its
/// analog of OpenAI's `compression_ratio_threshold`; 2.4 is the reference value. Above it,
/// the segment is treated as repetitive/hallucinated and the next temperature rung is tried.
const BATCH_ENTROPY_THOLD: f32 = 2.4;
/// Average-logprob gate; below -1.0 the decode is "low confidence" → temperature fallback.
const BATCH_LOGPROB_THOLD: f32 = -1.0;
/// No-speech probability gate; above 0.6 a segment is treated as silence (not hallucinated).
const BATCH_NO_SPEECH_THOLD: f32 = 0.6;
/// Max tokens of PRIOR text fed back as the decoder prompt (coreference / grammar carry-over).
/// whisper.cpp's own default; set explicitly to document that previous-text conditioning is ON.
const BATCH_N_MAX_TEXT_CTX: i32 = 16384;

// ── Fast (live) decoding constants ──

/// Encoder audio context for the FAST (live) profile ONLY (T1.2). The live loop decodes a
/// ~14 s rolling window, but whisper pads every input to a 30 s encoder pass (1500 frames) —
/// pure waste. Right-size it: `(14/30)*1500 + 128 ≈ 828 → 832` (rounded UP to a multiple of
/// 64 for Metal kernel alignment; > the 768 floor ggerganov calls quality-safe — his measured
/// `(len/30)*1500+128` formula held WER on base.en at ~3.4× encoder speed). 832 frames cover
/// ~16.6 s of audio per encoder pass, ≥ every Fast caller's window (live 14 s, voice-trigger
/// ~2.2 s; a longer manual-capture window decodes in successive passes — whisper advances
/// `seek` from the last decoded segment, so no audio is dropped). The ACCURATE batch profile
/// MUST keep the full context (quality authority) — `audio_ctx_for` returns `None` there.
/// CAVEAT (honest): Polish-neutrality of a reduced audio_ctx is only measured on base.en
/// upstream — gate on the in-house A/B harness (`asr_ab_harness_from_env`).
const LIVE_AUDIO_CTX: i32 = 832;

// Compile-time invariants (clippy `assertions_on_constants` forbids these as runtime asserts):
// a multiple of 64 (Metal kernel alignment) and above the 768 quality floor.
const _: () = assert!(LIVE_AUDIO_CTX % 64 == 0 && LIVE_AUDIO_CTX > 768);

/// Which encoder `audio_ctx` a [`TranscribeQuality`] profile sets: the Fast/live profile
/// right-sizes to [`LIVE_AUDIO_CTX`]; the Accurate/batch profile sets NOTHING (whisper's full
/// 30 s context — the quality authority is never truncated). Pure — the headless-testable seam
/// for the profile contract (`FullParams` exposes no getter to assert on directly).
fn audio_ctx_for(quality: TranscribeQuality) -> Option<i32> {
    match quality {
        TranscribeQuality::Fast => Some(LIVE_AUDIO_CTX),
        TranscribeQuality::Accurate => None,
    }
}

/// Wraps a loaded whisper.cpp model (Metal). Construct once; reuse per transcription.
///
/// `WhisperContext` is `Send + Sync`, so a single `Transcriber` can live in shared state
/// and be reused across meetings. Each `transcribe` call creates its own short-lived
/// `WhisperState` so concurrent calls do not interfere.
pub struct Transcriber {
    ctx: WhisperContext,
}

impl Transcriber {
    /// Load a GGUF model from `model_path`. Errors if the file is missing or load fails.
    ///
    /// The Metal backend is selected at compile time via the `metal` cargo feature
    /// (see `Cargo.toml`), so no runtime flag is required here — whisper.cpp picks up
    /// the GPU automatically on Apple Silicon.
    pub fn load(model_path: &Path) -> Result<Self> {
        if !model_path.is_file() {
            return Err(AppError::Transcribe(format!(
                "whisper model not found at {}",
                model_path.display()
            )));
        }
        let path_str = model_path
            .to_str()
            .ok_or_else(|| AppError::Transcribe("whisper model path is not valid UTF-8".into()))?;

        // Disable ggml-metal residency sets BEFORE the Metal device is created (read live in
        // `ggml_metal_device_init`). On macOS 15+/Apple-silicon the residency-set teardown asserts
        // `[rsets->data count] == 0` at device free (`ggml_metal_rsets_free`, GGML_ASSERT — NOT
        // NDEBUG-gated, so it aborts in release too) and `ggml_abort`s the process at whisper's
        // per-transcription Metal free. Setting `GGML_METAL_NO_RESIDENCY` makes ggml skip the
        // residency-set collection entirely (`dev->rsets = nil`), bypassing the assert with no
        // effect on transcription output (residency sets are a pure GPU-memory residency hint).
        // Idempotent mirror of the process-entry guard in `lib::run` — also covers test/bench and
        // any non-`run()` caller that loads a model. Set before `new_with_params` creates the
        // WhisperContext (and thus the Metal device).
        std::env::set_var("GGML_METAL_NO_RESIDENCY", "1");

        // FLASH ATTENTION ON (T1.1, 2026-07-09 transcription-performance plan): upstream
        // whisper.cpp 1.8.3 defaults `flash_attn = true`; whisper-rs 0.16's Rust-side
        // `WhisperContextParameters::default()` still carries the OLD `false` — silently
        // overriding upstream. Measured upstream (PR #2152, M1 Pro Metal): encoder ~13–21%
        // faster, decoder ~10–20%, at equal output on the tested sets. We don't use DTW token
        // timestamps, so FA's DTW conflict is moot. CAVEAT (honest): one unconfirmed report of
        // FA slightly changing Japanese output (whisper.cpp #3020) — Polish quality under FA is
        // unpublished; gate any doubt on the in-house A/B harness (`asr_ab_harness_from_env`).
        let mut ctx_params = WhisperContextParameters::default();
        ctx_params.flash_attn(true);
        let ctx = WhisperContext::new_with_params(path_str, ctx_params)
            .map_err(|e| AppError::Transcribe(format!("failed to load whisper model: {e}")))?;

        tracing::info!(target: "transcribe", model = %model_path.display(), "whisper model loaded");
        Ok(Self { ctx })
    }

    /// Transcribe 16 kHz mono f32 samples at the [`TranscribeQuality::Fast`] profile.
    ///
    /// `lang = Some("en")`/`Some("pl")` forces a language; `None` lets whisper auto-detect.
    /// This is the low-latency greedy path used by live captions + the voice trigger. The
    /// batch pipeline calls [`Transcriber::transcribe_with`] with
    /// [`TranscribeQuality::Accurate`] instead.
    pub fn transcribe(&self, samples_16k_mono: &[f32], lang: Option<&str>) -> Result<Transcript> {
        self.transcribe_with(samples_16k_mono, lang, TranscribeQuality::Fast)
    }

    /// Transcribe 16 kHz mono f32 samples at an explicit [`TranscribeQuality`] profile.
    ///
    /// `lang = Some("en")` forces a language; `None` lets whisper auto-detect.
    pub fn transcribe_with(
        &self,
        samples_16k_mono: &[f32],
        lang: Option<&str>,
        quality: TranscribeQuality,
    ) -> Result<Transcript> {
        if samples_16k_mono.is_empty() {
            return Ok(Transcript {
                full_text: String::new(),
                segments: Vec::new(),
                language: lang.map(str::to_string),
            });
        }

        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| AppError::Transcribe(format!("create whisper state: {e}")))?;

        let mut params = build_params(quality);
        // `None` => auto-detect language; `Some("en")`/`Some("pl")` => force it. Forcing
        // "pl" (the global Polish option) skips language detection and biases the decoder's
        // token priors toward Polish — the single biggest win for Polish quality.
        params.set_language(lang);
        // Silence whisper.cpp's own stdout chatter — we surface progress via events.
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state
            .full(params, samples_16k_mono)
            .map_err(|e| AppError::Transcribe(format!("whisper inference failed: {e}")))?;

        // whisper-rs 0.16: `full_n_segments` returns the count directly (no Result), and
        // per-segment data is read through a `WhisperSegment` handle.
        let n_segments = state.full_n_segments();

        let mut segments = Vec::with_capacity(n_segments.max(0) as usize);
        let mut full_text = String::new();

        for i in 0..n_segments {
            let segment = state
                .get_segment(i)
                .ok_or_else(|| AppError::Transcribe(format!("segment {i} out of bounds")))?;
            // Lossy decode tolerates the rare invalid-UTF8 byte rather than failing the run.
            let text = segment
                .to_str_lossy()
                .map_err(|e| AppError::Transcribe(format!("read segment {i} text: {e}")))?;
            let t0 = segment.start_timestamp();
            let t1 = segment.end_timestamp();

            // ASR CONFIDENCE (Tier 3b/A) — ONLY on the `Accurate` batch path (the authoritative,
            // persisted transcript). The `Fast` live/voice-trigger captions are throwaway (greedy
            // best_of:1) so they leave `confidence = None`. Confidence = the mean of whisper's
            // per-token LINEAR probabilities (`token_probability` = `whisper_full_get_token_p`),
            // DISCOUNTED by the segment's no-speech probability so a confident-but-likely-silence
            // decode scores low. All getters return plain `f32` / `Option` (`get_token`) — no
            // fallible call, no `unwrap`, no panic; a segment with no readable tokens yields `None`
            // rather than a wrong-but-confident value. HONEST SCOPE: the wiring ships here, but the
            // real confidence VALUES + the `LOW_CONFIDENCE_P` threshold calibration need a loaded
            // GGUF + Metal on a signed Mac — `cargo test --lib` cannot exercise a real decode.
            let confidence = if quality == TranscribeQuality::Accurate {
                let n_tok = segment.n_tokens();
                if n_tok > 0 {
                    let mut sum = 0.0_f32;
                    let mut count = 0_u32;
                    for tk in 0..n_tok {
                        if let Some(token) = segment.get_token(tk) {
                            sum += token.token_probability();
                            count += 1;
                        }
                    }
                    if count > 0 {
                        let mean_p = sum / count as f32;
                        let no_speech = segment.no_speech_probability().clamp(0.0, 1.0);
                        Some((mean_p * (1.0 - no_speech)).clamp(0.0, 1.0))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            let trimmed = text.trim();
            if !full_text.is_empty() && !trimmed.is_empty() {
                full_text.push(' ');
            }
            full_text.push_str(trimmed);

            segments.push(Segment {
                idx: i as i64,
                start_s: t0 as f64 / CENTISECONDS_PER_SECOND,
                end_s: t1 as f64 / CENTISECONDS_PER_SECOND,
                text: trimmed.to_string(),
                // The transcriber is stream-agnostic: speaker attribution ("me"/"others") is
                // assigned by the wall-clock merge in `audio::merge` from which stream produced
                // these segments, not here. Live/voice-trigger callers leave it as `None`.
                speaker: None,
                confidence,
            });
        }

        Ok(Transcript {
            full_text,
            segments,
            language: lang.map(str::to_string),
        })
    }

    /// Convenience: transcribe a 16 kHz mono WAV file at `wav_path`.
    ///
    /// Reads the WAV with `hound`, normalises samples to mono f32 in `[-1.0, 1.0]`, and
    /// delegates to [`Transcriber::transcribe`]. The file MUST already be 16 kHz (the
    /// audio module writes WAVs at `audio::wav::TARGET_RATE_HZ`); resampling is the audio
    /// module's responsibility, so a non-16 kHz file is rejected rather than silently
    /// mis-transcribed.
    pub fn transcribe_wav(&self, wav_path: &Path, lang: Option<&str>) -> Result<Transcript> {
        let samples = read_wav_16k_mono(wav_path)?;
        self.transcribe(&samples, lang)
    }
}

/// Build the decoder [`FullParams`] for a given [`TranscribeQuality`] profile.
///
/// Only the sampling strategy + decoding-hygiene knobs differ here; the caller still sets
/// `language` and silences the print flags. Keeping this in ONE place is why batch vs live
/// can diverge without duplicating the `transcribe` body.
fn build_params<'a>(quality: TranscribeQuality) -> FullParams<'a, 'a> {
    match quality {
        // LIVE captions + VOICE TRIGGER: keep greedy / best_of:1. These run on short,
        // overlapping windows every couple of seconds, so latency dominates — beam search +
        // a 6-rung temperature ladder would multiply the per-tick cost for output the user
        // only glances at. Quality is the batch path's job, NOT the live path's. Do NOT add
        // beam search here. `audio_ctx` is right-sized to the live window (see LIVE_AUDIO_CTX);
        // the Accurate arm deliberately never sets it.
        TranscribeQuality::Fast => {
            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            if let Some(audio_ctx) = audio_ctx_for(quality) {
                params.set_audio_ctx(audio_ctx);
            }
            params
        }

        // BATCH (post-Stop authoritative transcript): the anti-hallucination + inflection
        // levers. Beam search explores multiple hypotheses (better wording/grammar — matters
        // for heavily-inflected Polish); the temperature ladder + entropy/logprob/no-speech
        // gates let whisper retry a segment at a higher temperature when a decode looks
        // repetitive or low-confidence (the classic anti-hallucination loop); previous-text
        // conditioning carries grammar/coreference across segment boundaries.
        TranscribeQuality::Accurate => {
            let mut params = FullParams::new(SamplingStrategy::BeamSearch {
                beam_size: BATCH_BEAM_SIZE,
                patience: BATCH_BEAM_PATIENCE,
            });
            // Temperature-fallback ladder: 0.0 → +0.2 → … → 1.0.
            params.set_temperature(BATCH_TEMPERATURE);
            params.set_temperature_inc(BATCH_TEMPERATURE_INC);
            // Anti-hallucination gates that trigger the next ladder rung.
            params.set_entropy_thold(BATCH_ENTROPY_THOLD);
            params.set_logprob_thold(BATCH_LOGPROB_THOLD);
            params.set_no_speech_thold(BATCH_NO_SPEECH_THOLD);
            // Previous-text conditioning ON: feed prior decoded text back as the prompt.
            // whisper.cpp's flag is `no_context` (inverted) and defaults to TRUE (conditioning
            // OFF), so we MUST flip it to false to keep coreference/grammar carry-over.
            params.set_no_context(false);
            params.set_n_max_text_ctx(BATCH_N_MAX_TEXT_CTX);
            params
        }
    }
}

/// Read a 16 kHz WAV file into mono f32 samples in `[-1.0, 1.0]`.
///
/// Supports 16-bit integer PCM (what `audio/wav.rs` writes) and 32-bit float WAVs.
/// Multi-channel input is down-mixed to mono by averaging channels. A sample rate other
/// than 16 kHz is an error — see [`Transcriber::transcribe_wav`].
fn read_wav_16k_mono(wav_path: &Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(wav_path)
        .map_err(|e| AppError::Transcribe(format!("open wav {}: {e}", wav_path.display())))?;
    let spec = reader.spec();

    if spec.sample_rate != 16_000 {
        return Err(AppError::Transcribe(format!(
            "wav must be 16 kHz mono for whisper, got {} Hz",
            spec.sample_rate
        )));
    }

    let channels = spec.channels.max(1) as usize;

    // Decode interleaved samples to f32, then down-mix to mono.
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            // Normalise by the full-scale value for the given bit depth.
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| {
                    s.map(|v| v as f32 / max)
                        .map_err(|e| AppError::Transcribe(format!("decode wav sample: {e}")))
                })
                .collect::<Result<Vec<f32>>>()?
        }
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.map_err(|e| AppError::Transcribe(format!("decode wav sample: {e}"))))
            .collect::<Result<Vec<f32>>>()?,
    };

    if channels <= 1 {
        return Ok(interleaved);
    }

    let mono = interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    Ok(mono)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_profiles_are_distinct_and_copy() {
        // `Copy` lets call sites pass the profile by value without ceremony.
        let q = TranscribeQuality::Accurate;
        let _also = q;
        assert_ne!(TranscribeQuality::Fast, TranscribeQuality::Accurate);
    }

    #[test]
    fn build_params_constructs_both_profiles() {
        // `whisper_full_default_params` is a pure C struct-filler — no model/context needed —
        // so this exercises the batch setter chain (beam search + thresholds + no_context)
        // and the live greedy path without GPU work. A panic/UB here would fail the test.
        let _fast = build_params(TranscribeQuality::Fast);
        let _accurate = build_params(TranscribeQuality::Accurate);
    }

    #[test]
    fn batch_constants_match_whispercpp_reference_values() {
        // Guards against an accidental drift away from the OpenAI-Whisper reference hygiene.
        assert_eq!(BATCH_BEAM_SIZE, 5);
        assert_eq!(BATCH_TEMPERATURE, 0.0);
        assert_eq!(BATCH_TEMPERATURE_INC, 0.2);
        assert_eq!(BATCH_ENTROPY_THOLD, 2.4);
        assert_eq!(BATCH_LOGPROB_THOLD, -1.0);
        assert_eq!(BATCH_NO_SPEECH_THOLD, 0.6);
    }

    /// T1.2 — the profile contract: ONLY the Fast/live profile right-sizes the encoder
    /// context; the Accurate batch authority keeps whisper's full 30 s context.
    #[test]
    fn fast_profile_sets_live_audio_ctx_accurate_does_not() {
        assert_eq!(audio_ctx_for(TranscribeQuality::Fast), Some(832));
        assert_eq!(audio_ctx_for(TranscribeQuality::Accurate), None);
    }

    /// T1.2 — the 832 derivation: `(14/30)*1500 + 128 ≈ 828`, rounded UP to a multiple of 64,
    /// and above the 768 quality floor (ggerganov guidance).
    #[test]
    fn live_audio_ctx_is_aligned_and_above_the_quality_floor() {
        // Alignment + quality-floor invariants live as a compile-time `const _: () = assert!(…)`
        // beside LIVE_AUDIO_CTX (clippy assertions_on_constants). Here: the non-constant derivation.
        let derived = (14.0_f64 / 30.0 * 1500.0 + 128.0).ceil() as i32; // 828
        assert!(
            LIVE_AUDIO_CTX >= derived && LIVE_AUDIO_CTX - derived < 64,
            "832 is the next multiple of 64 above the derived {derived}"
        );
    }

    /// T0.3 — env-driven A/B harness: decode the SAME WAV through TWO models at BOTH profiles
    /// and print wall-clock per leg + write each transcript to a temp file for diffing. The
    /// instrument behind every speed/Polish-quality claim of the 2026-07-09 transcription plan
    /// (flash-attn, audio_ctx=832, quants, live pin): record a fixed ~10-min PL/EN WAV once,
    /// then run e.g.
    ///   MURMUR_ASR_AB_WAV=~/asr/meeting-16k.wav \
    ///   MURMUR_ASR_AB_MODEL_A=~/Library/Application\ Support/MeetNotes/models/ggml-small.bin \
    ///   MURMUR_ASR_AB_MODEL_B=.../ggml-large-v3-turbo-q8_0.bin \
    ///   cargo test --lib asr_ab_harness_from_env -- --ignored --nocapture
    /// PANICS NEVER: missing env / bad paths / decode failures print a message and return
    /// (follows the `run_bakeoff_over_real_db_from_env` env-driven `#[ignore]` pattern, but
    /// skip-soft). Needs a real Mac + loaded GGUFs — headless `cargo test` proves nothing here.
    #[test]
    #[ignore = "real A/B: needs MURMUR_ASR_AB_WAV + MURMUR_ASR_AB_MODEL_A/B on a Mac with Metal"]
    fn asr_ab_harness_from_env() {
        let Ok(wav) = std::env::var("MURMUR_ASR_AB_WAV") else {
            eprintln!("SKIP: set MURMUR_ASR_AB_WAV to a 16 kHz mono WAV path");
            return;
        };
        let Ok(model_a) = std::env::var("MURMUR_ASR_AB_MODEL_A") else {
            eprintln!("SKIP: set MURMUR_ASR_AB_MODEL_A to a ggml model path");
            return;
        };
        let Ok(model_b) = std::env::var("MURMUR_ASR_AB_MODEL_B") else {
            eprintln!("SKIP: set MURMUR_ASR_AB_MODEL_B to a ggml model path");
            return;
        };
        let lang = std::env::var("MURMUR_ASR_AB_LANG").ok();
        let samples = match read_wav_16k_mono(Path::new(&wav)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("SKIP: could not read WAV: {e}");
                return;
            }
        };
        eprintln!(
            "A/B over {:.1}s of audio (lang={})",
            samples.len() as f64 / 16_000.0,
            lang.as_deref().unwrap_or("auto")
        );
        for (leg, model_path) in [("A", &model_a), ("B", &model_b)] {
            let transcriber = match Transcriber::load(Path::new(model_path)) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("SKIP leg {leg}: model load failed: {e}");
                    continue;
                }
            };
            for quality in [TranscribeQuality::Fast, TranscribeQuality::Accurate] {
                let started = std::time::Instant::now();
                match transcriber.transcribe_with(&samples, lang.as_deref(), quality) {
                    Ok(t) => {
                        let wall = started.elapsed();
                        let out = std::env::temp_dir()
                            .join(format!("murmur-asr-ab-{leg}-{quality:?}.txt"));
                        if let Err(e) = std::fs::write(&out, &t.full_text) {
                            eprintln!("leg {leg} {quality:?}: transcript write failed: {e}");
                        }
                        eprintln!(
                            "leg {leg} {quality:?}: wall {:.2}s, {} segments, {} chars → {}",
                            wall.as_secs_f64(),
                            t.segments.len(),
                            t.full_text.chars().count(),
                            out.display()
                        );
                    }
                    Err(e) => eprintln!("leg {leg} {quality:?}: decode failed: {e}"),
                }
            }
        }
    }
}
