use serde::Serialize;

pub const EVENT_STATUS: &str = "meetnotes://status";

/// Emitted to the main window when the tray "Start / Stop recording" item is chosen.
pub const EVENT_TOGGLE_RECORD: &str = "murmur://toggle-record";

/// Best-effort live-transcription caption emitted periodically during recording.
pub const EVENT_LIVE_CAPTION: &str = "murmur://live-caption";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusPayload {
    /// "idle" | "recording" | "transcribing" | "summarizing" | "exporting" | "done" | "error"
    pub stage: String,
    /// human-readable, NO PII
    pub message: String,
    pub meeting_id: Option<String>,
}
