---
name: retrieval-memory-brain
description: The MAP of Murmur's shipped retrieval + agent-memory stack by file:symbol (hybrid fusion, FTS5/vec0 schema, embedding models, the reranker seam, bitemporal facts, consolidation, the entity graph, the eval harness) plus the MEASURED gaps and the load-bearing invariants. Use for any task touching retrieval quality, chunking, embeddings, reranking, fact/memory storage, consolidation, entity graph, or RAG eval. Not for embedder runtime cost on Metal (ondevice-model-perf).
---

# retrieval-memory-brain — the shipped brain, by symbol

The knowledge map for Murmur's retrieval + agent-memory stack (the "brain"). This file is the
LEAN index: what exists, where, and the invariants you cannot break. The deep, code-grounded
material lives in `references/*.md` — load the one you need. **Trust code, not this map:** grep the
symbol in the current tree before you rely on it (commands and storage are split across growing
domain modules, and line anchors drift).

**Pairs with the `memory-retrieval-architect` agent** — dispatch it (or follow this skill yourself).
Because every change here touches `visibility_clause`, `lock-security-reviewer` is a required second
gate, and the recall verdict is `adversarial-verifier`'s.

## Hybrid retrieval map

Ask / related-notes / MCP retrieve through **`Db::search_hybrid_visible`** (`storage/db.rs`), which
fuses three visibility-gated legs:

- **FTS (keyword/BM25)** — `search_visible_impl` + raw scores from `fts_meeting_scores` (external-content
  FTS5 over note + transcript + augmented topic chunks; `-bm25`, higher-better). Implicit-AND phrase match.
- **KNN (vector)** — `search_semantic_visible` + raw distances from `knn_meeting_distances` over the
  `vec0` KNN tables (`vec_chunks` / `topic_vec_chunks`). Only informative when the real e5 model is present.
- **Entity graph (GraphRAG-lite)** — `entities_matching_query` → `meetings_mentioning_entities_visible`
  (deterministic co-mention neighbourhood, no LLM), positionally scored.

Fusion is `embed::score_fuse(fts, knn, graph)` — weighted (`SCORE_FUSE_W_FTS`/`_W_KNN`/`_W_GRAPH` =
0.4/0.4/0.2), min-max-normalized per leg, KNN distance inverted to `1/(1+d)`, **query-adaptive empty-leg
redistribution**, ties → id-ASC. `rrf_fuse` (`RRF_K=60`) is the fallback + the doc-hit fuser
(`fuse_doc_hits`). Full mechanics + the measured gaps: **`references/hybrid-fusion.md`**.

## Embeddings & schema

- Model: **multilingual-e5-small** (`selected_embed_model` default; `mmlw-retrieval-e5-small` is the
  Polish-first alt). Real encoder `embed/candle_bert.rs`; `active_embedder` returns it when the model
  dir is present, else the hash-bag **`StubEmbedder`** (semantic == noise on the stub — always check which).
- **`EMBED_DIM = 384`** is the `vec0` column width. A model swap that stays 384 is ZERO schema migration
  but a full **RE-INDEX** (`reindex_embeddings` — vectors from different models are not comparable).
- **e5 asymmetric prefixes are load-bearing:** `QUERY_PREFIX="query: "` / `PASSAGE_PREFIX="passage: "`
  (`embed_query`/`embed_passage`). Chunking: `chunk_note` (~800 chars) / `chunk_transcript` (~1000+150
  overlap) / `augment_chunk_text` (deterministic contextual header). Details: **`references/embeddings-and-reranking.md`**.

## Reranker seam

`rerank.rs` (L1.4, Ask-only). `Reranker` trait; **`StubReranker`** (identity — no-model/cloud floor);
**`PromptedReranker`** (pointwise `{"relevant": bool}` per candidate over the resident on-device
reasoner, `RERANK_TIMEOUT_MS=3000`, `RERANK_TOP_K=10`). `active_reranker` → stub for `"stub"`/`"cloud:*"`
reasoners (**on-device-only, never cloud egress**). Candidates arrive pre-gated → opens no read path.
It is a STUB-grade lever today (measured: adds no value). Direction (cross-encoder / ColBERT):
**`references/embeddings-and-reranking.md`**.

## Bitemporal fact memory + consolidation

- **Facts** (`facts.rs`): `reconcile_facts` (pure) implements the Zep **invalidate-not-delete** pattern —
  a changed fact CLOSES the old row (`valid_to`) and ADDS a new open one; history is preserved. Parallel
  `user_facts` ("me") in `user_memory.rs`.
- **Consolidation** (`memory.rs`, L2.1 generative-agents recipe): `run_consolidation_pass` /
  `consolidation_tick` (hourly). Scores facts by `composite_score` = recency (`0.995^hours`) · importance
  · relevance (0.4/0.4/0.2); reflects important/populous entities into `memory_rollups`; GCs each rollup
  against its scope's current `fact_set_hash`. Details + invariants: **`references/memory-and-consolidation.md`**.

## The measured gaps (honest, 2026-07-10)

Per `eval/results/rag-bakeoff-real-vault.md` — these are MEASURED, do not contradict without a re-run:

- **hybrid 0.90 < semantic 0.95** on a deliberately semantic-favouring real query set is CORRECT hybrid
  behaviour, not a bug. Three fusion-tuning fixes all failed (adaptive redistribution = 0 change;
  OR-match FTS = reverted, regressed the CI baseline; **graph-drop = 0 change — the "graph noise"
  hypothesis was WRONG**). On keyword queries **hybrid 0.90 > semantic 0.84** — hybrid is the robust choice.
- **18/20 hard queries have an EMPTY FTS leg** (implicit-AND phrase match misses paraphrase/cross-lingual).
- The **reranker adds no measured value** (10 candidates exhaust the 3 s deadline → identity). A real win
  needs a cross-encoder, not sequential pointwise LLM calls.

## The invariants

1. Every retrieval leg + every memory read is **gated** (`visibility_clause` / a `*_visible` reader). A
   sealed-not-unlocked meeting leaks nothing through FTS, KNN, graph, reranker, MCP, or a rollup.
2. **Consolidation reads with the EMPTY unlock set** (`memory.rs` `no_unlocks = HashSet::new()`) — derived
   memory never surfaces sealed content, even in a session where the folder is unlocked.
3. Embedders/rerankers are **on-device or a stub — NEVER cloud** (no candidate-snippet egress).
4. **Consolidation purges on seal** (`purge_memory_rollups_tx` inside every seal tx; scores/facts cascade).
5. Any **chunk/model/prefix change MANDATES `reindex_embeddings`** (never mix vectors from two models).

Because you inevitably touch a gate or the fact/rollup store, the **lock-security-reviewer is a required
second gate** on any change here (see `.codex/rules/lock-model.md`).

## The eval harness + honesty bar

`eval/{mod,bakeoff}.rs`. `run_bakeoff` / `run_bakeoff_with_rerank` → recall@k / nDCG@k / MRR per
`RetrievalMode` (Fts/Semantic/Hybrid/Reranked). The real-vault run is `#[ignore]`d
(`run_bakeoff_over_real_db_from_env`, `MURMUR_BAKEOFF_DB`/`_DEK`/`_SET`/`_K`/`_OUT` env). `cargo test --lib`
proves the fusion + metric MATH and the gating purity; **recall needs the real e5 model + Metal + a real
PL/EN vault on a Mac.** Report the delta against the committed synthetic baseline
(`rag-bakeoff-latest.md`) AND the real vault; a synthetic number is not a real number. Runbook:
**`references/rag-bakeoff-runbook.md`**.

## Reference files (load on demand)

- `references/hybrid-fusion.md` — `search_hybrid_visible` + `score_fuse` mechanics, empty-leg
  redistribution, RRF fallback, the graph-noise + empty-FTS-leg gaps and candidate fixes.
- `references/memory-and-consolidation.md` — bitemporal `reconcile_facts`; consolidation scoring +
  `fact_set_hash` GC; the empty-unlock-set + purge-on-seal invariants.
- `references/embeddings-and-reranking.md` — the 384-dim `vec0` lock + reindex mandate; e5 prefixes;
  swappable models; the reranker's stub-grade reality and the cross-encoder/ColBERT direction.
- `references/rag-bakeoff-runbook.md` — the real-vault `MURMUR_BAKEOFF_*` run; retrieval vs generation
  metrics; reporting the honest delta.
