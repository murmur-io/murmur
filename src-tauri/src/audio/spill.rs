//! Crash-salvage of in-flight recordings (STAGE 2; STAGE 1 = the ghost-row reconcile in
//! `Db::reconcile_stuck_recordings`).
//!
//! CURRENT capture uses `audio::source`'s bounded ring + durable generation ledger. The RAM-mirror
//! `SpillWriter` and JSON sidecars below remain only to recover artifacts written by older builds;
//! no production Start path creates a new one.
//!
//! THE LEGACY GAP this closed: mic audio lived ONLY in RAM until Stop, so a crash / SIGKILL /
//! `tauri dev` hot-rebuild mid-recording lost the whole meeting's audio. Older builds mirrored the
//! growing RAM mic buffer to a plaintext spill file on disk during recording — a FULL mirror of the
//! RAM buffer (not a downsample/summary): raw mono `f32` at the
//! device rate, i.e. ~0.7 GB per hour at 48 kHz. Written on its own NON-real-time writer thread,
//! never in the cpal data callback — plus a
//! sidecar JSON naming the source sample rate + the paired far-side system-audio scratch WAV. On a
//! clean Stop the spill + sidecar are deleted (RAII, mirroring `pipeline::ScratchWav`). At the NEXT
//! launch, [`claim_inflight`] finds any surviving spill (⇒ a genuine crash), reconstructs the mic
//! samples and preserves the paired system scratch with an exclusive inflight clone while leaving
//! the original in place. The spawned salvage worker runs the meeting through the EXISTING post-Stop pipeline
//! ([`crate::pipeline::run_after_stop`]) → a real transcript + note.
//!
//! ── LOCK MODEL / PRIVACY ────────────────────────────────────────────────────────────────────
//! The spill is PLAINTEXT audio at rest DURING recording — the SAME exposure class as the helper
//! scratch WAVs (which also live plaintext in `$TMPDIR` while a call records). It is DELETED on a
//! clean Stop and lives under the app-data `inflight/` dir (NOT `$TMPDIR`), so neither the 1-hour
//! stale-scratch sweep nor the #174 storage auto-prune (which only ever touch `$TMPDIR` scratch /
//! the `audio/` dir's DB-referenced files) can delete a LIVE recording's spill out from under it.
//! Salvage OUTPUT rides the normal pipeline, so it INHERITS seal-aware export + the masked-DTO /
//! `convertFileSrc` gate (a salvaged meeting auto-filed into a locked folder is sealed by the same
//! path, and `audio_path` is nulled for a locked view) — no special-casing here. Logs carry the
//! meeting UUID + counts only; never audio content.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::audio::recorder::SampleReader;
use crate::error::{AppError, Result};
use crate::state::AppState;
use crate::storage::{Db, MeetingStatus};

/// App-data subdir (under `<data>/<app_dir_name()>/`) for in-flight recording spills. Deliberately a
/// sibling of `audio/`, NOT `$TMPDIR`, so the stale-scratch sweep + storage auto-prune never delete a
/// live recording (see the module header).
const INFLIGHT_SUBDIR: &str = "inflight";
/// Extension of the raw mic spill (mono `f32` little-endian at the device rate).
const SPILL_EXT: &str = "f32le";
/// Extension of the sidecar JSON ([`SpillSidecar`]).
const SIDECAR_EXT: &str = "json";
/// Suffix of the far-side system-audio scratch preserved in the inflight dir during a claim
/// (`<meeting_id>.sys.wav`). A compound suffix, so it is stripped/matched as a whole.
const SYSTEM_SCRATCH_SUFFIX: &str = "sys.wav";
/// How often the writer thread mirrors the newly-captured tail to disk. A crash loses at most this
/// much of the recording's tail (the RAM buffer holds everything up to the crash instant regardless).
const FLUSH_INTERVAL: Duration = Duration::from_millis(1000);
const LEGACY_PIPELINE_MAX_SECONDS: u64 = 120;
const MAX_LEGACY_SIDECAR_BYTES: u64 = 64 * 1024;
#[cfg(target_os = "macos")]
const LEGACY_NOFOLLOW_FLAG: i32 = 0x0000_0100;
#[cfg(not(target_os = "macos"))]
const LEGACY_NOFOLLOW_FLAG: i32 = 0;

/// `<app-data>/<app_dir_name()>/inflight`, created if absent. See [`INFLIGHT_SUBDIR`] for the
/// deliberate app-data (not `$TMPDIR`) placement.
pub fn inflight_dir() -> Result<PathBuf> {
    let base = dirs::data_dir()
        .ok_or_else(|| AppError::Storage("could not resolve app-data directory".into()))?;
    let dir = base
        .join(crate::state::app_dir_name())
        .join(INFLIGHT_SUBDIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::Storage(format!("create inflight dir: {e}")))?;
    Ok(dir)
}

/// Sidecar written ONCE at spill start: the source sample rate (needed to reconstruct a WAV) + the
/// paired far-side system-audio scratch path (so salvage can pair the "others" track). Small +
/// content-free (no audio, no PII beyond the meeting UUID).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpillSidecar {
    pub meeting_id: String,
    pub sample_rate: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_scratch_path: Option<String>,
}

/// PURE: mono `f32` samples → little-endian `f32` bytes (the on-disk spill encoding). The exact
/// inverse of [`f32le_to_samples`].
pub fn samples_to_f32le(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 4);
    for &s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// PURE: little-endian `f32` bytes → samples. A crash can truncate the spill mid-sample, so the
/// trailing partial (`< 4` bytes) is DROPPED — reconstruction never fabricates a garbage sample.
pub fn f32le_to_samples(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// One flush of the spill writer: append the samples captured SINCE `flushed` to `file` as f32le,
/// returning the new flushed offset. FAIL-CLOSED for the recording: on a write error it warns ONCE
/// (`warned`) and STILL advances past the failed chunk — the RAM buffer stays the primary source, so
/// at worst salvage reconstructs a slightly shorter tail; the recording itself is NEVER disrupted.
/// Extracted from the timing loop so the incremental-mirror logic is unit-testable without a thread.
fn flush_step(
    file: &mut impl Write,
    reader: &SampleReader,
    flushed: usize,
    warned: &mut bool,
) -> usize {
    let tail = reader.snapshot_from(flushed);
    if tail.is_empty() {
        return flushed;
    }
    // Encode + write OUTSIDE the sample lock (snapshot_from already released it by cloning the tail).
    let bytes = samples_to_f32le(&tail);
    if let Err(e) = file.write_all(&bytes) {
        if !*warned {
            tracing::warn!(target: "audio", error = %e, "recording spill write failed; continuing on the RAM buffer");
            *warned = true;
        }
    }
    flushed + tail.len()
}

/// Body of the NON-real-time writer thread: periodically mirror the newly-captured mic tail to the
/// spill file, until asked to stop. Never touches the cpal RT callback; reads only via the shared,
/// non-draining [`SampleReader`].
fn spill_loop(mut file: std::fs::File, reader: SampleReader, stop_rx: Receiver<()>) {
    let mut flushed = 0usize;
    let mut warned = false;
    loop {
        flushed = flush_step(&mut file, &reader, flushed, &mut warned);
        match stop_rx.recv_timeout(FLUSH_INTERVAL) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => continue,
        }
    }
    // Final catch-up flush at teardown (covers the brief crash-during-Stop window; a clean Stop
    // deletes the whole spill moments later regardless).
    let _ = flush_step(&mut file, &reader, flushed, &mut warned);
    let _ = file.flush();
}

/// Owns the STAGE-2 spill: a writer thread mirroring the live RAM mic buffer to `inflight/<id>.f32le`
/// plus the `inflight/<id>.json` sidecar. Its `Drop` is the CLEAN-STOP CLEANUP — it stops the thread
/// and deletes both files on EVERY exit path of `stop_recording` (success, `?`-error, panic),
/// mirroring `pipeline::ScratchWav` exactly. Only a genuine crash (where nothing drops) leaves the
/// files behind for [`claim_inflight`].
pub struct SpillWriter {
    stop_tx: Sender<()>,
    thread: Option<JoinHandle<()>>,
    spill_path: PathBuf,
    sidecar_path: PathBuf,
}

impl SpillWriter {
    /// Start mirroring `reader`'s live buffer for `meeting_id`. Writes the sidecar + creates the spill
    /// file synchronously (so later append errors are pure warnings), then spawns the writer thread.
    /// Returns `Err` only on a start-time IO failure — the caller treats that as "no spill" and records
    /// on the RAM buffer alone (fail-OPEN for recording, fail-CLOSED for salvage safety).
    pub fn start(
        meeting_id: &str,
        reader: SampleReader,
        sample_rate: u32,
        system_scratch_path: Option<PathBuf>,
    ) -> Result<Self> {
        let dir = inflight_dir()?;
        let spill_path = dir.join(format!("{meeting_id}.{SPILL_EXT}"));
        let sidecar_path = dir.join(format!("{meeting_id}.{SIDECAR_EXT}"));

        let sidecar = SpillSidecar {
            meeting_id: meeting_id.to_string(),
            sample_rate,
            system_scratch_path: system_scratch_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
        };
        let json = serde_json::to_vec(&sidecar)
            .map_err(|e| AppError::Audio(format!("spill sidecar encode: {e}")))?;
        std::fs::write(&sidecar_path, json)
            .map_err(|e| AppError::Audio(format!("spill sidecar write: {e}")))?;

        let file = std::fs::File::create(&spill_path)
            .map_err(|e| AppError::Audio(format!("spill create: {e}")))?;

        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("murmur-spill-writer".into())
            .spawn(move || spill_loop(file, reader, stop_rx))
            .map_err(|e| AppError::Audio(format!("spawn spill writer: {e}")))?;

        tracing::debug!(target: "audio", meeting_id = %meeting_id, "crash-salvage spill armed");
        Ok(Self {
            stop_tx,
            thread: Some(thread),
            spill_path,
            sidecar_path,
        })
    }
}

impl Drop for SpillWriter {
    fn drop(&mut self) {
        // Stop the writer thread (it wakes within FLUSH_INTERVAL), then remove the plaintext spill +
        // sidecar. Best-effort: an already-gone file or a remove error must never disrupt teardown.
        let _ = self.stop_tx.send(());
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.spill_path);
        let _ = std::fs::remove_file(&self.sidecar_path);
    }
}

// ── Startup salvage ─────────────────────────────────────────────────────────────────────────────

/// The startup salvage decision for ONE inflight sidecar, as a PURE fn (unit-tested). Split from the
/// IO so the branch logic is provable headless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SalvagePlan {
    /// Spill bytes present AND the meeting row is a genuine stuck `RECORDING` ghost → reconstruct +
    /// run the pipeline. Salvage OWNS this row's final status, so reconcile must SKIP it this launch.
    Salvage,
    /// Spill present but the row is NOT a stuck recording (a clean Stop already finished it, or the
    /// row is gone) → the spill is an orphan: delete spill + sidecar, do nothing else.
    DiscardOrphan,
    /// No usable spill bytes on disk → nothing to reconstruct; just remove the stale sidecar.
    NoSpill,
}

/// PURE salvage decision. `row_is_stuck_recording` = the meeting row exists AND is still in the
/// `RECORDING` status (i.e. a real crash left it mid-flight).
pub fn salvage_plan(spill_present: bool, row_is_stuck_recording: bool) -> SalvagePlan {
    match (spill_present, row_is_stuck_recording) {
        (true, true) => SalvagePlan::Salvage,
        (true, false) => SalvagePlan::DiscardOrphan,
        (false, _) => SalvagePlan::NoSpill,
    }
}

/// A claimed crashed recording, ready to reconstruct + run through the pipeline.
pub struct SalvageJob {
    pub meeting_id: String,
    pub spill_path: PathBuf,
    pub sidecar_path: PathBuf,
    pub sample_rate: u32,
    /// Reserved for the old paired-source compatibility worker. Production claims now leave this
    /// `None`: a far-side source is preserved with an exclusive copy-on-write clone and deferred,
    /// never loaded into the recovery pipeline.
    pub system_wav: Option<PathBuf>,
    mic_capability: ValidatedLegacyArtifact,
    sidecar_capability: ValidatedLegacySidecar,
    inflight_system_state: ValidatedLegacyArtifactState,
    external_scratch_absent_path: Option<PathBuf>,
}

#[derive(Clone)]
struct ValidatedLegacySidecar {
    path: PathBuf,
    owner: String,
    parsed: SpillSidecar,
    device: u64,
    inode: u64,
    byte_len: u64,
    sha256: String,
}

impl ValidatedLegacySidecar {
    fn verified(&self) -> Result<crate::audio::source::VerifiedFile> {
        let (_, proof) =
            crate::audio::source::read_existing_file_bounded(&self.path, MAX_LEGACY_SIDECAR_BYTES)?;
        if proof.device() != self.device
            || proof.inode() != self.inode
            || proof.byte_len() != self.byte_len
            || proof.sha256() != self.sha256
            || proof.basename() != format!("{}.{}", self.owner, SIDECAR_EXT)
        {
            return Err(AppError::Locked(
                "legacy recovery sidecar changed after startup preflight".into(),
            ));
        }
        Ok(proof)
    }

    fn revalidate(&self) -> Result<()> {
        self.verified().map(drop)
    }

    fn prepare_deletion(&self) -> Result<crate::audio::source::VerifiedDeletion> {
        let proof = self.verified()?;
        crate::audio::source::VerifiedDeletion::for_file(&self.path, &proof)
    }
}

#[derive(Clone)]
struct ValidatedLegacyScratch {
    canonical_path: PathBuf,
    device: u64,
    inode: u64,
    byte_len: Option<u64>,
    sha256: Option<String>,
}

impl ValidatedLegacyScratch {
    fn identity_len(&self) -> Result<u64> {
        let metadata = std::fs::symlink_metadata(&self.canonical_path).map_err(|error| {
            AppError::Locked(format!("legacy scratch identity unreadable: {error}"))
        })?;
        if !metadata.file_type().is_file()
            || metadata.nlink() != 1
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
            || self
                .byte_len
                .is_some_and(|byte_len| metadata.len() != byte_len)
            || std::fs::canonicalize(&self.canonical_path).ok().as_ref()
                != Some(&self.canonical_path)
        {
            return Err(AppError::Locked(
                "legacy scratch identity changed after startup preflight".into(),
            ));
        }
        Ok(metadata.len())
    }

    fn verified(&self) -> Result<crate::audio::source::VerifiedFile> {
        self.identity_len()?;
        let proof = crate::audio::source::verify_existing_file(&self.canonical_path)?;
        if proof.device() != self.device
            || proof.inode() != self.inode
            || self
                .byte_len
                .is_some_and(|byte_len| proof.byte_len() != byte_len)
            || self
                .sha256
                .as_deref()
                .is_some_and(|sha256| proof.sha256() != sha256)
            || std::fs::canonicalize(&self.canonical_path).ok().as_ref()
                != Some(&self.canonical_path)
        {
            return Err(AppError::Locked(
                "legacy scratch identity changed after startup preflight".into(),
            ));
        }
        Ok(proof)
    }

    fn revalidate(&self) -> Result<()> {
        self.identity_len().map(drop)
    }

    fn prepare_deletion(&self) -> Result<crate::audio::source::VerifiedDeletion> {
        let proof = self.verified()?;
        crate::audio::source::VerifiedDeletion::for_file(&self.canonical_path, &proof)
    }
}

#[derive(Clone)]
struct ValidatedLegacyArtifact {
    path: PathBuf,
    device: u64,
    inode: u64,
    byte_len: Option<u64>,
    sha256: Option<String>,
}

impl ValidatedLegacyArtifact {
    fn from_metadata(path: PathBuf, metadata: &std::fs::Metadata) -> Self {
        Self {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            byte_len: None,
            sha256: None,
        }
    }

    fn from_proof(path: PathBuf, proof: &crate::audio::source::VerifiedFile) -> Self {
        Self {
            path,
            device: proof.device(),
            inode: proof.inode(),
            byte_len: Some(proof.byte_len()),
            sha256: Some(proof.sha256().to_string()),
        }
    }

    fn verified(&self) -> Result<crate::audio::source::VerifiedFile> {
        self.identity_len()?;
        let proof = crate::audio::source::verify_existing_file(&self.path)?;
        if proof.device() != self.device
            || proof.inode() != self.inode
            || self
                .byte_len
                .is_some_and(|byte_len| proof.byte_len() != byte_len)
            || self
                .sha256
                .as_deref()
                .is_some_and(|sha256| proof.sha256() != sha256)
        {
            return Err(AppError::Locked(
                "legacy recovery artifact changed after startup preflight".into(),
            ));
        }
        Ok(proof)
    }

    fn identity_len(&self) -> Result<u64> {
        let metadata = std::fs::symlink_metadata(&self.path).map_err(|error| {
            AppError::Audio(format!(
                "inspect legacy recovery artifact identity: {error}"
            ))
        })?;
        if !metadata.file_type().is_file()
            || metadata.nlink() != 1
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
            || self
                .byte_len
                .is_some_and(|byte_len| metadata.len() != byte_len)
        {
            return Err(AppError::Locked(
                "legacy recovery artifact identity changed after startup preflight".into(),
            ));
        }
        Ok(metadata.len())
    }

    fn freeze(&self) -> Result<Self> {
        let proof = self.verified()?;
        Ok(Self::from_proof(self.path.clone(), &proof))
    }

    fn prepare_deletion(&self) -> Result<crate::audio::source::VerifiedDeletion> {
        let proof = self.verified()?;
        crate::audio::source::VerifiedDeletion::for_file(&self.path, &proof)
    }
}

#[derive(Clone)]
enum ValidatedLegacyArtifactState {
    Absent(PathBuf),
    Present(ValidatedLegacyArtifact),
}

#[derive(Clone)]
enum ValidatedLegacyDirectoryState {
    Absent(PathBuf),
    Present {
        path: PathBuf,
        canonical_path: PathBuf,
        device: u64,
        inode: u64,
    },
}

impl ValidatedLegacyDirectoryState {
    fn path(&self) -> &Path {
        match self {
            Self::Absent(path) | Self::Present { path, .. } => path,
        }
    }

    fn revalidate(&self, requested_path: &Path) -> Result<bool> {
        if requested_path != self.path() {
            return Err(AppError::Locked(
                "legacy recovery directory path changed after startup preflight".into(),
            ));
        }
        match self {
            Self::Absent(path) if legacy_node_proven_absent(path)? => Ok(false),
            Self::Absent(_) => Err(AppError::Locked(
                "legacy recovery directory appeared after startup preflight".into(),
            )),
            Self::Present {
                path,
                canonical_path,
                device,
                inode,
            } => {
                let metadata = std::fs::symlink_metadata(path).map_err(|error| {
                    AppError::Locked(format!(
                        "legacy recovery directory identity unreadable: {error}"
                    ))
                })?;
                if !metadata.file_type().is_dir()
                    || metadata.dev() != *device
                    || metadata.ino() != *inode
                    || std::fs::canonicalize(path).ok().as_ref() != Some(canonical_path)
                {
                    return Err(AppError::Locked(
                        "legacy recovery directory changed after startup preflight".into(),
                    ));
                }
                Ok(true)
            }
        }
    }

    fn open_verified(&self) -> Result<Option<File>> {
        let Self::Present {
            path,
            device,
            inode,
            ..
        } = self
        else {
            self.revalidate(self.path())?;
            return Ok(None);
        };
        self.revalidate(path)?;
        let directory = File::open(path)
            .map_err(|error| AppError::Audio(format!("open legacy recovery directory: {error}")))?;
        let metadata = directory.metadata().map_err(|error| {
            AppError::Audio(format!("stat opened legacy recovery directory: {error}"))
        })?;
        if !metadata.is_dir() || metadata.dev() != *device || metadata.ino() != *inode {
            return Err(AppError::Locked(
                "opened legacy recovery directory does not match startup authority".into(),
            ));
        }
        Ok(Some(directory))
    }
}

impl ValidatedLegacyArtifactState {
    fn path(&self) -> &Path {
        match self {
            Self::Absent(path) => path,
            Self::Present(capability) => &capability.path,
        }
    }

    fn verified_len(&self) -> Result<Option<u64>> {
        match self {
            Self::Absent(path) if legacy_node_proven_absent(path)? => Ok(None),
            Self::Absent(_) => Err(AppError::Locked(
                "legacy recovery artifact appeared after startup preflight".into(),
            )),
            Self::Present(capability) => Ok(Some(capability.identity_len()?)),
        }
    }

    fn prepare_optional_deletion(&self) -> Result<Option<crate::audio::source::VerifiedDeletion>> {
        match self {
            Self::Absent(path) if legacy_node_proven_absent(path)? => Ok(None),
            Self::Absent(_) => Err(AppError::Locked(
                "legacy recovery artifact appeared after startup preflight".into(),
            )),
            Self::Present(capability) => capability.prepare_deletion().map(Some),
        }
    }

    fn present_capability(&self) -> Result<Option<ValidatedLegacyArtifact>> {
        match self {
            Self::Absent(path) if legacy_node_proven_absent(path)? => Ok(None),
            Self::Absent(_) => Err(AppError::Locked(
                "legacy recovery artifact appeared after startup preflight".into(),
            )),
            Self::Present(capability) => Ok(Some(capability.freeze()?)),
        }
    }
}

pub(crate) struct LegacyRecoveryPreflight {
    pub(crate) scratch_protection: crate::audio::aec::StaleScratchProtection,
    directory: ValidatedLegacyDirectoryState,
    legacy_names: HashSet<String>,
    scratch_owner_by_path: HashMap<PathBuf, String>,
    scratch_by_meeting: HashMap<String, ValidatedLegacyScratch>,
    sidecar_by_meeting: HashMap<String, ValidatedLegacySidecar>,
    mic_by_meeting: HashMap<String, ValidatedLegacyArtifactState>,
    inflight_system_by_meeting: HashMap<String, ValidatedLegacyArtifactState>,
}

impl LegacyRecoveryPreflight {
    fn scratch_for(&self, meeting_id: &str) -> Result<Option<&ValidatedLegacyScratch>> {
        let mut owned_paths = self
            .scratch_owner_by_path
            .iter()
            .filter_map(|(path, owner)| (owner == meeting_id).then_some(path));
        let owned_path = owned_paths.next();
        if owned_paths.next().is_some() {
            return Err(AppError::Locked(
                "legacy scratch has duplicate startup ownership".into(),
            ));
        }
        match (self.scratch_by_meeting.get(meeting_id), owned_path) {
            (Some(scratch), Some(path)) if path == &scratch.canonical_path => Ok(Some(scratch)),
            (Some(_), _) => Err(AppError::Locked(
                "legacy scratch ownership changed after startup preflight".into(),
            )),
            (None, Some(path)) if legacy_node_proven_absent(path)? => Ok(None),
            (None, Some(_)) => Err(AppError::Locked(
                "legacy scratch appeared after startup preflight".into(),
            )),
            (None, None) => Ok(None),
        }
    }

    fn take_scratch(&mut self, meeting_id: &str) -> Result<Option<ValidatedLegacyScratch>> {
        let had_present_capability = self.scratch_for(meeting_id)?.is_some();
        if !had_present_capability {
            return Ok(None);
        }
        Ok(self.scratch_by_meeting.remove(meeting_id))
    }

    fn absent_scratch_path_for(&self, meeting_id: &str) -> Result<Option<PathBuf>> {
        if self.scratch_for(meeting_id)?.is_some() {
            return Err(AppError::Locked(
                "legacy external scratch is present, not absent".into(),
            ));
        }
        let mut paths = self
            .scratch_owner_by_path
            .iter()
            .filter_map(|(path, owner)| (owner == meeting_id).then_some(path.clone()));
        let path = paths.next();
        if paths.next().is_some() {
            return Err(AppError::Locked(
                "legacy external scratch has duplicate ownership".into(),
            ));
        }
        Ok(path)
    }

    fn sidecar_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.sidecar_by_meeting.keys().cloned().collect();
        ids.sort();
        ids
    }

    fn sidecar_for(&self, meeting_id: &str) -> Result<ValidatedLegacySidecar> {
        let capability = self.sidecar_by_meeting.get(meeting_id).ok_or_else(|| {
            AppError::Locked("legacy recovery sidecar was not in the startup inventory".into())
        })?;
        capability.revalidate()?;
        Ok(capability.clone())
    }

    fn mic_for(&self, meeting_id: &str) -> Result<ValidatedLegacyArtifactState> {
        let state = self.mic_by_meeting.get(meeting_id).ok_or_else(|| {
            AppError::Locked("legacy mic source was not in the startup inventory".into())
        })?;
        state.verified_len()?;
        Ok(state.clone())
    }

    fn inflight_system_for(&self, meeting_id: &str) -> Result<ValidatedLegacyArtifactState> {
        let state = self
            .inflight_system_by_meeting
            .get(meeting_id)
            .ok_or_else(|| {
                AppError::Locked(
                    "legacy inflight system clone was not in the startup inventory".into(),
                )
            })?;
        state.verified_len()?;
        Ok(state.clone())
    }
}

/// SYNCHRONOUS startup CLAIM — runs in `lib.rs` only AFTER a clean helper scan, then BEFORE
/// `reconcile_stuck_recordings` and the stale scratch sweep. The clean scan is load-bearing: a
/// surviving legacy helper may still have a scratch inode open after rename. Scans the inflight
/// dir; per sidecar it decides via [`salvage_plan`]:
/// - `Salvage`: emit a mic-only [`SalvageJob`]. A paired system scratch is preserved with an
///   exclusive copy-on-write clone outside `$TMPDIR` and deferred rather than loaded into RAM; the
///   claimed `meeting_id` is returned so reconcile skips it.
/// - `DiscardOrphan` / `NoSpill`: delete the stale spill + sidecar in place (NEVER touches the
///   `audio/` dir or any real recording).
///
/// Returns `(jobs, claimed_meeting_ids)`. Per-owner damage is preserved and skipped, but a global
/// post-preflight inventory/directory ambiguity is returned as an error so startup fails closed;
/// nothing here deletes an un-salvaged spill.
pub(crate) fn claim_inflight(
    db: &Db,
    preflight: &mut LegacyRecoveryPreflight,
) -> Result<(Vec<SalvageJob>, Vec<String>)> {
    let dir = preflight.directory.path().to_path_buf();
    claim_inflight_with_preflight(&dir, db, preflight)
}

/// Synchronous pre-window guard for legacy crash artifacts. Every readable sidecar publishes its
/// SQLCipher marker before normal UI/MCP/cleanup workers may start, and contributes its exact existing temp
/// scratch path to the stale-sweep protection set. A locked owner or any ambiguous legacy filename,
/// sidecar, ownership row, symlink, or artifact identity aborts ordinary startup: historical
/// plaintext is preserved and never falsely presented as sealed.
pub(crate) fn startup_legacy_recovery_preflight(db: &Db) -> Result<LegacyRecoveryPreflight> {
    startup_legacy_recovery_preflight_in(&inflight_dir()?, db)
}

fn read_stable_legacy_sidecar(
    path: &Path,
    filename_owner: &str,
    protection: &mut crate::audio::aec::StaleScratchProtection,
) -> Result<ValidatedLegacySidecar> {
    let (bytes, before) = crate::audio::source::read_existing_file_bounded(
        path,
        MAX_LEGACY_SIDECAR_BYTES,
    )
    .map_err(|error| {
        tracing::warn!(target: "startup", error = %error, "legacy recovery sidecar identity unreadable; aborting before content surfaces start");
        ambiguous_legacy_preflight(protection, "legacy recovery sidecar is ambiguous")
    })?;
    let (_, after) = crate::audio::source::read_existing_file_bounded(
        path,
        MAX_LEGACY_SIDECAR_BYTES,
    )
    .map_err(|error| {
        tracing::warn!(target: "startup", error = %error, "legacy recovery sidecar changed while reading; aborting before content surfaces start");
        ambiguous_legacy_preflight(protection, "legacy recovery sidecar changed while reading")
    })?;
    if before.device() != after.device()
        || before.inode() != after.inode()
        || before.byte_len() != after.byte_len()
        || before.sha256() != after.sha256()
    {
        return Err(ambiguous_legacy_preflight(
            protection,
            "legacy recovery sidecar identity changed during preflight",
        ));
    }
    let parsed: SpillSidecar = serde_json::from_slice(&bytes).map_err(|error| {
        tracing::warn!(target: "startup", error = %error, "legacy recovery sidecar malformed; aborting before content surfaces start");
        ambiguous_legacy_preflight(protection, "legacy recovery sidecar is malformed")
    })?;
    if !safe_legacy_owner_id(filename_owner)
        || parsed.meeting_id != filename_owner
        || !(8_000..=384_000).contains(&parsed.sample_rate)
    {
        return Err(ambiguous_legacy_preflight(
            protection,
            "legacy recovery sidecar has invalid ownership metadata",
        ));
    }
    Ok(ValidatedLegacySidecar {
        path: path.to_path_buf(),
        owner: filename_owner.to_string(),
        parsed,
        device: after.device(),
        inode: after.inode(),
        byte_len: after.byte_len(),
        sha256: after.sha256().to_string(),
    })
}

fn startup_legacy_recovery_preflight_in(dir: &Path, db: &Db) -> Result<LegacyRecoveryPreflight> {
    startup_legacy_recovery_preflight_in_with_temp_root(dir, db, &std::env::temp_dir())
}

/// Same preflight with the selected temporary root injected. Production always passes the OS temp
/// directory above; tests use an isolated root so the exact protected path and an unrelated stale
/// sibling are enumerated by one sweep without touching the developer's live `$TMPDIR`.
fn startup_legacy_recovery_preflight_in_with_temp_root(
    dir: &Path,
    db: &Db,
    temp_root: &Path,
) -> Result<LegacyRecoveryPreflight> {
    let mut protection = crate::audio::aec::StaleScratchProtection::default();
    let directory = validate_legacy_recovery_directory(dir, &mut protection)?;
    directory.revalidate(dir)?;
    repair_marker_only_legacy_recovery(dir, db, &mut protection, &directory)?;
    if !directory.revalidate(dir)? {
        return Ok(LegacyRecoveryPreflight {
            scratch_protection: protection,
            directory,
            legacy_names: HashSet::new(),
            scratch_owner_by_path: HashMap::new(),
            scratch_by_meeting: HashMap::new(),
            sidecar_by_meeting: HashMap::new(),
            mic_by_meeting: HashMap::new(),
            inflight_system_by_meeting: HashMap::new(),
        });
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ambiguous_legacy_preflight(
                &mut protection,
                "legacy recovery directory disappeared during preflight",
            ));
        }
        Err(error) => {
            tracing::warn!(target: "startup", error = %error, "legacy recovery directory unreadable; aborting before content surfaces start");
            return Err(ambiguous_legacy_preflight(
                &mut protection,
                "legacy recovery directory is unreadable",
            ));
        }
    };
    let mut sidecars = Vec::new();
    let mut mic_artifact_owners = Vec::new();
    let mut system_artifact_owners = Vec::new();
    let mut legacy_names = HashSet::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(target: "startup", error = %error, "legacy recovery directory entry unreadable; aborting before content surfaces start");
                return Err(ambiguous_legacy_preflight(
                    &mut protection,
                    "legacy recovery directory entry is unreadable",
                ));
            }
        };
        let file_name = match entry.file_name().into_string() {
            Ok(file_name) => file_name,
            Err(_) => {
                return Err(ambiguous_legacy_preflight(
                    &mut protection,
                    "legacy recovery filename is not valid UTF-8",
                ));
            }
        };
        let legacy_kind = if let Some(owner) = file_name.strip_suffix(".json") {
            Some((owner, true))
        } else if let Some(owner) = file_name.strip_suffix(".f32le") {
            Some((owner, false))
        } else {
            file_name
                .strip_suffix(".sys.wav")
                .map(|owner| (owner, false))
        };
        let Some((owner, is_sidecar)) = legacy_kind else {
            continue;
        };
        legacy_names.insert(file_name.clone());
        if owner.is_empty() {
            return Err(ambiguous_legacy_preflight(
                &mut protection,
                "legacy recovery filename has no meeting id",
            ));
        }
        if is_sidecar {
            let file_type = entry.file_type().map_err(|error| {
                tracing::warn!(target: "startup", error = %error, "legacy recovery sidecar metadata unreadable; aborting before content surfaces start");
                ambiguous_legacy_preflight(
                    &mut protection,
                    "legacy recovery sidecar metadata is unreadable",
                )
            })?;
            if !file_type.is_file() {
                return Err(ambiguous_legacy_preflight(
                    &mut protection,
                    "legacy recovery sidecar is not a regular file",
                ));
            }
            sidecars.push((owner.to_string(), entry.path()));
        } else if file_name.ends_with(".f32le") {
            mic_artifact_owners.push(owner.to_string());
        } else {
            system_artifact_owners.push((owner.to_string(), entry.path()));
        }
    }

    if mic_artifact_owners.iter().any(|owner| {
        !sidecars
            .iter()
            .any(|(sidecar_owner, _)| sidecar_owner == owner)
    }) {
        return Err(ambiguous_legacy_preflight(
            &mut protection,
            "legacy recovery artifact has no readable ownership sidecar",
        ));
    }
    // A preserved far-side `.sys.wav` clone can legitimately outlive the sidecar after a prior
    // terminal cleanup cut. Admit only an exact safe single-link regular candidate and publish
    // durable ownership; startup never guesses that a sidecar-less track is disposable.
    let mut locked_recovery = false;
    for (owner, path) in &system_artifact_owners {
        if !safe_legacy_owner_id(owner) {
            return Err(ambiguous_legacy_preflight(
                &mut protection,
                "legacy recovery system owner is invalid",
            ));
        }
        if !sidecars
            .iter()
            .any(|(sidecar_owner, _)| sidecar_owner == owner)
        {
            let capability = match validate_legacy_artifact(
                path,
                &mut protection,
                "orphaned legacy system scratch is ambiguous",
            )? {
                ValidatedLegacyArtifactState::Present(capability) => capability,
                ValidatedLegacyArtifactState::Absent(_) => {
                    return Err(ambiguous_legacy_preflight(
                        &mut protection,
                        "orphaned legacy system scratch disappeared during preflight",
                    ));
                }
            };
            // A sidecar-less historical far-side track may be the only surviving copy. Never hash
            // or delete it at startup. Publish durable meeting ownership so lock/move/delete cannot
            // race it; a locked owner aborts before any content surface starts. Missing-meeting
            // candidates remain validated, untouched terminal orphans.
            let _ = capability;
            locked_recovery |= db.mark_legacy_recording_recovery_pending(owner)?;
        }
    }

    let canonical_temp_root = std::fs::canonicalize(temp_root).map_err(|error| {
        tracing::warn!(target: "startup", error = %error, "temporary directory identity unreadable; aborting legacy recovery preflight");
        ambiguous_legacy_preflight(
            &mut protection,
            "temporary directory identity is unreadable",
        )
    })?;
    let mut scratch_owner_by_path = HashMap::new();
    let mut scratch_by_meeting = HashMap::new();
    let mut sidecar_by_meeting = HashMap::new();
    let mut mic_by_meeting = HashMap::new();
    let mut inflight_system_by_meeting = HashMap::new();
    for (owner, sidecar_path) in sidecars {
        let validated_sidecar = read_stable_legacy_sidecar(&sidecar_path, &owner, &mut protection)?;
        let sidecar = validated_sidecar.parsed.clone();

        // Publish ownership before inspecting any referenced plaintext artifact. If a later path
        // check fails, the marker remains durable and startup still aborts before any content
        // surface or cleanup worker can observe/delete the ambiguous files.
        let folder_locked = db.mark_legacy_recording_recovery_pending(&sidecar.meeting_id)?;

        let mic_state = validate_legacy_artifact(
            &dir.join(format!("{}.{SPILL_EXT}", sidecar.meeting_id)),
            &mut protection,
            "legacy recovery mic spill is ambiguous",
        )?;
        let inflight_system_state = validate_legacy_artifact(
            &dir.join(format!("{}.{}", sidecar.meeting_id, SYSTEM_SCRATCH_SUFFIX)),
            &mut protection,
            "legacy recovery inflight system clone is ambiguous",
        )?;
        if mic_by_meeting
            .insert(sidecar.meeting_id.clone(), mic_state)
            .is_some()
            || inflight_system_by_meeting
                .insert(sidecar.meeting_id.clone(), inflight_system_state)
                .is_some()
        {
            return Err(ambiguous_legacy_preflight(
                &mut protection,
                "legacy recovery has duplicate audio ownership",
            ));
        }
        if let Some(path) = sidecar.system_scratch_path.as_deref() {
            let (canonical_candidate, capability) = validate_system_scratch_capability(
                Path::new(path),
                &canonical_temp_root,
                &mut protection,
            )?;
            protection
                .protect_validated_candidate(&canonical_candidate)
                .map_err(|error| {
                    tracing::warn!(target: "startup", error = %error, "legacy recovery scratch candidate cannot be sweep-protected");
                    ambiguous_legacy_preflight(
                        &mut protection,
                        "legacy recovery scratch candidate protection is ambiguous",
                    )
                })?;
            if scratch_owner_by_path
                .insert(canonical_candidate, sidecar.meeting_id.clone())
                .is_some()
            {
                return Err(ambiguous_legacy_preflight(
                    &mut protection,
                    "legacy scratch path is claimed by multiple recovery sidecars",
                ));
            }
            if let Some(capability) = capability {
                protection.protect_existing(&capability.canonical_path).map_err(|error| {
                    tracing::warn!(target: "startup", error = %error, "legacy recovery scratch identity ambiguous; aborting before content surfaces start");
                    ambiguous_legacy_preflight(
                        &mut protection,
                        "legacy recovery scratch identity is ambiguous",
                    )
                })?;
                scratch_by_meeting.insert(sidecar.meeting_id.clone(), capability);
            }
        }
        if sidecar_by_meeting
            .insert(sidecar.meeting_id.clone(), validated_sidecar)
            .is_some()
        {
            return Err(ambiguous_legacy_preflight(
                &mut protection,
                "legacy recovery has duplicate sidecar ownership",
            ));
        }
        locked_recovery |= folder_locked;
    }
    if locked_recovery {
        return Err(AppError::Locked(
            "legacy recording recovery requires an authenticated folder unlock".into(),
        ));
    }
    directory.revalidate(dir)?;
    Ok(LegacyRecoveryPreflight {
        scratch_protection: protection,
        directory,
        legacy_names,
        scratch_owner_by_path,
        scratch_by_meeting,
        sidecar_by_meeting,
        mic_by_meeting,
        inflight_system_by_meeting,
    })
}

fn validate_legacy_recovery_directory(
    dir: &Path,
    protection: &mut crate::audio::aec::StaleScratchProtection,
) -> Result<ValidatedLegacyDirectoryState> {
    let before = match std::fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ValidatedLegacyDirectoryState::Absent(dir.to_path_buf()));
        }
        Err(error) => {
            tracing::warn!(target: "startup", error = %error, "legacy recovery directory metadata unreadable");
            return Err(ambiguous_legacy_preflight(
                protection,
                "legacy recovery directory metadata is unreadable",
            ));
        }
    };
    if !before.file_type().is_dir() {
        return Err(ambiguous_legacy_preflight(
            protection,
            "legacy recovery directory is not a real directory",
        ));
    }
    let canonical_path = std::fs::canonicalize(dir).map_err(|error| {
        tracing::warn!(target: "startup", error = %error, "legacy recovery directory canonical identity unreadable");
        ambiguous_legacy_preflight(
            protection,
            "legacy recovery directory identity is ambiguous",
        )
    })?;
    let after = std::fs::symlink_metadata(dir).map_err(|error| {
        tracing::warn!(target: "startup", error = %error, "legacy recovery directory changed during identity check");
        ambiguous_legacy_preflight(
            protection,
            "legacy recovery directory changed during preflight",
        )
    })?;
    if !after.file_type().is_dir() || after.dev() != before.dev() || after.ino() != before.ino() {
        return Err(ambiguous_legacy_preflight(
            protection,
            "legacy recovery directory changed during preflight",
        ));
    }
    Ok(ValidatedLegacyDirectoryState::Present {
        path: dir.to_path_buf(),
        canonical_path,
        device: after.dev(),
        inode: after.ino(),
    })
}

/// Repair the crash cut after the final sidecar unlink but before SQLCipher marker removal. A
/// marker can be cleared without reading content only when its three exact inflight names are all
/// proven absent. Any surviving node (including a broken symlink) or metadata ambiguity keeps the
/// marker and aborts ordinary startup before mutation/content surfaces begin.
fn repair_marker_only_legacy_recovery(
    dir: &Path,
    db: &Db,
    protection: &mut crate::audio::aec::StaleScratchProtection,
    directory: &ValidatedLegacyDirectoryState,
) -> Result<()> {
    for meeting_id in db.pending_legacy_recording_recovery_ids()? {
        directory.revalidate(dir)?;
        if !safe_legacy_owner_id(&meeting_id) {
            return Err(ambiguous_legacy_preflight(
                protection,
                "legacy recovery marker has an invalid meeting id",
            ));
        }
        let sidecar = dir.join(format!("{meeting_id}.{SIDECAR_EXT}"));
        if !legacy_node_proven_absent(&sidecar)? {
            continue;
        }
        let spill = dir.join(format!("{meeting_id}.{SPILL_EXT}"));
        let system = dir.join(format!("{meeting_id}.{SYSTEM_SCRATCH_SUFFIX}"));
        let spill_absent = legacy_node_proven_absent(&spill)?;
        let system_absent = legacy_node_proven_absent(&system)?;
        if spill_absent && system_absent {
            // The DB marker is the final authority. Rebind the directory immediately before its
            // removal so a path replacement observed during the absence checks fails closed.
            directory.revalidate(dir)?;
            db.clear_legacy_recording_recovery_pending(&meeting_id)?;
            tracing::warn!(target: "startup", meeting_id = %meeting_id, "cleared legacy recovery ownership after proving every inflight artifact absent");
        } else if spill_absent {
            // A system-only legacy track can be the sole surviving far-side copy. Its SQLCipher
            // marker deliberately survives without a sidecar; the inventory pass below validates
            // the exact inode and checks locked-folder ownership before any content surface starts.
            continue;
        } else {
            return Err(ambiguous_legacy_preflight(
                protection,
                "legacy recovery marker has an artifact but no ownership sidecar",
            ));
        }
    }
    Ok(())
}

fn legacy_node_proven_absent(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(AppError::Audio(format!(
            "inspect legacy recovery pathname: {error}"
        ))),
    }
}

fn safe_legacy_owner_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn legacy_filename_owner(file_name: &str) -> Option<&str> {
    file_name
        .strip_suffix(".json")
        .or_else(|| file_name.strip_suffix(".f32le"))
        .or_else(|| file_name.strip_suffix(".sys.wav"))
}

fn legacy_directory_inventory(dir: &Path) -> Result<HashSet<String>> {
    let mut names = HashSet::new();
    let entries = std::fs::read_dir(dir).map_err(|error| {
        AppError::Audio(format!("inventory legacy recovery directory: {error}"))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            AppError::Audio(format!("inventory legacy recovery entry: {error}"))
        })?;
        let file_name = entry
            .file_name()
            .into_string()
            .map_err(|_| AppError::Locked("legacy recovery filename is not valid UTF-8".into()))?;
        if legacy_filename_owner(&file_name).is_some() {
            names.insert(file_name);
        }
    }
    Ok(names)
}

fn validate_system_scratch_capability(
    path: &Path,
    canonical_temp_root: &Path,
    protection: &mut crate::audio::aec::StaleScratchProtection,
) -> Result<(PathBuf, Option<ValidatedLegacyScratch>)> {
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err(ambiguous_legacy_preflight(
            protection,
            "legacy recovery scratch path is not a direct absolute path",
        ));
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            ambiguous_legacy_preflight(protection, "legacy recovery scratch filename is invalid")
        })?;
    if !crate::audio::aec::is_legacy_system_scratch_filename(file_name) {
        return Err(ambiguous_legacy_preflight(
            protection,
            "legacy recovery scratch filename is not helper-owned",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        ambiguous_legacy_preflight(protection, "legacy recovery scratch has no parent")
    })?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(|error| {
        tracing::warn!(target: "startup", error = %error, "legacy scratch parent identity unreadable; aborting before content surfaces start");
        ambiguous_legacy_preflight(protection, "legacy recovery scratch parent is unreadable")
    })?;
    if canonical_parent != canonical_temp_root {
        return Err(ambiguous_legacy_preflight(
            protection,
            "legacy recovery scratch is outside the direct temporary directory",
        ));
    }
    let canonical_candidate = canonical_temp_root.join(file_name);
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((canonical_candidate, None));
        }
        Err(error) => {
            tracing::warn!(target: "startup", error = %error, "legacy recovery scratch metadata unreadable; aborting before content surfaces start");
            return Err(ambiguous_legacy_preflight(
                protection,
                "legacy recovery scratch metadata is unreadable",
            ));
        }
    };
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return Err(ambiguous_legacy_preflight(
            protection,
            "legacy recovery scratch is not an owned single-link regular file",
        ));
    }
    let canonical_path = std::fs::canonicalize(path).map_err(|error| {
        tracing::warn!(target: "startup", error = %error, "legacy recovery scratch identity unreadable; aborting before content surfaces start");
        ambiguous_legacy_preflight(protection, "legacy recovery scratch identity is unreadable")
    })?;
    if canonical_path != canonical_candidate {
        return Err(ambiguous_legacy_preflight(
            protection,
            "legacy recovery scratch canonical identity is ambiguous",
        ));
    }
    let after = std::fs::symlink_metadata(&canonical_path).map_err(|error| {
        tracing::warn!(target: "startup", error = %error, "legacy recovery scratch identity changed during preflight");
        ambiguous_legacy_preflight(protection, "legacy recovery scratch proof is ambiguous")
    })?;
    if !after.file_type().is_file()
        || after.nlink() != 1
        || after.dev() != metadata.dev()
        || after.ino() != metadata.ino()
    {
        return Err(ambiguous_legacy_preflight(
            protection,
            "legacy recovery scratch changed during preflight",
        ));
    }
    Ok((
        canonical_candidate,
        Some(ValidatedLegacyScratch {
            canonical_path,
            device: after.dev(),
            inode: after.ino(),
            // Large legacy audio may still be growing until the clean-helper scan completes.
            // Freeze length + content only in claim, after that scan, never in startup preflight.
            byte_len: None,
            sha256: None,
        }),
    ))
}

fn validate_legacy_artifact(
    path: &Path,
    protection: &mut crate::audio::aec::StaleScratchProtection,
    reason: &'static str,
) -> Result<ValidatedLegacyArtifactState> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ValidatedLegacyArtifactState::Absent(path.to_path_buf()));
        }
        Err(error) => {
            tracing::warn!(target: "startup", error = %error, "legacy recovery artifact metadata unreadable; aborting before content surfaces start");
            return Err(ambiguous_legacy_preflight(protection, reason));
        }
    };
    // `symlink_metadata` does not follow links, so `is_file()` is false for both symlinks and
    // non-file nodes. Recovery accepts only an identity-stable regular file.
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return Err(ambiguous_legacy_preflight(protection, reason));
    }
    if let Err(error) = std::fs::canonicalize(path) {
        tracing::warn!(target: "startup", error = %error, "legacy recovery artifact identity unreadable; aborting before content surfaces start");
        return Err(ambiguous_legacy_preflight(protection, reason));
    }
    let after = std::fs::symlink_metadata(path).map_err(|error| {
        tracing::warn!(target: "startup", error = %error, "legacy recovery artifact identity changed during preflight");
        ambiguous_legacy_preflight(protection, reason)
    })?;
    if !after.file_type().is_file()
        || after.nlink() != 1
        || after.dev() != metadata.dev()
        || after.ino() != metadata.ino()
    {
        return Err(ambiguous_legacy_preflight(protection, reason));
    }
    Ok(ValidatedLegacyArtifactState::Present(
        ValidatedLegacyArtifact::from_metadata(path.to_path_buf(), &after),
    ))
}

fn ambiguous_legacy_preflight(
    protection: &mut crate::audio::aec::StaleScratchProtection,
    reason: &'static str,
) -> AppError {
    protection.preserve_all();
    AppError::Locked(reason.into())
}

/// Core of [`claim_inflight`] with the inflight `dir` injected, so the claim logic is testable
/// against a temp dir + a real DB without touching app-data.
#[cfg(test)]
fn claim_inflight_in(dir: &Path, db: &Db) -> (Vec<SalvageJob>, Vec<String>) {
    let Ok(mut preflight) = startup_legacy_recovery_preflight_in(dir, db) else {
        return (Vec::new(), Vec::new());
    };
    claim_inflight_with_preflight(dir, db, &mut preflight)
        .unwrap_or_else(|_| (Vec::new(), Vec::new()))
}

fn claim_inflight_with_preflight(
    dir: &Path,
    db: &Db,
    preflight: &mut LegacyRecoveryPreflight,
) -> Result<(Vec<SalvageJob>, Vec<String>)> {
    let mut jobs = Vec::new();
    let mut claimed = Vec::new();
    match preflight.directory.revalidate(dir) {
        Ok(true) => {}
        Ok(false) => return Ok((jobs, claimed)),
        Err(error) => {
            preflight.scratch_protection.preserve_all();
            tracing::warn!(target: "startup", error = %error, "legacy recovery directory changed after preflight; preserving every inventoried artifact");
            return Err(error);
        }
    }
    let current_names = match legacy_directory_inventory(dir).and_then(|names| {
        preflight.directory.revalidate(dir)?;
        Ok(names)
    }) {
        Ok(names) => names,
        Err(error) => {
            preflight.scratch_protection.preserve_all();
            tracing::warn!(target: "startup", error = %error, "legacy recovery inventory changed after preflight; preserving every inventoried artifact");
            return Err(error);
        }
    };
    if current_names != preflight.legacy_names {
        preflight.scratch_protection.preserve_all();
        claimed = preflight.sidecar_ids();
        for file_name in current_names.difference(&preflight.legacy_names) {
            let Some(owner) = legacy_filename_owner(file_name) else {
                continue;
            };
            if !safe_legacy_owner_id(owner) {
                continue;
            }
            if !claimed.iter().any(|claimed_id| claimed_id == owner) {
                claimed.push(owner.to_string());
            }
            match db.mark_legacy_recording_recovery_pending(owner) {
                Ok(false) => {}
                Ok(true) => {
                    return Err(AppError::Locked(
                        "a post-preflight recovery artifact belongs to a locked folder".into(),
                    ));
                }
                Err(error) => {
                    return Err(error);
                }
            }
        }
        claimed.sort();
        claimed.dedup();
        tracing::warn!(target: "startup", "legacy recovery names changed after preflight; ignoring the new inventory and preserving every artifact");
        return Err(AppError::Locked(
            "legacy recovery inventory changed after startup preflight; restart required".into(),
        ));
    }
    // Consume only the exact sidecars authenticated by startup preflight. A new pathname that
    // appears afterwards is not authority for this launch, and a changed inode/content capability
    // preserves the whole set instead of letting JSON redirect derived artifact paths.
    for preflight_owner in preflight.sidecar_ids() {
        let sidecar_capability = match preflight.sidecar_for(&preflight_owner) {
            Ok(capability) => capability,
            Err(error) => {
                preflight.scratch_protection.preserve_all();
                tracing::warn!(target: "startup", meeting_id = %preflight_owner, error = %error, "legacy recovery sidecar changed after preflight; preserving every recovery artifact");
                claimed.push(preflight_owner);
                continue;
            }
        };
        let sidecar_path = sidecar_capability.path.clone();
        let sidecar = sidecar_capability.parsed.clone();
        let meeting_id = sidecar_capability.owner.clone();
        let mic_state = match preflight.mic_for(&meeting_id) {
            Ok(state) => state,
            Err(error) => {
                preflight.scratch_protection.preserve_all();
                tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %error, "legacy mic identity changed after helper scan; preserving every recovery artifact");
                claimed.push(meeting_id);
                continue;
            }
        };
        let inflight_system_state = match preflight.inflight_system_for(&meeting_id) {
            Ok(state) => state,
            Err(error) => {
                preflight.scratch_protection.preserve_all();
                tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %error, "legacy inflight-system identity changed after helper scan; preserving every recovery artifact");
                claimed.push(meeting_id);
                continue;
            }
        };
        let spill_path = mic_state.path().to_path_buf();
        let generation_pending = match db.meeting_has_nonterminal_recording_generation(&meeting_id)
        {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %error, "salvage: recording ownership unreadable; preserving legacy artifacts");
                claimed.push(meeting_id);
                continue;
            }
        };
        let row_is_stuck = match db.get_meeting(&meeting_id) {
            Ok(Some(meeting)) => {
                matches!(
                    meeting.status,
                    MeetingStatus::Recording | MeetingStatus::Error
                )
            }
            Ok(None) => false,
            Err(error) => {
                tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %error, "legacy recovery meeting state unreadable; preserving sidecar and artifacts");
                claimed.push(meeting_id);
                continue;
            }
        };
        if row_is_stuck || generation_pending {
            let folder_locked = match db.mark_legacy_recording_recovery_pending(&meeting_id) {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %error, "could not publish durable legacy recovery ownership; preserving sidecar and artifacts in place");
                    claimed.push(meeting_id);
                    continue;
                }
            };
            if folder_locked {
                tracing::info!(target: "startup", meeting_id = %meeting_id, "legacy recovery belongs to an at-rest locked folder; preserving original artifacts without cloning or reading them (unlock required)");
                claimed.push(meeting_id);
                continue;
            }
            // A durable generation wins execution ownership over the legacy sidecar, but the
            // legacy marker remains: generation retirement must not make those separate historical
            // artifacts invisible to lock/move/delete governance in this same process.
            if generation_pending {
                claimed.push(meeting_id);
                continue;
            }
        }

        let spill_len = match mic_state.verified_len() {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %error, "legacy mic state unreadable; preserving recovery marker, sidecar, and artifacts");
                claimed.push(meeting_id);
                continue;
            }
        };
        let spill_present = spill_len.is_some_and(|len| len >= 4);
        if row_is_stuck && spill_len.is_some() && !spill_present {
            tracing::warn!(target: "startup", meeting_id = %meeting_id, "legacy mic source has no complete sample; preserving recovery marker, sidecar, and artifacts");
            claimed.push(meeting_id);
            continue;
        }
        if row_is_stuck
            && spill_len.is_some_and(|byte_len| {
                byte_len % 4 != 0
                    || !legacy_samples_fit_bounded_pipeline(byte_len / 4, sidecar.sample_rate)
            })
        {
            tracing::warn!(target: "startup", meeting_id = %meeting_id, "legacy mic source exceeds the bounded compatibility pipeline; preserving every source without hashing it at startup");
            claimed.push(meeting_id);
            continue;
        }
        let system_present = match legacy_system_source_present(
            &inflight_system_state,
            preflight.scratch_for(&meeting_id),
        ) {
            Ok(value) => value,
            Err(error) => {
                preflight.scratch_protection.preserve_all();
                tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %error, "legacy far-side state unreadable; preserving recovery marker, sidecar, and artifacts");
                claimed.push(meeting_id);
                continue;
            }
        };
        if system_present {
            if let Err(error) = sidecar_capability.revalidate() {
                preflight.scratch_protection.preserve_all();
                tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %error, "legacy sidecar changed before paired-source preservation; preserving every source in place");
                claimed.push(meeting_id);
                continue;
            }
            let capability = match preflight.take_scratch(&meeting_id) {
                Ok(capability) => capability,
                Err(error) => {
                    preflight.scratch_protection.preserve_all();
                    tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %error, "legacy external scratch capability changed; preserving every source in place");
                    claimed.push(meeting_id);
                    continue;
                }
            };
            if let Err(error) = resolve_system_wav(
                dir,
                &meeting_id,
                &preflight.directory,
                &inflight_system_state,
                capability,
            ) {
                preflight.scratch_protection.preserve_all();
                tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %error, "legacy far-side preservation claim failed; original sources and recovery marker retained");
                claimed.push(meeting_id);
                continue;
            }
            tracing::info!(target: "startup", meeting_id = %meeting_id, "paired legacy crash salvage is unsupported; preserved mic/system sources and sidecar without loading them into the pipeline");
            claimed.push(meeting_id);
            continue;
        }
        if sidecar.system_scratch_path.is_some() {
            // An external system path named by the sidecar was absent at preflight. There is no
            // atomic filesystem primitive that can prove it stays absent while mic/sidecar names
            // are removed, so retain the complete ownership record rather than create a TOCTOU
            // window that loses provenance if the helper publishes late. Its exact validated
            // candidate is sweep-protected for this launch.
            tracing::info!(target: "startup", meeting_id = %meeting_id, "legacy sidecar reserves an absent far-side path; preserving the complete recovery set");
            claimed.push(meeting_id);
            continue;
        }
        if row_is_stuck && !spill_present && !system_present {
            match cleanup_terminal_legacy_recovery(
                dir,
                db,
                preflight,
                &meeting_id,
                &mic_state,
                &inflight_system_state,
                &sidecar_capability,
            ) {
                Ok(()) => {
                    tracing::info!(target: "startup", meeting_id = %meeting_id, "cleared legacy recovery ownership after proving every audio artifact absent")
                }
                Err(error) => {
                    preflight.scratch_protection.preserve_all();
                    tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %error, "legacy empty-recovery cleanup was incomplete; durable marker retained");
                    claimed.push(meeting_id.clone());
                }
            }
            continue;
        }

        match salvage_plan(spill_present, row_is_stuck) {
            SalvagePlan::Salvage => {
                if let Err(error) = sidecar_capability.revalidate() {
                    preflight.scratch_protection.preserve_all();
                    tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %error, "legacy sidecar changed before source claim; preserving the complete recovery set");
                    claimed.push(meeting_id);
                    continue;
                }
                let external_scratch_absent_path = match preflight
                    .absent_scratch_path_for(&meeting_id)
                {
                    Ok(path) => path,
                    Err(error) => {
                        preflight.scratch_protection.preserve_all();
                        tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %error, "legacy absent scratch authority changed; preserving recovery artifacts");
                        claimed.push(meeting_id);
                        continue;
                    }
                };
                let mic_capability = match mic_state.present_capability() {
                    Ok(Some(capability)) => capability,
                    Ok(None) => {
                        tracing::warn!(target: "startup", meeting_id = %meeting_id, "legacy mic disappeared before salvage claim; preserving the recovery marker and sidecar");
                        claimed.push(meeting_id);
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %error, "legacy mic changed before salvage claim; preserving the recovery marker and sidecar");
                        claimed.push(meeting_id);
                        continue;
                    }
                };
                jobs.push(SalvageJob {
                    meeting_id: meeting_id.clone(),
                    spill_path,
                    sidecar_path,
                    sample_rate: sidecar.sample_rate,
                    system_wav: None,
                    mic_capability,
                    sidecar_capability,
                    inflight_system_state,
                    external_scratch_absent_path,
                });
                claimed.push(meeting_id);
            }
            SalvagePlan::DiscardOrphan | SalvagePlan::NoSpill => {
                if let Err(error) = cleanup_terminal_legacy_recovery(
                    dir,
                    db,
                    preflight,
                    &meeting_id,
                    &mic_state,
                    &inflight_system_state,
                    &sidecar_capability,
                ) {
                    preflight.scratch_protection.preserve_all();
                    tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %error, "legacy orphan cleanup could not prove exact artifact deletion; durable ownership retained");
                    claimed.push(meeting_id);
                }
            }
        }
    }
    if !claimed.is_empty() {
        tracing::info!(
            target: "startup",
            salvaging = claimed.len(),
            "claimed crashed in-flight recording(s) for salvage"
        );
    }
    Ok((jobs, claimed))
}

/// Exact, restartable cleanup for a terminal/non-retryable legacy row. Audio sources go first,
/// the ownership sidecar next, and the SQLCipher marker last. A crash after sidecar unlink but
/// before marker removal is repaired by [`repair_marker_only_legacy_recovery`] on the next launch.
/// Any earlier failure retains both sidecar and marker, so lock/move/delete remain blocked.
fn cleanup_terminal_legacy_recovery(
    dir: &Path,
    db: &Db,
    preflight: &LegacyRecoveryPreflight,
    meeting_id: &str,
    mic_state: &ValidatedLegacyArtifactState,
    inflight_system_state: &ValidatedLegacyArtifactState,
    sidecar_capability: &ValidatedLegacySidecar,
) -> Result<()> {
    let expected_mic = dir.join(format!("{meeting_id}.{SPILL_EXT}"));
    let expected_inflight = dir.join(format!("{meeting_id}.{SYSTEM_SCRATCH_SUFFIX}"));
    let expected_sidecar = dir.join(format!("{meeting_id}.{SIDECAR_EXT}"));
    if mic_state.path() != expected_mic
        || inflight_system_state.path() != expected_inflight
        || sidecar_capability.owner != meeting_id
        || sidecar_capability.path != expected_sidecar
    {
        return Err(AppError::Locked(
            "legacy recovery cleanup authority does not match its owner".into(),
        ));
    }

    // Prepare every deletion authority before the first unlink. A hardlink, swap, or an artifact
    // that appeared after preflight therefore preserves the complete source set; cleanup cannot
    // delete the system track and only then discover that the mic/sidecar was ambiguous.
    let external_deletion = preflight
        .scratch_for(meeting_id)?
        .map(ValidatedLegacyScratch::prepare_deletion)
        .transpose()?;
    let inflight_deletion = inflight_system_state.prepare_optional_deletion()?;
    let mic_deletion = mic_state.prepare_optional_deletion()?;
    let sidecar_deletion = sidecar_capability.prepare_deletion()?;

    if let Some(deletion) = external_deletion {
        deletion.remove("remove terminal legacy external system scratch")?;
    }
    if let Some(deletion) = inflight_deletion {
        deletion.remove("remove terminal legacy inflight system clone")?;
    }
    if let Some(deletion) = mic_deletion {
        deletion.remove("remove terminal legacy mic spill")?;
    }
    sidecar_deletion.remove("remove terminal legacy recovery sidecar")?;
    db.clear_legacy_recording_recovery_pending(meeting_id)
}

fn legacy_system_source_present(
    inflight_system_state: &ValidatedLegacyArtifactState,
    capability: Result<Option<&ValidatedLegacyScratch>>,
) -> Result<bool> {
    if inflight_system_state.verified_len()?.is_some() {
        return Ok(true);
    }
    match capability? {
        Some(capability) => {
            capability.revalidate()?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Resolve the far-side system WAV for a compatibility claim. An existing inflight clone wins. A
/// still-external historical scratch is copied with an exclusive clonefile-at operation only after
/// the clean-helper scan. The original remains in place, so neither destination-directory races nor
/// later temp cleanup can destroy the only source. `None` means mic-only salvage.
fn resolve_system_wav(
    dir: &Path,
    meeting_id: &str,
    directory: &ValidatedLegacyDirectoryState,
    inflight_system_state: &ValidatedLegacyArtifactState,
    capability: Option<ValidatedLegacyScratch>,
) -> Result<Option<PathBuf>> {
    let preserved = dir.join(format!("{meeting_id}.{SYSTEM_SCRATCH_SUFFIX}"));
    if inflight_system_state.path() != preserved {
        return Err(AppError::Locked(
            "legacy inflight-system authority does not match its owner".into(),
        ));
    }
    if inflight_system_state.verified_len()?.is_some() {
        return Ok(Some(preserved));
    }
    capability
        .map(|capability| claim_system_scratch(capability, directory, meeting_id))
        .transpose()
}

fn claim_system_scratch(
    capability: ValidatedLegacyScratch,
    destination_directory: &ValidatedLegacyDirectoryState,
    meeting_id: &str,
) -> Result<PathBuf> {
    let source_len = capability.identity_len()?;
    let source = capability.canonical_path.clone();
    let source_file = OpenOptions::new()
        .read(true)
        .custom_flags(LEGACY_NOFOLLOW_FLAG)
        .open(&source)
        .map_err(|error| AppError::Audio(format!("open legacy scratch source: {error}")))?;
    let source_metadata = source_file
        .metadata()
        .map_err(|error| AppError::Audio(format!("stat legacy scratch source: {error}")))?;
    if !source_metadata.is_file()
        || source_metadata.nlink() != 1
        || source_metadata.dev() != capability.device
        || source_metadata.ino() != capability.inode
        || source_metadata.len() != source_len
    {
        return Err(AppError::Locked(
            "opened legacy system scratch does not match startup authority".into(),
        ));
    }
    let destination_directory_file = destination_directory
        .open_verified()?
        .ok_or_else(|| AppError::Locked("legacy recovery directory is absent".into()))?;
    let destination_root = destination_directory.path();
    let destination = destination_root.join(format!("{meeting_id}.{SYSTEM_SCRATCH_SUFFIX}"));
    if !legacy_node_proven_absent(&destination)? {
        return Err(AppError::Locked(
            "legacy system scratch destination appeared after startup preflight".into(),
        ));
    }
    let destination_name = destination.file_name().ok_or_else(|| {
        AppError::Audio("legacy system scratch destination has no filename".into())
    })?;
    clone_file_into_directory_exclusive(
        &source_file,
        &destination_directory_file,
        destination_name,
    )?;
    destination_directory_file.sync_all().map_err(|error| {
        AppError::Audio(format!(
            "sync preserved scratch destination directory: {error}"
        ))
    })?;
    destination_directory.revalidate(destination_root)?;
    let preserved = std::fs::symlink_metadata(&destination)
        .map_err(|error| AppError::Audio(format!("stat claimed legacy system scratch: {error}")))?;
    if !preserved.file_type().is_file() || preserved.nlink() != 1 || preserved.len() != source_len {
        return Err(AppError::Audio(
            "preserved legacy system scratch clone changed shape".into(),
        ));
    }
    Ok(destination)
}

#[cfg(target_os = "macos")]
fn clone_file_into_directory_exclusive(
    source: &File,
    destination_directory: &File,
    destination_name: &std::ffi::OsStr,
) -> Result<()> {
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    const CLONE_NOFOLLOW: u32 = 0x0000_0001;
    extern "C" {
        fn fclonefileat(
            source_fd: std::os::raw::c_int,
            destination_directory_fd: std::os::raw::c_int,
            destination: *const std::os::raw::c_char,
            flags: u32,
        ) -> std::os::raw::c_int;
    }
    let destination_name = CString::new(destination_name.as_bytes())
        .map_err(|_| AppError::Audio("legacy scratch destination contains NUL".into()))?;
    // SAFETY: both fds and the NUL-terminated destination name remain live for the syscall.
    // `fclonefileat` creates a new destination and fails if it already exists; the source stays
    // intact, so even a destination-directory rename race cannot lose discoverable temp audio.
    let result = unsafe {
        fclonefileat(
            source.as_raw_fd(),
            destination_directory.as_raw_fd(),
            destination_name.as_ptr(),
            CLONE_NOFOLLOW,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(AppError::Audio(format!(
            "preserve legacy system scratch clone: {}",
            std::io::Error::last_os_error()
        )))
    }
}

#[cfg(not(target_os = "macos"))]
fn clone_file_into_directory_exclusive(
    _source: &File,
    _destination_directory: &File,
    _destination_name: &std::ffi::OsStr,
) -> Result<()> {
    Err(AppError::Unavailable(
        "exclusive legacy scratch claim is available only on macOS".into(),
    ))
}

// ── Disk salvage (STAGE 3, 2026-07-16): re-run the pipeline off a surviving ARCHIVE WAV ─────────

/// A stuck-`RECORDING` meeting claimed for a FROM-DISK pipeline re-run: its original pipeline died
/// AFTER `finalize_meeting` wrote the archive WAV (so `audio_path` points at real audio) but before
/// a terminal status. Pre-fix these rows were reconciled straight to `ERROR` with intact audio on
/// disk that was never re-transcribed.
pub struct DiskSalvageJob {
    pub meeting_id: String,
    pub wav_path: PathBuf,
}

/// The startup disk-salvage decision for ONE stuck-`RECORDING` row, as a PURE fn (unit-tested).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskSalvagePlan {
    /// A plaintext archive WAV exists AND the meeting's folder is open (or absent) → re-run the
    /// pipeline from disk. Salvage owns the row's final status; reconcile must skip it.
    Salvage,
    /// The audio exists only sealed (`.enc`), or the folder is locked (at launch the session
    /// unlock set is ALWAYS empty, so a locked folder can never be salvaged here). NEVER decrypt
    /// for salvage — leave the row to reconcile (terminal `ERROR`, audio untouched), recoverable
    /// via `retry_transcription` after a Touch ID unlock.
    DeferSealed,
    /// No archive audio on disk at all → nothing to re-transcribe; reconcile flips it to `ERROR`
    /// exactly as before.
    NoAudio,
}

/// PURE disk-salvage decision. `folder_open` = the meeting has no folder OR its folder is not
/// locked. FAIL-CLOSED: a locked folder defers even when a plaintext WAV exists (a crash-window
/// leftover) — re-pipelining would write fresh plaintext (segments/note) behind the lock.
pub fn disk_salvage_plan(wav_exists: bool, enc_exists: bool, folder_open: bool) -> DiskSalvagePlan {
    if !folder_open {
        return DiskSalvagePlan::DeferSealed;
    }
    if wav_exists {
        DiskSalvagePlan::Salvage
    } else if enc_exists {
        DiskSalvagePlan::DeferSealed
    } else {
        DiskSalvagePlan::NoAudio
    }
}

/// SYNCHRONOUS startup disk-salvage claim — runs in `lib.rs` setup AFTER [`claim_inflight`]
/// (SPILL SALVAGE WINS: a spill job has both streams — mic + far side — so a meeting already in
/// `already_claimed` is skipped here) and BEFORE `reconcile_stuck_recordings_except`, so a claimed
/// row is not flipped to `ERROR` under the salvage worker. Per remaining stuck-`RECORDING` row it
/// decides via [`disk_salvage_plan`]; `DeferSealed`/`NoAudio` rows are NOT claimed (reconcile makes
/// them honest terminal `ERROR` rows — audio and `.enc` files are never touched here).
///
/// Returns `(jobs, claimed_meeting_ids)`. Best-effort + panic-free; logs carry UUIDs + counts only.
pub fn claim_disk_salvage(
    db: &Db,
    already_claimed: &[String],
) -> (Vec<DiskSalvageJob>, Vec<String>) {
    let mut jobs = Vec::new();
    let mut claimed = Vec::new();
    let ids = match db.stuck_recording_ids() {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(target: "startup", error = %e, "disk salvage: could not list stuck recordings; skipping claim");
            return (jobs, claimed);
        }
    };
    for id in ids {
        if already_claimed.contains(&id) {
            continue; // the spill salvage owns this row (it has the richer dual-stream audio).
        }
        let meeting = match db.get_meeting(&id) {
            Ok(Some(m)) => m,
            _ => continue,
        };
        let Some(path) = meeting.audio_path.filter(|p| !p.trim().is_empty()) else {
            continue; // NoAudio — reconcile flips it to ERROR exactly as before.
        };
        // Normalize: `audio_path` may name the plaintext WAV or (post-seal) the `.enc` itself.
        let plaintext = path.trim_end_matches(".enc").to_string();
        let wav_exists = Path::new(&plaintext).exists();
        let enc_exists = Path::new(&format!("{plaintext}.enc")).exists();
        // At launch the session unlock set is EMPTY, so "open" is purely the folder's lock bit.
        let folder_open = match db.folder_for_meeting(&id) {
            Ok(Some(fid)) => match db.folder_by_id(&fid) {
                Ok(Some(f)) => !f.locked,
                Ok(None) => true,
                Err(_) => false, // unreadable folder row ⇒ fail closed (defer).
            },
            Ok(None) => true,
            Err(_) => false, // fail closed.
        };
        match disk_salvage_plan(wav_exists, enc_exists, folder_open) {
            DiskSalvagePlan::Salvage => {
                jobs.push(DiskSalvageJob {
                    meeting_id: id.clone(),
                    wav_path: PathBuf::from(plaintext),
                });
                claimed.push(id);
            }
            DiskSalvagePlan::DeferSealed => {
                tracing::info!(
                    target: "startup",
                    meeting_id = %id,
                    "disk salvage deferred: the recording's audio is sealed / its folder is locked — the row becomes ERROR; unlock the folder and use Retry transcription"
                );
            }
            DiskSalvagePlan::NoAudio => {}
        }
    }
    if !claimed.is_empty() {
        tracing::info!(
            target: "startup",
            salvaging = claimed.len(),
            "claimed crashed recording(s) with surviving archive audio for from-disk re-transcription"
        );
    }
    (jobs, claimed)
}

/// Spawn one salvage worker and return its ownership to launch recovery. The caller MUST retain and
/// join this handle before reopening recording admission: merely detaching hours of ASR here would
/// let a new capture contend for RAM/Metal and would let old global status events overwrite the
/// active recording UI. Jobs remain sequential and best-effort; a no-op returns `None`.
pub(crate) fn spawn_salvage(
    app: AppHandle,
    ledger_jobs: Vec<crate::audio::source::RecoveredRecordingJob>,
    jobs: Vec<SalvageJob>,
    disk_jobs: Vec<DiskSalvageJob>,
) -> Option<std::thread::JoinHandle<()>> {
    if ledger_jobs.is_empty() && jobs.is_empty() && disk_jobs.is_empty() {
        return None;
    }
    match std::thread::Builder::new()
        .name("murmur-crash-salvage".into())
        .spawn(move || run_salvage_jobs(app, ledger_jobs, jobs, disk_jobs))
    {
        Ok(handle) => Some(handle),
        Err(error) => {
            tracing::warn!(target: "startup", error = %error, "could not spawn recording salvage worker; artifacts preserved");
            None
        }
    }
}

fn run_salvage_jobs(
    app: AppHandle,
    ledger_jobs: Vec<crate::audio::source::RecoveredRecordingJob>,
    jobs: Vec<SalvageJob>,
    disk_jobs: Vec<DiskSalvageJob>,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            // Leave the claimed rows for a future launch (their spills/sidecars remain on disk); do
            // NOT delete anything and do NOT crash.
            tracing::warn!(target: "startup", error = %e, "salvage runtime build failed; deferring salvage");
            return;
        }
    };
    // Ledger jobs own already-finalized exact prefixes and run first, then legacy spill jobs, then
    // from-disk re-runs. Sequential on one
    // thread — heavy ASR additionally serializes through the shared `perf::run_heavy` gate.
    for job in ledger_jobs {
        rt.block_on(salvage_ledger_one(&app, job));
    }
    for job in jobs {
        rt.block_on(salvage_one(&app, job));
    }
    for job in disk_jobs {
        rt.block_on(salvage_disk_one(&app, job));
    }
}

async fn salvage_ledger_one(app: &AppHandle, job: crate::audio::source::RecoveredRecordingJob) {
    let state = app.state::<AppState>();
    // Claim-time state is not authority to read raw audio later. Repeat the session-aware lock
    // gate immediately before handing the durable generation to the plaintext pipeline. A refusal
    // releases only the lease; raw/system/archive artifacts and their ledger proofs remain intact.
    match crate::commands::meeting_is_unlocked(&state, &job.meeting_id) {
        Ok(true) => {}
        Ok(false) => {
            if let Err(error) = job.recording.release_for_recovery(&state.db) {
                tracing::warn!(target: "startup", meeting_id = %job.meeting_id, error = %error, "locked ledger recovery could not release its lease; artifacts preserved");
            }
            tracing::info!(target: "startup", meeting_id = %job.meeting_id, "ledger recovery deferred by folder lock; artifacts preserved");
            return;
        }
        Err(gate_error) => {
            if let Err(error) = job.recording.release_for_recovery(&state.db) {
                tracing::warn!(target: "startup", meeting_id = %job.meeting_id, error = %error, "ledger recovery with unreadable lock state could not release its lease; artifacts preserved");
            }
            tracing::warn!(target: "startup", meeting_id = %job.meeting_id, error = %gate_error, "ledger recovery lock state unreadable; artifacts preserved");
            return;
        }
    }
    let duration_s = (job.recording.frames / job.recording.sample_rate.max(1) as u64) as i64;
    let terminal_guard = crate::pipeline::TerminalStatusGuard::arm(
        Some(app.clone()),
        state.db.clone(),
        &job.meeting_id,
    );
    let result = crate::pipeline::run_file_backed(
        app,
        &state,
        &job.meeting_id,
        job.recording,
        duration_s,
        None,
    )
    .await;
    terminal_guard.disarm();
    if let Err(error) = result {
        tracing::warn!(target: "startup", meeting_id = %job.meeting_id, error = %error, "ledger recovery pipeline failed; verified audio retained");
    }
}

async fn salvage_one(app: &AppHandle, job: SalvageJob) {
    let state = app.state::<AppState>();
    match crate::commands::meeting_is_unlocked(&state, &job.meeting_id) {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(target: "startup", meeting_id = %job.meeting_id, "legacy salvage deferred by the folder lock; mic/system sources and sidecar preserved");
            return;
        }
        Err(error) => {
            tracing::warn!(target: "startup", meeting_id = %job.meeting_id, error = %error, "legacy salvage lock state unreadable; preserving mic/system sources and sidecar");
            return;
        }
    }
    if let Err(error) = ensure_legacy_pipeline_supported(job.system_wav.as_deref()) {
        tracing::warn!(target: "startup", meeting_id = %job.meeting_id, error = %error, "legacy paired salvage deferred before the pipeline; full mic/system sources and sidecar preserved for retry");
        return;
    }
    let (bytes, mic_proof) = match read_stable_legacy_spill(
        &job.spill_path,
        job.sample_rate,
        &job.mic_capability,
    ) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(target: "startup", meeting_id = %job.meeting_id, error = %error, "legacy salvage deferred; mic spill and sidecar preserved");
            return;
        }
    };
    let samples = f32le_to_samples(&bytes);
    if samples.is_empty()
        || !legacy_samples_fit_bounded_pipeline(samples.len() as u64, job.sample_rate)
    {
        tracing::warn!(target: "startup", meeting_id = %job.meeting_id, frames = samples.len(), "legacy salvage exceeds the bounded compatibility pipeline; full mic/system sources and sidecar preserved");
        return;
    }
    let duration_s = (samples.len() as f64 / job.sample_rate as f64).round() as i64;
    let now = std::time::Instant::now();
    let result = crate::pipeline::run_after_stop(
        app,
        &state,
        &job.meeting_id,
        samples,
        job.sample_rate,
        duration_s,
        None,
        None,
        now,
        None,
    )
    .await;
    match result {
        Ok(_) => {
            let cleanup =
                complete_legacy_salvage_cleanup(&state.db, &job, &job.spill_path, &mic_proof);
            match cleanup {
                Ok(()) => {}
                Err(error) => {
                    tracing::warn!(target: "startup", meeting_id = %job.meeting_id, error = %error, "legacy salvage succeeded but exact source cleanup was incomplete; durable recovery ownership retained")
                }
            }
            tracing::info!(target: "startup", meeting_id = %job.meeting_id, "salvaged a crashed recording into a note")
        }
        Err(e) => {
            tracing::warn!(target: "startup", meeting_id = %job.meeting_id, error = %e, "legacy salvage pipeline failed; full mic/system sources and sidecar preserved for retry")
        }
    }
}

fn legacy_samples_fit_bounded_pipeline(frames: u64, sample_rate: u32) -> bool {
    sample_rate != 0
        && frames > 0
        && frames <= (sample_rate as u64).saturating_mul(LEGACY_PIPELINE_MAX_SECONDS)
}

fn read_stable_legacy_spill(
    path: &Path,
    sample_rate: u32,
    expected: &ValidatedLegacyArtifact,
) -> Result<(Vec<u8>, crate::audio::source::VerifiedFile)> {
    if expected.path != path {
        return Err(AppError::Locked(
            "legacy mic path does not match its startup authority".into(),
        ));
    }
    let max_bytes = (sample_rate as u64)
        .saturating_mul(LEGACY_PIPELINE_MAX_SECONDS)
        .saturating_mul(4);
    let (bytes, proof) = crate::audio::source::read_existing_file_bounded(path, max_bytes)?;
    if proof.device() != expected.device
        || proof.inode() != expected.inode
        || expected
            .byte_len
            .is_some_and(|byte_len| proof.byte_len() != byte_len)
        || expected
            .sha256
            .as_deref()
            .is_some_and(|sha256| proof.sha256() != sha256)
        || proof.byte_len() % 4 != 0
        || !legacy_samples_fit_bounded_pipeline(proof.byte_len() / 4, sample_rate)
    {
        return Err(AppError::Audio(
            "legacy mic spill changed or has a partial frame".into(),
        ));
    }
    Ok((bytes, proof))
}

fn preflight_legacy_system_wav(path: &Path) -> Result<()> {
    use crate::audio::source::MonoSource as _;

    let before = crate::audio::source::verify_existing_file(path)?;
    let source = crate::audio::source::WavMonoSource::open(path)?;
    if !legacy_samples_fit_bounded_pipeline(source.frames(), source.sample_rate()) {
        return Err(AppError::Audio(
            "legacy system WAV exceeds the bounded compatibility pipeline".into(),
        ));
    }
    let after = crate::audio::source::verify_existing_file(path)?;
    if before.device() != after.device()
        || before.inode() != after.inode()
        || before.byte_len() != after.byte_len()
        || before.sha256() != after.sha256()
    {
        return Err(AppError::Audio(
            "legacy system WAV changed during metadata preflight".into(),
        ));
    }
    Ok(())
}

fn ensure_legacy_pipeline_supported(system_path: Option<&Path>) -> Result<()> {
    let Some(system_path) = system_path else {
        return Ok(());
    };
    preflight_legacy_system_wav(system_path)?;
    Err(AppError::Audio(
        "paired legacy crash salvage is unsupported; sources retained for retry".into(),
    ))
}

fn prepare_legacy_source_if_unchanged(
    path: &Path,
    expected: &crate::audio::source::VerifiedFile,
    operation: &str,
) -> Result<crate::audio::source::VerifiedDeletion> {
    let current = crate::audio::source::verify_existing_file(path)?;
    if current.device() != expected.device()
        || current.inode() != expected.inode()
        || current.byte_len() != expected.byte_len()
        || current.sha256() != expected.sha256()
    {
        return Err(AppError::Audio(format!(
            "{operation}: source identity or content changed"
        )));
    }
    crate::audio::source::VerifiedDeletion::for_file(path, &current)
}

fn complete_legacy_salvage_cleanup(
    db: &Db,
    job: &SalvageJob,
    spill_path: &Path,
    mic_proof: &crate::audio::source::VerifiedFile,
) -> Result<()> {
    if job.mic_capability.path != spill_path
        || job.sidecar_capability.path != job.sidecar_path
        || job.sidecar_capability.owner != job.meeting_id
        || job.system_wav.is_some()
    {
        return Err(AppError::Locked(
            "legacy salvage cleanup authority does not match its job".into(),
        ));
    }
    // Validate and open both exact single-link inodes before the first unlink. If the sidecar was
    // replaced during the asynchronous pipeline, the recovered mic remains retryable as well.
    if job.inflight_system_state.verified_len()?.is_some() {
        return Err(AppError::Locked(
            "legacy far-side source appeared during mic-only salvage".into(),
        ));
    }
    if let Some(path) = &job.external_scratch_absent_path {
        if !legacy_node_proven_absent(path)? {
            return Err(AppError::Locked(
                "legacy external far-side source appeared during mic-only salvage".into(),
            ));
        }
    }
    job.mic_capability.verified()?;
    let mic_deletion = prepare_legacy_source_if_unchanged(
        spill_path,
        mic_proof,
        "remove successfully salvaged legacy mic spill",
    )?;
    let sidecar_deletion = job.sidecar_capability.prepare_deletion()?;
    mic_deletion.remove("remove successfully salvaged legacy mic spill")?;
    sidecar_deletion.remove("remove successfully salvaged legacy sidecar")?;
    db.clear_legacy_recording_recovery_pending(&job.meeting_id)
}

/// FROM-DISK re-run of one claimed stuck-`RECORDING` meeting (STAGE 3 — see [`claim_disk_salvage`]).
/// Re-runs the EXISTING post-Stop pipeline off the surviving archive WAV
/// ([`crate::pipeline::run_salvage_from_disk`] — same heavy-inference gate, ASR watchdog, and
/// seal-aware persist as a live Stop). Fail-closed re-checks at run time (the detached worker can
/// start seconds after the claim): the lock gate (a folder locked since the claim defers — flip to
/// `ERROR`, audio untouched, recoverable via unlock + retry) and the recorder (a user already
/// recording defers the same way — a salvage must not contend with a live capture; the row stays
/// retryable). A pipeline error is already persisted terminal by `run_after_stop`'s Err arm; a
/// PANIC is caught by the armed `TerminalStatusGuard`. NEVER deletes audio.
async fn salvage_disk_one(app: &AppHandle, job: DiskSalvageJob) {
    let state = app.state::<AppState>();

    let defer_to_error = |reason: &str| {
        let _ = state
            .db
            .update_meeting_status(&job.meeting_id, MeetingStatus::Error);
        tracing::warn!(
            target: "startup",
            meeting_id = %job.meeting_id,
            reason,
            "disk salvage deferred; the recording stays terminal-Error with its audio intact (Retry transcription re-runs it)"
        );
    };

    // Lock gate, re-checked fail-closed at run time (claim-time state can be stale).
    match crate::commands::meeting_is_unlocked(&state, &job.meeting_id) {
        Ok(true) => {}
        Ok(false) => {
            defer_to_error("folder locked");
            return;
        }
        Err(_) => {
            defer_to_error("lock state unreadable");
            return;
        }
    }
    // A recording the user started in the seconds since launch wins over the salvage.
    let recording_active = state.recorder.lock().map(|r| r.is_some()).unwrap_or(true);
    if recording_active {
        defer_to_error("recording in progress");
        return;
    }

    // Guard the non-terminal RECORDING row across the run: a panic inside the pipeline must still
    // leave a terminal status (the guard is status-aware, so a healthy Summarized/Exported finish
    // is never clobbered).
    let terminal_guard = crate::pipeline::TerminalStatusGuard::arm(
        Some(app.clone()),
        state.db.clone(),
        &job.meeting_id,
    );
    let result =
        crate::pipeline::run_salvage_from_disk(app, &state, &job.meeting_id, &job.wav_path).await;
    terminal_guard.disarm();
    match result {
        Ok(_) => {
            tracing::info!(target: "startup", meeting_id = %job.meeting_id, "re-transcribed a crashed recording from its on-disk archive")
        }
        Err(e) => {
            tracing::warn!(target: "startup", meeting_id = %job.meeting_id, error = %e, "from-disk salvage pipeline failed; audio intact, meeting marked Error")
        }
    }
}

#[cfg(test)]
fn remove_salvaged_legacy_artifact(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            let proof = crate::audio::source::verify_existing_file(path)?;
            crate::audio::source::VerifiedDeletion::for_file(path, &proof)?
                .remove("remove legacy recording recovery artifact")
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::Audio(format!(
            "inspect legacy recording recovery artifact: {error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "murmur-spill-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn direct_temp_scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "meetnotes-sys-{tag}-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn tmp_db() -> (Db, PathBuf) {
        let p = crate::storage::db::unique_temp_path("murmur-spill-db", "sqlite");
        let _ = std::fs::remove_file(&p);
        (Db::open_with_key(&p, TEST_DEK).unwrap(), p)
    }

    fn insert_meeting(db: &Db, id: &str, status: MeetingStatus) {
        db.insert_meeting(&crate::storage::Meeting {
            id: id.into(),
            started_at: "2026-07-04T09:00:00Z".into(),
            ended_at: None,
            title: None,
            duration_s: 0,
            audio_path: None,
            status,
            folder_id: None,
        })
        .unwrap();
    }

    fn frozen_legacy_artifact(path: &Path) -> ValidatedLegacyArtifact {
        let proof = crate::audio::source::verify_existing_file(path).unwrap();
        ValidatedLegacyArtifact::from_proof(path.to_path_buf(), &proof)
    }

    fn present_legacy_artifact(path: &Path) -> ValidatedLegacyArtifactState {
        ValidatedLegacyArtifactState::Present(frozen_legacy_artifact(path))
    }

    fn write_legacy_sidecar(
        dir: &Path,
        meeting_id: &str,
        system_scratch_path: Option<&Path>,
    ) -> PathBuf {
        let path = dir.join(format!("{meeting_id}.{SIDECAR_EXT}"));
        std::fs::write(
            &path,
            serde_json::to_vec(&SpillSidecar {
                meeting_id: meeting_id.to_string(),
                sample_rate: 48_000,
                system_scratch_path: system_scratch_path
                    .map(|value| value.to_string_lossy().into_owned()),
            })
            .unwrap(),
        )
        .unwrap();
        path
    }

    // ── PURE: spill format round-trip ────────────────────────────────────────────────────────

    #[test]
    fn f32le_round_trips_byte_identical() {
        let samples = vec![0.0f32, 1.0, -1.0, 0.123_456, -0.987_654, f32::MIN_POSITIVE];
        let bytes = samples_to_f32le(&samples);
        assert_eq!(bytes.len(), samples.len() * 4, "4 bytes per f32");
        // Little-endian, so the first sample (0.0) is four zero bytes.
        assert_eq!(&bytes[0..4], &[0, 0, 0, 0]);
        let back = f32le_to_samples(&bytes);
        assert_eq!(
            back, samples,
            "samples must survive the f32le round-trip byte-identically"
        );
    }

    #[test]
    fn f32le_drops_a_truncated_tail_sample() {
        // 2 full samples + 2 stray bytes (a crash truncated mid-sample) ⇒ decode yields exactly 2.
        let mut bytes = samples_to_f32le(&[0.25f32, -0.5]);
        bytes.push(0xAB);
        bytes.push(0xCD);
        let back = f32le_to_samples(&bytes);
        assert_eq!(
            back,
            vec![0.25f32, -0.5],
            "partial trailing bytes must be dropped, not fabricated"
        );
    }

    #[test]
    fn f32le_empty_is_empty() {
        assert!(f32le_to_samples(&[]).is_empty());
        assert!(samples_to_f32le(&[]).is_empty());
    }

    // ── PURE: salvage_plan truth table ───────────────────────────────────────────────────────

    #[test]
    fn salvage_plan_truth_table() {
        assert_eq!(
            salvage_plan(true, true),
            SalvagePlan::Salvage,
            "spill + stuck-recording ⇒ salvage"
        );
        assert_eq!(
            salvage_plan(true, false),
            SalvagePlan::DiscardOrphan,
            "spill but row not recording ⇒ orphan"
        );
        assert_eq!(
            salvage_plan(false, true),
            SalvagePlan::NoSpill,
            "no spill ⇒ nothing to reconstruct"
        );
        assert_eq!(
            salvage_plan(false, false),
            SalvagePlan::NoSpill,
            "no spill ⇒ nothing to reconstruct"
        );
    }

    // ── incremental mirror (flush_step, no thread) ───────────────────────────────────────────

    #[test]
    fn flush_step_mirrors_only_the_new_tail() {
        let reader = SampleReader::from_samples(vec![0.1f32, 0.2, 0.3]);
        let mut file: Vec<u8> = Vec::new();
        let mut warned = false;

        // First flush: writes all 3 samples, advances to 3.
        let flushed = flush_step(&mut file, &reader, 0, &mut warned);
        assert_eq!(flushed, 3);
        assert_eq!(file.len(), 12);

        // Simulate more capture, then flush again from offset 3: only the NEW 2 samples are written.
        reader.push_for_test(&[0.4f32, 0.5]);
        let flushed = flush_step(&mut file, &reader, flushed, &mut warned);
        assert_eq!(flushed, 5);
        assert_eq!(file.len(), 20);

        // The mirrored bytes reconstruct the full growing buffer, in order.
        assert_eq!(f32le_to_samples(&file), vec![0.1f32, 0.2, 0.3, 0.4, 0.5]);
    }

    // ── claim_inflight decisions (real DB + temp inflight dir) ────────────────────────────────

    #[test]
    fn locked_legacy_preflight_aborts_before_any_scratch_sweep() {
        let dir = tmp_dir("preflight-locked");
        let scratch_dir = tmp_dir("preflight-locked-scratch");
        let (db, db_path) = tmp_db();
        let id = "preflight-locked";
        let folder_id = uuid::Uuid::new_v4().to_string();
        insert_meeting(&db, id, MeetingStatus::Error);
        db.insert_folder(&crate::storage::Folder {
            id: folder_id.clone(),
            name: "Private".into(),
            path: "Private".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-22T12:00:00Z".into(),
        })
        .unwrap();
        db.upsert_note(&crate::storage::NoteRecord {
            meeting_id: id.into(),
            provider_id: "test".into(),
            markdown: "sealed".into(),
            created_at: "2026-07-22T12:00:01Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_meeting_folder(id, Some(&folder_id)).unwrap();
        db.set_folder_locked(&folder_id, true, Some(b"wrapped"))
            .unwrap();
        let scratch = direct_temp_scratch("locked");
        let unrelated = scratch_dir.join("meetnotes-sys-unrelated.wav");
        std::fs::write(&scratch, b"locked-recovery").unwrap();
        std::fs::write(&unrelated, b"unrelated").unwrap();
        std::fs::write(
            dir.join(format!("{id}.{SIDECAR_EXT}")),
            serde_json::to_vec(&SpillSidecar {
                meeting_id: id.into(),
                sample_rate: 48_000,
                system_scratch_path: Some(scratch.to_string_lossy().into()),
            })
            .unwrap(),
        )
        .unwrap();

        assert!(matches!(
            startup_legacy_recovery_preflight_in(&dir, &db),
            Err(AppError::Locked(_))
        ));

        assert!(scratch.exists() && unrelated.exists());
        assert!(db
            .meeting_has_pending_legacy_recording_recovery(id)
            .unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&scratch_dir);
        let _ = std::fs::remove_file(&scratch);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn unlocked_preflight_protects_marked_stale_tmp_and_sweeps_unrelated() {
        let dir = tmp_dir("preflight-open");
        let scratch_dir = tmp_dir("preflight-open-scratch");
        let (db, db_path) = tmp_db();
        let id = "preflight-open";
        insert_meeting(&db, id, MeetingStatus::Error);
        let protected = scratch_dir.join("meetnotes-sys-protected.wav");
        let unrelated = scratch_dir.join("meetnotes-sys-unrelated.wav");
        std::fs::write(&protected, b"protected-recovery").unwrap();
        std::fs::write(&unrelated, b"unrelated").unwrap();
        std::fs::write(
            dir.join(format!("{id}.{SIDECAR_EXT}")),
            serde_json::to_vec(&SpillSidecar {
                meeting_id: id.into(),
                sample_rate: 48_000,
                system_scratch_path: Some(protected.to_string_lossy().into()),
            })
            .unwrap(),
        )
        .unwrap();

        let protection =
            startup_legacy_recovery_preflight_in_with_temp_root(&dir, &db, &scratch_dir).unwrap();
        let future = std::fs::metadata(&protected).unwrap().modified().unwrap()
            + std::time::Duration::from_secs(7_201);
        let removed = crate::audio::aec::sweep_stale_scratch_in(
            &scratch_dir,
            future,
            &protection.scratch_protection,
        );

        assert_eq!(removed, 1);
        assert!(
            protected.exists(),
            "the exact marker-owned temp path survives"
        );
        assert!(
            !unrelated.exists(),
            "an unrelated stale scratch is reclaimed"
        );
        assert!(db
            .meeting_has_pending_legacy_recording_recovery(id)
            .unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&scratch_dir);
        let _ = std::fs::remove_file(&protected);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn malformed_sidecar_aborts_preflight_before_stale_scratch_sweep() {
        let dir = tmp_dir("preflight-unreadable");
        let scratch_dir = tmp_dir("preflight-unreadable-scratch");
        let (db, db_path) = tmp_db();
        std::fs::write(dir.join(format!("broken.{SIDECAR_EXT}")), b"{").unwrap();
        let scratch = scratch_dir.join("meetnotes-sys-ambiguous.wav");
        std::fs::write(&scratch, b"ambiguous-recovery").unwrap();

        assert!(matches!(
            startup_legacy_recovery_preflight_in(&dir, &db),
            Err(AppError::Locked(_))
        ));
        assert!(scratch.exists());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&scratch_dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn invalid_sidecar_meeting_id_aborts_preflight_before_stale_scratch_sweep() {
        let dir = tmp_dir("preflight-invalid-id");
        let scratch_dir = tmp_dir("preflight-invalid-id-scratch");
        let (db, db_path) = tmp_db();
        let scratch = scratch_dir.join("meetnotes-sys-invalid-id.wav");
        std::fs::write(&scratch, b"ambiguous-recovery").unwrap();
        std::fs::write(
            dir.join(format!("owner.{SIDECAR_EXT}")),
            serde_json::to_vec(&SpillSidecar {
                meeting_id: "different-owner".into(),
                sample_rate: 48_000,
                system_scratch_path: Some(scratch.to_string_lossy().into()),
            })
            .unwrap(),
        )
        .unwrap();

        assert!(matches!(
            startup_legacy_recovery_preflight_in(&dir, &db),
            Err(AppError::Locked(_))
        ));
        assert!(scratch.exists());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&scratch_dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn unreadable_referenced_scratch_path_aborts_after_publishing_marker() {
        let dir = tmp_dir("preflight-unreadable-path");
        let scratch_dir = tmp_dir("preflight-unreadable-path-scratch");
        let (db, db_path) = tmp_db();
        let id = "preflight-unreadable-path";
        insert_meeting(&db, id, MeetingStatus::Error);
        let unreadable = scratch_dir.join("x".repeat(300));
        let unrelated = scratch_dir.join("meetnotes-sys-unrelated.wav");
        std::fs::write(&unrelated, b"unrelated").unwrap();
        std::fs::write(
            dir.join(format!("{id}.{SIDECAR_EXT}")),
            serde_json::to_vec(&SpillSidecar {
                meeting_id: id.into(),
                sample_rate: 48_000,
                system_scratch_path: Some(unreadable.to_string_lossy().into()),
            })
            .unwrap(),
        )
        .unwrap();

        assert!(matches!(
            startup_legacy_recovery_preflight_in(&dir, &db),
            Err(AppError::Locked(_))
        ));
        assert!(db
            .meeting_has_pending_legacy_recording_recovery(id)
            .unwrap());
        assert!(unrelated.exists(), "startup aborts before stale sweep");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&scratch_dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_referenced_scratch_aborts_after_publishing_marker() {
        let dir = tmp_dir("preflight-symlink");
        let scratch_dir = tmp_dir("preflight-symlink-scratch");
        let (db, db_path) = tmp_db();
        let id = "preflight-symlink";
        insert_meeting(&db, id, MeetingStatus::Error);
        let target = direct_temp_scratch("symlink-target");
        let symlink = direct_temp_scratch("symlink-link");
        let unrelated = scratch_dir.join("meetnotes-sys-unrelated.wav");
        std::fs::write(&target, b"target").unwrap();
        std::fs::write(&unrelated, b"unrelated").unwrap();
        std::os::unix::fs::symlink(&target, &symlink).unwrap();
        std::fs::write(
            dir.join(format!("{id}.{SIDECAR_EXT}")),
            serde_json::to_vec(&SpillSidecar {
                meeting_id: id.into(),
                sample_rate: 48_000,
                system_scratch_path: Some(symlink.to_string_lossy().into()),
            })
            .unwrap(),
        )
        .unwrap();

        assert!(matches!(
            startup_legacy_recovery_preflight_in(&dir, &db),
            Err(AppError::Locked(_))
        ));
        assert!(db
            .meeting_has_pending_legacy_recording_recovery(id)
            .unwrap());
        assert!(symlink.exists() && target.exists() && unrelated.exists());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&scratch_dir);
        let _ = std::fs::remove_file(&symlink);
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn arbitrary_temp_regular_file_is_not_a_legacy_helper_capability() {
        let dir = tmp_dir("preflight-wrong-basename");
        let (db, db_path) = tmp_db();
        let id = "wrong-basename";
        insert_meeting(&db, id, MeetingStatus::Error);
        let source = std::env::temp_dir().join(format!(
            "private-user-file-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&source, b"must-not-move").unwrap();
        std::fs::write(
            dir.join(format!("{id}.{SIDECAR_EXT}")),
            serde_json::to_vec(&SpillSidecar {
                meeting_id: id.into(),
                sample_rate: 48_000,
                system_scratch_path: Some(source.to_string_lossy().into()),
            })
            .unwrap(),
        )
        .unwrap();

        assert!(matches!(
            startup_legacy_recovery_preflight_in(&dir, &db),
            Err(AppError::Locked(_))
        ));
        assert_eq!(std::fs::read(&source).unwrap(), b"must-not-move");
        let _ = std::fs::remove_file(&source);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn managed_or_nested_file_is_not_a_legacy_temp_capability() {
        let dir = tmp_dir("preflight-managed-path");
        let (db, db_path) = tmp_db();
        let id = "managed-path";
        insert_meeting(&db, id, MeetingStatus::Error);
        let source = dir.join("meetnotes-sys-managed.wav");
        std::fs::write(&source, b"managed-content").unwrap();
        std::fs::write(
            dir.join(format!("{id}.{SIDECAR_EXT}")),
            serde_json::to_vec(&SpillSidecar {
                meeting_id: id.into(),
                sample_rate: 48_000,
                system_scratch_path: Some(source.to_string_lossy().into()),
            })
            .unwrap(),
        )
        .unwrap();

        assert!(matches!(
            startup_legacy_recovery_preflight_in(&dir, &db),
            Err(AppError::Locked(_))
        ));
        assert_eq!(std::fs::read(&source).unwrap(), b"managed-content");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn traversal_spelling_is_rejected_even_when_it_resolves_to_temp_root() {
        let dir = tmp_dir("preflight-traversal");
        let nested = tmp_dir("preflight-traversal-parent");
        let (db, db_path) = tmp_db();
        let id = "traversal-path";
        insert_meeting(&db, id, MeetingStatus::Error);
        let source = direct_temp_scratch("traversal");
        std::fs::write(&source, b"must-not-move").unwrap();
        let traversal = nested.join("..").join(source.file_name().unwrap());
        std::fs::write(
            dir.join(format!("{id}.{SIDECAR_EXT}")),
            serde_json::to_vec(&SpillSidecar {
                meeting_id: id.into(),
                sample_rate: 48_000,
                system_scratch_path: Some(traversal.to_string_lossy().into()),
            })
            .unwrap(),
        )
        .unwrap();

        assert!(matches!(
            startup_legacy_recovery_preflight_in(&dir, &db),
            Err(AppError::Locked(_))
        ));
        assert_eq!(std::fs::read(&source).unwrap(), b"must-not-move");
        let _ = std::fs::remove_file(&source);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&nested);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn duplicate_scratch_path_across_sidecars_is_fatal_and_preserved() {
        let dir = tmp_dir("preflight-duplicate-path");
        let (db, db_path) = tmp_db();
        let source = direct_temp_scratch("duplicate");
        std::fs::write(&source, b"single-owned-source").unwrap();
        for id in ["duplicate-owner-a", "duplicate-owner-b"] {
            insert_meeting(&db, id, MeetingStatus::Error);
            std::fs::write(
                dir.join(format!("{id}.{SIDECAR_EXT}")),
                serde_json::to_vec(&SpillSidecar {
                    meeting_id: id.into(),
                    sample_rate: 48_000,
                    system_scratch_path: Some(source.to_string_lossy().into()),
                })
                .unwrap(),
            )
            .unwrap();
        }

        assert!(matches!(
            startup_legacy_recovery_preflight_in(&dir, &db),
            Err(AppError::Locked(_))
        ));
        assert_eq!(std::fs::read(&source).unwrap(), b"single-owned-source");
        let _ = std::fs::remove_file(&source);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[cfg(unix)]
    #[test]
    fn hardlinked_sidecar_and_external_scratch_are_rejected_without_unlinking() {
        let dir = tmp_dir("preflight-hardlinks");
        let scratch_dir = tmp_dir("preflight-hardlinks-scratch");
        let (db, db_path) = tmp_db();
        let sidecar_id = "hardlinked-sidecar";
        insert_meeting(&db, sidecar_id, MeetingStatus::Error);
        let sidecar = write_legacy_sidecar(&dir, sidecar_id, None);
        let sidecar_alias = dir.join("sidecar.alias");
        std::fs::hard_link(&sidecar, &sidecar_alias).unwrap();

        assert!(matches!(
            startup_legacy_recovery_preflight_in_with_temp_root(&dir, &db, &scratch_dir),
            Err(AppError::Locked(_))
        ));
        assert!(sidecar.exists() && sidecar_alias.exists());

        std::fs::remove_file(&sidecar_alias).unwrap();
        std::fs::remove_file(&sidecar).unwrap();
        let scratch_id = "hardlinked-scratch";
        insert_meeting(&db, scratch_id, MeetingStatus::Error);
        let scratch = scratch_dir.join("meetnotes-sys-hardlinked.wav");
        let scratch_alias = scratch_dir.join("scratch.alias");
        std::fs::write(&scratch, b"system-audio").unwrap();
        std::fs::hard_link(&scratch, &scratch_alias).unwrap();
        let scratch_sidecar = write_legacy_sidecar(&dir, scratch_id, Some(&scratch));

        assert!(matches!(
            startup_legacy_recovery_preflight_in_with_temp_root(&dir, &db, &scratch_dir),
            Err(AppError::Locked(_))
        ));
        assert!(scratch.exists() && scratch_alias.exists() && scratch_sidecar.exists());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&scratch_dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn sidecar_path_traversal_swap_after_preflight_touches_nothing() {
        let dir = tmp_dir("sidecar-swap");
        let (db, db_path) = tmp_db();
        let id = "sidecar-swap";
        insert_meeting(&db, id, MeetingStatus::Recording);
        let mic = dir.join(format!("{id}.{SPILL_EXT}"));
        std::fs::write(&mic, samples_to_f32le(&[0.1, -0.2])).unwrap();
        let sidecar = write_legacy_sidecar(&dir, id, None);
        let mut preflight = startup_legacy_recovery_preflight_in(&dir, &db).unwrap();
        let original_mic = std::fs::read(&mic).unwrap();

        std::fs::remove_file(&sidecar).unwrap();
        std::fs::write(
            &sidecar,
            serde_json::to_vec(&SpillSidecar {
                meeting_id: "../../victim".into(),
                sample_rate: 48_000,
                system_scratch_path: None,
            })
            .unwrap(),
        )
        .unwrap();
        let (jobs, claimed) = claim_inflight_with_preflight(&dir, &db, &mut preflight).unwrap();

        assert!(jobs.is_empty());
        assert_eq!(claimed, vec![id.to_string()]);
        assert_eq!(std::fs::read(&mic).unwrap(), original_mic);
        assert!(sidecar.exists());
        assert!(db
            .meeting_has_pending_legacy_recording_recovery(id)
            .unwrap());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn sidecar_added_after_preflight_is_ignored_and_durably_claimed() {
        let dir = tmp_dir("new-sidecar-after-preflight");
        let (db, db_path) = tmp_db();
        let id = "new-sidecar-after-preflight";
        insert_meeting(&db, id, MeetingStatus::Recording);
        let mut preflight = startup_legacy_recovery_preflight_in(&dir, &db).unwrap();
        let mic = dir.join(format!("{id}.{SPILL_EXT}"));
        std::fs::write(&mic, samples_to_f32le(&[0.1, -0.2])).unwrap();
        let sidecar = write_legacy_sidecar(&dir, id, None);

        assert!(claim_inflight_with_preflight(&dir, &db, &mut preflight).is_err());
        assert!(mic.exists() && sidecar.exists());
        assert!(db
            .meeting_has_pending_legacy_recording_recovery(id)
            .unwrap());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn absent_external_scratch_appearing_after_preflight_preserves_complete_set() {
        let dir = tmp_dir("late-external-scratch");
        let scratch_dir = tmp_dir("late-external-scratch-root");
        let (db, db_path) = tmp_db();
        let id = "late-external-scratch";
        insert_meeting(&db, id, MeetingStatus::Recording);
        let mic = dir.join(format!("{id}.{SPILL_EXT}"));
        std::fs::write(&mic, samples_to_f32le(&[0.1, -0.2])).unwrap();
        let scratch = scratch_dir.join("meetnotes-sys-late.wav");
        let sidecar = write_legacy_sidecar(&dir, id, Some(&scratch));
        let mut preflight =
            startup_legacy_recovery_preflight_in_with_temp_root(&dir, &db, &scratch_dir).unwrap();
        std::fs::write(&scratch, b"late-system-audio").unwrap();

        // The sidecar-owned candidate is reserved even while absent. A helper publishing it after
        // preflight but before claim must survive the actual stale-temp sweep independently of any
        // later ambiguity branch calling preserve_all.
        let future = std::fs::metadata(&scratch).unwrap().modified().unwrap()
            + std::time::Duration::from_secs(7_201);
        let removed_before_claim = crate::audio::aec::sweep_stale_scratch_in(
            &scratch_dir,
            future,
            &preflight.scratch_protection,
        );
        assert_eq!(removed_before_claim, 0);
        assert!(
            scratch.exists(),
            "reserved absent candidate survives before claim"
        );

        let (jobs, claimed) = claim_inflight_with_preflight(&dir, &db, &mut preflight).unwrap();

        assert!(jobs.is_empty());
        assert_eq!(claimed, vec![id.to_string()]);
        assert!(mic.exists() && sidecar.exists() && scratch.exists());

        // The claim ambiguity must also disable the subsequent startup stale-temp sweep. Without
        // preserve_all, a renamed old scratch can appear after preflight and be deleted moments
        // after this function returned, despite the immediate existence assertion above passing.
        let removed = crate::audio::aec::sweep_stale_scratch_in(
            &scratch_dir,
            future,
            &preflight.scratch_protection,
        );
        assert_eq!(removed, 0);
        assert!(
            scratch.exists(),
            "late recovery scratch survives the real sweep phase"
        );

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&scratch_dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn recovery_directory_replacement_after_preflight_preserves_original_tree() {
        let dir = tmp_dir("recovery-directory-swap");
        let original_dir = dir.with_extension("original");
        let (db, db_path) = tmp_db();
        let id = "recovery-directory-swap";
        insert_meeting(&db, id, MeetingStatus::Recording);
        let mic = dir.join(format!("{id}.{SPILL_EXT}"));
        std::fs::write(&mic, samples_to_f32le(&[0.1, -0.2])).unwrap();
        write_legacy_sidecar(&dir, id, None);
        let mut preflight = startup_legacy_recovery_preflight_in(&dir, &db).unwrap();
        std::fs::rename(&dir, &original_dir).unwrap();
        std::fs::create_dir_all(&dir).unwrap();

        assert!(claim_inflight_with_preflight(&dir, &db, &mut preflight).is_err());
        assert!(original_dir.join(format!("{id}.{SPILL_EXT}")).exists());
        assert!(original_dir.join(format!("{id}.{SIDECAR_EXT}")).exists());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&original_dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn claim_preserves_far_side_scratch_with_inflight_clone() {
        let dir = tmp_dir("claim-salvage");
        let (db, db_path) = tmp_db();
        let id = "crashed-meeting-1";
        insert_meeting(&db, id, MeetingStatus::Recording); // genuine crash ghost

        // A far-side system scratch living in a separate temp dir (stand-in for $TMPDIR).
        let sys_scratch = direct_temp_scratch(id);
        std::fs::write(&sys_scratch, b"FAKE-SYS-WAV").unwrap();

        // Spill (2 samples) + sidecar naming the scratch.
        std::fs::write(
            dir.join(format!("{id}.{SPILL_EXT}")),
            samples_to_f32le(&[0.1f32, 0.2]),
        )
        .unwrap();
        let sidecar = SpillSidecar {
            meeting_id: id.into(),
            sample_rate: 48_000,
            system_scratch_path: Some(sys_scratch.to_string_lossy().into()),
        };
        std::fs::write(
            dir.join(format!("{id}.{SIDECAR_EXT}")),
            serde_json::to_vec(&sidecar).unwrap(),
        )
        .unwrap();

        let (jobs, claimed) = claim_inflight_in(&dir, &db);

        assert_eq!(
            claimed,
            vec![id.to_string()],
            "the stuck-recording row is claimed"
        );
        assert!(
            jobs.is_empty(),
            "unsupported paired audio is never enqueued"
        );
        assert!(
            sys_scratch.exists(),
            "the original temp source remains discoverable"
        );
        let preserved = dir.join(format!("{id}.sys.wav"));
        assert!(
            preserved.exists(),
            "a clone is retained outside OS temp cleanup without loading audio into RAM"
        );
        assert_eq!(std::fs::read(&preserved).unwrap(), b"FAKE-SYS-WAV");
        assert!(dir.join(format!("{id}.{SPILL_EXT}")).exists());
        assert!(dir.join(format!("{id}.{SIDECAR_EXT}")).exists());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&sys_scratch);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn claim_discards_orphan_spill_when_row_is_not_recording() {
        let dir = tmp_dir("claim-orphan");
        let (db, db_path) = tmp_db();
        let id = "already-finished";
        insert_meeting(&db, id, MeetingStatus::Summarized); // clean Stop already finished it

        let spill = dir.join(format!("{id}.{SPILL_EXT}"));
        let sidecar = dir.join(format!("{id}.{SIDECAR_EXT}"));
        std::fs::write(&spill, samples_to_f32le(&[0.1f32])).unwrap();
        std::fs::write(
            &sidecar,
            serde_json::to_vec(&SpillSidecar {
                meeting_id: id.into(),
                sample_rate: 48_000,
                system_scratch_path: None,
            })
            .unwrap(),
        )
        .unwrap();

        let (jobs, claimed) = claim_inflight_in(&dir, &db);

        assert!(
            jobs.is_empty() && claimed.is_empty(),
            "a non-recording row is not salvaged"
        );
        assert!(
            !spill.exists() && !sidecar.exists(),
            "the orphan spill + sidecar are cleaned up"
        );
        assert!(!db
            .meeting_has_pending_legacy_recording_recovery(id)
            .unwrap());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn missing_meeting_sidecar_is_cleaned_as_terminal_orphan_without_fk_failure() {
        let dir = tmp_dir("missing-meeting-orphan");
        let (db, db_path) = tmp_db();
        let id = "missing-meeting-orphan";
        let mic = dir.join(format!("{id}.{SPILL_EXT}"));
        std::fs::write(&mic, samples_to_f32le(&[0.1, -0.2])).unwrap();
        let sidecar = write_legacy_sidecar(&dir, id, None);

        let (jobs, claimed) = claim_inflight_in(&dir, &db);

        assert!(jobs.is_empty() && claimed.is_empty());
        assert!(!mic.exists() && !sidecar.exists());
        assert!(!db
            .meeting_has_pending_legacy_recording_recovery(id)
            .unwrap());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn marker_only_after_sidecar_unlink_is_cleared_on_preflight() {
        let dir = tmp_dir("marker-only-repair");
        let (db, db_path) = tmp_db();
        let id = "marker-only-repair";
        insert_meeting(&db, id, MeetingStatus::Summarized);
        db.mark_legacy_recording_recovery_pending(id).unwrap();

        startup_legacy_recovery_preflight_in(&dir, &db).unwrap();

        assert!(!db
            .meeting_has_pending_legacy_recording_recovery(id)
            .unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn marker_only_repair_works_when_inflight_directory_is_missing() {
        let dir = tmp_dir("marker-only-missing-directory");
        std::fs::remove_dir_all(&dir).unwrap();
        let (db, db_path) = tmp_db();
        let id = "marker-only-missing-directory";
        insert_meeting(&db, id, MeetingStatus::Summarized);
        db.mark_legacy_recording_recovery_pending(id).unwrap();

        startup_legacy_recovery_preflight_in(&dir, &db).unwrap();

        assert!(!dir.exists());
        assert!(!db
            .meeting_has_pending_legacy_recording_recovery(id)
            .unwrap());
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn orphan_source_delete_failure_keeps_sidecar_and_marker() {
        let dir = tmp_dir("orphan-hardlink");
        let (db, db_path) = tmp_db();
        let id = "orphan-hardlink";
        insert_meeting(&db, id, MeetingStatus::Summarized);
        let spill = dir.join(format!("{id}.{SPILL_EXT}"));
        let alias_dir = tmp_dir("orphan-hardlink-alias");
        let alias = alias_dir.join("alias");
        let sidecar = dir.join(format!("{id}.{SIDECAR_EXT}"));
        std::fs::write(&spill, samples_to_f32le(&[0.1f32])).unwrap();
        std::fs::hard_link(&spill, &alias).unwrap();
        std::fs::write(
            &sidecar,
            serde_json::to_vec(&SpillSidecar {
                meeting_id: id.into(),
                sample_rate: 48_000,
                system_scratch_path: None,
            })
            .unwrap(),
        )
        .unwrap();

        assert!(matches!(
            startup_legacy_recovery_preflight_in(&dir, &db),
            Err(AppError::Locked(_))
        ));
        assert!(spill.exists() && alias.exists() && sidecar.exists());
        assert!(db
            .meeting_has_pending_legacy_recording_recovery(id)
            .unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&alias_dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn failed_legacy_salvage_is_reclaimed_with_both_tracks_on_restart() {
        let dir = tmp_dir("claim-error-retry");
        let (db, db_path) = tmp_db();
        let id = "failed-legacy-salvage";
        insert_meeting(&db, id, MeetingStatus::Error);
        let spill = dir.join(format!("{id}.{SPILL_EXT}"));
        let sidecar = dir.join(format!("{id}.{SIDECAR_EXT}"));
        let system = dir.join(format!("{id}.{SYSTEM_SCRATCH_SUFFIX}"));
        std::fs::write(&spill, samples_to_f32le(&[0.25, -0.5])).unwrap();
        std::fs::write(&system, b"far-side-survived").unwrap();
        std::fs::write(
            &sidecar,
            serde_json::to_vec(&SpillSidecar {
                meeting_id: id.into(),
                sample_rate: 48_000,
                system_scratch_path: None,
            })
            .unwrap(),
        )
        .unwrap();

        let (jobs, claimed) = claim_inflight_in(&dir, &db);

        assert_eq!(claimed, vec![id.to_string()]);
        assert!(jobs.is_empty());
        assert!(spill.exists() && sidecar.exists() && system.exists());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn claim_removes_sidecar_when_the_spill_is_absent() {
        let dir = tmp_dir("claim-nospill");
        let (db, db_path) = tmp_db();
        let id = "no-spill";
        insert_meeting(&db, id, MeetingStatus::Recording); // recording, but no spill bytes on disk

        let sidecar = dir.join(format!("{id}.{SIDECAR_EXT}"));
        std::fs::write(
            &sidecar,
            serde_json::to_vec(&SpillSidecar {
                meeting_id: id.into(),
                sample_rate: 48_000,
                system_scratch_path: None,
            })
            .unwrap(),
        )
        .unwrap();

        let (jobs, claimed) = claim_inflight_in(&dir, &db);

        assert!(jobs.is_empty() && claimed.is_empty());
        assert!(
            !sidecar.exists(),
            "a sidecar with no spill is removed (nothing to reconstruct)"
        );

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn successful_legacy_salvage_sidecar_cleanup_is_explicit_and_durable() {
        let dir = tmp_dir("adopted-sidecar");
        let sidecar = dir.join(format!("meeting.{SIDECAR_EXT}"));
        std::fs::write(&sidecar, b"{}").unwrap();

        remove_salvaged_legacy_artifact(&sidecar).unwrap();

        assert!(!sidecar.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn over_two_minute_legacy_spill_preserves_full_mic_system_and_sidecar() {
        let dir = tmp_dir("legacy-over-bound");
        let mic = dir.join("meeting.f32le");
        let system = dir.join("meeting.sys.wav");
        let sidecar = dir.join("meeting.json");
        let frames = 8_000u64 * (LEGACY_PIPELINE_MAX_SECONDS + 1);
        let file = std::fs::File::create(&mic).unwrap();
        file.set_len(frames * 4).unwrap();
        file.sync_all().unwrap();
        std::fs::write(&system, b"RIFF-full-far-side").unwrap();
        std::fs::write(&sidecar, b"{}").unwrap();

        let expected = frozen_legacy_artifact(&mic);
        assert!(read_stable_legacy_spill(&mic, 8_000, &expected).is_err());

        assert_eq!(std::fs::metadata(&mic).unwrap().len(), frames * 4);
        assert_eq!(std::fs::read(&system).unwrap(), b"RIFF-full-far-side");
        assert!(sidecar.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn far_side_preflight_creates_no_pipeline_alias_to_swap() {
        let dir = tmp_dir("legacy-system-no-alias");
        let source = dir.join("meeting.sys.wav");
        crate::audio::write_wav_f32(&source, &[0.25, -0.5, 0.75], 48_000, 1).unwrap();
        let bytes = std::fs::read(&source).unwrap();

        preflight_legacy_system_wav(&source).unwrap();

        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_paired_legacy_source_is_rejected_before_pipeline_and_preserved_retryable() {
        let dir = tmp_dir("legacy-paired-unsupported");
        let (db, db_path) = tmp_db();
        let id = "legacy-paired-unsupported";
        insert_meeting(&db, id, MeetingStatus::Error);
        let mic = dir.join(format!("{id}.{SPILL_EXT}"));
        let system = dir.join(format!("{id}.{SYSTEM_SCRATCH_SUFFIX}"));
        let sidecar = dir.join(format!("{id}.{SIDECAR_EXT}"));
        std::fs::write(&mic, samples_to_f32le(&[0.25, -0.5])).unwrap();
        crate::audio::write_wav_f32(&system, &[0.1, -0.1], 48_000, 1).unwrap();
        std::fs::write(
            &sidecar,
            serde_json::to_vec(&SpillSidecar {
                meeting_id: id.into(),
                sample_rate: 48_000,
                system_scratch_path: None,
            })
            .unwrap(),
        )
        .unwrap();
        let mic_before = std::fs::read(&mic).unwrap();
        let system_before = std::fs::read(&system).unwrap();

        let (jobs, claimed) = claim_inflight_in(&dir, &db);

        assert_eq!(claimed, vec![id.to_string()]);
        assert!(
            jobs.is_empty(),
            "paired legacy audio is rejected before enqueue"
        );
        assert_eq!(
            db.get_meeting(id).unwrap().unwrap().status,
            MeetingStatus::Error
        );
        assert_eq!(std::fs::read(&mic).unwrap(), mic_before);
        assert_eq!(std::fs::read(&system).unwrap(), system_before);
        assert!(sidecar.exists());
        assert!(db
            .meeting_has_pending_legacy_recording_recovery(id)
            .unwrap());

        let (restart_jobs, restart_claimed) = claim_inflight_in(&dir, &db);
        assert_eq!(restart_claimed, vec![id.to_string()]);
        assert!(restart_jobs.is_empty());
        assert!(db
            .meeting_has_pending_legacy_recording_recovery(id)
            .unwrap());
        assert_eq!(std::fs::read(&mic).unwrap(), mic_before);
        assert_eq!(std::fs::read(&system).unwrap(), system_before);
        assert!(sidecar.exists());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn locked_at_start_marks_pending_without_cloning_far_side_source() {
        let dir = tmp_dir("legacy-locked-no-clone");
        let (db, db_path) = tmp_db();
        let id = "legacy-locked-no-clone";
        let folder_id = uuid::Uuid::new_v4().to_string();
        insert_meeting(&db, id, MeetingStatus::Error);
        db.insert_folder(&crate::storage::Folder {
            id: folder_id.clone(),
            name: "Private".into(),
            path: "Private".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-22T12:00:00Z".into(),
        })
        .unwrap();
        db.upsert_note(&crate::storage::NoteRecord {
            meeting_id: id.into(),
            provider_id: "test".into(),
            markdown: "sealed".into(),
            created_at: "2026-07-22T12:00:01Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_meeting_folder(id, Some(&folder_id)).unwrap();
        db.set_folder_locked(&folder_id, true, Some(b"wrapped"))
            .unwrap();
        let mic = dir.join(format!("{id}.{SPILL_EXT}"));
        let sidecar = dir.join(format!("{id}.{SIDECAR_EXT}"));
        let system = direct_temp_scratch("locked-no-clone");
        std::fs::write(&mic, samples_to_f32le(&[0.25, -0.5])).unwrap();
        crate::audio::write_wav_f32(&system, &[0.1, -0.1], 48_000, 1).unwrap();
        std::fs::write(
            &sidecar,
            serde_json::to_vec(&SpillSidecar {
                meeting_id: id.into(),
                sample_rate: 48_000,
                system_scratch_path: Some(system.to_string_lossy().into()),
            })
            .unwrap(),
        )
        .unwrap();
        let system_before = std::fs::read(&system).unwrap();

        assert!(matches!(
            startup_legacy_recovery_preflight_in(&dir, &db),
            Err(AppError::Locked(_))
        ));
        assert!(db
            .meeting_has_pending_legacy_recording_recovery(id)
            .unwrap());
        assert_eq!(std::fs::read(&system).unwrap(), system_before);
        assert!(!dir.join(format!("{id}.{SYSTEM_SCRATCH_SUFFIX}")).exists());
        assert!(mic.exists() && sidecar.exists());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&system);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn successful_mic_only_cleanup_clears_durable_legacy_marker() {
        let dir = tmp_dir("legacy-mic-cleanup-marker");
        let (db, db_path) = tmp_db();
        let id = "legacy-mic-cleanup-marker";
        insert_meeting(&db, id, MeetingStatus::Error);
        let mic = dir.join(format!("{id}.{SPILL_EXT}"));
        let sidecar = dir.join(format!("{id}.{SIDECAR_EXT}"));
        std::fs::write(&mic, samples_to_f32le(&[0.25, -0.5])).unwrap();
        std::fs::write(
            &sidecar,
            serde_json::to_vec(&SpillSidecar {
                meeting_id: id.into(),
                sample_rate: 48_000,
                system_scratch_path: None,
            })
            .unwrap(),
        )
        .unwrap();
        let (jobs, claimed) = claim_inflight_in(&dir, &db);
        assert_eq!(claimed, vec![id.to_string()]);
        assert_eq!(jobs.len(), 1);
        let (_, mic_proof) =
            read_stable_legacy_spill(&mic, 48_000, &jobs[0].mic_capability).unwrap();

        complete_legacy_salvage_cleanup(&db, &jobs[0], &mic, &mic_proof).unwrap();

        assert!(!mic.exists() && !sidecar.exists());
        assert!(!db
            .meeting_has_pending_legacy_recording_recovery(id)
            .unwrap());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn mic_only_cleanup_preserves_everything_when_system_track_appears_async() {
        let dir = tmp_dir("legacy-mic-cleanup-late-system");
        let (db, db_path) = tmp_db();
        let id = "legacy-mic-cleanup-late-system";
        insert_meeting(&db, id, MeetingStatus::Error);
        let mic = dir.join(format!("{id}.{SPILL_EXT}"));
        let sidecar = dir.join(format!("{id}.{SIDECAR_EXT}"));
        let system = dir.join(format!("{id}.{SYSTEM_SCRATCH_SUFFIX}"));
        std::fs::write(&mic, samples_to_f32le(&[0.25, -0.5])).unwrap();
        std::fs::write(
            &sidecar,
            serde_json::to_vec(&SpillSidecar {
                meeting_id: id.into(),
                sample_rate: 48_000,
                system_scratch_path: None,
            })
            .unwrap(),
        )
        .unwrap();
        let (jobs, claimed) = claim_inflight_in(&dir, &db);
        assert_eq!(claimed, vec![id.to_string()]);
        assert_eq!(jobs.len(), 1);
        let (_, mic_proof) =
            read_stable_legacy_spill(&mic, 48_000, &jobs[0].mic_capability).unwrap();
        std::fs::write(&system, b"late-far-side").unwrap();

        assert!(complete_legacy_salvage_cleanup(&db, &jobs[0], &mic, &mic_proof).is_err());
        assert!(mic.exists() && sidecar.exists() && system.exists());
        assert!(db
            .meeting_has_pending_legacy_recording_recovery(id)
            .unwrap());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn huge_declared_far_side_duration_is_rejected_from_header_without_sample_read() {
        let dir = tmp_dir("legacy-system-huge-header");
        let source = dir.join("meeting.sys.wav");
        let frames = 8_000u32 * (LEGACY_PIPELINE_MAX_SECONDS as u32 + 1);
        let data_bytes = frames * 2;
        let mut header = Vec::with_capacity(44);
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&(36u32 + data_bytes).to_le_bytes());
        header.extend_from_slice(b"WAVEfmt ");
        header.extend_from_slice(&16u32.to_le_bytes());
        header.extend_from_slice(&1u16.to_le_bytes());
        header.extend_from_slice(&1u16.to_le_bytes());
        header.extend_from_slice(&8_000u32.to_le_bytes());
        header.extend_from_slice(&16_000u32.to_le_bytes());
        header.extend_from_slice(&2u16.to_le_bytes());
        header.extend_from_slice(&16u16.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&data_bytes.to_le_bytes());
        std::fs::write(&source, &header).unwrap();

        assert!(preflight_legacy_system_wav(&source).is_err());

        assert_eq!(std::fs::read(&source).unwrap(), header);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn durable_generation_wins_and_preserves_legacy_restart_artifacts() {
        let dir = tmp_dir("ledger-wins-legacy");
        let (db, db_path) = tmp_db();
        let id = uuid::Uuid::new_v4().hyphenated().to_string();
        insert_meeting(&db, &id, MeetingStatus::Recording);
        let spill = dir.join(format!("{id}.{SPILL_EXT}"));
        let sidecar = dir.join(format!("{id}.{SIDECAR_EXT}"));
        std::fs::write(&spill, samples_to_f32le(&[0.25, -0.5])).unwrap();
        std::fs::write(
            &sidecar,
            serde_json::to_vec(&SpillSidecar {
                meeting_id: id.clone(),
                sample_rate: 48_000,
                system_scratch_path: None,
            })
            .unwrap(),
        )
        .unwrap();
        let recording =
            crate::audio::source::adopt_legacy_raw_recording(&db, &id, &spill, 48_000, None, &dir)
                .unwrap();

        let (jobs, claimed) = claim_inflight_in(&dir, &db);

        assert!(
            jobs.is_empty(),
            "the ledger generation must not be adopted twice"
        );
        assert_eq!(claimed, vec![id]);
        assert!(spill.exists() && sidecar.exists());
        recording.release_for_recovery(&db).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    // ── Re-claim prefers the already-preserved inflight sys.wav clone ──────────────────────────

    #[test]
    fn resolve_system_wav_prefers_the_preserved_inflight_clone() {
        let dir = tmp_dir("resolve-prefer");
        let id = "re-salvaged";
        // A prior crash MID-salvage already preserved the far-side track here.
        let preserved = dir.join(format!("{id}.{SYSTEM_SCRATCH_SUFFIX}"));
        std::fs::write(&preserved, b"PRESERVED-SYS").unwrap();
        // The original $TMPDIR scratch the sidecar still names (stand-in — it may even be gone).
        let tmpdir = tmp_dir("resolve-tmp");
        let orig = tmpdir.join("orig-sys.wav");
        std::fs::write(&orig, b"ORIG-SYS").unwrap();

        let moved_state = present_legacy_artifact(&preserved);
        let mut protection = crate::audio::aec::StaleScratchProtection::default();
        let directory = validate_legacy_recovery_directory(&dir, &mut protection).unwrap();
        let got = resolve_system_wav(&dir, id, &directory, &moved_state, None).unwrap();
        assert_eq!(
            got.as_deref(),
            Some(preserved.as_path()),
            "re-claim prefers the preserved inflight sys.wav clone over the $TMPDIR path"
        );
        assert!(
            orig.exists(),
            "the $TMPDIR scratch is left untouched when an inflight clone exists"
        );

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn claim_reclaims_mid_salvage_crash_preferring_preserved_far_side_clone() {
        let dir = tmp_dir("claim-reclaim");
        let (db, db_path) = tmp_db();
        let id = "reclaimed-1";
        // Salvage crashed before setting a final status ⇒ the row is still a stuck RECORDING ghost.
        insert_meeting(&db, id, MeetingStatus::Recording);

        // The far-side track was already preserved in inflight by the crashed salvage. This
        // fixture has no external scratch; recovery must retain the governed inflight clone.
        let preserved = dir.join(format!("{id}.{SYSTEM_SCRATCH_SUFFIX}"));
        std::fs::write(&preserved, b"PRESERVED-SYS").unwrap();
        std::fs::write(
            dir.join(format!("{id}.{SPILL_EXT}")),
            samples_to_f32le(&[0.1f32, 0.2]),
        )
        .unwrap();
        let sidecar = SpillSidecar {
            meeting_id: id.into(),
            sample_rate: 48_000,
            system_scratch_path: None,
        };
        std::fs::write(
            dir.join(format!("{id}.{SIDECAR_EXT}")),
            serde_json::to_vec(&sidecar).unwrap(),
        )
        .unwrap();

        let (jobs, claimed) = claim_inflight_in(&dir, &db);

        assert_eq!(claimed, vec![id.to_string()]);
        assert!(jobs.is_empty(), "paired legacy audio remains deferred");
        assert!(
            preserved.exists(),
            "the preserved far-side clone survives the re-claim"
        );

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    // ── Sidecar-less far-side tracks remain durably governed and untouched ────────────────────

    #[test]
    fn sidecarless_system_track_for_locked_owner_aborts_and_stays_governed() {
        let dir = tmp_dir("locked-system-only");
        let (db, db_path) = tmp_db();
        let id = "locked-system-only";
        let folder_id = uuid::Uuid::new_v4().to_string();
        insert_meeting(&db, id, MeetingStatus::Summarized);
        db.insert_folder(&crate::storage::Folder {
            id: folder_id.clone(),
            name: "Private".into(),
            path: "Private".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-22T12:00:00Z".into(),
        })
        .unwrap();
        db.upsert_note(&crate::storage::NoteRecord {
            meeting_id: id.into(),
            provider_id: "test".into(),
            markdown: "sealed".into(),
            created_at: "2026-07-22T12:00:01Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_meeting_folder(id, Some(&folder_id)).unwrap();
        db.set_folder_locked(&folder_id, true, Some(b"wrapped"))
            .unwrap();
        let system = dir.join(format!("{id}.{SYSTEM_SCRATCH_SUFFIX}"));
        std::fs::write(&system, b"sole-far-side-copy").unwrap();

        assert!(matches!(
            startup_legacy_recovery_preflight_in(&dir, &db),
            Err(AppError::Locked(_))
        ));
        assert!(system.exists());
        assert!(db
            .meeting_has_pending_legacy_recording_recovery(id)
            .unwrap());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn startup_preflight_preserves_all_sidecarless_system_tracks_and_marks_meeting_owners() {
        let dir = tmp_dir("preflight-cleanup-ownership");
        let generation_dir = tmp_dir("preflight-cleanup-generation");
        let (db, db_path) = tmp_db();

        let orphan = dir.join(format!("orphan.{SYSTEM_SCRATCH_SUFFIX}"));
        std::fs::write(&orphan, b"orphan-system").unwrap();

        let recording_id = "11111111-1111-4111-8111-111111111111";
        insert_meeting(&db, recording_id, MeetingStatus::Recording);
        let recording_system = dir.join(format!("{recording_id}.{SYSTEM_SCRATCH_SUFFIX}"));
        std::fs::write(&recording_system, b"recording-system").unwrap();

        let generation_id = "22222222-2222-4222-8222-222222222222";
        insert_meeting(&db, generation_id, MeetingStatus::Recording);
        let generation_raw = generation_dir.join("generation-source.f32le");
        std::fs::write(&generation_raw, samples_to_f32le(&[0.1, -0.2])).unwrap();
        let generation = crate::audio::source::adopt_legacy_raw_recording(
            &db,
            generation_id,
            &generation_raw,
            48_000,
            None,
            &generation_dir,
        )
        .unwrap();
        db.update_meeting_status(generation_id, MeetingStatus::Summarized)
            .unwrap();
        let generation_system = dir.join(format!("{generation_id}.{SYSTEM_SCRATCH_SUFFIX}"));
        std::fs::write(&generation_system, b"generation-system").unwrap();

        let mut preflight = startup_legacy_recovery_preflight_in(&dir, &db).unwrap();
        let (jobs, claimed) = claim_inflight_with_preflight(&dir, &db, &mut preflight).unwrap();

        assert!(jobs.is_empty() && claimed.is_empty());
        assert!(
            orphan.exists(),
            "missing-meeting track is never guessed disposable"
        );
        assert!(recording_system.exists());
        assert!(generation_system.exists());
        assert!(db
            .meeting_has_pending_legacy_recording_recovery(recording_id)
            .unwrap());
        assert!(db
            .meeting_has_pending_legacy_recording_recovery(generation_id)
            .unwrap());

        generation.release_for_recovery(&db).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&generation_dir);
        let _ = std::fs::remove_file(&db_path);
    }

    #[cfg(unix)]
    #[test]
    fn hardlinked_mic_after_preflight_preserves_external_and_inflight_system_sources() {
        let dir = tmp_dir("terminal-hardlinked-mic");
        let scratch_dir = tmp_dir("terminal-hardlinked-mic-scratch");
        let (db, db_path) = tmp_db();
        let id = "terminal-hardlinked-mic";
        insert_meeting(&db, id, MeetingStatus::Summarized);
        let mic = dir.join(format!("{id}.{SPILL_EXT}"));
        let mic_alias = dir.join("mic.alias");
        let preserved = dir.join(format!("{id}.{SYSTEM_SCRATCH_SUFFIX}"));
        let external = scratch_dir.join("meetnotes-sys-terminal-hardlink.wav");
        std::fs::write(&mic, samples_to_f32le(&[0.1, -0.2])).unwrap();
        std::fs::write(&preserved, b"preserved-system").unwrap();
        std::fs::write(&external, b"external-system").unwrap();
        let sidecar = write_legacy_sidecar(&dir, id, Some(&external));
        let mut preflight =
            startup_legacy_recovery_preflight_in_with_temp_root(&dir, &db, &scratch_dir).unwrap();
        std::fs::hard_link(&mic, &mic_alias).unwrap();

        let (jobs, claimed) = claim_inflight_with_preflight(&dir, &db, &mut preflight).unwrap();

        assert!(jobs.is_empty());
        assert_eq!(claimed, vec![id.to_string()]);
        assert!(mic.exists() && mic_alias.exists());
        assert!(preserved.exists() && external.exists() && sidecar.exists());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&scratch_dir);
        let _ = std::fs::remove_file(&db_path);
    }

    // ── STAGE 3: disk-salvage claims (2026-07-16 salvage-from-disk) ────────────────────────────

    fn insert_meeting_with_audio(db: &Db, id: &str, status: MeetingStatus, audio: &Path) {
        db.insert_meeting(&crate::storage::Meeting {
            id: id.into(),
            started_at: "2026-07-16T09:00:00Z".into(),
            ended_at: None,
            title: None,
            duration_s: 60,
            audio_path: Some(audio.to_string_lossy().to_string()),
            status,
            folder_id: None,
        })
        .unwrap();
    }

    /// Put `id` into a LOCKED folder — folder membership rides the meeting's NOTE row
    /// (`folder_for_meeting` reads `notes.folder_id`), so a note row is required.
    fn file_into_locked_folder(db: &Db, id: &str, folder_id: &str) {
        db.insert_folder(&crate::storage::Folder {
            id: folder_id.into(),
            name: folder_id.into(),
            path: folder_id.into(),
            parent_id: None,
            locked: true,
            created_at: "2026-07-16T00:00:00Z".into(),
        })
        .unwrap();
        db.upsert_note(&crate::storage::NoteRecord {
            meeting_id: id.into(),
            provider_id: "claude_code".into(),
            markdown: String::new(),
            created_at: "2026-07-16T00:00:00Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_meeting_folder(id, Some(folder_id)).unwrap();
    }

    #[test]
    fn disk_salvage_plan_truth_table() {
        use DiskSalvagePlan::*;
        assert_eq!(
            disk_salvage_plan(true, false, true),
            Salvage,
            "plaintext WAV + open folder ⇒ re-run"
        );
        assert_eq!(
            disk_salvage_plan(true, true, true),
            Salvage,
            "plaintext WAV present ⇒ re-run (the .enc twin is irrelevant)"
        );
        assert_eq!(
            disk_salvage_plan(false, true, true),
            DeferSealed,
            "only the sealed .enc ⇒ never decrypt for salvage"
        );
        assert_eq!(
            disk_salvage_plan(true, false, false),
            DeferSealed,
            "locked folder defers even with a plaintext WAV (fail closed)"
        );
        assert_eq!(disk_salvage_plan(false, true, false), DeferSealed);
        assert_eq!(
            disk_salvage_plan(false, false, true),
            NoAudio,
            "nothing on disk ⇒ reconcile to ERROR as before"
        );
        assert_eq!(
            disk_salvage_plan(false, false, false),
            DeferSealed,
            "locked wins over missing audio (still fail closed)"
        );
    }

    /// RED-before-GREEN (the ITEM-1 headline): pre-fix, a stuck-RECORDING row whose archive WAV
    /// survived on disk (the pipeline died AFTER `finalize_meeting`) was reconciled straight to
    /// ERROR — intact audio never re-transcribed. Post-fix the claim runs BEFORE reconcile and the
    /// row is scheduled for a from-disk re-run instead of the ERROR flip.
    #[test]
    fn stuck_recording_with_surviving_archive_is_claimed_not_errored() {
        let dir = tmp_dir("disk-claim");
        let (db, db_path) = tmp_db();
        let id = "disk-crash-1";
        let wav = dir.join(format!("{id}.wav"));
        std::fs::write(&wav, b"RIFF-fake-archive").unwrap();
        insert_meeting_with_audio(&db, id, MeetingStatus::Recording, &wav);

        // The exact lib.rs setup ordering: spill claim → disk claim → reconcile-except.
        let (spill_jobs, mut claimed) = claim_inflight_in(&dir, &db);
        assert!(spill_jobs.is_empty(), "no spill survives in this scenario");
        let (disk_jobs, disk_claimed) = claim_disk_salvage(&db, &claimed);
        claimed.extend(disk_claimed);
        db.reconcile_stuck_recordings_except(&claimed).unwrap();

        assert_eq!(
            disk_jobs.len(),
            1,
            "the surviving archive is claimed for a from-disk re-run"
        );
        assert_eq!(disk_jobs[0].meeting_id, id);
        assert_eq!(disk_jobs[0].wav_path, wav);
        assert_eq!(
            db.get_meeting(id).unwrap().unwrap().status,
            MeetingStatus::Recording,
            "a disk-claimed row must NOT be flipped to ERROR — the salvage worker owns its final status"
        );
        assert!(wav.exists(), "the claim never touches the audio file");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    /// A stuck-RECORDING row with NO usable audio (no `audio_path`, or the file is gone) is NOT
    /// claimed — reconcile flips it to the honest terminal ERROR exactly as before.
    #[test]
    fn stuck_recording_without_audio_still_reconciles_to_error() {
        let dir = tmp_dir("disk-noaudio");
        let (db, db_path) = tmp_db();
        insert_meeting(&db, "no-path", MeetingStatus::Recording); // audio_path: None
        let gone = dir.join("gone.wav"); // named but never written
        insert_meeting_with_audio(&db, "file-gone", MeetingStatus::Recording, &gone);

        let (disk_jobs, claimed) = claim_disk_salvage(&db, &[]);
        db.reconcile_stuck_recordings_except(&claimed).unwrap();

        assert!(
            disk_jobs.is_empty() && claimed.is_empty(),
            "nothing to re-transcribe ⇒ nothing claimed"
        );
        for id in ["no-path", "file-gone"] {
            assert_eq!(
                db.get_meeting(id).unwrap().unwrap().status,
                MeetingStatus::Error,
                "an audio-less ghost still becomes a terminal ERROR row"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    /// LOCK MODEL: a stuck-RECORDING meeting in a LOCKED folder is NEVER claimed — with sealed
    /// `.enc`-only audio there is nothing to decrypt with at launch (the session unlock set is
    /// empty), and even a plaintext crash-window WAV must not be re-pipelined into fresh plaintext
    /// behind the lock. The claim leaves every audio file byte-untouched; reconcile makes the row a
    /// terminal ERROR that stays recoverable via unlock + `retry_transcription`.
    #[test]
    fn sealed_folder_salvage_defers_without_decrypting_or_leaking() {
        let dir = tmp_dir("disk-sealed");
        let (db, db_path) = tmp_db();

        // Meeting A: audio exists ONLY as the sealed `.enc`.
        let wav_a = dir.join("sealed-a.wav");
        let enc_a = dir.join("sealed-a.wav.enc");
        std::fs::write(&enc_a, b"AES-GCM-ciphertext").unwrap();
        insert_meeting_with_audio(&db, "m-sealed-a", MeetingStatus::Recording, &wav_a);
        file_into_locked_folder(&db, "m-sealed-a", "f-locked-a");

        // Meeting B: a PLAINTEXT WAV survived the crash window inside a locked folder.
        let wav_b = dir.join("sealed-b.wav");
        std::fs::write(&wav_b, b"RIFF-fake-archive").unwrap();
        insert_meeting_with_audio(&db, "m-sealed-b", MeetingStatus::Recording, &wav_b);
        file_into_locked_folder(&db, "m-sealed-b", "f-locked-b");

        let (disk_jobs, claimed) = claim_disk_salvage(&db, &[]);
        assert!(
            disk_jobs.is_empty() && claimed.is_empty(),
            "a locked folder's meetings are never claimed for salvage (fail closed)"
        );
        assert!(enc_a.exists(), "the sealed .enc is byte-untouched");
        assert!(
            !wav_a.exists(),
            "no plaintext was materialized for the sealed meeting"
        );
        assert!(wav_b.exists(), "the claim never deletes audio either");

        db.reconcile_stuck_recordings_except(&claimed).unwrap();
        for id in ["m-sealed-a", "m-sealed-b"] {
            assert_eq!(
                db.get_meeting(id).unwrap().unwrap().status,
                MeetingStatus::Error,
                "the deferred row becomes a terminal ERROR — recoverable via unlock + retry"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    /// Spill priority: a meeting the SPILL salvage already claimed (dual-stream audio — strictly
    /// richer than the mixed archive) is skipped by the disk claim.
    #[test]
    fn spill_claim_wins_over_disk_claim() {
        let dir = tmp_dir("disk-priority");
        let (db, db_path) = tmp_db();
        let id = "both-sources";
        let wav = dir.join(format!("{id}.wav"));
        std::fs::write(&wav, b"RIFF-fake-archive").unwrap();
        insert_meeting_with_audio(&db, id, MeetingStatus::Recording, &wav);

        let (disk_jobs, claimed) = claim_disk_salvage(&db, &[id.to_string()]);
        assert!(
            disk_jobs.is_empty() && claimed.is_empty(),
            "a spill-claimed meeting is never double-claimed by the disk salvage"
        );

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }
}
