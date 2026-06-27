use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
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
    /// Live mic-mute flag, toggled mid-recording via `set_mic_muted`. When `true`, the cpal
    /// data callback writes SILENCE (zeros) for the muted frames into `samples` — it does NOT
    /// drop them. Keeping full-length silence preserves the mic stream's wall-clock timeline so
    /// later segments stay aligned (dropping samples would shift everything after the mute and
    /// corrupt the wall-clock merge). Privacy is preserved: no real mic audio is captured while
    /// muted.
    muted: AtomicBool,
    /// Host wall-clock instant captured in the FIRST cpal data callback — the true
    /// capture-start anchor for the wall-clock merge. Anchoring here (rather than after the
    /// stream "ready" signal) drops the thread-spawn + stream-build latency from the offset.
    /// Set exactly once; later callbacks leave it untouched.
    first_frame: OnceLock<std::time::Instant>,
}

impl Shared {
    fn new() -> Self {
        Self {
            samples: Mutex::new(Vec::new()),
            peak: AtomicU32::new(0),
            muted: AtomicBool::new(false),
            first_frame: OnceLock::new(),
        }
    }

    fn store_peak(&self, value: f32) {
        self.peak.store(value.to_bits(), Ordering::Relaxed);
    }

    fn load_peak(&self) -> f32 {
        f32::from_bits(self.peak.load(Ordering::Relaxed))
    }

    fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
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
    /// Host wall-clock instant captured the moment cpal capture started, used to anchor this
    /// stream's sample-relative segment timestamps onto an absolute timeline in the wall-clock
    /// merge (see `audio::merge`). The mic (cpal) and system (ScreenCaptureKit) streams run on
    /// INDEPENDENT clocks, so sample-count alignment drifts seconds/hour — anchoring each stream
    /// to its own host start is what keeps "me" and "others" segments correctly interleaved.
    started_at: std::time::Instant,
    stop_tx: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Recorder {
    /// Open the default input device and start capturing on a dedicated thread.
    /// Non-blocking: returns once the stream is built and playing (or with the build
    /// error surfaced from the capture thread).
    pub fn start(device_name: Option<String>) -> Result<Self> {
        let shared = Arc::new(Shared::new());
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<StartInfo>>();

        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("meetnotes-audio-capture".into())
            .spawn(move || capture_thread(thread_shared, device_name, stop_rx, ready_tx))
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
            // Fallback anchor: the moment the stream reported ready. `started_at()` prefers the
            // first-frame instant captured in the data callback (tighter); this is used only if
            // no frame ever arrived (e.g. a dead device).
            started_at: std::time::Instant::now(),
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

    /// The device's native capture sample rate (Hz).
    pub fn source_sample_rate(&self) -> u32 {
        self.source_sample_rate
    }

    /// Host wall-clock instant when this stream's capture started (for the wall-clock merge).
    /// Prefers the instant captured in the FIRST data callback (true capture start); falls back
    /// to the stream-ready instant if no frame ever arrived.
    pub fn started_at(&self) -> std::time::Instant {
        self.shared
            .first_frame
            .get()
            .copied()
            .unwrap_or(self.started_at)
    }

    /// Live-toggle the mic mute mid-recording (no stream teardown). While muted the cpal data
    /// callback writes SILENCE into the buffer instead of the captured mic frames — the stream
    /// stays full-length so the timeline never shifts. Lock-free; safe to call from a command.
    pub fn set_muted(&self, muted: bool) {
        self.shared.set_muted(muted);
    }

    /// Read the current mute flag (lock-free).
    pub fn is_muted(&self) -> bool {
        self.shared.is_muted()
    }

    /// Clone up to the last `max_samples` captured mono samples WITHOUT draining — used by
    /// live transcription. Read-only; never disturbs capture or the final stop() buffer.
    pub fn snapshot_tail(&self, max_samples: usize) -> Vec<f32> {
        let Ok(guard) = self.shared.samples.lock() else {
            return Vec::new();
        };
        let start = guard.len().saturating_sub(max_samples);
        guard[start..].to_vec()
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
    device_name: Option<String>,
    stop_rx: Receiver<()>,
    ready_tx: Sender<Result<StartInfo>>,
) {
    let built = build_and_play(&shared, device_name.as_deref());
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

/// Pick the input device: the saved device by name if it's present and still available,
/// otherwise the system default. Device names are PII-adjacent — never log them.
fn select_input_device(host: &cpal::Host, name: Option<&str>) -> Result<cpal::Device> {
    if let Some(want) = name {
        if let Ok(devices) = host.input_devices() {
            for d in devices {
                if d.name().map(|n| n == want).unwrap_or(false) {
                    return Ok(d);
                }
            }
        }
        tracing::warn!(
            target: "audio",
            "saved input device unavailable; falling back to the default device"
        );
    }
    host.default_input_device()
        .ok_or_else(|| AppError::Audio("no default input device available".into()))
}

/// Build + start the input stream on the current thread, returning it plus the source
/// sample rate.
fn build_and_play(
    shared: &Arc<Shared>,
    device_name: Option<&str>,
) -> Result<(cpal::Stream, StartInfo)> {
    let host = cpal::default_host();
    let device = select_input_device(&host, device_name)?;

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
        accumulate_frames(&shared, data, channels);
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

/// Process one cpal capture buffer: downmix interleaved `data` to mono and append it to
/// `shared.samples`, tracking the peak meter — UNLESS muted, in which case append the SAME
/// number of SILENT (zero) frames and force the meter to 0.0.
///
/// Pulled out of the closure so the mute/silence behaviour is unit-testable without a device.
/// CRITICAL: the muted branch appends `frame_count` zeros (NOT zero samples) so the mic stream
/// stays exactly as long as it would have been live — that full-length silence is what keeps the
/// stream's wall-clock timeline intact for the merge.
fn accumulate_frames<T>(shared: &Arc<Shared>, data: &[T], channels: usize)
where
    T: Sample,
    f32: cpal::FromSample<T>,
{
    // Anchor the capture timeline on the first frame we ever see (set once; later frames leave
    // it). This is the true capture start the wall-clock merge anchors to (rec #7).
    if shared.first_frame.get().is_none() {
        let _ = shared.first_frame.set(std::time::Instant::now());
    }
    let channels = channels.max(1);
    let frame_count = data.len() / channels;

    if shared.is_muted() {
        shared.store_peak(0.0);
        if let Ok(mut buf) = shared.samples.lock() {
            let new_len = buf.len() + frame_count;
            buf.resize(new_len, 0.0);
        }
        return;
    }

    let mut mono = Vec::with_capacity(frame_count + 1);
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
}

/// Lightweight description of an input device for the FE device picker.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputDeviceInfo {
    pub name: String,
    pub is_default: bool,
}

/// Enumerate available input devices by name and flag the system default. Best-effort: an
/// empty list if enumeration fails. Names are surfaced only in the picker UI (never logged).
pub fn list_input_devices() -> Vec<InputDeviceInfo> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().and_then(|d| d.name().ok());
    let mut out = Vec::new();
    if let Ok(devices) = host.input_devices() {
        for d in devices {
            if let Ok(name) = d.name() {
                let is_default = default_name.as_deref() == Some(name.as_str());
                out.push(InputDeviceInfo { name, is_default });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A live (un-muted) buffer is downmixed to mono and appended; the meter tracks the peak.
    #[test]
    fn unmuted_appends_downmixed_audio() {
        let shared = Arc::new(Shared::new());
        // 2 mono frames at 1 channel.
        accumulate_frames(&shared, &[0.5f32, -0.25f32], 1);
        let buf = shared.samples.lock().unwrap();
        assert_eq!(&*buf, &[0.5f32, -0.25f32]);
        assert!((shared.load_peak() - 0.5).abs() < 1e-6);
    }

    /// MUTE: the buffer grows by the right number of SILENT frames (length preserved, content
    /// zeroed) and the meter is forced to 0 — the privacy + timeline-alignment guarantee.
    #[test]
    fn muted_writes_silence_but_keeps_length() {
        let shared = Arc::new(Shared::new());
        // Seed one real frame, then mute and feed a loud 3-frame stereo buffer (6 samples).
        accumulate_frames(&shared, &[1.0f32], 1);
        shared.set_muted(true);
        assert!(shared.is_muted());
        accumulate_frames(&shared, &[0.9f32, 0.9, -0.9, -0.9, 0.8, 0.8], 2);

        let buf = shared.samples.lock().unwrap();
        // 1 live frame + 3 muted frames (6 samples / 2 channels) = 4 samples, no dropped frames.
        assert_eq!(buf.len(), 4, "muted span must keep the stream full-length");
        assert_eq!(&buf[1..], &[0.0f32, 0.0, 0.0], "muted frames must be silence");
        assert_eq!(shared.load_peak(), 0.0, "meter reads 0 while muted");
    }

    /// The mute flag flips both ways and is observed by the helper.
    #[test]
    fn mute_flag_flips() {
        let shared = Arc::new(Shared::new());
        assert!(!shared.is_muted(), "starts unmuted");
        shared.set_muted(true);
        assert!(shared.is_muted());
        shared.set_muted(false);
        assert!(!shared.is_muted());

        // Un-muting resumes capturing real audio.
        shared.set_muted(true);
        accumulate_frames(&shared, &[0.5f32], 1);
        shared.set_muted(false);
        accumulate_frames(&shared, &[0.5f32], 1);
        let buf = shared.samples.lock().unwrap();
        assert_eq!(&*buf, &[0.0f32, 0.5f32], "silence then real audio after unmute");
    }

    /// The capture-start anchor is taken from the FIRST frame callback and set exactly once —
    /// later callbacks never move it (rec #7: tighten the merge anchor to true capture start).
    #[test]
    fn first_frame_anchor_is_set_once() {
        let shared = Arc::new(Shared::new());
        assert!(shared.first_frame.get().is_none(), "unset before any frame");
        accumulate_frames(&shared, &[0.1f32], 1);
        let first = *shared.first_frame.get().expect("set after the first frame");
        accumulate_frames(&shared, &[0.2f32], 1);
        assert_eq!(
            *shared.first_frame.get().unwrap(),
            first,
            "anchor must never move after the first frame"
        );
    }
}
