//! The bake-off RUNNER: over a real `Db` vault + an [`crate::embed::Embedder`], run all three
//! retrieval legs for each labeled query and compute the per-mode metrics.
//!
//! ## Modes
//!
//! - **FTS** — `Db::search_visible(query)` → the meeting ids in BM25 order.
//! - **Semantic** — embed the query (asymmetric `embed_query`), then
//!   `Db::search_semantic_visible(query_vec)` → the meeting ids in vector-distance order.
//! - **Hybrid** — RRF-fuse the FTS and semantic id lists via [`crate::embed::rrf_fuse`] (the same
//!   fusion the app uses for documents) → the fused meeting-id order.
//!
//! ## Gating (read the lock-model note in `mod.rs`)
//!
//! Both `search_visible` and `search_semantic_visible` apply `visibility_clause`, so a sealed-not-
//! session-unlocked meeting is invisible to the eval — identical to the app. The harness opens NO
//! raw connection and adds NO ungated read. `unlocked` is the session unlock set (pass an empty set
//! to eval only OPEN content).
//!
//! ## Headless honesty
//!
//! This file typechecks against the retrieval APIs in `cargo test --lib`, but a MEANINGFUL run needs
//! the embedding model on disk (else the semantic leg uses the deterministic `StubEmbedder`, whose
//! vectors are not semantic) and a populated vault. The end-to-end test is `#[ignore]`d; run it on a
//! Mac per `docs/RAG-BAKEOFF.md`. A green build is NOT a retrieval-quality claim.

use std::collections::HashSet;

use crate::embed::{rrf_fuse, Embedder, RRF_K};
use crate::error::{AppError, Result};
use crate::eval::{aggregate_metrics, BakeoffReport, LabeledSet, ModeMetrics, RetrievalMode};
use crate::storage::Db;

/// Retrieve the FTS meeting-id ranking for one query (BM25 order, deduped, visibility-gated).
fn fts_ranked(db: &Db, query: &str, k: usize, unlocked: &HashSet<String>) -> Result<Vec<String>> {
    // Fetch a few extra candidates so the top-k cutoff is applied to a full list, not a truncated one.
    let limit = (k.max(1) * 4) as i64;
    let hits = db.search_visible(query, limit, unlocked)?;
    Ok(hits.into_iter().map(|h| h.meeting.id).collect())
}

/// Retrieve the SEMANTIC meeting-id ranking for one query (vector KNN order, visibility-gated). The
/// query is embedded with the asymmetric `embed_query` convention (the same as index-side
/// `embed_passage` — see `Db::index_meeting_chunks`). Returns an empty ranking if the embedder yields
/// no vector (e.g. an all-punctuation query).
fn semantic_ranked(
    db: &Db,
    embedder: &dyn Embedder,
    query: &str,
    k: usize,
    unlocked: &HashSet<String>,
) -> Result<Vec<String>> {
    let query_vec = embedder
        .embed_query(&[query.to_string()])?
        .into_iter()
        .next()
        .unwrap_or_default();
    if query_vec.is_empty() {
        return Ok(Vec::new());
    }
    // KNN returns one hit per meeting (nearest chunk) already deduped by `search_semantic_visible`.
    let hits = db.search_semantic_visible(&query_vec, (k.max(1) * 4) as i64, unlocked)?;
    Ok(hits.into_iter().map(|h| h.meeting.id).collect())
}

/// RRF-fuse the FTS and semantic rankings into one meeting-id order (the hybrid leg). Either list may
/// be empty; RRF over the remaining one preserves its order. Uses the app's [`RRF_K`].
fn hybrid_ranked(fts: &[String], semantic: &[String]) -> Vec<String> {
    let fused = rrf_fuse(&[fts.to_vec(), semantic.to_vec()], RRF_K);
    fused.into_iter().map(|(id, _score)| id).collect()
}

/// Run the full bake-off over `set` at cutoff `k`, returning per-mode averaged metrics.
///
/// For each labeled query we compute the three rankings, then average recall@k / nDCG@k / MRR across
/// the set per mode. READ-ONLY + visibility-gated (see the module note). `k` is clamped to `>= 1`.
/// A meaningful semantic/hybrid result requires a REAL embedder + an indexed vault; with the stub or
/// an empty index the semantic leg is uninformative (documented, not an error).
pub fn run_bakeoff(
    db: &Db,
    embedder: &dyn Embedder,
    set: &LabeledSet,
    k: usize,
    unlocked: &HashSet<String>,
) -> Result<BakeoffReport> {
    let k = k.max(1);
    if set.is_empty() {
        return Err(AppError::InvalidArg(
            "bake-off labeled set is empty".to_string(),
        ));
    }

    let mut fts_rankings: Vec<Vec<String>> = Vec::with_capacity(set.len());
    let mut sem_rankings: Vec<Vec<String>> = Vec::with_capacity(set.len());
    let mut hyb_rankings: Vec<Vec<String>> = Vec::with_capacity(set.len());

    for q in &set.0 {
        let fts = fts_ranked(db, &q.query, k, unlocked)?;
        let sem = semantic_ranked(db, embedder, &q.query, k, unlocked)?;
        let hyb = hybrid_ranked(&fts, &sem);
        fts_rankings.push(fts);
        sem_rankings.push(sem);
        hyb_rankings.push(hyb);
    }

    let modes: Vec<ModeMetrics> = vec![
        aggregate_metrics(RetrievalMode::Fts, set, &fts_rankings, k),
        aggregate_metrics(RetrievalMode::Semantic, set, &sem_rankings, k),
        aggregate_metrics(RetrievalMode::Hybrid, set, &hyb_rankings, k),
    ];

    Ok(BakeoffReport {
        k,
        queries: set.len(),
        modes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hybrid_ranked_fuses_both_legs() {
        // m2 appears in both legs (high in each) → should outrank m1/m3 (each in only one leg).
        let fts = vec!["m1".to_string(), "m2".to_string()];
        let sem = vec!["m3".to_string(), "m2".to_string(), "m1".to_string()];
        let fused = hybrid_ranked(&fts, &sem);
        let pos = |id: &str| fused.iter().position(|x| x == id).unwrap();
        assert!(
            pos("m2") < pos("m3"),
            "m2 (both legs) must outrank m3 (one leg)"
        );
        assert!(
            pos("m1") < pos("m3"),
            "m1 (both legs) must outrank m3 (one leg)"
        );
        // Dedup: every id appears once.
        assert_eq!(fused.len(), 3);
    }

    #[test]
    fn hybrid_ranked_handles_empty_leg() {
        // Semantic leg empty (e.g. no model / punctuation query) → hybrid preserves the FTS order.
        let fts = vec!["a".to_string(), "b".to_string()];
        let fused = hybrid_ranked(&fts, &[]);
        assert_eq!(fused, vec!["a".to_string(), "b".to_string()]);
        // FTS leg empty → hybrid preserves the semantic order.
        let sem = vec!["x".to_string(), "y".to_string()];
        assert_eq!(
            hybrid_ranked(&[], &sem),
            vec!["x".to_string(), "y".to_string()]
        );
        // Both empty → empty.
        assert!(hybrid_ranked(&[], &[]).is_empty());
    }

    #[test]
    fn run_bakeoff_rejects_empty_set() {
        // The empty-set guard fires before any retrieval. A migrated throwaway Db satisfies the
        // signature; the guard returns before it is touched.
        let set = LabeledSet(vec![]);
        let db = throwaway_db("bakeoff-empty");
        let stub = crate::embed::StubEmbedder;
        let err = run_bakeoff(&db, &stub, &set, 5, &HashSet::new()).unwrap_err();
        assert!(matches!(err, AppError::InvalidArg(_)));
    }

    #[test]
    fn run_bakeoff_runs_over_empty_vault_and_reports_three_modes() {
        // With an empty (migrated) vault every retrieval returns nothing, so recall/ndcg/mrr are 0
        // for the gold-bearing query — but the WIRING is exercised: three modes reported, table
        // renders, no panic. This is a pure headless proof that `run_bakeoff` threads FTS + semantic
        // + hybrid through the gated readers without a model.
        let db = throwaway_db("bakeoff-empty-vault");
        let stub = crate::embed::StubEmbedder;
        let set = LabeledSet(vec![crate::eval::LabeledQuery {
            query: "budget planning".to_string(),
            lang: "en".to_string(),
            expected_meeting_ids: vec!["m-nonexistent".to_string()],
        }]);
        let report = run_bakeoff(&db, &stub, &set, 5, &HashSet::new()).unwrap();
        assert_eq!(report.modes.len(), 3, "all three modes must be reported");
        assert_eq!(report.k, 5);
        assert_eq!(report.queries, 1);
        for m in &report.modes {
            assert_eq!(
                m.recall_at_k, 0.0,
                "empty vault ⇒ nothing retrieved ⇒ recall 0"
            );
            assert_eq!(m.mrr, 0.0);
        }
        let table = crate::eval::format_report_table(&report);
        assert!(table.contains("fts") && table.contains("semantic") && table.contains("hybrid"));
    }

    /// END-TO-END real run — points at a REAL DB + a labeled-set JSON via env vars, so a human can run
    /// the actual quality bake-off on a Mac WITHOUT recompiling. `#[ignore]`d so it never runs in the
    /// normal loop. Set:
    ///   `MURMUR_BAKEOFF_DB`  — path to a SQLCipher murmur DB (a copy of the dev DB is fine),
    ///   `MURMUR_BAKEOFF_DEK` — the 64-hex DEK for that DB (the dev DEK for a dev DB),
    ///   `MURMUR_BAKEOFF_SET` — path to a labeled-set JSON (the `LabeledSet` array format),
    ///   `MURMUR_BAKEOFF_K`   — (optional) cutoff k, default 5.
    /// The embedder is `active_embedder()` — the REAL model when its files are on disk, else the stub
    /// (a warning is printed). See `docs/RAG-BAKEOFF.md`.
    #[test]
    #[ignore = "real bake-off: needs MURMUR_BAKEOFF_DB/DEK/SET env + the embed model on a Mac"]
    fn run_bakeoff_over_real_db_from_env() {
        let db_path = std::env::var("MURMUR_BAKEOFF_DB")
            .expect("set MURMUR_BAKEOFF_DB to a murmur SQLCipher DB path");
        let dek = std::env::var("MURMUR_BAKEOFF_DEK")
            .expect("set MURMUR_BAKEOFF_DEK to that DB's 64-hex DEK");
        let set_path = std::env::var("MURMUR_BAKEOFF_SET")
            .expect("set MURMUR_BAKEOFF_SET to a labeled-set JSON path");
        let k: usize = std::env::var("MURMUR_BAKEOFF_K")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5);

        let db = Db::open_with_key(std::path::Path::new(&db_path), &dek)
            .expect("open bake-off DB (check the DEK)");
        let set = LabeledSet::from_json(
            &std::fs::read_to_string(&set_path).expect("read labeled-set JSON"),
        )
        .expect("parse labeled-set JSON");

        if !crate::embed::embed_model_present() {
            eprintln!(
                "WARNING: no embedding model on disk — the semantic/hybrid legs use the STUB \
                 embedder and their numbers are NOT a quality signal. Download the model first."
            );
        }
        let embedder = crate::embed::active_embedder();
        // Empty unlocked set = eval OPEN content only. To include a sealed folder, unlock it in the
        // app and copy the WAL'd DB, or extend this to accept a folder-id list.
        let report = run_bakeoff(&db, embedder.as_ref(), &set, k, &HashSet::new()).unwrap();
        println!("\n{}", crate::eval::format_report_table(&report));
    }

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// A migrated, empty SQLCipher Db under the fixed test DEK (headless-safe temp file).
    fn throwaway_db(label: &str) -> Db {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-bakeoff-{label}-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        Db::open_with_key(&p, TEST_DEK).unwrap()
    }
}
