//! Brain v2 L1.4 — the RERANKER seam (Ask-only). A [`Reranker`] reorders an ALREADY-RETRIEVED,
//! ALREADY-GATED candidate list; it opens NO new read path (candidates arrive as `(id, text)`
//! pairs the caller assembled from visibility-gated readers) and NO egress (the prompted impl
//! runs on the on-device reasoner ONLY — a cloud or stub reasoner resolves to the identity
//! [`StubReranker`], see [`active_reranker`]).
//!
//! HARD CONTRACT: `rerank` NEVER errors and NEVER drops a candidate — on any failure, timeout, or
//! deadline expiry it degrades to the input order (retrieval quality falls back to the fused
//! ranking; nothing breaks). This is why the trait returns `Vec<String>`, not `Result`.

use std::time::{Duration, Instant};

use crate::reason::{GenOptions, LocalReasoner};

/// Total wall-clock budget for ONE rerank pass (all candidates together). Spec §L1.4: 3 s to
/// start; the eval gate decides whether to tighten it or shrink the candidate count.
pub const RERANK_TIMEOUT_MS: u64 = 3_000;

/// How many top candidates a caller should hand to the reranker (spec §L1.4: 10 to start).
pub const RERANK_TOP_K: usize = 10;

/// Hard decode cap per pointwise relevance call — the answer is a one-key JSON bool.
const RERANK_MAX_TOKENS: usize = 32;

/// A swappable candidate reranker. `candidates` are `(id, text)` pairs, best-first per the
/// upstream fused ranking; the return is the SAME ids, complete, reordered (or identical).
pub trait Reranker: Send + Sync {
    /// Stable id of the backing implementation (`"stub"` / `"prompted"`).
    fn id(&self) -> &str;

    /// Reorder `candidates` by relevance to `query` within `timeout_ms`. MUST return every input
    /// id exactly once and MUST degrade to the input order on any failure/timeout — never `Err`.
    fn rerank(&self, query: &str, candidates: &[(String, String)], timeout_ms: u64) -> Vec<String>;
}

/// The identity reranker: input order out. The no-model / cloud-brain floor.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubReranker;

impl Reranker for StubReranker {
    fn id(&self) -> &str {
        "stub"
    }

    fn rerank(
        &self,
        _query: &str,
        candidates: &[(String, String)],
        _timeout_ms: u64,
    ) -> Vec<String> {
        candidates.iter().map(|(id, _)| id.clone()).collect()
    }
}

/// POINTWISE prompted reranker over the resident on-device reasoner (spec §L1.4): one strict-JSON
/// `{"relevant": bool}` call per candidate, deadline-checked BETWEEN candidates via [`Instant`],
/// each call bounded by [`RERANK_MAX_TOKENS`] + the remaining wall-clock budget (the P0.3
/// `GenOptions.timeout` pattern — resource bounds scoped to the surface that needs them).
///
/// Output order: candidates judged relevant first (stable, input-relative), then the rest — a
/// candidate whose call fails or falls past the deadline is treated as RELEVANT (it keeps its
/// fused position; degrading toward the input order, never away from it). No PII is logged —
/// counts and durations only.
pub struct PromptedReranker {
    reasoner: std::sync::Arc<dyn LocalReasoner>,
}

impl PromptedReranker {
    pub fn new(reasoner: std::sync::Arc<dyn LocalReasoner>) -> Self {
        Self { reasoner }
    }
}

impl Reranker for PromptedReranker {
    fn id(&self) -> &str {
        "prompted"
    }

    fn rerank(&self, query: &str, candidates: &[(String, String)], timeout_ms: u64) -> Vec<String> {
        if candidates.len() < 2 {
            return candidates.iter().map(|(id, _)| id.clone()).collect();
        }
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "relevant": { "type": "boolean" } },
            "required": ["relevant"]
        });
        let system = "You judge search relevance. Reply ONLY with JSON: {\"relevant\": true} if \
                      the document could help answer the query, {\"relevant\": false} otherwise.";

        // relevance[i]: true = keep in the front group (default on failure — degrade to input order).
        let mut relevance = vec![true; candidates.len()];
        let mut judged = 0usize;
        for (i, (_id, text)) in candidates.iter().enumerate() {
            let now = Instant::now();
            if now >= deadline {
                break; // out of budget — the rest keep their fused positions.
            }
            let remaining = deadline - now;
            let user = format!("Query: {query}\n\nDocument:\n{text}");
            let opts = GenOptions {
                max_tokens: Some(RERANK_MAX_TOKENS),
                temperature: Some(0.0),
                enable_thinking: false,
                timeout: Some(remaining),
                ..GenOptions::default()
            };
            match self.reasoner.structured_with(system, &user, &schema, opts) {
                Ok(v) => {
                    if let Some(rel) = v.get("relevant").and_then(|b| b.as_bool()) {
                        relevance[i] = rel;
                        judged += 1;
                    }
                    // Malformed shape ⇒ leave `true` (input order).
                }
                Err(_) => {
                    // Any failure ⇒ leave `true` (input order). No PII in logs — not even the error
                    // formatting here; the count below is the observability.
                }
            }
        }
        tracing::debug!(
            target: "rerank",
            candidates = candidates.len(),
            judged,
            "prompted rerank pass"
        );

        // Stable partition: relevant candidates first, both groups in input-relative order.
        let mut front: Vec<String> = Vec::with_capacity(candidates.len());
        let mut back: Vec<String> = Vec::new();
        for (i, (id, _)) in candidates.iter().enumerate() {
            if relevance[i] {
                front.push(id.clone());
            } else {
                back.push(id.clone());
            }
        }
        front.extend(back);
        front
    }
}

/// Resolve the ACTIVE reranker for a resolved reasoner: the prompted impl when a REAL on-device
/// model backs it, else the identity stub. A `"stub"` reasoner has no model; a `"cloud:*"`
/// reasoner would EGRESS candidate snippets per pointwise call — the reranker seam is deliberately
/// on-device-only (spec §L1.4: the resident Qwen), so cloud resolves to the stub too.
pub fn active_reranker(reasoner: std::sync::Arc<dyn LocalReasoner>) -> Box<dyn Reranker> {
    let id = reasoner.id();
    if id == "stub" || id.starts_with("cloud:") {
        return Box::new(StubReranker);
    }
    Box::new(PromptedReranker::new(reasoner))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;
    use serde_json::Value;
    use std::sync::Arc;

    fn cands(n: usize) -> Vec<(String, String)> {
        (0..n)
            .map(|i| (format!("m{i}"), format!("candidate text {i}")))
            .collect()
    }

    #[test]
    fn stub_reranker_is_identity() {
        let c = cands(4);
        let out = StubReranker.rerank("query", &c, RERANK_TIMEOUT_MS);
        assert_eq!(out, vec!["m0", "m1", "m2", "m3"]);
        assert_eq!(StubReranker.id(), "stub");
        // Empty input → empty output, no panic.
        assert!(StubReranker.rerank("q", &[], 10).is_empty());
    }

    /// The prompted reranker over the STUB reasoner: the stub's `structured` returns a JSON object
    /// with NO `relevant` key, so every candidate keeps its default-true flag ⇒ identity order —
    /// the degrade contract, exercised end-to-end through the real code path.
    #[test]
    fn prompted_with_stub_reasoner_degrades_to_identity() {
        let rr = PromptedReranker::new(Arc::new(crate::reason::StubReasoner));
        let c = cands(3);
        let out = rr.rerank("budget", &c, RERANK_TIMEOUT_MS);
        assert_eq!(
            out,
            vec!["m0", "m1", "m2"],
            "unparseable judgments must keep input order"
        );
        assert_eq!(out.len(), c.len(), "no candidate may be dropped");
    }

    /// A reasoner that answers deterministically: relevant iff the candidate text contains the
    /// query token. Proves the relevant-first stable partition.
    struct KeywordReasoner;
    impl crate::reason::LocalReasoner for KeywordReasoner {
        fn id(&self) -> &str {
            "keyword-test"
        }
        fn reason(&self, _s: &str, _u: &str) -> Result<String> {
            Ok(String::new())
        }
        fn structured(&self, _s: &str, user: &str, _schema: &Value) -> Result<Value> {
            // `user` = "Query: <q>\n\nDocument:\n<text>".
            let q = user
                .lines()
                .next()
                .and_then(|l| l.strip_prefix("Query: "))
                .unwrap_or_default()
                .to_string();
            let doc = user.split("Document:\n").nth(1).unwrap_or_default();
            Ok(serde_json::json!({ "relevant": doc.contains(&q) }))
        }
    }

    #[test]
    fn prompted_moves_relevant_candidates_first_stably() {
        let rr = PromptedReranker::new(Arc::new(KeywordReasoner));
        let c = vec![
            ("a".to_string(), "nothing here".to_string()),
            ("b".to_string(), "the budget line".to_string()),
            ("c".to_string(), "also nothing".to_string()),
            ("d".to_string(), "budget again".to_string()),
        ];
        let out = rr.rerank("budget", &c, RERANK_TIMEOUT_MS);
        assert_eq!(
            out,
            vec!["b", "d", "a", "c"],
            "relevant first, stable within groups"
        );
    }

    /// A reasoner that blocks long enough to blow the deadline on the very first call — the pass
    /// must still return EVERY id, in input order (deadline degrade, never a drop or an Err).
    struct SlowReasoner;
    impl crate::reason::LocalReasoner for SlowReasoner {
        fn id(&self) -> &str {
            "slow-test"
        }
        fn reason(&self, _s: &str, _u: &str) -> Result<String> {
            Ok(String::new())
        }
        fn structured(&self, _s: &str, _u: &str, _schema: &Value) -> Result<Value> {
            std::thread::sleep(std::time::Duration::from_millis(50));
            // Judge everything IRRELEVANT — if the deadline logic failed, order would change.
            Ok(serde_json::json!({ "relevant": false }))
        }
    }

    #[test]
    fn prompted_deadline_degrades_to_input_order_for_the_rest() {
        let rr = PromptedReranker::new(Arc::new(SlowReasoner));
        let c = cands(6);
        // 10ms budget vs a 50ms first call: the FIRST candidate is judged (its pre-check happens
        // at t≈0, before the deadline) and comes back irrelevant; every LATER candidate falls past
        // the deadline and keeps its default-true flag ⇒ input order, ahead of the demoted one.
        let out = rr.rerank("q", &c, 10);
        assert_eq!(out.len(), 6, "every id must survive a deadline expiry");
        assert_eq!(
            out[5], "m0",
            "the one judged irrelevant moves back; the unjudged keep order"
        );
        assert_eq!(&out[..5], &["m1", "m2", "m3", "m4", "m5"]);
    }

    #[test]
    fn active_reranker_resolves_stub_cloud_and_local() {
        // Stub reasoner → stub reranker.
        let r = active_reranker(Arc::new(crate::reason::StubReasoner));
        assert_eq!(r.id(), "stub");
        // A cloud-id reasoner → stub reranker (rerank must never egress).
        struct CloudLike;
        impl crate::reason::LocalReasoner for CloudLike {
            fn id(&self) -> &str {
                "cloud:claude_code"
            }
            fn reason(&self, _s: &str, _u: &str) -> Result<String> {
                Ok(String::new())
            }
            fn structured(&self, _s: &str, _u: &str, _schema: &Value) -> Result<Value> {
                Ok(serde_json::json!({}))
            }
        }
        assert_eq!(active_reranker(Arc::new(CloudLike)).id(), "stub");
        // A local-model-like id → prompted.
        assert_eq!(active_reranker(Arc::new(KeywordReasoner)).id(), "prompted");
    }
}
