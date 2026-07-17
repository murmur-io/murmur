# Note↔Meeting linking + source-scoped Brain — design spec

Date: 2026-07-17 · Status: pre-implementation (design approved by the 4 forks below)
Base branch: `feat/note-meeting-links-source-brain`, forked from `feat/brain-v3-knowledge-diff`
tip `830f48e` (brain v3 PR-1..4). Merges AFTER brain v3 lands on `murmur`.

## Why this builds on brain v3 (not from scratch)

Brain v3 PR-3 (`#364`) already ships the **link engine** this feature needs — do NOT duplicate it:

- **`links` table** (`db.rs::migrate_links`): `id, src_kind, src_id, dst_kind, dst_id, edge_type,
  score, created_by DEFAULT 'user', status DEFAULT 'active', created_at,
  UNIQUE(src_kind,src_id,dst_kind,dst_id,edge_type)` + `idx_links_src`/`idx_links_dst`. Kinds
  `meeting|note|document` (`links::LinkKind`); edge types `wikilink|companion|semantic`
  (`links::EdgeType`).
- **Gated readers/writers**: `links_for_visible(kind,id,unlocked)` (both-endpoint gate),
  `upsert_link_tx`, `purge_links_tx` (drops edges touching a sealed folder's items, both
  directions), `accept_link`/`dismiss_link`, `link_by_id`, `link_endpoint_title_visible`,
  `index_wikilinks_for_source` (delete-then-insert `[[Title]]`→id edges on note save).
- **`.md` materialization** (the reuse path for "wikilink when source is a note"):
  `commands.rs::materialize_accepted_link` + `merge_related_hit` + `crate::enrich::apply_link_markers`
  write a `[[Title]]` into an OWNED, session-VISIBLE note's managed `murmur:links` block, gated.
- **FE**: `app-connections` (`src/app/shared/connections/`) self-loads `list_links(kind,id)` and is
  ALREADY mounted in the note editor and the meeting Note tab; `list_link_candidates(prefix,offset,limit)`
  + `LinkPickerComponent` (`features/notes/link-picker/`) is the paginated candidate search;
  `LinkEdge`/`LinkKind` models in `core/models.ts`.

Brain v3's link engine is the STORAGE + GRAPH + gating. This feature is the **user-control layer** on
top: user-initiated links, a source picker that scopes Ask, link-aware retrieval, and a note-side Ask
surface. The Ask retrieval (`ask_vault`/`ask_assistant_chat`/`chat_meeting` → `tools::execute_tool` /
`summarize::vault_context`) does NOT traverse `links` today — closing that is the core value.

## The 4 approved design forks

1. **Base** = worktree off the brain-v3 branch; extend the `links` engine, no duplication.
2. **Manual link semantics** = a `links` row ALWAYS (`edge_type='manual'`); ADDITIONALLY materialize
   `[[Title]]` into the note body when the source is a note (reusing `apply_link_markers`). A meeting
   source (no markdown body) creates the row only.
3. **Source-picker default** = pre-fill the current item + its active links (removable chips); an empty
   picker falls back to whole-vault retrieval (today's behavior).
4. **"Ask about this note"** = a dedicated chat panel below the note body — the twin of the meeting's
   `app-meeting-chat`.

## Goal

Let a user (a) link a meeting to N notes and a note to N meetings/notes from a first-class dropdown, (b)
control exactly which meetings/notes feed any Brain answer via a source picker above every Ask input,
(c) have the Brain automatically pull in a scoped item's LINKED items as context, and (d) ask the Brain
about a single note. All gated by the lock model, all Obsidian-native, no new deps.

---

## PR-1 — Manual linking (backend M + FE M)

**Backend.**
- `links::EdgeType` gains `Manual` (DIRECTED, like `Wikilink`/`Companion`): extend `as_str`
  (`"manual"`), `parse`, `is_undirected()` (→ false), and the two unit round-trip tests in `links.rs`.
- New gated commands (register in `lib.rs` `generate_handler!`):
  - `link_items(src_kind, src_id, dst_kind, dst_id) -> Result<()>`: parse kinds (reject unknown as
    `InvalidArg`); require BOTH endpoints session-VISIBLE (`meeting_is_unlocked` for a meeting,
    `folder_is_unlocked` for a note/document — mirror `materialize_accepted_link`'s gate order) —
    refuse `AppError::Locked` otherwise (never link behind a lock, never reveal a locked neighbour).
    Upsert one `manual` row (`created_by='user'`, `status='active'`, `score=1.0`) via `upsert_link_tx`.
    Then, per fork #2, if `src_kind` is a note we own the markdown of, materialize `[[dst Title]]` into
    it via the EXISTING `merge_related_hit` path (skip for a meeting/document source — no owned body).
    Best-effort materialize: a skip never rolls back the row.
  - `unlink_items(src_kind, src_id, dst_kind, dst_id) -> Result<()>`: delete the `manual` row; if the
    source is an owned note, strip the matching `[[Title]]` from its managed block (best-effort, reuse
    `apply_link_markers` with the hit removed). Never touches wikilink/companion/semantic rows.
- **Display dedupe (avoid double chips)**: a note→meeting manual link that also materializes `[[Title]]`
  will, on the next save, ALSO get a `wikilink` edge. `links_for_visible` (Connections) and
  `backlinks_for_visible` MUST collapse edges to ONE chip per `(other_kind, other_id)` pair, preferring
  the deterministic edge — the SAME dedupe idiom `backlinks_for_visible` already uses for the companion
  structured leg vs the wikilink string-scan leg. Add a `manual` badge/label in the collapsed chip.

**FE.**
- `LinkEdge`/`LinkKind` already exist in `core/models.ts`; add `"manual"` to the `EdgeType` union and
  IPC methods `linkItems`/`unlinkItems` (one per command, typed — angular-zoneless §3).
- `app-connections` gains a `+ Link` control (a small button in its header) that opens a **source
  picker** (the reusable `app-source-picker`, PR-3) filtered to `meeting|note|document`; on pick →
  `linkItems(anchorKind, anchorId, pickedKind, pickedId)` then re-fetch. Manual (deterministic) chips
  get a hover `×` → `unlinkItems(...)`. Wire the same into BOTH mount sites (note editor + meeting
  `note-panel`) — `app-connections` is already in both, so this is one component change. Semantic
  Accept/Dismiss rows are untouched.
- The meeting Note tab's Connections panel is the "link notes to this meeting" surface; the note
  editor's is the "link meetings/notes to this note" surface — SAME component, symmetric.

**Lock discipline.** Manual edges are derived relations already covered by `purge_links_tx`
(edge-type-agnostic) on seal and by `links_for_visible`'s both-endpoint gate on read. A materialized
`[[Title]]` lives in an owned note's markdown → blanked by the normal note seal. RED tests
(`db.rs`/`commands.rs` `#[test]`, headless):
- `manual_link_row_created_and_gated_both_endpoints` (sealed src OR dst hides the edge both directions);
- `link_from_meeting_creates_row_but_no_markdown` (meeting source → row only, no note body write);
- `link_from_note_materializes_wikilink_and_dedupes` (note source → row + `[[Title]]` + ONE chip);
- `unlink_removes_row_and_strips_marker`;
- `link_items_refuses_sealed_endpoint` (`AppError::Locked`).

---

## PR-2 — Source-scoped + link-aware retrieval (backend M/L)

**Parameter seam.** Add an OPTIONAL explicit-source list to the three Ask entry points (a `None`
preserves today's whole-vault behavior byte-for-byte):
- `ask_vault(question, history, ask_thread_id, explicit_sources: Option<Vec<SourceRef>>)`
- `ask_assistant_chat(messages, thread_id, anchor_text, meeting_id, explicit_sources: Option<…>)`
- `chat_meeting(meeting_id, question, history, explicit_sources: Option<…>)`
- `SourceRef { kind: LinkKind, id: String }` (new `storage::models` DTO, `Serialize`/`Deserialize`,
  FE camelCase `{kind,id}`).

**Constraint point.** Thread `explicit_sources` into the context builders
(`summarize::vault_context::build_vault_context_visible` / `build_vault_context_hybrid_visible`) and the
tool executor (`tools::execute_tool`):
- When `Some`, retrieval is PINNED to the explicit set: the FTS/vector search is SKIPPED entirely and
  the corpus is exactly the explicit sources (meetings via `pack_meetings`, notes via a
  `pack_notes`/`pack_doc_chunks` note leg) PLUS their capped link-expansion below — budget-bounded,
  nothing else. That is the point of the picker: the user controls the context, so a scoped Ask never
  silently pulls unlisted vault items. The `unlocked` visibility gate STILL applies AFTER the explicit
  filter (a sealed explicit source contributes nothing — never a leak). When `None`, the existing
  whole-vault search path is unchanged.
- The tool executor's `search_meetings`/`get_meeting`/`get_document` legs, when `explicit_sources` is
  `Some`, constrain their candidate set to that set (AFTER the visibility gate). No new
  `AssistantScope` variant — this is an orthogonal candidate constraint, passed alongside the existing
  scope.

**Link-aware expansion (the "brain knows the connections" piece).** Before packing, expand each explicit
source with its ACTIVE `links` neighbours via `links_for_visible(kind, id, unlocked)` — capped by a
named const `LINK_CONTEXT_CAP` (start 8) per source, deduped, gated — so a scoped meeting automatically
pulls its linked notes' bodies (and vice-versa). Neighbours are appended to the corpus with a lower
priority than the explicit sources (explicit first, links fill remaining budget). This is what makes
"1 note ↔ N recordings" actually feed the answer. `None` (whole-vault) does NOT auto-expand (search
already spans the vault).

**Lock discipline.** Every leg stays `unlocked`-gated; the expansion reuses `links_for_visible` (already
both-endpoint gated). RED tests:
- `scoped_ask_ignores_non_listed_meetings` (a meeting not in the set never appears in the corpus);
- `sealed_explicit_source_contributes_nothing`;
- `link_expansion_respects_visibility_gate` (a sealed linked neighbour is dropped);
- `none_sources_preserves_whole_vault_corpus` (regression: identical corpus to pre-change).

---

## PR-3 — Reusable source picker + wiring (FE M)

**New `app-source-picker`** (`src/app/design-system/source-picker/` — a reusable primitive, `mur-`
family, per angular-zoneless §6b): a chip multiselect over the OPAQUE link-picker popover pattern.
- Signal API: `selected = model<SourceRef[]>()`, `placeholder = input<string>()`, plus a trigger
  button. Opens a popover (OPAQUE `--surface-overlay`, `backdrop-filter:none`, `--border-strong`,
  `--shadow-lg` — trap T3) with a search field + paginated results from `list_link_candidates(prefix,
  offset, 40)` FILTERED to `meeting|note|document`, keyboard nav + infinite scroll (reuse the
  `LinkPickerComponent` / `RepositionOnScrollDirective` mechanics; extract the shared popover-search
  scaffold if clean, else compose). Selected sources render as removable chips (kind badge + title +
  `×`). Tokens only, signals-first, `afterNextRender` for focus/reposition.

**Wire above every Ask input**, pre-filled per fork #3 (current item + `list_links` active neighbours,
removable; empty → whole-vault):
- meeting `app-meeting-chat` (detail Note tab);
- the global Ask page (`features/ask/ask.component`);
- the note chat (PR-4).
Each surface passes `selected()` into its Ask IPC call as `explicit_sources` (PR-2). A default-prefill
helper resolves `[currentRef, ...list_links(currentKind,currentId).filter(active)]` on load.

---

## PR-4 — "Ask about this note" panel (FE S/M)

**New `app-note-chat`** (`src/app/features/notes/note-chat/`) — the twin of `app-meeting-chat`
(`features/detail/meeting-chat/`): a self-contained chat (conversation/draft/pending/error signals)
mounted BELOW the body in the ROUTED note editor (`embedded()===false` only). Its source picker
(PR-3) defaults to `[thisNote + its active links]`. It calls the source-scoped Ask (PR-2) — reuse
`ask_vault` with `explicit_sources` (simplest; thread persistence optional, mirror meeting chat if it
comes cheap). Hidden when the note is locked/masked (same gate as backlinks/Connections). This IS the
"zapytaj brain o tę notatkę" surface, and because its sources default to the note's links, it already
answers with the linked meetings/notes in context.

---

## Dependency order & parallelism

`PR-1` (manual links) ‖ `PR-2` (retrieval scope) — disjoint files (PR-1: links.rs/commands link cmds/
connections FE; PR-2: vault_context/tools/ask commands). `PR-3` (picker) depends on PR-1 (something to
link) + PR-2 (the param). `PR-4` (note chat) depends on PR-3 + PR-2. Backend PR-1/PR-2 run in parallel;
FE PR-3/PR-4 serialize after them.

## Explicit non-goals (v1)

Linking persons/entities/orgs as sources (only `meeting|note|document`); org-shared source scoping;
auto-suggesting manual links (semantic auto-link already covers suggestions); a `murmur://` deep-link
scheme; changing brain v3's semantic thresholds or graph; connectors as pickable sources.

## Verification matrix

| PR | adversarial-verifier | lock-security-reviewer | real-Mac note |
|----|----|----|----|
| 1 | yes (RED both-endpoint gates, dedupe, unlink) | yes (new write/seal-adjacent path) | — |
| 2 | yes (scope filter + None-regression) | yes (scoped/expanded reads must not leak sealed) | — |
| 3 | yes (opaque overlay, stale-guarded fetch, signals) | n/a (read-only picker over gated candidates) | picker UX on dev app |
| 4 | yes (twin-of-meeting-chat, lock-hidden) | yes (note-content read path) | Ask-about-note on dev app |

Gates per PR: `cargo test --lib` + `npx ng lint` + `npx ng build`; final `bash scripts/ci.sh`. QueaT
commits, PR to `murmur` (never direct push), no Claude trailers. The implementer never self-certifies —
adversarial-verifier owns PASS/FAIL, lock-security-reviewer is the required second gate on PR-1/2/4.
