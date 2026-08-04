# Dashboards rebuild — design

*Spec · 2026-08-04 · supersedes nothing; implements the verdict in
`docs/research/2026-08-04-dashboards-ux-research.md`.*

Research brief: `docs/research/2026-08-04-dashboards-ux-research.md` (383 lines, 12 agents,
three adversarial critiques). This document is the **decision** layer: what we build, in what
order, and what we refuse. Where the brief and this spec disagree, this spec wins — the
divergences are marked **[DECISION]** and carry the reason.

---

## 0. The invariant that outranks every phase — a board is brain context

The user's binding constraint, stated during design: *"to wszystko musi być połączone z brain
jako wspólne konteksty w dashboard"*. Verified state of the tree, so nobody re-litigates it:

**Already true (do not rebuild):**

| Path | Symbol | What it gives |
|---|---|---|
| Local MCP server | `mcp.rs:1667` `list_dashboards`, `mcp.rs:1672` `get_dashboard` | An external agent reads a board with **every tile resolved** through `commands::render_tile_for_agent`, sealed tiles redacted by `redact_tile_chrome` first |
| In-app agentic loop | `tools.rs:405` / `tools.rs:416` in `tool_specs()` | `@brain` and vault-wide Ask can pull a board in as context on their own initiative |
| Every source picker | `source-picker.component.ts::pickDashboard` | A board expands into its visible sources anywhere a picker exists |

**The gap, and it is inverted from intuition:** the board's own *"Ask this board"* is the
**weakest** consumer of the board in the whole app. `dashboard-view.component.ts:319` calls
`askVault(question, [], undefined, sources)`; a non-empty `explicit_sources` makes
`commands/mod.rs::ask_vault` (line ~4464) take the deterministic floor and **skip the agentic
path entirely** — so it never receives `get_dashboard` as a tool, and
`commands/dashboards.rs::get_dashboard_sources` hands it only `note`/`meeting`/`document`
(`_ => continue`). Claude Code over MCP sees more of the board than the panel rendered beside it.

**[DECISION] This is promoted from a Phase-4 item to a cross-cutting acceptance criterion.**

> **AC-BRAIN.** Every tile kind that renders content on screen must be readable by (a) in-app
> board Ask, (b) the in-app agentic loop, and (c) the MCP surface — through **one** renderer,
> `render_tile_for_agent`, with `redact_tile_chrome` applied first. A tile kind that a human can
> read on a board and an agent cannot is a defect, and no phase may introduce one.
>
> **The corollary that bounds it:** parity of *reading* must not become parity of *scope*.
> Derived tiles enter the prompt as a labelled **brief**, never as `SourceRef`s — `SourceRef.kind`
> is a `LinkKind` and a drift lane is not a retrievable document. The existing
> `get_dashboard_sources` filter is correct and stays.

Every phase below carries an AC-BRAIN line. Phase 4 is where it first becomes true for board Ask;
Phases 6–7 are where it must not regress.

---

## 1. What we are fixing, in one paragraph

The shipped board reads as a wall of identical grey boxes holding apologies. Three independent
causes, and they need three different fixes. **(a) A layout bug:** every tile is born
`span = 4` (`commands/dashboards.rs:477` `span.unwrap_or(4)`, and
`dashboard-view.component.ts::addTile` never passes one although `dashboards.service.ts:143`
accepts it) → 12/4 = three per row forever, plus a `min-height: 74px` floor. **(b) Thin data by
construction:** the fact extractor is never asked for quantities, mention counting is
`INSERT OR IGNORE` per *meeting* rather than per utterance, and `extract_owner` yields the
literal string `"(właściciel nieokreślony)"` — so Numbers, Pulse, Drift and Person's headline are
structurally near-empty regardless of how much the user records. **(c) A visual system that was
designed and then not shipped:** the `/dreaming` prototype's per-kind coloured mark, variable
tile sizes, smooth sparkline, and inline citations are all absent; `--text-muted` at 10–11 px
measures 3.46–3.62 : 1, below the legibility floor, across the entire secondary text layer.

Beneath all of it sits the real defect: **board Ask degrades monotonically in the variable the
feature asks the user to increase.** `fair_pack_explicit_sections` gives each source
`budget / n` characters and hard-truncates, so a 40-source board answers worse than a 6-source
one.

### Measurement caveat, carried forward honestly

The brief's hit-rate figures (Numbers 5.3 %, Drift 1.8 %, Pulse 0 %) were measured against
`~/Library/Application Support/MeetNotes-dev/meetnotes.sqlite` — **the dev database**, 78 meetings
averaging 63 s, i.e. test recordings. Per `release-murmur` rule 4, dev and release data are
isolated; the production vault was not read. **The mechanisms are proven from source and hold
regardless of vault size; the percentages are indicative, not a measurement of the user's real
vault.** No decision in this spec rests on a percentage alone — each rests on the mechanism.

---

## 2. Decisions taken

| # | Question | Decision | Consequence |
|---|---|---|---|
| D1 | Scope | **Full programme, Phases 1–8** | Several sessions. Phases 4, 5, 6, 7 need `lock-security-reviewer`; 5 and 6 are the high-risk ones |
| D2 | Numbers / Pulse / Drift | **Retire from the palette now; open a separate extractor investigation** | They stop being offered. `resolve_tile` arms stay alive (see the append-only constraint below). The extractor work is a *different* spec and must not be entangled with this one |
| D3 | New tiles | **Talk split · Quote · Board note · Stayed-on-device** | Board-scoped cadence and the board-scoped search view are deferred, not killed — cadence rides Phase 8, search rides the Phase 6 retrieval work |
| D4 | Composition | **Keep the tab, add "Pin to board" to transcript selection, note header, person page, and Ask answers** | Composition becomes a byproduct of normal use. This is Phase 7 and is the difference between a feature and a demo |

### The append-only constraint that shapes D2

`commands/dashboards.rs:1010` ends `resolve_tile` with
`other => Err(AppError::InvalidArg(format!("unknown tile kind: {other}")))`, and `get_dashboard`
collects with `.collect::<Result<Vec<_>, AppError>>()?`. **One unresolvable tile fails the entire
board**, and the user's live board contains both `numbers` and `pulse` rows. Migrating
`dashboard_tiles.kind` would be a destructive rewrite of user rows, forbidden by
`rust-tauri.md` §4.

> **Therefore "retire" means exactly:** remove the entry from `NODE_TYPES` in
> `tile-palette.component.ts` so no new one can be placed, and leave the `resolve_tile` arm alive
> rendering a proper degenerate state. **No `resolve_tile` arm is ever deleted.** This is a
> permanent rule for this feature, not a phase note.

---

## 3. Architecture — the seams we work at

Nothing here introduces a new AI path, a new gated reader, or a new egress class. That is
deliberate and is what keeps the programme reviewable.

```
                       ┌─ dashboard-view (in-app Ask) ──┐
storage/dashboards_store ─→ commands/dashboards.rs      │
  dashboards, dashboard_tiles     ├─ resolve_tile ──────┼─→ get_dashboard      → UI
  (additive only)                 │   (the ONE gated    │
                                  │    resolver)        ├─→ render_tile_for_agent
                                  │                     │      ├─→ tools.rs GetDashboard  (in-app loop)
                                  ├─ redact_tile_chrome │      ├─→ mcp.rs get_dashboard   (external)
                                  │   (applied FIRST)   │      └─→ get_dashboard_brief    (NEW, Ph4)
                                  └─ get_dashboard_sources → ask_vault(explicit_sources)
                                      note|meeting|document only — CORRECT, unchanged
```

**Four rules that fall out of this diagram, and that every phase is checked against:**

1. `resolve_tile` is the single gated resolver. No phase adds a second path to tile content.
2. `redact_tile_chrome` runs **before** any renderer. A sealed tile carries `{kind:"locked"}` and
   no fields; nothing may add one — no count, no kind glyph beyond the shared lock, no chart
   skeleton whose *shape* discloses the kind.
3. Derived tiles never become `SourceRef`s. They reach a prompt as text, through the brief.
4. Additive migrations only. **No `UNIQUE INDEX` on `(dashboard_id, kind, ref_id, config)`** —
   the user's live DB already violates it, and `Db::migrate()` runs at startup on real user DBs,
   so the index would fail migration and therefore launch.

---

## 4. Phases

Each phase is independently shippable and independently revertible. Phase 1 is the entire visible
delta the user complained about and must not be blocked by anything below it.

### Phase 1 — make the existing board look composed  ·  FE only · **M** · no lock review

Retroactive on boards that already exist; no migration, no backend.

- **Empty-state ladder** replacing eleven identical `<p class="muted">` boxes:
  - *never had data* → collapse to a **36-px dashed header strip**, `background: transparent`,
    `backdrop-filter: none`, body and footer `display:none`, copy states where data will land
    ("Figures said out loud land here"), never an apology.
  - *genuinely zero = good news* → **full tile, not grey**: `✓` in `--success` +
    `--text-secondary`. "Nothing open — every commitment on this board is closed" is a *result*;
    rendering it as an absence is the most demoralising thing the board does.
  - *degenerate* (drift `rows < 2`, pulse `total === 0`, any chart with `n ≤ 1`) → collapse the
    **mark**, keep the tile: "One value, never revised — nothing has drifted".
  - *degenerate-but-populated* → a 100 %/0 % talk bar, a one-cell strip, a one-row ledger pass
    every emptiness check and still look broken. Same treatment, keyed on a per-pattern variance
    predicate.
  - *missing / unconfigured* → strip + an inline action.
  - **Two layout rules the collapse depends on:** an empty strip takes **span 3** and **sorts to
    the end of the grid**. `.canvas` is `grid-auto-rows: max-content` with `align-self: start`,
    so a 36-px strip left in composition order beside a 200-px tile leaves ~164 px of dead air —
    scattered strips look *worse* than the grey boxes.
  - **One ghost preview per board, never more** (repeated illustrations fatigue): the first empty
    tile renders its pattern skeleton at 10 % opacity plus one sentence and a tertiary action.
- **Display-span override.** `displaySpan = tile.span === 4 ? DEFAULT_SPAN[kind] : tile.span`;
  the first explicit resize writes a real value. ~6 lines, retroactive.
  Bands: `person`/`pulse` 3 · `numbers`/`note`/`document`/`drift`/`reminders` 4 ·
  `promises`/`meeting`/`livingAnswer` 6. `3+3+6`, `4+4+4`, `6+6` — the row rhythm appears by
  itself. **[DECISION] Not an add-time default** — every existing tile already holds a concrete
  `4`, so an add-time default changes nothing about the board that was complained about.
  Delete `min-height: 74px`.
- **Render-time duplicate strip.** Two resolved tiles sharing `(kind, refId, config)` → the
  second renders as *"same as the tile above · Remove"*, and the palette scroll-highlights the
  existing tile instead of adding. Pure FE, works on the user's board today. **[DECISION] No
  backend `InvalidArg` guard** — it would be swallowed by `DashboardsService.addTile` into the
  hand-rolled `message(e)` renderer this programme is deleting.
- **Tokens.** Add `--text-tertiary` (#8a8a9c dark, 5.50 : 1; #6e6e78 light, 4.84 : 1) and
  `--hatch-stripe`, **in both the `[data-theme]` and the `prefers-color-scheme` blocks** of
  `theme-light.css`. Migrate `.tile-kind`, `.row-meta`, `.drift-meta`, `.number-key`,
  `.live-chip`, `.muted-small` off `--text-muted` (which stays for ≥13 px only). Delete the nine
  raw `rgba()` values that render white-on-near-white in light mode.
- **Per-kind mark + eyebrow-above-title** — §5.
- **Sealed tile** re-copied to *"Sealed — not in scope."* / *"Unlock the folder to bring this
  back into the board's scope."* Full-size, 45° hatch. A board that is screen-shareable as-is is
  a demo moment; today it reads as a bug.

**AC-BRAIN:** none of this changes what an agent reads. Assert unchanged
`render_tile_for_agent` output.

**Verification.** RED-before-GREEN per `angular-zoneless.md` **T5**: a Playwright spec that
fails on today's code — assert an empty-payload tile renders at `span 3` with no body, and that
nine tiles do not all report equal height. Then `ng lint`, `ng build`, and — because
`color-mix()`, `mask-image` and `backdrop-filter` are involved — **a packaged WKWebView check**
per **T4**: a green `ng build` proves nothing about the shipped engine.

### Phase 2 — data honesty  ·  **M** · no lock review

- **`normalize_owner` / `extract_owner`**: an owner head that is *only* a parenthetical becomes
  `None`. **Language-agnostic** — matching the Polish literal would silently regress for an
  English `note_language`. Also fix the note-generation prompt, or the extractor keeps producing
  the shape. **This is XS and un-zeroes `TileData::Person.open_commitments` vault-wide — the
  highest work-to-value ratio in the programme.**
- **Retire `numbers` and `pulse` from `NODE_TYPES`;** gate `drift` on a live count ≥ 2.
- **Palette counts from aggregate SQL.** `SELECT entity_id, COUNT(*) … GROUP BY entity_id` over
  `facts` (covered by `idx_facts_entity`) and `entity_mentions` (`idx_entity_mentions_entity`),
  plus `length(text) > 0` on note candidates. **[DECISION] Never speculative `resolve_tile` per
  candidate** — that is ~230 resolves per palette open, serialised on the one
  `Mutex<Connection>` a live recording writes segments through, and `resolve_tile`'s `"note"` arm
  does a full vault note-list read to resolve one note. Hide unavailable offers in the browse
  band; **grey with a reason** in the subject band, where silence would read as a missing feature.
- **Subject-first palette order.** Pick the subject, then see which tiles it can support. Today
  the user commits to "Drift lane" before discovering the subject cannot keep the promise — which
  is literally the question *"nie wiem ile one mają sensu"*, and it is unanswerable in the
  current order.
- **`promises` gains `mode: "entity"`** with an explicit *Everyone* option; the owner renders in
  the header. The backend has read `cfg.owner` since day one and no UI ever wrote it — which is
  what produced the two identical tiles.
- Wire or delete `registerField` (`tile-palette.component.ts:295`, bound nowhere, so the search
  field never focuses); add the focus move and trap the `role="dialog" aria-modal="true"`
  currently claims without implementing. Fix or delete `due_count` (it counts all active rows
  *before* the `TILE_ROWS` truncation, and the FE never renders it — both halves of the name are
  wrong).

**Honest bound on preflight:** it validates at *add* time and tiles are *persistent*. Seal a
folder and a preflight-validated board is the screenshot again, now with the user believing it
was checked. Preflight saves wasted composition effort; **Phase 1's render-time collapse is what
keeps a board good over time.** That ordering is deliberate.

**AC-BRAIN:** `Person.open_commitments` becoming non-zero changes `render_tile_for_agent` output.
Update the agent-surface oracle rather than letting it drift.

### Phase 3 — performance prerequisites  ·  **S–M** · no lock review

Do these before anything else touches `get_dashboard`; they are cheap now and load-bearing later.

- **Hoist `list_open_commitments` to once per board** in `get_dashboard` — `Db::list_people`
  already demonstrates the pattern. Today two Promises tiles cost two full-vault note scans, and
  `updateTile → reloadOpenBoard` re-resolves *every* tile, so one chevron click costs three.
- **Patch span locally** instead of `reloadOpenBoard()` — span is pure layout and no gated
  payload changes.
- **RED-first Rust oracles for `get_dashboard_sources`**, which currently has **zero** Rust tests
  despite being the function that *defines* the scope; its only coverage is Playwright mocks
  asserting the FE's own assumption. Required before Phase 5 reshapes anything near it:
  a sealed-folder source is absent · a derived tile contributes nothing · a deleted source yields
  `Missing`, not an error · duplicates dedupe by `(kind, id)`.

### Phase 4 — Ask honesty, and AC-BRAIN becomes true  ·  **M** · **lock review required**

- **`get_dashboard_brief(id) -> String`** — the board's derived tiles rendered through the
  existing, already-redaction-correct `render_tile_for_agent`, under one `lifecycle_guard`,
  capped ~4 000 chars, prepended to the pinned corpus as a labelled block:

  ```
  WHAT THIS BOARD ALREADY SHOWS (the user composed these views; do not re-derive them):
  - promises (Marcus Reid): Send Acme paperwork · due 2026-07-22 · late
  - drift: Atlas GA · target_date: Apr 30 → Jun 14
  ```

  Zero new gated reader, zero new egress class, ~1–2 % of budget. **This is the phase that
  satisfies AC-BRAIN for board Ask.**
- **Render answers and living answers through `src/app/shared/markdown/markdown.component.ts`.**
  The prompt in `summarize/vault_chat.rs::build` *demands* markdown; the board renders it as
  `{{ turn.text }}` with `white-space: pre-wrap`, so `**` and `[[…]]` appear literally. The
  component already turns `[[Wikilinks]]` into gated chips. Dashboards is the only AI surface in
  the app not using it. **Reuse its hard-won lesson:** read the title from `textContent`, never a
  `data-*` attribute — Angular's sanitizer strips unrecognised `data-*` even when DOMPurify
  allow-lists them.
- **Errors get a third turn role** rendering `.banner.is-danger` (`design-system/primitives.css`)
  with a
  Try-again button. Today an error is pushed as `{role:"assistant"}` and renders in a normal grey
  bubble, visually identical to a real answer, with no retry.
- **`errcode` tags at the provider** (`PROVIDER_TIMEOUT`, `PROVIDER_MODEL_REJECTED`,
  `PROVIDER_CLI_MISSING`, …), preserved through `summarize/redact.rs::content_free_dispatch_error`
  as a leading `[code]`. Pin with a test asserting the message matches `^\[[a-z-]+\] ` **and
  contains none of the inner error's bytes** — a kebab-case token from a closed set leaks nothing.
  Delete both hand-rolled FE renderers (`dashboard-view.component.ts::errorText`,
  `dashboards.service.ts::message`) in favour of `core/copy/error-copy.service.ts` — the boundary
  the app established by deleting nine of these, and which dashboards reintroduced two of.
- **Pass the 12-turn history.** `capped_ask_history` exists and every other chat surface uses it;
  the board sends `[]`.
- **Fix the bytes-vs-chars budget mismatch:** `pack_meetings`/`pack_notes` compute `remaining`
  from `corpus.len()` (**bytes**) then consume `.chars().take(remaining)` (**chars**) while the
  caller counts `chars().count()`. A Polish or CJK vault overruns the declared budget by 2–4×.
  This is a live correctness bug for this user specifically.

**[DECISION] No scope-blaming copy** ("this board is too big — 47 sources") until the Phase-5
manifest makes `refs.len()` real. On the board that produced the complaint it would have been
factually false: `get_dashboard_sources` yielded at most 4 sources, three of them notes holding
0 / 18 / 86 characters. **The Ask failure in the screenshot was a plain `claude_code` dispatch
failure, not a size problem** — `claude_code.rs::claude_failure_message` already produces a
content-free actionable diagnostic naming the likely bad model id, and
`content_free_dispatch_error` collapses it to one useless sentence.

**Lock review focus:** content enters a prompt through a new path, even though the renderer is
already redaction-correct.

### Phase 5 — the packer manifest (keystone)  ·  **M–L** · **lock review mandatory**

`build_vault_context_pinned_visible` returns
`PackedContext { corpus, refs: Vec<PackedRef { kind, id, title, folder_id, chars_packed, truncated, role }> }`.
Every field is already in hand where `pack_meetings`/`pack_notes` push a header. One change,
four unrelated fixes:

1. **Citations for every tile kind** — match on `(kind, id)` instead of `meetingId`. Today
   `VaultSource` carries only `meeting_id` and the note/document arm pushes `Vec::new()`, so a
   note or document tile **can never be cited**.
2. **A precise living-answer gate** — record `refs.folder_id` instead of the entire readable
   folder set. Sound, far tighter, and it fixes the shipped capacity cliff: `set_dashboard_answer`
   stores every readable folder id, a UUID serialises at ~39 bytes inside a JSON array, so
   **~210 folders exhaust `MAX_CONFIG_LEN` (8 KiB)** and an ordinary answer fails with
   `InvalidArg("answer too large")`. `list_folders` includes note folders; a real Obsidian vault
   passes 100 trivially.
3. **A truthful truncation readout** — "12 of 47 sources were trimmed to fit".
4. **Proof that MCP and in-app Ask ground identically.**

Then, and **only** then, the cited state (§5.6).

**Risk note for the reviewer: this phase NARROWS a security gate.** Keep `living_answer_withheld`
a pure function and **fail closed** on legacy or empty rows.

### Phase 6 — the scope leaves the app  ·  **L** · **lock review required** (protocol + egress)

- `tile:<id>` identifiers and a board header (`# Atlas · id:… · tiles:12 · sources:7 · sealed:1`)
  in `render_tile_for_agent` — an external agent currently **cannot cite a tile**.
- **`search_dashboard(dashboardId, query)`.** Without it, the recommended agent behaviour after
  `get_dashboard` is vault-wide search — **the scope evaporates at the exact moment the agent
  needs depth.** The id set must be pushed **into the search legs**
  (`search_visible_impl` / `search_semantic_visible` / the graph leg), **never** applied as a
  post-filter after `limit`: that returns zero rows for a board whose sources rank 41st–60th
  while the answer is demonstrably on the board. Note it is silently dead without a downloaded
  embedder (`Embedder::embed_query`) — the same stub-model failure class as the fact extractor,
  and it needs the same explicit branch.
- `get_dashboard_context` returning what in-app Ask packs, bounded by `MAX_TOOL_WINDOW_CHARS`.
- **Retrieval inside the scope.** When `n_sources × min_useful_chars > budget`, stop fair-sharing
  and retrieve top-k whole, restricted to the board's sources. Expose as a board toggle,
  **Whole board** vs **Most relevant** — neither dominates, and the user knows which they want.
  Default Whole-board under ~12 sources; **settle the threshold with the eval below, not by
  taste.**
- **Board identity through the picker.** `pickDashboard` currently splices refs into a flat
  selection, destroying board identity, snapshotting membership at pick time, and truncating
  silently at `selectionLimit`. Carry `viaBoard: {id, title}` so chips read `Atlas · 7 sources`,
  and **re-expand at ask time, not pick time** — free correctness: a tile added mid-thread joins
  the scope, a folder sealed mid-thread drops out.
- `.canvas` export (`export/canvas.rs` already emits Canvas JSON) + `murmur://dashboard/{id}` and
  the vault path on the board header.

**[DECISION] Do not wire boards into the record view.** The live transcript is the anchor there,
and `acquire_external_egress_lease` risk during recording is real.

**The measurement this phase owes (Track B).** Nobody proposed an oracle, and without one the
thesis is unfalsifiable: **until board-scoped Ask beats vault-wide Ask on a fixed question set
drawn from the real vault, "a board is a retrieval scope" is an aesthetic preference with a
canvas attached.** `eval/` exists. This experiment also adjudicates the Whole-board / Most-relevant
default.

### Phase 7 — the four new tiles + "Pin to board"  ·  **L** · **lock review required** (Quote)

| Tile | Source | Form | Note |
|---|---|---|---|
| **Board note** | `dashboard_tiles.config.text` + existing `app-markdown` | prose | **XS.** The board cannot currently be *written*, only assembled — and the user's own sentence is the highest-value token in the board's prompt |
| **Talk split** | `segments.speaker/start_s/end_s`, `SUM(end_s-start_s) GROUP BY speaker`, under the meeting gate; additive `me_ratio`/`segment_count` on `TileData::Meeting` | 8-px stacked bar, 2-px gap | **Per meeting only.** `speaker` is `add_column_if_missing(… "TEXT")` → nullable, so pre-dual-stream recordings need an explicit NULL branch. **No board-wide rollup:** `others-N` are per-meeting cluster tags and `speaker_voiceprints` has 0 rows, so summing across meetings adds different humans together and prints it as a roster — the same class of lie as `looks_numeric` printing deadlines |
| **Quote** | `db.get_segments` behind `meeting_is_visible`; config stores **only** `{meetingId, startS, endS}` | pull-quote + timestamp | The only tile structurally incapable of being empty — content is chosen at add time. Gate path verified: `get_segments` is explicitly ungated ("callers must first pass the meeting visibility boundary") and sealing blanks `segments.text` to `''`, so a relocked session yields empty text, never stale text. **v1 must not play audio in-tile** — `lock-model.md` documents `convertFileSrc`/`asset:` as the one audio path bypassing `meeting_is_unlocked`, which is why the masked DTO nulls `audio_path`. Deep-link to the gated detail view at `start_s` |
| **Stayed on device** | `egress_store::egress_summary(days)` over `egress_log` — content-free by construction | one stat + delta | Never empty, and its zero state ("0 cloud calls · 100 % local") is its best state. No cloud notetaker can render it |

- **"Pin to board"** as a first-class action in: transcript selection (which is also how Quote is
  created), the note header, the person page, and Ask answers. **This is D4 and it is the
  structural fix** — boards are a late-game artifact sitting in the primary nav, which is exactly
  why the first one was assembled from empty notes. If a user must visit the Dashboards tab to
  *build* a board, it happens once.

**AC-BRAIN:** all four new kinds get a `render_tile_for_agent` arm in the same change, with an
agent-surface oracle. A tile a human can read and an agent cannot is a defect.

### Phase 8 — polish  ·  **M** · no lock review

Ask rail collapses to 44 px when `turns()` is empty, carrying the scope count vertically (the
scope count *is* the thesis made visible and belongs on screen permanently; the empty transcript
does not), expanding to 380 px on use · board emoji/tint/rename **writers** (`TINTS` in
`dashboards.rs` and six SCSS mappings are dead code because nothing ever writes the field) ·
home-card sealed marker, freshness line, sparkline, delete confirmation, search + segmented
control · arrange-mode grip, `Alt+←/→` reorder, `⌘←/→` resize, `:focus-visible` rings on the
twelve background-less buttons that have none, the missing `(dragleave)`, and `draggable` moved
off the host (whose body is full of `<button>`s — WebKit does not reliably bubble a drag begun on
a nested button) · **board-scoped cadence** tile (fixed heat ladder) · **board diff — "since I
last looked"** (`last_seen_at`; the grid reports *state*, this reports *change*, which is the
line between a board opened weekly and one that goes stale) · motion pass · read/compose split ·
delete the duplicated `.arrange-hint` block at `dashboard-view.component.scss:235-244`.

---

## 5. The visual system

### 5.1 Tile anatomy

```
[22px mark]  EYEBROW (10px/600/+.08em/uppercase, --text-tertiary)   [grip|tools] or [freshness]
             TITLE   (14px/640/-.01em, --text-primary)
─ body (one of six patterns, per-pattern height band) ─
─ footer (CONDITIONAL): provenance ····················· state ─
```

- **Eyebrow above title.** The eyebrow is the classifier and must be scannable down the left
  edge; title-first opens every tile with a different-length proper noun.
- **The mark** is a 22 × 22 squircle (`border-radius: 7px` — a circle reads as an avatar),
  `background: color-mix(in srgb, var(--tile-hue) 18%, transparent)`, 1-px rim at 34 %, glyph
  `stroke: var(--tile-hue)` at 1.9. **Not** the prototype's solid fill with a near-black glyph —
  eleven saturated solids is the confetti failure at ⅕ of the ink budget. Material vs derived is
  carried by **fill vs outline**, not by a fifth hue.
- **The footer is conditional.** A mandatory 28-px bordered footer on every tile re-homogenises
  the heights this spec is trying to break.
- **Divider budget: one full-bleed rule per tile, and it is the footer.** Delete
  `.row { border-bottom }` — four promise rows currently draw three full-bleed rules inside
  ~120 px, which *is* the spreadsheet read. Rows become chips on `--surface-input` with
  `gap: var(--space-1)`.

**Hue map — four hues, capped:** `{--graph-note, --graph-entity, --graph-meeting,
--graph-document}`, ordered mint → azure → amber → orchid so orchid and azure are never adjacent.
Adding a fifth (`--warning`) is a measured failure (ΔE 5.7 vs amber). Derived tiles get **no
hue** — `--text-secondary` marks. **`--accent` is reserved for the AI channel only** (citation
ring and numeral, the `livingAnswer` mark, the Ask composer): it is user-selectable and 5 of 6
options collide with a family hue (orange vs `--graph-meeting` = ΔE 2.4). Today
`dashboard-tile.component.scss:300,318,339` paints the pulse bars and the drift rail `--accent` —
that is the accent doing category work; both become `var(--tile-hue)`.

**Text never wears the data hue.** A family hue at 10 px on the light tile surface measures
3.48–5.04 : 1. Current violation: `.audio-chip { color: var(--graph-meeting) }` at 0.66 rem =
**1.99 : 1** in light mode.

### 5.2 The closed set of body patterns

**A new tile kind may not invent a seventh.** This rule is what stops the board looking like ten
bespoke widgets, and it is binding on Phase 7.

| Pattern | Height band | Used by |
|---|---|---|
| **P1** stat cluster — eyebrow / 28-px value / delta | 62–96 px | person, stayed-on-device |
| **P1b** stat grid — 2-up wells, `tabular-nums` | 96–140 px | numbers *(retired, arm alive)* |
| **P2** ledger rows — chip rows, max 5 then `+N more` | 84–200 px | promises, reminders |
| **P3** lane — vertical rail, two-line steps | 76–200 px | drift *(gated)* |
| **P4** micro-chart + stat | 74–110 px | cadence, pulse *(retired, arm alive)* |
| **P5** prose — gradient mask, **not** `-webkit-line-clamp` | 84–170 px | note, document, living answer, board note, quote |
| **P6** proportional strip — segmented bar + chips | 56–88 px | meeting / talk split |

Type scale declared once on `:host`: `--t-eyebrow: .625rem`, `--t-title: .875rem`,
`--t-body: .8125rem`, `--t-meta: .6875rem` (**line-height 1.35, not the global 1.5** — at 11 px,
1.5 separates a two-line meta into two unrelated elements), `--t-stat: 1.75rem`. Top-to-bottom
ratio 2.8× (shipped is 1.8×, below the ~2.4× at which a card acquires a focal point). Note the
shipped hierarchy is *inverted*: `.number-value` is 15 px while `.stat dd` is 18 px, so the
Numbers tile has the smallest figures on the board. Big standalone figures get **proportional**
digits; `tabular-nums` only where digits align in a column.

### 5.3 Micro-charts — two rules that make every SVG survive Angular

1. **Never `<defs>` + `id` in a repeated component.** Emulated encapsulation scopes attributes,
   not ids — twelve tiles emit twelve `id="sparkFill"` and tile 7 silently paints tile 1's hue.
   Use `fill: currentColor; opacity: .14` or a CSS `mask-image`.
2. **`preserveAspectRatio="none"` + `vector-effect="non-scaling-stroke"`**, and **never a
   `<circle>` inside a stretched viewBox** — an end-of-series dot must be a CSS element
   positioned from a computed signal.

**Mark chooser**, deterministic from `density = nonzero/n` and `peak`:
`n === 0` → not a chart, empty ladder · `n === 1` → no chart, one stat + qualifier ·
`n ≤ 4` or `density < .6` → dot-strip · small ints, `n ≤ 20` → **heat strip (the default)** ·
`density ≥ .6 && peak ≥ 4` → line + 14 % area · `peak < 4` → line only · **never bars**.

Today's Pulse is the counter-example: `barHeight = v / max(...weekly) * 100` with
`min-height: 3px` means **a zero week draws a visible mark for data that does not exist**, and a
peak of 1 renders at 100 %, so 40 mentions/week looks identical to 1.

- **Heat strip:** `display:flex; gap:2px`, cells `aspect-ratio:1; max-width:14px`, quantised on a
  **fixed** ladder `v===0?0:v<=1?1:v<=3?2:v<=6?3:4` → opacity `[0,.28,.48,.72,1]`, level 0 painted
  `--surface-input` + `--border-subtle`. **Fixed, never normalised** — that is what makes two
  boards comparable, and exactly the property `barHeight` destroys.
- **Dot-strip:** flex columns, 1-px stem at 26 % opacity, 5-px dot; **a zero is a 3-px dim dot on
  the baseline** — present, unambiguously zero. A bar with `min-height` cannot express this.
- **Sparkline:** viewBox `0 0 100 32`, plot band y ∈ [4,30], `yMax = max(peak, 4)`, stroke 1.75,
  area `opacity .14`, end dot as a CSS `<i>` with `box-shadow: 0 0 0 2px var(--surface-solid)`.
  Draw-on via `pathLength="1"` + dashoffset 1→0, 700 ms, once — no JS path measurement.
- **Talk-split bar:** 8-px track, two segments with a 2-px `--surface-solid` gap, `Me` in
  `--tile-hue`, `Others` at 32 %.
- **No fake waveform, ever.** The prototype's `Math.abs(Math.sin(i*.9))` is decorative fake data
  on a tile claiming to represent a real recording — a credibility hole in an app whose entire
  pitch is provenance. **Consequence, stated out loud: the shipped board will be slightly quieter
  than that prototype.** Close the gap with real signals (talk split, chapter strip, heat strip),
  never by lowering the bar.

### 5.4 The sealed tile

`TileData::Locked` carries no fields; `redact_tile_chrome` nulls `title`/`config`;
`DashboardTileComponent.heading()` re-asserts it client-side. **Nothing may add a field.**

**Hazard to design around:** `redact_tile_chrome` treats `Drift`/`Numbers`/`Pulse` with
`entity == ENTITY_HIDDEN` as withheld and strips chrome, but the payload still ships `weekly`,
`total` and `quiet_days`. Any new footer or freshness mark **must branch on the same withheld
predicate**, not on "does the payload carry a timestamp" — a heat strip or a `quiet 9d` chip for
a hidden entity leaks timing about a sealed entity.

### 5.5 Layout

`--cols: 12 → 8 → 4` in media queries, **clamped in TypeScript**, not
`grid-column: span min(var(--tile-span), var(--cols))` — a math function in a `span <integer>`
context has no precedent in this repo, and if WebKit rejects it **the whole declaration drops**
and every tile falls back to span 1, collapsing the board into a 12-column sliver.
(`color-mix()` by contrast is already shipped in six files including
`design-system/primitives.css` and `board-card.component.scss`, and is safe.)
Tall tiles clamp with a mask + `+N more`, never a nested scrollbar.

### 5.6 The cited state — dim the field, and not before Phase 5

Brightening one card among eleven is a weak signal.
`.canvas.is-citing app-dashboard-tile:not(.cited) { opacity:.45; filter:saturate(.55) }`; the
cited tile gets the `--accent` ring, a glow, and a **numeral badge** (the accessible channel —
colour alone can never carry citation, and the user's accent may sit within ΔE 15 of a family
hue); `scroll-margin-top: var(--space-6)` + `scrollIntoView({block:"nearest"})`; held
`--motion-cite-dur`, then released to a **persistent 2-px accent bar on the tile's inline-start
edge** until the next question. That persistent bar is the visible proof that the board *is* the
retrieval scope.

**Three states, not two:** `cited(n)` · *in scope, not used* · *out of scope* (sealed, or a
derived tile that contributes no source — permanently desaturated, footer reads `not a source`).

**Blocking dependency.** Do **not** ship dim-the-field before the Phase-5 manifest. Today
`build_vault_context_pinned_visible`'s note/document arm pushes `Vec::new()` and `tilesForSources`
matches `s.meetingId` against `t.refId` — on a board of three notes and one meeting, dimming
would dim the three notes and spotlight the meeting, asserting a falsehood about what the answer
stood on. **Un-cited is honest; wrongly-cited is worse than no citation.**

### 5.7 Motion

Live dot `opacity .55→1` over 3.2 s (shipped is `1→.3` over 2.4 s — **amplitude**, not duration,
is what makes a loop noisy). **Liveness is a property of the data, not the kind** —
`LIVE_KINDS` currently blinks a three-month-stale Promises tile identically to one that changed
an hour ago. Budget **≤3 animated dots per board**, awarded to the most recently changed, only
while the route is focused. Tile entry: `translateY(10px)` + fade, 420 ms `--ease-spring`, stagger
`min(i,8) × 45ms`, registered in `afterNextRender(fn, {injector})` on **first paint only** —
otherwise every refresh replays the fade, and `reloadOpenBoard()` runs on every span change and
every add. Value change = a 600 ms colour wash, **no movement** (movement = arrival, colour =
change). Never animate `backdrop-filter` (WKWebView repaints the layer); never raise `background`
opacity on hover on a blurred surface.

**The reduced-motion trap:** `opacity:0; animation: … forwards` plus
`@media (prefers-reduced-motion) { animation: none }` leaves every tile **permanently invisible**.
Always restore the end state explicitly (`opacity:1; transform:none; stroke-dashoffset:0`).

### 5.8 Loading

Skeleton on **first fetch only**. On refetch hold the previous render at
`opacity:.55; pointer-events:none` — the stale-while-revalidate discipline `angular-zoneless.md`
§8 mandates for lists. A skeleton flash on every span change is the single most damaging cheap
tell.

---

## 6. What we are NOT building

Each was proposed by a research agent and each is refused for a stated reason. This section is
binding: re-proposing one of these requires new evidence, not a new argument.

| Refused | Reason |
|---|---|
| Deleting any `resolve_tile` arm | One unresolvable tile fails the whole board, and the user's board holds `numbers` and `pulse`. The alternative is a destructive `kind` rewrite |
| New tile kinds created to retire old ones (`source`, `commitments`, `facts` merges) | Nine touchpoints each, and `dashboard_cmd_tests` asserts arm/kind parity in **both** directions (`"tile kind is storable but has no resolver arm"` + `"no stale handled kind"`), so `TILE_KINDS` and the `resolve_tile` arms must move together. The old kinds cannot be removed anyway. Editing `NODE_TYPES` gets the same user-visible benefit for free |
| A vault-wide open-facts tile (`list_open_facts_visible`) | Not board-scoped. It puts content into a board the user did not compose and pushes it through `render_tile_for_agent` into an agent's view of a scope the user believes they bounded. Also unbounded — no `LIMIT`, correlated `EXISTS` per row |
| Preflight as speculative `resolve_tile` per candidate | ~230 resolves per palette open on the one `Mutex<Connection>` a live recording writes through. Use `GROUP BY` |
| A `UNIQUE INDEX` on `(dashboard_id, kind, ref_id, config)` | The live DB already violates it; `Db::migrate()` runs at startup → failed migration → failed launch |
| `grid-column: span min(…)` | No precedent for a math function in a `span <integer>` context; a WebKit rejection drops the whole declaration |
| Per-kind default span at add time | Every existing tile already holds a concrete `4`; it changes nothing about the board complained about |
| Dim-the-field citations before Phase 5 | Note/document tiles cannot be cited today; dimming would assert a falsehood |
| A mandatory footer on every tile | Re-homogenises the heights this spec exists to break |
| Board-wide talk-time rollup / speaker roster | `speaker_voiceprints` = 0 rows and `others-N` are per-meeting cluster tags — summing adds different humans together |
| Live query as a source contributor | Membership changing without the user touching it is auto-RAG in a tile costume. Allowed only as a view contributing zero `SourceRef`s |
| `search_dashboard` as a post-filter after `limit` | Returns zero rows for a board whose sources rank below the vault-wide top-k while the answer is on the board |
| An "Open questions" aggregate **count** | ~70 % precision is fine for a *row* (three seconds to verify by playing the tape) and a lie for the *headline*, which is what the user reads and what feeds the prompt. Ship the ledger without the number, later |
| Regex "Numbers v2" over notes + transcript | Matches `2026-07-04`, `1 min`, `Bielik-11B`, `8765`. Converts an *empty* tile into a *noisy* one. Only viable unit-anchored |
| In-tile audio for Quote v1 | `convertFileSrc`/`asset:` is the one audio path bypassing `meeting_is_unlocked` — the leak already closed once by nulling `audio_path` |
| Auto re-running living answers on a schedule | Silent scheduled cloud egress. Make staleness visible; let the user press the button |
| Scope-blaming error copy | Would have been factually false on the board that produced the complaint |
| Vault-wide "never empty" wallpaper (links/orphans, vault timeline, going-quiet, mini brain-map, vault audit, ask history, org feed, decision log) | Their non-emptiness comes from *ignoring the board*. They make it look full while making it mean less |
| Scope readout as a tile | The user would have to know to add it. It is board chrome |
| `grid-template-rows: masonry` | Not dependable in the shipping WKWebView |
| Sample/demo data on an empty board | Every empty-state playbook recommends it and it is wrong *here*: the app's pitch is that everything on screen was actually said. A fake board is the same credibility hole as a fake waveform |
| Boards in the record view | Live transcript is the anchor; `acquire_external_egress_lease` risk during recording is real |
| A board embedded in a note (`murmur:board` managed block) | Deferred, not killed. This is precisely the shape that leaked titles and live connector data on share in PR #416 — `clean_note_body` must strip it and it must never persist resolved content into the `.md`. Needs its own `lock-security-reviewer` pass |

---

## 7. Out of scope — the extractor investigation (D2)

Deliberately **not** in this programme, and must not be entangled with it:

- `facts::EXTRACT_SYSTEM` never asks for quantities → Numbers has almost nothing to filter.
- `facts::reconcile_facts` requires identical normalised `(entity, subject, predicate)` while
  predicates are free-form → supersessions do not form → Drift cannot fire.
  `commands/facts.rs::build_supersession_rows` additionally skips same-meeting supersessions.
- `facts::extract_fact_candidates` returns `Vec::new()` when `reasoner.id() == "stub"`.
- `storage/graph_store.rs::Db::add_mention` is `INSERT OR IGNORE` on PK
  `(entity_id, meeting_id)` — it counts **meetings, not utterances**, so the palette copy "how
  often this is actually talked about" is false, and no entity can produce a non-degenerate
  12-week series. Written best-effort by `commands/mod.rs::build_and_persist_entities` at note
  time, with no backfill.

These are model/prompt-shaped problems needing a real vault and a real Mac to judge. They get
their own spec. **Until then Numbers and Pulse stay out of the palette and Drift stays gated** —
we do not advertise a capability that structurally cannot fire.

---

## 8. Verification per phase

Beyond the standing gates (`cargo test --lib`, `npx ng lint`, `npx ng build`,
`scripts/agent-config-audit --ci`, and `scripts/ci.sh` once at the end):

| Phase | Required proof |
|---|---|
| 1 | **RED-before-GREEN Playwright** failing on today's code (empty tile at span 3, no body; nine tiles not all equal height). **Packaged WKWebView check** per T4 — `color-mix`, `mask-image`, `backdrop-filter` are involved and `ng build` proves nothing about the shipped engine |
| 2 | RED-first Rust test: an owner head that is only a parenthetical yields `None`, **in both a Polish and an English `note_language`**. Palette-count query plans confirmed to use `idx_facts_entity` / `idx_entity_mentions_entity` |
| 3 | The four `get_dashboard_sources` oracles, RED-first. A profile showing one full-vault note scan per board render, not per tile |
| 4 | Agent-surface oracle extended to the brief. `errcode` test asserting `^\[[a-z-]+\] ` **and** that the message contains none of the inner error's bytes. **`lock-security-reviewer`** |
| 5 | `living_answer_withheld` stays a pure function and **fails closed** on legacy/empty rows — RED-first. Citation matching on `(kind, id)` proven for a note tile. **`lock-security-reviewer`, mandatory** |
| 6 | Scope-restriction pushed **into the legs**, proven by a test where a board source ranks below the vault-wide top-k. Embedder-absent branch explicit. **The `eval/` experiment: board-scoped Ask vs vault-wide Ask on a fixed real-vault question set.** **`lock-security-reviewer`** (protocol + egress) |
| 7 | Quote: a relocked session yields empty text, never stale text; no `asset:`/`convertFileSrc` path added. Talk split: explicit NULL-`speaker` branch. `render_tile_for_agent` arm + oracle for all four kinds. **`lock-security-reviewer`** |
| 8 | Reduced-motion end states restored explicitly (the permanently-invisible-tile trap) |

**Honest boundary, per `agentic-workflow.md`:** headless checks cannot prove Touch-ID unlock
behaviour around a live board, real screen-share auto-relock of a sealed tile, or WKWebView
rendering of the packaged build. Those need a signed build on the Mac and must be named as
unproven until they are run.

---

## Open items for the user

1. **Phase ordering vs the extractor.** This spec ships the picture first and leaves the
   extractor to its own investigation (D2). If the real vault turns out to be much richer than
   the dev DB, Numbers/Pulse/Drift may deserve to stay in the palette — that check is one query
   against the production DB and it is worth running before Phase 2 retires them.
2. **Whole-board vs Most-relevant default** (Phase 6) is deliberately left to the `eval/`
   experiment rather than taste. If the experiment is not run, the default stands at Whole-board
   under ~12 sources and should be labelled provisional.
