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

## Honesty notes
- Stage 1 is fully doable **today** (FTS5 is live) and is the cheaper, decision-first gate — do it first.
- Stage 2 needs the real embedder (Phase 2c) + your signed/dev build on a real Mac; headless tests can't measure retrieval *quality* or Polish recall.
- Keep the question set stable across runs so scores are comparable. Real questions from your actual work give far better signal than synthetic ones.
