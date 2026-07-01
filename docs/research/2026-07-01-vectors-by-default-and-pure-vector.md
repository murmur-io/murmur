<!-- Generated 2026-07-01 via /research (murmur-researcher fan-out, 3 angles). Model sizes/benchmarks = point-in-time. -->
# Research: Vectors on by default + "everything on vectors" — should we, and how?

## TL;DR / Verdict

Two distinct questions, two clear answers:

1. **"Everything on vectors" (pure dense, drop FTS+graph)? → NO, that's a downgrade.** The 2024–2026 consensus (Anthropic's own contextual-retrieval study, Weaviate/Qdrant guidance, the Polish PIRB benchmark) is unanimous: **hybrid (dense + BM25, RRF) beats either alone**, and the places dense *loses* are exactly Murmur's content — proper nouns (people/project names), exact tokens/IDs, and the entity-graph relationship queries that are the product's identity. Murmur is **already three-leg hybrid** (`search_hybrid_visible`: FTS ∪ vector-KNN ∪ entity-graph, RRF k=60). The right framing is **"hybrid-by-default"**: keep FTS+graph as the permanent substrate, promote the vector leg from opt-in to a default fusion member — never a replacement.

2. **Vectors on by default? → YES in principle, but gate it on evidence, and it's a delivery problem not a code problem.** The blocker isn't the flag (a ~1-line default + a first-run backfill hook); it's that semantic search is meaningless without the e5 model present (no model → `StubEmbedder` = hash noise), the model is ~470MB downloaded-not-bundled, and **semantic quality + Polish recall are UNPROVEN** (`docs/RAG-BAKEOFF.md` unrun; `cargo test` never runs a real e5 forward pass). So: **run the bake-off first**; if hybrid ≥ FTS-only on every query bucket, flip to hybrid-when-model-present with a consented first-run auto-download + one-shot backfill.

## Co już mamy (z repo, file:line)

- **Retrieval is HYBRID, not pure-anything:** `search_hybrid_visible` fuses FTS5/BM25 (`search_visible`) ∪ vector-KNN (`search_semantic_visible`) ∪ entity-graph neighborhood by RRF (`db.rs:1299-1354`; `rrf_fuse` k=60 `embed.rs:35,244`).
- **Vectors are DORMANT by default:** `semantic_search_enabled=false` (`config.rs:234`); Ask-My-Vault takes the FTS-only path when off (`commands.rs:1877`); `search_semantic` tool returns "disabled" (`tools.rs:218`). So the *shipping default* is effectively **FTS + graph**.
- **The stub trap:** `active_embedder()` returns the real e5 `CandleBertEmbedder` only when `embed_model_present()`, else a semantically-meaningless hash `StubEmbedder` (`embed.rs:104-161`). Fresh install → no model → stub.
- **Embedded today = the NOTE markdown only** (`chunk_note`, `embed.rs:214`; auto-index at Stop gated by `should_auto_index(flag, model_present)`, `pipeline.rs:609,959`). Transcript segments are NOT embedded (a `source_type` column exists but is always `'voice'`, `db.rs:449` — a designed-for-later seam).
- **Lock posture already covers vectors:** purge-on-lock in the seal tx (`purge_chunks_tx` `db.rs:1103`, wired into every lock path); every read visibility-gated (`search_semantic_visible` via `visibility_clause` `db.rs:1144-1154`). Confirmed: MORE vectors = SAME posture, no new leak surface.
- **Download plumbing already ships:** `download_embed_model` / `embed_model_present` commands + FE progress stream (`embed.rs:267`, `ipc.service.ts:543-565`). Precedent: the ~3GB whisper `large-v3` is already downloaded-not-bundled (`transcribe/model.rs:75`).

## Findings

### Angle 1 — Model delivery (bundle vs download vs smaller)
- **No small multilingual model wins.** e5-small (470MB, 384-dim, PL-MTEB 53.11) and paraphrase-multilingual-MiniLM (same 470MB, PL-MTEB 45.89) are the same size — both carry a ~250k-token multilingual vocab (~384MB is the embedding matrix alone). The genuinely small models (all-MiniLM-L6 90MB, bge-micro 34MB, potion 30MB) are **English-first → weak Polish**. **There is no <150MB fp32 BERT embedder that is both 384-dim and good at Polish.** [PL-MTEB arXiv:2405.10138; HF file sizes]
- **Bundling is mechanically fine but bad UX:** 81MB → ~550MB DMG (6×) for an opt-in feature; notarizable, just slower. Nobody bundles a 470MB multilingual model — the apps that bundle (Smart Connections, AnythingLLM, Jan, LM Studio) bundle *small English* models and accept weak non-English recall.
- **Best lever = fp16 e5 (~235MB), keeps 384-dim + candle, no new dep** — but needs a Mac spike to prove candle's f16 forward matches fp32 Polish recall. INT8-ONNX (118MB) exists in the same repo but needs a new `ort` dep (x86-targeted build).
- **Recommendation: keep e5-small, deliver by consented auto-download on first run** (reuse the shipped whisper-download precedent + existing FE plumbing) — spike fp16 as the follow-up size win. Confidence: high.

### Angle 2 — Hybrid vs pure-dense at personal scale
- **Hybrid is the 2025-2026 consensus.** Anthropic: embeddings-only 5.7%→3.7% failure, **+BM25 → 2.9%** (BM25 "particularly effective for unique identifiers / technical terms"). Weaviate/Qdrant/Elastic all ship hybrid RRF as the default recommendation. [anthropic.com/news/contextual-retrieval]
- **Dense LOSES exactly where Murmur lives:** exact proper-noun / ID / rare-token matching — meeting notes are name-heavy (people, projects), and the `[[Person]]/[[Project]]` graph is the product. Dense diffuses "what did Kowalski commit to on Atlas?" across semantically-adjacent names; BM25 nails the literal token.
- **Polish cuts both ways → argues FOR hybrid:** dense recovers inflected common-noun recall (budżet/budżetu…) that lexical misses; BM25 (our FTS is `unicode61 remove_diacritics 2`) anchors the names. PIRB: dense > BM25 on Polish, **hybrid > dense**. [arXiv:2402.13350]
- **Small corpus = dense's advantage is smallest here.** At ~10²–10³ chunks, FTS+graph already covers most queries; the vector leg's gain (paraphrase, cross-lingual, "that meeting about hiring") is real but marginal → augment, not replace.
- **Dropping the graph leg is a real loss** for multi-hop/relationship queries (GraphRAG systematic eval: graphs win complex reasoning). Our graph leg is cheap + deterministic (reuses `entities`/`entity_mentions`, no LLM). Confidence: high.

### Angle 3 — Safe default-flip + what to embed
- **Blast radius of flipping the flag** = 9 read sites (`pipeline.rs:609`, `tools.rs:218`, `commands.rs:1877/1049/3012`, `voice_action.rs:322`, config round-trip, `mcp.rs:341`, `config.rs`). All traced.
- **The raw flip is inert / misleading alone:** `should_auto_index(true, false)==false`; existing users have an EMPTY `vec_chunks` until a MANUAL reindex — hybrid degenerates to FTS. **Don't flip the raw default alone.**
- **Safe path:** effective-on = `model_present`, + a **one-shot best-effort first-run backfill** hooked in `lib.rs setup()` (spawn a background task guarded by `embed_model_present()` + an idempotent `embeddings_backfilled` flag, calling the existing visibility-gated `reindex_embeddings_inner`). Per-Stop auto-index is already bounded + best-effort (never fails the pipeline, `pipeline.rs:616-620`).
- **What to embed: NOTE ONLY.** The transcript is 5-15× the index, noisy, low-precision — and FTS-over-segments already covers its literal-recall gain. The note is the LLM+human distillation. Keep the `source_type` seam unused until a bake-off proves note-only is the bottleneck. Documents already ride a separate, correct parallel store (`doc_chunks`/`doc_vec_chunks`).
- **Lock confirm:** more vectors = same posture (purge-on-lock + gated). The ONE rule: any *new* vector table must be added to `purge_chunks_tx` + startup reconcile + a gated `*_visible` reader + lock-security review. Confidence: high.

## Fit z ograniczeniami Murmur
- **Local-first:** auto-download is inbound-only (no content egress) and consistent with the shipped whisper download — but a *consented* first-run fetch, not silent. Bundling would be marginally more "offline-pure" but the app already fetches whisper, so it buys little.
- **SQLite-canonical:** hybrid is ideal — FTS5 + sqlite-vec both live in the one SQLCipher DB; keeping e5 at 384-dim = zero vec0 migration.
- **Lock model:** every leg already visibility-gated + purged-on-lock; unchanged by volume/default.
- **macOS / CI honesty:** semantic quality, Polish recall, Metal perf, per-note embed latency are ALL Mac-only (`candle_bert.rs:5-18` — CI proves plumbing via the stub, never recall). "On by default" is a quality claim `cargo test` cannot make.

## Opcje i tradeoffy
- **Reject: pure-dense / "everything on vectors"** (M effort, negative value) — loses name/ID precision + the graph, breaks with no model, contradicts consensus.
- **A — status quo (vectors opt-in, dormant)** (S) — safe, but users must find the toggle + download + manual reindex.
- **B — hybrid-by-default when model present** (S–M, RECOMMENDED) — keep FTS+graph, make the vector leg a default fusion member once e5 is downloaded; consented first-run auto-download + one-shot backfill. Gate the flip on the bake-off.
- **C — B + embed transcript** (L) — defer; large cost, marginal/noisy recall, FTS-over-segments already covers it.

## Rekomendacja i pierwszy krok
**Adopt "hybrid-by-default", gated on evidence — never pure-dense.** The single de-risking step is the **RAG bake-off on a real Mac with e5 present** (`docs/RAG-BAKEOFF.md`): ~20–30 representative queries against a real vault, deliberately including (a) exact proper-noun/project, (b) Polish inflected-form, (c) paraphrase/intent, (d) multi-hop entity — compare FTS-only vs FTS+vector vs full three-leg hybrid on recall@k. **If hybrid ≥ FTS-only on every bucket** (expected from the literature), then implement Option B: consented first-run e5 auto-download + a one-shot `lib.rs setup()` backfill + effective-on = model-present. Until the bake-off, leaving vectors opt-in is defensible; dropping FTS/graph is not. (This needs a signed/dev Mac — headless `cargo test` can't prove it.)

## Otwarte pytania / czego nie udało się zweryfikować
- **Murmur's real Polish + name retrieval quality is UNMEASURED** — the single biggest gap; all Polish/small-corpus claims are literature applied to our schema, not our index. Needs Mac + e5 + a labeled query set.
- **fp16 e5 recall parity in candle + Metal** — plausible, unproven (Mac spike).
- **Back-catalog reindex wall-clock** at realistic vault sizes (200–500 notes) — the reindex loop is serial; first backfill could take a noticeable background chunk.
- **RRF per-leg weighting** — our `rrf_fuse` is unweighted; whether weighting BM25 vs dense helps Murmur is an untested tuning question.

## Sources
**Web:** Anthropic Contextual Retrieval (anthropic.com/news/contextual-retrieval); Weaviate hybrid fusion (weaviate.io/blog/hybrid-search-fusion-algorithms); PIRB Polish retrieval benchmark (arXiv:2402.13350); PL-MTEB (arXiv:2405.10138); "When to use Graphs in RAG" (arXiv:2506.05690); HF file sizes (huggingface.co/api/models/intfloat/multilingual-e5-small?blobs=true); Obsidian Smart Connections / AnythingLLM / LM Studio / Khoj embedder-delivery docs.
**Code:** `embed.rs:35,104,146,214,244,267`; `embed/candle_bert.rs:5-18`; `storage/db.rs:442-465,449,1103,1144-1154,1299-1354`; `pipeline.rs:609,756,959`; `commands.rs:1877,3042,3049`; `settings/config.rs:234`; `lib.rs:169-231` (setup, the backfill hook); `transcribe/model.rs:75` (whisper download precedent).
