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

            let result = crate::voice_action::handle_voice_action(
                &intent,
                &*state.reasoner,
                &state.db,
                &unlocked,
                &config,
                &meeting_id,
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
}
