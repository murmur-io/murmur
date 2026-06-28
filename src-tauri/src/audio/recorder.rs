use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
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
    /// Hard ceiling on `samples.len()` (source-rate mono frames). `0` means "uncapped" — it
    /// stays 0 until [`build_and_play`] knows the device sample rate and sets it to
    /// `MAX_RECORDING_SECONDS * source_sample_rate`. Once the buffer reaches it the data
    /// callback drops further frames and flags `capped`, bounding RAM at ~MAX_RECORDING_SECONDS
    /// (S2: a forgotten recording otherwise grows the f32 buffer ~0.7 GB/hr → OOM).
    cap_samples: AtomicUsize,
    /// Set `true` the moment the buffer first reaches `cap_samples`. The owning capture thread
    /// polls it and tears the stream down (graceful self-stop); `Recorder::cap_reached` surfaces
    /// it so the command layer can report "recording stopped: maximum length reached".
    capped: AtomicBool,
}

impl Shared {
    fn new() -> Self {
        Self {
            samples: Mutex::new(Vec::new()),
            peak: AtomicU32::new(0),
            muted: AtomicBool::new(false),
            first_frame: OnceLock::new(),
            cap_samples: AtomicUsize::new(0),
            capped: AtomicBool::new(false),
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

    fn is_capped(&self) -> bool {
        self.capped.load(Ordering::Relaxed)
    }
}

/// Hard ceiling on a single recording's wall-clock length. Beyond this the cpal mic buffer
/// would keep growing unbounded (~0.7 GB/hr of f32 at 48 kHz), so a recording left running by
/// mistake could exhaust RAM. At the cap we stop accumulating, tear the stream down, and flush
/// what was captured normally on `stop()` (no data loss for the first `MAX_RECORDING_SECONDS`).
/// 4 hours comfortably covers any real meeting while bounding worst-case memory.
pub const MAX_RECORDING_SECONDS: u64 = 4 * 60 * 60;

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
/// This `Recorder` captures the **mic track** only: the default (or configured) input device,
/// any multi-channel input down-mixed to mono, buffered at the device's native sample rate
/// (conversion to 16 kHz happens later in [`crate::audio::wav`]). The buffer is hard-capped at
/// [`MAX_RECORDING_SECONDS`] so a forgotten recording can't grow it without bound.
///
/// System audio (the "other side" of a call) IS captured in parallel — by
/// [`crate::audio::system`] (Core Audio process tap on macOS 14.4+, else the ScreenCaptureKit
/// sidecar) into its own WAV, and optionally an echo-cancelled mic via [`crate::audio::aec`].
/// `pipeline.rs` resamples and wall-clock-merges those tracks with this one before transcription.
/// That dual-stream path is shipped; this struct owns only the cpal mic half.
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

    /// `true` once the recording hit its [`MAX_RECORDING_SECONDS`] hard cap and capture
    /// self-stopped. Lock-free; intended for the status poll so the UI can surface a
    /// "maximum recording length reached — recording stopped" notice and finalize the meeting.
    pub fn cap_reached(&self) -> bool {
        self.shared.is_capped()
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

    /// Total number of mono samples captured so far (the buffer length). During recording the
    /// buffer is never drained, so this only grows (until the S2 cap) — making it a monotonic
    /// "playhead" the MANUAL voice-command capture latches at arm time to isolate the POST-CLICK
    /// utterance. Read-only, lock-guarded; returns 0 on a poisoned lock (best-effort, never panics).
    pub fn total_samples(&self) -> usize {
        self.shared.samples.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// Clone every captured mono sample from `offset` to the current end WITHOUT draining — the
    /// growing window of audio captured SINCE `offset` was latched. Used by the manual voice-command
    /// capture to transcribe exactly what was said after the click, cleanly isolated from prior
    /// speech. If `offset` is past the current end (shouldn't happen — the buffer only grows during
    /// recording) the result is empty. Read-only; never disturbs capture or the final stop() buffer.
    pub fn snapshot_from(&self, offset: usize) -> Vec<f32> {
        let Ok(guard) = self.shared.samples.lock() else {
            return Vec::new();
        };
        let start = offset.min(guard.len());
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
            // Block until asked to stop (or the sender is dropped) — but also poll the hard cap
            // so a recording that hits MAX_RECORDING_SECONDS tears the stream down on its own
            // (graceful self-stop) instead of letting the data callback keep being invoked.
            loop {
                if shared.is_capped() {
                    tracing::warn!(
                        target: "audio",
                        "maximum recording length reached — stopping mic capture"
                    );
                    break;
                }
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

    // S2: arm the hard cap now that the device rate is known. `usize::MAX` saturation keeps it a
    // valid (effectively-unreachable) ceiling on any pathological rate instead of overflowing.
    let cap_samples = (MAX_RECORDING_SECONDS.saturating_mul(source_sample_rate as u64))
        .min(usize::MAX as u64) as usize;
    shared.cap_samples.store(cap_samples, Ordering::Relaxed);

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

    // S2 hard cap: once the buffer reaches its ceiling, stop accumulating (both the live and the
    // muted-silence paths grow it, so the check guards both) and flag `capped` so the capture
    // thread tears the stream down. Frames arriving in the ≤1-callback window before that teardown
    // are dropped — the recording is already at its maximum length. `cap == 0` ⇒ uncapped (the
    // rate isn't known yet, or a direct unit-test call).
    let cap = shared.cap_samples.load(Ordering::Relaxed);
    if cap > 0 {
        let len = shared.samples.lock().map(|b| b.len()).unwrap_or(usize::MAX);
        if len >= cap {
            shared.capped.store(true, Ordering::Relaxed);
            shared.store_peak(0.0);
            return;
        }
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

    /// S2 hard cap: once the buffer reaches `cap_samples` the accumulator stops growing it and
    /// flags `capped` — the OOM guard for a forgotten/very-long recording. Frames captured before
    /// the cap are preserved (flushed on `stop()`); frames after are dropped.
    #[test]
    fn buffer_stops_growing_at_the_cap() {
        let shared = Arc::new(Shared::new());
        shared.cap_samples.store(3, Ordering::Relaxed); // tiny cap for the test
        assert!(!shared.is_capped(), "uncapped before any frame");

        // Fill exactly to the cap: cap check sees len 0 < 3, so all 3 frames are appended.
        accumulate_frames(&shared, &[0.1f32, 0.2, 0.3], 1);
        assert_eq!(shared.samples.lock().unwrap().len(), 3);
        assert!(!shared.is_capped(), "at-cap but no over-cap callback yet");

        // Next callback: len(3) >= cap(3) → drop the frames and latch `capped`.
        accumulate_frames(&shared, &[0.4f32, 0.5, 0.6, 0.7], 1);
        assert_eq!(
            shared.samples.lock().unwrap().len(),
            3,
            "buffer must not grow past the cap"
        );
        assert!(shared.is_capped(), "cap-reached flag latched");
        assert_eq!(shared.load_peak(), 0.0, "meter reads 0 once capped");

        // A muted callback past the cap must also be dropped (no silent-frame growth either).
        shared.set_muted(true);
        accumulate_frames(&shared, &[0.0f32, 0.0], 1);
        assert_eq!(
            shared.samples.lock().unwrap().len(),
            3,
            "muted frames must not grow the buffer past the cap"
        );
    }

    /// A zero `cap_samples` (the default until the device rate is known) means uncapped — direct
    /// unit-test calls and the pre-`build_and_play` window keep today's behaviour.
    #[test]
    fn zero_cap_is_uncapped() {
        let shared = Arc::new(Shared::new());
        assert_eq!(shared.cap_samples.load(Ordering::Relaxed), 0);
        for _ in 0..1000 {
            accumulate_frames(&shared, &[0.5f32], 1);
        }
        assert_eq!(shared.samples.lock().unwrap().len(), 1000, "no cap applied at 0");
        assert!(!shared.is_capped());
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
