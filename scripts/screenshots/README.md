# README screenshot harness

Reproducibly captures the README screenshots as the **real shipping Angular UI**
rendered over a **mocked Tauri IPC** layer and a **privacy-safe demo world** — a
made-up startup ("Sonora") with invented people/projects. Nothing here touches a
real vault, database, microphone, or network, so the marketing shots are honest
UI, privacy-safe by construction (never your real meetings).

## Capture

```bash
# 1. serve the Angular frontend (no Rust core needed — the mock replaces Tauri)
npm start                      # → http://localhost:1420

# 2. capture every shot into docs/screenshots/ (dark theme, 2× retina)
bash scripts/screenshots/run.sh

# …or a subset:
bash scripts/screenshots/run.sh record-brain brain ask
```

Then downscale + palette-compress for the repo (2× shots → ~1600 px wide):

```bash
cd docs/screenshots
for n in analytics ask brain detail-note detail-timeline graph hero-record \
         library onboarding record-brain settings settings-brain \
         settings-privacy transcript; do
  sips --resampleWidth 1600 "$n.png" >/dev/null
  pngquant --force --quality=68-90 --strip --output "$n.png" "$n.png"
done
```

## Files

- **`mock-tauri.js`** — an `addInitScript` that installs a faithful
  `window.__TAURI_INTERNALS__` (invoke + `transformCallback` + the event
  plumbing) answering every command the FE calls, plus `window.__demoEmit(event,
  payload)` to fire backend events and `window.__demoConfig` to override the
  config per shot. Edit the demo world here.
- **`capture.mjs`** — the Playwright driver: one entry per screen (viewport,
  optional config override, and the interactions that put it in a
  screenshot-worthy state), each captured dark + 2×.
- **`run.sh`** — resolves Playwright from the npx cache (it is intentionally
  *not* a `package.json` dependency — a dev-only capture tool) and runs the
  driver.

## Notes

- The theme is forced dark (`localStorage['murmur-theme'] = 'dark'` +
  `colorScheme: 'dark'`) — Playwright otherwise defaults to a light color scheme
  and `data-theme="system"` would render light.
- Always **eyeball the PNGs** after a run — a green capture is not proof the
  screen rendered what you expected.
