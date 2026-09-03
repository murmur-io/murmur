//! Links storage surface — the semantic/wikilink/companion LINK ENGINE, extracted verbatim from
//! `storage::db` (brain-v3 audit-fix PR-11, a PURE MOVE — no behavior change). The methods below are
//! an inherent-impl split of [`crate::storage::db::Db`] across files (Rust allows one type's inherent
//! `impl` to live in multiple files of the same crate); every method retains its EXACT prior body,
//! signature, and gating. The link-endpoint sealed-at-rest probes and the schema `migrate_links` moved
//! with it. Shared db.rs module-level helpers (`map_err`, `visibility_clause`, `doc_sealed_at_rest_tx`,
//! `backlink_sort_key`) are imported below (promoted to `pub(crate)` in db.rs for the sibling access);
//! the Db-private accessor `lock()` and the private methods `full_graph_content_nodes` /
//! `meeting_by_title_folded_visible` were likewise promoted to `pub(crate)`. Tests for these methods
//! stay in db.rs's `mod tests` (they share that harness's `mem_db`/`file_db`/`sample_meeting` helpers,
//! which construct `Db` from its private `conn` field) and still run against the moved methods
//! unchanged — the count is conserved with no risky field promotion.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension};
use zeroize::Zeroizing;

use crate::error::{AppError, Result};
use crate::share::org_dto::parse_stable_uuid;
use crate::storage::db::{
    backlink_sort_key, doc_sealed_at_rest_tx, extract_wikilink_titles, map_err,
    meeting_visibility_clause, visibility_clause, Db, LinkRowRaw,
};
use crate::storage::models::{BacklinkSource, SourceKind, WikiTarget};

fn org_link_id(org_id: &str, doc_id: &str) -> Option<String> {
    parse_stable_uuid(org_id)?;
    parse_stable_uuid(doc_id)?;
    Some(format!("{org_id}:{doc_id}"))
}

fn parse_org_link_id(id: &str) -> Option<(&str, &str)> {
    let (org_id, doc_id) = id.split_once(':')?;
    parse_stable_uuid(org_id)?;
    parse_stable_uuid(doc_id)?;
    Some((org_id, doc_id))
}

fn parse_marker_identity(value: Option<String>, field: &str) -> Result<Option<u64>> {
    value
        .map(|raw| {
            raw.parse::<u64>().map_err(|_| {
                AppError::Storage(format!("invalid marker-export publish {field} identity"))
            })
        })
        .transpose()
}

/// One durable SQLCipher outbox row authorizing removal of a sealed neighbour title from the
/// exact vault export that Murmur previously wrote. The title/path never leave the encrypted DB
/// except inside the local filesystem reconciler and are never logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LockMarkerExportCleanup {
    pub(crate) source_kind: String,
    pub(crate) source_id: String,
    pub(crate) provider_id: String,
    pub(crate) exported_path: String,
    pub(crate) sealed_title: String,
    pub(crate) expected_hash: Option<String>,
}

/// SQLCipher-backed provenance for the one temporary inode used to publish a marker scrub.
/// Device/inode values are stored as decimal TEXT in SQLite so Darwin's unsigned 64-bit values
/// round-trip without truncation through SQLite's signed INTEGER type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LockMarkerExportPublish {
    pub(crate) exported_path: String,
    pub(crate) stage_name: String,
    pub(crate) source_device: Option<u64>,
    pub(crate) source_inode: Option<u64>,
    pub(crate) source_hash: Option<String>,
    pub(crate) stage_device: Option<u64>,
    pub(crate) stage_inode: Option<u64>,
    pub(crate) state: String,
}

/// Verify-before-destroy result prepared by the command layer for a legacy marker living in a
/// session-unlocked but durably locked note. The plaintext is included only to bind the ciphertext
/// to the exact transaction-computed scrub; this struct never crosses IPC or leaves process memory.
#[derive(Clone)]
pub(crate) struct PreparedManualMarkerSeal {
    pub(crate) note_id: String,
    pub(crate) folder_id: String,
    pub(crate) stripped_text: String,
    pub(crate) text_blob: Vec<u8>,
    pub(crate) content_key: Zeroizing<[u8; 32]>,
}

fn document_seal_aad(folder_id: &str, document_id: &str) -> Vec<u8> {
    format!(
        "murmur:document:v1|folder={folder_id}|document={document_id}|type=document"
    )
    .into_bytes()
}

/// TOCTOU seal re-check for a MEETING endpoint, run INSIDE the caller's write transaction — the
/// meeting-side twin of [`doc_sealed_at_rest_tx`]. A `lock_folder` seal blanks a meeting note's
/// plaintext `markdown` into `content_blob` (`markdown=''`, `content_blob` kept) — the same
/// session-independent, DB-side sealed-at-rest invariant [`upsert_live_bullets`] keys on. Brain v3
/// audit Fix 0: the LINK WRITERS (`index_wikilinks_for_source`, `auto_link_semantic`) resolve their
/// target set OUTSIDE the write tx (against a possibly-stale `unlocked` snapshot), so a `lock_folder`
/// committing its `purge_links_tx` BETWEEN that snapshot and the writer's own commit could re-insert
/// a `links` row naming the now-sealed endpoint. Keying the refusal on this invariant (not the
/// caller's snapshot) stops a link that reveals a sealed neighbour's existence/title from landing at
/// rest behind the lock. Returns `true` ⇒ the endpoint is sealed-at-rest and the caller must NOT
/// write an edge touching it. UNSEAL/session-unlock un-blanks `markdown` before re-deriving, so this
/// reads `false` there and the re-derive proceeds — the same contract the chunk indexers carry.
pub(crate) fn meeting_sealed_at_rest_tx(
    tx: &rusqlite::Transaction<'_>,
    meeting_id: &str,
) -> Result<bool> {
    tx.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM meetings m
            LEFT JOIN folders f ON f.id=m.folder_id
            WHERE m.id=?1 AND (
              EXISTS(SELECT 1 FROM notes n
                      WHERE n.meeting_id=m.id AND n.content_blob IS NOT NULL
                        AND (n.markdown IS NULL OR n.markdown=''))
              OR (f.locked=1 AND NOT EXISTS(SELECT 1 FROM notes n WHERE n.meeting_id=m.id))
            )
         )",
        rusqlite::params![meeting_id],
        |r| Ok(r.get::<_, i64>(0)? != 0),
    )
    .map_err(map_err)
}

/// Is a link ENDPOINT `(kind, id)` sealed-at-rest RIGHT NOW, inside the caller's write tx? A
/// `meeting` endpoint reads [`meeting_sealed_at_rest_tx`]; a `note`/`document` endpoint (both live in
/// the `documents` id space) reads [`doc_sealed_at_rest_tx`]. Brain v3 audit Fix 0 — the one probe
/// the link writers key their in-tx edge refusal on, so a link naming a sealed neighbour never lands
/// at rest.
fn link_endpoint_sealed_at_rest_tx(
    tx: &rusqlite::Transaction<'_>,
    kind: crate::links::LinkKind,
    id: &str,
) -> Result<bool> {
    match kind {
        crate::links::LinkKind::Meeting => meeting_sealed_at_rest_tx(tx, id),
        // A `note` id IS a `documents` id, so both non-meeting kinds probe the documents row.
        crate::links::LinkKind::Note | crate::links::LinkKind::Document => {
            doc_sealed_at_rest_tx(tx, id)
        }
        // Org items have no folder seal domain. Availability is a joined+enabled+live local replica
        // check, repeated inside the edge write transaction to close context/leave races.
        crate::links::LinkKind::Org => {
            let Some((org_id, doc_id)) = parse_org_link_id(id) else {
                return Ok(true);
            };
            let visible = tx
                .query_row(
                    "SELECT 1
                      FROM org_items oi
                      JOIN org_state os ON os.org_id = oi.org_id
                     WHERE oi.org_id = ?1 AND oi.doc_id = ?2
                        AND oi.tombstoned = 0 AND oi.is_current = 1
                        AND COALESCE(oi.source_kind, '') NOT IN ('task','container')
                        AND os.context_enabled = 1
                      LIMIT 1",
                    rusqlite::params![org_id, doc_id],
                    |_| Ok(()),
                )
                .optional()
                .map_err(map_err)?
                .is_some();
            Ok(!visible)
        }
    }
}

/// One `links` row, verbatim, for a trash snapshot.
///
/// Deliberately its own type rather than a reuse of a query DTO: this is an AT-REST format that
/// ships inside a user's trash entry, so it must stay stable independently of anything the UI or
/// IPC layer decides to rename. Snake_case for the same reason the snapshot payloads are.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LinkRowSnapshot {
    pub src_kind: String,
    pub src_id: String,
    pub dst_kind: String,
    pub dst_id: String,
    pub edge_type: String,
    pub score: f64,
    pub created_by: String,
    pub status: String,
    pub created_at: i64,
}

impl Db {
    /// Brain v3 PR-3 — idempotent LINK-ENGINE schema: the `links` table records DERIVED, content-
    /// revealing relations between `meeting|note|document` rows. Three edge kinds:
    ///   • `wikilink`  — a `[[Title]]` in a source body, RESOLVED to the target's id at write time
    ///                   (rename-proof — the root-cause fix over title-string scanning). DIRECTED,
    ///                   `status='active'`, `created_by='user'`.
    ///   • `companion` — the structured `documents.meeting_id` link (recording-time companion note →
    ///                   its meeting). DIRECTED, `active`, `user`.
    ///   • `semantic`  — a vec0-kNN content-similarity SUGGESTION. UNDIRECTED (endpoints
    ///                   canonicalized `src<dst`), `status='suggested'`, `created_by='auto'`,
    ///                   `score`=cosine. Accept flips it `active`/`accepted`; dismiss tombstones it.
    /// `UNIQUE(src_kind,src_id,dst_kind,dst_id,edge_type)` makes upsert idempotent; both-direction
    /// indexes serve the gated `links_for_visible` reader from either endpoint.
    ///
    /// Lock model (load-bearing): a link ROW names a neighbour (its existence + title reveal a
    /// possibly-sealed item), so it is PURGED on seal (`purge_links_tx`, run inside every seal/delete
    /// tx) and RE-DERIVED on unlock; every read gates BOTH endpoints. The table itself is additive +
    /// guarded — no DROP, no destructive rewrite of user rows.
    pub(crate) fn migrate_links(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS links (
               id INTEGER PRIMARY KEY,
               src_kind TEXT NOT NULL,
               src_id TEXT NOT NULL,
               dst_kind TEXT NOT NULL,
               dst_id TEXT NOT NULL,
               edge_type TEXT NOT NULL,
               score REAL NOT NULL DEFAULT 1.0,
               created_by TEXT NOT NULL DEFAULT 'user',
               status TEXT NOT NULL DEFAULT 'active',
               created_at INTEGER NOT NULL,
               UNIQUE (src_kind, src_id, dst_kind, dst_id, edge_type)
             );
             CREATE INDEX IF NOT EXISTS idx_links_src ON links(src_kind, src_id);
             CREATE INDEX IF NOT EXISTS idx_links_dst ON links(dst_kind, dst_id);
             CREATE TABLE IF NOT EXISTS lock_marker_export_cleanup (
               source_kind TEXT NOT NULL CHECK(source_kind IN ('meeting','note')),
               source_id TEXT NOT NULL,
               provider_id TEXT NOT NULL DEFAULT '',
               exported_path TEXT NOT NULL,
               sealed_title TEXT NOT NULL,
               expected_hash TEXT,
               PRIMARY KEY (source_kind, source_id, provider_id, exported_path, sealed_title)
             );
             CREATE TABLE IF NOT EXISTS lock_marker_export_publish (
               exported_path TEXT PRIMARY KEY,
               stage_name TEXT NOT NULL UNIQUE,
               source_device TEXT,
               source_inode TEXT,
               source_hash TEXT,
               stage_device TEXT,
               stage_inode TEXT,
               state TEXT NOT NULL CHECK(state IN ('reserved','created','prepared','swapped'))
             );",
        )
        .map_err(map_err)?;

        // ONE-TIME idempotent COMPANION backfill: every existing companion note
        // (`documents.kind='note'` with a non-null `meeting_id`) gets its directed
        // note → meeting `companion` edge. Sentinel-guarded so it runs EXACTLY once (later maintenance
        // is at the write site, `set_companion_link`); a fresh DB has no companion rows so the
        // INSERT is a harmless no-op either way. `INSERT OR IGNORE` respects the UNIQUE constraint so
        // even a re-run (were the sentinel ever lost) never duplicates. Uses the held `conn` directly
        // (self.set_setting would re-lock and deadlock). created_at = the note's own created_at (a
        // non-content epoch already on the row) so the backfilled edge carries a real timestamp.
        let backfilled: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'links_companion_backfill_v1'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_err)?;
        if backfilled.is_none() {
            conn.execute(
                "INSERT OR IGNORE INTO links
                   (src_kind, src_id, dst_kind, dst_id, edge_type, score, created_by, status, created_at)
                 SELECT 'note', d.id, 'meeting', d.meeting_id, 'companion', 1.0, 'user', 'active', d.created_at
                   FROM documents d
                  WHERE d.kind = 'note' AND d.meeting_id IS NOT NULL",
                [],
            )
            .map_err(map_err)?;
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('links_companion_backfill_v1', '1')",
                [],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    /// Resolve a `[[Title]]` wikilink to a VISIBLE navigation target — a standalone note (preferred,
    /// the more natural link target), else a meeting whose exact title matches, else (2026-07-15)
    /// an org (Shared Brain) item whose exact title matches. GATED on all three legs: notes/meetings
    /// via `visibility_clause` (a sealed-and-not-session-unlocked note/meeting with that title
    /// resolves to `None`, so clicking a wikilink can never reveal or navigate to locked content);
    /// the org leg via the SAME membership+per-instance-enabled gate `get_org_item`/
    /// `search_org_chunks_knn`/`_fts` already use (`JOIN org_state ... WHERE context_enabled = 1`),
    /// excluding tombstoned rows — never a laxer gate than the existing Shared Brain read path
    /// (`crate::tools::org_brain_available`/`search_org_brain_hits`). `None` when nothing matches.
    /// Title match uses the note's display title (`title`, falling back to `name`), matching how it
    /// is shown/exported. This closes the sibling-gap left by the newer prefix-search picker
    /// (`list_link_candidates_visible` + the org leg folded in at `commands::list_link_candidates`):
    /// that picker already offered org items as autocomplete candidates, but this EXACT-title
    /// resolver — used by both click-to-open and the "does this already exist" pre-check — did not.
    pub fn resolve_wikilink(
        &self,
        title: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<WikiTarget>> {
        let title = title.trim();
        if title.is_empty() {
            return Ok(None);
        }
        // CASE-INSENSITIVE resolution (brain-v3 audit Fix 6): Obsidian resolves `[[links]]`
        // case-insensitively, but this resolver matched titles BYTE-EXACTLY, so `[[project x]]`
        // silently failed to resolve to a note titled "Project X". We PREFER an exact match (the
        // `= ?1` predicate below), then FALL BACK to a case-folded match (`LOWER(...) = LOWER(?1)`)
        // when the exact one misses. Note on scope: SQLite's built-in `LOWER()` folds ASCII only —
        // full Unicode case/diacritic folding (`.nfc().to_lowercase()`) would need the
        // `unicode-normalization` crate (a new dep, deferred), so accented titles still fold only as
        // far as ASCII here; the common Obsidian case (letter-case differences) is covered. The
        // fallback runs ONLY on an exact miss (no cost on the hot exact path).
        let folded_note_sql = |exact: bool, visible: &str| {
            // SELF-LINK AVOIDANCE (2026-07-16 companion note): a companion note's managed title
            // equals its meeting's title, so a user-typed `[[Meeting]]` could otherwise hit the
            // companion note via this note-leg-first order. EXCLUDE a note carrying a non-null
            // `meeting_id` WHEN the queried title equals THAT note's own meeting's title — so
            // `[[Meeting]]` always falls through to the meeting leg below, never resolving to its
            // own companion note. (A companion note IS still a valid target for OTHER titles that
            // happen to name it — only the self-title collision is excluded.)
            let (title_pred, self_pred) = if exact {
                (
                    "COALESCE(NULLIF(TRIM(d.title), ''), d.name) = ?1",
                    "EXISTS (SELECT 1 FROM meetings m WHERE m.id = d.meeting_id AND m.title = ?1)",
                )
            } else {
                (
                    "LOWER(COALESCE(NULLIF(TRIM(d.title), ''), d.name)) = LOWER(?1)",
                    "EXISTS (SELECT 1 FROM meetings m WHERE m.id = d.meeting_id AND LOWER(m.title) = LOWER(?1))",
                )
            };
            format!(
                "SELECT d.id
                   FROM documents d
                   JOIN folders f ON f.id = d.folder_id
                  WHERE d.kind = 'note'
                    AND {title_pred}
                    AND {visible}
                    AND NOT (d.meeting_id IS NOT NULL AND {self_pred})
                  ORDER BY d.updated_at DESC, d.id ASC
                  LIMIT 1"
            )
        };
        // Note leg first (a standalone note is the more natural wikilink target). VISIBLE notes only.
        {
            let conn = self.lock();
            let visible = visibility_clause("f", unlocked);
            // Exact first, then the case-folded fallback (only if exact missed).
            let note_id: Option<String> = conn
                .query_row(
                    &folded_note_sql(true, &visible),
                    rusqlite::params![title],
                    |r| r.get(0),
                )
                .optional()
                .map_err(map_err)?
                .or_else(|| {
                    conn.query_row(
                        &folded_note_sql(false, &visible),
                        rusqlite::params![title],
                        |r| r.get(0),
                    )
                    .optional()
                    .ok()
                    .flatten()
                });
            if let Some(id) = note_id {
                return Ok(Some(WikiTarget {
                    kind: "note".to_string(),
                    id,
                    stable_id: None,
                }));
            }
        }
        // Meeting leg — exact via the proven gated title resolver; then a case-folded fallback that
        // rides the SAME visibility predicate (a sealed meeting never resolves, exact or folded).
        if let Some(m) = self.meeting_by_title_visible(title, unlocked)? {
            return Ok(Some(WikiTarget {
                kind: "meeting".to_string(),
                id: m.id,
                stable_id: None,
            }));
        }
        if let Some(m) = self.meeting_by_title_folded_visible(title, unlocked)? {
            return Ok(Some(WikiTarget {
                kind: "meeting".to_string(),
                id: m.id,
                stable_id: None,
            }));
        }
        // Document leg — AFTER meetings, BEFORE org. A note that links a document materializes
        // `[[Doc Title]]` into its body (`link_items`), and the inline `[[` autocomplete now
        // surfaces documents, so a clicked doc-titled wikilink must resolve. Note-first ordering
        // above means a title shared by a note AND a doc still prefers the note. VISIBLE documents
        // only (`visibility_clause` on the folder) — a sealed-not-unlocked document resolves to
        // None, exactly like the note/meeting legs. Exact first, then the ASCII-case-folded
        // fallback on an exact miss — the same two-tier pattern the note leg uses.
        {
            let conn = self.lock();
            let visible = visibility_clause("f", unlocked);
            let doc_sql = |exact: bool| {
                let title_pred = if exact {
                    "COALESCE(NULLIF(TRIM(d.title), ''), d.name) = ?1"
                } else {
                    "LOWER(COALESCE(NULLIF(TRIM(d.title), ''), d.name)) = LOWER(?1)"
                };
                format!(
                    "SELECT d.id
                       FROM documents d
                       JOIN folders f ON f.id = d.folder_id
                      WHERE d.kind = 'document'
                        AND {title_pred}
                        AND {visible}
                      ORDER BY d.updated_at DESC, d.id ASC
                      LIMIT 1"
                )
            };
            let doc_id: Option<String> = conn
                .query_row(&doc_sql(true), rusqlite::params![title], |r| r.get(0))
                .optional()
                .map_err(map_err)?
                .or_else(|| {
                    conn.query_row(&doc_sql(false), rusqlite::params![title], |r| r.get(0))
                        .optional()
                        .ok()
                        .flatten()
                });
            if let Some(id) = doc_id {
                return Ok(Some(WikiTarget {
                    kind: "document".to_string(),
                    id,
                    stable_id: None,
                }));
            }
        }
        // Org (Shared Brain) leg — deliberately-disclosed content living OUTSIDE the folder-lock
        // domain, so no `unlocked`/`visibility_clause` gate applies here; instead it is scoped to
        // orgs the caller has actually JOINED and left ENABLED on this install, and excludes
        // tombstoned items — the IDENTICAL gate `get_org_item` applies for the read-only viewer.
        // Deliberately NOT the broader/unscoped `documents`/`meetings` query: this leg only ever
        // touches `org_items`.
        {
            let conn = self.lock();
            let org_target: Option<(String, String, Option<String>)> = conn
                .query_row(
                    "SELECT oi.item_id, oi.org_id, oi.doc_id
                       FROM org_items oi
                       JOIN org_state os ON os.org_id = oi.org_id
                      WHERE oi.tombstoned = 0
                        AND COALESCE(oi.source_kind, '') NOT IN ('task','container')
                        AND os.context_enabled = 1
                        AND ((oi.doc_id IS NOT NULL AND oi.is_current = 1)
                             OR oi.doc_id IS NULL)
                        AND oi.title = ?1
                      ORDER BY CASE WHEN oi.doc_id IS NOT NULL THEN 0 ELSE 1 END,
                               oi.rev DESC, oi.seq DESC, oi.item_id ASC
                      LIMIT 1",
                    rusqlite::params![title],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(map_err)?;
            if let Some((item_id, org_id, doc_id)) = org_target {
                let stable_id = match doc_id {
                    Some(doc_id) => {
                        let Some(stable_id) = org_link_id(&org_id, &doc_id) else {
                            return Ok(None);
                        };
                        Some(stable_id)
                    }
                    None => None,
                };
                return Ok(Some(WikiTarget {
                    kind: "org".to_string(),
                    id: item_id,
                    stable_id,
                }));
            }
        }
        Ok(None)
    }

    /// INDEXED backlink fast path (brain-v3 audit Fix 3): every `wikilink`/`companion` edge in the
    /// `links` table that points AT `(target_kind, target_id)` (backed by `idx_links_dst`), resolved
    /// to a [`BacklinkSource`] with the SAME both-endpoint gating as the body-scan path. Returns the
    /// resolved sources PLUS the sets of source ids served (meeting ids, note/document ids) so the
    /// caller's body scan skips them — no double-count, no rescan of an indexed body. A `document`
    /// source (never a backlink body) is ignored. A source failing its own visibility gate is dropped.
    ///
    /// A `links` backlink edge is stored `src → dst` (the SOURCE mentions the TARGET), so we match on
    /// `dst`. The map back to [`SourceKind`]: a `meeting`/`note` src is a real backlink source; a
    /// `document` src is skipped (documents are not editable note bodies here). Timestamp: the
    /// meeting's `started_at`, a note's `updated_at`/`created_at` rendered RFC3339 — uniform with the
    /// body-scan legs so `backlink_sort_key` parses one format.
    fn backlinks_from_links_index(
        &self,
        target_kind: SourceKind,
        target_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<(Vec<BacklinkSource>, HashSet<String>, HashSet<String>)> {
        let target_kind_s = match target_kind {
            SourceKind::Meeting => "meeting",
            SourceKind::Note => "note",
        };
        // Read the raw incident (src → target) edges first (ids/kinds only — no content, no title).
        let raw: Vec<(String, String)> = {
            let conn = self.lock();
            let mut stmt = conn
                .prepare(
                    "SELECT DISTINCT src_kind, src_id FROM links
                       WHERE dst_kind = ?1 AND dst_id = ?2
                         AND edge_type IN ('wikilink', 'companion')
                         AND status != 'dismissed'",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![target_kind_s, target_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(map_err)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(map_err)?);
            }
            out
        };
        let mut sources: Vec<BacklinkSource> = Vec::new();
        let mut meeting_ids: HashSet<String> = HashSet::new();
        let mut note_ids: HashSet<String> = HashSet::new();
        for (src_kind, src_id) in raw {
            // Never list the target as its own backlink (a self-referential edge).
            if src_kind == target_kind_s && src_id == target_id {
                continue;
            }
            match src_kind.as_str() {
                "meeting" => {
                    // SOURCE GATE: the meeting must be visible; its title/started_at via a gated read.
                    let Some((title, started_at)) =
                        self.backlink_meeting_meta_visible(&src_id, unlocked)?
                    else {
                        continue;
                    };
                    if meeting_ids.insert(src_id.clone()) {
                        sources.push(BacklinkSource {
                            id: src_id,
                            kind: SourceKind::Meeting,
                            title,
                            timestamp: started_at,
                        });
                    }
                }
                "note" => {
                    // SOURCE GATE: the note's folder must be visible; title/updated_at via a gated read.
                    let Some((title, ts)) = self.backlink_note_meta_visible(&src_id, unlocked)?
                    else {
                        continue;
                    };
                    if note_ids.insert(src_id.clone()) {
                        sources.push(BacklinkSource {
                            id: src_id,
                            kind: SourceKind::Note,
                            title,
                            timestamp: chrono::DateTime::from_timestamp_millis(ts)
                                .map(|dt| dt.to_rfc3339())
                                .unwrap_or_default(),
                        });
                    }
                }
                // A `document` (non-note) src has no wikilink body backlink semantics here — skip.
                _ => continue,
            }
        }
        Ok((sources, meeting_ids, note_ids))
    }

    /// A meeting backlink SOURCE's `(title, started_at)` — ONLY when the meeting is session-VISIBLE
    /// (`meeting_is_visible`), else `None` (source gate). `started_at` is already RFC3339.
    fn backlink_meeting_meta_visible(
        &self,
        meeting_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<(String, String)>> {
        if !self.meeting_is_visible(meeting_id, unlocked)? {
            return Ok(None);
        }
        let conn = self.lock();
        conn.query_row(
            "SELECT COALESCE(NULLIF(TRIM(title), ''), ''), started_at FROM meetings WHERE id = ?1",
            rusqlite::params![meeting_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(map_err)
    }

    /// A note backlink SOURCE's `(title, updated_at_epoch_ms)` — ONLY when the note's folder is
    /// session-VISIBLE (`visibility_clause`), else `None` (source gate). `kind='note'` only.
    fn backlink_note_meta_visible(
        &self,
        note_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<(String, i64)>> {
        let conn = self.lock();
        let visible = visibility_clause("f", unlocked);
        let sql = format!(
            "SELECT COALESCE(NULLIF(TRIM(d.title), ''), d.name),
                    COALESCE(d.updated_at, d.created_at)
               FROM documents d
               JOIN folders f ON f.id = d.folder_id
              WHERE d.id = ?1 AND d.kind = 'note' AND {visible}
              LIMIT 1"
        );
        conn.query_row(&sql, rusqlite::params![note_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })
        .optional()
        .map_err(map_err)
    }

    /// "What links here" — every VISIBLE meeting note (`notes.markdown`) or standalone note
    /// (`documents.text` where `kind='note'`) whose body carries a `[[<target's exact title>]]`
    /// wikilink pointing AT the row identified by (`target_kind`, `target_id`). ON-DEMAND scan (no
    /// index, no migration, no persisted table). Newest-first (meeting `started_at` / note
    /// `updated_at`).
    ///
    /// FAIL-CLOSED, two gates:
    /// 1. **TARGET GATE (first).** The target's own exact title is resolved through the SAME
    ///    visibility predicate as [`Self::meeting_by_title_visible`] / [`Self::list_notes_visible`].
    ///    An unknown target, or one whose folder is sealed-and-not-session-unlocked, returns
    ///    `Ok(vec![])` BEFORE any source is scanned — so a locked target never even reveals that it
    ///    HAS backlinks (no existence leak).
    /// 2. **SOURCE GATE.** The candidate bodies come EXACTLY from the gated readers
    ///    (`visibility_clause` on the meeting-note leg and the note leg), so a sealed-and-not-unlocked
    ///    source can never contribute — its body is simply not in the scan set.
    ///
    /// INDEXED FAST PATH (brain-v3 audit Fix 3): the wikilink + companion legs of a backlink are
    /// already materialized in the `links` table (`edge_type IN ('wikilink','companion')`, keyed by
    /// RESOLVED target id + backed by `idx_links_dst`), so they are served from that index FIRST —
    /// without touching any source body. The O(entire-vault-text) regex body scan then runs ONLY over
    /// sources NOT already served from the index (legacy note/meeting bodies whose wikilinks predate
    /// the `links` write-time indexer, or a not-yet-re-indexed source) — a strict fallback, so no real
    /// backlink is lost while the hottest new read path stops re-scanning every note body on every open.
    /// BOTH gates are preserved: the fast-path source is dropped unless it resolves through the SAME
    /// visibility gate (source gate), and Gate 1 on the target is unchanged.
    ///
    /// Title collisions keep ALL same-titled matches (a dropped real backlink would be a silent false
    /// negative). Logs IDs/counts only — never body text or titles.
    pub fn backlinks_for_visible(
        &self,
        target_kind: SourceKind,
        target_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<BacklinkSource>> {
        // ── GATE 1: resolve the target's exact title THROUGH the visibility gate; empty on any miss. ──
        let target_title = match self.resolve_visible_title(target_kind, target_id, unlocked)? {
            Some(t) if !t.is_empty() => t,
            // Unknown, sealed-not-unlocked, or empty-titled → nothing to link to. Fail closed.
            _ => return Ok(Vec::new()),
        };

        // ── TITLE-STRING FAN-OUT GUARD (backlink-id fix): a source's `[[target_title]]` is a backlink
        // to THIS target ONLY IF `[[target_title]]` actually RESOLVES to it — exactly where clicking
        // the wikilink navigates. `resolve_wikilink` (the SAME gated resolver the write-time index and
        // click-navigation use: note-leg-first, most-recent `ORDER BY updated_at DESC`, case-folded
        // fallback, companion self-link avoidance) collapses a `[[Title]]` to EXACTLY ONE target.
        // When two items share a title (e.g. the default "Untitled"), the resolver picks ONE — so only
        // THAT one may claim the body-scan backlinks; the others must not fan-out and steal them.
        // Compute the mapping once: our (target_kind, target_id) as a `WikiTarget` kind string, and
        // check the resolved target equals us. An `org` resolution never equals a note/meeting target,
        // so `title_targets_us` is false there too. The two title-string body-scan legs below run ONLY
        // when this is true; the id-based INDEX fast path and the structural companion leg are
        // unconditional (they carry the correct, resolved backlinks regardless).
        let target_kind_wiki = match target_kind {
            SourceKind::Meeting => "meeting",
            SourceKind::Note => "note",
        };
        let title_targets_us = matches!(
            self.resolve_wikilink(&target_title, unlocked)?,
            Some(ref wt) if wt.kind == target_kind_wiki && wt.id == target_id
        );

        // ── INDEXED FAST PATH: wikilink + companion backlinks straight from `links` (Fix 3). ──
        // Each returned source id is remembered so the body-scan legs below skip it (no double-count,
        // no rescan). A source that fails its own visibility gate is dropped here (source gate).
        let (mut out, indexed_meeting_ids, indexed_note_ids) =
            self.backlinks_from_links_index(target_kind, target_id, unlocked)?;

        let conn = self.lock();
        // ── GATE 2, meeting-note leg: VISIBLE meeting notes only (same predicate as list_meetings_visible). ──
        // Newest note per meeting; body = `notes.markdown`; timestamp = `meetings.started_at`.
        let visible_meetings = meeting_visibility_clause("m", unlocked);
        let meeting_sql = format!(
            "SELECT m.id, m.title, m.started_at, n.markdown
               FROM meetings m
               JOIN notes n ON n.meeting_id = m.id
              WHERE {visible_meetings}
              ORDER BY m.started_at DESC, m.id DESC"
        );
        let mut stmt = conn.prepare(&meeting_sql).map_err(map_err)?;
        let meeting_rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(map_err)?;

        // A meeting can have several visible provider notes; scan EVERY visible note and include the
        // meeting once as soon as ANY of its notes links the target — mark `seen` only on a MATCH, so
        // a link that lives only in a non-first provider note is never missed. title/started_at are
        // meeting-level (identical across the meeting's notes), so the matching row is representative.
        // A meeting already served from the `links` index (Fix 3) is pre-seeded into `seen_meetings`
        // so the scan neither re-scans its body nor emits a duplicate row.
        let mut seen_meetings: HashSet<String> = indexed_meeting_ids;
        for row in meeting_rows {
            let (id, title, started_at, body) = row.map_err(map_err)?;
            if target_kind == SourceKind::Meeting && id == target_id {
                continue; // never list the target itself.
            }
            if seen_meetings.contains(&id) {
                continue; // already emitted this meeting (fast path or an earlier note).
            }
            // FAN-OUT GUARD: only attribute a title-string `[[target_title]]` when the title actually
            // resolves to US — otherwise it links a DIFFERENT same-titled item (or nothing), not us.
            if title_targets_us && extract_wikilink_titles(&body).contains(&target_title) {
                seen_meetings.insert(id.clone());
                out.push(BacklinkSource {
                    id,
                    kind: SourceKind::Meeting,
                    title,
                    timestamp: started_at,
                });
            }
        }
        drop(stmt);

        // ── GATE 2, note leg: VISIBLE standalone notes only (same predicate as list_notes_visible). ──
        // Body = `documents.text`; title = `documents.title` (fallback `name`); timestamp = `updated_at`.
        let visible_docs = visibility_clause("f", unlocked);
        let doc_sql = format!(
            "SELECT d.id, d.title, d.name, COALESCE(d.text, ''),
                    COALESCE(d.updated_at, d.created_at)
               FROM documents d
               JOIN folders f ON f.id = d.folder_id
              WHERE d.kind = 'note' AND {visible_docs}
              ORDER BY COALESCE(d.updated_at, d.created_at) DESC, d.id ASC"
        );
        let mut doc_stmt = conn.prepare(&doc_sql).map_err(map_err)?;
        let doc_rows = doc_stmt
            .query_map([], |r| {
                let id: String = r.get(0)?;
                let title: Option<String> = r.get(1)?;
                let name: String = r.get(2)?;
                let text: String = r.get(3)?;
                let ts: i64 = r.get(4)?;
                Ok((id, title, name, text, ts))
            })
            .map_err(map_err)?;
        let mut note_hits: Vec<BacklinkSource> = Vec::new();
        for row in doc_rows {
            let (id, title, name, text, ts) = row.map_err(map_err)?;
            if target_kind == SourceKind::Note && id == target_id {
                continue; // never list the target itself.
            }
            if indexed_note_ids.contains(&id) {
                continue; // already served from the `links` index (Fix 3) — never rescan/duplicate.
            }
            // FAN-OUT GUARD (see the meeting leg): a title-string `[[target_title]]` counts as a
            // backlink to US only when the title resolves to us; else it links a same-titled sibling.
            if title_targets_us && extract_wikilink_titles(&text).contains(&target_title) {
                note_hits.push(BacklinkSource {
                    id,
                    kind: SourceKind::Note,
                    title: title.filter(|t| !t.is_empty()).unwrap_or(name),
                    // Emit RFC3339 (like the meeting leg) so the wire `timestamp` is uniformly
                    // ISO-8601 — the FE `new Date(iso)` renders it directly; a bare epoch-millis
                    // string would parse to `Invalid Date` and show a blank chip date.
                    timestamp: chrono::DateTime::from_timestamp_millis(ts)
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_default(),
                });
            }
        }
        drop(doc_stmt);

        // ── GATE 2, STRUCTURED companion-note leg (2026-07-16): for a MEETING target, a companion
        // note is linked by the authoritative `documents.meeting_id` id — NOT by a fragile
        // front-matter `[[Title]]` string that the two title-scan legs above depend on. Surface it
        // structurally so the meeting's "Linked mentions" always includes its companion note even
        // if the title drifted mid-rename or never round-tripped as a wikilink. Lock-gated exactly
        // like the string leg (`visibility_clause` on the note's folder), and DEDUPED against
        // `note_hits` so a companion note that ALSO matched the string scan is listed once. ──
        if target_kind == SourceKind::Meeting {
            // Dedup against BOTH the body-scan `note_hits` AND the `links`-index note sources already
            // in `out` (a companion note is often ALSO served from the fast path via its companion
            // edge) so a companion note is listed exactly once.
            let mut already: HashSet<String> = note_hits.iter().map(|b| b.id.clone()).collect();
            already.extend(indexed_note_ids.iter().cloned());
            let visible_docs = visibility_clause("f", unlocked);
            let comp_sql = format!(
                "SELECT d.id, d.title, d.name, COALESCE(d.updated_at, d.created_at)
                   FROM documents d
                   JOIN folders f ON f.id = d.folder_id
                  WHERE d.kind = 'note' AND d.meeting_id = ?1 AND {visible_docs}
                  ORDER BY COALESCE(d.updated_at, d.created_at) DESC, d.id ASC"
            );
            let mut comp_stmt = conn.prepare(&comp_sql).map_err(map_err)?;
            let comp_rows = comp_stmt
                .query_map(rusqlite::params![target_id], |r| {
                    let id: String = r.get(0)?;
                    let title: Option<String> = r.get(1)?;
                    let name: String = r.get(2)?;
                    let ts: i64 = r.get(3)?;
                    Ok((id, title, name, ts))
                })
                .map_err(map_err)?;
            for row in comp_rows {
                let (id, title, name, ts) = row.map_err(map_err)?;
                if already.contains(&id) {
                    continue; // already counted by the string-scan leg — never duplicate.
                }
                note_hits.push(BacklinkSource {
                    id,
                    kind: SourceKind::Note,
                    title: title.filter(|t| !t.is_empty()).unwrap_or(name),
                    timestamp: chrono::DateTime::from_timestamp_millis(ts)
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_default(),
                });
            }
            drop(comp_stmt);
        }
        drop(conn);

        // Merge the two legs newest-first. Both legs now emit an RFC3339 `timestamp`, so
        // `backlink_sort_key` parses a single format to a comparable epoch-millis key.
        out.extend(note_hits);
        out.sort_by_key(|b| std::cmp::Reverse(backlink_sort_key(b)));

        tracing::debug!(
            target: "backlinks",
            count = out.len(),
            "backlinks_for_visible resolved"
        );
        Ok(out)
    }

    /// Resolve the exact, current title of a backlink TARGET through the SAME visibility gate the
    /// list readers use. `None` iff the target is unknown OR sealed-and-not-session-unlocked (the two
    /// are indistinguishable to the caller — no existence leak). Meeting → gated `meetings.title`;
    /// note → gated `documents.title`/`name` (`kind='note'`).
    fn resolve_visible_title(
        &self,
        kind: SourceKind,
        id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<String>> {
        match kind {
            SourceKind::Meeting => {
                // Visible iff the meeting has no notes OR at least one visible note (mirrors
                // `meeting_by_title_visible` / `meeting_is_visible`).
                if !self.meeting_is_visible(id, unlocked)? {
                    return Ok(None);
                }
                let conn = self.lock();
                conn.query_row(
                    "SELECT COALESCE(title, '') FROM meetings WHERE id = ?1",
                    rusqlite::params![id],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(map_err)
            }
            SourceKind::Note => {
                let conn = self.lock();
                let visible = visibility_clause("f", unlocked);
                let sql = format!(
                    "SELECT d.title, d.name
                       FROM documents d
                       JOIN folders f ON f.id = d.folder_id
                      WHERE d.id = ?1 AND d.kind = 'note' AND {visible}
                      LIMIT 1"
                );
                conn.query_row(&sql, rusqlite::params![id], |r| {
                    let title: Option<String> = r.get(0)?;
                    let name: String = r.get(1)?;
                    Ok(title.filter(|t| !t.is_empty()).unwrap_or(name))
                })
                .optional()
                .map_err(map_err)
            }
        }
    }

    // ── Brain v3 PR-3 — LINK ENGINE ───────────────────────────────────────────────────────────
    //
    // Persisted `links` rows (wikilink/companion/semantic) between meetings/notes/documents. A row
    // is a DERIVED, content-revealing relation, so: PURGED on seal (`purge_links_tx`), RE-DERIVED on
    // unlock, and read with BOTH endpoints visibility-gated (`links_for_visible`). SQL only — the
    // pure edge math (canonicalization, kNN selection) lives in `crate::links`.

    /// Every link row touching `meeting_id` on EITHER side, for a trash snapshot.
    ///
    /// Deleting a meeting purges its links outright (`purge_links_tx` with `preserve_decisions=false`,
    /// because the endpoint is gone), so unlike the cascade-driven tables there is nothing left to
    /// find afterwards. The snapshot is the only copy, and it has to carry both directions: a link
    /// naming this meeting as its DESTINATION is somebody else's edge into it, and losing that is
    /// what makes a restored meeting look connected to nothing.
    pub fn link_rows_for_meeting(&self, meeting_id: &str) -> Result<Vec<LinkRowSnapshot>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT src_kind, src_id, dst_kind, dst_id, edge_type, score, created_by, status,
                        created_at
                   FROM links
                  WHERE (src_kind = 'meeting' AND src_id = ?1)
                     OR (dst_kind = 'meeting' AND dst_id = ?1)
                  ORDER BY src_kind, src_id, dst_kind, dst_id, edge_type",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![meeting_id], |r| {
                Ok(LinkRowSnapshot {
                    src_kind: r.get(0)?,
                    src_id: r.get(1)?,
                    dst_kind: r.get(2)?,
                    dst_id: r.get(3)?,
                    edge_type: r.get(4)?,
                    score: r.get(5)?,
                    created_by: r.get(6)?,
                    status: r.get(7)?,
                    created_at: r.get(8)?,
                })
            })
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// Re-insert snapshotted link rows verbatim.
    ///
    /// `INSERT OR IGNORE` rather than the usual upsert: a restore must not overwrite a decision the
    /// user has made since the delete. If an identical edge was re-derived and then dismissed while
    /// the meeting sat in the trash, the dismissal wins — resurrecting it would undo a choice the
    /// user made after the fact.
    pub fn restore_link_rows(&self, rows: &[LinkRowSnapshot]) -> Result<usize> {
        if rows.is_empty() {
            return Ok(0);
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let mut restored = 0usize;
        for row in rows {
            restored += tx
                .execute(
                    "INSERT OR IGNORE INTO links
                       (src_kind, src_id, dst_kind, dst_id, edge_type, score, created_by, status,
                        created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    rusqlite::params![
                        row.src_kind,
                        row.src_id,
                        row.dst_kind,
                        row.dst_id,
                        row.edge_type,
                        row.score,
                        row.created_by,
                        row.status,
                        row.created_at
                    ],
                )
                .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)?;
        Ok(restored)
    }

    /// Upsert ONE edge (idempotent on the UNIQUE key). For an UNDIRECTED (semantic) edge the caller
    /// MUST have canonicalized endpoints (`src<dst`) so A~B and B~A collapse to one row. On a
    /// conflict we REFRESH the mutable fields (`score`, `created_by`, `status`, `created_at`) — a
    /// re-run of the semantic pass updates a suggestion's score; a wikilink re-index refreshes its
    /// timestamp. EXCEPTION: a `dismissed` semantic row is a TOMBSTONE — never resurrected by a later
    /// auto pass (the `WHERE ... status != 'dismissed'` guard on the DO UPDATE). Runs inside a tx.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn upsert_link_tx(
        tx: &rusqlite::Transaction<'_>,
        src_kind: &str,
        src_id: &str,
        dst_kind: &str,
        dst_id: &str,
        edge_type: &str,
        score: f64,
        created_by: &str,
        status: &str,
        created_at: i64,
    ) -> Result<()> {
        tx.execute(
            // On conflict we REFRESH the score/created_at, but we must NOT clobber a user's decision:
            //  - `WHERE links.status != 'dismissed'` — a dismissed TOMBSTONE is never resurrected (a
            //    later auto pass can't re-suggest it).
            //  - The `CASE` guards status/created_by against a DOWNGRADE: when the incoming write is a
            //    `suggested` auto re-suggest but the existing edge is already `active` (user-accepted or
            //    a materialized wikilink/companion), KEEP the existing `status`/`created_by`. Only the
            //    score is refreshed. An INCOMING `active` (accept, or a fresh wikilink/companion) still
            //    promotes normally. This preserves an accepted edge across every later semantic pass.
            "INSERT INTO links
               (src_kind, src_id, dst_kind, dst_id, edge_type, score, created_by, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (src_kind, src_id, dst_kind, dst_id, edge_type) DO UPDATE SET
               score = excluded.score,
               created_by = CASE
                 WHEN excluded.status = 'suggested' AND links.status = 'active'
                   THEN links.created_by
                 ELSE excluded.created_by
               END,
               status = CASE
                 WHEN excluded.status = 'suggested' AND links.status = 'active'
                   THEN links.status
                 ELSE excluded.status
               END,
               created_at = excluded.created_at
             WHERE links.status != 'dismissed'",
            rusqlite::params![
                src_kind, src_id, dst_kind, dst_id, edge_type, score, created_by, status, created_at
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// WRITE-TIME WIKILINK INDEXING (DESIGN §PR-3): resolve every `[[Title]]` in `body` to its TARGET
    /// ID (rename-proof) through the gated [`Self::resolve_wikilink`], then DELETE-THEN-INSERT this
    /// source's `edge_type='wikilink'` rows in one tx. Storing resolved IDs — not the title strings —
    /// is the root-cause fix: a later target rename never strands the edge. Unresolved titles (no
    /// visible target, or an org-only target — orgs are outside the folder-lock link domain) are
    /// simply skipped; the next re-index picks them up once a matching local target exists.
    ///
    /// `src_kind`/`src_id` identify the SOURCE (`meeting` for an AI note, `note` for an authored
    /// note). Called from the note-save funnels + `build_and_persist_entities`. Best-effort at the
    /// call site (a failure logs, never fails the save). Logs ids/counts only.
    pub fn index_wikilinks_for_source(
        &self,
        src_kind: crate::links::LinkKind,
        src_id: &str,
        body: &str,
        unlocked: &HashSet<String>,
    ) -> Result<()> {
        // Resolve OUTSIDE the write tx (resolve_wikilink takes its own connection lock).
        let titles = extract_wikilink_titles(body);
        let mut targets: Vec<(crate::links::LinkKind, String)> = Vec::new();
        for t in &titles {
            if let Some(target) = self.resolve_wikilink(t, unlocked)? {
                let kind = match target.kind.as_str() {
                    "meeting" => crate::links::LinkKind::Meeting,
                    "note" => crate::links::LinkKind::Note,
                    // Org relationships are explicit/manual only. A title collision in a local note
                    // must not silently create a private Shared Brain relation on save.
                    "org" => continue,
                    _ => continue,
                };
                // Never self-link (a note whose title resolves to itself).
                if kind == src_kind && target.id == src_id {
                    continue;
                }
                let endpoint_id = target.stable_id.unwrap_or(target.id);
                if !targets.iter().any(|(k, i)| *k == kind && i == &endpoint_id) {
                    targets.push((kind, endpoint_id));
                }
            }
        }
        let now = chrono::Utc::now().timestamp_millis();
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        // Fix 0 (brain-v3 audit) — IN-TX sealed-at-rest SOURCE re-check (TOCTOU): the `targets` above
        // were resolved OUTSIDE this tx against a possibly-stale `unlocked` snapshot. If a
        // `lock_folder` committed its seal (running `purge_links_tx`) in the meantime and this
        // source's own endpoint is now sealed-at-rest, a re-derive would re-insert wikilink rows
        // behind the lock. Refuse silently (rollback via drop) — the source is sealed; its edges are
        // re-derived on unlock. Never a new row naming a sealed source.
        if link_endpoint_sealed_at_rest_tx(&tx, src_kind, src_id)? {
            tracing::debug!(target: "links", src_kind = src_kind.as_str(), "wikilink index refused: source sealed at rest");
            return Ok(());
        }
        // Clean replace: drop this source's OLD wikilink rows first, then insert the fresh set. A
        // removed `[[Title]]` therefore vanishes from the graph on the next save (self-healing).
        tx.execute(
            "DELETE FROM links WHERE src_kind = ?1 AND src_id = ?2 AND edge_type = 'wikilink'",
            rusqlite::params![src_kind.as_str(), src_id],
        )
        .map_err(map_err)?;
        for (dst_kind, dst_id) in &targets {
            // Fix 0 (brain-v3 audit) — IN-TX sealed-at-rest DST re-check (TOCTOU): drop any target
            // whose endpoint sealed at rest since the OUTSIDE-tx resolve above, so a link never names
            // a now-sealed neighbour. The endpoint is re-derived on that folder's unlock.
            if link_endpoint_sealed_at_rest_tx(&tx, *dst_kind, dst_id)? {
                continue;
            }
            Self::upsert_link_tx(
                &tx,
                src_kind.as_str(),
                src_id,
                dst_kind.as_str(),
                dst_id,
                "wikilink",
                1.0,
                "user",
                "active",
                now,
            )?;
        }
        tx.commit().map_err(map_err)?;
        tracing::debug!(
            target: "links",
            src_kind = src_kind.as_str(),
            resolved = targets.len(),
            total = titles.len(),
            "index_wikilinks_for_source"
        );
        Ok(())
    }

    /// COMPANION edge maintenance: (re)assert the directed `note → meeting` `companion` edge when a
    /// companion note's `documents.meeting_id` is set. Idempotent (UNIQUE upsert). Called wherever
    /// `set_document_meeting_id` runs, so the edge is maintained beyond the one-time migrate backfill.
    pub fn set_companion_link(&self, note_id: &str, meeting_id: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        Self::upsert_link_tx(
            &tx,
            "note",
            note_id,
            "meeting",
            meeting_id,
            "companion",
            1.0,
            "user",
            "active",
            now,
        )?;
        tx.commit().map_err(map_err)?;
        Ok(())
    }

    /// Brain-v3 audit Fix 2 — RE-ASSERT the `companion` edges of a JUST-UNSEALED folder that the seal
    /// purge dropped. A `companion` edge (`note → meeting`, `created_by='user'`) is NOT a preserved
    /// decision row ([`LINK_DECISION_KEEP`] keeps dismissed tombstones, accepted edges and manual
    /// links), and `set_companion_link`
    /// only fires at the recording-time write site — so ONE lock cycle permanently deletes it (the
    /// one-time backfill sentinel is spent). This restores both legs in ONE tx on unlock:
    ///
    ///   • OUTBOUND — every companion note IN this folder (`documents.kind='note'` with a non-null
    ///     `meeting_id`) → its meeting; and
    ///   • INBOUND — every companion note in ANY folder whose `meeting_id` is a meeting IN this folder
    ///     (a companion note can live in a DIFFERENT folder than its meeting) → that meeting.
    ///
    /// Each edge is written ONLY when NEITHER endpoint is sealed-at-rest (the Fix-0 probe) — so a
    /// companion note that itself lives in a still-sealed folder never has its edge re-asserted (no
    /// link naming a sealed neighbour). `meeting_ids` is the folder's meetings (resolved by the
    /// caller's unlock, plaintext restored). Best-effort at the call site; logs ids/counts only.
    pub fn rederive_companion_links_for_folder(
        &self,
        folder_id: &str,
        meeting_ids: &[String],
    ) -> Result<usize> {
        let now = chrono::Utc::now().timestamp_millis();
        // Gather the (note_id, meeting_id) companion pairs to re-assert: OUTBOUND (notes in this
        // folder) UNION INBOUND (notes anywhere pointing at this folder's meetings). Resolve OUTSIDE
        // the guard loop so the SELECT and the guarded upserts share one tx.
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let mut pairs: Vec<(String, String)> = Vec::new();
        {
            // OUTBOUND: companion notes filed in THIS folder.
            let mut stmt = tx
                .prepare(
                    "SELECT id, meeting_id FROM documents
                       WHERE kind = 'note' AND folder_id = ?1 AND meeting_id IS NOT NULL",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![folder_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(map_err)?;
            for r in rows {
                pairs.push(r.map_err(map_err)?);
            }
        }
        // INBOUND: companion notes ANYWHERE whose meeting is one of this folder's meetings (a
        // companion note may be filed in a different folder than its meeting). Per-id so the set stays
        // small and the query needs no dynamic IN-list build.
        for mid in meeting_ids {
            let mut stmt = tx
                .prepare(
                    "SELECT id FROM documents
                       WHERE kind = 'note' AND meeting_id = ?1",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![mid], |r| r.get::<_, String>(0))
                .map_err(map_err)?;
            for r in rows {
                pairs.push((r.map_err(map_err)?, mid.clone()));
            }
        }
        // Dedupe (OUTBOUND and INBOUND overlap when the companion note is filed IN this folder).
        pairs.sort();
        pairs.dedup();
        let mut written = 0usize;
        for (note_id, meeting_id) in &pairs {
            // Fix-0 discipline: never re-assert an edge naming a sealed-at-rest endpoint. The meeting
            // is unlocked (plaintext restored), so this normally only guards a companion note that
            // still lives in another sealed folder.
            if meeting_sealed_at_rest_tx(&tx, meeting_id)? || doc_sealed_at_rest_tx(&tx, note_id)? {
                continue;
            }
            Self::upsert_link_tx(
                &tx,
                "note",
                note_id,
                "meeting",
                meeting_id,
                "companion",
                1.0,
                "user",
                "active",
                now,
            )?;
            written += 1;
        }
        tx.commit().map_err(map_err)?;
        tracing::debug!(target: "links", folder = %folder_id, companion_edges = written, "rederive_companion_links_for_folder");
        Ok(written)
    }

    /// Brain-v3 audit Fix 3 — RE-DERIVE the INBOUND wikilinks INTO a just-unsealed folder's items. A
    /// note A (in any OTHER, still-open folder) that carries `[[Project X]]` — whose target note X
    /// lives in folder F — had its `A → X` wikilink edge PURGED when F was sealed (X was a sealed
    /// endpoint). `rederive_links_for_folder` only re-indexes F's OWN sources, so without this the
    /// inbound edge stays gone indefinitely (A may never be edited again). This closes that leg:
    ///
    ///   1. For every meeting + note IN folder F (its titles now resolvable — plaintext restored),
    ///      find every VISIBLE SOURCE whose body carries `[[that title]]` via the SAME gated
    ///      [`Self::backlinks_for_visible`] scan the "Linked mentions" panel uses (both gates intact),
    ///      collecting the distinct `(source_kind, source_id)` set.
    ///   2. RE-RUN [`Self::index_wikilinks_for_source`] on each such source's body — a delete-then-
    ///      insert that re-resolves ALL its `[[Title]]`s against the current (post-unlock) visibility,
    ///      so the edge INTO F is re-established. Fix 0's in-tx re-check keeps it from naming any
    ///      OTHER still-sealed target.
    ///
    /// `meeting_ids` are F's meetings; `unlocked` is the post-unlock session set (F included). Returns
    /// the count of distinct sources re-indexed. Best-effort per source at the call site. IDs/counts
    /// only in logs.
    pub fn rederive_inbound_wikilinks_for_folder(
        &self,
        folder_id: &str,
        meeting_ids: &[String],
        unlocked: &HashSet<String>,
    ) -> Result<usize> {
        use crate::storage::models::SourceKind;
        // 1. Collect the distinct inbound sources across every item in the folder. A source is
        //    identified as (is_meeting, id) so the two kinds never collide in the id space.
        let mut sources: std::collections::HashSet<(bool, String)> =
            std::collections::HashSet::new();
        // Meeting targets in F.
        for mid in meeting_ids {
            for src in self.backlinks_for_visible(SourceKind::Meeting, mid, unlocked)? {
                sources.insert((src.kind == SourceKind::Meeting, src.id));
            }
        }
        // Note (document kind='note') targets in F.
        for did in self.document_ids_in_folder(folder_id)? {
            for src in self.backlinks_for_visible(SourceKind::Note, &did, unlocked)? {
                sources.insert((src.kind == SourceKind::Meeting, src.id));
            }
        }
        // 2. Re-index each distinct source's body so its `[[Title]]` INTO F resolves + re-inserts.
        let mut reindexed = 0usize;
        for (is_meeting, src_id) in &sources {
            if *is_meeting {
                // Never re-index a target inside F as its own inbound source (F's own sources are
                // re-derived by `rederive_links_for_folder`; here we only want OUTSIDE sources — but a
                // same-folder cross-link is harmless to re-run, just redundant).
                if let Some(note) = self.get_latest_note_for_meeting(src_id)? {
                    self.index_wikilinks_for_source(
                        crate::links::LinkKind::Meeting,
                        src_id,
                        &note.markdown,
                        unlocked,
                    )?;
                    reindexed += 1;
                }
            } else if let Some(row) = self.get_note_row(src_id)? {
                self.index_wikilinks_for_source(
                    crate::links::LinkKind::Note,
                    src_id,
                    &row.text,
                    unlocked,
                )?;
                reindexed += 1;
            }
        }
        tracing::debug!(target: "links", folder = %folder_id, inbound_sources = reindexed, "rederive_inbound_wikilinks_for_folder");
        Ok(reindexed)
    }

    /// note↔meeting-links PR-1 — upsert ONE user-initiated DIRECTED `manual` edge (`created_by='user'`,
    /// `status='active'`, `score=1.0`). Idempotent on the table's UNIQUE key (a repeat link is a no-op
    /// refresh, never a duplicate row). Directed like wikilink/companion — endpoints are stored AS
    /// PASSED (never canonicalized). The CALLER (`link_items_inner`) gates BOTH endpoints
    /// session-visible, and this transaction repeats the at-rest endpoint gate immediately before
    /// insertion so a concurrent seal, context-disable, leave, or final withdrawal cannot race a
    /// stale snapshot into a durable row. Local folder seals purge affected local endpoints; org
    /// withdrawal instead withholds the opaque private row through the both-endpoint-gated
    /// `links_for_visible` until a live successor exists again.
    pub fn upsert_manual_link(
        &self,
        src_kind: &str,
        src_id: &str,
        dst_kind: &str,
        dst_id: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let src = crate::links::LinkKind::parse(src_kind)
            .ok_or_else(|| AppError::InvalidArg("invalid source link kind".into()))?;
        let dst = crate::links::LinkKind::parse(dst_kind)
            .ok_or_else(|| AppError::InvalidArg("invalid destination link kind".into()))?;
        if link_endpoint_sealed_at_rest_tx(&tx, src, src_id)?
            || link_endpoint_sealed_at_rest_tx(&tx, dst, dst_id)?
        {
            return Err(AppError::Locked(
                "one of these items is no longer available to link".into(),
            ));
        }
        Self::upsert_link_tx(
            &tx, src_kind, src_id, dst_kind, dst_id, "manual", 1.0, "user", "active", now,
        )?;
        tx.commit().map_err(map_err)?;
        tracing::info!(
            target: "links",
            src_kind = src_kind,
            dst_kind = dst_kind,
            "upsert_manual_link"
        );
        Ok(())
    }

    /// note↔meeting-links PR-1 — delete ONLY the DIRECTED `manual` edge for `(src → dst)`. NEVER
    /// touches a `wikilink`/`companion`/`semantic` row for the same pair (the `edge_type='manual'`
    /// predicate is exact): unlinking a manual link leaves any derived wikilink/companion/semantic
    /// relation intact. This narrow restore/retry API is idempotent for a missing exact row; the
    /// collapsed multi-edge [`Self::delete_manual_links`] API remains strict and atomic. Legacy
    /// marker preparation is performed transactionally by that batch primitive.
    pub fn delete_manual_link(
        &self,
        src_kind: &str,
        src_id: &str,
        dst_kind: &str,
        dst_id: &str,
    ) -> Result<()> {
        let _cleanup_queued = self.delete_manual_links_with_marker_seals_mode(
            &[crate::storage::models::ManualLinkEdge {
                src_kind: src_kind.to_string(),
                src_id: src_id.to_string(),
                dst_kind: dst_kind.to_string(),
                dst_id: dst_id.to_string(),
            }],
            &[],
            false,
        )?;
        tracing::info!(
            target: "links",
            src_kind = src_kind,
            dst_kind = dst_kind,
            "delete_manual_link"
        );
        Ok(())
    }

    /// Delete a collapsed chip's complete set of exact directed `manual` rows in ONE transaction.
    /// Every endpoint is re-checked for at-rest availability before the first mutation. For every
    /// legacy note-source marker, the same transaction strips the DB body and journals its exact
    /// vault export BEFORE deleting the last naming edge. A preparation error rolls the whole set
    /// back, so a concurrent seal cannot lose the only durable cleanup intent. Derived
    /// `wikilink`/`companion`/`semantic` rows are excluded by the exact `edge_type` predicate.
    /// Returns whether at least one vault-cleanup outbox row was queued.
    pub fn delete_manual_links(
        &self,
        manual_edges: &[crate::storage::models::ManualLinkEdge],
    ) -> Result<bool> {
        self.delete_manual_links_with_marker_seals(manual_edges, &[])
    }

    /// Command-layer twin of [`Self::delete_manual_links`] carrying verified fresh seals for any
    /// session-unlocked locked source whose legacy marker is mutated in the transaction.
    pub(crate) fn delete_manual_links_with_marker_seals(
        &self,
        manual_edges: &[crate::storage::models::ManualLinkEdge],
        prepared_seals: &[PreparedManualMarkerSeal],
    ) -> Result<bool> {
        self.delete_manual_links_with_marker_seals_mode(manual_edges, prepared_seals, true)
    }

    fn delete_manual_links_with_marker_seals_mode(
        &self,
        manual_edges: &[crate::storage::models::ManualLinkEdge],
        prepared_seals: &[PreparedManualMarkerSeal],
        strict_missing: bool,
    ) -> Result<bool> {
        if manual_edges.is_empty() || manual_edges.len() > 2 {
            return Err(AppError::InvalidArg(
                "a collapsed pair must contain one or two manual link edges".into(),
            ));
        }
        for (index, edge) in manual_edges.iter().enumerate() {
            if manual_edges[..index].contains(edge) {
                return Err(AppError::InvalidArg(
                    "duplicate manual link edge in unlink request".into(),
                ));
            }
        }
        for (index, seal) in prepared_seals.iter().enumerate() {
            if seal.text_blob.is_empty()
                || prepared_seals[..index]
                    .iter()
                    .any(|prior| prior.note_id == seal.note_id)
            {
                return Err(AppError::InvalidArg(
                    "invalid prepared manual-marker seal set".into(),
                ));
            }
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        for edge in manual_edges {
            let src = crate::links::LinkKind::parse(&edge.src_kind)
                .ok_or_else(|| AppError::InvalidArg("invalid source link kind".into()))?;
            let dst = crate::links::LinkKind::parse(&edge.dst_kind)
                .ok_or_else(|| AppError::InvalidArg("invalid destination link kind".into()))?;
            if link_endpoint_sealed_at_rest_tx(&tx, src, &edge.src_id)?
                || link_endpoint_sealed_at_rest_tx(&tx, dst, &edge.dst_id)?
            {
                return Err(AppError::Locked(
                    "one of these items is no longer available to unlink".into(),
                ));
            }
        }
        let cleanup_queued =
            Self::prepare_manual_marker_cleanup_tx(&tx, manual_edges, prepared_seals)?;
        for edge in manual_edges {
            let deleted = tx
                .execute(
                    "DELETE FROM links
                   WHERE src_kind = ?1 AND src_id = ?2 AND dst_kind = ?3 AND dst_id = ?4
                     AND edge_type = 'manual'",
                    rusqlite::params![edge.src_kind, edge.src_id, edge.dst_kind, edge.dst_id],
                )
                .map_err(map_err)?;
            if strict_missing && deleted != 1 {
                return Err(AppError::InvalidArg(
                    "one of the selected manual link edges no longer exists".into(),
                ));
            }
        }
        tx.commit().map_err(map_err)?;
        tracing::info!(
            target: "links",
            count = manual_edges.len(),
            "delete_manual_links"
        );
        Ok(cleanup_queued)
    }

    /// Resolve the current marker title while holding the exact unlink transaction. This is not a
    /// general read surface: the command already gated both endpoints, and the transaction repeats
    /// their at-rest availability check before calling it. `None` is a fail-closed preparation
    /// refusal for a note-source edge, never permission to delete first and guess later.
    fn manual_marker_title_tx(
        tx: &rusqlite::Transaction<'_>,
        kind: crate::links::LinkKind,
        id: &str,
    ) -> Result<Option<String>> {
        match kind {
            crate::links::LinkKind::Meeting => tx
                .query_row(
                    "SELECT COALESCE(NULLIF(TRIM(title), ''), 'Meeting')
                       FROM meetings WHERE id = ?1",
                    rusqlite::params![id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(map_err),
            crate::links::LinkKind::Note | crate::links::LinkKind::Document => {
                let expected_kind = match kind {
                    crate::links::LinkKind::Note => "note",
                    crate::links::LinkKind::Document => "document",
                    crate::links::LinkKind::Meeting | crate::links::LinkKind::Org => unreachable!(),
                };
                tx.query_row(
                    "SELECT COALESCE(NULLIF(TRIM(title), ''), name)
                       FROM documents WHERE id = ?1 AND kind = ?2",
                    rusqlite::params![id, expected_kind],
                    |row| row.get(0),
                )
                .optional()
                .map_err(map_err)
            }
            crate::links::LinkKind::Org => {
                let Some((org_id, doc_id)) = parse_org_link_id(id) else {
                    return Ok(None);
                };
                tx.query_row(
                    "SELECT oi.title
                       FROM org_items oi
                       JOIN org_state os ON os.org_id = oi.org_id
                      WHERE oi.org_id = ?1 AND oi.doc_id = ?2
                        AND oi.tombstoned = 0 AND oi.is_current = 1
                        AND COALESCE(oi.source_kind, '') NOT IN ('task','container')
                        AND os.context_enabled = 1
                      LIMIT 1",
                    rusqlite::params![org_id, doc_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(map_err)
            }
        }
    }

    /// Prepare every legacy note-source marker cleanup in the unlink transaction. Only a real
    /// machine-owned marker is mutated/journalled; ordinary user-authored wikilinks outside the
    /// managed block are untouched. The returned flag tells the command whether it must drain the
    /// durable filesystem outbox before reporting success.
    fn prepare_manual_marker_cleanup_tx(
        tx: &rusqlite::Transaction<'_>,
        manual_edges: &[crate::storage::models::ManualLinkEdge],
        prepared_seals: &[PreparedManualMarkerSeal],
    ) -> Result<bool> {
        let mut titles_by_note: std::collections::HashMap<
            String,
            std::collections::HashSet<String>,
        > = std::collections::HashMap::new();
        for edge in manual_edges {
            let src = crate::links::LinkKind::parse(&edge.src_kind)
                .ok_or_else(|| AppError::InvalidArg("invalid source link kind".into()))?;
            if src != crate::links::LinkKind::Note {
                continue;
            }
            let dst = crate::links::LinkKind::parse(&edge.dst_kind)
                .ok_or_else(|| AppError::InvalidArg("invalid destination link kind".into()))?;
            let title = Self::manual_marker_title_tx(tx, dst, &edge.dst_id)?.ok_or_else(|| {
                AppError::Locked("one of these items is no longer available to unlink".into())
            })?;
            let title = crate::enrich::sanitize(&title);
            if title.trim().is_empty() {
                return Err(AppError::InvalidArg(
                    "a linked item has no usable marker title".into(),
                ));
            }
            titles_by_note
                .entry(edge.src_id.clone())
                .or_default()
                .insert(title);
        }

        let mut cleanup_queued = false;
        let mut used_seals = std::collections::HashSet::new();
        for (note_id, titles) in titles_by_note {
            let row: Option<(String, Option<String>, bool, String)> = tx
                .query_row(
                    "SELECT COALESCE(d.text, ''), d.exported_path, f.locked, d.folder_id
                       FROM documents d
                       JOIN folders f ON f.id = d.folder_id
                      WHERE d.id = ?1 AND d.kind = 'note'",
                    rusqlite::params![note_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()
                .map_err(map_err)?;
            let Some((text, exported_path, folder_locked, folder_id)) = row else {
                return Err(AppError::Locked(
                    "a linked note is no longer available to unlink".into(),
                ));
            };
            let Some(stripped) = Self::strip_titles_from_managed_block(&text, &titles) else {
                continue;
            };
            if let Some(path) = exported_path.filter(|path| !path.is_empty()) {
                let expected_hash = crate::export::note_content_hash(&stripped);
                for title in &titles {
                    Self::enqueue_marker_export_cleanup_tx(
                        tx,
                        false,
                        &note_id,
                        "",
                        &path,
                        title,
                        &expected_hash,
                    )?;
                }
                cleanup_queued = true;
            }
            let changed = if folder_locked {
                let prepared = prepared_seals
                    .iter()
                    .find(|prepared| prepared.note_id == note_id)
                    .ok_or_else(|| {
                        AppError::Locked(
                            "locked note marker cleanup has no verified fresh seal".into(),
                        )
                    })?;
                if prepared.stripped_text != stripped {
                    return Err(AppError::Locked(
                        "linked note changed while its marker cleanup was prepared".into(),
                    ));
                }
                if prepared.folder_id != folder_id
                    || crate::crypto::decrypt(
                        &prepared.content_key,
                        &prepared.text_blob,
                        &document_seal_aad(&folder_id, &note_id),
                    )? != stripped.as_bytes()
                {
                    return Err(AppError::Locked(
                        "locked note marker cleanup seal did not verify byte-identical".into(),
                    ));
                }
                used_seals.insert(note_id.clone());
                tx.execute(
                    "UPDATE documents SET text = '', text_blob = ?2
                       WHERE id = ?1 AND kind = 'note' AND text = ?3",
                    rusqlite::params![note_id, prepared.text_blob, text],
                )
                .map_err(map_err)?
            } else {
                tx.execute(
                    "UPDATE documents SET text = ?2
                       WHERE id = ?1 AND kind = 'note' AND text = ?3",
                    rusqlite::params![note_id, stripped, text],
                )
                .map_err(map_err)?
            };
            if changed != 1 {
                return Err(AppError::Storage(
                    "linked note changed while preparing marker cleanup".into(),
                ));
            }
        }
        if used_seals.len() != prepared_seals.len() {
            return Err(AppError::InvalidArg(
                "prepared manual-marker seal does not belong to this unlink".into(),
            ));
        }
        Ok(cleanup_queued)
    }

    /// TEST-ONLY: insert one raw `links` row and return its row id, so sibling-crate test modules
    /// (`commands.rs`) can seed a specific edge (a semantic suggestion, a wikilink) to drive the
    /// accept/dismiss gate tests without reaching the private `upsert_link_tx`. Not compiled into the
    /// shipping binary.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_link_for_test(
        &self,
        src_kind: &str,
        src_id: &str,
        dst_kind: &str,
        dst_id: &str,
        edge_type: &str,
        score: f64,
        created_by: &str,
        status: &str,
    ) -> i64 {
        let mut conn = self.lock();
        let tx = conn.transaction().unwrap();
        Self::upsert_link_tx(
            &tx,
            src_kind,
            src_id,
            dst_kind,
            dst_id,
            edge_type,
            score,
            created_by,
            status,
            1_700_000_000_000,
        )
        .unwrap();
        tx.commit().unwrap();
        conn.query_row(
            "SELECT id FROM links WHERE src_kind=?1 AND src_id=?2 AND dst_kind=?3 AND dst_id=?4 AND edge_type=?5",
            rusqlite::params![src_kind, src_id, dst_kind, dst_id, edge_type],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// TEST-ONLY raw count of `links` rows of `edge_type` incident on `(kind, id)` (either endpoint).
    /// Exposes the private connection to sibling-crate test modules (`commands.rs`) so they can assert
    /// the PRE-collapse row counts (manual vs wikilink separately) that `links_for_visible` hides. Not
    /// compiled into the shipping binary.
    #[cfg(test)]
    pub(crate) fn link_edge_count(&self, kind: &str, id: &str, edge_type: &str) -> i64 {
        self.lock()
            .query_row(
                "SELECT COUNT(*) FROM links
                   WHERE edge_type = ?3
                     AND ((src_kind = ?1 AND src_id = ?2) OR (dst_kind = ?1 AND dst_id = ?2))",
                rusqlite::params![kind, id, edge_type],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// The `links` rows a seal PRESERVES: a user's DECISION about a pair. Brain-v3 audit Fix 1 —
    /// `purge_links_tx` (and the startup reblank's inline twin) must NOT destroy these on seal, or a
    /// lock→unlock RESURRECTS a dismissed suggestion (the semantic pass re-proposes it) and forgets an
    /// accepted edge, contradicting the spec ("a rejected suggestion never reappears").
    ///
    /// Three decision classes survive:
    ///   - `status='dismissed'` — the tombstone that stops a re-suggest (its `upsert_link_tx`
    ///     DO-UPDATE guard skips dismissed rows),
    ///   - `status='active' AND created_by='accepted'` — a user-CONFIRMED semantic link, and
    ///   - `status='active' AND edge_type='manual'` — a link the user MADE.
    ///
    /// # Why `manual` had to join them
    ///
    /// This list used to end "everything else (wikilink/companion/manual/auto-suggested) is DERIVED
    /// and re-derivable on unlock, so it is purged". That was true of three of those four and false
    /// of `manual`, and the sentence is what made the loss look deliberate. A wikilink is re-derived
    /// by re-indexing the note body; a companion edge from the meeting↔note relation; a semantic
    /// suggestion by re-running the kNN pass. A manual edge has NO such source:
    /// `commands::links::link_items` writes the row and DELIBERATELY writes no `[[Title]]` marker
    /// into the body (that machine block went stale and rendered as junk, so it was removed), which
    /// its own comment states — "the manual edge is the AUTHORITATIVE record of the link".
    ///
    /// So sealing a folder destroyed every hand-made connection touching it, `rederive_links_for_folder`
    /// brought back only the derivable kinds, and unlocking returned an item with its automatic edges
    /// intact and the user's own ones gone. Silently: nothing failed, nothing was reported.
    ///
    /// The key is `edge_type = 'manual'` and NOT `created_by = 'user'`, though every manual row
    /// carries both. `created_by='user'` is also written for wikilink and companion edges, and those
    /// MUST still die on seal — preserving a wikilink across a seal would resurrect a link the user
    /// had removed from the body while the folder was closed.
    ///
    /// A preserved row carries ONLY ids/kind/edge_type/score inside the SQLCipher DB — NO titles, NO
    /// plaintext — so keeping it leaks nothing at rest; and every reader is both-endpoint-gated, so
    /// only the DECISION STATE survives, never its visibility: [`links_for_visible`] drops an edge
    /// whose neighbour is not visible (GATE 2), and the full-brain graph re-checks both endpoints
    /// against its visible-node set before emitting an edge. Everything genuinely DERIVED
    /// (wikilink/companion/auto-suggested) is still purged and re-derived on unlock.
    pub(crate) const LINK_DECISION_KEEP: &'static str = "status = 'dismissed' \
         OR (status = 'active' AND created_by = 'accepted') \
         OR (status = 'active' AND edge_type = 'manual')";

    fn enqueue_marker_export_cleanup_tx(
        tx: &rusqlite::Transaction<'_>,
        is_meeting: bool,
        source_id: &str,
        provider_id: &str,
        exported_path: &str,
        sealed_title: &str,
        expected_hash: &str,
    ) -> Result<()> {
        tx.execute(
            "INSERT INTO lock_marker_export_cleanup(
               source_kind, source_id, provider_id, exported_path, sealed_title, expected_hash
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(source_kind,source_id,provider_id,exported_path,sealed_title)
             DO UPDATE SET expected_hash=excluded.expected_hash",
            rusqlite::params![
                if is_meeting { "meeting" } else { "note" },
                source_id,
                provider_id,
                exported_path,
                sealed_title,
                expected_hash,
            ],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// Brain-v3 audit Fix 4 — STRIP a just-sealed neighbour's materialized `[[Title]]` from the
    /// MACHINE-OWNED `murmur:links` block of every VISIBLE note that links to it, INSIDE the seal tx
    /// (so the DB plaintext is scrubbed atomically with the purge). Compose accept/link wrote
    /// `[[Neighbour Title]]` into a source note's managed block; when the neighbour's folder later
    /// seals, the `links` row is purged and every gated read hides the neighbour — but the source
    /// note's BODY (plaintext in the DB AND its exported vault `.md`) still names the sealed neighbour.
    /// The seal's own rationale says a sealed item's title+existence is leak-relevant, so the marker
    /// must go too.
    ///
    /// MUST run BEFORE [`purge_links_tx`] in the same tx — the `links` rows being purged are exactly
    /// what names the affected source→sealed-item pairs. Sealed items' TITLES are still readable here
    /// (seal blanks BODY content, never `meetings.title` / `documents.title`). Only the MACHINE block
    /// is touched (via [`crate::enrich::extract_link_hits`] / `apply_link_markers`) — a user-typed
    /// `[[Title]]` outside the managed block is the user's own content and is NEVER stripped.
    ///
    /// A source is scrubbed ONLY when it is itself VISIBLE (not sealed-at-rest) — a source in another
    /// still-sealed folder has its body blanked already, so there is nothing to leak. Returns the
    /// `(is_meeting, source_id)` of every source whose plaintext body changed, so the COMMAND layer
    /// can re-export its vault `.md` (the same DB-in-tx / filesystem-at-command layering as sealed-note
    /// `.md` deletion). Re-materialized on unlock from the preserved accepted rows (Fix 1) +
    /// [`Self::rematerialize_accepted_markers_for_folder`]. IDs/counts only in logs.
    pub(crate) fn strip_sealed_neighbour_markers_tx(
        tx: &rusqlite::Transaction<'_>,
        sealed_meeting_ids: &[String],
        sealed_document_ids: &[String],
    ) -> Result<Vec<(bool, String)>> {
        // 1. Gather (sealed_kind, sealed_id) → collect the distinct SOURCE (src_kind, src_id) rows that
        //    materialize a marker: `wikilink`/`manual`/`companion`/accepted-`semantic` edges pointing
        //    AT the sealed item. A `companion` edge never materializes a title marker (it's structural),
        //    but including it is harmless (the strip is a no-op when the block lacks the title).
        let mut affected: std::collections::HashMap<
            (bool, String),
            std::collections::HashSet<String>,
        > = std::collections::HashMap::new();
        let mut collect = |tx: &rusqlite::Transaction<'_>,
                           sealed_kinds: &str,
                           sealed_id: &str|
         -> Result<()> {
            // The sealed item's CURRENT title (still readable — seal blanks body, not title). SANITIZE
            // it: a materialized marker is rendered through `enrich::sanitize` (whitespace-collapse +
            // comment-delimiter rewrite), so the raw DB title must be sanitized to match the rendered
            // `[[Title]]` (lock-security finding — a title with a double space would otherwise survive).
            let title = Self::sealed_item_title_tx(tx, sealed_kinds, sealed_id)?;
            let Some(title) = title.filter(|t| !t.trim().is_empty()) else {
                return Ok(());
            };
            let title = crate::enrich::sanitize(&title);
            // The marker OWNER is whichever `note`/`meeting` endpoint is NOT the sealed item. Semantic
            // edges are CANONICALIZED (the smaller `(kind,id)` is stored as `src`), so the sealed item
            // can be on EITHER side — scan BOTH directions (dst-leg: owner = `src`; src-leg: owner =
            // `dst`). A dst-only scan misses a dst-owned marker naming a sealed `src` (e.g. a
            // `document`-src / `note`-dst accepted edge — `"document" < "note"`), leaking the sealed
            // title in the visible note's plaintext + `.md` (lock-security finding).
            let sql = format!(
                "SELECT src_kind, src_id FROM links
                   WHERE dst_id = ?1 AND dst_kind IN ({sealed_kinds}) AND src_kind IN ('note','meeting')
                 UNION
                 SELECT dst_kind, dst_id FROM links
                   WHERE src_id = ?1 AND src_kind IN ({sealed_kinds}) AND dst_kind IN ('note','meeting')"
            );
            let mut stmt = tx.prepare(&sql).map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![sealed_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(map_err)?;
            for r in rows {
                let (owner_kind, owner_id) = r.map_err(map_err)?;
                let is_meeting = owner_kind == "meeting";
                affected
                    .entry((is_meeting, owner_id))
                    .or_default()
                    .insert(title.clone());
            }
            Ok(())
        };
        for mid in sealed_meeting_ids {
            collect(tx, "'meeting'", mid)?;
        }
        for did in sealed_document_ids {
            // A note id IS a document id — the marker's dst kind is 'note' (materialized wikilinks
            // resolve to the `note` kind) or 'document'.
            collect(tx, "'note','document'", did)?;
        }
        // 2. For each affected VISIBLE source, strip EVERY sealed title from its managed block.
        let mut changed: Vec<(bool, String)> = Vec::new();
        for ((is_meeting, src_id), titles) in &affected {
            // Skip a source that is itself sealed-at-rest (its body is already blanked — nothing to
            // leak, and we must not resurrect plaintext behind its own lock).
            let sealed = if *is_meeting {
                meeting_sealed_at_rest_tx(tx, src_id)?
            } else {
                doc_sealed_at_rest_tx(tx, src_id)?
            };
            if sealed {
                continue;
            }
            let did_change = if *is_meeting {
                Self::strip_and_journal_meeting_markers_tx(tx, src_id, titles)?
            } else {
                Self::strip_and_journal_note_markers_tx(tx, src_id, titles)?
            };
            if did_change {
                changed.push((*is_meeting, src_id.clone()));
            }
        }
        Ok(changed)
    }

    /// The current display TITLE of a to-be-sealed item, read inside the seal tx. `sealed_kinds` is the
    /// SQL kind-list literal (`'meeting'` or `'note','document'`) that identifies the endpoint's table.
    /// A meeting → `meetings.title`; a note/document → `documents.title` (fallback `name`). `None` when
    /// unknown or the title is empty. Titles are NEVER blanked by seal, so this is readable mid-seal.
    fn sealed_item_title_tx(
        tx: &rusqlite::Transaction<'_>,
        sealed_kinds: &str,
        sealed_id: &str,
    ) -> Result<Option<String>> {
        if sealed_kinds == "'meeting'" {
            tx.query_row(
                "SELECT NULLIF(TRIM(COALESCE(title, '')), '') FROM meetings WHERE id = ?1",
                rusqlite::params![sealed_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .map(Option::flatten)
            .map_err(map_err)
        } else {
            tx.query_row(
                "SELECT COALESCE(NULLIF(TRIM(title), ''), name) FROM documents WHERE id = ?1 AND kind = 'note'",
                rusqlite::params![sealed_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .map(Option::flatten)
            .map_err(map_err)
        }
    }

    /// Strip and journal EVERY provider row/export for a meeting source. Provider rows are distinct
    /// canonical notes and may share a timestamp; updating `created_at = MAX(...)` would collapse
    /// their different markdown, while selecting only one export would leave older vault files
    /// leaking the sealed title.
    fn strip_and_journal_meeting_markers_tx(
        tx: &rusqlite::Transaction<'_>,
        meeting_id: &str,
        titles: &std::collections::HashSet<String>,
    ) -> Result<bool> {
        let provider_rows: Vec<(String, String, Option<String>)> = {
            let mut stmt = tx
                .prepare(
                    "SELECT provider_id, COALESCE(markdown, ''), exported_path
                       FROM notes WHERE meeting_id = ?1
                      ORDER BY created_at DESC, provider_id ASC",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(rusqlite::params![meeting_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                })
                .map_err(map_err)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(map_err)?);
            }
            out
        };
        let mut changed = false;
        for (provider_id, markdown, exported_path) in provider_rows {
            let stripped = Self::strip_titles_from_managed_block(&markdown, titles)
                .unwrap_or_else(|| markdown.clone());
            // Enqueue even when this DB row is already scrubbed: that is the replay path after a
            // previous strip commit crashed before the exact provider export was rewritten.
            if let Some(path) = exported_path.filter(|path| !path.is_empty()) {
                let expected_hash = crate::export::note_content_hash(&stripped);
                for title in titles {
                    Self::enqueue_marker_export_cleanup_tx(
                        tx,
                        true,
                        meeting_id,
                        &provider_id,
                        &path,
                        title,
                        &expected_hash,
                    )?;
                }
            }
            if stripped == markdown {
                continue;
            }
            tx.execute(
                "UPDATE notes SET markdown = ?3 WHERE meeting_id = ?1 AND provider_id = ?2",
                rusqlite::params![meeting_id, provider_id, stripped],
            )
            .map_err(map_err)?;
            changed = true;
        }
        Ok(changed)
    }

    /// Authored-note twin: enqueue its exact export even when the DB body is already scrubbed, then
    /// mutate only the machine-owned block in the same transaction.
    fn strip_and_journal_note_markers_tx(
        tx: &rusqlite::Transaction<'_>,
        note_id: &str,
        titles: &std::collections::HashSet<String>,
    ) -> Result<bool> {
        let row: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT COALESCE(text, ''), exported_path
                   FROM documents WHERE id = ?1 AND kind = 'note'",
                rusqlite::params![note_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(map_err)?;
        let Some((text, exported_path)) = row else {
            return Ok(false);
        };
        let stripped = Self::strip_titles_from_managed_block(&text, titles)
            .unwrap_or_else(|| text.clone());
        if let Some(path) = exported_path.filter(|path| !path.is_empty()) {
            let expected_hash = crate::export::note_content_hash(&stripped);
            for title in titles {
                Self::enqueue_marker_export_cleanup_tx(
                    tx,
                    false,
                    note_id,
                    "",
                    &path,
                    title,
                    &expected_hash,
                )?;
            }
        }
        if stripped == text {
            return Ok(false);
        }
        tx.execute(
            "UPDATE documents SET text = ?2 WHERE id = ?1 AND kind = 'note'",
            rusqlite::params![note_id, stripped],
        )
        .map_err(map_err)?;
        Ok(true)
    }

    /// PURE: strip the `[[title]]` hits named by `titles` from the MACHINE-OWNED `murmur:links` block
    /// of `body`, via [`crate::enrich::extract_link_hits`] + `apply_link_markers` (the exact inverse
    /// the transactional manual-unlink preparation uses). Returns `Some(new_body)` iff a hit was
    /// removed (so the caller only writes on a real change), else `None`. A user-typed `[[Title]]`
    /// OUTSIDE the managed block is never in `extract_link_hits`'s output, so it is never stripped.
    pub(crate) fn strip_titles_from_managed_block(
        body: &str,
        titles: &std::collections::HashSet<String>,
    ) -> Option<String> {
        // A materialized neighbour marker renders `[[Title]]` in EITHER `detail` (the accept/manual
        // shape: `detail = "[[Title]]"`) OR `url` (the always-on auto related-notes shape:
        // `detail` = gist, `url = Some("[[Title]]")`). Match BOTH fields — a `detail`-only match keeps
        // the auto-related marker and leaks the sealed title (adversarial-verifier finding).
        fn wikilink_inner(s: &str) -> Option<&str> {
            s.trim()
                .strip_prefix("[[")
                .and_then(|x| x.strip_suffix("]]"))
                .map(str::trim)
        }
        let mut hits = crate::enrich::extract_link_hits(body);
        let before = hits.len();
        hits.retain(|h| {
            match wikilink_inner(&h.detail).or_else(|| h.url.as_deref().and_then(wikilink_inner)) {
                // `titles` holds SANITIZED titles; sanitize the inner too so a whitespace/delimiter
                // round-trip can't dodge the compare. Drop the hit iff its title is sealed.
                Some(t) => {
                    let key = crate::enrich::sanitize(t);
                    !titles.contains(key.trim())
                }
                None => true, // not a wikilink marker → keep (a connector hit).
            }
        });
        if hits.len() == before {
            return None; // nothing matched → no change.
        }
        Some(crate::enrich::apply_link_markers(body, &hits))
    }

    /// Brain-v3 audit Fix 4 — PUBLIC entry: strip the just-sealed items' `[[Title]]` markers from every
    /// VISIBLE source note's managed block, in ONE tx, and return the `(is_meeting, source_id)` of each
    /// source whose plaintext body changed. The COMMAND layer calls this on the seal path (right after
    /// its seal_note/seal_folder_extras + purge legs) and re-exports each changed source's vault `.md`
    /// — the DB-in-tx / filesystem-at-command layering that sealed-note `.md` deletion uses. Also run
    /// (with the locked-folder ids resolved) INSIDE [`Self::reblank_locked_folders_at_rest`], so a
    /// crash BETWEEN the seal and this strip is repaired on the next launch. `sealed_document_ids`
    /// covers both `note` and `document` id spaces. Idempotent (a body with no matching marker is a
    /// no-op). IDs/counts only in logs.
    pub fn strip_sealed_neighbour_markers(
        &self,
        sealed_meeting_ids: &[String],
        sealed_document_ids: &[String],
    ) -> Result<Vec<(bool, String)>> {
        if sealed_meeting_ids.is_empty() && sealed_document_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let changed =
            Self::strip_sealed_neighbour_markers_tx(&tx, sealed_meeting_ids, sealed_document_ids)?;
        tx.commit().map_err(map_err)?;
        tracing::debug!(target: "links", stripped_sources = changed.len(), "strip_sealed_neighbour_markers");
        Ok(changed)
    }

    /// Snapshot the durable marker-export outbox. Rows remain until the exact file has been
    /// durably scrubbed (or proved absent) and [`Self::ack_lock_marker_export_cleanup`] commits.
    pub(crate) fn pending_lock_marker_export_cleanup(
        &self,
    ) -> Result<Vec<LockMarkerExportCleanup>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT source_kind, source_id, provider_id, exported_path, sealed_title,
                        expected_hash
                   FROM lock_marker_export_cleanup
                  ORDER BY exported_path, source_kind, source_id, provider_id, sealed_title",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(LockMarkerExportCleanup {
                    source_kind: r.get(0)?,
                    source_id: r.get(1)?,
                    provider_id: r.get(2)?,
                    exported_path: r.get(3)?,
                    sealed_title: r.get(4)?,
                    expected_hash: r.get(5)?,
                })
            })
            .map_err(map_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_err)?);
        }
        Ok(out)
    }

    /// Return the existing authenticated stage reservation for `exported_path`, or durably reserve
    /// a fresh unpredictable name before any vault file is created. A pre-existing file under that
    /// name is never adopted: until [`Self::bind_lock_marker_export_publish`] records its inode the
    /// reconciler fails closed.
    pub(crate) fn reserve_lock_marker_export_publish(
        &self,
        exported_path: &str,
    ) -> Result<LockMarkerExportPublish> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        let existing = tx
            .query_row(
                "SELECT exported_path, stage_name, source_device, source_inode, source_hash,
                        stage_device, stage_inode, state
                   FROM lock_marker_export_publish WHERE exported_path = ?1",
                rusqlite::params![exported_path],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                },
            )
            .optional()
            .map_err(map_err)?;
        let raw = if let Some(existing) = existing {
            existing
        } else {
            let stage_name = format!(
                ".murmur-marker-cleanup-{}.pending",
                uuid::Uuid::new_v4().simple()
            );
            tx.execute(
                "INSERT INTO lock_marker_export_publish(exported_path, stage_name, state)
                 VALUES (?1, ?2, 'reserved')",
                rusqlite::params![exported_path, stage_name],
            )
            .map_err(map_err)?;
            (
                exported_path.to_string(),
                stage_name,
                None,
                None,
                None,
                None,
                None,
                "reserved".to_string(),
            )
        };
        tx.commit().map_err(map_err)?;
        Ok(LockMarkerExportPublish {
            exported_path: raw.0,
            stage_name: raw.1,
            source_device: parse_marker_identity(raw.2, "source device")?,
            source_inode: parse_marker_identity(raw.3, "source inode")?,
            source_hash: raw.4,
            stage_device: parse_marker_identity(raw.5, "stage device")?,
            stage_inode: parse_marker_identity(raw.6, "stage inode")?,
            state: raw.7,
        })
    }

    /// Bind the freshly-created empty stage to the exact source and stage inodes before writing
    /// bytes. This closes the old deterministic-name adoption bug: recovery accepts only these
    /// SQLCipher-authenticated identities.
    pub(crate) fn bind_lock_marker_export_publish(
        &self,
        publish: &LockMarkerExportPublish,
        source_device: u64,
        source_inode: u64,
        source_hash: &str,
        stage_device: u64,
        stage_inode: u64,
    ) -> Result<()> {
        let changed = self
            .lock()
            .execute(
                "UPDATE lock_marker_export_publish
                    SET source_device = ?3, source_inode = ?4, source_hash = ?5,
                        stage_device = ?6, stage_inode = ?7, state = 'created'
                  WHERE exported_path = ?1 AND stage_name = ?2 AND state = 'reserved'
                    AND source_device IS NULL AND source_inode IS NULL
                    AND stage_device IS NULL AND stage_inode IS NULL",
                rusqlite::params![
                    publish.exported_path,
                    publish.stage_name,
                    source_device.to_string(),
                    source_inode.to_string(),
                    source_hash,
                    stage_device.to_string(),
                    stage_inode.to_string(),
                ],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "marker-export publish reservation changed before identity bind".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn advance_lock_marker_export_publish(
        &self,
        publish: &LockMarkerExportPublish,
        from: &str,
        to: &str,
    ) -> Result<()> {
        let valid = matches!(
            (from, to),
            ("created", "prepared") | ("prepared", "swapped")
        );
        if !valid {
            return Err(AppError::Storage(
                "invalid marker-export publish state transition".into(),
            ));
        }
        let changed = self
            .lock()
            .execute(
                "UPDATE lock_marker_export_publish SET state = ?4
                  WHERE exported_path = ?1 AND stage_name = ?2 AND state = ?3",
                rusqlite::params![publish.exported_path, publish.stage_name, from, to],
            )
            .map_err(map_err)?;
        if changed != 1 {
            return Err(AppError::Storage(
                "marker-export publish state changed unexpectedly".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn clear_lock_marker_export_publish(
        &self,
        publish: &LockMarkerExportPublish,
    ) -> Result<()> {
        self.lock()
            .execute(
                "DELETE FROM lock_marker_export_publish
                  WHERE exported_path = ?1 AND stage_name = ?2",
                rusqlite::params![publish.exported_path, publish.stage_name],
            )
            .map_err(map_err)?;
        Ok(())
    }

    /// Acknowledge one exact-path group only after the filesystem write is durable. The optional
    /// hash is stamped in the SAME SQLCipher transaction as the row deletion, and is supplied only
    /// when the resulting file equals the canonical DB body byte-for-byte.
    pub(crate) fn ack_lock_marker_export_cleanup(
        &self,
        rows: &[LockMarkerExportCleanup],
        final_body: Option<&str>,
        canonical_hash: Option<&str>,
    ) -> Result<()> {
        let Some(first) = rows.first() else {
            return Ok(());
        };
        if rows.iter().any(|row| {
            row.source_kind != first.source_kind
                || row.source_id != first.source_id
                || row.provider_id != first.provider_id
                || row.exported_path != first.exported_path
        }) {
            return Err(AppError::Storage(
                "ambiguous marker-export cleanup path ownership".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        if let (Some(body), Some(hash)) = (final_body, canonical_hash) {
            let sealed_body_witness = rows
                .iter()
                .all(|row| row.expected_hash.as_deref() == Some(hash))
                as i64;
            let changed = match first.source_kind.as_str() {
                "meeting" => {
                    tx.execute(
                        "UPDATE notes SET exported_hash = ?4
                           WHERE meeting_id = ?1 AND provider_id = ?2 AND exported_path = ?3
                             AND (markdown = ?5 OR
                               (markdown = '' AND content_blob IS NOT NULL AND ?6 = 1))",
                        rusqlite::params![
                            first.source_id,
                            first.provider_id,
                            first.exported_path,
                            hash,
                            body,
                            sealed_body_witness,
                        ],
                    )
                    .map_err(map_err)?
                }
                "note" => {
                    tx.execute(
                        "UPDATE documents SET exported_hash = ?3
                           WHERE id = ?1 AND kind = 'note' AND exported_path = ?2
                             AND (text = ?4 OR
                               (text = '' AND text_blob IS NOT NULL AND ?5 = 1))",
                        rusqlite::params![
                            first.source_id,
                            first.exported_path,
                            hash,
                            body,
                            sealed_body_witness,
                        ],
                    )
                    .map_err(map_err)?
                }
                _ => {
                    return Err(AppError::Storage(
                        "invalid marker-export cleanup source kind".into(),
                    ));
                }
            };
            // A deleted source can legitimately leave this crash-recovery row behind: the exact
            // vault file has just been durably scrubbed, but there is no canonical row whose
            // `exported_hash` remains to be stamped. Close that orphaned journal. If the source
            // still exists, however, a zero-row UPDATE means its path/body witness drifted and we
            // must retain the journal fail-closed rather than bless unrelated bytes.
            let source_exists = match first.source_kind.as_str() {
                "meeting" => tx
                    .query_row(
                        "SELECT EXISTS(
                           SELECT 1 FROM notes WHERE meeting_id = ?1 AND provider_id = ?2
                         )",
                        rusqlite::params![first.source_id, first.provider_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(map_err)?,
                "note" => tx
                    .query_row(
                        "SELECT EXISTS(
                           SELECT 1 FROM documents WHERE id = ?1 AND kind = 'note'
                         )",
                        rusqlite::params![first.source_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(map_err)?,
                _ => unreachable!("source kind was validated above"),
            };
            if changed != 1 && source_exists != 0 {
                return Err(AppError::Storage(
                    "marker-export cleanup no longer matches its canonical source".into(),
                ));
            }
        }
        for row in rows {
            tx.execute(
                "DELETE FROM lock_marker_export_cleanup
                  WHERE source_kind = ?1 AND source_id = ?2 AND provider_id = ?3
                    AND exported_path = ?4 AND sealed_title = ?5",
                rusqlite::params![
                    row.source_kind,
                    row.source_id,
                    row.provider_id,
                    row.exported_path,
                    row.sealed_title,
                ],
            )
            .map_err(map_err)?;
        }
        tx.commit().map_err(map_err)
    }

    /// Brain-v3 audit Fix 4 (INVERSE) — on unlock, RE-MATERIALIZE the `[[Title]]` markers that the seal
    /// stripped ([`Self::strip_sealed_neighbour_markers`]), from the PRESERVED ACCEPTED edges (Fix 1)
    /// incident on the just-unlocked folder's items. For each accepted `semantic` edge whose one
    /// endpoint is a now-visible item in this folder, re-add THAT item's `[[title]]` into the OTHER
    /// endpoint's managed block WHEN the other endpoint is a VISIBLE, locally-owned note/meeting-note.
    /// Reuses [`crate::enrich::extract_link_hits`] + `apply_link_markers` (merge-preserving), so an
    /// existing block + its other hits survive. Returns the `(is_meeting, source_id)` of every source
    /// whose body changed, so the COMMAND layer re-exports its `.md`. Only ACCEPTED edges are walked —
    /// a wikilink/manual marker re-materializes naturally on the source's own re-index/re-save, but an
    /// accepted SEMANTIC marker has no body wikilink to re-derive from, so it needs this explicit
    /// re-add. `meeting_ids` are the folder's meetings; `unlocked` is the post-unlock session set. Skips
    /// any endpoint that is still sealed-at-rest (Fix-0 discipline). IDs/counts only in logs.
    pub fn rematerialize_accepted_markers_for_folder(
        &self,
        folder_id: &str,
        _meeting_ids: &[String],
        _unlocked: &HashSet<String>,
    ) -> Result<Vec<(bool, String)>> {
        // NO-OP (block-drop, fix/drop-links-block): the machine `> [!related]- Related notes`
        // (`murmur:links`) block that this used to RE-ADD into a source note's body on unlock was
        // RETIRED (it went stale + rendered as raw junk in the plain-text editor; the RELATED panel
        // reads the live `links` table instead). Re-materializing it here reborn the retired block in
        // BOTH the stored `notes.markdown`/`documents.text` AND the re-exported vault `.md` on every
        // sealed→unlock of a folder holding an accepted-semantic note — so this rematerialization must
        // stop with the block itself.
        //
        // This ONLY dropped the machine MARKER re-add; it is DISTINCT from — and never on the path of
        // — the actual content-unseal (note text / transcript / audio decrypt happens in the
        // `unlock_folder` command BEFORE this best-effort post-unlock link leg runs). The accepted
        // `semantic` link ROWS are preserved untouched (Brain-v3 Fix 1) and still surface in the
        // Related panel; only the body-block echo is gone. Returning an empty change set means the
        // command layer re-exports nothing for markers — correct, since no body changed.
        let _ = folder_id;
        Ok(Vec::new())
    }

    /// NO-OP (block-drop, fix/drop-links-block): this used to RE-ADD one `[[title]]` marker into an
    /// OWNER note's managed `> [!related]- Related notes` (`murmur:links`) block. That block is
    /// RETIRED (see [`Self::rematerialize_accepted_markers_for_folder`]), so re-adding a marker would
    /// resurrect it in the stored body + vault `.md`. Now a no-op returning `None` (no body change →
    /// caller re-exports nothing). Kept as a guarded stub so no future caller can re-introduce the
    /// block through this seam; the accepted link ROWS are untouched and still surface in the Related
    /// panel. Does NOT touch content seal/unseal.
    #[allow(dead_code)]
    fn readd_marker_to_owner(
        &self,
        _owner_kind: crate::links::LinkKind,
        _owner_id: &str,
        _title: &str,
    ) -> Result<Option<bool>> {
        Ok(None)
    }

    /// PURGE every DERIVED link row whose SRC OR DST is a sealed/deleted meeting or document (a note id
    /// IS a `documents` id, so `document_ids` covers the `note` kind too), PRESERVING a user's decision
    /// rows ([`LINK_DECISION_KEEP`] — dismissed tombstones, accepted edges, manual links). Runs INSIDE an existing
    /// seal / delete tx so a derived link — which reveals a neighbour's existence + title — never
    /// outlives the plaintext it was derived from. BOTH endpoint kinds for a meeting id are matched
    /// (`src`/`dst`); likewise for a document/note id across `document`/`note` kinds. Re-derived on
    /// unlock. Mirrors the `purge_chunks_tx` / `purge_doc_chunks_tx` choke-point idiom.
    ///
    /// `preserve_decisions`: a SEAL (reversible — the item returns on unlock) passes `true` so a
    /// user's dismissed/accepted/manual DECISION rows survive ([`LINK_DECISION_KEEP`]). A permanent
    /// DELETE (`delete_meeting`/`delete_document`/the delete-folder derived-doc leg) passes `false` —
    /// the endpoint is gone for good, so leaving a dangling decision row (that a future id reuse could
    /// resurface) would be a bug; purge EVERYTHING incident on it.
    pub(crate) fn purge_links_tx(
        tx: &rusqlite::Transaction<'_>,
        meeting_ids: &[String],
        document_ids: &[String],
        preserve_decisions: bool,
    ) -> Result<()> {
        // On a permanent delete, keep NOTHING (`NOT (1=0)` ⇒ always delete). On a seal, keep the
        // decision rows. Building the predicate ONCE keeps the two DELETEs identical modulo scope.
        let keep = if preserve_decisions {
            Self::LINK_DECISION_KEEP
        } else {
            "1 = 0"
        };
        for mid in meeting_ids {
            tx.execute(
                &format!(
                    "DELETE FROM links
                       WHERE ((src_kind = 'meeting' AND src_id = ?1)
                              OR (dst_kind = 'meeting' AND dst_id = ?1))
                         AND NOT ({keep})"
                ),
                rusqlite::params![mid],
            )
            .map_err(map_err)?;
        }
        for did in document_ids {
            // A note and a document share the `documents` id space — purge BOTH kinds for the id.
            tx.execute(
                &format!(
                    "DELETE FROM links
                       WHERE ((src_kind IN ('note','document') AND src_id = ?1)
                              OR (dst_kind IN ('note','document') AND dst_id = ?1))
                         AND NOT ({keep})"
                ),
                rusqlite::params![did],
            )
            .map_err(map_err)?;
        }
        Ok(())
    }

    /// SUGGEST-scope purge: a link's SOURCE re-runs its own auto pass, so drop this source's PRIOR
    /// `semantic` SUGGESTIONS (never its accepted/active or dismissed rows) before re-suggesting —
    /// keeps the suggestion set fresh without churning a user's decisions. Undirected semantic edges
    /// are stored canonicalized, so a source may be either endpoint: match both. Runs inside a tx.
    fn clear_semantic_suggestions_tx(
        tx: &rusqlite::Transaction<'_>,
        kind: &str,
        id: &str,
    ) -> Result<()> {
        tx.execute(
            "DELETE FROM links
               WHERE edge_type = 'semantic' AND status = 'suggested'
                 AND ((src_kind = ?1 AND src_id = ?2) OR (dst_kind = ?1 AND dst_id = ?2))",
            rusqlite::params![kind, id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// PER-NODE cap on INBOUND suggestions (brain-v3 audit Fix 2): the spec caps at
    /// `SEMANTIC_LINK_CAP` semantic edges per NODE, but the source-side pass only ever caps the
    /// SOURCE's own fan-out — a hub (a weekly-standup series) still accretes dozens of suggested chips
    /// from OTHER items' passes. This trims a node's SUGGESTED-SEMANTIC edges back down to
    /// `SEMANTIC_LINK_CAP`, deleting the WEAKEST (lowest `score`; ties broken by `id` DESC so the drop
    /// is deterministic) — run for BOTH endpoints right after each upsert so the newly-added 6th edge
    /// trims the node's weakest. SCOPED HARD: only `status='suggested' AND edge_type='semantic'` rows
    /// are ever eligible (an `active`/`accepted`/`manual`/`wikilink`/`companion` edge, or a `dismissed`
    /// tombstone, is NEVER touched). Runs inside the caller's tx.
    pub(crate) fn trim_node_semantic_suggestions_tx(
        tx: &rusqlite::Transaction<'_>,
        kind: &str,
        id: &str,
    ) -> Result<()> {
        // Keep the top-CAP suggested-semantic edges incident on (kind, id) by score DESC (id DESC as a
        // deterministic tiebreak); DELETE the rest. The subselect ranks only this node's suggestions,
        // so a shared edge counts against both its endpoints' caps independently (correct — the spec is
        // a per-node budget). LIMIT -1 OFFSET CAP yields "everything past the top CAP".
        tx.execute(
            "DELETE FROM links
               WHERE id IN (
                   SELECT id FROM links
                    WHERE edge_type = 'semantic' AND status = 'suggested'
                      AND ((src_kind = ?1 AND src_id = ?2) OR (dst_kind = ?1 AND dst_id = ?2))
                    ORDER BY score DESC, id DESC
                    LIMIT -1 OFFSET ?3
               )",
            rusqlite::params![kind, id, crate::links::SEMANTIC_LINK_CAP as i64],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// The centroid (L2-normalized mean of a `vec0` table's per-chunk vectors) for ONE item. Reuses
    /// [`Self::related_meetings_visible`]'s centroid math but reads the STORED vectors directly (no
    /// re-embed) from `vec_chunks` (meeting/note note_chunks) or `doc_vec_chunks` (documents/notes).
    /// `None` (skip — never an error) when the item has no vectors or a degenerate all-zero centroid.
    fn item_centroid(&self, kind: crate::links::LinkKind, id: &str) -> Result<Option<Vec<f32>>> {
        let conn = self.lock();
        // Which vector source table + join predicate this kind reads.
        let sql = match kind {
            crate::links::LinkKind::Meeting => {
                "SELECT v.embedding FROM vec_chunks v
                   JOIN note_chunks nc ON nc.id = v.chunk_id
                  WHERE nc.meeting_id = ?1"
            }
            crate::links::LinkKind::Note | crate::links::LinkKind::Document => {
                "SELECT v.embedding FROM doc_vec_chunks v
                   JOIN doc_chunks dc ON dc.id = v.chunk_id
                  WHERE dc.document_id = ?1"
            }
            crate::links::LinkKind::Org => return Ok(None),
        };
        let mut stmt = conn.prepare(sql).map_err(map_err)?;
        let rows = stmt
            .query_map(rusqlite::params![id], |r| r.get::<_, Vec<u8>>(0))
            .map_err(map_err)?;
        let dim = crate::embed::EMBED_DIM;
        let mut centroid = vec![0f32; dim];
        let mut counted = 0usize;
        for r in rows {
            let blob = r.map_err(map_err)?;
            let v = crate::embed::blob_to_vec(&blob);
            if v.len() != dim {
                continue; // defensive: skip a malformed stored vector.
            }
            for (acc, x) in centroid.iter_mut().zip(v.iter()) {
                *acc += *x;
            }
            counted += 1;
        }
        if counted == 0 {
            return Ok(None);
        }
        for x in centroid.iter_mut() {
            *x /= counted as f32;
        }
        let norm = centroid.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm == 0.0 {
            return Ok(None);
        }
        for x in centroid.iter_mut() {
            *x /= norm;
        }
        Ok(Some(centroid))
    }

    /// CALIBRATION read (brain-v3 PR-10 semantic-link threshold gate): the L2-normalized centroids of a
    /// sample of VISIBLE, EMBEDDED items (meeting/note/document), for building the NULL cosine
    /// distribution of RANDOM item pairs (are the shipped `SEMANTIC_LINK_FLOOR`/`_STRONG` above or below
    /// the noise floor?). Read-only, and GATED exactly like the app: items are enumerated ONLY through
    /// the same `visibility_clause`-backed predicates `list_meetings_visible` / `full_graph_content_nodes`
    /// use, so a sealed-and-not-session-unlocked item is never sampled — no new ungated read path. Each
    /// centroid comes from [`Self::item_centroid`] (an item's OWN stored vectors); items without vectors
    /// or a degenerate centroid are silently skipped. `max_items` caps the enumeration (newest-first per
    /// kind); the eval runner then samples pairs from the returned set. Returns `(kind, id, centroid)`.
    /// NO content text ever leaves this method (ids + numeric vectors only). Test-only: its sole caller
    /// is the PR-10 semantic-link calibration runner (`eval::calibration`), an `#[ignore]` manual test.
    #[cfg(test)]
    pub(crate) fn sampled_visible_item_centroids(
        &self,
        max_items: usize,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<(crate::links::LinkKind, String, Vec<f32>)>> {
        // 1. Enumerate visible ids per kind — meetings via the `list_meetings_visible` predicate,
        //    notes/documents via `full_graph_content_nodes` (both already visibility-gated). We only
        //    take ids here; the centroid read is a separate stored-vector fetch.
        let cap = max_items.max(1) as i64;
        let mut ids: Vec<(crate::links::LinkKind, String)> = Vec::new();
        for m in self.list_meetings_visible(cap, unlocked)? {
            ids.push((crate::links::LinkKind::Meeting, m.id));
        }
        for (node_kind, id, _label, _ts) in self.full_graph_content_nodes(unlocked)? {
            let kind = match node_kind {
                crate::storage::models::FullGraphNodeKind::Note => crate::links::LinkKind::Note,
                crate::storage::models::FullGraphNodeKind::Document => {
                    crate::links::LinkKind::Document
                }
                // full_graph_content_nodes only emits Note/Document; any other is not a linkable item.
                _ => continue,
            };
            ids.push((kind, id));
        }
        // 2. Centroid for each (skip items with no vectors / degenerate centroid). Capped to `max_items`
        //    total across kinds so the sample size is bounded regardless of vault shape.
        let mut out: Vec<(crate::links::LinkKind, String, Vec<f32>)> = Vec::new();
        for (kind, id) in ids.into_iter().take(max_items) {
            if let Some(centroid) = self.item_centroid(kind, &id)? {
                out.push((kind, id, centroid));
            }
        }
        Ok(out)
    }

    /// vec0 kNN of `centroid` over BOTH vector tables (`vec_chunks` ∪ `doc_vec_chunks`), rolled
    /// chunk→ITEM keeping the BEST (min) distance, converted to cosine, self dropped. Returns up to
    /// `want_items` DISTINCT NON-SELF items as `(kind, id) → cos`, best-first. GATED by
    /// `visibility_clause` on each leg's folder (a sealed neighbour is invisible — defense-in-depth on
    /// top of the seal-time chunk purge).
    ///
    /// COST (honest): sqlite-vec `vec0` is BRUTE-FORCE (no ANN index), so each MATCH is O(k·n) over the
    /// whole vec-table — NOT the "O(k·log n), no corpus scan" the earlier comment claimed. A larger `k`
    /// is therefore nearly free (the scan is the same size regardless of k), which is what makes the
    /// item-granular fan-out below cheap.
    ///
    /// ITEM-GRANULAR fan-out (brain-v3 audit Fix 1 — the crux): vec0 returns the top-`k` CHUNKS, not
    /// items. An item's OWN chunks are the nearest to its own centroid, so a probe with a fixed chunk-`k`
    /// (the old `k = SEMANTIC_LINK_K + 1`) is entirely consumed by ≥11-chunk items (every long
    /// note/document — exactly what linking is for) and returns few or ZERO distinct NON-SELF items. Two
    /// mitigations here: (a) the source item's OWN chunks are EXCLUDED inside each CTE (they can never
    /// consume the k budget), and (b) the chunk-`k` is ESCALATED (doubled from a base) until at least
    /// `want_items` distinct non-self items are collected OR `k` reaches the table's total row count
    /// (exhaustion — nothing more to fetch). Deterministic: identical vectors → identical result set +
    /// order regardless of the escalation path (the final GROUP BY + ORDER BY is over the union of hits).
    fn knn_items_visible(
        &self,
        centroid: &[f32],
        want_items: i64,
        self_kind: crate::links::LinkKind,
        self_id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<(crate::links::LinkKind, String, f32)>> {
        if centroid.is_empty() || want_items <= 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let vis_meeting = meeting_visibility_clause("m", unlocked);
        let vis_doc = visibility_clause("f", unlocked);
        // The source item's OWN chunk ids, per vec-table, so they never consume the vec0 k budget.
        // A meeting's chunks live in `note_chunks` (→ vec_chunks); a note/document's in `doc_chunks`
        // (→ doc_vec_chunks). The other table's exclusion clause is a no-op (empty set) for that kind.
        let (self_note_pred, self_doc_pred) = match self_kind {
            crate::links::LinkKind::Meeting => (
                "AND kn.chunk_id NOT IN (SELECT id FROM note_chunks WHERE meeting_id = ?3)"
                    .to_string(),
                String::new(),
            ),
            crate::links::LinkKind::Note | crate::links::LinkKind::Document => (
                String::new(),
                "AND kd.chunk_id NOT IN (SELECT id FROM doc_chunks WHERE document_id = ?3)"
                    .to_string(),
            ),
            crate::links::LinkKind::Org => return Ok(Vec::new()),
        };
        // Each vec0 table gets its own single-MATCH CTE (vec0 allows one MATCH+k per query); the
        // meeting leg maps chunk→meeting and gates on the meeting's note folder; the doc leg maps
        // chunk→document and gates on the document's folder, tagging note vs document by kind. The
        // self-chunk exclusion is applied AFTER the MATCH (vec0 wants a bare MATCH+k), so we fetch a
        // few extra chunks and drop the source's own — hence the escalation below covers the shortfall.
        let sql = format!(
            "WITH knn_note(chunk_id, distance) AS (
                 SELECT chunk_id, distance FROM vec_chunks WHERE embedding MATCH ?1 AND k = ?2
             ),
             knn_doc(chunk_id, distance) AS (
                 SELECT chunk_id, distance FROM doc_vec_chunks WHERE embedding MATCH ?1 AND k = ?2
             ),
             hits(kind, id, distance) AS (
                 SELECT 'meeting', nc.meeting_id, kn.distance
                   FROM knn_note kn JOIN note_chunks nc ON nc.id = kn.chunk_id
                   JOIN meetings m ON m.id = nc.meeting_id
                   WHERE {vis_meeting}
                   {self_note_pred}
                 UNION ALL
                 SELECT CASE WHEN d.kind = 'note' THEN 'note' ELSE 'document' END, d.id, kd.distance
                   FROM knn_doc kd JOIN doc_chunks dc ON dc.id = kd.chunk_id
                   JOIN documents d ON d.id = dc.document_id
                   JOIN folders f ON f.id = d.folder_id
                   WHERE d.kind IN ('note','document') AND {vis_doc}
                   {self_doc_pred}
             ),
             best(kind, id, distance) AS (
                 SELECT kind, id, MIN(distance) FROM hits GROUP BY kind, id
             )
             SELECT kind, id, distance FROM best ORDER BY distance ASC, kind ASC, id ASC"
        );
        let blob = crate::embed::vec_to_blob(centroid);
        // The total vec-row count bounds the escalation: once k covers every stored chunk in both
        // tables, a larger k yields nothing new (exhaustion), so we stop. Cheap single COUNT.
        let total_rows: i64 = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM vec_chunks) + (SELECT COUNT(*) FROM doc_vec_chunks)",
                [],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        let mut stmt = conn.prepare(&sql).map_err(map_err)?;
        // Escalate the chunk-k until we have `want_items` distinct non-self items OR k ≥ total rows.
        // Base is a small multiple of the target so a first probe usually suffices; doubling bounds the
        // number of re-scans to O(log(total_rows / want_items)).
        let mut k = (want_items.saturating_mul(4)).max(16);
        let out = loop {
            let rows = stmt
                .query_map(rusqlite::params![blob, k, self_id], |r| {
                    let kind: String = r.get(0)?;
                    let id: String = r.get(1)?;
                    let d: f64 = r.get(2)?;
                    Ok((kind, id, d as f32))
                })
                .map_err(map_err)?;
            let mut items: Vec<(crate::links::LinkKind, String, f32)> = Vec::new();
            for r in rows {
                let (kind_s, id, d) = r.map_err(map_err)?;
                let Some(kind) = crate::links::LinkKind::parse(&kind_s) else {
                    continue;
                };
                if kind == self_kind && id == self_id {
                    continue; // belt-and-braces: the CTE already excludes self chunks.
                }
                items.push((kind, id, crate::links::cosine_from_l2_distance(d)));
            }
            // Enough distinct items, or we've scanned every stored chunk → done.
            if (items.len() as i64) >= want_items || k >= total_rows {
                break items;
            }
            k = k.saturating_mul(2);
        };
        Ok(out)
    }

    /// SEMANTIC AUTO-LINKER (DESIGN §PR-3) — after a real-embedder index of ONE item, suggest up to
    /// `SEMANTIC_LINK_CAP` content-similar neighbours. Deterministic. Steps: source centroid →
    /// item-granular kNN → for each candidate compute MUTUALITY (source ∈ candidate's OWN kNN) →
    /// `crate::links::select_semantic_candidates` (floor/strong-or-mutual/cap) → upsert each survivor as
    /// a canonicalized UNDIRECTED `semantic` `suggested` edge (score=cos).
    ///
    /// COST (honest — audit Fix 4): sqlite-vec `vec0` is BRUTE-FORCE, so this is NOT the "O(k·log n),
    /// no corpus scan" the earlier comment claimed. One pass ≈ 1 forward probe + up to `SEMANTIC_LINK_K`
    /// mutuality back-probes, EACH a full vec-table scan (`knn_items_visible` may itself re-scan a few
    /// times while escalating its chunk-k to reach K distinct items). Bounded and fine for interactive
    /// vault sizes, but linear in the corpus — not logarithmic.
    ///
    /// CENTROID SOURCE (audit Fix 7): an item's centroid is the mean of its NOTE-derived vectors only —
    /// a MEETING reads `vec_chunks` (the AI NOTE's chunks), a note/document reads `doc_vec_chunks`. So
    /// meeting↔meeting linking rides the AI note's WORDING, never the raw transcript segments (which are
    /// never vectorized). This is intentional (the note is the curated summary) but means a link is only
    /// as good as the note text, not the full spoken content.
    ///
    /// GATED end to end: `knn_items_visible` applies `visibility_clause` on every leg, so a sealed
    /// neighbour is never a candidate and never surfaces the source to a sealed item. Model-gated at
    /// the CALL SITE (only invoked when `embed_model_present()` — never on stub vectors). Idempotent:
    /// this source's prior SUGGESTIONS are cleared first (never its accepted/dismissed rows).
    pub fn auto_link_semantic(
        &self,
        kind: crate::links::LinkKind,
        id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<usize> {
        use crate::links::{select_semantic_candidates, SemanticCandidate, SEMANTIC_LINK_K};
        // 1. Source centroid from its STORED vectors (skip silently if none / degenerate).
        let Some(centroid) = self.item_centroid(kind, id)? else {
            return Ok(0);
        };
        // 2. Source kNN — ask for SEMANTIC_LINK_K distinct NON-SELF ITEMS (self chunks are excluded
        //    inside `knn_items_visible`, which escalates its chunk-k until it has that many items).
        let neighbours = self.knn_items_visible(&centroid, SEMANTIC_LINK_K, kind, id, unlocked)?;
        // 3. Mutuality: for each candidate, back-probe ITS kNN and check the source appears in the
        //    candidate's OWN top-K distinct items. `knn_items_visible` already returns DISTINCT items,
        //    so a candidate never repeats within this pass — no back-probe dedup cache is needed.
        let mut scored: Vec<SemanticCandidate> = Vec::new();
        for (nk, nid, cos) in neighbours.into_iter() {
            let mutual = match self.item_centroid(nk, &nid)? {
                Some(nc) => {
                    let back = self.knn_items_visible(&nc, SEMANTIC_LINK_K, nk, &nid, unlocked)?;
                    back.into_iter().any(|(bk, bid, _)| bk == kind && bid == id)
                }
                None => false,
            };
            scored.push(SemanticCandidate {
                kind: nk,
                id: nid,
                cos,
                mutual,
            });
        }
        // 4. Pure selection (floor / strong-or-mutual / rank / cap).
        let kept = select_semantic_candidates(&scored);
        // 5. Upsert each survivor as a canonicalized UNDIRECTED semantic suggestion.
        let now = chrono::Utc::now().timestamp_millis();
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(map_err)?;
        // Fix 0 (brain-v3 audit) — IN-TX sealed-at-rest SOURCE re-check (TOCTOU): the kNN + mutuality
        // pass above ran OUTSIDE this tx against a possibly-stale `unlocked` snapshot. If a
        // `lock_folder` sealed this source in the meantime, a suggestion re-write would land behind
        // the lock. Refuse silently (rollback via drop) — re-derived on unlock. (The clear below must
        // NOT run either, else a concurrent seal + this pass would strip the source's suggestions
        // that `purge_links_tx` is about to purge anyway — a no-op, but the refusal keeps this pass a
        // clean no-write.)
        if link_endpoint_sealed_at_rest_tx(&tx, kind, id)? {
            tracing::debug!(target: "links", kind = kind.as_str(), "semantic auto-link refused: source sealed at rest");
            return Ok(0);
        }
        // Refresh: drop this source's stale suggestions (keeps accepted/dismissed) before re-adding.
        Self::clear_semantic_suggestions_tx(&tx, kind.as_str(), id)?;
        let mut written = 0usize;
        for c in &kept {
            // Fix 0 (brain-v3 audit) — IN-TX sealed-at-rest NEIGHBOUR re-check (TOCTOU): drop any
            // candidate whose endpoint sealed at rest since the OUTSIDE-tx kNN, so a suggestion never
            // names a now-sealed neighbour. `knn_items_visible` already gates on the (stale) snapshot;
            // this closes the race window. Re-suggested when that folder unlocks.
            if link_endpoint_sealed_at_rest_tx(&tx, c.kind, &c.id)? {
                continue;
            }
            let (src, dst) = crate::links::canonicalize_endpoints(
                (kind, id.to_string()),
                (c.kind, c.id.clone()),
            );
            Self::upsert_link_tx(
                &tx,
                src.0.as_str(),
                &src.1,
                dst.0.as_str(),
                &dst.1,
                "semantic",
                c.cos as f64,
                "auto",
                "suggested",
                now,
            )?;
            // Enforce the per-node cap on the NEIGHBOUR endpoint too (audit Fix 2): a hub that this
            // pass just suggested a 6th+ edge to has its weakest suggested-semantic edge trimmed.
            Self::trim_node_semantic_suggestions_tx(&tx, dst.0.as_str(), &dst.1)?;
            Self::trim_node_semantic_suggestions_tx(&tx, src.0.as_str(), &src.1)?;
            written += 1;
        }
        tx.commit().map_err(map_err)?;
        tracing::debug!(
            target: "links",
            kind = kind.as_str(),
            suggested = written,
            "auto_link_semantic"
        );
        Ok(written)
    }

    /// Read ONE link row by id (for accept/dismiss). Returns `(src_kind, src_id, dst_kind, dst_id,
    /// edge_type, status)`. `None` for an unknown id.
    #[allow(clippy::type_complexity)]
    pub fn link_by_id(
        &self,
        link_id: i64,
    ) -> Result<Option<(String, String, String, String, String, String)>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT src_kind, src_id, dst_kind, dst_id, edge_type, status FROM links WHERE id = ?1",
            rusqlite::params![link_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
        )
        .optional()
        .map_err(map_err)
    }

    /// ACCEPT a suggested semantic link: flip `status='active'`, `created_by='accepted'`. Idempotent
    /// (re-accepting is a no-op). The command layer materializes the `[[Title]]` into the source `.md`
    /// AFTER this — never here (a DB write must not touch the vault).
    pub fn accept_link(&self, link_id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE links SET status = 'active', created_by = 'accepted' WHERE id = ?1",
            rusqlite::params![link_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// DISMISS a suggested link: TOMBSTONE it (`status='dismissed'`) so a later auto pass never
    /// re-suggests it (the `upsert_link_tx` DO-UPDATE guard skips dismissed rows). Idempotent.
    pub fn dismiss_link(&self, link_id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE links SET status = 'dismissed' WHERE id = ?1",
            rusqlite::params![link_id],
        )
        .map_err(map_err)?;
        Ok(())
    }

    /// BOTH-ENDPOINT-GATED reader (DESIGN §PR-3, the `backlinks_for_visible` two-gate template):
    /// every non-dismissed link edge incident on `(kind, id)`, with the OTHER endpoint's current
    /// title resolved through the SAME visibility gate. Two hard gates, fail-closed:
    ///
    /// 1. **QUERIED-ITEM GATE (first).** If `(kind, id)` is itself sealed-and-not-session-unlocked
    ///    (its title does not resolve through the gate), return `Ok(vec![])` BEFORE any edge is read —
    ///    so a locked item never reveals that it HAS links (no existence leak).
    /// 2. **NEIGHBOUR GATE.** For each incident edge, resolve the OTHER endpoint's title through the
    ///    gate; an edge whose neighbour is sealed-not-unlocked is DROPPED (its title/existence never
    ///    leaks). Only edges with BOTH endpoints visible are returned.
    ///
    /// Ordering: active-then-suggested (deterministic wikilink/companion first), then score DESC,
    /// then id — stable for the FE. Logs ids/counts only, never titles.
    pub fn links_for_visible(
        &self,
        kind: crate::links::LinkKind,
        id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Vec<crate::storage::models::LinkEdge>> {
        // ── GATE 1: the queried item must itself be visible. ──
        if self
            .link_endpoint_title_visible(kind, id, unlocked)?
            .is_none()
        {
            return Ok(Vec::new());
        }
        // Read every incident, non-dismissed edge (either endpoint). Direction tells the FE which
        // side the queried item sits on. No content columns — ids/kinds/metadata only.
        let rows: Vec<LinkRowRaw> = {
            let conn = self.lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, src_kind, src_id, dst_kind, dst_id, edge_type, created_by, status,
                            score, created_at
                       FROM links
                      WHERE status != 'dismissed'
                        AND ((src_kind = ?1 AND src_id = ?2) OR (dst_kind = ?1 AND dst_id = ?2))
                      ORDER BY (status = 'active') DESC, score DESC, id ASC",
                )
                .map_err(map_err)?;
            let mapped = stmt
                .query_map(rusqlite::params![kind.as_str(), id], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, f64>(8)?,
                        r.get::<_, i64>(9)?,
                    ))
                })
                .map_err(map_err)?;
            let mut out = Vec::new();
            for r in mapped {
                out.push(r.map_err(map_err)?);
            }
            out
        };
        let mut edges: Vec<crate::storage::models::LinkEdge> = Vec::new();
        for (lid, sk, si, dk, di, et, cb, st, score, created_at) in rows {
            let manual_edges = if et == "manual" {
                vec![crate::storage::models::ManualLinkEdge {
                    src_kind: sk.clone(),
                    src_id: si.clone(),
                    dst_kind: dk.clone(),
                    dst_id: di.clone(),
                }]
            } else {
                Vec::new()
            };
            // Identify the OTHER endpoint (the one that is NOT the queried item) + the direction.
            let (direction, other_kind_s, other_id) = if sk == kind.as_str() && si == id {
                ("out", dk, di)
            } else {
                ("in", sk, si)
            };
            let Some(other_kind) = crate::links::LinkKind::parse(&other_kind_s) else {
                continue; // corrupt kind → skip defensively.
            };
            // ── GATE 2: the neighbour must be visible; else drop the edge (no title/existence leak). ──
            let Some(other_title) =
                self.link_endpoint_title_visible(other_kind, &other_id, unlocked)?
            else {
                continue;
            };
            let navigation_id = if other_kind == crate::links::LinkKind::Org {
                self.org_link_target_visible(&other_id)?
                    .map(|(item_id, _title)| item_id)
            } else {
                None
            };
            edges.push(crate::storage::models::LinkEdge {
                id: lid,
                direction: direction.to_string(),
                other_kind: other_kind_s,
                other_id,
                navigation_id,
                other_title,
                edge_type: et,
                created_by: cb,
                status: st,
                score,
                created_at,
                // Set on the base build; the collapse pass below flips it true per (other_kind,
                // other_id) pair that carries a `manual` edge.
                manual: !manual_edges.is_empty(),
                manual_edges,
            });
        }
        let edges = Self::collapse_manual_duplicate_edges(edges);
        // COMPANION COLLAPSE (read-time): fold a meeting + its auto-linked companion note into ONE
        // chip (prefer the meeting, the canonical entity). Pure in-memory transform over the
        // already-both-endpoint-gated `edges` — no `links` row is touched; the only DB read it adds
        // is a metadata-only (id + meeting_id) lookup on note endpoints ALREADY in the visible set.
        let edges = self.collapse_meeting_companion_note_edges(edges)?;
        tracing::debug!(target: "links", count = edges.len(), "links_for_visible resolved");
        Ok(edges)
    }

    /// READ-TIME COMPANION COLLAPSE (2026-07-19): when the already-visibility-gated edge set for an
    /// anchor contains BOTH a `(meeting, m)` neighbour edge AND a `(note, n)` neighbour edge where `n`
    /// is `m`'s STRUCTURAL companion note (`documents.meeting_id(n) == m`), collapse them to ONE chip
    /// by DROPPING the companion-note edge and keeping the MEETING (the canonical entity). This closes
    /// the "same title shows twice — once as a Meeting chip, once as a Note chip" duplicate that arises
    /// because a companion note is auto-created with the meeting's exact title and both get linked.
    ///
    /// Lock model — this is a PURE in-memory transform (INVARIANTS, in order):
    /// 1. **No stored row is deleted or modified.** It rewrites ONLY the `Vec<LinkEdge>` about to be
    ///    returned; the `links` rows (and their accept/dismiss/unlink handles) are untouched.
    /// 2. **Never bypasses the gate.** It ONLY inspects endpoints ALREADY present in `edges` — every
    ///    one passed BOTH-endpoint gating upstream. The `documents` lookup it adds is METADATA ONLY
    ///    (`id`, `meeting_id` — neither is content) and is scoped to note-endpoint ids already in the
    ///    set, so it gates nothing new in and leaks nothing.
    /// 3. **No silent reappear / no lost removability (the removal footgun).** It collapses ONLY when
    ///    BOTH the companion-note edge AND its meeting edge are AUTO (`manual == false`, i.e. the
    ///    `companion`/`wikilink`/`semantic` derivations). If EITHER edge is `manual`, it does NOT
    ///    collapse and both chips survive — because the user deliberately created that link and MUST be
    ///    able to see and remove it, and because a collapsed manual note-edge could otherwise vanish
    ///    invisibly (or, if the surviving meeting were later removed, silently reappear as a lone chip).
    ///    A clean manual-pair collapse would need to also fold the removability of the dropped edge onto
    ///    the survivor and re-materialize it on meeting-removal — deferred; the auto-only rule is the
    ///    safe, correct behavior here.
    /// 4. **Degrades gracefully.** Companion note present but its meeting NOT a neighbour → keep the
    ///    note (no meeting to fold into). Meeting present but its companion note NOT a neighbour → keep
    ///    the meeting (nothing to drop). Two notes that merely SHARE a title but are not tied via
    ///    `documents.meeting_id` → keep both (the relation is STRUCTURAL via `meeting_id`, never a
    ///    title-string match). No companion notes in the set → the metadata query is skipped entirely.
    ///
    /// Serves BOTH anchor kinds (viewing a note or a meeting) since `links_for_visible` does, and any
    /// caller (MCP/graph) that reuses it benefits identically. Logs a count only, never titles/ids.
    fn collapse_meeting_companion_note_edges(
        &self,
        edges: Vec<crate::storage::models::LinkEdge>,
    ) -> Result<Vec<crate::storage::models::LinkEdge>> {
        // Fast exit: nothing to fold unless the set has BOTH a note edge and a meeting edge.
        let note_ids: Vec<String> = edges
            .iter()
            .filter(|e| e.other_kind == "note")
            .map(|e| e.other_id.clone())
            .collect();
        let has_meeting = edges.iter().any(|e| e.other_kind == "meeting");
        if note_ids.is_empty() || !has_meeting {
            return Ok(edges);
        }
        // Which of those note endpoints are companion notes, and of which meeting? METADATA ONLY
        // (id + meeting_id, no content) and scoped to note ids ALREADY in the visible set — this
        // gates nothing new in. Built as a placeholder IN-list so ids never interpolate into SQL.
        let placeholders = (1..=note_ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let comp_of: std::collections::HashMap<String, String> = {
            let conn = self.lock();
            let sql = format!(
                "SELECT id, meeting_id FROM documents
                   WHERE kind = 'note' AND meeting_id IS NOT NULL AND id IN ({placeholders})"
            );
            let mut stmt = conn.prepare(&sql).map_err(map_err)?;
            let params = rusqlite::params_from_iter(note_ids.iter());
            let rows = stmt
                .query_map(params, |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(map_err)?;
            let mut out = std::collections::HashMap::new();
            for r in rows {
                let (nid, mid) = r.map_err(map_err)?;
                out.insert(nid, mid);
            }
            out
        };
        if comp_of.is_empty() {
            return Ok(edges); // no companion notes in the set → nothing to fold.
        }
        // The meeting endpoints present in the set, and whether each is on a `manual` edge — a
        // meeting reached by a manual link must NOT absorb (invariant 3). One meeting could
        // theoretically appear on both a manual and an auto edge (distinct edge rows); a manual on
        // EITHER side blocks the collapse for that pair, so mark a meeting manual if ANY of its edges is.
        let mut meeting_manual: std::collections::HashMap<String, bool> =
            std::collections::HashMap::new();
        for e in &edges {
            if e.other_kind == "meeting" {
                let entry = meeting_manual.entry(e.other_id.clone()).or_insert(false);
                *entry = *entry || e.manual;
            }
        }
        let before = edges.len();
        let kept: Vec<crate::storage::models::LinkEdge> = edges
            .into_iter()
            .filter(|e| {
                // Only a NON-manual companion-note edge whose meeting is a NON-manual neighbour in the
                // set is dropped; everything else survives untouched.
                if e.other_kind != "note" || e.manual {
                    return true; // meetings, documents, manual note-edges → always keep.
                }
                let Some(mid) = comp_of.get(&e.other_id) else {
                    return true; // not a companion note → keep.
                };
                match meeting_manual.get(mid) {
                    // The companion's meeting is a neighbour AND that meeting edge is auto → fold
                    // (drop the note edge, keep the meeting).
                    Some(false) => false,
                    // Meeting is a neighbour but reached MANUALLY → keep both (removability).
                    Some(true) => true,
                    // The companion's meeting is NOT a neighbour → keep the note (nothing to fold into).
                    None => true,
                }
            })
            .collect();
        if kept.len() != before {
            tracing::debug!(
                target: "links",
                dropped = before - kept.len(),
                "collapse_meeting_companion_note_edges folded companion note(s) into their meeting"
            );
        }
        Ok(kept)
    }

    /// note↔meeting-links PR-1 — DISPLAY DEDUPE (avoid double chips): a note→meeting `manual` link
    /// ALSO materializes `[[Title]]` into the note body, so the next save adds a `wikilink` edge for
    /// the SAME `(other_kind, other_id)` pair. This collapses each such pair to ONE row so the FE
    /// renders one chip, mirroring the `backlinks_for_visible` companion-vs-wikilink dedupe idiom.
    ///
    /// Rule per `(other_kind, other_id)` group:
    /// - PREFER the DETERMINISTIC edge (`wikilink` > `companion` > `semantic`) for the surviving
    ///   row's stable `id`/`edge_type` (its id is what accept/dismiss already key on), falling back to
    ///   the `manual` row when it is the only edge for the pair.
    /// - Preserve every exact directed manual tuple in `manual_edges` (at most two: one per direction
    ///   under the table UNIQUE key), and set `manual = true` iff that list is non-empty. The FE can
    ///   therefore remove the whole hidden manual set without reconstructing direction from the
    ///   representative.
    ///
    /// Input order is already the reader's stable sort (active-then-suggested, score DESC, id ASC);
    /// this preserves the FIRST-seen group's position so the output order stays deterministic. Pairs
    /// with no `manual` edge are unchanged (`manual` stays false). Groups NEVER merge across different
    /// `(other_kind, other_id)` — only true duplicates of the same neighbour collapse.
    fn collapse_manual_duplicate_edges(
        edges: Vec<crate::storage::models::LinkEdge>,
    ) -> Vec<crate::storage::models::LinkEdge> {
        // Preference rank: lower wins as the surviving deterministic representative.
        fn edge_rank(edge_type: &str) -> u8 {
            match edge_type {
                "wikilink" => 0,
                "companion" => 1,
                // A `manual` edge is a user's ACTIVE, removable link — it MUST outrank a mere
                // `semantic` SUGGESTION so a manually-linked pair that is ALSO auto-suggested collapses
                // to the active manual chip (removable `×`), NOT an Accept/Dismiss suggestion row. It
                // still loses to the deterministic wikilink/companion edges (which are also active).
                "manual" => 2,
                // A suggested semantic edge wins the representative slot ONLY when it is the SOLE edge
                // for the pair (a genuine, unconfirmed suggestion).
                "semantic" => 3,
                _ => 4,
            }
        }
        // Preserve first-seen group order for a deterministic output.
        let mut order: Vec<(String, String)> = Vec::new();
        let mut groups: std::collections::HashMap<
            (String, String),
            crate::storage::models::LinkEdge,
        > = std::collections::HashMap::new();
        for mut edge in edges {
            let key = (edge.other_kind.clone(), edge.other_id.clone());
            match groups.get_mut(&key) {
                None => {
                    order.push(key.clone());
                    edge.manual = !edge.manual_edges.is_empty();
                    groups.insert(key, edge);
                }
                Some(rep) => {
                    let mut manual_edges = std::mem::take(&mut rep.manual_edges);
                    manual_edges.append(&mut edge.manual_edges);
                    // Promote the representative to the more-deterministic edge (lower rank wins).
                    if edge_rank(&edge.edge_type) < edge_rank(&rep.edge_type) {
                        *rep = edge;
                    }
                    rep.manual = !manual_edges.is_empty();
                    rep.manual_edges = manual_edges;
                }
            }
        }
        order
            .into_iter()
            .filter_map(|k| groups.remove(&k))
            .collect()
    }

    /// Resolve a link endpoint's CURRENT display title through the visibility gate; `None` iff the
    /// endpoint is unknown OR sealed-and-not-session-unlocked (indistinguishable — no existence leak).
    /// Meeting → the gated `meetings.title`/`meeting_is_visible`; note/document → the gated
    /// `documents.title`/`name` under `visibility_clause`. The single title source for BOTH gates in
    /// [`Self::links_for_visible`], and for the accept command's neighbour-title resolve.
    pub fn link_endpoint_title_visible(
        &self,
        kind: crate::links::LinkKind,
        id: &str,
        unlocked: &HashSet<String>,
    ) -> Result<Option<String>> {
        match kind {
            crate::links::LinkKind::Meeting => {
                // One gated SQL read: never authorize and then hydrate the title in a second
                // connection interval where a relock could land between the two operations.
                Ok(self.get_meeting_if_visible(id, unlocked)?.map(|meeting| {
                    meeting
                        .title
                        .filter(|title| !title.trim().is_empty())
                        .unwrap_or_else(|| "Meeting".to_string())
                }))
            }
            crate::links::LinkKind::Note | crate::links::LinkKind::Document => {
                let conn = self.lock();
                let visible = visibility_clause("f", unlocked);
                let expected_kind = match kind {
                    crate::links::LinkKind::Note => "note",
                    crate::links::LinkKind::Document => "document",
                    crate::links::LinkKind::Meeting | crate::links::LinkKind::Org => unreachable!(),
                };
                let sql = format!(
                    "SELECT COALESCE(NULLIF(TRIM(d.title), ''), d.name)
                       FROM documents d
                       JOIN folders f ON f.id = d.folder_id
                      WHERE d.id = ?1 AND d.kind = ?2 AND {visible}
                      LIMIT 1"
                );
                conn.query_row(&sql, rusqlite::params![id, expected_kind], |r| {
                    r.get::<_, String>(0)
                })
                .optional()
                .map_err(map_err)
            }
            crate::links::LinkKind::Org => self
                .org_link_target_visible(id)
                .map(|target| target.map(|(_item_id, title)| title)),
        }
    }

    /// Resolve a strict stable Shared Brain `org_id:doc_id` composite to its current live feed item
    /// and title. The SQL join is the org read gate: membership must still exist locally, context
    /// must be enabled, and at least one non-tombstoned current replica revision must exist.
    /// Unknown/revoked/left/disabled are all indistinguishable `None` so a private edge never becomes
    /// an existence or title oracle.
    pub fn org_link_target_visible(&self, link_id: &str) -> Result<Option<(String, String)>> {
        let Some((org_id, doc_id)) = parse_org_link_id(link_id) else {
            return Ok(None);
        };
        let conn = self.lock();
        conn.query_row(
            "SELECT oi.item_id, oi.title
               FROM org_items oi
               JOIN org_state os ON os.org_id = oi.org_id
              WHERE oi.org_id = ?1 AND oi.doc_id = ?2
                AND oi.tombstoned = 0 AND oi.is_current = 1 AND os.context_enabled = 1
                AND COALESCE(oi.source_kind, '') NOT IN ('task','container')
              LIMIT 1",
            rusqlite::params![org_id, doc_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(map_err)
    }

    /// Translate one current feed revision id to its stable link identity. This is used only while
    /// building the already-gated org candidate page; legacy rows without a `doc_id` are deliberately
    /// not offered because an item-id edge would break on the next revision.
    pub fn org_link_doc_id_for_item_visible(&self, item_id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        let target: Option<(String, String)> = conn
            .query_row(
                "SELECT oi.org_id, oi.doc_id
               FROM org_items oi
               JOIN org_state os ON os.org_id = oi.org_id
              WHERE oi.item_id = ?1 AND oi.tombstoned = 0 AND os.context_enabled = 1
                AND COALESCE(oi.source_kind, '') NOT IN ('task','container')
                AND oi.doc_id IS NOT NULL AND oi.is_current = 1
              LIMIT 1",
                rusqlite::params![item_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(map_err)?;
        Ok(target.and_then(|(org_id, doc_id)| org_link_id(&org_id, &doc_id)))
    }
}
