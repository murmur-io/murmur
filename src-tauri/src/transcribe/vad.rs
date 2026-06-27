//! Silero VAD pre-segmentation for the ACCURATE batch path, via whisper.cpp's native VAD
//! (whisper-rs 0.16 [`WhisperVadContext`] — the STANDALONE wrapper).
//!
//! Why not `FullParams::enable_vad`: on `WhisperState::full` that flag is a confirmed no-op
//! (it lowers to `whisper_full_with_state`, which has no VAD code) AND panics if the VAD model
//! path is unset (whisper.cpp #3402). The standalone `WhisperVadContext` is the only working path.
//!
//! The win: split each 16 kHz stream into speech REGIONS before decoding, so (a) Whisper never
//! decodes long silences (the repetition-loop hallucination source), and (b) each region is
//! decoded with a FRESH context, resetting `condition_on_previous_text` across long gaps while
//! preserving it WITHIN a contiguous run. Muted/zero spans produce no speech segment, so the
//! "skip muted" behaviour falls out for free. Decode + re-offset happens in `pipeline.rs`.

use std::path::Path;

use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams};

use crate::audio::TARGET_RATE_HZ;
use crate::error::{AppError, Result};

/// Long-silence threshold (seconds): spans separated by MORE than this start a FRESH decode
/// (context reset); spans closer than this are grouped into one decode (context preserved).
/// Tunable on a real Mac against recorded Polish/English audio.
const GAP_RESET_S: f64 = 2.0;

/// 16 kHz samples per centisecond — VAD reports segment timestamps in centiseconds.
const SAMPLES_PER_CENTISECOND: f64 = TARGET_RATE_HZ as f64 / 100.0; // 160.0

/// A loaded Silero VAD model. Single-stream use (`segments_from_samples` needs `&mut`).
pub struct VadSegmenter {
    ctx: WhisperVadContext,
}

impl VadSegmenter {
    /// Load the Silero VAD model on the CPU.
    ///
    /// CPU on purpose: the Silero model is tiny (~885 kB) so Metal buys nothing, and running a
    /// SECOND ggml Metal context alongside the main whisper Metal context makes ggml's backend
    /// scheduler `ggml_abort` during graph init (`whisper_vad_init_with_params` →
    /// `ggml_backend_sched_alloc_graph`) — a hard C abort Rust can't catch, crashing the process.
    pub fn load(model_path: &Path) -> Result<Self> {
        let mut params = WhisperVadContextParams::default();
        params.set_use_gpu(false);
        let path = model_path
            .to_str()
            .ok_or_else(|| AppError::Transcribe("VAD model path is not valid UTF-8".into()))?;
        let ctx = WhisperVadContext::new(path, params)
            .map_err(|e| AppError::Transcribe(format!("load VAD model: {e}")))?;
        Ok(Self { ctx })
    }

    /// Detect speech in `samples_16k` and return speech REGIONS as `(start_sample, end_sample)`
    /// half-open ranges, grouping spans separated by < [`GAP_RESET_S`] into one region. An empty
    /// result means "no speech detected" (the caller then produces no segments for this stream).
    pub fn speech_regions(&mut self, samples_16k: &[f32]) -> Result<Vec<(usize, usize)>> {
        if samples_16k.is_empty() {
            return Ok(Vec::new());
        }
        let len = samples_16k.len();
        let segs = self
            .ctx
            .segments_from_samples(WhisperVadParams::default(), samples_16k)
            .map_err(|e| AppError::Transcribe(format!("VAD segmentation failed: {e}")))?;
        let spans: Vec<(usize, usize)> = segs
            .map(|s| {
                let start = (s.start as f64 * SAMPLES_PER_CENTISECOND) as usize;
                let end = (s.end as f64 * SAMPLES_PER_CENTISECOND) as usize;
                (start.min(len), end.min(len))
            })
            .filter(|(a, b)| b > a)
            .collect();
        Ok(group_spans(&spans, gap_samples()))
    }
}

/// Gap (in 16 kHz samples) above which two speech spans are NOT grouped (→ context reset).
fn gap_samples() -> usize {
    (GAP_RESET_S * TARGET_RATE_HZ as f64) as usize
}

/// Merge speech spans whose silence gap is below `gap`. Pure (no FFI), so it's unit-testable.
/// Spans are assumed sorted by start (VAD returns them in order); output regions are
/// non-overlapping and sorted.
fn group_spans(spans: &[(usize, usize)], gap: usize) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for &(start, end) in spans {
        if let Some(last) = out.last_mut() {
            // Group when this span starts within `gap` of the previous region's end.
            if start <= last.1 + gap {
                if end > last.1 {
                    last.1 = end;
                }
                continue;
            }
        }
        out.push((start, end));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_spans_merges_close_and_splits_far() {
        // gap = 100 samples. (0,50)&(120,200): 120-50=70 ≤ 100 → merge → (0,200).
        // (1000,1100): 1000-200=800 > 100 → its own region.
        let spans = [(0usize, 50usize), (120, 200), (1000, 1100)];
        assert_eq!(group_spans(&spans, 100), vec![(0, 200), (1000, 1100)]);
    }

    #[test]
    fn group_spans_absorbs_nested_and_keeps_max_end() {
        // A later span fully inside the running region must not shrink it.
        let spans = [(0usize, 500usize), (100, 300), (520, 600)];
        assert_eq!(group_spans(&spans, 50), vec![(0, 600)]);
    }

    #[test]
    fn group_spans_empty_is_empty() {
        assert!(group_spans(&[], 100).is_empty());
    }

    #[test]
    fn gap_samples_is_two_seconds_at_16k() {
        assert_eq!(gap_samples(), 32_000);
    }
}
