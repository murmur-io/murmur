//! Entity-graph + full-brain-graph storage surface — the self-assembling `entities` / `entity_mentions`
//! graph and the whole-vault "full brain" graph (nodes = meetings/notes/documents/entities, edges =
//! co-occurrence / mentions / wikilink+companion+semantic links), extracted verbatim from
//! `storage::db` (God-file split, a PURE MOVE — no behavior change). The methods below are an
//! inherent-impl split of [`crate::storage::db::Db`] across files (Rust allows one type's inherent
//! `impl` to live in multiple files of the same crate); every method retains its EXACT prior body,
//! signature, AND gating. EVERY visibility-gated read here pushes the session `unlocked` set through
//! `visibility_clause` over `entity_mentions → meetings → notes n LEFT JOIN folders f`, replicating
//! the `EXISTS(visible note) OR NOT EXISTS(any note)` predicate verbatim, so a
//! sealed-and-not-unlocked meeting contributes ZERO to nodes/edges/counts — this move relocated ONLY
//! the code, not one character of the gate. `get_entity` stays deliberately UN-gated (documented) and
//! callers gate it via `entity_is_visible` first, exactly as before.
//!
//! Shared db.rs module-level helpers `map_err` + `visibility_clause` + the Db accessor `lock` are
//! `pub(crate)` and imported below; the graph model types come from `crate::storage::models`. The
//! graph-only module-private type aliases (`FullGraphContentNode` / `FullGraphLinkRow` /
//! `FullGraphMentionEdge`), the two full-graph render caps, and the graph mapping free fns
//! (`epoch_ms_to_rfc3339`, `full_graph_link_node_kind`, `full_graph_edge_kind_from_type`) moved along
//! (they were db.rs module-private, used ONLY by these methods). `full_graph_content_nodes` stays
//! `pub(crate)` — it is called cross-file by `storage::links` via `self.full_graph_content_nodes(..)`
//! (an inherent method, resolved regardless of which file its `impl` block lives in). The seal-purge
//! helpers and `meeting_is_visible`/`visibility_clause` themselves STAY in db.rs. Tests stay in db.rs's
//! `mod tests` (shared harness); the count is conserved.

use std::collections::HashSet;
use std::str::FromStr;

use rusqlite::OptionalExtension;

use crate::error::Result;
use crate::storage::db::{
    map_err, meeting_visibility_clause, name_matches_query_tokens, row_to_meeting, tokenize_lower,
    visibility_clause, Db,
    MAX_FULL_GRAPH_LINK_EDGES, MAX_FULL_GRAPH_PER_KIND, MIN_ENTITY_NAME_LEN,
};
use crate::storage::models::{
    EntityDetail, EntityKind, EntityNeighbor, FullGraphData, FullGraphEdge, FullGraphEdgeKind,
    FullGraphNode, FullGraphNodeKind, FullGraphOpts, GraphData, GraphEdge, GraphEntity, GraphNode,
    Meeting, VaultSource,
};

impl Db {
    // ── self-assembling graph (entities + mentions) ───────────────────────────
    //
    // Sink A of the dual-sink: the encrypted DB is the source of truth for the in-app
    // graph. EVERY read below pushes the same `unlocked` set through `visibility_clause`
    // over `entity_mentions → meetings → notes n LEFT JOIN folders f`, replicating the
    // `EXISTS(visible note) OR NOT EXISTS(any note)` predicate of `list_meetings_visible`
    // verbatim — so a sealed-and-not-unlocked meeting contributes ZERO to nodes, edges,
    // and counts. The rows persist through sealing; they merely become invisible at read.

    /// Upsert an entity by `(name_ci, kind)`, case-insensitively de-duplicated. Keeps the
    /// FIRST-SEEN casing in `name` (a later "anna kowalska" does NOT overwrite "Anna Kowalska").
    /// `name_ci` uses full-Unicode `to_lowercase()` (NOT ASCII-only folding) so accented names
    /// dedup consistently. Returns the (new or existing) entity id. Race-safe: `INSERT OR IGNORE`
    /// then re-read, so a concurrent insert resolves to the single winning row.
    pub fn upsert_entity(&self, name: &str, kind: EntityKind) -> Result<String> {
        let conn = self.lock();
        let name = name.trim();
        let name_ci = name.to_lowercase();
        let kind_str = kind.as_str();
        let id = uuid::Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();
        // INSERT OR IGNORE: if `(name_ci, kind)` already exists the insert is a no-op (the
        // existing first-seen casing + id are kept). Either way we re-read the canonical id.
        conn.execute(
            "INSERT OR IGNORE INTO entities (id, name, name_ci, kind, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, name, name_ci, kind_str, created_at],
        )
        .map_err(map_err)?;
        let resolved: String = conn
            .query_row(
                "SELECT id FROM entities WHERE name_ci = ?1 AND kind = ?2",
                rusqlite::params![name_ci, kind_str],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        Ok(resolved)
    }

    /// This meeting's entity mentions, for a trash snapshot: `(entity_id, created_at)` pairs.
    ///
    /// These cascade off `meetings` on delete, so after the delete there is nothing to read. Their
    /// loss is what makes a restored meeting vanish from every person's and project's timeline while
    /// the meeting itself looks fine — the entity is still there, the meeting is still there, and
    /// the edge between them is silently gone.
    pub fn entity_mentions_for_meeting(&self, meeting_id: &str) -> Result<Vec<(String, String)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT entity_id, created_at FROM entity_mentions
                  WHERE meeting_id = ?1 ORDER BY entity_id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![meeting_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// Re-insert snapshotted mentions, preserving their ORIGINAL `created_at`.
    ///
    /// `add_mention` stamps "now", which would be wrong here: these mentions were made when the
    /// meeting happened, and the graph orders timelines by that stamp. A restore that re-dated them
    /// would put an old meeting at the top of every entity's history.
    ///
    /// A mention whose entity has since been deleted is skipped rather than failing the restore —
    /// the FK would refuse it, and losing one edge is better than refusing to bring the meeting
    /// back at all.
    pub fn restore_entity_mentions(
        &self,
        meeting_id: &str,
        mentions: &[(String, String)],
    ) -> Result<usize> {
        if mentions.is_empty() {
            return Ok(0);
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let mut restored = 0usize;
        for (entity_id, created_at) in mentions {
            let exists: bool = tx
                .query_row(
                    "SELECT 1 FROM entities WHERE id = ?1",
                    rusqlite::params![entity_id],
                    |_| Ok(true),
                )
                .optional()
                .map_err(map_err)?
                .unwrap_or(false);
            if !exists {
                continue;
            }
            restored += tx
                .execute(
                    "INSERT OR IGNORE INTO entity_mentions (entity_id, meeting_id, created_at)
                     VALUES (?1, ?2, ?3)",
                    rusqlite::params![entity_id, meeting_id, created_at],
                )
                .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(restored)
    }

    /// Record that `entity_id` was mentioned in `meeting_id`. Idempotent via the PK
    /// `(entity_id, meeting_id)` — re-summarize / re-extract never double-counts.
    pub fn add_mention(&self, entity_id: &str, meeting_id: &str) -> Result<()> {
        let conn = self.lock();
        let created_at = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR IGNORE INTO entity_mentions (entity_id, meeting_id, created_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![entity_id, meeting_id, created_at],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// All entities that have ≥1 VISIBLE mention, with that visible mention count. An entity
    /// mentioned ONLY in sealed-and-not-unlocked meetings has count 0 → dropped by `HAVING`,
    /// so its name (which lived only in encrypted markdown) never reaches the renderer.
    pub fn list_entities_visible(&self, unlocked: &HashSet<String>) -> Result<Vec<GraphNode>> {
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        // 2026-07-13 perf audit (MODERATE): this fed BOTH get_graph's node list and list_people
        // with NO limit, unlike the sibling graph_edges_visible (MAX_GRAPH_EDGES=600, added for
        // the same reason — see its own comment). Visibility is already enforced in WHERE; a
        // trailing LIMIT trims magnitude only, ordered by mention count so the most-relevant
        // entities/people survive the cut on a vault with many.
        const MAX_VISIBLE_ENTITIES: usize = 500;
        let sql = format!(
            "SELECT e.id, e.name, e.kind, COUNT(em.meeting_id) AS cnt
               FROM entities e
               JOIN entity_mentions em ON em.entity_id = e.id
               JOIN meetings m ON m.id = em.meeting_id
              WHERE {meeting_visible}
              GROUP BY e.id, e.name, e.kind
             HAVING cnt > 0
              ORDER BY cnt DESC, e.name COLLATE NOCASE ASC
              LIMIT {MAX_VISIBLE_ENTITIES}"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                let id: String = r.get(0)?;
                let name: String = r.get(1)?;
                let kind_str: String = r.get(2)?;
                let mention_count: i64 = r.get(3)?;
                Ok((id, name, kind_str, mention_count))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            let (id, name, kind_str, mention_count) = r.map_err(map_err)?;
            out.push(GraphNode {
                id,
                name,
                kind: EntityKind::from_str(&kind_str)?,
                mention_count,
            });
        }
        Ok(out)
    }

    /// The TRUE count of entities with ≥1 VISIBLE mention — same predicate as
    /// `list_entities_visible` but with NO `LIMIT`, so callers can detect the 500-row cap
    /// truncating the result and disclose it (added 2026-07-13: the cap trimmed `get_graph`'s
    /// node list and `list_people`'s roster silently, with no signal the FE could show — the
    /// existing `hasHidden`/`has_hidden_folders` flag only reports LOCKED folders, so on a vault
    /// with >500 visible entities and zero locked folders it stayed false while 100+ entities were
    /// dropped). `list_entities_visible(unlocked).len() < count_entities_visible(unlocked, None)`
    /// means the cap trimmed rows. `kind` optionally narrows to one `EntityKind` (e.g. `Person`,
    /// mirroring how `list_people` filters `list_entities_visible`'s output to persons) — the cap
    /// applies BEFORE that filter, so the all-kinds and Person-only totals must be counted
    /// separately rather than derived from each other.
    pub fn count_entities_visible(
        &self,
        unlocked: &HashSet<String>,
        kind: Option<EntityKind>,
    ) -> Result<i64> {
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let kind_filter = match kind {
            Some(k) => format!("AND e.kind = '{}'", k.as_str()),
            None => String::new(),
        };
        let sql = format!(
            "SELECT COUNT(*) FROM (
               SELECT e.id
                 FROM entities e
                 JOIN entity_mentions em ON em.entity_id = e.id
                 JOIN meetings m ON m.id = em.meeting_id
                WHERE {meeting_visible}
                      {kind_filter}
                GROUP BY e.id
               HAVING COUNT(em.meeting_id) > 0
             )"
        );
        conn.query_row(&sql, [], |r| r.get(0)).map_err(map_err)
    }

    /// The VISIBLE meetings mentioning `entity_id`, newest first, as `VaultSource` chips
    /// (the same shape the FE uses for backlink chips). Sealed-not-unlocked meetings excluded.
    pub fn entity_mentions_visible(
        &self,
        entity_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<VaultSource>> {
        self.entity_mentions_visible_limited(entity_id, unlocked, usize::MAX)
    }

    /// The newest bounded window of VISIBLE meetings mentioning `entity_id`.
    ///
    /// The limit is applied by SQLite after the visibility predicate and stable
    /// newest-first ordering. Callers that need an honest truncation bit should
    /// request `display_limit + 1`; this avoids materialising an unbounded
    /// mention set merely to learn that more rows exist.
    pub fn entity_mentions_visible_limited(
        &self,
        entity_id: &str,
        unlocked: &HashSet<String>,
        limit: usize,
    ) -> Result<Vec<VaultSource>> {
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let row_limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let sql = format!(
            "SELECT m.id, m.title, m.started_at
               FROM entity_mentions em
               JOIN meetings m ON m.id = em.meeting_id
              WHERE em.entity_id = ?1
                AND {meeting_visible}
              ORDER BY m.started_at DESC, m.id DESC
              LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![entity_id, row_limit], |r| {
                let meeting_id: String = r.get(0)?;
                let title: Option<String> = r.get(1)?;
                let started_at: String = r.get(2)?;
                Ok(VaultSource {
                    meeting_id,
                    title: title.unwrap_or_default(),
                    started_at,
                    origin: None,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Entity↔entity co-occurrence edges: two entities sharing the SAME visible meeting, weighted
    /// by the number of shared visible meetings. Pair-deduped via `a.entity_id < b.entity_id`
    /// → exactly one undirected edge per pair, `source < target`. Both endpoints' meetings are
    /// gated by the visibility predicate, so a co-occurrence in a sealed meeting yields no edge.
    pub fn graph_edges_visible(&self, unlocked: &HashSet<String>) -> Result<Vec<GraphEdge>> {
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        // F5: bound the quadratic co-occurrence self-join — return only the strongest edges. The
        // graph UI (brain-map, ≤60 nodes) consumes only the heaviest connections, so an unbounded
        // ORDER BY over every co-mentioned entity pair is wasted serialization on a large vault.
        // Visibility is already enforced in WHERE; a trailing LIMIT trims magnitude only — it can
        // never widen what is visible.
        const MAX_GRAPH_EDGES: usize = 600;
        // PR-9 F3: `weight DESC` alone leaves ties in engine-arbitrary order → the surviving 600-edge
        // subset could vary between opens, contradicting the graph's "Deterministic" claim. Break ties
        // on the pair identity (`a.entity_id`, then `b.entity_id`, both already `a < b`-canonicalized
        // above) so identical data → the identical edge set every call.
        let sql = format!(
            "SELECT a.entity_id, b.entity_id, COUNT(*) AS weight
               FROM entity_mentions a
               JOIN entity_mentions b
                 ON a.meeting_id = b.meeting_id AND a.entity_id < b.entity_id
               JOIN meetings m ON m.id = a.meeting_id
              WHERE {meeting_visible}
              GROUP BY a.entity_id, b.entity_id
              ORDER BY weight DESC, a.entity_id ASC, b.entity_id ASC
              LIMIT {MAX_GRAPH_EDGES}"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(GraphEdge {
                    source: r.get(0)?,
                    target: r.get(1)?,
                    weight: r.get(2)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Whether `entity_id` has ≥1 VISIBLE mention right now — i.e. at least one mention lands in a
    /// meeting that is visible under the SAME predicate as `list_meetings_visible` / the other
    /// graph reads (`EXISTS(visible note) OR NOT EXISTS(any note)`). An entity mentioned ONLY in
    /// sealed-and-not-unlocked meetings returns `false`, so its name (which lived only in encrypted
    /// markdown) can never leak through `get_entity` / `build_entity_detail`. This is the gate the
    /// detail path was missing: `get_entity` itself reads the raw `entities` row with no visibility
    /// predicate, so callers that expose an entity to the FE MUST go through this check first.
    pub fn entity_is_visible(&self, entity_id: &str, unlocked: &HashSet<String>) -> Result<bool> {
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let sql = format!(
            "SELECT EXISTS (
                      SELECT 1
                        FROM entity_mentions em
                        JOIN meetings m ON m.id = em.meeting_id
                       WHERE em.entity_id = ?1
                         AND {meeting_visible}
                    )"
        );
        let visible: bool = conn
            .query_row(&sql, rusqlite::params![entity_id], |r| {
                Ok(r.get::<_, i64>(0)? != 0)
            })
            .map_err(map_err)?;
        Ok(visible)
    }

    /// One entity row by id (`None` if absent), with its first-seen casing. NOTE: this reads the
    /// raw `entities` row WITHOUT a visibility predicate — it must NOT be exposed to the FE for an
    /// arbitrary id without first gating on [`entity_is_visible`] (see `build_entity_detail`).
    pub fn get_entity(&self, entity_id: &str) -> Result<Option<GraphEntity>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT id, name, kind, created_at FROM entities WHERE id = ?1",
                rusqlite::params![entity_id],
                |r| {
                    let id: String = r.get(0)?;
                    let name: String = r.get(1)?;
                    let kind_str: String = r.get(2)?;
                    let created_at: String = r.get(3)?;
                    Ok((id, name, kind_str, created_at))
                },
            )
            .optional()
            .map_err(map_err)?;
        match row {
            Some((id, name, kind_str, created_at)) => Ok(Some(GraphEntity {
                id,
                name,
                kind: EntityKind::from_str(&kind_str)?,
                created_at,
            })),
            None => Ok(None),
        }
    }

    /// The top-`limit` entities co-occurring with `entity_id` (the neighborhood satellites),
    /// ranked by shared VISIBLE meeting count. Both the anchor's and the neighbor's mention must
    /// land in a visible meeting, so sealed co-occurrences never surface a neighbor.
    pub fn entity_neighbors_visible(
        &self,
        entity_id: &str,
        unlocked: &HashSet<String>,
        limit: i64,
    ) -> Result<Vec<EntityNeighbor>> {
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let sql = format!(
            "SELECT e.id, e.name, e.kind, COUNT(*) AS shared
               FROM entity_mentions a
               JOIN entity_mentions b ON a.meeting_id = b.meeting_id AND b.entity_id != a.entity_id
               JOIN entities e ON e.id = b.entity_id
               JOIN meetings m ON m.id = a.meeting_id
              WHERE a.entity_id = ?1
                AND {meeting_visible}
              GROUP BY e.id, e.name, e.kind
              ORDER BY shared DESC, e.name COLLATE NOCASE ASC
              LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![entity_id, limit], |r| {
                let id: String = r.get(0)?;
                let name: String = r.get(1)?;
                let kind_str: String = r.get(2)?;
                let shared_meetings: i64 = r.get(3)?;
                Ok((id, name, kind_str, shared_meetings))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            let (id, name, kind_str, shared_meetings) = r.map_err(map_err)?;
            out.push(EntityNeighbor {
                id,
                name,
                kind: EntityKind::from_str(&kind_str)?,
                shared_meetings,
            });
        }
        Ok(out)
    }

    /// QUERY→ENTITY resolution for GraphRAG-lite (Phase 2d) — DETERMINISTIC, no LLM. Returns the
    /// ids of VISIBLE entities whose name appears as a whole-token match (see
    /// [`name_matches_query_tokens`]) inside `query`, case-insensitively. Gated by EXACTLY the
    /// `list_entities_visible` predicate: an entity mentioned ONLY in a sealed-and-not-unlocked
    /// folder is never resolved (its name lived only in encrypted markdown, so resolving it would
    /// leak its existence). A name shorter than [`MIN_ENTITY_NAME_LEN`] chars is skipped (noise
    /// guard). Empty query or no match → empty vec, leaving the hybrid path unchanged.
    pub fn entities_matching_query(
        &self,
        query: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<String>> {
        let q_tokens = tokenize_lower(query);
        if q_tokens.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        // Same visibility predicate as `list_entities_visible`: keep an entity iff it has ≥1
        // mention in a meeting that is open/NULL-folder OR session-unlocked (or note-less).
        let sql = format!(
            "SELECT DISTINCT e.id, e.name_ci
               FROM entities e
               JOIN entity_mentions em ON em.entity_id = e.id
               JOIN meetings m ON m.id = em.meeting_id
              WHERE {meeting_visible}"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                let id: String = r.get(0)?;
                let name_ci: String = r.get(1)?;
                Ok((id, name_ci))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            let (id, name_ci) = r.map_err(map_err)?;
            if name_ci.chars().count() >= MIN_ENTITY_NAME_LEN
                && name_matches_query_tokens(&q_tokens, &name_ci)
            {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// GATED entity-neighbour candidates for GraphRAG-lite (Phase 2d): the VISIBLE meetings
    /// mentioning ANY of `entity_ids`, ranked by how many of the matched entities they touch (desc)
    /// then recency. Uses EXACTLY the `list_meetings_visible`/graph visibility predicate, so a
    /// sealed-and-not-unlocked meeting NEVER appears even if it mentions a matched entity. Empty
    /// input → empty vec.
    pub fn meetings_mentioning_entities_visible(
        &self,
        entity_ids: &[String],
        unlocked: &HashSet<String>,
        scope: Option<&[String]>,
    ) -> Result<Vec<Meeting>> {
        if entity_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let scoped = crate::storage::db::folder_scope_clause("m", scope);
        let placeholders = (1..=entity_ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT m.id, m.started_at, m.ended_at, m.title, m.duration_s, m.audio_path, m.status, \
                    m.folder_id \
               FROM entity_mentions em \
               JOIN meetings m ON m.id = em.meeting_id \
              WHERE em.entity_id IN ({placeholders}) \
                AND {meeting_visible}{scoped} \
              GROUP BY m.id \
              ORDER BY COUNT(DISTINCT em.entity_id) DESC, m.started_at DESC, m.id DESC"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let params = rusqlite::params_from_iter(entity_ids.iter());
        let rows = stmt.query_map(params, row_to_meeting).map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)??);
        }
        Ok(out)
    }

    /// Whether ANY folder is sealed-and-not-unlocked right now (i.e. a locked folder whose id is
    /// NOT in the session `unlocked` set). Drives the FE's one honest "some entities hidden"
    /// disclosure banner — it never leaks how many or which.
    pub fn has_hidden_folders(&self, unlocked: &HashSet<String>) -> Result<bool> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id FROM folders WHERE locked = 1")
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(map_err)?;
        for r in rows {
            let id = r.map_err(map_err)?;
            if !unlocked.contains(&id) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Build the full graph payload (`get_graph`): all visible nodes + all visible edges +
    /// the hidden-folder disclosure flag, snapshotting the passed-in session `unlocked` set.
    pub fn build_graph(&self, unlocked: &HashSet<String>) -> Result<GraphData> {
        let nodes = self.list_entities_visible(unlocked)?;
        let edges = self.graph_edges_visible(unlocked)?;
        let has_hidden = self.has_hidden_folders(unlocked)?;
        let total_visible_entities = self.count_entities_visible(unlocked, None)?;
        Ok(GraphData {
            nodes,
            edges,
            has_hidden,
            total_visible_entities,
        })
    }

    /// Build the FULL-BRAIN graph payload (`get_full_graph`, DESIGN §PR-4). A PURE READ — no writes,
    /// no new storage — that unifies FOUR node kinds and FIVE edge kinds under one visibility model:
    ///
    /// NODES (each via its EXISTING gated reader; a sealed-and-not-session-unlocked item yields none):
    ///   • entities  — `list_entities_visible` (already 500-capped, HAVING visible mention);
    ///   • meetings  — `list_meetings_visible` (visible if no notes OR any visible note);
    ///   • notes + documents — `full_graph_content_nodes` over `documents JOIN folders` under
    ///     `visibility_clause` (a sealed folder's rows are absent), split by `kind`.
    ///
    /// EDGES (BOTH endpoints must be in the visible-node set built above — an edge to a sealed node
    /// is DROPPED, so no edge can leak a hidden node's existence):
    ///   • co_occurrence — entity↔entity (`graph_edges_visible`, already gated);
    ///   • mention       — entity→meeting (`entity_mentions`, gated by the SAME meeting predicate);
    ///   • wikilink/companion/semantic — `links` rows with `status='active'` (+ `status='suggested'`
    ///     semantic ONLY when `opts.include_suggested`), each endpoint re-checked against the set.
    ///
    /// Every edge carries `src_kind`/`dst_kind` (the endpoint node kinds it was gated on) so the FE
    /// matches endpoints by `(kind, id)`, safe against a cross-kind id collision (PR-9 F4).
    ///
    /// Deterministic: nodes ordered by (kind, id ASC), edges by (kind, src, dst, status); every edge
    /// leg has a deterministic `ORDER BY` before its cap (co_occurrence by weight then pair id;
    /// mention by meeting recency; links by score). Honest caps: `total_visible_nodes` is the pre-cap
    /// true NODE count so the FE can disclose a silent node trim; `edges_truncated` is true when an
    /// EDGE-leg cap (mention/links LIMIT) trimmed edges; `has_hidden` reflects LOCKED folders (mirrors
    /// the entity graph). No PII logged — ids/counts only.
    pub fn build_full_graph(
        &self,
        unlocked: &HashSet<String>,
        opts: FullGraphOpts,
    ) -> Result<FullGraphData> {
        // ── NODES: enumerate via the existing gated readers, keyed by (kind_str, id) so a cross-kind
        //    id collision can never conflate two nodes when an edge is gated. ──
        let mut nodes: Vec<FullGraphNode> = Vec::new();
        let mut visible: HashSet<(&'static str, String)> = HashSet::new();

        // Entities (already render-capped at 500 inside the reader).
        for e in self.list_entities_visible(unlocked)? {
            visible.insert((FullGraphNodeKind::Entity.as_str(), e.id.clone()));
            nodes.push(FullGraphNode {
                id: e.id,
                kind: FullGraphNodeKind::Entity,
                label: e.name,
                date: None,
                degree: 0,
            });
        }
        // Meetings (visible-meeting predicate; capped to keep the payload bounded).
        for m in self.list_meetings_visible(MAX_FULL_GRAPH_PER_KIND as i64, unlocked, None)? {
            visible.insert((FullGraphNodeKind::Meeting.as_str(), m.id.clone()));
            let label = m
                .title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .unwrap_or("Meeting")
                .to_string();
            nodes.push(FullGraphNode {
                id: m.id,
                kind: FullGraphNodeKind::Meeting,
                label,
                date: Some(m.started_at),
                degree: 0,
            });
        }
        // Notes + documents (one gated pass over `documents`, split by kind).
        for (kind, id, label, date) in self.full_graph_content_nodes(unlocked)? {
            visible.insert((kind.as_str(), id.clone()));
            nodes.push(FullGraphNode {
                id,
                kind,
                label,
                date,
                degree: 0,
            });
        }
        // TRUE (uncapped) visible-node count for honest disclosure — computed with NO per-kind LIMIT,
        // so `total_visible_nodes > nodes.len()` whenever a per-kind cap silently trimmed a leg (the
        // twin of the entity graph's `total_visible_entities`). Mirrors, never derives from, the capped
        // `nodes.len()`.
        let total_visible_nodes = self.count_full_graph_nodes_visible(unlocked)?;

        // ── EDGES: every leg is dropped unless BOTH endpoints are in `visible`. Degree is tallied
        //    INLINE with the true endpoint node-kinds (a links edge can connect meeting↔note, so the
        //    endpoint kinds are NOT derivable from the edge kind alone — they must be carried here). ──
        let mut edges: Vec<FullGraphEdge> = Vec::new();
        let mut degree: std::collections::HashMap<(&'static str, String), i64> =
            std::collections::HashMap::new();
        let bump = |degree: &mut std::collections::HashMap<(&'static str, String), i64>,
                    k: &'static str,
                    id: &str| {
            *degree.entry((k, id.to_string())).or_insert(0) += 1;
        };
        let ent = FullGraphNodeKind::Entity.as_str();
        let mtg = FullGraphNodeKind::Meeting.as_str();

        // co_occurrence — entity↔entity (already visibility-gated; re-check both endpoints against the
        // capped node set so an edge to a >500-cap-dropped entity is also dropped).
        for ge in self.graph_edges_visible(unlocked)? {
            if visible.contains(&(ent, ge.source.clone()))
                && visible.contains(&(ent, ge.target.clone()))
            {
                bump(&mut degree, ent, &ge.source);
                bump(&mut degree, ent, &ge.target);
                edges.push(FullGraphEdge {
                    src: ge.source,
                    dst: ge.target,
                    src_kind: FullGraphNodeKind::Entity,
                    dst_kind: FullGraphNodeKind::Entity,
                    kind: FullGraphEdgeKind::CoOccurrence,
                    score: ge.weight as f64,
                    status: "active".to_string(),
                });
            }
        }
        // mention — entity→meeting (gated by the SAME meeting-visible predicate; both endpoints then
        // re-checked against the node set, so a mention into a sealed OR cap-dropped meeting is gone).
        // `edges_truncated` accumulates whether ANY edge leg hit its cap, so the FE can disclose it.
        let (mention_edges, mentions_truncated) = self.entity_meeting_mentions_visible(unlocked)?;
        let (link_rows, links_truncated) = self.full_graph_links(opts.include_suggested)?;
        // True when a genuine EDGE-leg cap (the mention or links LIMIT) trimmed rows — distinct from
        // an edge dropped because its endpoint fell to a NODE cap (that is the node disclosure's job).
        let edges_truncated = mentions_truncated || links_truncated;
        for (entity_id, meeting_id, weight) in mention_edges {
            if visible.contains(&(ent, entity_id.clone()))
                && visible.contains(&(mtg, meeting_id.clone()))
            {
                bump(&mut degree, ent, &entity_id);
                bump(&mut degree, mtg, &meeting_id);
                edges.push(FullGraphEdge {
                    src: entity_id,
                    dst: meeting_id,
                    src_kind: FullGraphNodeKind::Entity,
                    dst_kind: FullGraphNodeKind::Meeting,
                    kind: FullGraphEdgeKind::Mention,
                    score: weight as f64,
                    status: "active".to_string(),
                });
            }
        }
        // links rows — wikilink/companion/manual/semantic. active always; suggested semantic behind the flag.
        // BOTH endpoints re-checked against the visible-node set (the links kind strings map 1:1 onto
        // the node-kind strings: meeting|note|document), so a link to a sealed/absent node is dropped.
        for (src_kind, src_id, dst_kind, dst_id, edge_type, score, status) in link_rows {
            let (Some(src_nk), Some(dst_nk)) = (
                full_graph_link_node_kind(&src_kind),
                full_graph_link_node_kind(&dst_kind),
            ) else {
                continue; // corrupt/unknown kind → skip defensively.
            };
            let (src_k, dst_k) = (src_nk.as_str(), dst_nk.as_str());
            if !visible.contains(&(src_k, src_id.clone()))
                || !visible.contains(&(dst_k, dst_id.clone()))
            {
                continue;
            }
            let Some(kind) = full_graph_edge_kind_from_type(&edge_type) else {
                continue; // unknown edge_type → skip.
            };
            bump(&mut degree, src_k, &src_id);
            bump(&mut degree, dst_k, &dst_id);
            edges.push(FullGraphEdge {
                src: src_id,
                dst: dst_id,
                src_kind: src_nk,
                dst_kind: dst_nk,
                kind,
                score,
                status,
            });
        }

        // ── Degrees: assign each node its in-graph incident-edge count (a layout hint, never the true
        //    corpus degree). Keyed by (kind, id) to avoid cross-kind id collisions. ──
        for n in &mut nodes {
            n.degree = degree
                .get(&(n.kind.as_str(), n.id.clone()))
                .copied()
                .unwrap_or(0);
        }

        // Deterministic ordering: nodes by (kind, id ASC); edges by (kind, src, dst, status).
        nodes.sort_by(|a, b| {
            a.kind
                .as_str()
                .cmp(b.kind.as_str())
                .then_with(|| a.id.cmp(&b.id))
        });
        edges.sort_by(|a, b| {
            a.kind
                .as_str()
                .cmp(b.kind.as_str())
                .then_with(|| a.src.cmp(&b.src))
                .then_with(|| a.dst.cmp(&b.dst))
                .then_with(|| a.status.cmp(&b.status))
        });

        let has_hidden = self.has_hidden_folders(unlocked)?;
        tracing::debug!(
            target: "graph",
            nodes = nodes.len(),
            edges = edges.len(),
            has_hidden,
            edges_truncated,
            "build_full_graph resolved"
        );
        Ok(FullGraphData {
            nodes,
            edges,
            has_hidden,
            total_visible_nodes,
            edges_truncated,
        })
    }

    /// The VISIBLE note + document nodes for the full-brain graph: `(kind, id, label, date)` per row,
    /// gated by `visibility_clause` over `documents JOIN folders` (a sealed-and-not-session-unlocked
    /// folder's rows are ABSENT — never masked). `kind='note'` → [`FullGraphNodeKind::Note`], anything
    /// else → [`FullGraphNodeKind::Document`]. `label` = title, else the filesystem `name`. `date` =
    /// the `updated_at`/`created_at` timestamp UNIFIED to an RFC3339 ISO string (PR-9 F4 — meetings
    /// already emit ISO `started_at`; notes/docs previously emitted the raw epoch-ms as a string,
    /// giving the FE two incompatible date formats on the SAME field). Per-kind capped
    /// (`MAX_FULL_GRAPH_PER_KIND`) ordered newest-first so the most-relevant rows survive on a big
    /// vault; visibility is enforced in WHERE, so the cap trims magnitude only.
    pub(crate) fn full_graph_content_nodes(
        &self,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<FullGraphContentNode>> {
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        // Two capped passes (one per kind) so notes and documents each get their own budget rather
        // than one starving the other on a lopsided vault.
        let mut out: Vec<FullGraphContentNode> = Vec::new();
        for (kind_str, node_kind) in [
            ("note", FullGraphNodeKind::Note),
            ("document", FullGraphNodeKind::Document),
        ] {
            let sql = format!(
                "SELECT d.id, COALESCE(NULLIF(TRIM(d.title), ''), d.name),
                        COALESCE(d.updated_at, d.created_at)
                   FROM documents d
                   JOIN folders f ON f.id = d.folder_id
                  WHERE d.kind = ?1 AND {visible}
                  ORDER BY COALESCE(d.updated_at, d.created_at) DESC, d.id ASC
                  LIMIT {MAX_FULL_GRAPH_PER_KIND}"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![kind_str], |r| {
                    let id: String = r.get(0)?;
                    let label: String = r.get(1)?;
                    let ts: Option<i64> = r.get(2)?;
                    Ok((id, label, ts))
                })
                .map_err(map_err)?;
            for r in rows {
                let (id, label, ts) = r.map_err(map_err)?;
                // Unify to RFC3339 so the FE sees ONE date format across every node kind. epoch-ms →
                // UTC ISO; an out-of-range/absent timestamp yields None (a bad hint, never a panic).
                let date = ts.and_then(epoch_ms_to_rfc3339);
                out.push((node_kind, id, label, date));
            }
        }
        Ok(out)
    }

    /// The TRUE (UNCAPPED) count of VISIBLE full-brain nodes: entities + meetings + notes +
    /// documents, each under the SAME visibility predicate as its node reader but with NO per-kind
    /// LIMIT. Drives `FullGraphData::total_visible_nodes` so the FE can disclose when a per-kind
    /// render cap silently trimmed the graph (mirrors `count_entities_visible` for the entity graph).
    /// A sealed-and-not-session-unlocked item is NOT counted (leak-free — a bare total).
    pub(crate) fn count_full_graph_nodes_visible(&self, unlocked: &HashSet<String>) -> Result<i64> {
        // Entities reuse the dedicated uncapped counter (same HAVING-visible-mention predicate).
        let entities = self.count_entities_visible(unlocked, None)?;
        let conn = self.lock();
        // Meetings — the `list_meetings_visible` predicate, uncapped.
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let meetings: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM meetings m WHERE {meeting_visible}"
                ),
                [],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        // Notes + documents — folder-gated, uncapped, both kinds.
        let d_visible = visibility_clause("f", unlocked);
        let docs: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM documents d
                       JOIN folders f ON f.id = d.folder_id
                      WHERE d.kind IN ('note', 'document') AND {d_visible}"
                ),
                [],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        Ok(entities + meetings + docs)
    }

    /// Entity→meeting MENTION edges for the full-brain graph: `(entity_id, meeting_id, weight)` for
    /// every mention landing in a VISIBLE meeting (the SAME `EXISTS(visible note) OR NOT EXISTS(any
    /// note)` predicate as `list_meetings_visible`/`graph_edges_visible`). `weight` is always 1 (one
    /// mention row per (entity, meeting) — the PK guarantees it); kept as a field for edge-uniformity.
    /// A mention into a sealed-and-not-session-unlocked meeting yields NO row, so it can never surface
    /// a hidden meeting.
    ///
    /// PR-9 F2: the previous `ORDER BY em.entity_id ASC` truncated at `MAX_MENTION_EDGES` in
    /// arbitrary entity-UUID order, so entities LATE in UUID order silently lost ALL their mention
    /// edges (rendered isolated, degree 0, with no signal). Order by meeting RECENCY first
    /// (`m.started_at DESC`) so the freshest mentions survive the cap regardless of UUID, with a
    /// deterministic `(entity_id, meeting_id)` tiebreak so identical data → the identical subset.
    /// Returns `(rows, truncated)` — `truncated` is true when the cap trimmed rows (there was a
    /// `MAX_MENTION_EDGES + 1`th visible mention), so the caller can disclose the edge trim.
    pub(crate) fn entity_meeting_mentions_visible(
        &self,
        unlocked: &HashSet<String>,
    ) -> Result<(Vec<FullGraphMentionEdge>, bool)> {
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        const MAX_MENTION_EDGES: usize = 2000;
        // Fetch ONE past the cap so a full page distinguishes "exactly the cap" from "trimmed".
        let sql = format!(
            "SELECT em.entity_id, em.meeting_id
               FROM entity_mentions em
               JOIN meetings m ON m.id = em.meeting_id
              WHERE {meeting_visible}
              ORDER BY m.started_at DESC, em.entity_id ASC, em.meeting_id ASC
              LIMIT {}",
            MAX_MENTION_EDGES + 1
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, 1i64))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        let truncated = out.len() > MAX_MENTION_EDGES;
        out.truncate(MAX_MENTION_EDGES);
        Ok((out, truncated))
    }

    /// Raw `links` rows for the full-brain graph, PRE-gating (the caller gates both endpoints against
    /// the visible-node set). `status='active'` always; `status='suggested'` semantic rows ONLY when
    /// `include_suggested`. `dismissed` rows are NEVER returned. Returns
    /// `(rows, truncated)` where each row is `(src_kind, src_id, dst_kind, dst_id, edge_type, score,
    /// status)` ordered deterministically. This reads no content columns — ids/kinds/metadata only —
    /// and is gated by the caller, so it is leak-free at every call site.
    ///
    /// PR-9 F2: `links` is the FASTEST-growing edge leg (every wikilink/companion/semantic suggestion
    /// is a row) and was read UNBOUNDED — the per-kind node caps existed while the highest-cardinality
    /// edge table had none. Bound it at `MAX_FULL_GRAPH_LINK_EDGES`, ordered by score DESC (strongest
    /// links survive) with a deterministic `(edge_type, src_id, dst_id, id)` tiebreak, and report
    /// `truncated` so the caller can disclose the edge trim (mirrors the mention-edge cap).
    pub(crate) fn full_graph_links(
        &self,
        include_suggested: bool,
    ) -> Result<(Vec<FullGraphLinkRow>, bool)> {
        let conn = self.lock();
        // active: any edge_type. suggested: ONLY semantic (wikilink/companion are always active by
        // construction; there is no "suggested wikilink"). dismissed tombstones stay excluded.
        let status_pred = if include_suggested {
            "(status = 'active' OR (status = 'suggested' AND edge_type = 'semantic'))"
        } else {
            "status = 'active'"
        };
        // Fetch ONE past the cap to distinguish "exactly the cap" from "trimmed".
        let sql = format!(
            "SELECT src_kind, src_id, dst_kind, dst_id, edge_type, score, status
               FROM links
              WHERE {status_pred}
                AND src_kind IN ('meeting','note','document')
                AND dst_kind IN ('meeting','note','document')
              ORDER BY score DESC, edge_type ASC, src_id ASC, dst_id ASC, id ASC
              LIMIT {}",
            MAX_FULL_GRAPH_LINK_EDGES + 1
        );
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, f64>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_err)?);
        }
        let truncated = out.len() > MAX_FULL_GRAPH_LINK_EDGES;
        out.truncate(MAX_FULL_GRAPH_LINK_EDGES);
        Ok((out, truncated))
    }

    /// Build the detail payload for one entity (`get_entity_detail`): the entity, its visible
    /// backlinked meetings, and its top co-occurring neighbors. `None` if the entity is unknown
    /// OR has ZERO visible mentions (mentioned only in sealed-not-unlocked meetings). The
    /// visibility gate is mandatory here: `get_entity` reads the raw `entities` row with NO
    /// predicate, so without this check a caller holding a stale entity id (cached from a prior
    /// open-folder `get_graph`, before the folder was sealed/auto-relocked) could read back the
    /// entity's `name` — which lived only in the sealed meeting's encrypted markdown. Routing
    /// through `entity_is_visible` keeps the detail path consistent with every other graph read.
    pub fn build_entity_detail(
        &self,
        entity_id: &str,
        unlocked: &HashSet<String>,
        neighbor_limit: i64,
    ) -> Result<Option<EntityDetail>> {
        // Anti-leak gate FIRST: a sealed-only entity is indistinguishable from an unknown id.
        if !self.entity_is_visible(entity_id, unlocked)? {
            return Ok(None);
        }
        let entity = match self.get_entity(entity_id)? {
            Some(e) => e,
            None => return Ok(None),
        };
        let meetings = self.entity_mentions_visible(entity_id, unlocked)?;
        let neighbors = self.entity_neighbors_visible(entity_id, unlocked, neighbor_limit)?;
        Ok(Some(EntityDetail {
            entity,
            meetings,
            neighbors,
        }))
    }

    /// Exact-ID entity projection for dashboard tiles. It applies the same mention visibility gate
    /// as the capped list without hydrating unrelated entity names or dropping a selected entity
    /// merely because 500 others rank ahead of it.
    pub(crate) fn get_entity_node_visible(
        &self,
        entity_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<GraphNode>> {
        let conn = self.lock();
        let meeting_visible = meeting_visibility_clause("m", unlocked);
        let sql = format!(
            "SELECT e.id,e.name,e.kind,COUNT(em.meeting_id) AS cnt
               FROM entities e
               JOIN entity_mentions em ON em.entity_id=e.id
               JOIN meetings m ON m.id=em.meeting_id
              WHERE e.id=?1 AND {meeting_visible}
              GROUP BY e.id,e.name,e.kind HAVING cnt>0"
        );
        let row = conn
            .query_row(&sql, [entity_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .optional()
            .map_err(map_err)?;
        row.map(|(id, name, kind, mention_count)| {
            Ok(GraphNode {
                id,
                name,
                kind: EntityKind::from_str(&kind)?,
                mention_count,
            })
        })
        .transpose()
    }
}

/// One VISIBLE note/document node row for the full-brain graph, pre-typed: `(kind, id, label, date)`
/// (`date` = the `updated_at`/`created_at` epoch-ms as a string). Aliased to keep
/// [`Db::full_graph_content_nodes`]'s return under clippy's type-complexity bar.
type FullGraphContentNode = (FullGraphNodeKind, String, String, Option<String>);

/// One raw `links` row for the full-brain graph, PRE-gating: `(src_kind, src_id, dst_kind, dst_id,
/// edge_type, score, status)` (ids/kinds/metadata only — no content). Aliased to keep
/// [`Db::full_graph_links`]'s return under clippy's type-complexity bar.
type FullGraphLinkRow = (String, String, String, String, String, f64, String);

/// One entity→meeting MENTION edge for the full-brain graph: `(entity_id, meeting_id, weight)`.
/// Aliased to keep [`Db::entity_meeting_mentions_visible`]'s `(Vec<_>, truncated)` return under
/// clippy's type-complexity bar (PR-9 F2 added the `truncated` flag).
type FullGraphMentionEdge = (String, String, i64);

/// Convert an epoch-MILLISECONDS timestamp to an RFC3339 UTC string (PR-9 F4: unify the
/// full-graph node `date` format — meetings already carry ISO `started_at`; notes/docs store
/// epoch-ms). `None` for an out-of-range value (never a panic — a bad date hint is dropped, not
/// fatal). Uses the same `chrono` dependency already in use across storage.
fn epoch_ms_to_rfc3339(ms: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).map(|dt| dt.to_rfc3339())
}

/// Map a persisted `links.src_kind`/`dst_kind` string onto the STABLE full-graph node kind
/// (`meeting`/`note`/`document`). `None` for anything unknown (a corrupt row → its edge is dropped).
/// PR-9 F4: returns the typed [`FullGraphNodeKind`] (not a bare `&str`) so the caller can carry the
/// endpoint kinds onto `FullGraphEdge` — the FE then matches endpoints by `(kind, id)`, not bare id,
/// closing a cross-kind id-collision mismatch. Its `.as_str()` remains the visible-node-set key.
fn full_graph_link_node_kind(kind: &str) -> Option<FullGraphNodeKind> {
    match kind {
        "meeting" => Some(FullGraphNodeKind::Meeting),
        "note" => Some(FullGraphNodeKind::Note),
        "document" => Some(FullGraphNodeKind::Document),
        _ => None,
    }
}

/// Map a persisted `links.edge_type` onto the full-graph edge kind. `None` for an unknown type (the
/// edge is dropped). co_occurrence/mention are computed edges, never stored, so they are not here.
fn full_graph_edge_kind_from_type(edge_type: &str) -> Option<FullGraphEdgeKind> {
    match edge_type {
        "wikilink" => Some(FullGraphEdgeKind::Wikilink),
        "companion" => Some(FullGraphEdgeKind::Companion),
        "semantic" => Some(FullGraphEdgeKind::Semantic),
        // A USER-created "Related" link (`upsert_manual_link`). Deterministic + always active — WITHOUT
        // this arm every manual edge fell to `None` and was dropped, so a manually-linked note/meeting/
        // document showed "0 connections" in the full-brain graph (2026-07-20 regression).
        "manual" => Some(FullGraphEdgeKind::Manual),
        _ => None,
    }
}
