<!-- Turnkey protocol for validating retrieval quality on YOUR real Mac + vault. Decides whether the vector/brain stack earns its keep, or FTS5 (already shipped) is enough. -->
# RAG bake-off — does the vector layer earn its keep? (run on your Mac)

The lock-safe vector + GraphRAG-lite stack is merged but **dormant in prod** (behind the default-off `semantic_search_enabled` flag, with a stub embedder). Before we invest in the real embedder + the local reasoning brain, this protocol answers — **on your real vault** — the one question headless tests can't:

> **Is the shipped FTS5 search already "a brain", or do paraphrase / cross-lingual / entity-spread questions genuinely need the vector + graph layer?**

The red-team's warning: at a few-dozen-meeting corpus, FTS5 may already be enough, and the +1–2GB embedder/brain could be over-engineering. **Measure before we pour more in.**

---

## Stage 1 — evaluate the SHIPPED FTS5 Ask (do this first; needs no new code)

FTS5 Ask is **live today**. Run it on your real data and score it.

### Setup
```bash
source ~/.cargo/env
MURMUR_DEV_DEK=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef npm run dev
```
Open **Ask-My-Vault** (the Ask screen). Use your real vault.

### The question set (fill the `<...>` with YOUR real entities/projects/dates)
Pick ~15–20 spanning all four categories. Variety matters more than count.

**A. Entity-anchored (cross-meeting gather):**
1. Co `<Osoba A>` obiecała / do czego się zobowiązała?
2. Jaki jest aktualny stan projektu `<Projekt X>`?
3. Co jeszcze otwarte w temacie `<Projekt X>` — kto co ma dowieźć?
4. Wszystko co ustaliliśmy z `<Osoba B>` przez ostatni miesiąc.

**B. Paraphrase / spoken-vs-written (where keyword search fails):**
5. Kiedy ustaliliśmy deadline na `<rzecz>`? *(note may say "umówiliśmy się na piątek" — no word "deadline")*
6. Na co się zgodziliśmy w sprawie `<temat>`? *("ship it" / "lecimy z tym")*
7. Jakie były zastrzeżenia do `<pomysł>`? *("nie jestem przekonany", "ryzyko")*

**C. Cross-lingual (PL query ↔ EN note / vice-versa):**
8. (PL pytanie o spotkanie prowadzone po angielsku) `<temat>` — co ustalono?
9. (EN query about a Polish meeting) What did we decide about `<topic>`?

**D. Synthesis / "what do I owe" rollup:**
10. Co ja mam do zrobienia po spotkaniach z tego tygodnia?
11. Jakie decyzje zapadły w `<projekt>` i dlaczego?
12. Czego jeszcze nie domknęliśmy w `<temat>`?

*(Add 3–6 more from your actual work — the more real, the better the signal.)*

### Scoring (per question, 0–2 each)
| Axis | 0 | 1 | 2 |
|---|---|---|---|
| **Completeness** | missed relevant meetings | found some | found ~all relevant meetings |
| **Correctness** | wrong / hallucinated | mostly right | accurate + cited `[[Title]]` |
| **"Reads like a brain"** | fragmented dump | partial synthesis | coherent synthesis across meetings |

Record per question; total each axis. A simple sheet:
```
Q#  Category  Completeness  Correctness  Brain   Notes (what it missed / nailed)
1   A         2             1            2       ...
...
```

### Decision rule (Stage 1)
- **Avg ≥ ~1.5 across all axes, esp. on B/C (paraphrase/cross-lingual):** FTS5 is already strong → the vector layer is likely **low ROI at your scale**. Keep FTS5 live, **deprioritize 2c/Phase-3 vector spend**, focus the brain on synthesis/actions instead. (This is a perfectly good outcome — it saves a +1–2GB model.)
- **B/C consistently ≤ 1 (it whiffs on paraphrase / PL↔EN / entity-spread):** that's exactly the gap the vector + graph layer fills → **proceed to Stage 2** (real embedder), then re-run to A/B.
- **A (entity-anchored) weak but B/C ok:** the win is **GraphRAG-lite** (entity expansion), not raw vectors — note that; it shifts which model size we need.

**Send me the filled sheet + your read** and I'll set the next priority from real evidence, not assumptions.

---

## Stage 2 — A/B FTS-only vs hybrid (after the real embedder lands, Phase 2c)

Once 2c wires a real on-device embedder (BGE-M3 via mistral.rs/llama.cpp or fastembed) and a settings toggle:

1. Run the **same** question set with `semantic_search_enabled = OFF` (FTS only) → score.
2. Flip it **ON** (hybrid: FTS ∪ vector ∪ entity-graph) → re-run the same questions → score.
3. Compare the two score sets question-by-question.

### What we're measuring
- **Δ on B/C** (paraphrase + cross-lingual) — the vector layer's home turf. If hybrid clearly wins here, vectors earn their keep.
- **Δ on A** (entity-anchored) — GraphRAG-lite's contribution.
- **Polish recall specifically** — does BGE-M3 retrieve correctly on your *spoken, code-switched, ASR-errorful* Polish? (the single biggest unmeasured unknown). If hybrid is no better — or worse — on Polish, the embedding model is wrong; try the alternate (mE5 / a Polish-native like Bielik for NER).
- **Latency / RAM** — note tok/s + whether it's comfortable alongside whisper large-v3 on your machine (the 14B-vs-32B default decision rides on this).

### Decision rule (Stage 2)
- Hybrid clearly beats FTS on B/C + Polish acceptable → **ship the vector stack** (flip the default on, after a confidence period).
- No meaningful Δ → **vectors don't earn the bundle cost at your scale**; keep FTS5 default, leave semantic as an opt-in.
- Good Δ but Polish recall poor → **swap the embedding model**, re-run.

---

## Stage 2b — AUTOMATED metric harness (`eval::bakeoff`) — recall@k / nDCG@k / MRR

The human 0–2 scoring above is the gold signal, but it's slow and subjective. The `eval::bakeoff` module (`src-tauri/src/eval/`) automates the comparison: give it a **labeled set** (queries + the meeting ids that *should* be retrieved) and it runs all three legs — **FTS-only**, **semantic-only** (vector KNN), and **hybrid** (RRF fusion) — and prints **recall@k**, **nDCG@k**, and **MRR** per mode. Same visibility gating as the app (sealed-not-unlocked meetings are invisible to the eval).

### 1. Build a labeled set (JSON)
Format = a top-level array of `{ query, lang, expected_meeting_ids }`. A sample lives at `src-tauri/src/eval/fixtures/rag-bakeoff-sample.json` — copy it and fill the `expected_meeting_ids` with **real meeting ids from your DB**.

To find meeting ids, read your dev DB (see the "inspect the dev DB" recipe): the ids are `meetings.id`. Reuse the Stage-1 question set — you already know which meetings each question *should* pull. Example:
```json
[
  { "query": "kiedy ustaliliśmy deadline na integrację API", "lang": "pl",
    "expected_meeting_ids": ["mtg_abc123"] },
  { "query": "what did we decide about the Q3 budget", "lang": "en",
    "expected_meeting_ids": ["mtg_def456", "mtg_ghi789"] }
]
```

### 2. Point the harness at a real DB + set and run it (on your Mac)
The end-to-end run is an `#[ignore]`d test driven by env vars (no recompile needed to change the set):

```bash
source ~/.cargo/env
# Copy your dev DB first (WAL-safe): the dev app writes MeetNotes-dev/meetnotes.sqlite with the dev DEK.
cp ~/Library/Application\ Support/MeetNotes-dev/meetnotes.sqlite* /tmp/   # copy the .sqlite + -wal + -shm

cd src-tauri
MURMUR_BAKEOFF_DB=/tmp/meetnotes.sqlite \
MURMUR_BAKEOFF_DEK=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
MURMUR_BAKEOFF_SET=/path/to/your-labeled-set.json \
MURMUR_BAKEOFF_K=5 \
cargo test --lib eval::bakeoff::tests::run_bakeoff_over_real_db_from_env -- --ignored --nocapture
```
- `MURMUR_BAKEOFF_DEK` = the dev DEK (above) for a dev DB; the real Keychain DEK for a release DB.
- The embedder is `active_embedder()` — the **real model when its files are on disk**, else the stub (it prints a WARNING; stub numbers are NOT a quality signal, so download the model first).
- To include a sealed folder, unlock it in the app, then copy the DB (its chunks are only present while unlocked).

### 3. Read the output
A table like:
```
RAG bake-off — 18 queries, k=5
mode          recall@5     ndcg@5        mrr
--------------------------------------------
fts             0.6111     0.5487     0.6389
semantic        0.7222     0.6841     0.7500
hybrid          0.7778     0.7213     0.7917
```
- **recall@k** — did the right meetings show up in the top-k at all (coverage).
- **nDCG@k** — were they ranked *high* (position-weighted).
- **MRR** — how high was the *first* correct hit.
Higher is better on all three (0–1). If **hybrid** beats **fts** on your PL + paraphrase queries, the vector layer earns its keep; if **fts ≈ hybrid**, it doesn't at your scale. Compare **semantic** vs **hybrid** to see whether fusion helps or the raw vectors are enough.

### 4. Comparing embedding MODELS (e5 vs mmlw-e5-small)
Two embedders ship as first-class selectable options (both BERT / 384-dim, so switching needs **no** DB migration — only a re-index):
- `multilingual-e5-small` (default, intfloat) — general multilingual.
- `mmlw-e5-small` (sdadas) — a **Polish-first** distilled e5, strong PL-MTEB retrieval.

To bake them off against each other on your Polish queries:
1. In the app, pick the model (or call the `select_embed_model` command with `"multilingual-e5-small"` / `"mmlw-e5-small"`) → download it (`download_embed_model`) → **re-index** (`reindex_embeddings`, required because a different model's vectors aren't comparable).
2. Copy the DB and run the harness above → record the table.
3. Switch the model, re-index, re-run → compare the `semantic`/`hybrid` rows. The model with the higher PL recall@k / nDCG@k wins for your vault.

*mmlw config verified: BERT architecture (`model_type: bert`, loadable by candle's `BertModel`), `hidden_size == 384` (matches `EMBED_DIM`, zero schema change), and the same `"query: "`/`"passage: "` asymmetric prefix convention as e5 (per its HF card).*

### Metric math is unit-tested (no model needed)
The recall@k / nDCG@k / MRR implementations are pure and covered by deterministic unit tests over synthetic rankings (`cargo test --lib eval::` — runs in the normal loop). The *real* run above is the only part that needs a Mac + the model.

---

## Honesty notes
- Stage 1 is fully doable **today** (FTS5 is live) and is the cheaper, decision-first gate — do it first.
- Stage 2 needs the real embedder + your signed/dev build on a real Mac; headless tests can't measure retrieval *quality* or Polish recall.
- Stage 2b (`eval::bakeoff`) gives OBJECTIVE recall@k/nDCG@k/MRR numbers, but they're only as good as your labeled set — label from real questions where you *know* the right meetings, and keep the set stable across runs so model/mode comparisons are apples-to-apples.
- Keep the question set stable across runs so scores are comparable. Real questions from your actual work give far better signal than synthetic ones.
