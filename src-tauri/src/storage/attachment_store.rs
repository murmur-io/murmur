//! Canonical note-image attachment storage.
//!
//! Attachment bytes live in SQLCipher, never in an app-private plaintext sidecar. A folder-owned
//! attachment mirrors its note's two-state seal: `data` is session-visible plaintext and
//! `data_blob` is the per-folder AES-GCM copy. Lock code verifies every blob before blanking data.
//! Org items are outside the folder-lock domain and are protected by SQLCipher only.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, Row, Transaction};

use crate::error::{AppError, Result};
use crate::storage::db::{map_err, Db};

pub const MAX_ATTACHMENT_BYTES: usize = 3 * 1024 * 1024;
pub const MAX_ATTACHMENTS_PER_OWNER: usize = 16;
pub const MAX_ATTACHMENT_BYTES_PER_OWNER: usize = 10 * 1024 * 1024;
pub const MAX_ATTACHMENT_DIMENSION: u32 = 12_000;
pub const MAX_ATTACHMENT_PIXELS: u64 = 40_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AttachmentOwner {
    Document {
        document_id: String,
    },
    Meeting {
        meeting_id: String,
        provider_id: String,
    },
    OrgItem {
        item_id: String,
    },
}

impl AttachmentOwner {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Document { .. } => "note",
            Self::Meeting { .. } => "meeting",
            Self::OrgItem { .. } => "org",
        }
    }

    pub fn owner_id(&self) -> &str {
        match self {
            Self::Document { document_id } => document_id,
            Self::Meeting { meeting_id, .. } => meeting_id,
            Self::OrgItem { item_id } => item_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRecord {
    pub id: String,
    pub owner: AttachmentOwner,
    pub mime_type: String,
    pub extension: String,
    pub byte_len: u64,
    pub width: u32,
    pub height: u32,
    pub sha256: [u8; 32],
    pub data: Vec<u8>,
    pub data_blob: Option<Vec<u8>>,
    pub exported_path: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingAttachment {
    pub id: String,
    pub mime_type: String,
    pub extension: String,
    pub width: u32,
    pub height: u32,
    pub sha256: [u8; 32],
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct NewAttachment<'a> {
    pub id: &'a str,
    pub owner: &'a AttachmentOwner,
    pub mime_type: &'a str,
    pub extension: &'a str,
    pub width: u32,
    pub height: u32,
    pub sha256: &'a [u8; 32],
    /// Validated plaintext size. Deliberately separate from `data`: sealed-from-birth rows store
    /// `data=X''` while caps/accounting must still use the real plaintext length.
    pub byte_len: usize,
    pub data: &'a [u8],
    pub data_blob: Option<&'a [u8]>,
    pub created_at: i64,
}

/// Complete, already-authorized plaintext needed to move one terminal recording bundle into an
/// open destination. A failed terminal recording can carry no generated-note projection. The
/// command layer gates both owners and resolves the companion before this reaches storage; the
/// transaction rechecks all content-free identity/lifecycle witnesses before changing a row.
pub(crate) struct OpenRecordingBundleMove<'a> {
    pub filing_attempt_id: &'a str,
    pub meeting_id: &'a str,
    pub expected_source_folder_id: Option<&'a str>,
    pub target_folder_id: Option<&'a str>,
    pub companion_id: Option<&'a str>,
    pub expected_companion_source_folder_id: Option<&'a str>,
    pub companion_target_folder_id: Option<&'a str>,
    /// Exact projection state committed with the canonical move. There is one entry per provider
    /// row, even when no vault is configured (`path/hash = None`).
    pub note_exports: &'a [RecordingNoteExportProjection<'a>],
    pub companion_export_path: Option<&'a str>,
    pub companion_export_hash: Option<&'a str>,
    pub meeting_attachment_plaintext: &'a HashMap<String, Vec<u8>>,
    pub companion_attachment_plaintext: &'a HashMap<String, Vec<u8>>,
}

pub(crate) struct RecordingNoteExportProjection<'a> {
    pub provider_id: &'a str,
    /// Exact pre-filing `notes.folder_id` witness for this provider Markdown/export row.
    pub expected_source_folder_id: Option<&'a str>,
    pub path: Option<&'a str>,
    pub hash: Option<&'a str>,
}

fn require_raw_open_recording_target_tx(
    tx: &Transaction<'_>,
    folder_id: Option<&str>,
) -> Result<()> {
    let Some(mut cursor) = folder_id.map(str::to_string) else {
        return Ok(());
    };
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(cursor.clone()) {
            return Err(AppError::InvalidArg(
                "the recording destination hierarchy contains a parent cycle".into(),
            ));
        }
        let row: Option<(Option<String>, bool)> = tx
            .query_row(
                "SELECT parent_id,locked FROM folders WHERE id=?1",
                [cursor.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(map_err)?;
        let Some((parent_id, locked)) = row else {
            return Err(AppError::InvalidArg(
                "the recording destination disappeared before commit".into(),
            ));
        };
        if locked {
            return Err(AppError::Locked(
                "the recording destination or one of its parents is locked".into(),
            ));
        }
        let closing: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM org_share_closures
                   WHERE scope_kind='folder' AND scope_id=?1)",
                [cursor.as_str()],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        if closing {
            return Err(AppError::Unavailable(
                "the recording destination or one of its parents is closing for sharing".into(),
            ));
        }
        let Some(parent_id) = parent_id else {
            return Ok(());
        };
        cursor = parent_id;
    }
}

impl Db {
    pub(crate) fn migrate_attachments(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS note_attachments (
               id            TEXT PRIMARY KEY,
               document_id   TEXT,
               meeting_id    TEXT,
               provider_id   TEXT,
               org_item_id   TEXT,
               mime_type     TEXT NOT NULL,
               extension     TEXT NOT NULL,
               byte_len      INTEGER NOT NULL CHECK (byte_len >= 0),
               width         INTEGER NOT NULL CHECK (width > 0),
               height        INTEGER NOT NULL CHECK (height > 0),
               sha256        BLOB NOT NULL,
               data          BLOB NOT NULL DEFAULT X'',
               data_blob     BLOB,
               exported_path TEXT,
               created_at    INTEGER NOT NULL,
               FOREIGN KEY (document_id) REFERENCES documents(id) ON DELETE CASCADE,
               FOREIGN KEY (meeting_id, provider_id)
                 REFERENCES notes(meeting_id, provider_id) ON DELETE CASCADE,
               FOREIGN KEY (org_item_id) REFERENCES org_items(item_id) ON DELETE CASCADE,
               CHECK (
                 (document_id IS NOT NULL AND meeting_id IS NULL AND provider_id IS NULL AND org_item_id IS NULL)
                 OR
                 (document_id IS NULL AND meeting_id IS NOT NULL AND provider_id IS NOT NULL AND org_item_id IS NULL)
                 OR
                 (document_id IS NULL AND meeting_id IS NULL AND provider_id IS NULL AND org_item_id IS NOT NULL)
               )
             );
             CREATE INDEX IF NOT EXISTS idx_note_attachments_document
               ON note_attachments(document_id);
             CREATE INDEX IF NOT EXISTS idx_note_attachments_meeting
               ON note_attachments(meeting_id, provider_id);
             CREATE INDEX IF NOT EXISTS idx_note_attachments_org
               ON note_attachments(org_item_id);",
        )
        .map_err(map_err)?;
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS org_share_attachment_source_version_ai
               AFTER INSERT ON note_attachments
               BEGIN
                 UPDATE org_items SET projection_sha256=NULL WHERE item_id=NEW.org_item_id;
                 UPDATE org_shares SET source_version = source_version + 1
                  WHERE (NEW.document_id IS NOT NULL AND document_id = NEW.document_id)
                     OR (NEW.meeting_id IS NOT NULL AND meeting_id = NEW.meeting_id);
                 UPDATE org_shares SET republish_dirty = republish_dirty + 1
                  WHERE state IN ('queued','uploaded','failed') AND
                    ((NEW.document_id IS NOT NULL AND document_id = NEW.document_id)
                     OR (NEW.meeting_id IS NOT NULL AND meeting_id = NEW.meeting_id));
                 INSERT INTO org_source_versions(source_kind,source_id,version)
                   SELECT 'document',NEW.document_id,1 WHERE NEW.document_id IS NOT NULL
                   ON CONFLICT(source_kind,source_id) DO UPDATE SET version=version+1;
                 INSERT INTO org_source_versions(source_kind,source_id,version)
                   SELECT 'meeting',NEW.meeting_id,1 WHERE NEW.meeting_id IS NOT NULL
                   ON CONFLICT(source_kind,source_id) DO UPDATE SET version=version+1;
               END;
             CREATE TRIGGER IF NOT EXISTS org_share_attachment_source_version_ad
               AFTER DELETE ON note_attachments
               BEGIN
                 UPDATE org_items SET projection_sha256=NULL WHERE item_id=OLD.org_item_id;
                 UPDATE org_shares SET source_version = source_version + 1
                  WHERE (OLD.document_id IS NOT NULL AND document_id = OLD.document_id)
                     OR (OLD.meeting_id IS NOT NULL AND meeting_id = OLD.meeting_id);
                 UPDATE org_shares SET republish_dirty = republish_dirty + 1
                  WHERE state IN ('queued','uploaded','failed') AND
                    ((OLD.document_id IS NOT NULL AND document_id = OLD.document_id)
                     OR (OLD.meeting_id IS NOT NULL AND meeting_id = OLD.meeting_id));
                 INSERT INTO org_source_versions(source_kind,source_id,version)
                   SELECT 'document',OLD.document_id,1 WHERE OLD.document_id IS NOT NULL
                   ON CONFLICT(source_kind,source_id) DO UPDATE SET version=version+1;
                 INSERT INTO org_source_versions(source_kind,source_id,version)
                   SELECT 'meeting',OLD.meeting_id,1 WHERE OLD.meeting_id IS NOT NULL
                   ON CONFLICT(source_kind,source_id) DO UPDATE SET version=version+1;
               END;
             CREATE TRIGGER IF NOT EXISTS org_item_attachment_projection_au
               AFTER UPDATE ON note_attachments
               BEGIN
                 UPDATE org_items SET projection_sha256=NULL
                  WHERE item_id IS OLD.org_item_id OR item_id IS NEW.org_item_id;
               END;
             CREATE TRIGGER IF NOT EXISTS org_share_attachment_source_version_au_v2
               AFTER UPDATE ON note_attachments
               BEGIN
                 UPDATE org_items SET projection_sha256=NULL
                  WHERE item_id IS OLD.org_item_id OR item_id IS NEW.org_item_id;
                 UPDATE org_shares SET source_version = source_version + 1
                  WHERE (OLD.document_id IS NOT NULL AND document_id = OLD.document_id)
                     OR (OLD.meeting_id IS NOT NULL AND meeting_id = OLD.meeting_id)
                     OR (NEW.document_id IS NOT NULL AND document_id = NEW.document_id)
                     OR (NEW.meeting_id IS NOT NULL AND meeting_id = NEW.meeting_id);
                 UPDATE org_shares SET republish_dirty = republish_dirty + 1
                  WHERE state IN ('queued','uploaded','failed') AND
                    ((OLD.document_id IS NOT NULL AND document_id = OLD.document_id)
                     OR (OLD.meeting_id IS NOT NULL AND meeting_id = OLD.meeting_id)
                     OR (NEW.document_id IS NOT NULL AND document_id = NEW.document_id)
                     OR (NEW.meeting_id IS NOT NULL AND meeting_id = NEW.meeting_id));
                 INSERT INTO org_source_versions(source_kind,source_id,version)
                   SELECT 'document',OLD.document_id,1 WHERE OLD.document_id IS NOT NULL
                   ON CONFLICT(source_kind,source_id) DO UPDATE SET version=version+1;
                 INSERT INTO org_source_versions(source_kind,source_id,version)
                   SELECT 'document',NEW.document_id,1
                    WHERE NEW.document_id IS NOT NULL AND NEW.document_id IS NOT OLD.document_id
                   ON CONFLICT(source_kind,source_id) DO UPDATE SET version=version+1;
                 INSERT INTO org_source_versions(source_kind,source_id,version)
                   SELECT 'meeting',OLD.meeting_id,1 WHERE OLD.meeting_id IS NOT NULL
                   ON CONFLICT(source_kind,source_id) DO UPDATE SET version=version+1;
                 INSERT INTO org_source_versions(source_kind,source_id,version)
                   SELECT 'meeting',NEW.meeting_id,1
                    WHERE NEW.meeting_id IS NOT NULL AND NEW.meeting_id IS NOT OLD.meeting_id
                   ON CONFLICT(source_kind,source_id) DO UPDATE SET version=version+1;
               END;
             CREATE TRIGGER IF NOT EXISTS closing_attachment_insert_guard
               BEFORE INSERT ON note_attachments
               WHEN EXISTS(SELECT 1 FROM org_share_closures c WHERE
                 (c.scope_kind='document' AND c.scope_id=NEW.document_id) OR
                 (c.scope_kind='meeting' AND c.scope_id=NEW.meeting_id))
               BEGIN SELECT RAISE(ABORT,'share source is closing'); END;
             CREATE TRIGGER IF NOT EXISTS closing_attachment_delete_guard
               BEFORE DELETE ON note_attachments
               WHEN EXISTS(SELECT 1 FROM org_share_closures c WHERE
                 ((c.scope_kind='document' AND c.scope_id=OLD.document_id) OR
                  (c.scope_kind='meeting' AND c.scope_id=OLD.meeting_id))
                 AND ((c.scope_kind='document' AND EXISTS(
                        SELECT 1 FROM documents d WHERE d.id=OLD.document_id)) OR
                      (c.scope_kind='meeting' AND EXISTS(
                        SELECT 1 FROM meetings m WHERE m.id=OLD.meeting_id))))
               BEGIN SELECT RAISE(ABORT,'share source is closing'); END;
             CREATE TRIGGER IF NOT EXISTS closing_attachment_update_guard
               BEFORE UPDATE ON note_attachments
               WHEN EXISTS(SELECT 1 FROM org_share_closures c WHERE
                 (c.scope_kind='document' AND (c.scope_id=OLD.document_id OR c.scope_id=NEW.document_id)) OR
                 (c.scope_kind='meeting' AND (c.scope_id=OLD.meeting_id OR c.scope_id=NEW.meeting_id)))
               BEGIN SELECT RAISE(ABORT,'share source is closing'); END;",
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub fn resolve_attachment_owner(&self, kind: &str, owner_id: &str) -> Result<AttachmentOwner> {
        let conn = self.lock();
        match kind {
            "note" => {
                let exists = conn
                    .query_row(
                        "SELECT id FROM documents WHERE id = ?1 AND kind = 'note'",
                        rusqlite::params![owner_id],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(map_err)?;
                exists
                    .map(|document_id| AttachmentOwner::Document { document_id })
                    .ok_or_else(|| AppError::InvalidArg(format!("no note {owner_id}")))
            }
            "task" => {
                let exists = conn
                    .query_row(
                        "SELECT id FROM documents WHERE id = ?1 AND kind = 'task'",
                        rusqlite::params![owner_id],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(map_err)?;
                exists
                    .map(|document_id| AttachmentOwner::Document { document_id })
                    .ok_or_else(|| AppError::InvalidArg(format!("no task source {owner_id}")))
            }
            "meeting" => {
                let provider_id = conn
                    .query_row(
                        "SELECT provider_id FROM notes WHERE meeting_id = ?1
                           ORDER BY created_at DESC, provider_id DESC LIMIT 1",
                        rusqlite::params![owner_id],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(map_err)?
                    .ok_or_else(|| {
                        AppError::InvalidArg(format!("no note for meeting {owner_id}"))
                    })?;
                Ok(AttachmentOwner::Meeting {
                    meeting_id: owner_id.to_string(),
                    provider_id,
                })
            }
            "org" => {
                let exists = conn
                    .query_row(
                        "SELECT item_id FROM org_items WHERE item_id = ?1 AND tombstoned = 0",
                        rusqlite::params![owner_id],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(map_err)?;
                exists
                    .map(|item_id| AttachmentOwner::OrgItem { item_id })
                    .ok_or_else(|| AppError::InvalidArg(format!("no live org item {owner_id}")))
            }
            _ => Err(AppError::InvalidArg(
                "ownerKind must be note, task, meeting, or org".into(),
            )),
        }
    }

    /// Governing folder for attachment seal/materialization. Meeting owners use the same canonical
    /// resolver as content reads, so a genuine unfiled meeting returns `None` while an ambiguous
    /// legacy provider split fails closed instead of selecting an arbitrary key/domain.
    pub fn folder_for_attachment_owner(&self, owner: &AttachmentOwner) -> Result<Option<String>> {
        match owner {
            AttachmentOwner::Document { document_id } => {
                let conn = self.lock();
                conn.query_row(
                    "SELECT folder_id FROM documents WHERE id = ?1 AND kind IN ('note','task')",
                    rusqlite::params![document_id],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(map_err)
            }
            AttachmentOwner::Meeting { meeting_id, .. } => self.folder_for_meeting(meeting_id),
            AttachmentOwner::OrgItem { .. } => Ok(None),
        }
    }

    pub fn insert_attachment(&self, a: &NewAttachment<'_>) -> Result<()> {
        self.insert_attachments(&[a])
    }

    pub fn insert_attachments(&self, attachments: &[&NewAttachment<'_>]) -> Result<()> {
        if attachments.is_empty() {
            return Ok(());
        }
        let owner = attachments[0].owner;
        if attachments.iter().any(|a| a.owner != owner) {
            return Err(AppError::InvalidArg(
                "attachment batch must have one exact owner".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let (count, bytes): (i64, i64) = match owner {
            AttachmentOwner::Document { document_id } => tx
                .query_row(
                    "SELECT COUNT(*), COALESCE(SUM(byte_len),0) FROM note_attachments WHERE document_id=?1",
                    rusqlite::params![document_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                ),
            AttachmentOwner::Meeting { meeting_id, provider_id } => tx
                .query_row(
                    "SELECT COUNT(*), COALESCE(SUM(byte_len),0) FROM note_attachments WHERE meeting_id=?1 AND provider_id=?2",
                    rusqlite::params![meeting_id, provider_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                ),
            AttachmentOwner::OrgItem { item_id } => tx
                .query_row(
                    "SELECT COUNT(*), COALESCE(SUM(byte_len),0) FROM note_attachments WHERE org_item_id=?1",
                    rusqlite::params![item_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                ),
        }
        .map_err(map_err)?;
        let incoming_bytes: usize = attachments.iter().map(|a| a.byte_len).sum();
        if count as usize + attachments.len() > MAX_ATTACHMENTS_PER_OWNER {
            return Err(AppError::InvalidArg(format!(
                "too many images (max {MAX_ATTACHMENTS_PER_OWNER})"
            )));
        }
        if bytes as usize + incoming_bytes > MAX_ATTACHMENT_BYTES_PER_OWNER {
            return Err(AppError::InvalidArg(format!(
                "images exceed the per-note limit of {} MiB",
                MAX_ATTACHMENT_BYTES_PER_OWNER / 1024 / 1024
            )));
        }
        for a in attachments {
            let (document_id, meeting_id, provider_id, org_item_id) = match a.owner {
                AttachmentOwner::Document { document_id } => {
                    (Some(document_id.as_str()), None, None, None)
                }
                AttachmentOwner::Meeting {
                    meeting_id,
                    provider_id,
                } => (
                    None,
                    Some(meeting_id.as_str()),
                    Some(provider_id.as_str()),
                    None,
                ),
                AttachmentOwner::OrgItem { item_id } => (None, None, None, Some(item_id.as_str())),
            };
            tx.execute(
                "INSERT INTO note_attachments
                   (id,document_id,meeting_id,provider_id,org_item_id,mime_type,extension,
                    byte_len,width,height,sha256,data,data_blob,exported_path,created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,NULL,?14)",
                rusqlite::params![
                    a.id,
                    document_id,
                    meeting_id,
                    provider_id,
                    org_item_id,
                    a.mime_type,
                    a.extension,
                    i64::try_from(a.byte_len)
                        .map_err(|_| AppError::InvalidArg("image is too large".into()))?,
                    a.width as i64,
                    a.height as i64,
                    a.sha256.as_slice(),
                    a.data,
                    a.data_blob,
                    a.created_at,
                ],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Atomically replace the image replica for one live org item. The caller validates the entire
    /// authenticated bundle before entering this transaction; a collision or write failure rolls
    /// the delete back, so readers never observe a partial manifest.
    pub fn replace_org_item_attachment_bundle(
        &self,
        item_id: &str,
        attachments: &[IncomingAttachment],
    ) -> Result<()> {
        if attachments.len() > MAX_ATTACHMENTS_PER_OWNER {
            return Err(AppError::InvalidArg(
                "attachment bundle exceeds local limits".into(),
            ));
        }
        let mut ids = HashSet::with_capacity(attachments.len());
        let mut total = 0usize;
        for attachment in attachments {
            if !ids.insert(attachment.id.as_str()) {
                return Err(AppError::InvalidArg("attachment ids must be unique".into()));
            }
            total = total
                .checked_add(attachment.data.len())
                .ok_or_else(|| AppError::InvalidArg("attachment bundle is too large".into()))?;
        }
        if total > MAX_ATTACHMENT_BYTES_PER_OWNER {
            return Err(AppError::InvalidArg(
                "attachment bundle exceeds local limits".into(),
            ));
        }

        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let live: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM org_items WHERE item_id=?1 AND tombstoned=0)",
                rusqlite::params![item_id],
                |r| Ok(r.get::<_, i64>(0)? != 0),
            )
            .map_err(map_err)?;
        if !live {
            return Err(AppError::InvalidArg(
                "no live org item for image bundle".into(),
            ));
        }
        tx.execute(
            "DELETE FROM note_attachments WHERE org_item_id=?1",
            rusqlite::params![item_id],
        )
        .map_err(map_err)?;
        let created_at = chrono::Utc::now().timestamp_millis();
        for attachment in attachments {
            tx.execute(
                "INSERT INTO note_attachments
                   (id,document_id,meeting_id,provider_id,org_item_id,mime_type,extension,
                    byte_len,width,height,sha256,data,data_blob,exported_path,created_at)
                 VALUES (?1,NULL,NULL,NULL,?2,?3,?4,?5,?6,?7,?8,?9,NULL,NULL,?10)",
                rusqlite::params![
                    attachment.id,
                    item_id,
                    attachment.mime_type,
                    attachment.extension,
                    i64::try_from(attachment.data.len())
                        .map_err(|_| AppError::InvalidArg("image is too large".into()))?,
                    i64::from(attachment.width),
                    i64::from(attachment.height),
                    attachment.sha256.as_slice(),
                    attachment.data,
                    created_at,
                ],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Drop every image replica belonging to ONE org item, inside an EXISTING transaction. Called by
    /// the single org eviction primitive (`org_store::Db::evict_org_item`) so a withdrawn colleague's
    /// pictures never outlive the withdrawn text.
    ///
    /// WHY THIS EXISTS AT ALL: `note_attachments.org_item_id` carries
    /// `REFERENCES org_items(item_id) ON DELETE CASCADE`, which only fires on a row DELETE — and an
    /// org eviction is an UPDATE (`tombstoned = 1`), because the header row must survive as a
    /// tombstone to keep an append-only re-pull idempotent. So the CASCADE never fired and the
    /// plaintext image BLOBs leaked past the revoke. Org attachments are never exported to the vault
    /// (`exported_path` is only ever written by the note/meeting export paths), so there is no
    /// on-disk file to reap here — the BLOB rows are the whole replica.
    pub(crate) fn purge_org_item_attachments_tx(
        tx: &rusqlite::Transaction<'_>,
        item_id: &str,
    ) -> Result<()> {
        tx.execute(
            "DELETE FROM note_attachments WHERE org_item_id=?1",
            rusqlite::params![item_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub fn list_attachments(&self, owner: &AttachmentOwner) -> Result<Vec<AttachmentRecord>> {
        let conn = self.lock();
        let (sql, p1, p2): (&str, &str, Option<&str>) = match owner {
            AttachmentOwner::Document { document_id } => (
                "SELECT id,document_id,meeting_id,provider_id,org_item_id,mime_type,extension,byte_len,width,height,sha256,data,data_blob,exported_path,created_at FROM note_attachments WHERE document_id=?1 ORDER BY created_at,id",
                document_id,
                None,
            ),
            AttachmentOwner::Meeting { meeting_id, provider_id } => (
                "SELECT id,document_id,meeting_id,provider_id,org_item_id,mime_type,extension,byte_len,width,height,sha256,data,data_blob,exported_path,created_at FROM note_attachments WHERE meeting_id=?1 AND provider_id=?2 ORDER BY created_at,id",
                meeting_id,
                Some(provider_id),
            ),
            AttachmentOwner::OrgItem { item_id } => (
                "SELECT id,document_id,meeting_id,provider_id,org_item_id,mime_type,extension,byte_len,width,height,sha256,data,data_blob,exported_path,created_at FROM note_attachments WHERE org_item_id=?1 ORDER BY created_at,id",
                item_id,
                None,
            ),
        };
        let mut stmt = conn.prepare(sql).map_err(map_err)?;
        let rows = if let Some(p2) = p2 {
            stmt.query_map(rusqlite::params![p1, p2], row_to_attachment)
        } else {
            stmt.query_map(rusqlite::params![p1], row_to_attachment)
        }
        .map_err(map_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(map_err)
    }

    pub fn list_referenced_attachments(
        &self,
        owner: &AttachmentOwner,
        ids: &HashSet<String>,
    ) -> Result<Vec<AttachmentRecord>> {
        let rows = self.list_attachments(owner)?;
        let out: Vec<_> = rows.into_iter().filter(|r| ids.contains(&r.id)).collect();
        if out.len() != ids.len() {
            return Err(AppError::InvalidArg(
                "markdown references an unknown image for this note".into(),
            ));
        }
        Ok(out)
    }

    pub fn delete_attachment(
        &self,
        owner: &AttachmentOwner,
        attachment_id: &str,
    ) -> Result<Option<String>> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let path: Option<String> = match owner {
            AttachmentOwner::Document { document_id } => tx
                .query_row(
                    "SELECT exported_path FROM note_attachments WHERE id=?1 AND document_id=?2",
                    rusqlite::params![attachment_id, document_id],
                    |r| r.get(0),
                ),
            AttachmentOwner::Meeting { meeting_id, provider_id } => tx
                .query_row(
                    "SELECT exported_path FROM note_attachments WHERE id=?1 AND meeting_id=?2 AND provider_id=?3",
                    rusqlite::params![attachment_id, meeting_id, provider_id],
                    |r| r.get(0),
                ),
            AttachmentOwner::OrgItem { item_id } => tx
                .query_row(
                    "SELECT exported_path FROM note_attachments WHERE id=?1 AND org_item_id=?2",
                    rusqlite::params![attachment_id, item_id],
                    |r| r.get(0),
                ),
        }
        .optional()
        .map_err(map_err)?
        .flatten();
        match owner {
            AttachmentOwner::Document { document_id } => tx.execute(
                "DELETE FROM note_attachments WHERE id=?1 AND document_id=?2",
                rusqlite::params![attachment_id, document_id],
            ),
            AttachmentOwner::Meeting {
                meeting_id,
                provider_id,
            } => tx.execute(
                "DELETE FROM note_attachments WHERE id=?1 AND meeting_id=?2 AND provider_id=?3",
                rusqlite::params![attachment_id, meeting_id, provider_id],
            ),
            AttachmentOwner::OrgItem { item_id } => tx.execute(
                "DELETE FROM note_attachments WHERE id=?1 AND org_item_id=?2",
                rusqlite::params![attachment_id, item_id],
            ),
        }
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(path)
    }

    pub fn set_attachment_exported_path(&self, id: &str, path: Option<&str>) -> Result<()> {
        let conn = self.lock();
        let changed = conn
            .execute(
                "UPDATE note_attachments SET exported_path=?2 WHERE id=?1",
                rusqlite::params![id, path],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "attachment disappeared while recording its export path".into(),
            ));
        }
        Ok(())
    }

    /// Content-free projection witness used by command-layer rollback. The attachment row remains
    /// the canonical owner of the path throughout publication and cleanup.
    pub(crate) fn attachment_exported_path(&self, id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT exported_path FROM note_attachments WHERE id=?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_err)?
        .ok_or_else(|| AppError::Storage("attachment disappeared during export rollback".into()))
    }

    /// Content-free integrity metadata used by crash/startup cleanup. The filesystem path is
    /// deleted only after its bytes match this SQLCipher-canonical digest; a missing row therefore
    /// fails closed instead of turning an untrusted path into a delete target.
    pub fn attachment_integrity(&self, id: &str) -> Result<Option<(u64, [u8; 32])>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT byte_len,sha256 FROM note_attachments WHERE id=?1",
            rusqlite::params![id],
            |row| {
                let byte_len = row.get::<_, i64>(0)?;
                let sha = row.get::<_, Vec<u8>>(1)?;
                let sha256: [u8; 32] = sha.try_into().map_err(|_| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Blob,
                        "attachment sha256 must be 32 bytes".into(),
                    )
                })?;
                let byte_len = u64::try_from(byte_len).map_err(|_| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Integer,
                        "attachment byte_len must be non-negative".into(),
                    )
                })?;
                Ok((byte_len, sha256))
            },
        )
        .optional()
        .map_err(map_err)
    }

    pub fn store_attachment_seals(&self, sealed: &[(String, Vec<u8>)]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        for (id, blob) in sealed {
            let changed = tx
                .execute(
                    "UPDATE note_attachments SET data_blob=?2 WHERE id=?1",
                    rusqlite::params![id, blob],
                )
                .map_err(map_err)?;
            if changed != 1 {
                return Err(AppError::Storage(
                    "attachment disappeared during seal".into(),
                ));
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    pub fn blank_attachments_in_folder(&self, folder_id: &str) -> Result<Vec<(String, String)>> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let mut stmt = tx
            .prepare(
                "SELECT a.id,a.exported_path FROM note_attachments a
                   WHERE a.exported_path IS NOT NULL AND (
                     a.document_id IN (SELECT id FROM documents WHERE folder_id=?1)
                     OR a.meeting_id IN (
                       SELECT id FROM meetings WHERE folder_id=?1
                       UNION SELECT n.meeting_id FROM notes n JOIN meetings m ON m.id=n.meeting_id
                              WHERE n.folder_id=?1 AND m.folder_id IS NULL)
                   )",
            )
            .map_err(map_err)?;
        let paths = stmt
            .query_map(rusqlite::params![folder_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<(String, String)>>>()
            .map_err(map_err)?;
        drop(stmt);
        tx.execute(
            "UPDATE note_attachments SET data=X''
               WHERE data_blob IS NOT NULL AND (
                 document_id IN (SELECT id FROM documents WHERE folder_id=?1)
                 OR meeting_id IN (
                   SELECT id FROM meetings WHERE folder_id=?1
                   UNION SELECT n.meeting_id FROM notes n JOIN meetings m ON m.id=n.meeting_id
                          WHERE n.folder_id=?1 AND m.folder_id IS NULL)
               )",
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(paths)
    }

    /// Startup crash reconciliation for every locked folder. Like the clean relock, plaintext is
    /// blanked only where a recoverable `data_blob` exists; exported paths are returned for the
    /// filesystem delete-then-clear half.
    pub fn reblank_locked_attachments_at_rest(&self) -> Result<Vec<(String, String)>> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let locked_owner = "(document_id IN (
              SELECT d.id FROM documents d JOIN folders f ON f.id=d.folder_id WHERE f.locked=1
            ) OR meeting_id IN (
              SELECT m.id FROM meetings m JOIN folders f ON f.id=m.folder_id WHERE f.locked=1
              UNION SELECT n.meeting_id FROM notes n
                    JOIN meetings m ON m.id=n.meeting_id JOIN folders f ON f.id=n.folder_id
                    WHERE m.folder_id IS NULL AND f.locked=1
            ))";
        let mut stmt = tx
            .prepare(&format!(
                "SELECT id,exported_path FROM note_attachments
                   WHERE data_blob IS NOT NULL AND exported_path IS NOT NULL AND {locked_owner}"
            ))
            .map_err(map_err)?;
        let paths = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<(String, String)>>>()
            .map_err(map_err)?;
        drop(stmt);
        tx.execute(
            &format!(
                "UPDATE note_attachments SET data=X'' WHERE data_blob IS NOT NULL AND {locked_owner}"
            ),
            [],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(paths)
    }

    pub fn attachments_in_folder(&self, folder_id: &str) -> Result<Vec<AttachmentRecord>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id,document_id,meeting_id,provider_id,org_item_id,mime_type,extension,byte_len,width,height,sha256,data,data_blob,exported_path,created_at
                   FROM note_attachments WHERE
                     document_id IN (SELECT id FROM documents WHERE folder_id=?1)
                     OR meeting_id IN (
                       SELECT id FROM meetings WHERE folder_id=?1
                       UNION SELECT n.meeting_id FROM notes n JOIN meetings m ON m.id=n.meeting_id
                              WHERE n.folder_id=?1 AND m.folder_id IS NULL)
                   ORDER BY created_at,id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], row_to_attachment)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    pub fn attachments_for_meeting(&self, meeting_id: &str) -> Result<Vec<AttachmentRecord>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id,document_id,meeting_id,provider_id,org_item_id,mime_type,extension,byte_len,width,height,sha256,data,data_blob,exported_path,created_at
                   FROM note_attachments WHERE meeting_id=?1 ORDER BY created_at,id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![meeting_id], row_to_attachment)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// Reassign every provider row and install target-folder seals for every attachment in one
    /// transaction. Plaintext is blanked in that same commit; verified session plaintext is restored
    /// by the caller only after the rest of the meeting seal succeeds.
    pub fn move_meeting_with_attachments_sealed(
        &self,
        meeting_id: &str,
        folder_id: &str,
        sealed: &HashMap<String, Vec<u8>>,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM note_attachments WHERE meeting_id=?1",
                rusqlite::params![meeting_id],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        if count as usize != sealed.len() {
            return Err(AppError::Storage(
                "attachment set changed during meeting move".into(),
            ));
        }
        tx.execute(
            "UPDATE meetings SET folder_id=?2 WHERE id=?1",
            rusqlite::params![meeting_id, folder_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE notes SET folder_id=?2 WHERE meeting_id=?1",
            rusqlite::params![meeting_id, folder_id],
        )
        .map_err(map_err)?;
        for (id, blob) in sealed {
            let changed = tx
                .execute(
                    "UPDATE note_attachments SET data=X'',data_blob=?3,exported_path=NULL
                       WHERE id=?1 AND meeting_id=?2",
                    rusqlite::params![id, meeting_id, blob],
                )
                .map_err(map_err)?;
            if changed != 1 {
                return Err(AppError::Storage(
                    "attachment disappeared during meeting move".into(),
                ));
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Move to an open/root target and atomically discard any source-folder seals only after the
    /// caller has recovered and verified the corresponding plaintext bytes.
    pub fn move_meeting_with_attachments_open(
        &self,
        meeting_id: &str,
        folder_id: Option<&str>,
        plaintext: &HashMap<String, Vec<u8>>,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM note_attachments WHERE meeting_id=?1",
                rusqlite::params![meeting_id],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        if count as usize != plaintext.len() {
            return Err(AppError::Storage(
                "attachment set changed during meeting move".into(),
            ));
        }
        tx.execute(
            "UPDATE meetings SET folder_id=?2 WHERE id=?1",
            rusqlite::params![meeting_id, folder_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE notes SET folder_id=?2 WHERE meeting_id=?1",
            rusqlite::params![meeting_id, folder_id],
        )
        .map_err(map_err)?;
        for (id, data) in plaintext {
            let changed = tx
                .execute(
                    "UPDATE note_attachments SET data=?3,data_blob=NULL
                       WHERE id=?1 AND meeting_id=?2",
                    rusqlite::params![id, meeting_id, data],
                )
                .map_err(map_err)?;
            if changed != 1 {
                return Err(AppError::Storage(
                    "attachment disappeared during meeting move".into(),
                ));
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Move every canonical row owned by one terminal recording in ONE transaction.
    /// The destination is already proven raw-open by the command layer. This storage seam still
    /// rechecks the meeting status, recovery-generation terminality, source-folder witnesses, the
    /// optional structured companion identity, and both attachment sets before mutating anything.
    pub(crate) fn move_open_recording_bundle(
        &self,
        move_: &OpenRecordingBundleMove<'_>,
    ) -> Result<()> {
        self.move_open_recording_bundle_core(move_, || Ok(()))
    }

    fn move_open_recording_bundle_core(
        &self,
        move_: &OpenRecordingBundleMove<'_>,
        mut checkpoint: impl FnMut() -> Result<()>,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;

        let meeting: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT status,folder_id FROM meetings WHERE id=?1",
                [move_.meeting_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(map_err)?;
        let Some((status, source_folder_id)) = meeting else {
            return Err(AppError::InvalidArg(format!(
                "no meeting {}",
                move_.meeting_id
            )));
        };
        if !matches!(
            status.as_str(),
            "TRANSCRIBED" | "SUMMARIZED" | "EXPORTED" | "ERROR"
        ) {
            return Err(AppError::Unavailable(
                "the recording must finish processing before it can be filed".into(),
            ));
        }
        if source_folder_id.as_deref() != move_.expected_source_folder_id {
            return Err(AppError::Unavailable(
                "the recording moved while filing; refresh and retry".into(),
            ));
        }
        let recovery_owned: bool = tx
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM recording_generations
                      WHERE meeting_id=?1 AND state!='RETIRED'
                     UNION ALL
                     SELECT 1 FROM legacy_recording_recovery WHERE meeting_id=?1
                 )",
                [move_.meeting_id],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        if recovery_owned {
            return Err(AppError::Unavailable(
                "recording recovery is still active; retry after it finishes".into(),
            ));
        }

        let companion_count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM documents
                  WHERE kind='note' AND meeting_id=?1",
                [move_.meeting_id],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        if companion_count > 1 {
            return Err(AppError::Storage(
                "the recording has more than one linked companion note".into(),
            ));
        }
        let companion: Option<(String, String)> = tx
            .query_row(
                "SELECT id,folder_id FROM documents
                  WHERE kind='note' AND meeting_id=?1
                  ORDER BY COALESCE(updated_at,created_at) DESC,id ASC LIMIT 1",
                [move_.meeting_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(map_err)?;
        match (companion.as_ref(), move_.companion_id) {
            (None, None) => {}
            (Some((id, folder_id)), Some(expected_id))
                if id == expected_id
                    && Some(folder_id.as_str()) == move_.expected_companion_source_folder_id
                    && move_.companion_target_folder_id.is_some() => {}
            _ => {
                return Err(AppError::Unavailable(
                    "the linked companion changed while filing; refresh and retry".into(),
                ))
            }
        }

        let meeting_attachment_count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM note_attachments WHERE meeting_id=?1",
                [move_.meeting_id],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        if meeting_attachment_count as usize != move_.meeting_attachment_plaintext.len() {
            return Err(AppError::Storage(
                "the recording attachment set changed during filing".into(),
            ));
        }
        let companion_attachment_count: i64 = match move_.companion_id {
            Some(companion_id) => tx
                .query_row(
                    "SELECT COUNT(*) FROM note_attachments WHERE document_id=?1",
                    [companion_id],
                    |row| row.get(0),
                )
                .map_err(map_err)?,
            None => 0,
        };
        if companion_attachment_count as usize != move_.companion_attachment_plaintext.len() {
            return Err(AppError::Storage(
                "the companion attachment set changed during filing".into(),
            ));
        }

        // This is strictly an open-to-open move. A legitimate raw-open source has no retained
        // per-folder ciphertexts: permanent unlock clears every blob and flips `locked=0` in one
        // transaction. Refuse ANY blob before the UPDATEs below can destroy it, even when a stale
        // plaintext twin happens to be present; without the source CK this seam cannot authenticate
        // that the two copies agree.
        let retained_content_blob: bool = tx
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM notes
                      WHERE meeting_id=?1 AND content_blob IS NOT NULL
                     UNION ALL
                     SELECT 1 FROM documents
                      WHERE ?2 IS NOT NULL AND id=?2 AND kind='note' AND meeting_id=?1
                        AND text_blob IS NOT NULL
                     UNION ALL
                     SELECT 1 FROM segments
                      WHERE meeting_id=?1 AND text_blob IS NOT NULL
                     UNION ALL
                     SELECT 1 FROM timelines
                      WHERE meeting_id=?1 AND data_blob IS NOT NULL
                     UNION ALL
                     SELECT 1 FROM meetings
                      WHERE id=?1 AND manual_notes_blob IS NOT NULL
                     UNION ALL
                     SELECT 1 FROM note_attachments
                      WHERE (meeting_id=?1 OR (?2 IS NOT NULL AND document_id=?2))
                        AND data_blob IS NOT NULL
                 )",
                rusqlite::params![move_.meeting_id, move_.companion_id],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        if retained_content_blob {
            return Err(AppError::Storage(
                "recording filing refused a retained sealed-content blob in an open source".into(),
            ));
        }

        let note_provider_domains = {
            let mut stmt = tx
                .prepare("SELECT provider_id,folder_id FROM notes WHERE meeting_id=?1")
                .map_err(map_err)?;
            let rows = stmt
                .query_map([move_.meeting_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })
                .map_err(map_err)?;
            let mut domains = HashMap::new();
            for row in rows {
                let (provider_id, folder_id) = row.map_err(map_err)?;
                if domains.insert(provider_id, folder_id).is_some() {
                    return Err(AppError::Storage(
                        "recording filing found duplicate provider protection domains".into(),
                    ));
                }
            }
            domains
        };
        let mut projected_provider_ids = HashSet::new();
        let mut projected_paths = HashSet::new();
        for projection in move_.note_exports {
            let source_matches = note_provider_domains
                .get(projection.provider_id)
                .is_some_and(|folder_id| {
                    folder_id.as_deref() == projection.expected_source_folder_id
                });
            if !source_matches {
                return Err(AppError::Unavailable(
                    "a recording provider source moved while filing; refresh and retry".into(),
                ));
            }
            if projection.path.is_some() != projection.hash.is_some()
                || projection.path.is_some_and(str::is_empty)
                || projection.hash.is_some_and(str::is_empty)
                || !projected_provider_ids.insert(projection.provider_id)
                || projection
                    .path
                    .is_some_and(|path| !projected_paths.insert(path))
            {
                return Err(AppError::InvalidArg(
                    "recording export projections are incomplete or share an identity/path".into(),
                ));
            }
        }
        if projected_provider_ids.len() != note_provider_domains.len()
            || !projected_provider_ids
                .iter()
                .all(|provider_id| note_provider_domains.contains_key(*provider_id))
        {
            return Err(AppError::Unavailable(
                "the recording provider set changed while filing; refresh and retry".into(),
            ));
        }
        if move_.companion_export_path.is_some() != move_.companion_export_hash.is_some()
            || move_.companion_export_path.is_some_and(str::is_empty)
            || move_.companion_export_hash.is_some_and(str::is_empty)
            || (move_.companion_id.is_none()
                && (move_.companion_export_path.is_some() || move_.companion_export_hash.is_some()))
            || move_
                .companion_export_path
                .is_some_and(|path| projected_paths.contains(path))
        {
            return Err(AppError::InvalidArg(
                "the companion export projection is invalid".into(),
            ));
        }

        // Defense in depth at the terminal write seam: source/target validation at the command
        // layer is not authority for this transaction. Re-read the complete ancestor chains before
        // the first UPDATE so a future caller cannot place plaintext into a locked/closing domain.
        require_raw_open_recording_target_tx(&tx, move_.expected_source_folder_id)?;
        require_raw_open_recording_target_tx(&tx, move_.expected_companion_source_folder_id)?;
        for projection in move_.note_exports {
            require_raw_open_recording_target_tx(&tx, projection.expected_source_folder_id)?;
        }
        require_raw_open_recording_target_tx(&tx, move_.target_folder_id)?;
        require_raw_open_recording_target_tx(&tx, move_.companion_target_folder_id)?;

        let changed = tx
            .execute(
                "UPDATE meetings SET folder_id=?2,manual_notes_blob=NULL WHERE id=?1",
                rusqlite::params![move_.meeting_id, move_.target_folder_id],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "the recording disappeared during filing".into(),
            ));
        }
        for projection in move_.note_exports {
            let changed = tx
                .execute(
                    "UPDATE notes
                        SET folder_id=?3,content_blob=NULL,exported_path=?4,exported_hash=?5
                      WHERE meeting_id=?1 AND provider_id=?2",
                    rusqlite::params![
                        move_.meeting_id,
                        projection.provider_id,
                        move_.target_folder_id,
                        projection.path,
                        projection.hash,
                    ],
                )
                .map_err(map_err)?;
            if changed != 1 {
                return Err(AppError::Storage(
                    "a recording provider row disappeared during filing".into(),
                ));
            }
        }
        tx.execute(
            "UPDATE segments SET text_blob=NULL WHERE meeting_id=?1",
            [move_.meeting_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE timelines SET data_blob=NULL WHERE meeting_id=?1",
            [move_.meeting_id],
        )
        .map_err(map_err)?;
        for (id, data) in move_.meeting_attachment_plaintext {
            let changed = tx
                .execute(
                    "UPDATE note_attachments SET data=?3,data_blob=NULL
                      WHERE id=?1 AND meeting_id=?2",
                    rusqlite::params![id, move_.meeting_id, data],
                )
                .map_err(map_err)?;
            if changed != 1 {
                return Err(AppError::Storage(
                    "a recording attachment disappeared during filing".into(),
                ));
            }
        }

        if let (Some(companion_id), Some(target_folder_id)) =
            (move_.companion_id, move_.companion_target_folder_id)
        {
            let changed = tx
                .execute(
                    "UPDATE documents
                        SET folder_id=?2,text_blob=NULL,exported_path=?4,exported_hash=?5
                      WHERE id=?1 AND kind='note' AND meeting_id=?3",
                    rusqlite::params![
                        companion_id,
                        target_folder_id,
                        move_.meeting_id,
                        move_.companion_export_path,
                        move_.companion_export_hash,
                    ],
                )
                .map_err(map_err)?;
            if changed != 1 {
                return Err(AppError::Storage(
                    "the linked companion disappeared during filing".into(),
                ));
            }
            for (id, data) in move_.companion_attachment_plaintext {
                let changed = tx
                    .execute(
                        "UPDATE note_attachments SET data=?3,data_blob=NULL
                          WHERE id=?1 AND document_id=?2",
                        rusqlite::params![id, companion_id, data],
                    )
                    .map_err(map_err)?;
                if changed != 1 {
                    return Err(AppError::Storage(
                        "a companion attachment disappeared during filing".into(),
                    ));
                }
            }
        }

        // The absence of this SQLCipher journal is the canonical-promotion witness. Delete it in
        // the SAME transaction that stamps every target path; a crash before commit leaves the
        // rows for cleanup, while a crash after commit can never roll back promoted plaintext.
        tx.execute(
            "DELETE FROM filing_projection_journal WHERE attempt_id=?1",
            [move_.filing_attempt_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "DELETE FROM filing_projection_sources WHERE attempt_id=?1",
            [move_.filing_attempt_id],
        )
        .map_err(map_err)?;
        let cleared_attempt = tx
            .execute(
                "DELETE FROM filing_projection_attempts WHERE attempt_id=?1",
                [move_.filing_attempt_id],
            )
            .map_err(map_err)?;
        if cleared_attempt != 1 {
            return Err(AppError::Storage(
                "recording filing lost its durable attempt witness".into(),
            ));
        }

        checkpoint()?;
        tx.commit().map_err(map_err)
    }

    #[cfg(test)]
    pub(crate) fn move_open_recording_bundle_failing(
        &self,
        move_: &OpenRecordingBundleMove<'_>,
    ) -> Result<()> {
        self.move_open_recording_bundle_core(move_, || {
            Err(AppError::Storage(
                "injected recording bundle filing failure".into(),
            ))
        })
    }

    pub fn restore_attachment_data(&self, id: &str, data: &[u8], clear_blob: bool) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE note_attachments SET data=?2,
               data_blob=CASE WHEN ?3 THEN NULL ELSE data_blob END WHERE id=?1",
            rusqlite::params![id, data, clear_blob],
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub fn sealed_attachment_export_rows_in_folder(
        &self,
        folder_id: &str,
    ) -> Result<Vec<(String, String)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id,exported_path FROM note_attachments
                   WHERE data_blob IS NOT NULL AND exported_path IS NOT NULL AND (
                     document_id IN (SELECT id FROM documents WHERE folder_id=?1)
                     OR meeting_id IN (
                       SELECT id FROM meetings WHERE folder_id=?1
                       UNION SELECT n.meeting_id FROM notes n JOIN meetings m ON m.id=n.meeting_id
                              WHERE n.folder_id=?1 AND m.folder_id IS NULL)
                   )",
            )
            .map_err(map_err)?;
        let paths = stmt
            .query_map(rusqlite::params![folder_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<(String, String)>>>()
            .map_err(map_err)?;
        Ok(paths)
    }

    pub fn delete_sealed_attachments_in_folder(&self, folder_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM note_attachments WHERE data_blob IS NOT NULL AND (
               document_id IN (SELECT id FROM documents WHERE folder_id=?1)
               OR meeting_id IN (
                 SELECT id FROM meetings WHERE folder_id=?1
                 UNION SELECT n.meeting_id FROM notes n JOIN meetings m ON m.id=n.meeting_id
                        WHERE n.folder_id=?1 AND m.folder_id IS NULL)
             )",
            rusqlite::params![folder_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    // This internal transaction deliberately receives the complete authenticated note snapshot:
    // splitting the scalar fields into separately mutable state would weaken the atomic move seam.
    #[allow(clippy::too_many_arguments)]
    pub fn move_note_with_attachments_sealed(
        &self,
        document_id: &str,
        folder_id: &str,
        title: &str,
        text: &str,
        text_blob: &[u8],
        updated_at: i64,
        sealed: &HashMap<String, Vec<u8>>,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM note_attachments WHERE document_id=?1",
                rusqlite::params![document_id],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        if count as usize != sealed.len() {
            return Err(AppError::Storage(
                "attachment set changed during note move".into(),
            ));
        }
        let changed = tx
            .execute(
                "UPDATE documents SET folder_id=?2,title=?3,text=?4,text_blob=?5,updated_at=?6,
                    exported_path=NULL,exported_hash=NULL WHERE id=?1 AND kind='note'",
                rusqlite::params![document_id, folder_id, title, text, text_blob, updated_at],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage("note disappeared during move".into()));
        }
        for (id, blob) in sealed {
            let changed = tx
                .execute(
                    "UPDATE note_attachments SET data=X'',data_blob=?3,exported_path=NULL
                       WHERE id=?1 AND document_id=?2",
                    rusqlite::params![id, document_id, blob],
                )
                .map_err(map_err)?;
            if changed != 1 {
                return Err(AppError::Storage(
                    "attachment disappeared during move".into(),
                ));
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    pub fn move_note_with_attachments_open(
        &self,
        document_id: &str,
        folder_id: &str,
        plaintext: &HashMap<String, Vec<u8>>,
    ) -> Result<()> {
        self.move_note_with_attachments_open_core(document_id, folder_id, plaintext, None)
    }

    /// Folder-trash restore twin of [`Db::move_note_with_attachments_open`]. The note placement,
    /// attachment state, and per-member progress witness commit atomically. A retry can therefore
    /// preserve a later explicit move back to the Notes root without guessing whether the fallback
    /// placement came from the original folder delete.
    pub(crate) fn move_note_with_attachments_open_for_trash_restore(
        &self,
        entry_id: &str,
        document_id: &str,
        folder_id: &str,
        plaintext: &HashMap<String, Vec<u8>>,
    ) -> Result<()> {
        self.move_note_with_attachments_open_core(
            document_id,
            folder_id,
            plaintext,
            Some(entry_id),
        )
    }

    fn move_note_with_attachments_open_core(
        &self,
        document_id: &str,
        folder_id: &str,
        plaintext: &HashMap<String, Vec<u8>>,
        restore_entry_id: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let changed = tx
            .execute(
                "UPDATE documents SET folder_id=?2,text_blob=NULL,exported_path=NULL,
                    exported_hash=NULL WHERE id=?1 AND kind='note'",
                rusqlite::params![document_id, folder_id],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage("note disappeared during move".into()));
        }
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM note_attachments WHERE document_id=?1",
                rusqlite::params![document_id],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        if count as usize != plaintext.len() {
            return Err(AppError::Storage(
                "attachment set changed during note move".into(),
            ));
        }
        for (id, data) in plaintext {
            let changed = tx
                .execute(
                    "UPDATE note_attachments SET data=?3,data_blob=NULL,exported_path=NULL
                       WHERE id=?1 AND document_id=?2",
                    rusqlite::params![id, document_id, data],
                )
                .map_err(map_err)?;
            if changed != 1 {
                return Err(AppError::Storage(
                    "attachment disappeared during move".into(),
                ));
            }
        }
        if let Some(entry_id) = restore_entry_id {
            let progress = tx
                .execute(
                    "INSERT INTO trash_folder_restore_members(entry_id,member_kind,member_id)
                     SELECT ?1,'note',?2
                      WHERE EXISTS(
                        SELECT 1 FROM trash_items
                         WHERE id=?1 AND kind IN ('folder','noteFolder')
                      )",
                    rusqlite::params![entry_id, document_id],
                )
                .map_err(map_err)?;
            if progress != 1 {
                return Err(AppError::Storage(
                    "folder restore lost its recovery journal before recording note progress"
                        .into(),
                ));
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Conversion-specific companion update: replace only the command-composed Markdown while
    /// relocating the existing companion into the meeting's exact container in the SAME
    /// transaction. The caller supplies authenticated attachment plaintext and, for a locked
    /// destination, target-CK blobs that it already verified byte-identical. The source folder and
    /// structured meeting id are optimistic identity witnesses; a concurrent move/relink therefore
    /// changes zero rows and rolls the whole operation back.
    #[allow(clippy::too_many_arguments)]
    pub fn update_converted_companion_atomic(
        &self,
        document_id: &str,
        expected_source_folder_id: &str,
        target_folder_id: &str,
        expected_target_locked: bool,
        meeting_id: &str,
        title: &str,
        text: &str,
        text_blob: Option<&[u8]>,
        updated_at: i64,
        attachment_plaintext: &HashMap<String, Vec<u8>>,
        attachment_seals: &HashMap<String, Vec<u8>>,
    ) -> Result<()> {
        self.update_converted_companion_atomic_core(
            document_id,
            expected_source_folder_id,
            target_folder_id,
            expected_target_locked,
            meeting_id,
            title,
            text,
            text_blob,
            updated_at,
            attachment_plaintext,
            attachment_seals,
            || Ok(()),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn update_converted_companion_atomic_core(
        &self,
        document_id: &str,
        expected_source_folder_id: &str,
        target_folder_id: &str,
        expected_target_locked: bool,
        meeting_id: &str,
        title: &str,
        text: &str,
        text_blob: Option<&[u8]>,
        updated_at: i64,
        attachment_plaintext: &HashMap<String, Vec<u8>>,
        attachment_seals: &HashMap<String, Vec<u8>>,
        mut checkpoint: impl FnMut() -> Result<()>,
    ) -> Result<()> {
        if expected_target_locked != text_blob.is_some()
            || (expected_target_locked && attachment_plaintext.len() != attachment_seals.len())
            || (!expected_target_locked && !attachment_seals.is_empty())
        {
            return Err(AppError::Storage(
                "converted companion seal set does not match destination lock state".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let target_matches = tx
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM folders
                     WHERE id = ?1
                       AND locked = ?2
                       AND COALESCE(kind, 'meeting') IN ('meeting', 'note')
                       AND path NOT LIKE '.murmur/%'
                 )",
                rusqlite::params![target_folder_id, expected_target_locked as i64],
                |row| row.get::<_, bool>(0),
            )
            .map_err(map_err)?;
        if !target_matches {
            return Err(AppError::Locked(
                "the conversion destination changed or is unavailable; retry".into(),
            ));
        }
        let attachment_count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM note_attachments WHERE document_id=?1",
                [document_id],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        if attachment_count as usize != attachment_plaintext.len() {
            return Err(AppError::Storage(
                "attachment set changed during converted companion update".into(),
            ));
        }
        let changed = tx
            .execute(
                "UPDATE documents
                    SET folder_id=?4, title=?5, text=?6, text_blob=?7, updated_at=?8,
                        exported_path=NULL, exported_hash=NULL
                  WHERE id=?1 AND kind='note' AND folder_id=?2 AND meeting_id=?3",
                rusqlite::params![
                    document_id,
                    expected_source_folder_id,
                    meeting_id,
                    target_folder_id,
                    title,
                    text,
                    text_blob,
                    updated_at
                ],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "the companion note moved or changed identity during conversion".into(),
            ));
        }
        for (id, data) in attachment_plaintext {
            let changed = if expected_target_locked {
                let blob = attachment_seals.get(id).ok_or_else(|| {
                    AppError::Storage("converted companion attachment seal is missing".into())
                })?;
                tx.execute(
                    "UPDATE note_attachments
                        SET data=X'', data_blob=?3, exported_path=NULL
                      WHERE id=?1 AND document_id=?2",
                    rusqlite::params![id, document_id, blob],
                )
            } else {
                tx.execute(
                    "UPDATE note_attachments
                        SET data=?3, data_blob=NULL, exported_path=NULL
                      WHERE id=?1 AND document_id=?2",
                    rusqlite::params![id, document_id, data],
                )
            }
            .map_err(map_err)?;
            if changed != 1 {
                return Err(AppError::Storage(
                    "attachment disappeared during converted companion update".into(),
                ));
            }
        }
        if expected_target_locked {
            let document_ids = [document_id.to_string()];
            Self::purge_doc_chunks_tx(&tx, &document_ids)?;
            Self::purge_links_tx(&tx, &[], &document_ids, true)?;
            Self::purge_all_pending_audit_findings_tx(&tx)?;
            Self::purge_all_ask_conversations_tx(&tx)?;
        }
        checkpoint()?;
        Self::upsert_link_tx(
            &tx,
            "note",
            document_id,
            "meeting",
            meeting_id,
            "companion",
            1.0,
            "user",
            "active",
            updated_at,
        )?;
        tx.execute(
            "UPDATE org_shares
                SET republish_dirty = republish_dirty + 1, republish_deferred=0
              WHERE document_id=?1 AND state IN ('queued','uploaded','failed')",
            [document_id],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update_converted_companion_atomic_failing(
        &self,
        document_id: &str,
        expected_source_folder_id: &str,
        target_folder_id: &str,
        expected_target_locked: bool,
        meeting_id: &str,
        title: &str,
        text: &str,
        text_blob: Option<&[u8]>,
        updated_at: i64,
        attachment_plaintext: &HashMap<String, Vec<u8>>,
        attachment_seals: &HashMap<String, Vec<u8>>,
    ) -> Result<()> {
        self.update_converted_companion_atomic_core(
            document_id,
            expected_source_folder_id,
            target_folder_id,
            expected_target_locked,
            meeting_id,
            title,
            text,
            text_blob,
            updated_at,
            attachment_plaintext,
            attachment_seals,
            || {
                Err(AppError::Storage(
                    "injected converted companion update failure".into(),
                ))
            },
        )
    }
}

fn row_to_attachment(r: &Row<'_>) -> rusqlite::Result<AttachmentRecord> {
    let document_id: Option<String> = r.get(1)?;
    let meeting_id: Option<String> = r.get(2)?;
    let provider_id: Option<String> = r.get(3)?;
    let org_item_id: Option<String> = r.get(4)?;
    let owner = if let Some(document_id) = document_id {
        AttachmentOwner::Document { document_id }
    } else if let (Some(meeting_id), Some(provider_id)) = (meeting_id, provider_id) {
        AttachmentOwner::Meeting {
            meeting_id,
            provider_id,
        }
    } else if let Some(item_id) = org_item_id {
        AttachmentOwner::OrgItem { item_id }
    } else {
        return Err(rusqlite::Error::InvalidQuery);
    };
    let hash: Vec<u8> = r.get(10)?;
    let sha256: [u8; 32] = hash.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(AttachmentRecord {
        id: r.get(0)?,
        owner,
        mime_type: r.get(5)?,
        extension: r.get(6)?,
        byte_len: u64::try_from(r.get::<_, i64>(7)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
        width: u32::try_from(r.get::<_, i64>(8)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
        height: u32::try_from(r.get::<_, i64>(9)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
        sha256,
        data: r.get(11)?,
        data_blob: r.get(12)?,
        exported_path: r.get(13)?,
        created_at: r.get(14)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Folder, Meeting, MeetingStatus, NoteRecord};

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn ambiguous_legacy_meeting_attachment_owner_fails_closed() {
        let db = Db::open_with_key(std::path::Path::new(":memory:"), TEST_DEK).unwrap();
        for id in ["f-one", "f-two"] {
            db.insert_folder(&Folder {
                id: id.into(),
                name: id.into(),
                path: id.into(),
                parent_id: None,
                locked: false,
                created_at: "2026-08-26T00:00:00Z".into(),
            })
            .unwrap();
        }
        db.insert_meeting(&Meeting {
            id: "m".into(),
            started_at: "2026-08-26T00:00:00Z".into(),
            ended_at: None,
            title: Some("meeting".into()),
            duration_s: 1,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        for (provider_id, folder_id) in [("one", "f-one"), ("two", "f-two")] {
            db.upsert_note(&NoteRecord {
                meeting_id: "m".into(),
                provider_id: provider_id.into(),
                markdown: "private".into(),
                created_at: "2026-08-26T00:00:00Z".into(),
                ..Default::default()
            })
            .unwrap();
            db.lock()
                .execute(
                    "UPDATE notes SET folder_id=?2 WHERE meeting_id='m' AND provider_id=?1",
                    rusqlite::params![provider_id, folder_id],
                )
                .unwrap();
        }
        let owner = AttachmentOwner::Meeting {
            meeting_id: "m".into(),
            provider_id: "one".into(),
        };

        assert!(matches!(
            db.folder_for_attachment_owner(&owner),
            Err(AppError::Locked(_))
        ));
    }
}
