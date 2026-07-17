<!-- Generated 2026-07-17 via /research (5 code audits + 5 murmur-researcher fan-out, wf_7167f584-80f). Pricing/version claims = point-in-time. -->
# Research: Brain v3 — beat Obsidian's graph + ClickUp Brain (huge-doc ingest, link engine, full-brain graph, killer features)

## TL;DR / Verdict

The brain's substrate is already excellent — hybrid FTS5+vec0+entity-graph fusion, bitemporal facts,
consolidation, lock-safe doc chunk/vector/FTS tables — but it is **format-crippled** (`.md`/`.txt` only),
**edge-less** (the ONLY persisted relation is `entity_mentions`; wikilinks/co-occurrence/related are
recomputed per read), **staleness-prone** (meeting-note edits never re-index vectors), and its graph UI
renders **one node type and one edge type**. No competitor holds "local-first + meetings + docs + typed
knowledge graph + E2EE sharing" together; NotebookLM caps sources at 500k words and has no meetings/local
mode, ClickUp Brain is cloud-only per-seat ($9–28/u/mo), Obsidian's graph is a link-topology toy without
semantics. The gap to "credibly the best AI brain" is narrow and concrete — five build tracks below.

**Positioning one-liner:** *"The only second brain that hears your meetings, reads your documents, and
draws the connections — entirely on your Mac, in files you own."*

## Co już mamy (verified in code)

- **Doc pipeline, generic but gated to md/txt:** `commands.rs: import_document_inner` (ext allowlist
  `DOC_ALLOWED_EXTS=["md","txt"]`), single gated seam `ingest_into_folder`, `doc_chunks` +
  `doc_vec_chunks` (vec0 384) + `fts_doc_chunks` with purge-on-seal / re-embed-on-unlock — **the storage
  layer already generalizes; only `extract(path)→text` is missing.** Sync command, whole-file
  `read_to_string`, one-batch `embed_passage` (memory ∝ doc size), no progress events.
- **Retrieval:** `search_hybrid_visible` (FTS+KNN+graph, `score_fuse` 0.4/0.4/0.2), doc leg via
  `fuse_doc_hits` (RRF). Agentic Ask: 6 steps, `RESULT_BUDGET=4000` chars/tool result, **no paging** on
  `get_document`/`get_meeting` → a big doc is unreachable past ~1 page; floor path shows ONE 800-char
  chunk per doc. No doc summaries / hierarchy / map-reduce anywhere.
- **Links:** `extract_wikilink_titles` + `backlinks_for_visible` (O(vault) live scan per open, exact
  title match), `resolve_wikilink`, `list_link_candidates`, `documents.meeting_id` companion leg,
  `link_related_notes_inner` (writes `[[Title]]` into markdown, max 4). **No edges table.** Imported
  docs get no entity extraction, no meeting links, no semantic links.
- **Graph UI:** entity cards + co-occurrence edges only (`build_graph`, `graph_edges_visible` LIMIT 600,
  computed per read); brain-map canvas (top-60, one-shot 3-D Fruchterman-Reingold); no graph lib.
- **Org Shared Brain:** notes-only, 1 MiB/item, opaque blobs (client-side envelope evolution = zero server
  change), receivers re-chunk+re-embed locally, `clean_note_body` **flattens wikilinks at egress** (edges
  destroyed), org items outside the entity graph. Push-side poison-retry risk on oversized blobs.
- **Facts/memory:** bitemporal `reconcile_facts` (invalidate-not-delete), hourly consolidation, rollups
  purge-on-seal, empty-unlock-set reads. **Nobody in the market has this** — and we don't surface it.

## Audit — real gaps found (fix-worthy, evidence in audit_gaps)

| # | Sev | Gap | Fix | Effort |
|---|-----|-----|-----|--------|
| 1 | Staleness | Meeting-note EDIT never re-derives chunks/vectors (`update_note_inner` lacks `index_meeting_chunks`); deleted text keeps surfacing in semantic search + snippets | mirror `update_note_doc_inner`'s re-index idiom | S |
| 2 | Staleness | No repair tick: model download / flag-flip / model-absent unlock leave vectors missing until manual Reindex | generalize `backfill_topic_chunks_idempotent` startup tick + FE post-download prompt | M |
| 3 | Quality | Manual Reindex re-embeds authored notes WITH YAML front-matter (`reindex_embeddings_inner` ignores `kind`) | route by kind → `index_note_chunks` | S |
| 4 | Staleness | Rename/edit never refreshes aug-headers, entities, facts (old title embedded in every chunk) | fold into #1's re-index + rename hook | M |
| 5 | Quality | Entity dedup = exact `(lower(name),kind)`; no aliases/merge → split nodes | `entity_aliases` + merge command (defer) | M/L |
| 7 | Low | `tools.rs` SearchSemantic arm misses `embed_model_present()` guard | one line | S |

Clean (verified): deletes purge everything incl. vec0 orphans; FTS triggers fresh; no stub vectors at
rest; no ungated reads found; consolidation hygiene correct. Measured (do not re-litigate):
hybrid 0.90 vs semantic 0.95 on the semantic-skewed set is CORRECT hybrid behavior; reranker adds
nothing (pointwise-LLM exhausts 3 s); the real lever is a cross-encoder, deferred.

## Findings (per angle, cited in the full agent briefs)

1. **Large-doc RAG (SOTA 2025-26):** Anthropic contextual retrieval (−49…67% failure w/ BM25+rerank);
   RAPTOR's collapsed-tree (embed doc-summaries alongside leaves); LlamaIndex parent-document expansion;
   LazyGraphRAG's core lesson = **defer LLM work to query time** (never LLM-process a 500-page PDF at
   ingest); Chroma "context rot" (tight ≤6k-token contexts beat stuffing); Karpathy: "context engineering
   = filling the window with just the right information." Late chunking needs an 8k-token embedder — not
   available at 384-dim/512-token e5-small; skip. → **3-level hierarchy on existing `doc_chunks`**
   (L0 800-char leaves embedded+FTS; L1 section parents ~6k chars FTS-only, fetched by expansion;
   L2 deterministic outline/summary embedded+FTS), deterministic headers `doc | section_path | p.N`.
   Math: 500-page PDF ≈ 1600 leaves ≈ 2.5 MB vectors; 20 books ≈ 50 MB — vec0 brute-force still fine.
2. **Extraction stack (macOS, AGPL-ok, minimal deps):** **PDFKit via `objc2-pdf-kit`** (same objc2 family
   we ship; per-page text + outline free; wrap in `objc2::exception::catch`, crash-safe-FFI rule) +
   **pure-Rust OOXML** (DOCX/PPTX = walking `zip`+`quick-xml`, both ALREADY compiled in transitively) +
   `calamine` (XLSX) + `html2text` (HTML). Rejected: extractous (stale GraalVM blob), mupdf (huge C tree),
   pdfium (dylib signing burden). Scanned-PDF OCR = Vision `VNRecognizeTextRequest` phase 2.
   **Dep requests: `objc2-pdf-kit`, `calamine`, `html2text` + promote `zip`/`quick-xml` to direct.**
3. **Auto-linking:** e5 cosine is compressed (~0.7–1.0, InfoNCE τ=0.01) — naive thresholds link
   everything; use **mutual-kNN (k=10) + cosine floor 0.80** (cos = 1 − d²/2 on L2-normalized vectors),
   ≥0.88 bypasses mutuality, cap 5 semantic edges/node, O(k·log n) per insert via vec0 kNN. Persist a
   **typed `links` table** (wikilink|semantic|entity|temporal|companion; status active|suggested|dismissed).
   Deterministic types auto-active; **semantic = suggest-then-accept, never silently injected into `.md`**
   (the Reflect/Mem lesson). Accepted → materialize `[[Title]]` via existing `apply_link_markers`.
4. **Competitors:** matrix in the brief. Attack lines: ingest a 1000-page PDF locally (NotebookLM can't —
   500k-word cap, cloud), auto-link it to the meeting where it was discussed (Obsidian can't — no
   semantics/meetings), share E2EE with zero per-seat AI fee (ClickUp can't). Rewind/Limitless died
   Dec 2025 → orphaned local-first audience.
5. **Killer features (ranked by moat):**
   - **Receipts** — every claim in a note/answer/dossier clickable to the SECOND of audio (extend
     `grounding.rs` alignment → `(claim → segment_id, start_s, speaker, confidence)`; seek the shipped
     timeline). Granola *deletes* audio post-transcription; ClickUp keeps it in their cloud. Structural moat.
   - **Knowledge Diff** — surface the bitemporal facts: as-of queries, per-entity decision ledger
     (supersession = old fact → new fact → source meeting → receipt), "changed since you last met X" in
     briefs. Pure interval algebra (`AS OF t` = `valid_from ≤ t AND (valid_to IS NULL OR valid_to > t)`),
     deterministic; reasoner only narrates. No competitor keeps bitemporal state at all.
   - Semantic Edge Layer ranks third only because it's already covered by the user's link-engine ask.

## Fit z ograniczeniami Murmur

Everything on-device (extraction, chunking, embedding, linking, diffing); zero new egress. Additive
migrations only (`links` table, `doc_chunks` level/parent/section/page columns, `entity_aliases`).
Lock model: edges + hierarchy rows are DERIVED content → purge-on-seal / re-derive-on-unlock exactly like
`doc_chunks` today; every read gates BOTH endpoints (the `backlinks_for_visible` two-gate template);
Receipts ride `meeting_is_unlocked` + the masked-DTO audio rule. Org: doc sharing needs zero wire change
below 1 MiB (v2 envelope already round-trips Document source-kind); edges/large docs → v3 append-only
envelope later; add a client-side pre-upload size check to kill the poison-retry loop. New crates named
above require approval — they are the minimal set entailed by the explicitly requested PDF/DOCX feature.

## Plan — 6 PR tracks (dependency-ordered)

1. **PR-1 Brain integrity** (S/M): audit fixes #1 #3 #7 (+#4 rename hook, +#2 repair tick).
2. **PR-2 Universal ingest** (M/L): `extract/` module (`ExtractedBlock {text, page, heading_path}`),
   DOCX/PPTX pure-Rust, PDF via PDFKit, XLSX/HTML; async + `perf::run_heavy` + RAM floor + progress
   events + sub-batched embeds; hierarchical chunks + L2 summaries; `get_document`/`get_meeting` tool
   paging (offset/section args) so the agent can actually read big docs.
3. **PR-3 Link engine** (M): `links` table + write-time wikilink indexing (hook: note-save funnels +
   `build_and_persist_entities`) + mutual-kNN semantic suggester + entity/temporal persist +
   Connections panel + backlinks reader switched to indexed lookup.
4. **PR-4 Full-brain graph** (M): typed nodes (meetings/notes/docs/entities) + typed edges from `links`,
   lenses/filters, on the existing canvas idiom.
5. **PR-5 Receipts** (S/M): grounding alignments → clickable per-claim citations seeking audio.
6. **PR-6 Knowledge Diff** (M): `facts_as_of_visible` + pure `diff_snapshots` + entity timeline panel +
   MCP `knowledge_diff` tool.

Every PR: RED-before-GREEN, adversarial-verifier + lock-security-reviewer (all touch gated reads or
derived-content storage), `scripts/ci.sh`, QueaT commit, PR to `murmur`.

## Otwarte pytania / nieweryfikowalne headless

- PDFKit RAM/fidelity on a real 500-page PDF + `attributedString` heading heuristics — real-Mac measure.
- e5-small cosine separation on the real PL/EN vault — the 0.80/0.88 thresholds need the 20-note spike
  (hand-label precision@5; raise floor if garbage >20%).
- Claim→segment alignment quality on paraphrased LLM note lines — spike in PR-5.
- Vision OCR Polish quality; `murmur://` deep-link registration — phase 2 / real Mac.

## Sources

Agent briefs (full citations inside): audit_{ingest,retrieval,linking,org,gaps}, research_{rag,parsing,
competitors,autolink,killer} — workflow wf_7167f584-80f, 2026-07-17. Key external: anthropic.com/news/
contextual-retrieval · arxiv 2401.18059 (RAPTOR) · microsoft.com LazyGraphRAG · trychroma.com/research/
context-rot · x.com/karpathy/status/1937902205765607626 · jina.ai late-chunking · github.com/brianpetro/
obsidian-smart-connections · support.google.com/gemininotebook (NotebookLM caps) · clickup.com/brain/pricing ·
granola.ai/security · simonwillison.net lethal-trifecta · docs.rs objc2-pdf-kit/objc2-vision · crates.io
API (calamine, html2text, pdf-extract, extractous, mupdf, pdfium-render licenses/versions).
