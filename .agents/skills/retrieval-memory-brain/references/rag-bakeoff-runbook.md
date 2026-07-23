# RAG bake-off runbook — proving retrieval quality honestly

The harness is `eval/{mod,bakeoff}.rs`. This is how you MEASURE a retrieval change instead of asserting it.
`cargo test --lib` proves the pure math + gating purity; RECALL needs the real e5 model + Metal + a real
PL/EN vault on a Mac. Grep the symbols before relying on them.

## What the harness computes

- **Modes** (`RetrievalMode`, `eval/mod.rs`): `Fts`, `Semantic`, `Hybrid`, `Reranked` (labels `"fts"` /
  `"semantic"` / `"hybrid"` / `"hybrid+rerank"`).
- **Retrieval metrics** (per query, then averaged over the `LabeledSet`):
  - `recall_at_k(ranked, expected, k)` — fraction of expected ids in the top-k.
  - `ndcg_at_k(ranked, expected, k)` — binary-relevance DCG normalized by the ideal (relevant ids first).
  - `reciprocal_rank(ranked, expected)` → averaged = MRR (first relevant hit's `1/rank`).
  - `aggregate_metrics(mode, set, per_query, k)` → `ModeMetrics { recall_at_k, ndcg_at_k, mrr }`.
- **These are RETRIEVAL metrics — they measure whether the right documents rank high, NOT whether the
  generated answer is good.** Generation quality (faithfulness / grounding / note quality) is a SEPARATE
  concern (`summarize/grounding.rs` deterministic hallucination flagging, `eval/notes_bakeoff.rs`, the
  provider eval). Don't conflate "recall@5 = 0.90" with "the Ask answer is correct."

## Running it

`run_bakeoff(db, embedder, set, k, unlocked, today)` → `run_bakeoff_with_rerank(db, embedder, reranker, ...)`.
`reranker: Some(...)` adds the fourth `hybrid+rerank` mode (top `RERANK_TOP_K` of the hybrid ranking
reordered; pass the PROMPTED reranker only when a real local model is resident — a stub reranker measures the
identity). READ-ONLY + visibility-gated. `today` is injected (the app passes `Utc::now().date_naive()`; the
harness passes `CORPUS_ANCHOR_DATE` so temporal gold labels never rot).

### Fast/headless — the math + gating, not recall

```bash
source ~/.cargo/env
( cd src-tauri && cargo test --lib )   # score_fuse purity, recall/ndcg/mrr math, empty-vault=0, gating tests
```
The committed `run_bakeoff_runs_over_empty_vault_and_reports_three_modes` proves an empty (migrated) vault ⇒
everything retrieves nothing ⇒ recall/ndcg/mrr = 0. This proves the HARNESS, not the model.

### The real-vault run — recall, on a Mac

`run_bakeoff_over_real_db_from_env` (`eval/bakeoff.rs`) is `#[ignore]`d so it never runs in CI. Env:

```bash
cd src-tauri && source ~/.cargo/env
MURMUR_BAKEOFF_DB=/tmp/bakeoff/meetnotes.sqlite \   # a SQLCipher murmur DB (a WAL-safe copy of the dev DB)
MURMUR_BAKEOFF_DEK=<64-hex> \                        # that DB's DEK (the dev DEK for a dev DB)
MURMUR_BAKEOFF_SET=/tmp/bakeoff/labeled-set.json \   # LabeledSet JSON: query texts + expected meeting ids
MURMUR_BAKEOFF_K=5 \                                 # optional cutoff, default 5
MURMUR_BAKEOFF_OUT=/tmp/bakeoff/out.md \             # optional: write the committed-markdown artifact
  cargo test --lib eval::bakeoff::tests::run_bakeoff_over_real_db_from_env -- --ignored --nocapture
```

Prerequisites (the real recall win only shows with the REAL model):
1. The e5 model must be resolvable to the test build — symlink the release model dir into
   `MeetNotes-dev/models/embed-multilingual-e5-small` (see `docs/research/2026-07-04-rag-bakeoff-results.md`
   for the exact `ln -s`).
2. The copied DB must be REINDEXED with the real model (a throwaway `#[ignore]` loop over
   `index_meeting_chunks` with `active_embedder()`), else `vec_chunks` is empty and semantic ≈ FTS.
3. The labeled set is NOT committed (real meeting titles = PII). Build it from your own vault; a good set
   deliberately spans entity-anchored / paraphrase / cross-lingual PL↔EN / temporal queries — but a
   BALANCED set MUST also include ordinary keyword queries (where FTS earns its weight), or you over-fit to
   semantic (see the honesty bar).

Companion diagnostic: `embed.rs` `diag_count_empty_fts_legs_on_real_set` (same env, `#[ignore]`) reports how
many queries have an empty FTS leg — the ceiling on what empty-leg fusion changes can even affect.

## Report the HONEST delta — the honesty bar

- **Two corpora, both reported.** CI enforces only the small deterministic FTS metric floor in
  `src-tauri/src/eval/bakeoff.rs`. The broader COMMITTED synthetic reports
  (`eval/results/rag-bakeoff-latest.md`, `eval/results/rag-bakeoff-baseline-synthetic.md`) remain a
  required manual comparison for retrieval changes, but are not an automated merge gate. The real vault
  (`eval/results/rag-bakeoff-real-vault.md`) is a signal, not a benchmark. A synthetic number is NOT a
  real-vault number — never present one as the other.
- **A skewed set proves the skew.** The 2026-07-10 real set was deliberately skewed AWAY from lexical overlap
  (18/20 empty FTS legs), so it makes pure semantic look best (0.95 vs hybrid 0.90). That is BY CONSTRUCTION —
  a balanced set including keyword searches would show hybrid's robustness (synthetic: hybrid 0.90 > semantic
  0.84). State the set's construction when you quote its numbers.
- **Improve BOTH or it's a tradeoff, not a win.** OR-match FTS improved real ranking but regressed the
  synthetic baseline → reverted. Graph-drop = 0 change on both → the graph-noise hypothesis was wrong. A
  fusion-tuning change that helps only the semantic-favouring subset is over-fitting; require a delta on the
  committed baseline too.
- **Don't fabricate a recall number.** If you couldn't run the real-vault bake-off (no Mac / no model / no
  labeled set), say exactly that — the math tests do NOT prove recall. "Recall unverified — needs a Mac run"
  is the honest verdict; hand it to `adversarial-verifier`.

## The committed baselines (as of trunk, 2026-07-10 — re-read the files, they update)

- Synthetic (manual full bake-off; CI gates only FTS ≥0.20): hybrid recall@5 **0.90**, nDCG
  **0.825**, fts 0.45; hybrid 0.90 > semantic 0.84 on keyword queries.
- Real dev vault (hard 20-query set): semantic 0.95 > hybrid 0.90 = hybrid+rerank 0.90; fts 0.10 — CORRECT
  hybrid behaviour on lexically-mismatched queries, proven un-closable by fusion tuning (three attempts).

## No PII

The labeled set + any surfaced snippet reference real meeting content — keep them out of the repo and out of
logs. Report shapes + aggregate numbers, never meeting/entity text.
