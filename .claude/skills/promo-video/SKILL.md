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
node scripts/promo/render.mjs --fps 3 --out .promo/probe                 # ~280 frames, ~60 s
node scripts/promo/render.mjs --fps 3 --scenes brain --no-endcard --out .promo/g
node scripts/promo/render.mjs --fps 3 --no-blur --out .promo/probe       # skip the motion blur
```

Then LOOK at the PNGs. A green run is not evidence — every framing bug in this harness's history
was found by opening a frame, never by reading a log. Four of them were found this way in the
2026-09-01 re-cut alone: a centred window in the title layout, a full stop orphaned a word-space
from its sentence, a lower third sitting on the app's own footer, and a spotlight label landing on
body copy.

A partial re-record keeps the scenes it is not re-shooting (`record.mjs boards people lock`), so
fixing one beat costs ~20 s rather than the whole three-minute take.

---

## 2. Direct a scene

Scenes are `SCENES` in `record.mjs` — one entry per SHOT, not per topic. Each declares the
transition it ENTERS on — `cut`, `dissolve`, `push`, `pushUp`, `whip`, `through` — and a `run` that performs the take. The `film` object records
a *mark* every time you move the camera, move the window or place a caption, so the edit is a
by-product of performing the scene:

| call | does |
| --- | --- |
| `film.goto(path)` | navigate (re-installs the cursor, marks the roll point) |
| `film.roll()` | start the take HERE — arrive/set up first, film only what follows |
| `film.clearAndType(sel, text)` | empty a field and type into it on camera |
| `film.click(sel)` / `film.type(sel, text, {cps})` | act, cursor glides there first |
| `film.cursorTo(sel)` | glide the cursor without clicking |
| `film.reveal(sel)` | scroll a target to the middle of the app viewport |
| `film.focus(target, {scale, ms, bias})` | push in on a **region** |
| `film.wide({ms})` | pull back to the whole window |
| `film.frame(mode, {ms})` | move the WINDOW: `full`·`lower`·`title`·`titleR`·`card` |
| `film.title(text, {kicker, sub, dur, kind})` | caption — `kind` matches the frame mode |
| `film.spotlight(target, {dur, label})` | ring a region, shade everything else down |
| `film.liveCaption(text)` / `film.emit(evt, payload)` | drive the mocked backend |
| `film.assertClean(where)` | privacy-gate whatever is on screen right now |

In caption text, `*starred words*` take the accent gradient and `|` is a hard line break.

**The window is a keyframed object, not a fixed plate.** `full` is large and centred; `lower` parks
it high for a lower third; `title` / `titleR` push it to one side, tilt it and let it bleed off
frame so the type gets a column. The 2026-09-01 re-cut exists largely because the first one used ONE
geometry throughout and reserved a permanent dead band for type — which wastes ~18% of every frame
and can only ever produce a centred *subtitle*, never a title. Re-composing between beats is also
the cheapest motion in the film.

**The rules that are not obvious:**

- **Say it wide, then push in.** A caption authored *after* a `focus` lands on whatever the push-in
  filled the frame with. Over the action-items panel or a board brief, 46 px type turns the shot to
  mush. Author the caption, hold a beat, then move the camera.
- **Aim at regions, not at controls.** The compositor clamps the frame inside the app screen, so a
  target near an edge is only reachable at a deep zoom; ask for a modest scale on a corner button
  and the camera quietly settles mid-screen instead. Pass a rect
  (`{x, y, width, height}` in 1600×900 app coordinates) when no single element is the subject — or
  use a `spotlight`, which points at something small without moving the camera at all.
- **Zoom deep enough to read, wide enough to fit.** At scale `s` roughly `2000/s` app pixels are in
  frame. A 1360-wide bar at 1.8× does not fit.
- **Scroll before you frame.** The action items, the speaker timeline and the graph canvas start
  above 900 px and run below it; the camera cannot leave the app screen, so a `focus` on a target
  centred off the bottom silently settles elsewhere and photographs the wrong thing. `reveal` first.
- **A static route hands you no footage.** The screencast emits on a paint: un-driven takes of
  `/dashboards` and `/people` captured six and five frames for ~7 s each. Drive the UI — a click, or
  a cursor glide — or the scene is a single held still.
- **Type no faster than the renderer samples.** The `/ask` scene captures ~19 fps; at 30 cps three
  characters land between frames and it reads as a stutter. Use `cps: 17`.
- **A scene with its own motion is worth more than a camera move.** The brain graph's force layout
  settles on screen (~1100 frames at ~90 fps); a static list gave 7 fps and needed the camera to
  manufacture interest it did not have.
- **Re-run `record` after any shell change.** The scenes address the live DOM. The first cut still
  aimed at `nav.global-rail`, deleted by the 2026-08-31 sidebar rewrite — 54 commits after the film
  shipped it described a UI that no longer existed, and nothing failed until a take was attempted.

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

Shipped shape (2026-09-01 re-cut, ~85 s, **eighteen shots**, ASL 4.7 s, 12 hard cuts). One entry
per SHOT, grouped into seven acts — cut inside an act, transition between acts:

| act | shots |
| --- | --- |
| capture | `hook` (the note typed on camera) · `hookLive` |
| the output | `note` · `noteRelated` (clicks through to the linked note) · `noteActions` |
| proof | `speakers` · `speakersTopics` · `askType` · `askAnswer` |
| the map | `brain` |
| organise | `workspace` · `workspaceRail` · `search` |
| surfaces | `boards` · `boardsTabs` · `people` |
| ownership | `lockRefuse` (open a workspace, then click the sealed one) · `lockFiles` |

**Every shot performs something, and the caption runs WITH the action, not after it** — words and
pictures at the same time (Mayer's temporal contiguity, 8/8 tests, d = 1.31), which also means the
caption's reading time is paid for by the action's duration instead of by a hold. That is what
removes the dead air. `lockRefuse` is the pattern in miniature: open a workspace that HAS contents,
then click the sealed one and watch it refuse — the refusal only means something once you have just
seen what an open workspace looks like.

**Say only what the frame shows.** This is not a style note, it is the failure mode with the
shortest fuse: a draft of the hook claimed "recording, transcription *and reasoning* run locally"
while the film itself photographs a `claude-opus-4-8` model chip, and `DEFAULT_PROVIDER_ID` is
`claude_code` — cloud. The caption was disproven by its own footage. Check every claim against the
code before it is rendered, not after: the unconditionally true version of that line is CLAUDE.md's
first constraint (audio and transcript stay on the device), and it is just as strong. Two more from
the same pass: "every claim cites the second of audio it came from" over source chips that carry a
date, and a "one keystroke" kicker over a sidebar toggle that has no accelerator bound to it.

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
| **Render cost** | ~4.3 frames/s at 1920×1080 with motion blur on, so a 93 s film at 60 fps is ~22 min. Probe at `--fps 3` (~1 min for the whole cut) and add `--no-blur` when only framing is in question. |
| **Partial re-record** | `record.mjs <scene…>` re-shoots only those scenes and merges them into the existing manifest, in `SCENES` order. Fixing one beat costs ~20 s, not the whole take. But durations shift, so the RENDER still restarts from the first changed scene. |
| **Roll point** | `film.goto` marks the moment the app is on screen; frames before it are the blank page painting in, and are dropped. The last pre-roll frame is kept and re-stamped, because a static route may never repaint again — filtering strictly deleted a whole opening beat and took a 10 s scene down to 5. |
| **Poster** | `PROMO_POSTER_SEC` picks the frame. The default of 2 s is only a safe default — it is whatever the film happens to be doing then, and with `preload="none"` the poster is the only frame most visitors ever see. |
| **Window size IS resolution** | The app is captured 1600 CSS px wide, so a window `w` px wide renders its 14 px UI type at `14·w/1600` output px. Shrinking the window to make room for type is paid for in legibility: 1210 px gives 10.6 px type and reads as "low resolution", because it is. A title shot should BLEED further off frame, not shrink — 1520 px (0.95×) with 600 px off-frame measured **+65% edge energy** over 1210 px at the same tilt. |
| **The 3-D tilt is NOT the softness** | Worth knowing because it is the obvious suspect and it is wrong: at identical geometry, `rotateY(-13deg)` costs ~17% of edge energy (111 vs 134 Laplacian variance), not the 3.8× a naive before/after suggests. That 3.8× was the window size, measured across two different layouts. Isolate the variable before you act on it. |

### THE CAMERA IS LOCKED UNLESS IT IS FOLLOWING A CLICK

Motion comes from the interface, from the cut, or from one motivated push that travels and stops.
**Never from the lens wandering.** There was briefly a `driftAt` here that floated the whole picture
continuously so no frame would ever be still; it is deleted, and the reasoning is worth keeping so
nobody rebuilds it. Full evidence: `docs/research/2026-09-01-promo-cut-rhythm.md`.

- Phase-correlation measurement of **13 product films from 10 companies** (Linear, Notion, Raycast,
  Arc, Superhuman, Descript, Tella, Screen Studio): the grammar is strictly **bimodal** —
  dead-locked (&lt;1 px/s) or decisive (&gt;15 px/s). Reference films spend **0–28%** of runtime in the
  1–15 px/s "visible but purposeless" band; the drifting cut spent **64%**. Linear holds one locked
  frame for **17.5 s**; Superhuman's hero is exactly 0.00 px/s.
- The drift's terms ran at 0.110/0.257/0.093/0.224 Hz. Apple's HIG, by name: *"avoid showing an
  oscillation that has a frequency of around 0.2 Hz because people can be very sensitive to this
  frequency."* Two of four landed on it.
- Ken Burns is pan/zoom on **still photographs**, *"principally used when film or video material is
  not available"*. We have a live app. The rationale inverts into an argument against it.

**Measure the FOOTAGE, not the render.** The old gate differenced rendered frames, which a global
translation raises everywhere at once — so a sinusoid was the cheapest way to move "half the film
frozen" to "11%" without adding one bit of information. It measured the camera and called it life.
Measure instead (a) gaps between SOURCE frames, i.e. the app standing still, and (b) the part of
that with **no caption on screen**, which is the real dead air. A locked shot while a caption reads
is correct; a locked shot with nothing to read and nothing happening is not. Target **≤15%**.

**And the dead air is usually a caption problem.** The pre-re-cut film had **15 of 15 captions under
their own read time** (56 s collective deficit) — it had to sit and stare because it was asking for
137 characters in 3.6 s. Netflix (20 cps, ≤84 chars, ≤7 s) and BBC (160–180 wpm) converge on
**`dur` ≈ 0.33 × words + 0.6 s**, counting kicker + headline + sub. Delete what nobody can finish
and the hold has no reason to exist.

**Do NOT fix pacing by cutting faster.** Beat length 8–10 s is right (Mayer's segmenting principle,
10/10 tests, d = 0.79); past ~12 shots per 30 s persuasion declines. The beat and the shot are
different units — target **ASL 4–6 s** with **≥6 hard cuts**, and put the missing events *inside*
the beat.

**A `title`/`titleR` frame needs a caption with the matching `kind`.** They are two halves of one
composition authored in two places, and a caption left on the default `lower` lands a bottom-left
third straight across a window that has been pushed to one side.

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
