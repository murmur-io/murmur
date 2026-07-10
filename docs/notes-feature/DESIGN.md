# Murmur — Notes feature: DESIGN + IPC CONTRACT (single source of truth)

Status: FROZEN v1. All build agents implement against this. If a decision here is wrong,
raise it — do not silently diverge.

Branch: `feat/notes-editor` · worktree `/Users/jakubgawronski/Projects/meetnotes-notes-wt`.

---

## 0. What we are building

A first-class **Notes** experience: the user authors standalone markdown notes (Obsidian-like:
headers, properties/tags, wikilinks), organizes them in **note folders**, locks/shares them exactly
like meetings, and they are **first-class brain context** (embedded / FTS / retrievable) like
recordings. Selecting text in the editor pops a **Brain assistant** (Refine / Shorten / Enhance
context) that is **mode-aware** (local Qwen vs cloud Claude per the brain posture).

### The decisive architecture decision (ground-truthed against the tree)

Standalone notes are **authored `documents` rows with `kind='note'`** — NOT a new table. The
`documents` table (`db.rs` `CREATE TABLE documents`) is already:
- folder-anchored (`folder_id` FK `folders(id)`, NOT NULL) — the lock/gate anchor,
- sealed with verify-before-destroy (`Db::seal_document`, purge in `seal_folder_extras`/`lock_folder_inner`),
- brain-indexed (`doc_chunks` + `doc_vec_chunks` (vec0) + `fts_doc_chunks`), retrievable via
  `search_doc_chunks_visible` → `DocChunkHit`, folded into Ask/brain grounding,
- gated (`list_documents`/`get_document` refuse a sealed-not-unlocked folder),
- already has `kind ∈ {'document' (uploaded file), 'note' (typed brain note)}` and
  `import_text(name,text,folder_id)` already creates a `kind='note'` row.

So we REUSE that substrate and only add the **authoring layer** it lacks: create-empty, edit/update
(re-index), title + properties/tags, a Notes nav section with note-kind folders, the editor, the
selection assistant, sharing, auto-organize, and vault `.md` export.

DO NOT create a `user_notes_docs` / `note_folders` table (an earlier synthesis suggested it before
`documents(kind='note')` was found — it would duplicate the entire seal/gate/brain machinery).

---

## 1. Data model (additive, guarded, idempotent — never destructive)

All in `Db::migrate()` via `add_column_if_missing` / `CREATE ... IF NOT EXISTS`. Legacy rows keep
working. Round-trip + idempotency tests required.

### 1a. `documents` — add columns
```
add_column_if_missing("documents", "title",         "TEXT")           -- note title; NULL ⇒ fall back to `name`
add_column_if_missing("documents", "updated_at",     "INTEGER")        -- epoch ms; NULL ⇒ created_at
add_column_if_missing("documents", "exported_path",  "TEXT")           -- vault .md path; NULL when never exported / sealed
```
- `text` continues to store the FULL markdown INCLUDING YAML front-matter. Front-matter carries
  properties/tags (vault-native, owned-file). When **chunking for the brain**, STRIP front-matter
  first (reuse the front-matter strip logic; only the body is embedded — tags must not pollute
  vectors). `name` = filesystem-safe slug; `title` = display title (may contain spaces/emoji).
- Sealing blanks `text` → `text_blob` exactly as today (kind-agnostic). `title` is NON-content
  metadata but MAY reveal topic → **mask the title in gated list/get DTOs for a sealed-not-unlocked
  note** (return "🔒 Locked", like meetings). `updated_at`/`created_at` stay (non-content).

### 1b. `folders` — add `kind` (separate Notes tree from Meetings tree)
```
add_column_if_missing("folders", "kind", "TEXT NOT NULL DEFAULT 'meeting'")
```
- Existing folders default to `'meeting'` → Meetings section behavior is byte-identical.
- Note folders are created with `kind='note'`. The Notes section shows only `kind='note'` folders;
  Meetings shows `kind != 'note'`. Lock/seal/CK machinery is `folder_id`-keyed and kind-agnostic —
  reused verbatim (a note-folder locks its notes; `seal_folder_extras` already seals documents).
- **Default note folder:** on first Notes use, ensure a root note-folder exists
  (`ensure_default_note_folder()` → name "Notes", `kind='note'`, path `"Notes"`). Every note anchors
  on a note-folder (documents.folder_id NOT NULL — this is the gate anchor, keep it).
- **Path namespace:** note-folder `path` is rooted so it can't collide with meeting-folder paths
  (`folders.path` is UNIQUE). Use a `"Notes/"` vault prefix for note-folder paths (default folder
  path `"Notes"`, children `"Notes/<name>"`). Vault export of notes therefore lands under
  `<vault>/Notes/...`.

### 1c. Sharing
Reuse the meeting link-share tables (`outbound_shares`). Add a nullable `document_id TEXT` column
(additive) alongside the existing meeting anchor; the E2EE envelope/server/crypto path is 100%
reused (a note's markdown is the shared body). Details in §7 (WP6) — assess the exact existing
columns when you implement; do not break meeting shares.

---

## 2. IPC CONTRACT (frozen) — Rust command ⇄ IpcService method ⇄ models.ts DTO

Every command: `#[tauri::command]` in `commands.rs` wrapping a `_inner(&AppState, …)`, registered in
`lib.rs generate_handler![]`, returns `Result<T, AppError>` (never a string the FE must parse). Every
read/list/export/share/assistant path GATES on folder-unlock. One typed `IpcService` method per
command; DTOs in `models.ts` (`camelCase`, matching `#[serde(rename_all="camelCase")]`).

### DTOs
```ts
// models.ts
export interface NoteSummary {          // list rows — leak-free (no body for sealed)
  id: string;
  title: string;                        // "🔒 Locked" when sealed-not-unlocked
  folderId: string;
  snippet: string;                      // "" when locked
  tags: string[];                       // [] when locked
  updatedAt: number;                    // epoch ms
  createdAt: number;
  locked: boolean;                      // sealed AND not session-unlocked
  shared: boolean;                      // has an active outbound share
}
export interface NoteDoc {              // full note (editor)
  id: string;
  title: string;                        // "🔒 Locked" when masked
  folderId: string;
  markdown: string;                     // FULL md incl. front-matter; "" when masked
  tags: string[];
  properties: Record<string, string>;  // parsed front-matter (excl. tags); {} when masked
  updatedAt: number;
  createdAt: number;
  exportedPath: string | null;
  locked: boolean;                      // masked (no markdown) when true
  shared: boolean;
}
export type NoteAssistAction = 'refine' | 'shorten' | 'enhance';
export interface NoteAssistRequest {
  noteId: string;
  action: NoteAssistAction;
  selection: string;                    // the selected text
  before?: string;                      // up to ~500 chars of context before selection
  after?: string;                       // up to ~500 chars of context after selection
}
export interface NoteCitation {         // enhance-context provenance
  kind: 'meeting' | 'note';
  id: string;
  title: string;
  snippet: string;
}
export interface NoteAssistResult {
  action: NoteAssistAction;
  suggestion: string;                   // refine/shorten: replacement for selection.
                                        // enhance: an ADDITIVE passage to INSERT after selection.
  citations: NoteCitation[];            // enhance only (else [])
  modelLabel: string;                   // e.g. "Claude" | "Qwen2.5 4B (local)" — shown in popover
  mode: 'local' | 'cloud';
  redacted: boolean;                    // true if the cloud path redacted the payload
}
export interface OrganizeMove {
  noteId: string; title: string;
  fromFolderId: string; fromFolder: string;
  toFolder: string;                     // proposed folder NAME (existing or new)
  toFolderId: string | null;            // null ⇒ a new folder to create on apply
  reason: string;                       // one-line why (shown to user)
}
export interface OrganizePlan { moves: OrganizeMove[]; }
export interface NoteFolder {           // reuse Folder shape; kind='note'
  id: string; name: string; path: string; parentId: string | null; locked: boolean; kind: string;
}
```

### Commands (name → args → returns)
Notes CRUD (all gated on the note's folder):
- `create_note(folderId: string | null, title: string) -> string`  // empty note; null ⇒ default note-folder. Returns id.
- `get_note(id: string) -> NoteDoc`                                  // masked if sealed-not-unlocked
- `update_note(id: string, title: string, markdown: string) -> NoteDoc`  // write-gated; re-index; re-export; bump updated_at
- `list_notes(folderId: string | null) -> NoteSummary[]`             // gated; null ⇒ all visible notes
- `move_note(id: string, folderId: string) -> void`                  // gated both sides; re-export into new folder path
- `delete_note(id: string) -> void`                                  // reuse delete_document semantics (cascade chunks)
- `export_note(id: string) -> string`                                // (re)write vault .md; returns path; gated

Note assistant:
- `note_assistant_action(req: NoteAssistRequest) -> NoteAssistResult` // provider_for(Role::Notes); enhance retrieves brain context

Auto-organize (two-step, non-destructive):
- `plan_organize_notes(folderId: string | null) -> OrganizePlan`      // propose folder assignments by content
- `apply_organize_plan(plan: OrganizePlan) -> void`                   // create needed folders + move notes (gated)

Note folders (reuse folder machinery, kind='note'):
- `list_note_folders() -> NoteFolder[]`                               // kind='note' only
- `create_note_folder(name: string, parentId: string | null) -> NoteFolder`
- `rename_note_folder(id, name) -> void` · `delete_note_folder(id) -> void` · `move_note_folder(id, parentId|null) -> void`
- Lock: REUSE existing `lock_folder` / `unlock_folder` / `relock_folder` / `remove_lock` (folder-id based; work on note-folders unchanged).

Sharing (WP6 — mirror meeting link-share):
- `share_note_to_link(id: string, expiresDays: number | null, password: string | null) -> ShareResult`
- `list_note_shares(id: string) -> ShareInfo[]` · `revoke_note_share(shareId: string) -> void`
  (Reuse the meeting share DTOs/commands' shapes; add note anchor. Confirm exact existing signatures when building.)

Settings additions (WP4b): extend the existing AI config DTO + `settings/config.rs` with note-assistant
toggles (see §6). Reuse the existing settings get/set commands — add fields, don't add commands unless
needed.

---

## 3. Backend seams to REUSE (by symbol — grep to confirm current line)

- Ingest/create: `ingest_into_folder(state, folder_id, name, text, "note")` (commands.rs) — the gated
  insert + conditional vector index. `create_note` = ingest an empty/most-minimal note (relax the
  empty-text refusal for create; keep write-gate). `import_text_inner` is the existing typed-note path.
- Update/re-index: purge the note's `doc_chunks` (+ `doc_vec_chunks` + `fts_doc_chunks` via triggers)
  and re-insert from the new BODY (front-matter stripped), same as ingest indexing. Reuse the doc
  chunk/embuild helpers used by `ingest_into_folder`. `purge_doc_chunks_for_documents([id])` exists.
- Read/gate: `get_document_inner` / `list_documents_inner` show the gating pattern
  (`folder_by_id` → unlock check → refuse/mask). `folder_is_unlocked` / `meeting_is_unlocked` (commands.rs)
  and `visibility_clause` (db.rs, table-agnostic) — use for note reads. Sealed-not-unlocked ⇒ mask.
- Seal/unseal (LOCK — verify-before-destroy): `Db::seal_document` (db.rs), the document leg already in
  `seal_folder_extras` (commands.rs) + `lock_folder_inner` + `remove_lock_inner` + `unlock_folder`.
  The AAD for a document seal already exists (`aad_content`/doc variant — grep). Notes ride the SAME
  document seal leg (kind-agnostic) — verify no note-specific gap (title masking, exported vault file
  deletion on lock like meeting notes delete their vault .md on seal, re-export on unlock/remove_lock).
  → **This is the lock-security-reviewer's required audit surface.**
- Brain retrieval: `search_doc_chunks_visible` → `DocChunkHit` already surfaces note chunks to
  Ask/brain grounding. Ensure notes participate identically (they do, as documents). No engine change.
- Assistant model routing: `summarize::provider_for(Role::Notes, &cfg)` → `SummarizerProvider::complete_with_meta(system, user)`. This gives local-Qwen-vs-cloud-Claude routing, consent gate,
  `RedactingProvider` (redaction firewall), and the egress ledger FOR FREE. NEVER call a provider
  directly / `make_provider` — always `provider_for`. `CallMeta` carries the model label + redaction
  flag for `NoteAssistResult.modelLabel`/`mode`/`redacted`.
- enhance-context retrieval: run `search_visible` (meeting notes/segments) + `search_doc_chunks_visible`
  (other notes/docs) over the user's brain, EXCLUDING the current note id, top-K (≤6), build a grounded
  prompt, ask the model to propose an ADDITIVE passage that expands the selection using ONLY the
  retrieved facts, returning citations. Gated: only visible/unlocked sources contribute.
- Auto-organize: `organize::classify_subfolder(...)` + `organize::sanitize_folder(reply)` to pick a
  vault-safe target folder name per note from its content; cluster/route notes; produce `OrganizePlan`.
- Vault export: `export::obsidian::write_note(...)` + provenance front-matter helpers. Notes export to
  `<vault>/Notes/<folder-path>/<title>.md`. Idempotent, atomic, collision-suffixed (tests exist for
  `write_note`). On lock, delete the vault file (like meeting notes); on unlock/remove_lock, re-export.

---

## 4. Note editor (frontend) — `src/app/features/notes/note-editor/`

Obsidian/Notion-grade, **no new npm deps** (only `marked` + `dompurify`, already present). Zoneless
signals, standalone, OnPush, dir-per-component, design-tokens only.

- **Mount:** shell child route `/notes/:id` (and `/notes/new` → create then replace URL with the new
  id). Renders in `<router-outlet>` (app-shell) exactly like `/meeting/:id`. Reuse the RouteReuse
  gotcha handling (explicitly reload on param change).
- **Layout (single centered document, `--content-max` 840px):**
  1. **Title** — large borderless input (`H1`-scale), placeholder "Untitled". Autosaves.
  2. **Properties bar** — Obsidian-style properties: `tags` (chip input w/ autocomplete over existing
     tags), plus add-property (`status` select, `date`, `aliases`, free key/value). Serializes to YAML
     front-matter. Collapsible. Design: quiet `.panel-card`, chips = `.pill`.
  3. **Body** — the markdown editing surface. Implementation: a styled `<textarea>` (auto-growing) as
     the source-of-truth edit surface + a **Preview** toggle rendering via `app-markdown`
     (`MarkdownComponent`). Provide:
     - a **formatting toolbar** (H1/H2/H3, bold ⌘B, italic ⌘I, strikethrough, bullet list, numbered
       list, checklist, quote, code, inline code, link, `[[wikilink]]`, divider) that wraps/toggles
       markdown around the selection;
     - **markdown keyboard behaviors** in the textarea: auto-continue lists/checkboxes on Enter,
       `Tab`/`Shift-Tab` indent, `⌘1/2/3` headings, smart list renumbering;
     - a **slash `/` menu** at line start to insert blocks (heading, list, checklist, quote, code
       block, table, divider, callout). Opaque `--surface-overlay` menu (T3).
  4. **Header bar** (sticky): folder breadcrumb + move, Preview toggle, Share, ⋯ (lock status,
     delete, export-reveal), and a subtle **Saved / Saving…** indicator.
- **Autosave:** debounced (`DebounceService`) `update_note(id,title,markdown)` on change (~600ms
  idle) + on blur + on route-leave. Optimistic; reconcile with returned `NoteDoc`. Signals only.
- **Headers required** (user ask): the toolbar + slash + `⌘1/2/3` all produce `#`/`##`/`###`; preview
  renders them via marked. The document supports the full markdown heading hierarchy.
- **Locked note:** if `get_note` returns `locked:true`, show the lock gate (reuse the detail lock-gate
  pattern: masked, Unlock button → `unlock_folder`/biometric). No body rendered while locked.
- **Perf:** the textarea handles large notes; preview render is `computed` off the markdown signal
  (cached). Do not re-render preview on every keystroke while in edit mode (only when Preview shown).

## 5. Selection Brain-assistant popover — `src/app/features/notes/note-brain-popover/`

- **Trigger:** on a non-empty text selection inside the body editor, float a popover ABOVE the
  selection rect (position via `Range.getBoundingClientRect()`; reposition with `afterNextRender({injector})`,
  no `setTimeout`). Opaque `--surface-overlay`, `--border-strong`, `--shadow-lg`, `backdrop-filter:none`
  (T3). Dismiss on outside-click / Escape / selection-collapse.
- **Actions (3 primary):** **Refine** (improve clarity/grammar/flow, same meaning), **Shorten**
  (make concise), **Enhance context** (retrieve related notes/meetings from the brain and propose an
  ADDITIVE expansion with citations). A small **mode chip** ("via Claude" / "via Qwen local") shows
  the current brain mode (from `NoteAssistResult.mode/modelLabel`, or a pre-fetched posture).
- **Stepped, animated flow (rich + clear steps):** on action click, the popover expands into a
  step tracker with a shimmer progress:
  - Refine/Shorten: `Reading selection → Drafting → Ready`.
  - Enhance: `Reading selection → Searching your brain → Found N related → Drafting → Ready`.
  Steps animate in sequence (client-driven; the IPC call is a single `note_assistant_action` await —
  animate steps optimistically while awaiting, land on the real result). Model the trace-chip pattern
  from `AskComponent` (tool-trace rows). Honor `prefers-reduced-motion`.
- **Result = reviewable diff:** show original (struck/subtle) vs suggestion (accent). For Enhance,
  show the additive passage + citation chips (click → open source note/meeting). Buttons: **Accept**
  (apply with a smooth insert/replace animation into the textarea + autosave), **Discard**, **Retry**.
  Refine/Shorten replace the selection; Enhance inserts the passage after the selection (keeps user
  text intact — additive, never destructive).
- **Stale-result guard (mandatory):** the fetch is an `effect()`/async with an `activeRequestId`
  signal guard — a late reply for a superseded selection/action is dropped (report trap #4).
- **Mode-awareness:** the backend routes via `provider_for(Role::Notes)`; the popover only DISPLAYS
  mode. Local mode → Qwen; cloud → Claude/anthropic — automatic, per posture.

## 6. Settings — AI & Models: Note assistant

Add a "Note assistant" group in `src/app/features/settings/` AI & Models:
- Which model handles note actions: reuse `Role::Notes` (v1) so it inherits the Notes connection
  (local/cloud) — surface a read-only line explaining "Note edits use your Notes model
  (<resolved model>)". If time permits, add a dedicated `Role::NoteAssist` picker row (extend
  `roles::Role`, `resolve`, `provider_target`, `ai_map_rows`, config keys) — OPTIONAL, gate behind
  the same posture UI. Prefer reuse for v1.
- Toggles: `noteAssistRefine`, `noteAssistShorten`, `noteAssistEnhance` (default all ON). Persist via
  `settings/config.rs` (add fields to the AI config DTO + load/save). The popover hides a disabled
  action.
- Copy must NOT name competitors (memory: no competitor comparisons in user-facing copy).

## 7. Sharing (WP6)

Notes shareable like meetings: reuse `share::` + `e2ee::` (envelope `clean_note_body`,
`seal_link_share`, `ShareClient::create_share`, `assemble_share_url`, egress ledger). Add a `document_id`
anchor to `outbound_shares` (additive nullable) so a share can reference a note. Gate: refuse sharing a
sealed-not-unlocked note (mirror the meeting test that refuses sharing a sealed meeting). Share panel in
the note header reuses the meeting `SharePanelComponent` patterns.

## 8. Auto-organize (WP-organize)

Notes section action "Auto-organize": `plan_organize_notes(folderId?)` proposes, per note, a target
note-folder by content (via `organize::classify_subfolder` + embeddings/LLM), returning `OrganizePlan`
(moves with reasons; new folders flagged). FE shows a review sheet (accept all / per-move toggle) →
`apply_organize_plan` creates needed note-folders + moves notes (gated; re-exports). Non-destructive,
confirm-before-apply.

---

## 9. Invariants & risks (BINDING — verified by adversarial-verifier + lock-security-reviewer)

1. **No sealed-content leak.** Every note read/list/export/share/assistant path gates via
   `folder_is_unlocked`(commands) / `visibility_clause`(db). `list_notes` filters IN THE QUERY (never
   per-row skip → title leak). Mask title too (topic leak). enhance-context retrieval contributes
   ONLY visible sources. Sharing refuses sealed-not-unlocked.
2. **Seal verify-before-destroy.** Note seal encrypts → decrypts-verify byte-identical → THEN blanks
   `text`; purge `doc_chunks`/`doc_vec_chunks`/`fts_doc_chunks` in the SAME tx; delete the vault .md.
   Round-trip unit test required. Unlock/remove_lock restores text + re-indexes + re-exports. AAD
   deterministic from row identity (folder+doc) or unlock fails closed.
3. **No new FE dep.** Editor = textarea + `MarkdownComponent`. No CKEditor/ProseMirror/CodeMirror.
4. **Zoneless.** signals/computed/effect only; stale-result guard on assistant fetch; built-in
   `@if/@for track id`; `afterNextRender({injector})` for popover positioning/focus (no setTimeout/rAF
   in components); IPC only via typed `IpcService` methods.
5. **Opaque overlays (T3).** Slash menu, selection popover, move/⋯ menus = `--surface-overlay`,
   `backdrop-filter:none`. Never the frosted `.card`.
6. **CSP (T4).** No index.html/CSP change. Component styles only (covered by
   `dangerousDisableAssetCspModification:["style-src"]`). Only the notarized WKWebView build proves it.
7. **Redaction firewall.** Cloud assistant actions pass note text through `RedactingProvider` (via
   `provider_for`) — surface `redacted` in the result. Never egress un-redacted.
8. **`generate_handler!` registration.** Add every new command to `lib.rs` in the same change.
9. **Additive migrations only.** No DROP/DELETE/rewrite. `migrate()` stays idempotent (extend the
   idempotency test). Real user DBs exist.
10. **No PII in logs.** IDs/stages/counts only — never note title/body/tags/paths-with-content.
11. **Meetings untouched.** `folders.kind` defaults 'meeting'; existing folder/meeting behavior must be
    byte-identical (no regression to the meetings folder tree / lock / brain).

---

## 10. Build plan (work packages; BE serialized on commands.rs/db.rs, FE parallel)

`commands.rs` (>14k lines) and `db.rs` (>12k lines) are shared by all BE WPs → **serialize BE edits**
(one owner, ordered). FE files are disjoint → parallel. Frozen IPC contract (§2) lets FE build before
BE lands.

- **BE (sequential, one owner):** WP0 schema+models+CRUD+gating → WP1 vault export → WP2 lock/seal leg
  (verify-before-destroy; lock-security-reviewer gate) → WP3 update-reindex/brain participation → WP4
  assistant action (provider_for + enhance retrieval) → WP5 auto-organize → WP6 sharing. `cargo test --lib`
  green after each; add unit tests (gate, seal round-trip, migration idempotency, enhance excludes
  current note, sealed refuse-share).
- **FE (parallel against §2):** FP0 nav+icon+routes+drilldown-lockstep + Notes home (folder rail +
  note list) → FP1 IPC methods + models DTOs → FP2 editor (title/properties/body/toolbar/slash/preview/
  autosave) → FP3 selection Brain popover (steps/animations/diff/accept, stale-guard) → FP4 settings +
  share panel + lock gate. `ng lint` + `ng build` green.
- **Verify:** adversarial-verifier owns PASS/FAIL; lock-security-reviewer audits WP2/WP6; live
  Playwright repro at `:1420` with mocked `invoke`; `scripts/ci.sh` final. Ship via QueaT PR to `murmur`.
