//! Production LIVE-caption ASR engine: NVIDIA parakeet-tdt-0.6b-v3 (int8) via the `sherpa-onnx`
//! crate already in the tree (the diarization crate — see `transcribe::diarize`). Runs the
//! nemo_transducer on CPU, DELIBERATELY OFF Metal (the whole point: the shared GPU stays free for
//! the brain LLM while a meeting is recording). CC-BY-4.0 (attribution in the About/licenses view).
//!
//! This is the SHIPPING counterpart of the `#[ignore]`d `parakeet_spike` harness: it wraps a
//! loaded `sherpa_onnx::OfflineRecognizer` (encoder/decoder/joiner `.int8.onnx` + `tokens.txt`,
//! `num_threads = 4`) behind the [`LiveAsr`] seam so the live loop consumes it engine-agnostically.
//!
//! Scope: LIVE captions ONLY. Whisper is the batch authority — the post-Stop Accurate pipeline is
//! untouched, and the wake/manual-capture paths keep the whisper Fast path. parakeet is single-
//! segment by construction here: its word timestamps are discarded and the whole decode becomes
//! ONE `Segment` (0.0..=duration), which the live loop consumes identically to whisper's segments
//! (it only reads the joined text). Best-effort + crash-safe: sherpa is a safe Rust wrapper over a
//! static onnxruntime (no `msg_send`, no throwing FFI); a null recognizer is a plain `Err`.

use sherpa_onnx::{
    OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
};

use crate::error::{AppError, Result};
use crate::transcribe::live_asr::{LiveAsr, ParakeetModelPaths, ENGINE_PARAKEET};
use crate::transcribe::types::{Segment, Transcript};

/// CPU thread budget for the parakeet decode. 4 matches the E-core budget the live loop's QoS
/// targets (`set_utility_qos`) and the spike's measured 13× realtime — enough headroom on the 3 s
/// live tick while leaving cores for the mic capture + the brain.
const PARAKEET_NUM_THREADS: i32 = 4;

/// The sherpa `model_type` tag for the parakeet transducer (must match the sherpa release id).
const PARAKEET_MODEL_TYPE: &str = "nemo_transducer";

/// A loaded parakeet recognizer. `OfflineRecognizer` is `Send + Sync` (sherpa guarantees it for
/// single-object use — same as `Diarizer`), so one `ParakeetAsr` lives on the caption thread for
/// the whole recording and decodes each tick's window.
pub struct ParakeetAsr {
    recognizer: OfflineRecognizer,
}

impl ParakeetAsr {
    /// Load the int8 transducer from an already-verified [`ParakeetModelPaths`] bundle. Errors
    /// (never panics) when a file is missing or the recognizer fails to construct (null pointer).
    pub fn load(paths: &ParakeetModelPaths) -> Result<Self> {
        if !paths.all_present() {
            return Err(AppError::Transcribe(
                "parakeet model files are not all present".into(),
            ));
        }
        let config = OfflineRecognizerConfig {
            model_config: OfflineModelConfig {
                transducer: OfflineTransducerModelConfig {
                    encoder: Some(paths.encoder.to_string_lossy().into_owned()),
                    decoder: Some(paths.decoder.to_string_lossy().into_owned()),
                    joiner: Some(paths.joiner.to_string_lossy().into_owned()),
                },
                tokens: Some(paths.tokens.to_string_lossy().into_owned()),
                model_type: Some(PARAKEET_MODEL_TYPE.into()),
                // CPU-ONLY on purpose (off Metal). The default provider is CPU; set num_threads to
                // the E-core budget. NO `provider = "coreml"/"metal"` — the GPU is the brain's.
                num_threads: PARAKEET_NUM_THREADS,
                ..Default::default()
            },
            ..Default::default()
        };
        let recognizer = OfflineRecognizer::create(&config)
            .ok_or_else(|| AppError::Transcribe("failed to create parakeet recognizer".into()))?;
        tracing::info!(target: "transcribe", "parakeet live engine loaded (CPU x4)");
        Ok(Self { recognizer })
    }
}

impl LiveAsr for ParakeetAsr {
    /// Decode a 16 kHz mono window into a SINGLE-segment [`Transcript`]. The parakeet nemo
    /// transducer auto-detects language (clean PL↔EN LID in the spike), so `lang` is advisory
    /// only — it is threaded onto `Transcript::language` for the consumer but does NOT force a
    /// decode (the sherpa transducer has no per-call language forcing). Empty input → empty
    /// transcript (mirrors whisper's guard). Never panics.
    fn transcribe_live(&self, samples_16k: &[f32], lang: Option<&str>) -> Result<Transcript> {
        if samples_16k.is_empty() {
            return Ok(Transcript {
                full_text: String::new(),
                segments: Vec::new(),
                language: lang.map(str::to_string),
            });
        }
        let stream = self.recognizer.create_stream();
        // The live window is already 16 kHz mono (resampled by the caller); feed it verbatim.
        stream.accept_waveform(16_000, samples_16k);
        self.recognizer.decode(&stream);
        let text = match stream.get_result() {
            Some(r) => r.text.trim().to_string(),
            None => String::new(),
        };
        if text.is_empty() {
            return Ok(Transcript {
                full_text: String::new(),
                segments: Vec::new(),
                language: lang.map(str::to_string),
            });
        }
        // ONE segment spanning the whole window: the live loop joins segment text into the caption
        // and never uses parakeet's word timestamps, so a single 0.0..=duration segment is
        // behavior-identical to whisper's multi-segment output for the caption/buffer/wake paths.
        let dur_s = samples_16k.len() as f64 / 16_000.0;
        let segment = Segment {
            idx: 0,
            start_s: 0.0,
            end_s: dur_s,
            text: text.clone(),
            // Live/voice-trigger paths leave speaker attribution to the wall-clock merge (mic-only
            // live), and confidence is a batch-only whisper field. Match the whisper Fast contract.
            speaker: None,
            confidence: None,
        };
        Ok(Transcript {
            full_text: text,
            segments: vec![segment],
            language: lang.map(str::to_string),
        })
    }

    fn engine_label(&self) -> &'static str {
        ENGINE_PARAKEET
    }
}

#[cfg(test)]
mod tests {
    // A real parakeet decode needs the ~600 MB int8 model on disk + onnxruntime on a real Mac —
    // the `#[ignore]`d `parakeet_spike` harness already covers decode quality/latency (13× RT,
    // clean PL↔EN LID) and is NOT re-run in the `cargo test --lib` loop. Here we only assert the
    // PURE shape contract that does not need a loaded model.

    use super::*;

    /// The engine constants are stable (the sherpa release id + the label the live loop logs).
    #[test]
    fn parakeet_constants_are_stable() {
        assert_eq!(PARAKEET_MODEL_TYPE, "nemo_transducer");
        assert_eq!(PARAKEET_NUM_THREADS, 4);
        // The `LiveAsr` label matches the config selector token.
        assert_eq!(ENGINE_PARAKEET, "parakeet");
    }
}
