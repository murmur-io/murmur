# How to make a top-notch app promo video — and how we make Murmur's

**Date:** 2026-08-31 · **Status:** research + shipped pipeline (`scripts/promo/`)

This brief answers two questions: (1) what actually makes a software promo video good in 2026, and
(2) how we produce one for Murmur *reproducibly, from the real UI*, without a video editor, a
screen recorder, a designer, or a single new npm dependency.

---

## Part 1 — What the good ones do

### 1.1 The first three seconds are the whole funnel

The consensus across every 2026 breakdown is blunt: the opening 3 seconds decide whether the rest of
the video is watched at all, and for B2B/technical software the highest-performing hook is **the
product doing the thing**, not a logo, not a title card, not a talking head, not a problem montage.
Showing the interface immediately signals "this is real and this is for you" and buys the credibility
to spend the next 10 seconds on the pitch.

Practical consequences for us:

- **No logo intro.** A wordmark at the head of a product video is dead airtime. The wordmark goes at
  the *end*, on the CTA card, when the viewer has a reason to remember it.
- **Cold-open on motion.** Frame 1 is the app mid-action — waveform moving, live caption arriving —
  not a static screen that fades in.
- **The differentiator is the hook.** For Murmur that is not "meeting notes" (a crowded, boring
  claim); it is *where the computation happens*. "Nothing leaves your Mac" over a live recording is
  the whole positioning in three seconds.

### 1.2 Structure: hook → problem → solution → proof → CTA

The five-part shape is near-universal, with the variant "first 10s hook on the problem, middle
demonstrates, close proves it works". Lengths that survive contact with real audiences:

| Use | Length |
| --- | --- |
| Landing-page **ambient** hero loop (muted, autoplay, no narrative) | 4–20 s |
| Landing-page / Product Hunt **narrative** demo | 60–90 s |
| Full walkthrough for an evaluator who opted in | 2–3 min |

We ship the 60–90 s narrative cut, because Murmur's pitch needs a *sequence* (record → transcribe →
note → ask → lock), and a 6-second ambient loop cut from the same footage for the hero slot.

### 1.3 It must work on mute

Hero videos autoplay muted or they do not autoplay at all; mobile browsers enforce this. So the video
has to carry its whole meaning with **no audio**: burned-in captions, on-screen labels, and pacing
that reads without narration. Sound becomes a bonus layer, never the carrier. This is also why we do
not need a voiceover to ship something good — and why a bad TTS voiceover would actively hurt.

Corollary: captions are not accessibility garnish here, they are the script. They get real
typographic care — the product's own typeface, its own accent color, generous size, safe margins.

### 1.4 The "Linear / Raycast / Vercel" motion language

What that aesthetic concretely *is*, decomposed:

- **Exaggerated cursor easing.** The cursor is a narrative actor: slow start, fast middle, slow stop
  (a strong ease-in-out), moving faster than a human hand ever does but never snapping. A real
  recorded cursor looks amateur by comparison — jittery, hesitant, with micro-corrections.
- **Motion with intent.** Every zoom, pan and cut points at something. No drifting "for production
  value"; restraint in motion mirrors restraint in color.
- **Auto-zoom on interaction.** The camera pushes in toward whatever was just clicked or typed, then
  releases. This is the single highest-leverage effect — it turns a flat screen recording into
  something that appears directed.
- **Continuous camera over hard cuts.** Whip-pans and pushes instead of cuts wherever the story
  allows; the eye stays oriented.
- **Quick ease-in-out curves read as "premium".** Slow-in/slow-out is the physics the eye expects;
  linear motion reads as cheap. Same easing family and duration ratios across every element = visual
  rhythm.

### 1.5 What Screen Studio automated, and why we can beat it here

Screen Studio's core trick is post-processing a raw capture: it infers zoom keyframes from click and
typing events, smooths the cursor path, enlarges the cursor, and drops the result on a styled
background.

**We are in a strictly better position than a screen recorder**, because we are not recording a
screen — we are *driving* the app. We do not have to infer where the click happened: we scheduled it.
We do not have to smooth a shaky cursor: there is no physical cursor, so we draw an ideal one on an
exact eased path. And we can capture at 2× and deliver at 1×, which makes a 2× "zoom in" **pixel-for-
pixel native rather than an upscale** — the one thing a screen recorder capturing at delivery
resolution cannot do.

### 1.6 The honest limits

- **No licensed music in this repo.** A promo video wants a bed. We ship silent-safe (see §1.3) and
  leave a documented drop-in slot rather than committing something we do not have rights to.
- **No voiceover.** See above; captions carry it.
- **The footage is the mocked demo world, not a real vault.** That is a feature — it is the same
  privacy-gated fixture the screenshots already use — but it means the video shows *the real UI with
  invented content*, which is exactly what the screenshots do, and must stay that way.

---

## Part 2 — How we build it

### 2.1 Constraints that shaped the design

- No new npm dependencies (binding FE rule). Playwright is already resolved from the npx cache by the
  screenshot harness; `ffmpeg` is already on the machine.
- The footage must be the **real shipping Angular UI**, never a mockup drawn in a design tool, and it
  must inherit the screenshot harness's **privacy gate** — a marketing asset gets published and cannot
  be un-published.
- It must be **re-runnable**: a version bump or a UI change should mean re-running one script, not
  redoing an edit in a timeline.

### 2.2 Three stages

```
  ng serve  ──▶  1. RECORD          ──▶  2. COMPOSE         ──▶  3. ENCODE
  (real UI)      Playwright drives       deterministic          ffmpeg
                 the app over the        frame-by-frame         PNG seq → mp4
                 mocked IPC;             render of the          + poster
                 CDP screencast          film in a headless     + hero loop cut
                 → JPEG frames @2×       browser
```

**Stage 1 — record.** Playwright drives the app exactly as `scripts/screenshots/capture.mjs` does,
reusing the same `mock-tauri.js` demo world. Frames come from CDP `Page.startScreencast`, which emits
a frame per browser paint with a presentation timestamp; we keep the timestamps and hold the last
frame through still periods, so timing is exact rather than approximated by a fixed frame rate. The
cursor is a DOM element we inject and animate with an eased transition, so it is captured in the
frames and is perfectly smooth by construction (§1.5).

**Stage 2 — compose.** A single HTML page is the "editor": it lays the recorded frames into a device
frame on the branded aurora background, applies the zoom/pan keyframes, and draws the caption layer in
the product's own typeface and design tokens. It exposes `seek(t)` and is rendered **frame by frame**
by Playwright — the same technique Remotion uses, minus the dependency. Deterministic: the same input
produces the same output, and a frame is never dropped because the machine was busy.

**Stage 3 — encode.** ffmpeg turns the PNG sequence into H.264 with `+faststart` for web playback,
plus a poster frame and a short ambient loop cut for the hero slot.

Why not just use Playwright's built-in `recordVideo`: it produces a viewport-sized VP8 webm at a
best-effort frame rate with no cursor and no control over quality — fine for a test artifact, not for
a published asset. Why not a real screen recording of the packaged app: it needs screen-recording
permission, a signed build, a human doing the clicking, and it would photograph a real vault.

### 2.3 The script we shot

| # | Beat | On screen | Caption |
| --- | --- | --- | --- |
| 1 | Hook (0–4 s) | live recording, caption streaming | **Nothing leaves your Mac.** |
| 2 | Problem (4–11 s) | the same screen, note being typed | Meetings end. The decisions scatter. |
| 3 | Solution (11–26 s) | transcript Me/Others → note | Recorded, transcribed and written up — on device. |
| 4 | Proof (26–44 s) | Ask with citations → receipts → graph | Ask across everything. Every claim cites its source. |
| 5 | Ownership (44–52 s) | Obsidian-shaped note, locked Space | Plain Markdown in your vault. A door you hold the key to. |
| 6 | CTA (52–58 s) | end card | **Murmur** · free & open source · macOS |

---

## Sources

- [10 Best SaaS Product Demo Videos That Convert (2026 Guide)](https://levitatemedia.com/learn/best-saas-demo-videos-2026-10-tips-for-creating-outstanding-ones)
- [12 Best Product Demo Video Examples in 2026 (With Breakdowns)](https://vidico.com/news/top-12-outstanding-saas-product-demo-videos/)
- [SaaS Demo Video Best Practices for Founders (2026 Guide)](https://demopolish.com/blog/saas-demo-video-best-practices/)
- [Product Demo Video Best Practices 2026 — ngram](https://www.ngram.com/blog/product-demo-best-practices)
- [Video Landing Page: How to Build One That Converts (2026)](https://swarmify.com/blog/video-landing-page/)
- [Cinematic Landing Pages with Video Backgrounds — 2026 Guide](https://sitesplaced.com/blog/cinematic-landing-pages-with-video-backgrounds)
- [Autoplay Videos: Best Practices for UX & Performance](https://www.ignite.video/en/articles/basics/autoplay-videos)
- [The Linear, Vercel, and Raycast Aesthetic: What It Actually Is](https://studiomaydit.com/blog/linear-vercel-raycast-aesthetic)
- [Screen Studio — Professional screen recorder for macOS](https://screen.studio/)
- [Screen Studio Review 2026 (auto-zoom, cursor smoothing)](https://scribehow.com/page/Screen_Studio_Review_2026_I_Tested_the_Auto-Zoom_Mac_Recorder_for_90_Days__Heres_the_Truth__0R7wu5TiSvqYAK3TzdygdQ)
- [Motion design fundamentals: Easing — Figma Learn](https://help.figma.com/hc/en-us/articles/41238219562007-Motion-design-fundamentals-Easing)
- [Mastering Motion Timing: Easing, Spacing, and Visual Rhythm](https://trydemotion.com/blog/mastering-motion-timing)
- [Playwright Screencast API guide (frame timestamps)](https://qaskills.sh/blog/playwright-1-59-screencast-api-guide-2026)
- [FFmpeg zoompan filter: Ken Burns without the jitter](https://www.ffmpeg-micro.com/blog/ffmpeg-zoompan-filter-ken-burns-zoom-and-pan-without-the-jitter)
- [ASS Override Tags — Aegisub](https://aegisub.org/docs/latest/ass_tags/)
