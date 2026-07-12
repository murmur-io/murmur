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

/// Emitted after a background org-feed sync tick INGESTED or TOMBSTONED ≥1 item — i.e. the local
/// org (Shared Brain) replica actually changed this tick. Lets an open FE view (the Notes org
/// picker, the Settings shared-brain list) refresh WITHOUT polling. Counts only, NO PII (no item
/// ids, titles, or content) — it is purely a "something changed, re-fetch" ping.
pub const EVENT_ORG_FEED_UPDATED: &str = "murmur://org-feed-updated";

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
    if let Err(e) = app.emit(EVENT_ORG_FEED_UPDATED, OrgFeedUpdatedPayload { orgs_changed }) {
        tracing::warn!(
            target: "org",
            error = %e,
            "failed to emit org-feed-updated notice"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The FE listens on this exact event name; a rename silently drops the live-refresh.
    #[test]
    fn org_feed_updated_event_name_is_stable() {
        assert_eq!(EVENT_ORG_FEED_UPDATED, "murmur://org-feed-updated");
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
}
