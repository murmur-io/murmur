//! RAG BAKE-OFF eval harness — the missing retrieval-quality infra for brain2's semantic layer.
//!
//! ## Why this exists
//!
//! Murmur ships THREE retrieval legs over one vault: keyword **FTS** (`Db::search_visible`),
//! on-device **semantic** vector KNN (`Db::search_semantic_visible`), and their **hybrid** RRF fusion
//! (`embed::rrf_fuse`). Which one — and which embedding model ([`crate::embed::EMBED_MODELS`]:
//! multilingual-e5-small vs mmlw-retrieval-e5-small) — actually retrieves the right meetings for a real (often
//! Polish) query can ONLY be answered empirically. This module is that measurement: given a labeled
//! set and a [`crate::embed::Embedder`], it runs all three modes and reports **recall@k**, **nDCG@k**,
//! and **MRR** per mode so a human can pick the winner on a Mac.
//!
//! ## What is / isn't testable headless
//!
//! - The METRIC MATH (this file) is pure and unit-tested with synthetic rankings — NO model, runs in
//!   `cargo test --lib`.
//! - The REAL RUN ([`bakeoff::run_bakeoff`]) needs a real `Db` vault + the embedding model on disk +
//!   Metal, so its end-to-end test is `#[ignore]`d and driven manually on a Mac (see
//!   `docs/RAG-BAKEOFF.md`). A green build proves the harness typechecks against the retrieval APIs,
//!   NOT that any model retrieves well.
//!
//! ## Gating
//!
//! The harness is READ-ONLY and routes EVERY retrieval through the SAME visibility-gated Db readers
//! the app uses (`search_visible` / `search_semantic_visible`), so a sealed-and-not-session-unlocked
//! meeting is invisible to the eval exactly as it is to the app. It never opens a raw connection,
//! never bypasses `visibility_clause`, and reads no plaintext outside those gated readers. Not
//! lock-touching: it adds no seal, no new read path, no new export.

pub mod bakeoff;
pub mod diarization;
pub mod notes_bakeoff;

use serde::{Deserialize, Serialize};

/// One labeled evaluation example: a `query` (optionally tagged with a `lang` for readability /
/// per-language slicing) and the set of `expected_meeting_ids` that a good retriever SHOULD surface.
/// Serde-friendly so a set lives in a small JSON fixture (see `fixtures/rag-bakeoff-sample.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabeledQuery {
    /// The natural-language query as a user would type it (any language; PL + EN are the priorities).
    pub query: String,
    /// A free-form language tag (`"pl"`, `"en"`, …) — informational, used only for a per-language
    /// breakdown in the printed table. `#[serde(default)]` so it may be omitted.
    #[serde(default)]
    pub lang: String,
    /// The meeting ids that count as RELEVANT for this query (the gold set). Order does not matter.
    pub expected_meeting_ids: Vec<String>,
}

/// A labeled set = a list of [`LabeledQuery`]. Thin newtype so the on-disk JSON is just an array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabeledSet(pub Vec<LabeledQuery>);

impl LabeledSet {
    /// Parse a labeled set from a JSON string (the fixture format: a top-level array of queries).
    pub fn from_json(s: &str) -> crate::error::Result<Self> {
        let queries: Vec<LabeledQuery> = serde_json::from_str(s)
            .map_err(|e| crate::error::AppError::InvalidArg(format!("parse labeled set: {e}")))?;
        Ok(LabeledSet(queries))
    }

    /// Number of labeled queries.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when the set has no queries.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Which retrieval leg a metric row describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrievalMode {
    /// Keyword FTS/BM25 only (`Db::search_visible`).
    Fts,
    /// On-device semantic vector KNN only (`Db::search_semantic_visible`).
    Semantic,
    /// RRF fusion of FTS ∪ semantic (`embed::rrf_fuse`).
    Hybrid,
}

impl RetrievalMode {
    /// Stable label for the printed table.
    pub fn label(self) -> &'static str {
        match self {
            RetrievalMode::Fts => "fts",
            RetrievalMode::Semantic => "semantic",
            RetrievalMode::Hybrid => "hybrid",
        }
    }
}

/// Averaged metrics for ONE mode over a whole labeled set. All three are in `[0, 1]`, higher = better.
#[derive(Debug, Clone, Copy)]
pub struct ModeMetrics {
    pub mode: RetrievalMode,
    /// Mean recall@k: fraction of a query's expected ids found in its top-k, averaged over queries.
    pub recall_at_k: f64,
    /// Mean nDCG@k: discounted cumulative gain (binary relevance) normalized by the ideal, averaged.
    pub ndcg_at_k: f64,
    /// Mean reciprocal rank: `1 / rank-of-first-relevant`, averaged over queries (0 when none found).
    pub mrr: f64,
    /// The `k` these metrics were computed at (the cutoff).
    pub k: usize,
    /// How many queries were averaged (the set size).
    pub queries: usize,
}

/// The full bake-off result: one [`ModeMetrics`] per mode, plus the cutoff and set size, ready to
/// print with [`format_report_table`].
#[derive(Debug, Clone)]
pub struct BakeoffReport {
    pub k: usize,
    pub queries: usize,
    pub modes: Vec<ModeMetrics>,
}

/// **recall@k** for ONE query: fraction of `expected` ids present in the first `k` of `ranked`.
/// `expected` empty ⇒ 1.0 (nothing to find is trivially fully found — a query with no gold set is
/// vacuous, not a miss). `k == 0` ⇒ 0.0 (no cutoff, no hits). Duplicates in `ranked` are ignored for
/// counting distinct expected ids. Pure — no DB, no model.
pub fn recall_at_k(ranked: &[String], expected: &[String], k: usize) -> f64 {
    if expected.is_empty() {
        return 1.0;
    }
    if k == 0 {
        return 0.0;
    }
    let topk: std::collections::HashSet<&String> = ranked.iter().take(k).collect();
    let hits = expected.iter().filter(|e| topk.contains(e)).count();
    hits as f64 / expected.len() as f64
}

/// **nDCG@k** for ONE query with BINARY relevance (an id is relevant iff it is in `expected`).
/// DCG sums `1 / log2(rank + 1)` (rank 1-based) over the first `k` ranked ids that are relevant; the
/// ideal DCG (IDCG) places `min(k, |expected|)` relevant ids first. `nDCG = DCG / IDCG`.
/// `expected` empty ⇒ 1.0 (vacuous); `k == 0` or `IDCG == 0` ⇒ 0.0. Pure.
pub fn ndcg_at_k(ranked: &[String], expected: &[String], k: usize) -> f64 {
    if expected.is_empty() {
        return 1.0;
    }
    if k == 0 {
        return 0.0;
    }
    let gold: std::collections::HashSet<&String> = expected.iter().collect();
    let mut dcg = 0.0;
    // Guard against duplicate ids in `ranked` double-counting a single relevant meeting.
    let mut counted: std::collections::HashSet<&String> = std::collections::HashSet::new();
    for (rank0, id) in ranked.iter().take(k).enumerate() {
        if gold.contains(id) && counted.insert(id) {
            dcg += 1.0 / ((rank0 as f64 + 2.0).log2()); // log2(rank+1), rank = rank0+1.
        }
    }
    let ideal_hits = expected.len().min(k);
    let mut idcg = 0.0;
    for rank0 in 0..ideal_hits {
        idcg += 1.0 / ((rank0 as f64 + 2.0).log2());
    }
    if idcg == 0.0 {
        return 0.0;
    }
    dcg / idcg
}

/// **reciprocal rank** for ONE query: `1 / (position-of-first-relevant)` (1-based), or `0.0` when no
/// relevant id appears in `ranked`. `expected` empty ⇒ 1.0 (vacuous). Pure — averaged across queries
/// this is MRR.
pub fn reciprocal_rank(ranked: &[String], expected: &[String]) -> f64 {
    if expected.is_empty() {
        return 1.0;
    }
    let gold: std::collections::HashSet<&String> = expected.iter().collect();
    for (rank0, id) in ranked.iter().enumerate() {
        if gold.contains(id) {
            return 1.0 / (rank0 as f64 + 1.0);
        }
    }
    0.0
}

/// Average [`recall_at_k`] / [`ndcg_at_k`] / [`reciprocal_rank`] over a whole labeled set for ONE
/// mode, given each query's ranked id list (`per_query_ranked[i]` is the mode's ranking for
/// `set.0[i]`). Panics-free: an empty set yields all-zero metrics. `per_query_ranked` MUST be the
/// same length/order as `set.0` (the bake-off runner guarantees this).
pub fn aggregate_metrics(
    mode: RetrievalMode,
    set: &LabeledSet,
    per_query_ranked: &[Vec<String>],
    k: usize,
) -> ModeMetrics {
    let n = set.0.len();
    if n == 0 {
        return ModeMetrics {
            mode,
            recall_at_k: 0.0,
            ndcg_at_k: 0.0,
            mrr: 0.0,
            k,
            queries: 0,
        };
    }
    let mut sum_recall = 0.0;
    let mut sum_ndcg = 0.0;
    let mut sum_rr = 0.0;
    for (q, ranked) in set.0.iter().zip(per_query_ranked.iter()) {
        sum_recall += recall_at_k(ranked, &q.expected_meeting_ids, k);
        sum_ndcg += ndcg_at_k(ranked, &q.expected_meeting_ids, k);
        sum_rr += reciprocal_rank(ranked, &q.expected_meeting_ids);
    }
    let denom = n as f64;
    ModeMetrics {
        mode,
        recall_at_k: sum_recall / denom,
        ndcg_at_k: sum_ndcg / denom,
        mrr: sum_rr / denom,
        k,
        queries: n,
    }
}

/// Render a [`BakeoffReport`] as a fixed-width comparison table (stdout-friendly; no color, no deps).
/// One row per mode, columns recall@k / nDCG@k / MRR. Deterministic — pure string formatting.
pub fn format_report_table(report: &BakeoffReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "RAG bake-off — {} queries, k={}\n",
        report.queries, report.k
    ));
    out.push_str(&format!(
        "{:<10} {:>10} {:>10} {:>10}\n",
        "mode",
        format!("recall@{}", report.k),
        format!("ndcg@{}", report.k),
        "mrr"
    ));
    out.push_str(&format!("{}\n", "-".repeat(44)));
    for m in &report.modes {
        out.push_str(&format!(
            "{:<10} {:>10.4} {:>10.4} {:>10.4}\n",
            m.mode.label(),
            m.recall_at_k,
            m.ndcg_at_k,
            m.mrr
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    // ── recall@k ────────────────────────────────────────────────────────────────────────────────
    #[test]
    fn recall_at_k_counts_expected_in_topk() {
        // ranked: a, b, c, d ; expected: b, d ; k=3 → b in top3, d NOT (rank 4) → 1/2.
        let ranked = ids(&["a", "b", "c", "d"]);
        let expected = ids(&["b", "d"]);
        assert!((recall_at_k(&ranked, &expected, 3) - 0.5).abs() < 1e-9);
        // k=4 → both found → 1.0.
        assert!((recall_at_k(&ranked, &expected, 4) - 1.0).abs() < 1e-9);
        // k=1 → only `a` seen, neither expected → 0.0.
        assert!((recall_at_k(&ranked, &expected, 1) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn recall_at_k_edge_cases() {
        // Empty expected ⇒ 1.0 (vacuous).
        assert!((recall_at_k(&ids(&["a"]), &[], 5) - 1.0).abs() < 1e-9);
        // k = 0 ⇒ 0.0.
        assert!((recall_at_k(&ids(&["a", "b"]), &ids(&["a"]), 0) - 0.0).abs() < 1e-9);
        // Nothing retrieved ⇒ 0.0.
        assert!((recall_at_k(&[], &ids(&["a"]), 5) - 0.0).abs() < 1e-9);
        // Duplicate in ranked doesn't inflate — one distinct expected still counts once.
        assert!((recall_at_k(&ids(&["a", "a", "b"]), &ids(&["a"]), 5) - 1.0).abs() < 1e-9);
    }

    // ── nDCG@k ──────────────────────────────────────────────────────────────────────────────────
    #[test]
    fn ndcg_at_k_perfect_ranking_is_one() {
        // Expected {a,b} at ranks 1,2 → DCG == IDCG → 1.0.
        let ranked = ids(&["a", "b", "c"]);
        let expected = ids(&["a", "b"]);
        assert!((ndcg_at_k(&ranked, &expected, 3) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ndcg_at_k_single_relevant_at_known_rank() {
        // One relevant id at rank 2 (0-based idx 1): DCG = 1/log2(3); IDCG (1 ideal hit) = 1/log2(2)=1.
        // nDCG = 1/log2(3) ≈ 0.6309.
        let ranked = ids(&["x", "a", "y"]);
        let expected = ids(&["a"]);
        let want = 1.0 / (3f64.log2());
        assert!((ndcg_at_k(&ranked, &expected, 3) - want).abs() < 1e-9);
        // Same id at rank 1 → nDCG 1.0.
        let ranked2 = ids(&["a", "x", "y"]);
        assert!((ndcg_at_k(&ranked2, &expected, 3) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ndcg_at_k_two_relevant_out_of_order_lt_one() {
        // Relevant {a,b} at ranks 1 and 3 → DCG = 1/log2(2) + 1/log2(4) = 1 + 0.5 = 1.5;
        // IDCG (2 hits) = 1/log2(2) + 1/log2(3) = 1 + 0.6309 = 1.6309 → nDCG ≈ 0.9197 (< 1).
        let ranked = ids(&["a", "z", "b", "q"]);
        let expected = ids(&["a", "b"]);
        let dcg = 1.0 + 1.0 / (4f64.log2());
        let idcg = 1.0 + 1.0 / (3f64.log2());
        assert!((ndcg_at_k(&ranked, &expected, 4) - dcg / idcg).abs() < 1e-9);
    }

    #[test]
    fn ndcg_at_k_edge_cases() {
        assert!((ndcg_at_k(&ids(&["a"]), &[], 5) - 1.0).abs() < 1e-9); // vacuous
        assert!((ndcg_at_k(&ids(&["a"]), &ids(&["a"]), 0) - 0.0).abs() < 1e-9); // k=0
        assert!((ndcg_at_k(&[], &ids(&["a"]), 5) - 0.0).abs() < 1e-9); // nothing retrieved
        // No relevant in top-k → 0 (relevant is beyond the cutoff).
        assert!((ndcg_at_k(&ids(&["x", "y", "a"]), &ids(&["a"]), 2) - 0.0).abs() < 1e-9);
    }

    // ── MRR / reciprocal rank ─────────────────────────────────────────────────────────────────────
    #[test]
    fn reciprocal_rank_uses_first_relevant() {
        // First relevant at rank 3 → 1/3.
        let ranked = ids(&["x", "y", "a", "b"]);
        let expected = ids(&["a", "b"]);
        assert!((reciprocal_rank(&ranked, &expected) - 1.0 / 3.0).abs() < 1e-9);
        // First relevant at rank 1 → 1.0.
        assert!((reciprocal_rank(&ids(&["b", "x"]), &expected) - 1.0).abs() < 1e-9);
        // None relevant → 0.0.
        assert!((reciprocal_rank(&ids(&["x", "y"]), &expected) - 0.0).abs() < 1e-9);
        // Empty expected ⇒ 1.0 (vacuous).
        assert!((reciprocal_rank(&ids(&["x"]), &[]) - 1.0).abs() < 1e-9);
    }

    // ── aggregate + table ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn aggregate_metrics_averages_over_queries() {
        let set = LabeledSet(vec![
            LabeledQuery {
                query: "q1".into(),
                lang: "en".into(),
                expected_meeting_ids: ids(&["a"]),
            },
            LabeledQuery {
                query: "q2".into(),
                lang: "pl".into(),
                expected_meeting_ids: ids(&["b"]),
            },
        ]);
        // q1: perfect (a at rank1). q2: b at rank2 → recall@2=1, ndcg@2=1/log2(3), rr=1/2.
        let per_query = vec![ids(&["a", "z"]), ids(&["z", "b"])];
        let m = aggregate_metrics(RetrievalMode::Hybrid, &set, &per_query, 2);
        assert_eq!(m.queries, 2);
        assert_eq!(m.k, 2);
        // recall: (1.0 + 1.0)/2 = 1.0.
        assert!((m.recall_at_k - 1.0).abs() < 1e-9);
        // ndcg: (1.0 + 1/log2(3)) / 2.
        let want_ndcg = (1.0 + 1.0 / 3f64.log2()) / 2.0;
        assert!((m.ndcg_at_k - want_ndcg).abs() < 1e-9);
        // mrr: (1.0 + 0.5)/2 = 0.75.
        assert!((m.mrr - 0.75).abs() < 1e-9);
    }

    #[test]
    fn aggregate_metrics_empty_set_is_zero() {
        let set = LabeledSet(vec![]);
        let m = aggregate_metrics(RetrievalMode::Fts, &set, &[], 5);
        assert_eq!(m.queries, 0);
        assert_eq!(m.recall_at_k, 0.0);
        assert_eq!(m.ndcg_at_k, 0.0);
        assert_eq!(m.mrr, 0.0);
    }

    #[test]
    fn labeled_set_parses_sample_json() {
        let json = r#"[
            {"query":"budget planning","lang":"en","expected_meeting_ids":["m-1","m-2"]},
            {"query":"planowanie budżetu","lang":"pl","expected_meeting_ids":["m-1"]}
        ]"#;
        let set = LabeledSet::from_json(json).unwrap();
        assert_eq!(set.len(), 2);
        assert_eq!(set.0[0].expected_meeting_ids, ids(&["m-1", "m-2"]));
        assert_eq!(set.0[1].lang, "pl");
        // lang may be omitted → defaults to "".
        let set2 = LabeledSet::from_json(r#"[{"query":"q","expected_meeting_ids":[]}]"#).unwrap();
        assert_eq!(set2.0[0].lang, "");
    }

    #[test]
    fn format_report_table_has_a_row_per_mode() {
        let report = BakeoffReport {
            k: 5,
            queries: 3,
            modes: vec![
                ModeMetrics {
                    mode: RetrievalMode::Fts,
                    recall_at_k: 0.5,
                    ndcg_at_k: 0.4,
                    mrr: 0.6,
                    k: 5,
                    queries: 3,
                },
                ModeMetrics {
                    mode: RetrievalMode::Semantic,
                    recall_at_k: 0.7,
                    ndcg_at_k: 0.65,
                    mrr: 0.72,
                    k: 5,
                    queries: 3,
                },
                ModeMetrics {
                    mode: RetrievalMode::Hybrid,
                    recall_at_k: 0.8,
                    ndcg_at_k: 0.75,
                    mrr: 0.81,
                    k: 5,
                    queries: 3,
                },
            ],
        };
        let table = format_report_table(&report);
        assert!(table.contains("fts"));
        assert!(table.contains("semantic"));
        assert!(table.contains("hybrid"));
        assert!(table.contains("recall@5"));
        assert!(table.contains("ndcg@5"));
        assert!(table.contains("mrr"));
        // The hybrid recall value is rendered to 4dp.
        assert!(table.contains("0.8000"));
    }
}
