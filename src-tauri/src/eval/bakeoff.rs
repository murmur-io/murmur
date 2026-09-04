//! The bake-off RUNNER: over a real `Db` vault + an [`crate::embed::Embedder`], run all three
//! retrieval legs for each labeled query and compute the per-mode metrics.
//!
//! ## Modes
//!
//! - **FTS** — `Db::search_visible_in_range(query, …, temporal-window, None, None)` → the meeting ids in BM25
//!   order (with the Brain v2 L1.5 date filter + temporal window fallback, exactly as the
//!   `search_meetings` tool queries it).
//! - **Semantic** — embed the query (asymmetric `embed_query`), then
//!   `Db::search_semantic_visible(query_vec)` → the meeting ids in vector-distance order.
//! - **Hybrid** — the REAL `Db::search_hybrid_visible` (Brain v2 L1.3 score fusion over the
//!   FTS ∪ topic-FTS, KNN ∪ topic-KNN, and entity-graph legs, plus the L1.5 date filter) — the
//!   bake-off measures the retrieval the app actually ships, not a local re-implementation.
//! - **Reranked** (optional) — hybrid + a [`crate::rerank::Reranker`] pass over the top
//!   [`crate::rerank::RERANK_TOP_K`] candidates (Brain v2 L1.4).
//!
//! `today` is an EXPLICIT parameter (never `now()`): the synthetic runner passes the FIXED
//! [`crate::eval::corpus::CORPUS_ANCHOR_DATE`] so temporal gold labels never rot; the real-vault
//! runner passes the actual date.
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

use crate::embed::Embedder;
use crate::error::{AppError, Result};
use crate::eval::{aggregate_metrics, BakeoffReport, LabeledSet, ModeMetrics, RetrievalMode};
use crate::rerank::Reranker;
use crate::storage::models::SearchHit;
use crate::storage::Db;
use crate::summarize::temporal::extract_date_filter;

/// Retrieve the FTS meeting-id ranking for one query (BM25 order, deduped, visibility-gated),
/// with the L1.5 temporal window — exactly the `search_meetings` tool path.
fn fts_ranked(
    db: &Db,
    query: &str,
    k: usize,
    unlocked: &HashSet<String>,
    today: chrono::NaiveDate,
) -> Result<Vec<String>> {
    // Fetch a few extra candidates so the top-k cutoff is applied to a full list, not a truncated one.
    let limit = (k.max(1) * 4) as i64;
    let date_filter = extract_date_filter(query, today);
    let hits = db.search_visible_in_range(query, limit, unlocked, date_filter, None)?;
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
    // No S1 floor in the bake-off (0.0) — the committed baseline must stay byte-identical.
    let hits = db.search_semantic_visible(&query_vec, (k.max(1) * 4) as i64, 0.0, unlocked, None)?;
    Ok(hits.into_iter().map(|h| h.meeting.id).collect())
}

/// The REAL hybrid retrieval, as shipped: `Db::search_hybrid_visible` (score fusion over
/// FTS ∪ topic-FTS, KNN ∪ topic-KNN, entity graph) with the L1.5 temporal window.
fn hybrid_hits(
    db: &Db,
    embedder: &dyn Embedder,
    query: &str,
    k: usize,
    unlocked: &HashSet<String>,
    today: chrono::NaiveDate,
) -> Result<Vec<SearchHit>> {
    let query_vec = embedder
        .embed_query(&[query.to_string()])?
        .into_iter()
        .next()
        .unwrap_or_default();
    let date_filter = extract_date_filter(query, today);
    db.search_hybrid_visible(
        query,
        &query_vec,
        (k.max(1) * 4) as i64,
        0.0, // no S1 floor in the bake-off — the committed baseline must stay byte-identical.
        unlocked,
        date_filter,
        None, // the bake-off measures the WHOLE vault; a container scope would change the baseline
    )
}

/// Run the full bake-off over `set` at cutoff `k`, returning per-mode averaged metrics
/// (fts / semantic / hybrid). Delegates to [`run_bakeoff_with_rerank`] with no reranker.
pub fn run_bakeoff(
    db: &Db,
    embedder: &dyn Embedder,
    set: &LabeledSet,
    k: usize,
    unlocked: &HashSet<String>,
    today: chrono::NaiveDate,
) -> Result<BakeoffReport> {
    run_bakeoff_with_rerank(db, embedder, None, set, k, unlocked, today)
}

/// Run the full bake-off over `set` at cutoff `k`, returning per-mode averaged metrics.
///
/// For each labeled query we compute the rankings, then average recall@k / nDCG@k / MRR across
/// the set per mode. READ-ONLY + visibility-gated (see the module note). `k` is clamped to `>= 1`.
/// A meaningful semantic/hybrid result requires a REAL embedder + an indexed vault; with the stub or
/// an empty index the semantic leg is uninformative (documented, not an error).
///
/// `reranker`: `Some` adds the fourth `hybrid+rerank` mode — the hybrid ranking's top
/// [`crate::rerank::RERANK_TOP_K`] candidates reordered by the reranker (candidate text =
/// title + snippet, exactly the Ask wiring in `vault_context`). Pass the PROMPTED reranker only
/// when a real local model is resident; a stub reranker would measure the identity.
pub fn run_bakeoff_with_rerank(
    db: &Db,
    embedder: &dyn Embedder,
    reranker: Option<&dyn Reranker>,
    set: &LabeledSet,
    k: usize,
    unlocked: &HashSet<String>,
    today: chrono::NaiveDate,
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
    let mut rr_rankings: Vec<Vec<String>> = Vec::with_capacity(set.len());

    for q in &set.0 {
        let fts = fts_ranked(db, &q.query, k, unlocked, today)?;
        let sem = semantic_ranked(db, embedder, &q.query, k, unlocked)?;
        let hyb = hybrid_hits(db, embedder, &q.query, k, unlocked, today)?;
        if let Some(rr) = reranker {
            rr_rankings.push(rerank_hits(rr, &q.query, &hyb));
        }
        fts_rankings.push(fts);
        sem_rankings.push(sem);
        hyb_rankings.push(hyb.into_iter().map(|h| h.meeting.id).collect());
    }

    let mut modes: Vec<ModeMetrics> = vec![
        aggregate_metrics(RetrievalMode::Fts, set, &fts_rankings, k),
        aggregate_metrics(RetrievalMode::Semantic, set, &sem_rankings, k),
        aggregate_metrics(RetrievalMode::Hybrid, set, &hyb_rankings, k),
    ];
    if reranker.is_some() {
        modes.push(aggregate_metrics(
            RetrievalMode::Reranked,
            set,
            &rr_rankings,
            k,
        ));
    }

    Ok(BakeoffReport {
        k,
        queries: set.len(),
        modes,
    })
}

/// Reorder a hybrid hit list's top [`crate::rerank::RERANK_TOP_K`] via `reranker` — the same
/// candidate shape (`id`, `title\nsnippet`) and degrade-safety as the Ask wiring in
/// `vault_context::build_vault_context_hybrid_visible`.
fn rerank_hits(reranker: &dyn Reranker, query: &str, hits: &[SearchHit]) -> Vec<String> {
    let ids: Vec<String> = hits.iter().map(|h| h.meeting.id.clone()).collect();
    let k = crate::rerank::RERANK_TOP_K.min(hits.len());
    if k < 2 {
        return ids;
    }
    let candidates: Vec<(String, String)> = hits[..k]
        .iter()
        .map(|h| {
            let title = h.meeting.title.clone().unwrap_or_default();
            (h.meeting.id.clone(), format!("{title}\n{}", h.snippet))
        })
        .collect();
    let order = reranker.rerank(query, &candidates, crate::rerank::RERANK_TIMEOUT_MS);
    let mut head: Vec<String> = Vec::with_capacity(k);
    let mut pool: Vec<String> = ids[..k].to_vec();
    for id in order {
        if let Some(pos) = pool.iter().position(|x| *x == id) {
            head.push(pool.remove(pos));
        }
    }
    head.extend(pool);
    head.extend_from_slice(&ids[k..]);
    head
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixed anchor the synthetic corpus + labeled set were authored against.
    fn anchor_date() -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(crate::eval::corpus::CORPUS_ANCHOR_DATE, "%Y-%m-%d")
            .expect("valid corpus anchor")
    }

    #[test]
    fn rerank_hits_reorders_topk_and_keeps_tail() {
        // A reranker that reverses the candidate order; ids beyond RERANK_TOP_K must keep their
        // positions, and a reranker that "loses" an id must not drop it (degrade-safety).
        struct Reverser;
        impl crate::rerank::Reranker for Reverser {
            fn id(&self) -> &str {
                "rev-test"
            }
            fn rerank(&self, _q: &str, candidates: &[(String, String)], _t: u64) -> Vec<String> {
                let mut ids: Vec<String> = candidates.iter().map(|(id, _)| id.clone()).collect();
                ids.reverse();
                ids.pop(); // "lose" one id — rerank_hits must restore it.
                ids
            }
        }
        let mk = |id: &str| SearchHit {
            meeting: crate::storage::models::Meeting {
                id: id.to_string(),
                started_at: "2026-01-01T00:00:00Z".to_string(),
                ended_at: None,
                title: Some(id.to_string()),
                duration_s: 60,
                audio_path: None,
                status: crate::storage::models::MeetingStatus::Summarized,
                folder_id: None,
            },
            snippet: String::new(),
            matched_in: "note".to_string(),
        };
        let hits: Vec<SearchHit> = (0..12).map(|i| mk(&format!("m{i}"))).collect();
        let out = rerank_hits(&Reverser, "q", &hits);
        assert_eq!(out.len(), 12, "no id may be dropped");
        // Top-10 reversed (m9..m1), the lost m0 re-appended, then the untouched tail m10, m11.
        assert_eq!(out[0], "m9");
        assert_eq!(out[9], "m0", "an id the reranker lost must be restored");
        assert_eq!(&out[10..], &["m10", "m11"]);
    }

    #[test]
    fn run_bakeoff_rejects_empty_set() {
        // The empty-set guard fires before any retrieval. A migrated throwaway Db satisfies the
        // signature; the guard returns before it is touched.
        let set = LabeledSet(vec![]);
        let db = throwaway_db("bakeoff-empty");
        let stub = crate::embed::StubEmbedder;
        let err = run_bakeoff(&db, &stub, &set, 5, &HashSet::new(), anchor_date()).unwrap_err();
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
        let report = run_bakeoff(&db, &stub, &set, 5, &HashSet::new(), anchor_date()).unwrap();
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
        // Opt out of the `cargo test --lib` real-Metal-forward-pass safety net (embed.rs
        // `active_embedder`) — this is the one legitimate manual test that wants the real model.
        std::env::set_var("MURMUR_TEST_REAL_EMBED", "1");
        let embedder = crate::embed::active_embedder();
        // Empty unlocked set = eval OPEN content only. To include a sealed folder, unlock it in the
        // app and copy the WAL'd DB, or extend this to accept a folder-id list.
        // Real vault ⇒ the REAL query-time anchor (the labeled set is authored against it).
        let today = chrono::Utc::now().date_naive();
        let reranker = local_reranker_or_note();
        let report = run_bakeoff_with_rerank(
            &db,
            embedder.as_ref(),
            reranker.as_deref(),
            &set,
            k,
            &HashSet::new(),
            today,
        )
        .unwrap();
        println!("\n{}", crate::eval::format_report_table(&report));

        // Spec §L1.6: honor MURMUR_BAKEOFF_OUT — write the committed markdown artifact. The corpus
        // line deliberately does NOT echo the DB path (it may embed a user home dir — no PII).
        let ctx = crate::eval::ReportContext {
            date: today_utc(),
            commit: git_short_sha(),
            corpus: "real vault DB (env MURMUR_BAKEOFF_DB)".to_string(),
            labeled_set: set_file_name(&set_path),
            config: format!("RRF_K={}", crate::embed::RRF_K),
            embedder_id: crate::embed::selected_embed_model().id.to_string(),
            embedder_real: crate::embed::embed_model_present(),
            prompt_version: crate::prompts::PROMPT_VERSION.to_string(),
        };
        write_artifact_if_requested(&crate::eval::format_report_markdown(&report, &ctx));
    }

    /// SYNTHETIC baseline run (brain2 PR 2) — seeds the deterministic `eval::corpus` fixture
    /// meetings into a throwaway SQLCipher DB, runs the bake-off over the committed
    /// `rag-bakeoff-synthetic.json` labeled set (k=5), prints the markdown artifact, and honors
    /// `MURMUR_BAKEOFF_OUT` to write it to a file. `#[ignore]`d because a MEANINGFUL run wants the
    /// real embed model on a Mac (without it the semantic/hybrid rows come from the stub and the
    /// artifact says so loudly — the fts row is real either way):
    ///   MURMUR_BAKEOFF_OUT=eval/results/rag-bakeoff-baseline-synthetic.md \
    ///   cargo test --lib run_bakeoff_over_synthetic_corpus -- --ignored --nocapture
    #[test]
    #[ignore = "synthetic baseline: run manually on a Mac (real embed model preferred); writes MURMUR_BAKEOFF_OUT"]
    fn run_bakeoff_over_synthetic_corpus() {
        let db = throwaway_db("synthetic");
        let real = crate::embed::embed_model_present();
        if !real {
            eprintln!(
                "WARNING: no embedding model on disk — the semantic/hybrid legs use the STUB \
                 embedder; only the fts row is a quality signal. Download the model and re-run."
            );
        }
        // Opt out of the `cargo test --lib` real-Metal-forward-pass safety net (embed.rs
        // `active_embedder`) — this is a legitimate manual test that wants the real model.
        std::env::set_var("MURMUR_TEST_REAL_EMBED", "1");
        let embedder = crate::embed::active_embedder();
        let ids = crate::eval::corpus::seed_synthetic_corpus(&db, embedder.as_ref())
            .expect("seed synthetic corpus");
        assert_eq!(ids.len(), 16, "synthetic corpus is 16 meetings");

        let set = LabeledSet::from_json(include_str!("fixtures/rag-bakeoff-synthetic.json"))
            .expect("parse synthetic labeled set");
        // The FIXED anchor (never now()) — the temporal gold labels are authored against it.
        let today = anchor_date();
        let reranker = local_reranker_or_note();
        let report = run_bakeoff_with_rerank(
            &db,
            embedder.as_ref(),
            reranker.as_deref(),
            &set,
            5,
            &HashSet::new(),
            today,
        )
        .unwrap();

        let ctx = crate::eval::ReportContext {
            date: today_utc(),
            commit: git_short_sha(),
            corpus: format!(
                "synthetic (eval::corpus, {} seeded meetings, anchor {})",
                ids.len(),
                crate::eval::corpus::CORPUS_ANCHOR_DATE
            ),
            labeled_set: "src-tauri/src/eval/fixtures/rag-bakeoff-synthetic.json".to_string(),
            config: format!(
                "score_fuse {}/{}/{} + topic legs + temporal filter (RRF_K={} fallback)",
                crate::embed::SCORE_FUSE_W_FTS,
                crate::embed::SCORE_FUSE_W_KNN,
                crate::embed::SCORE_FUSE_W_GRAPH,
                crate::embed::RRF_K
            ),
            embedder_id: crate::embed::selected_embed_model().id.to_string(),
            embedder_real: real,
            prompt_version: crate::prompts::PROMPT_VERSION.to_string(),
        };
        let markdown = crate::eval::format_report_markdown(&report, &ctx);
        println!("\n{markdown}");
        write_artifact_if_requested(&markdown);
    }

    /// HEADLESS deterministic retrieval gate (runs in the normal loop): seed the synthetic corpus
    /// with the stub embedder and enforce the committed FTS baseline for all three metrics. FTS is
    /// the only genuine quality signal without a model. Semantic/hybrid remain intentionally
    /// un-gated here (stub vectors are not semantic); generation quality has its separate manual
    /// real-provider bake-off.
    #[test]
    fn synthetic_corpus_bakeoff_wires_headless() {
        let db = throwaway_db("synthetic-headless");
        let stub = crate::embed::StubEmbedder;
        let ids = crate::eval::corpus::seed_synthetic_corpus(&db, &stub).unwrap();
        assert_eq!(ids.len(), 16);
        let set =
            LabeledSet::from_json(include_str!("fixtures/rag-bakeoff-synthetic.json")).unwrap();
        assert_eq!(set.len(), 20);
        let report = run_bakeoff(&db, &stub, &set, 5, &HashSet::new(), anchor_date()).unwrap();
        assert_eq!(report.modes.len(), 3, "no reranker passed — exactly 3 rows");
        let fts = report
            .modes
            .iter()
            .find(|m| m.mode == RetrievalMode::Fts)
            .expect("fts row present");
        const COMMITTED_FTS_FLOOR: f64 = 0.20;
        assert!(
            fts.recall_at_k >= COMMITTED_FTS_FLOOR,
            "FTS recall@5 regressed below committed floor {COMMITTED_FTS_FLOOR}: {}",
            fts.recall_at_k
        );
        assert!(
            fts.ndcg_at_k >= COMMITTED_FTS_FLOOR,
            "FTS nDCG@5 regressed below committed floor {COMMITTED_FTS_FLOOR}: {}",
            fts.ndcg_at_k
        );
        assert!(
            fts.mrr >= COMMITTED_FTS_FLOOR,
            "FTS MRR regressed below committed floor {COMMITTED_FTS_FLOOR}: {}",
            fts.mrr
        );
        // The markdown renders over the real report without panicking and carries all three rows.
        let ctx = crate::eval::ReportContext {
            date: "2026-07-10".to_string(),
            commit: "test".to_string(),
            corpus: "synthetic".to_string(),
            labeled_set: "rag-bakeoff-synthetic.json".to_string(),
            config: "RRF_K=60".to_string(),
            embedder_id: crate::embed::DEFAULT_EMBED_MODEL_ID.to_string(),
            embedder_real: false,
            prompt_version: crate::prompts::PROMPT_VERSION.to_string(),
        };
        let md = crate::eval::format_report_markdown(&report, &ctx);
        assert!(md.contains("| fts |") && md.contains("| semantic |") && md.contains("| hybrid |"));
        assert!(md.contains("WARNING — STUB EMBEDDER"));
    }

    /// Resolve the PROMPTED reranker over the LOCAL brain model when one is on disk, else `None`
    /// (a stub reranker would measure the identity — the row is skipped with a printed note).
    /// Test-only helper shared by both `#[ignore]` runners.
    fn local_reranker_or_note() -> Option<Box<dyn crate::rerank::Reranker>> {
        let cfg = crate::settings::AppConfig {
            brain_backend: crate::settings::BrainBackend::Local,
            brain_model_id: std::env::var("MURMUR_BAKEOFF_LIGHT_ID").ok(),
            ..Default::default()
        };
        // Offline eval harness (not the live app runtime) — a fresh semaphore here is correct:
        // there is no real concurrency to guard against in a standalone benchmark process.
        let heavy = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let reranker = crate::rerank::active_reranker(std::sync::Arc::from(
            crate::reason::active_reasoner(&cfg, &heavy),
        ));
        if reranker.id() == "stub" {
            eprintln!(
                "NOTE: no local brain model on disk — the hybrid+rerank row is SKIPPED \
                 (a stub reranker is the identity and would measure nothing)."
            );
            return None;
        }
        Some(reranker)
    }

    /// Write the artifact to `$MURMUR_BAKEOFF_OUT` when set (creating parent dirs). Test-only
    /// helper shared by both `#[ignore]` runners.
    fn write_artifact_if_requested(markdown: &str) {
        let Ok(path) = std::env::var("MURMUR_BAKEOFF_OUT") else {
            return;
        };
        if path.trim().is_empty() {
            return;
        }
        let p = std::path::Path::new(&path);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(p, markdown).expect("write MURMUR_BAKEOFF_OUT artifact");
        println!("bake-off artifact written to MURMUR_BAKEOFF_OUT");
    }

    /// Today as ISO `YYYY-MM-DD` (UTC) for the artifact provenance line.
    fn today_utc() -> String {
        chrono::Utc::now().format("%Y-%m-%d").to_string()
    }

    /// Short git sha of the working tree, `-dirty`-suffixed when uncommitted changes exist
    /// (an artifact produced from a dirty tree must say so — HEAD alone misattributes it),
    /// or `"unknown"` outside a checkout (never panics).
    fn git_short_sha() -> String {
        let sha = std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty());
        let Some(sha) = sha else {
            return "unknown".to_string();
        };
        let dirty = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false);
        if dirty {
            format!("{sha}-dirty")
        } else {
            sha
        }
    }

    /// The labeled-set FILE NAME only (never the full path — it may embed a user home dir).
    fn set_file_name(path: &str) -> String {
        std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "labeled-set.json".to_string())
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
