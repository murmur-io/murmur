//! Storage for SHARED CONTAINERS — publishing a whole Folder or Space to an org, receiving one,
//! and the recipient's private arrangement of what they received.
//!
//! Three tables, three jobs (schema + rationale: `db.rs::migrate_shared_containers`):
//!
//! - `org_container_shares` — the OUTBOUND journal. One row per (org, local container) this device
//!   publishes, carrying the crash-recovery fields the launch sweep reads.
//! - `org_containers` — the INBOUND decrypted manifest replica, keyed by the client-generated
//!   `container_id` so a rename supersedes the same container instead of minting a second one.
//! - `org_local_placements` — the recipient's PRIVATE arrangement. Device-local, never published.
//!
//! LOCK-DOMAIN NOTE: like the rest of `org_*`, these hold deliberately org-disclosed content that
//! lives OUTSIDE the folder-seal domain and is protected at rest by whole-DB SQLCipher. This module
//! adds no seal, no key, and no visibility predicate. The one gate it DOES honour is the same one
//! `list_org_items` honours: an org whose `context_enabled` is off contributes nothing.

use rusqlite::OptionalExtension;

use crate::error::{AppError, Result};
use crate::storage::db::{map_err, Db};
use crate::storage::models::{ContainerShareRow, LocalPlacementRow, OrgContainerRow};

/// The two things a private placement can point at.
const PLACEMENT_TARGET_KINDS: [&str; 2] = ["container", "doc"];

/// Build the synthetic primary key for a placement. SQLite cannot express a composite primary key
/// over "exactly one of two nullable columns", so the identity is materialized instead of implied.
fn placement_key(org_id: &str, target_kind: &str, target_id: &str) -> String {
    let tag = if target_kind == "container" { "c" } else { "d" };
    format!("{org_id}|{tag}|{target_id}")
}

fn container_share_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ContainerShareRow> {
    Ok(ContainerShareRow {
        id: r.get(0)?,
        org_id: r.get(1)?,
        folder_id: r.get(2)?,
        container_id: r.get(3)?,
        access: r.get(4)?,
        scrub: r.get::<_, i64>(5)? != 0,
        is_root: r.get::<_, i64>(6)? != 0,
        state: r.get(7)?,
        item_id: r.get(8)?,
        rev: r.get::<_, i64>(9)? as u32,
        generation: r.get::<_, i64>(10)? as u32,
        content_sha256: r.get(11)?,
        position: r.get(12)?,
        last_error: r.get(13)?,
        created_at: r.get(14)?,
        updated_at: r.get(15)?,
    })
}

const CONTAINER_SHARE_COLUMNS: &str = "id, org_id, folder_id, container_id, access, scrub, is_root,
     state, item_id, rev, generation, content_sha256, position, last_error, created_at, updated_at";

fn org_container_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<OrgContainerRow> {
    Ok(OrgContainerRow {
        org_id: r.get(0)?,
        container_id: r.get(1)?,
        item_id: r.get(2)?,
        level: r.get(3)?,
        name: r.get(4)?,
        emoji: r.get(5)?,
        tint: r.get(6)?,
        parent_container_id: r.get(7)?,
        position: r.get(8)?,
        access: r.get(9)?,
        author_hint: r.get(10)?,
        author_user_id: r.get(11)?,
        document_owner_user_id: r.get(12)?,
        seq: r.get::<_, i64>(13)? as u64,
        rev: r.get::<_, i64>(14)? as u32,
        generation: r.get::<_, i64>(15)? as u32,
        created_at: r.get(16)?,
    })
}

/// Alias-qualified, because the only read is a JOIN against `org_state` and both tables carry an
/// `org_id`.
const ORG_CONTAINER_COLUMNS: &str = "oc.org_id, oc.container_id, oc.item_id, oc.level, oc.name,
     oc.emoji, oc.tint, oc.parent_container_id, oc.position, oc.access, oc.author_hint,
     oc.author_user_id, oc.document_owner_user_id, oc.seq, oc.rev, oc.generation, oc.created_at";

impl Db {
    // ── OUTBOUND: containers this device publishes ────────────────────────────────────────────

    /// Insert or update one container-share journal row, keyed by `(org_id, folder_id)`.
    ///
    /// `container_id` is deliberately NOT overwritten on conflict: it is the stable document
    /// identity other members already hold. Re-sharing a container that was shared before must
    /// supersede the same document, not mint a second one that leaves a ghost in every peer's
    /// sidebar.
    pub fn upsert_container_share(&self, row: &ContainerShareRow) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO org_container_shares
               (id, org_id, folder_id, container_id, access, scrub, is_root, state, item_id, rev,
                generation, content_sha256, position, last_error, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
             ON CONFLICT(org_id, folder_id) DO UPDATE SET
               access=excluded.access, scrub=excluded.scrub, is_root=excluded.is_root,
               state=excluded.state, item_id=excluded.item_id, rev=excluded.rev,
               generation=excluded.generation, content_sha256=excluded.content_sha256,
               position=excluded.position, last_error=excluded.last_error,
               updated_at=excluded.updated_at",
            rusqlite::params![
                row.id,
                row.org_id,
                row.folder_id,
                row.container_id,
                row.access,
                i64::from(row.scrub),
                i64::from(row.is_root),
                row.state,
                row.item_id,
                row.rev as i64,
                row.generation as i64,
                row.content_sha256,
                row.position,
                row.last_error,
                row.created_at,
                row.updated_at,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The journal row for one local container in one org, if this device publishes it.
    pub fn get_container_share(
        &self,
        org_id: &str,
        folder_id: &str,
    ) -> Result<Option<ContainerShareRow>> {
        let conn = self.lock();
        conn.query_row(
            &format!(
                "SELECT {CONTAINER_SHARE_COLUMNS} FROM org_container_shares
                  WHERE org_id = ?1 AND folder_id = ?2"
            ),
            rusqlite::params![org_id, folder_id],
            container_share_from_row,
        )
        .optional()
        .map_err(map_err)
    }

    /// The journal row behind one published manifest identity.
    pub fn container_share_by_container(
        &self,
        org_id: &str,
        container_id: &str,
    ) -> Result<Option<ContainerShareRow>> {
        let conn = self.lock();
        conn.query_row(
            &format!(
                "SELECT {CONTAINER_SHARE_COLUMNS} FROM org_container_shares
                  WHERE org_id = ?1 AND container_id = ?2"
            ),
            rusqlite::params![org_id, container_id],
            container_share_from_row,
        )
        .optional()
        .map_err(map_err)
    }

    /// Every container share, optionally narrowed to one org. Ordered so a parent is listed before
    /// the descendants that hang off it (roots first, then by position and creation).
    pub fn list_container_shares(&self, org_id: Option<&str>) -> Result<Vec<ContainerShareRow>> {
        let conn = self.lock();
        let sql = format!(
            "SELECT {CONTAINER_SHARE_COLUMNS} FROM org_container_shares
              WHERE (?1 IS NULL OR org_id = ?1)
              ORDER BY is_root DESC, position, created_at"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![org_id], |r| container_share_from_row(r))
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// Only the containers the user explicitly picked — the sweep's work list. A descendant folder
    /// is reconciled as part of walking its root, never on its own.
    pub fn list_container_share_roots(&self) -> Result<Vec<ContainerShareRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {CONTAINER_SHARE_COLUMNS} FROM org_container_shares
                  WHERE is_root = 1 ORDER BY created_at"
            ))
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| container_share_from_row(r))
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// Advance one journal row's publish state. Content-free: `last_error` carries a reason code,
    /// never a message with a name in it.
    #[allow(clippy::too_many_arguments)]
    pub fn set_container_share_state(
        &self,
        id: &str,
        state: &str,
        item_id: Option<&str>,
        rev: u32,
        content_sha256: Option<&[u8]>,
        last_error: Option<&str>,
        now: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE org_container_shares
                SET state=?2, item_id=COALESCE(?3, item_id), rev=?4,
                    content_sha256=COALESCE(?5, content_sha256), last_error=?6, updated_at=?7
              WHERE id=?1",
            rusqlite::params![id, state, item_id, rev as i64, content_sha256, last_error, now],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Change one container's member access. The descendants' documents are re-permissioned
    /// separately — this only records the container's own choice.
    pub fn set_container_share_access(&self, id: &str, access: &str, now: &str) -> Result<()> {
        if !matches!(access, "view" | "edit") {
            return Err(AppError::InvalidArg("unknown container access".into()));
        }
        let conn = self.lock();
        conn.execute(
            "UPDATE org_container_shares SET access=?2, updated_at=?3 WHERE id=?1",
            rusqlite::params![id, access, now],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Forget one container share. Called only after its manifest has been withdrawn.
    pub fn delete_container_share(&self, id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM org_container_shares WHERE id=?1",
            rusqlite::params![id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The local folder ids this device currently publishes to one org.
    pub fn shared_container_folder_ids(&self, org_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT folder_id FROM org_container_shares WHERE org_id = ?1")
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![org_id], |r| r.get::<_, String>(0))
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    // ── INBOUND: containers received from an org feed ─────────────────────────────────────────

    /// Write one decrypted manifest into the replica. Re-ingesting the same container (a rename,
    /// a re-pull) updates it in place, which is what keeps a feed replay idempotent.
    pub fn upsert_org_container(&self, row: &OrgContainerRow) -> Result<()> {
        if crate::share::container_envelope::ContainerLevel::from_str(&row.level).is_err() {
            return Err(AppError::InvalidArg("unknown container level".into()));
        }
        let conn = self.lock();
        conn.execute(
            "INSERT INTO org_containers
               (org_id, container_id, item_id, level, name, emoji, tint, parent_container_id,
                position, access, author_hint, author_user_id, document_owner_user_id, seq, rev,
                generation, created_at, tombstoned)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,0)
             ON CONFLICT(org_id, container_id) DO UPDATE SET
               item_id=excluded.item_id, level=excluded.level, name=excluded.name,
               emoji=excluded.emoji, tint=excluded.tint,
               parent_container_id=excluded.parent_container_id, position=excluded.position,
               access=excluded.access, author_hint=excluded.author_hint,
               author_user_id=excluded.author_user_id,
               document_owner_user_id=excluded.document_owner_user_id, seq=excluded.seq,
               rev=excluded.rev, generation=excluded.generation, created_at=excluded.created_at,
               tombstoned=0",
            rusqlite::params![
                row.org_id,
                row.container_id,
                row.item_id,
                row.level,
                row.name,
                row.emoji,
                row.tint,
                row.parent_container_id,
                row.position,
                row.access,
                row.author_hint,
                row.author_user_id,
                row.document_owner_user_id,
                row.seq as i64,
                row.rev as i64,
                row.generation as i64,
                row.created_at,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Mark a received container withdrawn. The row is KEPT as a tombstone so a later re-pull of
    /// the same feed position is a no-op rather than a resurrection.
    pub fn tombstone_org_container(&self, org_id: &str, container_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE org_containers SET tombstoned=1 WHERE org_id=?1 AND container_id=?2",
            rusqlite::params![org_id, container_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Mark a received container withdrawn by the SERVER item id that carried it. The feed's
    /// tombstone entry names an item, and only this device knows which container that item was.
    pub fn tombstone_org_container_by_item(&self, item_id: &str) -> Result<bool> {
        let conn = self.lock();
        let changed = conn
            .execute(
                "UPDATE org_containers SET tombstoned=1 WHERE item_id=?1",
                rusqlite::params![item_id],
            )
            .map_err(map_err)?;
        Ok(changed > 0)
    }

    /// Every live received container in one org, honouring the same per-instance org toggle
    /// `list_org_items` honours. A disabled org contributes nothing.
    pub fn list_org_containers(&self, org_id: &str) -> Result<Vec<OrgContainerRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {ORG_CONTAINER_COLUMNS} FROM org_containers oc
                   JOIN org_state os ON os.org_id = oc.org_id
                  WHERE oc.org_id = ?1 AND oc.tombstoned = 0 AND os.context_enabled = 1
                  ORDER BY oc.position, oc.created_at, oc.name"
            ))
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![org_id], |r| org_container_from_row(r))
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    // ── PRIVATE: this device's own arrangement of received objects ────────────────────────────

    /// File a received container or document somewhere in this user's own tree.
    ///
    /// This mutates NOTHING that can be published: no share journal row, no envelope, no feed
    /// cursor. It is a rendering hint and stays on this device.
    pub fn set_local_placement(
        &self,
        org_id: &str,
        target_kind: &str,
        target_id: &str,
        local_parent_id: Option<&str>,
        position: i64,
        now: &str,
    ) -> Result<()> {
        if !PLACEMENT_TARGET_KINDS.contains(&target_kind) {
            return Err(AppError::InvalidArg("unknown placement target".into()));
        }
        if target_id.trim().is_empty() {
            return Err(AppError::InvalidArg("placement target is empty".into()));
        }
        let key = placement_key(org_id, target_kind, target_id);
        let conn = self.lock();
        conn.execute(
            "INSERT INTO org_local_placements
               (placement_key, org_id, target_kind, target_id, local_parent_id, position, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(placement_key) DO UPDATE SET
               local_parent_id=excluded.local_parent_id, position=excluded.position,
               updated_at=excluded.updated_at",
            rusqlite::params![
                key,
                org_id,
                target_kind,
                target_id,
                local_parent_id,
                position,
                now
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Return a received object to where its owner filed it.
    pub fn clear_local_placement(
        &self,
        org_id: &str,
        target_kind: &str,
        target_id: &str,
    ) -> Result<()> {
        if !PLACEMENT_TARGET_KINDS.contains(&target_kind) {
            return Err(AppError::InvalidArg("unknown placement target".into()));
        }
        let key = placement_key(org_id, target_kind, target_id);
        let conn = self.lock();
        conn.execute(
            "DELETE FROM org_local_placements WHERE placement_key=?1",
            rusqlite::params![key],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Every private placement on this device.
    pub fn list_local_placements(&self) -> Result<Vec<LocalPlacementRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT org_id, target_kind, target_id, local_parent_id, position
                   FROM org_local_placements ORDER BY position, target_id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(LocalPlacementRow {
                    org_id: r.get(0)?,
                    target_kind: r.get(1)?,
                    target_id: r.get(2)?,
                    local_parent_id: r.get(3)?,
                    position: r.get(4)?,
                })
            })
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// Drop every placement that points at a local folder which no longer exists.
    ///
    /// Without this, deleting a local folder would strand received content in a parent nobody can
    /// see: the placement would still name the dead id, the merge would find no host for it, and
    /// the shared Space would simply vanish from the sidebar with no way to get it back.
    pub fn prune_orphan_local_placements(&self) -> Result<u32> {
        let conn = self.lock();
        let removed = conn
            .execute(
                "DELETE FROM org_local_placements
                  WHERE local_parent_id IS NOT NULL
                    AND local_parent_id NOT IN (SELECT id FROM folders)",
                [],
            )
            .map_err(map_err)?;
        Ok(removed as u32)
    }
}

#[cfg(test)]
#[path = "db_tests/container_tests.rs"]
mod container_tests;
