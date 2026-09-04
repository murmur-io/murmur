//! Brain v3 PR-3 — the LINK ENGINE's pure, deterministic, headless-testable core.
//!
//! The persisted `links` table + its gated readers live in [`crate::storage::db`] (SQL is
//! co-located with the schema, as every other table). THIS module owns the parts that are pure
//! functions of their inputs — no DB, no clock, no network — so they are unit-testable in isolation:
//!
//! - the [`LinkKind`] endpoint enum (`meeting|note|document|org`) + its string round-trip;
//! - the [`EdgeType`] enum (`wikilink|companion|semantic`) + directedness;
//! - endpoint CANONICALIZATION for undirected (semantic) edges (`src<dst` so a pair is stored once);
//! - the SEMANTIC AUTO-LINKER math: the mutual-kNN / floor / cap candidate selection over already-
//!   computed cosine similarities (the DB layer runs the vec0 kNN + supplies the numbers).
//!
//! Lock model note: this module holds NO content and touches NO gate — a link ROW is a
//! content-revealing derived relation, so its purge-on-seal + both-endpoint read gating are enforced
//! at the DB/command layer (see `purge_links_tx` / `links_for_visible`). Nothing here can leak.

use serde::{Deserialize, Serialize};

/// The three thresholds + fan-out constants of the semantic auto-linker (DESIGN §PR-3). These are
/// e5-cosine START VALUES needing a real-vault calibration spike (hand-label precision@5; e5's
/// cosine range is compressed, so a per-corpus floor sweep is the honest calibration) — NOT proven
/// by `cargo test`, which only pins the SELECTION LOGIC (mutual-kNN / floor / cap) against synthetic
/// cosines.
///
/// - [`SEMANTIC_LINK_FLOOR`] — a candidate below this cosine is never a link, regardless of mutuality.
/// - [`SEMANTIC_LINK_STRONG`] — at/above this cosine a candidate is kept even if NOT mutual (a
///   confidently-close neighbour survives one-directional; below it, mutuality is REQUIRED, which is
///   the load-bearing defence against e5 hubness — a hub is near everything but reciprocated by few).
/// - [`SEMANTIC_LINK_K`] — the kNN fan-out per node (also the mutuality window: "is src in cand's own
///   top-K").
/// - [`SEMANTIC_LINK_CAP`] — at most this many semantic edges are suggested per node per pass.
pub const SEMANTIC_LINK_FLOOR: f32 = 0.80;
pub const SEMANTIC_LINK_STRONG: f32 = 0.88;
pub const SEMANTIC_LINK_K: i64 = 10;
pub const SEMANTIC_LINK_CAP: usize = 5;

/// Which kind of owned-content row a link ENDPOINT is. `document` and `note` are BOTH `documents`
/// rows (a note is `kind='note'`); they are distinct link kinds so the FE can route/label without a
/// second DB read, but they share the `documents` id space (so a `document_ids` purge covers both).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    Meeting,
    Note,
    Document,
    /// A private local edge to a Shared Brain document. Its local id is the revision-stable,
    /// org-scoped `org_id:doc_id` composite, never a feed `item_id` and never uploaded.
    Org,
    /// A whole LOCAL CONTAINER — a Space (`folders.level='project'`) or a folder
    /// (`folders.level='folder'`). Its id is the stable `folders.id`, which is why the relation
    /// survives a rename AND a reparent; the visible name is resolved live at read time.
    ///
    /// Space-versus-folder is METADATA (`level`), not a second relation kind: one endpoint kind
    /// keeps the write path, the purge path and every exhaustive match single-branched.
    ///
    /// A container relation is Related-panel METADATA ONLY. It names a place, never content, so it
    /// is deliberately NOT a content source ([`LinkKind::is_content_source`]): it never enters an
    /// Ask/provider scope, a summarization input, a semantic candidate set, an MCP content result,
    /// or a note body. It NEVER fans out to the container's descendants — exactly one directed edge
    /// is written.
    Container,
}

impl LinkKind {
    /// Stable lowercase string persisted in `links.src_kind`/`links.dst_kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            LinkKind::Meeting => "meeting",
            LinkKind::Note => "note",
            LinkKind::Document => "document",
            LinkKind::Org => "org",
            LinkKind::Container => "container",
        }
    }

    /// Parse a persisted/IPC kind string; `None` for anything unknown (the caller rejects it as
    /// `InvalidArg` — never silently coerces).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "meeting" => Some(LinkKind::Meeting),
            "note" => Some(LinkKind::Note),
            "document" => Some(LinkKind::Document),
            "org" => Some(LinkKind::Org),
            "container" => Some(LinkKind::Container),
            _ => None,
        }
    }

    /// Is this endpoint MATERIAL CONTENT a provider/summarizer/retriever may read?
    ///
    /// The ONE predicate every "does this endpoint contribute text" site asks, so a new
    /// metadata-only endpoint kind cannot silently widen an Ask scope, a conversion source, a
    /// semantic candidate set, or an MCP content result by being forgotten at one call site. Both
    /// `org` (a private graph relation to somebody else's Shared Brain document — public callers
    /// already excluded it one `== LinkKind::Org` at a time) and `container` (a PLACE, which holds
    /// no text of its own) answer `false`.
    pub fn is_content_source(self) -> bool {
        matches!(
            self,
            LinkKind::Meeting | LinkKind::Note | LinkKind::Document
        )
    }
}

/// The relation an edge encodes. `wikilink`/`companion`/`manual` are DIRECTED (source → target, kept
/// as written); `semantic` is UNDIRECTED (a symmetric similarity — stored ONCE with canonicalized
/// endpoints so A~B and B~A are the same row).
///
/// `manual` (note↔meeting-links PR-1) is a USER-INITIATED directed edge: the user explicitly links
/// two items from the Connections panel. Unlike `wikilink` (derived from a note's `[[Title]]` on
/// save) or `semantic` (auto-suggested), a `manual` row is written directly by the `link_items`
/// command with `created_by='user'`, `status='active'`, `score=1.0`. When the SOURCE is an owned
/// note, the command ALSO materializes a `[[Title]]` into its body — so the same pair later gains a
/// `wikilink` edge too; the display dedupe (`links_for_visible`) collapses the two to one chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeType {
    Wikilink,
    Companion,
    Semantic,
    Manual,
}

impl EdgeType {
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeType::Wikilink => "wikilink",
            EdgeType::Companion => "companion",
            EdgeType::Semantic => "semantic",
            EdgeType::Manual => "manual",
        }
    }

    /// Parse a persisted/IPC edge-type string; `None` for anything unknown (the caller rejects it).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "wikilink" => Some(EdgeType::Wikilink),
            "companion" => Some(EdgeType::Companion),
            "semantic" => Some(EdgeType::Semantic),
            "manual" => Some(EdgeType::Manual),
            _ => None,
        }
    }

    /// `true` for the undirected (semantic) edge — the ONLY edge type whose endpoints are
    /// canonicalized so the pair is stored once. Directed edges (wikilink/companion/manual) keep
    /// (src, dst) as written.
    pub fn is_undirected(self) -> bool {
        matches!(self, EdgeType::Semantic)
    }
}

/// One endpoint = `(kind, id)`. Comparable so an undirected edge can canonicalize to a stable order.
pub type Endpoint = (LinkKind, String);

/// Canonicalize the two endpoints of an UNDIRECTED (semantic) edge so the smaller endpoint is `src`
/// — the storage rule that makes A~B and B~A the SAME row (backed by the table's UNIQUE constraint).
/// Directed edges never call this (they keep their written direction). Order key is
/// `(kind_str, id)` — a total, deterministic ordering independent of insertion order.
pub fn canonicalize_endpoints(a: Endpoint, b: Endpoint) -> (Endpoint, Endpoint) {
    let key_a = (a.0.as_str(), a.1.as_str());
    let key_b = (b.0.as_str(), b.1.as_str());
    if key_a <= key_b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Convert a vec0 L2 DISTANCE (over UNIT vectors, the e5 convention) to COSINE similarity:
/// `cos = 1 − d²/2`. Clamped to `[-1, 1]` so a tiny floating-point overshoot never produces a
/// nonsense score. (DESIGN §PR-3 step 3.)
pub fn cosine_from_l2_distance(d: f32) -> f32 {
    (1.0 - d * d / 2.0).clamp(-1.0, 1.0)
}

/// One kNN candidate the DB layer surfaced for the SOURCE item: the neighbour item's `(kind, id)`,
/// the best (max) cosine over its chunks, and whether the SOURCE appears in the NEIGHBOUR's OWN
/// top-K (the mutuality flag — computed by the DB layer via a second kNN keyed on the neighbour's
/// centroid, or an in-set check).
#[derive(Debug, Clone)]
pub struct SemanticCandidate {
    pub kind: LinkKind,
    pub id: String,
    pub cos: f32,
    pub mutual: bool,
}

/// The SEMANTIC AUTO-LINKER selection math (DESIGN §PR-3 steps 4–5), a PURE function of the
/// already-scored candidates:
///
/// 1. FLOOR: drop any candidate with `cos < SEMANTIC_LINK_FLOOR` (never a link, whatever else).
/// 2. KEEP iff `mutual` OR `cos >= SEMANTIC_LINK_STRONG` — mutuality is the hubness defence below the
///    strong threshold; a confidently-close neighbour survives one-directionally.
/// 3. RANK by cosine DESC, ties broken by `(kind, id)` ASC — fully DETERMINISTIC (no dependence on the
///    kNN's row order).
/// 4. CAP at `SEMANTIC_LINK_CAP`.
///
/// Self is assumed ALREADY dropped by the DB layer (a source is trivially its own nearest hit); this
/// function does not know the source id, so it never re-adds it.
pub fn select_semantic_candidates(candidates: &[SemanticCandidate]) -> Vec<SemanticCandidate> {
    let mut kept: Vec<SemanticCandidate> = candidates
        .iter()
        .filter(|c| c.cos >= SEMANTIC_LINK_FLOOR && (c.mutual || c.cos >= SEMANTIC_LINK_STRONG))
        .cloned()
        .collect();
    // Deterministic order: strongest first, then a stable (kind, id) tiebreak.
    kept.sort_by(|a, b| {
        b.cos
            .partial_cmp(&a.cos)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.kind.as_str().cmp(b.kind.as_str()))
            .then_with(|| a.id.cmp(&b.id))
    });
    kept.truncate(SEMANTIC_LINK_CAP);
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(kind: LinkKind, id: &str, cos: f32, mutual: bool) -> SemanticCandidate {
        SemanticCandidate {
            kind,
            id: id.to_string(),
            cos,
            mutual,
        }
    }

    #[test]
    fn link_kind_and_edge_type_round_trip_strings() {
        for k in [
            LinkKind::Meeting,
            LinkKind::Note,
            LinkKind::Document,
            LinkKind::Org,
            LinkKind::Container,
        ] {
            assert_eq!(LinkKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(LinkKind::parse("container"), Some(LinkKind::Container));
        assert_eq!(LinkKind::Container.as_str(), "container");
        assert_eq!(LinkKind::parse("bogus"), None);
        assert_eq!(EdgeType::Semantic.as_str(), "semantic");
        assert!(EdgeType::Semantic.is_undirected());
        assert!(!EdgeType::Wikilink.is_undirected());
        assert!(!EdgeType::Companion.is_undirected());
    }

    /// The ONE content-source predicate: only the three MATERIAL local kinds answer `true`. A
    /// metadata-only endpoint (`org`, `container`) must never widen an Ask/provider/summarizer
    /// scope, so this is asserted rather than left to each call site to remember.
    #[test]
    fn only_material_local_kinds_are_content_sources() {
        assert!(LinkKind::Meeting.is_content_source());
        assert!(LinkKind::Note.is_content_source());
        assert!(LinkKind::Document.is_content_source());
        assert!(
            !LinkKind::Org.is_content_source(),
            "a Shared Brain relation is private graph metadata, never provider material"
        );
        assert!(
            !LinkKind::Container.is_content_source(),
            "a container names a PLACE — it holds no text and must never enter a content scope"
        );
    }

    /// Every edge type round-trips through `as_str`/`parse`, and `manual` (note↔meeting-links PR-1)
    /// is DIRECTED (never canonicalized) exactly like `wikilink`/`companion`.
    #[test]
    fn edge_type_round_trips_all_variants_and_manual_is_directed() {
        for et in [
            EdgeType::Wikilink,
            EdgeType::Companion,
            EdgeType::Semantic,
            EdgeType::Manual,
        ] {
            assert_eq!(EdgeType::parse(et.as_str()), Some(et));
        }
        assert_eq!(EdgeType::parse("manual"), Some(EdgeType::Manual));
        assert_eq!(EdgeType::Manual.as_str(), "manual");
        assert_eq!(EdgeType::parse("bogus"), None);
        // Manual is a DIRECTED user edge — endpoints are NEVER canonicalized (src/dst as written).
        assert!(!EdgeType::Manual.is_undirected());
        // Only the semantic similarity edge is undirected.
        assert!(EdgeType::Semantic.is_undirected());
    }

    #[test]
    fn canonicalize_is_order_independent_and_stable() {
        let a = (LinkKind::Meeting, "m1".to_string());
        let b = (LinkKind::Note, "n1".to_string());
        // Both argument orders canonicalize to the SAME (src, dst) pair.
        assert_eq!(
            canonicalize_endpoints(a.clone(), b.clone()),
            canonicalize_endpoints(b, a)
        );
        // "meeting" < "note" lexically, so meeting is src.
        let (src, dst) = canonicalize_endpoints(
            (LinkKind::Note, "n1".to_string()),
            (LinkKind::Meeting, "m1".to_string()),
        );
        assert_eq!(src.0, LinkKind::Meeting);
        assert_eq!(dst.0, LinkKind::Note);
    }

    #[test]
    fn cosine_from_distance_matches_the_unit_vector_identity() {
        // d = 0 (identical) → cos 1; d = sqrt(2) (orthogonal) → cos 0; d = 2 (antipodal) → cos -1.
        assert!((cosine_from_l2_distance(0.0) - 1.0).abs() < 1e-6);
        assert!(cosine_from_l2_distance(std::f32::consts::SQRT_2).abs() < 1e-6);
        assert!((cosine_from_l2_distance(2.0) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn floor_drops_low_cosine_even_when_mutual() {
        // Mutual but below the 0.80 floor → dropped.
        let out = select_semantic_candidates(&[cand(LinkKind::Note, "n1", 0.79, true)]);
        assert!(out.is_empty(), "sub-floor candidate must never be kept");
    }

    #[test]
    fn strong_cosine_kept_without_mutuality_but_weak_needs_mutual() {
        // 0.90 (>= STRONG) survives one-directional; 0.82 (< STRONG, not mutual) does NOT.
        let out = select_semantic_candidates(&[
            cand(LinkKind::Note, "strong", 0.90, false),
            cand(LinkKind::Note, "weak-solo", 0.82, false),
            cand(LinkKind::Note, "weak-mutual", 0.82, true),
        ]);
        let ids: Vec<&str> = out.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"strong"), "strong non-mutual must be kept");
        assert!(
            !ids.contains(&"weak-solo"),
            "below-strong non-mutual must be dropped (hubness defence)"
        );
        assert!(
            ids.contains(&"weak-mutual"),
            "above-floor mutual must be kept"
        );
    }

    #[test]
    fn cap_and_deterministic_tiebreak() {
        // Seven strong candidates, some cosine-tied; only SEMANTIC_LINK_CAP survive, strongest-first,
        // ties broken by (kind, id) ASC — fully deterministic regardless of input order.
        let out = select_semantic_candidates(&[
            cand(LinkKind::Note, "b", 0.90, true),
            cand(LinkKind::Note, "a", 0.90, true), // tie with "b" → "a" sorts first
            cand(LinkKind::Meeting, "m", 0.95, true),
            cand(LinkKind::Note, "c", 0.91, true),
            cand(LinkKind::Note, "d", 0.89, true),
            cand(LinkKind::Note, "e", 0.885, true),
            cand(LinkKind::Note, "f", 0.881, true),
        ]);
        assert_eq!(
            out.len(),
            SEMANTIC_LINK_CAP,
            "must cap at SEMANTIC_LINK_CAP"
        );
        assert_eq!(out[0].id, "m", "0.95 strongest first");
        assert_eq!(out[1].id, "c", "0.91 next");
        // The two 0.90 ties: "a" before "b" (kind equal → id ASC).
        assert_eq!(out[2].id, "a");
        assert_eq!(out[3].id, "b");
        assert_eq!(out[4].id, "d", "0.89 fills the last cap slot");
    }
}
