<!-- Note-detail redesign spec — research+design workflow, 2026-07-04. Approved for build. -->

I have everything I need. Delivering the design doc now.

---

# Murmur — Note-Detail Redesign

**A premium, tabbed, notes-first detail page.** FE-only, token-respecting, budget-safe.
Product-owner brief honored literally: real **tab sections** (Note · Audio · Share), an audio player + speaker timeline that live **only** in their own tab, and a **password-first** Share flow with full link management — all calm, clean, and built on Murmur's existing tokens.

---

## 0. TL;DR (the decisions)

1. **Four regions, not one scroll.** A fixed **identity header** (title · status · date · duration · folder · tags) sits *above* a real tab bar. Below it, one of three panels renders: **Note** (default), **Audio**, **Share**. The old "Note | Share" segmented control graduates into these full-page tabs.
2. **The player + speaker timeline + transcript move entirely into the Audio tab.** They stop cluttering the note. A single **compact sticky mini-player** is the *only* audio chrome that can appear outside the Audio tab (and only while something is playing).
3. **The ~12-button pile is demoted.** Two first-class buttons stay visible on the Note tab (**Re-summarize**, and a **⋯ More** overflow menu). Everything else — Rename, Move, Delete, Copy/Save Markdown, Save audio, Save PDF, Export Canvas, Link people — lives inside **⋯ More** grouped by intent. **Share** is promoted out of the pile to its own tab.
4. **Share is rebuilt as a 3-state flow** — **Configure → Created → Manage** — with **password set FIRST**, then expiry, then optional max-opens, then **Create link**. The created link is a **one-time reveal** (honest: the key lives only in the URL `#fragment`, un-recopyable). The Manage list shows every active link by metadata with revoke, and a precondition gate when not signed-in / no server.
5. **`detail.component.ts` (4,620 lines, ~28 kB CSS — 177 % over budget) is split** into `app-detail-tabs`, `app-note-panel`, `app-audio-panel`, `app-share-panel` (+ existing `meeting-timeline`, `meeting-chat`, `meeting-recipes`, `related-meetings`, `share-verify-sheet`). Shared chrome (tab bar, section headers, action menu) moves to `src/styles.css` as new primitives.

---

## 1. Information Architecture

### 1.1 The regions

```
┌─────────────────────────────────────────────────────────────────┐
│  ← Meetings                                                       │  ← back link (persists)
│                                                                   │
│  IDENTITY HEADER  (always visible, above the tabs)                │
│    Title (inline-rename) · ● Summarized · Jul 3 · 42:17 · 📁 Work │
│    #tag  #tag  [+ Add tag]                                        │
│                                                                   │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │  ● Note      ○ Audio      ○ Share                          │  │  ← TAB BAR (the seg control, page-scale)
│  └───────────────────────────────────────────────────────────┘  │
│                                                                   │
│  ░░░░░░░░░░░░░░░░  ACTIVE PANEL  ░░░░░░░░░░░░░░░░                   │  ← Note | Audio | Share
│                                                                   │
└─────────────────────────────────────────────────────────────────┘
     ▸ optional: sticky mini-player docks here while audio plays
```

**Why the header stays above the tabs.** Title / status / date / duration / folder / tags are *identity*, not *section content* — they answer "which meeting am I looking at" regardless of which tab is open (Otter, Fathom, Fellow, Notion all keep meeting identity persistent above the tab switch). Rename swaps the `<h2>` in place, exactly as today. Tags edit in place, exactly as today.

### 1.2 The tab set — proposed and justified

| Tab | Contains | Why it earns a tab |
|---|---|---|
| **Note** (default) | The summary/note body + provenance badge, Action items, Recipes, Chat/Ask, Related, and a **⋯ More** actions menu. | The industry converged on notes-first: the summary is the product. It must be the *first* thing you see, not buried under a player. |
| **Audio** | Compact player → speaker timeline → turn-grouped transcript. The three views that **share one clock**, finally adjacent. | Player + timeline + transcript are *one recording surface*. Today Analysis + Chat are wedged *between* timeline and transcript — divorcing three synced views. A tab reunites them and removes the always-on player from every view. |
| **Share** | Configure → Created → Manage flow + precondition gate. | Share is first-class in every reference app (a primary button/panel). It has real internal state and deserves its own surface, not a cramped sub-panel. |

**Rejected: a 4th "Transcript" tab.** Otter/Fireflies split Summary vs Transcript because transcript is their bulk. Murmur's transcript is tightly coupled to the *player + timeline* (shared playhead, click-to-seek). Splitting them re-creates today's disconnection. Keep all three under **Audio**.

**Where the ~12 actions go:**

| Action | New home |
|---|---|
| Re-summarize | **Note tab — primary button** (kept visible; it's the one high-intent verb). |
| Share (link/person) | **Its own Share tab** (promoted). |
| Rename, Move to folder, Delete | **⋯ More → "Manage"** group (Delete is `.btn-danger`, isolated at the bottom). |
| Copy Markdown, Save Markdown, Save as PDF, Export Canvas, Save audio | **⋯ More → "Export & save"** group. |
| Export master (mic/system) | **⋯ More → "Hi-res masters"** group (shown only when `keepsMasters() && audioSrc()`). |
| Link people & projects | **⋯ More → "Graph"** group. |

### 1.3 Wireframe — the whole page (Note tab active)

```
← Meetings

╭──────────────────────────────────────────────────────────────╮
│  Roadmap sync — Q3 planning                          [ ⋯ More ]│   ← title + overflow trigger (top-right)
│  ● Summarized  ·  Jul 3, 2026  ·  42:17  ·  📁 Work  ·  ✨ Enhanced │
│  #roadmap   #q3   #planning   ⌈ + Add tag ⌋                    │
╰──────────────────────────────────────────────────────────────╯

  ┌─ Note ──┬── Audio ──┬── Share ──┐
  │  ●●●●●  │           │           │        ← active tab underlined w/ accent
  └─────────┴───────────┴───────────┘

  ┌──────────────────────────────────────────────────────────┐
  │  [ Re-summarize ]                              generated by │  ← primary verb + tiny provenance line
  │                                                claude-opus-4-8│
  │                                                            │
  │  ## Summary                                                │
  │  The team aligned on shipping the sharing MVP before …     │   ← the NOTE BODY, first thing you read
  │                                                            │
  │  ## Decisions                                              │
  │  • Ship password-first share flow                          │
  │  • Defer per-folder model routing                          │
  │                                                            │
  │  ## Action items                                           │
  │  ☑ Draft the share panel states       — you                │
  │  ☐ Wire revoke_share to the list      — Kuba               │
  └──────────────────────────────────────────────────────────┘

  ┌── Recipes ──────────────────────────────────────────────┐   (collapsed by default)
  ┌── 🧠 Ask this meeting ──────────────────────────────────┐   (meeting-chat)
  ┌── Related meetings ─────────────────────────────────────┐

           ▸ [⋯ More] opens an OPAQUE overlay menu:
             ┌────────────────────────────┐
             │  MANAGE                     │
             │   Rename                    │
             │   Move to folder      →     │
             │  ─────────────────────────  │
             │  EXPORT & SAVE              │
             │   Copy Markdown             │
             │   Save Markdown…            │
             │   Save as PDF               │
             │   Export Canvas             │
             │   Save audio…               │
             │  ─────────────────────────  │
             │  GRAPH                      │
             │   Link people & projects    │
             │  ─────────────────────────  │
             │   Delete note      (danger) │
             └────────────────────────────┘
```

**Locked meeting:** when `locked()`, the header shows `🔒 Locked`, no tags, and the tab bar is **replaced** by a single lock-gate card (Unlock with Touch ID) — exactly today's masking, just centered where the tabs would be. No tab is reachable until unlocked (the backend already masks note/segments/audio/timeline).

---

## 2. Share tab — the full flow

Backend seam is already built (`share_note_to_link`, `list_my_shares`, `revoke_share`, `account_status`, `consent_to_share_egress`). The redesign is **FE state only**.

### 2.1 State machine

```
                         account_status()
                              │
        ┌─────────────────────┼──────────────────────┐
        ▼                     ▼                      ▼
   not signed in /       signed in +            (a link exists
   no server              ready                  this session)
        │                     │                      │
        ▼                     ▼                      ▼
   ┌─────────┐          ┌───────────┐          ┌───────────┐
   │  GATE   │          │ CONFIGURE │──Create──▶│  CREATED  │
   └─────────┘          └───────────┘          └───────────┘
                              ▲                      │
                              │      "Create another"│
                              └──────────────────────┘
                        (both always feed) ▼
                        ┌──────────────────────────────┐
                        │        MANAGE (list)         │  ← always visible below
                        └──────────────────────────────┘
```

Configure and Manage coexist on the page (Manage is the list under the create panel). Created is a transient success state that replaces the Configure panel until dismissed.

### 2.2 Precondition GATE (not signed-in / no server)

Gate the *entire* Share tab (and "Share with a person") on `account_status()`. Fail closed, be honest about why.

```
  ┌── Note ──┬── Audio ──┬─● Share ─┐

  ┌──────────────────────────────────────────────────────────┐
  │                        🔗                                  │   ← .empty-state / .empty-mark
  │              Sharing isn't set up yet                      │
  │                                                            │
  │   Murmur can share this note as an end-to-end encrypted    │
  │   link. It uploads only the ENCRYPTED note — the           │
  │   decryption key never leaves your Mac.                    │
  │                                                            │
  │   • You're not signed in to a sharing account.             │   ← show only the failing preconditions
  │   • No sharing server is configured.                       │
  │                                                            │
  │              [  Set up sharing  ]                          │   → routes to Settings › Sharing
  └──────────────────────────────────────────────────────────┘
```

Precondition logic (drives which line shows and whether the button is enabled):
- `!serverConfigured` → "No sharing server is configured." → CTA **Configure server** (Settings).
- `!loggedIn` → "You're not signed in." → CTA **Sign in**.
- `loggedIn && !unlockedForSharing` → inline **Unlock for sharing** (Touch ID) before Configure renders.
- `!shareConsented` → not a gate; surfaced as a one-time consent step *inside* Create (see 2.3).

### 2.3 CONFIGURE — password FIRST, then generate

Honors the brief literally. The button reads **Create link** (you can't "copy" what doesn't exist — the ciphertext is sealed *with* the password before upload, so protection cannot be added after the fact; it would be a new link).

```
  ┌──────────────────────────────────────────────────────────┐
  │  Create a share link                                       │
  │                                                            │
  │  Password                                                  │   ← FIRST field, focused
  │  ┌────────────────────────────────────────────┐  ┌─────┐   │
  │  │ ••••••••••                                  │  │ 👁  │   │   show/hide toggle
  │  └────────────────────────────────────────────┘  └─────┘   │
  │  ▁▁▃▃▅▅ Strong     Strengthens the encryption, not just a  │   ← honest: pw hardens the key (KEK_link=H(pw))
  │                    gate. Share it out-of-band.             │
  │        ☐ No password  (link key alone protects it)        │   ← explicit opt-out, unchecked by default
  │                                                            │
  │  Expires                                                   │
  │  ┌ Never ┬ 1 day ┬ 7 days ┬ 30 days ┐                     │   ← .seg segmented control, default 7 days
  │  │       │       │ ●●●●● │        │                        │
  │  └───────┴───────┴───────┴────────┘                        │
  │                                                            │
  │  Open limit                                                │
  │  ☐ Limit the number of opens                              │   ← checkbox; when on, a small stepper appears
  │       └─▶ [ − ]  5  [ + ]  opens                           │
  │                                                            │
  │  ─────────────────────────────────────────────────────────│
  │  ⓘ Uploads the ENCRYPTED note to share.murmur.io.          │   ← one-time consent line iff !shareConsented
  │     The key stays on your Mac.        [ I understand ]     │
  │                                                            │
  │                               [  Create link  ]            │   ← .btn-primary, disabled until consent
  └──────────────────────────────────────────────────────────┘
```

Field → arg mapping (exact backend contract):
- **Password** → `password?` (omit when "No password" checked). Live strength meter is cosmetic; do not block on it.
- **Expires** → `expires_days?` — `Never` = omit; `1|7|30` pass the int (backend clamps `1..365`).
- **Open limit** → `max_downloads?` — omit when unchecked; stepper value when on (backend clamps `>=1`).
- **Create** is disabled until `shareConsented` (the "I understand" flips it via `consent_to_share_egress`).

### 2.4 CREATED — one-time reveal (honest, un-recopyable)

```
  ┌──────────────────────────────────────────────────────────┐
  │  ✅ Link created                                           │
  │                                                            │
  │  ┌────────────────────────────────────────────┐  ┌──────┐ │
  │  │ https://share.murmur.io/s/af3…#L=██████████ │  │ Copy │ │   ← the ONE moment L is shown
  │  └────────────────────────────────────────────┘  └──────┘ │
  │                                                            │
  │  ⚠  This is the only time we can show this link.           │   ← the honesty centerpiece
  │     The decryption key lives in the link itself and is     │
  │     never stored. If you lose it, revoke and create a new  │
  │     one — we can't show it again.                          │
  │                                                            │
  │   🔒 Password-protected   ·   Expires in 7 days   ·   5 opens │
  │                                                            │
  │        [ Copy again ]        [ Create another ]  [ Done ]  │
  └──────────────────────────────────────────────────────────┘
```

- **Copy** copies the full URL (the fragment key included). "Copy again" works *only while this Created state is on screen* (the URL is held in a session signal, never persisted).
- After **Done** / navigating away, the URL is gone from memory → the Manage row for it has **no Copy button** (technically impossible: `insert_outbound_share` stores `share_id + mode + rev`, never `L`/URL/title).

### 2.5 MANAGE — active-links list

Always visible below Configure. Sourced from `list_my_shares()` filtered to `meetingId === this.meeting.id`.

```
  ┌── Active links for this note ──────────────────  [ ⟳ Refresh ] ┐
  │                                                                 │
  │  ● Active      Created Jul 3   ·  🔒  ·  2 / 5 opens  ·  6d left │
  │                                                    [ Revoke ]    │
  │  ─────────────────────────────────────────────────────────────  │
  │  ● Active      Created Jul 1   ·  no pw ·  1 open  ·  never      │
  │                                                    [ Revoke ]    │
  │  ─────────────────────────────────────────────────────────────  │
  │  ◐ Limit reached   Created Jun 28 · 🔒 · 5 / 5 opens · expired   │
  │                                                    [ Revoke ]    │
  │  ─────────────────────────────────────────────────────────────  │
  │  ○ Revoked     Created Jun 20                       (no actions) │
  │                                                                 │
  │  ⓘ Links can't be shown again after creation. To re-share,      │
  │     create a new link.                                          │
  └─────────────────────────────────────────────────────────────────┘

  Empty state:
  ┌─────────────────────────────────────────────────────────────────┐
  │        No active links for this note. Create one above.          │
  └─────────────────────────────────────────────────────────────────┘
```

**State pill logic** (a `computed` per row over `MyShareEntry`):

| State | Condition | Pill | Actions |
|---|---|---|---|
| **Active** | `!revoked && !expired && (max==null \|\| count<max)` | `● .is-success` | Revoke |
| **Limit reached** | `!revoked && max!=null && count>=max` | `◐ .is-warning` | Revoke |
| **Expired** | `!revoked && expiresAt < now` | `○ .is-muted` | Revoke |
| **Revoked** | `revoked` | `○ .is-muted` | (none — terminal) |
| **🔒 masked** | `locked` (sealed local meeting) | render `🔒 Locked` row, no title, no meta | Revoke still allowed |

- **Usage** — `count / max opens` when capped, else `count opens`.
- **Expiry** — `never` · `Nd left` (countdown, `computed` from `expiresAt`) · `expired`.
- **🔒 / no pw** — password state is known only for a link created *this session* (best-effort, never wrong); older links show neither claim.
- **Refresh** re-calls `list_my_shares()`. **Revoke** calls `revoke_share(shareId)` (idempotent), optimistically flips the row to Revoked.
- **No Copy button anywhere in this list** — with the explicit "can't be shown again" note. This is the single most important honesty affordance in the whole design.

### 2.6 "Share with a person" (mode B)

Lives as a second sub-section under Share (or a small **Person ↔ Link** segmented toggle at the top of the tab), gated identically on `account_status()`. Flow (email → registered-check via `RecipientPreview` → fingerprint verify on first contact / **block** on `keyChanged` → consent → result) reuses the existing `share-verify-sheet` (the floating fingerprint sheet, opaque `--surface-overlay`). Out of the redesign's core scope but the gate + placement are defined so it isn't orphaned.

---

## 3. Audio tab

One surface, three synced views, top-to-bottom. This is the consolidation the audio research calls for — no new tech, just reunion.

```
  ┌── Note ──┬─● Audio ─┬── Share ──┐

  ┌──────────────────────────────────────────────────────────┐
  │  ▶  ──────●───────────────────────────  12:04 / 42:17     │   ← COMPACT player (sticky within the tab)
  │        1.0×   ⏮ 15s   ⏭ 15s                                │   (progress bar, not a waveform — no samples kept)
  └──────────────────────────────────────────────────────────┘

  ┌── Speakers ──────────────────────────────────────────────┐
  │  Me     ▓▓░░▓▓▓░░░░▓░░▓▓░░░░░░░░░░░░░░  18:20 talk         │   ← <app-meeting-timeline> UNCHANGED, just relocated
  │  Anna   ░░▓▓░░░░▓▓▓░░▓▓░░░░▓▓░░░░░░░░░  11:05  ✎           │      (lanes · shared playhead · hover-scrub ·
  │  Sp 3   ░░░░░░░░░░░░░░░░░░░▓▓▓▓▓▓░░░░░   9:12  "Looks like  │       legend rename · voiceprint suggestion ·
  │                                          ▲ playhead  Bob?" │       topic ribbon · pin-moment)
  │  ┌ topics ┈┈┈┈┈┈┈┈┈ intro │ roadmap │ risks │ wrap ┈┈┈┈┐  │
  └──────────────────────────────────────────────────────────┘

  ┌── Transcript ─────────────────────  [ 🔍 Find ]  [ ⤓ ]────┐
  │                                                            │
  │  ▸ Me · 12:04                                              │   ← TURN-GROUPED (not one row/segment):
  │    So the plan is to ship password-first, then the manage  │      consecutive same-speaker segments fold
  │    list, and defer per-folder routing to next cycle.       │      into ONE turn block
  │                                                            │
  │  ▸ Anna · 12:31                              ◀ active turn │   ← karaoke highlight on the playing segment;
  │    Agreed — but can we get the fingerprint verify in for   │      auto-scroll to keep active turn in view
  │    first contact? That's the risky one.                    │
  │                                                            │
  │  ▸ Me · 12:48                                              │      click any turn → seek (existing handler)
  │    Yeah, and block on key-change, never click-through.     │
  └──────────────────────────────────────────────────────────┘
```

Improvements over today, all FE, no new deps:
1. **Player + timeline + transcript are adjacent** (Analysis/Chat are gone to the Note tab). The shared clock finally reads as one surface.
2. **Compact sticky player** — the progress bar + play + a 1.0× rate + ±15 s skip, pinned to the top of the tab while you scroll the transcript. Keep the progress bar; a real waveform needs amplitude samples Murmur doesn't retain — do **not** promise one.
3. **Turn-grouped transcript** — fold consecutive same-speaker `Segment`s into one turn block (the biggest perceived-quality gap vs Otter/Descript/Fathom). Reuses `speakerChip`, `isActiveSegment`, click-to-seek verbatim.
4. **Auto-scroll + karaoke highlight** — as `currentTime` advances, the active turn scrolls into view (via `afterNextRender` in the panel, never a raw `scrollIntoView` timer) and highlights. A **Find** box filters turns.
5. **Empty state** — no audio → a single `.empty-state` card ("This meeting has no recording"), not the note-cluttering empty player card of today.

`currentTime` remains one signal owned by the panel, fed to both the mini-player track and `<app-meeting-timeline [currentTime]>` — the clock stays shared, exactly as it is now.

---

## 4. Note tab

The note *is* the product — it's the first and largest thing.

```
  ┌── Analysis ──────────────────────────────────────────────┐
  │  [ Re-summarize ]                 generated by claude-opus │   ← primary verb + tiny, muted provenance
  │                                   (served: sonnet-4-6)     │
  │                                                            │
  │  ## Summary        (rendered markdown, --content-max 840px)│
  │  …                                                         │
  │  ## Decisions                                              │
  │  ## Action items    ☑ / ☐ interactive checklist            │   ← <app-meeting-actions>, inline in the body
  └──────────────────────────────────────────────────────────┘

  ┌── ⚡ Recipes ────────────────────────────────  (collapsed) ┐
  ┌── 🧠 Ask this meeting ──────────────────────────────────── ┐   ← <app-meeting-chat>
  ┌── Related ─────────────────────────────────────────────────┐   ← <app-related-meetings>

     [⋯ More]  (top-right of the header) → the grouped overlay menu (§1.3)
```

Actions grouping principle: **one primary verb visible (Re-summarize), everything else one click away in ⋯ More, grouped by intent (Manage / Export & save / Hi-res masters / Graph), destructive isolated last.** The ⋯ menu is an **opaque `--surface-overlay`** popover (never the frosted `.card` — T3), anchored top-right, `backdrop-filter: none`, `--border-strong`, `--shadow-lg`. All handlers (`resummarize`, `startRename`, `toggleMove`, `askDelete`, `copyMarkdown`, `saveMarkdown`, `saveAudio`, `saveAsPdf`, `exportCanvas`, `exportMaster`, `linkGraph`) move verbatim — the pile is *regrouped*, not rewritten.

---

## 5. Visual system (tokens only — no new palette)

The problem today is **too much depth at once**: frosted blur + heavy `--shadow-md/lg` on every card, 15 shadow/blur uses in the detail styles, over a dense flat stack. On `#07070b`, shadows barely register — depth should read through **luminance-lifted surfaces + hairline borders**, the Linear/Raycast/Craft way. The win is **subtraction**.

**Surface ladder (3 real steps, used consistently):**
- **L0 page** = `--surface-base` (`#07070b`). The tab panels breathe directly on it — *not* every block in a card.
- **L1 content card** = `--surface-raised` + `1px --border-subtle`, **no shadow**, blur only where it's genuinely floating. The note body, each Audio sub-section, each Share panel = one calm L1 card with generous internal padding (`--space-5`/`--space-6`).
- **L2 floating** = `--surface-overlay` (opaque `#1b1b24`) + `--border-strong` + `--shadow-lg`, `backdrop-filter: none`. **Only** for the ⋯ menu, move-to popover, and the share-verify sheet (T3).

**Spacing rhythm:** `--space-6` (32 px) between top-level sections, `--space-5` (24 px) inside a card, `--space-3` between rows. Fewer, quieter cards with more air beats many tight cards.

**Type hierarchy:** title `28/700`; section labels (`Analysis`, `Speakers`, `Transcript`, `Active links`) a uniform `13px/600 uppercase --text-secondary letter-spacing:.04em`; body `15/1.55 --text-primary`; meta `13 --text-muted`; times/counts `--font-mono`.

**Tab bar styling:** reuse the `.seg`/`.seg-btn` primitive at **page scale** — resting `--text-secondary`, active tab = `--accent-soft` fill + `--accent-hover` text + a 2px accent underline, `:focus-visible` → `--accent-ring`. One accent, used scarcely: the active tab, the primary buttons, links. Nothing else competes.

**State treatment:** success `--success-soft` pill (Active links), warning `--warning-soft` (Limit reached), muted (Expired/Revoked). Loading = skeleton lines in the card, not spinners over shadows. Empty = the shared `.empty-state` + `.empty-mark`.

**Destructive:** Delete and Revoke are the *only* `.btn-danger`. Delete is isolated at the very bottom of the ⋯ menu, separated by a divider, with a confirm step (existing `askDelete`). Never side-by-side with a positive verb.

**Motion:** the `rise` entrance keyframe on tab-panel switch; `--transition` (200 ms spring) on tab underline + menu open; honor `prefers-reduced-motion`.

---

## 6. Component architecture (the budget split)

`detail.component.ts` is **4,620 lines / ~28 kB inline CSS = 177 % over the 16 kB per-component error budget** (it only ships because the budget measures *minified* CSS). Any CSS the redesign adds *forces* a split. The folder already proves the pattern (`meeting-timeline` 1,632 lines, etc.).

**`app-detail.component`** (shell — orchestrator, ~keeps most state/IPC):
- Owns: `detail()` fetch, `locked()`, `renaming`/tags, `activeTab: signal<'note'|'audio'|'share'>`, `currentTime`, audio element ref, all existing IPC handlers.
- Renders: back link, identity header, tab bar, `<app-note-panel>` / `<app-audio-panel>` / `<app-share-panel>` (one `@if` per `activeTab()`), and the lock gate when `locked()`.

**`app-detail-tabs.component`** (presentational):
- `input tabs`, `input active`; `output tabChange`. Pure `.seg` render. ~60 lines, trivial CSS.

**`app-note-panel.component`:**
- `input note`, `input meetingId`, `input meeting`, `input aiProvider/aiModel/modelServed`, `input busy/renaming/editing`, `input keepsMasters/hasAudio`.
- `output resummarize`, `rename`, `move`, `delete`, `copyMd`, `saveMd`, `savePdf`, `exportCanvas`, `saveAudio`, `exportMaster`, `linkGraph`.
- Renders: primary Re-summarize + `⋯ More` menu + note body + hosts `<app-meeting-actions>`, `<app-meeting-recipes>`, `<app-meeting-chat>`, `<app-related-meetings>`. ~14 kB CSS.

**`app-audio-panel.component`:**
- `input segments`, `input timeline`, `input total`, `input hasAudio`, `input audioSrc`, `input currentTime`, `input suggestions`.
- `output seek`, `output timeupdate`, `output renameSpeaker`, `output pin`.
- Renders: compact player + `<app-meeting-timeline>` (unchanged) + turn-grouped transcript. ~12 kB CSS.

**`app-share-panel.component`:**
- `input meetingId`, `input accountStatus`, `input myShares` (or fetches internally via injected `IpcService`), `input locked`.
- Owns share sub-state: `shareStep: 'gate'|'configure'|'created'`, `sharePassword`, `expiryChoice`, `maxOpens`, session `createdUrl` signal.
- `output setupSharing`, `output changed` (re-fetch shares). Hosts `<app-share-verify-sheet>` for mode B. ~13 kB CSS.

**Shared styles that move to `src/styles.css`** (reused across panels, keeps each under budget):
- `.tabbar` scaling of `.seg` (or just document it — `.seg` already exists).
- `.panel-card` (the L1 calm card: `--surface-raised` + `--border-subtle`, no shadow) — a new primitive worth having globally.
- `.section-label` (the uniform uppercase section header) — reused in Audio, Share, Note.
- `.menu` / `.menu-group` / `.menu-item` / `.menu-item-danger` (opaque overlay action menu) — new global primitive, also useful elsewhere.
- Reuse existing `.pill`/`.count`/`.empty-state`/`.btn*` unchanged.

Each panel then ships well under 16 kB, and the shell's own CSS drops to header + tab bar + lock gate (~4 kB).

**State/inputs contract stays signal-first** (angular-zoneless): panels take `input()` signals, emit `output()`; the shell holds the writable state and IPC. No panel calls `invoke` directly except `app-share-panel`, which gets one injected `IpcService` for its self-contained share sub-flow (matching how `meeting-chat` already owns its IPC). IPC-on-input-change effects use `{ allowSignalWrites: true }` (T1); any self-referential imports use `forwardRef` (T2).

---

## 7. Build plan (phases)

**Phase 0 — Split with zero behavior change (safety first).**
Extract `app-note-panel`, `app-audio-panel`, `app-share-panel`, `app-detail-tabs` from the existing template, wiring the *current* markup + handlers verbatim behind three tabs. Add `.panel-card` / `.section-label` / `.menu` primitives to `src/styles.css`. **Gate:** `ng build` green (each component under 16 kB), `ng lint` green, Playwright smoke against `:1420` with mocked `invoke` — every existing action still fires. This alone kills the budget overage and the always-on player.

**Phase 1 — Note tab polish.**
Collapse the 12-button pile into the `⋯ More` opaque overlay menu (grouped Manage / Export / Masters / Graph, danger-isolated). Keep Re-summarize primary. Move provenance to a muted line.

**Phase 2 — Audio tab consolidation.**
Compact sticky player (add ±15 s + rate). Turn-group the transcript (fold consecutive same-speaker segments). Auto-scroll + karaoke highlight via `afterNextRender`. Find box. Timeline relocated as-is.

**Phase 3 — Share rebuild.**
Implement the Configure (password-first) → Created (one-time reveal) → Manage (state pills + revoke + refresh, no re-copy) state machine + the precondition gate over `account_status()`. Wire `share_note_to_link` / `list_my_shares` / `revoke_share` / `consent_to_share_egress`. Re-home the mode-B person flow + `share-verify-sheet`.

**Phase 4 — Visual pass + verify.**
Subtract shadows, apply the 3-step surface ladder, tighten type rhythm, `rise` on tab switch, `prefers-reduced-motion`. **Adversarial-verify** (independent): NG0600, `forwardRef` cycles, opacity-bleed on the ⋯ menu / share sheet, **no sealed-content leak** (locked meeting: tab bar → lock gate, share masked rows render `🔒` with no title), no `convertFileSrc` path handed to a locked view. Then `scripts/ci.sh`.

---

### Honesty notes for the product owner
- **The share key is genuinely un-recopyable from the list** — that's a *code* invariant (`insert_outbound_share` never stores the URL/`L`/title), not a UX choice. The design makes that constraint feel intentional and trustworthy (one-time reveal + "create a new one" affordance) rather than a limitation.
- **No waveform** in the Audio tab — Murmur doesn't retain amplitude samples; the progress bar is the honest player. Adding a real waveform is a separate, larger piece of work.
- **This is FE-only** — no backend, no lock-model, no new egress. Every backend command it touches already ships. The only new deps are zero (tokens + existing primitives + existing sub-components).

**Files referenced:** `/Users/jakubgawronski/Projects/meetnotes/src/app/features/detail/detail.component.ts` (the 4,620-line target), its siblings `meeting-timeline.component.ts` / `meeting-chat.component.ts` / `meeting-recipes.component.ts` / `related-meetings.component.ts` / `share-verify-sheet.component.ts` (the split precedent + reusable pieces), `/Users/jakubgawronski/Projects/meetnotes/src/styles.css` (tokens + `.seg`/`.card`/`.pill`/`.empty-state` primitives + where new `.panel-card`/`.section-label`/`.menu` land), and `/Users/jakubgawronski/Projects/meetnotes/src/app/core/models.ts` (`MeetingDetail`, `AccountStatus`, `MyShareEntry`, `RecipientPreview`, `SpeakerSuggestion` — the exact FE contracts the Share/Audio panels bind to).