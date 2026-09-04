//! Related-picker readers — the gated hierarchy + leaf paging the "Add related" modal walks.
//!
//! This module is READ-ONLY: no seal, no key, no write, and — critically — NO new visibility
//! predicate. Every leaf query is built from [`crate::storage::db::visibility_clause`] /
//! [`crate::storage::db::meeting_visibility_clause`], the same two clauses `list_notes_visible`,
//! `list_meetings_visible` and `list_link_candidates_visible` already express, so a
//! sealed-and-not-session-unlocked container contributes NOTHING: no rows, no totals, no search
//! hits, no anchor position.
//!
//! # Why it is not `workspace_store`
//!
//! The sidebar's forest ([`crate::storage::workspace_store`]) answers a different question. It is
//! keyed on the global [`crate::storage::models::ItemKind`] (which carries `task`/`dashboard` —
//! kinds the picker must NEVER offer), it caps each container at a handful of rows for a
//! *bounded tree payload*, and it has no notion of "the page that contains THIS item". The picker
//! needs the opposite: exactly three linkable leaf kinds, a stable total ordering per scope, and
//! the ability to answer "where in that ordering does the anchor sit" so the modal can open
//! centred on item #150 without shipping the other 149. Reusing the tree's cached, capped payload
//! for that would silently truncate a large vault; reusing the flat `list_link_candidates` feed
//! would lose the hierarchy the picker exists to show.
//!
//! # The three properties that are load-bearing
//!
//! 1. **A locked container discloses its NAME and nothing else.** Names are already disclosed by
//!    the sidebar (you have to see a container in order to unlock it); child titles, item ids,
//!    totals and search hits are not. The gate delivers this by construction — a sealed
//!    container's leaves fail the clause on every leg — and the command layer restates it.
//! 2. **No row carries an on-disk path.** `get_meeting_detail` nulls `audio_path` for a locked
//!    meeting precisely because the FE feeds any path it receives into `convertFileSrc`, the one
//!    audio read that bypasses `export_audio`. A picker row must never reopen that door, so these
//!    DTOs have no path field at all.
//! 3. **The ordering is STABLE and shared between page, total and index.** The page query, its
//!    COUNT twin and the anchor-position query are generated from ONE `SELECT` (the COUNT-twin
//!    discipline `list_link_candidates_visible` established), so "the anchor is row 153" and "row
//!    153 of that page is the anchor" cannot disagree.

use std::collections::HashSet;

use crate::error::Result;
use crate::storage::db::{map_err, meeting_visibility_clause, visibility_clause, Db};
use crate::storage::models::{PickerItemKind, PickerRow, PickerScope, PickerSearchRow};

/// Containers whose kind is outside this set are SYSTEM containers and never reach the picker.
/// Mirrors `workspace_store`'s predicate deliberately — two independently-written definitions of
/// "a container the user can see" is exactly how a machine-owned folder becomes a link target.
const USER_CONTAINER_KINDS: &str = "('meeting','note')";
const SYSTEM_PATH_ROOT: &str = ".murmur";
const SYSTEM_PATH_PREFIX: &str = ".murmur/%";

/// SQL predicate shared by ordinary picker containers and the reserved note root: user-owned,
/// never the machine-owned `.murmur/` subtree.
fn picker_user_container(alias: &str) -> String {
    format!(
        "COALESCE({alias}.kind, 'meeting') IN {USER_CONTAINER_KINDS}
         AND {alias}.path <> '{SYSTEM_PATH_ROOT}'
         AND {alias}.path NOT LIKE '{SYSTEM_PATH_PREFIX}'"
    )
}

/// SQL predicate: the `folders` row aliased `alias` is structurally a selectable Space/folder.
/// Reachability from a top-level Space is supplied separately by the command's one hierarchy index.
pub(crate) fn picker_renderable_container(alias: &str) -> String {
    format!(
        "{} AND COALESCE({alias}.is_root, 0) = 0
         AND COALESCE({alias}.level, 'folder') IN ('project','folder')",
        picker_user_container(alias)
    )
}

/// Pin a SQL leaf owner to the exact reachable-id set computed by `ContainerIndex` from the same
/// rows the command renders. This prevents an orphan or invalid-level row from appearing in search
/// or paging while being absent from the hierarchy.
fn picker_reachable_container(alias: &str, reachable: &HashSet<String>) -> String {
    if reachable.is_empty() {
        return "0".to_string();
    }
    let ids = reachable
        .iter()
        .map(|id| format!("'{}'", id.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{} AND {alias}.id IN ({ids})",
        picker_renderable_container(alias)
    )
}

/// SQL predicate: `container_expr` resolves to one of the hierarchy scopes whose VISIBLE,
/// root-first breadcrumb matched the user's query in the command layer.
///
/// The ids come from the command's resolved container index, not from user input, but they are
/// still quoted defensively exactly like the reachability predicate. An empty set is the constant
/// false predicate, keeping the title-only search byte-for-byte in charge when no hierarchy path
/// matched.
fn picker_container_in(container_expr: &str, matched: &HashSet<String>) -> String {
    if matched.is_empty() {
        return "0".to_string();
    }
    let ids = matched
        .iter()
        .map(|id| format!("'{}'", id.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    format!("({container_expr}) IN ({ids})")
}

/// The ONE storage-owned Notes root included in the command's scope-id set. Keeping the id
/// predicate here is intentional: `is_root=1` by itself is not identity and must not turn an
/// arbitrary flagged row into the picker's hidden `Not classified` container.
fn picker_canonical_notes_root(alias: &str, scope_ids: &HashSet<String>) -> String {
    if scope_ids.is_empty() {
        return "0".to_string();
    }
    let ids = scope_ids
        .iter()
        .map(|id| format!("'{}'", id.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{} AND COALESCE({alias}.is_root, 0) = 1 AND {alias}.id IN ({ids})",
        picker_user_container(alias)
    )
}

/// The ONE ordering every picker leaf query shares.
///
/// Newest-first, exactly like the workspace tree: the two time representations (RFC3339 TEXT for
/// meetings, epoch-ms INTEGER for documents) each project into their own slot and leave the other
/// NULL, and a single query only ever holds one kind, so exactly one term is significant. `item_id`
/// last makes the order TOTAL — which is what lets an offset mean the same thing across the page
/// query, its COUNT twin and the anchor-index probe.
const PICKER_ORDER: &str = "sort_text DESC, sort_ms DESC, item_id DESC";

/// The container that currently governs the `meetings` row aliased `m`, as ONE SQL expression.
///
/// A verbatim mirror of `workspace_store`'s attribution, and it has to be: if the picker decided a
/// recording lived somewhere else than the sidebar does, "the current item" would open under a
/// container the user has never seen it in. Canonical `meetings.folder_id` wins; the note-derived
/// fallback is legacy-only, gated by the same clause as the row itself, restricted to the same
/// renderable containers, and refused outright when an ambiguous provider split would otherwise
/// pick an arbitrary folder.
fn meeting_container_expr(unlocked: &HashSet<String>, reachable: &HashSet<String>) -> String {
    let visible = visibility_clause("f", unlocked);
    let reachable_f = picker_reachable_container("f", reachable);
    let reachable_cf = picker_reachable_container("cf", reachable);
    let legacy = format!(
        "(SELECT MIN(nf.folder_id) FROM notes nf
            JOIN folders f ON f.id = nf.folder_id
           WHERE nf.meeting_id = m.id AND {visible} AND {reachable_f}
          HAVING COUNT(DISTINCT nf.folder_id) = 1)",
    );
    format!(
        "COALESCE(
           (SELECT cf.id FROM folders cf WHERE cf.id = m.folder_id AND {reachable_cf}),
           CASE WHEN m.folder_id IS NULL THEN {legacy} END
         )"
    )
}

/// The visible-leaf `SELECT` for one scope + kind, projecting the same four columns for every kind
/// (`item_id, title, sort_text, sort_ms`) so the paging/index wrappers stay kind-agnostic.
///
/// `scope` is bound as `?1` when — and only when — the returned flag says the SQL declares it.
/// SQLite rejects a statement bound with a different parameter count than it declares, so the
/// producer states this rather than letting the caller infer it from the scope shape.
struct PickerQuery {
    sql: String,
    binds_scope: bool,
}

fn visible_leaves_sql(
    scope: &PickerScope,
    kind: PickerItemKind,
    unlocked: &HashSet<String>,
    reachable: &HashSet<String>,
) -> PickerQuery {
    match kind {
        PickerItemKind::Meeting => {
            // Canonical `meetings.folder_id` governs a recording even before a note exists; the
            // NULL case keeps the conservative legacy note-owned rule. Both come from the shipped
            // oracle rather than a predicate written here.
            let meeting_visible = meeting_visibility_clause("m", unlocked);
            let container = meeting_container_expr(unlocked, reachable);
            let (scope_pred, binds_scope) = match scope {
                // UNCLASSIFIED recordings are exactly the meetings with no renderable container —
                // the same set the sidebar calls "unfiled".
                PickerScope::Unclassified => (
                    " AND m.folder_id IS NULL
                       AND NOT EXISTS (
                         SELECT 1 FROM notes mn
                          WHERE mn.meeting_id = m.id AND mn.folder_id IS NOT NULL
                       )"
                    .to_string(),
                    false,
                ),
                PickerScope::Container(_) => (format!(" AND {container} = ?1"), true),
            };
            PickerQuery {
                binds_scope,
                sql: format!(
                    "SELECT m.id AS item_id,
                            m.title AS title,
                            m.started_at AS sort_text,
                            NULL AS sort_ms
                       FROM meetings m
                      WHERE {meeting_visible}{scope_pred}"
                ),
            }
        }
        PickerItemKind::Note | PickerItemKind::Document => {
            // `documents` holds BOTH authored notes (`kind='note'`) and imported documents
            // (`kind='document'`); they share an id space but never a row, so one leg with a bound
            // discriminator serves both. `d.meeting_id IS NULL` drops a recording's COMPANION note,
            // which belongs to its meeting's row — without it every recording would be offered
            // twice.
            let visible = visibility_clause("f", unlocked);
            let doc_kind = kind.document_kind();
            let (scope_pred, binds_scope) = match scope {
                // UNCLASSIFIED notes/documents are the ones filed in the RESERVED note root — the
                // always-open container the sidebar hides and presents as a section instead. This
                // is the one place `is_root` is INCLUDED rather than excluded.
                PickerScope::Unclassified => (
                    format!(
                        " AND {} AND COALESCE(f.level, 'folder') IN ('project','folder')",
                        picker_canonical_notes_root("f", reachable)
                    ),
                    false,
                ),
                PickerScope::Container(_) => (
                    format!(
                        " AND d.folder_id = ?1 AND {}",
                        picker_reachable_container("f", reachable)
                    ),
                    true,
                ),
            };
            PickerQuery {
                binds_scope,
                sql: format!(
                    "SELECT d.id AS item_id,
                            COALESCE(NULLIF(TRIM(d.title), ''), d.name) AS title,
                            NULL AS sort_text,
                            COALESCE(d.updated_at, d.created_at) AS sort_ms
                       FROM documents d
                       JOIN folders f ON f.id = d.folder_id
                      WHERE d.kind = '{doc_kind}' AND d.meeting_id IS NULL
                        AND {visible}{scope_pred}"
                ),
            }
        }
    }
}

/// Map the pinned four-column projection into a [`PickerRow`].
fn row_to_picker_row(kind: PickerItemKind, r: &rusqlite::Row<'_>) -> rusqlite::Result<PickerRow> {
    let id: String = r.get("item_id")?;
    let title: Option<String> = r.get("title")?;
    Ok(PickerRow {
        kind,
        id,
        title: title
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| kind.untitled_label().to_string()),
    })
}

impl Db {
    /// One PAGE of the linkable leaves of `(scope, kind)`, plus the true visible total.
    ///
    /// The COUNT twin shares the page query's exact `SELECT`, so a sealed leaf neither appears NOR
    /// inflates the total — the "N of M" the modal renders can never describe rows it is refusing
    /// to show.
    pub fn related_picker_page(
        &self,
        scope: &PickerScope,
        kind: PickerItemKind,
        offset: u32,
        limit: u32,
        unlocked: &HashSet<String>,
        reachable: &HashSet<String>,
    ) -> Result<(Vec<PickerRow>, u32)> {
        let PickerQuery {
            sql: inner,
            binds_scope,
        } = visible_leaves_sql(scope, kind, unlocked, reachable);
        let scope_param: Vec<String> = match scope {
            PickerScope::Container(id) if binds_scope => vec![id.clone()],
            _ => Vec::new(),
        };
        let next = scope_param.len() + 1;
        let conn = self.lock();

        let total: i64 = conn
            .prepare(&format!("SELECT COUNT(*) FROM ({inner})"))
            .map_err(map_err)?
            .query_row(rusqlite::params_from_iter(scope_param.iter()), |r| r.get(0))
            .map_err(map_err)?;

        let page_sql = format!(
            "SELECT item_id, title, sort_text, sort_ms FROM ({inner})
              ORDER BY {PICKER_ORDER} LIMIT ?{next} OFFSET ?{after}",
            after = next + 1,
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = scope_param
            .iter()
            .map(|id| Box::new(id.clone()) as Box<dyn rusqlite::ToSql>)
            .collect();
        params.push(Box::new(i64::from(limit)));
        params.push(Box::new(i64::from(offset)));

        let mut stmt = conn.prepare(&page_sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), move |r| {
                row_to_picker_row(kind, r)
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok((out, total as u32))
    }

    /// The 0-based POSITION of `item_id` inside the stable ordering of `(scope, kind)`, or `None`
    /// when it is not there (gone, filed elsewhere, or sealed-and-not-unlocked — indistinguishably).
    ///
    /// This is what lets the modal open ON the current item without shipping the vault: the count
    /// of rows that sort BEFORE it, over the SAME `SELECT` the page query uses. Expressed as a
    /// COUNT rather than a window function so it shares [`PICKER_ORDER`] literally instead of
    /// restating it — a second ordering here would put the anchor on the wrong page.
    pub fn related_picker_index_of(
        &self,
        scope: &PickerScope,
        kind: PickerItemKind,
        item_id: &str,
        unlocked: &HashSet<String>,
        reachable: &HashSet<String>,
    ) -> Result<Option<u32>> {
        let PickerQuery {
            sql: inner,
            binds_scope,
        } = visible_leaves_sql(scope, kind, unlocked, reachable);
        let scope_param: Vec<String> = match scope {
            PickerScope::Container(id) if binds_scope => vec![id.clone()],
            _ => Vec::new(),
        };
        let anchor_slot = scope_param.len() + 1;
        let conn = self.lock();

        // Present at all? (A row that is not in the visible set has no position, and must not be
        // reported as position 0.)
        let present_sql =
            format!("SELECT EXISTS(SELECT 1 FROM ({inner}) WHERE item_id = ?{anchor_slot})");
        let mut present_params: Vec<Box<dyn rusqlite::ToSql>> = scope_param
            .iter()
            .map(|id| Box::new(id.clone()) as Box<dyn rusqlite::ToSql>)
            .collect();
        present_params.push(Box::new(item_id.to_string()));
        let present: bool = conn
            .prepare(&present_sql)
            .map_err(map_err)?
            .query_row(rusqlite::params_from_iter(present_params.iter()), |r| {
                Ok(r.get::<_, i64>(0)? != 0)
            })
            .map_err(map_err)?;
        if !present {
            return Ok(None);
        }

        // How many visible rows sort STRICTLY BEFORE the anchor under `PICKER_ORDER`? Written as
        // the lexicographic comparison of the same three keys, NULL-normalised so the comparison
        // is total (SQLite's `>` is NULL-poisoned, and each kind leaves one key NULL for every row
        // — including the anchor's — so the normalisation is uniform, never a reordering).
        let before_sql = format!(
            "WITH rows AS ({inner}),
                  anchor AS (SELECT * FROM rows WHERE item_id = ?{anchor_slot})
             SELECT COUNT(*) FROM rows, anchor
              WHERE (COALESCE(rows.sort_text, ''), COALESCE(rows.sort_ms, 0), rows.item_id)
                  > (COALESCE(anchor.sort_text, ''), COALESCE(anchor.sort_ms, 0), anchor.item_id)"
        );
        let mut before_params: Vec<Box<dyn rusqlite::ToSql>> = scope_param
            .iter()
            .map(|id| Box::new(id.clone()) as Box<dyn rusqlite::ToSql>)
            .collect();
        before_params.push(Box::new(item_id.to_string()));
        let before: i64 = conn
            .prepare(&before_sql)
            .map_err(map_err)?
            .query_row(rusqlite::params_from_iter(before_params.iter()), |r| {
                r.get(0)
            })
            .map_err(map_err)?;
        Ok(Some(before as u32))
    }

    /// Which renderable container currently governs `(kind, id)`, or `None` for an unclassified /
    /// invisible item. The picker uses it to compute the anchor's ancestor path.
    pub fn related_picker_owner_of(
        &self,
        kind: PickerItemKind,
        id: &str,
        unlocked: &HashSet<String>,
        reachable: &HashSet<String>,
    ) -> Result<Option<String>> {
        let conn = self.lock();
        let sql = match kind {
            PickerItemKind::Meeting => {
                let meeting_visible = meeting_visibility_clause("m", unlocked);
                let container = meeting_container_expr(unlocked, reachable);
                format!(
                    "SELECT {container}
                       FROM meetings m
                      WHERE m.id = ?1 AND {meeting_visible}"
                )
            }
            PickerItemKind::Note | PickerItemKind::Document => {
                let visible = visibility_clause("f", unlocked);
                let doc_kind = kind.document_kind();
                let reachable_container = picker_reachable_container("f", reachable);
                let root = picker_canonical_notes_root("f", reachable);
                format!(
                    "SELECT CASE WHEN COALESCE(f.is_root, 0) = 1 THEN NULL ELSE f.id END
                       FROM documents d
                       JOIN folders f ON f.id = d.folder_id
                      WHERE d.id = ?1 AND d.kind = '{doc_kind}' AND d.meeting_id IS NULL
                        AND {visible}
                        AND (({root}
                              AND COALESCE(f.level, 'folder') IN ('project','folder'))
                             OR ({reachable_container}))"
                )
            }
        };
        let owner: Option<Option<String>> = conn
            .prepare(&sql)
            .map_err(map_err)?
            .query_row(rusqlite::params![id], |r| r.get::<_, Option<String>>(0))
            .optional_row()?;
        Ok(owner.flatten())
    }

    /// BOUNDED, GATED search across every linkable local leaf.
    ///
    /// Substring (not prefix) on the display title OR membership in a scope whose full visible
    /// breadcrumb matched in the command layer. This keeps hierarchy traversal out of SQL while
    /// still making `Product` find leaves in `Product / Atlas / Research`. Ordered kind-by-kind
    /// over the SAME stable orderings the tree uses, so paging is deterministic; the caller
    /// supplies breadcrumbs from the hierarchy it already holds, which is why nothing here
    /// returns a path.
    ///
    /// The visibility sets stay explicit because each is a separate privacy boundary: session
    /// unlocks, hierarchy reachability, and breadcrumb matches must never be interchangeable.
    #[allow(clippy::too_many_arguments)]
    pub fn related_picker_search(
        &self,
        query: &str,
        offset: u32,
        limit: u32,
        unlocked: &HashSet<String>,
        reachable: &HashSet<String>,
        breadcrumb_matched: &HashSet<String>,
        include_unclassified: bool,
    ) -> Result<(Vec<PickerSearchRow>, u32)> {
        let query = query.trim();
        if query.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let pattern = format!("%{}%", crate::storage::db::escape_like(query));
        let visible = visibility_clause("f", unlocked);
        let reachable_doc = picker_reachable_container("f", reachable);
        let root_doc = picker_canonical_notes_root("f", reachable);
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let meeting_container = meeting_container_expr(unlocked, reachable);
        let meeting_path_match = picker_container_in(&meeting_container, breadcrumb_matched);
        let document_path_match = picker_container_in("f.id", breadcrumb_matched);
        let unclassified_match = if include_unclassified { "1" } else { "0" };
        let unclassified_meeting = "(m.folder_id IS NULL AND NOT EXISTS (
                 SELECT 1 FROM notes mn
                  WHERE mn.meeting_id = m.id AND mn.folder_id IS NOT NULL
               ))";

        // ONE union so paging is a single stable ordering. `kind_rank` keeps the three legs in a
        // fixed presentation order (recordings, notes, documents) while each leg keeps its own
        // newest-first key.
        let inner = format!(
            "SELECT 0 AS kind_rank,
                    m.id AS item_id,
                    COALESCE(NULLIF(TRIM(m.title), ''), 'Untitled recording') AS title,
                    {meeting_container} AS container_id,
                    m.started_at AS sort_text,
                    NULL AS sort_ms
               FROM meetings m
              WHERE {meeting_visible}
                AND (
                  {meeting_container} IS NOT NULL
                  OR {unclassified_meeting}
                )
                AND (
                  COALESCE(NULLIF(TRIM(m.title), ''), 'Untitled recording') LIKE ?1 ESCAPE '\\'
                  OR {meeting_path_match}
                  OR ({unclassified_match} AND {unclassified_meeting})
                )
              UNION ALL
             SELECT CASE WHEN d.kind = 'note' THEN 1 ELSE 2 END AS kind_rank,
                    d.id AS item_id,
                    COALESCE(NULLIF(TRIM(d.title), ''), d.name) AS title,
                    CASE WHEN ({root_doc}) THEN NULL ELSE f.id END AS container_id,
                    NULL AS sort_text,
                    COALESCE(d.updated_at, d.created_at) AS sort_ms
               FROM documents d
               JOIN folders f ON f.id = d.folder_id
              WHERE d.kind IN ('note','document') AND d.meeting_id IS NULL
                AND {visible}
                AND (({root_doc}
                      AND COALESCE(f.level, 'folder') IN ('project','folder'))
                     OR ({reachable_doc}))
                AND (
                  COALESCE(NULLIF(TRIM(d.title), ''), d.name) LIKE ?1 ESCAPE '\\'
                  OR {document_path_match}
                  OR ({unclassified_match} AND ({root_doc}))
                )"
        );

        let conn = self.lock();
        let total: i64 = conn
            .prepare(&format!("SELECT COUNT(*) FROM ({inner})"))
            .map_err(map_err)?
            .query_row(rusqlite::params![pattern], |r| r.get(0))
            .map_err(map_err)?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT kind_rank, item_id, title, container_id FROM ({inner})
                  ORDER BY kind_rank ASC, {PICKER_ORDER} LIMIT ?2 OFFSET ?3"
            ))
            .map_err(map_err)?;
        let rows = stmt
            .query_map(
                rusqlite::params![pattern, i64::from(limit), i64::from(offset)],
                |r| {
                    let rank: i64 = r.get(0)?;
                    Ok(PickerSearchRow {
                        kind: match rank {
                            0 => PickerItemKind::Meeting,
                            1 => PickerItemKind::Note,
                            _ => PickerItemKind::Document,
                        },
                        id: r.get(1)?,
                        title: r.get(2)?,
                        container_id: r.get(3)?,
                    })
                },
            )
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok((out, total as u32))
    }
}

/// `query_row` that yields `None` instead of erroring on an empty result set.
///
/// `rusqlite::OptionalExtension` covers `Result<T>`, but the owner probe above reads a NULLABLE
/// column, so its natural type is `Result<Option<String>>` and the blanket extension would collapse
/// "no row" into "row with NULL" — two different answers ("the item is not visible" versus "the
/// item is unclassified"). Naming the distinction keeps them apart.
trait OptionalRow<T> {
    fn optional_row(self) -> Result<Option<T>>;
}

impl<T> OptionalRow<T> for rusqlite::Result<T> {
    fn optional_row(self) -> Result<Option<T>> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(map_err(e)),
        }
    }
}
