<!-- Phase 1 in-app graph plan, code-grounded, 2026-06-26 -->

# Murmur Phase 1 — In-App Self-Assembling Graph: Decision-Ready Plan

Verified against live code (`db.rs:87/249/460/828/944/1085/1145/1183`, `commands.rs:730/1348/1704`, `lib.rs:51/84/120`, `pipeline.rs:182/264`, `state.rs:31`). All references confirmed.

## 1. Chosen visualization approach + why

**Hybrid, two-layer: structured People/Projects directory (spine) + a bounded single-entity inline-SVG "neighborhood" view (decoration).**

- **REJECT full force-directed graph for v1.** A hand-rolled TS physics sim in a zoneless app fights change detection, burns battery on a desktop recorder, degrades past a few hundred nodes, is screen-reader-invisible, and grows unboundedly with every meeting — the classic "demo-pretty, field-janky" prod risk. No npm graph lib is allowed anyway.
- **Directory is the prod-ready, complete feature on its own:** grouped sortable entity cards with visible mention counts, expandable backlinks reusing the existing `sources.component` chip → `/meeting/:id` deep-link. Keyboard-native, scales to thousands via client slice/`limit()`, zero layout math.
- **Neighborhood SVG is additive and bounded:** selected entity at center, top-K≈12 co-occurring satellites on a circle, edge width ∝ shared-meeting count. Positions computed **once via `cos/sin` in a `computed()`** — never simulated, no `requestAnimationFrame`/`setInterval`. Reuses the `murmurWave` brand glyph in the hub, `--accent-gradient` for people vs violet `#9d7bff` for projects (existing tokens). If it ever feels heavy, drop the SVG with zero functional loss.

This is the prod-ready call because the feature is correct and complete using only the directory; the SVG decorates a sound data model rather than being load-bearing.

## 2. Build order (numbered; file:symbol; size; inline vs workflow)

Strictly serial dependency chain 1→4 (each step's output is the next's input); 5–6 fan out.

| # | Step | file:symbol | Size | Execution |
|---|------|-------------|------|-----------|
| 1 | **Schema** — append `entities` + `entity_mentions` `CREATE TABLE/INDEX IF NOT EXISTS` to the idempotent batch; add `EntityKind` enum (`as_str`/`FromStr` mirroring `MeetingStatus` at `db.rs:16-45`) + `EntityWithCount`/`GraphEdges` structs | `storage/db.rs::migrate` (~`:147`, alongside `idx_folders_parent`), `storage/models.rs` | **S** | **Serialize-inline** (foundation; everything blocks on it) |
| 2 | **DB helpers** — `upsert_entity` (case-insensitive dedup via `name_ci`, keep first-seen casing, `INSERT OR IGNORE` + re-read for race), `add_mention` (idempotent PK), `list_entities_visible`, `entity_mentions_visible`, `graph_edges_visible` — **all reuse `visibility_clause("n", unlocked)` (`:1183`)** over `entity_mentions → meetings → notes n LEFT JOIN folders f`, replicating the `EXISTS(visible note) OR NOT EXISTS(any note)` predicate of `list_meetings_visible` (`:1085`)/`meeting_is_visible` (`:1145`) verbatim | new section `storage/db.rs` (~`:1080`, before visibility block) | **M** | **Serialize-inline** (correctness-critical; the anti-leak guarantee lives here) |
| 3 | **Dual-sink + pipeline hook** — extract `build_and_persist_entities` free fn (Sink A: always DB upsert+mention; Sink B: vault stub mirror **only if vault configured AND meeting's folder `locked=false`** via `folder_by_id().locked` disk-truth, NOT session-unlock); rewrite `link_meeting_entities` to thin wrapper (drop the hard "no vault" error); add best-effort call in pipeline after export | `commands.rs::link_meeting_entities` (`:730`) + new `build_and_persist_entities`; `pipeline.rs::summarize_and_export` (just before `Ok(PipelineResult)` at `:266`) | **M** | **Serialize-inline** (touches shared `commands.rs` + `pipeline.rs`; sequencing matters) |
| 4 | **`get_graph` + `get_entity_detail` commands** — snapshot live `unlocked` set (same as `list_folders` at `commands.rs:1353`), call step-2 helpers, build `GraphData`/`EntityDetail` camelCase payloads; register both in `generate_handler!` | `commands.rs` (new commands + `GraphNode/Edge/Data` types); `lib.rs` `generate_handler!` (`:51`, beside `:84`) | **M** | **Serialize-inline** (shared `lib.rs` registration; depends on 2) |
| 5 | **IPC contract** — add `GraphEntity`/`GraphData`/`EntityNeighbor`/`EntityDetail` interfaces (reuse existing `VaultSource` for backlink chips); add `getGraph()`/`getEntityDetail(id)` methods | `src/app/core/models.ts`, `src/app/core/ipc.service.ts` | **S** | **Delegate-to-workflow** (frontend lane; starts once step-4 payload shape is frozen) |
| 6 | **Graph UI** — lazy `/graph` route + one nav link after Analytics; 4 standalone/OnPush/signals components: `graph.component` (container: `loading`/`isEmpty`/`kindFilter`/`sort`/`query`/`selectedId` signals, `computed` view-model), `entity-card` (`input()`/`output()`), `entity-detail` (side panel, `sources.component` chips), `entity-neighborhood` (trig-laid-out inline SVG). Re-fetch `getGraph()` on `FoldersService` lock-state change; one `.banner is-accent` disclosure when locked folders exist | `src/app/app.routes.ts`, `app.component.ts`, new `src/app/features/graph/{graph,entity-card,entity-detail,entity-neighborhood}.component.ts` | **L** | **Delegate-to-workflow** (frontend lane; depends on 5) |

## 3. Conflict map

**Shared Rust files (edited in steps 1–4, all serial — never parallelize across these):**
- `storage/db.rs` — **3 separate edit zones, no overlap:** migrate batch (~`:147`), helpers (~`:1080`), nothing in the visibility fn itself (read-only reuse). Append-only; low risk.
- `commands.rs` — `link_meeting_entities` (`:730`) rewritten **in place**; new fns + types appended; `lib.rs` import unchanged. Conflict only if another agent touches `:730` concurrently — none planned.
- `lib.rs` — single 2-line addition to `generate_handler!` array (`:84` neighborhood). Trivial, but it's the one file every Tauri-command PR touches → land step 4 before any other command work.
- `pipeline.rs` — single insertion before `Ok(PipelineResult)` (`:266`); shared by both `run_inner` and `resummarize_existing` (`:300`) → re-summarize refreshes the graph for free, `add_mention` idempotency prevents double-count.

**New modules (zero conflict):** `models.rs` additions are append-only; optional `summarize/graph_sink.rs` if `build_and_persist_entities` is split out. `summarize/graph.rs` and `export/entity_stub.rs` are **reused unchanged** (graph extraction is Sink A's source; entity_stub is Sink B).

**Angular (steps 5–6, isolated lane):** all-new `features/graph/` dir; only append-edits to `models.ts`/`ipc.service.ts`/`app.routes.ts`/`app.component.ts` (one nav link). No collision with Rust lane — can run in parallel **after** the step-4 payload contract is frozen.

**Ordering rule:** 1→2→3→4 strictly serial on the Rust side (shared files + data dependency). 5 starts when 4's serialized shape is fixed; 6 follows 5. Do not parallelize Rust steps.

## 4. Lock-awareness invariants (sealed meetings never leak)

1. **Visibility is computed at READ time, never persisted.** Every graph read snapshots the live `state.unlocked_folders` (`state.rs:31`) at command entry and pushes it through `visibility_clause`. No cache to invalidate.
2. **Mentions are written once while content was readable, and survive sealing.** Sealing blanks `notes.markdown` (`seal_note:944`) but never touches `entities`/`entity_mentions`. The rows persist; they merely become *invisible* at read. **Never re-extract a sealed meeting** — on-demand extraction reads `markdown=''` and yields nothing, so it can't corrupt or rebuild a sealed graph.
3. **The graph join reaches the predicate through the meeting's notes:** `entity_mentions → meetings → notes n LEFT JOIN folders f`, asserting `EXISTS(visible note) OR NOT EXISTS(any note)` — byte-identical to `list_meetings_visible`. The graph can never disagree with Library/MCP about what's visible.
4. **`HAVING mention_count > 0` makes single-sealed entities vanish.** An entity mentioned *only* in sealed-not-unlocked meetings contributes zero visible mentions → drops out of nodes, edges, and counts entirely. Its name lived only in encrypted markdown, so it never reaches the renderer.
5. **`relock_all` (`commands.rs:1704`, fired on screen-share start) clears the set** → the next `get_graph` instantly drops every sealed contribution. FE re-fetches on the shared `FoldersService` signal change — live drop-out, no stale view.
6. **`mentionCount` is ALWAYS the visible count** — the backend never leaks a higher "true" count. The FE makes no security decision; it renders what the backend returns, plus one honest `.banner` disclosure when locked folders exist.
7. **Sink B uses folder `locked` (disk truth), not session `unlocked`.** Even a session-unlocked folder must NOT re-emit `.md` stubs (they were removed on seal, stay out until permanent remove-lock). Reads use `unlocked`; the vault-write gate uses `locked`. This is the one place the two differ and must not be conflated.

## 5. Test plan

**Rust (`#[cfg(test)]` in `db.rs`, mirroring `migrate_is_idempotent` at `:1381`; in-memory SQLCipher Db):**
- **Entity dedup:** `upsert_entity("Anna Kowalska", Person)` then `upsert_entity("anna kowalska", Person)` → same id, **first-seen casing kept** ("Anna Kowalska"). Different `kind` with same name → two distinct rows (the `(name_ci, kind)` unique index). Concurrent-insert race path returns the winner's id.
- **Mention idempotency:** `add_mention` twice → one row (PK), `list_entities_visible` count = 1.
- **Visibility filter — the core anti-leak test:** seed entity E mentioned in meeting M whose note is in folder F. With F open OR `unlocked={F}` → E present with count 1; with F locked and `unlocked={}` → E **absent** (count 0, filtered by `HAVING`). Entity mentioned in BOTH an open meeting and a sealed one → present with count = **1** (visible only), never 2.
- **Co-occurrence visibility:** two entities sharing one sealed meeting → no edge when sealed; edge with `weight=1` when unlocked. Pair-dedup (`a.entity_id < b.entity_id`) yields exactly one edge per pair.
- **Cascade:** `delete_meeting` prunes mentions (existing `ON DELETE CASCADE`); entity with zero remaining mentions disappears from `list_entities_visible`.
- **Dual-sink skip:** `build_and_persist_entities` for a meeting in a `locked=true` folder → DB rows written, **zero vault `.md` files** created (assert against a temp vault dir). Open folder → both DB rows AND vault stubs. No vault configured → DB rows only, no error.

**Frontend (existing test harness):**
- `get_graph` empty → `isEmpty` state renders `.empty-state`; non-empty → grouped People/Projects with correct counts.
- `kindFilter`/`sort`/`query` `computed` view-model derivations.
- Neighborhood `computed` lays out exactly K satellites at correct trig coordinates; satellite click re-selects.
- Lock-state change on `FoldersService` triggers `getGraph()` re-fetch (sealed entity drops live).

**System / manual:** capture a meeting → entity appears in graph → move it to a folder → seal+relock → entity disappears → session-unlock → reappears; confirm vault has stubs for the open-folder meeting only.

## 6. The 2–3 things that must be right + risks

**MUST be right:**
1. **The visibility join is byte-identical to `list_meetings_visible` (`db.rs:1085`).** If the graph's predicate drifts from the canonical one, a sealed meeting's entity leaks — defeating Phase 0's entire lock model. Mitigation: copy the `EXISTS/NOT EXISTS` clause verbatim, and the visibility test (§5) is the gate. This is the single highest-stakes line of code.
2. **Sink B gates on folder `locked` (disk), not session `unlocked`.** Conflating them re-writes encrypted-content `.md` stubs back to the vault on a session-unlock — a plaintext leak to disk. The seal-timing subtlety (extraction needs plaintext, only available while open/unlocked; rows persist through seal; never re-extract sealed) must be implemented exactly as specified.
3. **Best-effort graph build never fails the note.** The `pipeline.rs:266` hook must `warn!`-and-continue on error. A graph-extraction LLM failure blocking note export would be a severe regression to the core product.

**RISKS:**
- **`name_ci` Unicode folding:** populate via `name.to_lowercase()` (full Unicode), NOT the existing ASCII-only `eq_ignore_ascii_case` in `graph.rs::clean` — otherwise accented names ("Zoë") dedup inconsistently. Superset of current behavior, no regression; optionally align `clean` too.
- **Graph growth / first-paint cost:** mitigated by the cheap aggregate `get_graph` (O(entities)) + lazy `get_entity_detail` (heavy join paid only on selection) + client `limit()` slice + server-capped top-K neighbors. No global layout, so no perf cliff.
- **Re-summarize double-counting:** `add_mention` idempotency (`INSERT OR IGNORE` on PK) makes the `resummarize_existing` path safe — verified by the idempotency test.
- **Orphan entities** (lost all mentions via cascade): harmless — filtered by `HAVING count > 0`; optional GC later, not v1.

**No new npm packages** (hand-rolled SVG only) and **no new Rust crates** (`uuid`/`chrono`/`rusqlite`/`serde` all already in use) — consistent with Murmur's self-contained positioning. Obsidian vault stubs remain the OPTIONAL Sink B mirror; the encrypted DB tables are the source of truth.
