# Brain v3 — design spec (universal ingest · link engine · full-brain graph · Receipts · Knowledge Diff)

Date: 2026-07-17 · Status: approved-by-goal (autonomous program), pre-implementation
Basis: `docs/research/2026-07-17-brain-v3-maximization.md` (5 code audits + 5 research briefs, wf_7167f584-80f)

Goal (user): the brain must ingest documents of ANY size (PDF/DOCX/…), auto-link notes/meetings/docs
(manual `[[wikilinks]]` + content similarity), visualize ALL connections, fix storage gaps, and ship 2
killer features — mathematically grounded, shareable via org, merged to `murmur`.

Six PR tracks, dependency-ordered. Every track: RED-before-GREEN, adversarial-verifier +
lock-security-reviewer, `cargo test --lib` + `ng lint` + `ng build` per PR, final `scripts/ci.sh`,
QueaT commits, PR to `murmur` (never direct push).

---

## PR-1 — Brain integrity (backend, S/M) — fixes the audit's real gaps

1. **Meeting-note edit re-index** (`commands.rs: update_note_inner`): after upsert/reseal, best-effort
   `embed_model_present().then(active_embedder)` → `db.index_meeting_chunks(mid, &segments, emb)`,
   warn-and-continue posture — the exact idiom of `update_note_doc_inner → index_note_body_chunks`.
   RED: edit a note, assert semantic snippet no longer serves deleted text / vectors refreshed.
2. **Reindex kind-routing** (`commands.rs: reindex_embeddings_inner` + the unseal doc re-index sites):
   `documents.kind='note'` rows go through `index_note_chunks` (front-matter stripped), never raw
   `index_document_chunks`. RED: reindex leaves an authored note's chunks front-matter-free.
3. **Stub-query guard** (`tools.rs` SearchSemantic arm): add `embed_model_present()` before
   `active_embedder().embed_query(...)` (mirror the citations site).
4. **Rename staleness** (`commands.rs: rename_meeting` / title-set path): trigger the same model-gated
   `index_meeting_chunks` so aug-headers/FTS/vectors carry the new title.
5. **Idempotent repair tick** (`lib.rs` setup, generalizing `backfill_topic_chunks_idempotent`):
   probe visible meetings with note-but-no-chunks / chunks-but-no-vectors and docs via
   `document_has_chunks`; model+flag+RAM gated; runs `index_*` per item. FE: after successful embed-model
   download (`settings.store.ts: downloadEmbedModel`), surface a "Reindex now" prompt (or auto-run).
6. **Org push pre-check** (`commands.rs: publish_org_body`): client-side ciphertext-size check vs
   `MAX_ORG_ITEM_BLOB_BYTES` → terminal `set_org_share_failed("too_large")`, not an infinite retry.

Lock notes: all re-index paths operate on already-gated plaintext of VISIBLE items only; the repair tick
reads with gated readers. lock-security-reviewer confirms no sealed text enters an index.

## PR-2 — Universal document ingestion (backend M/L + FE S)

**New module `src-tauri/src/extract/`** — `ExtractedBlock { text: String, page: Option<u32>,
heading_path: Option<String> }`, `extract_blocks(path, ext) -> Result<Vec<ExtractedBlock>>`:
- `md`/`txt`: current `read_to_string` (one block).
- `docx`/`pptx`: pure-Rust walk of the OOXML zip (`zip` + `quick-xml`, both already compiled in
  transitively — promote to direct deps): `w:p`/`w:t` runs, `w:pStyle` Heading1..9 → `heading_path`,
  `w:tbl` → pipe-rows; slides: `a:t`, slide N = page, title placeholder = heading. Unit-testable with
  fixture files in `cargo test --lib`.
- `pdf`: **PDFKit via `objc2-pdf-kit`** (same objc2 family as shipped deps; zero new binaries):
  `PDFDocument::initWithURL` → per-page `pageAtIndex(i).string` loop (bounded memory),
  `outlineRoot` → heading_path, wrapped in `objc2::exception::catch` (enable objc2 `exception`
  feature) → fail-closed `AppError::InvalidArg`. Crash-safe-FFI rule honored; real-Mac verify for
  fidelity/RAM. Scanned-PDF OCR (Vision) = explicit phase 2, NOT in this PR.
- `xlsx`: `calamine` (sheet name = heading, rows → pipe text). `html`: `html2text`.
- **Dep requests (minimal set entailed by the explicitly requested feature; all MIT-family):**
  `objc2-pdf-kit`, `calamine`, `html2text`, promote `zip` + `quick-xml`. NO extractous/mupdf/pdfium.

**Async + progress:** `import_document` becomes async; extract+chunk+embed inside `perf::run_heavy`;
RAM floor `topic_backfill_ram_permits_now()`; typed progress events in `events.rs`
(stages: extracting/chunking/embedding + counts) consumed by the Brain tab. Embedding sub-batched
(mirror `index_meeting_chunks`'s batching), chunk inserts committed in batches.

**Hierarchical index (additive cols on `doc_chunks`):** `level INTEGER NOT NULL DEFAULT 0`,
`parent_id INTEGER`, `section_path TEXT`, `page_no INTEGER` via `add_column_if_missing`.
- L0 leaf: 800-char paragraph-greedy chunks that NEVER cross heading boundaries; embed-text =
  `"<doc> | <section_path> | p.<N>"\n<raw>` (extends the shipped deterministic contextual-header
  mechanism); **embedded + FTS**.
- L1 section-parent: heading-bounded ≤ ~6000 chars, **FTS + fetch-by-id only, NOT embedded** (vector
  count stays flat).
- L2 doc outline: deterministic heading tree + first sentence per section (1–3 chunks),
  **embedded + FTS** (RAPTOR collapsed-tree effect). No LLM at ingest (LazyGraphRAG lesson).
- Retrieval: new gated helper `expand_doc_parents_visible` — when ≥2 L0 leaves of one parent hit, or for
  the top-3 fused doc hits, replace leaves with the L1 parent text; wire into `pack_doc_chunks` +
  the tools doc leg. Context budget to the reasoner stays tight (context-rot).
- `index_document_chunks` gains the hierarchy; purge/seal/unseal untouched semantics (rows live in
  `doc_chunks` → existing purge-on-seal + re-embed-on-unlock cover them; L>0 rows excluded from
  `doc_vec_chunks` except L2).

**Agent paging:** `get_document` tool gains `offset`/`max_chars` args (default today's behavior);
`get_meeting` transcript likewise — so the agentic loop can iterate a big doc past `RESULT_BUDGET`.
MCP mirrors (`tools_spec`/`dispatch_tool`).

**FE:** Brain-tab dialog filter → `["md","txt","pdf","docx","pptx","xlsx","html"]`; progress UI off the
new events; `friendlyImportError` mapping for unsupported/encrypted/scanned-PDF-no-text.

**Storage decision:** we store EXTRACTED TEXT only (documents.text), never the source binary (no new
seal path). Original filename kept in `documents.name`.

## PR-3 — Link engine (backend M + FE S/M)

**Additive `links` table** (in `Db::migrate`, `CREATE TABLE IF NOT EXISTS`):
`links(id INTEGER PK, src_kind TEXT, src_id TEXT, dst_kind TEXT, dst_id TEXT,
edge_type TEXT /* wikilink|companion|semantic */, score REAL DEFAULT 1.0,
created_by TEXT /* user|auto|accepted */, status TEXT DEFAULT 'active' /* active|suggested|dismissed */,
created_at INTEGER, UNIQUE(src_kind,src_id,dst_kind,dst_id,edge_type))` + both-direction indexes.
Kinds: `meeting|note|document` (entity edges stay live-computed — see PR-4). Undirected semantic edges
canonicalize `src<dst`; wikilink/companion keep direction. `dismissed` = tombstone (a rejected
suggestion never reappears).

**Write-time wikilink indexing:** at the note-save funnels (`update_note_doc_inner`,
`create_note_inner`, meeting-note `update_note_inner`, `build_and_persist_entities` post-pipeline) run
`extract_wikilink_titles` → `resolve_wikilink` → upsert `edge_type='wikilink'` rows storing TARGET IDS
(rename-proof; the audit's title-string fragility fixed at the root). Remove stale wikilink rows for the
source on each pass (delete-then-insert in one tx). Companion notes backfill `documents.meeting_id` →
`edge_type='companion'` rows (one-time additive backfill in migrate, idempotent).

**Semantic auto-linker (the math):** after a successful `index_document_chunks` /
`index_meeting_chunks` / `index_note_body_chunks` with the real embedder:
1. item centroid = L2-normalized mean of its chunk vectors (idiom: `related_meetings_visible`);
2. vec0 kNN `MATCH centroid AND k=10` over `vec_chunks` ∪ `doc_vec_chunks`, roll chunk→item keeping
   BEST distance; drop self;
3. cos = 1 − d²/2 (unit vectors, L2 metric);
4. keep candidate iff (mutual: this item ∈ candidate's top-10) OR cos ≥ 0.88; floor cos ≥ 0.80;
5. cap 5 semantic edges/node; upsert `status='suggested'`, `score=cos`, `created_by='auto'`.
Named consts `SEMANTIC_LINK_FLOOR=0.80`, `SEMANTIC_LINK_STRONG=0.88`, `SEMANTIC_LINK_K=10`,
`SEMANTIC_LINK_CAP=5`. O(k·log n) per insert — no corpus scans. e5 cosine is compressed (~0.7–1.0), so
mutual-kNN is load-bearing against hubness; thresholds are start values, calibrated by a dev-vault spike
(hand-label precision@5; if garbage >20% raise floor).

**Policy:** wikilink/companion → `active` immediately (deterministic). Semantic → **suggest-then-accept**;
Accept flips `created_by='accepted'`, `status='active'` AND materializes `[[Title]]` in the source
markdown via the existing `apply_link_markers`; Dismiss tombstones. **Never silently inject semantic
links into `.md`.**

**Lock discipline (binding):** links rows are DERIVED relations; purge rows touching a sealed folder's
items inside every seal tx (new `purge_links_tx` next to `purge_doc_chunks_tx` /
`purge_memory_rollups_tx`); re-derive on unlock (re-run wikilink pass + semantic pass for unsealed
items). Every read gates BOTH endpoints via `visibility_clause` (the `backlinks_for_visible` two-gate
template). RED tests: sealed endpoint hides the edge both directions; unlock restores; round-trip.

**Commands:** `list_links(kind,id)` (gated both endpoints), `accept_link(id)`, `dismiss_link(id)`;
register in `lib.rs`. `backlinks_for_visible` gains an indexed fast path over `links`
(keeps the live-scan as fallback for legacy bodies until first save re-indexes).

**FE:** `app-connections` panel (detail Note tab + note-editor, next to backlinks): grouped by edge
type, semantic rows show 3-tier confidence chip (≥0.88 / ≥0.84 / ≥0.80) + Accept/Dismiss. Opaque
overlays, tokens only, signals-first.

## PR-4 — Full-brain graph (backend S + FE M)

**Backend:** `get_full_graph` → `Db::build_full_graph(&unlocked, opts)`:
- nodes: entities (as today) + VISIBLE meetings, notes, documents (id, kind, title, date, degree);
- edges: entity↔entity co-occurrence (live, as today) + entity→meeting mentions +
  `links` rows (active; suggested behind a flag);
- caps with honest disclosure (mirror today's 500-cap + `hasHidden`); every leg visibility-gated.
**FE:** extend the brain-map canvas + graph feature: per-kind node colors (tokens), lenses = node-type
and edge-type toggles, click-through (meeting → detail, note/doc → editor/preview, entity → entity
detail), suggested edges rendered dashed when the toggle is on. Reuse the deterministic
Fruchterman-Reingold idiom; no graph lib (no new npm deps).

## PR-5 — Killer feature 1: Receipts (claim → second of audio) (backend S/M + FE S/M)

Extend `summarize/grounding.rs`'s deterministic token-overlap alignment to EMIT
`ClaimAlignment { claim_idx, segment_id, start_s, end_s, speaker, confidence, overlap_score }` per
note line/bullet (best-matching segment; threshold on overlap; no LLM). New gated command
`get_note_receipts(meeting_id)` → alignments for the CURRENT note, `meeting_is_unlocked`-gated,
recomputed on demand (no new storage → no new seal path). FE: in the meeting Note panel, an unobtrusive
per-claim chip (»receipt«) that seeks the shipped timeline/audio player to `start_s` and flashes the
matching transcript segment; speaker (Me/Others) + ASR confidence in the tooltip. Locked meeting →
no receipts (the masked DTO already nulls audio). Obsidian export unchanged in v1 (deep-link scheme =
fast-follow). Moat: Granola deletes audio post-transcription; ClickUp keeps it cloud-side — nobody can
answer "prove it" locally.

## PR-6 — Killer feature 2: Knowledge Diff (backend M + FE S/M)

Pure functions in `facts.rs`: `snapshot_as_of(facts, t)` (open at t: `valid_from ≤ t AND (valid_to IS
NULL OR valid_to > t)`) and `diff_snapshots(a, b) -> {added, removed, changed}` keyed
`(norm(subject), norm(predicate))` — deterministic set algebra, unit-tested headless. Gated command
`get_entity_knowledge_diff(entity_id, from, to)` reading via the visible-facts reader (facts anchored
to `meeting_id` keep the gate); returns the ledger: each change carries old/new object, valid_from,
source meeting id. FE: entity-detail "Zmiany / Decision ledger" section — chronological supersessions
(old → new, source-meeting chip; receipt chip when PR-5 present). MCP tool `knowledge_diff` mirrored in
`tools_spec`/`dispatch_tool`. "Changed since you last met" brief injection = fast-follow after the
ledger proves itself. Moat: nobody else keeps bitemporal state; ClickUp/Notion answer only current
state.

---

## Explicit non-goals (v1)
Vision OCR for scanned PDFs; org sharing of large docs (v3 envelope) beyond the pre-upload size check;
cross-encoder reranker; entity alias/merge (audit #5); temporal edge type; `murmur://` deep-link scheme;
storing source binaries.

## Verification matrix
| PR | adversarial-verifier | lock-security-reviewer | real-Mac note |
|----|----|----|----|
| 1 | yes | yes (index paths touch gated plaintext) | — |
| 2 | yes (async/RAM/progress) | yes (new content at rest via extraction) | PDF fidelity/RAM on 500-page PDF |
| 3 | yes (RED both-endpoint gates) | yes (derived-relation purge/gate) | threshold spike on dev vault |
| 4 | yes | yes (new aggregate read) | — |
| 5 | yes | yes (audio-adjacent read) | seek UX on dev app |
| 6 | yes | yes (facts read) | — |
