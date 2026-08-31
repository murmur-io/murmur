---
name: promo-video
description: Build Murmur's promo video from the REAL shipping UI — record (Playwright over the mocked Tauri IPC, CDP screencast at 2×) → compose (keyframed camera, kinetic captions, macOS window, end card) → encode (H.264 + VP9 + poster + web cut). Use whenever the user wants a promo/demo/tour video, a landing-page hero loop, a Product Hunt or launch clip, footage of a new feature, or a re-cut after the UI changed. Also covers directing a new scene, changing the script, and the verification a published video needs.
---

# /promo-video — build the tour from the real UI

Murmur's promo video is **rendered, not screen-recorded**: Playwright drives the real Angular app
over the screenshot harness's mocked Tauri IPC, and a single HTML page composites the result frame
by frame. No screen-recording permission, no signed build, no human clicking, and nothing
photographed from a real vault.

The harness is `scripts/promo/` — its [README](../../../scripts/promo/README.md) is the reference;
the research behind the edit is `docs/research/2026-08-31-app-promo-video.md`. **This skill is the
operating procedure**: what to run, what to decide, and what to check before anything is published.

---

## 1. Build it

```bash
# terminal 1 — serve the frontend (no Rust core needed; the mock replaces Tauri)
npx ng serve --host 127.0.0.1 --port 4310 --watch=false

# terminal 2 — record → compose → encode
MURMUR_URL=http://127.0.0.1:4310 bash scripts/promo/run.sh
```

~3 min to record, ~10 min to render at 60 fps, ~1 min to encode. Stages run individually:
`run.sh record` / `render` / `encode` / `hero` (the silent landing loop).

Everything lands in `.promo/` (gitignored). Copy to `landing/assets/` only what ships:
`promo-web.mp4`, `promo-web.webm`, `promo-poster.jpg`. The 60 fps master stays out of git.

**Iterate cheaply.** Never re-render at 60 fps to check a framing decision:

```bash
node scripts/promo/render.mjs --fps 2 --out .promo/probe      # ~100 frames, ~20 s
node scripts/promo/render.mjs --fps 2 --scenes graph --no-endcard --out .promo/g
```

Then LOOK at the PNGs. A green run is not evidence — every framing bug in this harness's history
was found by opening a frame, never by reading a log.

---

## 2. Direct a scene

Scenes are `SCENES` in `record.mjs`. The `film` object records a *mark* every time you move the
camera or place a caption, so the edit is a by-product of performing the scene:

| call | does |
| --- | --- |
| `film.goto(path)` | navigate (re-installs the drawn cursor) |
| `film.click(sel)` / `film.type(sel, text, {cps})` | act, cursor glides there first |
| `film.focus(target, {scale, ms, bias})` | push in on a **region** |
| `film.wide({ms})` | pull back to the whole window |
| `film.title(text, {sub, dur, kind})` | caption — `kind: "hero"` or `"lower"` |
| `film.liveCaption(text)` / `film.emit(evt, payload)` | drive the mocked backend |
| `film.assertClean(where)` | privacy-gate whatever is on screen right now |

**The rules that are not obvious:**

- **Aim at regions, not at controls.** The compositor clamps the frame inside the app screen, so a
  target near an edge is only reachable at a deep zoom; ask for a modest scale on a corner button
  and the camera quietly settles mid-screen instead. Pass a rect
  (`{x, y, width, height}` in 1600×900 app coordinates) when no single element is the subject.
- **Zoom deep enough to read, wide enough to fit.** At scale `s` roughly `2000/s` app pixels are in
  frame. A 1360-wide bar at 1.8× does not fit.
- **Captions own the bottom band.** The window is parked high; the ~190 px beneath it is theirs. At
  rest they never overlap the app — a deep push-in does, so do not schedule a caption over the beat
  where the viewer must read the UI.
- **Type no faster than the renderer samples.** The `/ask` scene captures ~19 fps; at 30 cps three
  characters land between frames and it reads as a stutter. Use `cps: 17`.
- **A scene with its own motion is worth more than a camera move.** The brain graph's force layout
  settles on screen (897 frames at 83 fps); a static list gave 7 fps and needed the camera to
  manufacture interest it did not have.

---

## 3. Write the script

Follow the research, not instinct (`docs/research/2026-08-31-app-promo-video.md`):

- **No logo intro.** The wordmark belongs on the end card, once the viewer has a reason for it.
- **Cold-open on the product mid-action.** The first 3 seconds decide whether the rest is watched,
  and for technical buyers the strongest hook is the software doing the thing.
- **Every beat must work on mute.** Hero videos autoplay muted or not at all, so the captions ARE
  the script — there is no narration to fall back on.
- **Lead with the differentiator, not the category.** "Meeting notes" is a crowded claim; *where the
  computation happens* is the whole positioning.
- **Lengths:** 60–90 s narrative demo; 4–20 s for an ambient hero loop; 2–3 min only for an
  evaluator who opted in.

Shipped shape: hook → problem → solution → proof → ownership → CTA.

---

## 4. Verify before anything is published

A marketing asset cannot be un-published. Two properties are enforced by the tool — the privacy
gate at every authored beat, and `assertSupersampled` on the capture scale — but these are yours:

- [ ] **Watch it.** End to end, not three sampled frames.
- [ ] **Every caption is true of what is on screen at that moment.** The "one brain" beat once ran
      over `/graph`, which is the entity *browser* — a list of chips, not the map.
- [ ] **Every URL, version and number is real.** The end card once carried an invented domain;
      `landing/CNAME` is the source of truth (`murmurnotes.io`).
- [ ] **The encoder's own gate passed** — it decodes each artefact and counts frames. A file can
      probe perfectly (right codec, right duration) and decode to one repeated frame.
- [ ] **Every `<source>` and `poster` the page references is a TRACKED file.** `.gitignore` carries
      a blanket `*.mp4` with a scoped exception for `landing/assets`; a new path outside it ships as
      a 404 and silently loses the video in browsers that cannot take the VP9 alternate.
- [ ] **The landing page was live-tested in a browser** — poster shown with nothing fetched
      (`preload="none"`), playback starting on click, zero page errors.

---

## 5. Facts worth not re-deriving

| | |
| --- | --- |
| **Capture scale** | `deviceScaleFactor: 2` gives `page.screenshot()` a 2× PNG but leaves **screencast** frames at 1×; `Emulation.setDeviceMetricsOverride` does not fix it. Only the launch flag `--force-device-scale-factor=2` does. Without it every camera move upscales. |
| **Headless vs headed** | SwiftShader raster starves the capture: 7 → 19 fps on `/ask`, 19 → 99 fps on `/record`. Headed is the default; `PROMO_HEADLESS=1` for a display-less machine. |
| **JPEG quality** | 45 and 65 produce identical frame counts — the cost is raster, not encode. Do not tune it for speed. |
| **Renderer decay** | Chromium retains decoded bitmaps across thousands of 3200×1800 frames: 10 → 0.6 fps at 2.6 GB RSS with 10 GB free. Hence a fresh context every `--recycle` (default 400) frames. |
| **Resume** | `--from N` keeps what is on disk. A long render that dies does not start over. |
| **Concurrency** | `encode.sh` takes a lock. Two encodes into one directory interleave into a file that probes fine and plays one frame. |

---

## 6. Deliberately absent

**Music** — no licensed track lives in this repo. The film is built to work on mute, so its absence
costs nothing; add one at the `encode.sh` step if a licence exists. **Voiceover** — the captions
carry the script. **Real user data** — the footage is the mocked demo world, exactly as the
screenshots are, and the privacy gate refuses a beat that renders anything else.

---

## Related

- `scripts/promo/README.md` — the harness reference (stages, mocking rules, gotchas)
- `scripts/screenshots/README.md` — the sibling harness; same mock, same demo world, same gate
- `.claude/skills/sync-release-copy` — when the CLAIMS change, not just the footage
- `.claude/rules/angular-zoneless.md` **T6** — why a hand-written mock defines a contract rather
  than verifying one
