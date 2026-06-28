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

#[derive(serde::Serialize, Clone)]
struct LiveCaption {
    text: String,
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

    loop {
        std::thread::sleep(TICK);

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
                // MANUAL voice-command capture (the button trigger) — checked EVERY tick,
                // independent of the wake path and independent of `realtime_reactions`. When the
                // user has clicked "ask the assistant" the freshly-transcribed tail IS the command:
                // no wake word, no word-order requirement. We decide per tick whether to dispatch
                // now (a recognized intent OR the tick budget is spent) or keep listening. Best-effort
                // + panic-free: a poisoned lock or empty tail simply leaves the capture armed; the
                // dispatch runs off-thread exactly like the wake path. NOTE: an EMPTY tail still
                // counts down the budget so a silent capture can't hang forever.
                if let Some(decision) =
                    step_manual_capture(&app, &transcriber, lang.as_deref(), &text)
                {
                    match decision {
                        ManualCaptureDecision::Dispatch { command } => {
                            // Resolve (keyword → brain → fallback) + dispatch OFF the tick, with the
                            // heard command surfaced onto the result for the FE card.
                            spawn_command_dispatch(app.clone(), command);
                            let _ = app.emit(
                                crate::events::EVENT_VOICE_COMMAND_LISTENING,
                                crate::events::VoiceCommandListeningPayload { active: false },
                            );
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
                    if let Some(payload) = wake_event_for(&text) {
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
            );
            // PII rule: log only the coarse intent kind + status, never the summary/citations.
            tracing::info!(
                target: "voice",
                intent = %result.intent_kind,
                status = %result.status,
                "voice action dispatched"
            );
            let _ = app.emit(crate::events::EVENT_VOICE_ACTION_RESULT, result);
        });
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
            let _ = app.emit(crate::events::EVENT_VOICE_ACTION_RESULT, result);
        });
}

/// What the live loop should do with the MANUAL voice-command capture on this tick.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ManualCaptureDecision {
    /// A NON-EMPTY command was captured (real speech heard since the click) → resolve + dispatch it
    /// (keyword fast-path, else brain interpret, else keyword fallback) and clear the capture.
    Dispatch { command: String },
    /// Nothing heard yet but budget remains → keep the capture armed and wait for the next tick.
    KeepListening,
    /// The whole budget expired with NOTHING heard (the user never spoke) → clear the capture and
    /// surface a graceful "nothing_heard". NEVER dispatches an empty command.
    NothingHeard,
}

/// Advance the MANUAL voice-command capture by one live tick. Snapshots ONLY the audio captured
/// SINCE the click (`Recorder::snapshot_from(start_sample)`) — the POST-CLICK utterance — and
/// transcribes it in isolation, so the command is exactly what the user said after clicking, not the
/// rolling 14s tail. Falls back to the rolling-tail `caption` text only when no offset was latched
/// (degenerate arm path). Returns `None` when no manual capture is armed (the common case).
///
/// When a capture IS armed it mutates the budget in place and returns the [`ManualCaptureDecision`],
/// CLEARING the capture state on dispatch / nothing-heard (so exactly one outcome fires per click).
/// Best-effort + panic-free: a poisoned lock is treated as "no capture" (`None`).
///
/// The transcription of the post-click window + the dispatch/emit are the caller's job (off the
/// tick); the budget arithmetic + the no-empty guard are the pure [`decide_manual_capture`].
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

/// PURE, headless-testable core of the manual-capture step. Given the armed [`CaptureState`] and the
/// `command` heard SINCE the click (the post-click window's transcript), decide the outcome and the
/// NEXT capture state (`None` = cleared on a terminal outcome, `Some(decremented)` while listening).
///
/// Rules (no wake word — the WHOLE post-click utterance is the command; NEVER dispatch empty):
/// 1. If the command is NON-EMPTY (real speech heard) → `Dispatch{command}` now and clear the
///    capture. The caller resolves it (keyword → brain → keyword fallback) and dispatches it with the
///    heard command surfaced.
/// 2. Else (silence this tick) → spend one tick of budget:
///    - budget still > 0 → KEEP LISTENING (give the user time to start/finish speaking).
///    - budget now == 0 → `NothingHeard`: clear the capture and surface a graceful "nothing_heard".
///      The capture can never hang AND it never dispatches an empty/Unknown command.
///
/// No I/O, no FFI, no egress — just emptiness check + arithmetic.
fn decide_manual_capture(
    capture: crate::state::CaptureState,
    command: &str,
) -> (ManualCaptureDecision, Option<crate::state::CaptureState>) {
    let command = command.trim();
    // GARBAGE FILTER: a too-short / pure-filler clip ("Uh,", "eee", "hmm", a lone char or
    // punctuation) is NOT a real command — Whisper emits these for a ~3s silent/noise clip. Treat
    // it exactly like a silent tick (KEEP LISTENING / NOTHING_HEARD at budget end) rather than
    // dispatching a Research on "Uh,". The user gets a clean "nothing_heard", never a garbage card.
    if !command.is_empty() && is_meaningful_command(command) {
        // Real speech heard since the click → dispatch it, clear the capture.
        return (
            ManualCaptureDecision::Dispatch { command: command.to_string() },
            None,
        );
    }
    // Silence OR garbage/filler this tick: spend one tick of budget.
    let remaining = capture.budget.saturating_sub(1);
    if remaining == 0 {
        // Budget exhausted with nothing ever heard → graceful give-up, clear the capture.
        (ManualCaptureDecision::NothingHeard, None)
    } else {
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
        CaptureState { budget, start_sample: Some(0) }
    }

    #[test]
    fn manual_capture_nonempty_command_dispatches_with_the_heard_command_and_clears() {
        // ANY non-empty post-click utterance is dispatched, carrying the heard command verbatim.
        let cap = armed(5);
        let (decision, next) = decide_manual_capture(cap, "  zrób research o konkurencji  ");
        assert_eq!(
            decision,
            ManualCaptureDecision::Dispatch { command: "zrób research o konkurencji".into() },
            "a non-empty command must dispatch with the heard (trimmed) command"
        );
        assert!(next.is_none(), "capture must be cleared after a dispatch");
    }

    #[test]
    fn manual_capture_silent_tick_with_budget_left_keeps_listening_and_decrements() {
        // An EMPTY post-click window (no speech yet) but budget remains → keep listening, budget −1.
        let cap = armed(3);
        let (decision, next) = decide_manual_capture(cap, "   ");
        assert_eq!(decision, ManualCaptureDecision::KeepListening);
        assert_eq!(
            next,
            Some(CaptureState { budget: 2, start_sample: Some(0) }),
            "a silent tick must decrement the budget and keep the SAME latched offset"
        );
    }

    #[test]
    fn manual_capture_budget_exhausted_silent_is_nothing_heard_not_empty_dispatch() {
        // The NO-EMPTY-DISPATCH guarantee: a fully-silent capture ends in NothingHeard (graceful),
        // NEVER an empty Unknown dispatch.
        let (d1, n1) = decide_manual_capture(armed(2), "");
        assert_eq!(d1, ManualCaptureDecision::KeepListening);
        assert_eq!(n1, Some(CaptureState { budget: 1, start_sample: Some(0) }));
        let (d2, n2) = decide_manual_capture(n1.unwrap(), "");
        assert_eq!(
            d2,
            ManualCaptureDecision::NothingHeard,
            "a fully-silent capture must terminate as NothingHeard, never an empty dispatch"
        );
        assert!(n2.is_none(), "capture must be cleared once it gives up");
    }

    #[test]
    fn nothing_heard_result_is_graceful_with_empty_command() {
        let r = crate::voice_action::VoiceActionResult::nothing_heard();
        assert_eq!(r.status, "nothing_heard");
        assert!(r.command.is_empty(), "nothing was heard ⇒ no command surfaced");
        assert!(r.summary.contains("Nie usłyszałem"), "friendly PL nudge, not 'didn't catch an action'");
    }

    #[test]
    fn manual_capture_default_budget_gives_a_reasonable_window() {
        // The default arms ~5 ticks (≈15s at the 3s TICK) — comfortable to click then speak.
        assert!(
            (3..=8).contains(&CaptureState::DEFAULT_BUDGET),
            "default capture budget should give the user a reasonable window to speak"
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
    fn decide_manual_capture_drops_garbage_keeps_listening_then_nothing_heard() {
        // "Uh," is garbage → treat like a silent tick: KEEP LISTENING (budget −1), NOT a dispatch.
        let (d1, n1) = decide_manual_capture(armed(2), "Uh,");
        assert_eq!(d1, ManualCaptureDecision::KeepListening, "garbage must not dispatch");
        assert_eq!(n1, Some(CaptureState { budget: 1, start_sample: Some(0) }));
        // A second garbage tick exhausts the budget → NothingHeard (graceful), never a dispatch.
        let (d2, n2) = decide_manual_capture(n1.unwrap(), "eee");
        assert_eq!(
            d2,
            ManualCaptureDecision::NothingHeard,
            "a fully-garbage capture ends as NothingHeard, never a garbage dispatch"
        );
        assert!(n2.is_none(), "capture cleared once it gives up");
    }

    #[test]
    fn decide_manual_capture_dispatches_a_real_command() {
        // A genuine command dispatches with the heard (trimmed) command verbatim.
        let (d, n) = decide_manual_capture(armed(5), "  zrób research o pogodzie  ");
        assert_eq!(
            d,
            ManualCaptureDecision::Dispatch { command: "zrób research o pogodzie".into() },
            "a real command must dispatch"
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
