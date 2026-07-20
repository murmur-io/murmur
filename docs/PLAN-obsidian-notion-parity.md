<!-- Plan authored 2026-07-14 via a Workflow fan-out (4 feature-dev:code-architect blueprints, code-grounded ~v0.9.13) + a self-audit against the binding rules. Follow-up to docs/research/2026-07-14-obsidian-notion-fusion-audit.md. -->
# Plan — Obsidian/Notion parity: 4 features

Four grounded, additive features that close the biggest gaps from the fusion audit **and** fix the
MCP full-notes/transcripts complaint. Every one respects the binding rules: **gate every read**
(`visibility_clause` / `meeting_is_unlocked`), **additive migrations only**, **verify-before-destroy**
(untouched — none add a new seal path), **no new deps**, **`cargo test --lib` loop**,
**RED-before-GREEN** per behavior change. All four are **lock-touching** (new content reads / a new
metadata exception / a new aggregate) → each ships only after **adversarial-verifier + lock-security-reviewer**
sign-off (build-time gate, per the DoD).

## Sequencing (dependency-honest)

| Order | Feature | Effort | Depends on | Why here |
|---|---|---|---|---|
| **1** | **D — MCP full notes + structured transcripts** | M | none (backend-only, no FE) | The literal complaint; self-contained; quick credibility win. |
| **1‖** | **A — Note↔note backlinks reader** | M | none | Independent of D → **build in parallel** (different files). Closes Obsidian's most-loved gap. |
| **2** | **B — Saved views (Table/Board/Calendar)** | M–L | none (but is C's shell) | Biggest "feels like Notion" payoff per effort; delivers the `mur-table`/view shell C reuses. |
| **3** | **C — Typed props + Table/Board + `query_database`** | L | B's view shell (FE only) | The structured-DB substrate. Backend (schema + typed rows + tool) is shell-independent and can start alongside B; only the Table/Board FE waits on B. |

**Start with D + A in parallel** (fully disjoint), then B, then C. Each feature below lists its
**smallest verifiable first slice**.

---

## Feature D — MCP returns FULL notes + structured transcripts  ·  M  ·  backend-only

**Reality check (verified in code):** `get_meeting` over MCP already returns the **full** note
markdown + **full** transcript — `RESULT_BUDGET=4000` truncates only the *in-app* Ask loop
(`agent.rs:326`), NOT the MCP surface. So the complaint is *partly* wrong for meetings. The **real**
gaps: (a) the transcript is **flattened** (`segs…map(text.trim()).join(" ")` in `tools.rs` GetMeeting)
— it drops `Segment.speaker` + `start_s/end_s`; (b) **no `get_document` tool** — standalone Notes
(`documents kind='note'`) + imports are reachable only as **search snippets**, you can't fetch a full
note body by id; (c) search hits don't say whether an id is a meeting or a document.

**Changes (no migration):**
- **Structured transcript:** add `transcriptFormat: "structured"|"plain"` (default `structured`) to
  `get_meeting`; new `format_structured_transcript(&segs)` renders `[start_s–end_s] Me/Others/Unknown: text`.
  Render happens **inside the existing `meeting_is_visible` `Ok(true)` arm** — gate untouched, can't leak
  more than the flat form. Keep `plain` byte-identical (backward-compat regression guard).
  **Timestamps in raw seconds**, not MM:SS (avoids the 2h+ meeting wrap bug — matches the perf memory).
- **New `get_document` tool:** `Db::get_document_if_visible(id, unlocked)` = a structural **clone of the
  proven `search_doc_chunks_fts_visible` `visibility_clause` JOIN** (`documents d LEFT JOIN folders f`),
  reads both `kind='note'` and `kind='document'`. Returns `None` (generic "No data" sentinel, never a
  masked partial — matches the MCP convention) for unknown-or-sealed. New `ToolCall::GetDocument` +
  `tool_specs` + `execute_tool` arm + `AssistantScope::VAULT_READS`; mirror onto `mcp.rs`
  `tools_spec`/`dispatch_tool` (bump the `tools_list` count 7→8).
- **Disambiguate hits:** add `DocChunkHit.kind` (existing `documents.kind` column, additive field —
  touches the 2 existing constructor SELECTs) and prefix `format_hits_and_docs` lines with
  `[meeting:{id}]` / `[document:{kind}:{id}]`.

**Leak-safety:** `get_document_if_visible` is a **new gated content read** → its own RED-before-GREEN
sealed-folder test (both kinds invisible pre-unlock, reappear post-unlock), not just the analogy to the
FTS reader. Structured transcript strictly after the meeting gate.

**First slice:** `get_document` + `get_document_if_visible` with the sealed-folder test (the literal
"return full notes" fix). Then the structured transcript. **Release-note** the `structured` default flip
(existing MCP callers see a richer/longer transcript).

---

## Feature A — Note↔note backlinks reader ("Linked mentions")  ·  M

**Gap:** wikilinks are *written* to the vault but never *surfaced* in-app; the only "backlink" surface
is entity co-occurrence (`entity_mentions`), NOT Obsidian's note↔note "what links here".

**Changes (no migration — on-demand, no index table):**
- `extract_wikilink_titles(text)` — pure, regex `\[\[([^\]\|#]+)` (degrades `[[T|alias]]`/`[[T#h]]` to
  bare title), reuses the existing `regex` crate. RED-before-GREEN unit tests.
- `backlinks_for_visible(target_kind, target_id, unlocked)` in `db.rs`: **(1) target-visibility FIRST**
  → `Ok(vec![])` if the target itself isn't visible (stops the "this locked note HAS backlinks"
  existence leak); **(2)** scan only bodies selected by the **same `visibility_clause` predicates as
  `list_meetings_visible`/`list_notes_visible`** (sealed sources never enter the scan set); **(3)** keep
  sources whose extracted titles contain the target's exact title; newest-first.
- New DTOs `SourceKind`/`BacklinkSource` (additive, doesn't touch meeting-only `VaultSource`).
- `get_backlinks` command + register in `lib.rs`.
- FE: `getBacklinks` IpcService method; new `app-backlinks` chip-row component (copies `app-sources`
  visual language, `var(--token)` only); wired into `note-panel` (meeting detail Note tab) + `note-editor`,
  **skipped while locked**, stale-guarded effect, **cleared on live seal** (`onLockTreeChanged`).

**Leak-safety:** two gates (target-first + source-scan), both RED-before-GREEN (sealed target hides all;
sealed source never contributes; unlock reverses both). Mirrors `build_entity_detail`'s gate-in-the-builder
precedent.

**Effort note / open Qs:** O(n) body scan per detail-open — fine at hundreds–low-thousands of notes;
profile before a precomputed index. Title collisions → resolve to **all** same-titled matches (no silent
false-negative). Uploaded `kind='document'` as a *source* = fast-follow.

**First slice:** `extract_wikilink_titles` (RED→GREEN) → `backlinks_for_visible` + the two gate tests →
command → the meeting-detail chip-row.

---

## Feature B — Saved views (Table / Board / Calendar)  ·  M–L  ·  mostly FE

**Gap:** one date-sorted list, folder-OR-tag filter. No switchable/saved views.

**Changes:**
- **Additive** `saved_views(id, scope, name, layout, config, sort_order, created_at, updated_at)` +
  index — mirrors `saved_recipes` exactly. Stores view **definitions only** (filters/sort/columns as
  opaque strings), **never content** → the one legitimately **un-gated** new command set
  (`list/upsert/delete/reorder_saved_views`). **Call this out explicitly in the PR** so lock-security
  doesn't flag a "missing gate".
- Two small gated backend extensions: `NoteSummary.properties` (additive field, masked row → `{}` like
  `tags`) so views group/filter by front-matter; and **`list_meeting_action_summaries`** — a per-meeting
  open/done rollup **gated exactly like `list_open_commitments`/analytics** (a sealed meeting contributes
  **zero rows**, not a masked row — the aggregate-leak class from the Analytics-tab incident).
- FE: root `SavedViewsService` (stale-while-revalidate), stateless `ViewEngineService` (filter/sort/group
  over the **already-masked** rows the gated list commands return), 4 `mur-*` view components
  (switcher/toolbar/board/calendar) following the `mur-table` contentChildren idiom. **Overlays opaque**
  (`--surface-overlay`) — view switcher / filter menus must not use the frosted `.card`. Default (no view
  selected) renders **byte-identical** to today.

**Leak-safety:** views only re-present **already-gated/masked** DTOs — the mask boundary is untouched.
The single new content-derived read is `list_meeting_action_summaries` → adversarial-verifier checks it
excludes sealed meetings.

**First slice:** the `saved_views` table + one **Table** view over the existing meetings list (no
board/calendar yet), persisted + restored.

---

## Feature C — Typed properties + folder Table/Board + `query_database`  ·  L  ·  (needs B's shell)

**Gap:** note "properties" are untyped `Record<string,string>`; no user-typed DB, no views over them, no
AI-over-structured-data.

**Load-bearing constraint:** a "row" stays **one `documents(kind='note')` `.md` with YAML front-matter**.
Typing is a **presentation+validation layer over the same strings** — `split_front_matter`/`parse_front_matter`
and `front-matter.ts` are **not touched**, so the byte-round-trip + seal (`text_blob`) path is unaware
anything changed. **No block model, no cross-note relations/rollups** (they'd fight owned-`.md`) — deferred.

**Changes:**
- **Additive** `note_folder_schemas(folder_id PK → folders ON DELETE CASCADE, schema_json, updated_at)` —
  a JSON array of `{key, kind: text|select|date|checkbox|number, options}`. Advisory metadata, not a SQL
  constraint; a note whose front-matter doesn't match still loads (unknown values preserved as Text,
  never dropped).
- `list_notes_visible_typed(folder, unlocked)` = thin wrapper over the **existing gated
  `list_notes_visible`** + per-row coercion → the ONE read the Table/Board view and `query_database` share.
- Commands: `get_note_folder_schema` (⚠️ **readable on a locked folder** — metadata names/types only, not
  values; a **deliberate narrow exception** flagged for explicit lock-security sign-off, with a fallback to
  gate it too), `set_note_folder_schema` (gated `folder_is_unlocked`, write), `list_notes_typed` (empty for
  sealed folder). Register in `lib.rs`.
- **`query_database` brain tool:** `ToolCall::QueryDatabase{folder, filter}` — a **deterministic Rust
  filter grammar** (`key op value`, AND/OR; **no second LLM call** → egress-free, no prompt-injection
  surface; unparseable → **zero rows**, never all). Bottoms out in `list_notes_visible_typed` → sealed
  folder rows never enter the input. Mirrored onto `mcp.rs` (`tools_spec`/`dispatch_tool`, count 8→9).
- FE: typed property widgets in `note-editor` (toggle/date/select/number/text over the existing
  autosave chain), `notes-table-view` + `notes-board-view` over B's shell.

**Leak-safety:** every content read reuses `list_notes_visible`'s gate (no new predicate). The only
judgment call is the schema-readable-on-lock exception → **lock-security-reviewer decides** (fallback:
gate it, blank schema until unlock).

**First slice:** the `note_folder_schemas` migration (idempotent test) + `coerce_property_value` unit
tests + `list_notes_typed` returning `[]` for a sealed folder (RED→GREEN). Then editor widgets, then
Table/Board, then `query_database`.

---

## Constraint audit (self-review — build-time verifier still owns the verdict)

| Rule | A | B | C | D |
|---|---|---|---|---|
| Gate every new content read | ✅ 2 gates | ✅ (only new read = action-summary, gated) | ✅ reuses `list_notes_visible` | ✅ new `get_document_if_visible` gated |
| Additive migration only | ✅ none | ✅ `saved_views` | ✅ `note_folder_schemas` | ✅ none |
| Verify-before-destroy (new seal) | n/a | n/a | n/a (no new seal path) | n/a |
| Command registered in `lib.rs` | ✅ | ✅ | ✅ | n/a (MCP tools) |
| Zoneless FE (root-service list state, opaque overlays, signals) | ✅ | ✅ ⚠️ opaque overlays | ✅ | n/a (no FE) |
| No new deps | ✅ (regex existing) | ✅ | ✅ | ✅ |
| RED-before-GREEN | ✅ | ✅ | ✅ | ✅ |

**Leak-risk checklist (this repo ships leaks — verify each at build):**
1. **A** — target-visibility FIRST (existence leak); source scan gated; both RED-before-GREEN + unlock-reverses.
2. **D** — `get_document_if_visible` returns `None` (not masked partial) for sealed; own sealed test for **both** `kind`s; structured transcript strictly after `meeting_is_visible`.
3. **C** — `query_database`/`list_notes_typed` reuse `list_notes_visible`; **decide** the `get_note_folder_schema`-on-lock exception with lock-security.
4. **B** — `list_meeting_action_summaries` excludes sealed meetings (aggregate-leak class); views re-present only already-masked DTOs.

**Every feature → adversarial-verifier + lock-security-reviewer before merge; QueaT commit; PR to `murmur` (never direct push).**
