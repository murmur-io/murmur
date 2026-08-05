<!-- Generated 2026-08-03 via a 19-agent research workflow (6 angles -> 6 adversarial
critiques -> 3 competing proposals -> 3 judges -> synthesis). External claims are
point-in-time. Every repo anchor below was re-verified by hand before publishing. -->

# Reminders UI: placement, composer, and time entry

**Date:** 2026-08-03 · **Repo root:** `/Users/jakubgawronski/Projects/meetnotes` · **Status:** decision-ready

---

## TL;DR / Verdict

**Build. Three complaints, three separable fixes, none of which needs a new dependency, a new backend command, or a new component.**

The owner's three complaints map onto three verified root causes:

| Complaint | Root cause (verified) | Fix |
|---|---|---|
| "schowane na dole" | `<app-smart-reminder-card>` is the last in-flow element of the note `<article>` (`note-editor.component.html:657-664`) and `<app-meeting-actions>` is at `note-panel.component.html:455` of 481 lines | Two template moves into the metadata band. No TS. |
| "modal biednie wygląda" | `input[type=date]` / `input[type=time]` are **absent** from the base-box selector in `primitives.css:100-102`, but the *bare* `input:hover/:focus/:disabled` rules at `:124-131` **do** match them — so they get `outline: none` plus a 3px accent focus ring on a control with no border. Plus the scrim paints `var(--surface-hover)` (a 6 % wash) where every other sheet paints `var(--scrim)`. | One selector-list line + one scrim declaration + `mur-disclosure` on Details/Sources. |
| "ciężko się zakresy czasowe ustawia" | Two `required` native controls, no presets, no default time, no readback (`reminder-composer.component.html:60-81`) | Four preset chips + a live echo line + one `mur-select` for recurrence. |

**What we are explicitly NOT building:** natural-language date parsing, the Action-items ↔ Smart-reminders merge, a custom date popover/calendar, `showPicker()`, and any new capture entry point (`/remind`, selection toolbar). Reasoning for each is in *Options and tradeoffs* and *Kill list*.

**First slice ships in ~3 hours with zero e2e edits.** The rest is two follow-on commits.

---

## What we already have

Every anchor below was re-read in the current tree today. Confidence: **high** unless marked.

### The surface being complained about

| Fact | Anchor |
|---|---|
| Smart card is the last in-flow child of the note `<article>`, gated `@if (!embedded() && !doc.locked)` | `src/app/features/notes/note-editor/note-editor.component.html:657-664` |
| `<app-connections>` (Related) sits in the metadata band immediately above the body | `note-editor.component.html:561-568` |
| Both are inside the same `@else if (note(); as doc)` scope opened at | `note-editor.component.html:269` |
| Meeting: `<app-meeting-actions>` is at line 455 of a 481-line template, after Related, the analysis block, Stage-2 and the Q&A list | `src/app/features/detail/note-panel/note-panel.component.html:455-459` |
| Meeting: `.related-primary` (Related, `[expandedByDefault]` `[prominent]`) already owns the top of the Note tab and closes at line 13 | `note-panel.component.html:6-13` |
| The smart card is nested **inside** meeting-actions and sits **outside** the `@if (items().length)` opened at `:1` — so it paints a full `.card` even with zero action items | `src/app/features/detail/meeting-actions/meeting-actions.component.html:1, :86` |
| The card renders **only** `_suggestions.slice(0, 3)` — it has never shown a single existing reminder | `smart-reminder-card.component.ts:18, :69-71` |
| Zero-state still paints a full frosted `.card` with `✦ Smart reminders` + `Don't let this slip` + "No reminder suggestions here." | `smart-reminder-card.component.html:1-5, :68-72` |
| The loading branch already gates on `rows().length === 0` — but `_suggestions` is component-local and wiped on every remount, so "Reviewing this source locally…" **does** render on every note open | `smart-reminder-card.component.html:31-32` |
| Recording surface: the card is gated `!embedded()`, so there is **no** reminder UI at all during a live recording | `note-editor.component.html:657` |

### The composer

| Fact | Anchor |
|---|---|
| Due entry = two `required` natives in a 2-col grid, no presets, no echo | `reminder-composer.component.html:60-81` |
| Scrim paints `var(--surface-hover)`; all four sibling sheets (organize-sheet, org-share-sheet, share-verify-sheet, lock-shares-dialog) paint `var(--scrim)` at line 9 of their SCSS | `reminder-composer.component.scss:20` |
| Panel is **already T3-correct**: `--surface-overlay` + `backdrop-filter: none` + `--border-strong` + `--shadow-lg` — overlay bleed is *not* the cause | `reminder-composer.component.scss:31-35` |
| `DEFAULT_DUE_OFFSET_MS = 60 * 60 * 1000` → opens on now+1h, i.e. 14:37-style times | `reminder-composer.component.ts:30, :406` |
| `date` / `time` are the only due state; `dueEpoch()` derives from them with a round-trip validation | `reminder-composer.component.ts:86-87, :465-476` |
| `setDue` and `dueEpoch` are **private**; `tsconfig.json` sets `"strictTemplates": true` → a template cannot bind them | `reminder-composer.component.ts:459, :465` |
| `purgeInvalidatedRequest()` resets exactly ten signals (title, details, date, time, repeats, repeatEvery, repeatUnit, sources, busy, error) | `reminder-composer.component.ts:242-254` |
| Render gate: `@if (service.request() && listenerState() === 'ready')` | `reminder-composer.component.html:1` |
| `OVERLAY_FOCUSABLE_SELECTOR` is built by prefixing every focusable selector with the literal `.sp-overlay` — any other teleported overlay is keyboard-invisible from inside the modal | `reminder-composer.component.ts:41-43, :300` |
| Suggestion with `suggestedDueAt === null` blanks date+time → Create silently disabled with no explanation | `reminder-composer.component.ts:418-420` |
| `valid()` checks title / dueEpoch / recurrence / source count — but **not** the backend due bounds | `reminder-composer.component.ts:126-141` |

### Backend truths that constrain the design

| Fact | Anchor |
|---|---|
| 09:00 is already the app's convention for a date without a time | `src-tauri/src/reminder_audit.rs:203` (`and_hms_opt(9, 0, 0)`) |
| Due bounds 2000-01-01 … 2200-01-01, enforced server-side only | `src-tauri/src/storage/reminder_store.rs:30-31, :1725` |
| Recurrence is scalar and CHECK-enforced: `repeat_every BETWEEN 1 AND 365`, `repeat_unit IN ('days','weeks','months','years')`, `(repeat_every IS NULL) = (repeat_unit IS NULL)` | `reminder_store.rs:154-158` |
| A scheduler **does** exist (15 s tick) — one research brief was wrong about this | `src-tauri/src/lib.rs:605` (`spawn_reminder_scheduler`) |
| No OS notification path anywhere: `grep -rin notification src-tauri/Cargo.toml package.json` → 0 hits | verified |
| Reminders are invisible to two of three consumption surfaces: `grep -rli reminder src-tauri/src/export/ src-tauri/src/mcp.rs` → 0 files, while `get_open_commitments` **is** an MCP tool | verified |
| `minimumSystemVersion: 13.4` | `src-tauri/tauri.conf.json:72` |
| `color-scheme: dark` is **already** on `:root` in the pre-paint inline block — do not add it | `index.html:97` (light overrides at `:106`, `:118-121`) |

### Design system and the merge gate

- **28** `mur-*` selectors across `src/app/design-system/`. **No date or time primitive exists.** `mur-input` restricts `type` to text/password/search/url/email, so date/time cannot move into the catalog without extending it.
- `mur-disclosure` exists: `selector: "mur-disclosure"`, `open = model(false)`, `panelLabel = input<string|null>(null)` (`disclosure/disclosure.component.ts:32, :39, :42`).
- `mur-select`, `mur-segmented`, `mur-toggle`, `mur-row-menu` all exist. `mur-toggle` is **CVA-only** (no `checked` input) — binding it needs `FormsModule` + `[ngModel]`.
- `e2e/reminders/reminders.spec.ts` is **2430 lines** and a hard merge gate via `scripts/ci.sh`. Verified bindings that constrain any change:
  - `getByLabel("Date")` / `getByLabel("Time")` — `:2169-2170`, `:2327-2330`
  - `getByLabel("Repeat")` / `"Repeat every"` / `"Repeat unit"` — `:2171-2173`
  - `page.locator("app-smart-reminder-card")` — `:1107, :1530, :1640, :2306, :2360`
  - `getByRole("button", { name: "Edit" })` — `:513, :643, :1200, :1423`
  - **`:2327-2328` asserts Date and Time are `""` in suggestion mode with a null due** — this is the assertion that a "default to 09:00" fix must change.

---

## Findings per angle

### A. Why the modal looks poor — it is a missing selector, not taste (confidence: high)

`primitives.css:100-102` styles `input[type="text"|"password"|"search"|"number"|"email"|"url"], input:not([type]), select, textarea` with `height: 40px`, `padding: 0 var(--space-3)`, `border: 1px solid var(--glass-border)`, `border-radius: var(--radius-md)`, `background: var(--surface-input)`, `appearance: none`. `date` and `time` are absent, and `input:not([type])` cannot match a typed input.

But `:124-131` uses **bare element selectors** — `input::placeholder`, `input:hover`, `input:focus`, `input:disabled` — which *do* match. The two due fields therefore receive `outline: none` and a 3 px `--accent-ring` focus shadow **on a control that was never given a visible border**. That is an accessibility defect layered on top of the visual one.

The house already solved this once, locally: the note editor's front-matter date field carries an explicit `.prop-input` class defining border / `--surface-input` / radius / focus (`note-editor.component.html:405-414`, `note-editor.component.scss:513-529`) precisely because the global sheet does not cover date inputs. Fixing the selector list fixes three consumers at once — the composer, that front-matter field, and `briefs.component.html:158`'s `<input type="time">`.

**Two things not to chase** (each cost a research brief a wrong conclusion):
1. `color-scheme: dark` is already declared at `index.html:97`. The native picker chrome is already dark-correct. Adding a global rule is a no-op at best and a bar-window regression at worst (`src/styles.css:102-107` exists to guard exactly that).
2. `.repeat-number { width: 72px }` is **not** losing on specificity. Angular emulated encapsulation ships it as `.repeat-number[_ngcontent-xxx]` = (0,2,0), which beats the global `input[type="number"] { width: 100% }` = (0,1,1). It is already 72 px.

`accent-color` appears nowhere on form fields, so the WebKit picker's selected-day highlight paints the **OS** accent while Murmur ships six user-selectable accent palettes (`src/design-tokens/accents.css`). One declaration fixes the desync.

### B. Where best-in-class puts the "when" control (confidence: high on the primary sources)

The single best-documented preset arithmetic anywhere is Fastmail's snooze, and it is worth copying verbatim: *Later today* = 3 hours from the **beginning** of the current hour (10:30 → 13:00, never 13:30); *This evening* = 18:00 and the option is **disabled** after 18:00 rather than silently rolling; *Tomorrow* / *This weekend* / *Next week* = 08:00, coming Saturday, next Monday. ([fastmail.help](https://www.fastmail.help/hc/en-us/articles/360058753634-Snoozing-mail))

Apple Mail on Mac ships four absolute-time-labelled options — "Remind Me in 1 Hour" / "Tonight" (9 pm) / "Tomorrow" (8 am) / "Later" (custom sheet) — which is the macOS-native idiom to rhyme with. ([support.apple.com](https://support.apple.com/en-euro/guide/mail/mlhl96dfe8ce/mac); labels corroborated at [idownloadblog](https://www.idownloadblog.com/2022/09/20/how-to-use-remind-me-in-mail-app-iphone-mac/) — *medium* confidence, Apple's page does not enumerate the clock times.)

Two preset sets circulating in design writing are **not** first-party sourced and must not be cited as evidence: Slack's help page does not enumerate its reminder presets, and Google does not publish the clock times behind Gmail's "Later today"/"Tomorrow".

NN/g's date-input guidance splits exactly along the axis of this complaint: calendar pickers pay off for near-present dates, "typing the date … in many cases it is the most efficient one", and presets are appropriate "for a limited number of date options". A reminder horizon is by construction near-present. ([nngroup.com/articles/date-input](https://www.nngroup.com/articles/date-input/))

### C. Where the surface should live (confidence: high on the repo facts, medium on the attention proxy)

NN/g's 2018 eyetracking study (120 participants, >130 000 fixations) found 57 % of page-viewing time above the fold, **>42 % in the top 20 % of the page**, 74 % in the first two screenfuls, 81 % in three. ([nngroup.com/articles/scrolling-and-attention](https://www.nngroup.com/articles/scrolling-and-attention/)) That is an attention-distribution proxy, not a measurement of this specific decision — no published A/B compares top-vs-bottom placement for AI follow-up suggestions.

Competitor placement corroborates: Circleback renders Action Items **above** Overview and Topics inside the Notes tab (single rendered share page, *medium* confidence); Fathom keeps action items in the document flow and gives the right rail to **chat**; Granola dissolves them into the note body entirely and puts the rollup in a Recipe.

Murmur already ships the correct IA shape twice, in the same band: `Properties` (collapsible, count chip) and `<app-connections>` — which degrades to a lone quiet `+ Link` when empty and renders AI suggestions as ambient dashed chips with the documented rule "*No confidence %, no persistent Accept/Dismiss buttons — the chip IS the affordance*".

**Counter-evidence against going further and auto-injecting suggestions inline while the user writes:** Bhat, Aubin Le Quéré, Naaman & Jakesch, *Reactive Writers* (arXiv 2603.10374, 19 interviews + 1 291 co-writing sessions) found that engaging with suggestions "becomes a central activity in the writing process, taking away from more traditional processes of ideation", while writers "did not notice the AI's influence and felt in full control". Keep suggestions summoned/quiet; do not promote them into the writing flow. (confidence: high)

### D. Natural-language date parsing (confidence: high)

- **chrono-node has no Polish locale.** Verified against both the shipped `dist/cjs/locales` directory listing and the upstream README: en + fi/fr/it/ja/nl/ru/uk/vi, partial de/es/pt/sv/zh. ([github.com/wanasit/chrono](https://github.com/wanasit/chrono)) It is MIT with zero runtime deps and 2.76 MB unpacked / 1643 files ([registry.npmjs.org/chrono-node](https://registry.npmjs.org/chrono-node)) — so licensing and supply chain are not the blocker; **coverage is**.
- **Every maintained Rust alternative is English-only**: `interim` 0.2.1 (MIT, 1.19 M downloads, explicitly "English only" with a UK/US `Dialect` enum), `chrono-english`, `human-date-parser`, `two_timer`, `date_time_parser`; `timewarp` adds German. A crates.io search for Polish date parsing returns nothing relevant.
- Therefore a dependency buys **English only**, and Polish must be hand-rolled regardless — the dep's real cost is two grammars with two failure modes.
- Murmur has **no** NL date resolver today. Four ISO-only scanners exist (`reminder_audit.rs:185` `first_valid_due_at`, `summarize/action_items.rs:145` `find_date`, `commands/reminders.rs:37` `parse_iso_ymd`, `audio/wake.rs:399` `extract_due` which returns a raw marker *string*).
- Half the Polish tokenization already exists: `strip_diacritics` (`audio/wake.rs:202-217`) and a bilingual PL/EN deadline lexicon with inflected weekday genitives (`summarize/recall_net.rs:146-206`).
- **The inflection trap that makes a naive parser dangerous:** `piąt-` prefixes both `piątek` (Friday) and `piątej` (five o'clock), so `o piątej` resolves to Friday under a stem matcher. Same class for `środ-` and `czwart-`. Explicit form lists only, never stems.

### E. Two real bugs found adjacent to this work (confidence: high, both out of scope here)

1. **Voice-path date loss.** `wake.rs::extract_due` returns `Some("jutro")`; `voice_action.rs:326` forwards it; `commands/reminders.rs:60` drops it via `parse_iso_ymd` (strict `len() == 10`). So *"przypomnij mi o spotkaniu jutro"* creates an Apple Reminder with **no date**. Note the existing test `invalid_due_date_falls_back_to_name_only` must stay green — `build_reminder_script` is correct to refuse garbage; the defect is upstream, and `build_reminder_script` additionally hardcodes `set hours of theDate to 9` so it cannot carry a time.
2. **Smart suggestions are date-blind for real phrasing.** `first_valid_due_at` only matches a literal `YYYY-MM-DD` substring, so *"Janek wyśle raport w piątek"* yields `suggestedDueAt: null` — which then hits the silent-disabled-Create trap at `reminder-composer.component.ts:418-420`.

File both as their own issues with RED-before-GREEN oracles. Neither is a UI complaint.

---

## Fit with Murmur constraints

| Constraint | How the proposal satisfies it |
|---|---|
| **No new npm packages** | None proposed. `Intl.DateTimeFormat` (already used at `reminders.component.ts:35, :43`) and the built-in `Intl.RelativeTimeFormat` cover all formatting. |
| **Angular 22 zoneless / signals only** | The preset row, the active-chip marker, the echo line and the range guard are **all `computed()` over the existing `date()`/`time()` signals**. Zero new state signals in slice 1. |
| **Lock model — sealed content never leaks** | This is the load-bearing constraint. Because slice 1 adds no signal, `purgeInvalidatedRequest()` (`ts:242-254`) stays **byte-identical** and no sealed-source-derived state can outlive a purge. The render gate at `html:1`, both listener installers, the focus trap and focus-restore are untouched. The `!doc.locked` guard travels with the moved block; the meeting move stays inside note-panel so it keeps the detail view's `@else (!locked())` ancestor guard. **Route the diff past `lock-security-reviewer` anyway** — the reviewable claim is "no new signal, purge unchanged". |
| **T3 opaque overlays** | Nothing new is teleported or floated. The composer panel is already `--surface-overlay` + `backdrop-filter: none`. |
| **Design tokens only** | Every new value is a token: `--scrim`, `--accent`, `--accent-ring`, `--text-muted`, `--danger`, `--border-subtle`, `--space-2/3`, `--surface-input`, `--glass-border`, `--radius-md`. |
| **Design-system catalog before new controls** | Uses the shipped `.seg`/`.seg-btn` primitives (precedent: the analytics range picker at `weekly-digest.component.html:9-16`), `mur-disclosure`, `mur-select`, `.field-help`. No new `mur-*` component. |
| **SQLite canonical** | Zero backend change. Presets resolve to an epoch and feed the existing `dueEpoch()` → `ReminderDraft.dueAt` contract; `toDraft()` and the IPC surface are unchanged. |
| **macOS / WKWebView first** | The native inputs stay native (styled, not replaced). No `showPicker()`, no CSS anchor positioning (Safari 26+), no HTML `popover` attribute (Safari 17+) — the floor is macOS 13.4. Locale-dependent segment order (`dd.MM.yyyy` on a Polish Mac) means **never assert on rendered segment text, only on `input.value`, which is always ISO**. |

---

## Proposed design

### Wireframe — note, populated

```
┌─ Note editor ─────────────────────────────────────────────────────┐
│  Q3 planning — meeting notes                    [Edit|Preview] ⋯  │
│  ▸ Properties · 4                                                 │
│                                                                   │
│  Related · 3   [Kickoff]  [Roadmap]  [Spec.pdf]        [+ Link]   │
│  ───────────────────────────────────────────────────────────────  │
│  ✦ Smart reminders                                                │
│  Don't let this slip                              [New reminder]  │   ← moved up
│    Send Marta the deck              Fri 5 Sep, 09:00              │
│      [Edit & create]  [Dismiss]                                   │
│  ───────────────────────────────────────────────────────────────  │
│                                                                   │
│  Body textarea …                                                  │
│  ▏                                                                │
└───────────────────────────────────────────────────────────────────┘
```

### Wireframe — note, EMPTY STATE (the part that makes promotion affordable)

```
│  Related · 0                                            [+ Link]  │
│  ───────────────────────────────────────────────────────────────  │
│  Reminders                                        [New reminder]  │   ← ~28px, ONE line
│  ───────────────────────────────────────────────────────────────  │      no .card, no kicker,
│                                                                   │      no h3, no intro
│  Body textarea …                                                  │
```

No `✦ Smart reminders` kicker, no `Don't let this slip`, no "No reminder suggestions here." **and no "Reviewing this source locally…" spinner.** Above the fold, the 850 ms-debounced audit must never gate the strip — render the idle line immediately and let suggestions replace it when they arrive.

### Wireframe — meeting Note tab

```
┌─ Note │ Audio │ Share ────────────────────────────────────────────┐
│  Related   [Kickoff]  [Roadmap]  [Spec.pdf]            [+ Link]   │
│  ───────────────────────────────────────────────────────────────  │
│  Action items                        [Save to Obsidian Tasks]     │   ← @if (items().length)
│    ☐ Marta: send the deck   @marta   📅 2026-09-05    [+ Apple]   │
│    ☐ Book the pilot review           📅 —             [+ Apple]   │
│  · · · · · · · · · · · · · · · · · · · · · · · · · · · · · · · ·  │
│  Reminders                                        [New reminder]  │   ← idle: one line
│  ───────────────────────────────────────────────────────────────  │
│  ▸ Summary / analysis block                                       │
│  ▸ Live context                                                   │
│  ▸ Assistant Q&A                                                  │
│  ▸ Related by meaning              (deferred, on viewport)        │
└───────────────────────────────────────────────────────────────────┘
```

`<app-meeting-actions>` moves **as a unit**, carrying the smart card with it. That keeps them adjacent (the duplication becomes visible in one glance, which is the precondition for the owner ruling on it), keeps zero TS changes, and keeps the lock ancestor guard.

### Wireframe — the new composer

```
╔═══ scrim: var(--scrim)  ← was var(--surface-hover), a 6% invisible wash ═══╗
║   ┌──────────────── 560px · var(--surface-overlay) ────────────────────┐   ║
║   │  New reminder                                                 [×] │   ║   ← eyebrow deleted
║   │                                                                   │   ║
║   │  Title                                                            │   ║
║   │  ┌─────────────────────────────────────────────────────────────┐  │   ║
║   │  │ Send Marta the deck                                         │  │   ║
║   │  └─────────────────────────────────────────────────────────────┘  │   ║
║   │                                                                   │   ║
║   │  When                                                             │   ║
║   │  ┌────────────┬────────────┬──────────────┬────────────┐          │   ║
║   │  │Later today │  Tomorrow  │ This weekend │ Next week  │          │   ║
║   │  │   13:00    │    9:00    │   Sat 9:00   │  Mon 9:00  │          │   ║
║   │  └────────────┴────────────┴──────────────┴────────────┘          │   ║
║   │       ▲ active chip = .is-active (--accent-soft)                  │   ║
║   │  ┌──── Date ──────────────┐  ┌──── Time ─────────────┐            │   ║
║   │  │ 05.09.2026             │  │ 09:00                 │            │   ║   ← now 40px,
║   │  └────────────────────────┘  └───────────────────────┘            │   ║      bordered,
║   │  Friday, 5 September at 09:00 · in 2 days                         │   ║      accent-color
║   │  ▲ aria-live="polite" echo — the "did that take?" fix             │   ║
║   │                                                                   │   ║
║   │  Repeat   ┌───────────────────────────────────────────┐           │   ║
║   │           │ Every 2 weeks on Friday                 ▾ │           │   ║   ← one mur-select
║   │           └───────────────────────────────────────────┘           │   ║
║   │                                                                   │   ║
║   │  ▸ Details                                                        │   ║   ← mur-disclosure
║   │  ▸ Sources · 1                                                    │   ║   ← mur-disclosure
║   │                                                                   │   ║
║   │                                  [ Cancel ]  [ Create reminder ]  │   ║
║   └───────────────────────────────────────────────────────────────────┘   ║
╚═══════════════════════════════════════════════════════════════════════════╝
```

Tab order: `×` → Title → the four preset buttons → Date → Time → Repeat select → Details trigger → Sources trigger → Cancel → Create. Every new control is an ordinary `<button>`/`<select>` **inside** `#panel`, so the existing focus trap picks them up with no change to `OVERLAY_FOCUSABLE_SELECTOR`. `.seg-btn:focus-visible` already ships a `--accent-ring` shadow.

---

## The exact preset chip set and default due time

**Four chips. Fastmail's arithmetic, published rather than configured — each chip renders its resolved absolute time as a second line, so the rule is visible without a settings screen.**

| Chip | Resolves to | Disabled / hidden when |
|---|---|---|
| `Later today` | **top of the current hour + 3 h** (10:30 → 13:00, never 13:30) | hidden once that value lands ≥ 18:00 local |
| `Tomorrow` | tomorrow **09:00** | never |
| `This weekend` | the coming Saturday **09:00** (if today is Saturday → next Saturday) | never |
| `Next week` | next Monday **09:00** | never |

**Reasoning, point by point:**

- **Why 09:00 and not Fastmail's 08:00.** 09:00 is *already Murmur's own convention* — `reminder_audit.rs:203` pins every extracted suggestion date to `and_hms_opt(9, 0, 0)`. Today the manual composer contradicts its own backend by opening on `now + 1h`. Aligning them costs one constant and makes manual and Smart reminders finally agree. (Apple Reminders' documented all-day default is also 9:00 AM — *medium* confidence, the number comes from secondary sources.)
- **Why top-of-hour rounding.** Round times read as intentional; `13:37` reads as a bug. This is the one piece of Fastmail's spec that is non-obvious and worth copying literally.
- **Why hide rather than roll "Later today".** A preset that silently becomes tomorrow is the classic wrong-time bug. Hiding it is honest; the row simply has three chips in the evening.
- **Why four and not five.** A fifth `This evening · 18:00` chip (disabled after 17:00) is defensible and matches Apple Mail's "Tonight" — but five two-line chips wrap at 560 px. Recommend shipping four and adding the fifth only if the owner asks. *(Open question 4.)*
- **Why chips show their resolved time.** "This weekend" means Saturday to some people and Friday evening to others. Rendering `Sat 9:00` converts a guess into a visible convention and makes a mismatch obvious in one glance — that is the cheap 90 % of what a settings screen would buy.

**Default due on open:** the **first enabled chip's epoch**, with that chip rendered active. Before ~15:00 that is `Later today` (a round time later the same day); after that it is `Tomorrow 9:00`. Delete `DEFAULT_DUE_OFFSET_MS`. Nothing in the e2e suite asserts the create-mode default value (verified — `:2169-2170` only `fill()`s).

**Formatting rule:** never hardcode a date format string. The native inputs render segments in OS-locale order (`dd.MM.yyyy` on a Polish Mac) and the owner writes Polish. Use `Intl.DateTimeFormat(undefined, …)` for the chip labels and the echo, and assert only on `input.value` (always ISO) in tests.

---

## The recurrence UI

**One `mur-select` of pre-resolved sentences, computed against the chosen due date, replacing the checkbox + number + native `<select>` assembly.**

```
Repeat   [ Does not repeat                          ▾ ]
           ├ Does not repeat                → (null, null)
           ├ Every day                      → (1, "days")
           ├ Every week on Friday           → (1, "weeks")
           ├ Every 2 weeks on Friday        → (2, "weeks")
           ├ Every month on the 5th         → (1, "months")
           ├ Every year on 5 September      → (1, "years")
           └ Custom…                        → reveals today's number + unit controls
```

**Why a sentence and not a pill row:** the whole value of the pattern is that the rule is *readable* in the collapsed state. `mur-segmented` physically cannot carry "Every 2 weeks on Friday"; `mur-select` can, and it is one line, so it does not need a disclosure and stays visible — which matters if "zakresy" turns out to mean the interval.

**Verified safe against the engine, not just the DTO.** `advance_calendar_due_raw` (`reminder_store.rs:1746-1785`) adds `every × 7` days for `weeks` while preserving local h:m:s and resolving DST gaps/ambiguity, and `next_recurrence_after` (`:1719`) catches up missed cycles. So "Every 2 weeks on Friday" provably stays on Friday.

**Explicitly refuse — put this in a doc comment next to the option list:** `Every weekday`, `2nd Tuesday of the month`, and `Ends after N occurrences` are **not representable** in the CHECK-constrained scalar schema (`reminder_store.rs:154-158`). A UI that offers them would silently create the wrong schedule. Do not add them without an additive migration.

**Cost:** this changes the three `getByLabel("Repeat" | "Repeat every" | "Repeat unit")` bindings at `reminders.spec.ts:2171-2173`, so it ships as a **separate commit** from the zero-e2e-cost slice.

---

## Verdict on natural-language date parsing

**Do not build it now. Do not ask the owner to approve chrono-node. When it is eventually built, it goes in Rust, with no new crate.**

The approve/reject question is settled by coverage, not taste:

- chrono-node ships **no Polish locale** (verified against the shipped dist and the upstream README). Every maintained Rust crate — `interim`, `chrono-english`, `human-date-parser`, `two_timer`, `date_time_parser` — is **English-only**. So a dependency buys English only, and the owner's own language must be hand-rolled either way. The dep's real cost is *two* grammars with two failure modes plus 2.76 MB, to avoid writing ~300 lines.
- If it is built later it belongs in Rust: `chrono` with `clock` is already a dependency (`src-tauri/Cargo.toml:149`), `strip_diacritics` and the bilingual PL/EN deadline lexicon already exist, it is covered by `cargo test --lib`, and the same function would upgrade `first_valid_due_at` and the voice path.

**Why not now, even though it is the most exciting idea in the research:**

1. For the common case it buys nothing over a `Tomorrow` chip sitting directly above the field.
2. It is a silent-wrong-answer machine. "next week" (Monday vs +7), "weekend" (Friday evening vs Saturday morning), DST boundaries, and typing "5 jan" in December all resolve plausibly and wrongly.
3. **It is invisible to the Playwright harness by construction** — the e2e drives a mocked `window.__TAURI_INTERNALS__.invoke`, so the spec can only exercise a hand-written mock that will silently diverge from the real grammar. The highest-risk surface would have the weakest runtime proof.
4. No upstream, no reference corpus, and the owner as sole oracle for the Polish token table.

If it is ever built, sequence it as: pure function first (`resolve(text, now) -> {at, label} | null`) with table-driven `cargo test --lib` coverage including a DST transition, a 31-Dec rollover, the MIN/MAX bound rejections, and the `piątek`/`piątej` collision — *then* wire it to the UI behind the same echo line, which is the entire mitigation for a wrong parse.

---

## Decision on the Action-items ↔ Smart-reminders duplication

**Do not merge them in this change. Make them adjacent and legible, and put the ruling to the owner.**

The duplication is real and structural. `meeting-actions.component.html` renders (a) a card-level "Save to Obsidian Tasks" → `patchNoteTasks`, (b) a per-row "Add to Apple Reminders" → `addReminder` (osascript, fire-and-forget, no id retained), and (c) `<app-smart-reminder-card>` at `:86`, **outside** the `@if (items().length)` guard. Both (a)/(b) and (c) are built from the **same backend parser**: `reminder_audit.rs:15,61` imports and iterates `crate::summarize::action_items::parse_action_items`, which is exactly what `get_action_items` uses. Two renderers over one data source, differing invisibly — the textbook NN/g duplicate-design failure ("the redundancy makes users wonder if the links support different actions", [nngroup.com/articles/duplicate-links](https://www.nngroup.com/articles/duplicate-links/)).

**Why not resolve it here.** Reconciling `ReminderView` rows (own ids, `occurrenceId` distinct from `dueAt`, repeat rules, source anchors, `complete_reminder(id, expected_due_at)` optimistic concurrency) with free-text `- [ ]` checklist lines is an **effort-L product decision with a losing side either way**:

- Subordinate reminders to the markdown checkbox → they are capped at what a `- [ ]` line can carry, and the Obsidian Tasks emoji format is **date-only** (`📅 YYYY-MM-DD`), so a timed 17:00 reminder truncates on export.
- Keep reminders first-class → the vault/Apple paths get demoted instead, and reminders remain invisible to two of Murmur's three consumption surfaces (`grep -rli reminder src-tauri/src/export/ src-tauri/src/mcp.rs` → nothing, while `get_open_commitments` **is** an MCP tool at `mcp.rs:1642`).

Neither is a UI complaint, and both need the owner's call.

**What this change does instead, at zero cost:** the two surfaces render adjacent at the top of the Note tab instead of 400 lines apart, so the duplication is visible in one glance; and the idle strip means the meeting page stops showing a full branded card under an already-present Action items card when there is nothing to remind about.

**Honest objection to record:** hoisting both puts two follow-up cards above the fold. That is only mitigated, not eliminated, by the idle strip and by `@if (items().length)`. If it reads as heavy in practice, the fallback is to leave Action items where it is and lift **only** the smart card into note-panel (`meetingId()` and `meetingTitle()` are both already in scope there) — but that splits the two surfaces apart and makes the duplication *less* visible, so it trades the IA argument for real estate.

---

## Options and tradeoffs

| Option | Size | What it buys | What it costs |
|---|---|---|---|
| **A. Do nothing** | — | — | All three complaints stand. The loudest chrome in the app (`Don't let this slip`) keeps rendering when there is nothing to say. |
| **B. Slice 1 — the surgical fix** *(recommended)* | **S** (~3 h) | All three complaints substantially answered; zero e2e edits; zero backend; zero new signals so the lock purge is provably untouched. | Repeat row untouched in this slice; the strip still shows only suggestions, never existing reminders. |
| **C. Slice 2 — modal hierarchy + recurrence** *(recommended, separate commit)* | **S–M** (~half a day) | Default modal becomes Title + When; recurrence becomes one readable sentence; covers the second reading of "zakresy". | Changes 3 `getByLabel` bindings + possibly the suggestion-mode empty-due assertion. Needs `FormsModule` if `mur-toggle` is reached for. |
| **D. Slice 3 — show existing reminders in the strip** | **M** | Closes the deepest hole: create a reminder from a note and it currently vanishes from that note forever. | Requires a new gated `list_reminders_for_source(kind, id)` command — because `RemindersStore.refresh()` sets `rowsRequested = true` (`reminders.store.ts:151`) and never unsets it, so a client-side filter would put the whole app into permanent full inbox+upcoming+completed refetching on **every** reminder event, triggered by merely opening a note. |
| **E. The IA merge (one Follow-ups object)** | **L** | Genuinely better product; ends the duplication. | Product decision with a losing side; needs owner ruling; two writers on the same markdown lines is a content-loss shape unless `patch_note_tasks` stays the single writer. **Not now.** |
| **F. NL date parsing** | **L** | Fast entry for complex phrasing; would fix two adjacent backend bugs. | No Polish upstream anywhere; silent-wrong-answer risk; untestable through the e2e harness. **Not now.** |
| **G. Custom `mur-date-popover` / calendar grid** | **L** | App-consistent picker. | ~200 lines of APG grid + the `.sp-overlay` focus-trap surgery, replacing two native inputs that one CSS line makes look correct. No CSS anchor positioning (Safari 26+) or `popover` attribute (Safari 17+) at the 13.4 floor. **Rejected.** |

### Kill list — things that look attractive and should not be built here

- **`showPicker()`** — splits by input type at the 13.4 floor (time = Safari 16, date = 17.4), so the fallback path for the date field is *the exact control being complained about*. Two code paths, one of them the bug. It also throws `NotAllowedError` without transient user activation, making it a footgun for anyone who later calls it from an `effect`.
- **A global `color-scheme` rule** — already at `index.html:97`; adding it is a whole-app blast radius for nothing.
- **"Fixing" `.repeat-number`'s width specificity** — it already wins via `[_ngcontent]`.
- **Collapsing the /reminders row actions behind `mur-row-menu`** — reds four `getByRole("button", { name: "Edit" })` assertions because row-menu renders projected items only while open, for zero user gain.
- **Deleting / renaming / retemplating `SmartReminderCardComponent`** — burns 5 e2e locators and risks reimplementing its security ordering wrong (listener-installed-before-first-audit, `listenerReady()` on both the audit and the create entry, the deliberate refusal to trust `sourceTitle()` because listener registration has no replay, `invalidatePending()` on every identity/lock change). Move the mount; leave the component alone.
- **New capture entry points (`/remind` slash item, selection-toolbar "Remind", a global hotkey)** — the complaint is that an *existing* surface is in the wrong place, not that there are too few ways to create a reminder. Also: any future entry point **must** route through `ReminderComposerService.openCreate` (never build a `ReminderDraft` directly — the listener-ready gate and the purge live on the composer *component*) and must mirror a readiness gate or it silently no-ops at cold start.
- **Snooze in the /reminders inbox** — genuinely valuable (it is the one reschedule flow Murmur lacks, and every preset the research cites is a reschedule pattern) but it is a third surface the owner did not mention. File it as the next slice after D.
- **Resident reminder chrome during a live recording** — the editor mounts `[embedded]="true"` and the Calm-Notepad model is deliberate.
- **MCP tool / vault export for reminders** — a real architectural gap that strains the SQLite-canonical / three-thin-readers rule, but it answers no part of a UI complaint. Own decision, own gate.

---

## Recommendation and first step

Ship in three commits on one branch, in this order.

### Slice 1 — the surgical fix (S, ~3 h, **zero e2e edits**)

**1.1 — CSS root cause** · `src/app/design-system/primitives.css`
Append `input[type="date"], input[type="time"],` to the base-box selector list at `:100-102`, then add immediately after that block:
```css
input[type="date"], input[type="time"] {
  display: inline-flex;      /* WebKit temporal inputs are inline-flex; without this
                                a forced 40px height bottom-aligns the
                                ::-webkit-datetime-edit segments */
  align-items: center;
  accent-color: var(--accent);  /* the picker's selected-day highlight follows the
                                   user's palette — accents.css ships six */
}
```
Do **not** add `color-scheme`. Do **not** touch `.repeat-number`. Do **not** delete the composer's `input, textarea, select { width: 100% }` (`scss:104-108`) — it is consumed by the repeat-row overrides at `:133-144`.

**1.2 — Composer chrome** · `reminder-composer.component.{html,scss}`
`scss:20` `background: var(--surface-hover)` → `var(--scrim)`. **Keep `z-index: var(--z-overlay)` on `.composer-backdrop`** — do not copy organize-sheet's `z-index: 100`, which would drop the app-wide modal two stacking tiers below the teleported source-picker overlay. Delete `<p class="composer-eyebrow">Murmur reminders</p>` (`html:20`) and its rule (`scss:59-65`). Replace the bespoke `.field-hint` (`html:135`, `scss:55, :99-102`) with the global `class="field-help text-muted"` (`primitives.css:264`).

**1.3 — Preset resolver** · pure, exported, injected `now`
```ts
export function resolvePresets(now: Date):
  { id: PresetId; label: string; at: number; hidden: boolean }[]
```
Table-tested against a frozen clock: 10:30 → Later today = 13:00; 17:30 → Later today hidden; Saturday → This weekend = next Saturday; Sunday → Next week = tomorrow; a DST-transition day; 31 December.

**1.4 — Wire presets + echo** · `reminder-composer.component.ts`
`readonly presets = computed(...)` (marks `active` by comparing `localDateParts(at)` to `{date(), time()}`); a **public** `applyPreset(id)` that calls the private `setDue` — *required*, because `strictTemplates` forbids binding a private member and `ng build` would fail; `dueLabel` / `dueOutOfRange` / `echo` computeds; `&& !this.dueOutOfRange()` in `valid()` with the copy "Pick a date between 2000 and 2200"; default due = first enabled preset; delete `DEFAULT_DUE_OFFSET_MS`. **Add no new signal** — every addition is a `computed()` over `date()`/`time()`, so `purgeInvalidatedRequest()` stays byte-identical.

**1.5 — Composer template** · wrap the unchanged `.due-grid` in a `.when-block` with the `.seg` preset row above and `<p class="field-help when-echo" aria-live="polite">` below. The composer has `role="dialog"` but **no live region today**; this is the one to add.

**1.6 — Idle strip** · `smart-reminder-card.component.{ts,html,scss}`
`readonly hasContent = computed(() => this.error() !== null || this.rows().length > 0)`. Bind `[class.card]="hasContent()"` / `[class.is-strip]="!hasContent()"`; wrap the kicker + `<h3>` + intro in `@if (hasContent())`. **Change the loading branch from `@if (loading() && rows().length === 0)` to render nothing** — keep the idle strip visible instead, so promoting the surface does not introduce a "Reviewing this source locally…" flash on every note open. Keep `New reminder` and its `[disabled]="!listenerReady()"` in both states. Keep the error branch rendering the card shell so `reminders.spec.ts:~2163` stays green.

**1.7 — The two moves** (template only, no TS)
- `src/app/features/notes/note-editor/note-editor.component.html`: cut lines **657-664** (the whole `@if (!embedded() && !doc.locked) { <app-smart-reminder-card …/> }` block) and paste immediately after the `<app-connections>` `@if` block that closes at **:568**. `doc` is in scope there (`@else if (note(); as doc)` opens at `:269`).
- `src/app/features/detail/note-panel/note-panel.component.html`: cut lines **454-459** (`<!-- ACTION ITEMS … -->` + `<app-meeting-actions …/>`) and paste immediately after the `</section>` closing `.related-primary` at **:13**. Do **not** extract the smart card out of `meeting-actions.component.html:86` — it travels with its parent, so both component TS files stay untouched.

**1.8 — Oracles** · `e2e/reminders/reminders.spec.ts` (additive only)
- Seed a ~4000-char note body; assert `page.locator("app-smart-reminder-card").boundingBox().y` falls inside the first viewport height. **RED on today's code, GREEN after 1.7.**
- Assert that with zero suggestions and no error, `.smart-kicker` has count 0 while `New reminder` is still visible. **Do not assert the literal string `Don't let this slip`** — the template ships a **curly** apostrophe (`Don’t`, U+2019, `smart-reminder-card.component.html:5`), so a straight-quote assertion returns 0 on unchanged code and is vacuously green forever. A dead oracle is worse than no oracle.
- Open the composer; assert the echo `<p class="when-echo">` is non-empty on open and that clicking `Tomorrow` sets `getByLabel("Time")` to `09:00`.
- **webkit project**: assert `getByLabel("Date")`'s bounding height equals `getByLabel("Title")`'s. `playwright.config.ts` already declares chromium + webkit and `scripts/ci.sh` runs the suite unfiltered — this is the only mechanical proof that 1.1 landed.

### Slice 2 — modal hierarchy + recurrence (S–M, separate commit — it costs e2e edits)

- Wrap **Details** and **Sources** in `<mur-disclosure>` (Sources seeded open when `sources().length > 0`, i.e. edit mode) so the default modal is **Title + When + Repeat**. This is the half of "biednie wygląda" that reads as *sparse/cheap* rather than *unstyled*.
- Replace the checkbox + number + `<select>` with the single `mur-select` of pre-resolved recurrence sentences described above; `Custom…` reveals today's controls unchanged. Update `reminders.spec.ts:2171-2173`.
- Fix the silent-disabled trap: a suggestion arriving with `suggestedDueAt === null` (`ts:418-420`) should default to the 09:00 convention instead of blanking. **Note the cost:** this changes `reminders.spec.ts:2327-2328`, which currently asserts both fields are `""`.

### Slice 3 — show this note's existing reminders (M, needs the backend command)

Add a gated `list_reminders_for_source(kind, id)` routed through the same `visible_source_views` drop-on-sealed logic, registered in `tauri::generate_handler!` in `src-tauri/src/lib.rs` in the same change. Then render Scheduled rows in the strip. Record the honest asymmetry: `visible_source_views` **drops** a sealed source's anchor while keeping the reminder row, so a source-scoped read **fails closed** (never leaks) but **under-reports** — a reminder anchored only to a sealed meeting appears in no strip and reads as source-less in the inbox. That is the correct direction to be wrong in, but it is behaviour to document, not rediscover.

### Gates and review

`scripts/agent-config-audit --ci` → `(cd src-tauri && cargo test --lib)` → `npx ng lint` (external templates are linted; the new `aria-live` region is what templateAccessibility wants) → `npx ng build` (16 kB per-component style budget) → `npm run test:e2e`. **Route the diff past `lock-security-reviewer`** even though it looks like pure template work — the reviewable claim is "no new component signal, `purgeInvalidatedRequest()` unchanged".

### One comment to leave behind now

At `reminder-composer.component.ts:41-43`, next to `OVERLAY_FOCUSABLE_SELECTOR`, record that it is built by prefixing the literal `.sp-overlay`, so **any** future teleported overlay inside the composer is keyboard-unreachable unless it carries that class or the const is widened in the same change. Slice 1 teleports nothing, so this is free insurance against the next contributor rediscovering it as a bug.

---

## What cannot be verified without a real signed Mac / packaged WKWebView build

`ng serve` on Chromium is not proof — this is the same false-green class as the T4 CSP style-loss bug, where a green `ng build` shipped a completely unstyled app.

1. **How `input[type=date|time]` actually render inside a forced 40 px `appearance: none` box.** Specifically whether `::-webkit-datetime-edit` vertically centres with the `display: inline-flex` fix, and how the empty `required` field's locale placeholder text colours (it inherits `--text-primary`, not `--text-muted`). The Playwright webkit project is a good proxy — it is a real WebKit build and it caught the height defect in the research — but it is **not** the system WKWebView.
2. **Whether `accent-color` reaches the native picker's selected-day highlight** in the packaged build.
3. **Native segment order and the locale the WKWebView actually resolves.** A Polish Mac renders `dd.MM.yyyy`; the region setting may differ from the UI language, which affects every `Intl` string in the echo line and the chip labels.
4. **Whether the composer's date/time controls open a usable picker at all in WKWebView.** caniuse marks Safari macOS as "partial" for date/time from 14.1 through 26.x without enumerating the gap, and there is credible reporting that Safari exposes no visible picker affordance (Chrome/Edge expose `-webkit-calendar-picker-indicator`; Safari does not). If the packaged build turns out to show no affordance, the preset chips carry even more weight — and that is an argument for shipping them first regardless.
5. **The `--scrim` change's perceived weight** against the aurora/glass background in the real compositor.
6. Anything about Touch ID, lock-at-rest, ScreenCaptureKit or the biometric unlock path — untouched by this change, but stated for completeness since the composer sits adjacent to lock-invalidated state.

Everything else — preset arithmetic, the echo string, the range guard, the placement DOM order, the purge invariant — is provable headless via `cargo test --lib` and Playwright against `:1420` with a mocked `window.__TAURI_INTERNALS__.invoke`.

---

## Open questions

1. **"Zakresy czasowe" — which reading?** (a) picking a date+time is fiddly, (b) I want a start→end window, or (c) the Repeat interval is the painful part. This plan covers (a) in slice 1 and (c) in slice 2. **(b) is a schema change** — `ReminderDraft` carries a single scalar `dueAt`, there is no start/end — and is out of scope for both.
2. **Which follow-up system is canonical** — the note's `- [ ]` markdown line, or the native Murmur reminder? Nothing should be made faster to reach until this is answered; making one path one-keystroke fast without demoting the others multiplies the confusion.
3. **Is hoisting Action items + Smart reminders together acceptable**, or should Action items stay at the bottom and only the reminder surface move up? Splitting them is cheap but makes the duplication less visible.
4. **A fifth chip, `This evening · 18:00`** (disabled after 17:00, matching Apple Mail's "Tonight")? Costs a wrap at 560 px.
5. **Default due on open** — first-enabled-preset (round time, active chip visible, changes through the day) or a fixed `Tomorrow 09:00` (maximally predictable)?
6. **OS notifications.** The 15 s scheduler exists but nothing fires while Murmur is closed, so the sidebar count is the only nudge. A notification plugin is a **new dependency requiring explicit approval**. Until then, no UI copy may promise an interruption — note that the string currently being promoted above the fold is literally *"Don't let this slip"*, which the system cannot deliver. Deleting it from the idle state is a correctness fix; whether it survives the populated state is worth deciding too.
7. **Should reminders reach the vault and MCP?** They are currently the one follow-up object invisible to two of Murmur's three consumption surfaces. Own decision, own PR.

---

## Sources

**Primary, first-party, re-fetched during the critique round**
- [Fastmail — Snoozing mail](https://www.fastmail.help/hc/en-us/articles/360058753634-Snoozing-mail) — the preset arithmetic copied verbatim (Later today = +3 h from the top of the hour; This evening = 18:00, disabled after 18:00; Tomorrow / This weekend / Next week; "Last custom").
- [Todoist — Introduction to dates and time](https://www.todoist.com/help/articles/introduction-to-dates-and-time-q7VobO) — the accepted NL vocabulary, the "Interpret 'Next Week' As" / "Interpret 'Weekend' As" settings, and the time-of-day defaults (morning 9am, afternoon 12pm, evening 7pm).
- [Todoist — Smart date recognition](https://www.todoist.com/help/articles/turn-smart-date-recognition-on-or-off-63WfIr) — the reversible-highlight affordance ("Click the highlighted word … or press Delete or Backspace") if inline parsing is ever built.
- [Things — Using dates and times](https://culturedcode.com/things/support/articles/9780167/) — the prefix grammar (`tod`, `2p`, `17d`, `10-3 9`) and the supported-language list (no Polish).
- [Apple Mail on Mac — Remind Me](https://support.apple.com/en-euro/guide/mail/mlhl96dfe8ce/mac) · labels corroborated at [iDownloadBlog](https://www.idownloadblog.com/2022/09/20/how-to-use-remind-me-in-mail-app-iphone-mac/) *(medium)*.
- [Notion — Reminders](https://www.notion.com/help/reminders) — `@remind` inline tag, red when overdue, sidebar Inbox badge. Note its payload: desktop push + mobile push + email, none of which Murmur has.
- [Obsidian Tasks — Emoji format](https://publish.obsidian.md/tasks/Reference/Task+Formats/Tasks+Emoji+Format) — signifiers are **date-only**, which is why a timed reminder truncates on export.
- [NN/g — Date-input form fields](https://www.nngroup.com/articles/date-input/) · [Scrolling and attention](https://www.nngroup.com/articles/scrolling-and-attention/) · [Duplicate links](https://www.nngroup.com/articles/duplicate-links/) · [Progressive disclosure](https://www.nngroup.com/articles/progressive-disclosure/) · [Empty states](https://www.nngroup.com/articles/empty-state-interface-design/).
- [Bhat et al., *Reactive Writers*, arXiv 2603.10374](https://arxiv.org/abs/2603.10374) — the argument against auto-surfacing suggestions inline while writing.
- [chrono-node](https://github.com/wanasit/chrono) + [registry.npmjs.org/chrono-node](https://registry.npmjs.org/chrono-node) — locale list (no Polish), MIT, zero deps, 2.76 MB / 1643 files.
- [interim](https://docs.rs/interim/latest/interim/), [chrono-english](https://crates.io/api/v1/crates/chrono-english), [human-date-parser](https://crates.io/api/v1/crates/human-date-parser), [two_timer](https://crates.io/api/v1/crates/two_timer) — all English-only.
- [caniuse — input-datetime](https://caniuse.com/input-datetime) · [caniuse — showPicker on date](https://caniuse.com/mdn-api_htmlinputelement_showpicker_date_input) · [MDN — showPicker](https://developer.mozilla.org/en-US/docs/Web/API/HTMLInputElement/showPicker) · [caniuse — CSS anchor positioning](https://caniuse.com/css-anchor-positioning).
- [W3C WAI-ARIA APG — Date Picker Dialog](https://www.w3.org/WAI/ARIA/apg/patterns/dialog-modal/examples/datepicker-dialog/) — the full contract, if a custom grid is ever built.

**Repo anchors (all re-verified 2026-08-03; repo root `/Users/jakubgawronski/Projects/meetnotes`)**
- `src/app/features/notes/note-editor/note-editor.component.html:269, :561-568, :657-664`
- `src/app/features/detail/note-panel/note-panel.component.html:6-13, :454-459`
- `src/app/features/detail/meeting-actions/meeting-actions.component.html:1, :86`
- `src/app/features/reminders/smart-reminder-card/smart-reminder-card.component.{html:1-5,:31-32,:68-72 | ts:18,:61,:69-71,:88,:124,:163,:180,:244}`
- `src/app/features/reminders/reminder-composer/reminder-composer.component.{html:1,:20,:60-118,:135 | ts:30,:41-43,:86-87,:101,:126-141,:242-254,:300,:406,:418-423,:459,:465 | scss:20,:31-35,:53-65,:99-108,:114,:137-144}`
- `src/app/features/reminders/reminders.store.ts:151`
- `src/app/design-system/primitives.css:100-102, :124-131, :262-268, :340-366, :400-412`
- `src/app/design-system/disclosure/disclosure.component.ts:32,:39,:42` · `select/`, `segmented/`, `toggle/`, `row-menu/`, `source-picker/` (28 `mur-*` selectors; no date/time primitive)
- `index.html:97, :106, :118-121` · `src/styles.css:102-107` · `src/design-tokens/accents.css`
- `src-tauri/src/reminder_audit.rs:15, :61, :185-215, :203`
- `src-tauri/src/storage/reminder_store.rs:30-31, :150-160, :1719, :1725, :1746-1785`
- `src-tauri/src/commands/reminders.rs:37-50, :58-70, :115, :3189-3228` · `src-tauri/src/lib.rs:605` · `src-tauri/src/mcp.rs:1642`
- `src-tauri/src/summarize/action_items.rs:142-158` · `src-tauri/src/audio/wake.rs:202-217, :399-430` · `src-tauri/src/summarize/recall_net.rs:146-206`
- `src-tauri/Cargo.toml:149` · `src-tauri/tauri.conf.json:72` · `tsconfig.json:5, :27`
- `e2e/reminders/reminders.spec.ts` (2430 lines): `:513, :643, :1107, :1200, :1423, :1530, :1640, :2169-2173, :2306, :2327-2330, :2360` · `playwright.config.ts:48-49` · `scripts/ci.sh:285-316`


---

## Addendum — evidence gathered outside the agent fan-out

Captured by driving the real Angular app under the Playwright FE harness with a mocked
`window.__TAURI_INTERNALS__.invoke` (`scripts/screenshots/mock-tauri.js`), 1440x950, dark theme.

### A1. The live-recording surface has no reminder affordance at all

The smart card is gated `@if (!embedded() && !doc.locked)`
(`note-editor.component.html:658`), and the recording companion note renders the editor in
`embedded` mode. Confirmed visually: the Calm-Notepad recording screen shows the note and a
mic / Ask-Brain bar, and nothing else. If a commitment is spoken mid-meeting there is nowhere
in Murmur to put it without leaving the recording.

This is the one placement gap the three slices do **not** close, and it is deliberate — see the
kill list on new capture entry points. Recording it here so it is a known gap rather than an
oversight.

### A2. Correction — the raw-ISO due chip is a MOCK artifact, not a shipped bug

An earlier reading of the meeting screenshot showed the Action-items due chip rendering
`2026-07-04T15:00:00.000Z`. That is **not** what ships. `meeting-actions.component.html:47-52`
prints `it.dueDate` verbatim, but the backend's `find_date` produces a plain `YYYY-MM-DD`
(`src-tauri/src/summarize/action_items.rs:24,241` — the test asserts `Some("2026-07-01")`).
The full timestamp came from `scripts/screenshots/mock-tauri.js:428`, which seeds
`daysAgo(-2, 17, 0)`.

The residual real issue is smaller: the chip renders an unlocalized ISO date rather than a
locale-formatted one. Same `Intl.DateTimeFormat` rule the preset chips adopt applies here.
Worth folding into Slice 2; not worth a bug report on its own.

### A3. Two composer defects confirmed by reading, both already in the plan

- `reminder-composer.component.scss:20` paints the scrim with `var(--surface-hover)` — a 6%
  *lightening* wash — where `--scrim: rgba(0,0,0,0.5)` exists and is documented in
  `src/design-tokens/colors.css:10` as "dimming backdrop behind a floating modal". Visible in
  the capture as a modal that barely separates from the page.
- `DEFAULT_DUE_OFFSET_MS = 60 * 60 * 1000` (`reminder-composer.component.ts:30`, applied at
  `:431`) means a reminder created at 23:09 proposes 00:09. The capture shows exactly that.

### A4. Prototype

A clickable before/after prototype built from the real design tokens lives at
`docs/research/prototypes/reminders-ux/index.html` (self-contained, no build step). It has four
views: Note and Meeting, each in *Teraz* / *Propozycja*.

**Read it as a discussion aid, not as the spec.** It was drafted before the synthesis landed and
still shows two things this report explicitly does **not** recommend: a merged single follow-ups
list (see *Decision on the Action-items <-> Smart-reminders duplication* — the recommendation is
adjacency plus an owner ruling), and a bespoke composer layout (the recommendation keeps the
native date/time inputs and fixes the missing selector instead). Where the prototype and this
report disagree, the report wins.
