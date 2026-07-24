# Embeddings & the reranker seam

Deep reference for the embedder (`embed.rs` / `embed/candle_bert.rs`) and the reranker (`rerank.rs`).
Grep the symbols before relying on them.

## The 384-dim `vec0` lock — a model swap is a REINDEX, not a migration

- **`EMBED_DIM = 384`** (`embed.rs`) is the width of the `vec0` KNN columns (`vec_chunks`,
  `topic_vec_chunks`, `doc_vec_chunks` are declared `float[EMBED_DIM]`). This MUST equal the real model's
  output width. multilingual-e5-small = 384, and `StubEmbedder` also emits 384 — so the real model swaps in
  with **ZERO `vec0` schema migration.**
- Every bundled/selectable embedder MUST be BERT / hidden_size 384. The loader (`embed/candle_bert.rs`)
  GUARDS `hidden_size == EMBED_DIM` — a differently-dimensioned model is refused, because a wider vec0 column
  would be a schema migration, which Murmur explicitly does NOT do (additive-only rule).
- **Changing the model (even same dim) invalidates the index.** Vectors from a different model are not
  comparable — mixing them silently poisons KNN. `select_embed_model` sets `reindex_needed = true` when the
  resolved model id actually CHANGED; the FE prompts the user to run `reindex_embeddings` (an explicit user
  action; flipping the model does NOT auto-index). NEVER leave old + new vectors co-resident.

## e5 asymmetric prefixes are load-bearing

The intfloat e5 family was trained with `"passage: "` on documents and `"query: "` on queries. Using the
right prefix is load-bearing for recall:
- `PASSAGE_PREFIX = "passage: "`, `QUERY_PREFIX = "query: "` (`embed.rs`).
- Index side calls `Embedder::embed_passage` (prefixes each chunk), query side calls `Embedder::embed_query`.
  The raw `embed` still works (the stub ignores prefixes; the real model treats a prefix-less text as a
  generic passage) but index/query callers SHOULD use the asymmetric methods.
- A selected `EmbedModel` carries its OWN prefixes (`selected_embed_model`). `mmlw-retrieval-e5-small`
  (sdadas, the Polish-first alt) happens to share e5's `"query: "`/`"passage: "` convention (verified against
  its HF card) — it is NOT the ROBERTA `"zapytanie: "` prefix. If you add a model, get its prefixes from its
  HF card, don't assume.

## Chunking + contextual augmentation

- `chunk_note(title, date, markdown)` — deterministic paragraph chunking, ~`CHUNK_CHAR_TARGET = 800` chars,
  each carrying a `<title> · <date>` header for provenance.
- `chunk_transcript(...)` — turns packed into sliding windows ~`TRANSCRIPT_CHUNK_CHAR_TARGET = 1000` with a
  ~15% overlap (`TRANSCRIPT_CHUNK_OVERLAP_CHARS = 150`, trailing whole turns re-emitted), same header.
- `augment_chunk_text(title, date, attendees, facts, raw)` — the L1.2 **deterministic contextual-retrieval**
  mechanism at ZERO LLM cost: prepend a one-line situating header (title · date · capped attendees · capped
  facts) so a chunk carries its context into the embedding + FTS. Caps: `AUG_MAX_ATTENDEES = 5`,
  `AUG_MAX_FACTS = 8`. Empty parts are skipped (byte-identical to `raw` when nothing to add).
- **A chunk/augmentation change is inert until reindex** — it changes the indexed passage, so
  `reindex_embeddings` is required and old vectors must not linger.

## The embedder seam — `active_embedder` / `StubEmbedder`

`active_embedder()` returns the REAL `CandleBertEmbedder` (multilingual-e5-small via candle) when the model
dir is present, else the deterministic hash-bag `StubEmbedder`. The stub NEVER panics and NEVER blocks — it's
the no-model floor so the app + tests run without a download. **But stub vectors are noise:** semantic recall
collapses to near-FTS on the stub. Any recall diagnosis MUST first confirm `embed_model_present()` — a "the
brain isn't finding things" report is very often just the un-downloaded model.

`reindex_embeddings` (`commands/models.rs`, async command; core
`commands/mod.rs::reindex_embeddings_inner`) snapshots the LIVE
session unlock set and re-indexes only VISIBLE meetings + documents (`index_meeting_chunks` /
`index_note_chunks` / `index_document_chunks`). With the model absent it skips (status `model_missing`) —
it does NOT index with the stub (stub vectors would be worse than none). A model swap ⇒ the user re-runs it.

## The reranker seam — `rerank.rs` (L1.4, Ask-only) — a STUB-grade lever today

- `Reranker` trait: `rerank(query, candidates: &[(id, text)], timeout_ms) -> Vec<String>`. HARD CONTRACT:
  NEVER errors, NEVER drops a candidate — on any failure/timeout it degrades to the INPUT order (so retrieval
  quality falls back to the fused ranking; nothing breaks). That's why it returns `Vec<String>`, not `Result`.
- `StubReranker` — identity (input order out). The no-model / cloud floor.
- `PromptedReranker` — POINTWISE over the resident on-device reasoner: one strict-JSON `{"relevant": bool}`
  call per candidate, deadline-checked between candidates, each call bounded by `RERANK_MAX_TOKENS` + the
  remaining wall-clock. Relevant candidates float to the front (stable, input-relative); a failed/timed-out
  call is treated as RELEVANT (keeps its fused position — degrades TOWARD input order). `RERANK_TIMEOUT_MS =
  3000`, `RERANK_TOP_K = 10`.
- `active_reranker(reasoner)` → `StubReranker` for a `"stub"` reasoner OR a `"cloud:*"` reasoner. **The seam is
  deliberately ON-DEVICE-ONLY**: a cloud reasoner would EGRESS candidate snippets per pointwise call, so cloud
  resolves to the identity stub. Candidates arrive already visibility-gated (the caller assembled them from
  `*_visible` readers) → the reranker opens NO new read path and NO egress.

### Measured reality + the real direction

Per `eval/results/rag-bakeoff-real-vault.md` (2026-07-10): the prompted 1.7B reranker adds **no measured
value** — 10 candidates exhaust the 3 s deadline → identity order (hybrid+rerank == hybrid, 0.90/0.856/0.850).

A pointwise-LLM reranker is fundamentally the wrong tool: N sequential generations can't fit a tight latency
budget, and "is this relevant?" as free generation is weak signal. The real levers, IF reranking is ever
wanted:
- **A cross-encoder** (e.g. `bge-reranker-v2-m3`) — ONE forward pass scores a (query, passage) pair, NO
  generation, so all K candidates rerank in a single batched inference well inside the budget. This is the
  standard retrieve→rerank win and materially lifts MRR/recall in the literature.
- **ColBERT late-interaction** — token-level MaxSim over multi-vector embeddings; higher quality, heavier
  index. Bigger lift-per-latency than pointwise-LLM, bigger footprint than a cross-encoder.

Either is a MODEL choice with a runtime-cost consequence — co-design with the model-perf-engineer (residency /
Metal budget) and MEASURE the delta on both eval corpora. Keep the `Reranker` trait seam: swapping the impl is
a one-line `active_reranker` change; the contract (never-error, never-drop, on-device-only) stays.

## Gotchas

- **Don't add a cloud reranker/embedder.** Egressing candidate snippets or chunk text to a cloud model is a
  redaction-firewall bypass. On-device or stub, always.
- **A model swap without reindex silently corrupts KNN.** `reindex_needed` exists precisely so this is a user
  action, not a surprise. Never mix vectors.
- **`RERANK_TOP_K` × per-candidate latency must fit `RERANK_TIMEOUT_MS`** — the pointwise impl exhausts the
  budget at K=10. A cross-encoder removes this constraint (batched, no generation).
