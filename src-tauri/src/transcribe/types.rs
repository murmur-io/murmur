use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub idx: i64,
    pub start_s: f64,
    pub end_s: f64,
    pub text: String,
    /// Cheap 2-way stream-attribution: `Some("me")` for the local mic stream,
    /// `Some("others")` for the captured system-audio stream, `None` when the producing
    /// stream is unknown (e.g. a legacy row, or the live/voice-trigger paths which do not
    /// attribute). HONEST DENT: this is NOT per-remote-person diarization — every remote
    /// participant collapses into the single "others" label, because attribution is decided
    /// purely by WHICH capture stream produced the segment, not by voice fingerprinting.
    #[serde(default)]
    pub speaker: Option<String>,
    /// Per-segment ASR confidence in `[0.0, 1.0]` (higher = the decoder was more certain). Computed
    /// ONLY on the `Accurate` batch path from whisper's per-token linear probabilities, discounted by
    /// the segment's no-speech probability (a confident-but-likely-silence decode — the classic
    /// hallucination — scores low). `None` when not computed: the low-latency live/voice-trigger
    /// (`Fast`) captions, a legacy row transcribed before this field existed, or a token read that
    /// yielded nothing. NON-CONTENT METADATA (a probability, never the words), so it survives
    /// seal-blanking exactly like `start_s`/`end_s`/`speaker`. The grounding pass surfaces a low value
    /// as `> unverified (low audio confidence)` in the exported note (see `summarize::grounding`).
    #[serde(default)]
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub full_text: String,
    pub segments: Vec<Segment>,
    pub language: Option<String>,
}
