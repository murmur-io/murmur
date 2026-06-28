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

/// Emitted when an in-meeting VOICE ACTION ("Claudku, zrób research o X") has been DISPATCHED and a
/// result is ready (Phase E, Flow B). Only fires when `realtime_reactions` is ON — when OFF the
/// wake is surfaced via [`EVENT_WAKE_DETECTED`] and nothing is dispatched. The payload is a
/// [`crate::voice_action::VoiceActionResult`] (intent kind + status + summary + `[[Title]]`
/// citations over VISIBLE meetings only). The rich result card is Phase H.
pub const EVENT_VOICE_ACTION_RESULT: &str = "murmur://voice-action-result";

/// Progress for the on-device brain (reasoning GGUF) download. Carries byte counts only — NO PII.
pub const EVENT_BRAIN_DOWNLOAD: &str = "murmur://brain-download";

/// Payload for [`EVENT_BRAIN_DOWNLOAD`]. `total` is `None` when the server omits `Content-Length`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrainDownloadPayload {
    /// Bytes written so far.
    pub downloaded: u64,
    /// Total bytes expected, when known.
    pub total: Option<u64>,
    /// True on the final event once the file is fully written + renamed into place.
    pub done: bool,
}

/// Progress for the on-device EMBED model (multilingual-e5-small) download. Carries byte/file counts
/// only — NO PII. The e5 model is three small files, so progress is reported per-file.
pub const EVENT_EMBED_DOWNLOAD: &str = "murmur://embed-download";

/// Payload for [`EVENT_EMBED_DOWNLOAD`]. `total` is `None` when the server omits `Content-Length`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbedDownloadPayload {
    /// Index of the file currently downloading (0-based, into the 3-file e5 set).
    pub file_index: usize,
    /// Total number of files in the set (3).
    pub file_count: usize,
    /// Bytes written so far for the CURRENT file.
    pub downloaded: u64,
    /// Total bytes expected for the current file, when known.
    pub total: Option<u64>,
    /// True on the final event once all three files are written + renamed into place.
    pub done: bool,
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
