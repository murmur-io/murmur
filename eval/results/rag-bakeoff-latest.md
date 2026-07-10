# RAG bake-off

- date: 2026-07-10
- commit: 2567226-dirty
- corpus: synthetic (eval::corpus, 16 seeded meetings, anchor 2026-06-29)
- labeled set: src-tauri/src/eval/fixtures/rag-bakeoff-synthetic.json (20 queries, k=5)
- config: score_fuse 0.4/0.4/0.2 + topic legs + temporal filter (RRF_K=60 fallback)
- prompts: v2026-07-10
- embedder: multilingual-e5-small (REAL model — semantic/hybrid rows are a genuine quality signal)

| mode | recall@5 | ndcg@5 | mrr |
|---|---:|---:|---:|
| fts | 0.4500 | 0.4500 | 0.4500 |
| semantic | 0.8417 | 0.7020 | 0.6801 |
| hybrid | 0.9000 | 0.8250 | 0.8146 |
