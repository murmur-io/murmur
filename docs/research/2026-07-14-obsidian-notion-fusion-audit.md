<!-- Generated 2026-07-14 via /research (murmur-researcher fan-out, 4 angles + adversarial code-grounded verify). Every "Murmur does X" claim was re-checked against the real tree (~v0.9.13), not docs. Pricing/version of competitors = point-in-time. -->
# Research: Is Murmur already a fusion of Obsidian + Notion?

## TL;DR / Verdict

**No — not yet, and the gap is lopsided.** Murmur draws **deeply and correctly** from
Obsidian's best (owned files, links, local-first) and from **Notion's *AI* half** (cited vault
Q&A + in-page AI writer), but it has **essentially none of Notion's three defining primitives**:
user-definable typed-property **databases**, switchable **database views**, and **real-time
multiplayer**. Adversarial, code-grounded score:

| World | How much of its "best" Murmur genuinely delivers | Why |
|---|---|---|
| **Obsidian** | **~65 %** | Nails the *file layer* (owned `.md`, front-matter, `[[wikilinks]]`, `obsidian://` + `^block-refs`, `.canvas`, vault auto-detect, local-first + SQLCipher-at-rest, reversible non-lock-in). Stops short of the *interaction layer*: no in-app note↔note backlinks panel, can't edit arbitrary existing vault files, no Bases, no plugins/themes/mobile. |
| **Notion** | **~28 %** | Strong on the *AI* pillars (workspace cited Q&A + in-page writer, privacy-superior). But **zero** user-schema'd databases, **zero** switchable views, **zero** live collaboration. The "block editor" is a markdown `<textarea>` with a snippet slash-menu. |

**Accurate one-liner (what the code supports):** Murmur is an **Obsidian-native meeting-memory
brain** with **Notion-grade AI and E2EE sharing bolted on** — *not* a general Obsidian+Notion PKM
workspace. Marketing it as an "Obsidian + Notion fusion" invites a database-and-collaboration
comparison it currently loses; **"the meeting layer of your Obsidian second brain"** is the claim
the code backs.

The two highest-leverage, constraint-respecting moves to draw *more* from both worlds:
**(1) a DB-backed note↔note backlinks reader** (closes the biggest "feels like Obsidian" gap),
and **(2) a folder-scoped Table/Board view over *typed* note front-matter properties**
(Obsidian-Bases-style — the one "database view" that stays owned-`.md`-native and SQLite-canonical,
and the substrate that later unlocks relations/rollups + AI-over-structured-data).

---

## What we already have (verified in code, ~v0.9.13)

> Docs like `docs/STATUS.md` are stale (say v0.6.4); this section is grounded in the current tree.

### Obsidian DNA — CONFIRMED (real code, not stubs)
- **Owned plaintext `.md` + YAML front-matter** — `export/obsidian.rs::write_note` /
  `write_and_sync` (atomic tmp+fsync+rename), `inject_provenance_frontmatter`,
  `inject_privacy_receipt_frontmatter`; front-matter contract in `summarize/template.rs`
  (keys English-locked so Obsidian keeps parsing). *Superset* of a hand-rolled Obsidian note.
- **`[[wikilinks]]`** — `export::list_vault_titles` feeds the LLM only *existing* titles
  (anti-hallucination); `entity_stub.rs::ensure_entity_backlink` writes `- [[Meeting Title]]`.
  Real self-assembling link layer (title-exact, LLM-authored — not free-form user linking).
- **`obsidian://` deep-links + `^block-refs` + callouts** — `build_open_url`, `append_pin`
  (`^block_id`), Re-Truth `[!superseded]`/`[!supersedes]` callouts.
- **`.canvas` spatial export** — `export/canvas.rs::build_canvas` emits valid Obsidian Canvas JSON.
  *Shallow*: one fixed meeting-node + topic-card layout, a one-shot artifact (no in-app canvas).
- **Vault auto-detection** — `detect_vaults_from` parses Obsidian's own
  `~/Library/Application Support/obsidian/obsidian.json`.
- **Local-first + at-rest crypto** — on-device whisper.cpp, `ollama`/`claude_code` local providers,
  whole-DB SQLCipher (`Db::open_with_key`), redaction firewall (`summarize/redact.rs`),
  `127.0.0.1` read-only MCP. *Stronger than Obsidian's plaintext vault.* (Caveat: Ask/Brain cloud
  + org sharing **do** egress, gated — "offline" is provider-dependent.)
- **Non-lock-in** — every note is re-exportable plaintext `.md`; even locked content is reversible
  to plaintext (`remove_lock`). Caveat: the *richest* state (entities/facts/timeline/vectors) lives
  in SQLite — the `.md` is a faithful but lossy projection.

### Notion DNA — CONFIRMED
- **Workspace-wide cited AI Q&A** ("Ask AI" analog) — `summarize/vault_chat.rs::agentic_system_jit`,
  `agent.rs::run_agentic_loop`, gated tool loop `tools.rs`. Local-first + provider-swappable +
  cited `[[Title]]` → **equals/beats Notion AI on privacy.** Murmur's real center of gravity.
- **In-page AI writer** (Notion-AI-on-selection analog) — `notes/note-brain-popover/note-assist-catalog.ts`,
  **19 actions** (refine, shorten, expand, tone, translate, table, key-points, link-entities,
  fact-check, action-items, draft-follow-up, spin-off…). Rides the provider seam + redaction.
  *(The briefs said "21"; verified 19 — see Overclaims.)*
- **E2EE async sharing + Org "Shared Brain"** — `e2ee/wrap.rs` (real HPKE X25519 + AES-256-GCM +
  detached Ed25519), `e2ee/org.rs` (OCK per-member wrap/rotate), `share/`, sibling server
  `../murmur-server/` (zero-knowledge REST relay). An axis **both** Obsidian-core and Notion
  (plaintext-server) lack. *Async snapshot re-publish-on-edit, **not** live co-editing.*

### Unique axes (neither Obsidian nor Notion has these natively)
- **Bitemporal fact/triple store** — `storage/db.rs` `facts`/`user_facts` (subject·predicate·object,
  `valid_from`/`valid_to`, supersede-not-delete). Structured data Notion lacks — but **AI-derived,
  read-only**, no user-editable schema.
- **Local read-only MCP server** — `mcp.rs` (`127.0.0.1:8765`, token-gated, visibility-gated tools).
  The honest "extensibility answer" in lieu of a plugin ecosystem.
- **The actual moat:** local-first **far-side capture** + on-device transcription + owned files +
  **no per-seat AI fee**.

---

## Findings per angle

### 1. Obsidian side — genuine, but stops at the *interaction* layer
CONFIRMED file-layer parity (above). What's **MISSING / partial** and makes it "not fully Obsidian":
- **No in-app note↔note backlinks / linked-mentions panel.** The "graph" is an **entity
  co-occurrence** graph (`entity_mentions` self-join, `db.rs::build_graph`); "backlinks" =
  *meetings mentioning an entity* (`models.ts` backlinked meetings), **not** Obsidian's per-note
  reverse-link index. Wikilinks are *written* to the vault but never *surfaced* in-app. **Verified
  MISSING** for the note-to-note direction — Obsidian's single most-loved feature.
- **Can't edit arbitrary *existing* vault files.** The Notes editor edits Murmur-owned
  `documents(kind='note')` rows; `import_document` ingests **one** specified `.md`/`.txt` on demand
  (rejects other types) and there's **no live vault watcher/scanner**. Blocks the "second brain over
  my *whole* vault" promise.
- **No Bases** (Obsidian's headline 2026 no-code DB views over front-matter), **no plugin ecosystem**
  (~2.5–5.6k plugins), **no community themes**, **no mobile**, **only partial sync** (E2EE per-note,
  not whole-vault device sync).

### 2. Notion side — strong AI, **absent** structure & collaboration
- **MISSING — user-definable typed-property databases.** Note "properties" are `Record<string,string>`
  untyped YAML scalars (`notes/note-editor/front-matter.ts`); the DB has only fixed app tables. No
  select/relation/rollup/formula **column types**. *This is Notion's literal core.*
- **MISSING — switchable database views.** `grep kanban|board-view|calendar-view|gallery-view|data-table`
  → **zero**. `library.component.ts` is one date-sorted list (folder **or** tag filter). The
  `notes-home` `<mur-table>` is self-described in-code as "a dense list to scan, not a reading
  column" — a styled list, **not** a DB engine.
- **MISSING — real-time multiplayer.** `grep crdt|yjs|automerge|presence|websocket` across the app
  **and** `../murmur-server/crates` → **zero**. The server is a pure-REST zero-knowledge blob relay;
  Org "Shared Brain" is **async re-publish snapshots**, not co-editing.
- **PARTIAL / analog (real, but not equal):** relations/rollups → auto-derived entity dossiers
  (`summarize/dossier.rs`, read-only); dashboards → **preset** Analytics/Digest/Briefs
  (`analytics.component.ts`, `brief_runner.rs`), not user-composable; templates → **meeting-only**
  prompt recipes (`summarize/recipes.rs`; `run_recipe(meeting_id)` reads segments — won't run over
  an arbitrary note); web-publish → static `/s` E2EE viewer; connectors → **Jira + Slack + web +
  calendar + BYO-MCP only**.
- **Block editor is faked:** `note-editor.component.html` body is a single `<textarea>`; the `/`
  menu inserts **markdown snippets**, not block objects. Notion-ish chrome over a flat markdown string.

### 3. Is "Obsidian + Notion fusion" a real category — and does Murmur belong?
**The category is real** — Anytype, AppFlowy, Capacities, Tana, SiYuan, Reflect and (post-Bases)
Obsidian itself explicitly pitch "local-first ownership × Notion-style structure." But they're
judged on **user-authored databases + mobile quick-capture** — exactly what Murmur lacks. **Murmur
does not honestly belong to that category**; it's a meeting-memory app, not a general PKM workspace.
Positioning implication below.

### 4. Gaps → what to add to genuinely fuse both (prioritized, code-grounded)
See Options. Highest-leverage single move: a **user-authored structured layer (typed front-matter
properties) + saved views**, because it's the one Notion primitive Murmur entirely lacks *and* the
substrate for relations/rollups + AI-over-structured-data — and it fits SQLite-canonical + owned-`.md`
cleanly (each row ⇄ a `.md` file with typed front-matter).

---

## Overclaims the adversarial pass caught (kept honest)
- **"21 in-page AI actions"** (briefs 2 & 4) → **actually 19** in `note-assist-catalog.ts`.
  Capability real; count inflated.
- **"Self-assembling graph *with backlinks*"** reads like Obsidian's note-link graph → it's an
  **entity co-occurrence** graph; the note↔note backlinks direction is **MISSING in-app**.
- **"Backlinks panel = partial"** → generous; **MISSING** for note-to-note.
- **"Templates"** → **meeting-transcript-only** prompt recipes, not page/DB templates.
- **Connectors roadmap "Jira→Slack→Linear→ClickUp"** (memory) → only **Jira+Slack+web+calendar+BYO-MCP**
  ship; **no** Linear/ClickUp/GitHub/Notion/Gmail.

---

## Fit with Murmur's constraints
| Constraint | Impact on the "become more Notion" moves |
|---|---|
| **Owned plain `.md` (no lock-in)** | Typed properties must live in **front-matter** and round-trip byte-safe (`front-matter.ts` is already a tolerant round-tripper). A true **block model is a poor fit** (rich blocks don't map cleanly to Obsidian markdown → lossy/lock-in). |
| **SQLite-canonical** | A "database" is just rows + a column-schema row; **natural fit**. Saved views = persisted `{filter,sort,group,columns,layout}` JSON. |
| **Per-folder lock model** | DB rows/views must route through `meeting_is_unlocked`/`visibility_clause` like every other read — no new ungated path. |
| **Provider seam + redaction firewall** | `AI-over-structured-data` = one new **gated `query_database` tool** on the existing `tools.rs` loop; **near-zero egress on Ollama.** |
| **Local-first / macOS-first / E2EE** | **Real-time multiplayer** fundamentally strains all three (a CRDT/presence relay sees edit streams) → **defer/skip**; async comments on shared copies is the honest ceiling. |

---

## Options & tradeoffs
| # | Move | World | Effort | Unlocks / why |
|---|---|---|---|---|
| **1** | **Note↔note backlinks reader** (DB-index `[[Title]]` occurrences across notes/meetings, gated by `visibility_clause`, surfaced as a chip row in detail + note-editor) | Obsidian | **M** | Closes the biggest "feels like Obsidian" gap; reuses the app-sources chip component. **Smallest self-contained win.** |
| **2** | **Saved views** over the meeting/note list (table w/ chosen columns, board by status/tag, calendar by date, gallery, group-by, multi-sort) | Notion | **M** | Pure FE over data Murmur already has (date/tags/folder/entities/action-items). Most of the felt "Notion database" magic **without** the schema layer. No new egress, no lock-model change. |
| **3** | **Typed front-matter properties + folder-scoped Table/Board view** (Bases-style; each DB row ⇄ an owned `.md`) | both | **L** | The one structured-DB primitive Murmur lacks; the substrate for #4/#5. Stays owned-`.md` + SQLite-canonical + lock-gated. |
| **4** | **`query_database` gated brain tool** → AI-over-structured-data ("projects with >3 open action items owned by Anna") | Notion | **M** (after #3) | Highly differentiated NL query over **local** structured data, cited, zero cloud egress on Ollama. Rides the built tool loop. |
| **5** | **Read-only whole-vault indexer** (scan+embed every `.md` into `doc_chunks`, gated, no write-back) | Obsidian | **L** | Lets Ask/Brain answer over the user's **entire existing vault** — the brain2 north-star. Must handle external edits + `.md`→DB drift. |
| **6** | **Note/DB templates** (starter `.md` + front-matter scaffold, instantiable into a folder; extend recipes beyond meetings) | Notion | **S** | Makes "Templates" mean what a Notion user expects. Best after #3. |
| — | **Deliberate non-goals:** true block editor, real-time multiplayer, plugin ecosystem, mobile editor | — | — | Frame explicitly; **MCP is the extensibility answer**, async E2EE snapshots the collaboration answer, read-only companion the mobile ceiling. |

---

## Recommendation & first step
**Positioning:** stop (or never start) calling Murmur an "Obsidian+Notion fusion." Own
**"the meeting layer of your Obsidian second brain"** — the moat the code actually backs
(far-side capture + on-device + owned files + no per-seat fee). This avoids a database/collaboration
comparison Murmur loses today.

**Product, if we want to draw *more* from both worlds** — build in this order, each a verifiable slice:
1. **Backlinks reader (#1, M)** — smallest, self-contained, closes Obsidian's most-loved gap.
2. **Saved views (#2, M)** — pure FE, biggest "feels like Notion" payoff per unit effort; also the
   stepping-stone to #3.
3. **Typed properties + Table/Board view (#3, L)** → then **`query_database` tool (#4, M)** almost
   for free on the existing agent loop.

**Smallest verifiable first step:** ship the **backlinks reader** — index `[[Title]]` occurrences
into a gated query, surface "what links here" as a chip row in the meeting-detail + note-editor.
Backend `cargo test --lib` RED→GREEN on the reverse-link query, FE `ng lint`/`ng build`, adversarial
verify (leak: the reader **must** route through `visibility_clause` so a locked note never leaks a
backlink). Then a saved-views spike (#2) as the second slice.

## Open questions / not verified here
- **Demand weighting:** which do *our* users want more — the Obsidian backlinks/whole-vault-brain
  side, or the Notion structured-views side? (This audit establishes feasibility & leverage, not
  demand; a user signal would re-rank #1 vs #2/#3.)
- **Bases interop:** should Murmur's typed front-matter deliberately match **Obsidian Bases'**
  property conventions so the same notes power Bases views *inside* Obsidian? (Would make #3 a
  force-multiplier rather than a parallel system — worth a spike.)
- Effort tags are code-grounded estimates, not spikes; #3/#5 need a real design pass (`.md`⇄row
  mapping, external-edit reconciliation).

## Sources
**Code (verified symbols):**
`export/obsidian.rs` (write_note, write_and_sync, inject_provenance_frontmatter,
inject_privacy_receipt_frontmatter, build_open_url, append_pin, detect_vaults_from, list_vault_titles);
`export/canvas.rs::build_canvas`; `export/entity_stub.rs::ensure_entity_backlink`;
`summarize/template.rs` (front-matter contract); `summarize/{vault_chat.rs,dossier.rs,recipes.rs,redact.rs}`;
`agent.rs::run_agentic_loop`, `tools.rs`, `mcp.rs`; `storage/db.rs` (entities/entity_mentions/facts/user_facts,
`build_graph` co-occurrence, `documents`); `e2ee/{wrap.rs,org.rs}`, `share/`, `commands.rs`
(get_graph, list_people, create_note/get_note/update_note_doc, import_document, run_recipe,
export_meeting_canvas, republish_org_shares_for_source); `connectors/{web,jira,slack,mcp_client,calendar}.rs`;
FE `features/{notes,graph,library,analytics,briefs,ask,people,sharing,org}/`,
`notes/note-editor/{note-editor.component.*,front-matter.ts}`, `note-brain-popover/note-assist-catalog.ts`;
sibling `../murmur-server/crates/murmur-server/src/routes/mod.rs` (REST shares/orgs, static `/s` viewer;
no ws/CRDT). Negative greps (MISSING): `kanban|board-view|calendar-view|gallery-view|data-table`,
`crdt|yjs|automerge|presence|websocket`, `PropertyType|columnType|rollup-column|formula`,
`create_entity|rename_entity|merge_entity` — all zero.

**Web (point-in-time, 2026):** Obsidian Bases docs (help.obsidian.md/bases) + 1.10 release notes +
XDA "Bases replacing Notion"; Obsidian plugin directory + "future of plugins" (4k+ plugins, 120M+
downloads); Notion 2026 database/views/relations/rollups + "Ask AI" guides; Anytype/AppFlowy/SiYuan/
Tana/Capacities/Reflect comparisons (AndroidPolice "I tried Notion/Obsidian/Capacities/Anytype",
openalternative, noteapps.info); Obsidian community meeting-transcription plugins (confirms demand +
that Murmur's edge is local far-side capture, not the fusion category).

**Related in-repo:** `docs/COMPETITIVE-LANDSCAPE.md`, `docs/KILLER-FEATURES.md`,
`docs/research/2026-07-06-note-and-brain-architecture.md`.
