use serde::Serialize;
use tauri::{AppHandle, Emitter};

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

/// Same shape again but scoped to the ASK-MY-VAULT page (the vault-wide agentic Q&A surface),
/// deliberately separate from [`EVENT_ASSISTANT_TOOL`] / [`EVENT_CHAT_TOOL`] so the record-screen
/// stores never see Ask chips and vice-versa. Payload is [`AssistantToolPayload`] (tool name +
/// state + count + the turn's opaque `thread_id`, NO PII).
pub const EVENT_ASK_TOOL: &str = "murmur://ask-tool";

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

/// Emitted when the PROACTIVE brain (P1, zero-egress) surfaces one recall card during a
/// recording: "you discussed this before → [[meeting]]" / "open commitment: …" / a current fact.
/// Deterministic LOCAL matching only (spec D1) — no LLM, no provider, no egress, no consent
/// needed. Throttled at the SOURCE (spec D2: ≥120 s cooldown, session dedup, and the
/// `proactive_hints_enabled` backend mute), so the FE never has to rate-limit it.
pub const EVENT_PROACTIVE_HINT: &str = "murmur://proactive-hint";

/// Realtime Reactions (spec §4) — a private in-meeting "whisper" the user alone sees: the far side
/// just asserted something that CONTRADICTS a fact you already recorded. The payload is a
/// [`crate::brain_reactions::WhisperCard`] (neutral summary + the EXTRACTIVE old-fact citation + the
/// source `[[meeting]]`), built on-device from the LIGHT engine + the deterministic reconcile. Only
/// fires when Brain Live is ON, the light model is present, and the contradiction sub-toggle is on
/// (else the detection runs in SHADOW mode and nothing is emitted). Ephemeral — never persisted; the
/// FE rail must purge it on lock/screen-share transitions (lock-model, product #2).
pub const EVENT_WHISPER_CARD: &str = "murmur://whisper-card";

/// Payload for [`EVENT_PROACTIVE_HINT`]. IDs + a SHORT title from an already-VISIBLE row only —
/// never sealed content, never content bodies. `kind` is `"past_meeting"` | `"open_commitment"`
/// | `"fact"`; `target_id` is the dedup/click-through id (the meeting id for past-meeting and
/// commitment cards, the fact id for fact cards); `meeting_id` is the source meeting to open.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProactiveHintPayload {
    /// "past_meeting" | "open_commitment" | "fact".
    pub kind: String,
    /// Short display title (meeting title / commitment line / fact triple) from a VISIBLE row.
    pub title: String,
    /// The dedup/click-through id for this card.
    pub target_id: String,
    /// The source meeting to open, when known.
    pub meeting_id: Option<String>,
    /// The matcher's relevance score (already ≥ the emission threshold).
    pub score: f32,
}

/// Brain v2 L5 — emitted when the scheduled-brief runner STAGES a proposed brief
/// (`brief_runs` row, status "pending"). Propose-accept: the FE shows a card and the user accepts
/// (vault export) or dismisses. Carries the run id, the schedule's user-authored label, and a
/// char count — NEVER the brief markdown itself (the FE fetches pending runs via the command).
pub const EVENT_BRIEF_PROPOSED: &str = "murmur://brief-proposed";

/// Payload for [`EVENT_BRIEF_PROPOSED`]. Run id + user-authored schedule label + size only.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BriefProposedPayload {
    /// The staged `brief_runs` row id (opaque).
    pub run_id: String,
    /// The schedule's user-authored label (config data the user typed, not meeting content).
    pub label: String,
    /// Size of the proposed markdown in bytes (a coarse signal — never the content).
    pub char_count: usize,
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

/// Brain v3 PR-2 — progress for an in-flight document import (`import_document`): the extract →
/// chunk → embed pipeline for ONE document. Carries the document id + a stage + counts ONLY — NO PII
/// (no filename, no text, no heading; the id is a random UUID). Lets the Brain tab show a progress
/// bar for a large PDF instead of a frozen dialog.
pub const EVENT_DOC_IMPORT: &str = "murmur://doc-import";

/// Payload for [`EVENT_DOC_IMPORT`]. `stage` is `"extracting"` | `"chunking"` | `"embedding"` |
/// `"done"`. `done`/`total` are real counts WITHIN a stage: PAGES for `"extracting"` (page k of N of
/// a PDF/scanned OCR), and embed SUB-BATCHES for `"embedding"` (batch k of M). `0`/`0` for stages
/// with no natural count (chunking). `truncated` is `true` ONLY on the final `"done"` event when the
/// scanned-PDF OCR page cap was reached (some scanned pages were skipped — partial content); the FE
/// surfaces a "some scanned pages exceeded the limit" notice. The `document_id` is a random UUID (no
/// content). NO PII.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocImportPayload {
    pub document_id: String,
    /// "extracting" | "chunking" | "embedding" | "done"
    pub stage: String,
    pub done: usize,
    pub total: usize,
    /// `true` on the final `"done"` event when the OCR page cap truncated a huge scanned document
    /// (partial content). `false` on every non-final stage and on a complete import.
    pub truncated: bool,
}

/// Emit [`EVENT_DOC_IMPORT`] for a NON-FINAL stage (`extracting`/`chunking`/`embedding`) with real
/// `done`/`total` counts (best-effort; swallows the emit failure with a non-PII warn). `truncated` is
/// always `false` here — only the terminal `done` event (via [`emit_doc_import_done`]) can flag it.
pub fn emit_doc_import(app: &AppHandle, document_id: &str, stage: &str, done: usize, total: usize) {
    if let Err(e) = app.emit(
        EVENT_DOC_IMPORT,
        DocImportPayload {
            document_id: document_id.to_string(),
            stage: stage.to_string(),
            done,
            total,
            truncated: false,
        },
    ) {
        tracing::warn!(target: "documents", error = %e, stage, "emit doc-import failed");
    }
}

/// Emit the TERMINAL [`EVENT_DOC_IMPORT`] `"done"` event, carrying `truncated` — `true` when the
/// scanned-PDF OCR page cap skipped pages (partial content). Best-effort; swallows the emit failure.
pub fn emit_doc_import_done(app: &AppHandle, document_id: &str, truncated: bool) {
    if let Err(e) = app.emit(
        EVENT_DOC_IMPORT,
        DocImportPayload {
            document_id: document_id.to_string(),
            stage: "done".to_string(),
            done: 0,
            total: 0,
            truncated,
        },
    ) {
        tracing::warn!(target: "documents", error = %e, stage = "done", "emit doc-import done failed");
    }
}

/// Progress for an in-flight BULK import (`import_notion_export`): one event per page written, so
/// the Settings → Imports screen shows a real "k of N" bar instead of a frozen dialog on a
/// thousand-page workspace. A stalled counter after the counting phase is the single most-reported
/// bulk-import complaint in the prior art, so the denominator ticks per page, not per stage.
/// Carries COUNTS ONLY — never a page title, a body, a filename or a path. NO PII.
pub const EVENT_BULK_IMPORT: &str = "murmur://bulk-import";

/// Payload for [`EVENT_BULK_IMPORT`]. `stage` is `"scanning"` | `"importing"` | `"linking"` |
/// `"done"`; `done`/`total` are page counts within that stage. Deliberately carries no id and no
/// title: a bulk import's ids are meaningful only in aggregate, and a title would be content.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkImportPayload {
    /// "scanning" | "importing" | "linking" | "done"
    pub stage: String,
    pub done: usize,
    pub total: usize,
}

/// Emit [`EVENT_BULK_IMPORT`] (best-effort; swallows the emit failure with a non-PII warn).
pub fn emit_bulk_import(app: &AppHandle, stage: &str, done: usize, total: usize) {
    if let Err(e) = app.emit(
        EVENT_BULK_IMPORT,
        BulkImportPayload {
            stage: stage.to_string(),
            done,
            total,
        },
    ) {
        tracing::warn!(target: "import", error = %e, stage, "emit bulk-import failed");
    }
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
    /// "idle" | "recording" | "transcribing" | "summarizing" | "exporting" | "done" |
    /// "finalized" | "error". Pipeline `done` is progress; `finalized` is the command/recovery
    /// owner's successful end-of-lifecycle boundary.
    pub stage: String,
    /// human-readable, NO PII
    pub message: String,
    pub meeting_id: Option<String>,
}

/// Emit the one truthful recording success boundary. Pipeline `saved`/`done` stages can precede
/// command-owned model retirement or startup-recovery cleanup, so every owner calls this only after
/// its full success tail has completed. Best-effort: an event-bus failure cannot undo a durable
/// recovered note. Carries an opaque meeting id and fixed text only — no recording content.
pub(crate) fn emit_recording_finalized(app: &AppHandle, meeting_id: &str) {
    if let Err(error) = app.emit(EVENT_STATUS, recording_finalized_payload(meeting_id)) {
        tracing::warn!(
            target: "pipeline",
            error = %error,
            "failed to emit finalized recording status"
        );
    }
}

fn recording_finalized_payload(meeting_id: &str) -> StatusPayload {
    StatusPayload {
        stage: "finalized".into(),
        message: "Recording finalized.".into(),
        meeting_id: Some(meeting_id.to_string()),
    }
}

/// Unblock recording renderers when the note pipeline succeeded but startup recovery's own
/// cleanup tail did not. This is deliberately NOT `finalized`: the exact recovery artifacts remain
/// owned for a later retry. Fixed text + opaque meeting id only; never includes an error string,
/// source path, title, transcript, or note content.
pub(crate) fn emit_recording_recovery_failed(app: &AppHandle, meeting_id: &str) {
    if let Err(error) = app.emit(EVENT_STATUS, recording_recovery_failed_payload(meeting_id)) {
        tracing::warn!(
            target: "startup",
            error = %error,
            "failed to emit recording recovery error status"
        );
    }
}

fn recording_recovery_failed_payload(meeting_id: &str) -> StatusPayload {
    StatusPayload {
        stage: "error".into(),
        message: "Recording recovery cleanup was incomplete.".into(),
        meeting_id: Some(meeting_id.to_string()),
    }
}

/// Content-free recording terminal capability. Production routes through the typed event helpers;
/// headless lifecycle tests substitute a recorder so a late lock refusal can prove it emitted
/// neither a false success nor a duplicate error.
pub(crate) trait RecordingTerminalNotifier {
    fn recording_finalized(&self, meeting_id: &str);
    fn recording_cleanup_failed(&self, meeting_id: &str);
}

impl RecordingTerminalNotifier for AppHandle {
    fn recording_finalized(&self, meeting_id: &str) {
        emit_recording_finalized(self, meeting_id);
    }

    fn recording_cleanup_failed(&self, meeting_id: &str) {
        emit_recording_recovery_failed(self, meeting_id);
    }
}

/// Emitted once after transcription when the cross-stream echo dedup removed ≥1 mic-echo
/// segment (the user recorded on speakers). Counts only — NO PII. The FE shows a toast
/// recommending headphones.
pub const EVENT_ECHO_SUPPRESSED: &str = "murmur://echo-suppressed";

/// Payload for [`EVENT_ECHO_SUPPRESSED`]. Counts only — NO PII.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EchoSuppressedPayload {
    /// Number of mic-echo segments removed from the transcript.
    pub suppressed: usize,
    pub meeting_id: String,
}

/// Emitted after an AUTO-prune removed ≥1 old recording's audio to stay under the storage cap.
/// Counts/bytes ONLY — NO PII. The FE refreshes the usage bar + shows a "freed space" toast.
pub const EVENT_STORAGE_PRUNED: &str = "murmur://storage-pruned";

/// Payload for [`EVENT_STORAGE_PRUNED`]. Bytes + count only — NO PII.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoragePrunedPayload {
    pub freed_bytes: u64,
    pub pruned_count: u64,
}

/// Emitted ONCE per recording when the mic capture hits the [`crate::audio::recorder::MAX_RECORDING_SECONDS`]
/// (4h) hard TIME cap and self-stops: past this point the capture thread has torn the stream down,
/// the meter reads 0, and everything spoken is silently dropped. Fires on the RISING edge only
/// (capped false→true, deduped per recording). The FE surfaces a "maximum recording length reached"
/// notice and finalizes the meeting by invoking `stop_recording` (the buffer is capped-but-intact,
/// so Stop still produces a note). This is a TIME cap — distinct from the byte/size-based
/// [`EVENT_STORAGE_PRUNED`] storage cap. Carries a length, NO PII.
pub const EVENT_RECORDING_CAPPED: &str = "murmur://recording-capped";

/// Payload for [`EVENT_RECORDING_CAPPED`]. The cap length in seconds only — NO PII (no content,
/// no meeting id, no path).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingCappedPayload {
    /// The maximum recording length (seconds) that was reached — i.e. the cap.
    pub limit_seconds: u64,
}

/// Emit [`EVENT_RECORDING_CAPPED`] to the FE (best-effort). The caller decides WHEN to fire (once,
/// on the rising edge — see [`crate::audio::recorder::should_emit_cap_notice`]); this only performs
/// the emit and swallows the failure with a `tracing::warn!` so a failed emit can NEVER break the
/// live status poll or the recording. NO PII is logged (a flag only).
pub fn emit_recording_capped(app: &AppHandle) {
    if let Err(e) = app.emit(
        EVENT_RECORDING_CAPPED,
        RecordingCappedPayload {
            limit_seconds: crate::audio::recorder::MAX_RECORDING_SECONDS,
        },
    ) {
        tracing::warn!(
            target: "audio",
            error = %e,
            "failed to emit recording-capped notice"
        );
    }
}

/// Emitted exactly once when realtime capture terminates on a device/storage/authority fault.
/// The payload contains only an allowlisted code and counts; the UI uses it as the auto-Stop seam
/// that finalizes the exact durable prefix instead of leaving a red-but-still-owned session.
pub const EVENT_RECORDING_CAPTURE_FAULT: &str = "murmur://recording-capture-fault";

/// System audio disappeared while the microphone was muted. The backend has already restored the
/// mic before emitting this content-free event; renderers only resync their control and explain it.
pub const EVENT_MIC_AUTO_UNMUTED: &str = "murmur://mic-auto-unmuted";

pub fn emit_mic_auto_unmuted(app: &AppHandle) {
    if let Err(error) = app.emit(EVENT_MIC_AUTO_UNMUTED, ()) {
        tracing::warn!(target: "audio", error = %error, "failed to emit mic-auto-unmuted notice");
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingCaptureFaultPayload {
    pub code: &'static str,
    pub retained_frames: u64,
    pub sample_rate: u32,
}

pub fn emit_recording_capture_fault(
    app: &AppHandle,
    fault: crate::audio::recorder::CaptureFault,
    retained_frames: u64,
    sample_rate: u32,
) {
    use crate::audio::recorder::CaptureFault;
    let code = match fault {
        CaptureFault::StreamError => "STREAM_ERROR",
        CaptureFault::CaptureThreadFailed => "CAPTURE_THREAD_FAILED",
        CaptureFault::ResidentCapacityExhausted => "RESIDENT_CAPACITY_EXHAUSTED",
        CaptureFault::BufferLockContended => "BUFFER_LOCK_CONTENDED",
        CaptureFault::InvalidInterleavedInput => "INVALID_INTERLEAVED_INPUT",
        CaptureFault::FrameCounterOverflow => "FRAME_COUNTER_OVERFLOW",
        CaptureFault::CheckpointAuthorityLost => "CHECKPOINT_AUTHORITY_LOST",
    };
    if let Err(error) = app.emit(
        EVENT_RECORDING_CAPTURE_FAULT,
        RecordingCaptureFaultPayload {
            code,
            retained_frames,
            sample_rate,
        },
    ) {
        tracing::warn!(target: "audio", error = %error, "failed to emit capture-fault notice");
    }
}

/// Emitted after a background org-feed sync tick INGESTED or TOMBSTONED ≥1 item — i.e. the local
/// org (Shared Brain) replica actually changed this tick. Lets an open FE view (the Notes org
/// picker, the Settings shared-brain list) refresh WITHOUT polling. Counts only, NO PII (no item
/// ids, titles, or content) — it is purely a "something changed, re-fetch" ping.
pub const EVENT_ORG_FEED_UPDATED: &str = "murmur://org-feed-updated";

/// Progress of one CONTAINER share — a Space or Folder publishing N items. Counts only, NO PII (no
/// folder name, item id, or title). The sheet renders a determinate bar from it, because a share of
/// a whole Space is N sequential round-trips and a spinner cannot say how far it got.
pub const EVENT_CONTAINER_SHARE_PROGRESS: &str = "murmur://container-share-progress";

/// Payload for [`EVENT_CONTAINER_SHARE_PROGRESS`]. Two counts only — NO PII.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerShareProgressPayload {
    /// Items attempted so far, published or failed.
    pub done: u32,
    /// Items the plan will attempt in total.
    pub total: u32,
}

/// Emit [`EVENT_CONTAINER_SHARE_PROGRESS`] (best-effort). A failed emit only costs the progress
/// bar an update; it must never affect the share itself.
pub fn emit_container_share_progress(app: &AppHandle, done: u32, total: u32) {
    if let Err(e) = app.emit(
        EVENT_CONTAINER_SHARE_PROGRESS,
        ContainerShareProgressPayload { done, total },
    ) {
        tracing::warn!(
            target: "org",
            error = %e,
            "failed to emit container-share progress"
        );
    }
}

/// Payload for [`EVENT_ORG_FEED_UPDATED`]. A single count only — NO PII. `orgsChanged` is the number
/// of joined orgs whose feed produced ≥1 ingest/tombstone this tick (the all-orgs background tick
/// aggregates across orgs). The FE treats any arrival as "re-fetch the org lists".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgFeedUpdatedPayload {
    /// Number of orgs whose replica changed this tick (≥1 when emitted).
    pub orgs_changed: u32,
}

/// Emit [`EVENT_ORG_FEED_UPDATED`] to the FE (best-effort). Fired ONLY on a productive tick
/// (≥1 ingest/tombstone). Swallows the emit failure with a `tracing::warn!` so a failed emit can
/// NEVER break the background sync loop. NO PII (a count only).
pub fn emit_org_feed_updated(app: &AppHandle, orgs_changed: u32) {
    if let Err(e) = app.emit(
        EVENT_ORG_FEED_UPDATED,
        OrgFeedUpdatedPayload { orgs_changed },
    ) {
        tracing::warn!(
            target: "org",
            error = %e,
            "failed to emit org-feed-updated notice"
        );
    }
}

/// The [`EVENT_ORG_FEED_UPDATED`] emit seen as a capability rather than as a concrete `AppHandle`.
///
/// Exists so the "did this command actually tell the FE to re-fetch?" half of an org-item mutation is
/// UNIT-TESTABLE. An org-item-mutating command that stays silent leaves `OrgBrainService.loadOrgs()`,
/// the Settings organization section and the org-item viewer showing a row that no longer exists
/// until an unrelated background tick happens to be productive (up to
/// [`crate::commands::ORG_SYNC_TICK_SECS`] later, or never on a quiet feed). That omission shipped
/// once already (`revoke_org_share`), so THAT command's emit is expressed as a seam a test can
/// observe rather than a bare line that is easy to forget and impossible to assert on. The other
/// org-item-mutating commands still call [`emit_org_feed_updated`] directly — this trait is the
/// testing seam for the path that regressed, not (yet) a repo-wide convention.
///
/// The production implementation is the `AppHandle` one below; the crate's tests substitute a
/// recording double. Content-free by construction — the only argument is a count.
///
/// `Send + Sync` is REQUIRED, not decorative: the notifier is held across the `await` of the revoke
/// inside an `async` `#[tauri::command]`, and Tauri's `generate_handler!` demands a `Send` future.
/// Without the supertraits `&dyn OrgFeedNotifier` is neither, and the command fails to compile with
/// "future returned by `revoke_org_share` is not `Send`". Both implementations (`AppHandle` and the
/// tests' `Mutex`-backed recorder) already satisfy them.
pub trait OrgFeedNotifier: Send + Sync {
    /// Tell the FE that `orgs_changed` orgs' local replicas changed and every org view should
    /// re-fetch. Best-effort: an implementation must never fail the caller's operation.
    fn org_feed_updated(&self, orgs_changed: u32);
}

impl OrgFeedNotifier for AppHandle {
    fn org_feed_updated(&self, orgs_changed: u32) {
        emit_org_feed_updated(self, orgs_changed);
    }
}

/// DELETE FAN-OUT FIX (2026-07-15): emitted once a note/meeting delete has FULLY succeeded — local
/// rows gone AND (per the org-share revoke-cascade fix) any live org shares of it already revoked.
/// Root cause this closes: no delete flow ever told OTHER open consumers (most visibly the tab-strip,
/// `TabsService`) that content vanished, so a stale tab opened from a different surface than the one
/// that deleted it stayed open and clickable, landing on a 404/error state. Content-free: an id + a
/// kind discriminator only — never a title or any other content.
pub const EVENT_CONTENT_DELETED: &str = "murmur://content-deleted";

/// The trash changed — something was moved in, restored, or purged. CONTENT-FREE: carries only the
/// entry COUNT, so the sidebar badge updates without any surface learning a label or payload. Every
/// consumer refetches through the gated `list_trash`, which masks a sealed entry.
pub const EVENT_TRASH_UPDATED: &str = "murmur://trash-updated";

/// Payload for [`EVENT_CONTENT_DELETED`]. `kind` is `"note"` | `"meeting"`; `id` is the deleted note's
/// or meeting's id (an opaque identifier, not content).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentDeletedPayload {
    /// `"note"` | `"meeting"`.
    pub kind: &'static str,
    /// The deleted note/meeting's id.
    pub id: String,
}

/// Emit [`EVENT_CONTENT_DELETED`] to the FE (best-effort). Swallows the emit failure with a
/// `tracing::warn!` so a failed emit can NEVER turn a successful delete into a reported failure. NO
/// PII (id + kind only).
pub fn emit_content_deleted(app: &AppHandle, kind: &'static str, id: &str) {
    if let Err(e) = app.emit(
        EVENT_CONTENT_DELETED,
        ContentDeletedPayload {
            kind,
            id: id.to_string(),
        },
    ) {
        tracing::warn!(
            target: "notes",
            error = %e,
            "failed to emit content-deleted notice"
        );
    }
}

/// Emitted after a Vault-Audit pass completes and after each finding resolve (accept/dismiss) —
/// a "something changed, re-fetch" ping for the FE audit inbox, exactly the
/// [`EVENT_ORG_FEED_UPDATED`] shape. Carries the pending COUNT only — never a finding's content.
pub const EVENT_AUDIT_UPDATED: &str = "murmur://audit-updated";

/// Payload for [`EVENT_AUDIT_UPDATED`]. A single count only — NO PII.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditUpdatedPayload {
    /// Pending findings after the pass/resolve.
    pub pending: u32,
}

/// Emit [`EVENT_AUDIT_UPDATED`] to the FE (best-effort). Swallows the emit failure with a
/// `tracing::warn!` so a failed emit can NEVER fail the audit pass or a resolve. NO PII (a count).
pub fn emit_audit_updated(app: &AppHandle, pending: u32) {
    if let Err(e) = app.emit(EVENT_AUDIT_UPDATED, AuditUpdatedPayload { pending }) {
        tracing::warn!(
            target: "audit",
            error = %e,
            "failed to emit audit-updated notice"
        );
    }
}

/// Content-free reminder invalidation ping. The app-process scheduler and every reminder mutation
/// emit only the unread due count; reminder titles/details never enter the event bus or logs.
pub const EVENT_REMINDERS_UPDATED: &str = "murmur://reminders-updated";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemindersUpdatedPayload {
    pub due_inbox_count: u64,
}

pub fn emit_reminders_updated(app: &AppHandle, due_inbox_count: u64) {
    if let Err(e) = app.emit(
        EVENT_REMINDERS_UPDATED,
        RemindersUpdatedPayload { due_inbox_count },
    ) {
        tracing::warn!(
            target: "reminders",
            error = %e,
            "failed to emit reminders-updated notice"
        );
    }
}

/// Content-free invalidation for one meeting/note Smart-reminder card. Canonical mutations enqueue
/// this durable ping in SQLCipher; the background drain emits only the source kind + opaque id so
/// the FE can re-fetch through the normal gated command.
pub const EVENT_REMINDER_SOURCE_UPDATED: &str = "murmur://reminder-source-updated";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderSourceUpdatedPayload {
    pub kind: String,
    pub id: String,
}

/// Best-effort emit. The boolean is the durable outbox acknowledgement decision: `true` means the
/// worker may CAS-delete that exact queue revision; `false` leaves it for replay.
pub fn emit_reminder_source_updated(app: &AppHandle, kind: &str, id: &str) -> bool {
    match app.emit(
        EVENT_REMINDER_SOURCE_UPDATED,
        ReminderSourceUpdatedPayload {
            kind: kind.to_string(),
            id: id.to_string(),
        },
    ) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(
                target: "reminders",
                error = %e,
                "failed to emit reminder-source-updated notice"
            );
            false
        }
    }
}

/// Global, content-free privacy invalidation emitted while the lock lifecycle guard is held,
/// immediately after visibility authority is revoked. Unlike source-edit invalidations this has no
/// source id: consumers must synchronously discard every cached reminder source title before
/// re-fetching through the canonical gated commands.
pub const EVENT_REMINDER_VISIBILITY_INVALIDATED: &str = "murmur://reminder-visibility-invalidated";

pub fn emit_reminder_visibility_invalidated(app: &AppHandle) -> bool {
    match app.emit(EVENT_REMINDER_VISIBILITY_INVALIDATED, ()) {
        Ok(()) => true,
        Err(e) => {
            tracing::error!(
                target: "reminders",
                error = %e,
                "failed to emit reminder visibility invalidation"
            );
            false
        }
    }
}

/// Global, content-free privacy barrier for every Ask Brain renderer cache. Emitted immediately
/// after a successful visibility reduction or destructive purge; consumers synchronously discard
/// loaded messages, history summaries, source labels and any in-flight result token.
pub const EVENT_ASK_HISTORY_INVALIDATED: &str = "murmur://ask-history-invalidated";

pub fn emit_ask_history_invalidated(app: &AppHandle) -> bool {
    match app.emit(EVENT_ASK_HISTORY_INVALIDATED, ()) {
        Ok(()) => true,
        Err(e) => {
            tracing::error!(
                target: "ask_history",
                error = %e,
                "failed to emit Ask history visibility invalidation"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `finalized` is the only terminal success stage consumed by the recording store. Bind its
    /// exact camelCase wire payload so recovery cannot silently fall back to pipeline `done`.
    #[test]
    fn recording_finalized_status_payload_is_stable() {
        assert_eq!(EVENT_STATUS, "meetnotes://status");
        let json = serde_json::to_string(&recording_finalized_payload("meeting-42")).unwrap();
        assert_eq!(
            json,
            r#"{"stage":"finalized","message":"Recording finalized.","meetingId":"meeting-42"}"#
        );
    }

    /// A cleanup-tail failure must unblock the renderer without claiming success or exposing the
    /// underlying filesystem/error detail.
    #[test]
    fn recording_recovery_failed_status_payload_is_fixed_and_content_free() {
        let json = serde_json::to_string(&recording_recovery_failed_payload("meeting-42")).unwrap();
        assert_eq!(
            json,
            r#"{"stage":"error","message":"Recording recovery cleanup was incomplete.","meetingId":"meeting-42"}"#
        );
        assert!(!json.contains("/"));
    }

    /// The FE listens on this exact event name; a rename silently drops the live-refresh.
    #[test]
    fn org_feed_updated_event_name_is_stable() {
        assert_eq!(EVENT_ORG_FEED_UPDATED, "murmur://org-feed-updated");
    }

    /// The FE listens on this exact event name; a rename silently drops the audit-inbox refresh.
    #[test]
    fn audit_updated_event_name_and_payload_are_stable() {
        assert_eq!(EVENT_AUDIT_UPDATED, "murmur://audit-updated");
        let json = serde_json::to_string(&AuditUpdatedPayload { pending: 4 }).unwrap();
        assert_eq!(json, r#"{"pending":4}"#);
    }

    #[test]
    fn reminders_updated_event_is_count_only() {
        assert_eq!(EVENT_REMINDERS_UPDATED, "murmur://reminders-updated");
        let json = serde_json::to_string(&RemindersUpdatedPayload { due_inbox_count: 2 }).unwrap();
        assert_eq!(json, r#"{"dueInboxCount":2}"#);
    }

    #[test]
    fn reminder_source_updated_event_is_opaque_id_and_kind_only() {
        assert_eq!(
            EVENT_REMINDER_SOURCE_UPDATED,
            "murmur://reminder-source-updated"
        );
        let json = serde_json::to_string(&ReminderSourceUpdatedPayload {
            kind: "meeting".into(),
            id: "m1".into(),
        })
        .unwrap();
        assert_eq!(json, r#"{"kind":"meeting","id":"m1"}"#);
    }

    #[test]
    fn reminder_visibility_invalidated_event_is_content_free() {
        assert_eq!(
            EVENT_REMINDER_VISIBILITY_INVALIDATED,
            "murmur://reminder-visibility-invalidated"
        );
        assert_eq!(serde_json::to_string(&()).unwrap(), "null");
    }

    #[test]
    fn ask_history_invalidated_event_is_stable_and_content_free() {
        assert_eq!(
            EVENT_ASK_HISTORY_INVALIDATED,
            "murmur://ask-history-invalidated"
        );
        assert_eq!(serde_json::to_string(&()).unwrap(), "null");
    }

    /// The FE listens on this exact event name; a rename silently drops the tab-strip fan-out.
    #[test]
    fn content_deleted_event_name_is_stable() {
        assert_eq!(EVENT_CONTENT_DELETED, "murmur://content-deleted");
    }

    /// The payload must serialize as camelCase `{"kind":"note","id":"n1"}` (the FE contract) —
    /// content-free (id + kind discriminator only).
    #[test]
    fn content_deleted_payload_is_camel_case_id_and_kind_only() {
        let json = serde_json::to_string(&ContentDeletedPayload {
            kind: "note",
            id: "n1".to_string(),
        })
        .unwrap();
        assert_eq!(json, r#"{"kind":"note","id":"n1"}"#);
    }

    /// The payload must serialize `orgs_changed` as camelCase `orgsChanged` (the FE contract) and
    /// carry NO PII field — a count only.
    #[test]
    fn org_feed_updated_payload_is_camel_case_count_only() {
        let json = serde_json::to_string(&OrgFeedUpdatedPayload { orgs_changed: 3 }).unwrap();
        assert_eq!(json, r#"{"orgsChanged":3}"#);
    }

    /// The global org auto-sync cadence is 1 minute (guards the 120→60 change — members have no push,
    /// only this pull, so it bounds a colleague's shared-note visibility to ~1 min).
    #[test]
    fn org_sync_tick_cadence_is_one_minute() {
        assert_eq!(crate::commands::ORG_SYNC_TICK_SECS, 60);
    }

    /// The FE listens on this exact event name; a rename silently drops the import progress bar.
    #[test]
    fn doc_import_event_name_is_stable() {
        assert_eq!(EVENT_DOC_IMPORT, "murmur://doc-import");
    }

    /// The doc-import payload serializes as camelCase with the real per-stage counts AND the terminal
    /// `truncated` flag (the FE contract — [`crate::events::DocImportPayload`] ⇒ `DocImportProgress`).
    /// A non-final stage carries `truncated: false`; the terminal `done` event can carry `true`.
    #[test]
    fn doc_import_payload_is_camel_case_counts_and_truncated() {
        let extracting = DocImportPayload {
            document_id: "d1".into(),
            stage: "extracting".into(),
            done: 12,
            total: 300,
            truncated: false,
        };
        let json = serde_json::to_string(&extracting).unwrap();
        assert_eq!(
            json,
            r#"{"documentId":"d1","stage":"extracting","done":12,"total":300,"truncated":false}"#
        );
        // The terminal done event can flag a truncated (partial) OCR import.
        let done = DocImportPayload {
            document_id: "d1".into(),
            stage: "done".into(),
            done: 0,
            total: 0,
            truncated: true,
        };
        let json = serde_json::to_string(&done).unwrap();
        assert_eq!(
            json,
            r#"{"documentId":"d1","stage":"done","done":0,"total":0,"truncated":true}"#
        );
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashUpdatedPayload {
    /// How many entries the trash now holds. NOT a list — no label, no payload, no ids.
    pub count: i64,
}

/// Emit [`EVENT_TRASH_UPDATED`] to the FE (best-effort). Swallows the emit failure with a
/// `tracing::warn!` so a failed emit can NEVER turn a successful trash/restore/purge into a
/// reported failure. NO PII (a count only).
pub fn emit_trash_updated(app: &AppHandle, count: i64) {
    if let Err(e) = app.emit(EVENT_TRASH_UPDATED, TrashUpdatedPayload { count }) {
        tracing::warn!(
            target: "trash",
            error = %e,
            "failed to emit trash-updated notice"
        );
    }
}
