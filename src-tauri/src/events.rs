use serde::Serialize;

pub const EVENT_STATUS: &str = "meetnotes://status";

/// Emitted to the main window when the tray "Start / Stop recording" item is chosen.
pub const EVENT_TOGGLE_RECORD: &str = "murmur://toggle-record";

/// Best-effort live-transcription caption emitted periodically during recording.
pub const EVENT_LIVE_CAPTION: &str = "murmur://live-caption";

/// Emitted when the in-meeting voice trigger ("Claudku …") is DETECTED in a live caption tail.
/// Phase A only SURFACES the hit (matched wake word + command tail + parsed intent); it does NOT
/// dispatch any action — execution is a later phase that needs the local brain.
pub const EVENT_WAKE_DETECTED: &str = "murmur://wake-detected";

/// Payload for [`EVENT_WAKE_DETECTED`]. Carries the recognized wake token, the trimmed command tail,
/// and the deterministically-parsed [`crate::audio::wake::VoiceIntent`]. NO raw transcript beyond the
/// command tail the user explicitly addressed to the assistant.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeDetectedPayload {
    /// The original (un-normalized) word that matched the wake lexicon, e.g. "Klałdku".
    pub matched_phrase: String,
    /// Everything the user said after the wake token (may be empty).
    pub command: String,
    /// The structured interpretation of `command` (research / slack / recall / reminder / note / unknown).
    pub intent: crate::audio::wake::VoiceIntent,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusPayload {
    /// "idle" | "recording" | "transcribing" | "summarizing" | "exporting" | "done" | "error"
    pub stage: String,
    /// human-readable, NO PII
    pub message: String,
    pub meeting_id: Option<String>,
}
