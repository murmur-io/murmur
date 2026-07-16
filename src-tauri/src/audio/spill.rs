//! Crash-salvage of in-flight recordings (STAGE 2; STAGE 1 = the ghost-row reconcile in
//! `Db::reconcile_stuck_recordings`).
//!
//! THE GAP this closes: mic audio lives ONLY in RAM (`recorder::Shared.samples`) until Stop, so a
//! crash / SIGKILL / `tauri dev` hot-rebuild mid-recording loses the whole meeting's audio. This
//! module MIRRORS the growing RAM mic buffer to a plaintext spill file on disk DURING the
//! recording — a FULL mirror of the RAM buffer (not a downsample/summary): raw mono `f32` at the
//! device rate, i.e. ~0.7 GB per hour at 48 kHz. Written on its own NON-real-time writer thread,
//! never in the cpal data callback — plus a
//! sidecar JSON naming the source sample rate + the paired far-side system-audio scratch WAV. On a
//! clean Stop the spill + sidecar are deleted (RAII, mirroring `pipeline::ScratchWav`). At the NEXT
//! launch, [`claim_inflight`] finds any surviving spill (⇒ a genuine crash), reconstructs the mic
//! samples, MOVES the paired system scratch out of `$TMPDIR` before the reaper deletes it, and the
//! spawned salvage worker runs the meeting through the EXISTING post-Stop pipeline
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

use std::io::Write;
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
/// sibling of `audio/`, NOT `$TMPDIR`, so the stale-scratch sweep + storage auto-prune never reap a
/// live recording (see the module header).
const INFLIGHT_SUBDIR: &str = "inflight";
/// Extension of the raw mic spill (mono `f32` little-endian at the device rate).
const SPILL_EXT: &str = "f32le";
/// Extension of the sidecar JSON ([`SpillSidecar`]).
const SIDECAR_EXT: &str = "json";
/// Suffix of the far-side system-audio scratch MOVED into the inflight dir during a claim
/// (`<meeting_id>.sys.wav`). A compound suffix, so it is stripped/matched as a whole.
const SYSTEM_SCRATCH_SUFFIX: &str = "sys.wav";
/// How often the writer thread mirrors the newly-captured tail to disk. A crash loses at most this
/// much of the recording's tail (the RAM buffer holds everything up to the crash instant regardless).
const FLUSH_INTERVAL: Duration = Duration::from_millis(1000);

/// `<app-data>/<app_dir_name()>/inflight`, created if absent. See [`INFLIGHT_SUBDIR`] for the
/// deliberate app-data (not `$TMPDIR`) placement.
pub fn inflight_dir() -> Result<PathBuf> {
    let base = dirs::data_dir()
        .ok_or_else(|| AppError::Storage("could not resolve app-data directory".into()))?;
    let dir = base.join(crate::state::app_dir_name()).join(INFLIGHT_SUBDIR);
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
    /// The far-side system WAV, MOVED into the inflight dir during the claim (out of `$TMPDIR` before
    /// the reaper deletes it). `None` when the recording had no system track or the move failed.
    pub system_wav: Option<PathBuf>,
}

/// SYNCHRONOUS startup CLAIM — runs in `lib.rs` setup BEFORE `reconcile_stuck_recordings` and the
/// reaper/sweep. Scans the inflight dir; per sidecar it decides via [`salvage_plan`]:
/// - `Salvage`: MOVE the paired system scratch out of `$TMPDIR` into the inflight dir (before the
///   reaper deletes it) and emit a [`SalvageJob`]; the claimed `meeting_id` is returned so reconcile
///   skips it (salvage sets the final status itself).
/// - `DiscardOrphan` / `NoSpill`: delete the stale spill + sidecar in place (NEVER touches the
///   `audio/` dir or any real recording).
///
/// Returns `(jobs, claimed_meeting_ids)`. Best-effort + panic-free: a bad sidecar is logged (no PII)
/// and skipped; nothing here crashes launch and nothing here deletes an un-salvaged spill.
pub fn claim_inflight(db: &Db) -> (Vec<SalvageJob>, Vec<String>) {
    let dir = match inflight_dir() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(target: "startup", error = %e, "salvage: inflight dir unavailable; skipping claim");
            return (Vec::new(), Vec::new());
        }
    };
    claim_inflight_in(&dir, db)
}

/// Core of [`claim_inflight`] with the inflight `dir` injected, so the claim logic is testable
/// against a temp dir + a real DB without touching app-data.
fn claim_inflight_in(dir: &Path, db: &Db) -> (Vec<SalvageJob>, Vec<String>) {
    let mut jobs = Vec::new();
    let mut claimed = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return (jobs, claimed), // no inflight dir yet ⇒ nothing to salvage.
    };
    for entry in entries.flatten() {
        let sidecar_path = entry.path();
        if sidecar_path.extension().and_then(|e| e.to_str()) != Some(SIDECAR_EXT) {
            continue;
        }
        let sidecar: SpillSidecar = match std::fs::read(&sidecar_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            Some(s) => s,
            None => {
                // Unreadable/corrupt sidecar (e.g. a truncated write): leave it in place, don't guess.
                tracing::warn!(target: "startup", "salvage: skipping an unreadable inflight sidecar");
                continue;
            }
        };
        let meeting_id = sidecar.meeting_id.clone();
        let spill_path = dir.join(format!("{meeting_id}.{SPILL_EXT}"));
        // ≥ 4 bytes ⇒ at least one complete f32 sample worth reconstructing.
        let spill_present = std::fs::metadata(&spill_path)
            .map(|m| m.len() >= 4)
            .unwrap_or(false);
        let row_is_stuck = db
            .get_meeting(&meeting_id)
            .ok()
            .flatten()
            .map(|m| m.status == MeetingStatus::Recording)
            .unwrap_or(false);

        match salvage_plan(spill_present, row_is_stuck) {
            SalvagePlan::Salvage => {
                let system_wav = resolve_system_wav(
                    dir,
                    &meeting_id,
                    sidecar.system_scratch_path.as_deref(),
                );
                jobs.push(SalvageJob {
                    meeting_id: meeting_id.clone(),
                    spill_path,
                    sidecar_path,
                    sample_rate: sidecar.sample_rate,
                    system_wav,
                });
                claimed.push(meeting_id);
            }
            SalvagePlan::DiscardOrphan | SalvagePlan::NoSpill => {
                let _ = std::fs::remove_file(&spill_path);
                let _ = std::fs::remove_file(&sidecar_path);
            }
        }
    }
    // GC any orphaned far-side scratch left by a completed/aborted job (its spill + sidecar are now
    // gone). Runs AFTER the sidecar loop so this launch's own DiscardOrphan/NoSpill deletions are
    // already reflected. NEVER touches a live/claimed job's sys.wav (those keep their spill+sidecar).
    reap_orphaned_system_wavs(dir);
    if !claimed.is_empty() {
        tracing::info!(
            target: "startup",
            salvaging = claimed.len(),
            "claimed crashed in-flight recording(s) for salvage"
        );
    }
    (jobs, claimed)
}

/// Resolve the far-side system WAV for a salvage job. RE-CLAIM SAFETY: PREFER an already-present
/// `inflight/<id>.sys.wav` — a prior crash MID-salvage may have already moved the far-side track out
/// of `$TMPDIR` (which by now the reaper has almost certainly deleted), and that moved copy is the
/// only recoverable one. Only if no moved copy exists do we do the first-time move out of `$TMPDIR`.
/// `None` ⇒ salvage proceeds mic-only.
fn resolve_system_wav(dir: &Path, meeting_id: &str, tmpdir_src: Option<&str>) -> Option<PathBuf> {
    let moved = dir.join(format!("{meeting_id}.{SYSTEM_SCRATCH_SUFFIX}"));
    if moved.exists() {
        return Some(moved);
    }
    tmpdir_src.and_then(|p| claim_system_scratch(Path::new(p), dir, meeting_id))
}

/// MOVE the far-side system scratch WAV out of `$TMPDIR` into the inflight dir (before the reaper
/// deletes it). Prefer a rename (same-volume, atomic); fall back to copy+remove across volumes. `None`
/// if the scratch is absent or the move failed → salvage proceeds mic-only.
fn claim_system_scratch(src: &Path, dir: &Path, meeting_id: &str) -> Option<PathBuf> {
    if !src.exists() {
        return None;
    }
    let dst = dir.join(format!("{meeting_id}.{SYSTEM_SCRATCH_SUFFIX}"));
    if std::fs::rename(src, &dst).is_ok() {
        return Some(dst);
    }
    match std::fs::copy(src, &dst) {
        Ok(_) => {
            let _ = std::fs::remove_file(src);
            Some(dst)
        }
        Err(_) => None,
    }
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
pub fn claim_disk_salvage(db: &Db, already_claimed: &[String]) -> (Vec<DiskSalvageJob>, Vec<String>) {
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

/// GC pass over the inflight dir: delete any orphaned `<id>.sys.wav` whose paired spill (`<id>.f32le`)
/// AND sidecar (`<id>.json`) are BOTH gone — i.e. its salvage lifecycle is over (the job succeeded, was
/// discarded, or preserved the far-side track on a mic-read error) and the file is now garbage that
/// would otherwise accumulate. HARD INVARIANT: NEVER reap a sys.wav whose spill OR sidecar still
/// exists — that is a LIVE / re-claimable salvage job (a crash mid-salvage), whose far-side track
/// [`resolve_system_wav`] must still be able to recover. Best-effort + panic-free; inflight dir only.
fn reap_orphaned_system_wavs(dir: &Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let suffix = format!(".{SYSTEM_SCRATCH_SUFFIX}");
    for entry in entries.flatten() {
        let path = entry.path();
        let id = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => match name.strip_suffix(&suffix) {
                Some(id) if !id.is_empty() => id.to_string(),
                _ => continue, // not an `<id>.sys.wav`
            },
            None => continue,
        };
        let spill = dir.join(format!("{id}.{SPILL_EXT}"));
        let sidecar = dir.join(format!("{id}.{SIDECAR_EXT}"));
        // Live salvage job (spill and/or sidecar still on disk) ⇒ leave the far-side track alone.
        if spill.exists() || sidecar.exists() {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            tracing::info!(target: "startup", "reaped an orphaned crash-salvage far-side scratch");
        }
    }
}

/// Spawn the DETACHED salvage worker (mirrors `transcribe::live::spawn_assistant_turn`): ONE OS
/// thread with its own current-thread runtime runs each claimed job through the EXISTING post-Stop
/// pipeline sequentially. Best-effort + panic-free: a job failure marks that meeting `Error` (via
/// `run_after_stop`) with its salvaged audio still attached — it NEVER deletes audio and NEVER
/// crashes launch. A no-op when there are no jobs.
pub fn spawn_salvage(app: AppHandle, jobs: Vec<SalvageJob>, disk_jobs: Vec<DiskSalvageJob>) {
    if jobs.is_empty() && disk_jobs.is_empty() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("murmur-crash-salvage".into())
        .spawn(move || run_salvage_jobs(app, jobs, disk_jobs));
}

fn run_salvage_jobs(app: AppHandle, jobs: Vec<SalvageJob>, disk_jobs: Vec<DiskSalvageJob>) {
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
    // Spill jobs FIRST (richer dual-stream audio), then the from-disk re-runs. Sequential on one
    // thread — heavy ASR additionally serializes through the shared `perf::run_heavy` gate.
    for job in jobs {
        rt.block_on(salvage_one(&app, job));
    }
    for job in disk_jobs {
        rt.block_on(salvage_disk_one(&app, job));
    }
}

async fn salvage_one(app: &AppHandle, job: SalvageJob) {
    let state = app.state::<AppState>();

    // Reconstruct the mic samples from the spill (pure decode). An unreadable / empty spill or a
    // bogus rate can't produce the mic track: mark the row Error honestly — but NEVER delete a
    // readable, RECOVERABLE far-side `sys.wav` on this error path. `cleanup_job_files_preserving_system`
    // removes only the (unusable) mic spill + sidecar; a present far-side WAV is left on disk for
    // manual recovery, and is later GC'd by `reap_orphaned_system_wavs` once it is fully orphaned.
    let samples = match std::fs::read(&job.spill_path) {
        Ok(bytes) => f32le_to_samples(&bytes),
        Err(e) => {
            tracing::warn!(target: "startup", meeting_id = %job.meeting_id, error = %e, far_side_preserved = job.system_wav.is_some(), "salvage: mic spill unreadable; marking meeting Error (far-side sys.wav preserved)");
            let _ = state
                .db
                .update_meeting_status(&job.meeting_id, MeetingStatus::Error);
            cleanup_job_files_preserving_system(&job);
            return;
        }
    };
    if samples.is_empty() || job.sample_rate == 0 {
        tracing::warn!(target: "startup", meeting_id = %job.meeting_id, far_side_preserved = job.system_wav.is_some(), "salvage: no reconstructable mic audio; marking meeting Error (far-side sys.wav preserved)");
        let _ = state
            .db
            .update_meeting_status(&job.meeting_id, MeetingStatus::Error);
        cleanup_job_files_preserving_system(&job);
        return;
    }

    let duration_s = (samples.len() as f64 / job.sample_rate as f64).round() as i64;
    // A crashed session's monotonic capture-start Instants are gone; both streams started ≈ record
    // start, so anchor both to a single fresh `now`. The wall-clock merge then interleaves by each
    // stream's internal timestamps (and, for the archive, by the cross-correlation offset the
    // pipeline measures from the audio itself) — slightly less precise than a live Stop, documented.
    let now = std::time::Instant::now();
    let system_started_at = job.system_wav.as_ref().map(|_| now);

    // Run the meeting through the EXISTING post-Stop pipeline — the SAME path a normal Stop takes, so
    // salvage inherits seal-aware export + the visibility gate with zero forking. `run_after_stop`
    // writes + finalizes the archive WAV BEFORE transcription, so even a pipeline failure (e.g. no
    // whisper model) leaves the salvaged audio ATTACHED and the meeting terminal-`Error` — audio is
    // never lost. The moved `system_wav` is consumed by the pipeline's own ScratchWav (delete-on-drop).
    let result = crate::pipeline::run_after_stop(
        app,
        &state,
        &job.meeting_id,
        samples,
        job.sample_rate,
        duration_s,
        job.system_wav.clone(),
        None, // no AEC helper track is spilled (the live VPIO helper is dormant)
        now,
        system_started_at,
    )
    .await;
    match result {
        Ok(_) => tracing::info!(target: "startup", meeting_id = %job.meeting_id, "salvaged a crashed recording into a note"),
        Err(e) => tracing::warn!(target: "startup", meeting_id = %job.meeting_id, error = %e, "salvage pipeline failed; audio attached, meeting marked Error"),
    }
    cleanup_job_files(&job);
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

/// Remove a job's inflight artifacts after processing. Best-effort + idempotent: the moved system WAV
/// is normally already gone (consumed by the pipeline's ScratchWav); a remove of a missing file is a
/// no-op. Only ever touches the inflight dir — never the `audio/` dir or the meeting's real audio.
fn cleanup_job_files(job: &SalvageJob) {
    let _ = std::fs::remove_file(&job.spill_path);
    let _ = std::fs::remove_file(&job.sidecar_path);
    if let Some(sys) = &job.system_wav {
        let _ = std::fs::remove_file(sys);
    }
}

/// Like [`cleanup_job_files`] but PRESERVES the moved far-side `inflight/<id>.sys.wav`. Used ONLY on
/// the mic-spill error paths in [`salvage_one`]: the mic track is unrecoverable, but the far-side
/// system WAV may still be perfectly readable — and the load-bearing rule is that an error path
/// NEVER deletes a recoverable audio file. Removes the (now-useless) mic spill + sidecar only; the
/// preserved sys.wav survives for manual recovery until it is fully orphaned, then
/// [`reap_orphaned_system_wavs`] GC's it on a later launch (so it can't accumulate).
fn cleanup_job_files_preserving_system(job: &SalvageJob) {
    let _ = std::fs::remove_file(&job.spill_path);
    let _ = std::fs::remove_file(&job.sidecar_path);
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

    // ── PURE: spill format round-trip ────────────────────────────────────────────────────────

    #[test]
    fn f32le_round_trips_byte_identical() {
        let samples = vec![0.0f32, 1.0, -1.0, 0.123_456, -0.987_654, f32::MIN_POSITIVE];
        let bytes = samples_to_f32le(&samples);
        assert_eq!(bytes.len(), samples.len() * 4, "4 bytes per f32");
        // Little-endian, so the first sample (0.0) is four zero bytes.
        assert_eq!(&bytes[0..4], &[0, 0, 0, 0]);
        let back = f32le_to_samples(&bytes);
        assert_eq!(back, samples, "samples must survive the f32le round-trip byte-identically");
    }

    #[test]
    fn f32le_drops_a_truncated_tail_sample() {
        // 2 full samples + 2 stray bytes (a crash truncated mid-sample) ⇒ decode yields exactly 2.
        let mut bytes = samples_to_f32le(&[0.25f32, -0.5]);
        bytes.push(0xAB);
        bytes.push(0xCD);
        let back = f32le_to_samples(&bytes);
        assert_eq!(back, vec![0.25f32, -0.5], "partial trailing bytes must be dropped, not fabricated");
    }

    #[test]
    fn f32le_empty_is_empty() {
        assert!(f32le_to_samples(&[]).is_empty());
        assert!(samples_to_f32le(&[]).is_empty());
    }

    // ── PURE: salvage_plan truth table ───────────────────────────────────────────────────────

    #[test]
    fn salvage_plan_truth_table() {
        assert_eq!(salvage_plan(true, true), SalvagePlan::Salvage, "spill + stuck-recording ⇒ salvage");
        assert_eq!(salvage_plan(true, false), SalvagePlan::DiscardOrphan, "spill but row not recording ⇒ orphan");
        assert_eq!(salvage_plan(false, true), SalvagePlan::NoSpill, "no spill ⇒ nothing to reconstruct");
        assert_eq!(salvage_plan(false, false), SalvagePlan::NoSpill, "no spill ⇒ nothing to reconstruct");
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
    fn claim_salvages_stuck_recording_and_moves_the_far_side_scratch() {
        let dir = tmp_dir("claim-salvage");
        let (db, db_path) = tmp_db();
        let id = "crashed-meeting-1";
        insert_meeting(&db, id, MeetingStatus::Recording); // genuine crash ghost

        // A far-side system scratch living in a separate temp dir (stand-in for $TMPDIR).
        let tmpdir = tmp_dir("claim-tmp");
        let sys_scratch = tmpdir.join(format!("meetnotes-sys-{id}.wav"));
        std::fs::write(&sys_scratch, b"FAKE-SYS-WAV").unwrap();

        // Spill (2 samples) + sidecar naming the scratch.
        std::fs::write(dir.join(format!("{id}.{SPILL_EXT}")), samples_to_f32le(&[0.1f32, 0.2])).unwrap();
        let sidecar = SpillSidecar {
            meeting_id: id.into(),
            sample_rate: 48_000,
            system_scratch_path: Some(sys_scratch.to_string_lossy().into()),
        };
        std::fs::write(dir.join(format!("{id}.{SIDECAR_EXT}")), serde_json::to_vec(&sidecar).unwrap()).unwrap();

        let (jobs, claimed) = claim_inflight_in(&dir, &db);

        assert_eq!(claimed, vec![id.to_string()], "the stuck-recording row is claimed");
        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];
        assert_eq!(job.sample_rate, 48_000);
        // The far-side scratch was MOVED out of $TMPDIR into the inflight dir (reaper can't eat it).
        assert!(!sys_scratch.exists(), "system scratch moved out of $TMPDIR");
        let moved = dir.join(format!("{id}.sys.wav"));
        assert_eq!(job.system_wav.as_deref(), Some(moved.as_path()));
        assert!(moved.exists(), "system scratch now lives in the inflight dir");
        // The spill + sidecar are NOT deleted by the claim — the async worker consumes them.
        assert!(job.spill_path.exists() && job.sidecar_path.exists());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&tmpdir);
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
            serde_json::to_vec(&SpillSidecar { meeting_id: id.into(), sample_rate: 48_000, system_scratch_path: None }).unwrap(),
        )
        .unwrap();

        let (jobs, claimed) = claim_inflight_in(&dir, &db);

        assert!(jobs.is_empty() && claimed.is_empty(), "a non-recording row is not salvaged");
        assert!(!spill.exists() && !sidecar.exists(), "the orphan spill + sidecar are cleaned up");

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
            serde_json::to_vec(&SpillSidecar { meeting_id: id.into(), sample_rate: 48_000, system_scratch_path: None }).unwrap(),
        )
        .unwrap();

        let (jobs, claimed) = claim_inflight_in(&dir, &db);

        assert!(jobs.is_empty() && claimed.is_empty());
        assert!(!sidecar.exists(), "a sidecar with no spill is removed (nothing to reconstruct)");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    // ── MUST-FIX 1: never delete a recoverable far-side track on the mic-error path ────────────

    #[test]
    fn cleanup_preserving_system_keeps_a_readable_far_side_wav() {
        let dir = tmp_dir("preserve-sys");
        let id = "mic-unreadable";
        let spill = dir.join(format!("{id}.{SPILL_EXT}"));
        let sidecar = dir.join(format!("{id}.{SIDECAR_EXT}"));
        let sys = dir.join(format!("{id}.{SYSTEM_SCRATCH_SUFFIX}"));
        std::fs::write(&spill, b"corrupt-mic-spill").unwrap();
        std::fs::write(&sidecar, b"{}").unwrap();
        std::fs::write(&sys, b"FAKE-SYS-WAV").unwrap();

        let job = SalvageJob {
            meeting_id: id.into(),
            spill_path: spill.clone(),
            sidecar_path: sidecar.clone(),
            sample_rate: 48_000,
            system_wav: Some(sys.clone()),
        };
        cleanup_job_files_preserving_system(&job);

        assert!(!spill.exists(), "the unusable mic spill is removed");
        assert!(!sidecar.exists(), "the mic sidecar is removed");
        assert!(
            sys.exists(),
            "the recoverable far-side sys.wav is PRESERVED on the mic-error path (never deleted)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── MUST-FIX 2(a): re-claim prefers the already-moved inflight sys.wav ─────────────────────

    #[test]
    fn resolve_system_wav_prefers_the_already_moved_inflight_copy() {
        let dir = tmp_dir("resolve-prefer");
        let id = "re-salvaged";
        // A prior crash MID-salvage already moved the far-side track here.
        let moved = dir.join(format!("{id}.{SYSTEM_SCRATCH_SUFFIX}"));
        std::fs::write(&moved, b"MOVED-SYS").unwrap();
        // The original $TMPDIR scratch the sidecar still names (stand-in — it may even be gone).
        let tmpdir = tmp_dir("resolve-tmp");
        let orig = tmpdir.join("orig-sys.wav");
        std::fs::write(&orig, b"ORIG-SYS").unwrap();

        let got = resolve_system_wav(&dir, id, Some(orig.to_string_lossy().as_ref()));
        assert_eq!(
            got.as_deref(),
            Some(moved.as_path()),
            "re-claim prefers the already-moved inflight sys.wav over the $TMPDIR path"
        );
        assert!(orig.exists(), "the $TMPDIR scratch is left untouched when a moved copy exists");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn claim_reclaims_a_mid_salvage_crash_preferring_the_moved_far_side() {
        let dir = tmp_dir("claim-reclaim");
        let (db, db_path) = tmp_db();
        let id = "reclaimed-1";
        // Salvage crashed before setting a final status ⇒ the row is still a stuck RECORDING ghost.
        insert_meeting(&db, id, MeetingStatus::Recording);

        // The far-side track was already moved into inflight by the crashed salvage; the ORIGINAL
        // $TMPDIR scratch named by the sidecar is long gone (the reaper ate it).
        let moved = dir.join(format!("{id}.{SYSTEM_SCRATCH_SUFFIX}"));
        std::fs::write(&moved, b"MOVED-SYS").unwrap();
        std::fs::write(
            dir.join(format!("{id}.{SPILL_EXT}")),
            samples_to_f32le(&[0.1f32, 0.2]),
        )
        .unwrap();
        let sidecar = SpillSidecar {
            meeting_id: id.into(),
            sample_rate: 48_000,
            system_scratch_path: Some("/private/var/folders/gone/meetnotes-sys.wav".into()),
        };
        std::fs::write(
            dir.join(format!("{id}.{SIDECAR_EXT}")),
            serde_json::to_vec(&sidecar).unwrap(),
        )
        .unwrap();

        let (jobs, claimed) = claim_inflight_in(&dir, &db);

        assert_eq!(claimed, vec![id.to_string()]);
        assert_eq!(jobs.len(), 1);
        // The far-side track is recovered from the moved inflight copy — NOT lost to the gone $TMPDIR.
        assert_eq!(jobs[0].system_wav.as_deref(), Some(moved.as_path()));
        assert!(moved.exists(), "the moved far-side track survives the re-claim (not reaped)");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&db_path);
    }

    // ── MUST-FIX 2(b): reap ONLY fully-orphaned far-side tracks, never a live job ──────────────

    #[test]
    fn reap_orphaned_system_wavs_reaps_only_fully_orphaned_tracks() {
        let dir = tmp_dir("reap");
        // ORPHAN: spill + sidecar both gone (job done / mic-error-preserved) ⇒ garbage, reap it.
        let orphan = dir.join(format!("orphan.{SYSTEM_SCRATCH_SUFFIX}"));
        std::fs::write(&orphan, b"ORPHAN-SYS").unwrap();
        // LIVE: spill + sidecar still present (crash mid-salvage, re-claimable) ⇒ MUST keep.
        let live = dir.join(format!("live.{SYSTEM_SCRATCH_SUFFIX}"));
        std::fs::write(&live, b"LIVE-SYS").unwrap();
        std::fs::write(dir.join(format!("live.{SPILL_EXT}")), samples_to_f32le(&[0.1f32])).unwrap();
        std::fs::write(dir.join(format!("live.{SIDECAR_EXT}")), b"{}").unwrap();
        // HALF-LIVE: sidecar gone but spill present ⇒ still a live job ⇒ MUST keep.
        let half = dir.join(format!("half.{SYSTEM_SCRATCH_SUFFIX}"));
        std::fs::write(&half, b"HALF-SYS").unwrap();
        std::fs::write(dir.join(format!("half.{SPILL_EXT}")), samples_to_f32le(&[0.2f32])).unwrap();

        reap_orphaned_system_wavs(&dir);

        assert!(!orphan.exists(), "a fully-orphaned far-side scratch is reaped");
        assert!(live.exists(), "a live salvage job's far-side track is never reaped");
        assert!(half.exists(), "a half-live job (spill still present) is never reaped");

        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(disk_salvage_plan(true, false, true), Salvage, "plaintext WAV + open folder ⇒ re-run");
        assert_eq!(disk_salvage_plan(true, true, true), Salvage, "plaintext WAV present ⇒ re-run (the .enc twin is irrelevant)");
        assert_eq!(disk_salvage_plan(false, true, true), DeferSealed, "only the sealed .enc ⇒ never decrypt for salvage");
        assert_eq!(disk_salvage_plan(true, false, false), DeferSealed, "locked folder defers even with a plaintext WAV (fail closed)");
        assert_eq!(disk_salvage_plan(false, true, false), DeferSealed);
        assert_eq!(disk_salvage_plan(false, false, true), NoAudio, "nothing on disk ⇒ reconcile to ERROR as before");
        assert_eq!(disk_salvage_plan(false, false, false), DeferSealed, "locked wins over missing audio (still fail closed)");
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

        assert_eq!(disk_jobs.len(), 1, "the surviving archive is claimed for a from-disk re-run");
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

        assert!(disk_jobs.is_empty() && claimed.is_empty(), "nothing to re-transcribe ⇒ nothing claimed");
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
        assert!(!wav_a.exists(), "no plaintext was materialized for the sealed meeting");
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
