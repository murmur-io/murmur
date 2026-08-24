//! Workspace-hierarchy readers — the container forest (Projects › Folders) and its per-kind item
//! groups.
//!
//! This module is READ-ONLY and adds no seal, no key, and no new visibility predicate. Every item
//! query reuses the SAME gate the shipped readers use: [`crate::storage::db::visibility_clause`],
//! expressed exactly as `list_meetings_visible` (meeting leg) and `list_notes_visible` (note leg)
//! express it. A hand-written predicate here would be a second, unaudited gate — the leak class
//! `commands/tests/lock_read_gate_tests.rs` exists to catch.
//!
//! Three properties are load-bearing and are asserted by tests, not by comment:
//!
//! 1. **A sealed-and-not-session-unlocked container contributes nothing** — no item rows and no
//!    group totals. That is the stricter of the two policies already in the tree
//!    (`list_notes_visible` filters sealed rows out entirely; only `mask_locked_meetings` keeps a
//!    masked row), and it is the one the hierarchy follows. It falls out of the gate rather than
//!    being applied on top of it: a sealed container's meetings have no visible note row and its
//!    documents fail the clause, so both legs return zero rows for it.
//! 2. **No row carries an on-disk path.** `get_meeting_detail` nulls `audio_path` for a locked
//!    meeting because the FE feeds any path it receives into `convertFileSrc` — the one audio read
//!    that bypasses `export_audio` and `meeting_is_unlocked`. A tree row must never reopen it.
//! 3. **The query count does not grow with the number of containers.** `list_notes` has no LIMIT at
//!    all and `list_meetings` is a flat 200, so a per-container loop would fan out into unbounded
//!    work. Each kind instead runs ONE windowed query (`ROW_NUMBER() OVER (PARTITION BY container)`)
//!    plus ONE grouped `COUNT`, mirroring how `list_link_candidates_visible` pairs a page query with
//!    a COUNT twin that shares its exact WHERE.

use std::collections::{HashMap, HashSet};

use crate::error::Result;
use crate::storage::db::{map_err, visibility_clause, Db};
use crate::storage::models::{ContainerRow, ItemKind, ItemRow};

/// Containers whose kind is outside this set are SYSTEM containers and never reach the tree.
///
/// An in-flight feature introduces `.murmur/tasks` (`kind='task'`, `parent_id IS NULL`) guarded by
/// four `RAISE(ABORT)` SQL triggers. Surfacing it would put an internal folder in the user's
/// sidebar; touching it would abort the write. The path prefix is belt-and-braces for any future
/// machine-owned container that forgets to pick a distinct kind.
const USER_CONTAINER_KINDS: &str = "('meeting','note')";
const SYSTEM_PATH_PREFIX: &str = ".murmur/%";

/// SQL predicate: the `folders` row aliased `alias` is a container this reader RENDERS.
///
/// One definition, used by every place that decides what a container is — the container listing AND
/// the subquery that attributes a meeting to one. Those two must agree: attributing an item to a
/// container the tree does not render makes the item appear in NO container while still existing,
/// which is unreachable-by-construction rather than a leak, and therefore invisible to every leak
/// test. Two independently-written predicates over the same rows is exactly how that happens.
fn renderable_container(alias: &str) -> String {
    format!(
        "COALESCE({alias}.kind, 'meeting') IN {USER_CONTAINER_KINDS}
         AND {alias}.path NOT LIKE '{SYSTEM_PATH_PREFIX}'"
    )
}

impl Db {
    /// Every user-visible container row, ordered for display.
    ///
    /// Returns a FLAT list; the caller assembles the forest (the shape `build_folder_tree` already
    /// uses for the legacy tree). Lock state is the row's own `locked` COLUMN — the disk truth —
    /// and the caller pairs it with the session unlock set, exactly like `list_folders` does.
    pub fn list_containers(&self) -> Result<Vec<ContainerRow>> {
        let conn = self.lock();
        let sql = format!(
            "SELECT f.id, f.name, f.parent_id, COALESCE(f.level, 'folder'), f.emoji, f.tint,
                    COALESCE(f.position, 0), f.locked, COALESCE(f.is_root, 0),
                    COALESCE(f.kind, 'meeting')
               FROM folders f
              WHERE {renderable}
              ORDER BY COALESCE(f.position, 0), f.created_at, f.name",
            renderable = renderable_container("f"),
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ContainerRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    parent_id: r.get(2)?,
                    level: r.get(3)?,
                    emoji: r.get(4)?,
                    tint: r.get(5)?,
                    position: r.get(6)?,
                    locked: r.get::<_, i64>(7)? != 0,
                    is_root: r.get::<_, i64>(8)? != 0,
                    kind: r.get(9)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// The newest `per_container` items of `kind`, for EVERY container at once, keyed by container
    /// id (`None` = the Inbox: an item with no container).
    ///
    /// One query, regardless of how many containers exist.
    pub fn container_group_pages(
        &self,
        kind: ItemKind,
        per_container: u32,
        unlocked: &HashSet<String>,
    ) -> Result<HashMap<Option<String>, Vec<ItemRow>>> {
        let conn = self.lock();
        let sql = format!(
            "SELECT container_id, item_id, title, duration_s, sort_text, sort_ms FROM (
               SELECT container_id, item_id, title, duration_s, sort_text, sort_ms,
                      ROW_NUMBER() OVER (
                        PARTITION BY COALESCE(container_id, '')
                        ORDER BY {ORDER}
                      ) AS rn
                 FROM ({inner})
             ) WHERE rn <= ?1 ORDER BY {ORDER}",
            ORDER = ITEM_ORDER,
            inner = visible_items_sql(kind, unlocked, ItemScope::All).sql,
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![per_container as i64], move |r| {
                Ok((r.get::<_, Option<String>>(0)?, row_to_item(kind, r)?))
            })
            .map_err(map_err)?;
        let mut out: HashMap<Option<String>, Vec<ItemRow>> = HashMap::new();
        for r in rows {
            let (container, item) = r.map_err(map_err)?;
            out.entry(container).or_default().push(item);
        }
        Ok(out)
    }

    /// The TOTAL visible item count of `kind` per container (`None` = the Inbox).
    ///
    /// Shares [`visible_items_sql`] with [`Db::container_group_pages`], so the count can never
    /// disagree with the page — the COUNT-twin discipline `list_link_candidates_visible` uses.
    pub fn container_group_totals(
        &self,
        kind: ItemKind,
        unlocked: &HashSet<String>,
    ) -> Result<HashMap<Option<String>, u32>> {
        let conn = self.lock();
        let sql = format!(
            "SELECT container_id, COUNT(*) FROM ({inner}) GROUP BY container_id",
            inner = visible_items_sql(kind, unlocked, ItemScope::All).sql,
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)? as u32))
            })
            .map_err(map_err)?;
        let mut out = HashMap::new();
        for r in rows {
            let (container, total) = r.map_err(map_err)?;
            out.insert(container, total);
        }
        Ok(out)
    }

    /// One page of ONE container's items of ONE kind, plus the disclosed total for that
    /// (container, kind) pair. `container_id = None` is the Inbox.
    ///
    /// The caller refuses a sealed-and-not-session-unlocked container BEFORE reaching this — see
    /// `commands::workspace::list_container_items`. The gate below would return an empty page
    /// anyway, but an empty page and a refusal are distinguishable by a prober, and only the
    /// refusal is honest.
    pub fn container_items_page(
        &self,
        container_id: Option<&str>,
        kind: ItemKind,
        offset: u32,
        limit: u32,
        unlocked: &HashSet<String>,
    ) -> Result<(Vec<ItemRow>, u32)> {
        let conn = self.lock();
        let scope = match container_id {
            Some(_) => ItemScope::Container,
            None => ItemScope::Inbox,
        };
        let ItemQuery {
            sql: inner,
            binds_container,
        } = visible_items_sql(kind, unlocked, scope);
        // Only bind the container id when the generated SQL actually declares a placeholder for it.
        // The Inbox scope's predicate is a bare `IS NULL`, and a seam kind ignores the scope
        // altogether, so both carry none — and SQLite rejects a statement bound with a different
        // parameter count than it declares. Numbering the remaining placeholders from the REAL list
        // length (rather than binding an inert sentinel to keep the count stable) keeps the page and
        // its COUNT twin sharing one parameter list with nothing for a later reader to decode.
        let container_param: Vec<String> = container_id
            .filter(|_| binds_container)
            .map(str::to_string)
            .into_iter()
            .collect();
        let next = container_param.len() + 1;
        let page_sql = format!(
            "SELECT container_id, item_id, title, duration_s, sort_text, sort_ms
               FROM ({inner}) ORDER BY {ITEM_ORDER} LIMIT ?{next} OFFSET ?{after}",
            after = next + 1,
        );
        let count_sql = format!("SELECT COUNT(*) FROM ({inner})");

        let mut count_stmt = conn.prepare(&count_sql).map_err(map_err)?;
        let total: i64 = count_stmt
            .query_row(rusqlite::params_from_iter(container_param.iter()), |r| {
                r.get(0)
            })
            .map_err(map_err)?;

        let mut page_params: Vec<Box<dyn rusqlite::ToSql>> = container_param
            .iter()
            .map(|id| Box::new(id.clone()) as Box<dyn rusqlite::ToSql>)
            .collect();
        page_params.push(Box::new(limit as i64));
        page_params.push(Box::new(offset as i64));

        let mut stmt = conn.prepare(&page_sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(page_params.iter()), move |r| {
                row_to_item(kind, r)
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok((out, total as u32))
    }
}

/// The ONE newest-first ordering, shared by the window function and the paged reader.
///
/// The two kinds store time differently — `meetings.started_at` is RFC3339 TEXT while
/// `documents.updated_at` is INTEGER epoch-ms — so each kind projects its native column into the
/// slot it can order on and leaves the other NULL. Within a single query every row is of one kind,
/// so exactly one of the two terms is ever significant and the other is a uniform NULL tie.
///
/// Ordering on the native column (rather than converting inside SQL) is deliberate:
/// `strftime('%s', …)` returns NULL for the 9-fractional-digit RFC3339 that
/// `chrono::Utc::now().to_rfc3339()` produces, which would silently collapse every meeting to the
/// same sort position. The RFC3339 TEXT is lexicographically ordered anyway, which is exactly what
/// `list_meetings_visible` already relies on (`ORDER BY m.started_at DESC`).
const ITEM_ORDER: &str = "sort_text DESC, sort_ms DESC, item_id DESC";

/// Which rows the inner select is restricted to.
#[derive(Clone, Copy)]
enum ItemScope {
    /// Every visible row, in every container (the tree-wide window/total queries).
    All,
    /// Only rows whose container is the bound container id.
    Container,
    /// Only rows with NO container.
    Inbox,
}

/// A kind's visible-item select, together with whether that SQL actually CONSUMES the container
/// parameter.
///
/// The caller must not infer this from the scope. A kind whose leg is still a seam ignores the scope
/// entirely and emits no placeholder, so an inferred bind hands SQLite one parameter for a statement
/// that declares none — `InvalidParameterCount`, i.e. an ERROR exactly where the contract promises a
/// truthful empty page. Making the producer state it removes the class, not just the instance.
struct ItemQuery {
    sql: String,
    binds_container: bool,
}

/// The visible-item select for one kind, projecting the SAME six columns for every kind so the
/// windowing/paging wrappers above stay kind-agnostic:
/// `container_id, item_id, title, duration_s, sort_text, sort_ms`.
fn visible_items_sql(kind: ItemKind, unlocked: &HashSet<String>, scope: ItemScope) -> ItemQuery {
    match kind {
        ItemKind::Meeting => {
            // The meeting leg's gate is `list_meetings_visible`'s, verbatim: a meeting is hidden
            // only when EVERY note it has is sealed-and-not-unlocked, and a meeting with zero notes
            // stays visible (it has no container either, so it lands in the Inbox).
            //
            // NOTE on the first argument: `visibility_clause` IGNORES it and always emits a clause
            // bound to the alias `f`, so every caller MUST alias the folders table as `f` — which
            // both joins below do. Passing "f" rather than the shipped call sites' "n" keeps the
            // call honest about which table the emitted SQL actually reads.
            let visible = visibility_clause("f", unlocked);

            // A meeting has no folder column at all; its container lives on its note rows. Three
            // properties of this subquery are load-bearing, and all three exist so that attribution
            // can never name a container the reader will not render:
            //
            // 1. It is GATED by the same clause as the row itself. A meeting whose provider rows
            //    span two containers is visible as soon as ONE of them is readable — attributing it
            //    to an unreadable one would put it in a container whose groups are suppressed, so
            //    the row would appear in NEITHER container: unreachable while still existing.
            // 2. It uses the SAME renderable-container definition as the container listing, so a
            //    note filed into a machine-owned container cannot capture the attribution of a
            //    meeting whose other note sits in a real one. Same failure shape as (1).
            // 3. It is TOTALLY ORDERED. The page query, the totals query and the scope predicate
            //    each evaluate it independently, so without a total order they could disagree with
            //    each other. (The shipped `folder_for_meeting` omits the ORDER BY; that
            //    nondeterminism is pre-existing and untouched — this only makes these three agree.)
            //
            // The JOIN is INNER on purpose: `notes.folder_id` carries no foreign key, so it can
            // dangle. A dangling id yields no row, attribution falls to NULL, and the meeting lands
            // in the Inbox — reachable — instead of being attributed to a folder that is gone.
            let meeting_container = format!(
                "(SELECT nf.folder_id FROM notes nf
                    JOIN folders f ON f.id = nf.folder_id
                   WHERE nf.meeting_id = m.id AND {visible} AND {renderable}
                   ORDER BY nf.folder_id, nf.provider_id
                   LIMIT 1)",
                renderable = renderable_container("f"),
            );
            let scope_pred = match scope {
                ItemScope::All => String::new(),
                ItemScope::Container => format!(" AND {meeting_container} = ?1"),
                ItemScope::Inbox => format!(" AND {meeting_container} IS NULL"),
            };
            ItemQuery {
                binds_container: matches!(scope, ItemScope::Container),
                sql: format!(
                    "SELECT {meeting_container} AS container_id,
                            m.id AS item_id,
                            m.title AS title,
                            m.duration_s AS duration_s,
                            m.started_at AS sort_text,
                            NULL AS sort_ms
                       FROM meetings m
                      WHERE (NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id)
                             OR EXISTS (
                                  SELECT 1 FROM notes n
                                   LEFT JOIN folders f ON f.id = n.folder_id
                                   WHERE n.meeting_id = m.id AND {visible}
                                )){scope_pred}"
                ),
            }
        }
        ItemKind::Note => {
            // The note leg's gate is `list_notes_visible`'s, verbatim (INNER JOIN, because
            // `documents.folder_id` is NOT NULL — so an authored note is never container-less and
            // never appears in the Inbox).
            //
            // `d.meeting_id IS NULL` excludes a recording's COMPANION note: it is created by
            // `get_or_create_companion_note_inner` and belongs to its meeting's row. Without this
            // predicate every recording would be listed TWICE in the same container.
            let visible = visibility_clause("f", unlocked);
            let scope_pred = match scope {
                ItemScope::All => "",
                ItemScope::Container => " AND d.folder_id = ?1",
                // An authored note always has a container, so the Inbox leg is deliberately empty
                // rather than "unfiled notes" — those live in the reserved `is_root` Notes folder.
                ItemScope::Inbox => " AND 1 = 0",
            };
            ItemQuery {
                binds_container: matches!(scope, ItemScope::Container),
                sql: format!(
                    "SELECT d.folder_id AS container_id,
                            d.id AS item_id,
                            COALESCE(NULLIF(d.title, ''), d.name) AS title,
                            NULL AS duration_s,
                            NULL AS sort_text,
                            COALESCE(d.updated_at, d.created_at) AS sort_ms
                       FROM documents d
                       JOIN folders f ON f.id = d.folder_id
                      WHERE d.kind = 'note' AND d.meeting_id IS NULL
                        AND {visible} AND {renderable}{scope_pred}",
                    renderable = renderable_container("f"),
                ),
            }
        }
        ItemKind::Task => {
            // A task appears in the tree ONLY once it has been filed. `container_id` is the local
            // anchor, and the Inbox leg is deliberately empty rather than "every task with no
            // container": the top-level Tasks view is org-scoped and gated on org read context
            // (`commands::tasks::list_tasks`), and mirroring every unfiled org task into the
            // hierarchy would be a SECOND, ungated route to the same rows. Filing is an explicit
            // local act; nothing is hidden by requiring it, because the unfiled tasks are exactly
            // where they always were.
            //
            // The title comes from the decrypted envelope via `json_extract` — the same string the
            // Tasks view shows, read from the projection this device already holds in the clear
            // inside its SQLCipher database.
            let visible = visibility_clause("f", unlocked);
            let scope_sql = match scope {
                ItemScope::All => "t.container_id IS NOT NULL",
                ItemScope::Container => "t.container_id = ?1",
                // Unfiled tasks belong to the Tasks view, not the tree — see above.
                ItemScope::Inbox => "1 = 0",
            };
            ItemQuery {
                binds_container: matches!(scope, ItemScope::Container),
                // The ORG gate is joined here, in SQL, exactly as `search_org_chunks_knn` /
                // `get_org_item` / `count_org_items` do it: an org row must EXIST locally (the
                // membership record) and have `context_enabled = 1`. Filing a task locally cannot
                // be allowed to outlive membership in the org that owns it — a leaver, a revoked
                // seat, or a device where the user turned that org's context off must all stop
                // this leg dead, and the folder's `visibility_clause` says nothing about any of
                // them.
                //
                // The comment on the Inbox arm above already spelled out why this matters: the
                // Tasks view "gates on session and org read context; mirroring every unfiled org
                // task into the hierarchy would be a SECOND, ungated route to the same rows".
                // Without the join, the Container arm was that second route — the reasoning was
                // written and then contradicted one branch later.
                //
                // `INNER JOIN`, deliberately: a task whose org row is absent has no membership
                // this device can attest, so it must vanish rather than default to visible.
                //
                // The `org_items` join is the repo's own idiom for "live, current head" — the same
                // `tombstoned = 0 AND is_current = 1` pair `dashboard_task_rows` and the Task
                // reference resolvers use. Membership alone was not enough: a task DELETED in the
                // org, or superseded by a newer revision, still had its `org_tasks` row and would
                // have kept rendering its old title under the container it was filed in, long
                // after it stopped existing for everyone else.
                sql: format!(
                    "SELECT t.container_id AS container_id, t.id AS item_id,
                            json_extract(t.envelope_json, '$.title') AS title,
                            NULL AS duration_s, t.updated_at AS sort_text, NULL AS sort_ms
                       FROM org_tasks t
                       JOIN org_state os ON os.org_id = t.org_id AND os.context_enabled = 1
                       JOIN org_items i
                         ON i.item_id = t.item_id AND i.tombstoned = 0 AND i.is_current = 1
                       LEFT JOIN folders f ON f.id = t.container_id
                      WHERE {scope_sql} AND t.container_id IS NOT NULL AND {visible}"
                ),
            }
        }
        // SEAM — dashboards gained `folder_id` + a real seal in their own step; this comment marks
        // where a future kind would join.
        //
        // An always-false select rather than an absent arm on purpose: the wrappers stay
        // kind-agnostic, the column projection is pinned for the future implementation, and a caller
        // asking for such a page gets a truthful empty page instead of an error. `binds_container`
        // is FALSE for every scope precisely because such an arm ignores the scope — see [`ItemQuery`].
        ItemKind::Dashboard => {
            // Boards now carry a container, so this leg reads real rows — through the SAME
            // visibility clause every other kind uses rather than a second rule of its own.
            // `updated_at` is the sort key: a board has no single moment it happened, and the
            // last time it changed is the closest thing to one.
            let visible = visibility_clause("f", unlocked);
            let scope_sql = match scope {
                ItemScope::All => "1 = 1",
                ItemScope::Container => "d.folder_id = ?1",
                ItemScope::Inbox => "d.folder_id IS NULL",
            };
            ItemQuery {
                binds_container: matches!(scope, ItemScope::Container),
                sql: format!(
                    "SELECT d.folder_id AS container_id, d.id AS item_id, d.title AS title,
                            NULL AS duration_s, d.updated_at AS sort_text, NULL AS sort_ms
                       FROM dashboards d
                       LEFT JOIN folders f ON f.id = d.folder_id
                      WHERE {scope_sql} AND (d.folder_id IS NULL OR {visible})"
                ),
            }
        }
    }
}

/// Map the pinned six-column projection into an [`ItemRow`].
///
/// `sort_at` is normalised to epoch MILLISECONDS here, in Rust, so the wire carries one time
/// representation for every kind. Meetings arrive as RFC3339 TEXT, which `chrono` parses including
/// the 9 fractional digits SQLite's own date functions cannot.
fn row_to_item(kind: ItemKind, r: &rusqlite::Row<'_>) -> rusqlite::Result<ItemRow> {
    let id: String = r.get("item_id")?;
    let title: Option<String> = r.get("title")?;
    let duration_s: Option<i64> = r.get("duration_s")?;
    let sort_text: Option<String> = r.get("sort_text")?;
    let sort_ms: Option<i64> = r.get("sort_ms")?;
    let sort_at = sort_ms.unwrap_or_else(|| {
        sort_text
            .as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|dt| dt.timestamp_millis())
            .unwrap_or(0)
    });
    Ok(ItemRow {
        kind,
        id,
        title: title.filter(|t| !t.is_empty()),
        duration_s,
        sort_at,
    })
}
