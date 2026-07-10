# RAG bake-off — REAL dev vault (2026-07-10) + the hybrid<semantic investigation

- date: 2026-07-10
- commit: 0ad5648 (trunk, post query-adaptive fusion)
- corpus: the author's **real dev vault** — ~60 meetings (PL-dominant, noisy: many empty/test titles), WAL-copied, dev DEK
- labeled set: **local only** (`~/Library/Application Support/MeetNotes-dev/eval/rag-bakeoff-real.json`) — 20 queries, deliberately **retrieval-hard**: 5 entity-anchored, 5 paraphrase (no lexical overlap), 5 cross-lingual PL↔EN, 5 temporal. NOT committed (real meeting titles = PII).
- embedder: multilingual-e5-small (REAL model); reranker: prompted Qwen3-1.7B (resident)

| mode | recall@5 | nDCG@5 | MRR |
|---|---:|---:|---:|
| fts | 0.1000 | 0.1000 | 0.1000 |
| semantic | **0.9500** | 0.8991 | 0.8875 |
| hybrid | 0.9000 | 0.8557 | 0.8500 |
| hybrid+rerank | 0.9000 | 0.8557 | 0.8500 |

## The hybrid < semantic gap is NOT a fixable bug — it is correct hybrid behaviour (3 measured attempts)

The goal was to recover hybrid recall@5 (0.90) toward semantic-alone (0.95) on paraphrase/cross-lingual queries. **Three independent measured attempts prove the gap is not closable by fusion tuning** — it is the inherent, *correct* cost of a balanced hybrid:

1. **Query-adaptive empty-leg redistribution** (shipped in #235): **0.0000** change. 18/20 queries have an empty FTS leg (implicit-AND match); redistribution only rescales the surviving {knn,graph} blend by a constant, so it cannot reorder the top-5.

2. **OR-match FTS fallback** (`fts_match_query_any` when the strict AND match is empty): improved REAL ranking (nDCG 0.8557→0.8807, MRR 0.8500→0.8871 — the right meeting ranks higher) but **recall stayed 0.9000**, and it **regressed the synthetic CI-gate ranking** (nDCG 0.8250→0.7846, MRR 0.8146→0.7597) with recall held. A goal-missing ranking tradeoff that degrades the committed baseline → **reverted** (not shipped). The gain was measured on a deliberately semantic-favouring 20-query set; shipping it risked over-fitting.

3. **Dropping / down-weighting the graph leg** (the eval's own earlier hypothesis — "graph co-mention noise costs the ~1 gold doc"): **0.0000** change on BOTH sets (measured via a `MURMUR_SCORE_FUSE_W_GRAPH=0` sweep). **The hypothesis was wrong** — the graph leg does not displace anything in the top-5 here.

**Conclusion.** The one gold doc that keeps real hybrid at 0.90 is simply ranked just below position 5 once the semantic score is fused with the (near-empty) FTS + KNN + graph legs — a fundamental fusion-vs-pure-semantic tension on queries constructed to have zero lexical overlap. Recovering it would require **over-trusting the semantic leg**, which the synthetic corpus proves would HURT keyword queries: there **hybrid 0.90 > semantic 0.84** — hybrid earns its keep by being robust across BOTH query types, not by beating semantic on the semantic-favouring subset. The system is working as designed. No code change ships from this investigation; the finding is the deliverable.

## Reranker (unchanged): a prompted 1.7B adds no measured value — 10 candidates exhaust the 3 s deadline → identity order. Keep the trait seam; evaluate bge-reranker-v2-m3 (cross-encoder, no generation) if reranking is ever wanted.

## Honest limits

20 queries, one PL-dominant vault, author-labeled, **deliberately skewed away from lexical overlap** — a signal, not a benchmark. The synthetic committed corpus (`rag-bakeoff-latest.md`, hybrid 0.90) remains the CI merge-gate baseline. A balanced real query set (including ordinary keyword searches, where FTS earns its weight) would show the true production tradeoff — this set does not, by construction.
