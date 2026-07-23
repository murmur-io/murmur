//! SQLCipher-backed recording-generation lifecycle ledger.
//!
//! This module stores only opaque ids, allowlisted fault codes, safe basenames, file identities,
//! lengths, digests, and timestamps. It never opens, reads, renames, unlinks, or deletes an audio
//! artifact. Filesystem verification is a later layer and must mint one of the private evidence
//! capabilities below; freely constructible row assertions are never accepted as verified proof.

#![allow(dead_code)] // Activated by the recording coordinator in the next bounded harness slice.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior};

use crate::error::{AppError, Result};
use crate::storage::db::{map_err, Db};
use crate::storage::models::{
    RecordingArtifactAssertion, RecordingArtifactRole, RecordingCaptureFault,
    RecordingCheckpointAssertion, RecordingGenerationKey, RecordingGenerationSnapshot,
    RecordingGenerationState, RecordingMicAssertion, RecordingRetirementReason,
};

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const MAX_LEASE_MS: i64 = 300_000;
pub(crate) const CLEANUP_MIC_RAW: u8 = 1 << 0;
pub(crate) const CLEANUP_SYSTEM_RAW: u8 = 1 << 1;
pub(crate) const CLEANUP_MIC_16K: u8 = 1 << 2;
pub(crate) const CLEANUP_SYSTEM_16K: u8 = 1 << 3;
pub(crate) const CLEANUP_PARTS: u8 = 1 << 4;

/// Affine ownership bearer. It is deliberately non-`Clone`, has no public parser/token accessor,
/// and never appears in an ordinary snapshot. Only prepare/recovery can mint one.
pub(crate) struct RecordingGenerationLease(String);

/// Cloneable renewal/alignment-only bearer. It cannot finalize, archive, retire, or delete a
/// generation; long-running workers use it solely to keep the affine owner's lease alive and to
/// persist the system-start anchor.
#[derive(Clone)]
pub(crate) struct RecordingGenerationHeartbeat(String);

impl std::fmt::Debug for RecordingGenerationLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RecordingGenerationLease([REDACTED])")
    }
}

impl RecordingGenerationLease {
    fn fresh() -> Self {
        Self(uuid::Uuid::new_v4().hyphenated().to_string())
    }

    fn token(&self) -> &str {
        &self.0
    }

    pub(crate) fn heartbeat(&self) -> RecordingGenerationHeartbeat {
        RecordingGenerationHeartbeat(self.0.clone())
    }
}

impl RecordingGenerationHeartbeat {
    fn token(&self) -> &str {
        &self.0
    }
}

/// Recovery result keeps observation and authority separate. Consuming `into_parts` transfers the
/// single non-clone lease to the recovery worker.
pub(crate) struct ClaimedRecordingGeneration {
    snapshot: RecordingGenerationSnapshot,
    lease: RecordingGenerationLease,
}

impl ClaimedRecordingGeneration {
    pub(crate) fn into_parts(self) -> (RecordingGenerationSnapshot, RecordingGenerationLease) {
        (self.snapshot, self.lease)
    }
}

/// Domain-bound capability minted only after an external verifier opened and read the canonical
/// `<generation>.mic.f32` artifact, checked its exact dev/inode, and hashed its durable prefix.
/// There is intentionally no production constructor in this storage-only slice.
pub(crate) struct VerifiedMicCheckpoint {
    key: RecordingGenerationKey,
    mic: RecordingMicAssertion,
    checkpoint: RecordingCheckpointAssertion,
}

impl VerifiedMicCheckpoint {
    /// Minted only from the stable-handle verifier in `audio::source`; freely constructed numeric
    /// assertions cannot cross this seam.
    pub(crate) fn from_file(
        key: &RecordingGenerationKey,
        mic: &RecordingMicAssertion,
        file: &crate::audio::source::VerifiedFile,
        durable_frames: u64,
    ) -> Result<Self> {
        if file.basename() != mic.basename()
            || file.device() != mic.device()
            || file.inode() != mic.inode()
            || file.byte_len() != durable_frames.saturating_mul(4)
        {
            return Err(AppError::Audio(
                "verified mic file does not match its prepared generation identity".into(),
            ));
        }
        Ok(Self {
            key: key.clone(),
            mic: mic.clone(),
            checkpoint: RecordingCheckpointAssertion::new(
                durable_frames,
                file.byte_len(),
                file.sha256(),
            )?,
        })
    }

    pub(crate) fn durable_frames(&self) -> u64 {
        self.checkpoint.durable_frames()
    }
}

/// Domain-bound capability minted only after a verifier opened and read the canonical
/// `<generation>.system.wav` artifact. The verifier must derive that basename from key + role and
/// reject identity aliasing with the mic artifact. There is no production constructor here.
pub(crate) struct VerifiedSystemArtifact {
    key: RecordingGenerationKey,
    artifact: RecordingArtifactAssertion,
}

impl VerifiedSystemArtifact {
    pub(crate) fn from_path(key: &RecordingGenerationKey, path: &std::path::Path) -> Result<Self> {
        let file = crate::audio::source::verify_existing_file(path)?;
        let expected = format!("{}.system.wav", key.generation_id());
        if file.basename() != expected {
            return Err(AppError::Audio(
                "system artifact basename is not canonical".into(),
            ));
        }
        Ok(Self {
            key: key.clone(),
            artifact: RecordingArtifactAssertion::for_generation(
                key,
                RecordingArtifactRole::System,
                file.device(),
                file.inode(),
                file.byte_len(),
                file.sha256(),
            )?,
        })
    }
}

/// Domain-bound capability minted only after a verifier opened and read the canonical
/// `<generation>.archive.wav` artifact. The verifier must derive that basename from key + role and
/// reject identity aliasing with either input artifact. The generation binding prevents replay.
pub(crate) struct VerifiedArchiveArtifact {
    key: RecordingGenerationKey,
    artifact: RecordingArtifactAssertion,
}

impl VerifiedArchiveArtifact {
    pub(crate) fn from_file(
        key: &RecordingGenerationKey,
        file: &crate::audio::source::VerifiedFile,
    ) -> Result<Self> {
        let expected = format!("{}.archive.wav", key.generation_id());
        if file.basename() != expected {
            return Err(AppError::Audio(
                "archive artifact basename is not canonical".into(),
            ));
        }
        Ok(Self {
            key: key.clone(),
            artifact: RecordingArtifactAssertion::for_generation(
                key,
                RecordingArtifactRole::Archive,
                file.device(),
                file.inode(),
                file.byte_len(),
                file.sha256(),
            )?,
        })
    }
}

/// Stronger-than-checkpoint capability: the verifier observed the exact PREPARED mic inode at zero
/// bytes and computed the empty digest. Only this capability permits the exceptional direct retire.
pub(crate) struct VerifiedEmptyMicArtifact {
    key: RecordingGenerationKey,
    mic: RecordingMicAssertion,
    byte_len: u64,
    sha256: String,
}

impl VerifiedEmptyMicArtifact {
    /// Minted only after the stable-handle audio verifier observed the exact prepared inode at the
    /// empty digest. This is the sole capability accepted by PREPARED -> EMPTY_ABANDONED.
    pub(crate) fn from_file(
        key: &RecordingGenerationKey,
        mic: &RecordingMicAssertion,
        file: &crate::audio::source::VerifiedFile,
    ) -> Result<Self> {
        if file.basename() != mic.basename()
            || file.device() != mic.device()
            || file.inode() != mic.inode()
            || file.byte_len() != 0
            || file.sha256() != EMPTY_SHA256
        {
            return Err(AppError::Audio(
                "prepared mic artifact is not the verified empty generation".into(),
            ));
        }
        Ok(Self {
            key: key.clone(),
            mic: mic.clone(),
            byte_len: file.byte_len(),
            sha256: file.sha256().to_owned(),
        })
    }
}

trait Clock {
    fn now_ms(&self) -> Result<i64>;
}

struct ProductionClock;

impl Clock for ProductionClock {
    fn now_ms(&self) -> Result<i64> {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AppError::Storage("system clock is before Unix epoch".into()))?;
        i64::try_from(elapsed.as_millis())
            .map_err(|_| AppError::Storage("system clock is outside supported range".into()))
    }
}

impl Db {
    pub(crate) fn migrate_recording_generations(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS recording_generations (
               meeting_id TEXT NOT NULL,
               generation_id TEXT NOT NULL,
               state TEXT NOT NULL,
               owner_token TEXT NOT NULL,
               lease_expires_at_ms INTEGER NOT NULL,
               mic_basename TEXT NOT NULL,
               sample_rate INTEGER NOT NULL,
               mic_device INTEGER NOT NULL,
               mic_inode INTEGER NOT NULL,
               durable_frames INTEGER NOT NULL DEFAULT 0,
               durable_byte_len INTEGER NOT NULL DEFAULT 0,
               durable_sha256_prefix TEXT NOT NULL,
               system_basename TEXT,
               system_device INTEGER,
               system_inode INTEGER,
               system_byte_len INTEGER,
               system_sha256 TEXT,
               capture_fault TEXT,
               archive_basename TEXT,
               archive_device INTEGER,
               archive_inode INTEGER,
               archive_byte_len INTEGER,
               archive_sha256 TEXT,
               retirement_reason TEXT,
               created_at_ms INTEGER NOT NULL,
               updated_at_ms INTEGER NOT NULL,
               finalized_at_ms INTEGER,
               archived_at_ms INTEGER,
               retired_at_ms INTEGER,
               system_start_offset_micros INTEGER,
               cleanup_mask INTEGER NOT NULL DEFAULT 0,
               recovery_blocked INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY (meeting_id, generation_id),
               FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE RESTRICT,
               CHECK (state IN ('PREPARED','CAPTURING','FINALIZED','ARCHIVED','RETIRED')),
               CHECK (capture_fault IS NULL OR capture_fault IN
                      ('MIC_IO','SYSTEM_IO','DISK_FULL','DEVICE_LOST','INTERRUPTED')),
               CHECK (retirement_reason IS NULL OR retirement_reason IN
                      ('ARCHIVED','EMPTY_ABANDONED')),
               CHECK (cleanup_mask BETWEEN 0 AND 31),
               CHECK (recovery_blocked IN (0, 1)),
               CHECK (length(meeting_id) = 36 AND meeting_id = lower(meeting_id)
                      AND substr(meeting_id,9,1) = '-' AND substr(meeting_id,14,1) = '-'
                      AND substr(meeting_id,19,1) = '-' AND substr(meeting_id,24,1) = '-'
                      AND length(replace(meeting_id,'-','')) = 32
                      AND replace(meeting_id,'-','') != '00000000000000000000000000000000'
                      AND replace(meeting_id,'-','') NOT GLOB '*[^0-9a-f]*'),
               CHECK (length(generation_id) = 36 AND generation_id = lower(generation_id)
                      AND substr(generation_id,9,1) = '-' AND substr(generation_id,14,1) = '-'
                      AND substr(generation_id,15,1) = '4' AND substr(generation_id,19,1) = '-'
                      AND substr(generation_id,20,1) IN ('8','9','a','b')
                      AND substr(generation_id,24,1) = '-'
                      AND length(replace(generation_id,'-','')) = 32
                      AND replace(generation_id,'-','') NOT GLOB '*[^0-9a-f]*'),
               CHECK (length(owner_token) = 36 AND owner_token = lower(owner_token)
                      AND substr(owner_token,9,1) = '-' AND substr(owner_token,14,1) = '-'
                      AND substr(owner_token,15,1) = '4' AND substr(owner_token,19,1) = '-'
                      AND substr(owner_token,20,1) IN ('8','9','a','b')
                      AND substr(owner_token,24,1) = '-'
                      AND length(replace(owner_token,'-','')) = 32
                      AND replace(owner_token,'-','') NOT GLOB '*[^0-9a-f]*'),
               CHECK (mic_basename = generation_id || '.mic.f32'),
               CHECK (sample_rate BETWEEN 8000 AND 384000),
               CHECK (mic_device > 0 AND mic_inode > 0),
               CHECK (durable_frames BETWEEN 0 AND 2305843009213693951
                      AND durable_byte_len = durable_frames * 4),
               CHECK (length(durable_sha256_prefix) = 64
                      AND durable_sha256_prefix = lower(durable_sha256_prefix)
                      AND durable_sha256_prefix NOT GLOB '*[^0-9a-f]*'),
               CHECK (durable_frames != 0 OR durable_sha256_prefix =
                      'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'),
               CHECK ((system_basename IS NULL AND system_device IS NULL
                       AND system_inode IS NULL AND system_byte_len IS NULL AND system_sha256 IS NULL)
                       OR (system_basename = generation_id || '.system.wav'
                       AND system_device > 0 AND system_inode > 0 AND system_byte_len > 0
                       AND (system_device != mic_device OR system_inode != mic_inode)
                       AND length(system_sha256) = 64 AND system_sha256 = lower(system_sha256)
                       AND system_sha256 NOT GLOB '*[^0-9a-f]*')),
               CHECK ((archive_basename IS NULL AND archive_device IS NULL
                       AND archive_inode IS NULL AND archive_byte_len IS NULL AND archive_sha256 IS NULL)
                   OR (archive_basename = generation_id || '.archive.wav'
                       AND archive_device > 0 AND archive_inode > 0 AND archive_byte_len > 0
                       AND (archive_device != mic_device OR archive_inode != mic_inode)
                       AND (system_device IS NULL OR archive_device != system_device
                            OR archive_inode != system_inode)
                       AND length(archive_sha256) = 64 AND archive_sha256 = lower(archive_sha256)
                       AND archive_sha256 NOT GLOB '*[^0-9a-f]*')),
               CHECK (created_at_ms >= 0 AND updated_at_ms >= created_at_ms
                      AND lease_expires_at_ms >= updated_at_ms),
               CHECK (finalized_at_ms IS NULL OR finalized_at_ms >= created_at_ms),
               CHECK (archived_at_ms IS NULL OR archived_at_ms >= finalized_at_ms),
               CHECK (retired_at_ms IS NULL OR
                      retired_at_ms >= COALESCE(archived_at_ms, created_at_ms)),
               CHECK ((state = 'PREPARED' AND durable_frames = 0
                       AND finalized_at_ms IS NULL AND archived_at_ms IS NULL AND retired_at_ms IS NULL
                       AND system_basename IS NULL AND capture_fault IS NULL
                       AND archive_basename IS NULL AND retirement_reason IS NULL)
                   OR (state = 'CAPTURING' AND finalized_at_ms IS NULL
                       AND archived_at_ms IS NULL AND retired_at_ms IS NULL
                       AND system_basename IS NULL AND capture_fault IS NULL
                       AND archive_basename IS NULL AND retirement_reason IS NULL)
                   OR (state = 'FINALIZED' AND finalized_at_ms IS NOT NULL
                       AND archived_at_ms IS NULL AND retired_at_ms IS NULL
                       AND archive_basename IS NULL AND retirement_reason IS NULL)
                   OR (state = 'ARCHIVED' AND finalized_at_ms IS NOT NULL
                       AND archived_at_ms IS NOT NULL AND retired_at_ms IS NULL
                       AND archive_basename IS NOT NULL AND retirement_reason IS NULL)
                   OR (state = 'RETIRED' AND retirement_reason = 'ARCHIVED'
                       AND finalized_at_ms IS NOT NULL AND archived_at_ms IS NOT NULL
                       AND retired_at_ms IS NOT NULL AND archive_basename IS NOT NULL)
                   OR (state = 'RETIRED' AND retirement_reason = 'EMPTY_ABANDONED'
                       AND durable_frames = 0 AND finalized_at_ms IS NULL AND archived_at_ms IS NULL
                       AND retired_at_ms IS NOT NULL AND system_basename IS NULL
                       AND capture_fault IS NULL AND archive_basename IS NULL))
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_recording_generations_one_active
               ON recording_generations(meeting_id) WHERE state != 'RETIRED';
             CREATE INDEX IF NOT EXISTS idx_recording_generations_stale
               ON recording_generations(lease_expires_at_ms, created_at_ms)
               WHERE state != 'RETIRED';
             CREATE TABLE IF NOT EXISTS legacy_recording_recovery (
               meeting_id TEXT PRIMARY KEY NOT NULL,
               created_at_ms INTEGER NOT NULL,
               FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE RESTRICT,
               CHECK (created_at_ms >= 0)
             );",
        )
        .map_err(map_err)?;
        // Additive upgrade for harness/dev databases that observed the earlier ledger shape.
        let (has_alignment, has_cleanup_mask, has_recovery_blocked) = {
            let mut statement = conn
                .prepare("PRAGMA table_info(recording_generations)")
                .map_err(map_err)?;
            let mut rows = statement.query([]).map_err(map_err)?;
            let mut found = false;
            let mut cleanup_found = false;
            let mut recovery_blocked_found = false;
            while let Some(row) = rows.next().map_err(map_err)? {
                let name: String = row.get(1).map_err(map_err)?;
                if name == "system_start_offset_micros" {
                    found = true;
                }
                if name == "cleanup_mask" {
                    cleanup_found = true;
                }
                if name == "recovery_blocked" {
                    recovery_blocked_found = true;
                }
            }
            (found, cleanup_found, recovery_blocked_found)
        };
        if !has_alignment {
            conn.execute(
                "ALTER TABLE recording_generations ADD COLUMN system_start_offset_micros INTEGER",
                [],
            )
            .map_err(map_err)?;
        }
        if !has_cleanup_mask {
            conn.execute(
                "ALTER TABLE recording_generations ADD COLUMN cleanup_mask INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(map_err)?;
        }
        if !has_recovery_blocked {
            conn.execute(
                "ALTER TABLE recording_generations ADD COLUMN recovery_blocked INTEGER NOT NULL DEFAULT 0 CHECK (recovery_blocked IN (0, 1))",
                [],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    pub(crate) fn prepare_recording_generation(
        &self,
        key: &RecordingGenerationKey,
        mic: &RecordingMicAssertion,
        lease_ms: i64,
    ) -> Result<RecordingGenerationLease> {
        self.prepare_recording_generation_with_clock(key, mic, lease_ms, &ProductionClock)
    }

    fn prepare_recording_generation_with_clock<C: Clock>(
        &self,
        key: &RecordingGenerationKey,
        mic: &RecordingMicAssertion,
        lease_ms: i64,
        clock: &C,
    ) -> Result<RecordingGenerationLease> {
        validate_lease_duration(lease_ms)?;
        let owner = RecordingGenerationLease::fresh();
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_err)?;
        let now_ms = clock.now_ms()?;
        let lease_expires_at_ms = lease_deadline(now_ms, lease_ms)?;
        let inserted = tx
            .execute(
                "INSERT INTO recording_generations
                   (meeting_id, generation_id, state, owner_token, lease_expires_at_ms,
                    mic_basename, sample_rate, mic_device, mic_inode, durable_frames,
                    durable_byte_len, durable_sha256_prefix, created_at_ms, updated_at_ms)
                 VALUES (?1,?2,'PREPARED',?3,?4,?5,?6,?7,?8,0,0,?9,?10,?10)",
                rusqlite::params![
                    key.meeting_id(),
                    key.generation_id(),
                    owner.token(),
                    lease_expires_at_ms,
                    mic.basename(),
                    mic.sample_rate() as i64,
                    mic.device() as i64,
                    mic.inode() as i64,
                    EMPTY_SHA256,
                    now_ms,
                ],
            )
            .map_err(map_err)?;
        affected_one(inserted, "prepare recording generation")?;
        tx.commit().map_err(map_err)?;
        Ok(owner)
    }

    pub(crate) fn begin_recording_capture(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationLease,
    ) -> Result<()> {
        self.begin_recording_capture_with_clock(key, owner, &ProductionClock)
    }

    /// Persist the wall-clock alignment anchor while the row is still CAPTURING. This happens
    /// immediately after the system helper is spawned, so a process crash before Stop can still
    /// recover the canonical system artifact without inventing equal start timestamps.
    pub(crate) fn set_recording_system_start_offset(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationHeartbeat,
        offset_micros: i64,
    ) -> Result<()> {
        const MAX_OFFSET_MICROS: i64 = 60 * 60 * 1_000_000;
        if offset_micros.unsigned_abs() > MAX_OFFSET_MICROS as u64 {
            return Err(AppError::InvalidArg(
                "recording stream alignment offset is out of range".into(),
            ));
        }
        let attempt = (|| -> Result<()> {
            let mut conn = self.lock();
            let tx = conn.transaction().map_err(map_err)?;
            let now_ms = ProductionClock.now_ms()?;
            let changed = tx
                .execute(
                    "UPDATE recording_generations
                        SET system_start_offset_micros=?5, updated_at_ms=?4
                      WHERE meeting_id=?1 AND generation_id=?2 AND owner_token=?3
                        AND state='CAPTURING' AND updated_at_ms<=?4 AND lease_expires_at_ms>?4",
                    rusqlite::params![
                        key.meeting_id(),
                        key.generation_id(),
                        owner.0.as_str(),
                        now_ms,
                        offset_micros,
                    ],
                )
                .map_err(map_err)?;
            affected_one(changed, "record system capture alignment")?;
            tx.commit().map_err(map_err)
        })();
        match attempt {
            Ok(()) => Ok(()),
            Err(error) => match self.get_recording_generation_snapshot(key) {
                Ok(Some(row))
                    if row.state == RecordingGenerationState::Capturing
                        && row.system_start_offset_micros == Some(offset_micros) =>
                {
                    Ok(())
                }
                Ok(Some(row))
                    if row.state == RecordingGenerationState::Capturing
                        && row.system_start_offset_micros.is_none() =>
                {
                    Err(error)
                }
                _ => Err(AppError::Storage(
                    "system capture alignment commit was ambiguous".into(),
                )),
            },
        }
    }

    /// Content-free durability witness for ARCHIVED cleanup. Status alone is insufficient because
    /// `Summarized` is written immediately before the note row; a crash in that narrow window must
    /// rerun post-processing, not destroy the sole raw sources. `Exported` is included because it
    /// is a later durable terminal status, but both statuses still require an actual note row.
    pub(crate) fn meeting_postprocess_is_durable(&self, meeting_id: &str) -> Result<bool> {
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM meetings m
                  WHERE m.id=?1 AND m.status IN ('SUMMARIZED', 'EXPORTED')
                    AND EXISTS (SELECT 1 FROM notes n WHERE n.meeting_id=m.id)
             )",
            rusqlite::params![meeting_id],
            |row| row.get(0),
        )
        .map_err(map_err)
    }

    fn begin_recording_capture_with_clock<C: Clock>(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationLease,
        clock: &C,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let now_ms = clock.now_ms()?;
        let changed = tx
            .execute(
                "UPDATE recording_generations SET state='CAPTURING', updated_at_ms=?4
                   WHERE meeting_id=?1 AND generation_id=?2 AND owner_token=?3
                     AND state='PREPARED' AND updated_at_ms<=?4 AND lease_expires_at_ms>?4",
                rusqlite::params![key.meeting_id(), key.generation_id(), owner.token(), now_ms],
            )
            .map_err(map_err)?;
        affected_one(changed, "begin recording capture")?;
        tx.commit().map_err(map_err)
    }

    pub(crate) fn checkpoint_recording_generation(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationLease,
        expected: &RecordingCheckpointAssertion,
        verified_next: &VerifiedMicCheckpoint,
    ) -> Result<()> {
        self.checkpoint_recording_generation_with_clock(
            key,
            owner,
            expected,
            verified_next,
            &ProductionClock,
        )
    }

    fn checkpoint_recording_generation_with_clock<C: Clock>(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationLease,
        expected: &RecordingCheckpointAssertion,
        verified_next: &VerifiedMicCheckpoint,
        clock: &C,
    ) -> Result<()> {
        require_key(key, &verified_next.key)?;
        if verified_next.checkpoint.durable_frames() <= expected.durable_frames() {
            return Err(AppError::InvalidArg(
                "recording checkpoint must advance durable frames".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let now_ms = clock.now_ms()?;
        let changed = tx
            .execute(
                "UPDATE recording_generations
                    SET durable_frames=?12, durable_byte_len=?13,
                        durable_sha256_prefix=?14, updated_at_ms=?4
                  WHERE meeting_id=?1 AND generation_id=?2 AND owner_token=?3
                    AND state='CAPTURING' AND updated_at_ms<=?4 AND lease_expires_at_ms>?4
                    AND durable_frames=?5 AND durable_byte_len=?6 AND durable_sha256_prefix=?7
                    AND mic_basename=?8 AND mic_device=?9 AND mic_inode=?10
                    AND sample_rate=?11",
                rusqlite::params![
                    key.meeting_id(),
                    key.generation_id(),
                    owner.token(),
                    now_ms,
                    expected.durable_frames() as i64,
                    expected.byte_len() as i64,
                    expected.sha256_prefix(),
                    verified_next.mic.basename(),
                    verified_next.mic.device() as i64,
                    verified_next.mic.inode() as i64,
                    verified_next.mic.sample_rate() as i64,
                    verified_next.checkpoint.durable_frames() as i64,
                    verified_next.checkpoint.byte_len() as i64,
                    verified_next.checkpoint.sha256_prefix(),
                ],
            )
            .map_err(map_err)?;
        affected_one(changed, "recording checkpoint proof CAS")?;
        tx.commit().map_err(map_err)
    }

    pub(crate) fn finalize_recording_generation(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationLease,
        verified_system: Option<&VerifiedSystemArtifact>,
        capture_fault: Option<RecordingCaptureFault>,
    ) -> Result<()> {
        self.finalize_recording_generation_with_clock(
            key,
            owner,
            verified_system,
            capture_fault,
            &ProductionClock,
        )
    }

    fn finalize_recording_generation_with_clock<C: Clock>(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationLease,
        verified_system: Option<&VerifiedSystemArtifact>,
        capture_fault: Option<RecordingCaptureFault>,
        clock: &C,
    ) -> Result<()> {
        if let Some(system) = verified_system {
            require_key(key, &system.key)?;
            require_artifact_role(&system.artifact, RecordingArtifactRole::System)?;
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let now_ms = clock.now_ms()?;
        let system = verified_system.map(|verified| &verified.artifact);
        let changed = tx
            .execute(
                "UPDATE recording_generations
                    SET state='FINALIZED', system_basename=?5, system_device=?6,
                        system_inode=?7, system_byte_len=?8, system_sha256=?9,
                        capture_fault=?10, finalized_at_ms=?4, updated_at_ms=?4
                  WHERE meeting_id=?1 AND generation_id=?2 AND owner_token=?3
                    AND state='CAPTURING' AND updated_at_ms<=?4 AND lease_expires_at_ms>?4
                    AND (?5 IS NULL OR mic_device!=?6 OR mic_inode!=?7)",
                rusqlite::params![
                    key.meeting_id(),
                    key.generation_id(),
                    owner.token(),
                    now_ms,
                    system.map(RecordingArtifactAssertion::basename),
                    system.map(|artifact| artifact.device() as i64),
                    system.map(|artifact| artifact.inode() as i64),
                    system.map(|artifact| artifact.byte_len() as i64),
                    system.map(RecordingArtifactAssertion::sha256),
                    capture_fault.map(RecordingCaptureFault::as_str),
                ],
            )
            .map_err(map_err)?;
        affected_one(changed, "finalize recording generation")?;
        tx.commit().map_err(map_err)
    }

    pub(crate) fn archive_recording_generation(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationLease,
        verified_archive: &VerifiedArchiveArtifact,
    ) -> Result<()> {
        self.archive_recording_generation_with_clock(key, owner, verified_archive, &ProductionClock)
    }

    fn archive_recording_generation_with_clock<C: Clock>(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationLease,
        verified_archive: &VerifiedArchiveArtifact,
        clock: &C,
    ) -> Result<()> {
        require_key(key, &verified_archive.key)?;
        let archive = &verified_archive.artifact;
        require_artifact_role(archive, RecordingArtifactRole::Archive)?;
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let now_ms = clock.now_ms()?;
        let changed = tx
            .execute(
                "UPDATE recording_generations
                    SET state='ARCHIVED', archive_basename=?5, archive_device=?6,
                        archive_inode=?7, archive_byte_len=?8, archive_sha256=?9,
                        archived_at_ms=?4, updated_at_ms=?4
                  WHERE meeting_id=?1 AND generation_id=?2 AND owner_token=?3
                    AND state='FINALIZED' AND updated_at_ms<=?4 AND lease_expires_at_ms>?4
                    AND (mic_device!=?6 OR mic_inode!=?7)
                    AND (system_device IS NULL OR system_device!=?6 OR system_inode!=?7)",
                rusqlite::params![
                    key.meeting_id(),
                    key.generation_id(),
                    owner.token(),
                    now_ms,
                    archive.basename(),
                    archive.device() as i64,
                    archive.inode() as i64,
                    archive.byte_len() as i64,
                    archive.sha256(),
                ],
            )
            .map_err(map_err)?;
        affected_one(changed, "archive recording generation")?;
        tx.commit().map_err(map_err)
    }

    pub(crate) fn retire_recording_generation(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationLease,
        exact_archive: &VerifiedArchiveArtifact,
    ) -> Result<()> {
        self.retire_recording_generation_with_clock(key, owner, exact_archive, &ProductionClock)
    }

    pub(crate) fn checkpoint_recording_cleanup(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationLease,
        expected_mask: u8,
        completed_bit: u8,
    ) -> Result<u8> {
        self.checkpoint_recording_cleanup_with_clock(
            key,
            owner,
            expected_mask,
            completed_bit,
            &ProductionClock,
        )
    }

    fn checkpoint_recording_cleanup_with_clock<C: Clock>(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationLease,
        expected_mask: u8,
        completed_bit: u8,
        clock: &C,
    ) -> Result<u8> {
        if completed_bit.count_ones() != 1 || completed_bit > CLEANUP_PARTS {
            return Err(AppError::InvalidArg(
                "recording cleanup checkpoint bit is invalid".into(),
            ));
        }
        let candidate = expected_mask | completed_bit;
        let attempt = (|| -> Result<()> {
            let mut conn = self.lock();
            let tx = conn.transaction().map_err(map_err)?;
            let now_ms = clock.now_ms()?;
            let changed = tx
                .execute(
                    "UPDATE recording_generations
                        SET cleanup_mask=?5, updated_at_ms=?4
                      WHERE meeting_id=?1 AND generation_id=?2 AND owner_token=?3
                        AND state='ARCHIVED' AND updated_at_ms<=?4 AND lease_expires_at_ms>?4
                        AND cleanup_mask=?6",
                    rusqlite::params![
                        key.meeting_id(),
                        key.generation_id(),
                        owner.token(),
                        now_ms,
                        candidate as i64,
                        expected_mask as i64,
                    ],
                )
                .map_err(map_err)?;
            affected_one(changed, "checkpoint recording cleanup")?;
            tx.commit().map_err(map_err)
        })();
        match attempt {
            Ok(()) => Ok(candidate),
            Err(error) => match self.get_recording_generation_snapshot(key) {
                Ok(Some(row))
                    if row.state == RecordingGenerationState::Archived
                        && row.cleanup_mask == candidate =>
                {
                    Ok(candidate)
                }
                Ok(Some(row))
                    if row.state == RecordingGenerationState::Archived
                        && row.cleanup_mask == expected_mask =>
                {
                    Err(error)
                }
                _ => Err(AppError::Storage(
                    "recording cleanup checkpoint commit was ambiguous".into(),
                )),
            },
        }
    }

    fn retire_recording_generation_with_clock<C: Clock>(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationLease,
        exact_archive: &VerifiedArchiveArtifact,
        clock: &C,
    ) -> Result<()> {
        require_key(key, &exact_archive.key)?;
        let archive = &exact_archive.artifact;
        require_artifact_role(archive, RecordingArtifactRole::Archive)?;
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let now_ms = clock.now_ms()?;
        let changed = tx
            .execute(
                "UPDATE recording_generations
                    SET state='RETIRED', retirement_reason='ARCHIVED',
                        retired_at_ms=?4, updated_at_ms=?4
                  WHERE meeting_id=?1 AND generation_id=?2 AND owner_token=?3
                    AND state='ARCHIVED' AND updated_at_ms<=?4 AND lease_expires_at_ms>?4
                    AND archive_basename=?5 AND archive_device=?6 AND archive_inode=?7
                    AND archive_byte_len=?8 AND archive_sha256=?9
                    AND cleanup_mask = CASE WHEN system_basename IS NULL THEN 21 ELSE 31 END",
                rusqlite::params![
                    key.meeting_id(),
                    key.generation_id(),
                    owner.token(),
                    now_ms,
                    archive.basename(),
                    archive.device() as i64,
                    archive.inode() as i64,
                    archive.byte_len() as i64,
                    archive.sha256(),
                ],
            )
            .map_err(map_err)?;
        affected_one(changed, "retire recording generation")?;
        tx.commit().map_err(map_err)
    }

    pub(crate) fn renew_recording_generation_lease(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationLease,
        expected_state: RecordingGenerationState,
        lease_ms: i64,
    ) -> Result<()> {
        self.renew_recording_generation_lease_with_clock(
            key,
            owner,
            expected_state,
            lease_ms,
            &ProductionClock,
        )
    }

    /// Cloneable, state-bound renewal used only by the long-running file-backed pipeline worker.
    /// It cannot transition lifecycle state or retire/delete anything.
    pub(crate) fn heartbeat_recording_generation_lease(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationHeartbeat,
        expected_state: RecordingGenerationState,
        lease_ms: i64,
    ) -> Result<()> {
        validate_non_retired(expected_state)?;
        validate_lease_duration(lease_ms)?;
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let now_ms = ProductionClock.now_ms()?;
        let deadline = lease_deadline(now_ms, lease_ms)?;
        let changed = tx
            .execute(
                "UPDATE recording_generations
                    SET lease_expires_at_ms=?5, updated_at_ms=?4
                  WHERE meeting_id=?1 AND generation_id=?2 AND owner_token=?3
                    AND state=?6 AND updated_at_ms<=?4 AND lease_expires_at_ms>?4
                    AND ?5>=lease_expires_at_ms",
                rusqlite::params![
                    key.meeting_id(),
                    key.generation_id(),
                    owner.0.as_str(),
                    now_ms,
                    deadline,
                    expected_state.as_str(),
                ],
            )
            .map_err(map_err)?;
        affected_one(changed, "heartbeat recording generation lease")?;
        tx.commit().map_err(map_err)
    }

    fn renew_recording_generation_lease_with_clock<C: Clock>(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationLease,
        expected_state: RecordingGenerationState,
        lease_ms: i64,
        clock: &C,
    ) -> Result<()> {
        validate_non_retired(expected_state)?;
        validate_lease_duration(lease_ms)?;
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let now_ms = clock.now_ms()?;
        let deadline = lease_deadline(now_ms, lease_ms)?;
        let changed = tx
            .execute(
                "UPDATE recording_generations
                    SET lease_expires_at_ms=?5, updated_at_ms=?4
                  WHERE meeting_id=?1 AND generation_id=?2 AND owner_token=?3
                    AND state=?6 AND updated_at_ms<=?4 AND lease_expires_at_ms>?4
                    AND ?5>=lease_expires_at_ms",
                rusqlite::params![
                    key.meeting_id(),
                    key.generation_id(),
                    owner.token(),
                    now_ms,
                    deadline,
                    expected_state.as_str(),
                ],
            )
            .map_err(map_err)?;
        affected_one(changed, "renew recording generation lease")?;
        tx.commit().map_err(map_err)
    }

    /// Voluntarily relinquish a live lease without mutating lifecycle state or artifact proof. The
    /// row becomes immediately recoverable; no stale worker can backdate because DB time is internal.
    pub(crate) fn release_recording_generation_lease(
        &self,
        key: &RecordingGenerationKey,
        owner: RecordingGenerationLease,
        expected_state: RecordingGenerationState,
    ) -> Result<()> {
        self.release_recording_generation_lease_with_clock(
            key,
            owner,
            expected_state,
            &ProductionClock,
        )
    }

    /// Emergency terminal-guard handoff when a panic destroyed the affine in-memory owner before
    /// it could call the ordinary owner-token CAS. Stop is single-flight and lifecycle-serialized;
    /// rotate the token and expire the one per-meeting nonterminal row without changing proofs.
    pub(crate) fn expire_recording_generation_after_owner_loss(
        &self,
        meeting_id: &str,
    ) -> Result<()> {
        let replacement = RecordingGenerationLease::fresh();
        let now_ms = ProductionClock.now_ms()?;
        let conn = self.lock();
        conn.execute(
            "UPDATE recording_generations
                SET owner_token=?3, lease_expires_at_ms=?2, updated_at_ms=?2
              WHERE meeting_id=?1 AND state!='RETIRED' AND updated_at_ms<=?2",
            rusqlite::params![meeting_id, now_ms, replacement.token()],
        )
        .map_err(map_err)?;
        Ok(())
    }

    fn release_recording_generation_lease_with_clock<C: Clock>(
        &self,
        key: &RecordingGenerationKey,
        owner: RecordingGenerationLease,
        expected_state: RecordingGenerationState,
        clock: &C,
    ) -> Result<()> {
        validate_non_retired(expected_state)?;
        let replacement_owner = RecordingGenerationLease::fresh();
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let now_ms = clock.now_ms()?;
        let changed = tx
            .execute(
                "UPDATE recording_generations
                    SET lease_expires_at_ms=?4, updated_at_ms=?4, owner_token=?6
                  WHERE meeting_id=?1 AND generation_id=?2 AND owner_token=?3
                    AND state=?5 AND updated_at_ms<=?4 AND lease_expires_at_ms>?4",
                rusqlite::params![
                    key.meeting_id(),
                    key.generation_id(),
                    owner.token(),
                    now_ms,
                    expected_state.as_str(),
                    replacement_owner.token(),
                ],
            )
            .map_err(map_err)?;
        affected_one(changed, "release recording generation lease")?;
        tx.commit().map_err(map_err)
    }

    /// Quarantine one ambiguous generation from the automatic oldest-first startup sweep without
    /// deleting, retiring, or hiding it from lock/delete governance. The affine recovery lease is
    /// rotated and expired immediately. A targeted user action may still claim this meeting;
    /// subsequent automatic passes keep skipping it so one malformed oldest row cannot
    /// starve every newer recoverable recording forever.
    pub(crate) fn quarantine_ambiguous_recording_generation(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationHeartbeat,
    ) -> Result<()> {
        self.quarantine_ambiguous_recording_generation_with_clock(key, owner, &ProductionClock)
    }

    fn quarantine_ambiguous_recording_generation_with_clock<C: Clock>(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationHeartbeat,
        clock: &C,
    ) -> Result<()> {
        let now_ms = clock.now_ms()?;
        let replacement = RecordingGenerationLease::fresh();
        let conn = self.lock();
        let changed = conn
            .execute(
                "UPDATE recording_generations
                    SET recovery_blocked=1, owner_token=?5,
                        lease_expires_at_ms=?4, updated_at_ms=?4
                  WHERE meeting_id=?1 AND generation_id=?2 AND owner_token=?3
                    AND state!='RETIRED' AND updated_at_ms<=?4",
                rusqlite::params![
                    key.meeting_id(),
                    key.generation_id(),
                    owner.token(),
                    now_ms,
                    replacement.token(),
                ],
            )
            .map_err(map_err)?;
        affected_one(changed, "quarantine ambiguous recording generation")
    }

    pub(crate) fn claim_oldest_stale_recording_generation(
        &self,
        lease_ms: i64,
    ) -> Result<Option<ClaimedRecordingGeneration>> {
        self.claim_oldest_stale_recording_generation_with_clock(lease_ms, &ProductionClock)
    }

    /// Claim the expired nonterminal generation for one meeting. Interactive Retry/Delete/Lock
    /// preflights use this targeted twin of startup recovery so a released cleanup owner can be
    /// resumed immediately in the same process without stealing unrelated work.
    pub(crate) fn claim_stale_recording_generation_for_meeting(
        &self,
        meeting_id: &str,
        lease_ms: i64,
    ) -> Result<Option<ClaimedRecordingGeneration>> {
        validate_lease_duration(lease_ms)?;
        let new_owner = RecordingGenerationLease::fresh();
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_err)?;
        let now_ms = ProductionClock.now_ms()?;
        let deadline = lease_deadline(now_ms, lease_ms)?;
        let candidate: Option<(String, String, String, i64)> = tx
            .query_row(
                "SELECT generation_id, owner_token, state, lease_expires_at_ms
                   FROM recording_generations
                  WHERE meeting_id=?1 AND state!='RETIRED' AND lease_expires_at_ms<=?2
                  ORDER BY created_at_ms ASC, generation_id ASC LIMIT 1",
                rusqlite::params![meeting_id, now_ms],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(map_err)?;
        let Some((generation_id, old_owner, old_state, old_lease)) = candidate else {
            tx.commit().map_err(map_err)?;
            return Ok(None);
        };
        let changed = tx
            .execute(
                "UPDATE recording_generations
                    SET owner_token=?6, lease_expires_at_ms=?7, updated_at_ms=?5
                  WHERE meeting_id=?1 AND generation_id=?2 AND owner_token=?3 AND state=?4
                    AND lease_expires_at_ms=?8 AND lease_expires_at_ms<=?5",
                rusqlite::params![
                    meeting_id,
                    generation_id,
                    old_owner,
                    old_state,
                    now_ms,
                    new_owner.token(),
                    deadline,
                    old_lease,
                ],
            )
            .map_err(map_err)?;
        affected_one(changed, "claim meeting recording generation")?;
        let snapshot = tx
            .query_row(
                &format!("{SELECT_ROW} WHERE meeting_id=?1 AND generation_id=?2"),
                rusqlite::params![meeting_id, generation_id],
                row_to_recording_generation,
            )
            .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(Some(ClaimedRecordingGeneration {
            snapshot,
            lease: new_owner,
        }))
    }

    pub(crate) fn nonterminal_recording_meetings_in_folder(
        &self,
        folder_id: &str,
    ) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut statement = conn
            .prepare(
                "SELECT meeting_id FROM (
                     SELECT rg.meeting_id AS meeting_id
                       FROM recording_generations rg
                       JOIN notes n ON n.meeting_id=rg.meeting_id
                      WHERE n.folder_id=?1 AND rg.state!='RETIRED'
                     UNION
                     SELECT lr.meeting_id AS meeting_id
                       FROM legacy_recording_recovery lr
                       JOIN notes n ON n.meeting_id=lr.meeting_id
                      WHERE n.folder_id=?1
                 ) ORDER BY meeting_id",
            )
            .map_err(map_err)?;
        let rows = statement
            .query_map([folder_id], |row| row.get::<_, String>(0))
            .map_err(map_err)?;
        let mut meetings = Vec::new();
        for row in rows {
            meetings.push(row.map_err(map_err)?);
        }
        Ok(meetings)
    }

    fn claim_oldest_stale_recording_generation_with_clock<C: Clock>(
        &self,
        lease_ms: i64,
        clock: &C,
    ) -> Result<Option<ClaimedRecordingGeneration>> {
        validate_lease_duration(lease_ms)?;
        let new_owner = RecordingGenerationLease::fresh();
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_err)?;
        let now_ms = clock.now_ms()?;
        let deadline = lease_deadline(now_ms, lease_ms)?;
        let candidate: Option<(String, String, String, String, i64)> = tx
            .query_row(
                "SELECT rg.meeting_id, rg.generation_id, rg.owner_token, rg.state,
                        rg.lease_expires_at_ms
                   FROM recording_generations rg
                  WHERE rg.state!='RETIRED'
                    AND rg.recovery_blocked=0
                    AND rg.lease_expires_at_ms<=?1
                    AND EXISTS(SELECT 1 FROM meetings m WHERE m.id=rg.meeting_id)
                    AND NOT EXISTS(
                        SELECT 1
                          FROM notes n
                          JOIN folders f ON f.id=n.folder_id
                         WHERE n.meeting_id=rg.meeting_id AND f.locked=1
                    )
                  ORDER BY rg.created_at_ms ASC, rg.generation_id ASC LIMIT 1",
                [now_ms],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(map_err)?;
        let Some((meeting_id, generation_id, old_owner, old_state, old_lease)) = candidate else {
            tx.commit().map_err(map_err)?;
            return Ok(None);
        };
        let changed = tx
            .execute(
                "UPDATE recording_generations
                    SET owner_token=?6, lease_expires_at_ms=?7, updated_at_ms=?5
                  WHERE meeting_id=?1 AND generation_id=?2 AND owner_token=?3 AND state=?4
                    AND lease_expires_at_ms=?8 AND lease_expires_at_ms<=?5",
                rusqlite::params![
                    meeting_id,
                    generation_id,
                    old_owner,
                    old_state,
                    now_ms,
                    new_owner.token(),
                    deadline,
                    old_lease,
                ],
            )
            .map_err(map_err)?;
        affected_one(changed, "claim stale recording generation")?;
        let snapshot = tx
            .query_row(
                &format!("{SELECT_ROW} WHERE meeting_id=?1 AND generation_id=?2"),
                rusqlite::params![meeting_id, generation_id],
                row_to_recording_generation,
            )
            .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(Some(ClaimedRecordingGeneration {
            snapshot,
            lease: new_owner,
        }))
    }

    /// Preserve an empty PREPARED attempt as an auditable RETIRED row. No file is unlinked. The
    /// evidence must be generation-bound and exactly match the stored mic dev/inode plus zero bytes
    /// and the empty digest.
    pub(crate) fn abandon_empty_prepared_recording_generation(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationLease,
        verified_empty: &VerifiedEmptyMicArtifact,
    ) -> Result<()> {
        self.abandon_empty_prepared_recording_generation_with_clock(
            key,
            owner,
            verified_empty,
            &ProductionClock,
        )
    }

    fn abandon_empty_prepared_recording_generation_with_clock<C: Clock>(
        &self,
        key: &RecordingGenerationKey,
        owner: &RecordingGenerationLease,
        verified_empty: &VerifiedEmptyMicArtifact,
        clock: &C,
    ) -> Result<()> {
        require_key(key, &verified_empty.key)?;
        if verified_empty.byte_len != 0 || verified_empty.sha256 != EMPTY_SHA256 {
            return Err(AppError::InvalidArg(
                "empty abandonment requires verified zero-byte evidence".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let now_ms = clock.now_ms()?;
        let changed = tx
            .execute(
                "UPDATE recording_generations
                    SET state='RETIRED', retirement_reason='EMPTY_ABANDONED',
                        retired_at_ms=?4, updated_at_ms=?4, lease_expires_at_ms=?4
                  WHERE meeting_id=?1 AND generation_id=?2 AND owner_token=?3
                    AND state='PREPARED' AND updated_at_ms<=?4 AND lease_expires_at_ms>?4
                    AND mic_basename=?5 AND mic_device=?6 AND mic_inode=?7 AND sample_rate=?8
                    AND durable_frames=0 AND durable_byte_len=0 AND durable_sha256_prefix=?9",
                rusqlite::params![
                    key.meeting_id(),
                    key.generation_id(),
                    owner.token(),
                    now_ms,
                    verified_empty.mic.basename(),
                    verified_empty.mic.device() as i64,
                    verified_empty.mic.inode() as i64,
                    verified_empty.mic.sample_rate() as i64,
                    EMPTY_SHA256,
                ],
            )
            .map_err(map_err)?;
        affected_one(changed, "abandon empty prepared recording generation")?;
        tx.commit().map_err(map_err)
    }

    pub(crate) fn get_recording_generation_snapshot(
        &self,
        key: &RecordingGenerationKey,
    ) -> Result<Option<RecordingGenerationSnapshot>> {
        let conn = self.lock();
        conn.query_row(
            &format!("{SELECT_ROW} WHERE meeting_id=?1 AND generation_id=?2"),
            rusqlite::params![key.meeting_id(), key.generation_id()],
            row_to_recording_generation,
        )
        .optional()
        .map_err(map_err)
    }

    /// Content-free preflight for callers that are about to remove meeting-owned files. A `true`
    /// result means deletion must stop before the first unlink. This storage API cannot prove the
    /// command/file ordering; `delete_meeting` repeats the guard transactionally as a backstop.
    pub(crate) fn meeting_has_nonterminal_recording_generation(
        &self,
        meeting_id: &str,
    ) -> Result<bool> {
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM recording_generations
                  WHERE meeting_id=?1 AND state!='RETIRED'
             )",
            [meeting_id],
            |row| row.get(0),
        )
        .map_err(map_err)
    }

    /// Fail-closed at-rest lock gate for automatic recording recovery. Startup has no
    /// session-unlocked folders, so a generation may be claimed/read only when its meeting exists
    /// and is not currently attached to a locked folder. The claim query repeats this predicate in
    /// the same transaction; callers repeat this helper immediately before opening any artifact.
    pub(crate) fn recording_recovery_owner_is_open(&self, meeting_id: &str) -> Result<bool> {
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM meetings WHERE id=?1)
                    AND NOT EXISTS(
                        SELECT 1
                          FROM notes n
                          JOIN folders f ON f.id=n.folder_id
                         WHERE n.meeting_id=?1 AND f.locked=1
                    )",
            [meeting_id],
            |row| row.get(0),
        )
        .map_err(map_err)
    }

    /// Transactionally publish durable ownership of legacy crash-recovery artifacts before startup
    /// moves or reads any of them. Presence is the only state: the marker remains pending until
    /// proof-bound cleanup succeeds or every referenced artifact is proven absent. Returns whether
    /// the meeting is currently associated with an at-rest locked folder in this same snapshot.
    pub(crate) fn mark_legacy_recording_recovery_pending(&self, meeting_id: &str) -> Result<bool> {
        let now_ms = ProductionClock.now_ms()?;
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_err)?;
        // A historical sidecar may outlive its meeting row. In that DiscardOrphan case the sidecar
        // itself is the cleanup journal and is removed last; conditionally skip the FK-bound marker
        // instead of turning `INSERT OR IGNORE` into an unsuppressed FK failure.
        tx.execute(
            "INSERT OR IGNORE INTO legacy_recording_recovery (meeting_id, created_at_ms)
             SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM meetings WHERE id=?1)",
            rusqlite::params![meeting_id, now_ms],
        )
        .map_err(map_err)?;
        let locked: bool = tx
            .query_row(
                "SELECT EXISTS(
                     SELECT 1
                       FROM notes n
                       JOIN folders f ON f.id=n.folder_id
                      WHERE n.meeting_id=?1 AND f.locked=1
                 )",
                [meeting_id],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(locked)
    }

    pub(crate) fn clear_legacy_recording_recovery_pending(&self, meeting_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM legacy_recording_recovery WHERE meeting_id=?1",
            [meeting_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub(crate) fn meeting_has_pending_legacy_recording_recovery(
        &self,
        meeting_id: &str,
    ) -> Result<bool> {
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM legacy_recording_recovery WHERE meeting_id=?1
             )",
            [meeting_id],
            |row| row.get(0),
        )
        .map_err(map_err)
    }

    /// Content-free startup inventory for repairing the one unavoidable cross-store crash cut:
    /// every legacy source + sidecar was durably removed, but the SQLCipher ownership marker was
    /// not cleared yet. Callers may clear an id only after proving every exact expected pathname is
    /// absent; a marker with any surviving/ambiguous artifact remains fail-closed.
    pub(crate) fn pending_legacy_recording_recovery_ids(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut statement = conn
            .prepare("SELECT meeting_id FROM legacy_recording_recovery ORDER BY meeting_id ASC")
            .map_err(map_err)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(map_err)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_err)
    }

    /// Unified mutation preflight. Legacy crash artifacts are governed exactly like a non-retired
    /// generation even though their historical filenames cannot safely be adopted into that ledger.
    pub(crate) fn meeting_has_recording_recovery_ownership(
        &self,
        meeting_id: &str,
    ) -> Result<bool> {
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM recording_generations
                  WHERE meeting_id=?1 AND state!='RETIRED'
                 UNION ALL
                 SELECT 1 FROM legacy_recording_recovery WHERE meeting_id=?1
             )",
            [meeting_id],
            |row| row.get(0),
        )
        .map_err(map_err)
    }

    /// Seal preflight: raw/transient recording bytes remain outside the managed audio seal until
    /// their generation retires or legacy recovery clears, so either ownership form blocks lock.
    pub(crate) fn folder_has_nonterminal_recording_generation(
        &self,
        folder_id: &str,
    ) -> Result<bool> {
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(
                 SELECT 1
                   FROM notes n
                  WHERE n.folder_id=?1 AND (
                        EXISTS(
                            SELECT 1 FROM recording_generations rg
                             WHERE rg.meeting_id=n.meeting_id AND rg.state!='RETIRED'
                        ) OR EXISTS(
                            SELECT 1 FROM legacy_recording_recovery lr
                             WHERE lr.meeting_id=n.meeting_id
                        )
                  )
             )",
            [folder_id],
            |row| row.get(0),
        )
        .map_err(map_err)
    }

    pub(crate) fn refuse_nonterminal_recording_generation_tx(
        tx: &Transaction<'_>,
        meeting_id: &str,
    ) -> Result<()> {
        let active: bool = tx
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM recording_generations
                      WHERE meeting_id=?1 AND state!='RETIRED'
                     UNION ALL
                     SELECT 1 FROM legacy_recording_recovery WHERE meeting_id=?1
                 )",
                [meeting_id],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        if active {
            return Err(AppError::Storage(
                "meeting has pending recording recovery ownership".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn purge_retired_recording_generations_tx(
        tx: &Transaction<'_>,
        meeting_id: &str,
    ) -> Result<()> {
        tx.execute(
            "DELETE FROM recording_generations
              WHERE meeting_id=?1 AND state='RETIRED'",
            [meeting_id],
        )
        .map_err(map_err)?;
        Ok(())
    }
}

const SELECT_ROW: &str = "SELECT meeting_id, generation_id, state,
    lease_expires_at_ms, mic_basename, sample_rate, mic_device, mic_inode, durable_frames,
    durable_byte_len, durable_sha256_prefix, system_basename, system_device, system_inode,
    system_byte_len, system_sha256, capture_fault, archive_basename, archive_device,
    archive_inode, archive_byte_len, archive_sha256, retirement_reason, created_at_ms,
    updated_at_ms, finalized_at_ms, archived_at_ms, retired_at_ms,
    system_start_offset_micros, cleanup_mask FROM recording_generations";

fn row_to_recording_generation(row: &Row<'_>) -> rusqlite::Result<RecordingGenerationSnapshot> {
    let model = (|| -> Result<RecordingGenerationSnapshot> {
        let meeting_id: String = row.get(0).map_err(map_err)?;
        let generation_id: String = row.get(1).map_err(map_err)?;
        let key = RecordingGenerationKey::new(&meeting_id, &generation_id)?;
        let state: String = row.get(2).map_err(map_err)?;
        let mic = mic_from_columns(row, &key)?;
        let system = artifact_from_columns(row, 11, &key, RecordingArtifactRole::System)?;
        let archive = artifact_from_columns(row, 17, &key, RecordingArtifactRole::Archive)?;
        let fault: Option<String> = row.get(16).map_err(map_err)?;
        let retirement: Option<String> = row.get(22).map_err(map_err)?;
        Ok(RecordingGenerationSnapshot {
            key,
            state: RecordingGenerationState::parse(&state)?,
            lease_expires_at_ms: row.get(3).map_err(map_err)?,
            mic,
            checkpoint: RecordingCheckpointAssertion::new(
                nonnegative_u64(row.get(8).map_err(map_err)?, "durable frames")?,
                nonnegative_u64(row.get(9).map_err(map_err)?, "durable byte length")?,
                &row.get::<_, String>(10).map_err(map_err)?,
            )?,
            system_artifact: system,
            capture_fault: fault
                .as_deref()
                .map(RecordingCaptureFault::parse)
                .transpose()?,
            archive,
            retirement_reason: retirement
                .as_deref()
                .map(RecordingRetirementReason::parse)
                .transpose()?,
            created_at_ms: row.get(23).map_err(map_err)?,
            updated_at_ms: row.get(24).map_err(map_err)?,
            finalized_at_ms: row.get(25).map_err(map_err)?,
            archived_at_ms: row.get(26).map_err(map_err)?,
            retired_at_ms: row.get(27).map_err(map_err)?,
            system_start_offset_micros: row.get(28).map_err(map_err)?,
            cleanup_mask: u8::try_from(nonnegative_u64(
                row.get(29).map_err(map_err)?,
                "cleanup mask",
            )?)
            .map_err(|_| AppError::Storage("invalid cleanup mask".into()))?,
        })
    })();
    model.map_err(to_from_sql_error)
}

fn mic_from_columns(row: &Row<'_>, key: &RecordingGenerationKey) -> Result<RecordingMicAssertion> {
    let stored_basename: String = row.get(4).map_err(map_err)?;
    let assertion = RecordingMicAssertion::for_generation(
        key,
        u32::try_from(nonnegative_u64(
            row.get(5).map_err(map_err)?,
            "sample rate",
        )?)
        .map_err(|_| AppError::Storage("invalid stored sample rate".into()))?,
        nonnegative_u64(row.get(6).map_err(map_err)?, "mic device")?,
        nonnegative_u64(row.get(7).map_err(map_err)?, "mic inode")?,
    )?;
    if assertion.basename() != stored_basename {
        return Err(AppError::Storage(
            "non-canonical stored mic basename".into(),
        ));
    }
    Ok(assertion)
}

fn artifact_from_columns(
    row: &Row<'_>,
    start: usize,
    key: &RecordingGenerationKey,
    role: RecordingArtifactRole,
) -> Result<Option<RecordingArtifactAssertion>> {
    let basename: Option<String> = row.get(start).map_err(map_err)?;
    let device: Option<i64> = row.get(start + 1).map_err(map_err)?;
    let inode: Option<i64> = row.get(start + 2).map_err(map_err)?;
    let byte_len: Option<i64> = row.get(start + 3).map_err(map_err)?;
    let sha256: Option<String> = row.get(start + 4).map_err(map_err)?;
    match (basename, device, inode, byte_len, sha256) {
        (Some(basename), Some(device), Some(inode), Some(byte_len), Some(sha256)) => {
            let assertion = RecordingArtifactAssertion::for_generation(
                key,
                role,
                nonnegative_u64(device, "artifact device")?,
                nonnegative_u64(inode, "artifact inode")?,
                nonnegative_u64(byte_len, "artifact byte length")?,
                &sha256,
            )?;
            if assertion.basename() != basename {
                return Err(AppError::Storage(
                    "non-canonical stored recording artifact basename".into(),
                ));
            }
            Ok(Some(assertion))
        }
        (None, None, None, None, None) => Ok(None),
        _ => Err(AppError::Storage(
            "partial recording artifact identity".into(),
        )),
    }
}

fn to_from_sql_error(error: AppError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn nonnegative_u64(value: i64, label: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| AppError::Storage(format!("invalid stored {label}")))
}

fn require_key(actual: &RecordingGenerationKey, evidence: &RecordingGenerationKey) -> Result<()> {
    if actual == evidence {
        Ok(())
    } else {
        Err(AppError::InvalidArg(
            "recording evidence belongs to a different generation".into(),
        ))
    }
}

fn require_artifact_role(
    artifact: &RecordingArtifactAssertion,
    expected: RecordingArtifactRole,
) -> Result<()> {
    if artifact.role() == expected {
        Ok(())
    } else {
        Err(AppError::InvalidArg(
            "recording evidence has the wrong artifact role".into(),
        ))
    }
}

fn validate_non_retired(state: RecordingGenerationState) -> Result<()> {
    if state == RecordingGenerationState::Retired {
        Err(AppError::InvalidArg(
            "retired recording generation has no lease".into(),
        ))
    } else {
        Ok(())
    }
}

fn validate_lease_duration(lease_ms: i64) -> Result<()> {
    if !(1..=MAX_LEASE_MS).contains(&lease_ms) {
        return Err(AppError::InvalidArg(
            "invalid recording lease duration".into(),
        ));
    }
    Ok(())
}

fn lease_deadline(now_ms: i64, lease_ms: i64) -> Result<i64> {
    now_ms
        .checked_add(lease_ms)
        .ok_or_else(|| AppError::Storage("recording lease deadline overflow".into()))
}

fn affected_one(count: usize, operation: &str) -> Result<()> {
    if count == 1 {
        Ok(())
    } else {
        Err(AppError::Storage(format!(
            "{operation} lost ownership, lease, state, identity, or proof CAS"
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::storage::models::{Folder, Meeting, MeetingStatus, NoteRecord};

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    // Published SHA-256 vectors for explicit byte strings. These tests exercise capability-bound
    // DB CAS, not filesystem hashing or cryptographic verification.
    const SHA_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const SHA_HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    const SHA_FOUR_ZERO_BYTES: &str =
        "df3f619804a92fdb4057192dc43dd748ea778adc52bc498ce80524c014b81119";
    const SHA_EIGHT_ZERO_BYTES: &str =
        "af5570f5a1810b7af78caf4bc70a660f0df51e42baf91d4de5b2328de0e83dfc";

    struct TestClock(Cell<i64>);

    impl TestClock {
        fn at(now_ms: i64) -> Self {
            Self(Cell::new(now_ms))
        }

        fn set(&self, now_ms: i64) {
            self.0.set(now_ms);
        }
    }

    impl Clock for TestClock {
        fn now_ms(&self) -> Result<i64> {
            Ok(self.0.get())
        }
    }

    fn db() -> Db {
        Db::open_with_key(std::path::Path::new(":memory:"), TEST_DEK).unwrap()
    }

    fn seed_meeting(db: &Db) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        db.insert_meeting(&Meeting {
            id: id.clone(),
            started_at: "2026-07-22T12:00:00Z".into(),
            ended_at: None,
            title: None,
            duration_s: 0,
            audio_path: None,
            status: MeetingStatus::Recording,
            folder_id: None,
        })
        .unwrap();
        id
    }

    fn mic(key: &RecordingGenerationKey) -> RecordingMicAssertion {
        RecordingMicAssertion::for_generation(key, 48_000, 7, 11).unwrap()
    }

    fn prepare(
        db: &Db,
        clock: &TestClock,
        meeting: &str,
    ) -> (RecordingGenerationKey, RecordingGenerationLease) {
        let key = RecordingGenerationKey::fresh(meeting).unwrap();
        let mic = mic(&key);
        let owner = db
            .prepare_recording_generation_with_clock(&key, &mic, 100, clock)
            .unwrap();
        (key, owner)
    }

    // Test-only simulation of the external verifier issuing a capability. No production arbitrary
    // constructor exists, and these helpers make no claim that storage itself verified bytes.
    fn simulated_mic_evidence(
        key: &RecordingGenerationKey,
        mic: &RecordingMicAssertion,
        frames: u64,
        digest: &str,
    ) -> VerifiedMicCheckpoint {
        VerifiedMicCheckpoint {
            key: key.clone(),
            mic: mic.clone(),
            checkpoint: RecordingCheckpointAssertion::new(frames, frames * 4, digest).unwrap(),
        }
    }

    fn simulated_system_evidence(key: &RecordingGenerationKey) -> VerifiedSystemArtifact {
        VerifiedSystemArtifact {
            key: key.clone(),
            artifact: RecordingArtifactAssertion::for_generation(
                key,
                RecordingArtifactRole::System,
                7,
                12,
                3,
                SHA_ABC,
            )
            .unwrap(),
        }
    }

    fn simulated_archive_evidence(
        key: &RecordingGenerationKey,
        inode: u64,
    ) -> VerifiedArchiveArtifact {
        VerifiedArchiveArtifact {
            key: key.clone(),
            artifact: RecordingArtifactAssertion::for_generation(
                key,
                RecordingArtifactRole::Archive,
                7,
                inode,
                5,
                SHA_HELLO,
            )
            .unwrap(),
        }
    }

    fn simulated_empty_evidence(
        key: &RecordingGenerationKey,
        mic: &RecordingMicAssertion,
    ) -> VerifiedEmptyMicArtifact {
        VerifiedEmptyMicArtifact {
            key: key.clone(),
            mic: mic.clone(),
            byte_len: 0,
            sha256: EMPTY_SHA256.into(),
        }
    }

    #[test]
    fn legal_transitions_require_domain_and_generation_bound_evidence() {
        let db = db();
        let clock = TestClock::at(10);
        let meeting = seed_meeting(&db);
        let (key, owner) = prepare(&db, &clock, &meeting);
        clock.set(11);
        db.begin_recording_capture_with_clock(&key, &owner, &clock)
            .unwrap();
        let empty = RecordingCheckpointAssertion::new(0, 0, EMPTY_SHA256).unwrap();
        let durable = simulated_mic_evidence(&key, &mic(&key), 1, SHA_FOUR_ZERO_BYTES);
        clock.set(12);
        db.checkpoint_recording_generation_with_clock(&key, &owner, &empty, &durable, &clock)
            .unwrap();
        clock.set(13);
        db.finalize_recording_generation_with_clock(
            &key,
            &owner,
            Some(&simulated_system_evidence(&key)),
            None,
            &clock,
        )
        .unwrap();
        let archive = simulated_archive_evidence(&key, 13);
        clock.set(14);
        db.archive_recording_generation_with_clock(&key, &owner, &archive, &clock)
            .unwrap();
        let mut mask = 0;
        for bit in [
            CLEANUP_MIC_RAW,
            CLEANUP_SYSTEM_RAW,
            CLEANUP_MIC_16K,
            CLEANUP_SYSTEM_16K,
            CLEANUP_PARTS,
        ] {
            mask = db
                .checkpoint_recording_cleanup_with_clock(&key, &owner, mask, bit, &clock)
                .unwrap();
        }
        assert_eq!(mask, 31);
        clock.set(15);
        db.retire_recording_generation_with_clock(&key, &owner, &archive, &clock)
            .unwrap();
        let row = db.get_recording_generation_snapshot(&key).unwrap().unwrap();
        assert_eq!(row.state, RecordingGenerationState::Retired);
        assert_eq!(
            row.retirement_reason,
            Some(RecordingRetirementReason::Archived)
        );
        let system = row.system_artifact.unwrap();
        assert_eq!(
            (system.device(), system.inode(), system.byte_len()),
            (7, 12, 3)
        );
        let stored_archive = row.archive.unwrap();
        assert_eq!(
            (
                stored_archive.device(),
                stored_archive.inode(),
                stored_archive.byte_len(),
            ),
            (7, 13, 5)
        );
        clock.set(16);
        let next_key = RecordingGenerationKey::fresh(&meeting).unwrap();
        assert!(db
            .prepare_recording_generation_with_clock(&next_key, &mic(&next_key), 100, &clock)
            .is_ok());
    }

    #[test]
    fn illegal_skip_and_competing_owner_are_rejected() {
        let db = db();
        let clock = TestClock::at(10);
        let meeting = seed_meeting(&db);
        let (key, owner) = prepare(&db, &clock, &meeting);
        clock.set(11);
        assert!(db
            .finalize_recording_generation_with_clock(&key, &owner, None, None, &clock)
            .is_err());
        let archive = simulated_archive_evidence(&key, 13);
        assert!(db
            .archive_recording_generation_with_clock(&key, &owner, &archive, &clock)
            .is_err());
        assert!(db
            .retire_recording_generation_with_clock(&key, &owner, &archive, &clock)
            .is_err());
        assert!(db
            .begin_recording_capture_with_clock(&key, &RecordingGenerationLease::fresh(), &clock,)
            .is_err());
    }

    #[test]
    fn capturing_alignment_is_durable_before_stop_for_both_offset_signs() {
        let db = db();
        for offset_micros in [37_500, -22_250] {
            let meeting = seed_meeting(&db);
            let key = RecordingGenerationKey::fresh(&meeting).unwrap();
            let owner = db
                .prepare_recording_generation(&key, &mic(&key), 120_000)
                .unwrap();
            db.begin_recording_capture(&key, &owner).unwrap();
            db.set_recording_system_start_offset(&key, &owner.heartbeat(), offset_micros)
                .unwrap();

            // Simulate a crash before Stop by reading only the CAPTURING ledger snapshot. Recovery
            // can reconstruct a signed system Instant without Stop ever having reasserted it.
            let snapshot = db.get_recording_generation_snapshot(&key).unwrap().unwrap();
            assert_eq!(snapshot.state, RecordingGenerationState::Capturing);
            assert_eq!(snapshot.system_start_offset_micros, Some(offset_micros));
        }
    }

    #[test]
    fn postprocess_cleanup_witness_requires_terminal_status_and_note_row() {
        let db = db();
        let meeting = seed_meeting(&db);
        db.update_meeting_status(&meeting, MeetingStatus::Summarized)
            .unwrap();
        assert!(!db.meeting_postprocess_is_durable(&meeting).unwrap());

        db.upsert_note(&NoteRecord {
            meeting_id: meeting.clone(),
            provider_id: "test".into(),
            markdown: "durable".into(),
            created_at: "2026-07-22T12:01:00Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        assert!(db.meeting_postprocess_is_durable(&meeting).unwrap());
        db.update_meeting_status(&meeting, MeetingStatus::Exported)
            .unwrap();
        assert!(db.meeting_postprocess_is_durable(&meeting).unwrap());
        db.update_meeting_status(&meeting, MeetingStatus::Error)
            .unwrap();
        assert!(!db.meeting_postprocess_is_durable(&meeting).unwrap());
    }

    #[test]
    fn stale_recovery_uses_internal_clock_and_claims_oldest_prepared() {
        let db = db();
        let clock = TestClock::at(5);
        let oldest_meeting = seed_meeting(&db);
        let stale_meeting = seed_meeting(&db);
        let fresh_meeting = seed_meeting(&db);
        let (oldest_key, oldest_owner) = prepare(&db, &clock, &oldest_meeting);
        clock.set(10);
        let (_stale_key, _) = prepare(&db, &clock, &stale_meeting);
        clock.set(50);
        let (_fresh_key, _) = prepare(&db, &clock, &fresh_meeting);
        clock.set(106);
        let claimed = db
            .claim_oldest_stale_recording_generation_with_clock(100, &clock)
            .unwrap()
            .unwrap();
        let (snapshot, claimed_lease) = claimed.into_parts();
        assert_eq!(snapshot.key, oldest_key);
        assert_eq!(snapshot.state, RecordingGenerationState::Prepared);
        assert!(db
            .begin_recording_capture_with_clock(&oldest_key, &oldest_owner, &clock)
            .is_err());
        db.begin_recording_capture_with_clock(&oldest_key, &claimed_lease, &clock)
            .unwrap();
        assert!(db
            .claim_oldest_stale_recording_generation_with_clock(100, &clock)
            .unwrap()
            .is_none());
    }

    #[test]
    fn automatic_stale_claim_skips_locked_owner_until_folder_opens() {
        let db = db();
        let clock = TestClock::at(5);
        let locked_meeting = seed_meeting(&db);
        let folder_id = uuid::Uuid::new_v4().to_string();
        db.insert_folder(&Folder {
            id: folder_id.clone(),
            name: "Private".into(),
            path: "Private".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-22T12:00:00Z".into(),
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: locked_meeting.clone(),
            provider_id: "test".into(),
            markdown: "durable".into(),
            created_at: "2026-07-22T12:00:01Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder(&locked_meeting, Some(&folder_id))
            .unwrap();
        let (locked_key, _) = prepare(&db, &clock, &locked_meeting);

        clock.set(10);
        let open_meeting = seed_meeting(&db);
        let (open_key, _) = prepare(&db, &clock, &open_meeting);
        db.set_folder_locked(&folder_id, true, Some(b"wrapped"))
            .unwrap();
        clock.set(111);

        assert!(!db
            .recording_recovery_owner_is_open(&locked_meeting)
            .unwrap());
        let first = db
            .claim_oldest_stale_recording_generation_with_clock(100, &clock)
            .unwrap()
            .unwrap();
        assert_eq!(first.into_parts().0.key, open_key);
        assert!(db
            .claim_oldest_stale_recording_generation_with_clock(100, &clock)
            .unwrap()
            .is_none());
        assert_eq!(
            db.get_recording_generation_snapshot(&locked_key)
                .unwrap()
                .unwrap()
                .state,
            RecordingGenerationState::Prepared
        );

        db.set_folder_locked(&folder_id, false, None).unwrap();
        assert!(db
            .recording_recovery_owner_is_open(&locked_meeting)
            .unwrap());
        let after_unlock = db
            .claim_oldest_stale_recording_generation_with_clock(100, &clock)
            .unwrap()
            .unwrap();
        assert_eq!(after_unlock.into_parts().0.key, locked_key);
    }

    #[test]
    fn quarantined_oldest_generation_cannot_starve_newer_startup_recovery() {
        let db = db();
        let clock = TestClock::at(5);
        let oldest_meeting = seed_meeting(&db);
        let newer_meeting = seed_meeting(&db);
        let (oldest_key, _) = prepare(&db, &clock, &oldest_meeting);
        clock.set(10);
        let (newer_key, _) = prepare(&db, &clock, &newer_meeting);
        clock.set(111);

        let oldest = db
            .claim_oldest_stale_recording_generation_with_clock(100, &clock)
            .unwrap()
            .unwrap();
        let (snapshot, lease) = oldest.into_parts();
        assert_eq!(snapshot.key, oldest_key);
        db.quarantine_ambiguous_recording_generation_with_clock(
            &oldest_key,
            &lease.heartbeat(),
            &clock,
        )
        .unwrap();

        let next = db
            .claim_oldest_stale_recording_generation_with_clock(100, &clock)
            .unwrap()
            .unwrap();
        assert_eq!(next.into_parts().0.key, newer_key);
        assert!(
            db.claim_oldest_stale_recording_generation_with_clock(100, &clock)
                .unwrap()
                .is_none(),
            "automatic recovery must skip the quarantined oldest row"
        );

        let targeted = db
            .claim_stale_recording_generation_for_meeting(&oldest_meeting, 100)
            .unwrap()
            .expect("an explicit meeting-scoped recovery may still inspect the quarantined row");
        assert_eq!(targeted.into_parts().0.key, oldest_key);
    }

    #[test]
    fn fresh_lease_cannot_be_stolen_or_backdated_by_a_worker() {
        let db = db();
        let clock = TestClock::at(100);
        let meeting = seed_meeting(&db);
        let (key, owner) = prepare(&db, &clock, &meeting);
        clock.set(150);
        assert!(db
            .claim_oldest_stale_recording_generation_with_clock(100, &clock)
            .unwrap()
            .is_none());
        db.begin_recording_capture_with_clock(&key, &owner, &clock)
            .unwrap();
    }

    #[test]
    fn lease_renewal_and_release_require_exact_owner_and_state() {
        let db = db();
        let clock = TestClock::at(10);
        let meeting = seed_meeting(&db);
        let (key, owner) = prepare(&db, &clock, &meeting);
        clock.set(20);
        assert!(db
            .renew_recording_generation_lease_with_clock(
                &key,
                &owner,
                RecordingGenerationState::Capturing,
                200,
                &clock,
            )
            .is_err());
        db.renew_recording_generation_lease_with_clock(
            &key,
            &owner,
            RecordingGenerationState::Prepared,
            200,
            &clock,
        )
        .unwrap();
        clock.set(21);
        db.release_recording_generation_lease_with_clock(
            &key,
            owner,
            RecordingGenerationState::Prepared,
            &clock,
        )
        .unwrap();
        assert_eq!(
            db.get_recording_generation_snapshot(&key)
                .unwrap()
                .unwrap()
                .state,
            RecordingGenerationState::Prepared
        );
        let claimed = db
            .claim_oldest_stale_recording_generation_with_clock(100, &clock)
            .unwrap();
        assert!(claimed.is_some());
    }

    #[test]
    fn checkpoint_rejects_backwards_stale_digest_and_wrong_generation() {
        let db = db();
        let clock = TestClock::at(10);
        let meeting = seed_meeting(&db);
        let other_meeting = seed_meeting(&db);
        let (key, owner) = prepare(&db, &clock, &meeting);
        let other_key = RecordingGenerationKey::fresh(&other_meeting).unwrap();
        clock.set(11);
        db.begin_recording_capture_with_clock(&key, &owner, &clock)
            .unwrap();
        let empty = RecordingCheckpointAssertion::new(0, 0, EMPTY_SHA256).unwrap();
        let first = simulated_mic_evidence(&key, &mic(&key), 1, SHA_FOUR_ZERO_BYTES);
        clock.set(12);
        db.checkpoint_recording_generation_with_clock(&key, &owner, &empty, &first, &clock)
            .unwrap();
        let stale = RecordingCheckpointAssertion::new(1, 4, SHA_ABC).unwrap();
        let next = simulated_mic_evidence(&key, &mic(&key), 2, SHA_EIGHT_ZERO_BYTES);
        clock.set(13);
        assert!(db
            .checkpoint_recording_generation_with_clock(&key, &owner, &stale, &next, &clock)
            .is_err());
        let wrong_generation =
            simulated_mic_evidence(&other_key, &mic(&other_key), 2, SHA_EIGHT_ZERO_BYTES);
        assert!(db
            .checkpoint_recording_generation_with_clock(
                &key,
                &owner,
                &first.checkpoint,
                &wrong_generation,
                &clock,
            )
            .is_err());
    }

    #[test]
    fn retirement_cas_requires_exact_archive_dev_ino_length_and_hash() {
        let db = db();
        let clock = TestClock::at(10);
        let meeting = seed_meeting(&db);
        let (key, owner) = prepare(&db, &clock, &meeting);
        clock.set(11);
        db.begin_recording_capture_with_clock(&key, &owner, &clock)
            .unwrap();
        clock.set(12);
        db.finalize_recording_generation_with_clock(&key, &owner, None, None, &clock)
            .unwrap();
        let archive = simulated_archive_evidence(&key, 13);
        clock.set(13);
        db.archive_recording_generation_with_clock(&key, &owner, &archive, &clock)
            .unwrap();
        let wrong_inode = simulated_archive_evidence(&key, 99);
        clock.set(14);
        assert!(db
            .retire_recording_generation_with_clock(&key, &owner, &wrong_inode, &clock)
            .is_err());
        assert!(db
            .retire_recording_generation_with_clock(&key, &owner, &archive, &clock)
            .is_err());
        let mut mask = 0;
        for bit in [CLEANUP_MIC_RAW, CLEANUP_MIC_16K, CLEANUP_PARTS] {
            mask = db
                .checkpoint_recording_cleanup_with_clock(&key, &owner, mask, bit, &clock)
                .unwrap();
            let persisted = db.get_recording_generation_snapshot(&key).unwrap().unwrap();
            assert_eq!(persisted.cleanup_mask, mask);
            if mask != 21 {
                assert!(db
                    .retire_recording_generation_with_clock(&key, &owner, &archive, &clock)
                    .is_err());
            }
        }
        assert_eq!(mask, 21);
        db.retire_recording_generation_with_clock(&key, &owner, &archive, &clock)
            .unwrap();
    }

    #[test]
    fn empty_prepared_abandon_retires_without_deleting_ledger() {
        let db = db();
        let clock = TestClock::at(10);
        let meeting = seed_meeting(&db);
        let (key, owner) = prepare(&db, &clock, &meeting);
        let evidence = simulated_empty_evidence(&key, &mic(&key));
        clock.set(11);
        db.abandon_empty_prepared_recording_generation_with_clock(&key, &owner, &evidence, &clock)
            .unwrap();
        let row = db.get_recording_generation_snapshot(&key).unwrap().unwrap();
        assert_eq!(row.state, RecordingGenerationState::Retired);
        assert_eq!(
            row.retirement_reason,
            Some(RecordingRetirementReason::EmptyAbandoned)
        );
    }

    #[test]
    fn empty_abandon_rejects_wrong_inode_and_non_prepared_state() {
        let db = db();
        let clock = TestClock::at(10);
        let meeting = seed_meeting(&db);
        let (key, owner) = prepare(&db, &clock, &meeting);
        let wrong_mic = RecordingMicAssertion::for_generation(&key, 48_000, 7, 99).unwrap();
        let wrong = simulated_empty_evidence(&key, &wrong_mic);
        clock.set(11);
        assert!(db
            .abandon_empty_prepared_recording_generation_with_clock(&key, &owner, &wrong, &clock,)
            .is_err());
        db.begin_recording_capture_with_clock(&key, &owner, &clock)
            .unwrap();
        let exact = simulated_empty_evidence(&key, &mic(&key));
        assert!(db
            .abandon_empty_prepared_recording_generation_with_clock(&key, &owner, &exact, &clock,)
            .is_err());
    }

    #[test]
    fn foreign_key_and_active_generation_uniqueness_are_enforced() {
        let db = db();
        let clock = TestClock::at(10);
        let missing = uuid::Uuid::new_v4().to_string();
        let missing_key = RecordingGenerationKey::fresh(&missing).unwrap();
        assert!(db
            .prepare_recording_generation_with_clock(&missing_key, &mic(&missing_key), 100, &clock)
            .is_err());
        let meeting = seed_meeting(&db);
        let (_key, _owner) = prepare(&db, &clock, &meeting);
        let competing = RecordingGenerationKey::fresh(&meeting).unwrap();
        assert!(db
            .prepare_recording_generation_with_clock(&competing, &mic(&competing), 100, &clock)
            .is_err());
    }

    #[test]
    fn delete_meeting_refuses_nonterminal_generation_before_db_mutation() {
        let db = db();
        let clock = TestClock::at(10);
        let meeting = seed_meeting(&db);
        let (key, _owner) = prepare(&db, &clock, &meeting);

        assert!(db
            .meeting_has_nonterminal_recording_generation(&meeting)
            .unwrap());
        assert!(db.delete_meeting(&meeting).is_err());
        assert_eq!(
            db.get_recording_generation_snapshot(&key)
                .unwrap()
                .unwrap()
                .state,
            RecordingGenerationState::Prepared
        );
        let conn = db.lock();
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM meetings WHERE id=?1)",
                [&meeting],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists);
        // Storage-only proof: command-side preflight-before-unlink ordering is not exercised here.
    }

    #[test]
    fn folder_seal_preflight_detects_nonterminal_generation() {
        let db = db();
        let clock = TestClock::at(10);
        let meeting = seed_meeting(&db);
        let folder_id = uuid::Uuid::new_v4().to_string();
        db.insert_folder(&Folder {
            id: folder_id.clone(),
            name: "Private".into(),
            path: "Private".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-22T12:00:00Z".into(),
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: meeting.clone(),
            provider_id: "test".into(),
            markdown: "durable".into(),
            created_at: "2026-07-22T12:00:01Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder(&meeting, Some(&folder_id)).unwrap();
        assert!(!db
            .folder_has_nonterminal_recording_generation(&folder_id)
            .unwrap());
        let (_key, _owner) = prepare(&db, &clock, &meeting);
        assert!(db
            .folder_has_nonterminal_recording_generation(&folder_id)
            .unwrap());
        assert_eq!(
            db.nonterminal_recording_meetings_in_folder(&folder_id)
                .unwrap(),
            vec![meeting]
        );
    }

    #[test]
    fn folder_seal_preflight_detects_pending_legacy_recovery() {
        let db = db();
        let meeting = seed_meeting(&db);
        let folder_id = uuid::Uuid::new_v4().to_string();
        db.insert_folder(&Folder {
            id: folder_id.clone(),
            name: "Private".into(),
            path: "Private".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-22T12:00:00Z".into(),
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: meeting.clone(),
            provider_id: "test".into(),
            markdown: "durable".into(),
            created_at: "2026-07-22T12:00:01Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder(&meeting, Some(&folder_id)).unwrap();

        assert!(!db.mark_legacy_recording_recovery_pending(&meeting).unwrap());

        assert!(db
            .folder_has_nonterminal_recording_generation(&folder_id)
            .unwrap());
        assert_eq!(
            db.nonterminal_recording_meetings_in_folder(&folder_id)
                .unwrap(),
            vec![meeting]
        );
    }

    #[test]
    fn locked_folder_assignment_refuses_nonterminal_generation_transactionally() {
        let db = db();
        let clock = TestClock::at(10);
        let meeting = seed_meeting(&db);
        let folder_id = uuid::Uuid::new_v4().to_string();
        db.insert_folder(&Folder {
            id: folder_id.clone(),
            name: "Private".into(),
            path: "Private".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-22T12:00:00Z".into(),
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: meeting.clone(),
            provider_id: "test".into(),
            markdown: "durable".into(),
            created_at: "2026-07-22T12:00:01Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        let (_key, _owner) = prepare(&db, &clock, &meeting);
        db.set_folder_locked(&folder_id, true, Some(b"wrapped"))
            .unwrap();

        assert!(matches!(
            db.set_meeting_folder(&meeting, Some(&folder_id)),
            Err(AppError::Locked(_))
        ));
        assert_eq!(db.folder_for_meeting(&meeting).unwrap(), None);
    }

    #[test]
    fn locked_folder_assignment_refuses_pending_legacy_recovery_transactionally() {
        let db = db();
        let meeting = seed_meeting(&db);
        let folder_id = uuid::Uuid::new_v4().to_string();
        db.insert_folder(&Folder {
            id: folder_id.clone(),
            name: "Private".into(),
            path: "Private".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-22T12:00:00Z".into(),
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: meeting.clone(),
            provider_id: "test".into(),
            markdown: "durable".into(),
            created_at: "2026-07-22T12:00:01Z".into(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.mark_legacy_recording_recovery_pending(&meeting).unwrap();
        db.set_folder_locked(&folder_id, true, Some(b"wrapped"))
            .unwrap();

        assert!(matches!(
            db.set_meeting_folder(&meeting, Some(&folder_id)),
            Err(AppError::Locked(_))
        ));
        assert_eq!(db.folder_for_meeting(&meeting).unwrap(), None);
    }

    #[test]
    fn delete_meeting_refuses_pending_legacy_recovery() {
        let db = db();
        let meeting = seed_meeting(&db);
        db.mark_legacy_recording_recovery_pending(&meeting).unwrap();

        assert!(db.delete_meeting(&meeting).is_err());
        assert!(db
            .meeting_has_pending_legacy_recording_recovery(&meeting)
            .unwrap());
        assert!(db.get_meeting(&meeting).unwrap().is_some());
    }

    #[test]
    fn delete_meeting_purges_retired_generation_then_deletes_meeting() {
        let db = db();
        let clock = TestClock::at(10);
        let meeting = seed_meeting(&db);
        let (key, owner) = prepare(&db, &clock, &meeting);
        let evidence = simulated_empty_evidence(&key, &mic(&key));
        clock.set(11);
        db.abandon_empty_prepared_recording_generation_with_clock(&key, &owner, &evidence, &clock)
            .unwrap();

        assert!(!db
            .meeting_has_nonterminal_recording_generation(&meeting)
            .unwrap());
        db.delete_meeting(&meeting).unwrap();
        assert!(db
            .get_recording_generation_snapshot(&key)
            .unwrap()
            .is_none());
        let conn = db.lock();
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM meetings WHERE id=?1)",
                [&meeting],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!exists);
    }

    #[test]
    fn canonical_basenames_and_distinct_artifact_identities_are_enforced() {
        let db = db();
        let clock = TestClock::at(10);
        let meeting = seed_meeting(&db);
        let (key, owner) = prepare(&db, &clock, &meeting);
        assert_eq!(
            mic(&key).basename(),
            format!("{}.mic.f32", key.generation_id())
        );
        assert_eq!(
            simulated_system_evidence(&key).artifact.basename(),
            format!("{}.system.wav", key.generation_id())
        );
        assert_eq!(
            simulated_archive_evidence(&key, 13).artifact.basename(),
            format!("{}.archive.wav", key.generation_id())
        );
        {
            let conn = db.lock();
            assert!(conn
                .execute(
                    "UPDATE recording_generations SET mic_basename='alice-private.mic.f32'
                      WHERE meeting_id=?1 AND generation_id=?2",
                    rusqlite::params![key.meeting_id(), key.generation_id()],
                )
                .is_err());
        }
        clock.set(11);
        db.begin_recording_capture_with_clock(&key, &owner, &clock)
            .unwrap();
        let wrong_role = VerifiedSystemArtifact {
            key: key.clone(),
            artifact: RecordingArtifactAssertion::for_generation(
                &key,
                RecordingArtifactRole::Archive,
                7,
                12,
                3,
                SHA_ABC,
            )
            .unwrap(),
        };
        clock.set(12);
        assert!(
            db.finalize_recording_generation_with_clock(
                &key,
                &owner,
                Some(&wrong_role),
                None,
                &clock,
            )
            .is_err()
        );
        let aliased_system = VerifiedSystemArtifact {
            key: key.clone(),
            artifact: RecordingArtifactAssertion::for_generation(
                &key,
                RecordingArtifactRole::System,
                7,
                11,
                3,
                SHA_ABC,
            )
            .unwrap(),
        };
        clock.set(13);
        assert!(db
            .finalize_recording_generation_with_clock(
                &key,
                &owner,
                Some(&aliased_system),
                None,
                &clock,
            )
            .is_err());
        clock.set(14);
        db.finalize_recording_generation_with_clock(
            &key,
            &owner,
            Some(&simulated_system_evidence(&key)),
            None,
            &clock,
        )
        .unwrap();
        let archive_aliases_mic = VerifiedArchiveArtifact {
            key: key.clone(),
            artifact: RecordingArtifactAssertion::for_generation(
                &key,
                RecordingArtifactRole::Archive,
                7,
                11,
                5,
                SHA_HELLO,
            )
            .unwrap(),
        };
        let archive_aliases_system = VerifiedArchiveArtifact {
            key: key.clone(),
            artifact: RecordingArtifactAssertion::for_generation(
                &key,
                RecordingArtifactRole::Archive,
                7,
                12,
                5,
                SHA_HELLO,
            )
            .unwrap(),
        };
        clock.set(15);
        assert!(db
            .archive_recording_generation_with_clock(&key, &owner, &archive_aliases_mic, &clock,)
            .is_err());
        assert!(db
            .archive_recording_generation_with_clock(&key, &owner, &archive_aliases_system, &clock,)
            .is_err());
    }

    #[test]
    fn malformed_identities_hashes_and_fault_codes_are_rejected() {
        assert!(
            RecordingGenerationKey::new("not-a-uuid", &uuid::Uuid::new_v4().to_string()).is_err()
        );
        assert!(RecordingCheckpointAssertion::new(1, 4, "ABC").is_err());
        assert!(RecordingCaptureFault::parse("/private/tmp/mic failed").is_err());
    }
}
