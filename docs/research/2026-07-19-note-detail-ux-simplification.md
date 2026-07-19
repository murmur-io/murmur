<!-- Generated 2026-07-19 via /research (murmur-researcher fan-out + code grounding + adversarial mechanism verify). Pricing/version = point-in-time. -->
# Research: Simplifying the note-detail IA (Connections / backlinks / suggestions / Ask panel)

**Trigger:** the routed note-detail view feels overwhelming — the "Suggested connections" wall, content overflowing past the Ask panel, and a confusing linking model where the same "Meeting 2026-07-17 01:31" shows up as a MEETING chip, a NOTE chip, and twice as an inline `[[wikilink]]`. Goal: minimalist **but functional** — keep every feature, fix the information architecture.

Companion visual proposal (before → after mockup): Artifact "note-detail-redesign".

---

## TL;DR / Verdict

- **The linking mechanism is CORRECT** — edges are recorded faithfully (one `[[Title]]` → exactly one edge), title-collisions are handled, and every read path is both-endpoint lock-gated. **No data bug, no leak.** The pain is **presentation + identity modeling**, exactly as the user suspected.
- **Root of the overwhelm:** the note-detail renders **~30 interactive surfaces**, with **three separate panels expressing the same note↔item relationship** — `Linked mentions` (inbound backlinks), `Connections` (all incident edges), and inline `[[wikilinks]]` — plus a persistent **Suggested-connections Accept/Dismiss wall**. **No researched PKM tool (Obsidian, Notion, Tana, Capacities, Craft, Reflect, Mem, Roam, Logseq) ships three persistent relationship panels.** They converge on a **two-surface model**: one collapsed footer "Related" section + (optionally) one AI side surface.
- **The "same target as a meeting chip AND a note chip"** is because the note is linked to **both** the meeting **and** its **auto-created companion note** (identical title by construction); `collapse_manual_duplicate_edges` keys on `(kind,id)` and has no notion of the companion relationship, so they never merge.
- **The Ask-panel overflow** is a one-line CSS bug: `.md-wikilink { white-space: nowrap }` on an `inline-flex` chip → long citation titles can't wrap inside the fixed-width pane.
- **Recommendation:** an **IA consolidation, not a feature cut**. Collapse the three relationship surfaces into **one collapsed "Related" section**; demote AI suggestions from a decision-wall to **ambient one-tap chips** (no %, no two-button rows); keep the Ask drawer as the one right-hand chrome column; collapse the meeting↔companion-note duplicate at the backend seam; slim the header. **Always-visible competing blocks drop from ~5–7 to 2. Zero features removed.**

---

## Co już mamy (from the repo, file:symbol — line numbers drift, grep the symbol)

- **Note-detail = `NoteEditorComponent`** (`src/app/features/notes/note-editor/note-editor.component.{ts,html,scss}`). Column order: header → title → collapsible Properties → **`app-backlinks`** ("Linked mentions") → **`app-connections`** ("Connections" + "Suggested connections") → body textarea/preview (with inline `[[wikilinks]]`). Right column: **`app-note-chat`** ("Ask about this note").
- **Backlinks:** `app-backlinks` (`src/app/shared/backlinks/backlinks.component.ts`) → `getBacklinks` → `storage/links.rs:backlinks_for_visible` (inbound sources that mention this item). Already has the `limit()` + "+N more"/"Show less" idiom.
- **Connections:** `app-connections` (`src/app/shared/connections/connections.component.ts`) → `listLinks` → `storage/links.rs:links_for_visible` (all incident edges, both directions). Partitions into `deterministic()` (wikilink/companion/manual/accepted-semantic) chips and `suggestions()` (semantic, `status==="suggested"`) Accept/Dismiss rows with a confidence tier (`0.88` high / `0.84` med / `0.80` low).
- **Edge model:** `EdgeType = wikilink | companion | semantic | manual` (`src-tauri/src/links.rs`). Within-connections dedup: `storage/links.rs:collapse_manual_duplicate_edges` keys on `(other_kind, other_id)` — "Groups NEVER merge across different `(other_kind, other_id)`" (its own doc).
- **Wikilink resolution:** `storage/links.rs:resolve_wikilink` returns a **single** endpoint by priority (note → meeting → org, each `LIMIT 1`), and **deliberately excludes the companion note** when the queried title equals that note's own meeting's title, so `[[Meeting…]]` falls through to the meeting. `index_wikilinks_for_source` pushes at most one edge per title; `extract_wikilink_titles` dedupes to first-seen.
- **Ask panel:** `app-note-chat` (`src/app/features/notes/note-chat/note-chat.component.{ts,html,scss}`) in `bare` drawer mode; renders assistant turns via `app-markdown` (`src/app/shared/markdown/markdown.component.*`) and a `mur-source-picker` (`src/app/design-system/source-picker/`) "Sources" pill row. Drawer width `clamp(320px, 30vw, 400px)` (`note-editor.component.scss:.note-chat-drawer`).
- **Semantic-link engine (unchanged by any FE work):** `src-tauri/src/links.rs` — `SEMANTIC_LINK_CAP = 5`, floor `0.80`, mutual-kNN selection, tombstone on dismiss (`acceptLink`/`dismissLink`/`unlinkItems`).

---

## Findings (per angle; each claim CONFIRMED/PLAUSIBLE + evidence)

### A. IA census — how much is on screen (grounding agent, CONFIRMED)
- **~30 distinct interactive surfaces** coexist in the routed view. Persistent: 8-control header (Move, "not sealed", save-state, Edit/Preview, Ask Brain, Share, ⋯[Full-width, Save-to-vault, Delete]), title, Properties(tags/typed widgets/add-prop), Linked mentions, Connections(chips, Suggested wall, +Link, remove ×), body, inline wikilinks, preview; transient overlays: slash menu, selection bubble, Brain popover, link picker, Share modal, Move/⋯ menus; Ask pane(header, log/starters, source-picker, composer); plus the top `detail-tabs` strip when opened from a meeting.
- **The header's own SCSS admits it jams:** `note-editor.component.scss:.editor-head` comment — controls "run out of room and jam together" and must wrap. (CONFIRMED)
- **Three Brain entry points** over one note: header Ask-Brain drawer, selection-bubble Ask-Brain (Brain popover), and the meeting-host `detail-tabs` Ask-Brain tab. (CONFIRMED)
- **The same `LinkPicker`/`listLinkCandidates` feed is instantiated three times** in this view (raw `[[`, Connections `+ Link`, Ask `Sources`). (CONFIRMED)

### B. Linking-mechanism correctness (adversarial verify agent — CONFIRMED, the user's explicit ask)
1. **Wikilink resolution — one link → one edge.** `resolve_wikilink` returns a single endpoint by priority; the self-link carve-out sends `[[Meeting…]]` to the **meeting**, excluding the companion note. **Not** a title-collision-into-two-edges bug. (CONFIRMED)
2. **Cross-surface overlap is real and un-deduped.** `backlinks_for_visible` (inbound-only) and `links_for_visible` (all incident) are two independent commands rendered side-by-side; `connections.edges` is only what `listLinks` returns, with **no filter against `backlinks()`** → an inbound wikilink neighbour appears in **both** sections. (CONFIRMED)
3. **Meeting vs companion-note = two chips (the reported bug).** `collapse_manual_duplicate_edges` keys on `(kind,id)`; a meeting `(meeting,m)` and its companion note `(note,n)` are distinct pairs → two groups → two identically-titled chips. The companion note is auto-created with the meeting's exact title (`get_or_create_companion_note_inner` → `create_note_inner(state, None, &meeting_name)`). **There is no place that collapses a meeting↔its-companion-note pair via `documents.meeting_id`.** (CONFIRMED — this is the root cause of "meeting chip AND note chip")
4. **Inline wikilink is also a chip.** A body `[[Title]]` materializes a `wikilink` edge → also a Connections chip; there's no "already inline" suppression. Two body wikilinks dedupe to first-seen → still one edge (the double body line is an authoring artifact, e.g. front-matter `meeting:` link + a body mention). (CONFIRMED)
5. **Lock gating — no leak.** Both `links_for_visible` and `backlinks_for_visible` are both-endpoint visibility-gated (queried item + each neighbour), TOCTOU-hardened on the write path, and covered by tests (`links_for_visible_gates_both_endpoints`, `backlinks_*`). (CONFIRMED)

**Verdict:** mechanism CORRECT; the issue is presentation/identity modeling.

### C. Overflow root cause (self-verified, CONFIRMED)
- `markdown.component.scss` `.md-wikilink` is `display:inline-flex` + **`white-space: nowrap`**. Long assistant-message citation titles (e.g. "Test nagrania — prośba o analizę pogody na następny tydzień") become one unbreakable box wider than the `clamp(320,30vw,400)px` Ask pane and bleed past its right edge. `.md-body` has `overflow-wrap: anywhere` but that cannot break a `nowrap` flex box. The source-picker pills (`.sp-chip` `max-width:220px` + `.sp-chip-title` ellipsis) already truncate, so **the culprit is the markdown citation pill, not the connections chips** (g1's initial `.cx-chip` attribution was wrong). Minimal fix: allow the pill to wrap or truncate; add a message-column width cap.

### D. Relationship-IA prior art (murmur-researcher — patterns with sources)
- **Two-surface convergence.** Not one mainstream PKM tool ships three persistent relationship panels. They use: (1) **one footer "relationships" section**, hidden-when-empty, with typed collapsible sub-groups (Obsidian "Linked/Unlinked mentions"; Roam "Linked/Unlinked References"; Tana "References"); (2) AI/semantic "related" kept **separate** (a right-rail "Similar notes" in Reflect/Mem, or an on-demand command). Sources: obsidian.md/help/plugins/backlinks, tana.inc/docs/nodes-and-references, reflect.app/blog/what-are-backlinks-a-guide.
- **Collapse to a count chip.** Notion shows just "{N} backlinks" under the title (revealed on hover, hidden when zero); Craft auto-hides the section when empty. Strongest antidote to "overwhelming": near-zero footprint until asked for. Source: notion.com/help/create-links-and-backlinks.
- **Dedup a neighbour across directions.** Obsidian tabs between a Backlinks pane and an Outgoing-links pane (never both stacked); Tana renders a bidirectional relationship once with direction as a label. One row per neighbour, direction as an attribute. Source: tana.inc/docs.

### E. AI-suggestion UX (murmur-researcher — the Accept/Dismiss wall is the outlier)
- **Ambient show-don't-ask.** Obsidian Smart Connections, Reflect "Similar notes", Heptabase, Mem all show a ranked related list and let **one click/drag** create the link — **no per-row Accept/Dismiss**. The suggestion IS the affordance; ignoring it costs nothing. Sources: github.com/brianpetro/obsidian-smart-connections, get.mem.ai/blog/mem-2-0.
- **Hide the raw confidence %.** Mem/Heptabase/Reflect show no score; Smart Connections uses a faint underline. AI-UX guidance: numeric confidence is not for creative/consumer surfaces — "showing 90%+ on almost every answer trains users to ignore the score." Our floor is already `0.80`, so every shown score is high → the % adds anxiety, not information. Source: aiuxplayground.com/pattern/confidence-score.
- **Hover-only secondary actions.** We already do this for manual-link removal (hover × in `connections.component.html`). Reuse it: chip body = accept/promote (`acceptLink`), hover × = dismiss (`dismissLink`) → the two-button row collapses to one chip. (CONFIRMED against our own code)
- **Antipattern to avoid:** auto-decorating the note **body** with AI backlinks (Reflect's "Decorate with backlinks" rewrites prose). WRONG for Murmur — the `.md` is user-owned; keep suggestions in the chip zone, never silently edit owned text.

### F. AI side-panel + minimalism principles (murmur-researcher)
- **Content is the hero; AI is a calm dismissible side column** (ChatGPT Canvas, Notion AI, Cursor, Granola, Claude). Murmur already has this shape — the fix is to stop polluting the content column with three relationship walls. Source: openai.com/index/introducing-canvas.
- **Single-level progressive disclosure.** NN/g: show frequent items up front, defer the rest behind **one** control; ">2 disclosure levels → users get lost." Source: nngroup.com/articles/progressive-disclosure.
- **Cognitive load / Miller's 7±2.** The resting view competes ~5–7 sibling blocks — past the limit; cut extraneous load via grouping + disclosure. Source: lawsofux.com/cognitive-load.
- **Liquid Glass = navigation/chrome layer only** — "avoid glass in the content layer… avoid glass on glass"; "hierarchy through layout and grouping," not decoration. Maps exactly to: note body = content; Ask column = the one glass chrome surface; floating pickers stay opaque (trap T3). Source: developer.apple.com/design/human-interface-guidelines/materials.

---

## Fit with Murmur's constraints

- **Local-first / privacy:** no new cloud egress — this is IA + CSS + one backend collapse. Suggestions already run on-device.
- **Obsidian-native / owned files:** the redesign is literally the Obsidian backlinks model (one footer relationship section). Do **not** auto-inject `[[links]]` into owned prose.
- **SQLite canonical:** the FE merge/dedup is a read-time client concern; the backend companion-collapse is a read-time transform, not a schema change.
- **Lock model:** all relationship reads are already both-endpoint gated (Finding B5). The backend companion-collapse (Option L) touches the visibility path → **requires lock-security-reviewer**. FE-only options do not.
- **Angular zoneless / Liquid Glass:** reuse existing primitives (`app-backlinks` limit/show-more idiom, hover-× pattern, opaque overlays). No new deps.
- **CI / verify:** FE changes verified headless at `:1420` on **WebKit and Chromium** (T4); a mutual-link fixture must render the neighbour exactly once.

---

## Options & tradeoffs

| Tier | Scope | What ships | Risk | Unlocks |
|---|---|---|---|---|
| **A — S (FE-only)** | 1 PR | Overflow fix (`.md-wikilink` wrap) · hide Connections when empty · collapse "Suggested" behind an "N suggested" count · drop raw % | none (no data) | immediate visual relief, reversible |
| **B — M (FE-only)** | 1–2 PR | Merge Linked-mentions + Connections into one deduped "Related" list (key `(kind,id)`, direction as a tag) · suggestions as ambient chips (tap=link, hover ×) · collapsed-by-default "N related" with "+N more" · don't re-chip an inline-linked neighbour | low (client merge/dedup; stale-guard already exists) | the core "one Related surface" IA |
| **C — L (BE + FE)** | 1–2 PR | Backend companion-aware collapse (meeting ↔ its companion note → one chip) · slim header to ≤5 · unify the 3 pickers behind one affordance | medium — **lock-security review** for the edge-visibility change | kills the most literal "same thing twice"; consistent across MCP/graph too |

**Sequencing:** A → B → C. A is a same-day win; B is the heart; C is the root-cause polish that also benefits the MCP/graph consumers.

---

## Recommendation & first step

**Do an IA consolidation, not a feature cut.** Target end-state:
1. **One "Related" footer section** under the body: a single deduped chip list (inbound + outbound merged, one row per neighbour, direction/type as a small tag), collapsed by default behind an "N related" count, auto-hidden when empty.
2. **Suggestions demoted** to ambient dashed chips inside that section — tap promotes (`acceptLink`), hover × dismisses (`dismissLink`), no % and no two-button rows. Engine, thresholds, and IPC unchanged.
3. **Ask drawer stays the one right-hand chrome column**, dismissible; fix the citation-pill overflow; keep the single Sources pill row.
4. **Header slimmed** to ≤5 (Share + Full-width into ⋯; "not sealed" → 🔒 in the breadcrumb).
5. **Meeting ↔ companion-note collapsed at the backend seam** (`collapse_manual_duplicate_edges` / a `links_for_visible` pre-pass, companion-aware via `documents.meeting_id`).

**Smallest verifiable first slice (Tier A):** fix `.md-wikilink` wrapping + collapse the "Suggested" wall behind a count + hide the empty Connections panel. FE-only, reversible, testable headless against mocked IPC; measure vertical footprint and "same target shown N times" before/after. Then Tier B (the real merge/dedup), then Tier C with a lock-security review.

---

## Open questions / not verified

- **Real WebKit render of the overflow fix** — needs the running dev app / a packaged build, not just `ng build` (T4). The nowrap→wrap root cause is CONFIRMED in source; the exact fix (wrap vs truncate-with-tooltip) is a taste call to try live.
- **Companion-collapse edge cases** — a note that links a meeting but NOT its companion (or vice-versa); a meeting with no companion note yet. The collapse must degrade to "show whatever exists," never hide a real link. Needs the backend implementation + `lock-security-reviewer`.
- **Touch ID / lock-at-rest behavior** of any relationship change is only truly verifiable on a signed build.
- **User taste on "collapsed by default"** — Notion hides relationships behind a hover chip; some users want them always visible. Worth a quick dogfood toggle (collapsed vs top-2-visible) before committing.

---

## Sources

**Code (file:symbol):** `note-editor.component.{ts,html,scss}` · `shared/connections/connections.component.{ts,html}` · `shared/backlinks/backlinks.component.ts` · `note-chat/note-chat.component.{html,scss}` · `shared/markdown/markdown.component.scss` (`.md-wikilink`) · `design-system/source-picker/source-picker.component.scss` · `src-tauri/src/links.rs` (`resolve_wikilink`, `SEMANTIC_LINK_CAP`) · `src-tauri/src/storage/links.rs` (`links_for_visible`, `backlinks_for_visible`, `collapse_manual_duplicate_edges`, `index_wikilinks_for_source`) · `src-tauri/src/commands/mod.rs` (`get_or_create_companion_note_inner`).

**Web:** obsidian.md/help/plugins/backlinks · notion.com/help/create-links-and-backlinks · tana.inc/docs/nodes-and-references · capacities.io/whats-new/release-9 · reflect.app/blog/what-are-backlinks-a-guide · reflect.app/blog/automatically-add-backlinks-using-ai · github.com/brianpetro/obsidian-smart-connections · smartconnections.app · get.mem.ai/blog/mem-2-0-dev-update-mem-copilot · aiuxplayground.com/pattern/confidence-score · openai.com/index/introducing-canvas · setproduct.com/blog/ai-chat-interface-ui-design · nngroup.com/articles/progressive-disclosure · lawsofux.com/cognitive-load · developer.apple.com/design/human-interface-guidelines/materials.
