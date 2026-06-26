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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub full_text: String,
    pub segments: Vec<Segment>,
    pub language: Option<String>,
}
