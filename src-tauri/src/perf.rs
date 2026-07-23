//! Shared "one heavy inference at a time" gate — see `AppState::heavy_inference`'s doc comment
//! for the full rationale. `spawn_blocking` alone gets CPU-bound native work off the async
//! runtime but is NOT a concurrency limiter (Tokio's own guidance: an unbounded blocking pool
//! means nothing stops N heavy calls from running simultaneously and fighting each other for the
//! same RAM/Metal context). Every native-runtime call site that loads/runs a heavy ML model
//! (whisper ASR, the diarizer, the Candle embedder/NER, a brain-sidecar dispatch) should route
//! through [`run_heavy`] instead of calling `tokio::task::spawn_blocking` directly.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use crate::error::{AppError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecordingSessionPhase {
    Starting,
    Live,
    Draining,
    Postprocess,
    Finished,
    Aborted,
}

/// Heavy runtime whose weights may remain resident after a dispatch. Active generations are
/// serialized separately; this tag forces an incompatible cache handoff before the next load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResidentModelKind {
    /// The opt-in standby wake-phrase cache. Distinct from meeting Whisper so a handoff can evict
    /// the tiny listener context before live/batch ASR loads another Whisper model.
    VoiceWhisper,
    Whisper,
    Embedder,
    Ner,
    BrainGguf,
    AppleFoundation,
    Ollama,
}

#[derive(Debug)]
struct RecordingSessionIdentity;

#[derive(Clone, Debug)]
pub(crate) struct RecordingSessionToken {
    identity: Arc<RecordingSessionIdentity>,
}

impl RecordingSessionToken {
    pub(crate) fn same_session_as(&self, other: &Self) -> bool {
        same_session(&self.identity, &other.identity)
    }

    /// Clone only when this identity still owns the LIVE session. `ActiveRecording` can expose this
    /// to chat/connectors without cloning or sharing the affine phase owner itself.
    pub(crate) fn validated_for_live_work(&self) -> Result<Self> {
        self.validated_for_phases(&[RecordingSessionPhase::Live])
    }

    /// Wait for one live-loop tick without making Stop wait for the whole thermal interval.
    /// Returns `true` only when this exact session remained [`RecordingSessionPhase::Live`] until
    /// `timeout` elapsed. Every phase transition notifies the coordinator Condvar, so Draining,
    /// abort, finish, or a stale token returns `false` promptly without a polling thread.
    pub(crate) fn wait_for_live_tick(&self, timeout: Duration) -> bool {
        let coordinator = recording_model_coordinator();
        let start = Instant::now();
        let mut state = lock_recording_model_coordinator();
        loop {
            let remains_live = state.active.as_ref().is_some_and(|active| {
                same_session(&active.identity, &self.identity)
                    && active.phase == RecordingSessionPhase::Live
            });
            if !remains_live {
                return false;
            }

            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return true;
            }
            let (next, _timed) = coordinator
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
            // Re-check both phase and elapsed time under the mutex. This handles notifications from
            // generation/egress changes and spurious wakes without shortening the thermal cadence.
        }
    }

    /// Clone for pipeline-owned local work after Stop has moved the owner into Postprocess.
    pub(crate) fn validated_for_postprocess(&self) -> Result<Self> {
        self.validated_for_phases(&[RecordingSessionPhase::Postprocess])
    }

    fn validated_for_phases(&self, allowed: &[RecordingSessionPhase]) -> Result<Self> {
        let state = lock_recording_model_coordinator();
        let Some(active) = state.active.as_ref() else {
            return Err(AppError::Unavailable(
                "recording model session token is stale".into(),
            ));
        };
        if !same_session(&active.identity, &self.identity) || !allowed.contains(&active.phase) {
            return Err(AppError::Unavailable(format!(
                "recording model session token is not valid during {:?}",
                active.phase
            )));
        }
        Ok(self.clone())
    }
}

#[derive(Debug)]
struct GenerationIdentity {
    owner: Option<Arc<RecordingSessionIdentity>>,
}

#[derive(Debug)]
struct LiveWorkIdentity {
    owner: Option<Arc<RecordingSessionIdentity>>,
}

struct ActiveRecordingSession {
    identity: Arc<RecordingSessionIdentity>,
    phase: RecordingSessionPhase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResidentModelQuarantine {
    kind: ResidentModelKind,
    recovery_key: String,
}

#[derive(Default)]
struct RecordingModelCoordinator {
    active: Option<ActiveRecordingSession>,
    generations: Vec<Arc<GenerationIdentity>>,
    live_work: Vec<Arc<LiveWorkIdentity>>,
    resident_kind: Option<ResidentModelKind>,
    quarantine: Option<ResidentModelQuarantine>,
}

struct RecordingModelCoordinatorCell {
    state: Mutex<RecordingModelCoordinator>,
    changed: Condvar,
}

fn recording_model_coordinator() -> &'static RecordingModelCoordinatorCell {
    static COORDINATOR: OnceLock<RecordingModelCoordinatorCell> = OnceLock::new();
    COORDINATOR.get_or_init(|| RecordingModelCoordinatorCell {
        state: Mutex::new(RecordingModelCoordinator::default()),
        changed: Condvar::new(),
    })
}

#[cfg(test)]
static MODEL_LIFECYCLE_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) fn model_lifecycle_test_guard() -> MutexGuard<'static, ()> {
    MODEL_LIFECYCLE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
pub(crate) fn reset_model_lifecycle_for_test() {
    let coordinator = recording_model_coordinator();
    *coordinator
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = RecordingModelCoordinator::default();
    startup_recovery_cell().store(false, Ordering::Release);
    coordinator.changed.notify_all();
}

fn recording_priority_epoch_cell() -> &'static AtomicU64 {
    static EPOCH: AtomicU64 = AtomicU64::new(0);
    &EPOCH
}

fn startup_recovery_cell() -> &'static AtomicBool {
    static ACTIVE: AtomicBool = AtomicBool::new(false);
    &ACTIVE
}

/// Affine ownership of launch-time crash recovery. Recovery may run ASR/summarization over hours
/// of preserved audio, so a fresh capture must fail fast instead of overlapping it. The owner is
/// retained until the actual salvage thread has joined—not merely until it was spawned.
#[must_use = "dropping the startup-recovery owner re-opens recording admission"]
pub(crate) struct StartupRecoveryOwner;

pub(crate) fn begin_startup_recovery() -> Result<StartupRecoveryOwner> {
    // Share the coordinator lock with `begin_recording_session`, making recovery-vs-capture
    // admission atomic even if a future caller starts recovery outside launch setup.
    let state = lock_recording_model_coordinator();
    if state.active.is_some() || !state.generations.is_empty() || !state.live_work.is_empty() {
        return Err(AppError::Unavailable(
            "model work is already active; startup recording recovery cannot overlap it".into(),
        ));
    }
    startup_recovery_cell()
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| {
            AppError::Unavailable("startup recording recovery is already active".into())
        })?;
    // Invalidate any background job that captured its epoch before recovery ownership was installed.
    recording_priority_epoch_cell().fetch_add(1, Ordering::AcqRel);
    drop(state);
    Ok(StartupRecoveryOwner)
}

pub(crate) fn startup_recovery_has_priority() -> bool {
    startup_recovery_cell().load(Ordering::Acquire)
}

impl Drop for StartupRecoveryOwner {
    fn drop(&mut self) {
        startup_recovery_cell().store(false, Ordering::Release);
    }
}

pub(crate) fn background_epoch() -> u64 {
    recording_priority_epoch_cell().load(Ordering::Acquire)
}

pub(crate) fn background_epoch_is_current(epoch: u64) -> bool {
    let state = lock_recording_model_coordinator();
    recording_priority_epoch_cell().load(Ordering::Acquire) == epoch
        && state.active.is_none()
        && !startup_recovery_has_priority()
}

/// Commit a bounded background mutation only if `epoch` is still current and recording has not
/// started. Admission is atomic against [`begin_recording_session`], but the coordinator mutex is
/// NOT held across `commit`: an admitted background-commit lease is published in `live_work`, so
/// Start can install priority promptly and then wait/cancel under its own bounded policy. This
/// avoids a large chunk transaction or filesystem fsync making Start itself mutex-block forever.
///
/// `commit` must not resolve/run a model, wait on the heavy semaphore, call this function again, or
/// perform network I/O. Model inference and content preparation happen before this seam.
pub(crate) fn with_current_background_epoch<T>(
    epoch: u64,
    commit: impl FnOnce() -> Result<T>,
) -> Result<Option<T>> {
    let lease = {
        let coordinator = recording_model_coordinator();
        let mut state = lock_recording_model_coordinator();
        if recording_priority_epoch_cell().load(Ordering::Acquire) != epoch
            || state.active.is_some()
            || startup_recovery_has_priority()
        {
            return Ok(None);
        }
        let identity = Arc::new(LiveWorkIdentity { owner: None });
        state.live_work.push(Arc::clone(&identity));
        coordinator.changed.notify_all();
        BackgroundCommitLease { identity }
    };
    let result = commit()?;
    drop(lease);
    Ok(Some(result))
}

#[must_use = "background commit admission must cover the complete mutation"]
struct BackgroundCommitLease {
    identity: Arc<LiveWorkIdentity>,
}

impl Drop for BackgroundCommitLease {
    fn drop(&mut self) {
        let coordinator = recording_model_coordinator();
        let mut state = lock_recording_model_coordinator();
        if let Some(index) = state
            .live_work
            .iter()
            .position(|work| Arc::ptr_eq(work, &self.identity))
        {
            state.live_work.swap_remove(index);
        }
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.phase == RecordingSessionPhase::Aborted)
            && state.generations.is_empty()
            && state.live_work.is_empty()
        {
            state.active = None;
        }
        coordinator.changed.notify_all();
    }
}

#[cfg(test)]
pub(crate) fn invalidate_background_epoch_for_test() {
    recording_priority_epoch_cell().fetch_add(1, Ordering::AcqRel);
}

pub(crate) fn recording_has_priority() -> bool {
    lock_recording_model_coordinator().active.is_some()
}

fn lock_recording_model_coordinator() -> MutexGuard<'static, RecordingModelCoordinator> {
    recording_model_coordinator()
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn same_session(a: &Arc<RecordingSessionIdentity>, b: &Arc<RecordingSessionIdentity>) -> bool {
    Arc::ptr_eq(a, b)
}

fn active_matches(state: &RecordingModelCoordinator, id: &Arc<RecordingSessionIdentity>) -> bool {
    state
        .active
        .as_ref()
        .is_some_and(|active| same_session(&active.identity, id))
}

fn belongs_to(generation: &GenerationIdentity, id: &Arc<RecordingSessionIdentity>) -> bool {
    generation
        .owner
        .as_ref()
        .is_some_and(|owner| same_session(owner, id))
}

fn live_work_belongs_to(work: &LiveWorkIdentity, id: &Arc<RecordingSessionIdentity>) -> bool {
    work.owner
        .as_ref()
        .is_some_and(|owner| same_session(owner, id))
}

#[must_use = "dropping the recording-session owner aborts its model lifecycle"]
pub(crate) struct RecordingSessionOwner {
    identity: Arc<RecordingSessionIdentity>,
}

impl RecordingSessionOwner {
    pub(crate) fn token(&self) -> RecordingSessionToken {
        RecordingSessionToken {
            identity: Arc::clone(&self.identity),
        }
    }

    #[cfg(test)]
    pub(crate) fn wait_for_quiescence(&self, timeout: Duration) -> Result<bool> {
        wait_for_session_quiescence(&self.token(), timeout)
    }

    /// Async-command counterpart: the Condvar wait runs only on Tokio's blocking pool. The
    /// authoritative affine owner remains with the caller; the worker receives its exact token.
    pub(crate) async fn wait_for_quiescence_async(&self, timeout: Duration) -> Result<bool> {
        let token = self.token();
        tokio::task::spawn_blocking(move || wait_for_session_quiescence(&token, timeout))
            .await
            .map_err(|e| {
                AppError::Other(anyhow::anyhow!("recording quiescence worker panicked: {e}"))
            })?
    }

    pub(crate) fn transition_to_live(&mut self) -> Result<()> {
        self.transition(
            RecordingSessionPhase::Starting,
            RecordingSessionPhase::Live,
            true,
        )
    }

    pub(crate) fn transition_to_draining(&mut self) -> Result<()> {
        self.transition(
            RecordingSessionPhase::Live,
            RecordingSessionPhase::Draining,
            false,
        )
    }

    pub(crate) fn transition_to_postprocess(&mut self) -> Result<()> {
        self.transition(
            RecordingSessionPhase::Draining,
            RecordingSessionPhase::Postprocess,
            true,
        )
    }

    pub(crate) fn finish(&mut self) -> Result<()> {
        let coordinator = recording_model_coordinator();
        let mut state = lock_recording_model_coordinator();
        let active = state.active.as_ref().ok_or_else(|| {
            AppError::Unavailable("recording model session is no longer active".into())
        })?;
        if !same_session(&active.identity, &self.identity) {
            return Err(AppError::Unavailable(
                "recording model session ownership is stale".into(),
            ));
        }
        if active.phase != RecordingSessionPhase::Postprocess {
            return Err(AppError::InvalidArg(format!(
                "illegal recording model phase transition: {:?} -> Finished",
                active.phase
            )));
        }
        if state
            .generations
            .iter()
            .any(|generation| belongs_to(generation, &self.identity))
            || state
                .live_work
                .iter()
                .any(|work| live_work_belongs_to(work, &self.identity))
        {
            return Err(AppError::Unavailable(
                "recording-owned model generations or live egress are still running".into(),
            ));
        }
        if let Some(active) = state.active.as_mut() {
            active.phase = RecordingSessionPhase::Finished;
        }
        state.active = None;
        coordinator.changed.notify_all();
        Ok(())
    }

    fn transition(
        &mut self,
        expected: RecordingSessionPhase,
        next: RecordingSessionPhase,
        require_quiescence: bool,
    ) -> Result<()> {
        let coordinator = recording_model_coordinator();
        let mut state = lock_recording_model_coordinator();
        let active = state.active.as_ref().ok_or_else(|| {
            AppError::Unavailable("recording model session is no longer active".into())
        })?;
        if !same_session(&active.identity, &self.identity) {
            return Err(AppError::Unavailable(
                "recording model session ownership is stale".into(),
            ));
        }
        if active.phase != expected {
            return Err(AppError::InvalidArg(format!(
                "illegal recording model phase transition: {:?} -> {:?}",
                active.phase, next
            )));
        }
        let must_wait = match expected {
            RecordingSessionPhase::Starting => {
                state.generations.iter().any(|g| g.owner.is_none())
                    || state.live_work.iter().any(|work| work.owner.is_none())
            }
            RecordingSessionPhase::Draining => {
                state
                    .generations
                    .iter()
                    .any(|g| belongs_to(g, &self.identity))
                    || state
                        .live_work
                        .iter()
                        .any(|work| live_work_belongs_to(work, &self.identity))
            }
            _ => !state.generations.is_empty() || !state.live_work.is_empty(),
        };
        if require_quiescence && must_wait {
            return Err(AppError::Unavailable(
                "model generations and live egress must drain before the next recording phase"
                    .into(),
            ));
        }
        if let Some(active) = state.active.as_mut() {
            active.phase = next;
        }
        coordinator.changed.notify_all();
        Ok(())
    }
}

fn wait_for_session_quiescence(token: &RecordingSessionToken, timeout: Duration) -> Result<bool> {
    let coordinator = recording_model_coordinator();
    let start = Instant::now();
    let mut state = lock_recording_model_coordinator();
    loop {
        if !active_matches(&state, &token.identity) {
            return Err(AppError::Unavailable(
                "recording model session is no longer active".into(),
            ));
        }
        if state.generations.is_empty() && state.live_work.is_empty() {
            return Ok(true);
        }
        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Ok(false);
        }
        let (next, timed) = coordinator
            .changed
            .wait_timeout(state, remaining)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state = next;
        if timed.timed_out() && (!state.generations.is_empty() || !state.live_work.is_empty()) {
            return Ok(false);
        }
    }
}

impl Drop for RecordingSessionOwner {
    fn drop(&mut self) {
        let coordinator = recording_model_coordinator();
        let mut state = lock_recording_model_coordinator();
        if active_matches(&state, &self.identity) {
            if let Some(active) = state.active.as_mut() {
                active.phase = RecordingSessionPhase::Aborted;
            }
            if state.generations.is_empty() && state.live_work.is_empty() {
                state.active = None;
            }
            coordinator.changed.notify_all();
        }
    }
}

/// Installs recording priority atomically before the caller performs any capture side effect.
pub(crate) fn begin_recording_session() -> Result<RecordingSessionOwner> {
    let coordinator = recording_model_coordinator();
    let mut state = lock_recording_model_coordinator();
    if state.active.is_some() {
        return Err(AppError::Unavailable(
            "another recording model session is already active".into(),
        ));
    }
    if startup_recovery_has_priority() {
        return Err(AppError::Unavailable(
            "crash recovery is preserving a previous recording; wait for it to finish before recording again"
                .into(),
        ));
    }
    let identity = Arc::new(RecordingSessionIdentity);
    state.active = Some(ActiveRecordingSession {
        identity: Arc::clone(&identity),
        phase: RecordingSessionPhase::Starting,
    });
    recording_priority_epoch_cell().fetch_add(1, Ordering::AcqRel);
    coordinator.changed.notify_all();
    Ok(RecordingSessionOwner { identity })
}

/// Affine ownership of one recording-coupled work item. External providers acquire it only AFTER
/// local prompt preparation and hold it across the network future; local manual turns acquire it
/// before spawning their worker. Draining closes admission atomically, then quiescence waits for
/// every already-issued lease to drop.
#[must_use = "the recording-work lease must cover the full owned work item"]
pub(crate) struct RecordingWorkLease {
    identity: Arc<LiveWorkIdentity>,
}

impl Drop for RecordingWorkLease {
    fn drop(&mut self) {
        let coordinator = recording_model_coordinator();
        let mut state = lock_recording_model_coordinator();
        if let Some(index) = state
            .live_work
            .iter()
            .position(|work| Arc::ptr_eq(work, &self.identity))
        {
            state.live_work.swap_remove(index);
        }
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.phase == RecordingSessionPhase::Aborted)
            && state.generations.is_empty()
            && state.live_work.is_empty()
        {
            state.active = None;
        }
        coordinator.changed.notify_all();
    }
}

fn acquire_work_lease(token: Option<&RecordingSessionToken>) -> Result<RecordingWorkLease> {
    let coordinator = recording_model_coordinator();
    let mut state = lock_recording_model_coordinator();
    let owner = match (state.active.as_ref(), token) {
        (None, None) => None,
        (None, Some(_)) => {
            return Err(AppError::Unavailable(
                "recording external-egress token is stale".into(),
            ));
        }
        (Some(active), Some(token))
            if matches!(
                active.phase,
                RecordingSessionPhase::Live | RecordingSessionPhase::Postprocess
            ) && same_session(&active.identity, &token.identity) =>
        {
            Some(Arc::clone(&active.identity))
        }
        (Some(active), _) => {
            return Err(AppError::Unavailable(format!(
                "external egress is not admitted during recording phase {:?}",
                active.phase
            )));
        }
    };
    let identity = Arc::new(LiveWorkIdentity { owner });
    state.live_work.push(Arc::clone(&identity));
    coordinator.changed.notify_all();
    Ok(RecordingWorkLease { identity })
}

/// Close the check-to-dispatch race for external providers/connectors. With no recording, an
/// unscoped lease makes a concurrently-starting recording wait. With an active recording, only its
/// exact token in Live or Postprocess can acquire; Starting/Draining and stale/missing tokens fail
/// before any egress attempt is ledgered.
pub(crate) fn acquire_external_egress_lease(
    token: Option<&RecordingSessionToken>,
) -> Result<RecordingWorkLease> {
    if crate::summarize::claude_code::has_unproven_process_group() {
        return Err(AppError::Unavailable(
            "external egress is blocked by an unproven Claude CLI process group".into(),
        ));
    }
    acquire_work_lease(token)
}

/// Reserve one local/manual worker under the exact recording identity before its thread exists.
/// Unlike the external-egress adapter, this carries no Claude-process prerequisite and makes no
/// egress claim; it only closes the Live/Draining/Postprocess dispatch race.
pub(crate) fn acquire_recording_work_lease(
    token: &RecordingSessionToken,
) -> Result<RecordingWorkLease> {
    acquire_work_lease(Some(token))
}

#[must_use = "the model-generation lease must be held for the full generation lifetime"]
pub(crate) struct RecordingModelGenerationLease {
    identity: Arc<GenerationIdentity>,
}

impl Drop for RecordingModelGenerationLease {
    fn drop(&mut self) {
        let coordinator = recording_model_coordinator();
        let mut state = lock_recording_model_coordinator();
        if let Some(index) = state
            .generations
            .iter()
            .position(|generation| Arc::ptr_eq(generation, &self.identity))
        {
            state.generations.swap_remove(index);
        }
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.phase == RecordingSessionPhase::Aborted)
            && state.generations.is_empty()
            && state.live_work.is_empty()
        {
            state.active = None;
        }
        coordinator.changed.notify_all();
    }
}

pub(crate) fn acquire_unscoped_model_generation(
    kind: ResidentModelKind,
) -> Result<RecordingModelGenerationLease> {
    let lease = acquire_model_generation(None, kind)?;
    prepare_resident_model_kind(kind)?;
    Ok(lease)
}

pub(crate) fn acquire_recording_model_generation(
    token: &RecordingSessionToken,
    kind: ResidentModelKind,
) -> Result<RecordingModelGenerationLease> {
    let lease = acquire_model_generation(Some(token), kind)?;
    prepare_resident_model_kind(kind)?;
    Ok(lease)
}

/// Run exactly one synchronous local-model segment under the global residency lane. Callers must
/// place only prompt-ready model load/forward/decode in `f`: no DB reads, prompt construction,
/// tools, network, or persistence. This is the common seam for e5, NER, reranking, AFM and Brain.
pub(crate) fn with_model_generation<T>(
    token: Option<&RecordingSessionToken>,
    kind: ResidentModelKind,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let _generation = match token {
        Some(token) => acquire_recording_model_generation(token, kind)?,
        None => acquire_unscoped_model_generation(kind)?,
    };
    f()
}

/// Evict every incompatible process-resident runtime while the caller's generation lease blocks
/// competitors, then publish the new resident kind. No coordinator mutex is held during eviction.
/// Same-kind dispatches retain their warm cache intentionally.
fn prepare_resident_model_kind(next: ResidentModelKind) -> Result<()> {
    let previous = {
        let state = lock_recording_model_coordinator();
        if state.generations.is_empty() {
            return Err(AppError::Unavailable(
                "model-kind handoff requires an active generation lease".into(),
            ));
        }
        state.resident_kind
    };
    if previous == Some(next) {
        return Ok(());
    }

    if next != ResidentModelKind::VoiceWhisper {
        crate::audio::listener::release_wake_transcriber_cache();
    }
    if next != ResidentModelKind::Embedder {
        crate::embed::release_real_embedder_cache();
    }
    if next != ResidentModelKind::Ner {
        crate::summarize::ner_deberta::release_all_caches();
    }
    if next != ResidentModelKind::BrainGguf
        && !crate::reason::sidecar::kill_for_recording(Duration::from_secs(2))?
    {
        return Err(AppError::Unavailable(
            "on-device Brain did not release for model-kind handoff".into(),
        ));
    }

    let mut state = lock_recording_model_coordinator();
    if state.generations.is_empty() {
        return Err(AppError::Unavailable(
            "model generation ended during resident-kind handoff".into(),
        ));
    }
    state.resident_kind = Some(next);
    Ok(())
}

/// Poison the residency lane after a daemon-backed generation could not prove it stopped. The
/// current generation lease must still be held when this is called; after it drops, every model
/// kind remains fail-closed until an explicit, identity-matched unload succeeds (or process restart).
pub(crate) fn quarantine_resident_model(
    kind: ResidentModelKind,
    recovery_key: String,
) -> Result<()> {
    let mut state = lock_recording_model_coordinator();
    if state.generations.is_empty() || state.resident_kind != Some(kind) {
        return Err(AppError::Unavailable(
            "resident-model quarantine requires its active generation lease".into(),
        ));
    }
    state.quarantine = Some(ResidentModelQuarantine { kind, recovery_key });
    recording_model_coordinator().changed.notify_all();
    Ok(())
}

/// Return the opaque recovery identity only for the quarantined kind. It contains no meeting
/// content; Ollama uses normalized endpoint+model so a different daemon/model cannot clear it.
pub(crate) fn resident_model_quarantine_key(kind: ResidentModelKind) -> Option<String> {
    lock_recording_model_coordinator()
        .quarantine
        .as_ref()
        .filter(|quarantine| quarantine.kind == kind)
        .map(|quarantine| quarantine.recovery_key.clone())
}

/// Clear a quarantine only after the caller has obtained a successful, fully-read unload response
/// for this exact recovery identity. A mismatched proof fails closed.
pub(crate) fn clear_resident_model_quarantine(
    kind: ResidentModelKind,
    recovery_key: &str,
) -> Result<()> {
    let mut state = lock_recording_model_coordinator();
    if !state.generations.is_empty() {
        return Err(AppError::Unavailable(
            "resident-model quarantine cannot clear while a generation lease is active".into(),
        ));
    }
    match state.quarantine.as_ref() {
        None => return Ok(()),
        Some(quarantine) if quarantine.kind == kind && quarantine.recovery_key == recovery_key => {}
        Some(_) => {
            return Err(AppError::Unavailable(
                "resident-model quarantine proof does not match the uncertain runtime".into(),
            ));
        }
    }
    state.quarantine = None;
    recording_model_coordinator().changed.notify_all();
    Ok(())
}

/// A non-reserving preflight used only to avoid spawning known-doomed live background workers.
/// Admission is still revalidated atomically by [`with_model_generation`] at actual inference.
pub(crate) fn recording_model_lane_is_free(token: &RecordingSessionToken) -> bool {
    let state = lock_recording_model_coordinator();
    active_matches(&state, &token.identity)
        && matches!(
            state.active.as_ref().map(|active| active.phase),
            Some(RecordingSessionPhase::Live | RecordingSessionPhase::Postprocess)
        )
        && state.generations.is_empty()
        && state.quarantine.is_none()
}

fn acquire_model_generation(
    token: Option<&RecordingSessionToken>,
    kind: ResidentModelKind,
) -> Result<RecordingModelGenerationLease> {
    // AFM teardown failures retain the Child handle outside the coordinator so a failed reap can be
    // retried. Never admit another runtime while that process may still own RAM/CPU, even when a
    // direct probe could not install the coordinator quarantine itself.
    if crate::reason::afm::has_unreaped_child() {
        return Err(AppError::Unavailable(
            "local-model admission is blocked by an unreaped Apple Foundation Models sidecar"
                .into(),
        ));
    }
    let coordinator = recording_model_coordinator();
    let mut state = lock_recording_model_coordinator();
    if let Some(quarantine) = state.quarantine.as_ref() {
        return Err(AppError::Unavailable(format!(
            "local-model residency is quarantined after an uncertain {:?} shutdown; verified unload or app restart required before {:?}",
            quarantine.kind, kind
        )));
    }
    let owner = match (state.active.as_ref(), token) {
        (None, None) => None,
        (None, Some(_)) => {
            return Err(AppError::Unavailable(
                "recording model session token is stale".into(),
            ));
        }
        (Some(_), None) => {
            return Err(AppError::Unavailable(
                "background local AI is paused for recording".into(),
            ));
        }
        (Some(active), Some(token)) => {
            if !same_session(&active.identity, &token.identity) {
                return Err(AppError::Unavailable(
                    "recording model session token does not own the active session".into(),
                ));
            }
            if !matches!(
                active.phase,
                RecordingSessionPhase::Live | RecordingSessionPhase::Postprocess
            ) {
                return Err(AppError::Unavailable(format!(
                    "recording-owned model generation is not admitted during {:?}",
                    active.phase
                )));
            }
            Some(Arc::clone(&active.identity))
        }
    };
    if !state.generations.is_empty() {
        return Err(AppError::Unavailable(
            "another local-model generation is already active".into(),
        ));
    }
    let identity = Arc::new(GenerationIdentity { owner });
    state.generations.push(Arc::clone(&identity));
    coordinator.changed.notify_all();
    Ok(RecordingModelGenerationLease { identity })
}

/// Acquire the ONE global heavy-work permit (`AppState::heavy_inference`), then run `f` on the
/// blocking pool. This is intentionally semaphore-only: legacy callers also use it for document
/// extraction, crypto and DB orchestration, none of which may reserve local-model residency.
/// Actual model calls use [`run_heavy_with_admission`] / [`run_heavy_maybe_recording`].
pub async fn run_heavy<F, T>(semaphore: &Arc<tokio::sync::Semaphore>, f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    run_blocking_serialized(semaphore, f).await
}

/// Recording-owned counterpart to [`run_heavy`]. The exact token, the model-generation lease,
/// and the semaphore permit all move into the non-cancellable blocking closure together.
pub(crate) async fn run_heavy_recording<F, T>(
    semaphore: &Arc<tokio::sync::Semaphore>,
    token: RecordingSessionToken,
    kind: ResidentModelKind,
    f: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    run_heavy_with_admission(semaphore, Some(token), kind, f).await
}

pub(crate) async fn run_heavy_maybe_recording<F, T>(
    semaphore: &Arc<tokio::sync::Semaphore>,
    token: Option<RecordingSessionToken>,
    kind: ResidentModelKind,
    f: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    match token {
        Some(token) => run_heavy_recording(semaphore, token, kind, f).await,
        None => run_heavy_with_admission(semaphore, None, kind, f).await,
    }
}

/// Run work after the heavy permit becomes available. Model admission happens INSIDE the
/// non-cancellable closure, after the await, so a queued task never reserves residency while a
/// different heavy task is still running.
pub(crate) async fn run_heavy_with_admission<F, T>(
    semaphore: &Arc<tokio::sync::Semaphore>,
    token: Option<RecordingSessionToken>,
    kind: ResidentModelKind,
    f: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let permit = semaphore
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| AppError::Other(anyhow::anyhow!("heavy-inference semaphore closed")))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        with_model_generation(token.as_ref(), kind, f)
    })
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("heavy inference task panicked: {e}")))?
}

/// Serialize a blocking orchestration closure whose concrete model implementation performs its
/// own per-call admission (for example `ModelAdmittedReasoner`). This function owns only the heavy
/// semaphore; it must not be used around an unadmitted native model call.
pub(crate) async fn run_blocking_serialized<F, T>(
    semaphore: &Arc<tokio::sync::Semaphore>,
    f: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let permit = semaphore
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| AppError::Other(anyhow::anyhow!("heavy-inference semaphore closed")))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f()
    })
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("heavy inference task panicked: {e}")))?
}

/// macOS kernel memory-pressure level via `sysctl -n kern.memorystatus_vm_pressure_level` — the
/// SAME no-new-crate/no-new-FFI shell-out convention as `total_ram_bytes`/`available_ram_bytes`
/// (`transcribe/model.rs`, `reason/sidecar.rs`). `1` = normal, `2` = warn, `4` = critical.
///
/// This is a DIFFERENT signal than the existing `vm_stat`-derived free/inactive/speculative
/// arithmetic: that answers "does THIS job's footprint fit the numbers we can see", this answers
/// "does the KERNEL ITSELF already think the whole system is under pressure" (e.g. another app
/// about to be jetsam-killed, or pressure that hasn't yet shown up as reduced free/inactive
/// pages). Meant to be consulted ALONGSIDE an existing RAM-floor check, not instead of it — see
/// [`heavy_op_permitted`].
///
/// Returns `None` on ANY parse/exec failure — callers must fail OPEN on `None`, matching every
/// other RAM-probe convention in this codebase (a broken measurement must never silently refuse
/// a legitimate task).
fn kernel_memory_pressure_level() -> Option<u8> {
    let out = std::process::Command::new("sysctl")
        .arg("-n")
        .arg("kern.memorystatus_vm_pressure_level")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

/// Combine an existing RAM-floor verdict (`ram_floor_ok`, e.g. `topic_backfill_ram_permits_now()`
/// or `parakeet_ram_permits_now()`) with the kernel's own pressure signal — refuses ONLY when the
/// floor check already said no, OR the kernel reports CRITICAL (4) pressure. `WARN` (2) is common
/// and deliberately NOT itself a refusal (per the research brief this codifies: blocking on WARN
/// would be over-eager for a user-initiated action like starting a recording). Fails OPEN on a
/// broken pressure probe — a `None` never adds a NEW refusal on top of an already-permitting floor
/// check.
pub fn heavy_op_permitted(ram_floor_ok: bool) -> bool {
    if !ram_floor_ok {
        return false;
    }
    kernel_memory_pressure_level().map_or(true, |level| level < 4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct SessionReset;

    impl Drop for SessionReset {
        fn drop(&mut self) {
            reset_model_lifecycle_for_test();
        }
    }

    fn session_test() -> (MutexGuard<'static, ()>, SessionReset) {
        (model_lifecycle_test_guard(), SessionReset)
    }

    #[test]
    fn startup_recovery_blocks_capture_and_background_epoch_until_owner_drops() {
        let (_serial, _reset) = session_test();
        let epoch_before = background_epoch();
        let recovery = begin_startup_recovery().expect("claim startup recovery");
        assert!(startup_recovery_has_priority());
        assert!(!background_epoch_is_current(epoch_before));
        assert!(
            begin_recording_session().is_err(),
            "capture must never overlap launch salvage"
        );

        drop(recovery);
        assert!(!startup_recovery_has_priority());
        let mut owner = begin_recording_session().expect("recording reopens after salvage join");
        owner.transition_to_live().unwrap();
        owner.transition_to_draining().unwrap();
        owner.transition_to_postprocess().unwrap();
        owner.finish().unwrap();
    }

    #[test]
    fn start_priority_blocks_unscoped_before_live_and_token_is_phase_bound() {
        let (_serial, _reset) = session_test();
        let mut owner = begin_recording_session().unwrap();
        let token = owner.token();
        assert!(acquire_unscoped_model_generation(ResidentModelKind::Whisper).is_err());
        assert!(acquire_recording_model_generation(&token, ResidentModelKind::Whisper).is_err());
        assert!(owner
            .wait_for_quiescence(Duration::from_millis(20))
            .unwrap());
        owner.transition_to_live().unwrap();
        let live = acquire_recording_model_generation(&token, ResidentModelKind::Whisper).unwrap();
        assert!(acquire_unscoped_model_generation(ResidentModelKind::Whisper).is_err());
        drop(live);
        owner.transition_to_draining().unwrap();
        assert!(acquire_recording_model_generation(&token, ResidentModelKind::Whisper).is_err());
        owner.transition_to_postprocess().unwrap();
        let post = acquire_recording_model_generation(&token, ResidentModelKind::Whisper).unwrap();
        drop(post);
        owner.finish().unwrap();
        assert!(acquire_unscoped_model_generation(ResidentModelKind::Whisper).is_ok());
    }

    #[test]
    fn stale_token_cannot_enter_a_later_recording() {
        let (_serial, _reset) = session_test();
        let mut first = begin_recording_session().unwrap();
        let stale = first.token();
        first.transition_to_live().unwrap();
        first.transition_to_draining().unwrap();
        first.transition_to_postprocess().unwrap();
        first.finish().unwrap();

        let mut second = begin_recording_session().unwrap();
        second.transition_to_live().unwrap();
        assert!(acquire_recording_model_generation(&stale, ResidentModelKind::Whisper).is_err());
        second.transition_to_draining().unwrap();
        second.transition_to_postprocess().unwrap();
        second.finish().unwrap();
    }

    #[test]
    fn active_recording_can_lend_a_validated_token_without_sharing_its_owner() {
        let (_serial, _reset) = session_test();
        let mut owner = begin_recording_session().unwrap();
        let token = owner.token();
        assert!(token.validated_for_live_work().is_err());
        owner.transition_to_live().unwrap();
        assert!(token.validated_for_live_work().is_ok());
        assert!(token.validated_for_postprocess().is_err());
        owner.transition_to_draining().unwrap();
        assert!(token.validated_for_live_work().is_err());
        owner.transition_to_postprocess().unwrap();
        assert!(token.validated_for_postprocess().is_ok());
        owner.finish().unwrap();
        assert!(token.validated_for_postprocess().is_err());
    }

    #[test]
    fn live_tick_wait_times_out_true_and_draining_wakes_false() {
        let (_serial, _reset) = session_test();
        let mut owner = begin_recording_session().unwrap();
        owner.transition_to_live().unwrap();
        let token = owner.token();

        assert!(token.wait_for_live_tick(Duration::from_millis(1)));

        let waiting_token = token.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            waiting_token.wait_for_live_tick(Duration::from_secs(5))
        });
        ready_rx.recv().unwrap();
        owner.transition_to_draining().unwrap();
        assert!(!waiter.join().unwrap());

        owner.transition_to_postprocess().unwrap();
        owner.finish().unwrap();
    }

    #[test]
    fn live_egress_admission_closes_on_draining_and_existing_lease_is_drained() {
        let (_serial, _reset) = session_test();
        let mut owner = begin_recording_session().unwrap();
        owner.transition_to_live().unwrap();
        let token = owner.token();
        let in_flight = acquire_external_egress_lease(Some(&token)).unwrap();
        owner.transition_to_draining().unwrap();
        assert!(acquire_external_egress_lease(Some(&token)).is_err());
        assert!(!owner.wait_for_quiescence(Duration::from_millis(5)).unwrap());
        drop(in_flight);
        assert!(owner
            .wait_for_quiescence(Duration::from_millis(50))
            .unwrap());
        owner.transition_to_postprocess().unwrap();
        let postprocess = acquire_external_egress_lease(Some(&token)).unwrap();
        assert!(!owner.wait_for_quiescence(Duration::from_millis(5)).unwrap());
        drop(postprocess);
        owner.finish().unwrap();
    }

    #[test]
    fn recording_start_drains_external_egress_that_won_the_prestart_race() {
        let (_serial, _reset) = session_test();
        let in_flight = acquire_external_egress_lease(None).unwrap();
        let mut owner = begin_recording_session().unwrap();
        assert!(!owner.wait_for_quiescence(Duration::from_millis(5)).unwrap());
        drop(in_flight);
        assert!(owner
            .wait_for_quiescence(Duration::from_millis(50))
            .unwrap());
        owner.transition_to_live().unwrap();
        owner.transition_to_draining().unwrap();
        owner.transition_to_postprocess().unwrap();
        owner.finish().unwrap();
    }

    #[test]
    fn uncertain_resident_runtime_quarantines_every_model_kind_until_exact_proof() {
        let (_serial, _reset) = session_test();
        let ollama = acquire_unscoped_model_generation(ResidentModelKind::Ollama).unwrap();
        quarantine_resident_model(ResidentModelKind::Ollama, "daemon-a/model-a".into()).unwrap();
        drop(ollama);
        assert!(acquire_unscoped_model_generation(ResidentModelKind::Ollama).is_err());
        assert!(acquire_unscoped_model_generation(ResidentModelKind::Whisper).is_err());
        assert!(
            clear_resident_model_quarantine(ResidentModelKind::Ollama, "daemon-b/model-a").is_err()
        );
        clear_resident_model_quarantine(ResidentModelKind::Ollama, "daemon-a/model-a").unwrap();
        assert!(acquire_unscoped_model_generation(ResidentModelKind::Whisper).is_ok());
    }

    #[test]
    fn start_epoch_invalidates_background_output_even_after_recording_finishes() {
        let (_serial, _reset) = session_test();
        let epoch = background_epoch();
        assert!(background_epoch_is_current(epoch));
        let mut owner = begin_recording_session().unwrap();
        assert!(!background_epoch_is_current(epoch));
        owner.transition_to_live().unwrap();
        owner.transition_to_draining().unwrap();
        owner.transition_to_postprocess().unwrap();
        owner.finish().unwrap();
        assert!(
            !background_epoch_is_current(epoch),
            "an answer dispatched before Start stays stale after Stop and must be discarded"
        );
    }

    #[test]
    fn stale_background_epoch_never_enters_the_commit_closure() {
        let (_serial, _reset) = session_test();
        let epoch = background_epoch();
        let commits = AtomicUsize::new(0);
        let mut owner = begin_recording_session().unwrap();

        let result = with_current_background_epoch(epoch, || {
            commits.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();

        assert!(result.is_none());
        assert_eq!(commits.load(Ordering::SeqCst), 0);
        owner.transition_to_live().unwrap();
        owner.transition_to_draining().unwrap();
        owner.transition_to_postprocess().unwrap();
        owner.finish().unwrap();
    }

    #[test]
    fn start_installs_priority_then_waits_for_admitted_epoch_commit() {
        let (_serial, _reset) = session_test();
        let epoch = background_epoch();
        let commits = Arc::new(AtomicUsize::new(0));
        let (commit_entered_tx, commit_entered_rx) = std::sync::mpsc::channel();
        let (release_commit_tx, release_commit_rx) = std::sync::mpsc::channel();
        let commits_in = Arc::clone(&commits);

        let commit_thread = std::thread::spawn(move || {
            with_current_background_epoch(epoch, || {
                commit_entered_tx.send(()).unwrap();
                release_commit_rx.recv().unwrap();
                commits_in.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .unwrap()
        });
        commit_entered_rx.recv().unwrap();

        let (start_tx, start_rx) = std::sync::mpsc::channel();
        let start_thread = std::thread::spawn(move || {
            start_tx.send(begin_recording_session()).unwrap();
        });
        let owner = start_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Start admission must not mutex-block behind the commit")
            .unwrap();
        assert!(
            !owner
                .wait_for_quiescence(Duration::from_millis(20))
                .unwrap(),
            "the admitted commit remains visible to Starting until it drops"
        );

        release_commit_tx.send(()).unwrap();
        assert!(commit_thread.join().unwrap().is_some());
        assert!(owner.wait_for_quiescence(Duration::from_secs(1)).unwrap());
        let mut owner = owner;
        start_thread.join().unwrap();
        assert_eq!(commits.load(Ordering::SeqCst), 1);
        assert!(
            with_current_background_epoch(epoch, || {
                commits.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .unwrap()
            .is_none(),
            "the old epoch cannot commit after Start"
        );
        assert_eq!(commits.load(Ordering::SeqCst), 1);

        owner.transition_to_live().unwrap();
        owner.transition_to_draining().unwrap();
        owner.transition_to_postprocess().unwrap();
        owner.finish().unwrap();
    }

    #[test]
    fn starting_wait_is_bounded_and_condvar_wakes_after_old_generation_drops() {
        let (_serial, _reset) = session_test();
        let old = acquire_unscoped_model_generation(ResidentModelKind::Whisper).unwrap();
        let owner = begin_recording_session().unwrap();
        assert!(!owner.wait_for_quiescence(Duration::from_millis(5)).unwrap());
        drop(old);
        assert!(owner
            .wait_for_quiescence(Duration::from_millis(50))
            .unwrap());
    }

    #[test]
    fn async_quiescence_moves_condvar_wait_off_the_runtime_worker() {
        let (_serial, _reset) = session_test();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let old = acquire_unscoped_model_generation(ResidentModelKind::Whisper).unwrap();
            let owner = begin_recording_session().unwrap();
            let dropper = tokio::task::spawn_blocking(move || {
                std::thread::sleep(Duration::from_millis(10));
                drop(old);
            });
            assert!(owner
                .wait_for_quiescence_async(Duration::from_secs(1))
                .await
                .unwrap());
            dropper.await.unwrap();
        });
    }

    #[test]
    fn incompatible_resident_kind_is_replaced_only_under_a_generation_lease() {
        let (_serial, _reset) = session_test();
        let whisper = acquire_unscoped_model_generation(ResidentModelKind::Whisper).unwrap();
        assert_eq!(
            lock_recording_model_coordinator().resident_kind,
            Some(ResidentModelKind::Whisper)
        );
        drop(whisper);

        let embedder = acquire_unscoped_model_generation(ResidentModelKind::Embedder).unwrap();
        assert_eq!(
            lock_recording_model_coordinator().resident_kind,
            Some(ResidentModelKind::Embedder),
            "the next kind is published only after incompatible caches are evicted"
        );
        drop(embedder);
        assert!(prepare_resident_model_kind(ResidentModelKind::Ner).is_err());
    }

    #[test]
    fn cloud_or_deterministic_background_wait_does_not_block_recording_start() {
        let (_serial, _reset) = session_test();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let background = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            // Represents a long deterministic scan or cloud await: deliberately NO local-model
            // generation lease while it is blocked.
            release_rx.recv().unwrap();
        });
        started_rx.recv().unwrap();

        let mut owner = begin_recording_session().unwrap();
        assert!(owner
            .wait_for_quiescence(Duration::from_millis(10))
            .unwrap());
        owner.transition_to_live().unwrap();
        owner.transition_to_draining().unwrap();
        owner.transition_to_postprocess().unwrap();
        owner.finish().unwrap();

        release_tx.send(()).unwrap();
        background.join().unwrap();
    }

    #[test]
    fn resident_live_model_blocks_brain_and_postprocess_until_it_drops() {
        let (_serial, _reset) = session_test();
        let mut owner = begin_recording_session().unwrap();
        owner.transition_to_live().unwrap();
        let token = owner.token();
        let live_residency =
            acquire_recording_model_generation(&token, ResidentModelKind::Whisper).unwrap();
        assert!(
            !recording_model_lane_is_free(&token),
            "live Whisper residency must make reaction/bullet/Brain preflight refuse spawning"
        );
        assert!(
            acquire_recording_model_generation(&token, ResidentModelKind::BrainGguf).is_err(),
            "a resident live model must prevent a second Brain/model residency"
        );
        owner.transition_to_draining().unwrap();
        assert!(owner.transition_to_postprocess().is_err());
        drop(live_residency);
        assert!(owner
            .wait_for_quiescence(Duration::from_millis(50))
            .unwrap());
        owner.transition_to_postprocess().unwrap();
        assert!(recording_model_lane_is_free(&token));
        owner.finish().unwrap();
    }

    /// The deterministic half of `heavy_op_permitted`'s logic: an already-failing RAM floor
    /// short-circuits to `false` regardless of the kernel pressure signal (which we can't
    /// deterministically mock — it shells out to the real `sysctl` on whatever machine runs this
    /// test). This is the ONE branch fully testable without depending on real system state.
    #[test]
    fn heavy_op_permitted_short_circuits_on_a_failing_ram_floor() {
        assert!(
            !heavy_op_permitted(false),
            "an already-failing RAM floor must refuse regardless of kernel pressure"
        );
    }

    /// `kernel_memory_pressure_level` must never panic and must return a value in the documented
    /// range (or None on a broken probe) — the one thing we CAN assert about the real machine
    /// this test runs on, without asserting a specific pressure level (which is real system state,
    /// not something this test controls).
    #[test]
    fn kernel_memory_pressure_level_never_panics() {
        if let Some(level) = kernel_memory_pressure_level() {
            assert!(
                level == 1 || level == 2 || level == 4,
                "unexpected kern.memorystatus_vm_pressure_level value: {level}"
            );
        }
    }

    /// The core contract: two `run_heavy` calls sharing ONE semaphore never overlap — the second
    /// call's closure only starts once the first has fully finished, even though both are handed
    /// to the blocking pool "concurrently" (a pool with >1 thread would otherwise happily run
    /// them at the same time). Proves the serialization is real, not just "it compiles".
    #[tokio::test]
    async fn run_heavy_serializes_two_concurrent_calls() {
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        let concurrent: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
        let max_concurrent: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));

        let make_task = |sem: Arc<tokio::sync::Semaphore>,
                         concurrent: Arc<AtomicUsize>,
                         max_concurrent: Arc<AtomicUsize>| {
            tokio::spawn(async move {
                run_heavy(&sem, move || -> Result<()> {
                    let now = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                    max_concurrent.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    concurrent.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
            })
        };

        let t1 = make_task(sem.clone(), concurrent.clone(), max_concurrent.clone());
        let t2 = make_task(sem.clone(), concurrent.clone(), max_concurrent.clone());
        t1.await.unwrap().unwrap();
        t2.await.unwrap().unwrap();

        assert_eq!(
            max_concurrent.load(Ordering::SeqCst),
            1,
            "two run_heavy calls on the same semaphore must never overlap"
        );
    }

    /// 2026-07-16 (pipeline hang watchdog, RED-before-GREEN): when the CALLER's future is dropped
    /// mid-flight — exactly what `tokio::time::timeout` around the pipeline's ASR stage does — the
    /// orphaned `spawn_blocking` closure keeps running (blocking closures are not cancellable) and
    /// MUST keep the one heavy-inference permit until it actually finishes. Before the fix the
    /// permit lived in `run_heavy`'s async future, so the timeout-drop released it while the
    /// blocking thread still ran — letting a second heavy inference co-run with the orphan (the
    /// whisper/diarizer co-residency class this semaphore exists to prevent).
    #[tokio::test]
    async fn run_heavy_keeps_the_permit_while_an_orphaned_closure_still_runs() {
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let finished_in = finished.clone();
        let fut = run_heavy(&sem, move || -> Result<()> {
            std::thread::sleep(Duration::from_millis(400));
            finished_in.store(true, Ordering::SeqCst);
            Ok(())
        });
        // Time the caller out long before the closure finishes — the future is DROPPED here,
        // but the blocking closure is already running (or queued) and will run to completion.
        let timed_out = tokio::time::timeout(Duration::from_millis(50), fut).await;
        assert!(timed_out.is_err(), "the caller must observe the timeout");

        // The orphaned closure is still running → the permit MUST still be held.
        assert!(
            !finished.load(Ordering::SeqCst),
            "closure must still be running at this point (timing precondition)"
        );
        assert_eq!(
            sem.available_permits(),
            0,
            "the permit must stay with the still-running orphaned closure, not the dropped future"
        );

        // Once the closure finishes, the permit is released (poll with a deadline — no flaky sleep).
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while sem.available_permits() == 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            finished.load(Ordering::SeqCst),
            "the orphaned closure must have run to completion"
        );
        assert_eq!(
            sem.available_permits(),
            1,
            "the permit must be released once the orphaned closure completes"
        );
    }

    /// The permit must release even when `f` returns `Err` — a failed heavy op must not
    /// permanently wedge every future heavy call behind a semaphore that never frees up.
    #[tokio::test]
    async fn run_heavy_releases_the_permit_on_error() {
        let sem = Arc::new(tokio::sync::Semaphore::new(1));

        let first: Result<()> =
            run_heavy(&sem, || Err(AppError::Other(anyhow::anyhow!("boom")))).await;
        assert!(first.is_err());

        // A second call must still be able to acquire — proves the failed first call didn't leak
        // its permit. Bounded by a timeout so a real leak fails the test instead of hanging it.
        let second = tokio::time::timeout(Duration::from_secs(2), run_heavy(&sem, || Ok(42))).await;
        assert_eq!(
            second.unwrap().unwrap(),
            42,
            "the permit must be free again after an Err"
        );
    }
}
