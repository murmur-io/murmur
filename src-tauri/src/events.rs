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

/// Emitted when the MANUAL voice-command capture (the button trigger) changes state: `active: true`
/// when the user clicks "ask the assistant" and the live loop starts collecting the next spoken
/// utterance as a command (NO wake word), `active: false` once it has been dispatched, given up at
/// budget, or armed while not recording. Lets the FE show/clear the "listening…" affordance. The
/// resulting answer still arrives via [`EVENT_VOICE_ACTION_RESULT`] (same gated dispatch path as the
/// wake trigger). Carries NO transcript — just the boolean.
pub const EVENT_VOICE_COMMAND_LISTENING: &str = "murmur://voice-command-listening";

/// Payload for [`EVENT_VOICE_COMMAND_LISTENING`]. Boolean only — NO PII.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceCommandListeningPayload {
    /// True while the manual capture is armed/listening; false once it ends.
    pub active: bool,
}

/// Emitted when a MANUAL voice-command capture has STOPPED listening and the accumulated utterance is
/// being DISPATCHED (the gated `handle_voice_action` round-trip — RAG + brain — can take seconds).
/// `active: true` lets the FE show a "thinking…" state in the gap between capture-end and the answer
/// (the user's complaint: "you don't know what it's doing"). It is cleared (`active: false` is
/// implied) by the arrival of [`EVENT_VOICE_ACTION_RESULT`] — the FE clears "processing" when the
/// result lands. Carries NO transcript — just the boolean.
pub const EVENT_VOICE_COMMAND_PROCESSING: &str = "murmur://voice-command-processing";

/// Payload for [`EVENT_VOICE_COMMAND_PROCESSING`]. Boolean only — NO PII.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceCommandProcessingPayload {
    /// True while the dispatch is in flight (between capture-stop and the result event).
    pub active: bool,
}

/// Emitted once per TOOL CALL the in-meeting brain makes during an agentic turn, so the FE can render
/// the live "tool trace" chips ("Searching notes… ✓", "Checking the web…"). Low-frequency (a handful
/// per turn), so a typed event is the right primitive. Carries the tool NAME + a coarse result-size
/// count only — NO PII (never the args, the results, or any content).
pub const EVENT_ASSISTANT_TOOL: &str = "murmur://assistant-tool";

/// Same shape as [`EVENT_ASSISTANT_TOOL`] but scoped to the in-meeting CHAT PANEL (the dedicated
/// multi-turn conversation), so the chat's live tool-trace never bleeds into the quick-Q&A assistant
/// card and vice-versa. Payload is [`AssistantToolPayload`] (tool name + state + count, NO PII).
pub const EVENT_CHAT_TOOL: &str = "murmur://chat-tool";

/// Payload for [`EVENT_ASSISTANT_TOOL`]. `state` is "running" | "done"; `ok` is false when the tool
/// call errored; `count` is a coarse result-size signal for the "✓ N" badge (NEVER the content).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantToolPayload {
    /// The tool name (search_meetings / search_semantic / web_search / calendar_lookup / …).
    pub tool: String,
    /// "running" when the call starts, "done" when it finishes.
    pub state: String,
    /// False when the tool call errored (the chip shows a muted / failed state).
    pub ok: bool,
    /// Coarse result-size signal for the done badge — NOT the content.
    pub count: Option<u32>,
    /// Opaque id of the conversation THREAD this tool call belongs to (an @brain thread id or the
    /// backend-generated voice-turn UUID), so simultaneous threads attribute their trace chips
    /// without cross-bleed. `None` only for legacy emitters. NOT PII (an opaque UUID).
    pub thread_id: Option<String>,
}

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

/// Progress for the Whisper transcribe model (GGML) download. Carries byte counts only — NO PII.
pub const EVENT_MODEL_DOWNLOAD: &str = "murmur://model-download";

/// Payload for [`EVENT_MODEL_DOWNLOAD`]. `total` is `None` when the server omits `Content-Length`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDownloadPayload {
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

/// Progress for the on-device NER name-redaction model (multilingual mDeBERTa-v3) download. Carries
/// byte/file counts only — NO PII. The model is three files, so progress is reported per-file.
pub const EVENT_NER_DOWNLOAD: &str = "murmur://ner-download";

/// Payload for [`EVENT_NER_DOWNLOAD`]. `total` is `None` when the server omits `Content-Length`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NerDownloadPayload {
    /// Index of the file currently downloading (0-based, into the 3-file NER set).
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

/// Progress for the semantic-search backfill (`reindex_embeddings`) over all visible meetings.
/// Carries COUNTS ONLY — no meeting ids, titles, or content (NO PII).
pub const EVENT_REINDEX: &str = "murmur://reindex-embeddings";

/// Payload for [`EVENT_REINDEX`]. Counts only — NO PII.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReindexPayload {
    /// Visible meetings indexed so far.
    pub done: usize,
    /// Total visible meetings to index this run.
    pub total: usize,
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
