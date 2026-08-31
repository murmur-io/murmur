# Promo-video harness

Builds Murmur's promotional video from the **real shipping Angular UI**, reproducibly, with no video
editor and no new dependencies. Same principle as the sibling
[`scripts/screenshots/`](../screenshots/README.md) harness — and it reuses that harness's mocked
Tauri IPC, its privacy-safe demo world, and its privacy gate.

The research behind the edit (what makes a software promo work, and why this is a *render* rather
than a screen recording) is `docs/research/2026-08-31-app-promo-video.md`.

## Build

```bash
# 1. serve the frontend (no Rust core needed — the mock replaces Tauri)
npx ng serve --host 127.0.0.1 --port 4310 --watch=false

# 2. record → compose → encode
MURMUR_URL=http://127.0.0.1:4310 bash scripts/promo/run.sh

# …or one stage at a time:
MURMUR_URL=http://127.0.0.1:4310 bash scripts/promo/run.sh record
bash scripts/promo/run.sh render
bash scripts/promo/run.sh encode
bash scripts/promo/run.sh hero     # the silent landing-page loop
```

Everything lands in `.promo/` (gitignored): `frames/` raw footage, `render/` composited PNGs,
`out/promo.mp4` + `out/promo.webm` + `out/promo-poster.jpg`. Copy the finished files to
`landing/assets/` when you are happy with them — the intermediates are large and rebuildable.

Roughly 3 min to record, ~10 min to render at 60 fps, ~1 min to encode.

## The three stages

```
  ng serve  ──▶  1. record.mjs      ──▶  2. compose.html   ──▶  3. encode.sh
  (real UI)      Playwright drives       the EDIT: camera,      ffmpeg
                 the app over the        captions, window,      PNG seq → mp4/webm
                 mocked IPC; CDP         cross-dissolves;       + poster
                 screencast @2×          render.mjs drives
                 → JPEG + marks          it frame by frame
```

**1. `record.mjs` — the take.** Drives the app scene by scene. Every call that matters to the cut
(`focus`, `wide`, `title`) records a *mark* stamped with the millisecond it happened, so the edit is
a by-product of performing the scene rather than a second, hand-maintained timeline. Frames come
from CDP `Page.startScreencast`, which emits one per browser paint with a presentation timestamp;
stage 2 places each frame at the instant it actually happened and holds the last one through still
periods.

**2. `compose.html` + `render.mjs` — the edit.** One HTML page *is* the film: it lays the footage
into a macOS window on the product's aurora field, applies the keyframed camera, and draws the
caption layer in the product's own typeface and tokens. `await window.seek(ms)` paints one instant
and resolves only once every image has decoded, so `render.mjs` can screenshot it frame by frame.
Deterministic: same input, same output, and a busy machine drops nothing.

**3. `encode.sh` — delivery.** H.264 High / yuv420p / `+faststart` (plays everywhere), a VP9
alternate, and a poster frame.

## Directing a scene

Scenes live in `SCENES` in `record.mjs`. The `film` object is the authoring surface:

| call | does |
| --- | --- |
| `film.goto(path)` | navigate (re-installs the drawn cursor) |
| `film.click(sel)` / `film.type(sel, text)` | act, with the cursor gliding there first |
| `film.focus(target, {scale, ms, bias})` | push the camera in on a **region** |
| `film.wide({ms})` | pull back out to the whole window |
| `film.title(text, {sub, dur, kind})` | a caption — `kind: "hero"` or `"lower"` |
| `film.liveCaption(text)` / `film.emit(...)` | drive the mocked backend |
| `film.assertClean(where)` | run the privacy gate on what is on screen right now |

### Things that will bite you

- **Aim the camera at regions, not at small controls.** The compositor clamps the frame to stay
  inside the app screen, so a target near an edge is only reachable at a deep zoom; ask for a modest
  scale on a corner button and the camera quietly settles mid-screen instead. The clamp is what
  keeps half the shot from being empty background.
- **Zoom deep enough to read, wide enough to fit.** At scale `s` you see about `2000/s` app pixels
  across. A 1360-wide bar at 1.8× does not fit — it did not, and the first cut clipped both ends.
- **Captions own the bottom band.** The window is parked high and the ~190 px beneath it belongs to
  the captions. At rest they never overlap; a deep push-in does put type over pixels, so do not
  schedule a caption over the beat where the viewer must read the UI.
- **Photograph the thing you are claiming.** The caption "one brain" over `/graph` was a lie by
  framing: that route is the entity *browser* and photographs as a list of chips. The actual map is
  behind the "Full brain graph" disclosure on `/brain` — and it needs its **canvas** scrolled into
  view, not its component host, or the map sits below the fold.
- **Type no faster than the renderer samples.** The `/ask` scene captures at ~19 fps; at 30 cps
  three characters land between frames and it reads as a stutter, not as typing.

## Two properties enforced by the tool

**The privacy gate**, inherited from the screenshot harness and run at every authored beat rather
than once at the end — a leak can sit in three frames out of three thousand, and nobody eyeballing a
50-second video will catch it. A hit aborts the run.

**The capture really is 2×.** `assertSupersampled` reads the JPEG header and fails the run if the
footage came back at 1×. This is not paranoia — it is a bug that already happened and is invisible
except as "the video looks a bit cheap":

- a context's `deviceScaleFactor: 2` gives `page.screenshot()` a 2× PNG but leaves **screencast**
  frames at 1×;
- an explicit `Emulation.setDeviceMetricsOverride` with `deviceScaleFactor: 2` does not fix it
  either;
- the **launch flag** `--force-device-scale-factor=2` does.

Without it every camera move is an upscale, right where the viewer is being asked to read.

## Measured, so nobody re-derives it

| | |
| --- | --- |
| headless (SwiftShader) vs headed (GPU), `/ask` scene | 7 fps → **19 fps** |
| headless vs headed, `/record` scene | 19 fps → **99 fps** |
| JPEG quality 45 vs 65 at 2× | identical frame counts — the cost is **raster, not encode** |

The screencast only emits a frame when the page paints one, so a slow renderer does not merely cost
time, it costs *footage*. Hence headed by default; `PROMO_HEADLESS=1` for a machine with no display.

## Deliberately not here

- **Music.** A promo wants a bed, and this repo has no licensed track. The film is built to work on
  mute (burned-in captions, no narration) because hero videos autoplay muted anyway — so the missing
  audio costs nothing. Add a track at the `encode.sh` step if you license one.
- **Voiceover.** Same reason: the captions carry the script.
- **Real user data.** The footage is the mocked demo world, exactly as the screenshots are.
