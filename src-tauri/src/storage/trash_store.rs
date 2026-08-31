//! Trash storage — the 30-day recoverable holding area for user-deleted content.
//!
//! # Why a SNAPSHOT table and not a `deleted_at` column
//!
//! The obvious soft-delete (`meetings.deleted_at IS NULL` on every read) was measured and REJECTED:
//! `src-tauri/src/` carries 455 SQL references to `meetings` / `documents` / `folders`, and a
//! trashed item must also vanish from every DERIVED surface — FTS, the vec0 partitions, the entity
//! graph, analytics aggregates, Ask retrieval and the MCP server. Missing ONE of those predicates
//! is a leak-class bug of exactly the shape `lock-model.md` exists to prevent: content the user
//! was told is deleted, still answering questions.
//!
//! So trash keeps the EXISTING, already-proven hard-delete cascade (rows genuinely go away, so
//! every derived read is correct for free) and captures a complete restorable SNAPSHOT first. The
//! risk this trades into is content LOSS on restore, and that is answered with the discipline the
//! repo already uses for sealing: **verify-before-destroy**. [`Db::insert_trash_entry`] stores the
//! payload and the caller re-reads and re-parses it BEFORE the destructive cascade runs; a snapshot
//! that does not verify REFUSES the delete rather than losing the content.
//!
//! # Lock model
//!
//! A snapshot holds PLAINTEXT content, so it is governed by the same folder lock as the content it
//! came from — see `commands::trash` for the two halves:
//!
//! 1. **At capture.** Deleting is already refused for a sealed-and-not-session-unlocked item, so
//!    everything entering trash is readable plaintext. When the SOURCE folder is sealed (`locked=1`,
//!    session-unlocked — the only way the delete was permitted), the payload is sealed under that
//!    folder's CK immediately, so it is never at rest in plaintext.
//! 2. **On a later lock.** A snapshot captured from an OPEN folder that is locked afterwards is
//!    sealed by `seal_folder_extras`, exactly like a note/transcript/document blob, and restored by
//!    `unlock_folder` / re-blanked by `relock_folder` / decrypted by `remove_lock`.
//!
//! `label` is the display title and is sealed alongside `payload`: a locked entry renders as
//! "🔒 Locked" with no title, mirroring the masked meeting DTO.

use rusqlite::OptionalExtension;

use crate::error::Result;
use crate::storage::db::{map_err, Db};

/// Default retention before an entry is permanently purged. Overridable via the
/// `trash_retention_days` setting; see `commands::trash::trash_retention_days`.
pub const DEFAULT_TRASH_RETENTION_DAYS: i64 = 30;
/// Bounds for the retention setting. `1` keeps the feature meaningful (a same-day undo) and `365`
/// keeps an unbounded snapshot table from becoming a silent second copy of the whole vault.
pub const MIN_TRASH_RETENTION_DAYS: i64 = 1;
pub const MAX_TRASH_RETENTION_DAYS: i64 = 365;

/// Hard bound on one `list_trash` response — the view is a recovery surface, not a browse-everything
/// list, and an unbounded read of snapshot payloads is a heap risk (a payload carries a meeting's
/// whole transcript).
pub(crate) const TRASH_LIST_LIMIT: i64 = 500;

/// What kind of entity a trash entry restores. The wire value is the camelCase discriminator the FE
/// switches on; `as_str`/`from_str` are the ONLY conversion (never an ad-hoc string literal at a
/// call site).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrashKind {
    /// A recording: the `meetings` row + segments + provider notes + timeline + manual notes + tags.
    /// Its audio files are deliberately LEFT ON DISK by the capture path and removed only on purge.
    Meeting,
    /// An authored note (`documents` with `kind='note'`).
    Note,
    /// A meeting folder (`folders` with `kind != 'note'`).
    Folder,
    /// A note folder (`folders` with `kind = 'note'`).
    NoteFolder,
}

impl TrashKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TrashKind::Meeting => "meeting",
            TrashKind::Note => "note",
            TrashKind::Folder => "folder",
            TrashKind::NoteFolder => "noteFolder",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "meeting" => Some(TrashKind::Meeting),
            "note" => Some(TrashKind::Note),
            "folder" => Some(TrashKind::Folder),
            "noteFolder" => Some(TrashKind::NoteFolder),
            _ => None,
        }
    }

    /// Human label for a refusal/toast message. Not a wire value.
    pub fn noun(self) -> &'static str {
        match self {
            TrashKind::Meeting => "recording",
            TrashKind::Note => "note",
            TrashKind::Folder => "folder",
            TrashKind::NoteFolder => "note folder",
        }
    }
}

/// One trash row as stored — BOTH seal states, exactly like [`crate::storage::db::RawDocument`].
/// `payload`/`label` are the plaintext columns (blank while sealed); `payload_blob`/`label_blob`
/// hold the AES-GCM ciphertext under the source folder's CK (present while sealed).
///
/// The command layer decides masking from the LIVE session unlock set — never from `payload_blob`
/// being present, which only says the folder is or WAS locked.
#[derive(Debug, Clone)]
pub struct RawTrashEntry {
    pub id: String,
    pub kind: String,
    pub source_id: String,
    /// The folder that governed the deleted item — the lock anchor for this snapshot. `None` for a
    /// vault-root meeting, or for a deleted folder that had no parent.
    pub source_folder_id: Option<String>,
    pub label: String,
    pub label_blob: Option<Vec<u8>>,
    pub payload: String,
    pub payload_blob: Option<Vec<u8>>,
    /// RFC3339. Retention is computed from this against the LIVE setting, so changing the retention
    /// applies to entries already in the trash instead of freezing each one at capture time.
    pub deleted_at: String,
}

impl RawTrashEntry {
    /// True when this snapshot's only surviving copy is ciphertext. A masked read must NOT surface
    /// `label`/`payload` for such a row unless the source folder is session-unlocked.
    pub fn is_sealed(&self) -> bool {
        self.payload_blob.is_some()
    }
}

impl Db {
    /// Create the trash table. Additive + `IF NOT EXISTS` + idempotent, per the migration rule.
    ///
    /// No FK to `folders(id)`: `source_folder_id` must SURVIVE its folder being deleted (deleting a
    /// folder trashes the folder AND, before this, its notes may already sit in the trash pointing
    /// at it). An `ON DELETE CASCADE` here would silently destroy recoverable snapshots — the exact
    /// content loss this table exists to prevent. It is a recorded anchor, resolved defensively.
    ///
    /// Takes `&Connection` (not `&self`) because `Db::migrate` already HOLDS the connection lock and
    /// runs inside its transaction — a `self.lock()` here would deadlock on the non-reentrant mutex.
    /// Same shape as `Db::migrate_dashboards`.
    pub(crate) fn migrate_trash(conn: &rusqlite::Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS trash_items (
               id TEXT PRIMARY KEY,
               kind TEXT NOT NULL,
               source_id TEXT NOT NULL,
               source_folder_id TEXT,
               label TEXT NOT NULL DEFAULT '',
               label_blob BLOB,
               payload TEXT NOT NULL DEFAULT '',
               payload_blob BLOB,
               deleted_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_trash_deleted_at ON trash_items(deleted_at);
             CREATE INDEX IF NOT EXISTS idx_trash_source_folder ON trash_items(source_folder_id);
             CREATE INDEX IF NOT EXISTS idx_trash_source ON trash_items(source_id);",
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Store one snapshot. The CALLER must read it back with [`Db::get_trash_entry`] and re-parse it
    /// BEFORE running the destructive cascade (verify-before-destroy) — this method only writes.
    ///
    /// `payload`/`label` are the plaintext form; pass the already-sealed form via
    /// [`Db::seal_trash_entry`] immediately afterwards when the source folder is sealed.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_trash_entry(
        &self,
        id: &str,
        kind: TrashKind,
        source_id: &str,
        source_folder_id: Option<&str>,
        label: &str,
        payload: &str,
        deleted_at: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO trash_items
               (id, kind, source_id, source_folder_id, label, label_blob, payload, payload_blob, deleted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, NULL, ?7)",
            rusqlite::params![
                id,
                kind.as_str(),
                source_id,
                source_folder_id,
                label,
                payload,
                deleted_at
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Insert an entry that is SEALED from its first instant.
    ///
    /// One statement, so there is no moment at which the row exists with plaintext behind a lock —
    /// the same reasoning as [`Db::insert_sealed_folder`]. Insert-then-seal would leave a window
    /// where a crash strands readable content inside a sealed folder, and no undo closes it (an undo
    /// that itself fails leaves the row behind).
    ///
    /// The CALLER must have already verified both blobs decrypt back byte-identical.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_trash_entry_sealed(
        &self,
        id: &str,
        kind: TrashKind,
        source_id: &str,
        source_folder_id: Option<&str>,
        label_blob: &[u8],
        payload_blob: &[u8],
        deleted_at: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO trash_items
               (id, kind, source_id, source_folder_id, label, label_blob, payload, payload_blob, deleted_at)
             VALUES (?1, ?2, ?3, ?4, '', ?5, '', ?6, ?7)",
            rusqlite::params![
                id,
                kind.as_str(),
                source_id,
                source_folder_id,
                label_blob,
                payload_blob,
                deleted_at
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Read one entry in whatever seal state it is in. `None` for an unknown id (every caller
    /// treats that as an idempotent no-op).
    pub(crate) fn get_trash_entry(&self, id: &str) -> Result<Option<RawTrashEntry>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, kind, source_id, source_folder_id, label, label_blob, payload, payload_blob,
                    deleted_at
               FROM trash_items WHERE id = ?1",
            rusqlite::params![id],
            row_to_raw_trash_entry,
        )
        .optional()
        .map_err(map_err)
    }

    /// Every entry, newest deletion first. Bounded by [`TRASH_LIST_LIMIT`]; the command layer masks
    /// sealed rows before they cross IPC.
    pub(crate) fn list_trash_entries(&self) -> Result<Vec<RawTrashEntry>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, source_id, source_folder_id, label, label_blob, payload,
                        payload_blob, deleted_at
                   FROM trash_items
                  ORDER BY deleted_at DESC, id
                  LIMIT ?1",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![TRASH_LIST_LIMIT], row_to_raw_trash_entry)
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Every entry anchored to one folder, in whatever seal state. The seal/unseal/relock passes
    /// iterate this — mirrors [`Db::raw_documents_in_folder`].
    pub(crate) fn raw_trash_entries_in_folder(&self, folder_id: &str) -> Result<Vec<RawTrashEntry>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, source_id, source_folder_id, label, label_blob, payload,
                        payload_blob, deleted_at
                   FROM trash_items WHERE source_folder_id = ?1 ORDER BY id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], row_to_raw_trash_entry)
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Seal ONE entry: store the AES-GCM blobs, blank the plaintext columns. The CALLER must verify
    /// both blobs decrypt back byte-identical BEFORE calling this (verify-before-destroy) — exactly
    /// like [`Db::seal_document`] / [`Db::seal_note`].
    pub(crate) fn seal_trash_entry(
        &self,
        id: &str,
        label_blob: &[u8],
        payload_blob: &[u8],
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE trash_items
                SET label_blob = ?2, payload_blob = ?3, label = '', payload = ''
              WHERE id = ?1",
            rusqlite::params![id, label_blob, payload_blob],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Restore (or re-blank) an entry's plaintext for the session, leaving the blobs intact. Pass
    /// the decrypted plaintext on unlock; pass `("", "")` on relock. Mirrors
    /// [`Db::set_document_text`].
    pub(crate) fn set_trash_entry_plaintext(
        &self,
        id: &str,
        label: &str,
        payload: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE trash_items SET label = ?2, payload = ?3 WHERE id = ?1",
            rusqlite::params![id, label, payload],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Drop an entry's ciphertext, leaving the plaintext — the permanent-unseal half
    /// (`remove_lock`). Mirrors [`Db::clear_timeline_blob`].
    pub(crate) fn clear_trash_entry_blobs(&self, id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE trash_items SET label_blob = NULL, payload_blob = NULL WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Re-anchor every entry pointing at `from` to `to`. Used when a folder is permanently purged
    /// from the trash: its member snapshots must not keep a dangling anchor that would make them
    /// unreadable (a sealed entry whose folder row is gone can never be unlocked again).
    pub(crate) fn reanchor_trash_entries(&self, from: &str, to: Option<&str>) -> Result<usize> {
        let conn = self.lock();
        let n = conn
            .execute(
                "UPDATE trash_items SET source_folder_id = ?2 WHERE source_folder_id = ?1",
                rusqlite::params![from, to],
            )
            .map_err(map_err)?;
        Ok(n)
    }

    /// Remove one entry row. Purging the entity's on-disk files is the CALLER's job (the command
    /// layer owns the filesystem, the db layer owns rows) — same split as the note `.md` deletion.
    pub(crate) fn delete_trash_entry(&self, id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM trash_items WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Count of entries — the sidebar badge. Cheap enough to call on every trash event.
    pub(crate) fn count_trash_entries(&self) -> Result<i64> {
        let conn = self.lock();
        conn.query_row("SELECT COUNT(*) FROM trash_items", [], |r| r.get(0))
            .map_err(map_err)
    }
}

/// TEST-ONLY: backdate an entry so the retention/purge oracles can reach expiry without sleeping
/// for 30 days (and so the fail-closed undateable case can be driven at all).
#[cfg(test)]
impl Db {
    pub(crate) fn set_trash_deleted_at_for_test(&self, id: &str, deleted_at: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE trash_items SET deleted_at = ?2 WHERE id = ?1",
            rusqlite::params![id, deleted_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// TEST-ONLY: flip a folder's `locked` bit directly, so a read-gate oracle can put a folder in
    /// the sealed-and-not-session-unlocked state without driving the whole Touch-ID lock command.
    pub(crate) fn set_folder_locked_for_test(&self, id: &str, locked: bool) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE folders SET locked = ?2 WHERE id = ?1",
            rusqlite::params![id, locked as i64],
        )
        .map_err(map_err)?;
        Ok(())
    }
}

fn row_to_raw_trash_entry(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawTrashEntry> {
    Ok(RawTrashEntry {
        id: r.get(0)?,
        kind: r.get(1)?,
        source_id: r.get(2)?,
        source_folder_id: r.get(3)?,
        label: r.get(4)?,
        label_blob: r.get(5)?,
        payload: r.get(6)?,
        payload_blob: r.get(7)?,
        deleted_at: r.get(8)?,
    })
}

/// A folder's non-content presentation/placement columns — everything beyond the [`crate::storage::Folder`]
/// struct that a faithful restore has to bring back. Without these a restored Workspace loses its
/// emoji, tint, ordering and (worse) its `level`, which decides whether it is a Project or a Folder.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FolderPresentation {
    pub kind: String,
    pub level: String,
    pub is_root: bool,
    pub emoji: Option<String>,
    pub tint: Option<String>,
    pub position: i64,
}

impl Db {
    /// Read the presentation/placement columns for one folder. `None` for an unknown id.
    pub(crate) fn folder_presentation(&self, id: &str) -> Result<Option<FolderPresentation>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT kind, level, is_root, emoji, tint, position FROM folders WHERE id = ?1",
            rusqlite::params![id],
            |r| {
                Ok(FolderPresentation {
                    kind: r.get(0)?,
                    level: r.get(1)?,
                    is_root: r.get::<_, i64>(2)? != 0,
                    emoji: r.get(3)?,
                    tint: r.get(4)?,
                    position: r.get(5)?,
                })
            },
        )
        .optional()
        .map_err(map_err)
    }

    /// Re-insert a folder row from a trash snapshot, preserving every column the snapshot carried.
    ///
    /// Deliberately inserts `locked = 0` with a NULL `wrapped_key`: `delete_folder_inner` PERMANENTLY
    /// removes a sealed container's lock before dropping the row (it must, or the sealed content
    /// would be orphaned from its key), so there is no key left to restore. A restored container
    /// comes back OPEN, and `FolderSnapshot::was_locked` is what tells the user so.
    pub(crate) fn insert_restored_folder(
        &self,
        id: &str,
        name: &str,
        path: &str,
        parent_id: Option<&str>,
        created_at: &str,
        p: &FolderPresentation,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO folders
               (id, name, path, parent_id, locked, wrapped_key, created_at,
                kind, is_root, level, emoji, tint, position)
             VALUES (?1, ?2, ?3, ?4, 0, NULL, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                id,
                name,
                path,
                parent_id,
                created_at,
                p.kind,
                p.is_root as i64,
                p.level,
                p.emoji,
                p.tint,
                p.position,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Every provider note row for one meeting, with the columns a snapshot needs.
    ///
    /// `Db::get_latest_note_for_meeting` returns only the newest and `Db::sealable_notes_for_meeting`
    /// omits `created_at`, so neither can reconstruct the full set. A trashed meeting may carry
    /// several provider notes, and losing the older ones on restore would be silent content loss.
    pub(crate) fn note_records_for_meeting(
        &self,
        meeting_id: &str,
    ) -> Result<Vec<crate::storage::NoteRecord>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT provider_id, markdown, created_at, exported_path
                   FROM notes WHERE meeting_id = ?1 ORDER BY created_at, provider_id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![meeting_id], |r| {
                Ok(crate::storage::NoteRecord {
                    meeting_id: meeting_id.to_string(),
                    provider_id: r.get(0)?,
                    markdown: r.get(1)?,
                    created_at: r.get(2)?,
                    exported_path: r.get(3)?,
                    ..Default::default()
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }
}
