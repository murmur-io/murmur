<!-- Generated 2026-07-18. Research-grounded audit of the merged brain v3 program (PRs #362–#367 + OCR #369,
     with #368 interactions), pinned at trunk 71bac31. Method: 60-agent workflow — 6 SOTA research briefs
     (web + code grounding) + 9 parallel code auditors + adversarial verification of every critical/high/medium
     finding (33 verified, 1 refuted, 32 survived) + independent grading pass. All claims below cite file:symbol
     and survived at least one dedicated refutation attempt unless marked low/info (unverified pass-through). -->

# Audit: Brain v3 — implementation vs AI-engineering best practice (2026-07-18)

**Scope:** the merged brain-v3 program — PR-1 #362 integrity, PR-2 #363 universal ingest + hierarchy,
PR-3 #364 link engine, PR-4 #365 full-brain graph, PR-5 #366 Receipts, PR-6 #367 Knowledge Diff,
follow-up #369 Vision OCR — audited as it lives on trunk `71bac31` (which also carries #368's manual-link
layer on top). Basis of comparison: the original user prompt, `docs/research/2026-07-17-brain-v3-maximization.md`,
`docs/superpowers/specs/2026-07-17-brain-v3-design.md`, and six fresh SOTA research briefs (contextual
retrieval / RAPTOR / LazyGraphRAG / context-rot / Karpathy context engineering; Docling-class extraction;
e5 hubness & threshold literature; Zep/Graphiti bitemporal KG; AIS/NLI attribution; Anthropic/SWE-agent/A-RAG
ACI design + July-2026 competitor reality).

## TL;DR verdict

**A disciplined, adversarially-verified implementation with a genuinely SOTA-adjacent security layer and an
architecturally-ahead-of-market substrate — whose product-visible quality still rests on uncalibrated
constants and a handful of confirmed retrieval-quality bugs.** The program delivered ~90% of its spec.
Where it is best (lock model, verification process, bitemporal core, suggest-then-accept linking policy) it
exceeds every named competitor and some published research systems. Where it falls short, the pattern is
consistent: *the architecture matches SOTA, the last mile of the mechanism doesn't* — parent expansion is
query-independent, the L2 outline degenerates to one truncated chunk, the kNN fan-out is chunk- not
item-granular, receipts are negation-blind, the agent loop truncates silently. None of these is visible to
`cargo test`; all were confirmed by adversarial verification against the code. The three headline claims
(500-page PDF fidelity, 0.80/0.88 link precision, receipt coverage on paraphrased notes) remain unvalidated
on real corpora — the program's own research gated release quality on spikes that have not run.

### Scorecard

Grades are this audit's synthesis of the six research briefs' criteria applied to the verified code findings
(an independent grader pass ran as well and landed within ±1 on every dimension). 10 = would impress the
authors of the cited SOTA work; 5 = competent but clearly behind SOTA.

| Dimension | Score | One-line justification |
|---|---|---|
| Lock-model security | **9** | purge-on-seal at every choke point, both-endpoint gating, in-tx TOCTOU guards — all RED-proven; two residual composition gaps, both bounded |
| Engineering quality & tests | **8.5** | suite 1762→1850, overwhelmingly RED-capable; dual-verify caught real bugs in 4 of 6 PRs; minus: God-file growth, several spec promises silently dropped |
| Ingest & extraction | **7.5** | crash-safe FFI + actual-bytes zip-bomb guard exceed industry practice; minus: nested-table/PPTX-table/localized-heading silent losses, plain-text in a structured-elements world |
| Knowledge Diff / bitemporal | **7.5** | genuine invalidate-not-delete + pure boundary-tested interval algebra (no consumer competitor has any); minus: time axes collapsed (as-of = knowledge-time), inert FE control |
| Semantic linking math | **7** | mutual-kNN is the literature's recommended hubness defence and the policy layer beats all shipped competitors; minus: chunk-granular fan-out bug + uncalibrated e5 thresholds |
| Full-brain graph | **7** | typed nodes/edges with exemplary gating and honest backend caps; minus: silent FE 140-node draw cap, no search/expand/cluster affordances (below Bloom/Kumu) |
| Performance & scalability | **7** | ingest properly off-thread/RAM-gated/sub-batched with excellent crash posture; minus: `update_note_doc` heavy-permit bypass, O(k·n) linker with false O(k·log n) claims, backlinks full-vault scan |
| Hierarchical index & retrieval | **6.5** | right architecture (L0/L1/L2, zero-LLM ingest, contextual headers); three of four load-bearing mechanisms partially broken (expansion, L2, PDF leaves) |
| Receipts / grounding | **6.5** | honest abstain-over-wrong posture and a real structural moat; minus: lexical-only alignment is negation-unsafe, coverage unmeasured, front-matter bug |
| Agentic ACI | **6.5** | paging + MCP mirrors + no-repeat guard match good practice; minus: silent 4k truncation (the "agents lie" class), doc map persisted but never surfaced |

**Overall ≈ 7.3/10** — excellent substrate and process; the gap to "credibly the best AI brain" is now
mostly *empirical calibration + last-mile mechanism fixes*, not architecture.

## 1. Conformance to the original prompt

| Prompt requirement | Status |
|---|---|
| Ingest documents of ANY size (PDF/DOCX/…) into brain contexts | **Delivered** (all formats + OCR), with silent-loss caveats (nested/PPTX tables, localized headings) and unvalidated 500-page fidelity |
| Analyze how the brain holds data; fix gaps/refactor | **Delivered** — the PR-1 audit found real staleness bugs and fixed all six, plus a TOCTOU nobody had asked about |
| Manual `[[links]]` + automatic content-similarity linking | **Delivered** — rename-proof wikilink edges by ID + mutual-kNN suggester; precision uncalibrated, fan-out bug confirmed |
| Visualize ALL connections, not just people | **Delivered** — typed 4-kind/5-edge graph; but the FE silently draws at most 140 nodes while captions report more |
| 2 killer features, maximally polished | **Delivered** (Receipts, Knowledge Diff) — both real moats, both with the polish gap in the last mile (negation-unsafe receipts; as-of diff computed but never rendered) |
| "Mathematically grounded, not empty words" | **Largely honest** — cos identity unit-tested, pure deterministic cores, named consts; but two documented complexity claims are false (O(k·log n)) and thresholds are uncalibrated start values |
| "Powerful tool for SHARING huge contexts (e.g. via org)" | **NOT delivered at "huge"** — org sharing of documents exists but is hard-capped at 1 MiB/item (`murmur_protocol::caps::MAX_ORG_ITEM_BLOB_BYTES`, terminal `too_large`); a 500-page PDF's extracted text typically exceeds it, so the exact artifact the program exists to ingest cannot be org-shared. The v3 append-only envelope was consciously deferred — but relative to the prompt this is the largest open scope item. |

## 2. Where the implementation MATCHES or EXCEEDS SOTA (verified)

- **Lock discipline (9/10, the program's crown).** `purge_links_tx` + `purge_doc_chunks_tx` fire inside all
  eight seal/relock/delete/reconcile transactions; `links_for_visible` gates BOTH endpoints fail-closed
  before touching a row; `build_full_graph` drops any edge whose endpoint is sealed (no existence leak);
  the PR-1 seal-vs-index TOCTOU fix keys on a session-independent at-rest invariant (`doc_sealed_at_rest_tx`)
  *inside the write tx, before the purge*, composing correctly with WAL snapshot semantics; the repair tick
  and consolidation run under the EMPTY unlock set; receipts are recompute-on-demand with a content-free DTO.
  No cloud SOTA system (Zep included) has an equivalent of purge-on-seal for derived knowledge.
- **Verification process.** Every PR dual-verified; verifiers caught real bugs the implementers missed
  (PR-1 TOCTOU, PR-5 dead FE code, PR-6 lexical-timestamp compare, #368's HIGH mislabel). The eval harness
  forced a measured revert of a tempting fusion change. This is the discipline most teams claim and few run.
- **Extraction safety.** Every PDFKit/Vision/AppKit ObjC call inside `objc2::exception::catch` (per-page
  granularity — one bad page never kills an import); the zip-bomb guard bounds *actual inflated bytes* via
  `Read::take` with one budget across entries (stronger than the industry's ratio-only screening); OCR bitmaps
  RAM-capped at ~16 MiB/page; import write-gate re-checked inside the blocking closure against a racing relock.
  Polish Vision OCR verified empirically on this Mac.
- **Linking policy layer.** Suggest-then-accept with a dismissed-tombstone + no-downgrade upsert
  (`ON CONFLICT … WHERE status != 'dismissed'` + CASE guard) is more disciplined than Smart Connections,
  Reflect, or Mem — none of which writes silently into user files either, so the differentiation is the
  *integration* (purge-on-seal, canonical undirected edges, both-endpoint gates), which nobody else has.
- **Bitemporal core.** `reconcile_facts` invalidate-not-delete + `snapshot_as_of` half-open intervals with
  instant compare (`cmp_instant`) and boundary tests incl. Z-vs-+00:00 cross-rendering — matches the
  SQL:2011/Zep convention and is *better-engineered for auditability* than Graphiti's LLM-in-the-loop
  invalidation (pure, deterministic, clock-injected).
- **ACI positives.** Search-first tool catalog, char-safe `page_text` with `[end of content]` marker, MCP
  mirrors, no-repeat guard (≈ A-RAG's context tracker), deterministic 32k transcript compaction with
  surviving markers (≈ Anthropic's compaction guidance).

## 3. Confirmed findings (survived adversarial refutation)

33 critical/high/medium claims went to verification (1–2 independent skeptics each, instructed to refute);
1 was refuted, 32 survived. The three HIGHs:

### H1 — Semantic-link kNN fan-out is CHUNK-granular; an item's own chunks starve its candidate list
`db.rs: Db::knn_items_visible / Db::auto_link_semantic`. The spec promised "k=10 **items** over
vec_chunks ∪ doc_vec_chunks"; the shipped query fetches k+1=11 **chunks per table**, and since an item's
own chunks dominate proximity to its own centroid, any item with ≥11 chunks (every long meeting note, every
real document — i.e. exactly the content-rich items the feature exists for) gets few or zero same-table
candidates, and the mutuality back-probe fails the same way. The links.rs unit tests pin only the pure
selection math on synthetic cosines, so this never shows in `cargo test`.
**Fix (S):** over-fetch chunks (k≈64; brute-force vec0 makes larger k nearly free) or exclude the source's
own chunk ids in the CTE, then truncate to K distinct items; same for the back-probe.

### H2 — DOCX nested tables silently destroy already-extracted text
`extract/ooxml.rs: parse_docx_xml`. An inner `w:tc`/`w:tr` fires `para_text.clear()`/`row_cells.clear()`
while still inside the outer cell — outer-cell text and earlier sibling cells are wiped. Nested tables are
routine in real Word documents (layout tables, forms, agendas). The import "succeeds", the FE toasts
success, and the brain silently cannot retrieve the lost text — silent index-content loss.
**Fix (S):** table-depth counter or a stack of (row_cells, para_text) frames + a RED fixture test.

### H3 — Authored-note save embeds + auto-links on the async runtime, bypassing the heavy-inference permit
`commands.rs: update_note_doc`. Runs Candle/Metal embedding + the semantic-link pass synchronously without
`perf::run_heavy` and without `embed_in_sub_batches` — the exact co-residency + unbounded-Metal-tensor
classes PR-1 fixed on its meeting twin `update_note`. A textbook instance of the repo's own
"sibling functions carry the same bug" heuristic; can co-run with an in-flight whisper pass (the documented
launch-freeze class).
**Fix (S):** wrap in `run_heavy` like the sibling; route `index_note_chunks` through `embed_in_sub_batches`.

### Mediums, grouped (all CONFIRMED unless marked PARTIAL)

**Retrieval quality (the "last mile" cluster — these blunt the hierarchy's whole point):**
- `db.rs: expand_doc_parents_visible` — parent expansion is **query-independent**: it serves the document's
  *dominant-by-leaf-count* L1 section, not the hit leaf's parent (the spec's "≥2 sibling L0 hits" trigger is
  unimplementable because both doc readers dedup to one snippet per doc before chunk ids survive), and flat
  docs DO get an L1 (their first ~6000 chars) contrary to the in-code comment — so for multi-section and
  flat docs alike, expansion replaces the relevant retrieved snippet with up to 6k chars of the WRONG
  section. Per Chroma's context-rot data this is a distractor *injector*. Fix: carry `chunk_id`/`parent_id`
  through `DocChunkHit`, dedup after fusion, expand to the hit's parent only when ≥2 retrieved siblings
  share it (LlamaIndex auto-merging semantics).
- `embed.rs: chunk_document_hierarchical` — the L2 outline is always exactly ONE unbounded chunk (outline
  joined with `\n` defeats `pack_leaves`' `\n\n` split; `truncate(3)` is dead code), so e5's 512-token
  window embeds only the first few dozen sections — the collapsed-tree effect is gutted for precisely the
  large docs it was built for. FTS still indexes the full outline, so the degradation is vector-side and
  invisible to tests.
- `embed.rs: pack_leaves` — PDF (the flagship format) emits one block per page with no blank lines, so L0
  "800-char" leaves are routinely whole pages whose tails never reach the vector index; sizes are counted
  in BYTES not chars (Polish ≈ 720 effective chars, CJK ≈ 266). Fix: hard-split fallback on `\n`/sentence
  boundaries; count with `chars()`.
- `agent.rs: truncate / RESULT_BUDGET` — tool results are cut at 4000 chars **silently**: no marker, no
  total. The model cannot distinguish "the doc is 4k chars" from "I saw 0.3% of it" — the documented
  "tool-result truncation makes agents lie" class: Ask can confidently assert *"the document doesn't mention
  X"* after seeing 1.6% of a 500-page PDF. (On MCP the opposite: default `(0,0)` returns the ENTIRE body.)
  Fix: `[truncated at 4000 of N chars — call again with offset]` + `TOTAL_CHARS` in windowed reads. The
  research brief also notes: `section_path`/`page_no`/`level` are *persisted but never surfaced* to the
  agent (`DocChunkHit` drops them; no `get_document_outline` tool) — the doc map exists in SQLite and the
  agent pages blind by char arithmetic.

**Extraction fidelity/availability:**
- `extract/pdf.rs: extract_pdf` — scanned-PDF OCR is all-or-nothing: ONE page with a text layer (a
  publisher cover page) suppresses OCR for the other 299 → ~100% of content silently absent. Fix: per-page
  fallback (OCR pages whose text layer is empty/short).
- `extract/ooxml.rs: parse_pptx_slide` — PPTX table text (`a:tbl` in `graphicFrame`) is dropped entirely;
  table-only slides (status grids, budgets) ingest empty, silently.
- `extract/pdf.rs: ocr_scanned_pdf` — a 500-page scanned PDF OCRs unboundedly (~4–15+ min) while holding
  the ONE heavy-inference permit — ASR/summarize for a meeting started meanwhile queue behind it; no page
  cap, no cancellation, no per-page progress.
- `extract/mod.rs: MAX_EXTRACT_DECOMPRESSED_BYTES` — the 512 MiB guard covers only zip containers; PDF
  content-stream inflation, calamine's in-memory expansion, and flow-format reads are unbounded, and the
  RAM floor gates only the embed stage, not extraction.
- Low but pointed (research-confirmed, hits the primary user): **localized Word heading styles are never
  detected** (`heading_level()` matches only the English `heading` prefix) — a Polish-authored DOCX
  (`Nagłówek1`) gets `heading_path=None` everywhere, degrading the entire hierarchy for the app's
  primary-language documents. Fix: resolve `w:outlineLvl` via `styles.xml` (language-agnostic).

**Link-engine state machine across seal cycles (needs a lock-security review as a set):**
- `db.rs: purge_links_tx` destroys dismissed tombstones and accepted status on seal → a lock→unlock cycle
  **resurrects dismissed suggestions**, contradicting the spec's "a rejected suggestion never reappears".
- `commands.rs: rederive_links_for_folder` re-derives wikilink+semantic only → **companion edges are
  permanently deleted** by one lock cycle (backfill sentinel is one-shot); **inbound wikilinks** from
  outside sources into the unlocked folder are purged and never restored.
- `db.rs: auto_link_semantic` — the cap 5 is source-side only: hub nodes accumulate unbounded inbound
  suggestions (the exact hubness noise mutual-kNN was meant to prevent).
- PARTIAL: the spec's **indexed backlinks fast path over `links` was silently not built** —
  `backlinks_for_visible` remains an O(entire-vault-text) regex scan per note/detail open, under the DB
  mutex (tens-to-hundreds of ms on a big vault, growing linearly).

**Lock-model composition gaps (the between-PR class):**
- `commands.rs: materialize_accepted_link / merge_related_hit` — a SYSTEM-written `[[Title]]` of a
  later-sealed neighbour survives in a visible note's plaintext body AND its exported vault `.md` — the
  same title+existence information the links-row purge exists to remove. Fix: strip matching hits from the
  machine-owned `murmur:links` block at seal (the purged rows name exactly the affected sources), or
  document the residual in lock-model.md as accepted.
- Low/PARTIAL: link writers (`index_wikilinks_for_source`/`auto_link_semantic`/`upsert_manual_link`) lack
  the in-tx sealed-at-rest re-check PR-1 added to every chunk indexer — a seal-vs-link-derive TOCTOU can
  persist id+score rows referencing sealed endpoints until the next relock/reconcile (reads stay gated;
  exposure bounded). Mirror `doc_sealed_at_rest_tx` in the link-write tx.

**Receipts:**
- `grounding.rs: align_claims_to_segments` — never splits YAML front-matter (contrary to its caller's
  comment): `attendees: Anna, Bob` earns a receipt chip if the names were spoken. Fix: `split_frontmatter`
  + index offset.
- Negation-blind: `not`/`nie` are stopwords, so a claim can earn a high-overlap receipt from a segment
  asserting the OPPOSITE — the one spec-flagged failure mode that fails UNSAFE (paraphrase/aggregation/short
  claims all fail safe via threshold refusal). Fix: keep negators as content tokens for this pass, or veto
  on one-sided negator presence. Related: the FE copy "Click to hear the moment this was said" overclaims
  for a 0.5 lexical match — the attribution literature is unanimous that lexical overlap is the bottom tier;
  say "likely source", show tiers not floats.
- Low: the spec's "ASR confidence in the tooltip" never shipped (fetched, then dropped by the FE).

**Knowledge Diff:**
- `facts.rs: supersession_ledger` — still sorts/pairs by byte-lexical `valid_from.cmp()`; the
  instant-compare fix stopped at `snapshot_as_of`. Mixed RFC3339 renderings (imported/shared facts) can
  flip old→new in the ledger — the exact class `cmp_instant` was added to kill.
- `entity-detail`: the 90d/All-time control is functionally inert (bounds a diff the template never
  renders) and the headline as-of diff (added/removed/changed) has **no human-facing UI at all** —
  MCP/agent text only. Shipped-but-dead machinery; render it or delete the control.
- Research-confirmed structural note: the two time axes are COLLAPSED — `valid_from` is always the
  reconcile instant, so "as of t" answers *knowledge-time*, not world-time; retroactive facts ("shipped
  last Tuesday") are mis-dated. Zep's `t_ref` pattern (extractor emits an optional 'since', resolved
  against the meeting date) would make the bitemporal schema actually bitemporal with no change to the
  deterministic core.

**Graph & perf hygiene:**
- FE draws at most `MAX_NODES=140` nodes while caption/banner report up to 2000 — the silent-trim the
  backend's honest `total_visible_nodes` was built to expose, reintroduced one layer up.
- `auto_link_semantic` is O(k·n) brute force (~22 mutex-held vec-table scans per save at the 20-book
  target ≈ 0.5–1 s; `rederive_links_for_folder` multiplies per item) — and the in-code "O(k·log n), no
  corpus scans" claims are false. sqlite-vec has no ANN; fix the comments, cache back-probes, consider a
  corpus-size-aware schedule.
- Import progress events always fire `done=0,total=0` — the FE's "Embedding 12/40" branch is dead code; a
  multi-minute import is indistinguishable from a hang, and there is no cancellation.
- God-file growth: brain-v3 added ~2.2k lines to `commands.rs` (now 37,238) and ~2.7k to `db.rs` (24,489);
  `extract/` proves the escape pattern works — the links storage half (+1,086 lines in db.rs) should follow.

## 4. Research-brief verdicts in one paragraph each

- **Hierarchical RAG:** the design matches all five SOTA pillars (contextual headers on both legs, small
  embedded leaves + fetch-only parents, zero-LLM ingest per LazyGraphRAG, tight budgets, hybrid+RRF) except
  hit-aligned expansion — which is precisely the confirmed bug. The "RAPTOR collapsed-tree effect at ZERO
  LLM cost" comment overstates (RAPTOR's +20% hinges on abstractive LLM summaries; a first-sentence outline
  is a floor). The unexploited seam: the existing `rerank.rs` never touches the doc leg — Anthropic's data
  makes reranking the largest post-hybrid stage (−49→−67% failure).
- **Extraction:** we ship a *plain-text-per-page* extractor in a world where the bar (Docling 97.9%
  table-cells, Marker 96.1% multi-column) is structured elements; no page-furniture removal (headers/footers
  poison every chunk embedding), headings only from the usually-absent PDF outline. The OOXML fixes are cheap
  and headless-testable. Strategic: the source binary is discarded (`mod.rs`), freezing v1 fidelity into
  every already-imported document — retain it (SQLCipher-encrypted) or add `extractor_version` for re-import
  prompts. OCR language list is hardcoded `["pl","en"]` — fine on Tahoe (verified), risky on the 13.4 floor;
  intersect with `supportedRecognitionLanguages()` at runtime.
- **Linking math:** mutual-kNN at K=10 is the literature's recommended hubness defence (CSLS family, K
  validated 5–50) — structurally right. But e5 cosines are compressed (~0.7–1.0, InfoNCE τ=0.01) and the
  maintainer says only *relative* order is meaningful: FLOOR=0.80 barely binds (unrelated pairs routinely
  score 0.75–0.85), the 0.88 non-mutual bypass is the concentrated false-positive channel, and the
  passage-passage similarity deviates from e5's query-query symmetric contract — so external threshold
  folklore does not transfer and in-situ calibration is mandatory. Best lever: a zero-egress per-vault
  calibration harness (null distribution from ~2k random visible pairs → percentile floor; accept/dismiss
  buckets as a live precision curve — the telemetry already exists in the links table).
- **Bitemporal KG:** matches Zep's load-bearing core (invalidate-not-delete, half-open as-of) with rarer
  virtues (pure/deterministic/clock-injected, gated+purged) — and three material gaps: collapsed time axes,
  EAV strings instead of relational edges (facts never link to the Anna node), exact-key contradiction
  matching (paraphrased predicates fragment ledgers silently; `confidence` is hardcoded 1.0 dead weight).
  Graph UX sits above Obsidian-native (honest caps, typed lenses, determinism), below Bloom/Kumu (no
  search-to-locate, no expand-from-selection, no clustering, no time filter).
- **Grounding:** lexical overlap is the bottom of the attribution hierarchy (entailment correlates best with
  human faithfulness judgments; MiniCheck shows sub-1B models reach GPT-4-level checking). The shipped
  *posture* is SOTA-pragmatic (abstain-over-wrong, cause-separated confidence fields, no storage) but
  negation fails unsafe and PL inflection (no stemming) depresses coverage. The moat is real and verified:
  Granola deletes audio post-transcription; Fathom/Fireflies do cloud transcript-timestamp jumps; nobody
  does LLM-claim→second-of-audio locally. v2 path: gold set first → e5 embedding rescue (model already
  resident) → on-device NLI (mDeBERTa-XNLI via the existing DeBERTa load path in `ner_deberta.rs`).
- **ACI & competitors:** paging/MCP mirrors/no-repeat/compaction match good practice; silent truncation and
  the unsurfaced doc map are the two violations of Anthropic's own tool-writing guidance ("steer agents when
  truncating"). 6 steps × 4k chars ≈ 24k visible ≈ 1.6% of a 500-page PDF: needle questions work (retrieval
  does the lifting), section summaries and aggregations do not. Competitive: **"only local-first meeting
  notetaker" is false today** (Hyprnote YC S25 + ≥4 other local Mac apps; capture is commoditizing via
  Recall.ai's Tauri-compatible SDK) — but the CONJUNCTION (local meetings + any-size hierarchical doc ingest
  + typed graph + bitemporal facts + E2EE org sharing + Obsidian-native owned files + no per-seat AI fee)
  remains unique as of 2026-07. NotebookLM's hard 500k-words/source cap is a genuine wedge — *once* the ACI
  gaps are fixed; today our 500-page story is no better in practice.

## 5. Priority fix plan

**P0 — trust & correctness (all S-effort, headless-testable, do before the next release):**
1. Truncation honesty: marker + `TOTAL_CHARS` in `agent.rs`/`tools.rs` windowed reads (kills the
   false-"not mentioned" class).
2. Hit-aligned, sibling-gated parent expansion (chunk_id/parent_id through `DocChunkHit`; RED test that
   today's SQL returns the wrong section).
3. `update_note_doc` → `run_heavy` + `embed_in_sub_batches` (the sibling bug).
4. OOXML batch: nested-table depth, PPTX `graphicFrame` tables, `w:outlineLvl` heading resolution
   (Polish DOCX!), `w:br`/`w:tab` whitespace, `instrText`/`delText` exclusion.
5. kNN over-fetch → K distinct items (+ same for the back-probe).
6. L2 outline: line-based packing into ≤3 ≤800-char chunks; `pack_leaves` hard-split for page-sized
   paragraphs; count chars not bytes.

**P1 — product promises (S/M):**
7. Per-page OCR fallback + OCR page cap/cancellation + real `done/total` progress counts.
8. Backlinks indexed fast path over `links` (the dropped spec promise; the scan stays as legacy fallback).
9. Receipts: front-matter skip + negation veto + "likely source" copy + confidence tooltip (or amend spec).
10. Knowledge Diff: `cmp_instant` in `supersession_ledger`; render the as-of diff or delete the inert toggle.
11. Link state machine across seal cycles (tombstones/accepted/companion/inbound survival) — one PR,
    lock-security-reviewed as a set; plus both-endpoint cap enforcement and the in-tx sealed-at-rest check
    in link writers; plus the seal-time `murmur:links` block scrub (or a documented accepted-residual).
12. FE graph: disclose the 140-node draw cap; bound `full_graph_links`; relevance-ordered mention LIMIT.

**P2 — the program's own release gates (real Mac, cannot be skipped by more code):**
13. Semantic-link calibration spike (hand-label precision@5; null-distribution percentile floor) — the
    release-gating condition from the design spec, still open.
14. 500-page PDF fidelity/RAM measurement + Polish scanned-PDF OCR quality.
15. Receipts coverage gold set (100–200 lines, EN+PL) — do not market "every claim has a receipt" before.

**P3 — strategic (needs decisions):**
16. **Org sharing of large docs (v3 append-only envelope)** — the original prompt's "share huge contexts"
    is still capped at 1 MiB; this is the biggest remaining prompt-level gap.
17. Retain encrypted source binaries (or `extractor_version`) so parser upgrades apply retroactively.
18. Doc-leg reranking through the existing `rerank.rs` seam + `get_document_outline` tool + doc-scoped step
    raise (makes the NotebookLM wedge real).
19. Zep-style `t_ref` extraction → true bitemporal valid time; entity alias/merge (the single upstream fix
    that improves fusion, dossiers, graph, and ledgers at once).
20. Factor links storage out of db.rs (extract/ is the proven pattern); narrow marketing to the conjunction.
21. Hygiene: remove the stray `meetnotes.db` from the repo root working tree + gitignore it (PII-bearing
    dev DB one broad `git add` away from a commit; the secret-scan hook targets keys, not DB files).

## 6. Honest limits of this audit

Static analysis + adversarial verification against trunk `71bac31`; no builds were run (the merged suite was
CI-green at each merge). Everything Metal/Vision/PDFKit-fidelity-related is explicitly *not provable
headless* — the P2 items are measurements, not code review. Competitor facts are point-in-time (2026-07);
pricing partly from aggregators (medium confidence). One workflow artifact: the automated grading agent
received malformed inputs (script bug) and re-derived its grades from in-tree sources; the scorecard above
is this audit's own synthesis from the verified findings and research briefs, with the grader pass used as
a cross-check (agreement within ±1 everywhere).

**Bottom line:** brain v3 is *real engineering, not empty words* — the user's bar. The substrate would
survive scrutiny from the authors of the systems it borrows from (Zep's invalidation, LazyGraphRAG's
deferred-LLM thesis, mutual-kNN hubness defence), and the security layer exceeds them. What stands between
"excellent substrate" and "credibly the best AI brain" is: 6 S-effort last-mile mechanism fixes (P0), the
program's own three uncompleted calibration spikes (P2), and the 1 MiB org ceiling that still contradicts
the prompt's sharing ambition (P3.16).
