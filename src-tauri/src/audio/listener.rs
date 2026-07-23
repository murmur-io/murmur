//! Voice trigger — an opt-in background listener that starts a recording when it hears a
//! wake phrase (e.g. "start recording" / "zacznij nagrywanie").
//!
//! It captures short mic windows, transcribes each with Whisper, and on a match emits
//! [`VOICE_START_EVENT`]; the frontend reacts by calling `start_recording`. It reuses
//! `Recorder` + `Transcriber`. The mic has a single owner: each window is captured then
//! released (so by the time a phrase is detected the mic is already free), and the real
//! recording stops the listener before it opens the mic.
//!
//! ⚠️ RUNTIME/BUILD PENDING: live detection needs a real mic, and this exact rewritten loop has not
//! been compiled while the installed DMG is recording. The pure phrase matcher is unit-covered.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use tauri::{AppHandle, Emitter};

use crate::audio::{resample_to_16k, Recorder};
use crate::error::{AppError, Result};
use crate::transcribe::Transcriber;

/// Emitted when the wake phrase is heard; the frontend then starts a recording.
pub const VOICE_START_EVENT: &str = "murmur://voice-start";

/// Length of each listening window.
const WINDOW: Duration = Duration::from_millis(2200);

type WakeTranscriberCache = Option<(PathBuf, Arc<Transcriber>)>;

fn wake_transcriber_cache() -> &'static Mutex<WakeTranscriberCache> {
    static CACHE: OnceLock<Mutex<WakeTranscriberCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn wake_transcriber(model_path: &PathBuf) -> Result<Arc<Transcriber>> {
    let mut cache = wake_transcriber_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((cached_path, transcriber)) = cache.as_ref() {
        if cached_path == model_path {
            return Ok(Arc::clone(transcriber));
        }
    }
    let loaded = Arc::new(Transcriber::load(model_path)?);
    *cache = Some((model_path.clone(), Arc::clone(&loaded)));
    Ok(loaded)
}

/// Evict the standby-only Whisper context during every incompatible resident-kind handoff. Called
/// only while the model coordinator owns the sole generation, so no listener decode can hold a
/// cache clone concurrently.
pub(crate) fn release_wake_transcriber_cache() {
    let mut cache = wake_transcriber_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *cache = None;
}

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

/// A running voice listener. [`stop_with_timeout`](Self::stop_with_timeout) is the explicit
/// bounded shutdown seam used before a real recording opens the mic.
pub struct VoiceListener {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    done: Receiver<()>,
}

impl VoiceListener {
    /// Spawn the listen loop on a background thread. `model_path` is the Whisper model
    /// used for wake-phrase detection; `language` biases recognition (None = auto-detect).
    pub fn start(app: AppHandle, model_path: PathBuf, language: Option<String>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let (done_tx, done) = mpsc::channel();

        let handle = std::thread::spawn(move || {
            tracing::info!(target: "voice", "voice listener started");

            while !stop_flag.load(Ordering::Relaxed) {
                // Wake detection is intentionally a short, ring-resident capture rather than a
                // meeting generation, but activation still requires the sole checkpoint
                // authority to live outside `Recorder`. Retain it until capture has stopped so a
                // callback can never run after the authority is dropped.
                let mut prepared = match Recorder::prepare_voice_listener() {
                    Ok(prepared) => prepared,
                    Err(e) => {
                        tracing::warn!(target: "voice", error = %e, "voice listener: mic open failed");
                        std::thread::sleep(Duration::from_millis(800));
                        continue;
                    }
                };
                let checkpoint_writer = match prepared.take_checkpoint_writer() {
                    Ok(writer) => writer,
                    Err(e) => {
                        tracing::warn!(target: "voice", error = %e, "voice listener: mic authority unavailable");
                        std::thread::sleep(Duration::from_millis(800));
                        continue;
                    }
                };
                let rec = match prepared.activate() {
                    Ok(recorder) => recorder,
                    Err(e) => {
                        tracing::warn!(target: "voice", error = %e, "voice listener: mic activation failed");
                        std::thread::sleep(Duration::from_millis(800));
                        continue;
                    }
                };
                std::thread::sleep(WINDOW);
                let stopped = rec.stop();
                // `Recorder::stop` is non-consuming so explicitly destroy the stopped stream
                // before releasing its checkpoint authority. On a timeout, Drop makes one final
                // bounded stop attempt while the authority is still live.
                drop(rec);
                drop(checkpoint_writer);
                let (samples, rate) = match stopped {
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
                let transcript = crate::perf::with_model_generation(
                    None,
                    crate::perf::ResidentModelKind::VoiceWhisper,
                    || {
                        let transcriber = wake_transcriber(&model_path)?;
                        transcriber.transcribe(&s16, language.as_deref())
                    },
                );
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                if let Ok(t) = transcript {
                    if is_wake_phrase(&t.full_text) {
                        tracing::info!(target: "voice", "wake phrase detected");
                        let _ = app.emit(VOICE_START_EVENT, ());
                        break;
                    }
                }
            }
            tracing::info!(target: "voice", "voice listener stopped");
            let _ = done_tx.send(());
        });

        Self {
            stop,
            handle: Some(handle),
            done,
        }
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Signal the worker and wait only for the supplied deadline. A timed-out worker remains owned
    /// by this value so Start can fail before capture and a later retry can reap it; it is never
    /// detached and falsely reported as stopped.
    pub fn stop_with_timeout(&mut self, timeout: Duration) -> Result<bool> {
        self.request_stop();
        if self.handle.is_none() {
            return Ok(true);
        }
        match self.done.recv_timeout(timeout) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                let handle = self
                    .handle
                    .take()
                    .ok_or_else(|| AppError::Audio("voice-listener ownership was lost".into()))?;
                handle.join().map_err(|_| {
                    AppError::Audio("voice-listener worker panicked during shutdown".into())
                })?;
                Ok(true)
            }
            Err(RecvTimeoutError::Timeout) => Ok(false),
        }
    }
}

impl Drop for VoiceListener {
    fn drop(&mut self) {
        self.request_stop();
        // Drop must never reproduce the old unbounded Start/shutdown join. Explicit owners use
        // `stop_with_timeout`; process teardown may detach a still-wedged native decode.
        if self
            .handle
            .as_ref()
            .is_some_and(|handle| handle.is_finished())
        {
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
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
