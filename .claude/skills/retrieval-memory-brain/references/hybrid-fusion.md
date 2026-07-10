# Hybrid fusion — `search_hybrid_visible` + `score_fuse`

Deep reference for Murmur's 3-leg hybrid retrieval. Grep the symbols in the current tree before
relying on any detail here — this file is a map, the code is the truth.

## The pipeline — `Db::search_hybrid_visible` (`storage/db.rs`)

Signature: `search_hybrid_visible(query: &str, query_vec: &[f32], limit: i64, unlocked: &HashSet<String>,
date_filter: Option<(String, String)>) -> Result<Vec<SearchHit>>`.

It builds **three visibility-gated legs**, each producing both a snippet-bearing hit list AND a raw
per-meeting score list, then fuses the score lists and reattaches snippets:

1. **FTS (keyword / BM25).**
   - Hit list: `search_visible_impl(query, limit, unlocked, range)`.
   - Raw scores: `fts_meeting_scores(query, limit, unlocked, range)` → `(meeting_id, -bm25)` (SQLite FTS5
     `bm25()` is lower/more-negative = better, so it's negated to higher-better). Sources are the
     external-content FTS5 mirrors over note chunks + transcript chunks + augmented topic chunks
     (`fts_topic_chunks`, `fts_doc_chunks`, …).
   - **Temporal fallback (L1.5):** if `fts_scored` is empty AND a date window is set, the window IS the
     query — `meetings_in_range_visible` returns in-window meetings newest-first, positionally scored
     (`1/(i+1)`). This is why a bare "last week" query still retrieves.
   - FTS uses **implicit-AND phrase matching** — every query term must be present. This is the root of the
     empty-FTS-leg gap below.

2. **KNN (vector / semantic).**
   - Hit list: `search_semantic_visible(query_vec, limit, unlocked)`.
   - Raw scores: `knn_meeting_distances(query_vec, limit, unlocked, range)` → `(meeting_id, distance)`
     (lower = better) over the `vec0` KNN tables (`vec_chunks`, `topic_vec_chunks`).
   - **Empty when the embedder is the stub or the vault is un-indexed.** `query_vec` comes from
     `active_embedder().embed_query(...)` — with `StubEmbedder` the vector is a hash-bag (semantic ==
     noise), and a never-reindexed vault has an empty `vec_chunks` so KNN returns nothing. ALWAYS
     determine which is live before diagnosing a semantic miss.

3. **Entity graph (GraphRAG-lite).**
   - `entities_matching_query(query, unlocked)` resolves the query to KNOWN VISIBLE entities
     (deterministic, no LLM), then `meetings_mentioning_entities_visible(&matched, unlocked)` gathers
     their co-mention neighbourhood. The temporal window is applied in Rust (`graph.retain(in_range)`).
   - Scored positionally (`1/(i+1)`). Snippet priority is LOWEST: the graph leg is inserted into the
     `by_id` map FIRST, so a meeting also hit by FTS/KNN keeps the lexical/semantic snippet the user
     queried; a graph-only neighbour carries `matched_in = "entity"` + its title.

Then: `fused = embed::score_fuse(&fts_scored, &knn_scored, &graph_scored)`. If `fused` is empty (raw legs
empty but plain hit lists are not — shouldn't happen), it falls back to `rrf_fuse([fts_ids, sem_ids], RRF_K)`.
Finally the top-`limit` fused ids are mapped back to `SearchHit`s (a topic-leg-only id with no snippet
gets one synthesized via `first_topic_snippet`, still gated-by-construction because the id came from a
gated leg).

**Every leg reader applies `visibility_clause` / the `unlocked` set.** A sealed-not-session-unlocked
meeting is invisible to FTS, KNN, AND the graph resolver + neighbour reader. Do not add a leg or a
snippet-synthesis path that reads a meeting row without an id from a gated leg.

## The fusion math — `embed::score_fuse` (`embed.rs`)

`score_fuse(fts: &[(String,f64)], knn: &[(String,f64)], graph: &[(String,f64)]) -> Vec<(String,f64)>`.
Pure — no DB, no model. Leg contracts: `fts` higher-better (`-bm25`), `knn` RAW DISTANCES (inverted to
`sim = 1/(1+d)` BEFORE normalizing), `graph` higher-better (`1/rank`).

- **Per-leg min-max normalization to `[0,1]`.** A constant/single-entry leg normalizes to all-`1.0`
  (presence in a leg is signal). Empty leg → empty vec.
- **Base weights** are the SOURCE ratios: `SCORE_FUSE_W_FTS`/`_W_KNN`/`_W_GRAPH` = `0.4 / 0.4 / 0.2`
  (named consts, calibrated by the eval gate — tune these, not magic numbers at the call site).
- **Query-adaptive empty-leg redistribution.** A leg that returned ZERO candidates for THIS query
  contributes ZERO weight; its mass redistributes proportionally over the PRESENT legs. Each present leg's
  effective weight = `base_w / present_mass`, where `present_mass` = sum of base weights of present legs. So:
  - all three present ⇒ weights unchanged `0.4/0.4/0.2` (divisor `1.0`) — no regression;
  - FTS empty, KNN+graph present ⇒ `{0.4,0.2}` renormalize to `{0.667,0.333}`;
  - a single present leg ⇒ weight `1.0` (hybrid == that leg's order);
  - all empty ⇒ empty.
  This is a **monotonic rescale of the present legs** (same divisor for all), so it NEVER reorders a
  fixed set of present legs — only the empty-leg mass moves. **Consequence (measured, load-bearing):** when
  FTS is empty (18/20 hard queries), redistribution only rescales the {knn,graph} blend by a constant →
  **it cannot change the top-k on those queries.** It only affects the all-legs-present cases.
- Output sorted DESC, ties broken by id ASC (deterministic).

`rrf_fuse(lists, k)` — Reciprocal Rank Fusion, each list contributes `1/(RRF_K + rank)` (`RRF_K = 60`),
scores sum across lists. It's the `score_fuse` fallback and the doc-hit fuser (`fuse_doc_hits`, RRF over
KNN∪FTS document ids, per-document deduped, KNN snippet preferred).

## The measured gaps (2026-07-10 — `eval/results/rag-bakeoff-real-vault.md`)

On a real dev vault (~60 PL-dominant meetings) with a deliberately retrieval-HARD 20-query set (5
entity-anchored, 5 paraphrase, 5 cross-lingual PL↔EN, 5 temporal), real e5 model:

| mode | recall@5 | nDCG@5 | MRR |
|---|---:|---:|---:|
| fts | 0.10 | 0.10 | 0.10 |
| semantic | **0.95** | 0.899 | 0.888 |
| hybrid | 0.90 | 0.856 | 0.850 |
| hybrid+rerank | 0.90 | 0.856 | 0.850 |

### Gap 1 — hybrid 0.90 < semantic 0.95 is CORRECT, not a bug (three measured attempts)

The intuition was "hybrid should be ≥ pure semantic; recover the ~1 gold doc." **Three independent
measured fixes all failed** — the gap is the inherent, correct cost of a balanced hybrid:

1. **Query-adaptive empty-leg redistribution** (shipped #235): **0.0000** change. 18/20 queries have an
   empty FTS leg, so redistribution only rescales the surviving {knn,graph} blend by a constant — it
   cannot reorder the top-5. (See the const-rescale note above.)
2. **OR-match FTS fallback** (`fts_match_query_any` when strict AND is empty): improved REAL ranking
   (nDCG 0.856→0.881, MRR 0.850→0.887 — the right meeting ranks higher) but **recall stayed 0.90**, and
   it **regressed the synthetic CI baseline** (nDCG 0.825→0.785). A goal-missing ranking tradeoff that
   degrades the committed baseline → **reverted, not shipped.** (Risk: over-fitting to a
   semantic-favouring 20-query set.)
3. **Dropping / down-weighting the graph leg** (the earlier "graph co-mention noise costs the gold doc"
   hypothesis): **0.0000** change on BOTH sets (measured via a `MURMUR_SCORE_FUSE_W_GRAPH=0` sweep). **The
   graph-noise hypothesis was WRONG** — the graph leg displaces nothing in the top-5 here.

**Conclusion.** The gold doc sits just below position 5 once the strong semantic score is fused with the
near-empty FTS + KNN + graph. Recovering it means OVER-TRUSTING the semantic leg — which the synthetic
corpus proves would HURT keyword queries: there **hybrid 0.90 > semantic 0.84**. Hybrid earns its keep by
being robust across BOTH query types, not by beating semantic on the semantic-favouring subset. **No code
change ships from a re-run of this investigation unless it improves BOTH the real set AND the synthetic CI
baseline.** The finding is the deliverable.

### Gap 2 — the empty FTS leg (implicit-AND)

`fts_meeting_scores` uses FTS5 implicit-AND: every query term must appear. Paraphrase / cross-lingual /
entity-anchored queries with no lexical overlap produce an EMPTY FTS leg — measured at 18/20 on the hard
set (`embed.rs` `diag_count_empty_fts_legs_on_real_set`, `#[ignore]`, same `MURMUR_BAKEOFF_*` env). The
temporal fallback covers the date-windowed subset; the rest lean entirely on the KNN leg.

Candidate levers if a FUTURE balanced query set justifies it (each MUST be measured on BOTH corpora and
must not regress the synthetic baseline): OR-match FTS fallback (measured; helped real ranking, hurt the
CI baseline — needs a co-designed synthetic update), FTS query expansion, or a genuine cross-encoder
reranker (the current pointwise-LLM reranker adds nothing — see `references/embeddings-and-reranking.md`).
Tuning `score_fuse` weights alone is proven insufficient.

## Gotchas

- **The stub embedder masquerades as semantic.** With no e5 model, `active_embedder` is `StubEmbedder`
  (hash-bag). KNN "works" but the vectors are noise → semantic recall collapses to near-FTS. Check
  `embed_model_present()` before trusting any semantic number.
- **A recall "miss" may be a correct seal.** If the gold meeting is in a sealed-not-unlocked folder it is
  INVISIBLE by design. Verify the meeting is actually visible (empty-unlock-set eval, or a session unlock)
  before treating it as a retrieval bug.
- **A model/chunk/prefix change is inert until reindex.** Fusion changes are live immediately (pure math);
  index-shape changes need `reindex_embeddings`. Never mix old and new vectors.
- **The synthetic corpus is the CI merge gate.** `rag-bakeoff-latest.md` (hybrid 0.90) is the committed
  baseline — a change that regresses it does not ship even if the real vault improves.
