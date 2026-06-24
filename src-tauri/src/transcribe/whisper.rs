use std::path::Path;

use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
};

use crate::error::{AppError, Result};
use crate::transcribe::types::{Segment, Transcript};

/// Whisper emits segment timestamps in centiseconds (1/100 s). Divide to get seconds.
const CENTISECONDS_PER_SECOND: f64 = 100.0;

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
        let path_str = model_path.to_str().ok_or_else(|| {
            AppError::Transcribe("whisper model path is not valid UTF-8".into())
        })?;

        let ctx = WhisperContext::new_with_params(path_str, WhisperContextParameters::default())
            .map_err(|e| AppError::Transcribe(format!("failed to load whisper model: {e}")))?;

        tracing::info!(target: "transcribe", model = %model_path.display(), "whisper model loaded");
        Ok(Self { ctx })
    }

    /// Transcribe 16 kHz mono f32 samples. `lang = Some("en")` forces a language;
    /// `None` lets whisper auto-detect.
    pub fn transcribe(&self, samples_16k_mono: &[f32], lang: Option<&str>) -> Result<Transcript> {
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

        // Greedy sampling is the fast, deterministic default for batch transcription.
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        // `None` => auto-detect language; `Some("en")` => force it.
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
            let segment = state.get_segment(i).ok_or_else(|| {
                AppError::Transcribe(format!("segment {i} out of bounds"))
            })?;
            // Lossy decode tolerates the rare invalid-UTF8 byte rather than failing the run.
            let text = segment
                .to_str_lossy()
                .map_err(|e| AppError::Transcribe(format!("read segment {i} text: {e}")))?;
            let t0 = segment.start_timestamp();
            let t1 = segment.end_timestamp();

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
