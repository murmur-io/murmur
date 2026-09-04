//! Bounded, stable-handle recording sources and the capture durability coordinator.
//!
//! Long recordings never become one `Vec<f32>`. The realtime recorder retains a fixed ring;
//! this module copies small absolute windows into a create-new raw file, fsyncs, records an exact
//! SQLCipher checkpoint, and only then authorizes ring-prefix reuse.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rubato::{FftFixedIn, Resampler};
use sha2::{Digest, Sha256};

use crate::audio::recorder::{
    CaptureFault, CheckpointWriter, DurableRecorderStopOutcome, Recorder, SampleReader,
};
use crate::error::{AppError, Result};
#[cfg(test)]
use crate::storage::models::{RecordingArtifactAssertion, RecordingArtifactRole};
use crate::storage::models::{
    RecordingCaptureFault, RecordingCheckpointAssertion, RecordingGenerationKey,
    RecordingGenerationSnapshot, RecordingGenerationState, RecordingMicAssertion,
};
use crate::storage::recording_store::{
    RecordingGenerationLease, VerifiedArchiveArtifact, VerifiedEmptyMicArtifact,
    VerifiedMicCheckpoint, VerifiedSystemArtifact,
};
use crate::storage::Db;

// Small enough to bound each non-RT allocation and keep spool latency predictable. Append-only
// callbacks do not invalidate the fixed-end copy; a concurrent base recycle still fails closed.
const COPY_FRAMES: usize = 8 * 1024;
// Commit/fsync at roughly constant wall cadence rather than once per fixed frame count (which
// would scale to ~5.9 fsync+SQLCipher transactions/s at 384 kHz). 64K remains the low-rate floor.
const MIN_CHECKPOINT_BATCH_FRAMES: usize = 64 * 1024;
const CHECKPOINT_POLL: Duration = Duration::from_millis(40);
pub(crate) const RECORDING_LEASE_MS: i64 = 120_000;
const RESAMPLE_INPUT_FRAMES: usize = 1024;

// Darwin's O_NOFOLLOW. Murmur is macOS-first; on other Unix targets the pre/post-open identity
// validation remains active and the flag is omitted rather than guessing another ABI value.
#[cfg(target_os = "macos")]
const NOFOLLOW_FLAG: i32 = 0x0000_0100;
#[cfg(not(target_os = "macos"))]
const NOFOLLOW_FLAG: i32 = 0;

pub(crate) struct VerifiedFile {
    basename: String,
    device: u64,
    inode: u64,
    byte_len: u64,
    sha256: String,
    /// Candidate SHA state produced by hashing only the newly appended stable-fd range on top of
    /// the last DB-certified prefix. Present only for `RawF32LeSink` checkpoint proofs.
    prefix_hasher: Option<Sha256>,
}

impl VerifiedFile {
    pub(crate) fn basename(&self) -> &str {
        &self.basename
    }
    pub(crate) fn device(&self) -> u64 {
        self.device
    }
    pub(crate) fn inode(&self) -> u64 {
        self.inode
    }
    pub(crate) fn byte_len(&self) -> u64 {
        self.byte_len
    }
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }
}

/// Consumed unlink authority for one already-verified, single-link inode in Murmur's private audio
/// directory. The open handle proves after unlink that the exact inode lost its final name.
pub(crate) struct VerifiedDeletion {
    path: PathBuf,
    file: File,
    device: u64,
    inode: u64,
}

impl VerifiedDeletion {
    pub(crate) fn for_file(path: &Path, proof: &VerifiedFile) -> Result<Self> {
        let file = open_existing_nofollow(path)?;
        let metadata = file
            .metadata()
            .map_err(|e| AppError::Audio(format!("stat deletion candidate: {e}")))?;
        if metadata.dev() != proof.device()
            || metadata.ino() != proof.inode()
            || metadata.len() != proof.byte_len()
            || metadata.nlink() != 1
        {
            return Err(AppError::Audio(
                "recording deletion candidate is not the verified single-link inode".into(),
            ));
        }
        Ok(Self {
            path: path.to_path_buf(),
            file,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub(crate) fn remove(self, operation: &str) -> Result<()> {
        let path_metadata = std::fs::symlink_metadata(&self.path)
            .map_err(|e| AppError::Audio(format!("{operation}: {e}")))?;
        if path_metadata.file_type().is_symlink()
            || path_metadata.dev() != self.device
            || path_metadata.ino() != self.inode
            || path_metadata.nlink() != 1
        {
            return Err(AppError::Audio(format!(
                "{operation}: path no longer names the verified single-link inode"
            )));
        }
        std::fs::remove_file(&self.path)
            .map_err(|e| AppError::Audio(format!("{operation}: {e}")))?;
        let after = self
            .file
            .metadata()
            .map_err(|e| AppError::Audio(format!("{operation}: verify unlinked inode: {e}")))?;
        if after.dev() != self.device || after.ino() != self.inode || after.nlink() != 0 {
            return Err(AppError::Audio(format!(
                "{operation}: exact inode did not lose its final link"
            )));
        }
        sync_parent_dir(&self.path, operation)
    }
}

fn basename(path: &Path) -> Result<String> {
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| AppError::Audio("recording artifact has no UTF-8 basename".into()))?;
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(AppError::Audio(
            "invalid recording artifact basename".into(),
        ));
    }
    Ok(name.to_owned())
}

fn open_create_new_nofollow(path: &Path) -> Result<File> {
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(AppError::Audio("recording artifact already exists".into()));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        // The parent recording workspace is verified 0700, but keep every plaintext transient
        // private even if it is later moved or an ancestor's permissions are misconfigured.
        .mode(0o600)
        .custom_flags(NOFOLLOW_FLAG)
        .open(path)
        .map_err(|e| AppError::Audio(format!("create recording artifact: {e}")))?;
    let opened = file
        .metadata()
        .map_err(|e| AppError::Audio(format!("stat new recording artifact: {e}")))?;
    let secured = file
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|e| AppError::Audio(format!("secure new recording artifact: {e}")))
        .and_then(|()| {
            let mode = file
                .metadata()
                .map_err(|e| AppError::Audio(format!("stat secured recording artifact: {e}")))?
                .permissions()
                .mode()
                & 0o777;
            if mode != 0o600 {
                return Err(AppError::Audio(
                    "new recording artifact permissions are not private".into(),
                ));
            }
            Ok(())
        });
    if let Err(error) = secured {
        // The inode has not entered SQLCipher or any caller-owned lifecycle yet. Remove only if the
        // path still names this exact empty create-new file; ambiguity preserves it inside 0700.
        if let Ok(path_meta) = std::fs::symlink_metadata(path) {
            if !path_meta.file_type().is_symlink()
                && path_meta.is_file()
                && path_meta.dev() == opened.dev()
                && path_meta.ino() == opened.ino()
                && path_meta.len() == 0
                && path_meta.nlink() == 1
            {
                let _ = std::fs::remove_file(path);
                let _ = sync_parent_dir(path, "sync insecure-create cleanup");
            }
        }
        return Err(error);
    }
    Ok(file)
}

fn open_existing_nofollow(path: &Path) -> Result<File> {
    let link_meta = std::fs::symlink_metadata(path)
        .map_err(|e| AppError::Audio(format!("inspect recording artifact: {e}")))?;
    if link_meta.file_type().is_symlink() || !link_meta.is_file() {
        return Err(AppError::Audio(
            "recording artifact is not a regular file".into(),
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(NOFOLLOW_FLAG)
        .open(path)
        .map_err(|e| AppError::Audio(format!("open recording artifact: {e}")))?;
    let opened = file
        .metadata()
        .map_err(|e| AppError::Audio(format!("stat recording artifact: {e}")))?;
    if !opened.is_file() || opened.dev() != link_meta.dev() || opened.ino() != link_meta.ino() {
        return Err(AppError::Audio(
            "recording artifact identity changed while opening".into(),
        ));
    }
    Ok(file)
}

fn identity(file: &File) -> Result<(u64, u64, u64)> {
    let meta = file
        .metadata()
        .map_err(|e| AppError::Audio(format!("stat recording artifact: {e}")))?;
    if !meta.is_file() || meta.dev() == 0 || meta.ino() == 0 {
        return Err(AppError::Audio(
            "recording artifact lost regular-file identity".into(),
        ));
    }
    Ok((meta.dev(), meta.ino(), meta.len()))
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let mut out = String::with_capacity(64);
    for byte in digest.as_ref() {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// Mutable create-new f32le sink. Its SHA state covers exactly the appended prefix.
pub(crate) struct RawF32LeSink {
    path: PathBuf,
    file: File,
    expected_device: u64,
    expected_inode: u64,
    frames: u64,
    hasher: Sha256,
    certified_frames: u64,
    certified_hasher: Sha256,
}

impl RawF32LeSink {
    pub(crate) fn create(path: PathBuf) -> Result<Self> {
        let file = open_create_new_nofollow(&path)?;
        let (device, inode, len) = identity(&file)?;
        if len != 0 {
            return Err(AppError::Audio(
                "new recording artifact is not empty".into(),
            ));
        }
        // The SQLCipher PREPARE row may outlive a power loss, so its create-new inode must already
        // have a durable directory entry before the caller can reference it. On either sync error,
        // unlink only after the path still proves it names this exact empty single-link inode.
        let create_durability = file
            .sync_all()
            .map_err(|e| AppError::Audio(format!("sync new recording artifact: {e}")))
            .and_then(|()| sync_parent_dir(&path, "sync new recording artifact directory"));
        if let Err(error) = create_durability {
            if let Ok(metadata) = std::fs::symlink_metadata(&path) {
                if !metadata.file_type().is_symlink()
                    && metadata.is_file()
                    && metadata.dev() == device
                    && metadata.ino() == inode
                    && metadata.len() == 0
                    && metadata.nlink() == 1
                {
                    let _ = std::fs::remove_file(&path);
                    let _ = sync_parent_dir(&path, "sync failed-create cleanup");
                }
            }
            return Err(error);
        }
        Ok(Self {
            path,
            file,
            expected_device: device,
            expected_inode: inode,
            frames: 0,
            hasher: Sha256::new(),
            certified_frames: 0,
            certified_hasher: Sha256::new(),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn frames(&self) -> u64 {
        self.frames
    }
    pub(crate) fn device(&self) -> u64 {
        self.expected_device
    }
    pub(crate) fn inode(&self) -> u64 {
        self.expected_inode
    }

    pub(crate) fn append(&mut self, samples: &[f32]) -> Result<()> {
        let bytes = samples
            .len()
            .checked_mul(4)
            .ok_or_else(|| AppError::Audio("recording checkpoint byte length overflow".into()))?;
        let mut encoded = Vec::with_capacity(bytes.min(COPY_FRAMES * 4));
        for sample in samples {
            encoded.extend_from_slice(&sample.to_le_bytes());
        }
        self.file
            .write_all(&encoded)
            .map_err(|e| AppError::Audio(format!("append mic checkpoint: {e}")))?;
        self.hasher.update(&encoded);
        self.frames = self
            .frames
            .checked_add(samples.len() as u64)
            .ok_or_else(|| AppError::Audio("recording frame count overflow".into()))?;
        Ok(())
    }

    pub(crate) fn sync_data_verified(&self) -> Result<VerifiedFile> {
        self.file
            .sync_data()
            .map_err(|e| AppError::Audio(format!("sync mic checkpoint: {e}")))?;
        self.verify_current()
    }

    pub(crate) fn sync_all_verified(&self) -> Result<VerifiedFile> {
        self.file
            .sync_all()
            .map_err(|e| AppError::Audio(format!("finalize mic artifact: {e}")))?;
        self.verify_current()
    }

    fn verify_current(&self) -> Result<VerifiedFile> {
        let metadata = self
            .file
            .metadata()
            .map_err(|e| AppError::Audio(format!("stat durable recording prefix: {e}")))?;
        let (device, inode, byte_len) = (metadata.dev(), metadata.ino(), metadata.len());
        let expected_len = self
            .frames
            .checked_mul(4)
            .ok_or_else(|| AppError::Audio("recording checkpoint byte length overflow".into()))?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || device != self.expected_device
            || inode != self.expected_inode
            || byte_len != expected_len
        {
            return Err(AppError::Audio(
                "recording artifact identity or length changed".into(),
            ));
        }

        // Never authorize a DB CAS or resident-ring trim from optimistic write accounting. Extend
        // the SHA state of the last DB-certified prefix by re-reading exactly the newly appended
        // stable-fd range. This is O(total recording bytes), not O(n^2) full-prefix hashing.
        if self.certified_frames > self.frames {
            return Err(AppError::Audio(
                "certified recording prefix is ahead of the sink".into(),
            ));
        }
        let mut durable_hasher = self.certified_hasher.clone();
        let mut offset = self.certified_frames.saturating_mul(4);
        let mut buffer = vec![0u8; 256 * 1024];
        while offset < expected_len {
            let take = buffer.len().min((expected_len - offset) as usize);
            let read = self
                .file
                .read_at(&mut buffer[..take], offset)
                .map_err(|e| AppError::Audio(format!("rehash durable recording prefix: {e}")))?;
            if read == 0 {
                return Err(AppError::Audio(
                    "durable recording prefix ended during verification".into(),
                ));
            }
            durable_hasher.update(&buffer[..read]);
            offset += read as u64;
        }
        let durable_digest = hex_digest(durable_hasher.clone().finalize());
        let appended_digest = hex_digest(self.hasher.clone().finalize());
        let after = self
            .file
            .metadata()
            .map_err(|e| AppError::Audio(format!("restat durable recording prefix: {e}")))?;
        if after.dev() != device
            || after.ino() != inode
            || after.len() != expected_len
            || after.nlink() != 1
            || durable_digest != appended_digest
        {
            return Err(AppError::Audio(
                "durable recording prefix changed while being verified".into(),
            ));
        }
        Ok(VerifiedFile {
            basename: basename(&self.path)?,
            device,
            inode,
            byte_len,
            sha256: durable_digest,
            prefix_hasher: Some(durable_hasher),
        })
    }

    fn promote_verified_prefix(&mut self, proof: &VerifiedFile) -> Result<()> {
        if proof.device != self.expected_device
            || proof.inode != self.expected_inode
            || proof.byte_len != self.frames.saturating_mul(4)
        {
            return Err(AppError::Audio(
                "checkpoint proof cannot promote a different recording prefix".into(),
            ));
        }
        let hasher = proof.prefix_hasher.clone().ok_or_else(|| {
            AppError::Audio("checkpoint proof has no incremental SHA state".into())
        })?;
        self.certified_frames = self.frames;
        self.certified_hasher = hasher;
        Ok(())
    }

    /// Roll the optimistic suffix back only after the ledger was re-read and proved to still name
    /// the certified prefix. The stable descriptor and exact inode are re-verified after fsync;
    /// callers must never invoke this for an ambiguous row.
    fn rollback_to_certified(&mut self, frames: u64, hasher: Sha256) -> Result<()> {
        if frames != self.certified_frames {
            return Err(AppError::Audio(
                "recording rollback target is not the DB-certified prefix".into(),
            ));
        }
        let byte_len = frames
            .checked_mul(4)
            .ok_or_else(|| AppError::Audio("recording rollback byte length overflow".into()))?;
        self.file
            .set_len(byte_len)
            .map_err(|e| AppError::Audio(format!("truncate uncertified recording suffix: {e}")))?;
        self.file
            .sync_all()
            .map_err(|e| AppError::Audio(format!("sync recording rollback: {e}")))?;
        let metadata = self
            .file
            .metadata()
            .map_err(|e| AppError::Audio(format!("verify recording rollback: {e}")))?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.dev() != self.expected_device
            || metadata.ino() != self.expected_inode
            || metadata.len() != byte_len
        {
            return Err(AppError::Audio(
                "recording artifact changed identity during authorized rollback".into(),
            ));
        }
        self.frames = frames;
        self.hasher = hasher;
        let proof = self.sync_all_verified()?;
        if proof.sha256 != hex_digest(self.certified_hasher.clone().finalize()) {
            return Err(AppError::Audio(
                "recording rollback no longer matches the certified prefix".into(),
            ));
        }
        Ok(())
    }
}

/// Exact-frame, bounded-read mono source.
pub(crate) trait MonoSource {
    fn sample_rate(&self) -> u32;
    fn frames(&self) -> u64;
    fn read_frames(&mut self, start: u64, max_frames: usize) -> Result<Vec<f32>>;
}

pub(crate) struct RawF32LeSource {
    file: File,
    rate: u32,
    frames: u64,
}

impl RawF32LeSource {
    pub(crate) fn open(path: &Path, rate: u32) -> Result<Self> {
        if rate == 0 {
            return Err(AppError::Audio("source sample rate is zero".into()));
        }
        let file = open_existing_nofollow(path)?;
        let (_, _, len) = identity(&file)?;
        if len % 4 != 0 {
            return Err(AppError::Audio(
                "raw mic artifact has a partial frame".into(),
            ));
        }
        Ok(Self {
            file,
            rate,
            frames: len / 4,
        })
    }
}

impl MonoSource for RawF32LeSource {
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn frames(&self) -> u64 {
        self.frames
    }

    fn read_frames(&mut self, start: u64, max_frames: usize) -> Result<Vec<f32>> {
        if start > self.frames {
            return Err(AppError::Audio("audio window starts past EOF".into()));
        }
        let count = max_frames.min((self.frames - start) as usize);
        let mut bytes = vec![0u8; count.saturating_mul(4)];
        let offset = start
            .checked_mul(4)
            .ok_or_else(|| AppError::Audio("audio offset overflow".into()))?;
        let mut filled = 0usize;
        while filled < bytes.len() {
            let n = self
                .file
                .read_at(&mut bytes[filled..], offset + filled as u64)
                .map_err(|e| AppError::Audio(format!("read raw mic window: {e}")))?;
            if n == 0 {
                return Err(AppError::Audio(
                    "raw mic artifact ended before verified length".into(),
                ));
            }
            filled += n;
        }
        Ok(bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect())
    }
}

#[derive(Clone)]
pub(crate) struct ManualClipSource {
    file: Arc<File>,
    durable_frames: Arc<AtomicU64>,
    spool_finished: Arc<AtomicBool>,
    sample_rate: u32,
    device: u64,
    inode: u64,
}

pub(crate) enum ManualClipRead {
    Pending,
    Ready(Vec<f32>),
}

struct StableRawRangeSource {
    file: Arc<File>,
    sample_rate: u32,
    absolute_start: u64,
    frames: u64,
}

impl MonoSource for StableRawRangeSource {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn frames(&self) -> u64 {
        self.frames
    }

    fn read_frames(&mut self, start: u64, max_frames: usize) -> Result<Vec<f32>> {
        if start > self.frames {
            return Err(AppError::Audio(
                "manual clip starts past its exact range".into(),
            ));
        }
        let count = max_frames.min((self.frames - start) as usize);
        let mut bytes = vec![0u8; count.saturating_mul(4)];
        let absolute_frame = self
            .absolute_start
            .checked_add(start)
            .ok_or_else(|| AppError::Audio("manual clip frame offset overflow".into()))?;
        let offset = absolute_frame
            .checked_mul(4)
            .ok_or_else(|| AppError::Audio("manual clip byte offset overflow".into()))?;
        let mut filled = 0usize;
        while filled < bytes.len() {
            let n = self
                .file
                .read_at(&mut bytes[filled..], offset + filled as u64)
                .map_err(|e| AppError::Audio(format!("read durable manual clip: {e}")))?;
            if n == 0 {
                return Err(AppError::Audio(
                    "durable manual clip ended before its certified range".into(),
                ));
            }
            filled += n;
        }
        Ok(bytes
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect())
    }
}

impl ManualClipSource {
    pub(crate) fn read_16k(&self, start: usize, end: usize) -> Result<ManualClipRead> {
        let start = u64::try_from(start)
            .map_err(|_| AppError::Audio("manual clip start exceeds file offsets".into()))?;
        let end = u64::try_from(end)
            .map_err(|_| AppError::Audio("manual clip end exceeds file offsets".into()))?;
        if end < start {
            return Err(AppError::Audio("manual clip range is reversed".into()));
        }
        if self.durable_frames.load(Ordering::Acquire) < end {
            if self.spool_finished.load(Ordering::Acquire) {
                return Err(AppError::Audio(
                    "mic spool ended before the exact manual-command range became durable".into(),
                ));
            }
            return Ok(ManualClipRead::Pending);
        }
        let before = self
            .file
            .metadata()
            .map_err(|e| AppError::Audio(format!("stat durable manual clip: {e}")))?;
        let required_bytes = end
            .checked_mul(4)
            .ok_or_else(|| AppError::Audio("manual clip length overflow".into()))?;
        if before.dev() != self.device
            || before.ino() != self.inode
            || before.len() < required_bytes
        {
            return Err(AppError::Audio(
                "manual clip handle no longer matches its recording generation".into(),
            ));
        }
        let mut source = StableRawRangeSource {
            file: Arc::clone(&self.file),
            sample_rate: self.sample_rate,
            absolute_start: start,
            frames: end - start,
        };
        let samples = resample_bounded_source_to_16k(&mut source)?;
        let after = self
            .file
            .metadata()
            .map_err(|e| AppError::Audio(format!("restat durable manual clip: {e}")))?;
        if after.dev() != self.device || after.ino() != self.inode || after.len() < required_bytes {
            return Err(AppError::Audio(
                "manual clip generation changed while reading".into(),
            ));
        }
        Ok(ManualClipRead::Ready(samples))
    }
}

fn append_resampled_vec(
    produced: Vec<Vec<f32>>,
    discard_delay: &mut usize,
    output: &mut Vec<f32>,
    expected_out: usize,
) {
    let block = produced.first().map(Vec::as_slice).unwrap_or(&[]);
    let skip = (*discard_delay).min(block.len());
    *discard_delay -= skip;
    let keep = (expected_out - output.len()).min(block.len() - skip);
    output.extend_from_slice(&block[skip..skip + keep]);
}

fn resample_bounded_source_to_16k(source: &mut dyn MonoSource) -> Result<Vec<f32>> {
    let src_rate = source.sample_rate();
    if src_rate == 0 {
        return Err(AppError::Audio("manual clip sample rate is zero".into()));
    }
    let expected_out_u64 = (source.frames() as u128 * crate::audio::TARGET_RATE_HZ as u128)
        .div_ceil(src_rate as u128) as u64;
    let expected_out = usize::try_from(expected_out_u64)
        .map_err(|_| AppError::Audio("manual clip resample length exceeds memory bounds".into()))?;
    let mut output = Vec::with_capacity(expected_out);
    if src_rate == crate::audio::TARGET_RATE_HZ {
        let mut offset = 0u64;
        while offset < source.frames() {
            let block = source.read_frames(offset, COPY_FRAMES)?;
            if block.is_empty() {
                break;
            }
            offset += block.len() as u64;
            output.extend_from_slice(&block);
        }
    } else {
        let mut resampler = FftFixedIn::<f32>::new(
            src_rate as usize,
            crate::audio::TARGET_RATE_HZ as usize,
            RESAMPLE_INPUT_FRAMES,
            1,
            1,
        )
        .map_err(|e| AppError::Audio(format!("build manual clip resampler: {e}")))?;
        let input_frames = resampler.input_frames_next();
        let mut source_offset = 0u64;
        let mut discard_delay = resampler.output_delay();
        while source.frames().saturating_sub(source_offset) >= input_frames as u64 {
            let block = source.read_frames(source_offset, input_frames)?;
            if block.len() != input_frames {
                return Err(AppError::Audio(
                    "manual clip shortened inside a resample block".into(),
                ));
            }
            source_offset += block.len() as u64;
            append_resampled_vec(
                resampler
                    .process(&[block], None)
                    .map_err(|e| AppError::Audio(format!("resample manual clip: {e}")))?,
                &mut discard_delay,
                &mut output,
                expected_out,
            );
        }
        let tail = source.read_frames(source_offset, input_frames)?;
        source_offset += tail.len() as u64;
        if !tail.is_empty() {
            append_resampled_vec(
                resampler
                    .process_partial(Some(&[tail]), None)
                    .map_err(|e| AppError::Audio(format!("resample manual clip tail: {e}")))?,
                &mut discard_delay,
                &mut output,
                expected_out,
            );
        }
        let mut drain_calls = 0usize;
        while output.len() < expected_out {
            let before = output.len();
            append_resampled_vec(
                resampler
                    .process_partial::<Vec<f32>>(None, None)
                    .map_err(|e| AppError::Audio(format!("drain manual clip resampler: {e}")))?,
                &mut discard_delay,
                &mut output,
                expected_out,
            );
            drain_calls += 1;
            if output.len() == before || drain_calls > 8 {
                return Err(AppError::Audio(
                    "manual clip resampler ended before the exact tail".into(),
                ));
            }
        }
        if source_offset != source.frames() {
            return Err(AppError::Audio(
                "manual clip resampler did not consume the exact range".into(),
            ));
        }
    }
    if output.len() != expected_out {
        return Err(AppError::Audio(
            "manual clip resampler produced an unexpected length".into(),
        ));
    }
    Ok(output)
}

pub(crate) struct WavMonoSource {
    reader: hound::WavReader<File>,
    spec: hound::WavSpec,
    frames: u64,
}

impl WavMonoSource {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let file = open_existing_nofollow(path)?;
        let reader = hound::WavReader::new(file)
            .map_err(|e| AppError::Audio(format!("open WAV source: {e}")))?;
        let spec = reader.spec();
        let frames = reader.duration() as u64;
        Ok(Self {
            reader,
            spec,
            frames,
        })
    }
}

impl MonoSource for WavMonoSource {
    fn sample_rate(&self) -> u32 {
        self.spec.sample_rate
    }
    fn frames(&self) -> u64 {
        self.frames
    }

    fn read_frames(&mut self, start: u64, max_frames: usize) -> Result<Vec<f32>> {
        if start > self.frames {
            return Err(AppError::Audio("WAV window starts past EOF".into()));
        }
        let channels = self.spec.channels.max(1) as usize;
        let count = max_frames.min((self.frames - start) as usize);
        self.reader
            .seek(start as u32)
            .map_err(|e| AppError::Audio(format!("seek WAV source: {e}")))?;
        let mut mono = Vec::with_capacity(count);
        match self.spec.sample_format {
            hound::SampleFormat::Float => {
                let mut samples = self.reader.samples::<f32>();
                for _ in 0..count {
                    let mut sum = 0.0;
                    for _ in 0..channels {
                        sum += samples
                            .next()
                            .ok_or_else(|| AppError::Audio("WAV ended inside frame".into()))?
                            .map_err(|e| AppError::Audio(format!("decode WAV sample: {e}")))?;
                    }
                    mono.push(sum / channels as f32);
                }
            }
            hound::SampleFormat::Int => {
                let scale = (1i64 << self.spec.bits_per_sample.saturating_sub(1)) as f32;
                let mut samples = self.reader.samples::<i32>();
                for _ in 0..count {
                    let mut sum = 0.0;
                    for _ in 0..channels {
                        let value = samples
                            .next()
                            .ok_or_else(|| AppError::Audio("WAV ended inside frame".into()))?
                            .map_err(|e| AppError::Audio(format!("decode WAV sample: {e}")))?;
                        sum += value as f32 / scale;
                    }
                    mono.push(sum / channels as f32);
                }
            }
        }
        Ok(mono)
    }
}

/// Persistent fixed-input resampler. Only its fixed input/output blocks are resident.
pub(crate) fn resample_source_to_f32le(
    source: &mut dyn MonoSource,
    output: &Path,
) -> Result<RawF32LeSource> {
    let src_rate = source.sample_rate();
    if src_rate == 0 {
        return Err(AppError::Audio(
            "streaming resampler source rate is zero".into(),
        ));
    }
    let expected_out = (source.frames() as u128 * crate::audio::TARGET_RATE_HZ as u128)
        .div_ceil(src_rate as u128) as u64;
    let mut sink = RawF32LeSink::create(output.to_path_buf())?;
    if src_rate == crate::audio::TARGET_RATE_HZ {
        let mut offset = 0;
        while offset < source.frames() {
            let block = source.read_frames(offset, COPY_FRAMES)?;
            if block.is_empty() {
                break;
            }
            sink.append(&block)?;
            offset += block.len() as u64;
        }
    } else {
        let mut resampler = FftFixedIn::<f32>::new(
            src_rate as usize,
            crate::audio::TARGET_RATE_HZ as usize,
            RESAMPLE_INPUT_FRAMES,
            1,
            1,
        )
        .map_err(|e| AppError::Audio(format!("build streaming resampler: {e}")))?;
        let input_frames = resampler.input_frames_next();
        let mut source_offset = 0u64;
        let mut written = 0u64;
        let mut discard_delay = resampler.output_delay();

        while source.frames().saturating_sub(source_offset) >= input_frames as u64 {
            let block = source.read_frames(source_offset, input_frames)?;
            if block.len() != input_frames {
                return Err(AppError::Audio(
                    "streaming resampler source shortened inside a full block".into(),
                ));
            }
            source_offset += block.len() as u64;
            append_resampled_output(
                &mut sink,
                resampler
                    .process(&[block], None)
                    .map_err(|e| AppError::Audio(format!("streaming resample: {e}")))?,
                &mut discard_delay,
                &mut written,
                expected_out,
            )?;
        }

        let tail = source.read_frames(source_offset, input_frames)?;
        source_offset += tail.len() as u64;
        if !tail.is_empty() {
            append_resampled_output(
                &mut sink,
                resampler
                    .process_partial(Some(&[tail]), None)
                    .map_err(|e| AppError::Audio(format!("streaming resample tail: {e}")))?,
                &mut discard_delay,
                &mut written,
                expected_out,
            )?;
        }
        // Flush the FFT overlap exactly until the rational-duration output is complete. Additional
        // zero-padded calls are bounded by the fixed filter delay; output is capped at expected_out.
        let mut drain_calls = 0usize;
        while written < expected_out {
            let before = written;
            append_resampled_output(
                &mut sink,
                resampler
                    .process_partial::<Vec<f32>>(None, None)
                    .map_err(|e| AppError::Audio(format!("streaming resample drain: {e}")))?,
                &mut discard_delay,
                &mut written,
                expected_out,
            )?;
            drain_calls += 1;
            if written == before || drain_calls > 8 {
                return Err(AppError::Audio(
                    "streaming resampler ended before the exact rational tail".into(),
                ));
            }
        }
        if source_offset != source.frames() {
            return Err(AppError::Audio(
                "streaming resampler did not consume the source exactly once".into(),
            ));
        }
    }
    let verified = sink.sync_all_verified()?;
    if verified.byte_len() != expected_out.saturating_mul(4) {
        return Err(AppError::Audio(
            "streaming resampler produced an unexpected tail length".into(),
        ));
    }
    RawF32LeSource::open(output, crate::audio::TARGET_RATE_HZ)
}

fn append_resampled_output(
    sink: &mut RawF32LeSink,
    produced: Vec<Vec<f32>>,
    discard_delay: &mut usize,
    written: &mut u64,
    expected_out: u64,
) -> Result<()> {
    let output_block = produced.first().map(Vec::as_slice).unwrap_or(&[]);
    let skip = (*discard_delay).min(output_block.len());
    *discard_delay -= skip;
    let available = &output_block[skip..];
    let remaining = expected_out.saturating_sub(*written) as usize;
    let keep = remaining.min(available.len());
    sink.append(&available[..keep])?;
    *written += keep as u64;
    Ok(())
}

pub(crate) struct SpoolDone {
    sink: RawF32LeSink,
    lease: RecordingGenerationLease,
    durable_frames: u64,
    fault: Option<RecordingCaptureFault>,
}

enum SpoolControl {
    Finish,
}

/// Sole owner of the mic file, ledger lease and destructive recorder checkpoint authority.
pub(crate) struct CaptureSpool {
    control: Sender<SpoolControl>,
    done: Receiver<SpoolDone>,
    thread: Option<JoinHandle<()>>,
    finishing: bool,
    manual_reader: Arc<File>,
    durable_frames: Arc<AtomicU64>,
    finished: Arc<AtomicBool>,
}

impl CaptureSpool {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start(
        db: Arc<Db>,
        key: RecordingGenerationKey,
        lease: RecordingGenerationLease,
        mic: RecordingMicAssertion,
        reader: SampleReader,
        checkpoint_writer: CheckpointWriter,
        sink: RawF32LeSink,
    ) -> Result<Self> {
        let manual_reader = match sink.file.try_clone() {
            Ok(file) => Arc::new(file),
            Err(error) => {
                abandon_verified_empty_prepared(&db, &key, &lease, &mic, sink)?;
                return Err(AppError::Audio(format!(
                    "clone stable mic handle for manual clips: {error}"
                )));
            }
        };
        let durable_frames = Arc::new(AtomicU64::new(0));
        let worker_durable_frames = Arc::clone(&durable_frames);
        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = Arc::clone(&finished);
        let (control_tx, control_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let (init_tx, init_rx) = mpsc::channel();
        let thread = match std::thread::Builder::new()
            .name("murmur-mic-spool".into())
            .spawn(move || {
                struct FinishedFlag(Arc<AtomicBool>);
                impl Drop for FinishedFlag {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::Release);
                    }
                }
                let _finished = FinishedFlag(worker_finished);
                if let Ok((db, key, lease, mic, reader, checkpoint_writer, sink, control_rx)) =
                    init_rx.recv()
                {
                    let result = spool_loop(SpoolLoopArgs {
                        db,
                        key,
                        lease,
                        mic,
                        reader,
                        checkpoint_writer,
                        sink,
                        control: control_rx,
                        durable_frames: worker_durable_frames,
                    });
                    let _ = done_tx.send(result);
                }
            }) {
            Ok(thread) => thread,
            Err(error) => {
                abandon_verified_empty_prepared(&db, &key, &lease, &mic, sink)?;
                return Err(AppError::Audio(format!("spawn mic spool: {error}")));
            }
        };

        // PREPARED stays rollback-safe until the worker definitely exists. The transition to
        // CAPTURING still precedes activation. If it fails, retire the verified-empty attempt and
        // only then unlink it; if handoff fails after CAPTURING, preserve the generation for stale
        // recovery rather than deleting an artifact that may be the sole source.
        if let Err(error) = db.begin_recording_capture(&key, &lease) {
            let cleanup = abandon_verified_empty_prepared(&db, &key, &lease, &mic, sink);
            drop(init_tx);
            let _ = thread.join();
            cleanup?;
            return Err(error);
        }
        if init_tx
            .send((
                db,
                key,
                lease,
                mic,
                reader,
                checkpoint_writer,
                sink,
                control_rx,
            ))
            .is_err()
        {
            let _ = thread.join();
            return Err(AppError::Audio(
                "mic spool exited before accepting the capturing generation".into(),
            ));
        }
        Ok(Self {
            control: control_tx,
            done: done_rx,
            thread: Some(thread),
            finishing: false,
            manual_reader,
            durable_frames,
            finished,
        })
    }

    pub(crate) fn manual_clip_source(&self, mic: &RecordingMicAssertion) -> ManualClipSource {
        ManualClipSource {
            file: Arc::clone(&self.manual_reader),
            durable_frames: Arc::clone(&self.durable_frames),
            spool_finished: Arc::clone(&self.finished),
            sample_rate: mic.sample_rate(),
            device: mic.device(),
            inode: mic.inode(),
        }
    }

    pub(crate) fn try_finish(&mut self) -> Result<Option<SpoolDone>> {
        if !self.finishing {
            let _ = self.control.send(SpoolControl::Finish);
            self.finishing = true;
        }
        match self.done.recv_timeout(Duration::from_secs(5)) {
            Ok(result) => {
                if let Some(thread) = self.thread.take() {
                    if thread.join().is_err() {
                        return Err(AppError::Audio("mic spool thread panicked".into()));
                    }
                }
                Ok(Some(result))
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                Err(AppError::Audio("mic spool exited without result".into()))
            }
        }
    }
}

fn abandon_verified_empty_prepared(
    db: &Db,
    key: &RecordingGenerationKey,
    lease: &RecordingGenerationLease,
    mic: &RecordingMicAssertion,
    sink: RawF32LeSink,
) -> Result<()> {
    let path = sink.path().to_path_buf();
    let verified = sink.sync_all_verified()?;
    let empty = VerifiedEmptyMicArtifact::from_file(key, mic, &verified)?;
    let deletion = VerifiedDeletion::for_file(&path, &verified)?;
    db.abandon_empty_prepared_recording_generation(key, lease, &empty)?;
    drop(sink);
    deletion.remove("remove retired empty mic artifact")
}

/// Cleanup for a create-new mic inode when SQLCipher PREPARE itself failed. No ledger row can
/// reference it; still require same-handle empty verification before unlinking.
pub(crate) fn discard_untracked_empty_mic(sink: RawF32LeSink) -> Result<()> {
    let path = sink.path().to_path_buf();
    let verified = sink.sync_all_verified()?;
    if verified.byte_len() != 0
        || verified.sha256() != "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    {
        return Err(AppError::Audio(
            "refusing to discard an untracked mic artifact that is not verified empty".into(),
        ));
    }
    let deletion = VerifiedDeletion::for_file(&path, &verified)?;
    drop(sink);
    deletion.remove("remove untracked empty mic artifact")
}

impl Drop for CaptureSpool {
    fn drop(&mut self) {
        let _ = self.control.send(SpoolControl::Finish);
    }
}

struct SpoolLoopArgs {
    db: Arc<Db>,
    key: RecordingGenerationKey,
    lease: RecordingGenerationLease,
    mic: RecordingMicAssertion,
    reader: SampleReader,
    checkpoint_writer: CheckpointWriter,
    sink: RawF32LeSink,
    control: Receiver<SpoolControl>,
    durable_frames: Arc<AtomicU64>,
}

fn spool_loop(args: SpoolLoopArgs) -> SpoolDone {
    let SpoolLoopArgs {
        db,
        key,
        lease,
        mic,
        reader,
        mut checkpoint_writer,
        mut sink,
        control,
        durable_frames,
    } = args;
    let mut expected = match RecordingCheckpointAssertion::new(
        0,
        0,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    ) {
        Ok(value) => value,
        Err(_) => {
            return SpoolDone {
                sink,
                lease,
                durable_frames: 0,
                fault: Some(RecordingCaptureFault::MicIo),
            }
        }
    };
    let mut finishing = false;
    let mut last_renew = Instant::now();
    let mut terminal_fault = None;
    let checkpoint_batch_frames = MIN_CHECKPOINT_BATCH_FRAMES.max(mic.sample_rate() as usize);
    loop {
        // Never insert CHECKPOINT_POLL between queued chunks. Poll non-blockingly while behind and
        // wait only when fully caught up; otherwise the 40 ms control cadence itself caps source
        // throughput below high-rate devices and makes Stop drain proportional to chunk count.
        if !finishing {
            match control.try_recv() {
                Ok(SpoolControl::Finish) | Err(TryRecvError::Disconnected) => finishing = true,
                Err(TryRecvError::Empty) => {}
            }
        }
        let (_, end) = checkpoint_writer.resident_bounds();
        let start = sink.frames() as usize;
        if end == start {
            if finishing {
                break;
            }
            match control.recv_timeout(CHECKPOINT_POLL) {
                Ok(SpoolControl::Finish) | Err(RecvTimeoutError::Disconnected) => finishing = true,
                Err(RecvTimeoutError::Timeout) => {}
            }
            if last_renew.elapsed() >= Duration::from_secs(30) {
                if db
                    .renew_recording_generation_lease(
                        &key,
                        &lease,
                        RecordingGenerationState::Capturing,
                        RECORDING_LEASE_MS,
                    )
                    .is_err()
                {
                    terminal_fault = Some(RecordingCaptureFault::Interrupted);
                    break;
                }
                last_renew = Instant::now();
            }
            continue;
        }
        if end > start {
            let batch_end = start.saturating_add(checkpoint_batch_frames).min(end);
            let prior_frames = sink.frames;
            let prior_hasher = sink.hasher.clone();
            let mut rollback_authorized = false;
            let checkpoint_result = (|| -> Result<()> {
                let mut cursor = start;
                while cursor < batch_end {
                    let samples =
                        reader.read_absolute(cursor, COPY_FRAMES.min(batch_end - cursor))?;
                    if samples.is_empty() {
                        return Err(AppError::Audio(
                            "mic spool observed an unexplained empty window".into(),
                        ));
                    }
                    cursor = cursor.checked_add(samples.len()).ok_or_else(|| {
                        AppError::Audio("mic spool source offset overflow".into())
                    })?;
                    sink.append(&samples)?;
                }
                let verified_file = sink.sync_data_verified()?;
                let verified =
                    VerifiedMicCheckpoint::from_file(&key, &mic, &verified_file, sink.frames())?;
                let next = RecordingCheckpointAssertion::new(
                    sink.frames(),
                    sink.frames().saturating_mul(4),
                    verified_file.sha256(),
                )?;
                if let Err(commit_error) =
                    db.checkpoint_recording_generation(&key, &lease, &expected, &verified)
                {
                    // SQLite can report an error after the commit became durable. Never infer
                    // rollback from the returned error. Re-read the canonical row: truncation is
                    // authorized only when the full checkpoint assertion is still exactly the old
                    // value. Any other result keeps the just-fsynced prefix and stops fail-closed.
                    match db.get_recording_generation_snapshot(&key) {
                        Ok(Some(row))
                            if row.state == RecordingGenerationState::Capturing
                                && row.checkpoint == expected =>
                        {
                            rollback_authorized = true;
                            return Err(commit_error);
                        }
                        Ok(Some(row))
                            if row.state == RecordingGenerationState::Capturing
                                && row.checkpoint == next =>
                        {
                            // The transaction committed even though SQLite returned an error.
                            // Promote the candidate SHA state and trim exactly as on the ordinary
                            // success path; treating this as failure would leak resident RAM.
                            sink.promote_verified_prefix(&verified_file)?;
                            expected = next;
                            durable_frames.store(sink.frames(), Ordering::Release);
                            checkpoint_writer.checkpoint_trim_verified(&verified)?;
                            return Ok(());
                        }
                        _ => {}
                    }
                    return Err(AppError::Storage(
                        "recording checkpoint commit was ambiguous; durable prefix preserved"
                            .into(),
                    ));
                }
                sink.promote_verified_prefix(&verified_file)?;
                expected = next;
                durable_frames.store(sink.frames(), Ordering::Release);
                // Ledger is now authoritative for the prefix. A trim failure must preserve that
                // prefix and stop capture, never roll the already-committed CAS back.
                checkpoint_writer.checkpoint_trim_verified(&verified)?;
                Ok(())
            })();
            if checkpoint_result.is_err() {
                if rollback_authorized {
                    let _ = sink.rollback_to_certified(prior_frames, prior_hasher);
                }
                terminal_fault = Some(RecordingCaptureFault::MicIo);
                break;
            }
        }
        if last_renew.elapsed() >= Duration::from_secs(30) {
            if db
                .renew_recording_generation_lease(
                    &key,
                    &lease,
                    RecordingGenerationState::Capturing,
                    RECORDING_LEASE_MS,
                )
                .is_err()
            {
                terminal_fault = Some(RecordingCaptureFault::Interrupted);
                break;
            }
            last_renew = Instant::now();
        }
        let (_, observed_end) = checkpoint_writer.resident_bounds();
        if finishing && sink.frames() as usize == observed_end {
            break;
        }
    }
    if sink.sync_all_verified().is_err() {
        terminal_fault = Some(RecordingCaptureFault::DiskFull);
    }
    drop(checkpoint_writer); // latches recorder stop when capture is still active
    SpoolDone {
        sink,
        lease,
        durable_frames: expected.durable_frames(),
        fault: terminal_fault,
    }
}

pub(crate) struct FinalizedRecording {
    pub(crate) key: RecordingGenerationKey,
    pub(crate) lease: RecordingGenerationLease,
    pub(crate) mic_path: PathBuf,
    pub(crate) sample_rate: u32,
    pub(crate) frames: u64,
    pub(crate) started_at: Instant,
    pub(crate) capture_fault: Option<RecordingCaptureFault>,
    pub(crate) system_wav: Option<PathBuf>,
    pub(crate) system_started_at: Option<Instant>,
}

impl FinalizedRecording {
    /// Relinquish a post-Stop pipeline lease without deleting any artifact. The generation keeps
    /// its exact FINALIZED/ARCHIVED proofs and becomes immediately claimable by recovery.
    pub(crate) fn release_for_recovery(self, db: &Db) -> Result<()> {
        let state = db
            .get_recording_generation_snapshot(&self.key)?
            .ok_or_else(|| AppError::Storage("pipeline lost its recording generation row".into()))?
            .state;
        if state == RecordingGenerationState::Retired {
            return Ok(());
        }
        db.release_recording_generation_lease(&self.key, self.lease, state)
    }
}

pub(crate) struct RecoveredRecordingJob {
    pub(crate) meeting_id: String,
    pub(crate) recording: FinalizedRecording,
}

/// One state slot owns every live capture component.
pub(crate) struct ActiveRecording {
    pub(crate) meeting_id: String,
    pub(crate) recorder: Recorder,
    pub(crate) spool: Option<CaptureSpool>,
    spool_done: Option<SpoolDone>,
    pub(crate) key: RecordingGenerationKey,
    pub(crate) mic: RecordingMicAssertion,
    pub(crate) system: Option<crate::audio::system::SystemAudioRecorder>,
    system_stop: Option<crate::audio::system::SystemAudioStopOutcome>,
    system_stop_handled: bool,
    system_capture_fault: bool,
    stopped_system: Option<(PathBuf, Instant)>,
    finalized: Option<FinalizedRecording>,
    model_session: Option<crate::perf::RecordingSessionOwner>,
    manual_clip_source: ManualClipSource,
}

impl std::ops::Deref for ActiveRecording {
    type Target = Recorder;

    fn deref(&self) -> &Self::Target {
        &self.recorder
    }
}

impl ActiveRecording {
    pub(crate) fn new(
        meeting_id: String,
        recorder: Recorder,
        spool: CaptureSpool,
        key: RecordingGenerationKey,
        mic: RecordingMicAssertion,
        system: Option<crate::audio::system::SystemAudioRecorder>,
        model_session: crate::perf::RecordingSessionOwner,
    ) -> Self {
        let manual_clip_source = spool.manual_clip_source(&mic);
        Self {
            meeting_id,
            recorder,
            spool: Some(spool),
            spool_done: None,
            key,
            mic,
            system,
            system_stop: None,
            system_stop_handled: false,
            system_capture_fault: false,
            stopped_system: None,
            finalized: None,
            model_session: Some(model_session),
            manual_clip_source,
        }
    }

    pub(crate) fn manual_clip_source(&self) -> ManualClipSource {
        self.manual_clip_source.clone()
    }

    /// Borrow an opaque token only while this exact capture owns the coordinator's Live phase.
    /// The affine phase owner never leaves `ActiveRecording` until capture finalization succeeds.
    pub(crate) fn live_model_token(&self) -> Result<crate::perf::RecordingSessionToken> {
        self.model_session
            .as_ref()
            .ok_or_else(|| AppError::Unavailable("recording model session owner is absent".into()))?
            .token()
            .validated_for_live_work()
    }

    /// Close Live admission while the recorder slot is still locked. Stop calls this immediately
    /// before taking the sole `ActiveRecording`, making phase transition + ownership transfer one
    /// atomic state operation.
    pub(crate) fn transition_model_to_draining(&mut self) -> Result<()> {
        self.model_session
            .as_mut()
            .ok_or_else(|| AppError::Unavailable("recording model session owner is absent".into()))?
            .transition_to_draining()
    }

    fn finish_with_model_owner(
        &mut self,
        recording: FinalizedRecording,
    ) -> Result<(FinalizedRecording, crate::perf::RecordingSessionOwner)> {
        let owner = self.model_session.take().ok_or_else(|| {
            AppError::Unavailable("recording model session owner was already consumed".into())
        })?;
        Ok((recording, owner))
    }

    pub(crate) fn try_finish(
        &mut self,
        db: &Db,
    ) -> Result<Option<(FinalizedRecording, crate::perf::RecordingSessionOwner)>> {
        if let Some(recording) = self.finalized.take() {
            let verified_system = recording
                .system_wav
                .as_deref()
                .map(|path| VerifiedSystemArtifact::from_path(&recording.key, path))
                .transpose()?;
            match db.finalize_recording_generation(
                &recording.key,
                &recording.lease,
                verified_system.as_ref(),
                recording.capture_fault,
            ) {
                Ok(()) => return self.finish_with_model_owner(recording).map(Some),
                Err(error) => {
                    let already_finalized = db
                        .get_recording_generation_snapshot(&recording.key)
                        .ok()
                        .flatten()
                        .map(|row| row.state == RecordingGenerationState::Finalized)
                        .unwrap_or(false);
                    if already_finalized {
                        return self.finish_with_model_owner(recording).map(Some);
                    }
                    self.finalized = Some(recording);
                    return Err(error);
                }
            }
        }
        let started_at = self.recorder.started_at();
        let mut capture_fault = self.recorder.fault().map(map_capture_fault);
        match self.recorder.try_stop_for_durable_assembly() {
            DurableRecorderStopOutcome::Pending => return Ok(None),
            DurableRecorderStopOutcome::Stopped { fault } => {
                if let Some(fault) = fault {
                    capture_fault = Some(map_capture_fault(fault));
                }
            }
        }
        if self.system_stop.is_none() && !self.system_stop_handled {
            self.system_stop = self.system.take().map(|system| system.stop());
            if self.system_stop.is_none() {
                self.system_stop_handled = true;
            }
        }
        if !self.system_stop_handled {
            let outcome = self.system_stop.as_ref().ok_or_else(|| {
                AppError::Audio("system capture teardown ownership was lost".into())
            })?;
            let (adopted, faulted) = adopt_system_stop(&self.key, outcome)?;
            self.stopped_system = adopted;
            self.system_capture_fault = faulted;
            self.system_stop_handled = true;
        }
        if self.spool_done.is_none() {
            let done = match self
                .spool
                .as_mut()
                .ok_or_else(|| AppError::Audio("mic spool ownership lost".into()))?
                .try_finish()?
            {
                Some(done) => done,
                None => return Ok(None),
            };
            self.spool = None;
            self.spool_done = Some(done);
        }
        let done = self
            .spool_done
            .as_ref()
            .ok_or_else(|| AppError::Audio("mic spool result lost".into()))?;
        if let Some((_, system_started_at)) = self.stopped_system.as_ref() {
            db.set_recording_system_start_offset(
                &self.key,
                &done.lease.heartbeat(),
                signed_instant_offset_micros(started_at, *system_started_at),
            )?;
        }
        let final_verified = done.sink.sync_all_verified()?;
        let final_mic = VerifiedMicCheckpoint::from_file(
            &self.key,
            &self.mic,
            &final_verified,
            done.durable_frames,
        )?;
        let verified_system = match self.stopped_system.as_ref().map(|(path, _)| path.as_path()) {
            Some(path) => Some(VerifiedSystemArtifact::from_path(&self.key, path)?),
            None => None,
        };
        let fault = capture_fault.or(done.fault).or(self
            .system_capture_fault
            .then_some(RecordingCaptureFault::SystemIo));
        let done = self
            .spool_done
            .take()
            .ok_or_else(|| AppError::Audio("mic spool result lost".into()))?;
        let finalized = FinalizedRecording {
            key: self.key.clone(),
            lease: done.lease,
            mic_path: done.sink.path().to_path_buf(),
            sample_rate: self.mic.sample_rate(),
            frames: done.durable_frames,
            started_at,
            capture_fault: fault,
            system_wav: self.stopped_system.as_ref().map(|(path, _)| path.clone()),
            system_started_at: self.stopped_system.as_ref().map(|(_, started)| *started),
        };
        if let Err(error) = db.finalize_recording_generation(
            &finalized.key,
            &finalized.lease,
            verified_system.as_ref(),
            fault,
        ) {
            self.finalized = Some(finalized);
            return Err(error);
        }
        // The final checkpoint capability was reconstructed after sync_all; retain the call so a
        // mismatched length/hash cannot be silently finalized even though the ledger already holds
        // the same exact prefix from the last checkpoint.
        if final_mic.durable_frames() != done.durable_frames {
            return Err(AppError::Audio(
                "final mic proof does not match durable prefix".into(),
            ));
        }
        self.finish_with_model_owner(finalized).map(Some)
    }

    /// Consume a terminally failed Stop owner without unlinking any artifact. This runs only on a
    /// detached blocking recovery worker: retain cpal/helper/spool/model ownership until every
    /// producer is proven settled, then release the exact ledger lease. It is intentionally
    /// unbounded off the caller path; a permanently wedged native producer must fail closed and
    /// keep future recording admission blocked rather than overlap two captures.
    pub(crate) fn release_for_recovery(mut self, db: &Db) -> Result<()> {
        while let DurableRecorderStopOutcome::Pending =
            self.recorder.try_stop_for_durable_assembly()
        {
            std::thread::sleep(Duration::from_millis(25));
        }
        if let Some(system) = self.system.take() {
            self.system_stop = Some(system.stop());
        }
        if self.spool_done.is_none() {
            let mut spool = self.spool.take().ok_or_else(|| {
                AppError::Audio("failed Stop lost its mic spool ownership".into())
            })?;
            while self.spool_done.is_none() {
                self.spool_done = spool.try_finish()?;
            }
        }
        let lease = if let Some(finalized) = self.finalized.take() {
            Some(finalized.lease)
        } else {
            self.spool_done.take().map(|done| done.lease)
        };
        let Some(lease) = lease else {
            return Err(AppError::Audio(
                "failed Stop preserved artifacts but could not recover its live ledger lease"
                    .into(),
            ));
        };
        let state = db
            .get_recording_generation_snapshot(&self.key)?
            .ok_or_else(|| {
                AppError::Storage("failed Stop lost its recording generation row".into())
            })?
            .state;
        db.release_recording_generation_lease(&self.key, lease, state)
    }
}

fn adopt_system_stop(
    key: &RecordingGenerationKey,
    outcome: &crate::audio::system::SystemAudioStopOutcome,
) -> Result<(Option<(PathBuf, Instant)>, bool)> {
    let path = outcome.path();
    let exists = match std::fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(AppError::Audio(format!(
                "inspect stopped system artifact: {error}"
            )))
        }
    };
    if !exists {
        return Ok((None, true));
    }

    if !outcome.helper_finalized() {
        // Pre-ready `_exit(3)`, parent-loss hard bound `_exit(6)`, forced SIGKILL, and
        // unknown/unreaped status provide no proof that AVAudioFile closed and published a valid
        // header/tail. A partially parseable file is not enough: remove only through the exact
        // stable-file proof, otherwise preserve the capturing row/path for recovery instead of
        // adopting corrupt far-side audio.
        let verified = verify_existing_file(path)?;
        VerifiedDeletion::for_file(path, &verified)?
            .remove("remove unfinalized stopped system artifact")?;
        return Ok((None, true));
    }

    let usable = WavMonoSource::open(path)
        .map(|source| source.frames() > 0 && source.sample_rate() > 0)
        .unwrap_or(false);
    if usable {
        // Canonical basename, stable identity and full-file digest are checked before the path can
        // enter FINALIZED. Exit 5 proves the ready-phase finalizer ran despite a latched capture
        // fault; keep that verified partial and persist SYSTEM_IO rather than orphan useful audio.
        let _ = VerifiedSystemArtifact::from_path(key, path)?;
        return Ok((
            Some((path.to_path_buf(), outcome.started_at())),
            !outcome.helper_succeeded(),
        ));
    }

    // An unusable partial is removed only through an exact stable-file proof. Failure leaves the
    // CAPTURING row and path intact for recovery; it never becomes an untracked orphan.
    let verified = verify_existing_file(path)?;
    VerifiedDeletion::for_file(path, &verified)?
        .remove("remove unusable stopped system artifact")?;
    Ok((None, true))
}

/// Stop-time cleanup for a helper that could not acquire a durable alignment witness during
/// Start. The path is still private and has never entered the ledger as a system artifact, so it
/// must be removed through an exact stable-file proof before Start may degrade to mic-only.
pub(crate) fn discard_unaligned_system_stop(
    outcome: &crate::audio::system::SystemAudioStopOutcome,
) -> Result<()> {
    let path = outcome.path();
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::Audio(format!(
            "inspect unaligned system artifact: {error}"
        ))),
        Ok(_) => {
            let verified = verify_existing_file(path)?;
            VerifiedDeletion::for_file(path, &verified)?.remove("remove unaligned system artifact")
        }
    }
}

fn map_capture_fault(fault: CaptureFault) -> RecordingCaptureFault {
    match fault {
        CaptureFault::StreamError | CaptureFault::CaptureThreadFailed => {
            RecordingCaptureFault::DeviceLost
        }
        CaptureFault::ResidentCapacityExhausted
        | CaptureFault::BufferLockContended
        | CaptureFault::InvalidInterleavedInput
        | CaptureFault::FrameCounterOverflow
        | CaptureFault::CheckpointAuthorityLost => RecordingCaptureFault::MicIo,
    }
}

fn signed_instant_offset_micros(mic: Instant, system: Instant) -> i64 {
    if system >= mic {
        i64::try_from(system.duration_since(mic).as_micros()).unwrap_or(i64::MAX)
    } else {
        -i64::try_from(mic.duration_since(system).as_micros()).unwrap_or(i64::MAX)
    }
}

/// Hash and reopen an archive from the same stable handle before exposing it to SQLCipher state.
pub(crate) fn verify_existing_file(path: &Path) -> Result<VerifiedFile> {
    verify_existing_file_with_nlink(path, 1)
}

/// Read a small control artifact from the same no-follow handle used to establish its identity.
/// The length gate runs before allocation/hash, so a forged multi-gigabyte sidecar cannot turn
/// startup recovery into an unbounded I/O or RAM operation.
pub(crate) fn read_existing_file_bounded(
    path: &Path,
    max_bytes: u64,
) -> Result<(Vec<u8>, VerifiedFile)> {
    let mut file = open_existing_nofollow(path)?;
    let before = file
        .metadata()
        .map_err(|e| AppError::Audio(format!("stat bounded artifact before read: {e}")))?;
    if !before.is_file() || before.nlink() != 1 || before.len() > max_bytes {
        return Err(AppError::Audio(
            "bounded artifact is not an owned single-link file within its size limit".into(),
        ));
    }
    let capacity = usize::try_from(before.len())
        .map_err(|_| AppError::Audio("bounded artifact length does not fit memory".into()))?;
    let mut bytes = Vec::with_capacity(capacity);
    std::io::Read::take(&mut file, max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| AppError::Audio(format!("read bounded artifact: {e}")))?;
    if bytes.len() as u64 > max_bytes || bytes.len() as u64 != before.len() {
        return Err(AppError::Audio(
            "bounded artifact changed length while being read".into(),
        ));
    }
    let after = file
        .metadata()
        .map_err(|e| AppError::Audio(format!("stat bounded artifact after read: {e}")))?;
    if after.dev() != before.dev()
        || after.ino() != before.ino()
        || after.len() != before.len()
        || after.nlink() != 1
    {
        return Err(AppError::Audio(
            "bounded artifact identity changed while being read".into(),
        ));
    }
    let sha256 = hex_digest(Sha256::digest(&bytes));
    Ok((
        bytes,
        VerifiedFile {
            basename: basename(path)?,
            device: after.dev(),
            inode: after.ino(),
            byte_len: after.len(),
            sha256,
            prefix_hasher: None,
        },
    ))
}

fn verify_existing_file_with_nlink(path: &Path, expected_nlink: u64) -> Result<VerifiedFile> {
    let mut file = open_existing_nofollow(path)?;
    let before = file
        .metadata()
        .map_err(|e| AppError::Audio(format!("stat artifact before hash: {e}")))?;
    if before.nlink() != expected_nlink {
        return Err(AppError::Audio(
            "recording artifact has an unexpected owned-path count".into(),
        ));
    }
    let (device, inode, byte_len) = identity(&file)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| AppError::Audio(format!("seek artifact for hash: {e}")))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|e| AppError::Audio(format!("hash artifact: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let (device_after, inode_after, len_after) = identity(&file)?;
    let after = file
        .metadata()
        .map_err(|e| AppError::Audio(format!("stat artifact after hash: {e}")))?;
    if (device, inode, byte_len) != (device_after, inode_after, len_after)
        || after.nlink() != expected_nlink
    {
        return Err(AppError::Audio("artifact changed while verifying".into()));
    }
    Ok(VerifiedFile {
        basename: basename(path)?,
        device,
        inode,
        byte_len,
        sha256: hex_digest(hasher.finalize()),
        prefix_hasher: None,
    })
}

/// Claim expired durable generations before legacy spill reconciliation. The loop is deliberately
/// bounded so malformed state can never stall launch. A verification failure preserves both row
/// and artifact, marks the meeting Error, and stops this pass rather than repeatedly reclaiming the
/// same ambiguous row.
pub(crate) fn claim_stale_recording_generations(
    db: &Db,
    inflight_dir: &Path,
    archive_dir: &Path,
) -> (Vec<RecoveredRecordingJob>, Vec<String>) {
    claim_stale_recording_generations_with_pre_recovery_hook(
        db,
        inflight_dir,
        archive_dir,
        |_, _| {},
    )
}

fn claim_stale_recording_generations_with_pre_recovery_hook(
    db: &Db,
    inflight_dir: &Path,
    archive_dir: &Path,
    mut before_recovery_gate: impl FnMut(&Db, &str),
) -> (Vec<RecoveredRecordingJob>, Vec<String>) {
    // One physical recorder exists, so normal operation can strand at most one newest generation.
    // Claiming exactly one also keeps its affine lease fresh until the pipeline archives it; bulk
    // pre-claim would let later jobs expire while the first job is in ASR.
    const MAX_RECOVERY_JOBS: usize = 1;
    let mut jobs = Vec::new();
    let mut claimed = Vec::new();
    for _ in 0..MAX_RECOVERY_JOBS {
        let claim = match db.claim_oldest_stale_recording_generation(RECORDING_LEASE_MS) {
            Ok(Some(claim)) => claim,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(target: "startup", error = %error, "recording ledger claim failed; preserving all artifacts");
                break;
            }
        };
        let (snapshot, lease) = claim.into_parts();
        let meeting_id = snapshot.key.meeting_id().to_owned();
        let generation_key = snapshot.key.clone();
        let generation_state = snapshot.state;
        before_recovery_gate(db, &meeting_id);

        // The transactional claim excludes owners in at-rest locked folders. Repeat the gate
        // immediately before the first artifact open so a lock-state change at the claim boundary
        // fails closed. Refusal relinquishes only the affine lease; every proof and byte remains
        // retryable after an explicit unlock.
        match db.recording_recovery_owner_is_open(&meeting_id) {
            Ok(true) => {}
            Ok(false) => {
                if let Err(error) =
                    db.release_recording_generation_lease(&generation_key, lease, generation_state)
                {
                    tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %error, "locked recording recovery could not release its lease; artifacts preserved");
                }
                tracing::info!(target: "startup", meeting_id = %meeting_id, "stale recording recovery deferred by folder lock; artifacts preserved");
                break;
            }
            Err(gate_error) => {
                if let Err(error) =
                    db.release_recording_generation_lease(&generation_key, lease, generation_state)
                {
                    tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %error, "recording recovery with unreadable lock state could not release its lease; artifacts preserved");
                }
                tracing::warn!(target: "startup", meeting_id = %meeting_id, error = %gate_error, "stale recording recovery lock state unreadable; artifacts preserved");
                break;
            }
        }
        let recovery_owner = lease.heartbeat();
        match recover_claimed_generation(db, inflight_dir, archive_dir, snapshot, lease) {
            Ok(Some(recording)) => {
                claimed.push(meeting_id.clone());
                jobs.push(RecoveredRecordingJob {
                    meeting_id,
                    recording,
                });
            }
            Ok(None) => {}
            Err(error) => {
                if let Err(quarantine_error) =
                    db.quarantine_ambiguous_recording_generation(&generation_key, &recovery_owner)
                {
                    tracing::warn!(
                        target: "startup",
                        meeting_id = %meeting_id,
                        error = %quarantine_error,
                        "ambiguous recording generation could not be quarantined from later startup passes"
                    );
                }
                let _ = db.update_meeting_status(
                    &meeting_id,
                    crate::storage::models::MeetingStatus::Error,
                );
                tracing::warn!(
                    target: "startup",
                    meeting_id = %meeting_id,
                    error = %error,
                    "stale recording generation was ambiguous; preserving row and artifact"
                );
                break;
            }
        }
    }
    (jobs, claimed)
}

/// Same-process interactive recovery for an expired ARCHIVED generation. It resumes the exact
/// cleanup mask and retires only after every proof-bound unlink is durable. On any failure the
/// newly-claimed lease is released again, so the next Retry/Delete/Lock attempt can retry without
/// restarting Murmur. Non-ARCHIVED generations are released untouched for the full pipeline
/// recovery path; the caller's final nonterminal reread will continue to refuse the mutation.
pub(crate) fn resume_released_generation_cleanup_for_meeting(
    db: &Db,
    inflight_dir: &Path,
    archive_dir: &Path,
    meeting_id: &str,
) -> Result<()> {
    let Some(claim) =
        db.claim_stale_recording_generation_for_meeting(meeting_id, RECORDING_LEASE_MS)?
    else {
        return Ok(());
    };
    let (snapshot, lease) = claim.into_parts();
    if snapshot.state != RecordingGenerationState::Archived {
        return db.release_recording_generation_lease(&snapshot.key, lease, snapshot.state);
    }
    let attempt = (|| -> Result<()> {
        let assertion = snapshot.archive.as_ref().ok_or_else(|| {
            AppError::Storage("archived recovery generation lost its archive proof".into())
        })?;
        let verified = verify_existing_file(&archive_dir.join(assertion.basename()))?;
        if verified.device() != assertion.device()
            || verified.inode() != assertion.inode()
            || verified.byte_len() != assertion.byte_len()
            || verified.sha256() != assertion.sha256()
        {
            return Err(AppError::Storage(
                "interactive recovery archive does not match its ledger proof".into(),
            ));
        }
        let archive = VerifiedArchiveArtifact::from_file(&snapshot.key, &verified)?;
        cleanup_completed_archived_generation(db, inflight_dir, &snapshot, &lease, &archive)
    })();
    match attempt {
        Ok(()) => Ok(()),
        Err(error) => {
            let release = db.release_recording_generation_lease(
                &snapshot.key,
                lease,
                RecordingGenerationState::Archived,
            );
            if let Err(release_error) = release {
                tracing::warn!(target: "audio", error = %release_error, "interactive cleanup could not release its retry lease");
            }
            Err(error)
        }
    }
}

fn recover_claimed_generation(
    db: &Db,
    inflight_dir: &Path,
    archive_dir: &Path,
    snapshot: RecordingGenerationSnapshot,
    lease: RecordingGenerationLease,
) -> Result<Option<FinalizedRecording>> {
    let mic_path = inflight_dir.join(snapshot.mic.basename());
    match snapshot.state {
        RecordingGenerationState::Prepared => {
            if std::fs::symlink_metadata(&mic_path)
                .map_err(|e| AppError::Audio(format!("inspect prepared recovery artifact: {e}")))?
                .len()
                != 0
            {
                return Err(AppError::Audio(
                    "prepared recovery artifact is not empty; preserving it as ambiguous".into(),
                ));
            }
            let file = verify_existing_file(&mic_path)?;
            let empty = VerifiedEmptyMicArtifact::from_file(&snapshot.key, &snapshot.mic, &file)?;
            let deletion = VerifiedDeletion::for_file(&mic_path, &file)?;
            db.abandon_empty_prepared_recording_generation(&snapshot.key, &lease, &empty)?;
            deletion.remove("remove recovered empty mic artifact")?;
            Ok(None)
        }
        RecordingGenerationState::Capturing | RecordingGenerationState::Finalized => {
            let verified = recover_exact_checkpoint(&mic_path, &snapshot)?;
            let proof = VerifiedMicCheckpoint::from_file(
                &snapshot.key,
                &snapshot.mic,
                &verified,
                snapshot.checkpoint.durable_frames(),
            )?;
            if proof.durable_frames() != snapshot.checkpoint.durable_frames() {
                return Err(AppError::Audio(
                    "recovered mic proof changed durable length".into(),
                ));
            }
            let recovered_mic_started_at = Instant::now();
            let (system_wav, system_started_at, system_fault) =
                recover_system_path(inflight_dir, &snapshot, recovered_mic_started_at)?;
            let fault = snapshot.capture_fault.or(Some(if system_fault {
                RecordingCaptureFault::SystemIo
            } else {
                RecordingCaptureFault::Interrupted
            }));
            if snapshot.state == RecordingGenerationState::Capturing {
                let verified_system = system_wav
                    .as_deref()
                    .map(|path| VerifiedSystemArtifact::from_path(&snapshot.key, path))
                    .transpose()?;
                db.finalize_recording_generation(
                    &snapshot.key,
                    &lease,
                    verified_system.as_ref(),
                    fault,
                )?;
            }
            Ok(Some(FinalizedRecording {
                key: snapshot.key,
                lease,
                mic_path,
                sample_rate: snapshot.mic.sample_rate(),
                frames: snapshot.checkpoint.durable_frames(),
                started_at: recovered_mic_started_at,
                capture_fault: fault,
                system_wav,
                system_started_at,
            }))
        }
        RecordingGenerationState::Archived => {
            let archive_assertion = snapshot.archive.as_ref().ok_or_else(|| {
                AppError::Audio("archived recording generation has no archive assertion".into())
            })?;
            let archive_path = archive_dir.join(archive_assertion.basename());
            let verified = verify_existing_file(&archive_path)?;
            if verified.device() != archive_assertion.device()
                || verified.inode() != archive_assertion.inode()
                || verified.byte_len() != archive_assertion.byte_len()
                || verified.sha256() != archive_assertion.sha256()
            {
                return Err(AppError::Audio(
                    "archived recovery artifact does not match its ledger proof".into(),
                ));
            }
            let archive = VerifiedArchiveArtifact::from_file(&snapshot.key, &verified)?;
            if db.meeting_postprocess_is_durable(snapshot.key.meeting_id())? {
                cleanup_completed_archived_generation(
                    db,
                    inflight_dir,
                    &snapshot,
                    &lease,
                    &archive,
                )?;
                return Ok(None);
            }
            let _verified_mic = recover_exact_checkpoint(&mic_path, &snapshot)?;
            let recovered_mic_started_at = Instant::now();
            let (system_wav, system_started_at, system_fault) =
                recover_system_path(inflight_dir, &snapshot, recovered_mic_started_at)?;
            Ok(Some(FinalizedRecording {
                key: snapshot.key,
                lease,
                mic_path,
                sample_rate: snapshot.mic.sample_rate(),
                frames: snapshot.checkpoint.durable_frames(),
                started_at: recovered_mic_started_at,
                capture_fault: snapshot
                    .capture_fault
                    .or(system_fault.then_some(RecordingCaptureFault::SystemIo)),
                system_wav,
                system_started_at,
            }))
        }
        RecordingGenerationState::Retired => Err(AppError::Storage(
            "retired generation was incorrectly returned by stale claim".into(),
        )),
    }
}

pub(crate) fn cleanup_completed_archived_generation(
    db: &Db,
    inflight_dir: &Path,
    snapshot: &RecordingGenerationSnapshot,
    lease: &RecordingGenerationLease,
    archive: &VerifiedArchiveArtifact,
) -> Result<()> {
    use crate::storage::recording_store::{
        CLEANUP_MIC_16K, CLEANUP_MIC_RAW, CLEANUP_PARTS, CLEANUP_SYSTEM_16K, CLEANUP_SYSTEM_RAW,
    };

    let mut cleanup_mask = snapshot.cleanup_mask;
    let mic_path = inflight_dir.join(snapshot.mic.basename());
    advance_cleanup_path(
        db,
        &snapshot.key,
        lease,
        &mut cleanup_mask,
        CLEANUP_MIC_RAW,
        &mic_path,
        |proof| {
            if proof.device() != snapshot.mic.device()
                || proof.inode() != snapshot.mic.inode()
                || proof.byte_len() != snapshot.checkpoint.byte_len()
                || proof.sha256() != snapshot.checkpoint.sha256_prefix()
            {
                return Err(AppError::Audio(
                    "cleanup-pending mic does not match its ledger proof".into(),
                ));
            }
            Ok(())
        },
    )?;
    if let Some(assertion) = snapshot.system_artifact.as_ref() {
        let system_path = inflight_dir.join(assertion.basename());
        advance_cleanup_path(
            db,
            &snapshot.key,
            lease,
            &mut cleanup_mask,
            CLEANUP_SYSTEM_RAW,
            &system_path,
            |proof| {
                if proof.device() != assertion.device()
                    || proof.inode() != assertion.inode()
                    || proof.byte_len() != assertion.byte_len()
                    || proof.sha256() != assertion.sha256()
                {
                    return Err(AppError::Audio(
                        "cleanup-pending system track does not match its ledger proof".into(),
                    ));
                }
                Ok(())
            },
        )?;
    }
    let mic_16k = inflight_dir.join(format!("{}.mic16.f32", snapshot.key.generation_id()));
    advance_cleanup_path(
        db,
        &snapshot.key,
        lease,
        &mut cleanup_mask,
        CLEANUP_MIC_16K,
        &mic_16k,
        |_| Ok(()),
    )?;
    if snapshot.system_artifact.is_some() {
        let system_16k =
            inflight_dir.join(format!("{}.system16.f32", snapshot.key.generation_id()));
        advance_cleanup_path(
            db,
            &snapshot.key,
            lease,
            &mut cleanup_mask,
            CLEANUP_SYSTEM_16K,
            &system_16k,
            |_| Ok(()),
        )?;
    }
    let parts = tracked_generation_part_deletions(inflight_dir, snapshot.key.generation_id())?;
    if cleanup_mask & CLEANUP_PARTS != 0 {
        if !parts.is_empty() {
            return Err(AppError::Storage(
                "recording cleanup ledger says staging parts are absent but names remain".into(),
            ));
        }
    } else {
        for deletion in parts {
            deletion.remove("remove tracked generation staging part")?;
        }
        if !tracked_generation_part_deletions(inflight_dir, snapshot.key.generation_id())?
            .is_empty()
        {
            return Err(AppError::Audio(
                "generation staging parts reappeared during cleanup".into(),
            ));
        }
        cleanup_mask =
            db.checkpoint_recording_cleanup(&snapshot.key, lease, cleanup_mask, CLEANUP_PARTS)?;
    }
    let required = if snapshot.system_artifact.is_some() {
        31
    } else {
        21
    };
    if cleanup_mask != required {
        return Err(AppError::Storage(
            "recording cleanup did not reach its required durable mask".into(),
        ));
    }
    db.retire_recording_generation(&snapshot.key, lease, archive)
}

fn advance_cleanup_path<F>(
    db: &Db,
    key: &RecordingGenerationKey,
    lease: &RecordingGenerationLease,
    cleanup_mask: &mut u8,
    bit: u8,
    path: &Path,
    validate: F,
) -> Result<()>
where
    F: FnOnce(&VerifiedFile) -> Result<()>,
{
    let present = artifact_path_present(path)?;
    if *cleanup_mask & bit != 0 {
        if present {
            return Err(AppError::Storage(
                "recording cleanup ledger says an artifact is absent but its name remains".into(),
            ));
        }
        return Ok(());
    }
    if present {
        let proof = verify_existing_file(path)?;
        validate(&proof)?;
        VerifiedDeletion::for_file(path, &proof)?
            .remove("remove cleanup-pending recording artifact")?;
    }
    // Missing with a clear bit is the crash window after exact unlink+directory fsync but before
    // its DB checkpoint. The canonical archive + durable summarized note are already proven, so
    // advancing the cleanup-only witness is safe and makes recovery idempotent.
    *cleanup_mask = db.checkpoint_recording_cleanup(key, lease, *cleanup_mask, bit)?;
    Ok(())
}

fn artifact_path_present(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(AppError::Audio(format!(
            "inspect cleanup-pending recording artifact: {error}"
        ))),
    }
}

pub(crate) fn tracked_generation_part_deletions(
    inflight_dir: &Path,
    generation_id: &str,
) -> Result<Vec<VerifiedDeletion>> {
    let archive_prefix = format!(".{generation_id}.archive-");
    let system_master_prefix = format!(".{generation_id}.system-master-");
    let mic_master_prefix = format!(".{generation_id}.mic-master-");
    let mut cleanup = Vec::new();
    for entry in std::fs::read_dir(inflight_dir)
        .map_err(|e| AppError::Audio(format!("inspect generation staging directory: {e}")))?
    {
        let entry =
            entry.map_err(|e| AppError::Audio(format!("inspect generation staging entry: {e}")))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if (name.starts_with(&archive_prefix)
            || name.starts_with(&system_master_prefix)
            || name.starts_with(&mic_master_prefix))
            && name.ends_with(".part")
        {
            let path = entry.path();
            let proof = verify_existing_file(&path)?;
            cleanup.push(VerifiedDeletion::for_file(&path, &proof)?);
        }
    }
    Ok(cleanup)
}

fn recover_exact_checkpoint(
    path: &Path,
    snapshot: &RecordingGenerationSnapshot,
) -> Result<VerifiedFile> {
    let link_meta = std::fs::symlink_metadata(path)
        .map_err(|e| AppError::Audio(format!("inspect recovery mic artifact: {e}")))?;
    if link_meta.file_type().is_symlink() || !link_meta.is_file() || link_meta.nlink() != 1 {
        return Err(AppError::Audio(
            "recovery mic artifact is not a regular file".into(),
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(NOFOLLOW_FLAG)
        .open(path)
        .map_err(|e| AppError::Audio(format!("open recovery mic artifact: {e}")))?;
    let (device, inode, actual_len) = identity(&file)?;
    let durable_len = snapshot.checkpoint.byte_len();
    if device != snapshot.mic.device()
        || inode != snapshot.mic.inode()
        || device != link_meta.dev()
        || inode != link_meta.ino()
        || actual_len < durable_len
        || file
            .metadata()
            .map_err(|e| AppError::Audio(format!("stat recovery mic links: {e}")))?
            .nlink()
            != 1
    {
        return Err(AppError::Audio(
            "recovery mic identity or committed length is ambiguous".into(),
        ));
    }
    let mut hasher = Sha256::new();
    let mut offset = 0u64;
    let mut buffer = vec![0u8; 256 * 1024];
    while offset < durable_len {
        let take = buffer.len().min((durable_len - offset) as usize);
        let read = file
            .read_at(&mut buffer[..take], offset)
            .map_err(|e| AppError::Audio(format!("hash recovery mic prefix: {e}")))?;
        if read == 0 {
            return Err(AppError::Audio(
                "recovery mic ended before committed prefix".into(),
            ));
        }
        hasher.update(&buffer[..read]);
        offset += read as u64;
    }
    let digest = hex_digest(hasher.finalize());
    if digest != snapshot.checkpoint.sha256_prefix() {
        return Err(AppError::Audio(
            "recovery mic prefix hash does not match ledger".into(),
        ));
    }
    // Destructive tail removal is allowed only after the committed prefix has been proven.
    file.set_len(durable_len)
        .map_err(|e| AppError::Audio(format!("truncate recovery mic to committed prefix: {e}")))?;
    file.sync_all()
        .map_err(|e| AppError::Audio(format!("sync recovered mic prefix: {e}")))?;
    let (device_after, inode_after, len_after) = identity(&file)?;
    if (device_after, inode_after, len_after) != (device, inode, durable_len)
        || file
            .metadata()
            .map_err(|e| AppError::Audio(format!("restat recovered mic links: {e}")))?
            .nlink()
            != 1
    {
        return Err(AppError::Audio(
            "recovery mic changed identity while truncating".into(),
        ));
    }
    Ok(VerifiedFile {
        basename: basename(path)?,
        device,
        inode,
        byte_len: durable_len,
        sha256: digest,
        prefix_hasher: None,
    })
}

fn recover_system_path(
    dir: &Path,
    snapshot: &RecordingGenerationSnapshot,
    mic_started_at: Instant,
) -> Result<(Option<PathBuf>, Option<Instant>, bool)> {
    let system_started_at = snapshot
        .system_start_offset_micros
        .map(|offset| instant_from_signed_offset(mic_started_at, offset));
    if let Some(assertion) = snapshot.system_artifact.as_ref() {
        let path = dir.join(assertion.basename());
        let file = verify_existing_file(&path)?;
        if file.device() != assertion.device()
            || file.inode() != assertion.inode()
            || file.byte_len() != assertion.byte_len()
            || file.sha256() != assertion.sha256()
        {
            return Err(AppError::Audio(
                "recovery system artifact does not match ledger".into(),
            ));
        }
        return Ok((Some(path), system_started_at, false));
    }

    if snapshot.state != RecordingGenerationState::Capturing {
        return Ok((None, None, false));
    }
    let path = dir.join(format!("{}.system.wav", snapshot.key.generation_id()));
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((None, None, snapshot.system_start_offset_micros.is_some()));
        }
        Err(error) => {
            return Err(AppError::Audio(format!(
                "inspect canonical recovery system artifact: {error}"
            )))
        }
        Ok(_) => {}
    }
    let usable = WavMonoSource::open(&path)
        .map(|source| source.frames() > 0 && source.sample_rate() > 0)
        .unwrap_or(false);
    if usable {
        let _ = VerifiedSystemArtifact::from_path(&snapshot.key, &path)?;
        return Ok((Some(path), system_started_at, true));
    }
    let verified = verify_existing_file(&path)?;
    VerifiedDeletion::for_file(&path, &verified)?
        .remove("remove unusable recovered system artifact")?;
    Ok((None, None, true))
}

fn instant_from_signed_offset(base: Instant, offset_micros: i64) -> Instant {
    if offset_micros >= 0 {
        base.checked_add(Duration::from_micros(offset_micros as u64))
            .unwrap_or(base)
    } else {
        base.checked_sub(Duration::from_micros(offset_micros.unsigned_abs()))
            .unwrap_or(base)
    }
}

fn sync_parent_dir(path: &Path, operation: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        let dir = File::open(parent).map_err(|e| AppError::Audio(format!("{operation}: {e}")))?;
        dir.sync_all()
            .map_err(|e| AppError::Audio(format!("{operation}: {e}")))?;
    }
    Ok(())
}

/// Create the canonical archive through a same-directory temporary file, then publish with a
/// no-clobber hard-link. The two passes keep only one bounded mix window resident.
pub(crate) struct AtomicAudioPublisher {
    temp: PathBuf,
    final_path: PathBuf,
}

impl AtomicAudioPublisher {
    pub(crate) fn new(
        staging_dir: &Path,
        final_dir: &Path,
        key: &RecordingGenerationKey,
    ) -> Result<Self> {
        let final_path = final_dir.join(format!("{}.archive.wav", key.generation_id()));
        let staging_prefix = format!(".{}.archive-", key.generation_id());
        for entry in std::fs::read_dir(staging_dir)
            .map_err(|e| AppError::Audio(format!("inspect archive staging directory: {e}")))?
        {
            let entry = entry
                .map_err(|e| AppError::Audio(format!("inspect archive staging entry: {e}")))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with(&staging_prefix) && name.ends_with(".part") {
                let stale = entry.path();
                if !reconcile_stale_publication(&stale, &final_path, "archive")? {
                    let proof = verify_existing_file(&stale)?;
                    VerifiedDeletion::for_file(&stale, &proof)?
                        .remove("remove stale tracked archive staging file")?;
                }
            }
        }
        let temp = staging_dir.join(format!(
            ".{}.archive-{}.part",
            key.generation_id(),
            uuid::Uuid::new_v4()
        ));
        Ok(Self { temp, final_path })
    }

    pub(crate) fn publish_mix(
        self,
        mic_path: &Path,
        system_path: Option<&Path>,
        mic_delay: u64,
        system_delay: u64,
    ) -> Result<(PathBuf, VerifiedFile)> {
        let mut mic = RawF32LeSource::open(mic_path, crate::audio::TARGET_RATE_HZ)?;
        let mut system = system_path
            .map(|path| RawF32LeSource::open(path, crate::audio::TARGET_RATE_HZ))
            .transpose()?;
        let total = (mic_delay + mic.frames()).max(
            system
                .as_ref()
                .map(|s| system_delay + s.frames())
                .unwrap_or(0),
        );

        let mut peak = 0.0f32;
        let mut offset = 0u64;
        while offset < total {
            let count = COPY_FRAMES.min((total - offset) as usize);
            let mixed = mixed_window(
                &mut mic,
                mic_delay,
                system.as_mut(),
                system_delay,
                offset,
                count,
            )?;
            peak = mixed
                .into_iter()
                .fold(peak, |value, sample| value.max(sample.abs()));
            offset += count as u64;
        }
        let gain = if peak > 1.0 { 1.0 / peak } else { 1.0 };

        let file = open_create_new_nofollow(&self.temp)?;
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: crate::audio::TARGET_RATE_HZ,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::new(file, spec)
            .map_err(|e| AppError::Audio(format!("create archive WAV: {e}")))?;
        let mut offset = 0u64;
        while offset < total {
            let count = COPY_FRAMES.min((total - offset) as usize);
            let mixed = mixed_window(
                &mut mic,
                mic_delay,
                system.as_mut(),
                system_delay,
                offset,
                count,
            )?;
            for sample in mixed {
                writer
                    .write_sample(
                        (sample.mul_add(gain, 0.0).clamp(-1.0, 1.0) * i16::MAX as f32).round()
                            as i16,
                    )
                    .map_err(|e| AppError::Audio(format!("write archive sample: {e}")))?;
            }
            offset += count as u64;
        }
        writer
            .finalize()
            .map_err(|e| AppError::Audio(format!("finalize archive WAV: {e}")))?;
        let temp_handle = open_existing_nofollow(&self.temp)?;
        temp_handle
            .sync_all()
            .map_err(|e| AppError::Audio(format!("sync archive WAV: {e}")))?;
        let candidate = verify_existing_file(&self.temp)?;

        match std::fs::hard_link(&self.temp, &self.final_path) {
            Ok(()) => {
                // Prove both names refer to the candidate inode before removing the private staging
                // name. During this tiny publication window nlink=2 is expected; afterwards the
                // canonical archive must again be single-link and fully hash-verified.
                let published = open_existing_nofollow(&self.final_path)?;
                let published_meta = published
                    .metadata()
                    .map_err(|e| AppError::Audio(format!("stat published archive: {e}")))?;
                let staging_meta = temp_handle
                    .metadata()
                    .map_err(|e| AppError::Audio(format!("restat archive staging inode: {e}")))?;
                if published_meta.dev() != candidate.device()
                    || published_meta.ino() != candidate.inode()
                    || staging_meta.dev() != candidate.device()
                    || staging_meta.ino() != candidate.inode()
                    || published_meta.nlink() != 2
                    || staging_meta.nlink() != 2
                {
                    return Err(AppError::Audio(
                        "published archive does not name the verified staging inode".into(),
                    ));
                }
                let staging_path_meta = std::fs::symlink_metadata(&self.temp)
                    .map_err(|e| AppError::Audio(format!("inspect archive staging link: {e}")))?;
                if staging_path_meta.dev() != candidate.device()
                    || staging_path_meta.ino() != candidate.inode()
                {
                    return Err(AppError::Audio(
                        "archive staging pathname changed before unlink".into(),
                    ));
                }
                std::fs::remove_file(&self.temp)
                    .map_err(|e| AppError::Audio(format!("remove archive staging link: {e}")))?;
                sync_parent_dir(&self.temp, "sync private archive staging directory")?;
                sync_parent_dir(&self.final_path, "sync archive directory")?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Crash after hard-link publication but before the ledger CAS: adopt only an exact
                // byte-identical canonical archive. Anything else is ambiguous and both files stay.
                let existing = verify_existing_file(&self.final_path)?;
                if existing.byte_len() != candidate.byte_len()
                    || existing.sha256() != candidate.sha256()
                {
                    return Err(AppError::Audio(
                        "existing canonical archive conflicts with the rebuilt candidate".into(),
                    ));
                }
                VerifiedDeletion::for_file(&self.temp, &candidate)?
                    .remove("remove duplicate rebuilt archive candidate")?;
                return Ok((self.final_path, existing));
            }
            Err(error) => {
                return Err(AppError::Audio(format!(
                    "publish archive without clobber: {error}"
                )))
            }
        }
        let verified = verify_existing_file(&self.final_path)?;
        if verified.byte_len() != candidate.byte_len() || verified.sha256() != candidate.sha256() {
            return Err(AppError::Audio(
                "published archive changed after staging unlink".into(),
            ));
        }
        Ok((self.final_path, verified))
    }
}

/// Publish ONE capture stream as a delay-aligned 16 kHz mono master WAV next to the archive.
///
/// WHY THIS EXISTS (T31). The canonical archive is `channels: 1` — [`AtomicAudioPublisher::publish_mix`]
/// SUMS the microphone and the system capture into a single channel, so the `Me`/`Others` split is
/// destroyed at publication and no amount of later processing recovers it. A retry that re-runs from
/// the archive is therefore single-stream BY CONSTRUCTION. Keeping each stream as its own master is
/// the only way a retry can reproduce what the live recording produced.
///
/// The master is written on the ARCHIVE's timeline: `delay` frames of leading silence are prepended,
/// exactly the offset `publish_mix` applied when mixing this stream. Both masters then share one
/// origin, so a retry hands [`crate::audio::merge::merge_streams`] two streams with the same
/// `started_at` and the merge reduces to sort-and-label. It also means a human who plays a master
/// hears it in sync with the archive.
///
/// At-rest handling is NOT new machinery: the meeting's `mic_master_path` / `sys_master_path`
/// columns are already sealed, blanked, unsealed, crash-reconciled and prune-accounted (see
/// `storage::seal_store` and `storage::usage`). This function only produces the file those columns
/// have always been able to describe.
///
/// Atomic like the archive: written to a same-directory dotfile, fsynced, then `rename`d into place,
/// so a crash can never leave a half-written master that a later read would trust.
pub(crate) fn publish_stream_master(
    source_path: &Path,
    final_path: &Path,
    delay_frames: u64,
) -> Result<VerifiedFile> {
    let mut source = RawF32LeSource::open(source_path, crate::audio::TARGET_RATE_HZ)?;
    let total = delay_frames.saturating_add(source.frames());
    if total == 0 {
        return Err(AppError::Audio(
            "stream master would be empty; nothing to publish".into(),
        ));
    }
    let parent = final_path
        .parent()
        .ok_or_else(|| AppError::Audio("stream master path has no parent dir".into()))?;
    let stem = final_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| AppError::Audio("stream master path has no file name".into()))?;
    let temp = parent.join(format!(".{stem}.{}.part", std::process::id()));
    let _ = std::fs::remove_file(&temp);

    {
        let file = open_create_new_nofollow(&temp)?;
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: crate::audio::TARGET_RATE_HZ,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::new(file, spec)
            .map_err(|e| AppError::Audio(format!("create stream master WAV: {e}")))?;
        let mut offset = 0u64;
        while offset < total {
            let count = COPY_FRAMES.min((total - offset) as usize);
            // Leading silence covers the alignment delay; past it, read the stream itself.
            let window = if offset + count as u64 <= delay_frames {
                vec![0.0f32; count]
            } else if offset >= delay_frames {
                source.read_frames(offset - delay_frames, count)?
            } else {
                let silent = (delay_frames - offset) as usize;
                let mut w = vec![0.0f32; silent];
                w.extend(source.read_frames(0, count - silent)?);
                w
            };
            for sample in window {
                writer
                    .write_sample((sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
                    .map_err(|e| AppError::Audio(format!("write stream master sample: {e}")))?;
            }
            offset += count as u64;
        }
        writer
            .finalize()
            .map_err(|e| AppError::Audio(format!("finalize stream master WAV: {e}")))?;
    }
    let staged = verify_existing_file(&temp)?;
    // Prove the master is a readable WAV BEFORE it is published under a name the DB will point at.
    // A column that names an unreadable file is worse than no column: every later read trusts it.
    let readable = WavMonoSource::open(&temp)
        .map(|s| s.frames() > 0 && s.sample_rate() > 0)
        .unwrap_or(false);
    if !readable {
        VerifiedDeletion::for_file(&temp, &staged)?
            .remove("remove unreadable stream master candidate")?;
        return Err(AppError::Audio(
            "published stream master is not a readable WAV".into(),
        ));
    }
    std::fs::rename(&temp, final_path)
        .map_err(|e| AppError::Audio(format!("publish stream master: {e}")))?;
    sync_parent_dir(final_path, "sync stream master directory")?;
    verify_existing_file(final_path)
}

fn mixed_window(
    mic: &mut RawF32LeSource,
    mic_delay: u64,
    mut system: Option<&mut RawF32LeSource>,
    system_delay: u64,
    output_start: u64,
    count: usize,
) -> Result<Vec<f32>> {
    let mut output = vec![0.0f32; count];
    let mut contributors = vec![0u8; count];
    add_aligned(mic, mic_delay, output_start, &mut output, &mut contributors)?;
    if let Some(source) = system.as_mut() {
        add_aligned(
            source,
            system_delay,
            output_start,
            &mut output,
            &mut contributors,
        )?;
    }
    for (sample, count) in output.iter_mut().zip(contributors) {
        if count > 1 {
            *sample /= count as f32;
        }
    }
    Ok(output)
}

fn add_aligned(
    source: &mut RawF32LeSource,
    delay: u64,
    output_start: u64,
    output: &mut [f32],
    contributors: &mut [u8],
) -> Result<()> {
    let output_end = output_start + output.len() as u64;
    let source_output_end = delay + source.frames();
    let overlap_start = output_start.max(delay);
    let overlap_end = output_end.min(source_output_end);
    if overlap_start >= overlap_end {
        return Ok(());
    }
    let source_start = overlap_start - delay;
    let samples = source.read_frames(source_start, (overlap_end - overlap_start) as usize)?;
    let destination = (overlap_start - output_start) as usize;
    for (index, sample) in samples.into_iter().enumerate() {
        output[destination + index] += sample;
        contributors[destination + index] = contributors[destination + index].saturating_add(1);
    }
    Ok(())
}

const CLASSIC_FLOAT_WAV_HEADER_BYTES: u64 = 44;
const RF64_FLOAT_WAV_HEADER_BYTES: u64 = 80;

fn needs_rf64(frames: u64) -> Result<bool> {
    let payload = frames
        .checked_mul(4)
        .ok_or_else(|| AppError::Audio("mic master payload length overflow".into()))?;
    Ok(payload.saturating_add(CLASSIC_FLOAT_WAV_HEADER_BYTES - 8) > u32::MAX as u64)
}

/// RF64 mono/f32 header. `ds64` carries all exact u64 lengths; the legacy RIFF and data length
/// fields are the mandated 0xFFFF_FFFF sentinels. Keeping this separate makes >4 GiB behavior
/// testable from virtual frame counts without allocating a multi-gigabyte fixture.
fn rf64_float_header(frames: u64, rate: u32) -> Result<[u8; RF64_FLOAT_WAV_HEADER_BYTES as usize]> {
    if rate == 0 {
        return Err(AppError::Audio("mic master sample rate is zero".into()));
    }
    let data_bytes = frames
        .checked_mul(4)
        .ok_or_else(|| AppError::Audio("RF64 mic master payload length overflow".into()))?;
    let riff_bytes = data_bytes
        .checked_add(RF64_FLOAT_WAV_HEADER_BYTES - 8)
        .ok_or_else(|| AppError::Audio("RF64 mic master length overflow".into()))?;
    let byte_rate = rate
        .checked_mul(4)
        .ok_or_else(|| AppError::Audio("RF64 mic master byte rate overflow".into()))?;
    let mut header = [0u8; RF64_FLOAT_WAV_HEADER_BYTES as usize];
    header[0..4].copy_from_slice(b"RF64");
    header[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    header[8..12].copy_from_slice(b"WAVE");
    header[12..16].copy_from_slice(b"ds64");
    header[16..20].copy_from_slice(&28u32.to_le_bytes());
    header[20..28].copy_from_slice(&riff_bytes.to_le_bytes());
    header[28..36].copy_from_slice(&data_bytes.to_le_bytes());
    header[36..44].copy_from_slice(&frames.to_le_bytes());
    header[44..48].copy_from_slice(&0u32.to_le_bytes()); // no ds64 table entries
    header[48..52].copy_from_slice(b"fmt ");
    header[52..56].copy_from_slice(&16u32.to_le_bytes());
    header[56..58].copy_from_slice(&3u16.to_le_bytes()); // IEEE float
    header[58..60].copy_from_slice(&1u16.to_le_bytes());
    header[60..64].copy_from_slice(&rate.to_le_bytes());
    header[64..68].copy_from_slice(&byte_rate.to_le_bytes());
    header[68..70].copy_from_slice(&4u16.to_le_bytes());
    header[70..72].copy_from_slice(&32u16.to_le_bytes());
    header[72..76].copy_from_slice(b"data");
    header[76..80].copy_from_slice(&u32::MAX.to_le_bytes());
    Ok(header)
}

fn write_rf64_float_master(source: &mut RawF32LeSource, file: &mut File) -> Result<()> {
    file.write_all(&rf64_float_header(source.frames(), source.sample_rate())?)
        .map_err(|e| AppError::Audio(format!("write RF64 mic master header: {e}")))?;
    let mut offset = 0u64;
    while offset < source.frames() {
        let samples = source.read_frames(offset, COPY_FRAMES)?;
        if samples.is_empty() {
            break;
        }
        let mut encoded = Vec::with_capacity(samples.len().saturating_mul(4));
        for sample in &samples {
            encoded.extend_from_slice(&sample.to_le_bytes());
        }
        file.write_all(&encoded)
            .map_err(|e| AppError::Audio(format!("write RF64 mic master: {e}")))?;
        offset += samples.len() as u64;
    }
    let expected_len = source
        .frames()
        .checked_mul(4)
        .and_then(|bytes| bytes.checked_add(RF64_FLOAT_WAV_HEADER_BYTES))
        .ok_or_else(|| AppError::Audio("RF64 mic master length overflow".into()))?;
    let (_, _, actual_len) = identity(file)?;
    if actual_len != expected_len {
        return Err(AppError::Audio(
            "RF64 mic master has an unexpected final length".into(),
        ));
    }
    Ok(())
}

pub(crate) fn stream_raw_to_float_wav(
    raw: &Path,
    rate: u32,
    staging_dir: &Path,
    wav: &Path,
    generation_id: &str,
) -> Result<VerifiedFile> {
    let source_before = verify_existing_file(raw)?;
    let mut source = RawF32LeSource::open(raw, rate)?;
    remove_stale_master_parts(staging_dir, generation_id, "mic-master", wav)?;
    let temp = staging_dir.join(format!(
        ".{generation_id}.mic-master-{}.part",
        uuid::Uuid::new_v4()
    ));
    let file = open_create_new_nofollow(&temp)?;
    if needs_rf64(source.frames())? {
        let mut file = file;
        write_rf64_float_master(&mut source, &mut file)?;
        file.sync_all()
            .map_err(|e| AppError::Audio(format!("sync RF64 mic master: {e}")))?;
    } else {
        let mut writer = hound::WavWriter::new(
            file,
            hound::WavSpec {
                channels: 1,
                sample_rate: rate,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .map_err(|e| AppError::Audio(format!("create mic master WAV: {e}")))?;
        let mut offset = 0u64;
        while offset < source.frames() {
            let samples = source.read_frames(offset, COPY_FRAMES)?;
            if samples.is_empty() {
                break;
            }
            for sample in &samples {
                writer
                    .write_sample(*sample)
                    .map_err(|e| AppError::Audio(format!("write mic master: {e}")))?;
            }
            offset += samples.len() as u64;
        }
        writer
            .finalize()
            .map_err(|e| AppError::Audio(format!("finalize mic master: {e}")))?;
        let handle = open_existing_nofollow(&temp)?;
        handle
            .sync_all()
            .map_err(|e| AppError::Audio(format!("sync mic master: {e}")))?;
    }
    let source_after = verify_existing_file(raw)?;
    if source_before.device() != source_after.device()
        || source_before.inode() != source_after.inode()
        || source_before.byte_len() != source_after.byte_len()
        || source_before.sha256() != source_after.sha256()
    {
        return Err(AppError::Audio(
            "mic raw source changed while creating its managed master".into(),
        ));
    }
    let candidate = verify_existing_file(&temp)?;
    publish_verified_staging(&temp, wav, &candidate, "mic master")
}

/// Publish a managed copy of a private raw artifact without ever linking the managed pathname to
/// the raw inode. The private source therefore stays single-link and remains eligible for exact
/// ledger-bound deletion after the DB pointer is durable.
pub(crate) fn copy_verified_file_no_clobber(
    source: &Path,
    staging_dir: &Path,
    destination: &Path,
    generation_id: &str,
) -> Result<VerifiedFile> {
    let source_before = verify_existing_file(source)?;
    remove_stale_master_parts(staging_dir, generation_id, "system-master", destination)?;
    let staging = staging_dir.join(format!(
        ".{generation_id}.system-master-{}.part",
        uuid::Uuid::new_v4()
    ));
    let mut input = open_existing_nofollow(source)?;
    let mut output = open_create_new_nofollow(&staging)?;
    std::io::copy(&mut input, &mut output)
        .map_err(|e| AppError::Audio(format!("copy system master staging file: {e}")))?;
    output
        .sync_all()
        .map_err(|e| AppError::Audio(format!("sync system master staging file: {e}")))?;
    let source_after = verify_existing_file(source)?;
    let candidate = verify_existing_file(&staging)?;
    if source_before.device() != source_after.device()
        || source_before.inode() != source_after.inode()
        || source_before.byte_len() != source_after.byte_len()
        || source_before.sha256() != source_after.sha256()
        || candidate.byte_len() != source_after.byte_len()
        || candidate.sha256() != source_after.sha256()
    {
        return Err(AppError::Audio(
            "system source changed while creating its managed master".into(),
        ));
    }
    publish_verified_staging(&staging, destination, &candidate, "system master")
}

fn remove_stale_master_parts(
    staging_dir: &Path,
    generation_id: &str,
    kind: &str,
    destination: &Path,
) -> Result<()> {
    let staging_prefix = format!(".{generation_id}.{kind}-");
    for entry in std::fs::read_dir(staging_dir)
        .map_err(|e| AppError::Audio(format!("inspect {kind} staging directory: {e}")))?
    {
        let entry =
            entry.map_err(|e| AppError::Audio(format!("inspect {kind} staging entry: {e}")))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(&staging_prefix) && name.ends_with(".part") {
            let stale = entry.path();
            if !reconcile_stale_publication(&stale, destination, kind)? {
                let proof = verify_existing_file(&stale)?;
                VerifiedDeletion::for_file(&stale, &proof)?
                    .remove("remove stale tracked master staging file")?;
            }
        }
    }
    Ok(())
}

/// Close the crash window after a no-clobber hard-link succeeded but before its private staging
/// name was unlinked. Both names must still identify one byte-identical two-link inode; only then
/// is the staging name removed and the managed destination returned to the required nlink=1 state.
fn reconcile_stale_publication(staging: &Path, destination: &Path, label: &str) -> Result<bool> {
    let staging_meta = std::fs::symlink_metadata(staging)
        .map_err(|e| AppError::Audio(format!("inspect stale {label} staging path: {e}")))?;
    if staging_meta.file_type().is_symlink() || !staging_meta.is_file() {
        return Err(AppError::Audio(format!(
            "stale {label} staging path is not a regular file"
        )));
    }
    if staging_meta.nlink() == 1 {
        return Ok(false);
    }
    if staging_meta.nlink() != 2 {
        return Err(AppError::Audio(format!(
            "stale {label} staging inode has an unexpected link count"
        )));
    }
    let staging_proof = verify_existing_file_with_nlink(staging, 2)?;
    let destination_proof = verify_existing_file_with_nlink(destination, 2)?;
    if staging_proof.device() != destination_proof.device()
        || staging_proof.inode() != destination_proof.inode()
        || staging_proof.byte_len() != destination_proof.byte_len()
        || staging_proof.sha256() != destination_proof.sha256()
    {
        return Err(AppError::Audio(format!(
            "stale {label} staging link does not match its managed destination"
        )));
    }
    let handle = open_existing_nofollow(staging)?;
    let path_now = std::fs::symlink_metadata(staging)
        .map_err(|e| AppError::Audio(format!("reinspect stale {label} staging link: {e}")))?;
    if path_now.file_type().is_symlink()
        || path_now.dev() != staging_proof.device()
        || path_now.ino() != staging_proof.inode()
        || path_now.nlink() != 2
    {
        return Err(AppError::Audio(format!(
            "stale {label} staging pathname changed before unlink"
        )));
    }
    std::fs::remove_file(staging)
        .map_err(|e| AppError::Audio(format!("remove stale {label} staging link: {e}")))?;
    let after = handle
        .metadata()
        .map_err(|e| AppError::Audio(format!("verify stale {label} staging unlink: {e}")))?;
    if after.dev() != staging_proof.device()
        || after.ino() != staging_proof.inode()
        || after.nlink() != 1
    {
        return Err(AppError::Audio(format!(
            "stale {label} staging unlink did not leave one managed name"
        )));
    }
    sync_parent_dir(staging, "sync recovered staging directory")?;
    sync_parent_dir(destination, "sync recovered managed directory")?;
    let published = verify_existing_file(destination)?;
    if published.device() != destination_proof.device()
        || published.inode() != destination_proof.inode()
        || published.byte_len() != destination_proof.byte_len()
        || published.sha256() != destination_proof.sha256()
    {
        return Err(AppError::Audio(format!(
            "recovered {label} publication changed after staging unlink"
        )));
    }
    Ok(true)
}

fn publish_verified_staging(
    staging: &Path,
    destination: &Path,
    candidate: &VerifiedFile,
    label: &str,
) -> Result<VerifiedFile> {
    match std::fs::hard_link(staging, destination) {
        Ok(()) => {
            let destination_file = open_existing_nofollow(destination)?;
            let destination_meta = destination_file
                .metadata()
                .map_err(|e| AppError::Audio(format!("stat {label} publication: {e}")))?;
            if destination_meta.dev() != candidate.device()
                || destination_meta.ino() != candidate.inode()
                || destination_meta.nlink() != 2
            {
                return Err(AppError::Audio(format!(
                    "published {label} does not name its verified staging inode"
                )));
            }
            let staging_meta = std::fs::symlink_metadata(staging)
                .map_err(|e| AppError::Audio(format!("inspect {label} staging link: {e}")))?;
            if staging_meta.dev() != candidate.device() || staging_meta.ino() != candidate.inode() {
                return Err(AppError::Audio(format!(
                    "{label} staging pathname changed before unlink"
                )));
            }
            std::fs::remove_file(staging)
                .map_err(|e| AppError::Audio(format!("remove {label} staging link: {e}")))?;
            sync_parent_dir(staging, "sync master staging directory")?;
            sync_parent_dir(destination, "sync managed master directory")?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = verify_existing_file(destination)?;
            if existing.byte_len() != candidate.byte_len()
                || existing.sha256() != candidate.sha256()
            {
                return Err(AppError::Audio(format!(
                    "existing {label} conflicts with the verified candidate"
                )));
            }
            VerifiedDeletion::for_file(staging, candidate)?
                .remove("remove duplicate master staging file")?;
            return Ok(existing);
        }
        Err(error) => {
            return Err(AppError::Audio(format!(
                "publish {label} without clobber: {error}"
            )))
        }
    }
    let published = verify_existing_file(destination)?;
    if published.byte_len() != candidate.byte_len() || published.sha256() != candidate.sha256() {
        return Err(AppError::Audio(format!(
            "managed {label} changed after publication"
        )));
    }
    Ok(published)
}

/// Exact unlink authorities for the legacy names which remain authoritative until the replacement
/// generation has been reread from SQLCipher as an exact FINALIZED generation.
#[cfg(test)]
pub(crate) struct LegacyRecordingSources {
    mic: VerifiedDeletion,
    system: Option<VerifiedDeletion>,
}

#[cfg(test)]
impl LegacyRecordingSources {
    pub(crate) fn remove(self) -> Result<()> {
        self.mic.remove("remove adopted legacy mic spill")?;
        if let Some(system) = self.system {
            system.remove("remove adopted legacy system spill")?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) struct AdoptedLegacyRecording {
    pub(crate) recording: FinalizedRecording,
    pub(crate) sources: LegacyRecordingSources,
}

#[cfg(test)]
trait LegacyAdoptionLedger {
    fn prepare(
        &self,
        key: &RecordingGenerationKey,
        mic: &RecordingMicAssertion,
    ) -> Result<RecordingGenerationLease>;
    fn begin(&self, key: &RecordingGenerationKey, lease: &RecordingGenerationLease) -> Result<()>;
    fn checkpoint(
        &self,
        key: &RecordingGenerationKey,
        lease: &RecordingGenerationLease,
        expected: &RecordingCheckpointAssertion,
        verified: &VerifiedMicCheckpoint,
    ) -> Result<()>;
    fn finalize(
        &self,
        key: &RecordingGenerationKey,
        lease: &RecordingGenerationLease,
        system: Option<&VerifiedSystemArtifact>,
    ) -> Result<()>;
    fn snapshot(&self, key: &RecordingGenerationKey)
        -> Result<Option<RecordingGenerationSnapshot>>;
}

#[cfg(test)]
impl LegacyAdoptionLedger for Db {
    fn prepare(
        &self,
        key: &RecordingGenerationKey,
        mic: &RecordingMicAssertion,
    ) -> Result<RecordingGenerationLease> {
        self.prepare_recording_generation(key, mic, RECORDING_LEASE_MS)
    }

    fn begin(&self, key: &RecordingGenerationKey, lease: &RecordingGenerationLease) -> Result<()> {
        self.begin_recording_capture(key, lease)
    }

    fn checkpoint(
        &self,
        key: &RecordingGenerationKey,
        lease: &RecordingGenerationLease,
        expected: &RecordingCheckpointAssertion,
        verified: &VerifiedMicCheckpoint,
    ) -> Result<()> {
        self.checkpoint_recording_generation(key, lease, expected, verified)
    }

    fn finalize(
        &self,
        key: &RecordingGenerationKey,
        lease: &RecordingGenerationLease,
        system: Option<&VerifiedSystemArtifact>,
    ) -> Result<()> {
        self.finalize_recording_generation(
            key,
            lease,
            system,
            Some(RecordingCaptureFault::Interrupted),
        )
    }

    fn snapshot(
        &self,
        key: &RecordingGenerationKey,
    ) -> Result<Option<RecordingGenerationSnapshot>> {
        self.get_recording_generation_snapshot(key)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
enum LegacyCopyFault {
    None,
    #[cfg(test)]
    DiskFull,
    #[cfg(test)]
    ShortCopy,
    #[cfg(test)]
    HashMismatch,
}

#[cfg(test)]
#[derive(Clone, Copy)]
struct LegacyCopyFaults {
    mic: LegacyCopyFault,
    system: LegacyCopyFault,
}

#[cfg(test)]
struct VerifiedCanonicalCopy {
    source: VerifiedFile,
    destination: VerifiedFile,
}

/// Adopt a legacy crash-spill without reading it into RAM. The returned source authorities are the
/// only permission to unlink the old spill names. Callers must retain them until this function has
/// returned success; every error leaves the original spill and sidecar recoverable.
#[cfg(test)]
pub(crate) fn adopt_legacy_crash_spill(
    db: &Db,
    meeting_id: &str,
    spill_path: &Path,
    sample_rate: u32,
    system_path: Option<&Path>,
    inflight_dir: &Path,
) -> Result<AdoptedLegacyRecording> {
    adopt_legacy_crash_spill_with(
        db,
        meeting_id,
        spill_path,
        sample_rate,
        system_path,
        inflight_dir,
        LegacyCopyFaults {
            mic: LegacyCopyFault::None,
            system: LegacyCopyFault::None,
        },
    )
}

#[cfg(test)]
fn adopt_legacy_crash_spill_with<L: LegacyAdoptionLedger>(
    ledger: &L,
    meeting_id: &str,
    spill_path: &Path,
    sample_rate: u32,
    system_path: Option<&Path>,
    inflight_dir: &Path,
    copy_faults: LegacyCopyFaults,
) -> Result<AdoptedLegacyRecording> {
    let key = RecordingGenerationKey::fresh(meeting_id)?;
    let empty = empty_checkpoint()?;
    if !(8_000..=384_000).contains(&sample_rate) {
        return Err(AppError::InvalidArg(
            "invalid legacy recording sample rate".into(),
        ));
    }
    let mic_path = inflight_dir.join(format!("{}.mic.f32", key.generation_id()));
    let mic_copy = copy_into_private_canonical(
        spill_path,
        &mic_path,
        "adopt legacy mic spill",
        copy_faults.mic,
    )?;
    let mic_file = &mic_copy.destination;
    if mic_file.byte_len() == 0 || mic_file.byte_len() % 4 != 0 {
        VerifiedDeletion::for_file(&mic_path, mic_file)?
            .remove("remove unusable untracked legacy mic copy")?;
        return Err(AppError::Audio(
            "legacy mic spill has no complete frames".into(),
        ));
    }
    let mic_result = RecordingMicAssertion::for_generation(
        &key,
        sample_rate,
        mic_file.device(),
        mic_file.inode(),
    );
    let mic = match mic_result {
        Ok(mic) => mic,
        Err(error) => {
            VerifiedDeletion::for_file(&mic_path, mic_file)?
                .remove("remove invalid untracked legacy mic copy")?;
            return Err(error);
        }
    };
    let lease = match ledger.prepare(&key, &mic) {
        Ok(lease) => lease,
        Err(error) => {
            if let Ok(None) = ledger.snapshot(&key) {
                VerifiedDeletion::for_file(&mic_path, mic_file)?
                    .remove("remove untracked legacy mic copy after prepare failure")?;
            }
            // A committed PREPARED row owns this inode even though its affine lease was not
            // returned. Retain both copies; stale-generation recovery can claim it later.
            return Err(error);
        }
    };
    if let Err(error) = ledger.begin(&key, &lease) {
        let began = matches!(
            ledger.snapshot(&key),
            Ok(Some(row)) if snapshot_matches_mic(&row, &key, &mic, &empty, false)
        );
        if !began {
            return Err(error);
        }
    }
    let frames = mic_file.byte_len() / 4;
    let verified_mic = VerifiedMicCheckpoint::from_file(&key, &mic, mic_file, frames)?;
    let durable =
        RecordingCheckpointAssertion::new(frames, mic_file.byte_len(), mic_file.sha256())?;
    if let Err(error) = ledger.checkpoint(&key, &lease, &empty, &verified_mic) {
        let checkpointed = matches!(
            ledger.snapshot(&key),
            Ok(Some(row)) if snapshot_matches_mic(&row, &key, &mic, &durable, false)
        );
        if !checkpointed {
            return Err(error);
        }
    }

    let system_copy = if let Some(system) = system_path {
        let path = inflight_dir.join(format!("{}.system.wav", key.generation_id()));
        Some((
            path.clone(),
            copy_into_private_canonical(
                system,
                &path,
                "adopt legacy system track",
                copy_faults.system,
            )?,
        ))
    } else {
        None
    };
    let verified_system_result = system_copy
        .as_ref()
        .map(|(path, _)| VerifiedSystemArtifact::from_path(&key, path))
        .transpose();
    let expected_system_result = system_copy
        .as_ref()
        .map(|(_, copy)| {
            RecordingArtifactAssertion::for_generation(
                &key,
                RecordingArtifactRole::System,
                copy.destination.device(),
                copy.destination.inode(),
                copy.destination.byte_len(),
                copy.destination.sha256(),
            )
        })
        .transpose();
    let (verified_system, expected_system) = match (verified_system_result, expected_system_result)
    {
        (Ok(verified), Ok(expected)) => (verified, expected),
        (Err(error), _) | (_, Err(error)) => {
            if let Some((path, copy)) = system_copy.as_ref() {
                VerifiedDeletion::for_file(path, &copy.destination)?
                    .remove("remove invalid untracked legacy system copy")?;
            }
            return Err(error);
        }
    };
    if let Err(error) = ledger.finalize(&key, &lease, verified_system.as_ref()) {
        match ledger.snapshot(&key) {
            Ok(Some(row))
                if snapshot_matches_finalized(
                    &row,
                    &key,
                    &mic,
                    &durable,
                    expected_system.as_ref(),
                ) => {}
            Ok(Some(row)) if snapshot_matches_mic(&row, &key, &mic, &durable, false) => {
                if let Some((path, copy)) = system_copy.as_ref() {
                    VerifiedDeletion::for_file(path, &copy.destination)?
                        .remove("remove untracked legacy system copy after finalize failure")?;
                }
                return Err(error);
            }
            Ok(_) | Err(_) => return Err(AppError::Storage(
                "legacy recording finalize commit was ambiguous; all source artifacts preserved"
                    .into(),
            )),
        }
    }

    let final_snapshot = ledger.snapshot(&key)?.ok_or_else(|| {
        AppError::Storage("finalized legacy recording generation disappeared".into())
    })?;
    if !snapshot_matches_finalized(
        &final_snapshot,
        &key,
        &mic,
        &durable,
        expected_system.as_ref(),
    ) {
        return Err(AppError::Storage(
            "legacy recording generation is not the exact finalized replacement".into(),
        ));
    }
    let _ = verify_same_file(&mic_path, mic_file, "finalized legacy mic replacement")?;
    if let Some((path, copy)) = system_copy.as_ref() {
        let _ = verify_same_file(
            path,
            &copy.destination,
            "finalized legacy system replacement",
        )?;
    }

    let now = Instant::now();
    let mic_source_proof = verify_same_file(
        spill_path,
        &mic_copy.source,
        "legacy mic source before deletion authorization",
    )?;
    let mic_source = VerifiedDeletion::for_file(spill_path, &mic_source_proof)?;
    let system_source = system_path
        .zip(system_copy.as_ref())
        .map(|(source, (_, copy))| {
            let proof = verify_same_file(
                source,
                &copy.source,
                "legacy system source before deletion authorization",
            )?;
            VerifiedDeletion::for_file(source, &proof)
        })
        .transpose()?;
    Ok(AdoptedLegacyRecording {
        recording: FinalizedRecording {
            key,
            lease,
            mic_path,
            sample_rate,
            frames,
            started_at: now,
            capture_fault: Some(RecordingCaptureFault::Interrupted),
            system_wav: system_copy.as_ref().map(|(path, _)| path.clone()),
            system_started_at: system_path.map(|_| now),
        },
        sources: LegacyRecordingSources {
            mic: mic_source,
            system: system_source,
        },
    })
}

/// Compatibility helper for lifecycle tests which create a generation from an arbitrary raw file.
/// It deliberately leaves that input file owned by the caller.
#[cfg(test)]
pub(crate) fn adopt_legacy_raw_recording(
    db: &Db,
    meeting_id: &str,
    spill_path: &Path,
    sample_rate: u32,
    system_path: Option<&Path>,
    inflight_dir: &Path,
) -> Result<FinalizedRecording> {
    adopt_legacy_crash_spill(
        db,
        meeting_id,
        spill_path,
        sample_rate,
        system_path,
        inflight_dir,
    )
    .map(|adopted| adopted.recording)
}

#[cfg(test)]
fn empty_checkpoint() -> Result<RecordingCheckpointAssertion> {
    RecordingCheckpointAssertion::new(
        0,
        0,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    )
}

#[cfg(test)]
fn snapshot_matches_mic(
    row: &RecordingGenerationSnapshot,
    key: &RecordingGenerationKey,
    mic: &RecordingMicAssertion,
    checkpoint: &RecordingCheckpointAssertion,
    finalized: bool,
) -> bool {
    row.key == *key
        && row.state
            == if finalized {
                RecordingGenerationState::Finalized
            } else {
                RecordingGenerationState::Capturing
            }
        && row.mic == *mic
        && row.checkpoint == *checkpoint
        && (finalized || (row.system_artifact.is_none() && row.capture_fault.is_none()))
        && row.archive.is_none()
        && row.retirement_reason.is_none()
        && row.archived_at_ms.is_none()
        && row.retired_at_ms.is_none()
        && row.system_start_offset_micros.is_none()
        && row.cleanup_mask == 0
        && (finalized || row.finalized_at_ms.is_none())
}

#[cfg(test)]
fn snapshot_matches_finalized(
    row: &RecordingGenerationSnapshot,
    key: &RecordingGenerationKey,
    mic: &RecordingMicAssertion,
    checkpoint: &RecordingCheckpointAssertion,
    system: Option<&RecordingArtifactAssertion>,
) -> bool {
    snapshot_matches_mic(row, key, mic, checkpoint, true)
        && row.system_artifact.as_ref() == system
        && row.capture_fault == Some(RecordingCaptureFault::Interrupted)
        && row.finalized_at_ms.is_some()
}

#[cfg(test)]
fn verify_same_file(path: &Path, expected: &VerifiedFile, label: &str) -> Result<VerifiedFile> {
    let actual = verify_existing_file(path)?;
    if actual.device() != expected.device()
        || actual.inode() != expected.inode()
        || actual.byte_len() != expected.byte_len()
        || actual.sha256() != expected.sha256()
    {
        return Err(AppError::Audio(format!(
            "{label} changed after verification"
        )));
    }
    Ok(actual)
}

#[cfg(test)]
fn copy_into_private_canonical(
    source: &Path,
    destination: &Path,
    operation: &str,
    fault: LegacyCopyFault,
) -> Result<VerifiedCanonicalCopy> {
    #[cfg(not(test))]
    let _ = fault;
    let mut input = open_existing_nofollow(source)?;
    let mut output = open_create_new_nofollow(destination)?;
    let result = (|| -> Result<VerifiedCanonicalCopy> {
        let source_before = input
            .metadata()
            .map_err(|e| AppError::Audio(format!("{operation}: stat source: {e}")))?;
        if source_before.nlink() != 1 {
            return Err(AppError::Audio(format!(
                "{operation}: source does not have exactly one owned name"
            )));
        }
        let expected_len = source_before.len();
        let mut copied = 0u64;
        let mut buffer = vec![0u8; 256 * 1024];
        loop {
            let read = input
                .read(&mut buffer)
                .map_err(|e| AppError::Audio(format!("{operation}: read source: {e}")))?;
            if read == 0 {
                break;
            }
            #[cfg(test)]
            if fault == LegacyCopyFault::DiskFull {
                let prefix = read.min(17);
                output.write_all(&buffer[..prefix]).map_err(|e| {
                    AppError::Audio(format!("{operation}: stage fault prefix: {e}"))
                })?;
                return Err(AppError::Audio(format!("{operation}: simulated disk full")));
            }
            #[cfg(test)]
            if fault == LegacyCopyFault::ShortCopy {
                let prefix = read.saturating_sub(1);
                output
                    .write_all(&buffer[..prefix])
                    .map_err(|e| AppError::Audio(format!("{operation}: stage short copy: {e}")))?;
                copied = copied.saturating_add(prefix as u64);
                break;
            }
            output
                .write_all(&buffer[..read])
                .map_err(|e| AppError::Audio(format!("{operation}: write destination: {e}")))?;
            copied = copied
                .checked_add(read as u64)
                .ok_or_else(|| AppError::Audio(format!("{operation}: copy length overflow")))?;
        }
        if copied != expected_len {
            return Err(AppError::Audio(format!(
                "{operation}: destination is a short copy"
            )));
        }
        #[cfg(test)]
        if fault == LegacyCopyFault::HashMismatch && expected_len > 0 {
            output
                .seek(SeekFrom::Start(0))
                .map_err(|e| AppError::Audio(format!("{operation}: seek fault target: {e}")))?;
            output
                .write_all(&[0xA5])
                .map_err(|e| AppError::Audio(format!("{operation}: write fault target: {e}")))?;
        }
        output
            .sync_all()
            .map_err(|e| AppError::Audio(format!("{operation}: sync destination: {e}")))?;
        let source_verified =
            verify_open_handle(source, &mut input, source_before.dev(), source_before.ino())?;
        let destination_meta = output
            .metadata()
            .map_err(|e| AppError::Audio(format!("{operation}: stat destination: {e}")))?;
        let destination_verified = verify_open_handle(
            destination,
            &mut output,
            destination_meta.dev(),
            destination_meta.ino(),
        )?;
        if source_verified.byte_len() != destination_verified.byte_len()
            || source_verified.sha256() != destination_verified.sha256()
        {
            return Err(AppError::Audio(format!(
                "{operation}: replacement is not byte-identical"
            )));
        }
        sync_parent_dir(destination, operation)?;
        Ok(VerifiedCanonicalCopy {
            source: source_verified,
            destination: destination_verified,
        })
    })();
    if result.is_err() {
        let metadata = output
            .metadata()
            .map_err(|e| AppError::Audio(format!("{operation}: stat failed output: {e}")))?;
        let proof = verify_open_handle(destination, &mut output, metadata.dev(), metadata.ino())?;
        VerifiedDeletion::for_file(destination, &proof)?
            .remove("remove failed untracked legacy copy")?;
    }
    result
}

#[cfg(test)]
fn verify_open_handle(
    path: &Path,
    file: &mut File,
    expected_device: u64,
    expected_inode: u64,
) -> Result<VerifiedFile> {
    let path_meta = std::fs::symlink_metadata(path)
        .map_err(|e| AppError::Audio(format!("inspect stable recording path: {e}")))?;
    let before = file
        .metadata()
        .map_err(|e| AppError::Audio(format!("stat stable recording handle: {e}")))?;
    if path_meta.file_type().is_symlink()
        || !before.is_file()
        || path_meta.dev() != expected_device
        || path_meta.ino() != expected_inode
        || before.dev() != expected_device
        || before.ino() != expected_inode
        || before.nlink() != 1
    {
        return Err(AppError::Audio(
            "recording path no longer names the stable single-link handle".into(),
        ));
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|e| AppError::Audio(format!("seek stable recording handle: {e}")))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    let mut byte_len = 0u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| AppError::Audio(format!("hash stable recording handle: {e}")))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        byte_len = byte_len
            .checked_add(read as u64)
            .ok_or_else(|| AppError::Audio("stable recording length overflow".into()))?;
    }
    let after = file
        .metadata()
        .map_err(|e| AppError::Audio(format!("restat stable recording handle: {e}")))?;
    if after.dev() != expected_device
        || after.ino() != expected_inode
        || after.len() != byte_len
        || after.len() != before.len()
        || after.nlink() != 1
    {
        return Err(AppError::Audio(
            "recording handle changed while hashing".into(),
        ));
    }
    Ok(VerifiedFile {
        basename: basename(path)?,
        device: expected_device,
        inode: expected_inode,
        byte_len,
        sha256: hex_digest(hasher.finalize()),
        prefix_hasher: None,
    })
}

#[cfg(test)]
mod tests {
    /// T31 — a promoted master is the stream itself, shifted onto the ARCHIVE's timeline.
    ///
    /// The delay is not decoration: `publish_mix` front-pads each stream by exactly this many
    /// frames when it builds the archive, so a master written WITHOUT the padding would place every
    /// retry segment earlier than the recording it came from, and the two streams would be merged
    /// against each other at the wrong offset.
    #[test]
    fn a_promoted_master_carries_the_archive_front_padding() {
        use crate::audio::source::MonoSource;
        let dir = std::env::temp_dir().join(format!(
            "murmur-master-pad-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("in.f32");
        let samples: Vec<f32> = vec![1.0, 1.0, 1.0, 1.0];
        let mut bytes = Vec::new();
        for s in &samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(&raw, &bytes).unwrap();

        let dest = dir.join("m.mic.wav");
        publish_stream_master(&raw, &dest, 3).unwrap();
        let mut got = WavMonoSource::open(&dest).unwrap();
        assert_eq!(got.frames(), 7, "3 frames of padding + 4 of signal");
        let frames = got.read_frames(0, 7).unwrap();
        assert!(
            frames[..3].iter().all(|s| *s == 0.0),
            "the delay is leading SILENCE, not the stream shifted into it: {frames:?}"
        );
        assert!(
            frames[3..].iter().all(|s| *s > 0.9),
            "the stream itself follows the padding: {frames:?}"
        );

        // Zero delay is the mic-leading case and must not pad at all.
        let dest0 = dir.join("m.sys.wav");
        publish_stream_master(&raw, &dest0, 0).unwrap();
        assert_eq!(WavMonoSource::open(&dest0).unwrap().frames(), 4);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A master is published ATOMICALLY: a reader never sees the staging dotfile under the final
    /// name, and nothing is left behind once it lands.
    #[test]
    fn publishing_a_master_leaves_no_staging_file() {
        let dir = std::env::temp_dir().join(format!(
            "murmur-master-atomic-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("in.f32");
        std::fs::write(&raw, 0.5f32.to_le_bytes()).unwrap();
        let dest = dir.join("m.mic.wav");
        publish_stream_master(&raw, &dest, 0).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with('.') && n.ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty(), "staging file survived: {leftovers:?}");
        assert!(dest.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An EMPTY stream is refused rather than published: a zero-frame master would be a column
    /// pointing at a file every later read has to special-case.
    #[test]
    fn an_empty_stream_is_not_published_as_a_master() {
        let dir = std::env::temp_dir().join(format!(
            "murmur-master-empty-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("in.f32");
        std::fs::write(&raw, b"").unwrap();
        let dest = dir.join("m.mic.wav");
        assert!(publish_stream_master(&raw, &dest, 0).is_err());
        assert!(!dest.exists(), "nothing is published for an empty stream");
        let _ = std::fs::remove_dir_all(&dir);
    }

    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::storage::models::{Folder, Meeting, MeetingStatus, NoteRecord};

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "murmur-source-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum LedgerFault {
        None,
        Prepare,
        Checkpoint,
        Finalize,
        FinalizeAfterCommit,
    }

    struct FaultingLegacyLedger<'a> {
        db: &'a Db,
        fault: LedgerFault,
    }

    impl LegacyAdoptionLedger for FaultingLegacyLedger<'_> {
        fn prepare(
            &self,
            key: &RecordingGenerationKey,
            mic: &RecordingMicAssertion,
        ) -> Result<RecordingGenerationLease> {
            if self.fault == LedgerFault::Prepare {
                return Err(AppError::Storage("injected legacy prepare failure".into()));
            }
            self.db
                .prepare_recording_generation(key, mic, RECORDING_LEASE_MS)
        }

        fn begin(
            &self,
            key: &RecordingGenerationKey,
            lease: &RecordingGenerationLease,
        ) -> Result<()> {
            self.db.begin_recording_capture(key, lease)
        }

        fn checkpoint(
            &self,
            key: &RecordingGenerationKey,
            lease: &RecordingGenerationLease,
            expected: &RecordingCheckpointAssertion,
            verified: &VerifiedMicCheckpoint,
        ) -> Result<()> {
            if self.fault == LedgerFault::Checkpoint {
                return Err(AppError::Storage(
                    "injected legacy checkpoint failure".into(),
                ));
            }
            self.db
                .checkpoint_recording_generation(key, lease, expected, verified)
        }

        fn finalize(
            &self,
            key: &RecordingGenerationKey,
            lease: &RecordingGenerationLease,
            system: Option<&VerifiedSystemArtifact>,
        ) -> Result<()> {
            if self.fault == LedgerFault::Finalize {
                return Err(AppError::Storage("injected legacy finalize failure".into()));
            }
            self.db.finalize_recording_generation(
                key,
                lease,
                system,
                Some(RecordingCaptureFault::Interrupted),
            )?;
            if self.fault == LedgerFault::FinalizeAfterCommit {
                return Err(AppError::Storage(
                    "injected ambiguous legacy finalize commit".into(),
                ));
            }
            Ok(())
        }

        fn snapshot(
            &self,
            key: &RecordingGenerationKey,
        ) -> Result<Option<RecordingGenerationSnapshot>> {
            self.db.get_recording_generation_snapshot(key)
        }
    }

    struct LegacyFixture {
        root: PathBuf,
        inflight: PathBuf,
        db: Db,
        meeting_id: String,
        mic: PathBuf,
        system: PathBuf,
    }

    impl LegacyFixture {
        fn new(tag: &str) -> Self {
            let root = temp(tag);
            let inflight = root.join("inflight");
            std::fs::create_dir_all(&inflight).unwrap();
            let db = Db::open_with_key(std::path::Path::new(":memory:"), TEST_DEK).unwrap();
            let meeting_id = uuid::Uuid::new_v4().hyphenated().to_string();
            db.insert_meeting(&Meeting {
                id: meeting_id.clone(),
                started_at: "2026-07-22T12:00:00Z".into(),
                ended_at: None,
                title: None,
                duration_s: 0,
                audio_path: None,
                status: MeetingStatus::Recording,
                folder_id: None,
            })
            .unwrap();
            let mic = root.join("legacy.spill");
            let system = root.join("legacy.sys.wav");
            let mut mic_bytes = Vec::new();
            for sample in [0.25f32, -0.5, 0.75, -1.0] {
                mic_bytes.extend_from_slice(&sample.to_le_bytes());
            }
            std::fs::write(&mic, mic_bytes).unwrap();
            std::fs::write(&system, b"RIFF-legacy-system-track").unwrap();
            Self {
                root,
                inflight,
                db,
                meeting_id,
                mic,
                system,
            }
        }
    }

    impl Drop for LegacyFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn legacy_copy_faults_clean_partial_destination_and_preserve_source() {
        for fault in [
            LegacyCopyFault::DiskFull,
            LegacyCopyFault::ShortCopy,
            LegacyCopyFault::HashMismatch,
        ] {
            let root = temp("legacy-copy-fault");
            std::fs::create_dir_all(&root).unwrap();
            let source = root.join("source.f32");
            let destination = root.join("destination.f32");
            let bytes = b"byte-identical-source-proof";
            std::fs::write(&source, bytes).unwrap();

            assert!(
                copy_into_private_canonical(&source, &destination, "faulted copy", fault).is_err()
            );
            assert_eq!(std::fs::read(&source).unwrap(), bytes);
            assert!(
                !destination.exists(),
                "failed private output must be exactly removed"
            );
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn legacy_prepare_failure_preserves_source_and_leaves_no_untracked_copy() {
        let fixture = LegacyFixture::new("legacy-prepare-failure");
        let ledger = FaultingLegacyLedger {
            db: &fixture.db,
            fault: LedgerFault::Prepare,
        };
        assert!(adopt_legacy_crash_spill_with(
            &ledger,
            &fixture.meeting_id,
            &fixture.mic,
            48_000,
            None,
            &fixture.inflight,
            LegacyCopyFaults {
                mic: LegacyCopyFault::None,
                system: LegacyCopyFault::None,
            },
        )
        .is_err());
        assert!(fixture.mic.exists());
        assert_eq!(std::fs::read_dir(&fixture.inflight).unwrap().count(), 0);
        assert!(!fixture
            .db
            .meeting_has_nonterminal_recording_generation(&fixture.meeting_id)
            .unwrap());
    }

    #[test]
    fn legacy_checkpoint_failure_preserves_source_under_recoverable_ledger() {
        let fixture = LegacyFixture::new("legacy-checkpoint-failure");
        let ledger = FaultingLegacyLedger {
            db: &fixture.db,
            fault: LedgerFault::Checkpoint,
        };
        assert!(adopt_legacy_crash_spill_with(
            &ledger,
            &fixture.meeting_id,
            &fixture.mic,
            48_000,
            None,
            &fixture.inflight,
            LegacyCopyFaults {
                mic: LegacyCopyFault::None,
                system: LegacyCopyFault::None,
            },
        )
        .is_err());
        assert!(fixture.mic.exists());
        assert_eq!(std::fs::read_dir(&fixture.inflight).unwrap().count(), 1);
        assert!(fixture
            .db
            .meeting_has_nonterminal_recording_generation(&fixture.meeting_id)
            .unwrap());
    }

    #[test]
    fn legacy_system_copy_failure_preserves_both_sources_without_partial_system() {
        let fixture = LegacyFixture::new("legacy-system-copy-failure");
        let ledger = FaultingLegacyLedger {
            db: &fixture.db,
            fault: LedgerFault::None,
        };
        assert!(adopt_legacy_crash_spill_with(
            &ledger,
            &fixture.meeting_id,
            &fixture.mic,
            48_000,
            Some(&fixture.system),
            &fixture.inflight,
            LegacyCopyFaults {
                mic: LegacyCopyFault::None,
                system: LegacyCopyFault::DiskFull,
            },
        )
        .is_err());
        assert!(fixture.mic.exists() && fixture.system.exists());
        let names = std::fs::read_dir(&fixture.inflight)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 1);
        assert!(names[0].ends_with(".mic.f32"));
    }

    #[test]
    fn legacy_finalize_failure_preserves_sources_and_cleans_untracked_system_copy() {
        let fixture = LegacyFixture::new("legacy-finalize-failure");
        let ledger = FaultingLegacyLedger {
            db: &fixture.db,
            fault: LedgerFault::Finalize,
        };
        assert!(adopt_legacy_crash_spill_with(
            &ledger,
            &fixture.meeting_id,
            &fixture.mic,
            48_000,
            Some(&fixture.system),
            &fixture.inflight,
            LegacyCopyFaults {
                mic: LegacyCopyFault::None,
                system: LegacyCopyFault::None,
            },
        )
        .is_err());
        assert!(fixture.mic.exists() && fixture.system.exists());
        let names = std::fs::read_dir(&fixture.inflight)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 1);
        assert!(names[0].ends_with(".mic.f32"));
    }

    #[test]
    fn legacy_sources_are_deleted_only_after_exact_finalized_reread() {
        for fault in [LedgerFault::None, LedgerFault::FinalizeAfterCommit] {
            let fixture = LegacyFixture::new("legacy-finalized-delete");
            let ledger = FaultingLegacyLedger {
                db: &fixture.db,
                fault,
            };
            let adopted = adopt_legacy_crash_spill_with(
                &ledger,
                &fixture.meeting_id,
                &fixture.mic,
                48_000,
                Some(&fixture.system),
                &fixture.inflight,
                LegacyCopyFaults {
                    mic: LegacyCopyFault::None,
                    system: LegacyCopyFault::None,
                },
            )
            .unwrap();
            assert!(fixture.mic.exists() && fixture.system.exists());
            let snapshot = fixture
                .db
                .get_recording_generation_snapshot(&adopted.recording.key)
                .unwrap()
                .unwrap();
            assert_eq!(snapshot.state, RecordingGenerationState::Finalized);
            adopted.sources.remove().unwrap();
            assert!(!fixture.mic.exists() && !fixture.system.exists());
        }
    }

    #[test]
    fn locked_stale_generation_is_not_read_or_claimed_and_remains_retryable() {
        let fixture = LegacyFixture::new("locked-ledger-recovery");
        let archive_dir = fixture.root.join("audio");
        std::fs::create_dir_all(&archive_dir).unwrap();
        let folder_id = uuid::Uuid::new_v4().to_string();
        fixture
            .db
            .insert_folder(&Folder {
                id: folder_id.clone(),
                name: "Private".into(),
                path: "Private".into(),
                parent_id: None,
                locked: false,
                created_at: "2026-07-22T12:00:00Z".into(),
            })
            .unwrap();
        fixture
            .db
            .upsert_note(&NoteRecord {
                meeting_id: fixture.meeting_id.clone(),
                provider_id: "test".into(),
                markdown: "durable".into(),
                created_at: "2026-07-22T12:00:01Z".into(),
                exported_path: None,
                model_requested: None,
                model_served: None,
                gateway_host: None,
            })
            .unwrap();
        fixture
            .db
            .set_note_folder(&fixture.meeting_id, Some(&folder_id))
            .unwrap();

        let recording = adopt_legacy_raw_recording(
            &fixture.db,
            &fixture.meeting_id,
            &fixture.mic,
            48_000,
            None,
            &fixture.inflight,
        )
        .unwrap();
        let key = recording.key.clone();
        let managed_mic = recording.mic_path.clone();
        let managed_bytes = std::fs::read(&managed_mic).unwrap();
        recording.release_for_recovery(&fixture.db).unwrap();
        let (locked_jobs, locked_claimed) =
            claim_stale_recording_generations_with_pre_recovery_hook(
                &fixture.db,
                &fixture.inflight,
                &archive_dir,
                |db, _| {
                    db.set_folder_locked(&folder_id, true, Some(b"wrapped"))
                        .unwrap();
                },
            );
        assert!(locked_jobs.is_empty() && locked_claimed.is_empty());
        assert_eq!(std::fs::read(&managed_mic).unwrap(), managed_bytes);
        assert_eq!(
            fixture
                .db
                .get_recording_generation_snapshot(&key)
                .unwrap()
                .unwrap()
                .state,
            RecordingGenerationState::Finalized
        );

        fixture
            .db
            .set_folder_locked(&folder_id, false, None)
            .unwrap();
        let (mut unlocked_jobs, unlocked_claimed) =
            claim_stale_recording_generations(&fixture.db, &fixture.inflight, &archive_dir);
        assert_eq!(unlocked_claimed, vec![fixture.meeting_id.clone()]);
        assert_eq!(unlocked_jobs.len(), 1);
        unlocked_jobs
            .pop()
            .unwrap()
            .recording
            .release_for_recovery(&fixture.db)
            .unwrap();
    }

    fn recovery_snapshot(
        path: &Path,
        committed: &[f32],
        state: RecordingGenerationState,
    ) -> RecordingGenerationSnapshot {
        let meeting_id = uuid::Uuid::new_v4().hyphenated().to_string();
        let key = RecordingGenerationKey::fresh(&meeting_id).unwrap();
        let file = open_existing_nofollow(path).unwrap();
        let (device, inode, _) = identity(&file).unwrap();
        let mic = RecordingMicAssertion::for_generation(&key, 48_000, device, inode).unwrap();
        let mut hasher = Sha256::new();
        for sample in committed {
            hasher.update(sample.to_le_bytes());
        }
        let checkpoint = RecordingCheckpointAssertion::new(
            committed.len() as u64,
            committed.len() as u64 * 4,
            &hex_digest(hasher.finalize()),
        )
        .unwrap();
        RecordingGenerationSnapshot {
            key,
            state,
            lease_expires_at_ms: 0,
            mic,
            checkpoint,
            system_artifact: None,
            capture_fault: None,
            archive: None,
            retirement_reason: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            finalized_at_ms: None,
            archived_at_ms: None,
            retired_at_ms: None,
            system_start_offset_micros: None,
            cleanup_mask: 0,
        }
    }

    #[test]
    fn interactive_retry_delete_lock_cleanup_reclaims_after_transient_failure_same_process() {
        let root = temp("interactive-cleanup-reclaim");
        let inflight = root.join("recording-inflight");
        let archive_dir = root.join("audio");
        std::fs::create_dir_all(&inflight).unwrap();
        std::fs::create_dir_all(&archive_dir).unwrap();
        let db = Db::open_with_key(std::path::Path::new(":memory:"), TEST_DEK).unwrap();
        let meeting_id = uuid::Uuid::new_v4().hyphenated().to_string();
        db.insert_meeting(&Meeting {
            id: meeting_id.clone(),
            started_at: "2026-07-22T12:00:00Z".into(),
            ended_at: Some("2026-07-22T12:01:00Z".into()),
            title: Some("cleanup retry".into()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Error,
            folder_id: None,
        })
        .unwrap();

        let legacy = root.join("legacy-mic.f32");
        let mut raw = Vec::new();
        for sample in [0.25f32, -0.5, 0.75, -1.0] {
            raw.extend_from_slice(&sample.to_le_bytes());
        }
        std::fs::write(&legacy, raw).unwrap();
        let recording =
            adopt_legacy_raw_recording(&db, &meeting_id, &legacy, 48_000, None, &inflight).unwrap();
        let key = recording.key.clone();
        let mic_path = recording.mic_path.clone();
        let archive_path = archive_dir.join(format!("{}.archive.wav", key.generation_id()));
        std::fs::write(&archive_path, b"RIFF-canonical-archive").unwrap();
        let archive_file = verify_existing_file(&archive_path).unwrap();
        let archive = VerifiedArchiveArtifact::from_file(&key, &archive_file).unwrap();
        db.archive_recording_generation(&key, &recording.lease, &archive)
            .unwrap();
        recording.release_for_recovery(&db).unwrap();

        // First interactive preflight sees a transient private-workspace I/O failure. The helper
        // must return the real error and release its newly claimed lease, not strand ownership
        // until process restart. Retry/Delete/Lock all call this exact helper under lifecycle.
        std::fs::set_permissions(&inflight, std::fs::Permissions::from_mode(0o000)).unwrap();
        let first = resume_released_generation_cleanup_for_meeting(
            &db,
            &inflight,
            &archive_dir,
            &meeting_id,
        );
        std::fs::set_permissions(&inflight, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(first.is_err());
        assert_eq!(
            db.get_recording_generation_snapshot(&key)
                .unwrap()
                .unwrap()
                .state,
            RecordingGenerationState::Archived
        );

        // Repair the transient filesystem condition. The very next attempt in this process must
        // claim the released row again, finish exact cleanup, and retire it; callers' final reread
        // can then allow Retry/Delete/Lock without a restart.
        resume_released_generation_cleanup_for_meeting(&db, &inflight, &archive_dir, &meeting_id)
            .unwrap();
        assert_eq!(
            db.get_recording_generation_snapshot(&key)
                .unwrap()
                .unwrap()
                .state,
            RecordingGenerationState::Retired
        );
        assert!(!mic_path.exists());
        assert!(archive_path.exists());

        std::fs::remove_file(legacy).unwrap();
        std::fs::remove_file(archive_path).unwrap();
        std::fs::remove_dir(inflight).unwrap();
        std::fs::remove_dir(archive_dir).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn stale_two_link_publication_is_reconciled_to_one_managed_name() {
        let dir = temp("stale-publication");
        std::fs::create_dir(&dir).unwrap();
        let staging = dir.join("staging.part");
        let destination = dir.join("final.wav");
        std::fs::write(&staging, b"verified audio").unwrap();
        std::fs::hard_link(&staging, &destination).unwrap();

        assert!(reconcile_stale_publication(&staging, &destination, "test").unwrap());
        assert!(!staging.exists());
        let published = verify_existing_file(&destination).unwrap();
        assert_eq!(published.byte_len(), 14);

        std::fs::remove_file(destination).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    struct MemorySource {
        rate: u32,
        samples: Vec<f32>,
    }

    impl MonoSource for MemorySource {
        fn sample_rate(&self) -> u32 {
            self.rate
        }
        fn frames(&self) -> u64 {
            self.samples.len() as u64
        }

        fn read_frames(&mut self, start: u64, max_frames: usize) -> Result<Vec<f32>> {
            let start = usize::try_from(start)
                .map_err(|_| AppError::Audio("test source offset overflow".into()))?;
            if start > self.samples.len() {
                return Err(AppError::Audio("test source starts past EOF".into()));
            }
            let end = self.samples.len().min(start.saturating_add(max_frames));
            Ok(self.samples[start..end].to_vec())
        }
    }

    #[test]
    fn streaming_resampler_preserves_exact_rational_tail_at_common_device_rates() {
        for rate in [44_100u32, 48_000, 96_000, 192_000] {
            for frames in [17usize, RESAMPLE_INPUT_FRAMES * 4 + 137] {
                let output = temp(&format!("resample-{rate}-{frames}"));
                let mut samples = (0..frames)
                    .map(|index| ((index as f32) * 0.013).sin() * 0.5)
                    .collect::<Vec<_>>();
                if let Some(last) = samples.last_mut() {
                    *last = 0.75;
                }
                let mut source = MemorySource { rate, samples };
                let mut resampled = resample_source_to_f32le(&mut source, &output).unwrap();
                let expected = (frames as u128 * crate::audio::TARGET_RATE_HZ as u128)
                    .div_ceil(rate as u128) as u64;
                assert_eq!(resampled.frames(), expected, "rate={rate}, frames={frames}");
                let tail_start = expected.saturating_sub(32);
                let tail = resampled.read_frames(tail_start, 32).unwrap();
                assert!(tail.iter().all(|sample| sample.is_finite()));
                assert!(tail.iter().any(|sample| sample.abs() > 1.0e-6));
                let _ = std::fs::remove_file(output);
            }
        }
    }

    #[test]
    fn raw_source_reads_exact_bounded_windows_without_full_collect() {
        let path = temp("raw");
        let mut sink = RawF32LeSink::create(path.clone()).unwrap();
        sink.append(&(0..20_000).map(|v| v as f32).collect::<Vec<_>>())
            .unwrap();
        sink.sync_all_verified().unwrap();
        let mut source = RawF32LeSource::open(&path, 192_000).unwrap();
        assert_eq!(source.frames(), 20_000);
        assert_eq!(
            source.read_frames(19_997, 120 * 192_000).unwrap(),
            vec![19_997.0, 19_998.0, 19_999.0]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn manual_clip_handle_survives_owner_drop_and_transitions_pending_to_ready() {
        let path = temp("manual-stable-handle");
        let mut sink = RawF32LeSink::create(path.clone()).unwrap();
        let reader = Arc::new(sink.file.try_clone().unwrap());
        let metadata = reader.metadata().unwrap();
        let durable_frames = Arc::new(AtomicU64::new(0));
        let finished = Arc::new(AtomicBool::new(false));
        let source = ManualClipSource {
            file: reader,
            durable_frames: Arc::clone(&durable_frames),
            spool_finished: Arc::clone(&finished),
            sample_rate: 16_000,
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        let samples = (0..8_000)
            .map(|index| ((index as f32) * 0.011).sin() * 0.25)
            .collect::<Vec<_>>();
        sink.append(&samples).unwrap();
        sink.sync_data_verified().unwrap();

        assert!(matches!(
            source.read_16k(0, samples.len()).unwrap(),
            ManualClipRead::Pending
        ));

        // Mimic Stop taking/dropping the ActiveRecording while the live thread retains its cloned
        // generation handle. Publishing the certified prefix is sufficient; no recorder lookup is
        // needed to read the exact command afterward.
        drop(sink);
        durable_frames.store(samples.len() as u64, Ordering::Release);
        match source.read_16k(0, samples.len()).unwrap() {
            ManualClipRead::Ready(ready) => assert_eq!(ready.len(), samples.len()),
            ManualClipRead::Pending => panic!("certified manual clip remained pending"),
        }

        // A terminal producer with a missing certified suffix is an honest error, never silence.
        durable_frames.store(0, Ordering::Release);
        finished.store(true, Ordering::Release);
        assert!(source.read_16k(0, samples.len()).is_err());

        drop(source);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn create_new_is_private_and_rejects_existing_or_symlink_targets() {
        let path = temp("nofollow");
        let sink = RawF32LeSink::create(path.clone()).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "plaintext recording transients must never depend only on the process umask"
        );
        assert!(RawF32LeSink::create(path.clone()).is_err());
        drop(sink);
        let _ = std::fs::remove_file(path);

        let target = temp("nofollow-target");
        let link = temp("nofollow-link");
        std::fs::write(&target, b"must-not-be-truncated").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(RawF32LeSink::create(link.clone()).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"must-not-be-truncated");
        let _ = std::fs::remove_file(link);
        let _ = std::fs::remove_file(target);
    }

    #[test]
    fn rf64_header_carries_exact_virtual_multigigabyte_lengths() {
        let frames = 4 * 60 * 60 * 192_000u64;
        assert!(needs_rf64(frames).unwrap());
        let header = rf64_float_header(frames, 192_000).unwrap();
        assert_eq!(&header[0..4], b"RF64");
        assert_eq!(
            u32::from_le_bytes(header[4..8].try_into().unwrap()),
            u32::MAX
        );
        assert_eq!(
            u64::from_le_bytes(header[20..28].try_into().unwrap()),
            frames * 4 + 72
        );
        assert_eq!(
            u64::from_le_bytes(header[28..36].try_into().unwrap()),
            frames * 4
        );
        assert_eq!(
            u64::from_le_bytes(header[36..44].try_into().unwrap()),
            frames
        );
        assert_eq!(&header[72..76], b"data");
        assert_eq!(
            u32::from_le_bytes(header[76..80].try_into().unwrap()),
            u32::MAX
        );
    }

    #[test]
    fn classic_riff_boundary_switches_before_u32_riff_size_overflow() {
        let max_classic_frames = (u32::MAX as u64 - (CLASSIC_FLOAT_WAV_HEADER_BYTES - 8)) / 4;
        assert!(!needs_rf64(max_classic_frames).unwrap());
        assert!(needs_rf64(max_classic_frames + 1).unwrap());
    }

    #[test]
    fn crash_recovery_verifies_prefix_before_truncating_uncommitted_tail() {
        let path = temp("recover-prefix");
        let committed = [1.0f32, 2.0, 3.0];
        let mut sink = RawF32LeSink::create(path.clone()).unwrap();
        sink.append(&committed).unwrap();
        sink.append(&[99.0, 100.0]).unwrap();
        sink.sync_all_verified().unwrap();
        drop(sink);
        let snapshot = recovery_snapshot(&path, &committed, RecordingGenerationState::Capturing);
        let verified = recover_exact_checkpoint(&path, &snapshot).unwrap();
        assert_eq!(verified.byte_len(), committed.len() as u64 * 4);
        let mut source = RawF32LeSource::open(&path, 48_000).unwrap();
        assert_eq!(source.read_frames(0, 99).unwrap(), committed);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn crash_recovery_hash_mismatch_preserves_the_whole_artifact() {
        let path = temp("recover-mismatch");
        let mut sink = RawF32LeSink::create(path.clone()).unwrap();
        sink.append(&[1.0f32, 2.0, 3.0, 4.0]).unwrap();
        sink.sync_all_verified().unwrap();
        drop(sink);
        let snapshot =
            recovery_snapshot(&path, &[8.0f32, 9.0], RecordingGenerationState::Capturing);
        let before = std::fs::metadata(&path).unwrap().len();
        assert!(recover_exact_checkpoint(&path, &snapshot).is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), before);
        let _ = std::fs::remove_file(path);
    }
}
