//! Best-effort LIVE transcription: a read-only background loop that runs while a recording
//! is in progress. Every few seconds it snapshots the tail of the mic buffer, transcribes
//! that window with Whisper, and emits a [`crate::events::EVENT_LIVE_CAPTION`] event.
//!
//! Design guarantees (so this can never destabilise the core record/transcribe flow):
//! - It only *reads* a clone of the recent samples (`Recorder::snapshot_tail`); it never
//!   drains or mutates the capture buffer.
//! - It self-terminates as soon as the recorder is gone (recording stopped/taken).
//! - Every error (model load, resample, transcribe) is logged and skipped — the recording
//!   and the authoritative final transcript produced at stop are unaffected.
//! - Live quality/latency depends on the chosen model (use a small model for snappy
//!   captions); a slow tick just means less frequent captions, never a broken recording.

use std::path::PathBuf;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;
use crate::transcribe::Transcriber;

/// How often to attempt a live caption.
const TICK: Duration = Duration::from_millis(3000);
/// How many trailing seconds of audio to transcribe each tick (overlapping window).
const WINDOW_SECS: usize = 14;

/// How many consecutive ticks the SAME spoken wake stays de-duplicated. Consecutive ~14s tails
/// OVERLAP heavily (a `TICK` of 3s into a `WINDOW_SECS` of 14s ⇒ the same vocative is visible for
/// ~4-5 ticks), so without dedup one spoken "Klaudku" would re-fire every tick. A window of 5 ticks
/// (≈ 15s ≈ one full tail) collapses the overlapping re-detections of ONE utterance into a single
/// dispatch, while a fresh "Klaudku" later — or the same command after the window lapses — DOES
/// fire again. Recall is preserved (a NEW ask always catches); only the duplicate echo is dropped.
const WAKE_DEDUP_TICKS: u32 = 5;

#[derive(serde::Serialize, Clone)]
struct LiveCaption {
    text: String,
}

/// DEDUP state for the in-meeting wake trigger (the #23 fix). [`detect_wake`] now fires ANYWHERE in
/// the rolling, OVERLAPPING ~14s tail, so the SAME spoken "Klaudku zrób research" would otherwise
/// re-fire on every tick it remains visible. This collapses the overlapping re-detections of ONE
/// utterance into a SINGLE dispatch: it remembers the last fired wake (its normalized command text)
/// and a tick countdown, skipping a detection that matches the remembered one while the countdown is
/// live. A DIFFERENT command, or the SAME command after the window lapses, fires again — so one
/// spoken wake = one dispatch, but a fresh ask later in the same recording still catches.
///
/// Pure + stateful — held by the live loop, advanced once per tick via [`Self::tick`] and consulted
/// per wake hit via [`Self::should_fire`]. No I/O. Headless-testable.
#[derive(Default)]
struct WakeDedup {
    /// The normalized command of the last FIRED wake, while it is still suppressing repeats.
    last: Option<String>,
    /// Ticks remaining before `last` stops suppressing a matching repeat (0 = no active suppression).
    cooldown: u32,
}

impl WakeDedup {
    /// Advance one live tick: age out an expired suppression window. Call once per loop iteration,
    /// BEFORE the wake check, so the countdown reflects ticks elapsed since the last fire.
    fn tick(&mut self) {
        if self.cooldown > 0 {
            self.cooldown -= 1;
            if self.cooldown == 0 {
                self.last = None;
            }
        }
    }

    /// Decide whether a wake hit with normalized command `cmd` should FIRE (dispatch+emit) on this
    /// tick. Fires when it is a NEW/different command, or the suppression window for the same command
    /// has lapsed. On a fire it arms the suppression window ([`WAKE_DEDUP_TICKS`]); on a suppressed
    /// repeat it leaves the window untouched (so the cooldown counts from the FIRST fire, not the
    /// last echo — the overlap of one utterance can't extend the window indefinitely).
    fn should_fire(&mut self, cmd: &str) -> bool {
        let key = normalize_command(cmd);
        if self.cooldown > 0 && self.last.as_deref() == Some(key.as_str()) {
            return false; // overlapping re-detection of the same just-fired wake → skip.
        }
        self.last = Some(key);
        self.cooldown = WAKE_DEDUP_TICKS;
        true
    }
}

/// Normalize a wake command for dedup comparison: lowercase, collapse whitespace, drop surrounding
/// punctuation. So the SAME utterance transcribed with slightly different trailing punctuation /
/// spacing / casing across overlapping tails still compares equal and is de-duplicated.
fn normalize_command(cmd: &str) -> String {
    cmd.to_lowercase()
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Spawn the live-caption loop for the current recording. Returns immediately; the loop
/// runs on its own OS thread and ends on its own when recording stops.
pub fn spawn(app: AppHandle, model_path: PathBuf, lang: Option<String>) {
    let _ = std::thread::Builder::new()
        .name("murmur-live-captions".into())
        .spawn(move || run(app, model_path, lang));
}

fn run(app: AppHandle, model_path: PathBuf, lang: Option<String>) {
    // Fresh transcript per recording: clear any leftover from a previous meeting so the in-meeting
    // assistant can never answer about a stale recording.
    if let Ok(mut lt) = app.state::<AppState>().live_transcript.lock() {
        lt.clear();
    }
    let transcriber = match Transcriber::load(&model_path) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(target: "live", error = %e, "live captions disabled: model load failed");
            return;
        }
    };

    // DEDUP state for the wake trigger (#23): detect_wake now fires ANYWHERE in the overlapping tail,
    // so without this the same spoken wake re-fires every tick it stays visible. Lives across ticks.
    let mut wake_dedup = WakeDedup::default();

    loop {
        std::thread::sleep(TICK);
        // Age out an expired wake-suppression window once per tick (before this tick's wake check).
        wake_dedup.tick();

        // Snapshot the recent tail; stop as soon as the recording is gone.
        let snapshot = {
            let state = app.state::<AppState>();
            let guard = match state.recorder.lock() {
                Ok(g) => g,
                Err(_) => break,
            };
            match guard.as_ref() {
                Some(r) => {
                    let rate = r.source_sample_rate();
                    Some((r.snapshot_tail(WINDOW_SECS * rate as usize), rate))
                }
                None => None,
            }
        };
        let Some((tail, rate)) = snapshot else {
            break; // recorder taken → recording stopped
        };
        if rate == 0 || tail.len() < rate as usize {
            continue; // <1s captured so far — nothing worth transcribing yet
        }

        let samples_16k = match crate::audio::resample_to_16k(&tail, rate) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(target: "live", error = %e, "live resample tick failed");
                continue;
            }
        };

        // LIVE captions use the Fast (greedy/best_of:1) profile via `transcribe` — NOT the
        // batch beam-search path. Captions tick every few seconds on overlapping windows, so
        // latency must dominate; beam search + temperature fallback would burn CPU per tick.
        // The authoritative high-quality transcript is produced once at Stop (pipeline.rs).
        match transcriber.transcribe(&samples_16k, lang.as_deref()) {
            Ok(t) => {
                let text = t
                    .segments
                    .iter()
                    .map(|s| s.text.trim())
                    .collect::<Vec<_>>()
                    .join(" ")
                    .trim()
                    .to_string();

                // Accumulate the rolling caption into the live transcript (best-effort, read-only) so
                // the in-meeting assistant can answer questions about the recording IN PROGRESS.
                // Deduped (overlapping tails) + size-bounded inside `accumulate_live_caption`.
                accumulate_live_caption(app.state::<AppState>().inner(), &text);

                // MANUAL voice-command capture (the button trigger) — CLICK-TO-STOP, checked EVERY
                // tick, independent of the wake path and independent of `realtime_reactions`. When the
                // user has clicked "ask the assistant" the growing post-click window IS the command:
                // no wake word, no word-order requirement. We ACCUMULATE it tick by tick and keep
                // listening until the user clicks "stop" (`end_voice_command` → dispatch the FULL
                // utterance) — only a generous backstop cap forces an end so it can't listen forever.
                // Best-effort + panic-free: a poisoned lock or empty tail simply leaves the capture
                // armed; the dispatch runs off-thread exactly like the wake path.
                if let Some(decision) =
                    step_manual_capture(&app, &transcriber, lang.as_deref(), &text)
                {
                    match decision {
                        ManualCaptureDecision::Dispatch { command } => {
                            // Capture ENDED (user stop click OR the backstop cap) with a real
                            // utterance → stop "listening", show "thinking…", then resolve (keyword →
                            // brain → fallback) + dispatch the FULL accumulated command OFF the tick.
                            // PROCESSING is cleared by EVENT_VOICE_ACTION_RESULT when the answer lands.
                            let _ = app.emit(
                                crate::events::EVENT_VOICE_COMMAND_LISTENING,
                                crate::events::VoiceCommandListeningPayload { active: false },
                            );
                            let _ = app.emit(
                                crate::events::EVENT_VOICE_COMMAND_PROCESSING,
                                crate::events::VoiceCommandProcessingPayload { active: true },
                            );
                            spawn_assistant_turn(app.clone(), command);
                        }
                        ManualCaptureDecision::NothingHeard => {
                            // Budget expired with nothing heard → graceful, NOT a confusing
                            // "didn't catch an action". Clear the listening affordance + surface it.
                            let _ = app.emit(
                                crate::events::EVENT_VOICE_COMMAND_LISTENING,
                                crate::events::VoiceCommandListeningPayload { active: false },
                            );
                            let _ = app.emit(
                                crate::events::EVENT_VOICE_ACTION_RESULT,
                                crate::voice_action::VoiceActionResult::nothing_heard(),
                            );
                        }
                        ManualCaptureDecision::KeepListening => {}
                    }
                }

                if !text.is_empty() {
                    // In-meeting voice trigger (Phase A: DETECT + SURFACE only — NO action dispatch;
                    // that needs the local brain in a later phase). Best-effort + panic-free: the
                    // detection is a pure function and the emit error is ignored, so a wake miss/hit
                    // can NEVER disrupt the caption or the authoritative record/transcribe flow.
                    // DEDUP (#23): the rolling tail OVERLAPS, so a single spoken "Klaudku …" is
                    // visible for several ticks and `detect_wake` (now firing anywhere) would re-hit
                    // it each tick. `should_fire` collapses those overlapping re-detections of the
                    // SAME command into ONE dispatch, while a DIFFERENT command — or the same one
                    // after the suppression window lapses — still fires. Skipped repeats produce
                    // neither a dispatch nor an `EVENT_WAKE_DETECTED`, but the caption still emits.
                    if let Some(payload) =
                        wake_event_for(&text).filter(|p| wake_dedup.should_fire(&p.command))
                    {
                        // PII rule (§8): NEVER log the spoken command text — only the
                        // non-PII wake token and a coarse intent KIND (variant
                        // discriminant, no content). The full payload goes to the FE
                        // event, not the log.
                        let intent_kind = match &payload.intent {
                            crate::audio::wake::VoiceIntent::Research { .. } => "research",
                            crate::audio::wake::VoiceIntent::SlackSearch { .. } => "slack_search",
                            crate::audio::wake::VoiceIntent::Recall { .. } => "recall",
                            crate::audio::wake::VoiceIntent::CreateReminder { .. } => "create_reminder",
                            crate::audio::wake::VoiceIntent::NoteAside { .. } => "note_aside",
                            crate::audio::wake::VoiceIntent::Unknown { .. } => "unknown",
                        };
                        // Phase E (Flow B): when the opt-in `realtime_reactions` toggle is ON,
                        // DISPATCH the parsed action over the gated vault + consent-gated brain on a
                        // DETACHED thread (the brain call can take seconds — it must NEVER block the
                        // transcription tick or the caption). When OFF (default) we only SURFACE the
                        // hit, exactly as before. A dispatch panic is contained to its own thread.
                        let dispatch = {
                            let state = app.state::<AppState>();
                            state.config.lock().map(|c| should_dispatch(&c)).unwrap_or(false)
                        };
                        tracing::info!(
                            target: "voice",
                            matched = %payload.matched_phrase,
                            intent = intent_kind,
                            dispatch,
                            "wake word detected in live caption"
                        );
                        if dispatch {
                            spawn_assistant_turn(app.clone(), payload.command.clone());
                        }
                        let _ = app.emit(crate::events::EVENT_WAKE_DETECTED, payload);
                    }
                    let _ = app.emit(crate::events::EVENT_LIVE_CAPTION, LiveCaption { text });
                }
            }
            Err(e) => tracing::debug!(target: "live", error = %e, "live transcribe tick failed"),
        }
    }
}

/// The single decision point for whether a wake hit DISPATCHES an in-meeting action (Flow B) vs
/// only being surfaced (Phase A). Pure, so the off-vs-on behaviour is headless-testable. Dispatch
/// happens ONLY when the opt-in `realtime_reactions` toggle is ON — OFF (the default) is exactly
/// today's behaviour: wake detected + surfaced via `EVENT_WAKE_DETECTED`, no dispatch.
fn should_dispatch(config: &crate::settings::AppConfig) -> bool {
    config.realtime_reactions
}

/// Max agentic rounds on the cloud brain (decide → tool → … → answer). Bounded for live latency.
const CLOUD_MAX_STEPS: usize = 4;

/// Spawn ONE assistant turn on a DETACHED OS thread — the SINGLE entry to the in-meeting brain for the
/// wake path, the manual button, AND the text composer (all three funnel here with a `command` string).
/// The brain + tools can take seconds, so it MUST run off-thread; the whole body is best-effort +
/// panic-free and runs on its own thread, so even an unexpected panic is contained and can never
/// disrupt recording or the caption.
pub fn spawn_assistant_turn(app: AppHandle, command: String) {
    let _ = std::thread::Builder::new()
        .name("murmur-assistant-turn".into())
        .spawn(move || run_assistant_turn(&app, command));
}

/// The CARD path (wake / manual button / single-shot text composer): run the shared query core, then
/// EMIT `EVENT_VOICE_ACTION_RESULT` so the assistant card resolves the pending row. The per-tool trace
/// streams via `EVENT_ASSISTANT_TOOL`.
fn run_assistant_turn(app: &AppHandle, command: String) {
    let result = run_assistant_query(app, &command, &command, crate::events::EVENT_ASSISTANT_TOOL);
    let _ = app.emit(crate::events::EVENT_VOICE_ACTION_RESULT, result);
}

/// THE shared query core — the brain's executive entry, reused by the card turn AND the chat panel.
/// 1. Re-snapshot config + the LIVE `unlocked` set + the current meeting id — FRESH per turn (C6).
/// 2. Resolve `command` (the user's LATEST message) to an intent (keyword → brain → fallback). This is
///    NO LONGER a write router — the agentic loop's tool-use DECIDES whether to answer or write (the
///    user's explicit "no hardcoded regex" philosophy). The resolved `intent` is used ONLY as the
///    deterministic FLOOR's read fan-out + its surfaced `intent_kind` (so the no-consent/local path is
///    unchanged and there is no read-answer regression).
/// 3. EVERY request (voice / text / @brain) runs the model-driven AGENTIC LOOP (now `allow_writes`)
///    over `loop_user` (= `command` for the card; = the whole conversation for the chat). The agent
///    picks `save_note` / `create_reminder` (gated) or answers. On non-convergence / `Unavailable` (no
///    cloud consent) / the local-or-stub backend it FALLS THROUGH to the deterministic floor — the
///    floor no longer owns write routing; it is the informational safety net (ZERO read regression).
/// 4. PERSIST the interaction (gated, purged-on-seal) + RETURN the result; the per-tool trace streams
///    to `tool_event` (card vs chat). The CALLER delivers the result (emit for the card, return for chat).
pub fn run_assistant_query(
    app: &AppHandle,
    command: &str,
    loop_user: &str,
    tool_event: &'static str,
) -> crate::voice_action::VoiceActionResult {
    let state = app.state::<AppState>();
    let config = match state.config.lock() {
        Ok(c) => c.clone(),
        Err(_) => return crate::voice_action::VoiceActionResult::nothing_heard().with_command(command),
    };
    // C6: re-snapshot the LIVE unlocked set for THIS turn (never trust a snapshot taken before it).
    let unlocked = match state.unlocked_folders.lock() {
        Ok(u) => u.clone(),
        Err(_) => return crate::voice_action::VoiceActionResult::nothing_heard().with_command(command),
    };
    let meeting_id = state
        .current_meeting
        .lock()
        .ok()
        .and_then(|m| m.map(|id| id.to_string()))
        .unwrap_or_default();

    // Resolve an intent for the FLOOR only (its read fan-out + surfaced kind) — it NO LONGER routes
    // writes. The agentic loop (with writes) is the single executive path; the agent DECIDES.
    let intent = resolve_command_intent(&*state.reasoner, command);
    let result = run_informational(
        app,
        state.inner(),
        &config,
        &unlocked,
        &meeting_id,
        command,
        loop_user,
        &intent,
        tool_event,
    )
    .with_command(command);

    // PII rule: log only the coarse intent kind + status, never the command/summary text.
    tracing::info!(
        target: "voice",
        intent = %result.intent_kind,
        status = %result.status,
        "assistant query dispatched"
    );
    persist_interaction(state.inner(), &meeting_id, command, &result);
    result
}

/// Answer a request. On the CLOUD brain, run the model-driven agentic loop over the gated, WRITE-CAPABLE
/// tool executor (the agent DECIDES answer-vs-write); on convergence (`Ok(Some)`) map the outcome to a
/// `VoiceActionResult`. On `Ok(None)` (no convergence), any `Err` (e.g. `Unavailable` = no cloud
/// consent), or the LOCAL/stub backend, FALL THROUGH to the deterministic `handle_voice_action` floor —
/// the verified informational safety net (gated fan-out + cited synthesis + `needs_consent`).
///
/// The floor is INFORMATIONAL ONLY: it no longer owns write routing, so a `CreateReminder`/`NoteAside`
/// intent is DEMOTED to a `Research` over the literal command before flooring. Otherwise a no-consent /
/// local user saying "note that …" would trigger a deterministic write that bypassed the agent's
/// decision — exactly the hardcoded routing this change removes. So a write only ever happens when the
/// AGENT chose it; when the agent can't run, the user gets a deterministic informational answer instead.
#[allow(clippy::too_many_arguments)] // cohesive dispatch surface: gated state + loop-user + trace event.
fn run_informational(
    app: &AppHandle,
    state: &AppState,
    config: &crate::settings::AppConfig,
    unlocked: &std::collections::HashSet<String>,
    meeting_id: &str,
    command: &str,
    loop_user: &str,
    intent: &crate::audio::wake::VoiceIntent,
    tool_event: &'static str,
) -> crate::voice_action::VoiceActionResult {
    // The agentic loop runs only on the CLOUD brain — local-GGUF multi-step tool-call reliability is
    // unproven (Q4 + the Bielik 32K-overflow lesson), so the local + stub backends use the
    // deterministic floor: honest, fast, and a strict no-regression vs today.
    if config.brain_backend == crate::settings::BrainBackend::Cloud {
        let executor = crate::tools::GatedToolExecutor {
            db: &state.db,
            // The LIVE set behind its Mutex — re-read per tool call (C6), so a mid-loop relock is gated
            // out immediately. (The deterministic floor below still uses the per-turn `unlocked` snapshot.)
            unlocked: &state.unlocked_folders,
            config,
            meeting_id,
            app: Some(app),
            // PROPOSE-then-ACCEPT model (Rev 2): the in-meeting agent is READ-ONLY — it GENERATES
            // content (incl. note drafts enriched with the live context) but NEVER auto-writes. The
            // user commits a draft via the FE's "Add to notes" (→ `save_manual_notes`). The
            // `save_note`/`create_reminder` write tools stay DORMANT (not advertised while read-only)
            // for a future structured "agent acts" iteration. The dispatch still routes EVERY request
            // through this loop (no hardcoded write classifier), so the model decides answer-vs-draft.
            allow_writes: false,
            // The always-on `propose_note` tool records its draft HERE (no DB write). We read it after
            // the loop to mark the reply as a NOTE PROPOSAL vs a plain ANSWER. The model decides which.
            proposed_note: std::sync::Mutex::new(None),
        };
        let sink = ToolEventSink { app: app.clone(), event: tool_event };
        // Inject the LIVE transcript of the recording IN PROGRESS + the user's OWN typed notes for
        // the CURRENT meeting (segments aren't persisted until Stop, and typed notes carry the
        // user's emphasis). Both are read through `gated_live_context` — gated by
        // `meeting_is_visible` on the LIVE per-turn `unlocked` set (fail-closed) — and egress
        // through the SAME redaction firewall as every prompt (`RedactingProvider::complete`
        // scrubs `system` + `user`), only when cloud egress is consented (this branch is
        // Cloud-only). NO new egress class.
        let (live, typed_notes) =
            gated_live_context(&state.db, &state.live_transcript, meeting_id, unlocked);
        let system = assistant_system_prompt(&live, &typed_notes);
        match crate::agent::run_agentic_loop(
            &*state.reasoner,
            &system,
            loop_user,
            &executor,
            CLOUD_MAX_STEPS,
            Some(&sink as &dyn crate::agent::DeltaSink),
        ) {
            Ok(Some(outcome)) => {
                // Read the model's NOTE PROPOSAL (if it called `propose_note` this turn) off the
                // executor scratch and thread it onto the result — `Some` ⇒ a note draft, `None` ⇒ a
                // plain answer. No DB write happened; the FE commits the draft on Accept.
                let proposed = executor.proposed_note.lock().ok().and_then(|g| g.clone());
                return crate::voice_action::VoiceActionResult::from_agent(intent, outcome)
                    .with_proposed_note(proposed);
            }
            Ok(None) => tracing::debug!(
                target: "voice",
                "agentic loop did not converge; flooring to deterministic retrieval"
            ),
            Err(e) => tracing::debug!(
                target: "voice",
                error = %e,
                "agentic loop unavailable/failed; flooring to deterministic retrieval"
            ),
        }
    }
    // FLOOR — deterministic, gated, cited, needs_consent-aware, INFORMATIONAL ONLY. `floor_intent_for`
    // demotes a write intent to a Research so the floor NEVER performs a hardcoded write (a write
    // happens only when the AGENT chooses it; the floor is the read safety net).
    let floor_intent = floor_intent_for(intent, command);
    crate::voice_action::handle_voice_action(
        &floor_intent,
        &*state.reasoner,
        &state.db,
        unlocked,
        config,
        meeting_id,
        command,
        Some(app),
    )
}

/// The intent the INFORMATIONAL FLOOR runs with. Read intents (Research / Recall / SlackSearch /
/// Unknown) pass through UNCHANGED — zero read-answer regression. A WRITE intent
/// (`CreateReminder` / `NoteAside`) is DEMOTED to a `Research` over the literal command, so a
/// no-consent / local user whose phrase happened to parse as a write gets a deterministic
/// informational answer instead of a hardcoded write the AGENT never chose. This is the load-bearing
/// "the floor no longer owns write routing" rule, made pure + headless-testable.
fn floor_intent_for(
    intent: &crate::audio::wake::VoiceIntent,
    command: &str,
) -> crate::audio::wake::VoiceIntent {
    match intent {
        crate::audio::wake::VoiceIntent::CreateReminder { .. }
        | crate::audio::wake::VoiceIntent::NoteAside { .. } => {
            crate::audio::wake::VoiceIntent::Research { topic: command.trim().to_string() }
        }
        other => other.clone(),
    }
}

/// The live tool-trace sink: emits a per-tool-call event (`EVENT_ASSISTANT_TOOL` for the card,
/// `EVENT_CHAT_TOOL` for the chat panel — chosen by `event`) so the FE can render the "Searching
/// notes… ✓" chips. NO PII — tool NAME + a coarse result-size count only.
struct ToolEventSink {
    app: AppHandle,
    event: &'static str,
}
impl crate::agent::DeltaSink for ToolEventSink {
    fn tool_running(&self, tool: &str) {
        let _ = self.app.emit(
            self.event,
            crate::events::AssistantToolPayload {
                tool: tool.to_string(),
                state: "running".into(),
                ok: true,
                count: None,
            },
        );
    }
    fn tool_done(&self, tool: &str, ok: bool, result_chars: usize) {
        let _ = self.app.emit(
            self.event,
            crate::events::AssistantToolPayload {
                tool: tool.to_string(),
                state: "done".into(),
                ok,
                count: Some(result_chars as u32),
            },
        );
    }
}

/// Persist one dispatched voice interaction against the CURRENT recording meeting — best-effort +
/// PANIC-FREE so a persist failure can NEVER disrupt recording/dispatch. Skips when there is no
/// active recording (`meeting_id` empty). `command` is the user's own dictated words (the heard
/// command for the manual path, the intent's literal words for the wake path). The `summary`/
/// `citations`/`status`/`intent_kind` come straight off the dispatch result. Derived convenience
/// data: it is PURGED on seal (it mirrors sealed content), never surfaced for a sealed meeting.
fn persist_interaction(
    state: &AppState,
    meeting_id: &str,
    command: &str,
    result: &crate::voice_action::VoiceActionResult,
) {
    if meeting_id.is_empty() {
        return; // no active recording → nothing to attach the interaction to.
    }
    let created_at = chrono::Utc::now().to_rfc3339();
    match state.db.insert_assistant_interaction(
        meeting_id,
        command,
        &result.summary,
        &result.citations,
        &result.status,
        Some(result.intent_kind.as_str()),
        &created_at,
    ) {
        Ok(_) => {}
        // PII rule: log only that a persist failed + the coarse status — never the command/answer.
        Err(e) => tracing::debug!(
            target: "voice",
            error = %e,
            status = %result.status,
            "persisting assistant interaction failed; continuing"
        ),
    }
}

/// What the live loop should do with the MANUAL voice-command capture on this tick.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ManualCaptureDecision {
    /// The capture is DONE (the user clicked stop, OR the backstop cap was reached) AND the
    /// accumulated post-click utterance is a real (non-empty, meaningful) command → resolve +
    /// dispatch it (keyword fast-path, else brain interpret, else keyword fallback) and clear the
    /// capture. Carries the FULL accumulated utterance, not a per-tick fragment.
    Dispatch { command: String },
    /// Still listening (the user has not stopped and the backstop cap is not yet reached) → keep the
    /// capture armed, ACCUMULATING the growing post-click window, and wait for the next tick / the
    /// user's stop click. Does NOT dispatch on hearing speech (CLICK-TO-STOP).
    KeepListening,
    /// The capture ended (user stop OR backstop) with NOTHING meaningful heard (the user never spoke,
    /// or only silence/filler) → clear the capture and surface a graceful "nothing_heard". NEVER
    /// dispatches an empty command.
    NothingHeard,
}

/// Advance the MANUAL voice-command capture by one live tick. Snapshots ONLY the audio captured
/// SINCE the click (`Recorder::snapshot_from(start_sample)`) — the POST-CLICK utterance — and
/// transcribes it in isolation, so the command is exactly what the user said after clicking, not the
/// rolling 14s tail. Falls back to the rolling-tail `caption` text only when no offset was latched
/// (degenerate arm path). Returns `None` when no manual capture is armed (the common case).
///
/// When a capture IS armed it decrements the backstop budget in place and returns the
/// [`ManualCaptureDecision`], CLEARING the capture state on dispatch / nothing-heard (so exactly one
/// outcome fires per capture). CLICK-TO-STOP: it returns `Dispatch` only when the user has ended the
/// capture (`ended`) or the backstop cap was reached — never merely on hearing speech.
/// Best-effort + panic-free: a poisoned lock is treated as "no capture" (`None`).
///
/// The transcription of the GROWING post-click window + the dispatch/emit are the caller's job (off
/// the tick); the backstop arithmetic + the no-empty guard are the pure [`decide_manual_capture`].
fn step_manual_capture(
    app: &AppHandle,
    transcriber: &Transcriber,
    lang: Option<&str>,
    caption: &str,
) -> Option<ManualCaptureDecision> {
    let state = app.state::<AppState>();
    // Read the armed capture (and release the lock before any transcription work).
    let current = { (*state.voice_command_capture.lock().ok()?)? };

    // FORCE the command-clip language. A ~3s clip is far too short for Whisper to reliably
    // auto-detect — it routinely mis-detects a short Polish command as Russian/Slovak, which then
    // mangles the transcript. Resolve the forced language from `config.language` (the user's
    // setting), which is the SAME language the meeting transcription uses this session. When it is
    // unset/auto we keep `None` (whisper auto-detects) — the user can fix that in Settings ▸ Language.
    let clip_lang = resolve_clip_lang(lang, &state);
    let clip_lang_ref = clip_lang.as_deref();

    // Transcribe ONLY the post-click window when an offset was latched; otherwise fall back to the
    // rolling-tail caption the live loop already produced (degenerate arm path with no recorder).
    let command = match current.start_sample {
        Some(offset) => transcribe_since(app, transcriber, clip_lang_ref, offset).unwrap_or_default(),
        None => caption.trim().to_string(),
    };

    let (decision, next) = decide_manual_capture(current, &command);
    // Re-acquire to store the next state (clear on terminal outcomes, decrement on keep-listening).
    *state.voice_command_capture.lock().ok()? = next;
    Some(decision)
}

/// Transcribe the growing window of audio captured SINCE `offset` (the click) into a single trimmed
/// command string. Reads `Recorder::snapshot_from` (read-only, never drains), resamples to 16k, and
/// runs the Fast profile — exactly like the caption tick, but on the ISOLATED post-click window.
/// Returns `None` when the recorder is gone or there is < ~0.4s of audio yet (too little to bother).
fn transcribe_since(
    app: &AppHandle,
    transcriber: &Transcriber,
    lang: Option<&str>,
    offset: usize,
) -> Option<String> {
    let (window, rate) = {
        let state = app.state::<AppState>();
        let guard = state.recorder.lock().ok()?;
        let r = guard.as_ref()?;
        (r.snapshot_from(offset), r.source_sample_rate())
    };
    // Need a little audio before transcribing — < ~0.4s is just the click latency / silence.
    if rate == 0 || window.len() < (rate as usize) * 2 / 5 {
        return Some(String::new());
    }
    let samples_16k = match crate::audio::resample_to_16k(&window, rate) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(target: "live", error = %e, "manual-capture resample failed");
            return Some(String::new());
        }
    };
    match transcriber.transcribe(&samples_16k, lang) {
        Ok(t) => Some(
            t.segments
                .iter()
                .map(|s| s.text.trim())
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
                .to_string(),
        ),
        Err(e) => {
            tracing::debug!(target: "live", error = %e, "manual-capture transcribe failed");
            Some(String::new())
        }
    }
}

/// Resolve the forced language for a manual-capture command clip from the live-loop `lang` (which is
/// `config.language` at spawn time) and the CURRENT config (re-read from `state` in case the user
/// changed it mid-recording). Side-effecting only in that it reads the config lock; the decision is
/// the pure [`resolve_clip_lang_core`].
fn resolve_clip_lang(loop_lang: Option<&str>, state: &AppState) -> Option<String> {
    let cfg_lang = state
        .config
        .lock()
        .ok()
        .and_then(|c| c.language.clone());
    resolve_clip_lang_core(loop_lang, cfg_lang.as_deref())
}

/// PURE, headless-testable core: pick the forced clip language. Prefer the freshest configured
/// language (the user may have set/changed it after the recording started), else the loop's spawn-
/// time language. A blank/whitespace value (`Some("")`) counts as UNSET → `None` (auto-detect). The
/// returned `Some(lang)` is what gets passed to Whisper for the short clip so it does NOT free-
/// auto-detect a 3s utterance into the wrong Slavic language.
fn resolve_clip_lang_core(loop_lang: Option<&str>, config_lang: Option<&str>) -> Option<String> {
    let norm = |s: Option<&str>| -> Option<String> {
        s.map(str::trim)
            .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto"))
            .map(str::to_string)
    };
    norm(config_lang).or_else(|| norm(loop_lang))
}

/// Standalone filler/backchannel utterances Whisper emits for a near-silent / noise-only clip, in
/// EN + PL. Normalized (lowercased, diacritics stripped, inner non-alphanumerics removed) before the
/// lookup, so "Uh," / "Eee…" / "Yyy" all collapse onto a member. A command consisting ONLY of these
/// (after splitting on whitespace) is NOT a real command.
const FILLERS: &[&str] = &[
    // English
    "uh", "uhh", "uhm", "um", "umm", "er", "err", "erm", "hmm", "hm", "hmmm", "ah", "aha", "ahh",
    "oh", "ohh", "eh", "ehh", "mm", "mmm", "mhm", "huh", "yeah", "ok", "okay",
    // Polish fillers / backchannels
    "eee", "ee", "yyy", "yy", "yhy", "ehe",
];

/// Whether a normalized token is a pure filler/backchannel (after stripping inner punctuation).
fn is_filler_token(tok: &str) -> bool {
    let t: String = tok
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ź' | 'ż' => 'z',
            other => other,
        })
        .filter(|c| c.is_alphanumeric())
        .collect();
    t.is_empty() || FILLERS.contains(&t.as_str())
}

/// Whether the heard `command` is a MEANINGFUL command worth dispatching, vs garbage/filler. PURE +
/// headless-testable. A command is garbage when, after trimming, EITHER:
/// - it has fewer than 2 tokens that are not pure filler, OR
/// - its non-filler alphanumeric content is shorter than ~8 chars (a single short word like "Uh").
///
/// So "Uh," / "eee" / "." / "a" / "ok" → NOT meaningful (keep listening); but real commands like
/// "zrób research o pogodzie" / "co wiemy o atlasie" → meaningful (dispatch). Conservative on the
/// short side so we never DROP a real one-word command that carries enough signal (≥8 non-filler
/// chars, e.g. "pogoda?" is 6 → borderline; we require it ride with a verb in practice — the manual
/// path's whole utterance is the command, which is virtually always ≥2 words).
fn is_meaningful_command(command: &str) -> bool {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let non_filler: Vec<&str> = tokens.iter().copied().filter(|t| !is_filler_token(t)).collect();
    // Count of meaningful (non-filler) tokens.
    let meaningful_tokens = non_filler.len();
    // Total non-filler alphanumeric chars (diacritic-insensitive count via char count of the kept
    // alphanumerics — we don't need the normalized form, just the length signal).
    let non_filler_chars: usize = non_filler
        .iter()
        .flat_map(|t| t.chars())
        .filter(|c| c.is_alphanumeric())
        .count();
    meaningful_tokens >= 2 || non_filler_chars >= 8
}

/// PURE, headless-testable core of the manual-capture step (CLICK-TO-STOP). Given the armed
/// [`CaptureState`] and the FULL `command` accumulated SINCE the click (the growing post-click
/// window's transcript), decide the outcome and the NEXT capture state (`None` = cleared on a
/// terminal outcome, `Some(decremented)` while still listening).
///
/// The capture ENDS only when the user clicked stop (`capture.ended == true`) OR the backstop cap is
/// reached (`budget` hits 0) — NOT on merely hearing speech. Until then we KEEP LISTENING so the user
/// can finish their whole question without being cut off mid-sentence. Rules (no wake word — the WHOLE
/// post-click utterance is the command; NEVER dispatch empty):
/// 1. The capture is DONE (`ended` set by the user's stop click, OR `budget` exhausted = backstop):
///    - accumulated command is meaningful → `Dispatch{command}` with the FULL utterance, clear.
///    - else (silent / pure filler) → `NothingHeard`, clear (graceful, never an empty dispatch).
/// 2. Still going (not ended, backstop not reached) → spend one tick of the backstop budget and
///    `KeepListening` (keep accumulating). On a non-`ended` capture this is the common path even when
///    real speech is heard — we wait for the user's stop click, only the backstop forces an end.
///
/// No I/O, no FFI, no egress — just the meaningfulness check + arithmetic.
fn decide_manual_capture(
    capture: crate::state::CaptureState,
    command: &str,
) -> (ManualCaptureDecision, Option<crate::state::CaptureState>) {
    let command = command.trim();
    // Spend one tick of the backstop budget; 0 means the safety cap is now reached this tick.
    let remaining = capture.budget.saturating_sub(1);
    let backstop_reached = remaining == 0;

    // The capture is DONE when the user clicked stop OR the backstop cap is reached.
    if capture.ended || backstop_reached {
        // GARBAGE FILTER: a too-short / pure-filler accumulation ("Uh,", "eee", "hmm", a lone char or
        // punctuation) is NOT a real command — Whisper emits these for a silent/noise clip. Treat it
        // as nothing heard rather than dispatching a Research on "Uh,". The user gets a clean
        // "nothing_heard", never a garbage card.
        if !command.is_empty() && is_meaningful_command(command) {
            (
                ManualCaptureDecision::Dispatch { command: command.to_string() },
                None,
            )
        } else {
            (ManualCaptureDecision::NothingHeard, None)
        }
    } else {
        // Still listening — accumulate the growing window, decrement the backstop, wait for the
        // user's stop click. We do NOT dispatch on hearing speech (CLICK-TO-STOP).
        (
            ManualCaptureDecision::KeepListening,
            Some(crate::state::CaptureState { budget: remaining, ..capture }),
        )
    }
}

/// Resolve a NON-EMPTY heard `command` to a [`crate::audio::wake::VoiceIntent`] using the
/// keyword fast-path, then the BRAIN, then a keyword fallback. PURE w.r.t. the reasoner (the only
/// effect is the reasoner call, which the caller runs off-thread):
/// 1. `parse_voice_intent` — deterministic PL+EN keyword parser. A RECOGNIZED intent wins immediately.
/// 2. Else ask the brain ([`crate::voice_action::interpret_with_brain`]) to map any phrasing/order to
///    a known action — so "poszukaj mi info o X", "zrób research o wakacjach", etc. all work.
/// 3. Else (brain unavailable / no-consent / no mapping) fall back to **Research over the literal
///    command** — a sensible default for a non-empty command, never an empty/Unknown dispatch.
fn resolve_command_intent(
    reasoner: &dyn crate::reason::LocalReasoner,
    command: &str,
) -> crate::audio::wake::VoiceIntent {
    let keyword = crate::audio::wake::parse_voice_intent(command);
    if !matches!(keyword, crate::audio::wake::VoiceIntent::Unknown { .. }) {
        return keyword; // keyword fast-path understood it.
    }
    // Keyword said Unknown but the command is non-empty → ask the brain (best-effort).
    if let Some(intent) = crate::voice_action::interpret_with_brain(reasoner, command) {
        return intent;
    }
    // Brain unavailable / no mapping → Research over the literal command is a fine default.
    crate::audio::wake::VoiceIntent::Research { topic: command.trim().to_string() }
}

/// Pure, headless-testable core of the wake wiring: detect a wake utterance at the head of a
/// transcript `tail` and, on a hit, build the typed [`crate::events::WakeDetectedPayload`] (matched
/// wake token + command tail + deterministically-parsed intent). Returns `None` when nothing fires.
///
/// No I/O, no FFI, no egress — just `detect_wake` + `parse_voice_intent`. The live mic loop above
/// calls this and emits the payload; the real-mic precision is the Mac step (`cargo test` is not
/// proof for acoustic behaviour — see `crate::audio::wake`).
fn wake_event_for(tail: &str) -> Option<crate::events::WakeDetectedPayload> {
    let hit = crate::audio::wake::detect_wake(tail)?;
    let intent = crate::audio::wake::parse_voice_intent(&hit.command);
    Some(crate::events::WakeDetectedPayload {
        matched_phrase: hit.matched_phrase,
        command: hit.command,
        intent,
    })
}

// ── Live-meeting awareness: accumulate the rolling captions into a running transcript and inject it
// into the in-meeting assistant's context, so it can answer "what is this meeting about?" about the
// recording IN PROGRESS (whose segments aren't persisted until Stop). ──────────────────────────────

/// Hard cap on the accumulated live-transcript buffer (chars) — bounds memory over a multi-hour
/// meeting; the oldest text is trimmed from the FRONT (keep the recent tail, which is what matters).
const MAX_LIVE_TRANSCRIPT_CHARS: usize = 16_000;
/// How much of the live transcript (most-recent tail) is injected into the assistant's prompt. The
/// tail is what's relevant + bounds the per-turn token cost across the agentic loop.
const LIVE_TRANSCRIPT_INJECT_CHARS: usize = 6_000;

/// Append a rolling live CAPTION to the accumulated live transcript, removing the OVERLAP between the
/// end of `accumulated` and the start of `caption` (live captions are overlapping tail windows, so a
/// naive append would trIplicate every phrase). Word-level overlap: find the largest `k` where the
/// last `k` words of `accumulated` equal the first `k` words of `caption`, and append only the words
/// after `k`. Approximate (Whisper varies run-to-run) but good enough for the assistant's gist
/// context. Pure + cheap (no extra transcription — reuses the caption the loop already produced).
fn merge_live_caption(accumulated: &str, caption: &str) -> String {
    let cap = caption.trim();
    if cap.is_empty() {
        return accumulated.to_string();
    }
    let acc = accumulated.trim_end();
    if acc.is_empty() {
        return cap.to_string();
    }
    let acc_words: Vec<&str> = acc.split_whitespace().collect();
    let cap_words: Vec<&str> = cap.split_whitespace().collect();
    let max_k = acc_words.len().min(cap_words.len());
    let mut overlap = 0;
    for k in (1..=max_k).rev() {
        let acc_tail = &acc_words[acc_words.len() - k..];
        let cap_head = &cap_words[..k];
        if acc_tail
            .iter()
            .zip(cap_head)
            .all(|(a, b)| caption_words_match(a, b))
        {
            overlap = k;
            break;
        }
    }
    let new_part = cap_words[overlap..].join(" ");
    if new_part.is_empty() {
        return acc.to_string();
    }
    format!("{acc} {new_part}")
}

/// Whether two caption words are the SAME word for overlap detection, tolerating the transcription
/// variance Whisper shows between overlapping tails: Unicode case ("Że" vs "że" — ASCII-only
/// folding misses Polish diacritics) and leading/trailing punctuation ("piątek." vs "piątek").
/// COMPARISON ONLY — the appended text stays byte-original. Two punctuation-only tokens (both
/// normalize to empty) count as a match: both are transcription noise, so skipping one is safe.
fn caption_words_match(a: &str, b: &str) -> bool {
    normalize_caption_word(a) == normalize_caption_word(b)
}

/// Normalize one word for the overlap compare: trim leading/trailing non-alphanumerics (Unicode-
/// aware, so diacritics survive) and Unicode-lowercase the rest.
fn normalize_caption_word(w: &str) -> String {
    w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

/// Merge a caption into the shared live-transcript buffer (read-modify-write under its Mutex) and
/// trim to [`MAX_LIVE_TRANSCRIPT_CHARS`] from the FRONT. Best-effort: a poisoned lock is ignored so a
/// caption tick can never disrupt the recording.
fn accumulate_live_caption(state: &AppState, caption: &str) {
    if caption.trim().is_empty() {
        return;
    }
    if let Ok(mut buf) = state.live_transcript.lock() {
        let merged = merge_live_caption(&buf, caption);
        *buf = if merged.chars().count() > MAX_LIVE_TRANSCRIPT_CHARS {
            tail_chars(&merged, MAX_LIVE_TRANSCRIPT_CHARS)
                .trim_start_matches('…')
                .to_string()
        } else {
            merged
        };
    }
}

/// The in-flight context injected into the assistant's system prompt: the LIVE transcript tail of
/// the recording in progress + the user's own typed notes for the CURRENT meeting. BOTH are gated
/// by `meeting_is_visible` on the LIVE per-turn `unlocked` set, fail-closed: no current meeting
/// (`meeting_id` empty), a sealed-and-not-session-unlocked meeting, or a gate error injects
/// NOTHING. The live buffer only ever holds the CURRENT recording (cleared at recording start and
/// at Stop), so gating on the current meeting's visibility covers a mid-recording folder relock
/// (screen-share auto-relock included) WITHOUT wiping the user's in-flight buffer — a session
/// re-unlock makes the context inject again. Best-effort: a read error degrades to no injection,
/// never a failure.
fn gated_live_context(
    db: &crate::storage::Db,
    live_transcript: &std::sync::Mutex<String>,
    meeting_id: &str,
    unlocked: &std::collections::HashSet<String>,
) -> (String, String) {
    let visible = !meeting_id.is_empty()
        && db.meeting_is_visible(meeting_id, unlocked).unwrap_or(false);
    if !visible {
        return (String::new(), String::new());
    }
    let live = live_transcript.lock().map(|t| t.clone()).unwrap_or_default();
    let typed = db.get_manual_notes(meeting_id).unwrap_or_default();
    (live, typed)
}

/// Clear the accumulated live-transcript buffer. Called when a recording STOPS
/// (`commands::stop_recording`) so a stale tail can never be injected into assistant prompts after
/// Stop — nor keep egressing once the just-recorded folder is sealed — and by the lock-surface
/// hygiene below. PII rule (§8): logs only the cleared char COUNT, never the content. Best-effort:
/// a poisoned lock is ignored (the buffer is re-cleared at the next recording start anyway).
pub(crate) fn clear_live_transcript(live: &std::sync::Mutex<String>) {
    if let Ok(mut buf) = live.lock() {
        if !buf.is_empty() {
            tracing::debug!(
                target: "live",
                chars = buf.chars().count(),
                "cleared live-transcript buffer"
            );
            buf.clear();
        }
    }
}

/// Belt-and-braces RAM hygiene for the lock surface (`lock_folder` / `relock_all`): clear the
/// buffer ONLY when no recording is active (post clear-on-Stop it is normally already empty —
/// this is cheap idempotent hygiene). NEVER clears mid-recording: the user's in-flight buffer
/// stays, and egress correctness there is owned by the `gated_live_context` visibility gate.
pub(crate) fn clear_live_transcript_if_idle(live: &std::sync::Mutex<String>, is_recording: bool) {
    if !is_recording {
        clear_live_transcript(live);
    }
}

/// The most-recent `n` chars of `s` (on a char boundary), prefixed with `…` when truncated.
fn tail_chars(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        s.to_string()
    } else {
        let tail: String = chars[chars.len() - n..].iter().collect();
        format!("…{tail}")
    }
}

/// Build the in-meeting assistant's system prompt. When the LIVE transcript of the current recording
/// is present, inject its recent tail (≤ [`LIVE_TRANSCRIPT_INJECT_CHARS`]) so the brain can answer
/// questions about the meeting IN PROGRESS; otherwise the base prompt is unchanged (no regression when
/// not recording / before any caption). When the user has typed their OWN notes for this meeting
/// (`typed_notes`), inject them as their own bounded section (their emphasis — weight these),
/// truncated to the recent tail like the transcript. EMPTY `typed_notes` ⇒ the prompt is BYTE-IDENTICAL
/// to the pre-feature output (the section is simply absent). Both the injected transcript AND the typed
/// notes egress through the SAME redaction firewall as every other prompt
/// (`RedactingProvider::complete` scrubs `system` + `user`) — they are NOT a new egress class.
fn assistant_system_prompt(live_transcript: &str, typed_notes: &str) -> String {
    let base = "You are an in-meeting assistant. Answer the user's request CONCISELY (2-4 \
                sentences). Do not invent facts; if you cannot find the answer, say so plainly. \
                Decide what the user wants: for a plain QUESTION or conversation, just ANSWER it. \
                ONLY when the user asks you to MAKE / SAVE / DRAFT / WRITE a note (e.g. \"make me a \
                note about the decisions\", \"save that we ship Friday\"), call the propose_note tool \
                with the note content enriched from the meeting context — that drafts a note for the \
                user to review and accept; do NOT call propose_note for ordinary questions.";
    let t = live_transcript.trim();
    let mut prompt = if t.is_empty() {
        format!(
            "{base} Ground answers in tool results from the user's own gated vault (and web / \
             calendar when you use them)."
        )
    } else {
        let tail = tail_chars(t, LIVE_TRANSCRIPT_INJECT_CHARS);
        // HONESTY: the buffer is mic-stream-only and carries no speaker labels — the prompt must
        // not invite "who said what" answers (any attribution from it would be hallucinated).
        format!(
            "{base} A meeting is being recorded RIGHT NOW — use the LIVE TRANSCRIPT below to answer \
             questions about THIS current meeting (its topic and what has been said so far), and your \
             gated tools for anything in the user's saved notes/vault.\n\nLIVE TRANSCRIPT — an \
             UNATTRIBUTED, possibly-partial rolling capture of the recent portion of the meeting from \
             the user's microphone side; it may be garbled and it does NOT indicate who said what, so \
             never attribute a statement to a specific speaker:\n{tail}"
        )
    };
    // The user's OWN typed notes for this meeting — their explicit emphasis, so the brain should
    // weight them. Appended as a distinct bounded section; absent when empty (prompt unchanged).
    let notes = typed_notes.trim();
    if !notes.is_empty() {
        let notes_tail = tail_chars(notes, LIVE_TRANSCRIPT_INJECT_CHARS);
        prompt.push_str(&format!(
            "\n\nUSER'S OWN TYPED NOTES for this meeting (their emphasis — weight these):\n{notes_tail}"
        ));
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::wake::VoiceIntent;

    #[test]
    fn wake_event_for_builds_payload_with_parsed_intent_on_hit() {
        let p = wake_event_for("klodku zrób research o konkurencji")
            .expect("vocative wake must fire");
        assert_eq!(p.matched_phrase, "klodku");
        assert_eq!(p.command, "zrób research o konkurencji");
        assert_eq!(p.intent, VoiceIntent::Research { topic: "konkurencji".into() });
    }

    // ── #23 DEDUP: one spoken wake = one dispatch; a fresh wake later DOES fire ───────────────────

    #[test]
    fn wake_dedup_collapses_overlapping_repeats_of_the_same_wake() {
        // The #23 echo: the same "Klaudku zrób research" stays visible across several overlapping
        // ~14s tails. The FIRST tick fires; the next ticks (within the window) are SKIPPED.
        let mut d = WakeDedup::default();
        assert!(d.should_fire("zrób research o konkurencji"), "first detection must fire");
        for _ in 0..WAKE_DEDUP_TICKS - 1 {
            d.tick();
            assert!(
                !d.should_fire("zrób research o konkurencji"),
                "an overlapping re-detection of the SAME command within the window must be skipped"
            );
        }
    }

    #[test]
    fn wake_dedup_normalizes_punctuation_and_case_across_tails() {
        // The same utterance re-transcribed with different trailing punctuation / casing across
        // overlapping tails still compares equal and is de-duplicated.
        let mut d = WakeDedup::default();
        assert!(d.should_fire("zrób research o konkurencji"));
        d.tick();
        assert!(
            !d.should_fire("Zrób research o konkurencji."),
            "case/punctuation-only differences must still dedup as the same command"
        );
    }

    #[test]
    fn wake_dedup_allows_a_different_command_immediately() {
        // RECALL: a DIFFERENT wake command in the same recording must fire right away, even while the
        // previous one's suppression window is still live (the user's "to ma zawsze łapać").
        let mut d = WakeDedup::default();
        assert!(d.should_fire("zrób research o konkurencji"), "first command fires");
        d.tick();
        assert!(
            d.should_fire("co wiemy o atlasie"),
            "a fresh, different command must fire even within the prior window"
        );
    }

    #[test]
    fn wake_dedup_allows_the_same_command_again_after_the_window_lapses() {
        // The user genuinely asks the SAME thing again, later. Once the suppression window has fully
        // aged out, the same command fires again — dedup suppresses the echo, not a real re-ask.
        let mut d = WakeDedup::default();
        assert!(d.should_fire("zrób research o konkurencji"), "first ask fires");
        for _ in 0..WAKE_DEDUP_TICKS {
            d.tick();
        }
        assert!(
            d.should_fire("zrób research o konkurencji"),
            "after the window lapses the same command must be allowed to fire again"
        );
    }

    #[test]
    fn wake_dedup_window_counts_from_the_first_fire_not_the_last_echo() {
        // A suppressed echo must NOT re-arm the window, else a long overlap would suppress forever.
        let mut d = WakeDedup::default();
        assert!(d.should_fire("zrób research"), "first fire arms the window");
        // Echo once mid-window: suppressed, and must not extend the cooldown.
        d.tick();
        assert!(!d.should_fire("zrób research"), "echo suppressed");
        // Age out the REMAINDER of the window counted from the FIRST fire.
        for _ in 0..WAKE_DEDUP_TICKS - 1 {
            d.tick();
        }
        assert!(
            d.should_fire("zrób research"),
            "the window must expire on schedule from the first fire, not be extended by echoes"
        );
    }

    #[test]
    fn wake_event_for_is_silent_on_ordinary_speech_and_empty() {
        assert!(wake_event_for("let's talk about the budget for friday").is_none());
        assert!(wake_event_for("cloud computing is the future").is_none());
        assert!(wake_event_for("").is_none());
    }

    #[test]
    fn realtime_reactions_off_does_not_dispatch_on_default_config() {
        // Default config has `realtime_reactions = false` ⇒ a wake hit is SURFACED, never dispatched.
        let cfg = crate::settings::AppConfig::default();
        assert!(!cfg.realtime_reactions, "the in-meeting assistant must be opt-in");
        assert!(!should_dispatch(&cfg), "default (OFF) must not dispatch a voice action");

        // The wake still FIRES regardless of the toggle — only the dispatch is gated by it, so OFF
        // is exactly today's behaviour (detect + surface).
        assert!(wake_event_for("klodku zrób research o konkurencji").is_some());
    }

    #[test]
    fn realtime_reactions_on_enables_dispatch() {
        let cfg = crate::settings::AppConfig {
            realtime_reactions: true,
            ..Default::default()
        };
        assert!(should_dispatch(&cfg), "ON must dispatch a voice action");
    }

    // ── MANUAL voice-command capture (the button trigger) ───────────────────
    use crate::reason::LocalReasoner;
    use crate::state::CaptureState;
    use serde_json::Value;

    fn armed(budget: u32) -> CaptureState {
        CaptureState { budget, start_sample: Some(0), ended: false }
    }

    /// An armed capture the user has CLICKED STOP on (`end_voice_command` flipped `ended`).
    fn ended(budget: u32) -> CaptureState {
        CaptureState { budget, start_sample: Some(0), ended: true }
    }

    #[test]
    fn manual_capture_still_listening_does_not_dispatch_on_hearing_speech() {
        // CLICK-TO-STOP: hearing a real, meaningful utterance while NOT ended (and under the backstop)
        // must KEEP LISTENING — the user controls when they're done, not a per-tick auto-dispatch.
        let cap = armed(20);
        let (decision, next) = decide_manual_capture(cap, "Więc tak, zrób web");
        assert_eq!(
            decision,
            ManualCaptureDecision::KeepListening,
            "a non-ended capture must keep accumulating, not cut the user off mid-question"
        );
        assert_eq!(
            next,
            Some(CaptureState { budget: 19, start_sample: Some(0), ended: false }),
            "a listening tick decrements the backstop and keeps the latched offset + ended flag"
        );
    }

    #[test]
    fn manual_capture_end_click_dispatches_full_accumulated_utterance_and_clears() {
        // The user clicked STOP (`ended`) with a full accumulated utterance → dispatch the WHOLE
        // trimmed command, clear the capture. This is the primary end of a CLICK-TO-STOP capture.
        let cap = ended(15);
        let (decision, next) =
            decide_manual_capture(cap, "  Więc tak, zrób web research o konkurencji  ");
        assert_eq!(
            decision,
            ManualCaptureDecision::Dispatch {
                command: "Więc tak, zrób web research o konkurencji".into()
            },
            "the user's stop click must dispatch the FULL accumulated (trimmed) utterance"
        );
        assert!(next.is_none(), "capture must be cleared after a dispatch");
    }

    #[test]
    fn manual_capture_backstop_cap_dispatches_a_real_utterance_as_a_backstop() {
        // No stop click, but the backstop cap is reached (budget hits 0 this tick) with a real
        // utterance accumulated → auto-stop + dispatch so the capture can't listen forever.
        let cap = armed(1); // this tick takes budget 1 → 0 = backstop reached
        let (decision, next) = decide_manual_capture(cap, "zrób research o konkurencji");
        assert_eq!(
            decision,
            ManualCaptureDecision::Dispatch { command: "zrób research o konkurencji".into() },
            "reaching the backstop cap with a real utterance must dispatch it (backstop)"
        );
        assert!(next.is_none(), "capture cleared at the backstop");
    }

    #[test]
    fn manual_capture_end_click_on_silent_capture_is_nothing_heard_not_empty_dispatch() {
        // The NO-EMPTY-DISPATCH guarantee: the user clicked stop but nothing was ever heard → graceful
        // NothingHeard, NEVER an empty Unknown dispatch.
        let (d, n) = decide_manual_capture(ended(15), "");
        assert_eq!(
            d,
            ManualCaptureDecision::NothingHeard,
            "an ended-but-silent capture must terminate as NothingHeard, never an empty dispatch"
        );
        assert!(n.is_none(), "capture must be cleared once it gives up");
    }

    #[test]
    fn manual_capture_backstop_with_nothing_heard_is_nothing_heard() {
        // No stop click, backstop reached, nothing ever heard → NothingHeard (graceful), never empty.
        let (d, n) = decide_manual_capture(armed(1), "   ");
        assert_eq!(
            d,
            ManualCaptureDecision::NothingHeard,
            "the backstop with a fully-silent capture must give up gracefully"
        );
        assert!(n.is_none(), "capture cleared at the backstop");
    }

    #[test]
    fn manual_capture_silent_tick_with_budget_left_keeps_listening_and_decrements() {
        // An EMPTY post-click window (no speech yet) but backstop remains → keep listening, budget −1.
        let cap = armed(3);
        let (decision, next) = decide_manual_capture(cap, "   ");
        assert_eq!(decision, ManualCaptureDecision::KeepListening);
        assert_eq!(
            next,
            Some(CaptureState { budget: 2, start_sample: Some(0), ended: false }),
            "a silent tick must decrement the backstop and keep the SAME latched offset + flag"
        );
    }

    #[test]
    fn nothing_heard_result_is_graceful_with_empty_command() {
        let r = crate::voice_action::VoiceActionResult::nothing_heard();
        assert_eq!(r.status, "nothing_heard");
        assert!(r.command.is_empty(), "nothing was heard ⇒ no command surfaced");
        assert!(r.summary.contains("Nie usłyszałem"), "friendly PL nudge, not 'didn't catch an action'");
    }

    #[test]
    fn manual_capture_default_backstop_is_generous_not_a_felt_cutoff() {
        // CLICK-TO-STOP: the default backstop must be GENEROUS (~20 ticks ≈ 60s at the 3s TICK) so a
        // normal-length question never feels cut off — the user's stop click is the primary end, this
        // is only the can't-listen-forever safety cap.
        assert!(
            (15..=30).contains(&CaptureState::DEFAULT_BUDGET),
            "the backstop cap must be generous (~60s) so it never feels like a cutoff mid-question"
        );
    }

    // ── resolve_command_intent: keyword fast-path → brain → fallback ─────────

    /// Maps "research" for any non-empty command via `structured`, so we can prove the brain path.
    struct BrainResearch;
    impl LocalReasoner for BrainResearch {
        fn id(&self) -> &str {
            "brain-research"
        }
        fn reason(&self, _s: &str, _u: &str) -> crate::error::Result<String> {
            Ok(String::new())
        }
        fn structured(&self, _s: &str, _u: &str, _schema: &Value) -> crate::error::Result<Value> {
            Ok(serde_json::json!({ "action": "research", "argument": "wakacjach" }))
        }
    }

    /// A reasoner that always errors — proves the keyword FALLBACK kicks in when the brain is down.
    struct DeadBrain;
    impl LocalReasoner for DeadBrain {
        fn id(&self) -> &str {
            "dead"
        }
        fn reason(&self, _s: &str, _u: &str) -> crate::error::Result<String> {
            Err(crate::error::AppError::Unavailable("no consent".into()))
        }
        fn structured(&self, _s: &str, _u: &str, _schema: &Value) -> crate::error::Result<Value> {
            Err(crate::error::AppError::Unavailable("no consent".into()))
        }
    }

    #[test]
    fn resolve_keyword_fast_path_wins_without_touching_the_brain() {
        // A keyword-recognized command never calls the brain (DeadBrain would error if it did).
        let intent = resolve_command_intent(&DeadBrain, "zrób research o konkurencji");
        assert_eq!(intent, VoiceIntent::Research { topic: "konkurencji".into() });
    }

    #[test]
    fn resolve_unknown_command_uses_the_brain_mapping() {
        // Keyword Unknown ("poszukaj mi info o…") → brain maps it to Research over its argument.
        let intent = resolve_command_intent(&BrainResearch, "poszukaj mi info o wakacjach");
        assert_eq!(intent, VoiceIntent::Research { topic: "wakacjach".into() });
    }

    #[test]
    fn resolve_unknown_command_falls_back_to_research_when_brain_unavailable() {
        // Keyword Unknown + brain unavailable (no consent) → Research over the LITERAL command, a
        // sensible non-empty default; never an empty/Unknown dispatch.
        let intent = resolve_command_intent(&DeadBrain, "find me the latest on widgets");
        assert_eq!(
            intent,
            VoiceIntent::Research { topic: "find me the latest on widgets".into() },
            "a non-empty command with no brain must default to Research over the literal text"
        );
    }

    // ── DISPATCH: the floor no longer owns WRITE routing (conversation-first design, 2026-06-30) ────
    // The agentic loop (with writes) is the SINGLE executive path — the agent DECIDES answer-vs-write,
    // NOT a hardcoded classifier branch. The intent is resolved ONLY for the informational floor, and
    // the floor is INFORMATIONAL ONLY: a parsed write intent is demoted to a Research so a no-consent /
    // local user never gets a hardcoded write the agent never chose.

    #[test]
    fn floor_demotes_write_intents_to_informational_research() {
        // A "save"-style phrase that the classifier parses as a WRITE (NoteAside) must NOT drive a
        // hardcoded write on the floor — it is demoted to a Research over the literal command. (The
        // actual answer-vs-act DECISION belongs to the agentic loop, not this floor.)
        let note = VoiceIntent::NoteAside { text: "send the deck to Anna".into() };
        assert_eq!(
            floor_intent_for(&note, "note that I send the deck to Anna"),
            VoiceIntent::Research { topic: "note that I send the deck to Anna".into() },
            "the floor must NOT perform a hardcoded NoteAside write — it answers informationally"
        );

        let reminder = VoiceIntent::CreateReminder { text: "email Bob".into(), due: None };
        assert_eq!(
            floor_intent_for(&reminder, "remind me to email Bob"),
            VoiceIntent::Research { topic: "remind me to email Bob".into() },
            "the floor must NOT perform a hardcoded CreateReminder write — it answers informationally"
        );
    }

    #[test]
    fn floor_passes_read_intents_through_unchanged_no_regression() {
        // Read intents are UNTOUCHED by the floor demotion ⇒ the no-consent/local informational answer
        // is exactly today's behavior (zero regression). Research/Recall/SlackSearch/Unknown all pass.
        for intent in [
            VoiceIntent::Research { topic: "atlas pricing".into() },
            VoiceIntent::Recall { entity: "Anna".into() },
            VoiceIntent::SlackSearch { query: "raport".into() },
            VoiceIntent::Unknown { raw: "gibberish".into() },
        ] {
            assert_eq!(
                floor_intent_for(&intent, "the literal command"),
                intent,
                "a read intent must reach the floor unchanged: {intent:?}"
            );
        }
    }

    // ── FIX 3: GARBAGE / FILLER filter — "Uh," / "eee" / single char must NOT dispatch ───────────

    #[test]
    fn is_meaningful_command_rejects_filler_and_too_short() {
        // Pure fillers / single short tokens / punctuation → NOT meaningful.
        for junk in ["Uh,", "uh", "eee", "Hmm", "yyy", "aha", "ok", ".", ",", "a", "—", "  uh  ", "eh"] {
            assert!(
                !is_meaningful_command(junk),
                "garbage/filler {junk:?} must NOT be a meaningful command"
            );
        }
        // Real commands → meaningful.
        for good in [
            "zrób research o pogodzie",
            "co wiemy o atlasie",
            "jaka była pogoda",
            "do research on pricing",
            "przypomnij mi o spotkaniu",
        ] {
            assert!(is_meaningful_command(good), "real command {good:?} must be meaningful");
        }
    }

    #[test]
    fn decide_manual_capture_end_click_on_garbage_only_is_nothing_heard() {
        // The user clicked stop but only garbage/filler ("eee") was accumulated → NothingHeard
        // (graceful), never a garbage Research dispatch.
        let (d, n) = decide_manual_capture(ended(10), "eee");
        assert_eq!(
            d,
            ManualCaptureDecision::NothingHeard,
            "an ended garbage-only capture ends as NothingHeard, never a garbage dispatch"
        );
        assert!(n.is_none(), "capture cleared once it gives up");
    }

    #[test]
    fn decide_manual_capture_end_click_dispatches_a_real_command() {
        // The user clicked stop with a genuine command → dispatch the (trimmed) command verbatim.
        let (d, n) = decide_manual_capture(ended(15), "  zrób research o pogodzie  ");
        assert_eq!(
            d,
            ManualCaptureDecision::Dispatch { command: "zrób research o pogodzie".into() },
            "an ended real command must dispatch"
        );
        assert!(n.is_none(), "capture cleared on dispatch");
    }

    // ── FIX 2: forced clip language threading (config.language reaches the clip) ──────────────────

    #[test]
    fn resolve_clip_lang_core_forces_configured_language() {
        // A configured language is forced for the short command clip (the param that reaches
        // `transcribe_since` → Whisper), so a 3s Polish clip is NOT free-auto-detected as Russian.
        assert_eq!(
            resolve_clip_lang_core(None, Some("pl")).as_deref(),
            Some("pl"),
            "config.language must force the clip language even when the loop lang was None"
        );
        // The freshest config language wins over the spawn-time loop language.
        assert_eq!(
            resolve_clip_lang_core(Some("en"), Some("pl")).as_deref(),
            Some("pl"),
            "the freshest configured language must win"
        );
        // Loop lang is used when config is unset.
        assert_eq!(resolve_clip_lang_core(Some("de"), None).as_deref(), Some("de"));
        // Blank / "auto" / both-unset → None (whisper auto-detects; user can set it in Settings).
        assert_eq!(resolve_clip_lang_core(None, Some("")), None);
        assert_eq!(resolve_clip_lang_core(None, Some("auto")), None);
        assert_eq!(resolve_clip_lang_core(Some("  "), Some("  ")), None);
        assert_eq!(resolve_clip_lang_core(None, None), None);
    }

    #[test]
    fn merge_live_caption_seeds_then_dedups_overlap() {
        // First caption seeds the buffer.
        let a = merge_live_caption("", "we need to ship the beta");
        assert_eq!(a, "we need to ship the beta");
        // Rolling captions OVERLAP (same tail re-transcribed) — the shared head must NOT be duplicated;
        // only the genuinely-new tail is appended.
        let b = merge_live_caption(&a, "ship the beta on Friday");
        assert_eq!(b, "we need to ship the beta on Friday");
        // A caption fully contained in the buffer adds nothing.
        let c = merge_live_caption(&b, "on Friday");
        assert_eq!(c, "we need to ship the beta on Friday");
    }

    #[test]
    fn merge_live_caption_handles_empty_and_no_overlap() {
        // Empty caption leaves the buffer untouched; empty buffer takes the caption verbatim.
        assert_eq!(merge_live_caption("hello world", "   "), "hello world");
        assert_eq!(merge_live_caption("", "fresh start"), "fresh start");
        // No shared boundary ⇒ space-joined (a gap in captions, not an overlap).
        assert_eq!(
            merge_live_caption("budget approved", "contract signed"),
            "budget approved contract signed"
        );
    }

    #[test]
    fn merge_live_caption_folds_unicode_case_for_overlap() {
        // PR-A #4a: Whisper re-transcribes the overlapping tail with varying capitalization —
        // "że" vs "Że". `eq_ignore_ascii_case` does NOT fold non-ASCII (Polish diacritics), so the
        // old compare missed the overlap and DUPLICATED the shared words. Unicode folding must
        // detect it; the appended text stays byte-original.
        let merged = merge_live_caption("mówił że projekt", "Że projekt ruszy w piątek");
        assert_eq!(
            merged, "mówił że projekt ruszy w piątek",
            "a Unicode-case-only difference must still merge without duplication"
        );
    }

    #[test]
    fn merge_live_caption_ignores_edge_punctuation_for_overlap() {
        // PR-A #4b: trailing-punctuation variance between overlapping tails ("piątek." vs
        // "piątek") must not break overlap detection. The ACCUMULATED text keeps its original
        // bytes (incl. the "."); only the comparison is normalized.
        let merged = merge_live_caption("spotkamy się w piątek.", "w piątek omówimy budżet");
        assert_eq!(
            merged, "spotkamy się w piątek. omówimy budżet",
            "punctuation-only variance must still dedup the shared overlap"
        );
    }

    /// PR-A #5 HONESTY: the live buffer is a mic-side, unattributed rolling capture — the prompt
    /// must SAY so, and must not claim the transcript can answer "who said what" (any speaker
    /// attribution from it would be hallucinated).
    #[test]
    fn assistant_system_prompt_is_honest_about_attribution() {
        let p = assistant_system_prompt("we shipped the beta", "");
        let lower = p.to_lowercase();
        assert!(lower.contains("unattributed"), "must state the transcript is unattributed: {p}");
        assert!(lower.contains("microphone"), "must state the mic-side capture origin: {p}");
        assert!(
            !p.contains("its topic, decisions, who said what"),
            "must not claim the transcript knows who said what: {p}"
        );
    }

    // ── PR-A #2: the live-tail injection is GATED on meeting visibility (fail-closed) ─────────────

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn tmp_db(tag: &str) -> crate::storage::Db {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-live-gate-{tag}-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&p);
        crate::storage::Db::open_with_key(&p, TEST_DEK).unwrap()
    }

    /// Seed one summarized meeting with a note, foldered into `folder_id` when given.
    fn seed_meeting(db: &crate::storage::Db, mid: &str, folder_id: Option<&str>) {
        db.insert_meeting(&crate::storage::Meeting {
            id: mid.to_string(),
            started_at: "2026-07-01T09:00:00Z".to_string(),
            ended_at: None,
            title: Some("Sync".to_string()),
            duration_s: 60,
            audio_path: None,
            status: crate::storage::MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&crate::storage::NoteRecord {
            meeting_id: mid.to_string(),
            provider_id: "claude_code".to_string(),
            markdown: "# note".to_string(),
            created_at: "2026-07-01T09:05:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_meeting_folder(mid, folder_id).unwrap();
    }

    #[test]
    fn gated_live_context_masks_live_tail_when_meeting_not_visible() {
        // The mid-recording-relock window: the current meeting's folder is sealed and NOT
        // session-unlocked → the live tail (still legitimately in RAM) must NOT be injected into
        // the assistant's prompt. Fail-closed, exactly like the typed notes.
        let db = tmp_db("masked");
        db.insert_folder(&crate::storage::Folder {
            id: "f1".into(),
            name: "Secret".into(),
            path: "Secret".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-01T00:00:00Z".into(),
        })
        .unwrap();
        seed_meeting(&db, "m1", Some("f1"));
        db.set_folder_locked("f1", true, Some(&b"wrapped"[..])).unwrap();

        let live = std::sync::Mutex::new("sealed meeting tail still in RAM".to_string());
        let unlocked = std::collections::HashSet::new();
        let (tail, notes) = gated_live_context(&db, &live, "m1", &unlocked);
        assert!(tail.is_empty(), "sealed-not-unlocked meeting must inject NO live tail");
        assert!(notes.is_empty(), "sealed-not-unlocked meeting must inject NO typed notes");
        let prompt = assistant_system_prompt(&tail, &notes);
        assert!(!prompt.contains("LIVE TRANSCRIPT"), "no live section for a sealed meeting: {prompt}");
        assert!(!prompt.contains("sealed meeting tail"), "the RAM buffer must not reach the prompt");
        // The in-flight buffer itself is NOT wiped — a session re-unlock re-injects it.
        assert_eq!(*live.lock().unwrap(), "sealed meeting tail still in RAM");
    }

    #[test]
    fn gated_live_context_injects_for_visible_meeting_and_masks_without_one() {
        let db = tmp_db("visible");
        // An in-progress recording has no note rows yet ⇒ trivially visible: tail + notes inject.
        db.insert_meeting(&crate::storage::Meeting {
            id: "m-rec".to_string(),
            started_at: "2026-07-01T09:00:00Z".to_string(),
            ended_at: None,
            title: None,
            duration_s: 0,
            audio_path: None,
            status: crate::storage::MeetingStatus::Recording,
            folder_id: None,
        })
        .unwrap();
        db.set_manual_notes("m-rec", "ship Friday").unwrap();
        let live = std::sync::Mutex::new("we agreed to ship friday".to_string());
        let unlocked = std::collections::HashSet::new();
        let (tail, notes) = gated_live_context(&db, &live, "m-rec", &unlocked);
        assert_eq!(tail, "we agreed to ship friday", "a visible meeting injects the live tail");
        assert_eq!(notes, "ship Friday", "a visible meeting injects the typed notes");

        // NO current meeting (not recording) ⇒ fail-closed: nothing injects even if a stale
        // buffer somehow survived.
        let (tail, notes) = gated_live_context(&db, &live, "", &unlocked);
        assert!(tail.is_empty(), "no current meeting must inject NO live tail");
        assert!(notes.is_empty(), "no current meeting must inject NO typed notes");
    }

    #[test]
    fn gated_live_context_reinjects_after_session_unlock() {
        // A session unlock of the sealed folder makes the meeting visible again → the (unwiped)
        // in-flight buffer injects once more. Proves the gate is reversible, not a wipe.
        let db = tmp_db("reunlock");
        db.insert_folder(&crate::storage::Folder {
            id: "f1".into(),
            name: "Secret".into(),
            path: "Secret".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-01T00:00:00Z".into(),
        })
        .unwrap();
        seed_meeting(&db, "m1", Some("f1"));
        db.set_folder_locked("f1", true, Some(&b"wrapped"[..])).unwrap();

        let live = std::sync::Mutex::new("tail".to_string());
        let mut unlocked = std::collections::HashSet::new();
        assert!(gated_live_context(&db, &live, "m1", &unlocked).0.is_empty());
        unlocked.insert("f1".to_string());
        assert_eq!(gated_live_context(&db, &live, "m1", &unlocked).0, "tail");
    }

    // ── PR-A #1/#3: clearing the buffer at Stop + lock-surface hygiene ─────────────────────────────

    #[test]
    fn clear_live_transcript_empties_the_buffer() {
        let live = std::sync::Mutex::new("stale tail of the finished recording".to_string());
        clear_live_transcript(&live);
        assert!(live.lock().unwrap().is_empty(), "the buffer must be empty after Stop clears it");
        // Idempotent on an already-empty buffer.
        clear_live_transcript(&live);
        assert!(live.lock().unwrap().is_empty());
    }

    #[test]
    fn clear_live_transcript_if_idle_never_wipes_a_recording_in_flight() {
        // Mid-recording the lock surface must NOT wipe the user's in-flight buffer (egress
        // correctness there is the visibility gate's job); idle it clears.
        let live = std::sync::Mutex::new("in-flight captions".to_string());
        clear_live_transcript_if_idle(&live, true);
        assert_eq!(*live.lock().unwrap(), "in-flight captions", "mid-recording buffer untouched");
        clear_live_transcript_if_idle(&live, false);
        assert!(live.lock().unwrap().is_empty(), "idle buffer cleared");
    }

    #[test]
    fn assistant_system_prompt_injects_transcript_only_when_present() {
        // No live transcript (not recording / no captions yet) ⇒ the base prompt, no transcript section.
        let base = assistant_system_prompt("", "");
        assert!(!base.contains("LIVE TRANSCRIPT"), "no transcript section when empty: {base}");
        assert!(base.contains("in-meeting assistant"));
        // With a transcript ⇒ it is embedded + the brain is told to use it for the current meeting.
        let with = assistant_system_prompt("we shipped the beta and assigned the deck to Anna", "");
        assert!(with.contains("LIVE TRANSCRIPT"), "names the transcript section: {with}");
        assert!(with.contains("assigned the deck to Anna"), "embeds the transcript text");
        assert!(with.to_lowercase().contains("current"), "tells the brain it's the current meeting");
    }

    /// The system prompt instructs the model to DECIDE answer-vs-propose: name the `propose_note` tool
    /// and tell it to call it ONLY when the user asks for a note (the model decides; no regex in code).
    /// Present in BOTH the no-transcript and with-transcript branches (it lives in the shared base).
    #[test]
    fn assistant_system_prompt_instructs_propose_note_decision() {
        for prompt in [
            assistant_system_prompt("", ""),
            assistant_system_prompt("we shipped the beta", ""),
        ] {
            assert!(prompt.contains("propose_note"), "names the propose_note tool: {prompt}");
            assert!(
                prompt.to_lowercase().contains("note"),
                "tells the model when to draft a note: {prompt}"
            );
            // Both an answer path and a note-draft path are described (decide between them).
            assert!(prompt.to_lowercase().contains("answer"), "describes plain answering too: {prompt}");
        }
    }

    #[test]
    fn assistant_system_prompt_truncates_to_recent_tail() {
        // A very long transcript is truncated to the RECENT tail (so a 2-hour meeting can't blow the
        // context) and marked elided.
        let long = "x ".repeat(LIVE_TRANSCRIPT_INJECT_CHARS); // way over the inject budget
        let p = assistant_system_prompt(&long, "");
        assert!(p.contains('…'), "elision marker present when truncated");
        assert!(p.chars().count() < long.chars().count(), "shorter than the raw transcript");
    }

    /// brain2 realtime notes: typed notes are injected as their own section when present, and the
    /// EMPTY-typed-notes prompt is BYTE-IDENTICAL to the pre-feature output (no regression).
    #[test]
    fn assistant_system_prompt_injects_typed_notes_and_empty_is_byte_identical() {
        // Empty typed notes ⇒ no typed-notes section, AND byte-identical to the no-notes prompt for
        // BOTH the no-transcript and with-transcript branches.
        let no_tx = assistant_system_prompt("", "");
        assert!(!no_tx.contains("TYPED NOTES"), "no typed-notes section when empty: {no_tx}");
        let tx = "we shipped the beta and assigned the deck to Anna";
        assert_eq!(
            assistant_system_prompt(tx, ""),
            assistant_system_prompt(tx, "   "),
            "whitespace-only typed notes must be byte-identical to none (no regression)"
        );

        // Present typed notes ⇒ embedded under the labeled section, alongside the transcript.
        let with = assistant_system_prompt(tx, "DECISION: ship Friday. Anna owns QA sign-off.");
        assert!(with.contains("TYPED NOTES"), "names the typed-notes section: {with}");
        assert!(with.contains("Anna owns QA sign-off"), "embeds the typed-notes text");
        assert!(with.contains("LIVE TRANSCRIPT"), "still injects the transcript too");

        // Typed notes inject even with NO transcript (the user can type before any caption lands).
        let notes_only = assistant_system_prompt("", "remember: budget cap is the blocker");
        assert!(notes_only.contains("TYPED NOTES"), "typed notes inject without a transcript");
        assert!(notes_only.contains("budget cap is the blocker"));
        assert!(!notes_only.contains("LIVE TRANSCRIPT"), "no transcript section when transcript empty");

        // A very long typed-notes buffer is truncated to the recent tail (bounded like the transcript).
        let long = "y ".repeat(LIVE_TRANSCRIPT_INJECT_CHARS);
        let p = assistant_system_prompt("", &long);
        assert!(p.contains('…'), "elision marker present when typed notes truncated");
    }
}
