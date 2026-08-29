# Product screenshot harness

Reproducibly captures every screenshot the README and the landing page use, as the
**real shipping Angular UI** rendered over a **mocked Tauri IPC** layer and a
**privacy-safe demo world** — a made-up startup ("Sonora") with invented people
and projects. Nothing here touches a real vault, database, microphone or network,
so the marketing shots are honest UI, privacy-safe by construction.

## Capture

```bash
# 1. serve the frontend (no Rust core needed — the mock replaces Tauri)
npx ng serve --host 127.0.0.1 --port 4310 --watch=false

# 2. capture every shot into docs/screenshots/ (dark theme, 2× retina)
MURMUR_URL=http://127.0.0.1:4310 bash scripts/screenshots/run.sh

# …or a subset:
MURMUR_URL=http://127.0.0.1:4310 bash scripts/screenshots/run.sh dashboard tasks

# 3. downscale + palette-compress for the repo (33 MB -> ~5 MB)
bash scripts/screenshots/optimize.sh
```

`npm start` also serves the app, but it builds `murmur-brain` first; for
screenshots that Rust build buys you nothing.

> **`ng serve --watch=false` does not pick up edits.** If you change anything under
> `src/` — including a string that appears in a shot — restart the server or the
> capture will silently photograph the previous build.

## The two guarantees

A marketing image gets published and cannot be un-published, so two properties are
enforced by the driver rather than left to whoever runs it.

**1. The privacy gate.** Every shot's rendered text (plus the document title) is
scanned before the PNG is written. A hit **refuses** the shot — it is not saved,
and the run exits non-zero. It covers:

- operator identity (a real name, a work domain, a GitHub handle);
- any `/Users/<name>` that is not the demo one — a real home directory in a shot
  means the mock was bypassed and a real vault rendered;
- email addresses outside the fictional demo domains;
- strings from the operator's actual vault that once leaked into this repo's e2e
  fixtures (they are fine in a test, and not fine on a landing page);
- the pre-rename product name, because "MeetNotes" in a 2.x shot is its own kind of
  untruth.

It earned its keep on its first run by refusing all 27 shots over a stale window
title. Add to `PRIVACY_DENY` in `capture.mjs` rather than reasoning about whether a
given run happens to be clean.

**2. The version comes from `package.json`.** The driver injects it as
`window.__demoVersion`; the mock never carries a literal. This mock served
`"0.6.3"` into 2.0-era captures of the About screen.

## Files

- **`mock-tauri.js`** — an `addInitScript` that installs a faithful
  `window.__TAURI_INTERNALS__` (invoke + `transformCallback` + the event plumbing)
  answering every command the FE calls, plus `window.__demoEmit(event, payload)` to
  fire backend events and `window.__demoConfig` to override config per shot. **The
  demo world lives here.**
- **`capture.mjs`** — the Playwright driver: one entry per screen (viewport, optional
  config override, and the interactions that put it in a screenshot-worthy state),
  each captured dark + 2×, each scanned by the privacy gate.
- **`banner.html`** — the README/landing header image, rendered rather than drawn: it
  reuses the app's design tokens, its self-hosted typeface, and the shell's five-bar
  brand mark, so the banner cannot drift from the product. Captured as the `banner`
  shot.
- **`optimize.sh`** — resample to 1600 px wide + `pngquant`. Globs the directory, so
  a newly added shot cannot ship uncompressed the way it could when this was a
  copy-paste loop in this file with a hardcoded list of names.
- **`run.sh`** — resolves Playwright from the npx cache (it is intentionally *not* a
  `package.json` dependency — a dev-only capture tool) and runs the driver.

## Mocking rules, learned the hard way

**A hand-written mock DEFINES a contract; it does not verify one** (see
`.claude/rules/angular-zoneless.md` T6). Take every shape from the producer — the
Rust DTO in `src-tauri/src/`, or the interface in `src/app/core/models.ts` — and
never from memory. Guessing at `SpeakerSuggestion`'s fields threw inside a
`computed()` on the first render.

Watch for these specifically, each of which produced a silently wrong or blank
screenshot:

- **A command name that matches no fallback prefix returns `null`.** The router
  ends with `list_*`/`get_*` → `[]` and `has_*`/`is_*` → `false`; anything else
  falls to `null`. A component that iterates a null input throws, and one broken
  `computed()` can blank a whole panel — that is how the meeting timeline rendered
  as empty tracks.
- **A field the UI prints raw must arrive display-ready.** An action item's
  `dueDate` is a plain `YYYY-MM-DD` from `action_items.rs::find_date`, and the
  template prints it verbatim; a full ISO timestamp renders as
  `2026-07-04T15:00:00.000Z` beside a 📅.
- **Times must span the thing they describe.** Segment and timeline spans that sit
  in the first 60 seconds of a 47-minute meeting draw an invisible sliver.
- **Lazily-loaded data needs its own mock.** The transcript is fetched when the
  Audio tab opens, not with the meeting detail.

## Notes

- The theme is forced dark (`localStorage['murmur-theme'] = 'dark'` +
  `colorScheme: 'dark'`) — Playwright otherwise defaults to a light color scheme and
  `data-theme="system"` would render light.
- Always **eyeball the PNGs** after a run. A green capture is not proof the screen
  rendered what you expected: the run that produced "27/27 captured" also produced a
  record screen stuck on "Loading note…", an Ask answer with no citations, and a
  timeline with nothing in it.
