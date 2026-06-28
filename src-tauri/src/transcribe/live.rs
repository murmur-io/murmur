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
                            spawn_command_dispatch(app.clone(), command);
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
                            spawn_dispatch(app.clone(), payload.intent.clone());
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

/// Dispatch a parsed [`crate::audio::wake::VoiceIntent`] OFF the live transcription tick, on a
/// detached OS thread, and emit [`crate::events::EVENT_VOICE_ACTION_RESULT`] with the result. The
/// brain call can take seconds, so it MUST run off-thread — the live loop never blocks on it. The
/// whole body is best-effort + panic-free (`handle_voice_action` never panics, the emit error is
/// ignored), and it runs on its OWN thread so even an unexpected panic is contained and cannot
/// disrupt the recording or the caption.
///
/// All state (config snapshot, reasoner, db, the LIVE `unlocked` set, the current meeting id) is
/// read from the managed [`AppState`] inside the thread — the reasoner is borrowed (`&*`) rather
/// than cloned (it is `Box<dyn LocalReasoner>`), which is sound because `AppState` lives for the
/// whole app lifetime. Gating: `handle_voice_action` routes every read through the visibility gate
/// over THIS unlocked set, and the brain honors the consent gate.
fn spawn_dispatch(app: AppHandle, intent: crate::audio::wake::VoiceIntent) {
    let _ = std::thread::Builder::new()
        .name("murmur-voice-action".into())
        .spawn(move || {
            let state = app.state::<AppState>();
            // Snapshot the config + the live unlocked set + the current meeting id. Cheap clones so
            // we don't hold the locks across the (possibly slow) dispatch.
            let config = match state.config.lock() {
                Ok(c) => c.clone(),
                Err(_) => return,
            };
            let unlocked = match state.unlocked_folders.lock() {
                Ok(u) => u.clone(),
                Err(_) => return,
            };
            let meeting_id = state
                .current_meeting
                .lock()
                .ok()
                .and_then(|m| m.map(|id| id.to_string()))
                .unwrap_or_default();

            // The wake parser does NOT translate — the intent's own topic/entity IS the user's
            // literal words, so retrieval already keys off them. Pass them as the literal command.
            let literal = literal_command_of(&intent);
            let result = crate::voice_action::handle_voice_action(
                &intent,
                &*state.reasoner,
                &state.db,
                &unlocked,
                &config,
                &meeting_id,
                &literal,
                Some(&app),
            );
            // PII rule: log only the coarse intent kind + status, never the summary/citations.
            tracing::info!(
                target: "voice",
                intent = %result.intent_kind,
                status = %result.status,
                "voice action dispatched"
            );
            // PERSIST the interaction against the CURRENT recording (best-effort + panic-free): the
            // wake-path command lives in the intent's literal words. A persist failure NEVER disrupts
            // the dispatch — it is logged (non-PII) and dropped.
            persist_interaction(&state, &meeting_id, &literal, &result);
            let _ = app.emit(crate::events::EVENT_VOICE_ACTION_RESULT, result);
        });
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

/// Resolve a NON-EMPTY heard `command` to an intent (keyword → brain → fallback) and dispatch it
/// over the SAME gated `handle_voice_action` path as the wake trigger, with the HEARD command
/// surfaced onto the result so the FE card can show "usłyszano: {command}". Runs entirely OFF the
/// transcription tick on its own detached OS thread — the brain interpret + the dispatch can each
/// take seconds and MUST NOT block the live loop. Best-effort + panic-free, exactly like
/// [`spawn_dispatch`]; a poisoned lock simply aborts the thread.
fn spawn_command_dispatch(app: AppHandle, command: String) {
    let _ = std::thread::Builder::new()
        .name("murmur-voice-command".into())
        .spawn(move || {
            let state = app.state::<AppState>();
            let config = match state.config.lock() {
                Ok(c) => c.clone(),
                Err(_) => return,
            };
            let unlocked = match state.unlocked_folders.lock() {
                Ok(u) => u.clone(),
                Err(_) => return,
            };
            let meeting_id = state
                .current_meeting
                .lock()
                .ok()
                .and_then(|m| m.map(|id| id.to_string()))
                .unwrap_or_default();

            // Keyword fast-path → BRAIN interpret (consent-gated, same reasoner) → keyword fallback.
            let intent = resolve_command_intent(&*state.reasoner, &command);
            // RETRIEVAL keys off the user's LITERAL dictated `command` (their own language/words),
            // NOT the brain-interpreted topic — so a Polish note matches a Polish question.
            let result = crate::voice_action::handle_voice_action(
                &intent,
                &*state.reasoner,
                &state.db,
                &unlocked,
                &config,
                &meeting_id,
                &command,
                Some(&app),
            )
            // Surface the user's OWN dictated command onto the result for the FE card.
            .with_command(&command);

            // PII rule: log only the coarse intent kind + status, never the command/summary text.
            tracing::info!(
                target: "voice",
                intent = %result.intent_kind,
                status = %result.status,
                "manual voice command dispatched"
            );
            // PERSIST the interaction against the CURRENT recording (best-effort + panic-free): the
            // manual-path command is the user's HEARD dictation. A persist failure NEVER disrupts the
            // dispatch — it is logged (non-PII) and dropped.
            persist_interaction(&state, &meeting_id, &command, &result);
            let _ = app.emit(crate::events::EVENT_VOICE_ACTION_RESULT, result);
        });
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

/// The user's LITERAL words carried by an intent, for use as the retrieval-side literal command on
/// the WAKE path (where the deterministic parser already kept the user's own language — it never
/// translates). For non-RAG intents this is the payload text; the Research/Recall topics ARE the
/// literal command tail. Empty `String` for `Unknown` (nothing actionable).
fn literal_command_of(intent: &crate::audio::wake::VoiceIntent) -> String {
    use crate::audio::wake::VoiceIntent as VI;
    match intent {
        VI::Research { topic } => topic.clone(),
        VI::Recall { entity } => entity.clone(),
        VI::SlackSearch { query } => query.clone(),
        VI::CreateReminder { text, .. } => text.clone(),
        VI::NoteAside { text } => text.clone(),
        VI::Unknown { .. } => String::new(),
    }
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
}
