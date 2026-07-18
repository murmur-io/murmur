# Note / Recording Surface Redesign — Design Spec

**Date:** 2026-07-18
**Branch:** `murmur` (implemented in an isolated worktree → PR into trunk)
**Status:** Design approved on direction (4 forks decided); awaiting spec review before planning.
**Related in-flight work:** an uncommitted concurrent-agent change adds an "Ask Brain" right-side drawer (`bare` mode on `note-chat`) + collapses semantic suggestions in `connections`. **This redesign ABSORBS and canonicalizes that drawer** — it does not replace or discard it.

---

## 1. Why (the problem)

The single surface that serves as both the **idle note editor** and the **live recording companion** is incoherent. Three visible user complaints, plus one composition problem, trace to a small number of roots:

### 1a. One CSS root cause = four broken overlays (CONFIRMED empirically on WebKit)

`<mur-source-picker>`'s popover, the note **selection formatting toolbar**, the in-note **Brain popover**, and the `[[` **link-picker** are all `position: fixed` elements rendered *inside their host component's subtree* and positioned with **viewport coordinates** from `getBoundingClientRect()`.

Per CSS, a `position: fixed` element anchors to the nearest ancestor that establishes a **containing block** — any ancestor with `transform`, `filter`, `backdrop-filter`, `perspective`, `will-change`, or `contain` — **not** the viewport. On every real surface such an ancestor exists:

- the frosted global `.card` (`primitives.css` → `backdrop-filter: blur(30px) saturate(1.35)`) wraps `note-chat`, `meeting-chat`, and `ask`;
- `.ask-panel` additionally has `overflow: hidden`, which *clips* the popover;
- worst: the concurrent `.note-chat-drawer` is itself `position: fixed` **and** runs an entry animation whose keyframes set `transform: translateX()` — an active transform makes it the containing block for any fixed descendant.

So the viewport-computed `left/top` resolve relative to that ancestor's box → the overlay lands offset / off-screen / clipped → **"the source picker looks dead", "the toolbar is positioned wrong"**. These are the *same bug* on different overlays.

**Evidence (live):** a `position:fixed` child at viewport `left:200` inside an ancestor offset by `margin-left:400` renders at `left:600` (`anchoredToAncestor:true`) when the ancestor has *any* of `backdrop-filter`/`transform`/`filter`/`will-change`; with none it stays at `left:200`. Measured in the app's shipping engine (Playwright + WebKit + the real stylesheet).

**Anti-fix:** subtracting the ancestor's rect in `reposition()` is a symptom patch — it re-breaks the instant an ancestor's transform changes, which the drawer's entry animation guarantees.

### 1b. Idle "Loading note…" forever (CONFIRMED)

The record screen mounts the embedded companion-note editor whenever `realtimeReactions` is on, **independent of whether a meeting exists** (`record.component.ts` → `showAssistant()` is true at stage `idle`). Idle, `meetingId` is `null` → `ensureCompanionNote()` early-returns → the editor receives `noteIdInput = null` → its `_load` effect deliberately holds `loading = true` forever for the "embedded + no id yet" case — a state assumed transient ("host will supply an id shortly") but **permanent** while idle because there is no host meeting. Embedded mode also strips title/props/chrome, so the pane reads as a purposeless, permanently-spinning box. The routed `/notes/new` create path shows the same bare `Loading note…` line (`note-editor.component.html`) during the create round-trip.

### 1c. Recording view reads as "a mess" (composition, not a single bug)

Two competing heroes: the embedded document editor (Note tab) **and** a structurally different live `@brain` thread (`MeetingConversationStore`), behind a `Note | Ask Brain` segmented control, plus a reactions rail — layered over the toolbar containing-block bug (1a), glass-on-glass stacking (`.rec-strip` and `.card` both blur), and nested scroll regions.

---

## 2. Decisions (locked)

| # | Decision | Choice | Consequence |
|---|---|---|---|
| D1 | **Mode model** | **One morphing surface** | idle → recording → note are *states* of one note surface, not separate screens/components. |
| D2 | **Idle record screen** | **"Ready to record" launch hero** | No companion editor while idle. The companion note is lazily created on Record-start; `delete_companion_note_if_empty` cleans up empties. "Loading note…" is gone. |
| D3 | **Ask Brain placement** | **Right-side drawer everywhere** | The drawer (recording + routed notes) is the single note+Brain pattern. The `Note | Ask Brain` segmented tabs are retired. Absorbs the concurrent drawer work. |
| D4 | **Live strip density** | **Keep the rich "live" strip** | Preserve the energetic feel (waveform + caption + controls). Fix *composition/hierarchy*, not richness. |
| D5 | **Source-scope UI** (resolved by research) | **Persistent inline "Sources" chip row** | Chips always visible above every Ask composer; `+ Source` opens the (portaled) popover only to *add*; `×` removes. Surface `askVault` citations on replies. |

---

## 3. The design

### 3.1 Load-bearing root fix — `mur-overlay-host` (a body-level overlay portal)

A new design-system component mounted **once at the app root**, providing a `document.body`-level outlet into which floating overlays teleport their popover + scrim so **no ancestor can ever be their containing block**.

- **Consumers (all four):** `SourcePickerComponent`, `NoteSelectionToolbarComponent`, `NoteBrainPopoverComponent`, `LinkPickerComponent`.
- **Mechanism:** on open, move the popover/scrim DOM node into the overlay host via `afterNextRender({ injector })` (zoneless, **no `setTimeout`/`rAF`**); on close and in `DestroyRef.onDestroy`, detach/return it. `reposition()` (viewport `getBoundingClientRect` → `style.left/top`) then works **unchanged** because `body` has no containing-block ancestor.
- **Styling:** the popover stays the **opaque** `.menu` (`--surface-overlay`, `backdrop-filter: none`) per **T3** — glass never stacks on glass. Tokens-only. **No new npm deps.**
- **Stacking:** the host owns a single, documented z-index band above app chrome; supports multiple concurrent overlays without leaks (teardown on destroy).

> This one change fixes the source-picker AND the recording formatting toolbar AND the Brain popover AND the link-picker on **every** surface, and can never re-break when an ancestor gains a transform/filter. It is verified **first**.

### 3.2 The morphing surface — states, not screens

A single note surface expresses three states. State morphs the **chrome around** the document; the editable document itself is the persistent hero and is never swapped out.

```
IDLE (record screen, no meeting)      RECORDING / NOTE
┌───────────────────────────┐         ┌──────────────────────────────────────┐
│  ▶  Ready to record        │         │ ● 00:14  ~~live caption~~     [⏹ Stop] │ ← rich status strip
│      ⌘R                    │   ──▶   ├───────────────────────────┬──────────┤
│  device ok · vault ok      │         │  Your note                │  Ask     │
│  (launch hero — no editor) │         │  (one growing document,   │  Brain   │ ← one hero:
└───────────────────────────┘         │   the real embedded       │  drawer  │   doc + Brain drawer
                                       │   editor)                 │          │
                                       └───────────────────────────┴──────────┘
```

- **IDLE (D2):** the record screen shows a **launch hero** — recording/device/vault status + a primary Record affordance (⌘R). No embedded companion editor is mounted (root fix for 1b: don't show a companion editor when there's nothing to be a companion to). Gate on `store.meetingId()` — do not mount the note-hosting path / do not default to the note tab when there is no meeting.
- **RECORDING / NOTE:** the embedded editor is the single hero — **one growing document** (AI "Add to note" appends into the body, styled to show provenance, per Granola/Notion). The **Ask Brain drawer** docks on the right and *shrinks* the document column (never covers it — the existing flex-row behavior). The `Note | Ask Brain` segmented tabs are retired (D3). The live `.rec-strip` sits above as **rich status chrome** (D4).
- **ROUTED note (`/notes/:id`, `/notes/new`):** identical editor with full chrome (title, properties, backlinks) + the same Ask Brain drawer. The `/notes/new` create renders the editor **optimistically with a placeholder** (`Type to start writing · / for blocks · Record to capture a meeting`) while `create` resolves underneath — **never a bare spinner** (root fix for the routed half of 1b; stale-while-revalidate per rule §8).

### 3.3 Ask Brain drawer = the one note+Brain pattern (D3)

- The right drawer hosts `NoteChatComponent` in `bare` mode (the concurrent work), on **both** the recording surface and routed notes.
- The recording surface's `MeetingConversationStore` live `@brain` thread is **consolidated toward the drawer + the companion note**: Q&A lives in the drawer (source-scoped `askVault`); useful answers are promotable into the document ("Add to note"). Proactive/live Brain reactions (hint cards, whisper cards) remain **ambient and subordinate** — not a competing hero.
- The strip's **"Ask" button toggles the Ask Brain drawer** (D4 consistency), replacing the separate voice-thread entry point.

> **Open trace before final wiring (from research):** confirm whether the live `@brain` thread (`MeetingConversationStore`) holds any persistence/behavior the companion note + drawer lack, before retiring it. Called out in §7.

### 3.4 Source scope = persistent inline chips + citations (D5)

- Above **every** Ask composer (note-chat, meeting-chat, ask), render `sources()` as an **always-visible chip row** ("Grounded in: [This note ×] [Design sync ×] [+ Source]"). Lift the chips **out** of the source-picker's `open()` gate — today they only show while the popover is engaged, which is the legibility bug.
- `+ Source` opens the (now-portaled, correctly-positioned) popover **only to add**; each chip's `×` removes. Pre-fill remains the note + its active links (`SourceScopeService.defaultSources`).
- On each grounded reply, **surface the sources it cited** (`askVault` already returns citations), closing the "did it actually scope?" loop.

### 3.5 Glass discipline + formatting-bubble placement

- **Opaque hero card:** give the meeting-conversation / note-hosting card an **opaque, filter-free** variant (a token) so glass is strictly *chrome*, never a content wrapper hosting fixed overlays (T3/T4). Removes glass-on-glass; part of the durable toolbar fix.
- **Rich-but-coherent strip (D4):** keep waveform + live-caption + controls; fix hierarchy and spacing (resolve the 16-vs-28-bar overflow), tidy the "your side · full transcript after Stop" honesty chip. No stripping of the live energy.
- **Formatting bubble rules:** with overlays portaled to body, the selection bubble must **clamp horizontally to the shrunk document column** (not `window.innerWidth`) so it never straddles/hides under the open drawer; **hide** while the drawer/popover holds focus (a `shouldShow`-style guard, no two floating surfaces stacked); z-index below the drawer, above the doc.

---

## 4. What changes where (implementation surface)

| Area | Change |
|---|---|
| `src/app/design-system/overlay-host/` (NEW) | The body-level overlay outlet + a small directive/service consumers teleport into. Mounted at app root. |
| `source-picker.component.ts/.scss` | Teleport popover+scrim to the overlay host on open; detach on close/destroy. **Lift chips into a persistent inline row** (or expose them so consumers render them always-visible). |
| `note-selection-toolbar`, `note-brain-popover`, `link-picker` | Same teleport-to-overlay-host treatment. Selection bubble: clamp to doc column + hide-when-drawer-focused. |
| `record.component.ts/.html/.scss` | Idle **launch hero**; gate the companion-editor mount on `meetingId()`; strip **"Ask" toggles the drawer**; tidy the rich strip (hierarchy/spacing). |
| `meeting-conversation.component.*` + `meeting-conversation.store.ts` | Retire `Note | Ask Brain` segmented tabs → embedded editor hero + Ask Brain drawer; **opaque** hero card; consolidate the live thread toward drawer+companion note (pending §7 trace). |
| `note-editor.component.ts/.html` | Split `loading` (in-flight fetch only) from the idle/empty state; add an `idleNoMeeting`-style computed; **optimistic create + placeholder** for `/notes/new`; adopt the drawer as the routed note+Brain pattern (absorb concurrent work). |
| `note-chat.component.*`, `meeting-chat.component.*`, `ask.component.*` | Persistent inline **Sources** chip row above the composer; render reply **citations**. `.ask-panel` overflow no longer clips (popover is portaled). |
| `src/design-tokens/*.css` | Add the opaque filter-free surface variant + any drawer/overlay-band tokens (with light-theme overrides). |

---

## 5. Phasing (each phase independently verifiable; RED→GREEN)

Sequenced **portal-first** so the load-bearing fix lands and is verified before the larger recompose. If scope must shrink, stopping after Phase 2 already resolves the two "feels broken" bugs (this is the "minimal-coherent" fallback, which is literally the first phases of this plan).

1. **Portal** — `mur-overlay-host` + move all four overlays into it. **RED:** picker/toolbar offset inside a transformed drawer on real WebKit. **GREEN:** lands on-anchor everywhere.
2. **Idle** — launch hero on the record screen (no companion editor when `meetingId` null) + optimistic `/notes/new` placeholder + split `loading`. **RED:** idle shows `Loading note…` forever / null-id spinner. **GREEN:** hero / placeholder, never a permanent spinner.
3. **Drawer + recompose** — retire segmented tabs → drawer everywhere; opaque hero card; "Ask" toggles drawer; tidy rich strip. (Touches the record surface's Stop→flush→delete-if-empty path — re-verify the content-loss race is intact.)
4. **Source pills + citations** — persistent inline chips + reply citations across the three Ask surfaces.
5. **Consistency pass** — token/spacing/hierarchy sweep so idle/recording/routed read as one language; formatting-bubble clamp + hide-when-drawer rules.

**Verification (every phase):** `cargo test --lib` (if backend touched) + `npx ng lint` + `npx ng build`; live-reproduce against `:1420` (mocked `invoke`); **adversarial-verifier owns PASS/FAIL**; **lock-security-reviewer** on anything touching content reads / the masked-DTO audio gate (this work should not, but the reviewer confirms no regression). Real-WebKit render test for the CSP/style + overlay behavior (green `ng serve` ≠ shipped). The live strip's true behavior (mic→waveform, caption cadence, Stop→flush→delete race) needs a **signed build on a real Mac** — stated honestly, not claimed from a unit test.

---

## 6. Coordination (multiple agents in the shared tree)

- Another agent has **uncommitted work** in `note-chat` / `note-editor` / `connections` / `commands.rs` / `db.rs` (the drawer + collapsed suggestions). This redesign **absorbs** that drawer.
- **All implementation runs in an isolated git worktree** (branched from `murmur` HEAD; shared `CARGO_TARGET_DIR`, murmur-server symlink, private Playwright port per the repo recipe) so the shared working tree is never stomped ([[shared-workdir-commit-on-wrong-branch]], [[worktree-collision-mid-verification]]).
- Land via **PR → `murmur`** (direct trunk push is blocked). Author `QueaT <kgm004a@gmail.com>`, no Claude trailers.
- Before merge: rebase onto latest trunk and **re-run the gate on the merged result** (merge-skew guard) — especially if the concurrent drawer work merges first.

---

## 7. Open questions / risks

1. **Live-thread consolidation:** does `MeetingConversationStore`'s live `@brain` thread carry persistence/behavior the companion note + drawer don't? Trace before retiring it (Phase 3). If yes, keep it as an ambient reactions feed, not a hero.
2. **Proactive reactions home:** where do hint/whisper cards live in the drawer-everywhere model — inside the drawer, or as ambient cards over the doc? Kept subordinate either way; final placement decided during Phase 3 with a live look.
3. **Overlay-host stacking:** multiple concurrent overlays (e.g. link-picker open while a bubble exists) must not fight for z-index or leak on destroy — covered by the host's single band + `DestroyRef` teardown; verify.
4. **Real-Mac-only truths:** waveform/caption perceptual density and the Stop→flush→delete-if-empty race are not headless-verifiable — signed build required.

---

## 8. Constraints honored

Local-first (pure client UI over the existing `askVault` seam — no new egress; scoping *strengthens* privacy legibility) · Obsidian-owned `.md` (the "one growing document" **is** the vault note; "Add to note" appends into the real body) · SQLite canonical (transcript/note/AI output are three views of one store — resist a separate persisted live artifact) · Liquid Glass / tokens-only, T3 opaque overlays, T4 CSP untouched · provider seam + redaction firewall untouched · **no new npm deps** · zoneless signals, `@if`/`@for`, `afterNextRender`, no `setTimeout` in components.

---

## 9. References (research, 2026-07-18)

Granola (transcript = toggle, notepad = hero; gray-AI/black-user in one doc; ephemeral-chat is the anti-pattern) · Notion AI Meeting Notes (Notes/Transcript tabs in-doc, idle = prep + Start, in-place summary + clickable transcript citations) · Obsidian meeting/voice plugins (record → one `.md`, live-transcript sidebar) · Cursor / Perplexity / ChatGPT & Claude Projects (persistent context pills / Sources tab = scope legibility) · Tiptap BubbleMenu / Lexical (floating bubble, `shouldShow`, no persistent toolbar) · Notion "blank page" (placeholder-as-affordance, never a spinner). Full source list in the research brief (workflow `w6aqqcezj` + follow-up research agent).
