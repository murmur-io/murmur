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
    SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig,
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

// ── Voiceprints (opt-in): a per-cluster CAM++ speaker embedding for the diarized "others" ──────────
//
// PRIVACY: an embedding here is a VOICE BIOMETRIC of a remote participant. It is derived from the
// system stream ONLY (never the mic / "me"), stored on-device (SQLCipher, folder-lock-sealed, purged
// on seal) and NEVER egressed. Capturing a non-consenting participant's voiceprint is an explicit
// opt-in (`voiceprint_enabled`, default OFF) — see the config field doc. Everything below is
// best-effort: any sherpa failure yields an empty result and NEVER panics / blocks the pipeline, so
// diarization + labels still succeed. NO PII is logged (never log an embedding, a label, or audio).

/// One computed voiceprint: the diarizer's cluster index (the `others-{n}` suffix) + its L2-normalized
/// mean CAM++ embedding.
#[derive(Clone, Debug)]
pub struct ClusterVoiceprint {
    pub cluster_index: i32,
    pub embedding: Vec<f32>,
}

/// L2-normalize in place. A zero vector is left untouched (avoids a divide-by-zero NaN). Pure +
/// unit-testable (no FFI).
pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Cosine similarity of two equal-length vectors (0.0 for a length mismatch or a zero vector). Pure +
/// unit-testable (no FFI). Used by the (later) match/enroll path on already-normalized voiceprints.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// Pack an embedding as a little-endian f32 blob (for the `speaker_voiceprints.embedding BLOB`
/// column). Pure + unit-testable. Round-trips byte-exact with [`blob_to_embedding`].
pub fn embedding_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Decode a little-endian f32 blob back into an embedding. A blob whose length is not a multiple of 4
/// is rejected (`None`) rather than silently truncated. Pure + unit-testable.
pub fn blob_to_embedding(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.len() % 4 != 0 {
        return None;
    }
    Some(
        blob.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

/// PURE testable seam (no FFI): gather the mono `samples` (at `sample_rate` Hz) belonging to ONE
/// diarized cluster by concatenating every span whose `speaker == cluster_index`, clamped to the
/// buffer. Returns the concatenated samples for that cluster (empty when the cluster contributes no
/// in-range audio). Splitting this out lets us test the gather math with synthetic spans+samples and
/// no models.
pub fn gather_cluster_samples(
    samples: &[f32],
    spans: &[SpeakerSpan],
    cluster_index: i32,
    sample_rate: u32,
) -> Vec<f32> {
    if sample_rate == 0 || samples.is_empty() {
        return Vec::new();
    }
    let sr = sample_rate as f64;
    let mut out = Vec::new();
    for span in spans {
        if span.speaker != cluster_index {
            continue;
        }
        // Clamp [start,end) seconds → sample indices inside the buffer.
        let start = ((span.start.max(0.0) * sr).floor() as usize).min(samples.len());
        let end = ((span.end.max(0.0) * sr).ceil() as usize).min(samples.len());
        if end > start {
            out.extend_from_slice(&samples[start..end]);
        }
    }
    out
}

/// The distinct cluster indices present in `spans`, sorted ascending. Pure.
fn distinct_clusters(spans: &[SpeakerSpan]) -> Vec<i32> {
    let mut v: Vec<i32> = spans.iter().map(|s| s.speaker).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Compute ONE L2-normalized mean voiceprint per distinct diarized cluster, from the system-stream
/// `samples` (mono f32) + the diarizer's `spans`. Loads a STANDALONE [`SpeakerEmbeddingExtractor`]
/// from the SAME CAM++ `embedding` model path the diarizer used, feeds each cluster's concatenated
/// audio through a fresh [`sherpa_onnx::OnlineStream`], and takes the extractor's pooled embedding.
///
/// BEST-EFFORT: a failure to load the extractor, an empty cluster, a not-ready stream, or a null
/// compute all just skip that cluster (or return an empty `Vec`) — this NEVER panics and NEVER blocks
/// the pipeline (diarization labels are unaffected). `sample_rate` is the diarizer's rate (the caller
/// passes the exact rate it fed to `diarize`). NO PII is logged.
pub fn compute_cluster_voiceprints(
    embedding_model: &Path,
    samples: &[f32],
    spans: &[SpeakerSpan],
    sample_rate: u32,
) -> Vec<ClusterVoiceprint> {
    let clusters = distinct_clusters(spans);
    if clusters.is_empty() || samples.is_empty() || sample_rate == 0 {
        return Vec::new();
    }
    let config = SpeakerEmbeddingExtractorConfig {
        model: Some(embedding_model.to_string_lossy().into_owned()),
        ..Default::default()
    };
    let extractor = match SpeakerEmbeddingExtractor::create(&config) {
        Some(e) => e,
        None => {
            tracing::warn!(
                target: "transcribe",
                "voiceprint extractor load failed; no voiceprints (diarization unaffected)"
            );
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for cluster_index in clusters {
        let cluster_samples = gather_cluster_samples(samples, spans, cluster_index, sample_rate);
        if cluster_samples.is_empty() {
            continue;
        }
        let stream = match extractor.create_stream() {
            Some(s) => s,
            None => continue,
        };
        stream.accept_waveform(sample_rate as i32, &cluster_samples);
        stream.input_finished();
        if !extractor.is_ready(&stream) {
            // Not enough audio accumulated for this cluster → skip it (best-effort).
            continue;
        }
        if let Some(mut emb) = extractor.compute(&stream) {
            l2_normalize(&mut emb);
            out.push(ClusterVoiceprint {
                cluster_index,
                embedding: emb,
            });
        }
    }
    out
}

// ── Voiceprint matching (Phase 2): cosine re-identification over the GATED set ─────────────────────
//
// GATE DISCIPLINE: everything here is PURE (no DB, no FFI). The caller (a gated command) passes ONLY
// voiceprints that already survived `list_voiceprints_visible` — a sealed-not-unlocked meeting's
// voiceprint is NEVER in either slice, so the matcher can never surface a suggestion sourced from a
// sealed row. This function does no I/O; it cannot widen the visible set.

/// The cosine-similarity threshold at/above which a match is offered as a label suggestion.
///
/// @Mac TUNABLE — this default is a PLUMBING placeholder, NOT a validated operating point. Real
/// re-identification accuracy / cluster purity / the optimal threshold can ONLY be measured on a
/// signed build on a real Mac with real multi-speaker audio and the CAM++ model present. CAM++
/// cosine on same-speaker pairs typically lands well above 0.5 and cross-speaker well below, but
/// that is UNVERIFIED here. Below this we return no suggestion (leave the cluster unlabeled) rather
/// than risk a false enroll.
pub const VOICEPRINT_MATCH_THRESHOLD: f32 = 0.5;

/// One suggested label for a diarized cluster of the current meeting, from the best cosine match
/// against the already-gated set of LABELED voiceprints from OTHER meetings.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceprintSuggestion {
    /// The diarized cluster index in THIS meeting (the `others-{n}` suffix) being suggested for.
    pub cluster_index: i32,
    /// The suggested person name (the label of the best-matching prior voiceprint).
    pub label: String,
    /// The cosine similarity of the best match (0..=1 for L2-normalized embeddings).
    pub score: f32,
}

/// One (cluster_index, embedding) candidate from THIS meeting — a reference view so the caller
/// doesn't have to clone its `Voiceprint` rows.
#[derive(Clone, Copy)]
pub struct ClusterEmbeddingRef<'a> {
    pub cluster_index: i32,
    pub embedding: &'a [f32],
}

/// One (label, embedding) reference from a PRIOR labeled voiceprint (already gated + already known
/// to have a non-empty label).
#[derive(Clone, Copy)]
pub struct LabeledEmbeddingRef<'a> {
    pub label: &'a str,
    pub embedding: &'a [f32],
}

/// Suggest a person label for each of THIS meeting's `clusters` by nearest cosine match against
/// `labeled` (prior, VISIBLE, labeled voiceprints from OTHER meetings). A suggestion is returned for
/// a cluster ONLY when its best match scores `>= threshold` — below that the cluster is left
/// unlabeled (no false enroll). Ties are broken by first-seen order (stable). Pure: no I/O, no FFI —
/// so it can only ever see the gated slices the caller built.
///
/// The caller MUST pass a `labeled` slice drawn from `list_voiceprints_visible` and MUST exclude
/// entries whose `meeting_id` equals the current meeting (self-match is meaningless). This function
/// makes no visibility decision of its own.
pub fn suggest_voiceprint_labels(
    clusters: &[ClusterEmbeddingRef<'_>],
    labeled: &[LabeledEmbeddingRef<'_>],
    threshold: f32,
) -> Vec<VoiceprintSuggestion> {
    let mut out = Vec::new();
    for c in clusters {
        let mut best: Option<(f32, &str)> = None; // (score, label)
        for l in labeled {
            let score = cosine(c.embedding, l.embedding);
            // Strictly-greater keeps the FIRST-seen label on a tie (stable, deterministic).
            if best.map(|(s, _)| score > s).unwrap_or(true) {
                best = Some((score, l.label));
            }
        }
        if let Some((score, label)) = best {
            if score >= threshold {
                out.push(VoiceprintSuggestion {
                    cluster_index: c.cluster_index,
                    label: label.to_string(),
                    score,
                });
            }
        }
    }
    out
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
            confidence: None,
        }
    }

    #[test]
    fn relabel_assigns_max_overlap_speaker() {
        let mut segs = vec![seg(0.0, 2.0), seg(5.0, 7.0)];
        let spans = vec![
            SpeakerSpan {
                start: 0.0,
                end: 3.0,
                speaker: 0,
            },
            SpeakerSpan {
                start: 4.0,
                end: 8.0,
                speaker: 1,
            },
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
            SpeakerSpan {
                start: 0.0,
                end: 3.0,
                speaker: 0,
            },
            SpeakerSpan {
                start: 3.0,
                end: 9.0,
                speaker: 1,
            },
        ];
        relabel_others(&mut segs, &spans);
        assert_eq!(segs[0].speaker.as_deref(), Some("others-1"));
    }

    #[test]
    fn relabel_single_speaker_is_noop() {
        let mut segs = vec![seg(0.0, 2.0)];
        let spans = vec![SpeakerSpan {
            start: 0.0,
            end: 3.0,
            speaker: 0,
        }];
        relabel_others(&mut segs, &spans);
        assert_eq!(
            segs[0].speaker, None,
            "one speaker → keep the plain others label"
        );
    }

    #[test]
    fn relabel_no_overlap_keeps_none() {
        let mut segs = vec![seg(10.0, 12.0)];
        let spans = vec![
            SpeakerSpan {
                start: 0.0,
                end: 3.0,
                speaker: 0,
            },
            SpeakerSpan {
                start: 3.0,
                end: 5.0,
                speaker: 1,
            },
        ];
        relabel_others(&mut segs, &spans);
        assert_eq!(segs[0].speaker, None);
    }

    // ── Voiceprint pure-seam tests (no FFI / no models) ──────────────────────────────────────────

    /// The embedding BLOB encode/decode round-trips byte-exact — the at-rest storage contract for
    /// `speaker_voiceprints.embedding` (the same discipline as a seal round-trip).
    #[test]
    fn embedding_blob_round_trips_byte_exact() {
        let v: Vec<f32> = vec![0.0, 1.0, -1.0, 0.5, -0.25, 12345.678, f32::MIN, f32::MAX];
        let blob = embedding_to_blob(&v);
        assert_eq!(blob.len(), v.len() * 4, "4 LE bytes per f32");
        let back = blob_to_embedding(&blob).expect("valid blob decodes");
        assert_eq!(back, v, "blob → vec is byte-exact");
        // A misaligned blob (not a multiple of 4) is rejected, never silently truncated.
        assert!(blob_to_embedding(&[1, 2, 3]).is_none());
        assert_eq!(
            blob_to_embedding(&[]),
            Some(Vec::new()),
            "empty blob → empty vec"
        );
    }

    /// Cosine on known vectors: identical → 1.0, orthogonal → 0.0, opposite → -1.0; a length
    /// mismatch or an empty/zero vector is a safe 0.0.
    #[test]
    fn cosine_on_known_vectors() {
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![1.0f32, 0.0, 0.0];
        let c = vec![0.0f32, 1.0, 0.0];
        let d = vec![-1.0f32, 0.0, 0.0];
        assert!((cosine(&a, &b) - 1.0).abs() < 1e-6, "identical → 1.0");
        assert!(cosine(&a, &c).abs() < 1e-6, "orthogonal → 0.0");
        assert!((cosine(&a, &d) + 1.0).abs() < 1e-6, "opposite → -1.0");
        assert_eq!(cosine(&a, &[1.0, 0.0]), 0.0, "length mismatch → 0.0");
        assert_eq!(cosine(&[0.0, 0.0, 0.0], &a), 0.0, "zero vector → 0.0");
        assert_eq!(cosine(&[], &[]), 0.0, "empty → 0.0");
    }

    /// L2-normalize makes a non-zero vector unit-length; a zero vector is untouched (no NaN).
    #[test]
    fn l2_normalize_unit_length_and_zero_safe() {
        let mut v = vec![3.0f32, 4.0];
        l2_normalize(&mut v);
        let norm = (v[0] * v[0] + v[1] * v[1]).sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "unit length after normalize");
        let mut z = vec![0.0f32, 0.0, 0.0];
        l2_normalize(&mut z);
        assert_eq!(
            z,
            vec![0.0, 0.0, 0.0],
            "zero vector untouched (no divide-by-zero)"
        );
    }

    /// The pure gather seam concatenates ONLY the requested cluster's spans, clamps to the buffer,
    /// and yields empty for a cluster with no in-range audio. Sample-rate math: 8 kHz → 1s = 8000
    /// samples.
    #[test]
    fn gather_cluster_samples_selects_and_clamps() {
        let sr = 8000u32;
        // 3 seconds of ramp so each sample index is identifiable.
        let samples: Vec<f32> = (0..24_000).map(|i| i as f32).collect();
        let spans = vec![
            SpeakerSpan {
                start: 0.0,
                end: 1.0,
                speaker: 0,
            }, // [0,8000)
            SpeakerSpan {
                start: 2.0,
                end: 3.0,
                speaker: 0,
            }, // [16000,24000)
            SpeakerSpan {
                start: 1.0,
                end: 2.0,
                speaker: 1,
            }, // cluster 1 → excluded
        ];
        let c0 = gather_cluster_samples(&samples, &spans, 0, sr);
        assert_eq!(c0.len(), 16_000, "cluster 0 = two 1s spans concatenated");
        assert_eq!(c0[0], 0.0);
        assert_eq!(c0[8000], 16_000.0, "second span starts at sample 16000");

        let c1 = gather_cluster_samples(&samples, &spans, 1, sr);
        assert_eq!(c1.len(), 8_000, "cluster 1 = its single 1s span");
        assert_eq!(c1[0], 8_000.0);

        // A cluster with no spans → empty.
        assert!(gather_cluster_samples(&samples, &spans, 9, sr).is_empty());
        // Out-of-range span end is clamped to the buffer, never panics.
        let over = vec![SpeakerSpan {
            start: 2.0,
            end: 100.0,
            speaker: 0,
        }];
        assert_eq!(gather_cluster_samples(&samples, &over, 0, sr).len(), 8_000);
        // Guard rails: zero sample-rate / empty buffer → empty.
        assert!(gather_cluster_samples(&samples, &spans, 0, 0).is_empty());
        assert!(gather_cluster_samples(&[], &spans, 0, sr).is_empty());
    }

    // ── Matcher pure-seam tests (no FFI / no DB): cosine re-identification ─────────────────────────

    /// The nearest labeled voiceprint above threshold is suggested for the cluster; the exact score
    /// is reported. Two clusters, two labeled priors → each maps to its nearest.
    #[test]
    fn suggest_maps_each_cluster_to_nearest_label() {
        // Alice ≈ x-axis, Bob ≈ y-axis (L2-normalized).
        let alice = [1.0f32, 0.0, 0.0];
        let bob = [0.0f32, 1.0, 0.0];
        // Cluster 0 is very close to Alice, cluster 1 very close to Bob.
        let c0 = [0.99f32, 0.14, 0.0]; // ~unit, cos(alice)≈0.99
        let c1 = [0.10f32, 0.99, 0.0]; // cos(bob)≈0.99
        let clusters = [
            ClusterEmbeddingRef {
                cluster_index: 0,
                embedding: &c0,
            },
            ClusterEmbeddingRef {
                cluster_index: 1,
                embedding: &c1,
            },
        ];
        let labeled = [
            LabeledEmbeddingRef {
                label: "Alice",
                embedding: &alice,
            },
            LabeledEmbeddingRef {
                label: "Bob",
                embedding: &bob,
            },
        ];
        let sugg = suggest_voiceprint_labels(&clusters, &labeled, VOICEPRINT_MATCH_THRESHOLD);
        assert_eq!(sugg.len(), 2);
        assert_eq!(sugg[0].cluster_index, 0);
        assert_eq!(sugg[0].label, "Alice");
        assert!(sugg[0].score >= 0.9, "near-identical → high cosine");
        assert_eq!(sugg[1].cluster_index, 1);
        assert_eq!(sugg[1].label, "Bob");
    }

    /// RED-before-GREEN for the threshold gate: a cluster whose BEST match is below the threshold
    /// yields NO suggestion (we never enroll on a weak match). Orthogonal → cos 0.0 < 0.5.
    #[test]
    fn suggest_below_threshold_returns_nothing() {
        let alice = [1.0f32, 0.0, 0.0];
        let orthogonal = [0.0f32, 1.0, 0.0];
        let clusters = [ClusterEmbeddingRef {
            cluster_index: 0,
            embedding: &orthogonal,
        }];
        let labeled = [LabeledEmbeddingRef {
            label: "Alice",
            embedding: &alice,
        }];
        let sugg = suggest_voiceprint_labels(&clusters, &labeled, VOICEPRINT_MATCH_THRESHOLD);
        assert!(sugg.is_empty(), "cos 0.0 < 0.5 threshold → no suggestion");
        // Exactly-at-threshold IS offered (>=): a hand-built pair at cos 0.5.
        let half = [0.5f32, (0.75f32).sqrt(), 0.0]; // cos(alice)=0.5
        let at = [ClusterEmbeddingRef {
            cluster_index: 3,
            embedding: &half,
        }];
        let s2 = suggest_voiceprint_labels(&at, &labeled, VOICEPRINT_MATCH_THRESHOLD);
        assert_eq!(s2.len(), 1, "cos==threshold is inclusive");
        assert!((s2[0].score - 0.5).abs() < 1e-5);
    }

    /// The gate lives in the CALLER's slice: an EMPTY `labeled` set (what a fully-sealed prior corpus
    /// produces after `list_voiceprints_visible` filtering) yields NO suggestion — the matcher cannot
    /// invent a labeled candidate it wasn't handed.
    #[test]
    fn suggest_with_no_visible_labeled_priors_is_empty() {
        let c0 = [1.0f32, 0.0, 0.0];
        let clusters = [ClusterEmbeddingRef {
            cluster_index: 0,
            embedding: &c0,
        }];
        let sugg = suggest_voiceprint_labels(&clusters, &[], VOICEPRINT_MATCH_THRESHOLD);
        assert!(
            sugg.is_empty(),
            "no visible labeled priors → nothing to suggest from"
        );
    }

    /// Tie-break is stable (first-seen label wins) and a length mismatch is a safe 0.0 (never a
    /// spurious suggestion).
    #[test]
    fn suggest_tie_break_and_dim_mismatch_safe() {
        let a = [1.0f32, 0.0];
        let c0 = [1.0f32, 0.0];
        let labeled = [
            LabeledEmbeddingRef {
                label: "First",
                embedding: &a,
            },
            LabeledEmbeddingRef {
                label: "Second",
                embedding: &a,
            }, // identical → tie
        ];
        let clusters = [ClusterEmbeddingRef {
            cluster_index: 0,
            embedding: &c0,
        }];
        let sugg = suggest_voiceprint_labels(&clusters, &labeled, VOICEPRINT_MATCH_THRESHOLD);
        assert_eq!(sugg.len(), 1);
        assert_eq!(sugg[0].label, "First", "tie → first-seen label (stable)");

        // A wrong-dim prior scores 0.0 (cosine guard) → below threshold → dropped.
        let wrong_dim = [1.0f32, 0.0, 0.0];
        let only_bad = [LabeledEmbeddingRef {
            label: "Bad",
            embedding: &wrong_dim,
        }];
        assert!(
            suggest_voiceprint_labels(&clusters, &only_bad, VOICEPRINT_MATCH_THRESHOLD).is_empty(),
            "dimension mismatch → cosine 0.0 → no suggestion"
        );
    }
}
