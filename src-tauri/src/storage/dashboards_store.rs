//! Dashboard storage surface — boards + their tiles (2026-08-03).
//!
//! A dashboard is a user-composed board of TILES over sources that already exist elsewhere in the
//! vault (a meeting, a note, a document, an entity, or a derived view like the fact-drift lane).
//! The tables here therefore store **pointers and layout, never content**: `dashboard_tiles` keeps a
//! `kind` + an optional `ref_id` + cosmetic layout, and every tile READ resolves through the
//! existing gated readers (`visibility_clause` / `meeting_is_unlocked`) at read time.
//!
//! ## Lock model (see `.claude/rules/lock-model.md`)
//! * Nothing sealed is ever copied into these tables, so there is nothing here to seal or purge.
//! * A tile pointing at a sealed-and-not-session-unlocked source resolves to a MASKED tile at the
//!   command layer — the board renders a redacted placeholder instead of leaking a title.
//! * `ref_id` is intentionally NOT a foreign key: a deleted source must degrade the tile to
//!   "source missing", never cascade-delete the user's layout.
//!
//! The methods below are an inherent-impl split of [`crate::storage::db::Db`] across files, the
//! same pattern the other `*_store.rs` modules use.

use rusqlite::{OptionalExtension, Row};

use crate::error::{AppError, Result};
use crate::storage::db::{map_err, Db};
use crate::storage::models::{Dashboard, DashboardTile};

/// Tile kinds Murmur knows how to render. Anything else is refused at the command layer, so a
/// malformed row can never reach the renderer as an unhandled variant.
pub const TILE_KINDS: &[&str] = &[
    "note",
    "meeting",
    "document",
    "person",
    "reminders",
    "drift",
    "numbers",
    "pulse",
    "promises",
    "living_answer",
];

/// The grid is 12 columns wide; a tile spans 3–12 of them.
pub const MIN_SPAN: i64 = 3;
pub const MAX_SPAN: i64 = 12;
/// Hard ceiling on tiles per board — a board is a curated view, not an unbounded dump, and every
/// tile costs a gated read on load.
pub const MAX_TILES_PER_BOARD: i64 = 60;
/// Hard ceiling on boards, for the same reason.
pub const MAX_DASHBOARDS: i64 = 200;

fn row_to_dashboard(row: &Row<'_>) -> rusqlite::Result<Dashboard> {
    Ok(Dashboard {
        id: row.get(0)?,
        title: row.get(1)?,
        emoji: row.get(2)?,
        tint: row.get(3)?,
        pinned: row.get::<_, i64>(4)? != 0,
        position: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn row_to_tile(row: &Row<'_>) -> rusqlite::Result<DashboardTile> {
    Ok(DashboardTile {
        id: row.get(0)?,
        dashboard_id: row.get(1)?,
        kind: row.get(2)?,
        ref_id: row.get(3)?,
        title: row.get(4)?,
        span: row.get(5)?,
        position: row.get(6)?,
        config: row.get(7)?,
        created_at: row.get(8)?,
    })
}

const DASHBOARD_COLS: &str =
    "id, title, emoji, tint, pinned, position, created_at, updated_at";
const TILE_COLS: &str =
    "id, dashboard_id, kind, ref_id, title, span, position, config, created_at";

impl Db {
    /// Every board, pinned first, then by explicit position, then newest-updated.
    pub fn list_dashboards(&self) -> Result<Vec<Dashboard>> {
        let conn = self.lock();
        let sql = format!(
            "SELECT {DASHBOARD_COLS} FROM dashboards
              ORDER BY pinned DESC, position ASC, updated_at DESC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], row_to_dashboard)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    pub fn get_dashboard(&self, id: &str) -> Result<Option<Dashboard>> {
        let conn = self.lock();
        let sql = format!("SELECT {DASHBOARD_COLS} FROM dashboards WHERE id = ?1");
        let out = conn
            .query_row(&sql, [id], row_to_dashboard)
            .optional()
            .map_err(map_err)?;
        Ok(out)
    }

    pub fn dashboard_count(&self) -> Result<i64> {
        let conn = self.lock();
        let n = conn
            .query_row("SELECT COUNT(*) FROM dashboards", [], |r| r.get::<_, i64>(0))
            .map_err(map_err)?;
        Ok(n)
    }

    /// Insert a board. `position` is appended after the current maximum so a new board lands last.
    pub fn insert_dashboard(
        &self,
        id: &str,
        title: &str,
        emoji: Option<&str>,
        tint: Option<&str>,
        now: &str,
    ) -> Result<()> {
        let conn = self.lock();
        let next: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM dashboards",
                [],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        conn.execute(
            "INSERT INTO dashboards (id, title, emoji, tint, pinned, position, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?6)",
            rusqlite::params![id, title, emoji, tint, next, now],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Patch a board's user-editable fields.
    ///
    /// Three-state per nullable column, because plain `COALESCE(?, col)` can only ever SET a
    /// value — it makes "remove the emoji" unexpressible:
    ///   * `None`      ⇒ leave the column untouched,
    ///   * `Some("")`  ⇒ CLEAR the column to NULL,
    ///   * `Some(v)`   ⇒ set it to `v`.
    pub fn update_dashboard(
        &self,
        id: &str,
        title: Option<&str>,
        emoji: Option<&str>,
        tint: Option<&str>,
        pinned: Option<bool>,
        now: &str,
    ) -> Result<bool> {
        let conn = self.lock();
        let n = conn
            .execute(
                "UPDATE dashboards
                    SET title      = COALESCE(?2, title),
                        emoji      = CASE WHEN ?3 IS NULL THEN emoji
                                          WHEN ?3 = '' THEN NULL ELSE ?3 END,
                        tint       = CASE WHEN ?4 IS NULL THEN tint
                                          WHEN ?4 = '' THEN NULL ELSE ?4 END,
                        pinned     = COALESCE(?5, pinned),
                        updated_at = ?6
                  WHERE id = ?1",
                rusqlite::params![id, title, emoji, tint, pinned.map(i64::from), now],
            )
            .map_err(map_err)?;
        Ok(n > 0)
    }

    /// Bump `updated_at` — called whenever a tile changes, so the list's "updated 2h ago" is honest.
    pub fn touch_dashboard(&self, id: &str, now: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE dashboards SET updated_at = ?2 WHERE id = ?1",
            rusqlite::params![id, now],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Delete a board and (by cascade) its tiles. Returns false when the id was unknown.
    pub fn delete_dashboard(&self, id: &str) -> Result<bool> {
        let conn = self.lock();
        // The FK cascade only fires with foreign_keys=ON; delete tiles explicitly so the row can
        // never be orphaned regardless of the connection pragma.
        conn.execute(
            "DELETE FROM dashboard_tiles WHERE dashboard_id = ?1",
            [id],
        )
        .map_err(map_err)?;
        let n = conn
            .execute("DELETE FROM dashboards WHERE id = ?1", [id])
            .map_err(map_err)?;
        Ok(n > 0)
    }

    /// Tiles of one board, in layout order.
    pub fn list_dashboard_tiles(&self, dashboard_id: &str) -> Result<Vec<DashboardTile>> {
        let conn = self.lock();
        let sql = format!(
            "SELECT {TILE_COLS} FROM dashboard_tiles
              WHERE dashboard_id = ?1 ORDER BY position ASC, created_at ASC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([dashboard_id], row_to_tile)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// The tile KINDS on every board, keyed by board id — the list view's miniature preview reads
    /// this instead of loading every tile's (gated) payload.
    pub fn dashboard_tile_kinds(&self) -> Result<Vec<(String, String, i64)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT dashboard_id, kind, span FROM dashboard_tiles
                  ORDER BY dashboard_id, position ASC, created_at ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
            })
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    pub fn get_dashboard_tile(&self, id: &str) -> Result<Option<DashboardTile>> {
        let conn = self.lock();
        let sql = format!("SELECT {TILE_COLS} FROM dashboard_tiles WHERE id = ?1");
        let out = conn
            .query_row(&sql, [id], row_to_tile)
            .optional()
            .map_err(map_err)?;
        Ok(out)
    }

    pub fn dashboard_tile_count(&self, dashboard_id: &str) -> Result<i64> {
        let conn = self.lock();
        let n = conn
            .query_row(
                "SELECT COUNT(*) FROM dashboard_tiles WHERE dashboard_id = ?1",
                [dashboard_id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(map_err)?;
        Ok(n)
    }

    /// Append a tile to a board.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_dashboard_tile(
        &self,
        id: &str,
        dashboard_id: &str,
        kind: &str,
        ref_id: Option<&str>,
        title: Option<&str>,
        span: i64,
        config: Option<&str>,
        now: &str,
    ) -> Result<()> {
        if !TILE_KINDS.contains(&kind) {
            return Err(AppError::InvalidArg(format!("unknown tile kind: {kind}")));
        }
        let conn = self.lock();
        let next: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM dashboard_tiles WHERE dashboard_id = ?1",
                [dashboard_id],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        conn.execute(
            "INSERT INTO dashboard_tiles
               (id, dashboard_id, kind, ref_id, title, span, position, config, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                id,
                dashboard_id,
                kind,
                ref_id,
                title,
                span.clamp(MIN_SPAN, MAX_SPAN),
                next,
                config,
                now
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Patch a tile's title / span / config. Same three-state nullable contract as
    /// [`Self::update_dashboard`]: `None` = untouched, `Some("")` = clear, `Some(v)` = set.
    pub fn update_dashboard_tile(
        &self,
        id: &str,
        title: Option<&str>,
        span: Option<i64>,
        config: Option<&str>,
    ) -> Result<bool> {
        let conn = self.lock();
        let n = conn
            .execute(
                "UPDATE dashboard_tiles
                    SET title  = CASE WHEN ?2 IS NULL THEN title
                                      WHEN ?2 = '' THEN NULL ELSE ?2 END,
                        span   = COALESCE(?3, span),
                        config = CASE WHEN ?4 IS NULL THEN config
                                      WHEN ?4 = '' THEN NULL ELSE ?4 END
                  WHERE id = ?1",
                rusqlite::params![id, title, span.map(|s| s.clamp(MIN_SPAN, MAX_SPAN)), config],
            )
            .map_err(map_err)?;
        Ok(n > 0)
    }

    pub fn delete_dashboard_tile(&self, id: &str) -> Result<bool> {
        let conn = self.lock();
        let n = conn
            .execute("DELETE FROM dashboard_tiles WHERE id = ?1", [id])
            .map_err(map_err)?;
        Ok(n > 0)
    }

    /// Rewrite the layout order of a board from an explicit id list, in ONE transaction so a
    /// partial reorder can never persist.
    ///
    /// The caller's list is treated as a PREFERENCE, not a trusted permutation: duplicates are
    /// collapsed to their first occurrence, unknown ids are dropped, and any tile the caller
    /// omitted is appended in its existing order. Without that, a duplicated or short list left
    /// rows sharing a position — and the rendered order then depended on the secondary sort key
    /// rather than on what the user dragged.
    pub fn reorder_dashboard_tiles(&self, dashboard_id: &str, tile_ids: &[String]) -> Result<()> {
        let existing: Vec<String> = self
            .list_dashboard_tiles(dashboard_id)?
            .into_iter()
            .map(|t| t.id)
            .collect();
        let order = dense_order(tile_ids, &existing);
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        for (i, tile_id) in order.iter().enumerate() {
            tx.execute(
                "UPDATE dashboard_tiles SET position = ?3
                  WHERE id = ?1 AND dashboard_id = ?2",
                rusqlite::params![tile_id, dashboard_id, i as i64],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// GATED mention pulse for one entity: `(iso_started_at)` of every VISIBLE meeting that
    /// mentions it, newest first, capped. Powers the Pulse tile (mentions per week + "quiet since").
    ///
    /// Gating is the SAME shape the graph readers use: a meeting is visible when it has no note
    /// rows at all, or a note row whose folder passes `visibility_clause`. A sealed-and-not-
    /// session-unlocked meeting contributes NOTHING — so the pulse of an entity that only appears
    /// in locked meetings is empty, never a count that betrays hidden activity.
    pub fn entity_mention_pulse_visible(
        &self,
        entity_id: &str,
        limit: i64,
        unlocked: &std::collections::HashSet<String>,
    ) -> Result<Vec<String>> {
        let conn = self.lock();
        let visible = crate::storage::db::visibility_clause("n", unlocked);
        let sql = format!(
            "SELECT m.started_at
               FROM entity_mentions em
               JOIN meetings m ON m.id = em.meeting_id
              WHERE em.entity_id = ?1
                AND (NOT EXISTS (SELECT 1 FROM notes nn WHERE nn.meeting_id = m.id)
                     OR EXISTS (SELECT 1 FROM notes n
                                 LEFT JOIN folders f ON f.id = n.folder_id
                                WHERE n.meeting_id = m.id AND {visible}))
              ORDER BY m.started_at DESC
              LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![entity_id, limit], |r| {
                r.get::<_, String>(0)
            })
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// EXISTENCE-ONLY probe for a tile's anchor, used to tell "sealed" apart from "deleted" when
    /// rendering a tile. It returns a BOOLEAN and nothing else — no title, no folder, no dates —
    /// which is the same existence signal the masked note/meeting DTOs already expose (a locked
    /// meeting renders as "🔒 Locked", so its existence was never the secret; its content is).
    pub fn dashboard_ref_exists(&self, kind: &str, id: &str) -> Result<bool> {
        // `note` and `document` share the `documents` table, so the probe must also match the
        // row's KIND — otherwise a note tile pointing at a document row reports "sealed" when the
        // honest answer is "missing".
        let (table, kind_filter) = match kind {
            "note" => ("documents", " AND kind = 'note'"),
            "document" => ("documents", " AND kind <> 'note'"),
            "meeting" => ("meetings", ""),
            "person" | "drift" | "numbers" | "pulse" => ("entities", ""),
            _ => return Ok(false),
        };
        let conn = self.lock();
        let sql =
            format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE id = ?1{kind_filter})");
        let exists: i64 = conn.query_row(&sql, [id], |r| r.get(0)).map_err(map_err)?;
        Ok(exists != 0)
    }

    /// Reorder BOARDS themselves (drag in the list view). Same permutation discipline as
    /// [`Self::reorder_dashboard_tiles`].
    pub fn reorder_dashboards(&self, ids: &[String]) -> Result<()> {
        let existing: Vec<String> = self
            .list_dashboards()?
            .into_iter()
            .map(|d| d.id)
            .collect();
        let order = dense_order(ids, &existing);
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        for (i, id) in order.iter().enumerate() {
            tx.execute(
                "UPDATE dashboards SET position = ?2 WHERE id = ?1",
                rusqlite::params![id, i as i64],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(())
    }
}

/// Turn a caller-supplied (possibly partial, possibly duplicated, possibly bogus) id list into a
/// TOTAL order over `existing`: requested ids first in their requested order, then everything the
/// caller left out, in its current order. Ids not in `existing` are dropped.
fn dense_order(requested: &[String], existing: &[String]) -> Vec<String> {
    let known: std::collections::HashSet<&str> = existing.iter().map(String::as_str).collect();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::with_capacity(existing.len());
    for id in requested {
        if known.contains(id.as_str()) && seen.insert(id.clone()) {
            out.push(id.clone());
        }
    }
    for id in existing {
        if seen.insert(id.clone()) {
            out.push(id.clone());
        }
    }
    out
}
