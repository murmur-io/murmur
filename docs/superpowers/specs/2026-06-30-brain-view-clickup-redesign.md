<!-- Design spec — 2026-06-30. Brainstormed + approved (layout: "Karty źródeł / ClickUp-way"). -->
# Brain view redesign — knowledge sources (ClickUp-way)

**Goal:** Replace the sparse, unreadable entity-graph-led `/brain` view with a structured "what's in my brain" view: a status header, three knowledge-source cards (Meetings / Documents / Notes) with counts + add affordances (documents AND typed notes), and the entity graph demoted to a readable "Connections" section. Fixes the three complaints: can't tell what's in the brain, can only add one file (no notes), graph is a cluster in the corner.

## Backend (small additions; reuse the existing documents/seal/embed infra — no new lock surface)

1. **Migration (additive, guarded, idempotent):** add `kind TEXT NOT NULL DEFAULT 'document'` to `documents` (`add_column_if_missing`). Distinguishes uploaded files (`'document'`) from typed brain notes (`'note'`). Existing rows default to `'document'`.

2. **`brain_overview() -> BrainOverview` (new gated command, registered in lib.rs):** returns, counting ONLY visible/unlocked content (reuse `visibility_clause`):
   - `meeting_count` (visible meetings), `document_count` (visible documents `kind='document'`), `note_count` (visible documents `kind='note'`), `indexed_chunk_count` (note_chunks + doc_chunks in visible folders),
   - `semantic_enabled` (`config.semantic_search_enabled`), `embed_model_present` (`embed::embed_model_present()`).
   `BrainOverview` is a serializable DTO in models.rs. For the header status + the "vectorized?" nudge. No content text returned — counts + flags only.

3. **`import_text(name: String, text: String, folder_id: String) -> String` (new command, registered):** ingest typed text as a `kind='note'` document — reuse `import_document`'s body (insert `documents` row with `kind='note'`, chunk via `embed::chunk`, embed only when `embed_model_present()`, store doc_chunks/doc_vec_chunks) WITHOUT the file read or the extension allowlist. Write-gated (`AppError::Locked` when the folder is sealed-not-unlocked), exactly like `import_document`. Returns the new document id. Lock: a `kind='note'` document is sealed-and-restored under the folder CK identically to a `kind='document'` (the verified document seal path is unchanged — `seal_folder_extras` already covers all `documents` rows).

4. **`list_documents`**: add `kind` to the returned `DocumentInfo` (so the FE can split Documents vs Notes), OR keep one list + the FE filters by kind. Either way `DocumentInfo` gains `kind`.

   No change to the seal/unseal/purge/gating paths — `documents` already seal-and-restore + purge-on-lock; `import_text` adds a row of the same shape. lock-security focus: confirm `kind='note'` rows ride the SAME verified seal path and the gated retrieval (a note in a locked folder is invisible to search/Ask/MCP, just like a document).

## Frontend — rebuild `src/app/features/brain/brain.component.ts` (ClickUp-way)

Layout top→bottom:
- **Header status bar:** "🧠 {meeting_count} meetings · {document_count} documents · {note_count} notes" + a semantic badge ("semantic on · e5 ✓" / "semantic off" / "model not downloaded"). When `!semantic_enabled || !embed_model_present` → a gentle inline nudge with a link to Settings ("turn on semantic search + download the e5 model to vectorize your brain"). An **[Ask ↗]** link/button routing to `/ask`. Data from `brainOverview()`.
- **Knowledge Sources — three cards** (a `source-card` sub-component or inline, in-flow `.card`s, NOT floating):
  - **🎙 Meetings** — count; a link to `/meetings`. (Read-only source — meetings are added by recording.)
  - **📄 Documents** — count; an expandable list of the folder's documents (name + date + delete); **"+ Add document"** → `@tauri-apps/plugin-dialog` `open` (md/txt) → `importDocument(path, folderId)`.
  - **📝 Notes** — count; an expandable list of notes (name + date + delete); **"+ Add note"** → opens the note editor.
  - A folder selector (reuse `FoldersService.tree()`, default first folder) governs which folder docs/notes are added to + listed from (locked folders fail closed, as today).
- **Add-note editor:** an OPAQUE (`var(--surface-overlay)`, T3) modal/inline panel: a name field + a `<textarea>` → "Add to brain" → `importText(name, text, folderId)` → toast + refresh. Cancel closes it.
- **Connections (collapsible, demoted):** the existing `brain-map.component` moved below the sources, in a collapsible `@if`-gated section ("Connections — how people/projects link across your brain"). **Fix readability: fit-to-view** — after the one-shot layout, compute the bounding box of the laid-out node positions and set the initial `viewBox` to that bbox + padding (so 4 nodes fill the canvas instead of clustering in a corner). Keep pan/zoom + neighborhood-highlight.

IPC (ipc.service.ts): add `brainOverview(): Promise<BrainOverview>`, `importText(name, text, folderId): Promise<string>`; extend `DocumentInfo` with `kind`. `BrainOverview` in models.ts.

## Constraints (binding)
- Backend: `AppError`/`Result`; additive guarded migration; gate every read (`brain_overview` counts + `import_text` write gated); verify-before-destroy already in the document seal (unchanged); no PII in logs (counts/ids only — never note/document text). `cargo test --lib` loop.
- FE: zoneless — standalone + OnPush + signals/computed/effect + inject(); `@if`/`@for track id`; `input()`/`output()`/`viewChild()`; `afterNextRender` not setTimeout; `var(--token)`; ≤16 kB per-component budget (split sub-components: `brain.component` shell + `source-card`/`note-editor`/keep `brain-map`); NO new npm packages; opaque overlays (T3) for the note-editor modal; NG0600 `allowSignalWrites` on async-load effects.

## Testing / DoD
- Backend: `cargo test --lib` — `import_text` round-trip (note ingested + chunked when model present, == 0 chunks when absent — model-presence-gated like the doc test); `brain_overview` counts only visible content (a sealed folder's docs/notes/meetings excluded); note in a locked folder invisible to `search_doc_chunks_visible`; migration idempotent. lock-security-reviewer (new ingest path, even though it reuses the verified seal).
- FE: `ng lint` + `ng build` green; adversarial-verifier live mocked-IPC smoke (header counts render; add-document + add-note flows invoke the right commands; the graph fits-to-view — nodes fill the canvas, not a corner; locked folder fails closed).
- Full `scripts/ci.sh` before merge. Batch to trunk, no version bump, no release.

## Out of scope
- Documents/notes as distinct node-kinds ON the graph (the graph stays entity-only; sources are shown as cards).
- A real animated/WebGL force-graph (needs a new npm dep — not approved).
- Per-source re-index controls (semantic toggle stays in Settings).
