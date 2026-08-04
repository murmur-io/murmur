# Dashboards — UX/UI research and the tile-catalogue verdict

*Research brief · 2026-08-04 · intended path `docs/research/2026-08-04-dashboards-ux-research.md`*

Every load-bearing claim below is cited `file:symbol`. Claims from the grounding investigations that I re-checked against the tree and found **wrong** are marked ⚠ and corrected in place.

---

## The one-paragraph answer

**The screenshot is a rendering bug, not a design failure, and it has a four-edit fix that works on the board the user already has.** Five of the nine tiles on that board render an apology inside a full-height card; all nine carry `span = 4` in SQLite because `commands/dashboards.rs:477` is `span.unwrap_or(4)` and `dashboard-view.component.ts::addTile` never passes a span even though `dashboards.service.ts::addTile` already accepts one — 12/4 = exactly three per row, forever. Collapse empty tiles to a 36-px dashed strip, override the display span per kind, render duplicates as a "same as above" strip, and add the missing `--text-tertiary` tier plus a 22-px per-kind icon mark, and the existing board looks composed. That is one session, pure frontend, no migration, and it is the entire visible delta the user complained about. Everything else in this brief is a second, deeper problem that the pretty layer does not touch: **"Ask this board" does not use the board as a retrieval scope — it uses it as a retrieval *bypass*.** `commands/mod.rs::ask_vault` routes any non-empty `explicit_sources` to the deterministic floor (no agent loop, no tools), `summarize/vault_context.rs::fair_pack_explicit_sections` then gives every source `budget / n` characters and hard-truncates, and `commands/dashboards.rs::get_dashboard_sources` drops seven of ten tile kinds with `_ => continue`. So the board's answers get *worse* the more the user composes into it, and the six only-Murmur tiles are invisible to the one feature they exist to feed. Fix the picture first because that is what was reported; fix the scope second because that is what makes the feature worth having; and do not add tile kinds to a board that cannot yet read the tiles it has.

---

## 1. Why the shipped board looks poor — root causes

### 1a. The data is genuinely thin (measured on the user's own vault)

Measured against `~/Library/Application Support/MeetNotes-dev/meetnotes.sqlite` — 78 meetings, Σ 1.38 h, **avg 63 s per meeting**. Absolute counts are small; the structural ratios are size-independent.

| Symptom | Root cause (symbol) | Measured |
|---|---|---|
| "No figures recorded for this yet." | `facts::EXTRACT_SYSTEM` asks for *"durable state worth tracking"* (status/owner/deadline/role) and **never asks for quantities**; `commands/dashboards.rs::looks_numeric` is a post-filter accepting a leading ASCII digit | **6 of 48 facts (12.5 %) pass**, and **2 of the 6 are dates** (`deadline = 2026-07-04`). They belong to 3 of 57 entities → **P(non-empty \| random entity) = 5.3 %**. The user's anchor ("Brain") has 4 facts, all prose |
| Drift lane with one step | `list_facts_visible` → `max_by_key(predicate count)`; the winner had count 1 | **1 of 19 fact-bearing entities** has ≥2 facts under one predicate → **1.8 %**. Three independent brakes: `facts::extract_fact_candidates` returns `Vec::new()` when `reasoner.id() == "stub"`; `facts::reconcile_facts` requires the *same normalized* `(entity, subject, predicate)` while the vault has **43 distinct predicates across 48 facts**; `commands/facts.rs::build_supersession_rows` skips same-meeting supersessions. `supersessions` = **0 rows** |
| Flat pulse | `storage/graph_store.rs::Db::add_mention` is `INSERT OR IGNORE` on PK `(entity_id, meeting_id)` — **it counts MEETINGS, not utterances**, so the palette copy ("how often this is actually talked about") is false | 45 entities @1 mention, 5 @2, **0 @≥3**. `entity_mentions` covers **18 of 78 meetings**, written best-effort by `commands/mod.rs::build_and_persist_entities` via a cloud `complete_json` call at note time, with no backfill. **No entity in the vault can produce a non-degenerate 12-week series** |
| Two identical Promises tiles | `tile-palette.component.ts` declares `promises` as `mode: "none"` → `pick()` emits `{kind}` with no config → `cfg.owner = None` on both → the same global list. `resolve_tile`'s `"promises"` arm **has supported `cfg.owner` since day one**; nothing in the UI ever writes it, and `TileData::Promises.owner` is never rendered | Structurally guaranteed, not a user error |
| Promise meta reads as noise; Person headline is 0 | `summarize::action_items::extract_owner` takes the head before ` — ` verbatim | **29 of 29** open items carry the literal owner `"(właściciel nieokreślony)"`. Cascade: `resolve_tile`'s `"person"` arm matches `normalize_owner` against the person's name → **`TileData::Person.open_commitments` is structurally 0 for every person in the vault** |
| "This note has no text yet." ×3 | `list_notes_visible` offers every note; picker has no non-empty filter | **20 of 56** `kind='note'` documents have `text` length 0. The three pinned ones are 0 / 18 / 86 chars, with meaningful-looking titles |

**The one-line diagnosis:** three of the four "only-Murmur" tile kinds are anchored to a single entity id over the two thinnest tables in the schema (`facts` 48 rows, `entity_mentions` 55 rows), and the palette offers all 57 entities with no preflight.

### 1b. The visual system is flat (specific devices the prototype had, the ship dropped)

| Device in `docs/dreams/prototypes/dashboards/index.html` | Shipped |
|---|---|
| 22-px rounded icon chip per kind, coloured (`:207-209`, `:650`) | **nothing** — `dashboard-tile.component.html:5-33` is `<h3>` + `<span class="tile-kind">`. Zero colour in any tile header. This single omission is most of "nie wygląda bogato": the home-screen *thumbnail* (`board-card`) is richer than the board |
| Variable tile sizes | every tile born `span 4` (`dashboards.rs:477` + `addTile` never passing one) → a literal 3-column wall. Plus `min-height: 74px` on the body → uniform heights |
| Smooth `<polyline>` + area fill (`:537-545`) | 12 bars, `barHeight = v / max(...weekly) * 100` with `.pulse-bar { min-height: 3px }` — **a zero week draws a visible mark for data that does not exist**, and a peak of 1 renders at 100 %, so a board with 40 mentions/wk looks identical to one with 1 |
| Drift rows carrying `Sarah Chen · Apr 28 · "auth is heavier than we thought"` | `meta: Some(short_day(&f.valid_from))` — a bare date, while `f.meeting_id` sits right there |
| Per-board tint + emoji + subtitle + freshness line + sealed marker (`:603-621`) | `emoji`/`tint` are **read** in three places and **written nowhere** (`dashboards-home.component.ts:95` calls `create(title)` only; the one `update` call passes `{pinned}`). No rename. `TINTS` in `dashboards.rs` and six scss mappings are dead code → every board card is a flat grey wash |
| Inline `[1][2][3]` citations that scroll to the tile (`:546-550`, `:732-740`) | `{{ turn.text }}` as plain text with `white-space: pre-wrap`. The answer is markdown the prompt *demands* (`summarize/vault_chat.rs::build`), so `**` and `[[…]]` render literally. `src/app/shared/markdown/markdown.component.ts` exists and already turns `[[Wikilinks]]` into gated chips — **dashboards is the only AI surface in the app not using it** |
| tile entrance stagger, 45 ms | none |

Plus, measured: `--text-muted` is **3.62 : 1** on the tile surface (dark) and **3.46 : 1** (light), and it is used at 10–11 px for `.tile-kind`, `.row-meta`, `.drift-meta`, `.number-key`, `.live-chip` — the entire secondary layer is below the legibility floor. The 6-family hue map is a measured accessibility failure (`--accent` ↔ `--graph-document` ΔE 14.2 normal / **3.6 protan**; `--warning` ↔ `--graph-meeting` ΔE 5.7), and `--accent` is user-selectable with **5 of 6 options colliding** with a family hue. Nine raw `rgba()` values render white-on-near-white in light mode. Eleven empty states all render as the same `<p class="muted">` inside a full-height box. The Ask panel holds `flex: 0 0 330px` permanently, usually showing three suggestion buttons.

### 1c. The Ask error — diagnosed, and one grounding hypothesis rejected

`summarizer error: cloud provider response failed after protected dispatch; details omitted` is produced by `summarize/redact.rs::content_free_dispatch_error`, the `AppError::Summarize(_)` arm, after `RedactingProvider::complete` sees the inner provider fail. It collapses into one sentence: the 180 s `CLAUDE_TIMEOUT`, spawn failure, empty stdout, non-UTF8 output, **and** `claude_code.rs::claude_failure_message` — which is *already* a content-free, actionable diagnostic naming the likely bad model id and the exact Settings path to fix it.

⚠ **The "board too big / 200 k chars / timeout" hypothesis is wrong for this board.** `get_dashboard_sources` skips every derived tile (`dashboards.rs:679` `_ => continue`), so the user's 9-tile board produced **at most 4 sources — three of them notes with 0 / 18 / 86 characters of text**. The corpus was nearly empty. This was a plain `claude_code` dispatch failure. Do **not** ship scope-blaming copy ("this board is too big, 47 sources") until `PackedContext` exists and `refs.len()` is real — it would have been factually false on the board that produced the complaint.

Two aggravations on the frontend: `dashboard-view.component.ts::errorText` and `dashboards.service.ts::message` are hand-rolled raw-string renderers (the app deleted nine of these to establish `core/copy/error-copy.service.ts` as the boundary; dashboards reintroduced two), and the error is pushed as `{role: "assistant"}` so it renders in a normal grey bubble, visually identical to a real answer, with no retry.

---

## 2. Verdict on the 10 shipped tiles

**Hard constraint that shapes every verdict — tile kinds are append-only forever.** `resolve_tile`'s terminal arm is `other => Err(AppError::InvalidArg(format!("unknown tile kind: {other}")))` (`dashboards.rs:1010`) and `get_dashboard` collects with `.collect::<Result<Vec<_>, AppError>>()?`. **One unresolvable tile fails the entire board.** The user's board contains `numbers` and `pulse` rows, so deleting those arms turns their board into a hard error at open. Migrating `dashboard_tiles.kind` would be a destructive rewrite of user rows — forbidden. Therefore **"KILL" always means: remove the entry from `NODE_TYPES` in `tile-palette.component.ts` so no new one can be placed, and leave the `resolve_tile` arm alive rendering a proper degenerate/empty state.**

| # | Tile | Verdict | Evidence |
|---|---|---|---|
| 1 | **Note** | **KEEP** — palette-level merge with Document | One of only 3 kinds that feed Ask. Picker must filter the 20 empty-text notes |
| 2 | **Meeting** | **KEEP — REDESIGN (additive payload)** | Ships `{started_at, duration_s, has_audio}` and nothing else while `segments.speaker` (714 rows, 100 % attributed) and `timelines` chapters sit one gated read away. Every tile reads "1 min" on this vault |
| 3 | **Document** | **KEEP** — same palette entry as Note | 4 rows; identical job. Merge the *palette offer* ("Add a source"), not the storage kind — the candidate row already carries `LINK_KIND` |
| 4 | **Person** | **KEEP — REFEED** | Headline `open_commitments` is structurally 0 vault-wide (see §1a). Un-zeroed by the `normalize_owner` fix alone |
| 5 | **Drift lane** | **RETIRE FROM PALETTE unless preflight says ≥2 steps** | 1.8 % hit rate; `supersessions` = 0. Keep the arm; `rows.len() < 2` must render one line ("unchanged since 3 Jun"), never a rail — a rail asserts movement |
| 6 | **Numbers** | **RETIRE FROM PALETTE** | 5.3 % hit rate, 2 of 6 hits are dates. `looks_numeric` is a post-filter over a substrate that was never asked to contain numbers |
| 7 | **Pulse** | **RETIRE FROM PALETTE** | 0 % non-degenerate. Its own copy is false (meetings, not utterances). Its bar mark actively lies twice. `dashboard-tile.component.html:104` has **no empty branch at all** |
| 8 | **Promises** | **KEEP — THE FLAGSHIP; needs a subject** | 29 real open items. Add `config.owner` (backend already reads it) with an explicit "Everyone" option, and render the owner in the header. This is the dream's #2 and the tile that most justifies the feature |
| 9 | **Reminders** | **KEEP AS-IS, low priority** | 1 row. `let due_count = rows.len()` counts *all* active rows **before** the `TILE_ROWS` truncation — both halves of the name are wrong — and the FE never renders it. Delete the field or fix it |
| 10 | **Living answer** | **KEEP — PROMOTE TO HERO** | Its gate (`living_answer_withheld`) is the most carefully built thing in the feature. Its failures are upstream (dispatch) and presentational (`{{ la.answer }}` renders markdown raw) |

**Rejected merges.** `tile-verdict` proposed collapsing 10 kinds → 6 (`source`, `commitments`, `facts`…). Creating new kinds to retire old ones costs nine touchpoints each (`TILE_KINDS`, a `resolve_tile` arm, a `TileData` variant, `models.ts`, template branch, empty branch, palette entry, span default, icon — with arm/kind parity enforced at `commands/tests/dashboard_cmd_tests.rs:439`) and **cannot remove the old kinds anyway** (append-only). The user-visible benefit — a shorter palette — is fully obtainable by editing `NODE_TYPES`. **Do the palette merge; do not do the storage merge.**

**Rejected: the vault-wide open-facts replacement.** `tile-verdict`'s keystone repair ("swap `list_facts_visible(entity)` for `list_open_facts_visible(unlocked)` → 47 rows instead of 1") is a thesis violation. `storage/facts_store.rs::list_open_facts_visible` takes **only the unlock set** — no board, no source filter, no `LIMIT`. It achieves 47 rows precisely by rendering facts from meetings that are not on the board, and since `render_tile_for_agent` feeds MCP, it would leak out-of-scope material into an agent's view of a scope the user believes they bounded. Board-scoped correctly (facts whose `meeting_id ∈ get_dashboard_sources`), the user's 4-source board yields ~0–4 rows and the headline arithmetic evaporates. **Cut.**

### Palette IA fix

**Invert the two steps: subject first, kind second.** Today the user commits to a promise ("Drift lane") before discovering the subject can't keep it. The question *"nie wiem ile one mają sensu"* is literally "will this work for me", and it is unanswerable in the current order.

**Every offer carries a live count, and counts come from aggregate SQL — never from speculative resolution.** Both synthesis proposals specified preflight as *"build a synthetic `DashboardTile`, call `resolve_tile`, keep it if it has rows"*. That is ~230 speculative resolves per palette open on this vault, serialized on the one `Mutex<Connection>` a live recording writes segments through — and `resolve_tile`'s `"note"` arm does a **full vault note-list read to resolve one note**, its `"person"` arm a full entity-list read plus `list_open_commitments` (which walks `list_meetings_visible(1000)` → `get_note_if_visible` per meeting). Replace with one `GROUP BY` per kind: `SELECT entity_id, COUNT(*) FROM facts WHERE valid_to IS NULL GROUP BY entity_id` (covered by `idx_facts_entity`), same over `entity_mentions` (`idx_entity_mentions_entity`), and a `length(text) > 0` filter on the note candidates.

Be honest about what preflight buys: **it validates at add time, and tiles are persistent.** Seal a folder or delete a note and a preflight-validated board is the screenshot again, now with the added harm that the user believes it was checked. Preflight saves wasted composition effort; **render-time collapse (§4) is what keeps a board looking good over time.** Ship the render fix first.

Also in this file: `registerField` (`tile-palette.component.ts:295`) is dead code — bound nowhere, so the search field is never focused; and the panel declares `role="dialog" aria-modal="true"` with no focus move and no trap.

### The duplicate-tile decision

It is a missing *parameter*, not a missing *constraint*. Fix in this order:

1. **Owner scope on `promises`** (`mode: "entity"` + explicit "Everyone"), rendered in the header. Two ledgers scoped to two people is a legitimate board; a blanket ban is wrong.
2. **Render-time duplicate strip, pure FE.** If two resolved tiles share `(kind, refId, config)`, render the second as *"same as the tile above · Remove"*, and have the palette scroll-and-highlight the existing tile instead of adding. This works on the board in the screenshot **today**, with zero backend, and it does not add a new error dialog to a feature whose complaint is that it surfaces raw errors as content. (An `AppError::InvalidArg` guard would be swallowed by `DashboardsService.addTile` into the hand-rolled `message(e)` renderer we are trying to delete.)
3. If a backend guard is later wanted, it belongs in `commands/dashboards.rs::add_dashboard_tile` over `(dashboard_id, kind, ref_id, canonical_hash(config))`. **Never a `UNIQUE INDEX`** — `Db::migrate()` runs on real user DBs and the user's DB already contains the violating pair; the index would fail migration and therefore startup.

---

## 3. The tile catalogue we should have

Ranked. Everything the critiques killed is in §8, not here.

| Rank | Tile | Data source (symbol) | Cost | Only-Murmur | Visual form | Notes |
|---|---|---|---|---|---|---|
| 1 | **Board note / heading** | `dashboard_tiles.config.text` | **XS** | ✗ | `app-markdown` | The board cannot currently be *written*, only assembled. Datadog ships a Note widget for exactly this. Per §6 the user's own sentence is the highest-value token in the board's prompt |
| 2 | **Talk split (per meeting)** | `segments.speaker/start_s/end_s`, `SUM(end_s-start_s) GROUP BY speaker` under the meeting gate | **S** (additive `me_ratio`, `segment_count` on `TileData::Meeting`) | ★★★ | 8-px stacked bar, 2-px surface gap, `Me 38% · Others 62%` | The richest untouched signal in the DB; Obsidian structurally cannot have it. **Two corrections:** (a) `speaker` is `add_column_if_missing(… "TEXT")` → nullable; pre-dual-stream recordings are NULL and need an explicit branch — "100 % attributed" holds only for post-2026-07 recordings; (b) **no board-wide rollup** — diarization labels are per-meeting cluster tags (`others-1`), `speaker_voiceprints` = 0 rows, so summing `others-*` across meetings adds different humans together and prints it as a roster. That is `looks_numeric` in a new coat |
| 3 | **Quote** | `db.get_segments` behind `meeting_is_visible`; config stores **only** `{meetingId, startS, endS}` | **M** (needs a "Pin to board" flow in the transcript view) | ★★★ | Pull-quote + timestamp + "open at 02:14" | The only tile structurally incapable of being empty — its content is chosen at add time. Gate path verified sound: `get_segments` is explicitly ungated ("callers must first pass the meeting visibility boundary"), and seal blanks `segments.text` to `''`, so a relocked session yields empty text, never stale text. **v1 must NOT play audio in-tile** — `lock-model.md` documents `convertFileSrc`/`asset:` as the one audio path that bypasses `meeting_is_unlocked`, which is why the masked DTO nulls `audio_path`. v1 deep-links to the meeting detail at `start_s`, reusing the existing gated receipt-seek path |
| 4 | **Stayed on device** | `egress_log` · `egress_store::egress_summary(days)` — **content-free by construction** | **S** | ★★★ | One stat + delta | 279 rows. Never empty, and its zero state ("0 cloud calls · 100 % local") is its best state. No cloud notetaker can render it |
| 5 | **Board cadence** | `meetings.started_at` restricted to the board's meeting sources | **S** | ★ | Heat strip, **fixed** ladder (0 / 1 / ≤3 / ≤6 / 7+) | Board-scoped only. Vault-wide cadence is wallpaper. The fixed ladder is what makes two boards comparable — the exact property `barHeight`'s normalisation destroys |
| 6 | **Board-scoped search view** | `search_hybrid_visible` **with the id set pushed into the legs**, contributing **zero** `SourceRef` | **L** | ★★ | Ranked rows + score bar | Gated on the retrieval work in §6. Two blockers the proposal missed: it needs a query embedding from `Embedder::embed_query` (dead without a downloaded model — this is exactly the stub-reasoner failure class the proposal invented its own test to catch), and a **post-filter after `limit`** returns zero rows for a board whose sources rank 41st–60th while the answer is demonstrably on the board. It must be a *view*, never a source-contributor, or the board is no longer hand-composed |

**Board chrome, not tiles.** The scope readout (`11 tiles · 4 notes · 3 recordings · 2 docs · 2 derived views · 1 sealed`), the retrieval-coverage line (`9 of 11 indexed for recall`), the vault path, and the `murmur://dashboard/{id}` handle all belong in the board header (§4). Making the scope readout a *tile* means the user must know to add it.

---

## 4. The visual system

### 4.1 New tokens (the only additions)

```css
/* src/design-tokens/colors.css :root */
--text-tertiary: #8a8a9c;              /* 5.50:1 on the tile surface — the ≤12px tier */
--hatch-stripe: rgba(255,255,255,.05); /* the sealed-tile hatch, currently a raw rgba */
/* src/design-tokens/theme-light.css — BOTH the [data-theme] and prefers-color-scheme blocks */
--text-tertiary: #6e6e78;              /* 4.84:1 */
--hatch-stripe: rgba(18,18,40,.055);
/* src/design-tokens/layout.css — duration-only, per the existing convention */
--motion-enter-dur: 420ms; --motion-stagger: 45ms;
--motion-cite-dur: 2600ms; --motion-breath-dur: 3200ms;
```

Migrate `.tile-kind`, `.row-meta`, `.drift-meta`, `.number-key`, `.live-chip`, `.muted-small` from `--text-muted` → `--text-tertiary`; keep `--text-muted` for ≥13 px only. Delete the nine raw `rgba()` (`dashboard-tile.component.scss:186,375,466`; `dashboard-view.component.scss:152,186`; `board-card.component.scss:67,118,208`; `tile-palette.component.scss:132`). **This single change does more for "rich" than any chart.**

⚠ **Correction to one critique:** `color-mix(in srgb, …)` is *not* unprecedented in this repo — it ships in six files today including `design-system/primitives.css` and `features/dashboards/board-card/board-card.component.scss`. It is safe to use. What is genuinely unprecedented and **must not ship** is `grid-column: span min(var(--tile-span), var(--cols))`: a math function in a `span <integer>` context has no precedent here, and if WebKit rejects it the **entire declaration drops** and every tile falls back to span 1 — the board collapses into a 12-column sliver. Clamp in TypeScript (the span is already a signal).

### 4.2 Tile anatomy

```
[22px mark]  EYEBROW (10px/600/+.08em/uppercase, --text-tertiary)   [grip|tools] or [freshness]
             TITLE   (14px/640/-.01em, --text-primary)
─ body (one of six patterns, per-pattern height band) ─
─ footer (conditional): provenance ····················· state ─
```

- **Eyebrow above title.** The eyebrow is the classifier and must be scannable down the left edge; title-first makes every tile open with a different-length proper noun.
- **The mark** is a 22 × 22 squircle (`border-radius: 7px` — a circle reads as an avatar), `background: color-mix(in srgb, var(--tile-hue) 18%, transparent)`, 1 px rim at 34 %, glyph `stroke: var(--tile-hue)` at 1.9. **Not** the prototype's solid-fill chip with a near-black glyph — eleven saturated solids is the confetti failure at ⅕ the ink budget.
- **Material vs derived is carried by fill, not by a fifth hue:** material = tinted fill, derived = outline.
- **The footer is conditional.** A mandatory 28-px bordered footer on every tile re-homogenises heights, working against the same diagnosis it sits next to. Render it only when the tile has provenance worth printing.
- **Divider budget: one full-bleed rule per tile**, and it is the footer. Delete `.row { border-bottom }` — four promise rows currently draw three full-bleed rules inside ~120 px, which *is* the spreadsheet read. Rows become chips on `--surface-input` with `gap: var(--space-1)`.
- Delete `min-height: 74px`. Twelve identical-height boxes come from that floor plus the uniform span.

**Hue map — four hues, capped.** Reordered mint → azure → amber → orchid so orchid and azure are never adjacent, `{--graph-note, --graph-entity, --graph-meeting, --graph-document}` measures PASS on chroma, CVD (ΔE 17.4), normal-vision (19.2) and contrast in both themes. Adding a fifth (`--warning`) is a measured failure (ΔE 5.7 vs amber). Derived tiles (`reminders`, and the retired kinds) get **no hue** — `--text-secondary` marks. **`--accent` is reserved for the AI channel only** (citation ring + numeral, the `living_answer` mark, the Ask composer): it is user-selectable and 5 of 6 options collide with a family hue (orange vs `--graph-meeting` = ΔE 2.4, indistinguishable). Today `dashboard-tile.component.scss:300,318,339` paints pulse bars and the drift rail `--accent` — that is the accent doing category work; both become `var(--tile-hue)`.

**Text never wears the data hue** — a family hue at 10 px on the light tile surface measures 3.48–5.04 : 1. Current violation: `.audio-chip { color: var(--graph-meeting) }` at 0.66 rem = **1.99 : 1** in light mode.

### 4.3 The closed set of body patterns

A new tile kind may not invent a seventh. That rule is what stops the board looking like ten bespoke widgets.

| Pattern | Height band | Used by |
|---|---|---|
| **P1 stat cluster** — eyebrow / 28 px value / delta | 62–96 px | person, egress |
| **P1b stat grid** — 2-up wells, `tabular-nums` | 96–140 px | numbers |
| **P2 ledger rows** — chip rows, max 5 then `+N more` | 84–200 px | promises, reminders |
| **P3 lane** — vertical rail with two-line steps | 76–200 px | drift |
| **P4 micro-chart + stat** — 28–36 px chart band + one stat line | 74–110 px | cadence, pulse |
| **P5 prose** — gradient mask, not `-webkit-line-clamp` | 84–170 px | note, document, living answer |
| **P6 proportional strip** — segmented bar + chips | 56–88 px | meeting (talk split, chapters) |

Type scale declared once on `:host`: `--t-eyebrow: .625rem`, `--t-title: .875rem`, `--t-body: .8125rem`, `--t-meta: .6875rem` (**line-height 1.35, not the global 1.5** — at 11 px, 1.5 separates a two-line meta into two unrelated elements), `--t-stat: 1.75rem`. Top-to-bottom ratio 2.8× (shipped is 1.8×, below the ~2.4× at which a card acquires a focal point). Big standalone figures get **proportional** digits; `tabular-nums` only where digits align in a column. Note the shipped hierarchy is inverted — `.number-value` is 15 px while `.stat dd` is 18 px, so the *Numbers* tile has the smallest figures on the board.

### 4.4 Micro-chart specs

Two rules make every SVG below survive Angular:
1. **Never `<defs>` + `id` in a repeated component.** Emulated encapsulation scopes attributes, not ids — twelve tiles emit twelve `id="sparkFill"` and tile 7 silently paints tile 1's hue. Use `fill: currentColor; opacity: .14` or a CSS `mask-image`.
2. **`preserveAspectRatio="none"` + `vector-effect="non-scaling-stroke"`**, and **never a `<circle>` inside a stretched viewBox** — an end-of-series dot must be a CSS element positioned from a computed signal.

**Mark chooser** (deterministic, from `density = nonzero/n` and `peak`): `n === 0` → not a chart, empty ladder · `n === 1` → no chart, one stat + qualifier · `n ≤ 4` or `density < .6` → dot-strip · small ints, `n ≤ 20` → **heat strip (the Pulse/Cadence default)** · `density ≥ .6 && peak ≥ 4` → line + 14 % area · `peak < 4` → line only · **never bars**.

- **Heat strip:** `display:flex; gap:2px`, cells `aspect-ratio:1; max-width:14px`, quantised on a **fixed** ladder `v===0?0:v<=1?1:v<=3?2:v<=6?3:4` → opacity `[0,.28,.48,.72,1]`, level 0 painted `--surface-input` + `--border-subtle`. Fixed, never normalised — that is what makes two boards comparable.
- **Dot-strip:** flex columns, 1-px stem at 26 % opacity, 5-px dot; **a zero is a 3-px dim dot on the baseline** — present (the week exists), unambiguously zero. This is the property `min-height: 3px` on a bar cannot express.
- **Sparkline:** viewBox `0 0 100 32`, plot band y∈[4,30], `yMax = max(peak, 4)`, stroke 1.75, area `opacity .14`, end dot as a CSS `<i>` with `box-shadow: 0 0 0 2px var(--surface-solid)`. Draw-on via `pathLength="1"` + dashoffset 1→0, 700 ms, once — no JS path measurement.
- **Talk-split bar:** 8 px track, two segments with a 2-px `--surface-solid` gap, `Me` in `--tile-hue`, `Others` at 32 %.
- **Do not ship a fake waveform.** The prototype's `Math.abs(Math.sin(i*.9))` is decorative fake data on a tile claiming to represent a real recording — a credibility hole in an app whose entire pitch is provenance.

### 4.5 Empty states — a five-class ladder

The screenshot's failure is precise: four full-size cards each holding one grey apology, eleven such strings shipped.

| Class | Treatment |
|---|---|
| **Never had data** | **Collapse to a 36-px dashed header strip**: `border-style: dashed; background: transparent; backdrop-filter: none`, body and footer `display:none`, reason at `--t-meta` right-aligned. Copy is a positive statement of where data lands ("Figures said out loud land here"), never an apology |
| **Genuinely zero = good news** | **Full tile, not grey.** A ✓ in `--success` + the sentence at `--text-secondary`. "Nothing open — every commitment on this board is closed" is a *result*; rendering a success as an absence is the most demoralising thing a board does |
| **Degenerate** (drift `rows < 2`, pulse `total === 0`, chart `n ≤ 1`) | Collapse the *mark*, keep the tile: "One value, never revised — nothing has drifted" |
| **Degenerate-but-populated** *(new class — no proposal named it)* | A 100 %/0 % talk bar on a solo recording, a one-cell cadence strip, a one-row ledger. These pass every emptiness check and still look broken. Same collapse treatment, keyed on a per-pattern variance predicate |
| **Missing / Unconfigured** | Strip + an inline action ("Remove tile" / "Choose one", reopening the palette pre-filtered) |

**Two layout rules the collapse depends on:** an empty strip must take **span 3** and **sort to the end of the grid**. `.canvas` is `grid-auto-rows: max-content` with `align-self: start`, so a 36-px strip left in composition order beside a 200-px tile leaves ~164 px of dead air — four scattered strips look *worse* than four grey boxes.

**One ghost preview per board, never more** (repeated illustrations cause fatigue): the first empty tile renders its pattern's skeleton at 10 % opacity plus one sentence and a *tertiary* action; every subsequent one is a strip.

**Loading vs refetch:** skeleton on first fetch only. On refetch hold the previous render at `opacity: .55; pointer-events: none` — same stale-while-revalidate discipline `angular-zoneless.md` §8 mandates for lists. `reloadOpenBoard()` runs on every span change and every add; a skeleton flash there is the single most damaging cheap tell.

### 4.6 The sealed tile — make it look deliberate, leak nothing

`TileData::Locked` carries **no fields**, `redact_tile_chrome` nulls `title`/`config`, and `DashboardTileComponent.heading()` re-asserts it client-side. **Nothing in this system may add a field to a sealed tile** — no count, no kind glyph beyond the shared lock, no chart skeleton whose shape reveals the kind.

Render it full-size (a redacted region is *content*): a 45° hatch using `--hatch-stripe`, a lock glyph, and copy that ties the lock model to the thesis — **"Sealed — not in scope."** / *"Unlock the folder to bring this back into the board's scope."* A board that is screen-shareable as-is is a demo moment no cloud notetaker can offer, and today it looks like a bug.

**One hazard to design around:** `redact_tile_chrome` treats `Drift`/`Numbers`/`Pulse` with `entity == ENTITY_HIDDEN` as withheld and strips chrome, but the payload still ships `weekly`, `total` and `quiet_days`. Any new footer or freshness mark must branch on the same withheld predicate, not on "does the payload carry a timestamp" — a heat strip or a `quiet 9d` chip for a hidden entity **leaks timing about a sealed entity**.

### 4.7 Layout

- **Display-span override, not an add-time default.** ⚠ `commands/dashboards.rs:477` clamps to a concrete value, so every existing tile has `span = 4` in SQLite and an add-time default changes nothing about the board in the screenshot. Do `displaySpan = tile.span === 4 ? DEFAULT_SPAN[kind] : tile.span`, with the first explicit resize writing a real value — ~6 lines, retroactive on every existing board. Bands: `person`/`pulse` 3 · `numbers`/`note`/`document`/`drift`/`reminders` 4 · `promises`/`meeting`/`living_answer` 6. `3+3+6`, `4+4+4`, `6+6` — the row rhythm changes by itself.
- **Responsive by column remap**, `--cols: 12 → 8 → 4` in media queries, **clamped in TS** (not `span min()`).
- **Tall tiles clamp with a mask + `+N more`**, never a nested scrollbar.
- **The Ask column collapses to a 44-px rail** when `turns().length === 0`, carrying the scope count vertically. The scope count *is* the thesis made visible and belongs on screen permanently; the empty transcript does not. Expands to 380 px on use.
- **Errors get a third turn role** rendering `.banner.is-danger` (`primitives.css:253`) with a Try-again button. Never a bubble.
- Arrange mode: move `draggable` off the host (whose body is full of `<button>`s — WebKit does not reliably bubble a drag begun on a nested button) onto a header grip; add the missing `(dragleave)`; add `Alt+←/→` reorder and `⌘←/→` resize; add a `:focus-visible` ring to the twelve background-less buttons that have none.
- Delete the duplicated `.arrange-hint` block inside the `max-width:1080px` query (`dashboard-view.component.scss:235-244`).

### 4.8 The cited state — dim the field, and do not ship it early

Brightening one card among eleven is a weak signal. `.canvas.is-citing app-dashboard-tile:not(.cited) { opacity:.45; filter:saturate(.55) }`, cited tile gets the `--accent` ring + glow + a **numeral badge** (the accessible channel — colour alone can never carry citation, and the user's accent may be within ΔE 15 of a family hue), `scroll-margin-top: var(--space-6)` + `scrollIntoView({block:"nearest"})`, held `--motion-cite-dur` then released to a **persistent 2-px accent bar on the tile's inline-start edge** until the next question. That persistent bar is the visible proof that the board *is* the retrieval scope.

**Three states, not two:** `cited(n)` / `in scope, not used` / `out of scope` (sealed, or a derived tile that contributes no source, permanently desaturated with a footer reading `not a source`).

**Blocking dependency — do not ship dim-the-field before §6's manifest.** `build_vault_context_pinned_visible`'s note/document arm pushes `Vec::new()`, and `tilesForSources` matches `s.meetingId` against `t.refId`. On a board of three notes and one meeting, dim-the-field would dim the three notes and spotlight the meeting — asserting a falsehood about which sources the answer stood on. **Un-cited is honest; wrongly-cited is worse than no citation.**

### 4.9 Motion

Live dot `opacity .55→1` over 3.2 s (shipped is `1→.3` over 2.4 s — **amplitude**, not duration, is what makes a loop noisy). Liveness is a property of the **data**, not the kind (`LIVE_KINDS` currently blinks a three-month-stale Promises tile identically to one that changed an hour ago); budget **≤3 animated dots per board**, awarded to the most recently changed, only while the route is focused. Tile entry: `translateY(10px)` + fade, 420 ms `--ease-spring`, stagger `min(i,8) × 45ms`, registered in `afterNextRender(fn, {injector})` on **first paint only** — otherwise every refresh replays the fade. Value change = a 600 ms colour wash, **no movement** (movement = arrival, colour = change). Never animate `backdrop-filter` (WKWebView repaints the layer) and never raise `background` opacity on hover (reads as a flash on a blurred surface).

**The reduced-motion trap:** the prototype's `opacity:0; animation: tin … forwards` + `@media { animation: none }` leaves every tile **permanently invisible**. Always restore the end state explicitly (`opacity:1; transform:none; stroke-dashoffset:0`) and keep ≤150 ms opacity fades so state changes stay perceivable.

---

## 5. Views beyond the grid

| View | Job | Why it beats the grid | Cost |
|---|---|---|---|
| **The board as an MCP handle** (`murmur://dashboard/{id}` on the header, `tile:<id>` in `render_tile_for_agent`, `search_dashboard`) | Use the scope you hand-composed inside Claude Code / Cursor | It is not a competing view — it is the board *leaving the app*. Every competitor's scope (NotebookLM notebooks, Granola folders, Dust spaces) is trapped inside the vendor by design, because the scope *is* their lock-in. `mcp.rs` already ships `get_dashboard`/`list_dashboards` and the UI never mentions it. **The only item in this brief no competitor can follow even in principle** | S–M (+ the retrieval work, §6) |
| **`.canvas` export** | The scope outlives the app | The dream's unbuilt piece; `export/canvas.rs` already emits Canvas JSON. Also the strongest "you own this file" signal — the prototype put the vault path in the header (`index.html:664`) | S |
| **Board diff — "since I last looked"** | Weekly re-entry: *3 new recordings · 2 promises flipped late · 4 questions still unanswered* | The grid reports **state**; this reports **change**. Reporting change is the measured line between a board people open weekly and one that goes stale (Linear's data: median workspace has two dashboards, creation outruns usage). It is also the honest fix for `LivingAnswer`, which today cannot say "3 of your sources changed since" | M (`last_seen_at` per board; everything already has a timestamp) |
| **Read mode / Compose mode split** | Density when reading; chevrons, grips and delete only when composing | "Arrange mode" already exists but is the *only* path to a non-default span, which is why every board is a 3-column wall | S |
| **Subject-bound board templates (`$subject`)** | One "Person board" that works for all 57 people | Today a board is hand-built `ref_id`s, so a Person board must be rebuilt per person. Bases' `this` and Tana's `PARENT` are the best ideas in either product. `TileConfig` already carries `owner`/`predicate`/`question` slots. Also the substrate for auto-compose (§7) | M |
| **Question → board** (proposes sources, **user ratifies**) | Kills the blank page for the case where you don't yet know your scope | Only survives with the ratification step: AI *proposes*, user *ticks*. Auto-materialising is auto-RAG with a canvas and dissolves the thesis | M–L |

**Deferred with a named hazard:** a board embedded in a note. A `murmur:board` managed block is precisely the shape that leaked titles and live connector data on share in PR #416 — `clean_note_body` must strip it and the block must never persist resolved content into the `.md`. Requires `lock-security-reviewer`.

---

## 6. The board as AI context (the part that must not be diluted)

### The current contract, exactly

```
dashboard-view.ts::ask() → getDashboardSources → askVault(q, [], undefined, sources)
  → commands/mod.rs::ask_vault  (pinned_sources non-empty ⇒ the agentic path is SKIPPED)
  → commands/ask.rs::build_ask_vault_floor_prompt
  → summarize/vault_context.rs::build_vault_context_pinned_visible
  → provider.complete(system, user)          // ONE completion, no tools, no trace
```

| Quantity | Value | Symbol |
|---|---|---|
| Tiles per board | ≤ 60 | `dashboards_store.rs::MAX_TILES_PER_BOARD` |
| Tile kinds contributing a source | **3 of 10** | `get_dashboard_sources` — `_ => continue` |
| Corpus budget | 200 000 chars (4 000 for `ollama`) | `vault_context.rs::budget_for` |
| Per-source share | `budget / n`, hard `chars().take(quota)` | `fair_pack_explicit_sections` |
| Chat history sent | **`[]`, always** | `dashboard-view.component.ts` |
| Living-answer gate scope | the **entire** readable folder set | `TileConfig::answer_readable_folders` |
| Tile config cap | 8 KiB | `dashboards.rs:37 MAX_CONFIG_LEN` |

**The four gaps, in order of severity.**

**G1 — the scope is concatenated, not retrieved.** Pinned sources skip the agent loop; fair-share truncation means a 6-source board gives each source ~33 k chars and a 40-source board gives 5 k each cut mid-sentence. On `ollama` (4 k budget) 40 sources gets ~100 chars each — less than the `### [[Title]] · date · id:<uuid>` header itself, so the model confabulates from titles with no warning. **The board gets worse the more the user composes into it** — the one feature whose premise is "compose more scope" degrades monotonically in the composition variable. Two aggravations: each section is built at full budget and then thrown away (60 full note reads to emit 3 % of them), and `pack_meetings`/`pack_notes` compute `remaining` from `corpus.len()` (**bytes**) then consume `.chars().take(remaining)` (**chars**) while the caller counts `chars().count()` — a Polish or CJK vault overruns the declared budget by 2–4×.

**G2 — derived tiles are invisible to Ask.** Seven of ten kinds contribute nothing. A user staring at a Promises tile listing three late commitments asks "who owes me something on this board?" — the app's own suggested question — and the model cannot see that tile. **The in-app Ask sees strictly less of the board than an external MCP agent does.**

**G3 — citations cannot close.** `VaultSource` carries only `meeting_id` and is produced only by `pack_meetings`; the note/document arm pushes `Vec::new()`. `AskVaultResult.citations` is filled only on the agentic path, which pinned sources skip by construction. So **a note or document tile can never be cited**, and derived tiles have an entity `refId` or `null`.

**G4 — the living-answer gate has a capacity cliff.** `set_dashboard_answer` stores every readable folder id; a UUID serialises at ~39 bytes inside a JSON array, so **~210 folders exhaust the 8 KiB config** and an ordinary answer returns `InvalidArg("answer too large")`. `list_folders` includes note folders; a real Obsidian vault passes 100 trivially. This is a shipped bug nobody costed.

### The design

**D1 — the keystone: make the packer return a manifest.** Change `build_vault_context_pinned_visible` to return `PackedContext { corpus, refs: Vec<PackedRef{ kind, id, title, folder_id, chars_packed, truncated, role }> }`. Every field is already in hand at the moment `pack_meetings`/`pack_notes` push a header. It unlocks four unrelated fixes at once: citations for every tile kind (match on `(kind, id)`, not `meetingId`); a **precise** living-answer gate (record `refs.folder_id` instead of the whole readable set — sound, far tighter, and it fixes G4); a truthful truncation readout; and a proof that MCP and Ask ground identically.

**D2 — derived tiles enter the prompt.** New `get_dashboard_brief(id) -> String`: the board's derived tiles rendered through the **existing, already-redaction-correct** `render_tile_for_agent`, under one `lifecycle_guard`, capped ~4 000 chars, prepended as a labelled block:

```
WHAT THIS BOARD ALREADY SHOWS (the user composed these views; do not re-derive them):
- promises (Marcus Reid): Send Acme paperwork · due 2026-07-22 · late
- drift: Atlas GA · target_date: Apr 30 → Jun 14
```

Zero new gated reader, zero new egress class, ~1–2 % of budget. **Do not** turn derived tiles into `SourceRef`s — `SourceRef.kind` is a `LinkKind` and a drift lane is not a retrievable document.

**D3 — retrieve inside the scope.** When `n_sources × min_useful_chars > budget`, stop fair-sharing: run hybrid retrieval **restricted to the board's source set** and pack the top-k whole. The restriction must be pushed **into the search legs** (`search_visible_impl` / `search_semantic_visible` / the graph leg), not applied after fusion — a post-filter after `limit` returns zero rows for a board whose sources rank 41st–60th. Expose it as a board toggle, **Whole board** vs **Most relevant**, because neither dominates (Dust ships Include and Search as two explicit modes for exactly this reason) and the user is the one who knows.

**D4 — in-app citations.** Render answers with `src/app/shared/markdown/markdown.component.ts` (bold, sections, and `[[Title]]` as clickable gated chips — instantly). Number tiles from the manifest. Then inline `[n]` superscripts that scroll-and-flash the tile. **Reuse markdown.component's hard-won lesson:** read the title from `textContent`, never a `data-*` attribute — Angular's sanitizer strips unrecognised `data-*` even when DOMPurify allow-lists them.

**D5 — the MCP surface**, in order: (1) emit `tile:<id>` plus a board header (`# Atlas · id:… · tiles:12 · sources:7 · sealed:1`) in `render_tile_for_agent` — an external agent currently *cannot* cite a tile; (2) **`search_dashboard(dashboardId, query)`** — without it, the recommended agent behaviour after `get_dashboard` is vault-wide search, i.e. **the scope evaporates at the exact moment the agent needs depth**; (3) `get_dashboard_context` returning what in-app Ask packs, bounded by the existing `MAX_TOOL_WINDOW_CHARS`; (4) later, promote boards to MCP **resources** (`murmur://dashboard/{id}`) plus one prompt `ask_this_board` — returning large static blobs from *tools* is the known anti-pattern.

**D6 — scope reuse outside the board view.** `source-picker.component.ts::pickDashboard` already splices a board's refs into a flat selection — which destroys board identity, snapshots the membership at pick time, and truncates silently at `selectionLimit`. Carry `viaBoard: {id, title}` so chips read `Atlas · 7 sources` and the answer header can say "Grounded in Atlas", and **re-expand at ask time, not pick time** — free correctness: a tile added mid-thread joins the scope, a folder sealed mid-thread drops out. Do **not** wire boards into the record view (live transcript is the anchor there, and `acquire_external_egress_lease` risk during recording is real).

**D7 — the prompt-bloat bound.** Pass the 12-turn history (`capped_ask_history` exists; every other chat surface uses it). Surface `truncated` from the manifest ("12 of 47 sources were trimmed to fit"). Refuse *before* dispatch when the packed scope exceeds a sane bound rather than burning 180 s. Tag provider errors with `errcode` codes (`PROVIDER_TIMEOUT`, `PROVIDER_MODEL_REJECTED`, `PROVIDER_CLI_MISSING`, …) **inside the provider**, teach `content_free_dispatch_error` to preserve a leading `[code]` (a kebab-case token from a closed set leaks nothing — pin it with a test asserting the message matches `^\[[a-z-]+\] ` and contains none of the inner error's bytes), and delete both hand-rolled FE renderers in favour of `ErrorCopyService.humanize`.

**D8 — measure it.** Track B demands an oracle and nobody proposed one. **Until board-scoped Ask beats vault-wide Ask on a fixed question set drawn from the real vault, "a board is a retrieval scope" is an aesthetic preference with a canvas attached.** `eval/` exists; this is the missing experiment, and it is the only thing that can adjudicate D3's toggle defaults.

**Untested gate.** `get_dashboard_sources` — the function that *defines* the scope — has **zero** Rust tests; its only coverage is Playwright mocks asserting the FE's assumption. RED-first oracles before anything reshapes it: a sealed-folder source is absent; a derived tile contributes nothing; a deleted source yields `Missing` not an error; duplicates dedupe by `(kind,id)`.

---

## 7. The empty-board problem

**Reject sample data.** Every empty-state playbook recommends preloading demo content; it is wrong *here specifically*, because this app's entire pitch is that everything on screen was actually said. A fake board is the same credibility hole as the prototype's sine-wave waveform.

The strategy, in dependency order:

1. **Fix rendering first (§4.5).** The empty-tile problem is a *rendering* problem with a ~25-line fix that works on boards that already exist. Every composition-time mechanism costs an order of magnitude more and cannot protect a board over time. Ship the render fix, then measure whether the palette complaint survives it.
2. **Counts on the palette rows** (aggregate SQL, §2): `Numbers · 3 figures for "Atlas"` / `Drift — nothing of his has been revised yet` [greyed]. Hide unavailable offers in the browse band; **grey with a reason** in the subject band — where the user explicitly asked about someone, silence reads as a missing feature and a reason teaches the concept.
3. **"Compose a board about…"** — pick a subject, run the counts across kinds, materialise the 5–8 that return rows, ordered material-first. Uses the `$subject` binding so the result is a template instance, not a one-off.
4. **The empty Dashboards tab proposes computed boards**, not templates — real miniatures from real data (`Kuba — 12 recordings · 4 open promises`). One click materialises. If the vault has 0 meetings, say *"Record something first"* and link to Record. **Never a fake board.** If an auto "This week" board ships, label it *Auto*, make it read-only, and give it one button — **"Make this mine"** — that forks it into an editable board; the fork is the conversion event and it teaches composition by example. Exactly one auto-board; a shelf of them is Notion.
5. **The structural fix nobody proposed: composition must be a byproduct of normal use.** Boards are a late-game feature sitting in the primary nav, which is exactly why the user's first board was assembled from empty notes and single-fact entities. Add **"Pin to board"** to the transcript selection, the note header, the person page, and the Ask answer. If a user has to visit the Dashboards tab to *build* a board, it happens once.

---

## 8. What NOT to build

Each of these was proposed and each is killed for a specific reason.

| Killed | Reason |
|---|---|
| **Deleting or replacing any `resolve_tile` arm** (`numbers`, `pulse`, `drift`) | `dashboards.rs:1010` errors on an unknown kind and `get_dashboard` collects with `?` — **one unresolvable tile fails the whole board**, and the user's board contains both. The only alternative is a destructive rewrite of `dashboard_tiles.kind`. Retire from `NODE_TYPES` instead |
| **A vault-wide open-facts tile** (`list_open_facts_visible`) | It is not board-scoped. It puts content into a board the user did not compose, and `render_tile_for_agent` would push it into MCP's view of a scope the user believes they bounded. Board-scoped, its "47 rows" collapses to ~0–4 on the board in question. Also unbounded — no `LIMIT`, correlated `EXISTS` per row |
| **Preflight as speculative `resolve_tile` per candidate** (`get_tile_offers` / `dashboard_tile_candidates`) | ~230 resolves per palette open on this vault, on the one `Mutex<Connection>` a live recording writes through, where the `"note"` arm does a full vault note-list read *per note* and the `"person"` arm does a full-vault note scan. Replace with `GROUP BY` counts |
| **New tile kinds to retire old ones** (`source`, `commitments`, `facts` merges) | Nine touchpoints each, arm/kind parity enforced by test, and the old kinds cannot be removed anyway. The palette gets the same benefit for free |
| **A DB `UNIQUE INDEX` on `(dashboard_id, kind, ref_id, config)`** | The user's live DB already violates it; `Db::migrate()` runs on real user DBs → failed migration → failed startup |
| **`grid-column: span min(var(--tile-span), var(--cols))`** | No precedent for a math function in a `span <integer>` context; if WebKit rejects it the whole declaration drops and the board becomes a 12-column sliver. Clamp in TS |
| **Per-kind default span at add time** | Every existing tile already has a concrete `span = 4`; the default changes nothing about the board that was complained about. Use a display-span override |
| **Dim-the-field citations before the packer manifest** | Note/document tiles cannot be cited today, so dimming would spotlight the one meeting tile and assert a falsehood about what the answer stood on. Wrongly-cited is worse than un-cited |
| **A mandatory footer on every tile** | Adds ~28 px to every tile, re-homogenising the heights the same spec correctly identifies as the problem. Make it conditional |
| **Board-wide talk-time rollup / speaker roster** | `speaker_voiceprints` = 0 rows; `others-N` are per-meeting cluster tags. Summing them across meetings adds different humans together — the same class of lie as `looks_numeric` printing deadlines |
| **Live query as a source contributor** | Membership changing without the user touching it is auto-RAG in a tile costume; the board stops being hand-composed. Allowed only as a zero-`SourceRef` view |
| **`search_dashboard` / live query as a post-filter after `limit`** | Returns zero rows for a board whose sources rank below the vault-wide top-k, while the answer is on the board. The id set must go into the legs. Also silently dead without a downloaded embedder |
| **N3 "Open questions" aggregate count** | ~70 % precision is fine for a *row* (three seconds to verify by playing the tape) and a lie for the *headline* — the count is what the user reads and what feeds the prompt, and nobody plays all three. Ship the ledger without the number, later |
| **Regex "Numbers v2" over notes + transcript** | Matches `2026-07-04`, `1 min`, `Bielik-11B`, `8765`. It converts an *empty* tile into a *noisy* one. "No figures recorded" is honest; a grid of `11 / 3 / 2026 / 1` is a lie with better density. Only viable unit-anchored (currency/percent + unit words) |
| **In-tile audio playback for Quote v1** | `convertFileSrc`/`asset:` is the one audio path that bypasses `meeting_is_unlocked` — the leak that was already closed once by nulling `audio_path` in the masked DTO. Deep-link to the gated detail view instead |
| **Auto re-running living answers on a schedule** | Silent scheduled cloud egress. Make staleness visible; let the user press the button |
| **Scope-blaming error copy** ("this board is too big — 47 sources") | Would have been factually false on the board that produced the complaint (≤4 sources, ~100 chars of text) |
| **Board-wide "never empty" wallpaper** — vault-wide links/orphans, vault-wide timeline, going-quiet, mini brain-map, vault audit, ask history, org feed, attachments, decision log | Their non-emptiness comes from *ignoring the board*. They make the board look full while making it mean less. Cadence survives only board-scoped |
| **Scope readout as a tile** | The user would have to know to add it. It is board chrome |
| **`grid-template-rows: masonry`** | Not dependable in the shipping WKWebView |
| **A fake/synthetic waveform** | Decorative fake data in a provenance app |

---

## 9. Recommended sequence

| Phase | Scope | Files | Size | Risk | Lock review |
|---|---|---|---|---|---|
| **1 — make the existing screenshot look good** (FE only, retroactive, no migration) | Empty ladder: `is-empty` → 36-px dashed strip, forced span 3, sorted last · display-span override per kind · render-time duplicate strip · `--text-tertiary` + `--hatch-stripe` tokens, migrate the six ≤11 px call sites, delete the nine raw `rgba()` · 22-px per-kind mark + eyebrow-above-title (5 new `mur-icon` arms) · degenerate guards (drift `<2` rows, pulse `total===0`) · success-framed zeros · sealed tile re-copy | `dashboard-tile.*`, `dashboard-view.*`, `colors.css`, `theme-light.css`, `icon.component.html` | **M** | Low. Verify `color-mix` (already shipped in 6 files) and the mask fade in a **packaged** WKWebView build per T4/T5 — `ng build` green proves nothing here | No |
| **2 — data honesty** | `normalize_owner` treats any parenthetical-only owner head as `None` (**language-agnostic**, plus the note-generation prompt, or it silently regresses for an English `note_language`) · retire `numbers`/`pulse` from `NODE_TYPES`, gate `drift` on a count · palette count chips from aggregate `GROUP BY` SQL · non-empty filter on the note picker · subject-first palette order · wire or delete `registerField` + focus trap · fix/delete `due_count` | `summarize/action_items.rs`, `tile-palette.*`, one new count command | **M** | Low. `normalize_owner` is XS and un-zeroes `Person.open_commitments` vault-wide — the highest ratio in the whole brief | No |
| **3 — perf prerequisites** (block later phases; do them now) | Hoist `list_open_commitments` to **once per board** in `get_dashboard` (`Db::list_people` already does exactly this) — today two Promises tiles = two full-vault note scans, and `updateTile → reloadOpenBoard` re-resolves *every* tile, so one chevron click costs three full scans · patch span locally instead of `reloadOpenBoard` (span is pure layout, no gated payload changes) · RED-first Rust oracles for `get_dashboard_sources` | `commands/dashboards.rs`, `dashboards.service.ts`, `commands/tests/` | **S–M** | Low | No |
| **4 — Ask honesty** | `get_dashboard_brief` via the existing `render_tile_for_agent`, prepended to the pinned corpus · render answers + living answer through `app-markdown` · error turns as `.banner.is-danger` with retry · `errcode` tags at the provider, preserved through `content_free_dispatch_error` · pass the 12-turn history · fix the bytes-vs-chars budget mismatch | `commands/dashboards.rs`, `commands/ask.rs`, `errcode.rs`, `claude_code.rs`, `redact.rs`, `vault_context.rs`, dashboards FE | **M** | Medium — content enters a prompt via a new path, even though the renderer is already redaction-correct | **Yes** |
| **5 — the manifest (keystone)** | `PackedContext`/`PackedRef` from `build_vault_context_pinned_visible` · citations matched on `(kind, id)` → note/document tiles citable · living-answer gate narrowed to `refs.folder_id` (**also fixes the 8 KiB / ~210-folder cliff**) · truncation readout · then the §4.8 cited state | `vault_context.rs`, `models.rs`, `commands/dashboards.rs`, FE | **M–L** | **High — this narrows a security gate.** Keep `living_answer_withheld` a pure function and fail-closed on legacy/empty rows | **Yes, mandatory** |
| **6 — the scope leaves the app** | `tile:<id>` + board header in `render_tile_for_agent` · `search_dashboard` with the id set pushed into the search legs · `get_dashboard_context` · retrieval-inside-scope with the Whole-board / Most-relevant toggle · board identity through `source-picker` (`viaBoard`, re-expand at ask time) · `.canvas` export · MCP handle + vault path + composition line on the board header | `mcp.rs`, `tools.rs`, `db.rs` (search legs), `source-picker.*`, `export/canvas.rs` | **L** | High — protocol surface + a new query shape over gated readers | **Yes** (protocol + egress classification) |
| **7 — new tiles** | Board note/heading · per-meeting talk split (additive `me_ratio`/`segment_count`, NULL-speaker branch) · Quote (pin-from-transcript, anchors-only config, deep-link not in-tile audio) · Stayed-on-device · board-scoped cadence | `commands/dashboards.rs`, `dashboard-tile.*`, detail view (pin affordance) | **L** | Medium–high — Quote is a new read of `segments` (ungated reader, must gate at the call site) and touches the audio-path trap | **Yes** for Quote |
| **8 — polish** | Ask rail collapse · board emoji/tint/rename writers · home-card sealed marker, freshness line, sparkline, delete confirm, search + segmented · arrange-mode grip/keyboard/focus rings · motion pass · board diff · read/compose split | dashboards FE | **M** | Low | No |

**Phase 1 alone is the entire visible delta the user complained about.** Do not let phases 4–7 delay it, and do not sell phases 1–3 as differentiation — they are correctness and accessibility debt (a measured 3.46 : 1 contrast and a ΔE-3.6-under-protanopia palette are bugs, not taste).

---

## Open questions for the user

1. **Do boards earn their place in the primary nav, or should composition move into the surfaces where you already are?** The board is a late-game artifact — the industry base rate is that dashboards get created and then go stale — and yours was assembled from empty notes because there was a "+ Add tile" button and nothing else. **Recommendation:** keep the tab, but make "Pin to board" a first-class action in the transcript, the note header, the person page and the Ask answer (Phase 7). If composition never becomes a byproduct of normal use, boards will be built once each.

2. **When a board grows past the budget, should Ask *truncate everything* or *retrieve the most relevant*?** Today it silently truncates, so a 40-source board answers worse than a 6-source one — the composition incentive is inverted. **Recommendation:** ship both as a visible toggle (Dust ships Include and Search as explicit modes because neither dominates), default to Whole-board under ~12 sources and Most-relevant above it — and settle the threshold with the D8 eval, not by taste.

3. **Do you want `drift` fixed at the extractor, or retired?** It fires for 1 of 57 entities and `supersessions` has 0 rows, blocked by three independent mechanisms (stub reasoner, free-form predicates — 43 distinct across 48 facts, same-meeting skip). It is also the single most only-Murmur concept in the catalogue. **Recommendation:** retire it from the palette now (Phase 2) and open a separate investigation into predicate normalisation in `facts::reconcile_facts` — do not keep advertising it while it cannot fire, and do not entangle that fix with this work.

4. **Should a board be a static scope or a live one?** A saved-query tile whose membership changes without you touching it is more useful and less *yours*; the whole thesis rests on "the user hand-composed the universe." **Recommendation:** static membership for anything that contributes a source; live queries allowed only as views that contribute zero sources. If you want live membership, it should be an explicit per-board setting labelled as such, not a tile that quietly does it.

5. **Is a fake waveform / decorative chart ever acceptable?** The prototype's richness partly came from `Math.abs(Math.sin(i*.9))`. **Recommendation:** no, never on a tile representing a real recording — but say so out loud, because it means the shipped board will always be slightly quieter than that prototype, and the gap should be closed with real signals (talk split, chapter strip, heat strip) rather than by lowering the bar.