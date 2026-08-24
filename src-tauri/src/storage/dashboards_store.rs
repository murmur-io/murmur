//! Dashboard storage surface — boards + their tiles (2026-08-03).
//!
//! A dashboard is a user-composed board of TILES over sources that already exist elsewhere in the
//! vault (a meeting, a note, a document, an entity, or a derived view like the fact-drift lane).
//! Most rows therefore store pointers + layout. The one deliberate content cache is a
//! `living_answer`: dedicated columns hold the backend-generated answer plus exact structural,
//! corpus and readable-folder provenance; the command layer validates that provenance before it
//! hydrates a single content column.
//!
//! ## Lock model (see `.claude/rules/lock-model.md`)
//! * Living-answer text is a derived paraphrase, so it is withheld unless its backend-owned
//!   folder stamp and exact excluded-Living corpus witness are still current.
//! * A tile pointing at a sealed-and-not-session-unlocked source resolves to a MASKED tile at the
//!   command layer — the board renders a redacted placeholder instead of leaking a title.
//! * `ref_id` is intentionally NOT a foreign key: a deleted source must degrade the tile to
//!   "source missing", never cascade-delete the user's layout.
//!
//! The methods below are an inherent-impl split of [`crate::storage::db::Db`] across files, the
//! same pattern the other `*_store.rs` modules use.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, Row, Transaction};

use crate::error::{AppError, Result};
use crate::storage::db::{map_err, Db};
use crate::storage::models::{Dashboard, DashboardTile};
use crate::storage::visibility_clause;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LivingAnswerContent {
    pub(crate) question: String,
    pub(crate) answer: Option<String>,
    pub(crate) answered_at: Option<String>,
}

fn row_to_dashboard(row: &Row<'_>) -> rusqlite::Result<Dashboard> {
    Ok(Dashboard {
        id: row.get(0)?,
        title: row.get(1)?,
        emoji: row.get(2)?,
        tint: row.get(3)?,
        pinned: row.get::<_, i64>(4)? != 0,
        position: row.get(5)?,
        folder_id: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        // The raw mapper never claims a board is locked: it has not consulted the session
        // unlock set and cannot. Only the GATED readers set this, so a caller that skips them
        // gets `false` — which is why they are the only ones that may return a board at all.
        locked: false,
    })
}

/// Blank a board that is sealed and not unlocked for this session.
///
/// Title, emoji and tint all go: an emoji and an accent are weak signals, but they are still
/// signals about a thing the user asked to be unreadable.
/// Blank a board the caller has already decided is sealed-and-not-unlocked.
///
/// The ONE masking rule, so the row-at-a-time reader and the SQL-side list reader cannot drift
/// into masking different fields.
fn mask_board(board: Dashboard) -> Dashboard {
    Dashboard {
        title: "🔒 Locked".to_string(),
        emoji: None,
        tint: None,
        locked: true,
        // The CONTAINER goes too. Keeping it let a caller group masked boards by folder and count
        // how many a sealed container holds — the exact fact the tree leg withholds on purpose
        // (`every_dashboard_read_sink_withholds_a_sealed_board` asserts `total == 0` there). Two
        // readers changed in one diff cannot take opposite positions on the same disclosure; the
        // board's own row is the weaker one, because nothing downstream needs a sealed board's
        // container to render it as locked.
        folder_id: None,
        ..board
    }
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

/// A tile's plaintext, recovered from its sealed payload.
///
/// Every field is `Option` so a column that was NULL before the seal is NULL after the unseal.
/// Collapsing NULL to "" would make the round-trip lossy in a way no equality check on the
/// visible text would catch.
#[derive(Debug, Clone)]
pub(crate) struct RestoredTile {
    pub id: String,
    pub title: Option<String>,
    pub ref_id: Option<String>,
    pub config: Option<String>,
    pub kind: Option<String>,
}

/// One sealed board's ciphertext: its title blob (absent when the board was never sealed) and a
/// blob per tile that has one. The unseal's whole input.
pub(crate) type SealedDashboardBlobs = (String, Option<Vec<u8>>, Vec<(String, Vec<u8>)>);

const DASHBOARD_COLS: &str =
    "id, title, emoji, tint, pinned, position, folder_id, created_at, updated_at";
/// The same columns, qualified — required by any read that JOINs `folders`, which brings a second
/// `id` (and a `locked`) into scope and makes the bare list ambiguous.
const DASHBOARD_COLS_D: &str = "d.id, d.title, d.emoji, d.tint, d.pinned, d.position, d.folder_id,
     d.created_at, d.updated_at";
const TILE_COLS: &str = "id, dashboard_id, kind, ref_id, title, span, position, config, created_at";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LivingAnswerCacheState {
    Empty,
    Valid {
        readable_folders_json: String,
        context_generation: i64,
        context_digest: String,
        context_budget: i64,
        ask_dispatch_generation: i64,
    },
    Malformed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LivingAnswerPreflight {
    pub(crate) question_readable_folders_json: Option<String>,
    pub(crate) answer: LivingAnswerCacheState,
}

fn advance_context_generation(
    tx: &Transaction<'_>,
    dashboard_id: &str,
    exists_now: bool,
) -> Result<()> {
    tx.execute(
        "INSERT INTO dashboard_context_state
         (dashboard_id, generation, structural_generation, exists_now)
         VALUES (?1, 1, 1, ?2)
         ON CONFLICT(dashboard_id) DO UPDATE SET
           generation = dashboard_context_state.generation + 1,
           structural_generation = dashboard_context_state.structural_generation + 1,
           exists_now = excluded.exists_now",
        rusqlite::params![dashboard_id, i64::from(exists_now)],
    )
    .map_err(map_err)?;
    Ok(())
}

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

    /// Every board, with a SEALED board's title and cosmetics masked.
    ///
    /// A board filed in a container inherits that container's lock, and its title is content:
    /// "Q3 layoffs" names the thing whether or not the tiles are readable. The ungated
    /// `list_dashboards` returned every title unconditionally, which was correct only while a
    /// board could not live in a folder at all.
    ///
    /// Masked rather than omitted, for the reason the meeting detail is masked rather than
    /// omitted: a user who locked a folder should still see that the board exists and can be
    /// unlocked, not wonder whether it was deleted.
    pub fn list_dashboards_visible(&self, unlocked: &HashSet<String>) -> Result<Vec<Dashboard>> {
        // ONE statement on ONE connection, and the sealed-ness rule is the SHARED
        // `visibility_clause` every other reader uses. The first version took two separate
        // `self.lock()`s — rows, then a hand-rolled `SELECT id FROM folders WHERE locked = 1` —
        // which is two defects at once: a relock landing between them yields plaintext rows
        // judged against a stale sealed set, and any predicate `visibility_clause` grows beyond
        // `locked = 1` would silently not apply here.
        let visible = visibility_clause("f", unlocked);
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {DASHBOARD_COLS_D},
                        CASE WHEN d.folder_id IS NULL OR {visible} THEN 0 ELSE 1 END AS sealed
                   FROM dashboards d
                   LEFT JOIN folders f ON f.id = d.folder_id
                  ORDER BY d.pinned DESC, d.position ASC, d.updated_at DESC"
            ))
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                let board = row_to_dashboard(r)?;
                let sealed: i64 = r.get("sealed")?;
                Ok(if sealed == 1 { mask_board(board) } else { board })
            })
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// One board, masked on the same terms as [`Db::list_dashboards_visible`].
    pub fn get_dashboard_visible(
        &self,
        id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<Dashboard>> {
        let visible = visibility_clause("f", unlocked);
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {DASHBOARD_COLS_D},
                        CASE WHEN d.folder_id IS NULL OR {visible} THEN 0 ELSE 1 END AS sealed
                   FROM dashboards d
                   LEFT JOIN folders f ON f.id = d.folder_id
                  WHERE d.id = ?1"
            ))
            .map_err(map_err)?;
        let board = stmt
            .query_row(rusqlite::params![id], |r| {
                let board = row_to_dashboard(r)?;
                let sealed: i64 = r.get("sealed")?;
                Ok(if sealed == 1 { mask_board(board) } else { board })
            })
            .optional()
            .map_err(map_err)?;
        Ok(board)
    }

    /// Re-file a board into a container, or unfile it with `None`.
    ///
    ///
    /// Returns false when no board carries that id, so the caller refuses rather than reporting a
    /// success that moved nothing.
    pub(crate) fn set_dashboard_folder(&self, id: &str, folder_id: Option<&str>) -> Result<bool> {
        let conn = self.lock();
        let n = conn
            .execute(
                "UPDATE dashboards SET folder_id = ?2 WHERE id = ?1",
                rusqlite::params![id, folder_id],
            )
            .map_err(map_err)?;
        Ok(n > 0)
    }

    /// True when any board in this folder still holds PLAINTEXT.
    ///
    /// Asked by the at-rest re-blank when no content key could be resolved: skipping the seal
    /// silently would report the folder locked while its boards stayed readable, so the re-blank
    /// refuses instead.
    ///
    /// THIS PREDICATE MUST MATCH THE SEAL'S, and an earlier revision of it did not. The seal asks
    /// `!board.title.is_empty() || board.emoji.is_some() || board.tint.is_some()`; this one asked
    /// about the title alone. A board with an empty title, an emoji or a tint, and no plaintext
    /// tiles therefore answered "nothing to seal" — so a keyless relock SUCCEEDED and left the
    /// emoji and the accent readable in a folder it had just reported as locked. The gap opened
    /// the moment the seal was widened to cover the two cosmetic columns and this half was not:
    /// one side of a paired invariant moved. `seal_dashboards_in_folder` names this function as
    /// its twin, and any future column that becomes sealable has to arrive in both.
    pub(crate) fn folder_has_plaintext_dashboards(&self, folder_id: &str) -> Result<bool> {
        let conn = self.lock();
        let found: i64 = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM dashboards d
                     WHERE d.folder_id = ?1
                       AND (COALESCE(d.title, '') <> ''
                            OR d.emoji IS NOT NULL
                            OR d.tint IS NOT NULL
                            OR EXISTS(SELECT 1 FROM dashboard_tiles t
                                       WHERE t.dashboard_id = d.id AND t.config_blob IS NULL)))",
                rusqlite::params![folder_id],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        Ok(found == 1)
    }


    /// Every board filed in a folder — the seal's enumeration.
    pub(crate) fn dashboards_in_folder(&self, folder_id: &str) -> Result<Vec<Dashboard>> {
        let conn = self.lock();
        let sql = format!("SELECT {DASHBOARD_COLS} FROM dashboards WHERE folder_id = ?1");
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![folder_id], row_to_dashboard)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// `(tile id, sealable payload)` for one board — the seal's per-tile enumeration.
    ///
    /// Only tiles that still hold PLAINTEXT — `config_blob IS NULL` — are returned, and that
    /// predicate is load-bearing rather than an optimisation. `seal_dashboards_in_folder` runs
    /// again on every relock of an already-sealed folder, and an already-sealed tile's columns
    /// are blank by construction; without this filter that second pass would encrypt three empty
    /// strings and overwrite the good ciphertext with them, destroying the tile's content on the
    /// first relock and leaving nothing to recover. The title half is covered by its own
    /// `!board.title.is_empty()` guard, which is the same predicate spelled differently.
    ///
    /// The payload is the tile's `title`, `ref_id` and `config` together, encoded as one JSON
    /// object so a single blob restores all three. Sealing only `config` left the other two in
    /// plaintext, and both are disclosures: `title` is a COPY of the source meeting's or note's
    /// title, and `ref_id` names which one the board is built from. A lock that hid the board's
    /// own name while leaving "Standup" and its meeting id readable would be a lock with a hole
    /// exactly where someone would look.
    pub(crate) fn dashboard_tile_payloads(
        &self,
        dashboard_id: &str,
    ) -> Result<Vec<(String, String)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, title, ref_id, config, kind
                   FROM dashboard_tiles WHERE dashboard_id = ?1 AND config_blob IS NULL",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![dashboard_id], |r| {
                // The columns go in as `Option<String>`, NOT coalesced to "". A tile whose
                // `ref_id` was NULL and came back as "" is not a byte-identical restore — it is
                // a silent type change that any `IS NULL` predicate downstream then reads
                // differently. JSON null round-trips; the empty string does not stand in for it.
                let payload = serde_json::json!({
                    "title": r.get::<_, Option<String>>(1)?,
                    "refId": r.get::<_, Option<String>>(2)?,
                    "config": r.get::<_, Option<String>>(3)?,
                    "kind": r.get::<_, Option<String>>(4)?,
                })
                .to_string();
                Ok((r.get::<_, String>(0)?, payload))
            })
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// Store the sealed title and blank the plaintext — called only after the caller has proved
    /// the blob decrypts back byte-identical.
    pub(crate) fn seal_dashboard_title(&self, id: &str, blob: &[u8]) -> Result<()> {
        let conn = self.lock();
        // `emoji` and `tint` go with the title, because the blob now carries all three. The read
        // path masked them from the start on the grounds that they are "still signals about a
        // thing the user asked to be unreadable" — and leaving the COLUMNS populated made that
        // masking protection against the app rather than against anyone reading the database,
        // which is the threat the seal is for.
        conn.execute(
            "UPDATE dashboards SET title = '', emoji = NULL, tint = NULL, title_blob = ?2
              WHERE id = ?1",
            rusqlite::params![id, blob],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The tile twin of [`Db::seal_dashboard_title`]: blanks title, ref, config and kind.
    ///
    /// `kind` goes with them because "this locked board contains three MEETING tiles" is a
    /// disclosure about a thing the user asked to be unreadable. `span` and `position` stay:
    /// they are the grid geometry, carry no reference to any content, and keeping them means an
    /// unsealed board comes back in the layout it had rather than collapsed to a default.
    pub(crate) fn seal_dashboard_tile(&self, id: &str, blob: &[u8]) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE dashboard_tiles
                SET title = '', ref_id = '', config = '', kind = '', config_blob = ?2
              WHERE id = ?1",
            rusqlite::params![id, blob],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Restore a sealed board: decrypt its cosmetics and every tile payload back into place.
    ///
    /// The mirror of the seal, and the reason the seal is allowed to blank anything. Runs under
    /// the same CK and CLEARS the blob — the earlier version of this sentence claimed the opposite
    /// ("leaves the blob in place so an interrupted unseal can be retried"), which was not what
    /// the SQL did and not what the rest of the system assumes. `folder_has_plaintext_dashboards`
    /// and the keyless-relock refusal in `reblank_folder_extras_after_verification` both depend on
    /// the blob being gone after an unlock: that absence is precisely why a board, unlike a
    /// transcript segment, cannot be re-blanked without a key. A comment asserting the reverse on
    /// a crypto-destructive path is worse than no comment, because it is the thing a later reader
    /// trusts instead of the code.
    pub(crate) fn unseal_dashboard(
        &self,
        id: &str,
        cosmetics: Option<&(String, Option<String>, Option<String>)>,
        tiles: &[RestoredTile],
    ) -> Result<()> {
        let conn = self.lock();
        // `None` means this board was never cosmetics-sealed (one with no title, emoji or tint is
        // not), so those columns are already correct and must be left alone — writing over them
        // would make the unseal destroy what the seal deliberately did not take.
        if let Some((title, emoji, tint)) = cosmetics {
            conn.execute(
                "UPDATE dashboards SET title = ?2, emoji = ?3, tint = ?4, title_blob = NULL
                  WHERE id = ?1",
                rusqlite::params![id, title, emoji, tint],
            )
            .map_err(map_err)?;
        }
        for tile in tiles {
            // Plain `?5`, NOT `COALESCE(?5, kind)`. An earlier revision coalesced, reasoning
            // about a blob written before `kind` joined the payload — but `title_blob` and
            // `config_blob` are introduced in the same change that added `kind` to it, so no such
            // blob can exist. Worse, the seal blanks `kind` to '', so the fallback value would
            // always have been that empty string rather than anything worth keeping, and the
            // coalesce silently broke the NULL-round-trips-as-NULL property every other column
            // here holds.
            conn.execute(
                "UPDATE dashboard_tiles
                    SET title = ?2, ref_id = ?3, config = ?4, kind = ?5, config_blob = NULL
                  WHERE id = ?1",
                rusqlite::params![tile.id, tile.title, tile.ref_id, tile.config, tile.kind],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    /// The sealed title blob and every sealed tile blob for one board — the unseal's source.
    pub(crate) fn sealed_dashboard_blobs(
        &self,
        folder_id: &str,
    ) -> Result<Vec<SealedDashboardBlobs>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id, title_blob FROM dashboards WHERE folder_id = ?1")
            .map_err(map_err)?;
        let boards = stmt
            .query_map(rusqlite::params![folder_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<Vec<u8>>>(1)?))
            })
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        drop(stmt);

        let mut out = Vec::new();
        for (board_id, title_blob) in boards {
            let mut tile_stmt = conn
                .prepare(
                    "SELECT id, config_blob FROM dashboard_tiles
                      WHERE dashboard_id = ?1 AND config_blob IS NOT NULL",
                )
                .map_err(map_err)?;
            let tiles = tile_stmt
                .query_map(rusqlite::params![board_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
                })
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            out.push((board_id, title_blob, tiles));
        }
        Ok(out)
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

    /// Monotonic identity/lifecycle witness for one board. The state row survives deletion, so
    /// delete→recreate and X→Y→X mutations cannot admit an answer built from stale board text.
    pub(crate) fn dashboard_context_state(&self, id: &str) -> Result<(i64, bool)> {
        let conn = self.lock();
        conn.query_row(
            "SELECT generation, exists_now FROM dashboard_context_state WHERE dashboard_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0)),
        )
        .optional()
        .map(|state| state.unwrap_or((0, false)))
        .map_err(map_err)
    }

    pub(crate) fn dashboard_structural_context_state(&self, id: &str) -> Result<(i64, bool)> {
        let conn = self.lock();
        conn.query_row(
            "SELECT structural_generation, exists_now
               FROM dashboard_context_state WHERE dashboard_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0)),
        )
        .optional()
        .map(|state| state.unwrap_or((0, false)))
        .map_err(map_err)
    }

    pub fn dashboard_count(&self) -> Result<i64> {
        let conn = self.lock();
        let n = conn
            .query_row("SELECT COUNT(*) FROM dashboards", [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(map_err)?;
        Ok(n)
    }

    /// Insert a board. `position` is appended after the current maximum so a new board lands last.
    /// Insert an UNFILED board — the shape every caller had before containers existed, kept so
    /// forty-five call sites do not have to say "no folder" to mean what they already meant.
    pub fn insert_dashboard(
        &self,
        id: &str,
        title: &str,
        emoji: Option<&str>,
        tint: Option<&str>,
        now: &str,
    ) -> Result<()> {
        self.insert_dashboard_in_folder(id, title, emoji, tint, None, now)
    }

    /// Insert a board, optionally FILED in a container.
    pub fn insert_dashboard_in_folder(
        &self,
        id: &str,
        title: &str,
        emoji: Option<&str>,
        tint: Option<&str>,
        folder_id: Option<&str>,
        now: &str,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let next: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM dashboards",
                [],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        tx.execute(
            "INSERT INTO dashboards (id, title, emoji, tint, pinned, position, folder_id,
                                     created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?7, ?6, ?6)",
            rusqlite::params![id, title, emoji, tint, next, now, folder_id],
        )
        .map_err(map_err)?;
        advance_context_generation(&tx, id, true)?;
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Patch board chrome only. Title/emoji/tint/pinned never enter the provider input, and durable
    /// history intentionally resolves title/emoji live, so this does not advance context generation.
    /// Tile/source/derived mutations below do advance it atomically with their content change.
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
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        // The FK cascade only fires with foreign_keys=ON; delete tiles explicitly so the row can
        // never be orphaned regardless of the connection pragma.
        tx.execute("DELETE FROM dashboard_tiles WHERE dashboard_id = ?1", [id])
            .map_err(map_err)?;
        let n = tx
            .execute("DELETE FROM dashboards WHERE id = ?1", [id])
            .map_err(map_err)?;
        if n > 0 {
            advance_context_generation(&tx, id, false)?;
        }
        tx.commit().map_err(map_err)?;
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

    /// Structural board layout only. Source-derived `title`/`config` are deliberately NULL until
    /// the command resolver authorizes the tile under the current lifecycle snapshot.
    pub(crate) fn list_dashboard_tile_structures(
        &self,
        dashboard_id: &str,
    ) -> Result<Vec<DashboardTile>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id,dashboard_id,kind,ref_id,NULL,span,position,NULL,created_at
                   FROM dashboard_tiles WHERE dashboard_id=?1
                  ORDER BY position ASC,created_at ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([dashboard_id], row_to_tile)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// Content-free Living-answer preflight. This reads only provenance and SQLite type tags;
    /// question/answer text is hydrated by `dashboard_living_answer_content_after_preflight`
    /// only after the command layer proves every stamped folder is readable.
    pub(crate) fn dashboard_living_answer_preflight(
        &self,
        id: &str,
    ) -> Result<Option<LivingAnswerPreflight>> {
        self.lock()
            .query_row(
                "SELECT CASE WHEN typeof(question_readable_folders_json)='text'
                             THEN question_readable_folders_json ELSE NULL END,
                        typeof(living_answer),
                        typeof(living_answered_at),
                        CASE WHEN typeof(answer_readable_folders_json)='text'
                             THEN answer_readable_folders_json ELSE NULL END,
                        CASE WHEN typeof(living_answer_context_generation)='integer'
                             THEN living_answer_context_generation ELSE NULL END,
                        CASE WHEN typeof(living_answer_context_digest)='text'
                             THEN living_answer_context_digest ELSE NULL END,
                        CASE WHEN typeof(living_answer_context_budget)='integer'
                             THEN living_answer_context_budget ELSE NULL END,
                        CASE WHEN typeof(living_answer_ask_dispatch_generation)='integer'
                             THEN living_answer_ask_dispatch_generation ELSE NULL END
                   FROM dashboard_tiles WHERE id=?1 AND kind='living_answer'",
                [id],
                |row| {
                    let question_readable_folders_json = row.get(0)?;
                    let answer_type: String = row.get(1)?;
                    let answered_at_type: String = row.get(2)?;
                    let readable_folders_json: Option<String> = row.get(3)?;
                    let context_generation: Option<i64> = row.get(4)?;
                    let context_digest: Option<String> = row.get(5)?;
                    let context_budget: Option<i64> = row.get(6)?;
                    let ask_dispatch_generation: Option<i64> = row.get(7)?;
                    let answer = if answer_type == "null"
                        && answered_at_type == "null"
                        && readable_folders_json.is_none()
                        && context_generation.is_none()
                        && context_digest.is_none()
                        && context_budget.is_none()
                        && ask_dispatch_generation.is_none()
                    {
                        LivingAnswerCacheState::Empty
                    } else if answer_type == "text" && answered_at_type == "text" {
                        match (
                            readable_folders_json,
                            context_generation,
                            context_digest,
                            context_budget,
                            ask_dispatch_generation,
                        ) {
                            (
                                Some(readable_folders_json),
                                Some(context_generation),
                                Some(context_digest),
                                Some(context_budget),
                                Some(ask_dispatch_generation),
                            ) => {
                                LivingAnswerCacheState::Valid {
                                    readable_folders_json,
                                    context_generation,
                                    context_digest,
                                    context_budget,
                                    ask_dispatch_generation,
                                }
                            }
                            _ => LivingAnswerCacheState::Malformed,
                        }
                    } else {
                        LivingAnswerCacheState::Malformed
                    };
                    Ok(LivingAnswerPreflight {
                        question_readable_folders_json,
                        answer,
                    })
                },
            )
            .optional()
            .map_err(map_err)
    }

    pub(crate) fn dashboard_living_answer_content_after_preflight_with_dispatch(
        &self,
        id: &str,
        expected_ask_dispatch_generation: i64,
        expected_context_generation: i64,
        expected_context_digest: &str,
        expected_context_budget: i64,
    ) -> Result<Option<LivingAnswerContent>> {
        self.lock()
            .query_row(
                "SELECT tile.living_question,tile.living_answer,tile.living_answered_at
                   FROM dashboard_tiles tile
                   JOIN dashboard_context_state state ON state.dashboard_id=tile.dashboard_id
                  WHERE tile.id=?1 AND tile.kind='living_answer'
                    AND state.exists_now=1 AND state.structural_generation=?3
                    AND tile.living_answer_context_generation=?3
                    AND tile.living_answer_context_digest=?4
                    AND tile.living_answer_context_budget=?5
                    AND typeof(living_question)='text'
                    AND living_answer_ask_dispatch_generation=?2
                    AND typeof(living_answer_ask_dispatch_generation)='integer'
                    AND living_answer_ask_dispatch_generation=
                        (SELECT generation FROM ask_dispatch_state WHERE singleton=1
                          AND typeof(generation)='integer' AND generation>=0)
                    AND (living_answer IS NULL OR
                         (typeof(living_answer)='text' AND
                          typeof(living_answered_at)='text'))",
                rusqlite::params![
                    id,
                    expected_ask_dispatch_generation,
                    expected_context_generation,
                    expected_context_digest,
                    expected_context_budget,
                ],
                |row| {
                    Ok(LivingAnswerContent {
                        question: row.get(0)?,
                        answer: row.get(1)?,
                        answered_at: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(map_err)
    }

    #[cfg(test)]
    pub(crate) fn dashboard_living_answer_content_after_preflight(
        &self,
        id: &str,
    ) -> Result<Option<LivingAnswerContent>> {
        let Some(preflight) = self.dashboard_living_answer_preflight(id)? else {
            return Ok(None);
        };
        let LivingAnswerCacheState::Valid {
            context_generation,
            context_digest,
            context_budget,
            ask_dispatch_generation,
            ..
        } = preflight.answer
        else {
            return Ok(None);
        };
        self.dashboard_living_answer_content_after_preflight_with_dispatch(
            id,
            ask_dispatch_generation,
            context_generation,
            &context_digest,
            context_budget,
        )
    }

    pub(crate) fn dashboard_living_question_after_preflight(
        &self,
        id: &str,
    ) -> Result<Option<String>> {
        self.lock()
            .query_row(
                "SELECT living_question FROM dashboard_tiles
                  WHERE id=?1 AND kind='living_answer' AND typeof(living_question)='text'",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_err)
    }

    #[cfg(test)]
    pub(crate) fn stamp_dashboard_question_provenance(
        &self,
        id: &str,
        question: &str,
        readable_folders_json: &str,
    ) -> Result<()> {
        self.lock()
            .execute(
                "UPDATE dashboard_tiles SET living_question=?2,question_readable_folders_json=?3
                  WHERE id=?1",
                rusqlite::params![id, question, readable_folders_json],
            )
            .map(|_| ())
            .map_err(map_err)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_dashboard_living_answer_tile(
        &self,
        id: &str,
        dashboard_id: &str,
        span: i64,
        question: &str,
        readable_folders_json: &str,
        now: &str,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let next: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(position),-1)+1 FROM dashboard_tiles WHERE dashboard_id=?1",
                [dashboard_id],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        tx.execute(
            "INSERT INTO dashboard_tiles
             (id,dashboard_id,kind,span,position,created_at,living_question,question_readable_folders_json)
             VALUES (?1,?2,'living_answer',?3,?4,?5,?6,?7)",
            rusqlite::params![id, dashboard_id, span.clamp(MIN_SPAN, MAX_SPAN), next, now, question, readable_folders_json],
        )
        .map_err(map_err)?;
        advance_context_generation(&tx, dashboard_id, true)?;
        tx.commit().map_err(map_err)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn store_dashboard_living_answer_cas_with_dispatch(
        &self,
        id: &str,
        dashboard_id: &str,
        expected_question: &str,
        answer: &str,
        answered_at: &str,
        readable_folders_json: &str,
        expected_generation: i64,
        context_digest: &str,
        context_budget: usize,
        expected_ask_dispatch_generation: i64,
    ) -> Result<bool> {
        let context_budget = i64::try_from(context_budget)
            .map_err(|_| AppError::InvalidArg("living-answer context budget is too large".into()))?;
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let current_ask_dispatch_generation = tx
            .query_row(
                "SELECT generation FROM ask_dispatch_state WHERE singleton=1
                   AND typeof(generation)='integer' AND generation>=0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(map_err)?;
        if current_ask_dispatch_generation != expected_ask_dispatch_generation {
            return Ok(false);
        }
        let state = tx
            .query_row(
                "SELECT structural_generation,exists_now
                   FROM dashboard_context_state WHERE dashboard_id=?1",
                [dashboard_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? != 0)),
            )
            .optional()
            .map_err(map_err)?;
        if state != Some((expected_generation, true)) {
            return Ok(false);
        }
        let committed_generation = expected_generation;
        let changed = tx
            .execute(
                "UPDATE dashboard_tiles SET
                   living_answer=?4,
                   living_answered_at=?5,
                   answer_readable_folders_json=?6,
                   living_answer_context_generation=?7,
                   living_answer_context_digest=?8,
                   living_answer_context_budget=?9,
                   living_answer_ask_dispatch_generation=?10
                 WHERE id=?1 AND dashboard_id=?2 AND kind='living_answer'
                   AND typeof(living_question)='text' AND living_question=?3",
                rusqlite::params![
                    id,
                    dashboard_id,
                    expected_question,
                    answer,
                    answered_at,
                    readable_folders_json,
                    committed_generation,
                    context_digest,
                    context_budget,
                    expected_ask_dispatch_generation,
                ],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Ok(false);
        }
        tx.execute(
            "UPDATE dashboard_context_state SET generation=generation+1
              WHERE dashboard_id=?1 AND exists_now=1",
            [dashboard_id],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE dashboards SET updated_at=?2 WHERE id=?1",
            rusqlite::params![dashboard_id, answered_at],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(true)
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn store_dashboard_living_answer_cas(
        &self,
        id: &str,
        dashboard_id: &str,
        expected_question: &str,
        answer: &str,
        answered_at: &str,
        readable_folders_json: &str,
        expected_generation: i64,
        context_digest: &str,
        context_budget: usize,
    ) -> Result<bool> {
        self.store_dashboard_living_answer_cas_with_dispatch(
            id,
            dashboard_id,
            expected_question,
            answer,
            answered_at,
            readable_folders_json,
            expected_generation,
            context_digest,
            context_budget,
            self.ask_dispatch_generation()?,
        )
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
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
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

    /// Content-free mutation anchor. Callers that only need ownership/kind must never hydrate
    /// `title` or `config`, which can retain source-derived text while the source is sealed.
    pub fn dashboard_tile_metadata(&self, id: &str) -> Result<Option<(String, String)>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT dashboard_id, kind FROM dashboard_tiles WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(map_err)
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
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let next: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM dashboard_tiles WHERE dashboard_id = ?1",
                [dashboard_id],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        tx.execute(
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
        advance_context_generation(&tx, dashboard_id, true)?;
        tx.commit().map_err(map_err)?;
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
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let dashboard_id = tx
            .query_row(
                "SELECT dashboard_id FROM dashboard_tiles WHERE id=?1",
                [id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(map_err)?;
        let n = tx
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
        if n > 0 {
            if let Some(dashboard_id) = dashboard_id.as_deref() {
                advance_context_generation(&tx, dashboard_id, true)?;
            }
        }
        tx.commit().map_err(map_err)?;
        Ok(n > 0)
    }

    pub fn delete_dashboard_tile(&self, id: &str) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let dashboard_id = tx
            .query_row(
                "SELECT dashboard_id FROM dashboard_tiles WHERE id=?1",
                [id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(map_err)?;
        let n = tx
            .execute("DELETE FROM dashboard_tiles WHERE id = ?1", [id])
            .map_err(map_err)?;
        if n > 0 {
            if let Some(dashboard_id) = dashboard_id.as_deref() {
                advance_context_generation(&tx, dashboard_id, true)?;
            }
        }
        tx.commit().map_err(map_err)?;
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
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let existing = {
            let mut stmt = tx
                .prepare(
                    "SELECT id FROM dashboard_tiles WHERE dashboard_id=?1
                     ORDER BY position ASC, created_at ASC",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map([dashboard_id], |row| row.get::<_, String>(0))
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            rows
        };
        let order = dense_order(tile_ids, &existing);
        for (i, tile_id) in order.iter().enumerate() {
            tx.execute(
                "UPDATE dashboard_tiles SET position = ?3
                  WHERE id = ?1 AND dashboard_id = ?2",
                rusqlite::params![tile_id, dashboard_id, i as i64],
            )
            .map_err(map_err)?;
        }
        if !order.is_empty() {
            advance_context_generation(&tx, dashboard_id, true)?;
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
        let sql = format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE id = ?1{kind_filter})");
        let exists: i64 = conn.query_row(&sql, [id], |r| r.get(0)).map_err(map_err)?;
        Ok(exists != 0)
    }

    /// Reorder BOARDS themselves (drag in the list view). Same permutation discipline as
    /// [`Self::reorder_dashboard_tiles`].
    pub fn reorder_dashboards(&self, ids: &[String]) -> Result<()> {
        let existing: Vec<String> = self.list_dashboards()?.into_iter().map(|d| d.id).collect();
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
