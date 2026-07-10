# RAG bake-off — REAL dev vault (2026-07-10, post query-adaptive fusion)

- date: 2026-07-10
- commit: e862302-dirty (+ query-adaptive empty-leg redistribution in `embed::score_fuse`)
- corpus: the author's **real dev vault** — ~60 meetings (PL-dominant, noisy: many empty/test titles), WAL-copied, dev DEK
- labeled set: **local only** (`~/Library/Application Support/MeetNotes-dev/eval/rag-bakeoff-real.json`) — 20 queries, deliberately **retrieval-hard**: 5 entity-anchored, 5 paraphrase (no lexical overlap), 5 cross-lingual PL↔EN, 5 temporal (absolute dates). NOT committed (real meeting titles = PII); reproduce with the env vars below.
- embedder: multilingual-e5-small (REAL model)
- reranker: prompted Qwen3-1.7B GGUF (REAL model, resident), `RERANK_TOP_K=10`, `RERANK_TIMEOUT_MS=3000`
- config: RRF_K=60

| mode | recall@5 | nDCG@5 | MRR |
|---|---:|---:|---:|
| fts | 0.1000 | 0.1000 | 0.1000 |
| semantic | **0.9500** | 0.8991 | 0.8875 |
| hybrid | 0.9000 | 0.8557 | 0.8500 |
| hybrid+rerank | 0.9000 | 0.8557 | 0.8500 |

## Result of the query-adaptive empty-leg redistribution: NO change on this set (honest negative)

The fix does exactly what it was specified to do — a leg that returns ZERO candidates for a query
contributes ZERO effective weight, its mass redistributing over the present legs — but on THIS
real set it moves hybrid recall by **0.0000**. Hybrid stays 0.9000, still below semantic 0.9500.
That is not a bug in the fix; it is a property of the fusion math that the original hypothesis
missed. Two facts, both measured here, explain it:

1. **18 / 20 of these queries have a genuinely EMPTY FTS leg** (diagnostic
   `embed::tests::diag_count_empty_fts_legs_on_real_set`). `fts_meeting_scores` builds an
   **implicit-AND** FTS5 match over all query terms (`fts_match_query`, `db.rs`), so a 4-word
   paraphrase like *"Erste Bank advertising campaign motivational slogan"* matches no meeting
   containing ALL four terms → the FTS candidate list is empty. Only 2 queries (Snowflake / Robert)
   land a single AND-hit.

2. **When a leg is empty, redistribution only RESCALES the surviving blend by a constant — it can
   never reorder it.** With FTS empty the fused score of every id was already `0.4·knn + 0.2·graph`
   (the empty leg added 0 to everyone); the fix makes it `0.667·knn + 0.333·graph`, i.e. the SAME
   values × (1/0.6). The knn:graph ratio is unchanged (2:1 in both), so the top-k membership — and
   therefore recall/nDCG/MRR — is byte-for-byte identical. Ranking-relevant weight changes require
   the ratio BETWEEN present legs to change, which redistribution never does. This is pinned by
   `score_fuse_fts_empty_redistributes_to_semantic_and_graph` (asserts the exact renormalized
   scores) and `score_fuse_all_legs_present_identical_to_fixed_blend` (the no-regression pin).

**So where does hybrid's 0.90 < semantic's 0.95 actually come from on this set?** NOT the FTS leg
(empty on 18/20, contributes nothing). It is the **graph leg**: on the empty-FTS queries the fused
order is `knn ⊕ graph`, and the entity-neighbourhood graph leg promotes a few co-mention meetings
that semantic-alone would not surface, displacing a gold doc out of the top-5 on ~1 query. The
redistribution *raises* graph's effective weight (0.2 → 0.333) on those queries, which is the
opposite of what would help — but it happened not to change the top-5 membership here, hence the
exact 0.9000 tie with the pre-change run.

**Honest conclusion.** Query-adaptive empty-leg redistribution is the CORRECT, principled
normalization (and it protects the all-legs-present keyword case exactly — see the synthetic gate
below, still 0.90), but it is **not** the lever that recovers hybrid toward semantic on
paraphrase/cross-lingual queries. The real levers, for a follow-up (NOT this change): (a) OR-match
the FTS leg (`fts_match_query_any` already exists) so FTS returns ranked partial-overlap candidates
instead of nothing — then its down-weighting would actually bite; and/or (b) make the GRAPH leg
weight adaptive too, or gate it to entity-anchored queries, since it is graph noise (not FTS) that
costs the ~1 gold doc here.

## The reranker row is unchanged (still identical to hybrid) — see prior finding; a prompted 1.7B adds no measured value within the 3 s budget.

## Reproduce

```sh
MURMUR_BAKEOFF_LIGHT_ID=qwen3-1.7b \
MURMUR_BAKEOFF_DB=<wal-copy of the dev DB> \
MURMUR_BAKEOFF_DEK=<dev DEK> \
MURMUR_BAKEOFF_SET=<local labeled-set json> \
MURMUR_BAKEOFF_K=5 \
cargo test --lib run_bakeoff_over_real_db_from_env -- --ignored --nocapture

# empty-FTS-leg diagnostic (same env, no reranker/model needed for the count):
cargo test --lib diag_count_empty_fts_legs_on_real_set -- --ignored --nocapture
```

## Honest limits

20 queries, one vault, PL-dominant, author-labeled, **deliberately skewed away from lexical
overlap** — a signal, not a benchmark, and by construction the FTS leg is near-useless here (it
would earn its weight on ordinary keyword queries, which this set has almost none of). The
synthetic committed corpus (`rag-bakeoff-latest.md`, hybrid 0.90) remains the CI merge-gate
baseline; this real-vault run is the reality check. The redistribution change is safe (no
regression either place) and correct in principle; its measured recovery on THIS skewed set is
zero, and this report says so rather than claiming a win.
