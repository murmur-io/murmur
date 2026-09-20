//! Metadata-only candidate and relation reads for the deterministic Smart-organize planner.
//!
//! Three properties are load-bearing here and are asserted by the command-level tests:
//!
//! 1. **Raw-open only.** Smart organize never plans against a sealed container, not even a
//!    session-unlocked one, so every query hard-codes the EMPTY unlock set
//!    (`f.locked IS NULL OR f.locked = 0`) rather than accepting a caller-supplied set. A move can
//!    therefore never de-seal content.
//! 2. **Immutable date sources.** A note is bucketed by `documents.created_at` and a recording by
//!    `meetings.started_at`. `ItemRow.sort_at` projects `updated_at` for notes, so editing an old
//!    note would silently re-bucket it; this reader never touches that column.
//! 3. **No content.** Titles are needed for the preview; bodies, transcripts, paths, tasks and
//!    dashboards are not read at all.

use crate::error::Result;
use rusqlite::OptionalExtension;

use crate::storage::db::{map_err, Db};

/// One planning candidate: identity, current owner, and its immutable event time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SmartOrganizeCandidateRow {
    pub id: String,
    pub title: Option<String>,
    /// The container that owns the item RIGHT NOW. Apply compares against this exact value.
    pub container_id: Option<String>,
    /// Epoch MILLISECONDS, UTC. Notes: `documents.created_at`. Recordings: `meetings.started_at`.
    pub event_at_ms: i64,
}

/// One eligible relation endpoint of a recording, already filtered to the policy the rule states.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SmartOrganizeEndpointRow {
    pub meeting_id: String,
    pub endpoint_kind: String,
    pub endpoint_id: String,
}

fn placeholders(count: usize) -> String {
    (0..count).map(|_| "?").collect::<Vec<_>>().join(", ")
}

/// Relation evidence Smart organize accepts. Companion edges are the recording's OWN note and
/// would group every recording with itself; suggested/dismissed semantic edges are not a user
/// decision and must never move a file.
const ELIGIBLE_EDGE_PREDICATE: &str = "l.status = 'active'
     AND l.edge_type <> 'companion'
     AND (l.edge_type IN ('manual', 'wikilink')
          OR (l.edge_type = 'semantic' AND l.created_by = 'accepted'))";

/// Endpoint kinds a relation bucket may be named after. `meeting` would group two recordings under
/// a third recording, and `org`/`container` endpoints are authorization edges, not user relations.
const ENDPOINT_KINDS: &str = "('note', 'document', 'container')";

impl Db {
    /// Authored notes (never meeting companions) owned by one of `container_ids`, newest event
    /// first, capped at `limit`. Returns `(rows, total_matching)` so the caller can report an
    /// honest deferred count without reading the deferred rows.
    pub(crate) fn smart_organize_note_candidates(
        &self,
        container_ids: &[String],
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<SmartOrganizeCandidateRow>, u32)> {
        if container_ids.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let conn = self.lock();
        let ids = placeholders(container_ids.len());
        let where_sql = format!(
            "FROM documents d
             JOIN folders f ON f.id = d.folder_id
             WHERE d.folder_id IN ({ids})
               AND (f.locked IS NULL OR f.locked = 0)
               AND d.kind = 'note'
               AND d.meeting_id IS NULL"
        );
        let params = rusqlite::params_from_iter(container_ids.iter());
        let total: i64 = conn
            .query_row(&format!("SELECT COUNT(*) {where_sql}"), params, |row| {
                row.get(0)
            })
            .map_err(map_err)?;
        let sql = format!(
            "SELECT d.id, COALESCE(NULLIF(TRIM(d.title),''), d.name), d.folder_id, d.created_at {where_sql}
             ORDER BY d.created_at ASC, d.id ASC LIMIT {limit} OFFSET {offset}"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(container_ids.iter()), |row| {
                Ok(SmartOrganizeCandidateRow {
                    id: row.get(0)?,
                    title: row.get::<_, Option<String>>(1)?,
                    container_id: row.get(2)?,
                    event_at_ms: row.get(3)?,
                })
            })
            .map_err(map_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_err)?;
        Ok((rows, total.max(0) as u32))
    }

    /// Recordings owned by one of `container_ids` (or unfiled, when `unfiled` is set), oldest event
    /// first, capped at `limit`.
    pub(crate) fn smart_organize_meeting_candidates(
        &self,
        container_ids: &[String],
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<SmartOrganizeCandidateRow>, u32)> {
        let conn = self.lock();
        let ids = placeholders(container_ids.len());
        let selection = if container_ids.is_empty() {
            "m.folder_id IS NULL".to_string()
        } else {
            format!("m.folder_id IN ({ids})")
        };
        let visible =
            crate::storage::db::meeting_visibility_clause("m", &std::collections::HashSet::new());
        let where_sql = format!(
            "FROM meetings m LEFT JOIN folders f ON f.id = m.folder_id
             WHERE ({selection}) AND {visible} AND (f.locked IS NULL OR f.locked = 0)
             AND NOT EXISTS (
               WITH RECURSIVE protected(id) AS (
                 SELECT id FROM folders WHERE locked=1
                 UNION SELECT child.id FROM folders child JOIN protected p ON child.parent_id=p.id
               )
               SELECT 1 FROM protected p WHERE p.id=m.folder_id
                 OR EXISTS (SELECT 1 FROM notes n WHERE n.meeting_id=m.id AND n.folder_id=p.id)
                 OR EXISTS (SELECT 1 FROM documents d WHERE d.meeting_id=m.id AND d.folder_id=p.id)
             )"
        );
        let total: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) {where_sql}"),
                rusqlite::params_from_iter(container_ids.iter()),
                |row| row.get(0),
            )
            .map_err(map_err)?;
        let sql = format!(
            "SELECT m.id, NULL, m.folder_id, m.started_at {where_sql}
             ORDER BY m.started_at ASC, m.id ASC LIMIT {limit} OFFSET {offset}"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(container_ids.iter()), |row| {
                let started_at: String = row.get(3)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    started_at,
                ))
            })
            .map_err(map_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_err)?;
        let rows = rows
            .into_iter()
            .map(|(id, title, container_id, started_at)| {
                let event_at_ms = chrono::DateTime::parse_from_rfc3339(&started_at)
                    .ok()
                    .map(|date| date.timestamp_millis())
                    .unwrap_or(i64::MIN);
                SmartOrganizeCandidateRow {
                    id,
                    title,
                    container_id,
                    event_at_ms,
                }
            })
            .collect();
        Ok((rows, total.max(0) as u32))
    }

    /// Every eligible direct relation endpoint of `meeting_ids`, in a deterministic order. Both
    /// link directions are read: a wikilink is authored from the note side, a manual relation from
    /// either side.
    pub(crate) fn smart_organize_relation_endpoints(
        &self,
        meeting_ids: &[String],
    ) -> Result<Vec<SmartOrganizeEndpointRow>> {
        if meeting_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let ids = placeholders(meeting_ids.len());
        let sql = format!(
            "SELECT l.src_id, l.dst_kind, l.dst_id FROM links l
              WHERE l.src_kind = 'meeting' AND l.src_id IN ({ids})
                AND l.dst_kind IN {ENDPOINT_KINDS} AND {ELIGIBLE_EDGE_PREDICATE}
             UNION
             SELECT l.dst_id, l.src_kind, l.src_id FROM links l
              WHERE l.dst_kind = 'meeting' AND l.dst_id IN ({ids})
                AND l.src_kind IN {ENDPOINT_KINDS} AND {ELIGIBLE_EDGE_PREDICATE}"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let doubled = meeting_ids.iter().chain(meeting_ids.iter());
        let mut rows = stmt
            .query_map(rusqlite::params_from_iter(doubled), |row| {
                Ok(SmartOrganizeEndpointRow {
                    meeting_id: row.get(0)?,
                    endpoint_kind: row.get(1)?,
                    endpoint_id: row.get(2)?,
                })
            })
            .map_err(map_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_err)?;
        rows.sort();
        rows.dedup();
        Ok(rows)
    }

    /// EVERY direct child container of `parent_id` whose name case-folds to `name`.
    ///
    /// Deliberately not `child_folder_by_name`, which returns the first EXACT-name match: two
    /// legacy folders differing only in case would make bucket resolution depend on row order, and
    /// a macOS user reads them as one folder.
    pub(crate) fn smart_organize_child_folders_case_folded(
        &self,
        parent_id: &str,
        name: &str,
    ) -> Result<Vec<(String, String)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id, name, COALESCE(locked, 0) FROM folders WHERE parent_id = ?1")
            .map_err(map_err)?;
        let folded = name.to_lowercase();
        let rows = stmt
            .query_map(rusqlite::params![parent_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(map_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_err)?;
        let mut matches = rows
            .into_iter()
            .filter(|(_, candidate, _)| candidate.to_lowercase() == folded)
            .map(|(id, candidate, _)| (id, candidate))
            .collect::<Vec<_>>();
        matches.sort();
        Ok(matches)
    }
}

impl Db {
    /// `documents.created_at` (epoch ms) — the note's IMMUTABLE date source.
    pub(crate) fn document_created_at_ms(&self, id: &str) -> Result<Option<i64>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT created_at FROM documents WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(map_err)
    }

    /// `meetings.started_at` as epoch ms — the recording's IMMUTABLE date source.
    pub(crate) fn meeting_started_at_ms(&self, id: &str) -> Result<Option<i64>> {
        let conn = self.lock();
        let started: Option<String> = conn
            .query_row(
                "SELECT started_at FROM meetings WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_err)?;
        Ok(started.and_then(|value| {
            chrono::DateTime::parse_from_rfc3339(&value)
                .ok()
                .map(|parsed| parsed.timestamp_millis())
        }))
    }

    pub(crate) fn smart_organize_meeting_owner(&self, id: &str) -> Result<Option<String>> {
        self.lock()
            .query_row(
                "SELECT folder_id FROM meetings WHERE id=?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .map_err(map_err)
    }

    /// Content-free endpoint ownership: authorization must precede reading its name.
    pub(crate) fn smart_organize_document_owner(
        &self,
        id: &str,
        kind: &str,
    ) -> Result<Option<String>> {
        self.lock()
            .query_row(
                "SELECT folder_id FROM documents WHERE id=?1 AND kind=?2",
                rusqlite::params![id, kind],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_err)
    }

    /// A document's display name, used only to NAME a relation bucket.
    pub(crate) fn document_title(&self, id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT COALESCE(NULLIF(TRIM(title), ''), name) FROM documents WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(map_err)
    }
}
