# RAG bake-off

- date: 2026-07-09
- commit: ddfbad0-dirty
- corpus: synthetic (eval::corpus, 16 seeded meetings, anchor 2026-06-29)
- labeled set: src-tauri/src/eval/fixtures/rag-bakeoff-synthetic.json (20 queries, k=5)
- config: RRF_K=60
- embedder: multilingual-e5-small (REAL model — semantic/hybrid rows are a genuine quality signal)

| mode | recall@5 | ndcg@5 | mrr |
|---|---:|---:|---:|
| fts | 0.2000 | 0.2000 | 0.2000 |
| semantic | 0.8417 | 0.6834 | 0.6563 |
| hybrid | 0.8417 | 0.6842 | 0.6563 |
