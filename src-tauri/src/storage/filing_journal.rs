//! Durable SQLCipher authority for plaintext projections created while filing a finalized bundle.
//!
//! The filesystem cannot participate in the canonical SQLite transaction. Every app-created temp
//! or final inode is therefore reserved here before creation and identity-bound before its first
//! plaintext byte. Startup and lock/relock drain these rows before claiming the owner is sealed.

use rusqlite::{Connection, OptionalExtension};

use crate::error::{AppError, Result};
use crate::storage::db::{map_err, Db};

#[cfg(test)]
static FAIL_NEXT_ATTACHMENT_RESTORE_PROMOTION: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
pub(crate) fn fail_next_attachment_restore_promotion() {
    FAIL_NEXT_ATTACHMENT_RESTORE_PROMOTION.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[derive(Debug, Clone)]
pub(crate) struct FilingProjectionReservation<'a> {
    pub(crate) attempt_id: &'a str,
    pub(crate) projection_id: &'a str,
    pub(crate) operation_kind: &'a str,
    pub(crate) owner_kind: &'a str,
    pub(crate) owner_id: &'a str,
    pub(crate) provider_id: &'a str,
    pub(crate) source_folder_id: &'a str,
    pub(crate) target_folder_id: &'a str,
    pub(crate) source_path: Option<&'a str>,
    pub(crate) temp_path: &'a str,
    pub(crate) final_path: Option<&'a str>,
    pub(crate) expected_len: u64,
    pub(crate) expected_sha256: &'a [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FilingProjectionJournalRow {
    pub(crate) attempt_id: String,
    pub(crate) projection_id: String,
    pub(crate) operation_kind: String,
    pub(crate) owner_kind: String,
    pub(crate) owner_id: String,
    pub(crate) provider_id: String,
    pub(crate) source_folder_id: String,
    pub(crate) target_folder_id: String,
    pub(crate) source_path: Option<String>,
    pub(crate) temp_path: String,
    pub(crate) final_path: Option<String>,
    pub(crate) expected_len: u64,
    pub(crate) expected_sha256: [u8; 32],
    pub(crate) device: Option<u64>,
    pub(crate) inode: Option<u64>,
    pub(crate) phase: String,
}

#[derive(Debug, Clone)]
pub(crate) struct FilingSourceReservation<'a> {
    pub(crate) attempt_id: &'a str,
    pub(crate) source_id: &'a str,
    /// Exact raw-open protection domain that governed this source at capture time. Empty means
    /// Unfiled; it is never inferred from the vault path during recovery.
    pub(crate) source_folder_id: &'a str,
    pub(crate) path: &'a str,
    pub(crate) bytes: &'a [u8],
    pub(crate) permissions_mode: u32,
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) parent_device: u64,
    pub(crate) parent_inode: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct FilingSourceJournalRow {
    pub(crate) attempt_id: String,
    pub(crate) source_id: String,
    pub(crate) source_folder_id: String,
    pub(crate) path: String,
    pub(crate) bytes: Vec<u8>,
    pub(crate) permissions_mode: u32,
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) parent_device: u64,
    pub(crate) parent_inode: u64,
    pub(crate) phase: String,
}

fn parse_identity(value: Option<String>, field: &str) -> Result<Option<u64>> {
    value
        .map(|raw| {
            raw.parse::<u64>().map_err(|_| {
                AppError::Storage(format!("invalid filing projection {field} identity"))
            })
        })
        .transpose()
}

fn digest_array(bytes: Vec<u8>) -> Result<[u8; 32]> {
    bytes
        .try_into()
        .map_err(|_| AppError::Storage("invalid filing projection digest length".into()))
}

fn digest_hex(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl Db {
    pub(crate) fn migrate_filing_projection_journal(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS filing_projection_attempts (
               attempt_id TEXT PRIMARY KEY,
               operation_kind TEXT NOT NULL,
               meeting_id TEXT NOT NULL,
               source_folder_id TEXT NOT NULL DEFAULT '',
               target_folder_id TEXT NOT NULL DEFAULT '',
               companion_id TEXT,
               phase TEXT NOT NULL CHECK(phase IN ('prepared'))
             );
             CREATE TABLE IF NOT EXISTS filing_projection_journal (
               attempt_id TEXT NOT NULL,
               projection_id TEXT NOT NULL,
               operation_kind TEXT NOT NULL,
               owner_kind TEXT NOT NULL CHECK(owner_kind IN ('meeting_note','document','attachment')),
               owner_id TEXT NOT NULL,
               provider_id TEXT NOT NULL DEFAULT '',
               source_folder_id TEXT NOT NULL DEFAULT '',
               target_folder_id TEXT NOT NULL DEFAULT '',
               source_path TEXT,
               temp_path TEXT NOT NULL,
               final_path TEXT,
               expected_len INTEGER NOT NULL CHECK(expected_len >= 0),
               expected_sha256 BLOB NOT NULL CHECK(length(expected_sha256) = 32),
               device TEXT,
               inode TEXT,
               phase TEXT NOT NULL CHECK(phase IN ('reserved','bound','publish_reserved','published','conflict','keep_external')),
               PRIMARY KEY(attempt_id, projection_id)
             );
             CREATE TABLE IF NOT EXISTS filing_projection_sources (
               attempt_id TEXT NOT NULL,
               source_id TEXT NOT NULL,
               source_folder_id TEXT NOT NULL,
               path TEXT NOT NULL,
               bytes BLOB NOT NULL,
               permissions_mode INTEGER NOT NULL CHECK(permissions_mode >= 0),
               device TEXT NOT NULL,
               inode TEXT NOT NULL,
               parent_device TEXT NOT NULL,
               parent_inode TEXT NOT NULL,
               phase TEXT NOT NULL CHECK(phase IN ('captured','removed','conflict','keep_existing')),
               PRIMARY KEY(attempt_id, source_id),
               UNIQUE(attempt_id, path)
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_filing_projection_temp
               ON filing_projection_journal(temp_path);
             CREATE INDEX IF NOT EXISTS idx_filing_projection_owner
               ON filing_projection_journal(owner_kind, owner_id);",
        )
        .map_err(map_err)?;
        // Early feature builds created the journal without an exact per-source protection domain.
        // Add the column without a guessed default: legacy in-flight rows remain NULL and the row
        // reader fails closed instead of inferring governance from a mutable filesystem path.
        Self::add_column_if_missing(
            conn,
            "filing_projection_sources",
            "source_folder_id",
            "TEXT",
        )
    }

    pub(crate) fn reserve_filing_attempt(
        &self,
        attempt_id: &str,
        meeting_id: &str,
        source_folder_id: &str,
        target_folder_id: &str,
        companion_id: Option<&str>,
    ) -> Result<()> {
        self.lock()
            .execute(
                "INSERT INTO filing_projection_attempts(
                   attempt_id,operation_kind,meeting_id,source_folder_id,target_folder_id,
                   companion_id,phase)
                 VALUES(?1,'recording_filing',?2,?3,?4,?5,'prepared')",
                rusqlite::params![
                    attempt_id,
                    meeting_id,
                    source_folder_id,
                    target_folder_id,
                    companion_id
                ],
            )
            .map_err(map_err)?;
        Ok(())
    }

    /// Content-free protection-domain witnesses for every provider row of one meeting.
    ///
    /// `meetings.folder_id` governs meeting-wide content, but each provider Markdown/export row is
    /// physically owned by its own `notes.folder_id`. A canonical meeting anchor must never mask a
    /// skewed legacy/provider row here: filing gates these witnesses before reading Markdown and
    /// carries the same exact values into its projection/source journal and terminal transaction.
    pub(crate) fn filing_note_source_domains(
        &self,
        meeting_id: &str,
    ) -> Result<Vec<(String, Option<String>)>> {
        let conn = self.lock();
        let mut statement = conn
            .prepare(
                "SELECT provider_id,folder_id FROM notes
                  WHERE meeting_id=?1 ORDER BY provider_id",
            )
            .map_err(map_err)?;
        let rows = statement
            .query_map([meeting_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_err)?);
        }
        Ok(out)
    }

    pub(crate) fn reserve_filing_projection(
        &self,
        reservation: &FilingProjectionReservation<'_>,
    ) -> Result<()> {
        let expected_len = i64::try_from(reservation.expected_len)
            .map_err(|_| AppError::Storage("filing projection length exceeds SQLite".into()))?;
        let changed = self
            .lock()
            .execute(
                "INSERT INTO filing_projection_journal(
                   attempt_id,projection_id,operation_kind,owner_kind,owner_id,provider_id,
                   source_folder_id,target_folder_id,source_path,temp_path,final_path,
                   expected_len,expected_sha256,phase)
                 SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'reserved'
                   FROM filing_projection_attempts WHERE attempt_id=?1",
                rusqlite::params![
                    reservation.attempt_id,
                    reservation.projection_id,
                    reservation.operation_kind,
                    reservation.owner_kind,
                    reservation.owner_id,
                    reservation.provider_id,
                    reservation.source_folder_id,
                    reservation.target_folder_id,
                    reservation.source_path,
                    reservation.temp_path,
                    reservation.final_path,
                    expected_len,
                    reservation.expected_sha256.as_slice(),
                ],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "filing projection has no durable parent attempt".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn pending_filing_attempt_ids(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut statement = conn
            .prepare("SELECT attempt_id FROM filing_projection_attempts ORDER BY attempt_id")
            .map_err(map_err)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Authoritative bundle-level source/target protection domains. Per-projection fields retain
    /// exact artifact scope, but cannot replace the parent attempt witness when recovery is about
    /// to acknowledge or clear that attempt's last row.
    pub(crate) fn filing_attempt_domains(
        &self,
        attempt_id: &str,
    ) -> Result<Option<(String, String)>> {
        self.lock()
            .query_row(
                "SELECT source_folder_id,target_folder_id
                   FROM filing_projection_attempts WHERE attempt_id=?1",
                [attempt_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(map_err)
    }

    /// Content-free lookup used to authorize a keep-existing decision before reading the
    /// SQLCipher source snapshot carried by the recovery row.
    pub(crate) fn filing_source_scope(
        &self,
        source_id: &str,
    ) -> Result<Option<(String, String)>> {
        self.lock()
            .query_row(
                "SELECT attempt_id,source_folder_id
                   FROM filing_projection_sources WHERE source_id=?1",
                [source_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(map_err)
    }

    /// Pending filing attempts whose durable source/target ownership intersects `folder_id`.
    ///
    /// Lock/relock use this narrow index instead of draining the process-global journal: one
    /// unrelated vault conflict must not deny sealing a different folder. The attempt row is the
    /// authoritative bundle scope, while the projection leg covers older/interrupted writers that
    /// reached their per-artifact reservation before all attempt metadata was populated.
    pub(crate) fn pending_filing_attempt_ids_for_folder(
        &self,
        folder_id: &str,
    ) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut statement = conn
            .prepare(
                "SELECT attempt_id FROM filing_projection_attempts
                   WHERE source_folder_id=?1 OR target_folder_id=?1
                 UNION
                 SELECT attempt_id FROM filing_projection_journal
                   WHERE source_folder_id=?1 OR target_folder_id=?1
                 UNION
                 SELECT attempt_id FROM filing_projection_sources
                   WHERE source_folder_id=?1 OR source_folder_id IS NULL
                 ORDER BY attempt_id",
            )
            .map_err(map_err)?;
        let rows = statement
            .query_map([folder_id], |row| row.get::<_, String>(0))
            .map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_err)?);
        }
        Ok(out)
    }

    pub(crate) fn folder_has_pending_filing_sources(&self, folder_id: &str) -> Result<bool> {
        self.lock()
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM filing_projection_sources s
                   LEFT JOIN filing_projection_attempts a ON a.attempt_id=s.attempt_id
                  WHERE s.source_folder_id=?1 OR s.source_folder_id IS NULL
                     OR a.source_folder_id=?1 OR a.target_folder_id=?1
                 )",
                [folder_id],
                |row| row.get::<_, bool>(0),
            )
            .map_err(map_err)
    }

    pub(crate) fn reserve_filing_source(&self, source: &FilingSourceReservation<'_>) -> Result<()> {
        let changed = self
            .lock()
            .execute(
                "INSERT INTO filing_projection_sources(
                   attempt_id,source_id,source_folder_id,path,bytes,permissions_mode,device,inode,
                   parent_device,parent_inode,phase)
                 SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'captured'
                   FROM filing_projection_attempts WHERE attempt_id=?1",
                rusqlite::params![
                    source.attempt_id,
                    source.source_id,
                    source.source_folder_id,
                    source.path,
                    source.bytes,
                    i64::from(source.permissions_mode),
                    source.device.to_string(),
                    source.inode.to_string(),
                    source.parent_device.to_string(),
                    source.parent_inode.to_string(),
                ],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "filing source has no durable parent attempt".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn mark_filing_source_removed(
        &self,
        attempt_id: &str,
        source_id: &str,
    ) -> Result<()> {
        let changed = self
            .lock()
            .execute(
                "UPDATE filing_projection_sources SET phase='removed'
                  WHERE attempt_id=?1 AND source_id=?2 AND phase='captured'",
                rusqlite::params![attempt_id, source_id],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "filing source changed before removal acknowledgement".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn pending_filing_sources(&self) -> Result<Vec<FilingSourceJournalRow>> {
        let conn = self.lock();
        let mut statement = conn
            .prepare(
                "SELECT attempt_id,source_id,source_folder_id,path,bytes,permissions_mode,device,
                        inode,parent_device,parent_inode,phase
                   FROM filing_projection_sources ORDER BY attempt_id,source_id",
            )
            .map_err(map_err)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                ))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            let row = row.map_err(map_err)?;
            let source_folder_id = row.2.ok_or_else(|| {
                AppError::Storage("filing source is missing its exact protection domain".into())
            })?;
            let permissions_mode = u32::try_from(row.5)
                .map_err(|_| AppError::Storage("invalid filing source permissions".into()))?;
            out.push(FilingSourceJournalRow {
                attempt_id: row.0,
                source_id: row.1,
                source_folder_id,
                path: row.3,
                bytes: row.4,
                permissions_mode,
                device: parse_identity(Some(row.6), "source device")?
                    .ok_or_else(|| AppError::Storage("missing filing source device".into()))?,
                inode: parse_identity(Some(row.7), "source inode")?
                    .ok_or_else(|| AppError::Storage("missing filing source inode".into()))?,
                parent_device: parse_identity(Some(row.8), "source parent device")?.ok_or_else(
                    || AppError::Storage("missing filing source parent device".into()),
                )?,
                parent_inode: parse_identity(Some(row.9), "source parent inode")?.ok_or_else(
                    || AppError::Storage("missing filing source parent inode".into()),
                )?,
                phase: row.10,
            });
        }
        Ok(out)
    }

    pub(crate) fn clear_filing_source(&self, attempt_id: &str, source_id: &str) -> Result<()> {
        self.lock()
            .execute(
                "DELETE FROM filing_projection_sources WHERE attempt_id=?1 AND source_id=?2",
                rusqlite::params![attempt_id, source_id],
            )
            .map_err(map_err)?;
        Ok(())
    }

    pub(crate) fn mark_filing_source_conflict(
        &self,
        attempt_id: &str,
        source_id: &str,
    ) -> Result<()> {
        let changed = self
            .lock()
            .execute(
                "UPDATE filing_projection_sources SET phase='conflict'
                  WHERE attempt_id=?1 AND source_id=?2 AND phase IN ('captured','removed','conflict')",
                rusqlite::params![attempt_id, source_id],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "filing source conflict state changed unexpectedly".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn keep_existing_filing_source(&self, source_id: &str) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let matches = tx
            .query_row(
                "SELECT COUNT(*) FROM filing_projection_sources
                  WHERE source_id=?1 AND phase='conflict'",
                [source_id],
                |row| row.get::<_, u64>(0),
            )
            .map_err(map_err)?;
        if matches != 1 {
            return Ok(false);
        }
        let changed = tx
            .execute(
                "UPDATE filing_projection_sources SET phase='keep_existing'
                  WHERE source_id=?1 AND phase='conflict'",
                [source_id],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "filing source resolution changed unexpectedly".into(),
            ));
        }
        tx.commit().map_err(map_err)?;
        Ok(true)
    }

    pub(crate) fn acknowledge_kept_filing_source(
        &self,
        attempt_id: &str,
        source_id: &str,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let changed = tx
            .execute(
                "DELETE FROM filing_projection_sources
                  WHERE attempt_id=?1 AND source_id=?2 AND phase='keep_existing'",
                rusqlite::params![attempt_id, source_id],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "kept filing source acknowledgement changed unexpectedly".into(),
            ));
        }
        tx.execute(
            "DELETE FROM filing_projection_attempts
              WHERE attempt_id=?1
                AND NOT EXISTS (SELECT 1 FROM filing_projection_journal WHERE attempt_id=?1)
                AND NOT EXISTS (SELECT 1 FROM filing_projection_sources WHERE attempt_id=?1)",
            [attempt_id],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)
    }

    pub(crate) fn bind_filing_projection_identity(
        &self,
        attempt_id: &str,
        projection_id: &str,
        device: u64,
        inode: u64,
    ) -> Result<()> {
        let changed = self
            .lock()
            .execute(
                "UPDATE filing_projection_journal
                    SET device=?3,inode=?4,phase='bound'
                  WHERE attempt_id=?1 AND projection_id=?2 AND phase='reserved'
                    AND device IS NULL AND inode IS NULL",
                rusqlite::params![
                    attempt_id,
                    projection_id,
                    device.to_string(),
                    inode.to_string()
                ],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "filing projection changed before identity bind".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn reserve_filing_projection_publish(
        &self,
        attempt_id: &str,
        projection_id: &str,
        final_path: &str,
    ) -> Result<()> {
        let changed = self
            .lock()
            .execute(
                "UPDATE filing_projection_journal
                    SET final_path=?3,phase='publish_reserved'
                  WHERE attempt_id=?1 AND projection_id=?2
                    AND phase IN ('bound','publish_reserved')",
                rusqlite::params![attempt_id, projection_id, final_path],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "filing projection changed before publish reservation".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn mark_filing_projection_published(
        &self,
        attempt_id: &str,
        projection_id: &str,
    ) -> Result<()> {
        let changed = self
            .lock()
            .execute(
                "UPDATE filing_projection_journal SET phase='published'
                  WHERE attempt_id=?1 AND projection_id=?2
                    AND phase IN ('bound','publish_reserved')",
                rusqlite::params![attempt_id, projection_id],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "filing projection changed before publish acknowledgement".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn pending_filing_projections(&self) -> Result<Vec<FilingProjectionJournalRow>> {
        let conn = self.lock();
        let mut statement = conn
            .prepare(
                "SELECT attempt_id,projection_id,operation_kind,owner_kind,owner_id,provider_id,
                        source_folder_id,target_folder_id,source_path,temp_path,final_path,
                        expected_len,expected_sha256,device,inode,phase
                   FROM filing_projection_journal
                  ORDER BY attempt_id,projection_id",
            )
            .map_err(map_err)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, Vec<u8>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, String>(15)?,
                ))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            let row = row.map_err(map_err)?;
            if row.11 < 0 {
                return Err(AppError::Storage(
                    "negative filing projection length".into(),
                ));
            }
            out.push(FilingProjectionJournalRow {
                attempt_id: row.0,
                projection_id: row.1,
                operation_kind: row.2,
                owner_kind: row.3,
                owner_id: row.4,
                provider_id: row.5,
                source_folder_id: row.6,
                target_folder_id: row.7,
                source_path: row.8,
                temp_path: row.9,
                final_path: row.10,
                expected_len: row.11 as u64,
                expected_sha256: digest_array(row.12)?,
                device: parse_identity(row.13, "device")?,
                inode: parse_identity(row.14, "inode")?,
                phase: row.15,
            });
        }
        Ok(out)
    }

    pub(crate) fn filing_projection_is_promoted(
        &self,
        row: &FilingProjectionJournalRow,
    ) -> Result<bool> {
        let Some(final_path) = row.final_path.as_deref() else {
            return Ok(false);
        };
        let conn = self.lock();
        match row.owner_kind.as_str() {
            "meeting_note" => conn
                .query_row(
                    "SELECT 1 FROM notes
                      WHERE meeting_id=?1 AND provider_id=?2
                        AND exported_path=?3 AND exported_hash=?4 LIMIT 1",
                    rusqlite::params![
                        row.owner_id,
                        row.provider_id,
                        final_path,
                        digest_hex(&row.expected_sha256)
                    ],
                    |_| Ok(()),
                )
                .optional()
                .map(|value| value.is_some())
                .map_err(map_err),
            "document" => conn
                .query_row(
                    "SELECT 1 FROM documents
                      WHERE id=?1 AND exported_path=?2 AND exported_hash=?3 LIMIT 1",
                    rusqlite::params![row.owner_id, final_path, digest_hex(&row.expected_sha256)],
                    |_| Ok(()),
                )
                .optional()
                .map(|value| value.is_some())
                .map_err(map_err),
            "attachment" => conn
                .query_row(
                    "SELECT 1 FROM note_attachments
                      WHERE id=?1 AND exported_path=?2 AND byte_len=?3 AND sha256=?4 LIMIT 1",
                    rusqlite::params![
                        row.owner_id,
                        final_path,
                        i64::try_from(row.expected_len).map_err(|_| {
                            AppError::Storage("filing projection length exceeds SQLite".into())
                        })?,
                        row.expected_sha256.as_slice()
                    ],
                    |_| Ok(()),
                )
                .optional()
                .map(|value| value.is_some())
                .map_err(map_err),
            _ => Err(AppError::Storage(
                "invalid filing projection owner kind".into(),
            )),
        }
    }

    pub(crate) fn clear_filing_projection(
        &self,
        attempt_id: &str,
        projection_id: &str,
    ) -> Result<()> {
        self.lock()
            .execute(
                "DELETE FROM filing_projection_journal
                  WHERE attempt_id=?1 AND projection_id=?2",
                rusqlite::params![attempt_id, projection_id],
            )
            .map_err(map_err)?;
        Ok(())
    }

    /// Attachment rollback owns every attachment projection for its attempt. Clear only that
    /// owner class after its in-memory exact receipts all verified removal/restoration; note rows
    /// are acknowledged individually because a writer can fail before entering the staged bundle.
    pub(crate) fn clear_filing_attachment_projections_after_verified_rollback(
        &self,
        attempt_id: &str,
    ) -> Result<()> {
        self.lock()
            .execute(
                "DELETE FROM filing_projection_journal
                  WHERE attempt_id=?1 AND owner_kind='attachment'",
                [attempt_id],
            )
            .map_err(map_err)?;
        Ok(())
    }

    pub(crate) fn mark_filing_projection_conflict(
        &self,
        attempt_id: &str,
        projection_id: &str,
    ) -> Result<()> {
        let changed = self
            .lock()
            .execute(
                "UPDATE filing_projection_journal SET phase='conflict'
                  WHERE attempt_id=?1 AND projection_id=?2
                    AND phase IN ('reserved','conflict') AND device IS NULL AND inode IS NULL",
                rusqlite::params![attempt_id, projection_id],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "filing projection conflict state changed unexpectedly".into(),
            ));
        }
        Ok(())
    }

    /// Persist that a bound attempt inode was scrubbed before recovery exposed a conflicting
    /// expected-path occupant. Clearing the identity is safe only after the exact descriptor was
    /// truncated and fsynced; the existing unbound conflict/keep flow can then preserve or accept
    /// the external occupant without retaining plaintext authority.
    pub(crate) fn mark_bound_filing_projection_conflict_scrubbed(
        &self,
        attempt_id: &str,
        projection_id: &str,
    ) -> Result<()> {
        let changed = self
            .lock()
            .execute(
                "UPDATE filing_projection_journal
                    SET device=NULL,inode=NULL,phase='conflict'
                  WHERE attempt_id=?1 AND projection_id=?2
                    AND phase IN ('bound','publish_reserved','published')
                    AND device IS NOT NULL AND inode IS NOT NULL",
                rusqlite::params![attempt_id, projection_id],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "bound filing projection conflict state changed unexpectedly".into(),
            ));
        }
        Ok(())
    }

    /// Atomic promoted twin of `mark_bound_filing_projection_conflict_scrubbed`. The caller has
    /// already scrubbed and fsynced the exact displaced attempt inode; one UPDATE both drops its
    /// destructive identity and makes the external canonical occupant permanently non-waivable.
    pub(crate) fn mark_bound_filing_projection_promoted_conflict_scrubbed(
        &self,
        attempt_id: &str,
        projection_id: &str,
    ) -> Result<()> {
        let changed = self
            .lock()
            .execute(
                "UPDATE filing_projection_journal
                    SET device=NULL,inode=NULL,phase='published'
                  WHERE attempt_id=?1 AND projection_id=?2
                    AND phase IN ('bound','publish_reserved','published')
                    AND device IS NOT NULL AND inode IS NOT NULL",
                rusqlite::params![attempt_id, projection_id],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "bound promoted filing conflict state changed unexpectedly".into(),
            ));
        }
        Ok(())
    }

    /// Reclassify an already-scrubbed conflict once canonical metadata proves that its external
    /// occupant belongs to a promoted projection. The non-waivable marker cannot be changed to
    /// `keep_external`, and remains authoritative even if a later collision-suffixed re-export
    /// moves canonical path metadata.
    pub(crate) fn mark_filing_projection_promoted_conflict(
        &self,
        attempt_id: &str,
        projection_id: &str,
    ) -> Result<()> {
        let changed = self
            .lock()
            .execute(
                "UPDATE filing_projection_journal SET phase='published'
                  WHERE attempt_id=?1 AND projection_id=?2 AND phase='conflict'
                    AND device IS NULL AND inode IS NULL",
                rusqlite::params![attempt_id, projection_id],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "promoted filing conflict state changed unexpectedly".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn keep_external_filing_projection(&self, projection_id: &str) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let matches = tx
            .query_row(
                "SELECT COUNT(*) FROM filing_projection_journal
                  WHERE projection_id=?1 AND phase='conflict'
                    AND device IS NULL AND inode IS NULL",
                [projection_id],
                |row| row.get::<_, u64>(0),
            )
            .map_err(map_err)?;
        if matches != 1 {
            return Ok(false);
        }
        let changed = tx
            .execute(
                "UPDATE filing_projection_journal SET phase='keep_external'
                  WHERE projection_id=?1 AND phase='conflict'
                    AND device IS NULL AND inode IS NULL",
                [projection_id],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "filing projection resolution changed unexpectedly".into(),
            ));
        }
        tx.commit().map_err(map_err)?;
        Ok(true)
    }

    pub(crate) fn acknowledge_kept_filing_projection(
        &self,
        attempt_id: &str,
        projection_id: &str,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let changed = tx
            .execute(
                "DELETE FROM filing_projection_journal
                  WHERE attempt_id=?1 AND projection_id=?2 AND phase='keep_external'
                    AND device IS NULL AND inode IS NULL",
                rusqlite::params![attempt_id, projection_id],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "kept filing projection acknowledgement changed unexpectedly".into(),
            ));
        }
        tx.execute(
            "DELETE FROM filing_projection_attempts
              WHERE attempt_id=?1
                AND NOT EXISTS (SELECT 1 FROM filing_projection_journal WHERE attempt_id=?1)
                AND NOT EXISTS (SELECT 1 FROM filing_projection_sources WHERE attempt_id=?1)",
            [attempt_id],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)
    }

    pub(crate) fn clear_empty_filing_attempt(&self, attempt_id: &str) -> Result<()> {
        let changed = self
            .lock()
            .execute(
                "DELETE FROM filing_projection_attempts
                  WHERE attempt_id=?1
                    AND NOT EXISTS (
                      SELECT 1 FROM filing_projection_journal WHERE attempt_id=?1
                    )
                    AND NOT EXISTS (
                      SELECT 1 FROM filing_projection_sources WHERE attempt_id=?1
                    )",
                rusqlite::params![attempt_id],
            )
            .map_err(map_err)?;
        if changed != 1 {
            let still_exists = self
                .lock()
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM filing_projection_attempts WHERE attempt_id=?1)",
                    [attempt_id],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(map_err)?;
            if still_exists {
            return Err(AppError::Storage(
                "filing attempt still owns unreconciled projections".into(),
            ));
        }
        }
        Ok(())
    }

    /// Content-free recovery health for the startup/UI degraded-state surface.
    pub(crate) fn filing_recovery_counts(&self) -> Result<(u64, u64, u64)> {
        let conn = self.lock();
        let attempts = conn
            .query_row(
                "SELECT COUNT(*) FROM filing_projection_attempts",
                [],
                |row| row.get::<_, u64>(0),
            )
            .map_err(map_err)?;
        let projections = conn
            .query_row(
                "SELECT COUNT(*) FROM filing_projection_journal",
                [],
                |row| row.get::<_, u64>(0),
            )
            .map_err(map_err)?;
        let sources = conn
            .query_row(
                "SELECT COUNT(*) FROM filing_projection_sources",
                [],
                |row| row.get::<_, u64>(0),
            )
            .map_err(map_err)?;
        Ok((attempts, projections, sources))
    }

    pub(crate) fn first_filing_recovery_issue(&self) -> Result<Option<(String, String, bool)>> {
        self.lock()
            .query_row(
                "SELECT token,issue_kind,can_keep FROM (
                   SELECT source_id AS token,'externalSourceReplacement' AS issue_kind,
                          1 AS can_keep,attempt_id
                     FROM filing_projection_sources WHERE phase='conflict'
                   UNION ALL
                   SELECT projection_id AS token,'externalTargetOccupant' AS issue_kind,
                          1 AS can_keep,attempt_id
                     FROM filing_projection_journal WHERE phase='conflict'
                   UNION ALL
                   SELECT projection_id AS token,'externalTargetOccupant' AS issue_kind,
                          0 AS can_keep,attempt_id
                     FROM filing_projection_journal
                    WHERE phase='published' AND device IS NULL AND inode IS NULL
                 ) ORDER BY attempt_id,token LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(map_err)
    }

    pub(crate) fn promote_attachment_restore_and_clear(
        &self,
        attempt_id: &str,
        attachment_id: &str,
        exported_path: Option<&str>,
    ) -> Result<()> {
        #[cfg(test)]
        if FAIL_NEXT_ATTACHMENT_RESTORE_PROMOTION.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(AppError::Storage(
                "injected attachment restore promotion failure".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let changed = tx
            .execute(
                "UPDATE note_attachments SET exported_path=?2 WHERE id=?1",
                rusqlite::params![attachment_id, exported_path],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "attachment disappeared during filing restore promotion".into(),
            ));
        }
        tx.execute(
            "DELETE FROM filing_projection_journal
              WHERE attempt_id=?1 AND owner_kind='attachment' AND owner_id=?2",
            rusqlite::params![attempt_id, attachment_id],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)
    }

    #[cfg(test)]
    pub(crate) fn filing_projection_count(&self) -> Result<u64> {
        self.lock()
            .query_row(
                "SELECT COUNT(*) FROM filing_projection_journal",
                [],
                |row| row.get::<_, u64>(0),
            )
            .map_err(map_err)
    }

    #[cfg(test)]
    pub(crate) fn filing_attempt_count(&self) -> Result<u64> {
        self.lock()
            .query_row(
                "SELECT COUNT(*) FROM filing_projection_attempts",
                [],
                |row| row.get::<_, u64>(0),
            )
            .map_err(map_err)
    }

    #[cfg(test)]
    pub(crate) fn filing_source_count(&self) -> Result<u64> {
        self.lock()
            .query_row(
                "SELECT COUNT(*) FROM filing_projection_sources",
                [],
                |row| row.get::<_, u64>(0),
            )
            .map_err(map_err)
    }
}
