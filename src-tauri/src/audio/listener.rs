//! Voice trigger — an opt-in background listener that starts a recording when it hears a
//! wake phrase (e.g. "start recording" / "zacznij nagrywanie").
//!
//! It captures short mic windows, transcribes each with Whisper, and on a match emits
//! [`VOICE_START_EVENT`]; the frontend reacts by calling `start_recording`. It reuses
//! `Recorder` + `Transcriber`. The mic has a single owner: each window is captured then
//! released (so by the time a phrase is detected the mic is already free), and the real
//! recording stops the listener before it opens the mic.
//!
//! ⚠️ RUNTIME-UNVERIFIED headless: live detection needs a real mic. The phrase matcher
//! ([`is_wake_phrase`]) is unit-tested; the loop is compile-verified.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use tauri::{AppHandle, Emitter};

use crate::audio::{resample_to_16k, Recorder};
use crate::transcribe::Transcriber;

/// Emitted when the wake phrase is heard; the frontend then starts a recording.
pub const VOICE_START_EVENT: &str = "murmur://voice-start";

/// Length of each listening window.
const WINDOW: Duration = Duration::from_millis(2200);

/// Whether `text` contains a recognised "start recording" wake phrase (English / Polish).
pub fn is_wake_phrase(text: &str) -> bool {
    let t = text.to_lowercase();
    const PHRASES: [&str; 7] = [
        "start recording",
        "start the recording",
        "begin recording",
        "start nagrywanie",
        "zacznij nagrywanie",
        "rozpocznij nagrywanie",
        "zacznij nagrywać",
    ];
    PHRASES.iter().any(|p| t.contains(p))
}

/// A running voice listener. Dropping it (or calling [`stop`](Self::stop)) ends the loop
/// and releases the mic.
pub struct VoiceListener {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl VoiceListener {
    /// Spawn the listen loop on a background thread. `model_path` is the Whisper model
    /// used for wake-phrase detection; `language` biases recognition (None = auto-detect).
    pub fn start(app: AppHandle, model_path: PathBuf, language: Option<String>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();

        let handle = std::thread::spawn(move || {
            let transcriber = match Transcriber::load(&model_path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(target: "voice", error = %e, "voice listener: model load failed");
                    return;
                }
            };
            tracing::info!(target: "voice", "voice listener started");

            while !stop_flag.load(Ordering::Relaxed) {
                let rec = match Recorder::start(None) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(target: "voice", error = %e, "voice listener: mic open failed");
                        std::thread::sleep(Duration::from_millis(800));
                        continue;
                    }
                };
                std::thread::sleep(WINDOW);
                let (samples, rate) = match rec.stop() {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                let s16 = match resample_to_16k(&samples, rate) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                // VOICE TRIGGER uses the Fast (greedy/best_of:1) profile via `transcribe` —
                // NOT the batch beam-search path. We only need a wake-phrase match on a short
                // ~2s window many times over; low latency matters far more than transcript
                // quality here, so beam search would be wasted CPU. Keep it greedy.
                if let Ok(t) = transcriber.transcribe(&s16, language.as_deref()) {
                    if is_wake_phrase(&t.full_text) {
                        tracing::info!(target: "voice", "wake phrase detected");
                        let _ = app.emit(VOICE_START_EVENT, ());
                        break;
                    }
                }
            }
            tracing::info!(target: "voice", "voice listener stopped");
        });

        Self { stop, handle: Some(handle) }
    }

    /// Signal the loop to stop and join the thread (releases the mic). Blocks up to one
    /// window (~2s) if a capture is in flight — call from a blocking context.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for VoiceListener {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_english_phrases() {
        assert!(is_wake_phrase("ok start recording now"));
        assert!(is_wake_phrase("Start The Recording please"));
        assert!(is_wake_phrase("could you begin recording"));
    }

    #[test]
    fn detects_polish_phrases() {
        assert!(is_wake_phrase("dobra zacznij nagrywanie"));
        assert!(is_wake_phrase("Rozpocznij Nagrywanie teraz"));
    }

    #[test]
    fn ignores_unrelated_speech() {
        assert!(!is_wake_phrase("let's talk about the budget for friday"));
        assert!(!is_wake_phrase("the recording sounded great yesterday"));
        assert!(!is_wake_phrase(""));
    }
}
