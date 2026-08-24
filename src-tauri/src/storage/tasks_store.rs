//! Org Task storage.
//!
//! `org_tasks` is the SQLCipher-canonical projection of Task envelopes. `documents(kind='task')`
//! stores only an origin device's editable source payload so the existing stable org-document CAS
//! machinery can recover and republish it. `task_local_refs` never egresses.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, Transaction};

use crate::error::{AppError, Result};
use crate::share::task_envelope::{TaskEnvelope, TaskOrgRef};
use crate::storage::db::{map_err, Db};

pub const TASK_FOLDER_ID: &str = "00000000-0000-4000-8000-00000000a501";
const TASK_FOLDER_PATH: &str = ".murmur/tasks";
/// Hard response bound for the top-level Task view. The ordering is total, so a later paginated
/// surface can continue from the final `(status,due_at,updated_at,id)` tuple without changing which
/// rows are admitted today.
pub(crate) const TASK_LIST_LIMIT: i64 = 500;
/// Dashboard Work is navigation chrome, not an unbounded second Task export surface.
pub(crate) const DASHBOARD_TASK_LIMIT: i64 = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgTaskRow {
    pub id: String,
    pub org_id: String,
    pub doc_id: String,
    pub item_id: String,
    pub source_document_id: Option<String>,
    pub envelope_json: String,
    pub access: String,
    pub author_user_id: Option<String>,
    pub owner_user_id: Option<String>,
    pub rev: u32,
    pub generation: u32,
    pub seq: u64,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLocalRefRow {
    pub kind: String,
    pub ref_id: String,
    pub position: u32,
}

impl Db {
    pub(crate) fn migrate_tasks(conn: &rusqlite::Connection) -> Result<()> {
        conn.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS org_tasks (
               id TEXT PRIMARY KEY,
               org_id TEXT NOT NULL,
               doc_id TEXT NOT NULL,
               item_id TEXT NOT NULL UNIQUE,
               source_document_id TEXT,
               envelope_json TEXT NOT NULL,
               status TEXT NOT NULL CHECK(status IN ('todo','inProgress','done')),
               due_at TEXT,
               assignee_user_id TEXT,
               access TEXT NOT NULL CHECK(access IN ('view','edit')),
               author_user_id TEXT,
               owner_user_id TEXT,
               rev INTEGER NOT NULL CHECK(rev > 0),
               generation INTEGER NOT NULL CHECK(generation > 0),
               seq INTEGER NOT NULL CHECK(seq >= 0),
               updated_at TEXT NOT NULL,
               UNIQUE(org_id,doc_id)
             );
             CREATE INDEX IF NOT EXISTS idx_org_tasks_status_due
               ON org_tasks(status,due_at);
             CREATE INDEX IF NOT EXISTS idx_org_tasks_org
               ON org_tasks(org_id,status,due_at);
             CREATE UNIQUE INDEX IF NOT EXISTS idx_org_tasks_source_document
               ON org_tasks(source_document_id) WHERE source_document_id IS NOT NULL;
             CREATE TABLE IF NOT EXISTS task_org_refs (
               task_id TEXT NOT NULL,
               org_id TEXT NOT NULL,
               doc_id TEXT NOT NULL,
               position INTEGER NOT NULL CHECK(position >= 0),
               PRIMARY KEY(task_id,org_id,doc_id),
               FOREIGN KEY(task_id) REFERENCES org_tasks(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_task_org_refs_target
               ON task_org_refs(org_id,doc_id,position);
             CREATE TABLE IF NOT EXISTS task_local_refs (
               task_id TEXT NOT NULL,
               kind TEXT NOT NULL CHECK(kind IN ('note','meeting','dashboard')),
               ref_id TEXT NOT NULL,
               position INTEGER NOT NULL CHECK(position >= 0),
               PRIMARY KEY(task_id,kind,ref_id),
               FOREIGN KEY(task_id) REFERENCES org_tasks(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_task_local_refs_dashboard
               ON task_local_refs(kind,ref_id,position);
             CREATE TRIGGER IF NOT EXISTS task_folder_update_guard
             BEFORE UPDATE ON folders
             WHEN OLD.kind='task' OR OLD.id='{TASK_FOLDER_ID}'
             BEGIN SELECT RAISE(ABORT,'task folder is internal'); END;
             CREATE TRIGGER IF NOT EXISTS task_folder_delete_guard
             BEFORE DELETE ON folders
             WHEN OLD.kind='task' OR OLD.id='{TASK_FOLDER_ID}'
             BEGIN SELECT RAISE(ABORT,'task folder is internal'); END;
             CREATE TRIGGER IF NOT EXISTS task_folder_child_guard
             BEFORE INSERT ON folders
             WHEN NEW.parent_id='{TASK_FOLDER_ID}'
             BEGIN SELECT RAISE(ABORT,'task folder is internal'); END;
             CREATE TRIGGER IF NOT EXISTS task_document_insert_scope_guard
             BEFORE INSERT ON documents
             WHEN (NEW.kind='task' AND NEW.folder_id<>'{TASK_FOLDER_ID}')
               OR (NEW.kind<>'task' AND NEW.folder_id='{TASK_FOLDER_ID}')
             BEGIN SELECT RAISE(ABORT,'invalid task document scope'); END;
             CREATE TRIGGER IF NOT EXISTS task_document_update_scope_guard
             BEFORE UPDATE OF folder_id,kind ON documents
             WHEN (NEW.kind='task' AND NEW.folder_id<>'{TASK_FOLDER_ID}')
               OR (NEW.kind<>'task' AND NEW.folder_id='{TASK_FOLDER_ID}')
             BEGIN SELECT RAISE(ABORT,'invalid task document scope'); END;"
        ))
        .map_err(map_err)?;
        Ok(())
    }

    pub fn create_task_source(
        &self,
        source_id: &str,
        title: &str,
        envelope_json: &str,
        created_at_ms: i64,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        tx.execute(
            "INSERT INTO folders(id,name,path,parent_id,locked,wrapped_key,created_at,kind)
             VALUES(?1,'Tasks',?2,NULL,0,NULL,?3,'task')
             ON CONFLICT(id) DO NOTHING",
            rusqlite::params![TASK_FOLDER_ID, TASK_FOLDER_PATH, created_at_ms.to_string()],
        )
        .map_err(map_err)?;
        let folder_is_exact = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM folders
                  WHERE id=?1 AND kind='task' AND path=?2 AND parent_id IS NULL
                    AND locked=0 AND wrapped_key IS NULL)",
                rusqlite::params![TASK_FOLDER_ID, TASK_FOLDER_PATH],
                |row| row.get::<_, i64>(0),
            )
            .map_err(map_err)?
            != 0;
        if !folder_is_exact {
            return Err(AppError::Storage(
                "internal task folder witness is invalid".into(),
            ));
        }
        tx.execute(
            "INSERT INTO documents
               (id,folder_id,name,title,text,kind,text_blob,created_at,updated_at,exported_path)
             VALUES(?1,?2,?3,?3,?4,'task',NULL,?5,?5,NULL)",
            rusqlite::params![
                source_id,
                TASK_FOLDER_ID,
                title,
                envelope_json,
                created_at_ms
            ],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// File a task into a local container, or clear its placement with `None`.
    ///
    /// Returns false when no task carries that id, so the caller can refuse rather than report a
    /// success that moved nothing.
    pub(crate) fn set_task_container(&self, task_id: &str, container_id: Option<&str>) -> Result<bool> {
        let conn = self.lock();
        let n = conn
            .execute(
                "UPDATE org_tasks SET container_id = ?2 WHERE id = ?1",
                rusqlite::params![task_id, container_id],
            )
            .map_err(map_err)?;
        Ok(n > 0)
    }

    /// Unfile every task in a container, returning how many moved.
    ///
    /// Called when the container is SEALED. A task's content lives in an org's E2EE store, so a
    /// container's content key cannot seal it — which means a task left inside a sealed container
    /// would stay exactly as readable as before while the user was told the container was locked.
    /// That is the one outcome the lock model must never produce, so the task leaves the container
    /// instead. Nothing is lost: the task is intact and unfiled, precisely where it was before
    /// anyone filed it, and it is still listed in the Tasks view.
    pub(crate) fn unfile_tasks_in_container(&self, container_id: &str) -> Result<usize> {
        let conn = self.lock();
        let n = conn
            .execute(
                "UPDATE org_tasks SET container_id = NULL WHERE container_id = ?1",
                rusqlite::params![container_id],
            )
            .map_err(map_err)?;
        Ok(n)
    }

    /// The container a task is filed in, or `None` when it is unfiled. TESTS ONLY.
    ///
    /// RAW: no visibility predicate. An earlier version of this comment justified that by saying
    /// the seal and relock paths use it — they do not; they use
    /// [`Db::unfile_tasks_in_container`], which needs no per-task read at all.
    ///
    /// It survives because the unfiling tests need it and CANNOT use the gated variant. They
    /// assert that a task is no longer filed after a container is sealed, and
    /// `task_container_visible` returns `None` for a sealed container whether the task is filed
    /// there or not — so asserting `None` through it would pass on a relock that unfiled nothing.
    /// The raw read is what makes that assertion mean something.
    #[cfg(test)]
    pub(crate) fn task_container(&self, task_id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT container_id FROM org_tasks WHERE id = ?1",
            rusqlite::params![task_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(map_err)
        .map(Option::flatten)
    }

    /// The container a task is filed in, as the FE may see it.
    ///
    /// A sealed-and-not-unlocked container yields `None`. "Which sealed container is this item
    /// in" is a disclosure this diff already decided to withhold elsewhere: `mask_board` drops
    /// `folder_id` for exactly this reason, because keeping it lets a caller group items by
    /// sealed folder and count them. Tasks are normally unfiled by both the seal and the relock,
    /// so the window is narrow — but "narrow" is not the same as closed, and two readers in one
    /// change should not take opposite positions on the same fact.
    pub(crate) fn task_container_visible(
        &self,
        task_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<String>> {
        let visible = crate::storage::visibility_clause("f", unlocked);
        let conn = self.lock();
        conn.query_row(
            &format!(
                "SELECT t.container_id
                   FROM org_tasks t
                   LEFT JOIN folders f ON f.id = t.container_id
                  WHERE t.id = ?1 AND (t.container_id IS NULL OR {visible})"
            ),
            rusqlite::params![task_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(map_err)
        .map(Option::flatten)
    }

    pub fn task_source(&self, source_id: &str) -> Result<Option<(String, String, i64)>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT COALESCE(title,name),text,created_at
               FROM documents WHERE id=?1 AND kind='task' AND text_blob IS NULL",
            [source_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(map_err)
    }

    pub fn update_task_source(
        &self,
        source_id: &str,
        title: &str,
        envelope_json: &str,
        updated_at_ms: i64,
    ) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let changed = tx
            .execute(
                "UPDATE documents SET title=?2,text=?3,updated_at=?4
              WHERE id=?1 AND kind='task' AND text_blob IS NULL",
                rusqlite::params![source_id, title, envelope_json, updated_at_ms],
            )
            .map_err(map_err)?;
        if changed == 1 {
            tx.execute(
                "UPDATE org_shares
                    SET republish_dirty=republish_dirty+1,republish_deferred=0
                  WHERE document_id=?1 AND state IN ('queued','uploaded','failed')",
                [source_id],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(changed == 1)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn upsert_org_task_projection_tx(
        tx: &Transaction<'_>,
        item_id: &str,
        org_id: &str,
        doc_id: Option<&str>,
        envelope_json: &str,
        access: &str,
        author_user_id: Option<&str>,
        owner_user_id: Option<&str>,
        rev: u32,
        generation: u32,
        seq: u64,
    ) -> Result<()> {
        let Some(doc_id) = doc_id else {
            return Err(AppError::InvalidArg(
                "shared task omitted its stable document id".into(),
            ));
        };
        let envelope = TaskEnvelope::from_json(envelope_json, org_id)?;
        let id = format!("{org_id}:{doc_id}");
        // Tasks share the authenticated org feed row for permissions/revision lineage, but they are
        // not Brain/Ask corpus. Remove every derived chunk/vector in the SAME projection transaction
        // so even a future generic reader missing its `source_kind` predicate has no task text to
        // retrieve. The external-content FTS delete trigger follows the `org_chunks` deletion.
        tx.execute(
            "DELETE FROM org_vec_chunks WHERE chunk_id IN
               (SELECT id FROM org_chunks WHERE item_id=?1)",
            [item_id],
        )
        .map_err(map_err)?;
        tx.execute("DELETE FROM org_chunks WHERE item_id=?1", [item_id])
            .map_err(map_err)?;
        let source_document_id: Option<String> = tx
            .query_row(
                "SELECT document_id FROM org_shares
                  WHERE org_id=?1 AND doc_id=?2 AND kind='task'
                    AND document_id IS NOT NULL
                  ORDER BY created_at LIMIT 1",
                rusqlite::params![org_id, doc_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_err)?;
        tx.execute(
            "INSERT INTO org_tasks
               (id,org_id,doc_id,item_id,source_document_id,envelope_json,status,due_at,
                assignee_user_id,access,author_user_id,owner_user_id,rev,generation,seq,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
             ON CONFLICT(id) DO UPDATE SET
               item_id=excluded.item_id,
               source_document_id=COALESCE(org_tasks.source_document_id,excluded.source_document_id),
               envelope_json=excluded.envelope_json,status=excluded.status,due_at=excluded.due_at,
               assignee_user_id=excluded.assignee_user_id,access=excluded.access,
               author_user_id=excluded.author_user_id,owner_user_id=excluded.owner_user_id,
               rev=excluded.rev,generation=excluded.generation,seq=excluded.seq,
               updated_at=excluded.updated_at",
            rusqlite::params![
                id,
                org_id,
                doc_id,
                item_id,
                source_document_id,
                envelope_json,
                envelope.status.as_str(),
                envelope.due_at,
                envelope.assignee_user_id,
                access,
                author_user_id,
                owner_user_id,
                rev as i64,
                generation as i64,
                seq as i64,
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .map_err(map_err)?;
        tx.execute("DELETE FROM task_org_refs WHERE task_id=?1", [&id])
            .map_err(map_err)?;
        for (position, reference) in envelope.org_refs.iter().enumerate() {
            tx.execute(
                "INSERT INTO task_org_refs(task_id,org_id,doc_id,position)
                 VALUES(?1,?2,?3,?4)",
                rusqlite::params![id, reference.org_id, reference.doc_id, position as i64],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    pub(crate) fn delete_org_task_projection_tx(
        tx: &Transaction<'_>,
        item_id: &str,
    ) -> Result<bool> {
        tx.execute("DELETE FROM org_tasks WHERE item_id=?1", [item_id])
            .map(|changed| changed != 0)
            .map_err(map_err)
    }

    pub fn delete_task_source(&self, source_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM documents WHERE id=?1 AND kind='task'",
            [source_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    pub fn list_org_tasks(&self, org_id: Option<&str>) -> Result<Vec<OrgTaskRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT t.id,t.org_id,t.doc_id,t.item_id,t.source_document_id,t.envelope_json,t.access,
                        t.author_user_id,t.owner_user_id,t.rev,t.generation,t.seq,t.updated_at
                   FROM org_tasks t
                   JOIN org_state os ON os.org_id=t.org_id AND os.context_enabled=1
                  WHERE (?1 IS NULL OR t.org_id=?1)
                  ORDER BY CASE status WHEN 'inProgress' THEN 0 WHEN 'todo' THEN 1 ELSE 2 END,
                           due_at IS NULL,due_at,updated_at DESC,t.id
                  LIMIT ?2",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![org_id, TASK_LIST_LIMIT], |row| {
                Ok(OrgTaskRow {
                    id: row.get(0)?,
                    org_id: row.get(1)?,
                    doc_id: row.get(2)?,
                    item_id: row.get(3)?,
                    source_document_id: row.get(4)?,
                    envelope_json: row.get(5)?,
                    access: row.get(6)?,
                    author_user_id: row.get(7)?,
                    owner_user_id: row.get(8)?,
                    rev: row.get::<_, i64>(9)? as u32,
                    generation: row.get::<_, i64>(10)? as u32,
                    seq: row.get::<_, i64>(11)? as u64,
                    updated_at: row.get(12)?,
                })
            })
            .map_err(map_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(map_err)
    }

    pub fn get_org_task(&self, id: &str) -> Result<Option<OrgTaskRow>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT t.id,t.org_id,t.doc_id,t.item_id,t.source_document_id,t.envelope_json,t.access,
                    t.author_user_id,t.owner_user_id,t.rev,t.generation,t.seq,t.updated_at
               FROM org_tasks t
               JOIN org_state os ON os.org_id=t.org_id AND os.context_enabled=1
              WHERE t.id=?1",
            [id],
            |row| {
                Ok(OrgTaskRow {
                    id: row.get(0)?,
                    org_id: row.get(1)?,
                    doc_id: row.get(2)?,
                    item_id: row.get(3)?,
                    source_document_id: row.get(4)?,
                    envelope_json: row.get(5)?,
                    access: row.get(6)?,
                    author_user_id: row.get(7)?,
                    owner_user_id: row.get(8)?,
                    rev: row.get::<_, i64>(9)? as u32,
                    generation: row.get::<_, i64>(10)? as u32,
                    seq: row.get::<_, i64>(11)? as u64,
                    updated_at: row.get(12)?,
                })
            },
        )
        .optional()
        .map_err(map_err)
    }

    /// Content-free admission lookup used before command code loads `envelope_json`.
    pub(crate) fn org_task_org_for_id(&self, id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row("SELECT org_id FROM org_tasks WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(map_err)
    }

    pub(crate) fn visible_org_task_for_item(&self, item_id: &str) -> Result<bool> {
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM org_tasks t
                 JOIN org_state os ON os.org_id=t.org_id AND os.context_enabled=1
                 WHERE t.item_id=?1
             )",
            [item_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|exists| exists != 0)
        .map_err(map_err)
    }

    /// The org owning a retained Task projection. Deliberately does not apply the context gate: a
    /// caller uses this only to recognize that an attachment belongs to Task content and must then
    /// pass the command-level live session + membership/context gate before any bytes are returned.
    pub(crate) fn org_task_org_for_item(&self, item_id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT org_id FROM org_tasks WHERE item_id=?1",
            [item_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_err)
    }

    /// Source-backed twin of [`Self::org_task_org_for_item`]. A hidden `documents(kind='task')`
    /// row remains recognizable even after context/membership invalidation, so it cannot fall
    /// through to the ordinary always-open document attachment gate.
    pub(crate) fn org_task_org_for_source(
        &self,
        source_document_id: &str,
    ) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT t.org_id FROM org_tasks t
              JOIN documents d ON d.id=t.source_document_id AND d.kind='task'
             WHERE t.source_document_id=?1",
            [source_document_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_err)
    }

    pub(crate) fn is_task_source(&self, source_document_id: &str) -> Result<bool> {
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM documents WHERE id=?1 AND kind='task')",
            [source_document_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|exists| exists != 0)
        .map_err(map_err)
    }

    /// Resolve the one context-visible shared Task backed by an origin device's hidden source.
    /// The partial unique index on `source_document_id` makes ambiguity fail at persistence time.
    pub(crate) fn visible_org_task_item_for_source(
        &self,
        source_document_id: &str,
    ) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT t.item_id FROM org_tasks t
               JOIN org_state os ON os.org_id=t.org_id AND os.context_enabled=1
               JOIN org_items i ON i.item_id=t.item_id AND i.tombstoned=0 AND i.is_current=1
              WHERE t.source_document_id=?1",
            [source_document_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_err)
    }

    /// Command-boundary witness for an encrypted Task-to-Task reference. Structural JSON
    /// validation cannot distinguish a Task docId from a Note or a tombstoned document.
    pub(crate) fn visible_org_task_ref(&self, org_id: &str, doc_id: &str) -> Result<bool> {
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM org_tasks t
                 JOIN org_state os ON os.org_id=t.org_id AND os.context_enabled=1
                 JOIN org_items i ON i.item_id=t.item_id AND i.tombstoned=0 AND i.is_current=1
                 WHERE t.org_id=?1 AND t.doc_id=?2
             )",
            rusqlite::params![org_id, doc_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|exists| exists != 0)
        .map_err(map_err)
    }

    /// Return only Task-to-Task edges whose targets are currently authenticated live Task
    /// projections. The raw encrypted envelope remains canonical, while this deferred edge table
    /// makes feed ordering harmless: a referenced Task may arrive later in the same page, but a
    /// Note/nonexistent/tombstoned docId is never exposed as a Task reference.
    pub(crate) fn visible_task_org_refs(&self, task_id: &str) -> Result<Vec<TaskOrgRef>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT r.org_id,r.doc_id
                   FROM task_org_refs r
                   JOIN org_tasks source
                     ON source.id=r.task_id AND source.org_id=r.org_id
                   JOIN org_tasks target
                     ON target.org_id=r.org_id AND target.doc_id=r.doc_id
                   JOIN org_state os
                     ON os.org_id=target.org_id AND os.context_enabled=1
                   JOIN org_items i
                     ON i.item_id=target.item_id AND i.tombstoned=0 AND i.is_current=1
                  WHERE r.task_id=?1
                  ORDER BY r.position,r.org_id,r.doc_id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([task_id], |row| {
                Ok(TaskOrgRef {
                    org_id: row.get(0)?,
                    doc_id: row.get(1)?,
                })
            })
            .map_err(map_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(map_err)
    }

    pub fn replace_task_local_refs(&self, task_id: &str, refs: &[TaskLocalRefRow]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        if tx
            .query_row(
                "SELECT 1 FROM org_tasks t
                   JOIN org_state os ON os.org_id=t.org_id AND os.context_enabled=1
                  WHERE t.id=?1",
                [task_id],
                |_| Ok(()),
            )
            .optional()
            .map_err(map_err)?
            .is_none()
        {
            return Err(AppError::InvalidArg("no such task".into()));
        }
        for row in refs {
            if row.ref_id.trim().is_empty() {
                return Err(AppError::InvalidArg("invalid local task reference".into()));
            }
            let target_exists = match row.kind.as_str() {
                "note" => tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM documents WHERE id=?1 AND kind='note')",
                        [&row.ref_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(map_err)?,
                "meeting" => tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM meetings WHERE id=?1)",
                        [&row.ref_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(map_err)?,
                "dashboard" => tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM dashboards WHERE id=?1)",
                        [&row.ref_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(map_err)?,
                _ => return Err(AppError::InvalidArg("invalid local task reference".into())),
            };
            if target_exists == 0 {
                return Err(AppError::InvalidArg(
                    "local task reference target does not exist".into(),
                ));
            }
        }
        tx.execute("DELETE FROM task_local_refs WHERE task_id=?1", [task_id])
            .map_err(map_err)?;
        for row in refs {
            tx.execute(
                "INSERT INTO task_local_refs(task_id,kind,ref_id,position) VALUES(?1,?2,?3,?4)",
                rusqlite::params![task_id, row.kind, row.ref_id, row.position as i64],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    pub fn task_local_refs(&self, task_id: &str) -> Result<Vec<TaskLocalRefRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT r.kind,r.ref_id,r.position FROM task_local_refs r
                  WHERE r.task_id=?1 AND (
                    (r.kind='note' AND EXISTS(
                      SELECT 1 FROM documents d WHERE d.id=r.ref_id AND d.kind='note')) OR
                    (r.kind='meeting' AND EXISTS(
                      SELECT 1 FROM meetings m WHERE m.id=r.ref_id)) OR
                    (r.kind='dashboard' AND EXISTS(
                      SELECT 1 FROM dashboards d WHERE d.id=r.ref_id))
                  )
                  ORDER BY r.position,r.kind,r.ref_id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([task_id], |row| {
                Ok(TaskLocalRefRow {
                    kind: row.get(0)?,
                    ref_id: row.get(1)?,
                    position: row.get::<_, i64>(2)? as u32,
                })
            })
            .map_err(map_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(map_err)
    }

    pub fn dashboard_task_rows(&self, dashboard_id: &str) -> Result<Vec<OrgTaskRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT t.id,t.org_id,t.doc_id,t.item_id,t.source_document_id,t.envelope_json,
                        t.access,t.author_user_id,t.owner_user_id,t.rev,t.generation,t.seq,t.updated_at
                   FROM task_local_refs r JOIN org_tasks t ON t.id=r.task_id
                  JOIN dashboards d ON d.id=r.ref_id
                  JOIN org_state os ON os.org_id=t.org_id AND os.context_enabled=1
                  WHERE r.kind='dashboard' AND r.ref_id=?1
                  ORDER BY r.position,t.updated_at DESC,t.id
                  LIMIT ?2",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(
                rusqlite::params![dashboard_id, DASHBOARD_TASK_LIMIT],
                |row| {
                    Ok(OrgTaskRow {
                        id: row.get(0)?,
                        org_id: row.get(1)?,
                        doc_id: row.get(2)?,
                        item_id: row.get(3)?,
                        source_document_id: row.get(4)?,
                        envelope_json: row.get(5)?,
                        access: row.get(6)?,
                        author_user_id: row.get(7)?,
                        owner_user_id: row.get(8)?,
                        rev: row.get::<_, i64>(9)? as u32,
                        generation: row.get::<_, i64>(10)? as u32,
                        seq: row.get::<_, i64>(11)? as u64,
                        updated_at: row.get(12)?,
                    })
                },
            )
            .map_err(map_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(map_err)
    }

    /// Content-free dashboard admission set. Every org is gated before the corresponding Task
    /// envelope JSON is selected by [`Self::dashboard_task_rows`].
    pub(crate) fn dashboard_task_org_ids(&self, dashboard_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT t.org_id
                   FROM task_local_refs r JOIN org_tasks t ON t.id=r.task_id
                   JOIN dashboards d ON d.id=r.ref_id
                   JOIN org_state os ON os.org_id=t.org_id AND os.context_enabled=1
                  WHERE r.kind='dashboard' AND r.ref_id=?1
                  ORDER BY t.org_id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([dashboard_id], |row| row.get(0))
            .map_err(map_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(map_err)
    }
}
