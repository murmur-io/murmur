//! Offline speaker diarization (#8) via sherpa-onnx — runs ONLY on the system ("others") stream
//! to give N-way labels for remote speakers. The mic ("me") is never diarized.
//!
//! Pipeline: pyannote segmentation → speaker embeddings (CAM++) → fast clustering (auto count).
//! Opt-in; bundles a STATIC onnxruntime (macOS 13.4+). On ANY failure the caller keeps the single
//! "others" label, so diarization is strictly best-effort and never blocks a recording.

use std::path::Path;

use sherpa_onnx::{
    FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
    OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
    SpeakerEmbeddingExtractorConfig,
};

use crate::audio::merge::SPEAKER_OTHERS;
use crate::error::{AppError, Result};
use crate::transcribe::types::Segment;

/// A diarized speech span (seconds) with a 0-based speaker index.
#[derive(Clone, Copy, Debug)]
pub struct SpeakerSpan {
    pub start: f64,
    pub end: f64,
    pub speaker: i32,
}

/// A loaded diarizer (segmentation + embedding models + clustering). `Send + Sync` per sherpa-onnx.
pub struct Diarizer {
    inner: OfflineSpeakerDiarization,
}

impl Diarizer {
    /// Load from the on-disk segmentation (pyannote) + embedding (CAM++) ONNX models.
    pub fn load(segmentation: &Path, embedding: &Path) -> Result<Self> {
        let config = OfflineSpeakerDiarizationConfig {
            segmentation: OfflineSpeakerSegmentationModelConfig {
                pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                    model: Some(segmentation.to_string_lossy().into_owned()),
                },
                ..Default::default()
            },
            embedding: SpeakerEmbeddingExtractorConfig {
                model: Some(embedding.to_string_lossy().into_owned()),
                ..Default::default()
            },
            // num_clusters = -1 → estimate the speaker count automatically (threshold 0.5).
            clustering: FastClusteringConfig::default(),
            // min_duration_on / min_duration_off keep their library defaults.
            ..Default::default()
        };
        let inner = OfflineSpeakerDiarization::create(&config)
            .ok_or_else(|| AppError::Transcribe("failed to create speaker diarizer".into()))?;
        Ok(Self { inner })
    }

    /// The sample rate (Hz) the segmentation model expects — the caller must feed samples at it.
    pub fn sample_rate(&self) -> u32 {
        self.inner.sample_rate().max(0) as u32
    }

    /// Diarize `samples` (mono f32 at [`sample_rate`]), returning speaker spans sorted by start.
    pub fn diarize(&self, samples: &[f32]) -> Result<Vec<SpeakerSpan>> {
        let result = self
            .inner
            .process(samples)
            .ok_or_else(|| AppError::Transcribe("diarization process returned nothing".into()))?;
        Ok(result
            .sort_by_start_time()
            .into_iter()
            .map(|s| SpeakerSpan {
                start: s.start as f64,
                end: s.end as f64,
                speaker: s.speaker,
            })
            .collect())
    }
}

/// Relabel each "others" segment with the diarized speaker it overlaps MOST (→ `others-{n}`).
/// Pure (no FFI), so it's unit-testable. A segment with no overlap keeps the single
/// `SPEAKER_OTHERS` label (`None` here → set by the merge). If the diarizer found ≤1 distinct
/// speaker, every segment is left plain `others` (no point splitting a single remote speaker).
pub fn relabel_others(segments: &mut [Segment], spans: &[SpeakerSpan]) {
    let distinct = {
        let mut s: Vec<i32> = spans.iter().map(|p| p.speaker).collect();
        s.sort_unstable();
        s.dedup();
        s.len()
    };
    if distinct <= 1 {
        return;
    }
    for seg in segments.iter_mut() {
        let mut best: Option<(f64, i32)> = None; // (overlap_seconds, speaker)
        for span in spans {
            let overlap = (seg.end_s.min(span.end) - seg.start_s.max(span.start)).max(0.0);
            if overlap > 0.0 && best.map(|(o, _)| overlap > o).unwrap_or(true) {
                best = Some((overlap, span.speaker));
            }
        }
        if let Some((_, speaker)) = best {
            seg.speaker = Some(format!("{SPEAKER_OTHERS}-{speaker}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start_s: f64, end_s: f64) -> Segment {
        Segment {
            idx: 0,
            start_s,
            end_s,
            text: "x".into(),
            speaker: None,
        }
    }

    #[test]
    fn relabel_assigns_max_overlap_speaker() {
        let mut segs = vec![seg(0.0, 2.0), seg(5.0, 7.0)];
        let spans = vec![
            SpeakerSpan { start: 0.0, end: 3.0, speaker: 0 },
            SpeakerSpan { start: 4.0, end: 8.0, speaker: 1 },
        ];
        relabel_others(&mut segs, &spans);
        assert_eq!(segs[0].speaker.as_deref(), Some("others-0"));
        assert_eq!(segs[1].speaker.as_deref(), Some("others-1"));
    }

    #[test]
    fn relabel_picks_the_larger_overlap() {
        // Segment 1..6 overlaps speaker 0 for 2s (1..3) and speaker 1 for 3s (3..6) → speaker 1.
        let mut segs = vec![seg(1.0, 6.0)];
        let spans = vec![
            SpeakerSpan { start: 0.0, end: 3.0, speaker: 0 },
            SpeakerSpan { start: 3.0, end: 9.0, speaker: 1 },
        ];
        relabel_others(&mut segs, &spans);
        assert_eq!(segs[0].speaker.as_deref(), Some("others-1"));
    }

    #[test]
    fn relabel_single_speaker_is_noop() {
        let mut segs = vec![seg(0.0, 2.0)];
        let spans = vec![SpeakerSpan { start: 0.0, end: 3.0, speaker: 0 }];
        relabel_others(&mut segs, &spans);
        assert_eq!(segs[0].speaker, None, "one speaker → keep the plain others label");
    }

    #[test]
    fn relabel_no_overlap_keeps_none() {
        let mut segs = vec![seg(10.0, 12.0)];
        let spans = vec![
            SpeakerSpan { start: 0.0, end: 3.0, speaker: 0 },
            SpeakerSpan { start: 3.0, end: 5.0, speaker: 1 },
        ];
        relabel_others(&mut segs, &spans);
        assert_eq!(segs[0].speaker, None);
    }
}
