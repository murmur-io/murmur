use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock, TryLockError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat, StreamConfig};

use crate::error::{AppError, Result};

#[cfg(test)]
type RecorderRaceHook = (
    std::thread::ThreadId,
    Arc<std::sync::Barrier>,
    Arc<std::sync::Barrier>,
);

#[cfg(test)]
static CALLBACK_RACE_HOOK: Mutex<Option<RecorderRaceHook>> = Mutex::new(None);

#[cfg(test)]
static READER_RACE_HOOK: Mutex<Option<RecorderRaceHook>> = Mutex::new(None);

/// Serializes the two tests that drive [`READER_RACE_HOOK`], which is ONE global slot shared by
/// both while libtest runs them on parallel threads.
///
/// Each test publishes `(its own reader thread id, arrived, resume)` into that slot and then blocks
/// on a two-party barrier until its reader trips it. Run concurrently, the second writer overwrites
/// the first's entry, so the first test's reader finds an id that is not its own, sails past without
/// tripping anything, and its `arrived.wait()` waits on a barrier no one else will ever reach — a
/// permanent hang, not a failure: the run prints every test as passing and then never terminates.
/// The clear-to-`None` at the end of each test is a second way to lose the other's entry.
///
/// Observed twice in a row on a loaded machine, and passing on the same commit when the machine was
/// quiet, which is exactly the shape that gets re-run rather than fixed. The mutex is poison-
/// tolerant because a panic in one of these tests must surface as that test's own failure, not as a
/// second, misleading failure in the other.
#[cfg(test)]
static READER_RACE_HOOK_SERIAL: Mutex<()> = Mutex::new(());

#[cfg(test)]
fn callback_race_hook() {
    let hook = CALLBACK_RACE_HOOK
        .lock()
        .ok()
        .and_then(|guard| guard.clone());
    if let Some((thread_id, arrived, resume)) = hook {
        if thread_id == std::thread::current().id() {
            arrived.wait();
            resume.wait();
        }
    }
}

#[cfg(test)]
fn reader_race_hook() {
    let hook = READER_RACE_HOOK.lock().ok().and_then(|guard| guard.clone());
    if let Some((thread_id, arrived, resume)) = hook {
        if thread_id == std::thread::current().id() {
            arrived.wait();
            resume.wait();
        }
    }
}

/// Maximum native-rate mono audio retained by the recorder itself. Older frames may be removed
/// only after a non-real-time owner proves the exact prefix is durable via
/// [`CheckpointWriter::checkpoint_trim`]. This is a byte bound, independent of device sample rate and
/// the four-hour wall-clock limit.
// Fixed capture + live-caption history. Manual commands read their exact range from the certified
// spool and therefore do not inflate this realtime ring.
pub const RESIDENT_RECORDING_BUFFER_BYTES: usize = 32 * 1024 * 1024;
pub const RESIDENT_RECORDING_BUFFER_SAMPLES: usize =
    RESIDENT_RECORDING_BUFFER_BYTES / std::mem::size_of::<AtomicU32>();
const VOICE_LISTENER_BUFFER_BYTES: usize = 8 * 1024 * 1024;
const VOICE_LISTENER_BUFFER_SAMPLES: usize =
    VOICE_LISTENER_BUFFER_BYTES / std::mem::size_of::<AtomicU32>();

/// Hard wall-clock ceiling for a single recording at every supported source rate.
pub const MAX_RECORDING_SECONDS: u64 = 4 * 60 * 60;
/// The fixed 32 MiB ring's 14 s history + 7 s spool headroom is proven through this native rate.
/// Reject exotic virtual devices above it before any capture artifact or stream is activated.
pub const MAX_SUPPORTED_CAPTURE_RATE_HZ: u32 = 384_000;
const MIN_SUPPORTED_CAPTURE_RATE_HZ: u32 = 8_000;

const CAPTURE_CONTROL_TIMEOUT: Duration = Duration::from_secs(2);
const CAPTURE_PREPARE_TIMEOUT: Duration = Duration::from_secs(8);
// Preserve the full caption window. At the supported maximum 384 kHz, 32 MiB still leaves ~7.8 s
// of checkpoint/fsync headroom beyond this 14 s retained tail.
const LIVE_HISTORY_RETENTION_SECONDS: usize = 14;
const SPOOL_HEADROOM_SECONDS: usize = 7;
const MANUAL_CAPTURE_MAX_SECONDS: usize = 60;
const READER_COPY_RETRIES: usize = 128;
// A tail may be the full 14 s / 21.5 MiB window at 384 kHz. Keep retries deliberately tiny so a
// rapidly advancing durable trim cannot turn one live-caption tick into hundreds of large copies.
const TAIL_COPY_RETRIES: usize = 3;
const ACTIVE_CONTROL_POLL: Duration = Duration::from_millis(25);
static NEXT_CHECKPOINT_AUTHORITY: AtomicU64 = AtomicU64::new(1);

/// Terminal capture failures latched by the real-time callback. The first fault wins and capture
/// is stopped; continuing after dropping an unknown callback would create a silent timeline gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CaptureFault {
    BufferLockContended = 1,
    ResidentCapacityExhausted = 2,
    InvalidInterleavedInput = 3,
    FrameCounterOverflow = 4,
    StreamError = 5,
    CheckpointAuthorityLost = 6,
    CaptureThreadFailed = 7,
}

impl CaptureFault {
    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::BufferLockContended),
            2 => Some(Self::ResidentCapacityExhausted),
            3 => Some(Self::InvalidInterleavedInput),
            4 => Some(Self::FrameCounterOverflow),
            5 => Some(Self::StreamError),
            6 => Some(Self::CheckpointAuthorityLost),
            7 => Some(Self::CaptureThreadFailed),
            _ => None,
        }
    }
}

/// A fixed-allocation circular window. `base_frame..end_frame` are absolute source-frame
/// offsets and at most `capacity` frames are resident. Atomic slots let the realtime callback append
/// without locking. A release/acquire fence pair links reused-slot observations back to the durable
/// trim that authorized them, so a reader rejects any copy crossed by reuse.
struct SampleWindow {
    storage: Box<[AtomicU32]>,
    base_frame: AtomicUsize,
    end_frame: AtomicUsize,
    /// Even while stable, odd while the sole realtime callback owns its append. This detects an
    /// unexpected concurrent/re-entrant callback; readers do not couple to it because append-only
    /// writes beyond their fixed `end` snapshot are harmless. Slot reuse is detected by the fenced
    /// monotonic `base_frame` protocol in `copy_absolute`/`accumulate_frames`.
    generation: AtomicUsize,
}

/// Short atomic epoch held by the realtime callback. Only callbacks write `generation`; a
/// preempted checkpoint therefore cannot strand it odd and create priority inversion. Drop always
/// restores an even generation on every early-return path.
struct CallbackEpoch<'a> {
    window: &'a SampleWindow,
    generation: usize,
}

impl Drop for CallbackEpoch<'_> {
    fn drop(&mut self) {
        self.window
            .generation
            .store(self.generation.wrapping_add(2), Ordering::Release);
    }
}

impl SampleWindow {
    fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            storage: (0..capacity)
                .map(|_| AtomicU32::new(0.0f32.to_bits()))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            base_frame: AtomicUsize::new(0),
            end_frame: AtomicUsize::new(0),
            generation: AtomicUsize::new(0),
        }
    }

    fn capacity(&self) -> usize {
        self.storage.len()
    }

    fn bounds(&self) -> (usize, usize) {
        (
            self.base_frame.load(Ordering::Acquire),
            self.end_frame.load(Ordering::Acquire),
        )
    }

    fn available(&self, base_frame: usize, end_frame: usize) -> usize {
        self.capacity()
            .saturating_sub(end_frame.saturating_sub(base_frame))
    }

    fn store(&self, absolute_frame: usize, sample: f32) {
        let index = absolute_frame % self.capacity();
        self.storage[index].store(sample.to_bits(), Ordering::Relaxed);
    }

    fn copy_absolute(&self, start: usize, max_samples: usize) -> Option<Vec<f32>> {
        let base_before = self.base_frame.load(Ordering::Acquire);
        let end = self.end_frame.load(Ordering::Acquire);
        if start < base_before || start > end {
            return None;
        }
        let count = max_samples.min(end - start);
        let mut out = Vec::with_capacity(count);
        for absolute_frame in start..start + count {
            let index = absolute_frame % self.capacity();
            out.push(f32::from_bits(self.storage[index].load(Ordering::Relaxed)));
            #[cfg(test)]
            if absolute_frame == start {
                reader_race_hook();
            }
        }

        // If any Relaxed slot load above observed a slot reused by the callback, this Acquire fence
        // synchronizes with the callback's Release fence before that reused-slot store. The callback
        // acquired the durable trim first, so the trailing base load must observe that trim (or a
        // later one) and reject this copy. Appends into capacity that was already free do not reuse
        // a resident slot and are harmless beyond our fixed `end` snapshot.
        std::sync::atomic::fence(Ordering::Acquire);
        let base_after = self.base_frame.load(Ordering::Acquire);
        (base_after <= start).then_some(out)
    }

    /// Copy the newest bounded window. `None` means the durable checkpoint trim crossed every
    /// bounded retry; callers must retry/fail-open, never reinterpret it as an honestly empty
    /// signal. `Some(empty)` is reserved for a genuinely empty resident window.
    fn copy_tail(&self, max_samples: usize) -> Option<Vec<f32>> {
        for _ in 0..3 {
            let (base, end) = self.bounds();
            let count = max_samples.min(end.saturating_sub(base));
            if let Some(samples) = self.copy_absolute(end - count, count) {
                return Some(samples);
            }
        }
        None
    }

    fn checkpoint_trim(
        &self,
        expected_base: usize,
        durable_end: usize,
        retain_frames: usize,
    ) -> Result<usize> {
        let observed_end = self.end_frame.load(Ordering::Acquire);
        if durable_end < expected_base || durable_end > observed_end {
            return Err(AppError::Audio(format!(
                "sample checkpoint end {durable_end} outside resident window {expected_base}..{observed_end}"
            )));
        }
        // Keep a bounded rolling history even though the prefix is durable. This feeds live
        // captions and an armed manual command without letting multi-hour capture grow RAM.
        let trim_to = durable_end.saturating_sub(retain_frames).max(expected_base);
        if trim_to == expected_base {
            return Ok(expected_base);
        }
        self.base_frame
            .compare_exchange(expected_base, trim_to, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| trim_to)
            .map_err(|current| {
                AppError::Audio(format!(
                    "sample checkpoint base changed: expected {expected_base}, current {current}"
                ))
            })
    }

    fn try_callback_epoch(&self) -> Option<CallbackEpoch<'_>> {
        // cpal promises one mutable callback stream. A failed single CAS therefore means an
        // unexpected concurrent/re-entrant callback, not ordinary checkpoint contention.
        let generation = self.generation.load(Ordering::Acquire);
        if generation % 2 == 0
            && self
                .generation
                .compare_exchange(
                    generation,
                    generation.wrapping_add(1),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        {
            Some(CallbackEpoch {
                window: self,
                generation,
            })
        } else {
            None
        }
    }
}

/// State shared with the cpal callbacks. The callback only uses atomics; it never allocates, waits
/// on a mutex, logs, or performs IO.
struct Shared {
    samples: SampleWindow,
    peak: AtomicU32,
    muted: AtomicBool,
    first_frame: OnceLock<Instant>,
    wall_cap_frames: AtomicUsize,
    retention_frames: AtomicUsize,
    capped: AtomicBool,
    fault: AtomicU8,
    stop_requested: AtomicBool,
    checkpoint_authority_id: u64,
    checkpoint_authority_live: AtomicBool,
    capture_active: AtomicBool,
}

impl Shared {
    fn with_capacity(capacity: usize) -> Self {
        let checkpoint_authority_id = NEXT_CHECKPOINT_AUTHORITY.fetch_add(1, Ordering::Relaxed);
        Self {
            samples: SampleWindow::with_capacity(capacity),
            peak: AtomicU32::new(0),
            muted: AtomicBool::new(false),
            first_frame: OnceLock::new(),
            wall_cap_frames: AtomicUsize::new(0),
            retention_frames: AtomicUsize::new(0),
            capped: AtomicBool::new(false),
            fault: AtomicU8::new(0),
            stop_requested: AtomicBool::new(false),
            checkpoint_authority_id,
            checkpoint_authority_live: AtomicBool::new(false),
            capture_active: AtomicBool::new(false),
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
        self.capped.load(Ordering::Acquire)
    }

    fn latch_cap(&self) {
        self.capped.store(true, Ordering::Release);
        self.stop_requested.store(true, Ordering::Release);
        self.store_peak(0.0);
    }

    fn latch_fault(&self, fault: CaptureFault) {
        let _ = self
            .fault
            .compare_exchange(0, fault as u8, Ordering::AcqRel, Ordering::Acquire);
        self.stop_requested.store(true, Ordering::Release);
        self.store_peak(0.0);
    }

    fn fault(&self) -> Option<CaptureFault> {
        CaptureFault::from_code(self.fault.load(Ordering::Acquire))
    }

    fn should_stop(&self) -> bool {
        self.stop_requested.load(Ordering::Acquire)
    }
}

mod durable_checkpoint_seal {
    pub trait Sealed {}
}

/// Affine evidence that a precise source-frame prefix is durably stored and identity/hash verified.
///
/// The trait is sealed and the only implementation's fields/constructor are private. This core
/// intentionally exposes no production proof factory yet: the spill integration must add a
/// verifier-backed constructor in this module before it can trim a single frame. Numeric offsets
/// and cloneable [`SampleReader`] handles can never authorize destruction.
// The private supertrait is intentional Rust sealing, not an accidentally unreachable public
// bound. Keep the exception on this single declaration so `clippy -D warnings` remains strict.
#[allow(private_bounds)]
pub trait VerifiedDurableCheckpoint: durable_checkpoint_seal::Sealed {
    fn authority_id(&self) -> u64;
    fn expected_base(&self) -> usize;
    fn durable_end(&self) -> usize;
}

pub struct DurableCheckpointProof {
    authority_id: u64,
    expected_base: usize,
    durable_end: usize,
}

impl durable_checkpoint_seal::Sealed for DurableCheckpointProof {}

impl VerifiedDurableCheckpoint for DurableCheckpointProof {
    fn authority_id(&self) -> u64 {
        self.authority_id
    }

    fn expected_base(&self) -> usize {
        self.expected_base
    }

    fn durable_end(&self) -> usize {
        self.durable_end
    }
}

/// The single non-cloneable destructive authority for one recorder generation.
pub struct CheckpointWriter {
    shared: Arc<Shared>,
    authority_id: u64,
}

impl CheckpointWriter {
    pub fn resident_bounds(&self) -> (usize, usize) {
        self.shared.samples.bounds()
    }

    /// Consume verified durable evidence and trim exactly that proven prefix. A proof from another
    /// recorder generation, a stale base, or a future end fails without removing frames.
    pub fn checkpoint_trim<P>(&mut self, proof: P) -> Result<usize>
    where
        P: VerifiedDurableCheckpoint,
    {
        if proof.authority_id() != self.authority_id {
            return Err(AppError::Audio(
                "durable checkpoint proof belongs to another recorder generation".into(),
            ));
        }
        self.shared.samples.checkpoint_trim(
            proof.expected_base(),
            proof.durable_end(),
            self.shared.retention_frames.load(Ordering::Acquire),
        )
    }

    /// The production destructive seam: only storage evidence minted from a stable, fsynced mic
    /// handle can advance the resident base. The sole affine writer snapshots the current base,
    /// and the ring CAS re-checks it while preserving the configured rolling live-history tail.
    pub(crate) fn checkpoint_trim_verified(
        &mut self,
        proof: &crate::storage::recording_store::VerifiedMicCheckpoint,
    ) -> Result<usize> {
        let durable_end = usize::try_from(proof.durable_frames()).map_err(|_| {
            AppError::Audio("durable mic checkpoint exceeds platform offsets".into())
        })?;
        let expected_base = self.shared.samples.base_frame.load(Ordering::Acquire);
        self.checkpoint_trim(DurableCheckpointProof {
            authority_id: self.authority_id,
            expected_base,
            durable_end,
        })
    }

    #[cfg(test)]
    fn verified_proof_for_test(
        &self,
        expected_base: usize,
        durable_end: usize,
    ) -> DurableCheckpointProof {
        DurableCheckpointProof {
            authority_id: self.authority_id,
            expected_base,
            durable_end,
        }
    }
}

impl Drop for CheckpointWriter {
    fn drop(&mut self) {
        self.shared
            .checkpoint_authority_live
            .store(false, Ordering::Release);
        if self.shared.capture_active.load(Ordering::Acquire) {
            self.shared
                .latch_fault(CaptureFault::CheckpointAuthorityLost);
        }
    }
}

/// Source-rate frame ceiling for the four-hour wall-clock contract.
pub fn recording_sample_cap(source_sample_rate: u32) -> usize {
    MAX_RECORDING_SECONDS
        .saturating_mul(source_sample_rate as u64)
        .min(usize::MAX as u64) as usize
}

fn live_history_retention_frames(source_sample_rate: u32, capacity: usize) -> usize {
    let rate = source_sample_rate as usize;
    let desired = rate.saturating_mul(LIVE_HISTORY_RETENTION_SECONDS);
    let headroom = rate.saturating_mul(SPOOL_HEADROOM_SECONDS);
    desired.min(capacity.saturating_sub(headroom))
}

/// Pure rising-edge decision for the maximum-length notice.
pub fn should_emit_cap_notice(capped: bool, already_emitted: bool) -> bool {
    capped && !already_emitted
}

struct StartInfo {
    source_sample_rate: u32,
}

enum ControlMessage {
    Activate(Sender<Result<()>>),
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlState {
    Prepared,
    Active,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlAction {
    Activate,
    Stop,
}

fn control_transition(state: ControlState, action: ControlAction) -> Option<ControlState> {
    match (state, action) {
        (ControlState::Prepared, ControlAction::Activate) => Some(ControlState::Active),
        (ControlState::Prepared | ControlState::Active, ControlAction::Stop) => {
            Some(ControlState::Stopping)
        }
        _ => None,
    }
}

struct DoneSignal(Option<Sender<()>>);

impl Drop for DoneSignal {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

/// Send-side ownership of the dedicated cpal thread. A stop waits for at most
/// [`CAPTURE_CONTROL_TIMEOUT`]; timeout detaches rather than risking an unbounded app shutdown.
struct CaptureThreadOwner {
    control_tx: Sender<ControlMessage>,
    done_rx: Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureStopStatus {
    Stopped,
    TimedOut,
    ThreadFailed,
}

impl CaptureThreadOwner {
    fn stop_with_timeout(&mut self, timeout: Duration) -> CaptureStopStatus {
        if self.thread.is_none() {
            return CaptureStopStatus::Stopped;
        }
        let _ = self.control_tx.send(ControlMessage::Stop);

        match self.done_rx.recv_timeout(timeout) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                let Some(handle) = self.thread.take() else {
                    return CaptureStopStatus::Stopped;
                };
                if handle.join().is_ok() {
                    CaptureStopStatus::Stopped
                } else {
                    CaptureStopStatus::ThreadFailed
                }
            }
            Err(RecvTimeoutError::Timeout) => CaptureStopStatus::TimedOut,
        }
    }

    fn stop_bounded(&mut self) -> CaptureStopStatus {
        self.stop_with_timeout(CAPTURE_CONTROL_TIMEOUT)
    }
}

impl Drop for CaptureThreadOwner {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.stop_bounded();
        }
    }
}

/// A microphone stream which has been built on its dedicated thread but has not been played.
/// Callers may establish durable spill state before [`PreparedRecorder::activate`] makes the
/// first frame capturable.
pub struct PreparedRecorder {
    shared: Arc<Shared>,
    source_sample_rate: u32,
    capture: Option<CaptureThreadOwner>,
    checkpoint_writer: Option<CheckpointWriter>,
}

impl PreparedRecorder {
    /// Build the cpal input stream and return only after the capture thread confirms it is ready.
    /// The stream remains paused until [`Self::activate`] receives a successful play ack.
    pub fn prepare(device_name: Option<String>) -> Result<Self> {
        Self::prepare_with_capacity(device_name, RESIDENT_RECORDING_BUFFER_SAMPLES)
    }

    fn prepare_with_capacity(device_name: Option<String>, capacity: usize) -> Result<Self> {
        let shared = Arc::new(Shared::with_capacity(capacity));
        let (control_tx, control_rx) = mpsc::channel::<ControlMessage>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<StartInfo>>();
        let (done_tx, done_rx) = mpsc::channel::<()>();

        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("meetnotes-audio-capture".into())
            .spawn(move || {
                let _done = DoneSignal(Some(done_tx));
                capture_thread(thread_shared, device_name, control_rx, ready_tx);
            })
            .map_err(|e| AppError::Audio(format!("failed to spawn capture thread: {e}")))?;

        let capture = CaptureThreadOwner {
            control_tx,
            done_rx,
            thread: Some(thread),
        };

        let info = match ready_rx.recv_timeout(CAPTURE_PREPARE_TIMEOUT) {
            Ok(Ok(info)) => info,
            Ok(Err(error)) => {
                return Err(error);
            }
            Err(RecvTimeoutError::Timeout) => {
                return Err(AppError::Audio(
                    "capture stream preparation did not complete before deadline".into(),
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(AppError::Audio(
                    "capture thread exited before reporting readiness".into(),
                ));
            }
        };

        let checkpoint_writer = CheckpointWriter {
            shared: shared.clone(),
            authority_id: shared.checkpoint_authority_id,
        };
        Ok(Self {
            shared,
            source_sample_rate: info.source_sample_rate,
            capture: Some(capture),
            checkpoint_writer: Some(checkpoint_writer),
        })
    }

    /// Transfer the sole destructive checkpoint authority to the durable spill owner. Activation
    /// fails closed until this authority is held outside the recorder.
    pub fn take_checkpoint_writer(&mut self) -> Result<CheckpointWriter> {
        let writer = self
            .checkpoint_writer
            .take()
            .ok_or_else(|| AppError::Audio("checkpoint writer was already transferred".into()))?;
        self.shared
            .checkpoint_authority_live
            .store(true, Ordering::Release);
        Ok(writer)
    }

    /// Play the already-built stream. Success is returned only after the capture thread has called
    /// `Stream::play` and acknowledged it.
    pub fn activate(mut self) -> Result<Recorder> {
        if !self
            .shared
            .checkpoint_authority_live
            .load(Ordering::Acquire)
        {
            return Err(AppError::Audio(
                "mic capture requires a retained durable checkpoint writer before activation"
                    .into(),
            ));
        }
        let (ack_tx, ack_rx) = mpsc::channel::<Result<()>>();
        let capture = self
            .capture
            .as_ref()
            .ok_or_else(|| AppError::Audio("prepared recorder has no capture thread".into()))?;
        capture
            .control_tx
            .send(ControlMessage::Activate(ack_tx))
            .map_err(|_| AppError::Audio("capture thread exited before activation".into()))?;

        match ack_rx.recv_timeout(CAPTURE_CONTROL_TIMEOUT) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(error),
            Err(RecvTimeoutError::Timeout) => {
                return Err(AppError::Audio(
                    "capture activation did not complete before deadline".into(),
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(AppError::Audio(
                    "capture thread exited before activation acknowledgement".into(),
                ))
            }
        }

        let capture = self
            .capture
            .take()
            .ok_or_else(|| AppError::Audio("prepared recorder capture ownership lost".into()))?;
        Ok(Recorder {
            shared: self.shared.clone(),
            source_sample_rate: self.source_sample_rate,
            started_at: Instant::now(),
            capture: Mutex::new(Some(capture)),
        })
    }

    pub fn source_sample_rate(&self) -> u32 {
        self.source_sample_rate
    }

    pub fn set_muted(&self, muted: bool) {
        self.shared.set_muted(muted);
    }

    pub fn is_muted(&self) -> bool {
        self.shared.is_muted()
    }

    pub fn level(&self) -> f32 {
        self.shared.load_peak()
    }

    pub fn fault(&self) -> Option<CaptureFault> {
        self.shared.fault()
    }

    pub fn sample_reader(&self) -> SampleReader {
        SampleReader {
            shared: self.shared.clone(),
        }
    }
}

/// Owns an active microphone capture stream confined to its dedicated OS thread.
pub struct Recorder {
    shared: Arc<Shared>,
    source_sample_rate: u32,
    started_at: Instant,
    capture: Mutex<Option<CaptureThreadOwner>>,
}

/// Retryable result of a bounded capture stop. Terminal callback faults are never reported as a
/// successful complete recording, and a trimmed window explicitly requires durable assembly.
#[derive(Debug)]
pub enum RecorderStopOutcome {
    Pending,
    Complete {
        samples: Vec<f32>,
        sample_rate: u32,
    },
    Faulted {
        captured_prefix: Vec<f32>,
        sample_rate: u32,
        fault: CaptureFault,
    },
    RequiresDurableAssembly {
        resident_samples: Vec<f32>,
        resident_base: usize,
        captured_end: usize,
        sample_rate: u32,
        fault: Option<CaptureFault>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableRecorderStopOutcome {
    Pending,
    Stopped { fault: Option<CaptureFault> },
}

impl Recorder {
    /// Build a paused stream for callers that must establish durable state before capture starts.
    pub fn prepare(device_name: Option<String>) -> Result<PreparedRecorder> {
        PreparedRecorder::prepare(device_name)
    }

    /// The standby wake listener captures only ~2.2 s and recreates the stream each window. It
    /// must not allocate/zero the meeting recorder's 32 MiB history ring every iteration.
    pub(crate) fn prepare_voice_listener() -> Result<PreparedRecorder> {
        PreparedRecorder::prepare_with_capacity(None, VOICE_LISTENER_BUFFER_SAMPLES)
    }

    /// Fail-closed legacy seam. The integration must prepare, transfer the affine checkpoint
    /// writer to durable spill, and only then activate.
    pub fn start(_device_name: Option<String>) -> Result<Self> {
        Err(AppError::Audio(
            "Recorder::start is disabled until the durable spill owns a CheckpointWriter; use Recorder::prepare"
                .into(),
        ))
    }

    fn try_stop_capture(&self, timeout: Duration) -> CaptureStopStatus {
        let mut capture_slot = match self.capture.try_lock() {
            Ok(slot) => slot,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return CaptureStopStatus::TimedOut,
        };
        let Some(capture) = capture_slot.as_mut() else {
            return CaptureStopStatus::Stopped;
        };
        let status = capture.stop_with_timeout(timeout);
        if status != CaptureStopStatus::TimedOut {
            *capture_slot = None;
        }
        if status == CaptureStopStatus::ThreadFailed {
            self.shared.latch_fault(CaptureFault::CaptureThreadFailed);
        }
        status
    }

    /// Attempt a bounded stop without consuming the recorder. `Pending` retains the thread owner,
    /// fixed ring, and checkpoint recovery handles so the caller can retry.
    pub fn try_stop(&self) -> Result<RecorderStopOutcome> {
        self.try_stop_with_timeout(CAPTURE_CONTROL_TIMEOUT)
    }

    /// Meeting capture is assembled from the durable spool, so Stop needs only proof that the cpal
    /// owner settled plus its fault—not a duplicate Vec of the retained live-history window.
    pub(crate) fn try_stop_for_durable_assembly(&self) -> DurableRecorderStopOutcome {
        if self.try_stop_capture(CAPTURE_CONTROL_TIMEOUT) == CaptureStopStatus::TimedOut {
            DurableRecorderStopOutcome::Pending
        } else {
            DurableRecorderStopOutcome::Stopped {
                fault: self.fault(),
            }
        }
    }

    fn try_stop_with_timeout(&self, timeout: Duration) -> Result<RecorderStopOutcome> {
        if self.try_stop_capture(timeout) == CaptureStopStatus::TimedOut {
            return Ok(RecorderStopOutcome::Pending);
        }

        let (base_frame, end_frame) = self.shared.samples.bounds();
        let samples = self
            .shared
            .samples
            .copy_absolute(base_frame, end_frame.saturating_sub(base_frame))
            .ok_or_else(|| {
                AppError::Audio(
                    "resident sample window changed while materializing bounded Stop".into(),
                )
            })?;
        let fault = self.fault();

        if base_frame != 0 {
            return Ok(RecorderStopOutcome::RequiresDurableAssembly {
                resident_samples: samples,
                resident_base: base_frame,
                captured_end: end_frame,
                sample_rate: self.source_sample_rate,
                fault,
            });
        }
        if let Some(fault) = fault {
            return Ok(RecorderStopOutcome::Faulted {
                captured_prefix: samples,
                sample_rate: self.source_sample_rate,
                fault,
            });
        }

        tracing::info!(
            target: "audio",
            frames = samples.len(),
            sample_rate = self.source_sample_rate,
            "stopped mic capture"
        );
        Ok(RecorderStopOutcome::Complete {
            samples,
            sample_rate: self.source_sample_rate,
        })
    }

    /// Legacy compatibility view. It is non-consuming, never treats a terminal fault or trimmed
    /// recording as success, and directs durable callers to [`Self::try_stop`].
    pub fn stop(&self) -> Result<(Vec<f32>, u32)> {
        match self.try_stop()? {
            RecorderStopOutcome::Complete {
                samples,
                sample_rate,
            } => Ok((samples, sample_rate)),
            RecorderStopOutcome::Pending => Err(AppError::Audio(
                "capture stop is still pending; recorder ownership was retained for retry".into(),
            )),
            RecorderStopOutcome::Faulted { fault, .. } => Err(AppError::Audio(format!(
                "mic capture ended with {fault:?}; partial frames remain available through try_stop"
            ))),
            RecorderStopOutcome::RequiresDurableAssembly {
                resident_base,
                fault,
                ..
            } => Err(AppError::Audio(format!(
                "mic capture requires durable assembly from frame {resident_base}; fault={fault:?}"
            ))),
        }
    }

    pub fn level(&self) -> f32 {
        self.shared.load_peak()
    }

    pub fn source_sample_rate(&self) -> u32 {
        self.source_sample_rate
    }

    pub fn started_at(&self) -> Instant {
        self.shared
            .first_frame
            .get()
            .copied()
            .unwrap_or(self.started_at)
    }

    pub fn set_muted(&self, muted: bool) {
        self.shared.set_muted(muted);
    }

    pub fn is_muted(&self) -> bool {
        self.shared.is_muted()
    }

    pub fn cap_reached(&self) -> bool {
        self.shared.is_capped()
    }

    pub fn fault(&self) -> Option<CaptureFault> {
        self.shared.fault()
    }

    /// Clone the newest resident samples. The returned offset-independent tail keeps the legacy
    /// live-transcription semantics after durable prefix trimming.
    /// `None` is a bounded trim-race retry signal, not silence. See [`SampleWindow::copy_tail`].
    pub fn snapshot_tail(&self, max_samples: usize) -> Option<Vec<f32>> {
        self.shared.samples.copy_tail(max_samples)
    }

    /// Absolute number of accepted source frames, independent of resident-prefix trimming.
    pub fn total_samples(&self) -> usize {
        self.shared.samples.end_frame.load(Ordering::Acquire)
    }

    /// Absolute source-frame bounds for one manual voice command. The hard 60 s cap is independent
    /// of the thermal tick and ASR success; the exact range is later streamed from the certified
    /// spool, not pinned in the realtime ring.
    pub fn manual_capture_bounds(&self) -> Option<(usize, usize)> {
        let rate = self.source_sample_rate as usize;
        let frames = rate.saturating_mul(MANUAL_CAPTURE_MAX_SECONDS);
        if rate == 0 {
            return None;
        }
        let start = self.total_samples();
        start.checked_add(frames).map(|end| (start, end))
    }

    /// Legacy absolute snapshot. A request for already-trimmed history returns empty rather than
    /// silently moving forward and fabricating a contiguous result.
    pub fn snapshot_from(&self, offset: usize) -> Vec<f32> {
        let (base, end) = self.shared.samples.bounds();
        if offset < base {
            return Vec::new();
        }
        self.shared
            .samples
            .copy_absolute(offset.min(end), usize::MAX)
            .unwrap_or_default()
    }

    pub fn sample_reader(&self) -> SampleReader {
        SampleReader {
            shared: self.shared.clone(),
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let capture_slot = match self.capture.get_mut() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(capture) = capture_slot.as_mut() else {
            return;
        };
        let status = capture.stop_bounded();
        if status == CaptureStopStatus::ThreadFailed {
            self.shared.latch_fault(CaptureFault::CaptureThreadFailed);
        }
        if status == CaptureStopStatus::TimedOut {
            // Recorder ownership is itself being dropped. Detach the still-owned thread only after
            // the bounded stop attempt; its Arc keeps the ring alive until the capture function
            // actually exits, while avoiding a second blocking wait from CaptureThreadOwner::drop.
            let _ = capture.thread.take();
        } else {
            *capture_slot = None;
        }
    }
}

/// Cloneable non-real-time snapshot-only view of the fixed resident sample window. It intentionally
/// carries no destructive checkpoint capability.
#[derive(Clone)]
pub struct SampleReader {
    shared: Arc<Shared>,
}

/// One internally consistent resident-tail snapshot with absolute source-frame bounds. The bounds
/// let incremental readers prove that a checkpoint trim did not create an unobserved gap.
#[derive(Debug)]
pub struct SampleTailSnapshot {
    pub samples: Vec<f32>,
    pub start_frame: usize,
    pub end_frame: usize,
}

impl SampleReader {
    /// Absolute number of accepted source frames, independent of resident-prefix trimming.
    pub fn total_samples(&self) -> usize {
        self.shared.samples.end_frame.load(Ordering::Acquire)
    }

    /// Copy the newest bounded resident window without holding the recorder ownership mutex.
    ///
    /// A checkpoint may trim/reuse slots while the copy runs. In that case the atomic window
    /// rejects the mixed generation and this method retries from fresh bounds. Exhausting the
    /// bounded retry budget is an error, never an empty/silent signal.
    pub fn snapshot_tail(&self, max_samples: usize) -> Result<SampleTailSnapshot> {
        for attempt in 0..TAIL_COPY_RETRIES {
            let (base, end) = self.shared.samples.bounds();
            let count = max_samples.min(end.saturating_sub(base));
            let start = end.saturating_sub(count);
            if let Some(samples) = self.shared.samples.copy_absolute(start, count) {
                let copied_end = start.saturating_add(samples.len());
                return Ok(SampleTailSnapshot {
                    samples,
                    start_frame: start,
                    end_frame: copied_end,
                });
            }
            if attempt < 16 {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
        Err(AppError::Audio(
            "resident tail copy remained contended beyond its bounded retry budget".into(),
        ))
    }

    /// Legacy absolute read. Returns empty for an offset older than the resident base, so a caller
    /// cannot accidentally append a later suffix after losing a gap.
    pub fn snapshot_from(&self, offset: usize) -> Vec<f32> {
        let (base, end) = self.shared.samples.bounds();
        if offset < base {
            return Vec::new();
        }
        self.shared
            .samples
            .copy_absolute(offset.min(end), usize::MAX)
            .unwrap_or_default()
    }

    /// Fallible exact absolute read for a durable spill/archive writer.
    pub fn read_absolute(&self, offset: usize, max_samples: usize) -> Result<Vec<f32>> {
        for attempt in 0..READER_COPY_RETRIES {
            let (base, end) = self.shared.samples.bounds();
            if offset < base {
                return Err(AppError::Audio(format!(
                    "requested frame {offset} was already trimmed; resident base is {base}"
                )));
            }
            if offset > end {
                return Err(AppError::Audio(format!(
                    "requested frame {offset} is beyond captured end {end}"
                )));
            }
            if let Some(samples) = self.shared.samples.copy_absolute(offset, max_samples) {
                return Ok(samples);
            }
            if attempt < 16 {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
        Err(AppError::Audio(
            "resident sample copy remained contended beyond its bounded retry budget".into(),
        ))
    }

    /// Compatibility helper for the existing spill seam. `None` explicitly reports a stale or
    /// future absolute offset.
    pub fn snapshot_exact_from(&self, offset: usize, max_samples: usize) -> Option<Vec<f32>> {
        self.read_absolute(offset, max_samples).ok()
    }

    #[cfg(test)]
    pub(crate) fn from_samples(samples: Vec<f32>) -> Self {
        let capacity = samples.len().saturating_mul(2).max(64);
        let shared = Arc::new(Shared::with_capacity(capacity));
        accumulate_frames(&shared, &samples, 1);
        Self { shared }
    }

    #[cfg(test)]
    pub(crate) fn push_for_test(&self, more: &[f32]) {
        accumulate_frames(&self.shared, more, 1);
    }
}

/// Build the stream on its owning thread, then wait in a strict Prepared -> Active -> Stopping
/// state machine. The cpal stream never crosses the thread boundary.
fn capture_thread(
    shared: Arc<Shared>,
    device_name: Option<String>,
    control_rx: Receiver<ControlMessage>,
    ready_tx: Sender<Result<StartInfo>>,
) {
    let (stream, info) = match build_paused(&shared, device_name.as_deref()) {
        Ok(built) => built,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };

    if ready_tx.send(Ok(info)).is_err() {
        return;
    }

    let mut state = ControlState::Prepared;
    loop {
        if state == ControlState::Active && shared.should_stop() {
            break;
        }

        let message = if state == ControlState::Prepared {
            control_rx
                .recv()
                .map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            control_rx.recv_timeout(ACTIVE_CONTROL_POLL)
        };

        match message {
            Ok(ControlMessage::Activate(ack_tx)) => {
                if control_transition(state, ControlAction::Activate).is_none() {
                    let _ = ack_tx.send(Err(AppError::Audio(
                        "capture stream activation requested in an invalid state".into(),
                    )));
                    continue;
                }
                shared.capture_active.store(true, Ordering::Release);
                match stream.play() {
                    Ok(()) => {
                        state = ControlState::Active;
                        if let Some(fault) = shared.fault() {
                            let _ = ack_tx.send(Err(AppError::Audio(format!(
                                "mic capture faulted during activation: {fault:?}"
                            ))));
                            break;
                        }
                        if ack_tx.send(Ok(())).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        shared.capture_active.store(false, Ordering::Release);
                        let _ = ack_tx.send(Err(AppError::Audio(format!(
                            "failed to start input stream: {error}"
                        ))));
                        break;
                    }
                }
            }
            Ok(ControlMessage::Stop) => {
                debug_assert!(control_transition(state, ControlAction::Stop).is_some());
                break;
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => continue,
        }
    }

    let _ = stream.pause();
    shared.capture_active.store(false, Ordering::Release);
    if let Some(fault) = shared.fault() {
        tracing::error!(target: "audio", ?fault, "mic capture stopped after a callback fault");
    } else if shared.is_capped() {
        tracing::warn!(target: "audio", "maximum recording length reached; stopping mic capture");
    }
}

/// Select a configured input device without logging its PII-adjacent name.
fn select_input_device(host: &cpal::Host, name: Option<&str>) -> Result<cpal::Device> {
    if let Some(want) = name {
        if let Ok(devices) = host.input_devices() {
            for device in devices {
                if device.name().map(|name| name == want).unwrap_or(false) {
                    return Ok(device);
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

/// Build, but deliberately do not play, the input stream.
fn build_paused(
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
    validate_source_sample_rate(source_sample_rate)?;
    let channels = supported.channels();
    let config: StreamConfig = supported.into();
    shared
        .wall_cap_frames
        .store(recording_sample_cap(source_sample_rate), Ordering::Release);
    shared.retention_frames.store(
        live_history_retention_frames(source_sample_rate, shared.samples.capacity()),
        Ordering::Release,
    );

    tracing::info!(
        target: "audio",
        sample_rate = source_sample_rate,
        channels,
        ?sample_format,
        "prepared paused mic capture"
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

    Ok((stream, StartInfo { source_sample_rate }))
}

fn validate_source_sample_rate(source_sample_rate: u32) -> Result<()> {
    if (MIN_SUPPORTED_CAPTURE_RATE_HZ..=MAX_SUPPORTED_CAPTURE_RATE_HZ).contains(&source_sample_rate)
    {
        Ok(())
    } else {
        Err(AppError::Audio(format!(
            "unsupported input sample rate {source_sample_rate} Hz; supported range is {MIN_SUPPORTED_CAPTURE_RATE_HZ}..={MAX_SUPPORTED_CAPTURE_RATE_HZ} Hz"
        )))
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    shared: Arc<Shared>,
) -> Result<cpal::Stream>
where
    T: Sample + cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = (config.channels as usize).max(1);
    let error_shared = shared.clone();
    let data_callback = move |data: &[T], _: &cpal::InputCallbackInfo| {
        accumulate_frames(&shared, data, channels);
    };
    let error_callback = move |_error| {
        error_shared.latch_fault(CaptureFault::StreamError);
    };

    device
        .build_input_stream(config, data_callback, error_callback, None)
        .map_err(|e| AppError::Audio(format!("failed to build input stream: {e}")))
}

/// Real-time callback core. It accepts either the whole callback (except the final prefix at the
/// four-hour wall cap) or none of it. No temporary mono Vec is created and the fixed window cannot
/// grow. Mutex contention/capacity exhaustion is terminal and visible through [`CaptureFault`].
fn accumulate_frames<T>(shared: &Arc<Shared>, data: &[T], channels: usize)
where
    T: Sample,
    f32: cpal::FromSample<T>,
{
    if shared.should_stop() {
        return;
    }
    if shared.capture_active.load(Ordering::Acquire)
        && !shared.checkpoint_authority_live.load(Ordering::Acquire)
    {
        shared.latch_fault(CaptureFault::CheckpointAuthorityLost);
        return;
    }

    let channels = channels.max(1);
    if data.len() % channels != 0 {
        shared.latch_fault(CaptureFault::InvalidInterleavedInput);
        return;
    }
    let frame_count = data.len() / channels;
    if frame_count == 0 {
        return;
    }

    #[cfg(test)]
    callback_race_hook();

    // Acquire the callback-only generation epoch before reading base/end. Checkpoints never own
    // this epoch, so a preempted lower-priority spool cannot stall the realtime thread. No IO,
    // allocation, mutex wait or unbounded spin occurs here.
    let _epoch = match shared.samples.try_callback_epoch() {
        Some(epoch) => epoch,
        None => {
            shared.latch_fault(CaptureFault::BufferLockContended);
            return;
        }
    };

    let (mut base_frame, end_frame) = shared.samples.bounds();
    let wall_cap = shared.wall_cap_frames.load(Ordering::Acquire);
    let accepted = if wall_cap == 0 {
        frame_count
    } else {
        frame_count.min(wall_cap.saturating_sub(end_frame))
    };
    if accepted == 0 {
        shared.latch_cap();
        return;
    }
    let Some(new_end_frame) = end_frame.checked_add(accepted) else {
        shared.latch_fault(CaptureFault::FrameCounterOverflow);
        return;
    };
    if shared.samples.available(base_frame, end_frame) < accepted {
        // A checkpoint may have advanced base after the first snapshot. One late atomic reload
        // avoids a false full-window fault; if it is still full, no verified capacity exists and
        // stopping is safer than dropping an unknown callback.
        base_frame = shared.samples.base_frame.load(Ordering::Acquire);
        if shared.samples.available(base_frame, end_frame) < accepted {
            shared.latch_fault(CaptureFault::ResidentCapacityExhausted);
            return;
        }
    }

    // Pair the Acquire load of `base_frame` used for the capacity decision with readers that see a
    // subsequently reused slot. If a reader observes one of the Relaxed stores below, its Acquire
    // fence forces its trailing base load to observe the durable trim that authorized this reuse.
    std::sync::atomic::fence(Ordering::Release);

    let _ = shared.first_frame.set(Instant::now());
    let muted = shared.is_muted();
    let mut peak = 0.0f32;
    if muted {
        for position in 0..accepted {
            shared.samples.store(end_frame + position, 0.0);
        }
    } else {
        for (position, frame) in data.chunks_exact(channels).take(accepted).enumerate() {
            let mut sum = 0.0f32;
            for sample in frame {
                sum += f32::from_sample(*sample);
            }
            let mono = sum / channels as f32;
            peak = peak.max(mono.abs());
            shared.samples.store(end_frame + position, mono);
        }
    }

    shared
        .samples
        .end_frame
        .store(new_end_frame, Ordering::Release);
    let reached_wall_cap = wall_cap > 0 && new_end_frame >= wall_cap;
    if reached_wall_cap || accepted < frame_count {
        shared.latch_cap();
    } else if muted {
        shared.store_peak(0.0);
    } else {
        shared.store_peak(peak.clamp(0.0, 1.0));
    }
}

/// Lightweight description of an input device for the FE device picker.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputDeviceInfo {
    pub name: String,
    pub is_default: bool,
}

/// Enumerate available input devices. Names are returned to the picker but never logged.
pub fn list_input_devices() -> Vec<InputDeviceInfo> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|device| device.name().ok());
    let mut output = Vec::new();
    if let Ok(devices) = host.input_devices() {
        for device in devices {
            if let Ok(name) = device.name() {
                let is_default = default_name.as_deref() == Some(name.as_str());
                output.push(InputDeviceInfo { name, is_default });
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_shared(capacity: usize) -> Arc<Shared> {
        Arc::new(Shared::with_capacity(capacity))
    }

    fn resident(shared: &Shared) -> Vec<f32> {
        shared
            .samples
            .copy_tail(usize::MAX)
            .expect("single-threaded test snapshot must not race a trim")
    }

    fn test_checkpoint_writer(shared: &Arc<Shared>) -> CheckpointWriter {
        shared
            .checkpoint_authority_live
            .store(true, Ordering::Release);
        CheckpointWriter {
            shared: shared.clone(),
            authority_id: shared.checkpoint_authority_id,
        }
    }

    #[test]
    fn unmuted_callback_downmixes_without_changing_fixed_capacity() {
        let shared = test_shared(8);
        let capacity_before = shared.samples.capacity();
        accumulate_frames(&shared, &[0.5f32, 0.25, -0.5, -0.25], 2);

        assert_eq!(resident(&shared), vec![0.375, -0.375]);
        assert_eq!(shared.samples.capacity(), capacity_before);
        assert!((shared.load_peak() - 0.375).abs() < 1e-6);
    }

    #[test]
    fn muted_callback_writes_full_length_silence() {
        let shared = test_shared(8);
        accumulate_frames(&shared, &[0.5f32], 1);
        shared.set_muted(true);
        accumulate_frames(&shared, &[0.9f32, 0.8, -0.9, -0.8, 0.7, 0.6], 2);

        assert_eq!(resident(&shared), vec![0.5, 0.0, 0.0, 0.0]);
        assert_eq!(shared.load_peak(), 0.0);
        assert_eq!(shared.samples.end_frame.load(Ordering::Acquire), 4);
    }

    #[test]
    fn resident_overflow_rejects_the_whole_callback_and_latches_fault() {
        let shared = test_shared(4);
        accumulate_frames(&shared, &[0.1f32, 0.2, 0.3], 1);
        let before = resident(&shared);
        accumulate_frames(&shared, &[0.4f32, 0.5], 1);

        assert_eq!(
            resident(&shared),
            before,
            "overflow callback must be all-or-none"
        );
        assert_eq!(
            shared.fault(),
            Some(CaptureFault::ResidentCapacityExhausted)
        );
        assert!(shared.should_stop());
        assert_eq!(shared.load_peak(), 0.0);
    }

    #[test]
    fn full_window_trim_race_revalidates_before_capacity_fault() {
        use std::sync::Barrier;

        let shared = test_shared(4);
        accumulate_frames(&shared, &[0.0f32, 1.0, 2.0, 3.0], 1);
        let mut checkpoint = test_checkpoint_writer(&shared);
        let arrived = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let callback_shared = shared.clone();
        let (id_tx, id_rx) = mpsc::channel();
        let (start_tx, start_rx) = mpsc::channel();
        let callback = std::thread::spawn(move || {
            id_tx
                .send(std::thread::current().id())
                .expect("publish callback id");
            start_rx.recv().expect("start callback");
            accumulate_frames(&callback_shared, &[4.0f32], 1);
        });
        let callback_id = id_rx.recv().expect("callback id");
        *CALLBACK_RACE_HOOK.lock().expect("race hook") =
            Some((callback_id, arrived.clone(), resume.clone()));
        start_tx.send(()).expect("release callback");
        arrived.wait();
        let proof = checkpoint.verified_proof_for_test(0, 2);
        checkpoint
            .checkpoint_trim(proof)
            .expect("free exact prefix");
        resume.wait();
        callback.join().expect("callback thread");
        *CALLBACK_RACE_HOOK.lock().expect("clear race hook") = None;

        assert_eq!(resident(&shared), vec![2.0, 3.0, 4.0]);
        assert_eq!(
            shared.fault(),
            None,
            "a verified trim must prevent stale-full fault"
        );
    }

    #[test]
    fn checkpoint_never_waits_on_a_preempted_callback_epoch() {
        use std::sync::Barrier;

        let shared = test_shared(8);
        accumulate_frames(&shared, &[0.0f32, 1.0, 2.0, 3.0], 1);
        let callback_epoch = shared
            .samples
            .try_callback_epoch()
            .expect("hold simulated callback epoch");
        let entered = Arc::new(Barrier::new(2));
        let worker_shared = shared.clone();
        let worker_entered = entered.clone();
        let trim = std::thread::spawn(move || {
            worker_entered.wait();
            worker_shared.samples.checkpoint_trim(0, 2, 0)
        });
        entered.wait();
        assert_eq!(trim.join().expect("trim thread").expect("base CAS"), 2);
        drop(callback_epoch);
        assert_eq!(shared.fault(), None);
        assert_eq!(resident(&shared), vec![2.0, 3.0]);
    }

    #[test]
    fn reader_crossed_by_full_slot_reuse_rejects_mixed_generation() {
        use std::sync::Barrier;

        // ONE global hook slot, two tests, parallel threads — see `READER_RACE_HOOK_SERIAL`.
        let _serial = READER_RACE_HOOK_SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let shared = test_shared(4);
        accumulate_frames(&shared, &[0.0f32, 1.0, 2.0, 3.0], 1);
        let reader = SampleReader {
            shared: shared.clone(),
        };
        let mut checkpoint = test_checkpoint_writer(&shared);
        let arrived = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let (id_tx, id_rx) = mpsc::channel();
        let (start_tx, start_rx) = mpsc::channel();
        let reader_thread = std::thread::spawn(move || {
            id_tx
                .send(std::thread::current().id())
                .expect("publish reader id");
            start_rx.recv().expect("start reader");
            reader.read_absolute(0, 4)
        });
        let reader_id = id_rx.recv().expect("reader id");
        *READER_RACE_HOOK.lock().expect("reader race hook") =
            Some((reader_id, arrived.clone(), resume.clone()));
        start_tx.send(()).expect("release reader");
        arrived.wait();

        let proof = checkpoint.verified_proof_for_test(0, 4);
        checkpoint
            .checkpoint_trim(proof)
            .expect("authorize complete slot reuse");
        accumulate_frames(&shared, &[4.0f32, 5.0, 6.0, 7.0], 1);
        resume.wait();

        let old_read = reader_thread.join().expect("reader thread");
        *READER_RACE_HOOK.lock().expect("clear reader race hook") = None;
        assert!(
            old_read.is_err(),
            "a reader crossed by reuse must reject instead of returning mixed generations"
        );
        let fresh = SampleReader {
            shared: shared.clone(),
        };
        assert_eq!(
            fresh.read_absolute(4, 4).expect("new generation"),
            vec![4.0, 5.0, 6.0, 7.0]
        );
        assert_eq!(shared.fault(), None);
    }

    #[test]
    fn tail_snapshot_retries_trim_race_and_returns_absolute_generation_bounds() {
        use std::sync::Barrier;

        // ONE global hook slot, two tests, parallel threads — see `READER_RACE_HOOK_SERIAL`.
        let _serial = READER_RACE_HOOK_SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let shared = test_shared(4);
        accumulate_frames(&shared, &[0.0f32, 1.0, 2.0, 3.0], 1);
        let reader = SampleReader {
            shared: shared.clone(),
        };
        let mut checkpoint = test_checkpoint_writer(&shared);
        let arrived = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let (id_tx, id_rx) = mpsc::channel();
        let (start_tx, start_rx) = mpsc::channel();
        let reader_thread = std::thread::spawn(move || {
            id_tx
                .send(std::thread::current().id())
                .expect("publish reader id");
            start_rx.recv().expect("start reader");
            reader.snapshot_tail(4)
        });
        let reader_id = id_rx.recv().expect("reader id");
        *READER_RACE_HOOK.lock().expect("reader race hook") =
            Some((reader_id, arrived.clone(), resume.clone()));
        start_tx.send(()).expect("release reader");
        arrived.wait();

        let proof = checkpoint.verified_proof_for_test(0, 4);
        checkpoint
            .checkpoint_trim(proof)
            .expect("authorize complete slot reuse");
        accumulate_frames(&shared, &[4.0f32, 5.0, 6.0, 7.0], 1);
        // Disable the hook before releasing the crossed first copy so its bounded retry can run.
        *READER_RACE_HOOK.lock().expect("clear reader race hook") = None;
        resume.wait();

        let snapshot = reader_thread
            .join()
            .expect("reader thread")
            .expect("fresh-generation retry");
        assert_eq!(snapshot.start_frame, 4);
        assert_eq!(snapshot.end_frame, 8);
        assert_eq!(snapshot.samples, vec![4.0, 5.0, 6.0, 7.0]);
        assert_eq!(shared.fault(), None);
    }

    #[test]
    fn durable_checkpoint_keeps_bounded_live_and_manual_history() {
        let shared = test_shared(16);
        shared.retention_frames.store(8, Ordering::Release);
        let reader = SampleReader {
            shared: shared.clone(),
        };
        let mut checkpoint = test_checkpoint_writer(&shared);

        accumulate_frames(
            &shared,
            &[
                0.0f32, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0,
            ],
            1,
        );
        let proof = checkpoint.verified_proof_for_test(0, 12);
        assert_eq!(checkpoint.checkpoint_trim(proof).expect("trim"), 4);
        assert_eq!(
            reader.snapshot_from(4),
            vec![4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0]
        );
        assert_eq!(
            reader.read_absolute(8, 4).expect("retained live tail"),
            vec![8.0, 9.0, 10.0, 11.0]
        );
    }

    #[test]
    fn production_ring_holds_full_live_tail_at_384_khz() {
        assert_eq!(
            live_history_retention_frames(384_000, RESIDENT_RECORDING_BUFFER_SAMPLES),
            14 * 384_000
        );
    }

    #[test]
    fn capture_rate_contract_rejects_rates_outside_the_proven_ring_budget() {
        assert!(validate_source_sample_rate(8_000).is_ok());
        assert!(validate_source_sample_rate(48_000).is_ok());
        assert!(validate_source_sample_rate(384_000).is_ok());
        assert!(validate_source_sample_rate(7_999).is_err());
        assert!(validate_source_sample_rate(384_001).is_err());
    }

    #[test]
    fn exact_checkpoint_trim_preserves_absolute_offsets_and_fixed_memory() {
        let reader = SampleReader::from_samples(vec![0.0, 1.0, 2.0, 3.0]);
        let mut checkpoint = test_checkpoint_writer(&reader.shared);
        let capacity = reader.shared.samples.capacity();
        let proof = checkpoint.verified_proof_for_test(0, 3);
        assert_eq!(checkpoint.checkpoint_trim(proof).expect("trim"), 3);
        reader.push_for_test(&[4.0, 5.0, 6.0]);

        assert_eq!(checkpoint.resident_bounds(), (3, 7));
        assert_eq!(
            reader.read_absolute(3, usize::MAX).expect("exact read"),
            vec![3.0, 4.0, 5.0, 6.0]
        );
        assert_eq!(reader.shared.samples.capacity(), capacity);
        assert!(
            reader.snapshot_from(2).is_empty(),
            "trimmed history must not silently skip forward"
        );
        assert!(reader.read_absolute(2, 1).is_err());
    }

    #[test]
    fn stale_or_out_of_range_checkpoint_cannot_delete_frames() {
        let reader = SampleReader::from_samples(vec![1.0, 2.0, 3.0, 4.0]);
        let mut checkpoint = test_checkpoint_writer(&reader.shared);
        let wrong_generation = DurableCheckpointProof {
            authority_id: checkpoint.authority_id.wrapping_add(1),
            expected_base: 0,
            durable_end: 1,
        };
        assert!(checkpoint.checkpoint_trim(wrong_generation).is_err());
        assert_eq!(checkpoint.resident_bounds(), (0, 4));
        let first = checkpoint.verified_proof_for_test(0, 2);
        assert_eq!(checkpoint.checkpoint_trim(first).expect("first trim"), 2);
        let before = reader
            .read_absolute(2, usize::MAX)
            .expect("resident suffix");

        let stale = checkpoint.verified_proof_for_test(0, 3);
        assert!(
            checkpoint.checkpoint_trim(stale).is_err(),
            "stale expected base"
        );
        let future = checkpoint.verified_proof_for_test(2, 5);
        assert!(
            checkpoint.checkpoint_trim(future).is_err(),
            "durable end past capture"
        );
        assert_eq!(checkpoint.resident_bounds(), (2, 4));
        assert_eq!(
            reader
                .read_absolute(2, usize::MAX)
                .expect("unchanged suffix"),
            before
        );
    }

    #[test]
    fn four_hour_cap_uses_absolute_end_after_prefix_trimming() {
        let shared = test_shared(4);
        shared.wall_cap_frames.store(5, Ordering::Release);
        let reader = SampleReader {
            shared: shared.clone(),
        };
        let mut checkpoint = test_checkpoint_writer(&shared);

        accumulate_frames(&shared, &[0.0f32, 1.0, 2.0], 1);
        let proof = checkpoint.verified_proof_for_test(0, 3);
        checkpoint
            .checkpoint_trim(proof)
            .expect("durable first chunk");
        accumulate_frames(&shared, &[3.0f32, 4.0, 5.0], 1);

        assert_eq!(checkpoint.resident_bounds().1, 5, "absolute cap");
        assert_eq!(resident(&shared), vec![3.0, 4.0]);
        assert_eq!(reader.snapshot_from(3), vec![3.0, 4.0]);
        assert!(shared.is_capped());
        assert_eq!(
            recording_sample_cap(192_000),
            MAX_RECORDING_SECONDS as usize * 192_000
        );
    }

    #[test]
    fn bounded_real_append_and_durable_reuse_at_192_khz_keeps_fixed_memory() {
        const CYCLES: usize = 20_000;
        const CALLBACK_FRAMES: usize = 32;
        let shared = test_shared(CALLBACK_FRAMES * 2);
        shared
            .wall_cap_frames
            .store(recording_sample_cap(192_000), Ordering::Release);
        let mut checkpoint = test_checkpoint_writer(&shared);
        let capacity = shared.samples.capacity();
        let callback = [0.25f32; CALLBACK_FRAMES];
        for _ in 0..CYCLES {
            accumulate_frames(&shared, &callback, 1);
            let (base, durable_end) = checkpoint.resident_bounds();
            let proof = checkpoint.verified_proof_for_test(base, durable_end);
            checkpoint
                .checkpoint_trim(proof)
                .expect("one-frame durable checkpoint");
        }

        let (base, end) = checkpoint.resident_bounds();
        assert_eq!(end, CYCLES * CALLBACK_FRAMES);
        assert_eq!(base, end);
        assert_eq!(
            shared.samples.capacity(),
            capacity,
            "resident allocation stays fixed"
        );
        assert_eq!(shared.fault(), None);
        let configured_bytes = RESIDENT_RECORDING_BUFFER_SAMPLES
            .checked_mul(std::mem::size_of::<AtomicU32>())
            .expect("configured resident byte count");
        assert_eq!(configured_bytes, RESIDENT_RECORDING_BUFFER_BYTES);
        assert!(configured_bytes <= 32 * 1024 * 1024);
    }

    #[test]
    fn concurrent_snapshot_and_proven_trim_preserve_deterministic_samples() {
        use std::sync::Barrier;

        const ITERATIONS: usize = 20_000;
        let shared = test_shared(1_024);
        let reader = SampleReader {
            shared: shared.clone(),
        };
        for frame in 0..64 {
            accumulate_frames(&shared, &[(frame % 257) as f32], 1);
        }
        let start = Arc::new(Barrier::new(2));
        let published_end = Arc::new(AtomicUsize::new(64));
        let successful_reads = Arc::new(AtomicUsize::new(0));
        let reader_start = start.clone();
        let reader_end = published_end.clone();
        let reader_successes = successful_reads.clone();
        let reader_thread = std::thread::spawn(move || {
            reader_start.wait();
            for _ in 0..ITERATIONS {
                let end = reader_end.load(Ordering::Acquire);
                let offset = end.saturating_sub(32);
                if let Ok(samples) = reader.read_absolute(offset, 32) {
                    if !samples.is_empty() {
                        reader_successes.fetch_add(1, Ordering::Relaxed);
                    }
                    for (index, sample) in samples.into_iter().enumerate() {
                        assert_eq!(sample, ((offset + index) % 257) as f32);
                    }
                }
            }
        });

        start.wait();
        let mut checkpoint = test_checkpoint_writer(&shared);
        for frame in 64..ITERATIONS {
            accumulate_frames(&shared, &[(frame % 257) as f32], 1);
            published_end.store(frame + 1, Ordering::Release);
            let (base, end) = checkpoint.resident_bounds();
            if end.saturating_sub(base) >= 768 {
                let proof = checkpoint.verified_proof_for_test(base, end - 256);
                checkpoint
                    .checkpoint_trim(proof)
                    .expect("single-writer checkpoint");
            }
            assert_eq!(shared.fault(), None);
        }
        reader_thread.join().expect("reader pressure thread");

        assert_eq!(checkpoint.resident_bounds().1, ITERATIONS);
        assert_eq!(
            shared.fault(),
            None,
            "readers never take the callback lease"
        );
        assert_eq!(shared.samples.capacity(), 1_024);
        assert!(successful_reads.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn stop_timeout_retains_thread_owner_and_can_be_retried() {
        let (control_tx, control_rx) = mpsc::channel::<ControlMessage>();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let _control_rx = control_rx;
            let _ = release_rx.recv();
            let _ = done_tx.send(());
        });
        let recorder = Recorder {
            shared: test_shared(8),
            source_sample_rate: 48_000,
            started_at: Instant::now(),
            capture: Mutex::new(Some(CaptureThreadOwner {
                control_tx,
                done_rx,
                thread: Some(thread),
            })),
        };

        assert!(matches!(
            recorder
                .try_stop_with_timeout(Duration::ZERO)
                .expect("bounded stop attempt"),
            RecorderStopOutcome::Pending
        ));
        assert!(
            recorder
                .capture
                .lock()
                .expect("capture owner")
                .as_ref()
                .and_then(|owner| owner.thread.as_ref())
                .is_some(),
            "timeout must retain join ownership"
        );
        release_tx.send(()).expect("release capture thread");
        assert!(matches!(
            recorder
                .try_stop_with_timeout(CAPTURE_CONTROL_TIMEOUT)
                .expect("retry stop"),
            RecorderStopOutcome::Complete { .. }
        ));
        assert!(recorder
            .capture
            .lock()
            .expect("capture owner after stop")
            .is_none());
    }

    #[test]
    fn terminal_fault_stop_is_typed_and_never_legacy_success() {
        let shared = test_shared(8);
        accumulate_frames(&shared, &[0.25f32, -0.5], 1);
        shared.latch_fault(CaptureFault::StreamError);
        let recorder = Recorder {
            shared,
            source_sample_rate: 48_000,
            started_at: Instant::now(),
            capture: Mutex::new(None),
        };

        match recorder.try_stop().expect("typed stop") {
            RecorderStopOutcome::Faulted {
                captured_prefix,
                sample_rate,
                fault,
            } => {
                assert_eq!(captured_prefix, vec![0.25, -0.5]);
                assert_eq!(sample_rate, 48_000);
                assert_eq!(fault, CaptureFault::StreamError);
            }
            other => panic!("expected typed fault outcome, got {other:?}"),
        }
        assert!(
            recorder.stop().is_err(),
            "legacy Stop must not hide the fault"
        );
        assert_eq!(recorder.fault(), Some(CaptureFault::StreamError));
    }

    #[test]
    fn unconnected_legacy_start_fails_before_opening_a_device() {
        assert!(Recorder::start(None).is_err());
    }

    #[test]
    fn prepared_control_state_requires_activate_ack_before_active() {
        assert_eq!(
            control_transition(ControlState::Prepared, ControlAction::Activate),
            Some(ControlState::Active)
        );
        assert_eq!(
            control_transition(ControlState::Prepared, ControlAction::Stop),
            Some(ControlState::Stopping)
        );
        assert_eq!(
            control_transition(ControlState::Active, ControlAction::Stop),
            Some(ControlState::Stopping)
        );
        assert_eq!(
            control_transition(ControlState::Active, ControlAction::Activate),
            None
        );
    }

    #[test]
    fn first_frame_anchor_is_set_once() {
        let shared = test_shared(4);
        assert!(shared.first_frame.get().is_none());
        accumulate_frames(&shared, &[0.1f32], 1);
        let first = *shared.first_frame.get().expect("first frame anchor");
        accumulate_frames(&shared, &[0.2f32], 1);
        assert_eq!(*shared.first_frame.get().expect("stable anchor"), first);
    }

    #[test]
    fn cap_notice_is_a_rising_edge() {
        assert!(!should_emit_cap_notice(false, false));
        assert!(should_emit_cap_notice(true, false));
        assert!(!should_emit_cap_notice(true, true));
    }
}
