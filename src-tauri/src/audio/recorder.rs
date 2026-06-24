use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat, StreamConfig};

use crate::error::{AppError, Result};

/// Shared state written by the cpal capture callback and read by the owner.
///
/// `samples` accumulates mono `f32` frames at the device's native sample rate.
/// `peak` holds the most recent peak amplitude (0.0..=1.0) encoded as `f32` bits in
/// an `AtomicU32` so the UI meter can read it without taking the sample-buffer lock.
struct Shared {
    samples: Mutex<Vec<f32>>,
    peak: AtomicU32,
}

impl Shared {
    fn new() -> Self {
        Self {
            samples: Mutex::new(Vec::new()),
            peak: AtomicU32::new(0),
        }
    }

    fn store_peak(&self, value: f32) {
        self.peak.store(value.to_bits(), Ordering::Relaxed);
    }

    fn load_peak(&self) -> f32 {
        f32::from_bits(self.peak.load(Ordering::Relaxed))
    }
}

/// Message sent from the capture thread back to the owner once the stream is built.
struct StartInfo {
    source_sample_rate: u32,
}

/// Owns a dedicated capture **thread** that builds and runs the cpal input stream and
/// accumulates mono f32 samples at the device rate.
///
/// Why a thread: `cpal::Stream` is `!Send` on CoreAudio (its callback boxes a `!Send`
/// closure), so it cannot live inside Tauri's shared `State` (which must be `Send +
/// Sync`). We confine the stream entirely to one OS thread — it is created, played, and
/// dropped there and never crosses a thread boundary. The owner communicates with that
/// thread only through `Send` handles: an `Arc<Shared>` for samples/peak and a `stop`
/// channel. This keeps the `start`/`stop`/`level` API (PHASE0-PLAN §5.4) intact while
/// making `Recorder` `Send + Sync`.
///
/// Phase 0 is **mic-only mono**: the default input device is captured, any multi-channel
/// input is down-mixed to mono, and samples are buffered at the device's native sample
/// rate. Conversion to 16 kHz happens later in [`crate::audio::wav`].
///
/// TODO(phase2): system-audio capture via ScreenCaptureKit lives alongside this mic path
/// (separate Objective-C/Swift bridge producing a second mono track); `pipeline.rs` will
/// mix the two before transcription. No system-audio hook is wired in Phase 0.
pub struct Recorder {
    shared: Arc<Shared>,
    source_sample_rate: u32,
    stop_tx: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Recorder {
    /// Open the default input device and start capturing on a dedicated thread.
    /// Non-blocking: returns once the stream is built and playing (or with the build
    /// error surfaced from the capture thread).
    pub fn start() -> Result<Self> {
        let shared = Arc::new(Shared::new());
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<StartInfo>>();

        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("meetnotes-audio-capture".into())
            .spawn(move || capture_thread(thread_shared, stop_rx, ready_tx))
            .map_err(|e| AppError::Audio(format!("failed to spawn capture thread: {e}")))?;

        // Wait for the thread to report stream-build success/failure.
        let info = match ready_rx.recv() {
            Ok(Ok(info)) => info,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                let _ = thread.join();
                return Err(AppError::Audio(
                    "capture thread exited before reporting readiness".into(),
                ));
            }
        };

        Ok(Self {
            shared,
            source_sample_rate: info.source_sample_rate,
            stop_tx,
            thread: Some(thread),
        })
    }

    /// Stop the stream, return (mono_samples_at_source_rate, source_sample_rate_hz).
    pub fn stop(mut self) -> Result<(Vec<f32>, u32)> {
        // Signal the capture thread to drop the stream and exit, then join it.
        let _ = self.stop_tx.send(());
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }

        let samples = self
            .shared
            .samples
            .lock()
            .map_err(|_| AppError::Audio("sample buffer mutex poisoned".into()))?
            .drain(..)
            .collect::<Vec<f32>>();

        tracing::info!(
            frames = samples.len(),
            sample_rate = self.source_sample_rate,
            "stopped mic capture"
        );

        Ok((samples, self.source_sample_rate))
    }

    /// Current peak level 0.0..=1.0 for the UI meter (best-effort, lock-free read).
    pub fn level(&self) -> f32 {
        self.shared.load_peak()
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // If dropped without an explicit `stop` (e.g. on shutdown), tear the thread down.
        let _ = self.stop_tx.send(());
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

/// Body of the capture thread: build the cpal stream, report readiness, then block until
/// a stop signal arrives. The `Stream` lives only here and is dropped on the way out.
fn capture_thread(
    shared: Arc<Shared>,
    stop_rx: Receiver<()>,
    ready_tx: Sender<Result<StartInfo>>,
) {
    let built = build_and_play(&shared);
    match built {
        Ok((stream, info)) => {
            // Notify the owner the stream is live; keep the stream alive on this thread.
            if ready_tx.send(Ok(info)).is_err() {
                return; // owner gone
            }
            // Block until asked to stop (or the sender is dropped).
            loop {
                match stop_rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => continue,
                }
            }
            let _ = stream.pause();
            drop(stream);
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
        }
    }
}

/// Build + start the input stream on the current thread, returning it plus the source
/// sample rate.
fn build_and_play(shared: &Arc<Shared>) -> Result<(cpal::Stream, StartInfo)> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| AppError::Audio("no default input device available".into()))?;

    let supported = device
        .default_input_config()
        .map_err(|e| AppError::Audio(format!("failed to query default input config: {e}")))?;

    let sample_format = supported.sample_format();
    let source_sample_rate = supported.sample_rate().0;
    let channels = supported.channels();
    let config: StreamConfig = supported.into();

    // NOTE(privacy): never log captured audio or device names that could be PII;
    // ids/format only.
    tracing::info!(
        sample_rate = source_sample_rate,
        channels,
        ?sample_format,
        "starting mic capture"
    );

    let stream = match sample_format {
        SampleFormat::F32 => build_stream::<f32>(&device, &config, shared.clone()),
        SampleFormat::I16 => build_stream::<i16>(&device, &config, shared.clone()),
        SampleFormat::U16 => build_stream::<u16>(&device, &config, shared.clone()),
        SampleFormat::I32 => build_stream::<i32>(&device, &config, shared.clone()),
        SampleFormat::I8 => build_stream::<i8>(&device, &config, shared.clone()),
        SampleFormat::U8 => build_stream::<u8>(&device, &config, shared.clone()),
        other => {
            return Err(AppError::Audio(format!(
                "unsupported input sample format: {other:?}"
            )))
        }
    }?;

    stream
        .play()
        .map_err(|e| AppError::Audio(format!("failed to start input stream: {e}")))?;

    Ok((stream, StartInfo { source_sample_rate }))
}

/// Build a typed input stream that downmixes to mono `f32` and tracks the peak level.
fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    shared: Arc<Shared>,
) -> Result<cpal::Stream>
where
    T: Sample + cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = config.channels as usize;
    let channels = channels.max(1);

    let err_shared = shared.clone();
    let data_cb = move |data: &[T], _: &cpal::InputCallbackInfo| {
        // Downmix interleaved frames to mono by averaging channels.
        let mut mono = Vec::with_capacity(data.len() / channels + 1);
        let mut peak = 0.0f32;
        for frame in data.chunks(channels) {
            let mut acc = 0.0f32;
            for s in frame {
                acc += f32::from_sample(*s);
            }
            let v = acc / channels as f32;
            let mag = v.abs();
            if mag > peak {
                peak = mag;
            }
            mono.push(v);
        }
        shared.store_peak(peak.clamp(0.0, 1.0));
        if let Ok(mut buf) = shared.samples.lock() {
            buf.extend_from_slice(&mono);
        }
    };

    let err_cb = move |err| {
        // Reset the meter on stream error so the UI doesn't latch a stale level.
        err_shared.store_peak(0.0);
        tracing::error!(error = %err, "input stream error");
    };

    device
        .build_input_stream(config, data_cb, err_cb, None)
        .map_err(|e| AppError::Audio(format!("failed to build input stream: {e}")))
}
