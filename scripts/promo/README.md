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

## The composition model

The window is a **keyframed object**, not a fixed plate. `compose.html` `LAYOUTS` defines where it
sits and how it is tilted; `film.frame(mode)` moves it, on the same easing curve as everything else.

| mode | the window | the type |
| --- | --- | --- |
| `full` | large and centred | none — this is the shot for when the UI is the point |
| `lower` | parked high, ~190 px band beneath | a lower third, left-aligned to the window's edge |
| `title` | pushed right, tilted −13°, bleeding off frame | a column on the LEFT |
| `titleR` | mirrored: bleeding off the left | a column on the RIGHT |
| `card` | pushed back and down | a full-frame chapter caption |

**Why this exists.** The first cut used one fixed geometry for its whole length and reserved a dead
~190 px band underneath for type. That band is empty for most of the running time — about 18% of
every frame wasted — and, worse, it meant a caption could only ever be centred *underneath* the
product, which is the grammar of a SUBTITLE. Titles sit beside or over their subject. The layout
moving between beats is also the film's cheapest source of motion: the frame re-composes, so a cut
never lands on a picture the same shape as the one before it.

Two consequences worth knowing:

- **At rest the transform is the identity**, so the window really is where `LAYOUTS` puts it. As the
  camera pushes in, the anchor eases to the centre of frame and the containment clamp takes over.
  An earlier draft anchored to the centre *always*, which silently re-centred the window and made
  `title` produce a centred product with the headline lying across the app's own sidebar.
- **Window width IS resolution.** The app is captured 1600 CSS px wide, so a window `w` px wide
  renders its 14 px UI type at `14·w/1600` output px. The first draft of the re-cut shrank the
  window to make room for type and got 10.6 px type in the title shots — which reads as "low
  resolution", because it is. Bleed further off frame instead: 1520 px with 600 px outside the
  frame measured **+65% edge energy** over 1210 px at the same tilt. (The 3-D tilt is the obvious
  suspect for softness and is not the cause — at identical geometry it costs ~17%.)
- **`lower` is 1520 px, not more.** Bigger reads sharper, but the window's bottom edge then runs
  under the lower third — and the sidebar's Capture button sits in exactly that strip, in every
  scene, so the kicker lands on a button.

## Rhythm: action → description → transition → action

**The camera is LOCKED unless it is following a click.** Motion comes from the interface, from the
cut, or from one motivated push that travels and then stops. Never from the lens wandering.

This paragraph used to say the opposite — that "nothing should ever be perfectly still" — and
`driftAt`/`breathAt` existed to guarantee it. Both are gone. The full evidence is
`docs/research/2026-09-01-promo-cut-rhythm.md`; the short version:

- Phase-correlation global-motion measurement of **13 product films from 10 companies** (Linear,
  Notion, Raycast, Arc, Superhuman, Descript, Tella, Screen Studio) found the grammar is strictly
  **bimodal** — dead-locked (&lt;1 px/s) or decisive (&gt;15 px/s). Every reference film spends
  **0–28%** of its runtime in the 1–15 px/s "visible but purposeless" band; the drifting cut spent
  **64%** there. Linear holds one locked frame for **17.5 s**. Superhuman's hero is exactly 0.00 px/s.
- The drift's four terms ran at 0.110, 0.257, 0.093 and 0.224 Hz. Apple's HIG says by name: *"avoid
  showing an oscillation that has a frequency of around 0.2 Hz because people can be very sensitive
  to this frequency."*
- Ken Burns is defined as pan/zoom on **still photographs**, *"principally used when film or video
  material is not available"*. We have moving material — a live app. The rationale inverts.

### Measure the FOOTAGE, not the render

The old gate differenced the rendered frames, which a global translation raises everywhere at once —
so a 16 px sinusoid was the cheapest way to move "half the film frozen" to "11%" without adding one
bit of information. It measured the camera and called it life. Measure two things instead:

```bash
# 1. Does the APP do anything? Gaps between source frames = the product standing still.
python3 - <<'PY'
import json; m=json.load(open('.promo/manifest.json'))
for s in m['scenes']:
    ts=sorted(f['t'] for f in s['frames'])+[s['dur']]
    dead=sum(g for g in (ts[i+1]-ts[i] for i in range(len(ts)-1)) if g>400)
    print(f"{s['id']:<16}{s['dur']/1000:6.1f}s  app still {100*dead/s['dur']:3.0f}%")
PY
```

**2. Dead time with NO caption to read** — the number that actually matters. A locked shot while a
caption is being read is correct and wanted (see the brackets below); a locked shot with nothing to
read and nothing happening is the dead air. Subtract the caption windows from the still gaps.
Target **≤15%**. This cut measures 15%, against ~52% before the re-cut.

### Caption budget

Netflix (**20 cps**, ≤84 chars on screen, ≤2 lines, ≤7 s per event) and BBC (**160–180 wpm**)
converge on **≈0.33 s per word**, counting kicker + headline + sub together:

> **`dur` ≈ 0.33 × words + 0.6 s**

The pre-re-cut film had **15 of 15 captions under their own read time**, a 56-second collective
deficit — which is *why* it had to sit and stare. Delete the text nobody can finish and the hold has
no reason to exist.

### Shot brackets

| Beat | Hold | Camera |
| --- | --- | --- |
| Action beat (UI performing, no text) | 1.5–3 s | locked; cut on completion |
| Caption beat | `max(3 s, 0.33 × words + 0.6)`, copy ≤84 chars | **locked for the whole hold** |
| Demo hold (multi-step action worth watching) | 6–10 s | locked; motion from the UI only |
| Motivated push (follow a click / reveal) | 0.6–1.2 s travel, then a full stop | eased |

Target **ASL 4–6 s** and **≥6 hard cuts**. Beat length 8–10 s is right (Mayer's segmenting
principle, 10/10 tests, d = 0.79) — do NOT "fix" pacing by cutting faster: past ~12 shots per 30 s
persuasion declines. The beat and the shot are different units.

Boundaries have a vocabulary — `cut`, `dissolve`, `push`, `pushUp`, `whip`, `through`. Cut inside an
act, transition between acts. Dissolving every boundary is the video equivalent of ending every
sentence the same way.

## Directing a scene

Scenes live in `SCENES` in `record.mjs`. Each entry declares the transition it ENTERS on
(`dissolve` · `push` · `pushUp` · `whip` · `through`) plus a `run` that performs the take. The
`film` object is the authoring surface:

| call | does |
| --- | --- |
| `film.goto(path)` | navigate (re-installs the drawn cursor, and marks the roll point) |
| `film.click(sel)` / `film.type(sel, text)` | act, with the cursor gliding there first |
| `film.cursorTo(sel)` | glide the cursor without clicking |
| `film.reveal(sel)` | scroll a target to the middle of the app viewport |
| `film.focus(target, {scale, ms, bias})` | push the camera in on a **region** |
| `film.wide({ms})` | pull back out to the whole window |
| `film.frame(mode, {ms})` | move the WINDOW: `full` · `lower` · `title` · `titleR` · `card` |
| `film.title(text, {kicker, sub, dur, kind, align})` | a caption — `kind` matches the frame mode |
| `film.spotlight(target, {dur, label})` | ring a region and shade everything else down |
| `film.liveCaption(text)` / `film.emit(...)` | drive the mocked backend |
| `film.assertClean(where)` | run the privacy gate on what is on screen right now |

In a caption's `text`, `*starred words*` take the accent gradient and `|` is a hard line break. Use
the break — the good split in "Nothing leaves your Mac." is after *leaves*, and no column width
produces it reliably.

### Things that will bite you

- **Say it wide, then push in.** A caption authored *after* a `focus` lands on whatever the push-in
  filled the frame with, and over a dense panel — action items, a board brief — 46 px type turns the
  shot to mush. Author the caption, hold a beat, then move the camera.
- **Aim the camera at regions, not at small controls.** The compositor clamps the frame to stay
  inside the app screen, so a target near an edge is only reachable at a deep zoom; ask for a modest
  scale on a corner button and the camera quietly settles mid-screen instead. A `spotlight` is the
  right tool for something small — it points anywhere without moving the camera at all.
- **Zoom deep enough to read, wide enough to fit.** At scale `s` you see about `2000/s` app pixels
  across. A 1360-wide bar at 1.8× does not fit — it did not, and the first cut clipped both ends.
- **Scroll before you frame.** The action items, the speaker timeline and the graph canvas all start
  above 900 px and run below it. The camera cannot leave the app screen, so a `focus` on a target
  whose centre is off the bottom silently settles somewhere else and photographs the wrong thing.
  `film.reveal(sel)` first.
- **A static route hands you almost no footage.** The screencast emits on a *paint*: an un-driven
  take of `/dashboards` captured six frames in seven seconds, and `/people` five. Drive the UI — a
  click, or even a cursor glide — or the scene is one held still.
- **Photograph the thing you are claiming.** The caption "one brain" over `/graph` was a lie by
  framing: that route is the entity *browser* and photographs as a list of chips. The actual map is
  behind the "Full brain graph" disclosure on `/brain` — and it needs its **canvas** scrolled into
  view, not its component host, or the map sits below the fold.
- **Type no faster than the renderer samples.** The `/ask` scene captures at ~19 fps; at 30 cps
  three characters land between frames and it reads as a stutter, not as typing.
- **Re-run `record` after any shell change.** The scenes address the live DOM. The first cut was
  still aiming at `nav.global-rail`, which the 2026-08-31 sidebar rewrite deleted — 54 commits after
  the film shipped it was describing a UI that no longer existed, and nothing failed until a take
  was attempted.

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
